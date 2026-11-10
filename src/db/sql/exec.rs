//! The line between the SQL code and sqlx's engine types. Every enum has one arm per engine, so adding an
//! engine is a new arm and the compiler lists what still needs one. Generics over `sqlx::Database` were
//! ruled out: they put higher-ranked bounds in every signature, and the engines still differ in how a
//! database is selected, how a statement is stopped and how an insert reports its key.
//!
//! Modules that only make sense on one engine (DDL, users, dump and so on) keep sqlx's own types and reach
//! them through `Db::my_pool` and `Conn::my`, which answer with an error when the connection is another engine.

use super::value::Param;
use super::{Db, Pool};
use crate::db::{e, Res};
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use sqlx::mysql::{MySqlColumn, MySqlConnectOptions, MySqlRow};
use sqlx::{raw_sql, Column, Connection, Either, Executor, Row as _, TypeInfo};

/// How one statement ended: the rows it changed and the key its insert made, when the engine says so.
#[derive(Clone, Copy, Debug, Default)]
pub struct Done {
    affected: u64,
    last_id: u64,
}

impl Done {
    pub fn rows_affected(&self) -> u64 {
        self.affected
    }

    pub fn last_insert_id(&self) -> u64 {
        self.last_id
    }
}

/// One item of a text-protocol read: a row, or the end of one result (or the OK of a write).
pub enum Item {
    Row(SqlRow),
    End(Done),
}

/// A result column as the server reports it. `ty` is the type name, such as "INT UNSIGNED".
#[derive(Clone, Debug)]
pub struct ColInfo {
    pub name: String,
    pub ty: String,
}

/// What `make_cols` needs of a result column, so rows read through sqlx directly (the MySQL-only modules) and
/// rows of a `SqlRow` share one reader.
pub trait ColMeta {
    fn col_name(&self) -> &str;
    fn col_type(&self) -> String;
}

impl ColMeta for ColInfo {
    fn col_name(&self) -> &str {
        &self.name
    }

    fn col_type(&self) -> String {
        self.ty.clone()
    }
}

impl ColMeta for MySqlColumn {
    fn col_name(&self) -> &str {
        self.name()
    }

    fn col_type(&self) -> String {
        self.type_info().name().to_string()
    }
}

impl<T: Cells + ?Sized> Cells for &T {
    fn cell(&self, i: usize) -> Option<&[u8]> {
        (**self).cell(i)
    }

    fn cell_named(&self, name: &str) -> Option<&[u8]> {
        (**self).cell_named(name)
    }

    fn col_pos(&self, name: &str) -> Option<usize> {
        (**self).col_pos(name)
    }

    fn int(&self, name: &str) -> Option<i64> {
        (**self).int(name)
    }

    fn uint(&self, name: &str) -> Option<u64> {
        (**self).uint(name)
    }
}

/// What the readers need of a row. Implemented for `SqlRow` and, for the MySQL-only modules, for sqlx's row.
pub trait Cells {
    /// Column `i` as the server's own bytes. Metadata queries read strings through this too, because a `_bin`
    /// collation makes sqlx report text as VARBINARY.
    fn cell(&self, i: usize) -> Option<&[u8]>;
    fn cell_named(&self, name: &str) -> Option<&[u8]>;
    fn col_pos(&self, name: &str) -> Option<usize>;
    fn int(&self, name: &str) -> Option<i64>;
    fn uint(&self, name: &str) -> Option<u64>;
}

impl Cells for MySqlRow {
    fn cell(&self, i: usize) -> Option<&[u8]> {
        self.try_get_unchecked::<Option<&[u8]>, _>(i).ok().flatten()
    }

    fn cell_named(&self, name: &str) -> Option<&[u8]> {
        self.try_get_unchecked::<Option<&[u8]>, _>(name).ok().flatten()
    }

    fn col_pos(&self, name: &str) -> Option<usize> {
        self.columns().iter().position(|c| c.name() == name)
    }

    fn int(&self, name: &str) -> Option<i64> {
        self.try_get::<i64, _>(name).ok()
    }

    /// Whichever width and signedness the server reports.
    fn uint(&self, name: &str) -> Option<u64> {
        if let Ok(v) = self.try_get::<Option<u64>, _>(name) {
            return v;
        }
        self.try_get::<Option<i64>, _>(name).ok().flatten().and_then(|n| u64::try_from(n).ok())
    }
}

/// A row of either engine. Cells are borrowed from the row, never copied.
pub enum SqlRow {
    My(MySqlRow),
}

impl SqlRow {
    pub fn wrap(rows: Vec<MySqlRow>) -> Vec<SqlRow> {
        rows.into_iter().map(SqlRow::My).collect()
    }

    pub fn cols(&self) -> Vec<ColInfo> {
        let SqlRow::My(r) = self;
        r.columns().iter().map(|c| ColInfo { name: c.name().to_string(), ty: c.type_info().name().to_string() }).collect()
    }
}

impl Cells for SqlRow {
    fn cell(&self, i: usize) -> Option<&[u8]> {
        let SqlRow::My(r) = self;
        r.cell(i)
    }

    fn cell_named(&self, name: &str) -> Option<&[u8]> {
        let SqlRow::My(r) = self;
        r.cell_named(name)
    }

    fn col_pos(&self, name: &str) -> Option<usize> {
        let SqlRow::My(r) = self;
        r.col_pos(name)
    }

    fn int(&self, name: &str) -> Option<i64> {
        let SqlRow::My(r) = self;
        r.int(name)
    }

    fn uint(&self, name: &str) -> Option<u64> {
        let SqlRow::My(r) = self;
        r.uint(name)
    }
}

/// How a connection is opened, kept so a connection of its own can be made beside the pool.
#[derive(Clone)]
pub enum Opts {
    My(MySqlConnectOptions),
}

/// Lets the MySQL-only modules keep reading their options as sqlx's own type. It goes away with the first
/// engine that has no such options, which is when those modules start asking for the MySQL arm explicitly.
impl std::ops::Deref for Opts {
    type Target = MySqlConnectOptions;

    fn deref(&self) -> &MySqlConnectOptions {
        let Opts::My(o) = self;
        o
    }
}

/// A connection of its own, outside any pool.
pub enum Conn {
    My(sqlx::MySqlConnection),
}

impl Conn {
    pub async fn connect(opts: &Opts) -> Result<Conn, sqlx::Error> {
        let Opts::My(o) = opts;
        sqlx::MySqlConnection::connect_with(o).await.map(Conn::My)
    }

    /// The MySQL connection, for the modules that only exist for MySQL.
    pub fn my(&mut self) -> Res<&mut sqlx::MySqlConnection> {
        let Conn::My(c) = self;
        Ok(c)
    }

    pub fn into_my(self) -> Res<sqlx::MySqlConnection> {
        let Conn::My(c) = self;
        Ok(c)
    }

    /// Runs `sql` through the text protocol and streams what comes back, so values arrive as the server
    /// wrote them. A CALL ends several results, each with an `Item::End`.
    pub fn fetch_text<'c>(&'c mut self, sql: &'c str) -> BoxStream<'c, Result<Item, sqlx::Error>> {
        let Conn::My(c) = self;
        raw_sql(sql)
            .fetch_many(&mut *c)
            .map(|item| {
                item.map(|x| match x {
                    Either::Right(row) => Item::Row(SqlRow::My(row)),
                    Either::Left(r) => Item::End(Done { affected: r.rows_affected(), last_id: r.last_insert_id() }),
                })
            })
            .boxed()
    }

    /// Runs `sql` through the text protocol and collects its rows.
    pub async fn fetch_all(&mut self, sql: &str) -> Result<Vec<SqlRow>, sqlx::Error> {
        let Conn::My(c) = self;
        c.fetch_all(sql).await.map(SqlRow::wrap)
    }

    /// Runs one statement and drops its rows.
    pub async fn execute(&mut self, sql: &str) -> Result<Done, sqlx::Error> {
        let Conn::My(c) = self;
        c.execute(sql).await.map(|r| Done { affected: r.rows_affected(), last_id: r.last_insert_id() })
    }

    /// The columns a statement would return, without running it. Empty for a write.
    pub async fn describe(&mut self, sql: &str) -> Result<Vec<ColInfo>, sqlx::Error> {
        let Conn::My(c) = self;
        let d = c.describe(sql).await?;
        Ok(d.columns().iter().map(|c| ColInfo { name: c.name().to_string(), ty: c.type_info().name().to_string() }).collect())
    }

    /// Starts a transaction that rolls back unless it is committed.
    pub async fn begin(&mut self) -> Result<Tx<'_>, sqlx::Error> {
        let Conn::My(c) = self;
        c.begin().await.map(Tx::My)
    }

    pub async fn close(self) -> Result<(), sqlx::Error> {
        let Conn::My(c) = self;
        c.close().await
    }
}

/// A transaction on a `Conn`.
pub enum Tx<'c> {
    My(sqlx::Transaction<'c, sqlx::MySql>),
}

impl Tx<'_> {
    /// Runs one statement with its values bound, so none of them is ever spliced into the text.
    pub async fn exec_bound(&mut self, sql: &str, params: &[Param]) -> Result<Done, sqlx::Error> {
        let Tx::My(t) = self;
        bound(sql, params).execute(&mut **t).await.map(|r| Done { affected: r.rows_affected(), last_id: r.last_insert_id() })
    }

    pub async fn commit(self) -> Result<(), sqlx::Error> {
        let Tx::My(t) = self;
        t.commit().await
    }
}

type Query<'q> = sqlx::query::Query<'q, sqlx::MySql, sqlx::mysql::MySqlArguments>;

pub(super) fn bound<'q>(sql: &'q str, params: &'q [Param]) -> Query<'q> {
    let mut q = sqlx::query(sql);
    for p in params {
        q = match p {
            Param::Null => q.bind(None::<String>),
            Param::Int(i) => q.bind(*i),
            Param::UInt(u) => q.bind(*u),
            Param::Float(f) => q.bind(*f),
            Param::Text(s) => q.bind(s.as_str()),
            Param::Bytes(b) => q.bind(b.as_slice()),
        };
    }
    q
}

impl Db {
    /// A connection of its own, outside the pool, so a busy or exhausted pool never blocks it.
    pub async fn detached(&self) -> Res<Conn> {
        Conn::connect(&self.opts).await.map_err(e)
    }

    /// The MySQL pool, for the modules that only exist for MySQL.
    pub fn my_pool(&self) -> Res<&sqlx::MySqlPool> {
        let Pool::My(p) = &self.pool;
        Ok(p)
    }
}
