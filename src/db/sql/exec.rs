//! The line between the SQL code and sqlx's engine types. Every enum has one arm per engine, so adding an
//! engine is a new arm and the compiler lists what still needs one. Generics over `sqlx::Database` were
//! ruled out: they put higher-ranked bounds in every signature, and the engines still differ in how a
//! database is selected, how a statement is stopped and how an insert reports its key.
//!
//! Modules that only make sense on one engine (DDL, users, dump and so on) keep sqlx's own types and reach
//! them through `Db::my_pool` and `Conn::my`, which answer with an error when the connection is another engine.

use super::se;
use super::value::{Kind, Param};
use super::{Db, Dialect, Pool};
use crate::db::Res;
use futures_util::stream::BoxStream;
use futures_util::StreamExt;
use sqlx::mysql::{MySqlColumn, MySqlConnectOptions, MySqlRow};
use sqlx::postgres::{PgColumn, PgConnectOptions, PgRow};
use sqlx::{raw_sql, Column, Connection, Either, Executor, Row as _, TypeInfo, ValueRef};

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
    /// The engine it came from, whose type names differ. `MySql` stands for the whole MySQL family.
    pub dialect: Dialect,
}

/// What `make_cols` needs of a result column, so rows read through sqlx directly (the MySQL-only modules) and
/// rows of a `SqlRow` share one reader.
pub trait ColMeta {
    fn col_name(&self) -> &str;
    fn col_type(&self) -> String;

    /// The kind of a result column that is not a table column. MySQL names are read as MySQL's, Postgres ones as its own.
    fn col_kind(&self) -> Kind {
        Kind::from_type_name(&self.col_type())
    }
}

impl ColMeta for ColInfo {
    fn col_name(&self) -> &str {
        &self.name
    }

    fn col_type(&self) -> String {
        self.ty.clone()
    }

    fn col_kind(&self) -> Kind {
        if self.dialect == Dialect::Postgres { Kind::from_pg(&self.ty, "", "") } else { Kind::from_type_name(&self.ty) }
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

impl ColMeta for PgColumn {
    fn col_name(&self) -> &str {
        self.name()
    }

    fn col_type(&self) -> String {
        self.type_info().name().to_string()
    }

    fn col_kind(&self) -> Kind {
        Kind::from_pg(self.type_info().name(), "", "")
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

    fn dialect(&self) -> Dialect {
        (**self).dialect()
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

    /// The engine the row is from, whose text differs for a few types (PostgreSQL's bytea is hex text). A MySQL
    /// row cannot tell MariaDB apart and says `MySql` for both.
    fn dialect(&self) -> Dialect {
        Dialect::MySql
    }
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

impl Cells for PgRow {
    /// Through the raw value, because decoding a text-protocol bytea to `&[u8]` is refused by sqlx. A text row
    /// holds the server's own text and a binary row of a `::text` column holds the same bytes.
    fn cell(&self, i: usize) -> Option<&[u8]> {
        let v = self.try_get_raw(i).ok()?;
        if v.is_null() { None } else { v.as_bytes().ok() }
    }

    fn cell_named(&self, name: &str) -> Option<&[u8]> {
        let v = self.try_get_raw(name).ok()?;
        if v.is_null() { None } else { v.as_bytes().ok() }
    }

    fn col_pos(&self, name: &str) -> Option<usize> {
        self.columns().iter().position(|c| c.name() == name)
    }

    fn int(&self, name: &str) -> Option<i64> {
        let v = self.try_get_raw(name).ok()?;
        if v.is_null() {
            return None;
        }
        if matches!(v.format(), sqlx::postgres::PgValueFormat::Text) {
            return std::str::from_utf8(v.as_bytes().ok()?).ok()?.trim().parse().ok();
        }
        // Binary rows carry the column's own width.
        self.try_get::<i64, _>(name).ok().or_else(|| self.try_get::<i32, _>(name).ok().map(i64::from)).or_else(|| self.try_get::<i16, _>(name).ok().map(i64::from))
    }

    fn uint(&self, name: &str) -> Option<u64> {
        self.int(name).and_then(|n| u64::try_from(n).ok())
    }

    fn dialect(&self) -> Dialect {
        Dialect::Postgres
    }
}

/// A row of either engine. Cells are borrowed from the row, never copied.
pub enum SqlRow {
    My(MySqlRow),
    Pg(PgRow),
}

pub(super) fn col_infos<C: Column>(cols: &[C], dialect: Dialect) -> Vec<ColInfo> {
    cols.iter().map(|c| ColInfo { name: c.name().to_string(), ty: c.type_info().name().to_string(), dialect }).collect()
}

impl SqlRow {
    pub fn wrap(rows: Vec<MySqlRow>) -> Vec<SqlRow> {
        rows.into_iter().map(SqlRow::My).collect()
    }

    pub fn wrap_pg(rows: Vec<PgRow>) -> Vec<SqlRow> {
        rows.into_iter().map(SqlRow::Pg).collect()
    }

    pub fn cols(&self) -> Vec<ColInfo> {
        match self {
            SqlRow::My(r) => col_infos(r.columns(), Dialect::MySql),
            SqlRow::Pg(r) => col_infos(r.columns(), Dialect::Postgres),
        }
    }
}

impl Cells for SqlRow {
    fn cell(&self, i: usize) -> Option<&[u8]> {
        match self {
            SqlRow::My(r) => r.cell(i),
            SqlRow::Pg(r) => r.cell(i),
        }
    }

    fn cell_named(&self, name: &str) -> Option<&[u8]> {
        match self {
            SqlRow::My(r) => r.cell_named(name),
            SqlRow::Pg(r) => r.cell_named(name),
        }
    }

    fn col_pos(&self, name: &str) -> Option<usize> {
        match self {
            SqlRow::My(r) => r.col_pos(name),
            SqlRow::Pg(r) => r.col_pos(name),
        }
    }

    fn int(&self, name: &str) -> Option<i64> {
        match self {
            SqlRow::My(r) => r.int(name),
            SqlRow::Pg(r) => r.int(name),
        }
    }

    fn uint(&self, name: &str) -> Option<u64> {
        match self {
            SqlRow::My(r) => r.uint(name),
            SqlRow::Pg(r) => r.uint(name),
        }
    }

    fn dialect(&self) -> Dialect {
        match self {
            SqlRow::My(_) => Dialect::MySql,
            SqlRow::Pg(_) => Dialect::Postgres,
        }
    }
}

/// How a connection is opened, kept so a connection of its own can be made beside the pool.
#[derive(Clone)]
pub enum Opts {
    My(MySqlConnectOptions),
    Pg(PgConnectOptions),
}

impl Opts {
    /// The MySQL options, which only the live tests read.
    #[cfg(test)]
    pub fn my(&self) -> Res<&MySqlConnectOptions> {
        match self {
            Opts::My(o) => Ok(o),
            Opts::Pg(_) => Err(super::pg::NOT_YET.into()),
        }
    }

    /// The database the options connect to, when they name one.
    pub fn database(&self) -> Option<&str> {
        match self {
            Opts::My(o) => o.get_database(),
            Opts::Pg(o) => o.get_database(),
        }
    }
}

/// A connection of its own, outside any pool.
pub enum Conn {
    My(sqlx::MySqlConnection),
    Pg(super::pg::PgConn),
}

fn done_my(r: sqlx::mysql::MySqlQueryResult) -> Done {
    Done { affected: r.rows_affected(), last_id: r.last_insert_id() }
}

/// Postgres reports no generated key in the result, so inserts ask for it with RETURNING.
fn done_pg(r: sqlx::postgres::PgQueryResult) -> Done {
    Done { affected: r.rows_affected(), last_id: 0 }
}

impl Conn {
    pub async fn connect(opts: &Opts) -> Result<Conn, sqlx::Error> {
        match opts {
            Opts::My(o) => sqlx::MySqlConnection::connect_with(o).await.map(Conn::My),
            Opts::Pg(o) => sqlx::PgConnection::connect_with(o).await.map(|c| Conn::Pg(super::pg::PgConn::free(c))),
        }
    }

    /// The MySQL connection, for the modules that only exist for MySQL.
    pub fn my(&mut self) -> Res<&mut sqlx::MySqlConnection> {
        match self {
            Conn::My(c) => Ok(c),
            Conn::Pg(_) => Err(super::pg::NOT_YET.into()),
        }
    }

    pub fn into_my(self) -> Res<sqlx::MySqlConnection> {
        match self {
            Conn::My(c) => Ok(c),
            Conn::Pg(_) => Err(super::pg::NOT_YET.into()),
        }
    }

    /// Runs `sql` through the text protocol and streams what comes back, so values arrive as the server
    /// wrote them. A CALL ends several results, each with an `Item::End`.
    pub fn fetch_text<'c>(&'c mut self, sql: &'c str) -> BoxStream<'c, Result<Item, sqlx::Error>> {
        match self {
            Conn::My(c) => raw_sql(sql)
                .fetch_many(&mut *c)
                .map(|item| {
                    item.map(|x| match x {
                        Either::Right(row) => Item::Row(SqlRow::My(row)),
                        Either::Left(r) => Item::End(done_my(r)),
                    })
                })
                .boxed(),
            Conn::Pg(c) => raw_sql(sql)
                .fetch_many(&mut **c)
                .map(|item| {
                    item.map(|x| match x {
                        Either::Right(row) => Item::Row(SqlRow::Pg(row)),
                        Either::Left(r) => Item::End(done_pg(r)),
                    })
                })
                .boxed(),
        }
    }

    /// Runs `sql` through the text protocol and collects its rows.
    pub async fn fetch_all(&mut self, sql: &str) -> Result<Vec<SqlRow>, sqlx::Error> {
        match self {
            Conn::My(c) => c.fetch_all(sql).await.map(SqlRow::wrap),
            Conn::Pg(c) => c.fetch_all(sql).await.map(SqlRow::wrap_pg),
        }
    }

    /// Runs one statement and drops its rows.
    pub async fn execute(&mut self, sql: &str) -> Result<Done, sqlx::Error> {
        match self {
            Conn::My(c) => c.execute(sql).await.map(done_my),
            Conn::Pg(c) => c.execute(sql).await.map(done_pg),
        }
    }

    /// The columns a statement would return, without running it. Empty for a write.
    pub async fn describe(&mut self, sql: &str) -> Result<Vec<ColInfo>, sqlx::Error> {
        match self {
            Conn::My(c) => Ok(col_infos(c.describe(sql).await?.columns(), Dialect::MySql)),
            Conn::Pg(c) => {
                let d = c.describe(sql).await.map(|d| col_infos(d.columns(), Dialect::Postgres));
                // sqlx describes through a named statement it caches, which behind a pooler would stay on a backend other
                // clients share. This runs inside the statement's own transaction, so the backend is the same one.
                let _ = c.clear_cached_statements().await;
                Ok(d?)
            }
        }
    }

    /// Starts the transaction a write runs in. On PostgreSQL the session stays read-only, so the write opens its own
    /// READ WRITE transaction and lifts the statement timeout and sets a lock timeout for it alone with SET LOCAL, which
    /// ends with the transaction and so never reaches another client of a connection pooler.
    pub async fn begin_write(&mut self) -> Result<Tx<'_>, sqlx::Error> {
        match self {
            Conn::My(c) => c.begin().await.map(Tx::My),
            Conn::Pg(c) => {
                let tx = c.begin_with(format!("BEGIN READ WRITE; {}", super::pg::write_locals())).await?;
                Ok(Tx::Pg(tx))
            }
        }
    }

    pub async fn close(self) -> Result<(), sqlx::Error> {
        match self {
            Conn::My(c) => c.close().await,
            Conn::Pg(c) => c.close().await,
        }
    }
}

/// A transaction on a `Conn`.
pub enum Tx<'c> {
    My(sqlx::Transaction<'c, sqlx::MySql>),
    Pg(sqlx::Transaction<'c, sqlx::Postgres>),
}

impl Tx<'_> {
    /// Runs one statement with its values bound, so none of them is ever spliced into the text.
    pub async fn exec_bound(&mut self, sql: &str, params: &[Param]) -> Result<Done, sqlx::Error> {
        match self {
            Tx::My(t) => bound(sql, params).execute(&mut **t).await.map(done_my),
            Tx::Pg(t) => bound_pg(sql, params).execute(&mut **t).await.map(done_pg),
        }
    }

    /// Like `exec_bound`, for a statement that returns rows (an INSERT with RETURNING). A PostgreSQL row is binary, so
    /// the columns the caller reads must be cast to text in the statement.
    pub async fn fetch_bound(&mut self, sql: &str, params: &[Param]) -> Result<Vec<SqlRow>, sqlx::Error> {
        match self {
            Tx::My(t) => bound(sql, params).fetch_all(&mut **t).await.map(SqlRow::wrap),
            Tx::Pg(t) => bound_pg(sql, params).fetch_all(&mut **t).await.map(SqlRow::wrap_pg),
        }
    }

    pub async fn commit(self) -> Result<(), sqlx::Error> {
        match self {
            Tx::My(t) => t.commit().await,
            Tx::Pg(t) => t.commit().await,
        }
    }

    pub async fn rollback(self) -> Result<(), sqlx::Error> {
        match self {
            Tx::My(t) => t.rollback().await,
            Tx::Pg(t) => t.rollback().await,
        }
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

type PgQuery<'q> = sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>;

/// Postgres has no unsigned integers, so a value above i64::MAX is sent as text and left to the server's cast.
///
/// Every bound PostgreSQL statement is unnamed (`persistent(false)`). A named statement is cached per connection by sqlx
/// and belongs to one backend, and behind transaction pooling the next transaction can run on another backend, which then
/// answers `prepared statement "sqlx_s_1" does not exist` or `already exists` (another client may own that name).
/// `statement_cache_capacity(0)` would not help: sqlx then still names the statement and never closes it. The cost in
/// normal mode is one Parse per call, which is nothing next to the round trip.
pub(super) fn bound_pg<'q>(sql: &'q str, params: &'q [Param]) -> PgQuery<'q> {
    let mut q = sqlx::query(sql).persistent(false);
    for p in params {
        q = match p {
            Param::Null => q.bind(None::<String>),
            Param::Int(i) => q.bind(*i),
            Param::UInt(u) => match i64::try_from(*u) {
                Ok(i) => q.bind(i),
                Err(_) => q.bind(u.to_string()),
            },
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
        match &self.pool {
            Pool::Pg(p) => p.detached(&self.opts).await,
            Pool::My(_) => Conn::connect(&self.opts).await.map_err(se),
        }
    }

    /// A metadata query with its values bound as text, never spliced into it, on either engine (the SQL text spells its
    /// placeholders as the engine wants: `?` or `$1`). Every row is read whole, and a Postgres result is a binary row, so
    /// the columns are cast to text in the query. The statement goes in the query log under `schema`.
    pub(super) fn meta<'a, const N: usize>(&'a self, schema: &'a str, sql: &str, params: [&str; N]) -> impl std::future::Future<Output = Res<Vec<SqlRow>>> + Send + use<'a, N> {
        self.meta_with(schema, sql, params, false)
    }

    /// `meta`, and on PostgreSQL with `schema` as the search_path of the read when `in_schema` is set. The catalog prints a
    /// type, function or sequence unqualified only when it is on the path, so a read made under the path the statements
    /// will run under (see `ddl_pg`) is text that resolves the same way when it is written back.
    pub(super) fn meta_with<'a, const N: usize>(&'a self, schema: &'a str, sql: &str, params: [&str; N], in_schema: bool) -> impl std::future::Future<Output = Res<Vec<SqlRow>>> + Send + use<'a, N> {
        let path = if in_schema && self.dialect == Dialect::Postgres { format!("SET LOCAL search_path = {}", self.dialect.quote_ident(schema)) } else { String::new() };
        // The values are taken by value and copied before the first await. A borrowed `&[a, b]` would live across the
        // caller's await and the compiler then cannot prove the caller's future Send once it is spawned.
        let full = format!("{sql}{}", self.dialect.all_rows());
        let sql = sql.to_string();
        let params: Vec<String> = params.iter().map(|p| p.to_string()).collect();
        async move {
            let t = std::time::Instant::now();
            let res = match &self.pool {
                Pool::My(p) => {
                    let mut q = sqlx::query(&full);
                    for x in params {
                        q = q.bind(x);
                    }
                    q.fetch_all(p).await.map(SqlRow::wrap)
                }
                Pool::Pg(pp) => {
                    let pool = self.pg_pool()?;
                    let mut q = sqlx::query(&full).persistent(false);
                    for x in params {
                        q = q.bind(x);
                    }
                    if pp.locals().is_empty() && path.is_empty() {
                        q.fetch_all(&pool).await.map(SqlRow::wrap_pg)
                    } else {
                        // Behind a pooler the session carries no settings, so a read sets them for its own transaction.
                        async {
                            let mut conn = pool.acquire().await?;
                            let r = super::pg_read::in_read_only(&mut conn, self, &path, async move |tx| q.fetch_all(&mut *tx).await).await;
                            if r.is_err() {
                                conn.close_on_drop();
                            }
                            r
                        }
                        .await
                        .map(SqlRow::wrap_pg)
                    }
                }
            };
            self.note(schema, &sql, t, &res);
            res.map_err(se)
        }
    }

    /// The gate at the entry of every worker call of a module that only exists for MySQL and MariaDB (accounts, the
    /// server window), so nothing below it has to be written for PostgreSQL.
    pub(super) fn mysql_only(&self) -> Res<()> {
        if self.dialect.is_mysql_family() { Ok(()) } else { Err(super::pg::NOT_YET.into()) }
    }

    /// The MySQL pool, for the modules that only exist for MySQL.
    pub fn my_pool(&self) -> Res<&sqlx::MySqlPool> {
        match &self.pool {
            Pool::My(p) => Ok(p),
            Pool::Pg(_) => Err(super::pg::NOT_YET.into()),
        }
    }
}
