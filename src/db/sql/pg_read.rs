//! What the read path needs of PostgreSQL beyond the catalog queries: running text that carries user SQL inside a
//! read-only transaction that is always rolled back, and listing the schemas of a database.

use super::exec::{col_infos, ColInfo, SqlRow};
use super::{Db, Dialect, Pool};
use crate::db::{e, Msg, Res, Worker};
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use sqlx::postgres::{PgConnection, PgPool, PgQueryResult, PgRow};
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
            Pool::Pg(_) => read_only(&self.pg_pool()?, user_text, before, sql).await.map(SqlRow::wrap_pg),
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
                let rows = raw_sql(Dialect::Postgres.schemas()).fetch_all(&at.pg_pool()?).await.map(SqlRow::wrap_pg);
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
