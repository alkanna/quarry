//! What the read path needs of PostgreSQL beyond the catalog queries: running text that carries user SQL inside a
//! read-only transaction that is always rolled back, and listing the schemas of a database.

use super::exec::{col_infos, ColInfo, SqlRow};
use super::{Db, Dialect, Pool};
use crate::db::{e, Msg, Res, Worker};
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use sqlx::postgres::{PgConnection, PgDatabaseError, PgErrorPosition, PgPool, PgQueryResult, PgRow};
use sqlx::{raw_sql, Connection, Either, Executor};
use std::time::Instant;

/// The rows of one simple-protocol statement. A function with explicit lifetimes, because inside a spawned future the
/// executor lifetimes of `raw_sql(..).fetch_all(&mut conn)` do not infer.
fn rows_of<'c>(c: &'c mut PgConnection, sql: &'c str) -> BoxStream<'c, Result<Either<PgQueryResult, PgRow>, sqlx::Error>> {
    raw_sql(sql).fetch_many(c)
}

/// Runs one statement of the user's in its own `BEGIN READ ONLY`, rolls it back and never commits. The session guard
/// alone would not hold against `set_config('default_transaction_read_only', 'off', false)`, which a pooled session
/// would keep for the next statement, and a `client_encoding` change would make the server split the next statement
/// differently from the lexer. The rollback undoes both, and it is per statement, so no statement is ever parsed under
/// a setting an earlier one of the user's made. `before` runs first inside the same transaction, for a `SET LOCAL`.
///
/// A session advisory lock or a dblink connection opened by a function the user calls survives the rollback, and even
/// DISCARD ALL does not close a named dblink connection. So when the statement carries text the user typed
/// (`user_text`) the connection is closed instead of going back to the pool. Statements Quarry built itself keep the
/// cheap path.
pub(super) async fn read_only(pool: &PgPool, user_text: bool, before: &str, sql: &str) -> Result<Vec<PgRow>, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    let res = {
        // The transaction rolls back when it is dropped, so a future that is cancelled midway leaves nothing open either.
        let mut tx = conn.begin_with("BEGIN READ ONLY").await?;
        let mut rows = Vec::new();
        let mut failed = None;
        if !before.is_empty() {
            if let Err(x) = tx.execute(before).await {
                failed = Some(x);
            }
        }
        if failed.is_none() {
            let mut s = rows_of(&mut tx, sql);
            while let Some(item) = s.next().await {
                match item {
                    Ok(Either::Right(r)) => rows.push(r),
                    Ok(Either::Left(_)) => {}
                    Err(x) => {
                        failed = Some(x);
                        break;
                    }
                }
            }
        }
        let _ = tx.rollback().await;
        match failed {
            Some(x) => Err(x),
            None => Ok(rows),
        }
    };
    if user_text {
        conn.close_on_drop();
    }
    res
}

/// Points the script's session at the tab's schema. Bound, never spliced, and run by Quarry before any user text; the
/// user cannot change it afterwards, because the lexer refuses SET and set_config and the per-statement rollback would
/// undo one that got through. An empty schema keeps the server's own search_path.
pub(super) async fn set_search_path(c: &mut PgConnection, schema: &str) -> Result<(), sqlx::Error> {
    if schema.is_empty() {
        return Ok(());
    }
    sqlx::query("SELECT set_config('search_path', $1, false)").bind(Dialect::Postgres.quote_ident(schema)).execute(&mut *c).await?;
    Ok(())
}

/// The session's statement_timeout as the server spells it ("0", "30s"), to be applied again with SET LOCAL inside
/// each transaction. A pooler in transaction mode can hand the next statement to another backend, so a session value
/// alone would not hold there.
pub(super) async fn statement_timeout(c: &mut PgConnection) -> Result<String, sqlx::Error> {
    let rows = c.fetch_all("SELECT current_setting('statement_timeout')").await?;
    use super::exec::Cells;
    Ok(rows.first().and_then(|r| r.cell(0)).map(|b| String::from_utf8_lossy(b).into_owned()).filter(|v| v.chars().all(|c| c.is_ascii_alphanumeric())).unwrap_or_else(|| "0".into()))
}

/// Opens the transaction a locked statement runs in. When a setting cannot be applied the transaction is rolled back
/// at once, so the connection is never left inside one. `locals` (`Db::pg_locals`) comes last so that behind a pooler,
/// where the session has none of Quarry's settings, its timeout and output formats win over the session's.
pub(super) async fn begin_read_only(c: &mut PgConnection, timeout: &str, locals: &str) -> Result<(), sqlx::Error> {
    c.execute("BEGIN READ ONLY").await?;
    let mut set = c.execute(format!("SET LOCAL statement_timeout = '{timeout}'").as_str()).await.map(|_| ());
    if set.is_ok() && !locals.is_empty() {
        set = c.execute(locals).await.map(|_| ());
    }
    if let Err(x) = set {
        let _ = c.execute("ROLLBACK").await;
        return Err(x);
    }
    Ok(())
}

/// The characters sent before the user's statement when it sits in a paged wrapper (`lex::page_sql`), which a
/// server position has to be moved back by.
pub(super) const PAGE_PREFIX: usize = 16;
/// The same for `lex::count_sql`.
pub(super) const COUNT_PREFIX: usize = 23;

/// A server error worded for `db::explain`: the SQLSTATE, the message, the detail and hint when the server sends
/// them, and where in the user's statement it points. `shift` is how many characters were put in front of the
/// statement. Anything that is not a PostgreSQL error keeps the plain wording.
pub(super) fn pg_error(err: &sqlx::Error, stmt: &str, shift: usize) -> String {
    let Some(d) = err.as_database_error().and_then(|d| d.try_downcast_ref::<PgDatabaseError>()) else { return e(err) };
    let mut out = format!("error returned from database: {}: {}", d.code(), d.message());
    if let Some(x) = d.detail() {
        out.push_str(&format!("\nDetail: {x}"));
    }
    if let Some(x) = d.hint() {
        out.push_str(&format!("\nHint: {x}"));
    }
    if let Some(PgErrorPosition::Original(p)) = d.position() {
        // The server counts characters from 1.
        if p > shift {
            let at = p - shift;
            let (line, col) = stmt.chars().take(at - 1).fold((1, 1), |(l, c), ch| if ch == '\n' { (l + 1, 1) } else { (l, c + 1) });
            out.push_str(&format!("\nPosition: {at} (line {line}, column {col})"));
        }
    }
    out
}

impl Db {
    /// Text of the user's own (the Content filter and sort) as a read: on MySQL on the pool, on PostgreSQL in the
    /// read-only transaction of `read_only`. Every row is read, over the text protocol, so values arrive as the server
    /// wrote them.
    pub(super) async fn read_text(&self, schema: &str, sql: &str, user_text: bool) -> Res<Vec<SqlRow>> {
        self.read_text_with(schema, "", sql, user_text).await
    }

    /// `read_text` with a statement run first in the same transaction, which only PostgreSQL has. `user_text` says the
    /// statement carries something the user typed, so a PostgreSQL connection that ran it is closed afterwards.
    pub(super) async fn read_text_with(&self, schema: &str, before: &str, sql: &str, user_text: bool) -> Res<Vec<SqlRow>> {
        let t = Instant::now();
        let res = match &self.pool {
            Pool::My(p) => raw_sql(sql).fetch_all(p).await.map(SqlRow::wrap),
            Pool::Pg(p) => {
                // Behind a pooler the settings are SET LOCAL in the transaction itself.
                let first = [p.locals(), before].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join("; ");
                read_only(&self.pg_pool()?, user_text, &first, sql).await.map(SqlRow::wrap_pg)
            }
        };
        self.note(schema, sql, t, &res);
        res.map_err(e)
    }

    /// The columns `sql` would return, without running it, for a page that came back empty and still needs its headers.
    /// Preparing runs nothing, so no transaction is needed.
    pub(super) async fn describe_text(&self, sql: &str) -> Res<Vec<ColInfo>> {
        match &self.pool {
            Pool::My(p) => Ok(col_infos((&*p).describe(sql).await.map_err(e)?.columns(), false)),
            Pool::Pg(_) => {
                let pool = self.pg_pool()?;
                Ok(col_infos((&pool).describe(sql).await.map_err(e)?.columns(), true))
            }
        }
    }
}

impl Worker {
    /// The schemas of `database` that hold user objects, sorted. `db` is any `Db` of the connection; the database is
    /// picked here, because a PostgreSQL connection belongs to one database. Answers `Msg::SqlSchemas`.
    #[allow(dead_code)] // the sidebar asks from step 8 on
    pub fn sql_schemas(&self, conn: u64, db: Db, database: String) {
        self.spawn(async move {
            let res = async {
                if db.dialect != Dialect::Postgres {
                    return Err("Only PostgreSQL has schemas".to_string());
                }
                let at = db.at(&database);
                let t = Instant::now();
                let rows = read_only(&at.pg_pool()?, false, &at.pg_locals(), Dialect::Postgres.schemas()).await.map(SqlRow::wrap_pg);
                at.note(&database, Dialect::Postgres.schemas(), t, &rows);
                let mut names: Vec<String> = rows.map_err(e)?.iter().filter_map(|r| super::worker::text(r, "name")).collect();
                names.sort();
                Ok(names)
            }
            .await;
            Msg::SqlSchemas { conn, database, res }
        });
    }

    /// The schemas of `database` and, when `schema` is not empty, the tables and views in it, for a shell attachment.
    /// A PostgreSQL connection belongs to one database, so `database` picks the pool here. Answers
    /// `Msg::Command { tag: "shpg" }` with `conn`, `database`, `schema`, `schemas`, `tables` and `views`, or an `error`
    /// text in place of the lists, so the answer still says which attachment it was for.
    pub fn pg_shell_list(&self, conn: u64, db: Db, database: String, schema: String) {
        self.spawn(async move {
            let at = db.at(&database);
            let res = async {
                let t = Instant::now();
                let rows = raw_sql(Dialect::Postgres.schemas()).fetch_all(&at.pg_pool()?).await.map(SqlRow::wrap_pg);
                at.note(&database, Dialect::Postgres.schemas(), t, &rows);
                let mut schemas: Vec<String> = rows.map_err(e)?.iter().filter_map(|r| super::worker::text(r, "name")).collect();
                schemas.sort();
                let (mut tables, mut views) = (Vec::new(), Vec::new());
                if !schema.is_empty() {
                    for r in at.meta(&schema, Dialect::Postgres.tables(), [schema.as_str()]).await? {
                        let Some(n) = super::worker::text(&r, "name") else { continue };
                        if super::worker::text(&r, "kind").is_some_and(|k| k.contains("VIEW")) {
                            views.push(n.clone());
                        }
                        tables.push(n);
                    }
                    tables.sort();
                }
                Ok::<_, String>((schemas, tables, views))
            }
            .await;
            let strings = |v: Vec<String>| v.into_iter().map(mongodb::bson::Bson::String).collect::<Vec<_>>();
            let mut d = mongodb::bson::doc! { "conn": conn as i64, "database": &database, "schema": &schema };
            match res {
                Ok((schemas, tables, views)) => {
                    d.insert("schemas", strings(schemas));
                    d.insert("tables", strings(tables));
                    d.insert("views", strings(views));
                }
                Err(x) => {
                    d.insert("error", x);
                }
            }
            Msg::Command { tag: "shpg".into(), res: Ok(d) }
        });
    }
}
