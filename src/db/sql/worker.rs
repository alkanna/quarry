//! The SQL half of the worker: connecting, listing tables, paging a table and running Custom Query
//! scripts. Rows leave here as BSON like Mongo's, see `Row`.


use super::se;
use super::value::{self, Kind};
// What the modules that only exist for MySQL call, with sqlx's own connection type. The neutral ones are in `run`.
pub(super) use super::run::pin_reads_my as pin_reads;
use super::run::{run, RunHandle};
use super::schema::{Schema, SchemaCol, SchemaTable};
use super::{lex, Addr, Advanced, TestInfo, Col, Db, Dialect, PageQuery, Pool, SslMode};
use crate::db::{col_op_result, e, ColOp, ColOut, Msg, Net, Res, Row, Worker, COL_VALUES_CAP};
use mongodb::bson::{Bson, Document};
use super::exec::{Cells, ColInfo, ColMeta, Conn, Opts, SqlRow};
use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions, MySqlSslMode};
use sqlx::{raw_sql, Connection, Executor, Row as _};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Below this many rows the estimate in information_schema is too loose to show, and counting is cheap.
const ESTIMATE_MIN: u64 = 10_000;

/// A cell of a text-protocol row as the server's own bytes. Metadata queries read strings through this
/// too, because a `_bin` collation makes sqlx report text as VARBINARY.
pub(super) fn raw<R: Cells>(row: &R, i: usize) -> Option<&[u8]> {
    row.cell(i)
}

/// Column `i` of a text-protocol row as text.
pub(super) fn text_at<R: Cells>(row: &R, i: usize) -> Option<String> {
    raw(row, i).map(|b| String::from_utf8_lossy(b).into_owned())
}

/// Column `i` of a text-protocol row as a number, for the counts and ids the server sends as text.
pub(super) fn num_at<R: Cells>(row: &R, i: usize) -> Option<u64> {
    raw(row, i).and_then(|b| std::str::from_utf8(b).ok()?.parse().ok())
}

pub(super) fn text<R: Cells>(row: &R, name: &str) -> Option<String> {
    row.cell_named(name).map(|b| String::from_utf8_lossy(b).into_owned())
}

fn ssl_mode(m: SslMode) -> MySqlSslMode {
    match m {
        SslMode::Disabled => MySqlSslMode::Disabled,
        SslMode::Preferred => MySqlSslMode::Preferred,
        SslMode::Required => MySqlSslMode::Required,
        SslMode::VerifyCa => MySqlSslMode::VerifyCa,
        SslMode::VerifyIdentity => MySqlSslMode::VerifyIdentity,
    }
}

/// What every pool connection gets: read-only, the settings of the Advanced tab, the statement timeout and the
/// init command, in that order. The init command goes last so it can refine what came before, and after the
/// guard so it cannot run as a writer.
pub(super) async fn session(c: &mut sqlx::MySqlConnection, ms: u64, adv: &Setup) -> Result<(), sqlx::Error> {
    // The dialect is not known yet, so each connection asks for it to pick the session statements.
    let v: String = sqlx::query_scalar("SELECT VERSION()").fetch_one(&mut *c).await?;
    let d = Dialect::from_version(&v);
    c.execute(d.session_read_only(true)).await?;
    // A server whose global autocommit is off would keep every pooled read in one old snapshot holding metadata
    // locks, so each session starts with it on whatever the server says.
    c.execute("SET SESSION autocommit = 1").await?;
    // A refused setting fails the connect with the server's words, since the user asked for it.
    for q in &adv.statements {
        if let Err(err) = c.execute(q.as_str()).await {
            return Err(adv.failed(format!("{q}: {}", e(err))));
        }
    }
    // Old servers and proxies may lack the variable; a missing timeout is better than no connection, but a
    // missing read-only guard is not, so only that one fails the connect.
    let t = d.session_timeout(ms);
    let _ = c.execute(t.as_str()).await;
    if adv.init.is_empty() {
        return Ok(());
    }
    // Prepared, so the server parses each one as exactly one statement whatever its quotes hide.
    for q in &adv.init {
        if let Err(err) = sqlx::query(q).persistent(false).execute(&mut *c).await {
            return Err(adv.failed(format!("Init command failed on {q:?}: {}", e(err))));
        }
    }
    // The init command was checked before it ran, but the guard is what keeps writes out, so it is set again and
    // read back. Autocommit is forced on because an unlocked write on a session without it reports success and
    // is rolled back when the connection closes.
    c.execute(d.session_read_only(true)).await?;
    c.execute("SET SESSION autocommit = 1").await?;
    // MariaDB before 11.1 knows the flag only as tx_read_only.
    let ro = match c.fetch_one("SELECT @@SESSION.transaction_read_only, @@SESSION.autocommit").await {
        Ok(r) => r,
        Err(_) => c.fetch_one("SELECT @@SESSION.tx_read_only, @@SESSION.autocommit").await?,
    };
    if raw(&ro, 0) != Some(b"1") || raw(&ro, 1) != Some(b"1") {
        return Err(adv.failed("The session was not read-only with autocommit after the init command, so the connection was refused".into()));
    }
    Ok(())
}

/// The settings of the Advanced tab as a connection applies them. A pool wraps what a connection hook failed
/// with in text of its own and retries until its timeout, so the message is kept on the side to report instead.
#[derive(Clone, Default)]
pub struct Setup {
    pub(super) statements: Vec<String>,
    pub(super) init: Vec<String>,
    failure: Arc<std::sync::Mutex<Option<String>>>,
}

impl Setup {
    pub(super) fn new(statements: Vec<String>, init: Vec<String>) -> Setup {
        Setup { statements, init, failure: Default::default() }
    }

    fn failed(&self, msg: String) -> sqlx::Error {
        *self.failure.lock().unwrap() = Some(msg.clone());
        sqlx::Error::Protocol(msg)
    }

    /// What the last failing setup said, if a connection of this pool failed one.
    pub(super) fn failed_text(&self) -> Option<String> {
        self.failure.lock().unwrap().clone()
    }
}

impl Db {
    /// A pooled connection taken out of the pool for good, so its session can be changed.
    pub(super) async fn acquire_conn(&self) -> Res<Conn> {
        match &self.pool {
            Pool::Pg(p) => Ok(Conn::Pg(p.acquire_held(self.opts.database()).await?)),
            Pool::My(_) => Ok(Conn::My(self.acquire().await?.detach())),
        }
    }

    /// A pooled connection. When the pool gave up because new connections failed their session setup, the error
    /// names the setting that failed instead of a bare timeout.
    pub(super) async fn acquire(&self) -> Res<sqlx::pool::PoolConnection<sqlx::MySql>> {
        let p = self.my_pool()?;
        p.acquire().await.map_err(|x| match x {
            sqlx::Error::PoolTimedOut => self.setup.failed_text().unwrap_or_else(|| e(x)),
            x => e(x),
        })
    }
}

async fn open(uri: &str, timeout: Duration, net: &Net, adv: &Advanced) -> Res<(Db, Vec<String>, Option<crate::db::Way>)> {
    open_full(uri, timeout, net, adv, false).await.map(|(db, dbs, t, _)| (db, dbs, t))
}

/// `info` also reads what the form's Test button shows: the server's default sql_mode and its UTF-8 collations.
async fn open_full(uri: &str, timeout: Duration, net: &Net, adv: &Advanced, info: bool) -> Res<(Db, Vec<String>, Option<crate::db::Way>, TestInfo)> {
    // Checked before anything connects, so a bad value never costs a tunnel or a login.
    let a = Addr::parse(uri).ok_or("Not a SQL address")?;
    if a.engine() == super::Engine::Postgres {
        return super::pg::open(a, timeout, net, adv, info).await;
    }
    if net.cloud_sql.is_some() {
        return Err("Cloud SQL connections are for PostgreSQL only".into());
    }
    let setup = Setup::new(adv.session_statements(super::advanced::local_offset_secs())?, super::advanced::init_statements(&adv.init)?);
    let (timeout, ms) = adv.timeouts(timeout);
    let tunnel = match &net.ssh {
        // The socket only matters when sqlx checks the certificate's name.
        Some((conf, secrets)) => Some(crate::ssh::open(conf, secrets, timeout, Some((a.host.clone(), a.port)), matches!(a.ssl_mode, SslMode::VerifyCa | SslMode::VerifyIdentity)).await?),
        None => None,
    };
    // sqlx checks the certificate against the host it is given. Through the tunnel's socket that stays the name typed;
    // Windows has only the tunnel's port, so there the name checked is 127.0.0.1.
    let (host, port) = match &tunnel {
        Some(t) if t.socket.is_none() => ("127.0.0.1".to_string(), t.port),
        _ => (a.host.clone(), a.port),
    };
    // The sqlx defaults change how the server reads user SQL (|| as concat, UTC), so they are turned off.
    let mut o = MySqlConnectOptions::new().host(&host).port(port).username(&a.user).ssl_mode(ssl_mode(a.ssl_mode)).pipes_as_concat(false).no_engine_substitution(false).timezone(None);
    if let Some(s) = tunnel.as_ref().and_then(|t| t.socket.as_ref()) {
        o = o.socket(s);
    }
    if !a.password.is_empty() {
        o = o.password(&a.password);
    }
    if !a.database.is_empty() {
        o = o.database(&a.database);
    }
    if !a.ssl_ca.is_empty() {
        o = o.ssl_ca(&a.ssl_ca);
    }
    // The pool retries a connection whose setup failed until its timeout, so one plain connection runs the setup
    // first and a failing setting or init command is reported at once, with its statement.
    let first = async {
        let mut c = sqlx::MySqlConnection::connect_with(&o).await.map_err(|x| crate::db::mysql_name_note(se(x), a.ssl_mode == SslMode::VerifyCa))?;
        let r = session(&mut c, ms, &setup).await;
        let _ = c.close().await;
        r.map_err(|err| setup.failed_text().unwrap_or_else(|| e(err)))
    };
    tokio::time::timeout(timeout, first).await.map_err(|_| format!("Could not connect and set up the session within {} s", timeout.as_secs()))??;
    let mut pool_opts = MySqlPoolOptions::new().max_connections(4).acquire_timeout(timeout);
    if let Some(idle) = adv.idle_timeout() {
        pool_opts = pool_opts.idle_timeout(idle);
    }
    let hook = setup.clone();
    let pool = pool_opts.after_connect(move |c, _| {
        let hook = hook.clone();
        Box::pin(async move { session(c, ms, &hook).await })
    }).connect_with(o.clone()).await.map_err(|err| setup.failed_text().unwrap_or_else(|| e(err)))?;
    let version: String = sqlx::query_scalar("SELECT VERSION()").fetch_one(&pool).await.map_err(se)?;
    // Read apart from VERSION so a proxy that refuses @@sql_mode still connects, with the default mode.
    let sql_mode: String = sqlx::query_scalar("SELECT @@sql_mode").fetch_one(&pool).await.unwrap_or_default();
    let tls = raw_sql("SHOW SESSION STATUS LIKE 'Ssl_cipher'").fetch_all(&pool).await.map_err(se)?.iter().any(|r| raw(r, 1).is_some_and(|v| !v.is_empty()));
    let dbs = sorted_names(&raw_sql("SHOW DATABASES").fetch_all(&pool).await.map_err(se)?);
    let mut ti = TestInfo::default();
    if info {
        ti.sql_mode = sqlx::query_scalar("SELECT @@GLOBAL.sql_mode").fetch_one(&pool).await.unwrap_or_default();
        let rows = raw_sql("SELECT CHARACTER_SET_NAME, COLLATION_NAME FROM information_schema.COLLATIONS WHERE CHARACTER_SET_NAME IN ('utf8mb4', 'utf8mb3') ORDER BY COLLATION_NAME").fetch_all(&pool).await.unwrap_or_default();
        ti.collations = rows.iter().filter_map(|r| Some((String::from_utf8_lossy(raw(r, 0)?).into_owned(), String::from_utf8_lossy(raw(r, 1)?).into_owned()))).collect();
    }
    let db = Db { dialect: Dialect::from_version(&version), pool: Pool::My(pool), opts: Opts::My(o), version, tls, sql_mode, setup, conn: 0 };
    Ok((db, dbs, tunnel.map(crate::db::Way::Ssh), ti))
}

/// The databases the login can see, sorted. Text protocol, because SHOW is not preparable on every old server.
/// `fetch_all` on the connection rather than `raw_sql`, because inside a spawned block the executor lifetimes of a
/// borrowed connection do not infer for the latter.
pub(super) async fn database_names(c: &mut sqlx::MySqlConnection) -> Res<Vec<String>> {
    Ok(sorted_names(&c.fetch_all("SHOW DATABASES").await.map_err(se)?))
}

/// The first column of each row, sorted. `open` reads through the pool, where a generic executor does not infer.
fn sorted_names<R: Cells>(rows: &[R]) -> Vec<String> {
    let mut names: Vec<String> = rows.iter().filter_map(|r| text_at(r, 0)).collect();
    names.sort();
    names
}

/// A table kind from information_schema's COLUMN_TYPE ("varchar(20)", "bigint unsigned"), which stays
/// right for `_bin` text columns that sqlx reports as binary.
fn kind_of_full(full: &str) -> Kind {
    Kind::from_column_type(full)
}

/// Names must be unique because a row is a BSON document keyed by them. `reserve_id` keeps a real
/// column from taking the `_id` key that carries the primary key.
fn unique(cols: &mut [Col], reserve_id: bool) {
    let mut seen = HashSet::new();
    if reserve_id {
        seen.insert("_id".to_string());
    }
    for c in cols {
        let base = c.name.clone();
        let mut n = 1;
        while !seen.insert(c.name.clone()) {
            n += 1;
            c.name = format!("{base}_{n}");
        }
    }
}

/// Columns as the result reports them. `meta` (name to COLUMN_TYPE and nullability) wins when the
/// result column is a table column.
pub(super) fn make_cols<C: ColMeta>(cols: &[C], meta: &HashMap<String, (String, bool, String)>, reserve_id: bool) -> Vec<Col> {
    make_cols_kinds(cols, meta, &HashMap::new(), reserve_id)
}

/// `make_cols` with the kinds the server's catalog gave the table's columns. PostgreSQL's type names say too little
/// (a domain, an enum), so its kinds are read from `pg_type` and a MySQL table leaves `kinds` empty.
pub(super) fn make_cols_kinds<C: ColMeta>(cols: &[C], meta: &HashMap<String, (String, bool, String)>, kinds: &HashMap<String, Kind>, reserve_id: bool) -> Vec<Col> {
    let mut out: Vec<Col> = cols
        .iter()
        .map(|c| {
            let ty = c.col_type();
            match meta.get(c.col_name()) {
                Some((full, nullable, extra)) => {
                    let extra = extra.to_ascii_uppercase();
                    let kind = kinds.get(c.col_name()).copied().unwrap_or_else(|| kind_of_full(full));
                    Col { generated: extra.contains("GENERATED"), auto_increment: extra.contains("AUTO_INCREMENT"), ..Col::plain(c.col_name(), full, kind, *nullable) }
                }
                None => Col::plain(c.col_name(), &ty, c.col_kind(), true),
            }
        })
        .collect();
    unique(&mut out, reserve_id);
    out
}

pub(super) fn to_row<R: Cells>(cols: &[Col], row: &R, pk: &[String]) -> Res<Row> {
    let vals: Vec<Bson> = cols.iter().enumerate().map(|(i, c)| value::to_bson(row.dialect(), c.kind, raw(row, i))).collect();
    let mut d = Document::new();
    // The key is looked up by the server's column names, since a renamed column keeps its real name there.
    // A key with a missing column would address too many rows, so then there is no key at all.
    let idx: Option<Vec<usize>> = pk.iter().map(|p| row.col_pos(p)).collect();
    if let Some(idx) = idx.filter(|i| !i.is_empty()) {
        let mut k = Document::new();
        for (p, i) in pk.iter().zip(idx) {
            k.insert(p.clone(), vals[i].clone());
        }
        d.insert("_id", k);
    }
    for (c, v) in cols.iter().zip(vals) {
        d.insert(c.name.clone(), v);
    }
    Row::from_doc(&d)
}

/// Text the user typed is spliced into a SELECT, so it must stay one read-only statement.
pub(super) fn one_read(sql: &str, d: Dialect) -> Res<()> {
    if lex::split_statements(sql, d).len() != 1 {
        return Err("The filter and sort must not contain another statement".into());
    }
    lex::read_only_check(sql, d)
}

/// The count and whether it is the information_schema estimate rather than an exact count. A
/// grouped or distinct count that the server refuses (duplicate output names) falls back to
/// zero, since the data query reports the same problem.
async fn count(db: &Db, schema: &str, table: &str, q: &PageQuery) -> Res<(u64, bool)> {
    let d = db.dialect;
    if q.plain() {
        let est = match &db.pool {
            Pool::My(pool) => {
                let mut c = pool.acquire().await.map_err(se)?;
                // MySQL 8 caches TABLE_ROWS for a day. Fresh numbers cost a lot per schema, so only this one table's
                // lookup asks for them; 5.7 and MariaDB lack the variable, so a refusal is fine.
                if matches!(d, Dialect::MySql) {
                    let _ = c.execute("SET SESSION information_schema_stats_expiry = 0").await;
                }
                let t = Instant::now();
                let est = sqlx::query(d.table_status()).bind(schema).bind(table).fetch_optional(&mut *c).await;
                db.note(schema, d.table_status(), t, &est);
                est.map_err(se)?.and_then(|r| r.try_get::<Option<u64>, _>("row_estimate").ok().flatten())
            }
            // NULL while the planner has no estimate, and then the count below is exact.
            Pool::Pg(_) => db.meta(schema, d.table_status(), [schema, table]).await?.first().and_then(|r| r.uint("row_estimate")),
        };
        if let Some(n) = est.filter(|n| *n >= ESTIMATE_MIN) {
            return Ok((n, true));
        }
    }
    let sql = q.count_sql(d, schema, table);
    one_read(&sql, d)?;
    match db.read_text(schema, &sql, user_text(q)).await {
        Ok(rows) => Ok((rows.first().and_then(|r| num_at(r, 0)).unwrap_or(0), false)),
        Err(_) if q.grouped() || q.distinct => Ok((0, false)),
        Err(err) => Err(err),
    }
}

/// The column rows of one PostgreSQL table for a write. `format_type` spells a type without its schema when the schema
/// is on the search_path, and the type is written back as `CAST(.. AS <that text>)`. A user who can create objects in a
/// schema that comes earlier on the editor's path could then make the name resolve to a domain of their own, whose CHECK
/// would run with the editor's privileges. With the path reduced to pg_catalog every user type comes back as `schema.type`.
/// It runs in a read-only transaction of its own that is always rolled back, so a pooler never sees the setting leave it.
pub(super) async fn pg_columns(db: &Db, schema: &str, table: &str) -> Res<Vec<SqlRow>> {
    // Quarry's own query, no user text, so the connection goes back to the pool.
    let Pool::Pg(pools) = &db.pool else { return Err("Not a PostgreSQL connection".into()) };
    let mut conn = pools.acquire(db.opts.database()).await?;
    let sql = format!("{}{}", db.dialect.columns(), db.dialect.all_rows());
    let t = Instant::now();
    let res = super::pg::read::in_read_only(&mut conn, db, "SET LOCAL search_path = pg_catalog", async |c| sqlx::query(&sql).persistent(false).bind(schema).bind(table).fetch_all(c).await).await;
    db.note(schema, &sql, t, &res);
    if res.is_err() {
        conn.close_on_drop();
    }
    res.map(SqlRow::wrap_pg).map_err(se)
}

/// What `make_cols_kinds` reads as a column's `extra`. On PostgreSQL it is built from the catalog's own identity and
/// generated flags, never from the words of the column's expression: a computed column whose expression names a column
/// called auto_increment is not an identity.
fn pg_extra<R: Cells>(r: &R) -> String {
    let (ident, generated) = (text(r, "attidentity").unwrap_or_default(), text(r, "attgenerated").unwrap_or_default());
    let serial = ident.is_empty() && generated.is_empty() && text(r, "extra").as_deref() == Some("auto_increment");
    let mut v = Vec::new();
    if !ident.is_empty() || serial {
        v.push("auto_increment");
    }
    if ident == "a" || !generated.is_empty() {
        v.push("GENERATED");
    }
    v.join(" ")
}

/// Column types and the primary key, from information_schema.
pub(super) async fn table_meta(db: &Db, schema: &str, table: &str) -> Res<(HashMap<String, (String, bool, String)>, HashMap<String, Kind>, Vec<String>)> {
    let d = db.dialect;
    let cols = async { if d == Dialect::Postgres { pg_columns(db, schema, table).await } else { db.meta(schema, d.columns(), [schema, table]).await } };
    let (cols, idx) = tokio::try_join!(cols, db.meta(schema, d.indexes(), [schema, table]))?;
    let meta = cols
        .iter()
        .filter_map(|r| {
            let nullable = r.int("nullable").map(|n| n != 0).unwrap_or(true);
            let extra = if d == Dialect::Postgres { pg_extra(r) } else { text(r, "extra").unwrap_or_default() };
            Some((text(r, "name")?, (text(r, "full_type")?, nullable, extra)))
        })
        .collect();
    let kinds = if d == Dialect::Postgres { cols.iter().filter_map(|r| Some((text(r, "name")?, pg_kind(r)))).collect() } else { HashMap::new() };
    // A Postgres key is named by the table, so the index query says which one is the primary key.
    let pk = idx.iter().filter(|r| if d == Dialect::Postgres { r.int("is_primary") == Some(1) } else { text(r, "name").as_deref() == Some("PRIMARY") }).filter_map(|r| text(r, "col")).collect();
    Ok((meta, kinds, pk))
}

/// Whether any clause of the page query is text the user typed. The index hint is quoted by `PageQuery`, and the limit
/// and offset are numbers, so they are not.
fn user_text(q: &PageQuery) -> bool {
    [&q.filter, &q.order, &q.having].iter().any(|s| !s.trim().is_empty()) || q.select.iter().chain(&q.group).any(|s| !s.trim().is_empty())
}

/// The kind of a column of a Postgres catalog row, which carries the resolved base type and its category.
pub(super) fn pg_kind<R: Cells>(r: &R) -> Kind {
    Kind::from_pg(&text(r, "pg_type").unwrap_or_default(), &text(r, "pg_cat").unwrap_or_default(), &text(r, "pg_typtype").unwrap_or_default())
}

/// One page: columns, primary key, rows, the count of what the pages come from (groups when
/// grouped), whether that is an estimate, the time, and the rows before grouping when grouped.
type Paged = (Vec<Col>, Vec<String>, Vec<Row>, u64, bool, u128, Option<u64>);

/// Without `counted` the count and the grouped row count are skipped and come back as 0 and None.
async fn page(db: &Db, schema: &str, table: &str, q: &PageQuery, limit: u64, offset: u64, counted: bool) -> Res<Paged> {
    let start = Instant::now();
    let d = db.dialect;
    // The key is needed before the data query: without a total order, pages overlap and skip rows.
    let (meta, kinds, mut pk) = table_meta(db, schema, table).await?;
    if q.grouped() {
        pk.clear();
    }
    let sql = q.select_sql(d, schema, table, &pk, Some((limit, offset)));
    one_read(&sql, d)?;
    let data = db.read_text(schema, &sql, user_text(q));
    let total = async { if counted { count(db, schema, table, q).await } else { Ok((0, false)) } };
    let ((total, estimated), rows) = tokio::try_join!(total, data)?;
    // The LIMIT chip caps the whole result, not a page.
    let count = total.saturating_sub(q.skip).min(q.limit.unwrap_or(u64::MAX));
    let matched = if q.grouped() && counted {
        let sql = q.rows_sql(d, schema, table);
        one_read(&sql, d)?;
        let r = db.read_text(schema, &sql, user_text(q)).await?;
        Some(r.first().and_then(|r| num_at(r, 0)).unwrap_or(0))
    } else {
        None
    };
    let rcols: Vec<ColInfo> = match rows.first() {
        Some(r) => r.cols(),
        // An empty page still needs its headers.
        None => db.describe_text(&sql).await?,
    };
    // An invisible primary key is in the index list but not in SELECT *, so the rows cannot carry it.
    if !pk.iter().all(|p| rcols.iter().any(|c| c.name == *p)) {
        pk.clear();
    }
    // Output columns of a grouped query are computed, so they have no table type to look up.
    let (meta, kinds) = if q.grouped() { (HashMap::new(), HashMap::new()) } else { (meta, kinds) };
    let cols = make_cols_kinds(&rcols, &meta, &kinds, !pk.is_empty());
    let rows = rows.iter().map(|r| to_row(&cols, r, &pk)).collect::<Res<Vec<_>>>()?;
    Ok((cols, pk, rows, count, estimated, start.elapsed().as_millis(), matched))
}

/// Tables, views and columns of one database in three round trips run together.
async fn load_schema(db: &Db, schema: &str) -> Res<Schema> {
    let d = db.dialect;
    let p = [schema];
    let (tables, cols, fks) = tokio::try_join!(db.meta(schema, d.tables(), p), db.meta(schema, d.all_columns(), p), db.meta(schema, d.all_foreign_keys(), p))?;
    let fk: HashSet<(String, String)> = fks.iter().filter_map(|r| Some((text(r, "tbl")?, text(r, "name")?))).collect();
    let mut out: Vec<SchemaTable> = tables
        .iter()
        .filter_map(|r| {
            let kind = text(r, "kind").unwrap_or_default();
            Some(SchemaTable { name: text(r, "name")?, view: kind.contains("VIEW"), rows: super::structure::num(r, "row_estimate"), cols: Vec::new() })
        })
        .collect();
    for r in &cols {
        let (Some(t), Some(name), Some(ty)) = (text(r, "tbl"), text(r, "name"), text(r, "full_type")) else { continue };
        let Some(table) = out.iter_mut().find(|x| x.name == t) else { continue };
        let nullable = r.int("nullable").map(|n| n != 0).unwrap_or(true);
        let kind = if d == Dialect::Postgres { pg_kind(r) } else { kind_of_full(&ty) };
        table.cols.push(SchemaCol { fk: fk.contains(&(t, name.clone())), kind, key: text(r, "col_key").unwrap_or_default(), nullable, name, ty });
    }
    Ok(Schema { tables: out })
}

/// Below this many rows a table is cheap enough to group in full while someone types.
const VALUES_SMALL: u64 = 100_000;

/// A column's most frequent values, or None where looking would scan a big table: the estimate is
/// large and no index leads with the column. Both checks read metadata only. The lookup runs on a
/// connection of its own with a 300 ms limit, so a slow one never holds the pool.
async fn field_values(db: &Db, schema: &str, table: &str, column: &str, prefix: Option<&str>) -> Res<Option<Vec<(String, u64)>>> {
    let d = db.dialect;
    if d == Dialect::Postgres {
        return pg_field_values(db, schema, table, column, prefix).await;
    }
    let pool = db.my_pool()?;
    let Some(gate) = sqlx::query(d.value_gate()).bind(column).bind(schema).bind(table).fetch_optional(pool).await.map_err(se)? else { return Ok(None) };
    let small = super::structure::num(&gate, "est").is_some_and(|n| n < VALUES_SMALL);
    let leads = super::structure::num(&gate, "leads").unwrap_or(0) > 0;
    if !small && !leads {
        return Ok(None);
    }
    let mut c = super::run::side_conn(db, "Could not reach the server").await?.into_my()?;
    let res = async {
        // The pool's own setup, so values read in the same zone and charset as the grid shows them.
        session(&mut c, 300, &db.setup).await.map_err(|x| db.setup.failed_text().unwrap_or_else(|| e(x)))?;
        // The init command may raise the statement limit, and this lookup must stay short.
        let t = d.session_timeout(300);
        let _ = c.execute(t.as_str()).await;
        let sql = d.value_counts(schema, table, column, prefix.is_some(), 201);
        let mut q = sqlx::query(&sql);
        if let Some(p) = prefix {
            // An escape character of its own, because NO_BACKSLASH_ESCAPES leaves LIKE with none.
            q = q.bind(format!("{}%", super::rules::like_bang(p)));
        }
        let t = Instant::now();
        let rows = q.fetch_all(&mut c).await;
        db.note(schema, &sql, t, &rows);
        let rows = rows.map_err(se)?;
        Ok(Some(rows.iter().filter_map(|r| Some((text(r, "v")?, super::structure::num(r, "n")?))).collect()))
    }
    .await;
    let _ = c.close().await;
    res
}

/// `field_values` on PostgreSQL. The planner's estimate is NULL until the table has been analyzed, so a bounded count
/// stands in for it, and a table that is not small is looked up only when an index leads with the column.
async fn pg_field_values(db: &Db, schema: &str, table: &str, column: &str, prefix: Option<&str>) -> Res<Option<Vec<(String, u64)>>> {
    let d = db.dialect;
    let Some(gate) = db.meta(schema, d.value_gate(), [column, schema, table]).await?.into_iter().next() else { return Ok(None) };
    let est = super::structure::num(&gate, "est");
    let leads = super::structure::num(&gate, "leads").unwrap_or(0) > 0;
    if est.is_some_and(|n| n >= VALUES_SMALL) && !leads {
        return Ok(None);
    }
    let mut c = db.detached().await?;
    let res = async {
        let Conn::Pg(pc) = &mut c else { return Err("Not a PostgreSQL connection".to_string()) };
        // The lookup must stay short, so a slow one gives up instead of holding a connection. The limit is a SET LOCAL in
        // the read-only transaction of each read, never a session value, because a pooler would hand the session on.
        let short = super::pg::set_timeout("300");
        if est.is_none() && !leads {
            let sql = format!("SELECT count(*)::text FROM (SELECT 1 FROM {} LIMIT {}) AS q", d.qualified(schema, table), VALUES_SMALL + 1);
            match super::pg::read::in_read_only(pc, db, &short, async |c| c.fetch_all(sql.as_str()).await).await {
                Ok(rows) if rows.first().and_then(|r| r.try_get::<String, _>(0).ok()).and_then(|n| n.parse::<u64>().ok()).is_some_and(|n| n <= VALUES_SMALL) => {}
                // Too big to group, or too slow to tell.
                _ => return Ok(None),
            }
        }
        let sql = d.value_counts(schema, table, column, prefix.is_some(), 201);
        let pattern = prefix.map(|p| format!("{}%", super::rules::like_bang(p)));
        let t = Instant::now();
        let rows = super::pg::read::in_read_only(pc, db, &short, async |c| {
            let mut q = sqlx::query(&sql).persistent(false);
            if let Some(p) = &pattern {
                q = q.bind(p.as_str());
            }
            q.fetch_all(c).await
        })
        .await
        .map(SqlRow::wrap_pg);
        db.note(schema, &sql, t, &rows);
        let rows = rows.map_err(se)?;
        Ok(Some(rows.iter().filter_map(|r| Some((text(r, "v")?, super::structure::num(r, "n")?))).collect()))
    }
    .await;
    let _ = c.close().await;
    res
}

/// Columns that read as a row's name, tried in order before any other text column.
const LABELS: [&str; 9] = ["name", "full_name", "display_name", "title", "label", "username", "email", "code", "slug"];

/// What the key picker needs to know about a table once: its name-like column, and whether it is too big to scan.
async fn key_table(db: &Db, schema: &str, table: &str, key: &str) -> Res<super::KeyTable> {
    let d = db.dialect;
    let cols = async { if d == Dialect::Postgres { pg_columns(db, schema, table).await } else { db.meta(schema, d.columns(), [schema, table]).await } };
    let (cols, gate) = tokio::try_join!(cols, db.meta(schema, d.value_gate(), [key, schema, table]))?;
    let cols: Vec<(String, String)> = cols.iter().filter_map(|r| Some((text(r, "name")?, text(r, "full_type")?))).filter(|c| c.0 != key).collect();
    // Columns come in table order, so the fallback is the first text column.
    let label = LABELS.iter().find(|l| cols.iter().any(|c| c.0 == **l)).map(|l| l.to_string()).or_else(|| cols.iter().find(|c| c.1.contains("char") || c.1.contains("text")).map(|c| c.0.clone()));
    // An unknown estimate counts as big: the scan it would allow is the expensive mistake.
    let big = gate.first().and_then(|r| super::structure::num(r, "est")).is_none_or(|n| n >= VALUES_SMALL);
    Ok(super::KeyTable { label, big })
}

/// Up to 8 rows of `table` whose `key` starts with `q` or whose name-like column contains it, as (key, name) text.
/// On a big table only the exact key is looked up, through its index, because a LIKE on cast text scans every row.
async fn key_search(db: &Db, schema: &str, table: &str, key: &str, q: &str, known: Option<super::KeyTable>) -> Res<(super::KeyTable, Vec<(String, String)>)> {
    let d = db.dialect;
    let kt = match known {
        Some(k) => k,
        None => key_table(db, schema, table, key).await?,
    };
    let pg = d == Dialect::Postgres;
    let as_text = |c: &str| if pg { format!("{}::text", d.quote_ident(c)) } else { format!("CAST({} AS CHAR)", d.quote_ident(c)) };
    let lit = |s: &str| if pg { value::pg_lit(s) } else { value::lit(s, db.backslash_escapes()) };
    // An escape character of its own, because NO_BACKSLASH_ESCAPES leaves LIKE with none.
    let pat = super::rules::like_bang(q);
    let shown = kt.label.as_deref().map_or_else(|| "''".to_string(), as_text);
    let mut sql = format!("SELECT {} AS k, {shown} AS l FROM {}", as_text(key), d.qualified(schema, table));
    if !q.is_empty() && kt.big {
        sql += &format!(" WHERE {} = {}", d.quote_ident(key), lit(q));
    } else if !q.is_empty() {
        sql += &format!(" WHERE {} LIKE {} ESCAPE '!'", as_text(key), lit(&format!("{pat}%")));
        if let Some(l) = &kt.label {
            sql += &format!(" OR {} {} {} ESCAPE '!'", as_text(l), if pg { "ILIKE" } else { "LIKE" }, lit(&format!("%{pat}%")));
        }
    }
    sql += &format!(" ORDER BY {} LIMIT 8", d.quote_ident(key));
    // A key the server cannot compare (text typed for a number) is no match, and the table facts stay learned.
    let rows = db.read_text(schema, &sql, false).await.unwrap_or_default();
    Ok((kt, rows.iter().map(|r| (text_at(r, 0).unwrap_or_default(), text_at(r, 1).unwrap_or_default())).collect()))
}

impl Worker {
    /// Rows of the table a foreign key points at, for the key picker of the grid editor. `known` is what an earlier
    /// lookup learned about the table.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_key_search(&self, req: u64, db: Db, schema: String, table: String, key: String, q: String, known: Option<super::KeyTable>) {
        self.spawn(async move { Msg::SqlKeys { req, res: key_search(&db, &schema, &table, &key, &q, known).await } });
    }

    /// Every table, view and column of `schema`, for autocomplete.
    pub fn sql_schema(&self, conn: u64, db: Db, schema: String) {
        self.spawn(async move {
            let database = db.pg_database();
            let res = load_schema(&db, &schema).await;
            Msg::SqlSchema { conn, database, schema, res }
        });
    }

    /// The most frequent values of a column, with `prefix` only those that start with it. Skipped
    /// (None) for big tables unless an index leads with the column.
    pub fn sql_field_values(&self, conn: u64, db: Db, schema: String, table: String, column: String, prefix: Option<String>) {
        self.spawn(async move {
            let database = db.pg_database();
            let res = field_values(&db, &schema, &table, &column, prefix.as_deref()).await;
            Msg::SqlValues { conn, database, schema, table, column, prefix, res }
        });
    }

    /// A column data action over every row the Content bar matches, at most COL_VALUES_CAP of them in its order. Like
    /// the Mongo one it ignores the LIMIT and OFFSET chips. `col` is the row key, `src` the column's name on the server.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_column_values(&self, conn: u64, db: Db, database: String, schema: String, table: String, mut q: PageQuery, op: ColOp, col: String, src: String) {
        self.spawn_sql(conn, &schema.clone(), async move {
            (q.limit, q.skip) = (None, 0);
            // Reads only this column when the bar selects every column, as the Mongo path projects one field.
            let narrow = q.select.is_empty() && !q.grouped() && !q.distinct;
            if narrow {
                q.select = vec![db.dialect.quote_ident(&src)];
            }
            let res = async {
                let (cols, _, rows, ..) = page(&db, &schema, &table, &q, COL_VALUES_CAP as u64, 0, false).await?;
                let c = if narrow { cols.first() } else { cols.iter().find(|x| x.name == col) };
                let c = c.ok_or_else(|| format!("{col} is not in the result"))?;
                let values = rows.iter().filter_map(|r| r.raw().get(&c.name).ok().flatten()).filter_map(|v| Bson::try_from(v).ok()).collect();
                let values = value::stat_values(op, Some(c.kind), values);
                Ok((col_op_result(op, &values), rows.len() as i64 >= COL_VALUES_CAP))
            }
            .await;
            match res {
                Ok((ColOut::Groups(rows), _)) => Msg::Grouped { conn, db: database, coll: table, path: col, pipeline: Vec::new(), res: Ok(rows) },
                res => Msg::ColumnValues { conn, op, coll: table, path: col, res },
            }
        });
    }

    #[cfg(test)]
    pub fn sql_connect(&self, conn: u64, uri: String, timeout: Duration, net: Net) {
        self.sql_connect_with(conn, 0, uri, timeout, net, Advanced::default());
    }

    /// `attempt` comes back with the answer, so the app can drop one for a connect it no longer waits for.
    pub fn sql_connect_with(&self, conn: u64, attempt: u64, uri: String, timeout: Duration, net: Net, adv: Advanced) {
        self.spawn(async move {
            let res = open(&uri, timeout, &net, &adv).await.map(|(mut db, dbs, t)| {
                db.conn = conn;
                (db, dbs, t)
            });
            Msg::SqlConnected { conn, attempt, res }
        });
    }

    /// Connects and drops everything again, for the connection form.
    #[cfg(test)]
    pub fn sql_test(&self, uri: String, timeout: Duration, net: Net) {
        self.sql_test_with(0, uri, timeout, net, Advanced::default());
    }

    pub fn sql_test_with(&self, req: u64, uri: String, timeout: Duration, net: Net, adv: Advanced) {
        self.spawn(async move {
            let start = Instant::now();
            let mut info = None;
            let res = open_full(&uri, timeout, &net, &adv, true).await.and_then(|(db, _, t, ti)| {
                let refused = ti.setting_errors.len();
                let cert = super::pg::cert::test_note(&uri, &ti.client_dn, db.tls, matches!(t, Some(crate::db::Way::CloudSql(_))));
                info = Some(ti);
                if refused > 0 {
                    return Err(format!("Connected, but the server refused {refused} {}. See the marked rows on the Advanced tab.", if refused == 1 { "setting" } else { "settings" }));
                }
                let via = match &t {
                    Some(crate::db::Way::Ssh(t)) => format!(" through SSH (host key {})", t.fingerprint),
                    Some(crate::db::Way::CloudSql(p)) => format!(" through the Cloud SQL proxy (credentials: {})", if p.gcloud_account { "gcloud account" } else { "Application Default Credentials" }),
                    None => String::new(),
                };
                Ok(format!("Connected{via} to {} in {} ms{cert}", db.label(), start.elapsed().as_millis()))
            });
            Msg::Tested { req, res, info }
        });
    }

    /// The size on disk of each table of a schema, data and indexes together, for the Export dialog's estimate.
    pub fn sql_table_sizes(&self, req: u64, db: Db, schema: String) {
        self.spawn(async move {
            const PG: &str = "SELECT c.relname AS name, pg_total_relation_size(c.oid) AS bytes FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relkind IN ('r', 'p', 'm')";
            const MY: &str = "SELECT TABLE_NAME AS name, COALESCE(DATA_LENGTH, 0) + COALESCE(INDEX_LENGTH, 0) AS bytes FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_TYPE = 'BASE TABLE'";
            let sql = if db.dialect == Dialect::Postgres { PG } else { MY };
            let res = db.meta(&schema, sql, [schema.as_str()]).await.map(|rows| rows.iter().filter_map(|r| Some((text(r, "name")?, super::structure::num(r, "bytes")?))).collect());
            Msg::SqlSizes { req, res }
        });
    }

    /// Tables sorted, and which of them are views.
    pub fn sql_tables(&self, conn: u64, db: Db, schema: String) {
        self.spawn(async move {
            let database = db.pg_database();
            let res = async {
                let rows = db.meta(&schema, db.dialect.tables(), [schema.as_str()]).await?;
                let mut names = Vec::new();
                let mut views = Vec::new();
                for r in &rows {
                    let Some(n) = text(r, "name") else { continue };
                    if text(r, "kind").is_some_and(|k| k.contains("VIEW")) {
                        views.push(n.clone());
                    }
                    names.push(n);
                }
                names.sort();
                Ok((names, views))
            }
            .await;
            // A PostgreSQL schema name repeats across databases, so its answer says which database it belongs to.
            if database.is_empty() {
                Msg::Collections { conn, db: schema, res }
            } else {
                Msg::SqlTables { conn, database, schema, res }
            }
        });
    }

    /// One page of a table. The clauses are the text of the Content bar as the user typed them.
    /// Sends `SqlCols` and then `Docs`.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_page(&self, req: u64, db: Db, schema: String, table: String, q: PageQuery, limit: u64, offset: u64) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let res = match page(&db, &schema, &table, &q, limit, offset, true).await {
                Ok((cols, pk, rows, count, estimated, ms, matched)) => {
                    let _ = tx.send(Msg::SqlCols { req, cols, pk, estimated, matched });
                    Ok((rows, count, ms))
                }
                Err(err) => Err(err),
            };
            let _ = tx.send(Msg::Docs { req, res });
            ctx.request_repaint();
        });
    }

    /// Runs a Custom Query script on its own connection, whose id is stored in `h` as soon as it is
    /// known so `sql_kill` can stop it.
    pub fn sql_run(&self, req: u64, db: Db, schema: String, text: String, read_only: bool, page: usize, h: RunHandle) {
        self.spawn(async move {
            let sets = run(&db, &schema, &text, read_only, &h.conn, &h.cancel, page).await;
            // The server reuses connection ids, so a late Stop must not hit another session.
            h.conn.store(0, Ordering::SeqCst);
            Msg::SqlRan { req, sets }
        });
    }

    /// The row count of a paged statement, read in the background on a connection of its own under the
    /// statement timeout. `idx` is the result tab it belongs to.
    pub fn sql_count(&self, req: u64, idx: usize, db: Db, schema: String, stmt: String) {
        self.spawn(async move {
            let res = super::run::count_rows(&db, &schema, &stmt).await;
            Msg::SqlCount { req, idx, res }
        });
    }

    /// Stops a running script: sets `cancel` and interrupts the statement running on connection
    /// `connection_id`. It uses a connection of its own because the pool may be busy with paging.
    pub fn sql_kill(&self, db: Db, connection_id: u64, cancel: Arc<AtomicBool>) {
        self.spawn(async move {
            cancel.store(true, Ordering::SeqCst);
            // Zero means the script has not started a connection yet or already ended; the flag is enough.
            if connection_id == 0 {
                return Msg::Info(Ok("Stopped the script".to_string()));
            }
            let res = db.stop(connection_id).await;
            Msg::Info(res)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use super::super::relay::fill;
    use super::super::{ResultSet, RUN_ROWS_MAX};
    use std::sync::mpsc::Receiver;
    use std::sync::Mutex;

    /// Tests share one database and run in parallel, so the last fixture to go drops it again.
    static OPEN: Mutex<usize> = Mutex::new(0);

    /// The writable fixture pool. Keep it bound for the whole test: dropping it counts the test as finished.
    struct Fixture {
        pool: sqlx::MySqlPool,
        rt: tokio::runtime::Handle,
    }

    impl std::ops::Deref for Fixture {
        type Target = sqlx::MySqlPool;
        fn deref(&self) -> &sqlx::MySqlPool {
            &self.pool
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let mut n = OPEN.lock().unwrap_or_else(|e| e.into_inner());
            *n -= 1;
            if *n == 0 {
                // Dropping the database takes its views with it.
                let _ = self.rt.block_on(raw_sql("DROP DATABASE IF EXISTS quarry_test").execute(&self.pool));
            }
        }
    }

    /// Needs a server: QUARRY_TEST_MYSQL, like mysql://root:quarry@127.0.0.1:3306. Skipped when unset.
    /// Returns a worker, its channel, a connected Db whose queries time out after 30 s, and a plain
    /// writable pool for fixtures (the Db's own session is read-only).
    fn live(table: &str, ddl: &str) -> Option<(Worker, Receiver<Msg>, Db, Fixture)> {
        let uri = std::env::var("QUARRY_TEST_MYSQL").ok()?;
        *OPEN.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        let (tx, rx) = std::sync::mpsc::channel();
        let w = Worker::new(tx, eframe::egui::Context::default());
        let setup = w.rt.block_on(async {
            let p = sqlx::MySqlPool::connect(&uri).await.unwrap();
            raw_sql("CREATE DATABASE IF NOT EXISTS quarry_test").execute(&p).await.unwrap();
            if !table.is_empty() {
                raw_sql(&format!("DROP TABLE IF EXISTS quarry_test.{table}")).execute(&p).await.unwrap();
                raw_sql(&format!("CREATE TABLE quarry_test.{table} ({ddl})")).execute(&p).await.unwrap();
            }
            p
        });
        w.sql_connect(1, uri, Duration::from_secs(30), Net::default());
        let Msg::SqlConnected { res: Ok((db, dbs, _)), .. } = next(&rx) else { panic!("expected SqlConnected") };
        assert!(dbs.iter().any(|d| d == "quarry_test"), "{dbs:?}");
        let rt = w.rt.handle().clone();
        Some((w, rx, db, Fixture { pool: setup, rt }))
    }

    fn next(rx: &Receiver<Msg>) -> Msg {
        rx.recv_timeout(Duration::from_secs(15)).expect("worker answer")
    }

    fn exec(w: &Worker, p: &sqlx::MySqlPool, sql: &str) {
        w.rt.block_on(async { raw_sql(sql).execute(p).await.unwrap() });
    }

    fn page_of(w: &Worker, rx: &Receiver<Msg>, db: &Db, table: &str, filter: &str, order: &str, limit: u64, offset: u64) -> (Vec<Col>, Vec<String>, Vec<Document>, u64) {
        page_q(w, rx, db, table, PageQuery { filter: filter.into(), order: order.into(), ..Default::default() }, limit, offset)
    }

    fn page_q(w: &Worker, rx: &Receiver<Msg>, db: &Db, table: &str, q: PageQuery, limit: u64, offset: u64) -> (Vec<Col>, Vec<String>, Vec<Document>, u64) {
        w.sql_page(7, db.clone(), "quarry_test".into(), table.into(), q, limit, offset);
        let Msg::SqlCols { cols, pk, .. } = next(rx) else { panic!("expected SqlCols") };
        let Msg::Docs { res: Ok((rows, count, _)), .. } = next(rx) else { panic!("expected Docs") };
        (cols, pk, rows.iter().map(|r| r.decode().unwrap()).collect(), count)
    }

    #[test]
    fn live_page_clauses_select_distinct_group_having_and_index() {
        let Some((w, rx, db, p)) = live("t_clause", "id INT PRIMARY KEY, status VARCHAR(10), total INT, at DATETIME, KEY k_status (status)") else { return };
        let vals: Vec<String> = (0..20).map(|i| format!("({i}, '{}', {}, '2026-11-{:02} 10:00:00')", ["paid", "shipped", "open"][i % 3], i * 10, 1 + i % 4)).collect();
        exec(&w, &p, &format!("INSERT INTO quarry_test.t_clause VALUES {}", vals.join(",")));
        let base = PageQuery::default();
        // A select list: only those columns, and the key is gone with its column.
        let (cols, pk, rows, count) = page_q(&w, &rx, &db, "t_clause", PageQuery { select: vec!["status".into(), "total * 2 AS dbl".into()], order: "total".into(), ..base.clone() }, 5, 0);
        assert_eq!(cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["status", "dbl"]);
        assert!(pk.is_empty());
        assert_eq!((rows.len(), count), (5, 20));
        assert_eq!(rows[1].get_str("status"), Ok("shipped"));
        // DISTINCT counts distinct rows.
        let (_, _, rows, count) = page_q(&w, &rx, &db, "t_clause", PageQuery { select: vec!["status".into()], distinct: true, order: "status".into(), ..base.clone() }, 10, 0);
        assert_eq!((rows.len(), count), (3, 3));
        // LIMIT caps the whole result and OFFSET skips: 7 rows in 5 per page leave 2 on page 2.
        let (_, _, rows, count) = page_q(&w, &rx, &db, "t_clause", PageQuery { order: "id".into(), limit: Some(7), skip: 2, ..base.clone() }, 5, 5);
        assert_eq!((rows.len(), count), (2, 7));
        assert_eq!(rows[0].get_i64("id"), Ok(7));
        // GROUP BY with an aggregate and HAVING: counts are groups, rows have no key.
        let g = PageQuery { select: vec!["status".into(), "COUNT(*) AS n".into(), "SUM(total) AS sum_total".into()], group: vec!["status".into()], having: "n > 6".into(), order: "n DESC".into(), ..base.clone() };
        let (cols, pk, rows, count) = page_q(&w, &rx, &db, "t_clause", g.clone(), 10, 0);
        assert_eq!(cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["status", "n", "sum_total"]);
        assert!(pk.is_empty() && rows[0].get("_id").is_none());
        assert_eq!((rows.len(), count), (2, 2));
        assert_eq!(rows[0].get_str("status"), Ok("paid"));
        w.sql_page(8, db.clone(), "quarry_test".into(), "t_clause".into(), g, 10, 0);
        let Msg::SqlCols { matched, .. } = next(&rx) else { panic!("expected SqlCols") };
        assert_eq!(matched, Some(20));
        let _ = next(&rx);
        // A date grain groups by the day.
        let day = crate::db::sql::rules::Grain::Day.expr("at", db.dialect);
        let (_, _, rows, count) = page_q(&w, &rx, &db, "t_clause", PageQuery { select: vec![format!("{day} AS day"), "COUNT(*) AS n".into()], group: vec!["day".into()], order: "day".into(), ..base.clone() }, 10, 0);
        assert_eq!((rows.len(), count), (4, 4));
        // The index hint is quoted, and a bad name is the server's error, not an injection.
        let (_, _, rows, _) = page_q(&w, &rx, &db, "t_clause", PageQuery { filter: "status = 'paid'".into(), index_hint: "k_status".into(), ..base.clone() }, 100, 0);
        assert_eq!(rows.len(), 7);
        w.sql_page(9, db.clone(), "quarry_test".into(), "t_clause".into(), PageQuery { index_hint: "k) ; DROP TABLE t_clause; --".into(), ..base }, 10, 0);
        let Msg::Docs { res, .. } = next(&rx) else { panic!("expected Docs") };
        assert!(res.is_err());
        assert_eq!(page_of(&w, &rx, &db, "t_clause", "", "", 100, 0).3, 20);
    }

    fn run_all(w: &Worker, rx: &Receiver<Msg>, db: &Db, text: &str, read_only: bool) -> Vec<ResultSet> {
        w.sql_run(9, db.clone(), "quarry_test".into(), text.into(), read_only, 0, Default::default());
        let Msg::SqlRan { sets, .. } = next(rx) else { panic!("expected SqlRan") };
        sets
    }

    #[test]
    fn live_connect_reports_version_dialect_and_tls() {
        let Some((w, rx, db, _p)) = live("", "") else { return };
        println!("{} tls={} mode={}", db.label(), db.tls, db.sql_mode);
        assert!(db.version.chars().next().unwrap().is_ascii_digit());
        assert_eq!(db.dialect, Dialect::from_version(&db.version));
        assert!(db.label().starts_with("MySQL ") || db.label().starts_with("MariaDB "));
        let uri = std::env::var("QUARRY_TEST_MYSQL").unwrap();
        w.sql_test(uri, Duration::from_secs(10), Net::default());
        let Msg::Tested { res: Ok(m), .. } = next(&rx) else { panic!("expected Tested") };
        assert!(m.starts_with("Connected to ") && m.contains(&db.label()) && m.ends_with(" ms"), "{m}");
        w.sql_test("mysql://root:wrong@127.0.0.1:3306".into(), Duration::from_secs(10), Net::default());
        let Msg::Tested { res, .. } = next(&rx) else { panic!("expected Tested") };
        assert!(res.is_err());
    }

    #[test]
    fn live_tables_lists_views_apart() {
        let Some((w, rx, db, p)) = live("t_list", "id INT PRIMARY KEY") else { return };
        exec(&w, &p, "DROP VIEW IF EXISTS quarry_test.v_list");
        exec(&w, &p, "CREATE VIEW quarry_test.v_list AS SELECT id FROM quarry_test.t_list");
        w.sql_tables(1, db, "quarry_test".into());
        let Msg::Collections { res: Ok((names, views)), .. } = next(&rx) else { panic!("expected Collections") };
        assert!(names.contains(&"t_list".to_string()) && names.contains(&"v_list".to_string()));
        assert!(views.contains(&"v_list".to_string()) && !views.contains(&"t_list".to_string()));
    }

    #[test]
    fn live_schema_lists_columns_keys_and_estimates() {
        let Some((w, rx, db, p)) = live("t_sch_parent", "id INT UNSIGNED PRIMARY KEY") else { return };
        exec(&w, &p, "DROP TABLE IF EXISTS quarry_test.t_sch_child");
        exec(&w, &p, "CREATE TABLE quarry_test.t_sch_child (id INT PRIMARY KEY, parent_id INT UNSIGNED NOT NULL, status ENUM('a','b'), note TEXT, KEY k_parent (parent_id), CONSTRAINT fk_parent FOREIGN KEY (parent_id) REFERENCES t_sch_parent (id))");
        exec(&w, &p, "DROP VIEW IF EXISTS quarry_test.v_sch");
        exec(&w, &p, "CREATE VIEW quarry_test.v_sch AS SELECT id FROM quarry_test.t_sch_parent");
        w.sql_schema(1, db, "quarry_test".into());
        let Msg::SqlSchema { schema, res: Ok(s), .. } = next(&rx) else { panic!("expected SqlSchema") };
        assert_eq!(schema, "quarry_test");
        let child = s.table("t_sch_child").unwrap();
        assert!(!child.view && child.rows.is_some());
        let col = |n: &str| child.col(n).unwrap();
        assert_eq!((col("id").key.as_str(), col("id").nullable, col("id").fk), ("PRI", false, false));
        assert_eq!((col("parent_id").ty.as_str(), col("parent_id").kind, col("parent_id").fk), ("int unsigned", Kind::UInt, true));
        assert_eq!((col("status").kind, col("status").nullable), (Kind::Enum, true));
        assert_eq!(crate::db::sql::schema::enum_values(&col("status").ty), ["a", "b"]);
        let view = s.table("v_sch").unwrap();
        assert!(view.view && view.rows.is_none() && view.cols.len() == 1);
        assert_eq!(child.cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "parent_id", "status", "note"]);
    }

    #[test]
    fn live_column_values_read_the_whole_query() {
        let Some((w, rx, db, p)) = live("t_colvals", "id INT PRIMARY KEY, `a.b` DECIMAL(6,2), at DATETIME") else { return };
        exec(&w, &p, "INSERT INTO quarry_test.t_colvals VALUES (1, 1.50, '2026-01-01 00:00:00'), (2, 2.50, '2026-01-03 06:00:00'), (3, NULL, NULL), (4, 2.50, '2026-01-02 00:00:00')");
        let run = |op, col: &str, q: PageQuery| {
            w.sql_column_values(1, db.clone(), "quarry_test".into(), "quarry_test".into(), "t_colvals".into(), q, op, col.into(), col.into());
            next(&rx)
        };
        // The LIMIT chip is ignored, the dotted name is one column, and DECIMAL text counts as numbers.
        let Msg::ColumnValues { res: Ok((ColOut::Notice(s), false)), .. } = run(ColOp::Sum, "a.b", PageQuery { limit: Some(1), ..Default::default() }) else { panic!("expected a sum") };
        assert!(s.starts_with("6.5\n") && s.contains("3 of 4 values are numbers"), "{s}");
        let Msg::ColumnValues { res: Ok((ColOut::Notice(s), _)), .. } = run(ColOp::Range, "at", PageQuery { filter: "id < 3".into(), ..Default::default() }) else { panic!("expected a range") };
        assert!(s.contains("2026-01-01T00:00:00") && s.contains("Span 2d 6h"), "{s}");
        // Groups keep the exact text, and a SQL table has no pipeline to open.
        let Msg::Grouped { res: Ok(rows), pipeline, .. } = run(ColOp::Group, "a.b", PageQuery::default()) else { panic!("expected groups") };
        assert!(pipeline.is_empty());
        assert_eq!(rows[0], mongodb::bson::doc! { "_id": "2.50", "count": 2i64 });
        let Msg::ColumnValues { res: Ok((ColOut::Copy(text, 3), _)), .. } = run(ColOp::CopyDistinct, "a.b", PageQuery { order: "id".into(), ..Default::default() }) else { panic!("expected a copy") };
        assert_eq!(text, "1.50\n2.50\n");
    }

    #[test]
    fn live_field_values_count_and_respect_the_gate() {
        let Some((w, rx, db, p)) = live("t_vals", "id INT PRIMARY KEY, status VARCHAR(10), n INT, note VARCHAR(10), KEY k_status (status)") else { return };
        let vals: Vec<String> = (0..10).map(|i| format!("({i}, '{}', {}, NULL)", if i < 6 { "paid" } else if i < 9 { "open" } else { "it''s" }, i % 2)).collect();
        exec(&w, &p, &format!("INSERT INTO quarry_test.t_vals VALUES {}", vals.join(",")));
        let ask = |col: &str, prefix: Option<&str>| {
            w.sql_field_values(1, db.clone(), "quarry_test".into(), "t_vals".into(), col.into(), prefix.map(str::to_string));
            let Msg::SqlValues { res, column, .. } = next(&rx) else { panic!("expected SqlValues") };
            assert_eq!(column, col);
            res
        };
        // Most frequent first, an integer column comes back as text, NULLs are left out.
        assert_eq!(ask("status", None).unwrap().unwrap(), [("paid".to_string(), 6), ("open".to_string(), 3), ("it's".to_string(), 1)]);
        assert_eq!(ask("n", None).unwrap().unwrap().len(), 2);
        assert!(ask("note", None).unwrap().unwrap().is_empty());
        assert_eq!(ask("status", Some("p")).unwrap().unwrap(), [("paid".to_string(), 6)]);
        // A wildcard typed by the user is a plain character.
        assert!(ask("status", Some("%")).unwrap().unwrap().is_empty());
        // A name that is not a column is the server's error, not an injection.
        assert!(ask("x` FROM t_vals; --", None).is_err());
        // Big estimate and no leading index: not looked up. The estimate is faked with a stats-free table of many rows.
        exec(&w, &p, "DROP TABLE IF EXISTS quarry_test.t_big");
        exec(&w, &p, "CREATE TABLE quarry_test.t_big (id INT PRIMARY KEY, a INT, b INT, KEY k_ab (a, b))");
        exec(&w, &p, "SET SESSION cte_max_recursion_depth = 200000; INSERT INTO quarry_test.t_big WITH RECURSIVE s(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM s WHERE i < 150000) SELECT i, i % 7, i % 5 FROM s");
        exec(&w, &p, "ANALYZE TABLE quarry_test.t_big");
        let big = |col: &str| {
            w.sql_field_values(1, db.clone(), "quarry_test".into(), "t_big".into(), col.into(), None);
            let Msg::SqlValues { res, .. } = next(&rx) else { panic!("expected SqlValues") };
            res
        };
        assert_eq!(big("b").unwrap(), None);
        assert_eq!(big("a").unwrap().unwrap().len(), 7);
    }

    #[test]
    fn live_page_filters_orders_and_carries_the_primary_key() {
        let Some((w, rx, db, p)) = live("t_page", "a INT, b VARCHAR(20) COLLATE utf8mb4_bin, n INT, PRIMARY KEY (b, a)") else { return };
        let vals: Vec<String> = (0..30).map(|i| format!("({i}, 'k{i}', {})", i % 3)).collect();
        exec(&w, &p, &format!("INSERT INTO quarry_test.t_page VALUES {}", vals.join(",")));
        let (cols, pk, rows, count) = page_of(&w, &rx, &db, "t_page", "n = 1", "a DESC -- trailing comment", 4, 2);
        assert_eq!(count, 10);
        assert_eq!(pk, ["b", "a"]);
        assert_eq!(cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["a", "b", "n"]);
        // A _bin text column is reported as binary by sqlx, the column's declared type keeps it text.
        assert_eq!(cols[1].kind, Kind::Text);
        assert_eq!(rows.iter().map(|d| d.get_i64("a").unwrap()).collect::<Vec<_>>(), [22, 19, 16, 13]);
        assert_eq!(rows[0].get_str("b"), Ok("k22"));
        assert_eq!(rows[0].get_document("_id").unwrap(), &mongodb::bson::doc! { "b": "k22", "a": 22_i64 });
        // An empty page still has its columns, and injection through the filter is refused.
        let (cols, _, rows, count) = page_of(&w, &rx, &db, "t_page", "n = 99", "", 10, 0);
        assert_eq!((cols.len(), rows.len(), count), (3, 0, 0));
        w.sql_page(8, db.clone(), "quarry_test".into(), "t_page".into(), PageQuery { filter: "1=1; DELETE FROM t_page".into(), ..Default::default() }, 10, 0);
        let Msg::Docs { res, .. } = next(&rx) else { panic!("expected Docs") };
        assert!(res.is_err());
        // The rows are all still there.
        assert_eq!(page_of(&w, &rx, &db, "t_page", "", "", 100, 0).3, 30);
    }

    #[test]
    fn live_every_column_type_converts() {
        let ddl = "id INT PRIMARY KEY, i INT, bu BIGINT UNSIGNED, bi BIGINT, d DECIMAL(10,2), f DOUBLE, v VARCHAR(20), t TEXT, b BLOB, dt DATE, dtt DATETIME, tm TIME, y YEAR, j JSON, e ENUM('a','b'), s SET('a','b','c'), bt BIT(8)";
        let Some((w, rx, db, p)) = live("t_types", ddl) else { return };
        exec(&w, &p, "INSERT INTO quarry_test.t_types VALUES (1, -5, 18446744073709551615, 9007199254740993, 12.50, 1.5, 'héllo', 'long text', 0x0001FF, '2024-02-29', '2024-02-29 13:14:15', '-12:34:56', 2024, '{\"a\": [1, 2]}', 'b', 'a,c', b'101')");
        exec(&w, &p, "INSERT INTO quarry_test.t_types (id) VALUES (2)");
        let (cols, _, rows, _) = page_of(&w, &rx, &db, "t_types", "", "id", 10, 0);
        let bin = |b: &[u8]| Bson::Binary(mongodb::bson::Binary { subtype: mongodb::bson::spec::BinarySubtype::Generic, bytes: b.to_vec() });
        let r = &rows[0];
        assert_eq!(r.get("i"), Some(&Bson::Int64(-5)));
        // Above i64::MAX stays exact as text.
        assert_eq!(r.get("bu"), Some(&Bson::String("18446744073709551615".into())));
        assert_eq!(r.get("bi"), Some(&Bson::Int64(9007199254740993)));
        assert_eq!(r.get("d"), Some(&Bson::String("12.50".into())));
        assert_eq!(r.get("f"), Some(&Bson::Double(1.5)));
        assert_eq!(r.get_str("v"), Ok("héllo"));
        assert_eq!(r.get_str("t"), Ok("long text"));
        assert_eq!(r.get("b"), Some(&bin(&[0, 1, 255])));
        assert_eq!(r.get_str("dt"), Ok("2024-02-29"));
        assert_eq!(r.get_str("dtt"), Ok("2024-02-29 13:14:15"));
        assert_eq!(r.get_str("tm"), Ok("-12:34:56"));
        assert_eq!(r.get("y"), Some(&Bson::Int64(2024)));
        assert!(r.get_str("j").unwrap().contains("\"a\""));
        assert_eq!((r.get_str("e"), r.get_str("s")), (Ok("b"), Ok("a,c")));
        assert_eq!(r.get("bt"), Some(&bin(&[5])));
        let n = &rows[1];
        for c in cols.iter().filter(|c| c.name != "id") {
            assert_eq!(n.get(&c.name), Some(&Bson::Null), "{}", c.name);
            assert!(c.nullable, "{}", c.name);
        }
        let kinds: Vec<Kind> = cols.iter().map(|c| c.kind).collect();
        // MariaDB stores JSON as LONGTEXT, so it is text there.
        let json = if db.dialect == Dialect::MariaDb { Kind::Text } else { Kind::Json };
        assert_eq!(kinds, [Kind::Int, Kind::Int, Kind::UInt, Kind::Int, Kind::Decimal, Kind::Float, Kind::Text, Kind::Text, Kind::Binary, Kind::Date, Kind::DateTime, Kind::Time, Kind::Year, json, Kind::Enum, Kind::Set, Kind::Bit]);
        assert!(!cols[0].nullable);
    }

    #[test]
    fn fill_binds_values_as_escaped_literals() {
        use value::Param;
        let f = |t: &str, p: &[Param], nb: bool| fill(t, p, !nb, false);
        assert_eq!(f("SELECT ? , '?', `?` -- ?\n, ?", &[Param::Int(1), Param::Null], false).unwrap(), "SELECT  1  , '?', `?` -- ?\n,  NULL ");
        assert_eq!(f("WHERE a = ?", &[Param::Text("it's \\ x".into())], false).unwrap(), "WHERE a =  'it''s \\\\ x' ");
        assert_eq!(f("WHERE a = ?", &[Param::Text("a\\".into())], true).unwrap(), "WHERE a =  'a\\' ");
        assert_eq!(f("?, ?", &[Param::Bytes(vec![0, 255]), Param::Float(1.5)], false).unwrap(), " X'00ff' ,  1.5e0 ");
        // A literal never fuses with the word next to it.
        assert_eq!(f("SELECT X?, a?, @?", &[Param::Int(1), Param::Int(2), Param::Int(3)], false).unwrap(), "SELECT X 1 , a 2 , @ 3 ");
        // A value that tries to end the string stays inside it, so the text is still one statement.
        let evil = f("SELECT ?", &[Param::Text("'; DROP TABLE t; --".into())], false).unwrap();
        assert_eq!(lex::split_statements(&evil, Dialect::MySql).len(), 1);
        assert!(f("?", &[], false).is_err() && f("1", &[Param::Null], false).is_err());
        assert!(f("?", &[Param::Float(f64::NAN)], false).is_err() && f("?", &[Param::Text("a\0".into())], false).is_err());
    }

    /// Runs the runner with this input line and these SQL relays, returning the queries it reported and its result.
    fn shell_run(w: &Worker, rx: &Receiver<Msg>, input: serde_json::Value, sql: Vec<(String, crate::db::SqlRelay)>) -> (Vec<crate::db::ShellQuery>, Res<String>) {
        let job = crate::db::ShellJob { runtime: "bun".into(), dir: std::env::temp_dir().join("quarry-shell-live-test"), input: input.to_string(), packages: Vec::new(), sql };
        let (_cancel, cancel_rx) = tokio::sync::oneshot::channel();
        w.shell(5, job, cancel_rx);
        let mut queries = Vec::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(180)).expect("shell answer") {
                Msg::ShellQuery { q, .. } => queries.push(q),
                Msg::ShellDone { res, .. } => return (queries, res),
                _ => {}
            }
        }
    }

    fn relay(db: &Db, read_only: bool) -> crate::db::SqlRelay {
        crate::db::SqlRelay { db: db.clone(), schema: "quarry_test".into(), read_only, conn_id: Default::default() }
    }

    /// Runs a shell script on a SQL relay, returning the queries it sent and its result.
    fn shell_script(w: &Worker, rx: &Receiver<Msg>, db: &Db, code: &str, tables: &[&str], read_only: bool) -> (Vec<crate::db::ShellQuery>, Res<String>) {
        let input = serde_json::json!({ "code": code, "packages": [], "timeoutMs": 30000, "main": "shop", "mongo": [], "sql": [{ "name": "shop", "db": "quarry_test", "tables": tables }] });
        shell_run(w, rx, input, vec![("shop".into(), relay(db, read_only))])
    }

    fn bun_missing() -> bool {
        let missing = std::process::Command::new("bun").arg("--version").output().is_err();
        if missing {
            println!("skipped: bun is not installed");
        }
        missing
    }

    fn query_names(queries: &[crate::db::ShellQuery]) -> String {
        queries.iter().map(|q| format!("{} {}", q.conn, q.text)).collect::<Vec<_>>().join(" | ")
    }

    #[test]
    fn live_shell_second_sql_connection_keeps_its_own_read_only_and_names_its_queries() {
        let Some((w, rx, db, p)) = live("t_att", "id INT PRIMARY KEY, note VARCHAR(20)") else { return };
        if bun_missing() {
            return;
        }
        exec(&w, &p, "INSERT INTO quarry_test.t_att VALUES (1, 'a')");
        let script = "async function run() {
  const [{ one }] = await other.sql`SELECT 1 AS one`;
  let refused = null;
  try { await other.sql`UPDATE t_att SET note = 'x'` } catch (e) { refused = e.message }
  let viaTable = null;
  try { await other.t_att.update({ id: 1 }, { note: 'y' }) } catch (e) { viaTable = e.message }
  const seen = await other.t_att.find({});
  await t_att.update({ id: 1 }, { note: 'main' });
  return { one, refused, viaTable, seen: seen.length, now: (await other.sql.unsafe('SELECT note FROM t_att'))[0].note, hasTable: typeof other.table };
}";
        // Tables named like function properties must not replace them.
        let input = serde_json::json!({ "code": script, "packages": [], "timeoutMs": 30000, "main": "shop", "mongo": [], "sql": [
            { "name": "shop", "db": "quarry_test", "tables": ["t_att"] }, { "name": "other", "db": "quarry_test", "tables": ["t_att", "call", "bind"] }] });
        let (queries, res) = shell_run(&w, &rx, input, vec![("shop".into(), relay(&db, false)), ("other".into(), relay(&db, true))]);
        let v: serde_json::Value = serde_json::from_str(&res.expect("script result")).unwrap();
        let v = &v["v"];
        assert_eq!(v["one"], 1, "{v}");
        assert!(v["refused"].as_str().is_some_and(|m| !m.is_empty()) && v["viaTable"].as_str().is_some_and(|m| !m.is_empty()), "{v}");
        assert_eq!(v["seen"], 1);
        assert_eq!(v["hasTable"], "function");
        // The unlocked main connection wrote, the locked one did not.
        assert_eq!(v["now"], "main");
        assert!(queries.iter().any(|q| q.conn == "other" && q.error.is_some()) && queries.iter().any(|q| q.conn == "shop" && q.error.is_none() && q.affected == Some(1)), "{}", query_names(&queries));
        assert!(queries.iter().all(|q| q.conn == "shop" || q.conn == "other"));
    }

    fn sql_entry(name: &str, tables: &[&str]) -> serde_json::Value {
        serde_json::json!({ "name": name, "db": "quarry_test", "tables": tables })
    }

    #[test]
    fn live_shell_refuses_repeated_connection_names() {
        let Some((w, rx, db, _p)) = live("t_dup", "id INT PRIMARY KEY") else { return };
        if bun_missing() {
            return;
        }
        let code = "async function run() { return await shop.sql`SELECT 1` }";
        // The runner refuses the repeat when Rust did not catch it.
        let input = serde_json::json!({ "code": code, "packages": [], "timeoutMs": 30000, "main": "shop", "mongo": [], "sql": [sql_entry("shop", &[]), sql_entry("shop", &[])] });
        let (_, res) = shell_run(&w, &rx, input.clone(), vec![("shop".into(), relay(&db, true))]);
        assert!(res.unwrap_err().contains("more than once"));
        // run_shell refuses before Bun starts.
        let (queries, res) = shell_run(&w, &rx, input, vec![("shop".into(), relay(&db, true)), ("shop".into(), relay(&db, true))]);
        assert!(res.unwrap_err().contains("more than once") && queries.is_empty());
        // A Mongo entry and an SQL entry share a name; no server is reached because the check comes first.
        let input = serde_json::json!({ "code": code, "packages": [], "timeoutMs": 30000, "main": "x",
            "mongo": [{ "name": "x", "uri": "mongodb://127.0.0.1:1", "db": "d", "proxy": null, "collections": [], "readOnly": true }], "sql": [sql_entry("x", &[])] });
        let (_, res) = shell_run(&w, &rx, input, vec![("x".into(), relay(&db, true))]);
        assert!(res.unwrap_err().contains("more than once"));
    }

    #[test]
    fn live_shell_results_keep_invalid_dates_and_proto_keys() {
        let Some((w, rx, db, _p)) = live("t_dates", "id INT PRIMARY KEY") else { return };
        if bun_missing() {
            return;
        }
        let script = "async function run() { return { bad: new Date(NaN), old: new Date(-62198755200000 - 86400000*400), far: new Date(8.64e15), ok: new Date(0), p: JSON.parse('{\"__proto__\":{\"a\":1},\"b\":2}') } }";
        let check = |res: Res<String>| {
            let v = decode(&res.expect("script result"));
            assert_eq!(v.get("bad"), Some(&Bson::Null), "{v}");
            assert_eq!(v.get("old"), Some(&Bson::Null));
            assert_eq!(v.get("far"), Some(&Bson::Null));
            assert!(matches!(v.get("ok"), Some(Bson::DateTime(_))));
            let p = v.get_document("p").unwrap();
            assert_eq!(p.get_i32("b").unwrap(), 2);
            assert_eq!(p.get_document("__proto__").unwrap().get_i32("a").unwrap(), 1, "{p}");
        };
        let input = serde_json::json!({ "code": script, "packages": [], "timeoutMs": 30000, "main": "shop", "mongo": [], "sql": [sql_entry("shop", &[])] });
        check(shell_run(&w, &rx, input, vec![("shop".into(), relay(&db, true))]).1);
        // With the driver loaded the dates are spelled differently.
        let Some(uri) = mongo_user(&w, "quarry_attach_c") else { return };
        let input = serde_json::json!({ "code": script, "packages": [], "timeoutMs": 30000, "main": "shop",
            "mongo": [{ "name": "travels", "uri": uri, "db": "quarry_attach_c", "proxy": null, "collections": [], "readOnly": true }], "sql": [sql_entry("shop", &[])] });
        check(shell_run(&w, &rx, input, vec![("shop".into(), relay(&db, true))]).1);
    }

    #[test]
    fn live_shell_lists_mongo_queries_only_with_attachments_and_words_them_right() {
        let Some((w, rx, db, _p)) = live("t_mq", "id INT PRIMARY KEY") else { return };
        let Some(uri) = mongo_user(&w, "quarry_attach_d") else { return };
        if bun_missing() {
            return;
        }
        let mongo = serde_json::json!([{ "name": "docs", "uri": uri, "db": "quarry_attach_d", "proxy": null, "collections": ["users"], "readOnly": false }]);
        // One connection: no Mongo query lines.
        let input = serde_json::json!({ "code": "async function run() { return { n: (await db.collection('users').find({}).toArray()).length } }", "packages": [], "timeoutMs": 30000, "main": "docs", "mongo": mongo, "sql": [] });
        let (queries, res) = shell_run(&w, &rx, input, vec![]);
        assert_eq!(decode(&res.expect("script result")).get_i32("n").unwrap(), 1);
        assert!(queries.is_empty(), "{}", query_names(&queries));
        // The two long filters put an emoji across the 2000 character cut in one of the two parities.
        let script = "async function run() {
  const u = db.collection('users');
  await u.insertMany([{ k: 1 }, { k: 2 }, { k: 3 }]);
  await db.command({ count: 'users' });
  await u.find({}).batchSize(1).toArray();
  await u.find({}).toArray();
  try { await db.command({ dropUser: 'nobody' }) } catch (e) {}
  for (const n of [0, 1]) await u.find({ t: 'a'.repeat(1980 + n) + '😀'.repeat(300) }).toArray();
  return { ok: true };
}";
        let input = serde_json::json!({ "code": script, "packages": [], "timeoutMs": 30000, "main": "docs", "mongo": mongo, "sql": [sql_entry("shop", &[])] });
        let (queries, res) = shell_run(&w, &rx, input, vec![("shop".into(), relay(&db, false))]);
        res.expect("script result");
        let ours: Vec<_> = queries.iter().filter(|q| q.conn == "docs").collect();
        let names = query_names(&queries);
        // count reports rows, never affected.
        assert!(ours.iter().any(|q| q.text.starts_with("users.count(") && q.rows == Some(4) && q.affected.is_none()), "{names}");
        assert!(ours.iter().any(|q| q.text.starts_with("users.insert(") && q.affected == Some(3)), "{names}");
        // A multi-batch find omits rows; a one-batch find has them.
        assert!(ours.iter().any(|q| q.text == "users.find({})" && q.rows.is_none() && q.error.is_none()), "{names}");
        assert!(ours.iter().any(|q| q.text == "users.find({})" && q.rows == Some(4)), "{names}");
        // A string argument is not a collection, and the error is text.
        let drop = ours.iter().find(|q| q.text.starts_with("db.dropUser(")).unwrap_or_else(|| panic!("{names}"));
        assert!(drop.error.as_deref().is_some_and(|e| !e.is_empty() && !e.contains("[object Object]")), "{:?}", drop.error);
        // Both cuts arrive as queries, so neither held a lone surrogate that serde_json refuses.
        assert_eq!(ours.iter().filter(|q| q.text.starts_with("users.find({\"t\"") && q.text.len() > 1900).count(), 2, "{names}");
    }

    #[test]
    fn live_shell_attachment_names_cannot_replace_globals() {
        let Some((w, rx, db, _p)) = live("t_glob", "id INT PRIMARY KEY") else { return };
        if bun_missing() {
            return;
        }
        let code = "async function run() { return 1 }";
        for (clash, attach_first) in [("_", false), ("dayjs", false), ("t_glob", false), ("t_glob", true)] {
            let (main, other) = (sql_entry("shop", &["t_glob"]), sql_entry(clash, &[]));
            let list = if attach_first { vec![other, main] } else { vec![main, other] };
            let input = serde_json::json!({ "code": code, "packages": [], "timeoutMs": 30000, "main": "shop", "mongo": [], "sql": list });
            let (_, res) = shell_run(&w, &rx, input, vec![("shop".into(), relay(&db, true)), (clash.into(), relay(&db, true))]);
            let err = res.unwrap_err();
            assert!(err.contains(&format!("\"{clash}\"")) && err.contains("already defines"), "{clash}: {err}");
        }
    }

    #[test]
    fn live_shell_unknown_sql_connection_name_answers_an_error() {
        let Some((w, rx, db, _p)) = live("t_att2", "id INT PRIMARY KEY") else { return };
        if bun_missing() {
            return;
        }
        // The input lists a connection the job has no relay for, as a stale attachment would.
        let input = serde_json::json!({ "code": "async function run() { return await ghost.sql`SELECT 1` }", "packages": [], "timeoutMs": 30000, "main": "shop", "mongo": [], "sql": [
            { "name": "shop", "db": "quarry_test", "tables": [] }, { "name": "ghost", "db": "quarry_test", "tables": [] }] });
        let (_, res) = shell_run(&w, &rx, input, vec![("shop".into(), relay(&db, true))]);
        assert!(res.unwrap_err().contains("ghost"));
    }

    /// Needs QUARRY_TEST_MONGO too. Seeds one document and returns the server's uri.
    fn mongo_user(w: &Worker, db: &str) -> Option<String> {
        let uri = std::env::var("QUARRY_TEST_MONGO").ok()?;
        w.rt.block_on(async {
            let c = mongodb::Client::with_uri_str(&uri).await.unwrap().database(db).collection::<Document>("users");
            c.drop().await.unwrap();
            c.insert_one(mongodb::bson::doc! { "legacyId": 7, "name": "Ada" }).await.unwrap();
        });
        Some(uri)
    }

    /// What poll.rs does with a result line.
    fn decode(res: &str) -> Document {
        let v: serde_json::Value = serde_json::from_str(res).unwrap();
        let Bson::Document(mut d) = Bson::try_from(v).unwrap() else { panic!("not a document") };
        let Some(Bson::Document(v)) = d.remove("v") else { panic!("v is not a document") };
        v
    }

    #[test]
    fn live_shell_sql_main_reads_an_attached_mongo_and_mixes_types_in_one_result() {
        let Some((w, rx, db, _p)) = live("t_att3", "id INT PRIMARY KEY") else { return };
        let Some(uri) = mongo_user(&w, "quarry_attach_a") else { return };
        if bun_missing() {
            return;
        }
        let script = "async function run() {
  const found = await travels.users.find({ legacyId: 7 }).toArray();
  const [row] = await sql`SELECT CAST(9007199254740993 AS SIGNED) AS big, X'0aff' AS bytes`;
  return { oid: ObjectId('507f1f77bcf86cd799439011'), big: row.big, bytes: row.bytes, name: found[0].name };
}";
        // A collection named like a Db method keeps the method.
        let input = serde_json::json!({ "code": script, "packages": [], "timeoutMs": 30000, "main": "shop",
            "mongo": [{ "name": "travels", "uri": uri, "db": "quarry_attach_a", "proxy": null, "collections": ["users", "command"], "readOnly": false }],
            "sql": [{ "name": "shop", "db": "quarry_test", "tables": [] }] });
        let (queries, res) = shell_run(&w, &rx, input, vec![("shop".into(), relay(&db, true))]);
        let v = decode(&res.expect("script result"));
        assert_eq!(v.get_object_id("oid").unwrap().to_hex(), "507f1f77bcf86cd799439011");
        assert_eq!(v.get_i64("big").unwrap(), 9007199254740993);
        assert_eq!(v.get_binary_generic("bytes").unwrap(), &vec![0x0a, 0xff]);
        assert_eq!(v.get_str("name").unwrap(), "Ada");
        assert!(queries.iter().any(|q| q.conn == "travels" && q.text.starts_with("users.find(") && q.text.contains("legacyId") && q.rows == Some(1)), "{}", query_names(&queries));
        assert!(queries.iter().any(|q| q.conn == "shop"));
    }

    #[test]
    fn live_shell_mongo_main_uses_an_attached_sql_connection_and_the_read_only_guard() {
        let Some((w, rx, db, _p)) = live("t_att4", "id INT PRIMARY KEY") else { return };
        let Some(uri) = mongo_user(&w, "quarry_attach_b") else { return };
        if bun_missing() {
            return;
        }
        let script = "async function run() {
  const users = await db.collection('users').find({}).toArray();
  const [{ n }] = await shop.sql`SELECT 41 + 1 AS n`;
  let refused = null;
  try { await db.collection('users').insertOne({ x: 1 }) } catch (e) { refused = e.message }
  return { users: users.length, n, refused };
}";
        let input = serde_json::json!({ "code": script, "packages": [], "timeoutMs": 30000, "main": "docs",
            "mongo": [{ "name": "docs", "uri": uri, "db": "quarry_attach_b", "proxy": null, "collections": ["users"], "readOnly": true }],
            "sql": [{ "name": "shop", "db": "quarry_test", "tables": [] }] });
        let (queries, res) = shell_run(&w, &rx, input, vec![("shop".into(), relay(&db, true))]);
        let v = decode(&res.expect("script result"));
        assert_eq!(v.get_i32("users").unwrap(), 1);
        assert_eq!(v.get_i32("n").unwrap(), 42);
        assert!(v.get_str("refused").unwrap().contains("read-only"));
        assert!(queries.iter().any(|q| q.conn == "docs" && q.text.starts_with("users.find(")) && queries.iter().any(|q| q.conn == "shop"), "{}", query_names(&queries));
    }

    #[test]
    fn live_shell_runs_sql_tables_and_refuses_writes_when_locked() {
        let Some((w, rx, db, p)) = live("t_shell", "id INT PRIMARY KEY, status ENUM('paid','open') NOT NULL, note VARCHAR(20), total DECIMAL(8,2)") else { return };
        if std::process::Command::new("bun").arg("--version").output().is_err() {
            println!("skipped: bun is not installed");
            return;
        }
        exec(&w, &p, "INSERT INTO quarry_test.t_shell VALUES (1, 'paid', 'it''s', 9.50), (2, 'open', NULL, 3.25), (3, 'paid', NULL, 1.00)");
        let script = "async function run() {
  await sql`SET @x = 41`;
  const [{ y }] = await sql`SELECT @x + 1 AS y`;
  const rows = await sql`SELECT status, COUNT(*) AS n FROM t_shell WHERE id IN (${[1, 2, 3]}) AND (note = ${\"it's\"} OR note IS NULL) GROUP BY status ORDER BY status`;
  const found = await t_shell.find({ status: 'paid' }, { orderBy: '-id' });
  const missing = await t_shell.findOne({ id: 99 });
  const count = await t_shell.count({ total: { gt: 2 } });
  console.log('count', count);
  const refused = [];
  for (const f of [() => t_shell.delete({ id: 1 }), () => t_shell.insert({ id: 9, status: 'open' }), () => sql`UPDATE t_shell SET note = 'x'`, () => sql.unsafe('DROP TABLE t_shell')]) {
    try { await f(); refused.push(null) } catch (e) { refused.push(e.message) }
  }
  return { y, rows, found, missing, count, refused, after: await t_shell.count() };
}";
        let (queries, res) = shell_script(&w, &rx, &db, script, &["t_shell"], true);
        let v: serde_json::Value = serde_json::from_str(&res.expect("script result")).unwrap();
        let v = &v["v"];
        assert_eq!(v["y"], 42, "{v}");
        assert_eq!(v["rows"].as_array().unwrap().len(), 2);
        assert_eq!(v["found"].as_array().unwrap().iter().map(|r| r["id"].as_i64().unwrap()).collect::<Vec<_>>(), [3, 1]);
        // DECIMAL stays exact text.
        assert_eq!(v["found"][1]["total"], "9.50");
        assert!(v["missing"].is_null());
        assert_eq!(v["count"], 2);
        for r in v["refused"].as_array().unwrap() {
            assert!(r.as_str().is_some_and(|m| m.contains("read-only") || m.contains("not allowed") || m.contains("Unlock")), "{v}");
        }
        // Nothing was written, and the refused statements never reached the server.
        assert_eq!(v["after"], 3);
        assert!(queries.iter().any(|q| q.error.is_some()) && queries.iter().any(|q| q.rows == Some(2)));
        // Unlocked, the same table methods write.
        let (_, res) = shell_script(&w, &rx, &db, "async function run() { await t_shell.update({ id: 2 }, { note: 'new' }); await t_shell.insert([{ id: 4, status: 'open' }, { id: 5, status: 'open' }]); return await t_shell.count() }", &["t_shell"], false);
        assert_eq!(serde_json::from_str::<serde_json::Value>(&res.unwrap()).unwrap()["v"], 5);
    }

    #[test]
    fn live_run_several_statements_with_an_empty_result() {
        let Some((w, rx, db, p)) = live("t_run", "id INT PRIMARY KEY, name VARCHAR(10)") else { return };
        exec(&w, &p, "INSERT INTO quarry_test.t_run VALUES (1, 'a'), (2, 'b')");
        let sets = run_all(&w, &rx, &db, "SELECT * FROM t_run ORDER BY id; SELECT id, name FROM t_run WHERE id > 99; SELECT 1 + 1 AS two; SET @x = 5; SELECT @x AS x", true);
        assert_eq!(sets.len(), 5);
        assert_eq!((sets[0].rows.len(), sets[0].cols.len()), (2, 2));
        assert!(sets[1].rows.is_empty() && sets[1].affected.is_none() && sets[1].error.is_none());
        assert_eq!(sets[1].cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "name"]);
        assert_eq!(sets[2].rows[0].decode().unwrap().get_i64("two"), Ok(2));
        assert!(sets[3].cols.is_empty() && sets[3].affected == Some(0));
        // The user variable lives on the script's own connection.
        assert_eq!(sets[4].rows[0].decode().unwrap().get_i64("x"), Ok(5));
        // A failing statement stops the script and says why.
        let sets = run_all(&w, &rx, &db, "SELECT 1; SELECT * FROM missing_table; SELECT 3", true);
        assert_eq!(sets.len(), 2);
        assert!(sets[1].error.as_deref().is_some_and(|m| m.contains("missing_table")));
        // A long SELECT is read a page at a time, with a row more asked for to know another page follows.
        let sets = run_all(&w, &rx, &db, "SELECT 1 AS n FROM information_schema.columns a, information_schema.columns b LIMIT 1500", true);
        if sets[0].error.is_none() {
            assert_eq!((sets[0].rows.len(), sets[0].capped), (RUN_ROWS_MAX, false));
            assert_eq!(sets[0].page.map(|p| (p.n, p.next)), Some((0, true)));
        }
    }

    #[test]
    fn live_read_only_refuses_writes_but_runs_selects() {
        let Some((w, rx, db, p)) = live("t_ro", "id INT PRIMARY KEY") else { return };
        exec(&w, &p, "INSERT INTO quarry_test.t_ro VALUES (1)");
        let text = "SELECT COUNT(*) AS n FROM t_ro; DELETE FROM t_ro; SET SESSION transaction_read_only=0; /*!DELETE FROM t_ro*/; SELECT id FROM t_ro";
        let sets = run_all(&w, &rx, &db, text, true);
        assert_eq!(sets.len(), 5);
        assert!(sets[0].error.is_none() && sets[4].error.is_none());
        for i in [1, 2, 3] {
            assert!(sets[i].error.is_some(), "{}", sets[i].stmt);
        }
        assert_eq!(sets[4].rows.len(), 1);
        // Even a statement that slipped past the check would hit the read-only session.
        let sets = run_all(&w, &rx, &db, "SELECT 1", false);
        assert!(sets[0].error.is_none());
        let pool = db.my_pool().unwrap();
        let n: i64 = w.rt.block_on(async { sqlx::query_scalar("SELECT COUNT(*) FROM quarry_test.t_ro").fetch_one(pool).await.unwrap() });
        assert_eq!(n, 1);
        // The unlocked run must not leave READ WRITE on a connection that went back to the pool. 1792 is
        // ER_CANT_EXECUTE_IN_READ_ONLY_TRANSACTION on MySQL and MariaDB alike.
        let err = w.rt.block_on(async { raw_sql("DELETE FROM quarry_test.t_ro").execute(pool).await.unwrap_err() });
        let code = err.as_database_error().and_then(|d| d.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>()).map(|d| d.number());
        assert_eq!(code, Some(1792), "{err}");
        // Unlocked, the same write runs.
        let sets = run_all(&w, &rx, &db, "DELETE FROM t_ro", false);
        assert_eq!(sets[0].affected, Some(1), "{:?}", sets[0].error);
    }

    #[test]
    fn live_kill_stops_a_running_statement() {
        let Some((w, rx, db, _p)) = live("", "") else { return };
        let h = RunHandle::default();
        let (id, cancel) = (h.conn.clone(), h.cancel.clone());
        w.sql_run(3, db.clone(), "quarry_test".into(), "SELECT SLEEP(10)".into(), true, 0, h);
        let start = Instant::now();
        while id.load(Ordering::SeqCst) == 0 {
            assert!(start.elapsed() < Duration::from_secs(5), "no connection id");
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(300));
        w.sql_kill(db, id.load(Ordering::SeqCst), cancel);
        let (mut ran, mut info) = (None, None);
        while ran.is_none() || info.is_none() {
            match next(&rx) {
                Msg::SqlRan { sets, .. } => ran = Some(sets),
                Msg::Info(r) => info = Some(r),
                _ => {}
            }
        }
        assert!(info.unwrap().is_ok());
        assert!(start.elapsed() < Duration::from_secs(6), "the sleep was not interrupted");
        assert!(ran.is_some());
        // A finished run resets the id so a late Stop cannot hit another session.
        assert_eq!(id.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn live_stop_keeps_later_statements_from_running() {
        let Some((w, rx, db, p)) = live("t_stop", "id INT PRIMARY KEY") else { return };
        exec(&w, &p, "INSERT INTO quarry_test.t_stop VALUES (1)");
        let h = RunHandle::default();
        let (id, cancel) = (h.conn.clone(), h.cancel.clone());
        // Unlocked, so the DELETE would really run if Stop only interrupted the sleep.
        w.sql_run(3, db.clone(), "quarry_test".into(), "SELECT SLEEP(5); DELETE FROM t_stop".into(), false, 0, h);
        let start = Instant::now();
        while id.load(Ordering::SeqCst) == 0 {
            assert!(start.elapsed() < Duration::from_secs(5), "no connection id");
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(300));
        w.sql_kill(db, id.load(Ordering::SeqCst), cancel);
        let sets = loop {
            if let Msg::SqlRan { sets, .. } = next(&rx) {
                break sets;
            }
        };
        // MySQL answers an interrupted SLEEP with 1, so the flag marks the DELETE; MariaDB fails the SLEEP itself.
        if sets.len() == 2 {
            assert!(sets[1].error.as_deref().is_some_and(|m| m.contains("Stopped before running")), "{:?}", sets[1].error);
        } else {
            assert!(sets[0].error.is_some());
        }
        let n: i64 = w.rt.block_on(async { sqlx::query_scalar("SELECT COUNT(*) FROM quarry_test.t_stop").fetch_one(&*p).await.unwrap() });
        assert_eq!(n, 1);
    }

    #[test]
    fn live_invisible_primary_key_is_not_a_usable_key() {
        let Some((w, rx, db, p)) = live("", "") else { return };
        let made = w.rt.block_on(async {
            let mut c = p.acquire().await.unwrap();
            raw_sql("DROP TABLE IF EXISTS quarry_test.t_inv").execute(&mut *c).await.unwrap();
            // Only MySQL 8.0.30 and later know this variable.
            if raw_sql("SET SESSION sql_generate_invisible_primary_key = ON").execute(&mut *c).await.is_err() {
                return false;
            }
            raw_sql("CREATE TABLE quarry_test.t_inv (a INT)").execute(&mut *c).await.unwrap();
            raw_sql("INSERT INTO quarry_test.t_inv VALUES (1), (2)").execute(&mut *c).await.unwrap();
            true
        });
        if !made {
            return;
        }
        let (cols, pk, rows, _) = page_of(&w, &rx, &db, "t_inv", "", "", 10, 0);
        assert!(pk.is_empty(), "{pk:?}");
        assert_eq!(cols.len(), 1);
        assert!(rows.iter().all(|d| !d.contains_key("_id")));
    }

    #[test]
    fn live_pages_are_stable_and_a_real_id_column_survives() {
        let Some((w, rx, db, p)) = live("t_stable", "_id INT, k INT, g INT, PRIMARY KEY (k)") else { return };
        let vals: Vec<String> = (0..20).map(|i| format!("({}, {i}, {})", 100 + i, i % 2)).collect();
        exec(&w, &p, &format!("INSERT INTO quarry_test.t_stable VALUES {}", vals.join(",")));
        // No order: the key orders it. A user order that ties gets the key as tie-breaker, so pages never overlap.
        let (_, _, first, _) = page_of(&w, &rx, &db, "t_stable", "", "g", 10, 0);
        let (cols, pk, second, _) = page_of(&w, &rx, &db, "t_stable", "", "g", 10, 10);
        let mut ks: Vec<i64> = first.iter().chain(&second).map(|d| d.get_i64("k").unwrap()).collect();
        ks.sort();
        assert_eq!(ks, (0..20).collect::<Vec<_>>());
        let (_, _, plain, _) = page_of(&w, &rx, &db, "t_stable", "", "", 3, 0);
        assert_eq!(plain.iter().map(|d| d.get_i64("k").unwrap()).collect::<Vec<_>>(), [0, 1, 2]);
        // The real _id column is renamed, the key keeps the _id slot.
        assert_eq!(pk, ["k"]);
        assert_eq!(cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["_id_2", "k", "g"]);
        assert_eq!(second[0].get_document("_id").unwrap(), &mongodb::bson::doc! { "k": second[0].get_i64("k").unwrap() });
        assert!(second[0].get_i64("_id_2").is_ok());
    }

    #[test]
    fn live_call_returns_each_result_set_apart() {
        let Some((w, rx, db, p)) = live("", "") else { return };
        exec(&w, &p, "DROP PROCEDURE IF EXISTS quarry_test.p_two");
        exec(&w, &p, "CREATE PROCEDURE quarry_test.p_two() BEGIN SELECT 1 AS a; SELECT 'x' AS b, 2 AS c; END");
        let sets = run_all(&w, &rx, &db, "CALL p_two(); SELECT 3 AS z", false);
        assert_eq!(sets.len(), 3, "{:?}", sets.iter().map(|s| (s.cols.len(), s.rows.len(), s.affected, s.error.clone())).collect::<Vec<_>>());
        assert_eq!(sets[0].cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["a"]);
        assert_eq!(sets[1].cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["b", "c"]);
        assert_eq!(sets[2].rows[0].decode().unwrap().get_i64("z"), Ok(3));
    }

    #[test]
    fn live_pages_follow_each_other_and_a_count_comes_apart() {
        let Some((w, rx, db, p)) = live("t_pages", "id INT PRIMARY KEY, v VARCHAR(5)") else { return };
        let vals: Vec<String> = (1..=2500).map(|i| format!("({i}, 'x')")).collect();
        exec(&w, &p, &format!("INSERT INTO quarry_test.t_pages VALUES {}", vals.join(",")));
        let stmt = "SELECT id -- the key\nFROM t_pages ORDER BY id DESC";
        let page = |n: usize| {
            w.sql_run(9, db.clone(), "quarry_test".into(), stmt.into(), true, n, Default::default());
            let Msg::SqlRan { mut sets, .. } = next(&rx) else { panic!("expected SqlRan") };
            sets.remove(0)
        };
        let (a, b, c) = (page(0), page(1), page(2));
        assert_eq!((a.rows.len(), b.rows.len(), c.rows.len()), (1000, 1000, 500));
        assert_eq!(a.stmt, stmt);
        assert_eq!((a.page.unwrap().next, b.page.unwrap().next, c.page.unwrap().next), (true, true, false));
        assert_eq!(a.rows[0].decode().unwrap().get_i64("id"), Ok(2500));
        assert_eq!(c.rows[0].decode().unwrap().get_i64("id"), Ok(500));
        assert!(a.page.unwrap().ordered);
        w.sql_count(4, 0, db.clone(), "quarry_test".into(), stmt.into());
        let Msg::SqlCount { req, idx, res } = next(&rx) else { panic!("expected SqlCount") };
        assert_eq!((req, idx, res), (4, 0, Ok(2500)));
        // A repeated column name cannot be wrapped, so it falls back to a plain run without pages.
        let dup = page_of_text(&w, &rx, &db, "SELECT 1 AS a, 2 AS a");
        assert!(dup.error.is_none() && dup.page.is_none() && dup.rows.len() == 1);
    }

    fn page_of_text(w: &Worker, rx: &Receiver<Msg>, db: &Db, text: &str) -> ResultSet {
        run_all(w, rx, db, text, true).remove(0)
    }

    #[test]
    fn live_export_streams_200k_rows_to_csv_and_json_one_batch_at_a_time() {
        use super::super::export::{export_to, Plan, Sink, BATCH};
        use crate::db::copy::Fmt;
        let Some((w, _rx, db, p)) = live("t_export_big", "id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(20)") else { return };
        exec(&w, &p, "INSERT INTO quarry_test.t_export_big (name) SELECT 'x' FROM information_schema.columns a, information_schema.columns b LIMIT 200000");
        let dir = std::env::temp_dir().join(format!("quarry-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stmt = "SELECT id, name FROM t_export_big ORDER BY id";
        let run = |fmt: Fmt, file: &str| {
            let plan = Plan { fmt, header: true, table: "t_export_big".into(), bs: true };
            let path = dir.join(file);
            let count = AtomicU64::new(0);
            let done = w.rt.block_on(export_to(&db, "quarry_test", stmt, &plan, Sink::File(path.clone()), &count, &AtomicBool::new(false), &AtomicU64::new(0))).unwrap();
            // The worker never held more than one batch, and the count the status bar reads is the whole result.
            assert!(done.peak_batch <= BATCH, "{}", done.peak_batch);
            assert_eq!((done.rows, count.load(Ordering::Relaxed)), (200_000, 200_000));
            // Written beside and renamed: no half file stays.
            assert!(!dir.join(format!(".{file}.part")).exists());
            path
        };
        let csv = std::fs::read_to_string(run(Fmt::Csv, "big.csv")).unwrap();
        assert_eq!((csv.lines().count(), csv.lines().next(), csv.lines().last()), (200_001, Some("id,name"), Some("200000,x")));
        let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(run(Fmt::Json, "big.json")).unwrap()).unwrap();
        assert_eq!((json.as_array().map(|a| a.len()), json[199_999]["id"].as_i64()), (Some(200_000), Some(200_000)));
        // A statement that writes is refused before it reaches the server, and leaves no file.
        let plan = Plan { fmt: Fmt::Csv, header: true, table: String::new(), bs: true };
        let refused = dir.join("never.csv");
        let err = w.rt.block_on(export_to(&db, "quarry_test", "DELETE FROM t_export_big", &plan, Sink::File(refused.clone()), &AtomicU64::new(0), &AtomicBool::new(false), &AtomicU64::new(0))).err().unwrap();
        assert!(err.contains("read"), "{err}");
        assert!(!refused.exists());
        let n: i64 = w.rt.block_on(async { sqlx::query_scalar("SELECT COUNT(*) FROM quarry_test.t_export_big").fetch_one(&p.pool).await.unwrap() });
        assert_eq!(n, 200_000);
        // An empty result still writes its header and footer.
        let empty = w.rt.block_on(export_to(&db, "quarry_test", "SELECT id FROM t_export_big WHERE id < 0", &Plan { fmt: Fmt::Json, header: true, table: String::new(), bs: true }, Sink::Clipboard, &AtomicU64::new(0), &AtomicBool::new(false), &AtomicU64::new(0))).unwrap();
        assert_eq!((empty.rows, empty.text.as_deref()), (0, Some("[]\n")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn live_export_all_statements_runs_one_session_and_writes_json_or_csv_files() {
        use super::super::export::{check_script, export_all, AllPlan, Plan};
        use crate::db::copy::Fmt;
        let Some((w, _rx, db, p)) = live("t_exp", "id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(20)") else { return };
        exec(&w, &p, "INSERT INTO quarry_test.t_exp (name) VALUES ('a'), ('b'), ('c')");
        let dir = std::env::temp_dir().join(format!("quarry-export-all-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let names = |d: &std::path::Path| {
            let mut v: Vec<String> = std::fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
            v.sort();
            v
        };
        // USE and @variables carry over: the table has no schema in front of it and the variable is still set.
        let script = "USE quarry_test; SET @lim = 2; SELECT id, name FROM t_exp ORDER BY id; SELECT @lim AS lim, COUNT(*) AS n FROM t_exp";
        let stmts = check_script(script, Dialect::MySql).unwrap();
        let run = |stmts: &[String], fmt: Fmt, single: bool, file: &str, cancel: bool| {
            let ap = AllPlan { plan: Plan { fmt, header: true, table: "t_exp".into(), bs: true }, single };
            let (count, step) = (AtomicU64::new(0), AtomicU64::new(0));
            let r = w.rt.block_on(export_all(&db, "", stmts, &ap, &dir.join(file), &count, &step, &AtomicBool::new(cancel), &AtomicU64::new(0)));
            (r, count.load(Ordering::Relaxed), step.load(Ordering::Relaxed))
        };

        let (r, count, step) = run(&stmts, Fmt::Json, true, "all.json", false);
        let done = r.unwrap();
        // The count the status bar reads is every row of every result, and the step is the last statement.
        assert_eq!((done.rows, done.files, count, step), (4, 1, 4, 4));
        let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("all.json")).unwrap()).unwrap();
        assert_eq!(json.as_array().map(|a| a.len()), Some(2));
        assert_eq!(json[0]["statement"], "SELECT id, name FROM t_exp ORDER BY id");
        assert_eq!(json[0]["rows"].as_array().map(|a| a.len()), Some(3));
        assert_eq!(json[1]["rows"][0]["lim"], 2);
        assert_eq!(json[1]["rows"][0]["n"], 3);

        let done = run(&stmts, Fmt::Csv, false, "res.csv", false).0.unwrap();
        assert_eq!((done.rows, done.files), (4, 2));
        assert_eq!(std::fs::read_to_string(dir.join("res-1.csv")).unwrap(), "id,name\n1,a\n2,b\n3,c\n");
        assert_eq!(std::fs::read_to_string(dir.join("res-2.csv")).unwrap(), "lim,n\n2,3\n");
        // No temporary file is left, and USE and SET made no file of their own.
        assert_eq!(names(&dir), ["all.json", "res-1.csv", "res-2.csv"]);

        // A statement that fails midway leaves nothing, not even the first result's file.
        let bad = check_script("SELECT 1 AS a; SELECT * FROM quarry_test.no_such_table_here", Dialect::MySql).unwrap();
        let err = run(&bad, Fmt::Csv, false, "bad.csv", false).0.err().unwrap();
        assert!(err.contains("no_such_table_here"), "{err}");
        let err = run(&bad, Fmt::Json, true, "bad.json", false).0.err().unwrap();
        assert!(err.contains("no_such_table_here"), "{err}");
        assert_eq!(names(&dir), ["all.json", "res-1.csv", "res-2.csv"]);

        // Cancel before the first statement writes nothing either.
        let err = run(&stmts, Fmt::Csv, false, "stop.csv", true).0.err().unwrap();
        assert_eq!(err, "Export cancelled");
        assert_eq!(names(&dir).len(), 3);

        // A script with a DELETE is refused up front and the table stays whole.
        let refused = check_script("SELECT 1; DELETE FROM t_exp", Dialect::MySql);
        assert!(refused.is_err());
        let n = |rt: &tokio::runtime::Handle, pool: &sqlx::MySqlPool| -> i64 { rt.block_on(async { sqlx::query_scalar("SELECT COUNT(*) FROM quarry_test.t_exp").fetch_one(pool).await.unwrap() }) };
        assert_eq!(n(&p.rt, &p.pool), 3);
        // The worker itself refuses it too, when a caller skips check_script.
        let sneaky = vec!["SELECT 1".to_string(), "DELETE FROM t_exp".to_string()];
        let err = run(&sneaky, Fmt::Csv, false, "sneaky.csv", false).0.err().unwrap();
        assert!(err.contains("read"), "{err}");
        assert_eq!(n(&p.rt, &p.pool), 3);
        assert_eq!(names(&dir).len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn live_a_capped_result_ends_the_script() {
        let Some((w, rx, db, _p)) = live("", "") else { return };
        exec(&w, &_p, "DROP PROCEDURE IF EXISTS quarry_test.p_big");
        exec(&w, &_p, "CREATE PROCEDURE quarry_test.p_big() BEGIN SELECT 1 AS n FROM information_schema.columns a, information_schema.columns b LIMIT 1500; END");
        // A CALL may write after it selects, so it is read to its end: the grid keeps the cap and the script goes on.
        let sets = run_all(&w, &rx, &db, "CALL p_big(); SELECT 2", false);
        assert_eq!(sets[0].rows.len(), RUN_ROWS_MAX);
        let last = sets.last().unwrap();
        assert!(last.error.is_none() && last.rows.len() == 1, "{:?}", last.error);
    }

    #[test]
    fn fill_refuses_text_that_reads_two_ways() {
        use value::Param;
        let evil = [Param::Text("x; DELETE FROM t; #".into())];
        let text = "SELECT '\\'' AS a, ? AS b -- '";
        // With backslash escapes the ? is inside a comment-free tail; without them the quote closes early.
        assert!(fill(text, &evil, true, false).is_err());
        assert!(fill(text, &evil, false, false).is_err());
        // A text both readings agree on still fills, under either mode.
        assert!(fill("SELECT ? AS b", &evil, true, false).is_ok() && fill("SELECT ? AS b", &evil, false, true).is_ok());
        // ANSI_QUOTES makes "?" an identifier, not a placeholder, which both backslash readings share.
        assert!(fill("SELECT \"a\" AS \"?\", ?", &evil, true, true).is_ok());
    }

    #[test]
    fn int_params_keep_every_digit() {
        use value::Param;
        let p = |j: serde_json::Value| crate::db::shell::sql_params_for_test(&j);
        assert_eq!(p(serde_json::json!([{ "$int": "9007199254740993" }, { "$int": "-9223372036854775808" }, { "$int": "18446744073709551615" }])).unwrap(), [Param::Int(9007199254740993), Param::Int(i64::MIN), Param::UInt(u64::MAX)]);
        assert!(p(serde_json::json!([{ "$int": "18446744073709551616" }])).is_err());
    }

    /// Needs Bun and a server. Shared by the live tests that run scripts the person could not trust.
    fn bun_ok() -> bool {
        std::process::Command::new("bun").arg("--version").output().is_ok()
    }

    #[test]
    fn live_shell_where_never_means_every_row_by_accident() {
        let Some((w, rx, db, p)) = live("t_where", "id BIGINT PRIMARY KEY, note VARCHAR(20)") else { return };
        if !bun_ok() {
            return;
        }
        exec(&w, &p, "INSERT INTO quarry_test.t_where VALUES (1, 'a'), (2, NULL), (9007199254740993, 'big')");
        let script = "async function run() {
  const bad = [5, null, [], false, 'x', { id: {} }, { id: undefined }, { id: { gt: undefined } }, { id: new Map() }];
  const out = [];
  for (const w of bad) {
    for (const f of [() => t_where.delete(w), () => t_where.update(w, { note: 'z' }), () => t_where.find(w), () => t_where.count(w)]) {
      try { await f(); out.push('ran') } catch (e) { out.push('refused') }
    }
  }
  const big = await t_where.findOne({ id: 9007199254740993n });
  let unsafe;
  try { await sql`SELECT ${2 ** 60}`; unsafe = 'ran' } catch (e) { unsafe = e.message }
  return { out, count: await t_where.count({}), big, unsafe, sum: (await sql`SELECT ${9223372036854775807n} AS v, ${18446744073709551615n} AS u`)[0] };
}";
        let (_, res) = shell_script(&w, &rx, &db, script, &["t_where"], false);
        let v: serde_json::Value = serde_json::from_str(&res.expect("script result")).unwrap();
        let v = &v["v"];
        assert!(v["out"].as_array().unwrap().iter().all(|x| x == "refused"), "{v}");
        assert_eq!(v["count"], 3, "{v}");
        assert_eq!(v["big"]["note"], "big");
        // The row id came back as an exact value, not a rounded number.
        assert_eq!(v["big"]["id"]["$numberLong"], "9007199254740993", "{v}");
        assert!(v["unsafe"].as_str().unwrap().contains("BigInt"), "{v}");
        assert_eq!(v["sum"]["v"]["$numberLong"], "9223372036854775807", "{v}");
        assert_eq!(v["sum"]["u"].to_string().trim_matches('"'), "18446744073709551615", "{v}");
    }

    #[test]
    fn live_shell_refuses_sets_that_move_the_escaping_even_when_unlocked() {
        let Some((w, rx, db, _p)) = live("", "") else { return };
        if !bun_ok() {
            return;
        }
        let script = "async function run() {
  const out = [];
  for (const t of ['SET NAMES utf8mb4', 'SET CHARACTER SET utf8mb4', 'SET CHARSET utf8mb4', 'SET SESSION character_set_client = 1', 'set @@character_set_connection = 1', \"SET sql_mode = ''\", \"SET @@session.sql_mode = ''\", \"SET @a = 1, @@sql_mode = ''\"]) {
    try { await sql.unsafe(t); out.push('ran') } catch (e) { out.push(e.message) }
  }
  await sql`SET @a = 1`;
  return out;
}";
        let (_, res) = shell_script(&w, &rx, &db, script, &[], false);
        let v: serde_json::Value = serde_json::from_str(&res.expect("script result")).unwrap();
        assert!(v["v"].as_array().unwrap().iter().all(|m| m.as_str().unwrap().contains("not allowed in the shell")), "{v}");
    }

    #[test]
    fn live_no_backslash_escapes_cannot_smuggle_a_statement() {
        let Some((w, rx, db, p)) = live("t_nb", "id INT PRIMARY KEY") else { return };
        if !bun_ok() {
            return;
        }
        exec(&w, &p, "INSERT INTO quarry_test.t_nb VALUES (1)");
        // Every session of this pool starts in NO_BACKSLASH_ESCAPES, as on a server set that way. A pool of its own
        // rather than SET GLOBAL, which raced the other live tests: their new sessions changed mode under them.
        let mut db = db;
        let opts = db.opts.my().unwrap().clone();
        let pool = w.rt.block_on(async move {
            sqlx::mysql::MySqlPoolOptions::new()
                .max_connections(2)
                .after_connect(|c, _| Box::pin(async move { sqlx::Executor::execute(c, "SET SESSION sql_mode = CONCAT(@@SESSION.sql_mode, ',NO_BACKSLASH_ESCAPES')").await.map(|_| ()) }))
                .connect_with(opts)
                .await
                .unwrap()
        });
        db.sql_mode = w.rt.block_on(async { sqlx::query_scalar::<_, String>("SELECT @@SESSION.sql_mode").fetch_one(&pool).await.unwrap() });
        assert!(db.sql_mode.contains("NO_BACKSLASH_ESCAPES"), "{}", db.sql_mode);
        db.pool = Pool::My(pool);
        let script = "async function run() {
  let r;
  try { r = await sql.unsafe(\"SELECT '\\\\'' AS a, ? AS b -- '\", ['x; DELETE FROM t_nb; #']); r = 'ran' } catch (e) { r = e.message }
  return { r, left: await t_nb.count() };
}";
        let (_, res) = shell_script(&w, &rx, &db, script, &["t_nb"], false);
        let v: serde_json::Value = serde_json::from_str(&res.expect("script result")).unwrap();
        assert_eq!(v["v"]["left"], 1, "{v}");
        assert!(v["v"]["r"].as_str().unwrap().contains("differently"), "{v}");
    }
}
