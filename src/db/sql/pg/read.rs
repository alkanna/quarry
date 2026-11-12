//! What the read path needs of PostgreSQL beyond the catalog queries: running text that carries user SQL inside a
//! read-only transaction that is always rolled back, and listing the schemas of a database.

use crate::db::sql::se;
use crate::db::sql::exec::{col_infos, Cells, ColInfo, SqlRow};
use crate::db::sql::{Db, Dialect, Pool};
use crate::db::{Msg, Res, Worker};
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use sqlx::postgres::{PgConnection, PgQueryResult, PgRow};
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
/// a setting an earlier one of the user's made. `extra` is a `SET LOCAL` run in the same transaction (see `begin_sql`).
///
/// A session advisory lock or a dblink connection opened by a function the user calls survives the rollback, and even
/// DISCARD ALL does not close a named dblink connection. So when the statement carries text the user typed
/// (`user_text`) the connection is closed instead of going back to the pool. Statements Quarry built itself keep the
/// cheap path.
pub(in crate::db::sql) async fn read_only(db: &Db, user_text: bool, extra: &str, sql: &str) -> Result<Vec<PgRow>, sqlx::Error> {
    let mut conn = db.pg_pool().map_err(sqlx::Error::Protocol)?.acquire().await?;
    let res = in_read_only(&mut conn, db, extra, async |tx| {
        let mut rows = Vec::new();
        let mut s = rows_of(tx, sql);
        while let Some(item) = s.next().await {
            if let Either::Right(r) = item? {
                rows.push(r);
            }
        }
        Ok(rows)
    })
    .await;
    if user_text || res.is_err() {
        conn.close_on_drop();
    }
    res
}

/// Runs `work` inside a `BEGIN READ ONLY` that is always rolled back. The transaction rolls back when it is dropped, so a
/// future that is cancelled midway leaves nothing open either. A ROLLBACK that fails is returned as the error when the
/// work succeeded: the connection may then still sit inside the transaction, so the caller must close it, never reuse it.
pub(in crate::db::sql) async fn in_read_only<T>(conn: &mut PgConnection, db: &Db, extra: &str, work: impl AsyncFnOnce(&mut PgConnection) -> Result<T, sqlx::Error>) -> Result<T, sqlx::Error> {
    let mut tx = conn.begin_with(db.pg_begin("BEGIN READ ONLY", extra)).await?;
    let res = work(&mut *tx).await;
    let end = tx.rollback().await;
    match res {
        Ok(v) => end.map(|_| v),
        Err(x) => Err(x),
    }
}

/// Points the script's session at the tab's schema. Bound, never spliced, and run by Quarry before any user text; the
/// user cannot change it afterwards, because the lexer refuses SET and set_config and the per-statement rollback would
/// undo one that got through. `path` is `Db::pg_path` of the tab's schema: the schema first, then the list saved on the
/// Advanced tab. An empty one keeps the session's own search_path.
pub(in crate::db::sql) async fn set_search_path(c: &mut PgConnection, path: &str) -> Result<(), sqlx::Error> {
    if path.is_empty() {
        return Ok(());
    }
    sqlx::query("SELECT set_config('search_path', $1, false)").persistent(false).bind(path).execute(&mut *c).await?;
    Ok(())
}

impl Db {
    /// The text that opens a transaction of Quarry's, all in one message: the BEGIN, the session's statement timeout as
    /// SET LOCAL, the settings a pooler that refused startup options leaves to each transaction, and `extra` (the
    /// per-run `pg_run_locals`). A pooler can hand the next statement to another backend, so none of these can be a
    /// session value there. The pooler's settings come before `extra` and after the timeout so they win over it. The
    /// user's own statement is never part of this text; it goes alone in the message that follows.
    pub(in crate::db::sql) fn pg_begin(&self, begin: &str, extra: &str) -> String {
        let (timeout, locals) = match &self.pool {
            Pool::Pg(p) => (p.timeout(), p.locals()),
            Pool::My(_) => ("0", ""),
        };
        let mut sql = format!("{begin}; {}", crate::db::sql::pg::set_timeout(timeout));
        for part in [locals, extra].into_iter().filter(|p| !p.is_empty()) {
            sql.push_str("; ");
            sql.push_str(part);
        }
        sql
    }
}

/// Opens the transaction a locked statement runs in, for a caller that cannot hand its work to `in_read_only` (a result
/// that streams to a file). When a setting cannot be applied the transaction is rolled back at once, so the connection
/// is never left inside one.
pub(in crate::db::sql) async fn begin_read_only(c: &mut PgConnection, db: &Db, extra: &str) -> Result<(), sqlx::Error> {
    begin_with(c, db.pg_begin("BEGIN READ ONLY", extra)).await
}

/// The transaction of one unlocked statement behind a pooler: `READ WRITE` stated in the transaction itself, because
/// the session cannot be changed there and its default may be read-only on the backend that answers.
pub(in crate::db::sql) async fn begin_read_write(c: &mut PgConnection, db: &Db, extra: &str) -> Result<(), sqlx::Error> {
    begin_with(c, db.pg_begin("BEGIN READ WRITE", extra)).await
}

async fn begin_with(c: &mut PgConnection, sql: String) -> Result<(), sqlx::Error> {
    if let Err(x) = c.execute(sql.as_str()).await {
        let _ = c.execute("ROLLBACK").await;
        return Err(x);
    }
    Ok(())
}

/// The characters sent before the user's statement when it sits in a paged wrapper (`lex::page_sql`), which a
/// server position has to be moved back by.
pub(in crate::db::sql) const PAGE_PREFIX: usize = 16;
/// The same for `lex::count_sql`.
pub(in crate::db::sql) const COUNT_PREFIX: usize = 23;

impl Db {
    /// Text of the user's own (the Content filter and sort) as a read: on MySQL on the pool, on PostgreSQL in the
    /// read-only transaction of `read_only`. Every row is read, over the text protocol, so values arrive as the server
    /// wrote them.
    pub(in crate::db::sql) async fn read_text(&self, schema: &str, sql: &str, user_text: bool) -> Res<Vec<SqlRow>> {
        self.read_text_with(schema, "", sql, user_text).await
    }

    /// `read_text` with a statement run first in the same transaction, which only PostgreSQL has. `user_text` says the
    /// statement carries something the user typed, so a PostgreSQL connection that ran it is closed afterwards.
    pub(in crate::db::sql) async fn read_text_with(&self, schema: &str, before: &str, sql: &str, user_text: bool) -> Res<Vec<SqlRow>> {
        let t = Instant::now();
        let res = match &self.pool {
            Pool::My(p) => raw_sql(sql).fetch_all(p).await.map(SqlRow::wrap),
            Pool::Pg(_) => read_only(self, user_text, before, sql).await.map(SqlRow::wrap_pg),
        };
        self.note(schema, sql, t, &res);
        res.map_err(se)
    }

    /// The columns `sql` would return, without running it, for a page that came back empty and still needs its headers.
    /// Preparing runs nothing, so no transaction is needed.
    pub(in crate::db::sql) async fn describe_text(&self, sql: &str) -> Res<Vec<ColInfo>> {
        match &self.pool {
            Pool::My(p) => Ok(col_infos(p.describe(sql).await.map_err(se)?.columns(), self.dialect)),
            Pool::Pg(_) => {
                // Planning with EXPLAIN can evaluate STABLE and IMMUTABLE functions of the text, so it runs inside a
                // read-only transaction that is rolled back, like every other statement that carries user text.
                let mut conn = self.pg_pool()?.acquire().await.map_err(se)?;
                let d = in_read_only(&mut conn, self, "", async |tx| tx.describe(sql).await).await;
                conn.close_on_drop();
                Ok(col_infos(d.map_err(se)?.columns(), Dialect::Postgres))
            }
        }
    }
}

/// One schema of a PostgreSQL database as the sidebar's switcher lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct PgSchema {
    pub name: String,
    /// Tables, views and the like the sidebar would list in it.
    pub tables: u64,
    /// The catalogs, TOAST and temporary schemas, hidden until "Show system schemas" is on.
    pub system: bool,
}

impl Worker {
    /// The schemas of `database` the login may use, sorted by name, with their table counts and a flag for the system
    /// ones. `db` is any `Db` of the connection; the database is picked here, because a PostgreSQL connection belongs
    /// to one database. Answers `Msg::SqlSchemas`.
    pub fn sql_schemas(&self, conn: u64, db: Db, database: String) {
        self.spawn(async move {
            let res = async {
                if db.dialect != Dialect::Postgres {
                    return Err("Only PostgreSQL has schemas".to_string());
                }
                let at = db.at(&database);
                let t = Instant::now();
                let sql = Dialect::Postgres.schemas_all();
                let rows = read_only(&at, false, "", sql).await.map(SqlRow::wrap_pg);
                at.note(&database, sql, t, &rows);
                let mut list: Vec<PgSchema> = rows
                    .map_err(se)?
                    .iter()
                    .filter_map(|r| Some(PgSchema { name: crate::db::sql::worker::text(r, "name")?, tables: crate::db::sql::structure::num(r, "tables").unwrap_or(0), system: r.int("system").is_some_and(|n| n != 0) }))
                    .collect();
                list.sort_by(|a, b| a.name.cmp(&b.name));
                Ok(list)
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
                let mut schemas: Vec<String> = rows.map_err(se)?.iter().filter_map(|r| crate::db::sql::worker::text(r, "name")).collect();
                schemas.sort();
                let (mut tables, mut views) = (Vec::new(), Vec::new());
                if !schema.is_empty() {
                    for r in at.meta(&schema, Dialect::Postgres.tables(), [schema.as_str()]).await? {
                        let Some(n) = crate::db::sql::worker::text(&r, "name") else { continue };
                        if crate::db::sql::worker::text(&r, "kind").is_some_and(|k| k.contains("VIEW")) {
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
