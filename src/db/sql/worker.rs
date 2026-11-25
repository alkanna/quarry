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
        if let Err(err) = c.execute(sqlx::AssertSqlSafe(q.as_str())).await {
            return Err(adv.failed(format!("{q}: {}", e(err))));
        }
    }
    // Old servers and proxies may lack the variable; a missing timeout is better than no connection, but a
    // missing read-only guard is not, so only that one fails the connect.
    let t = d.session_timeout(ms);
    let _ = c.execute(sqlx::AssertSqlSafe(t.as_str())).await;
    if adv.init.is_empty() {
        return Ok(());
    }
    // Prepared, so the server parses each one as exactly one statement whatever its quotes hide.
    for q in &adv.init {
        if let Err(err) = sqlx::query(sqlx::AssertSqlSafe(q.as_str())).persistent(false).execute(&mut *c).await {
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

/// Adds the client certificate and key. Both files are read once here so a missing file or a wrong kind of file is
/// named at once instead of surfacing as a handshake error. sqlx reads unencrypted PEM keys only and MySQL has no
/// passphrase field, so an encrypted key is refused with the way out. The server sees no certificate when TLS is off.
pub(super) fn with_client_cert(o: MySqlConnectOptions, a: &Addr) -> Res<MySqlConnectOptions> {
    use super::pg::cert::{expand, load, KEY_LOCKED, KEY_OLD_CIPHER};
    if a.ssl_mode == SslMode::Disabled {
        return Ok(o);
    }
    match load(&a.ssl_cert, &a.ssl_key, None) {
        Ok(None) => Ok(o),
        Ok(Some(_)) => Ok(o.ssl_client_cert(expand(a.ssl_cert.trim())).ssl_client_key(expand(a.ssl_key.trim()))),
        Err(m) if m == KEY_LOCKED || m == KEY_OLD_CIPHER => Err("The client key is encrypted, and MySQL connections cannot use a passphrase. Save an unencrypted copy of it (openssl pkey -in key.pem -out key-plain.pem) and choose that file".into()),
        Err(m) => Err(m),
    }
}

async fn open(uri: &str, timeout: Duration, net: &Net, adv: &Advanced) -> Res<(Db, Vec<String>, Option<crate::db::Way>)> {
    open_full(uri, timeout, net, adv, false).await.map(|(db, dbs, t, _)| (db, dbs, t))
}

/// Opens the SSH tunnel or the SOCKS5 proxy forwarder to the address, whichever the connection has; None when it has neither.
/// `named` also asks for the private socket, which matters only when sqlx checks the certificate's name.
pub(super) async fn open_carrier(net: &Net, a: &Addr, named: bool, timeout: Duration) -> Res<Option<crate::db::Way>> {
    let to = (a.host.clone(), a.port);
    Ok(match (&net.ssh, &net.proxy) {
        (Some((conf, secrets)), _) => Some(crate::db::Way::Ssh(crate::ssh::open(conf, secrets, timeout, Some(to), named).await?)),
        (None, Some(p)) => Some(crate::db::Way::Proxy(crate::ssh::socks_forward(p, to, named, timeout).await?)),
        _ => None,
    })
}

/// `info` also reads what the form's Test button shows: the server's default sql_mode and its UTF-8 collations.
async fn open_full(uri: &str, timeout: Duration, net: &Net, adv: &Advanced, info: bool) -> Res<(Db, Vec<String>, Option<crate::db::Way>, TestInfo)> {
    // Checked before anything connects, so a bad value never costs a tunnel or a login.
    let a = Addr::parse(uri).ok_or("Not a SQL address")?;
    if net.cloud_sql.is_some() && net.ssh.is_some() {
        return Err("Cloud SQL and an SSH tunnel cannot be used together".into());
    }
    if net.cloud_sql.is_some() && net.proxy.is_some() {
        return Err("Cloud SQL and a SOCKS5 proxy cannot be used together".into());
    }
    if net.ssh.is_some() && net.proxy.is_some() {
        return Err("An SSH tunnel and a SOCKS5 proxy cannot be used together".into());
    }
    if a.engine() == super::Engine::Postgres {
        return super::pg::open(a, timeout, net, adv, info).await;
    }
    let setup = Setup::new(adv.session_statements(super::advanced::local_offset_secs())?, super::advanced::init_statements(&adv.init)?);
    let (timeout, ms) = adv.timeouts(timeout);
    // Held until the connection is dropped, which ends the proxy.
    let cloud = match &net.cloud_sql {
        Some(conf) => Some(crate::cloudsql::open_found(conf, &net.tools, timeout).await?),
        None => None,
    };
    // The socket only matters when sqlx checks the certificate's name.
    let named = matches!(a.ssl_mode, SslMode::VerifyCa | SslMode::VerifyIdentity);
    let carrier = open_carrier(net, &a, named, timeout).await?;
    let tunnel = carrier.as_ref().and_then(crate::db::Way::tunnel);
    // sqlx checks the certificate against the host it is given. Through the tunnel's socket that stays the name typed;
    // Windows has only the tunnel's port, so there the name checked is 127.0.0.1.
    let (host, port) = match (tunnel, &cloud) {
        (Some(t), _) if t.socket.is_none() => ("127.0.0.1".to_string(), t.port),
        (_, Some(p)) => ("127.0.0.1".to_string(), p.port),
        _ => (a.host.clone(), a.port),
    };
    // The proxy already encrypts to the instance, and the instance's certificate would not match 127.0.0.1.
    let ssl = if cloud.is_some() { SslMode::Disabled } else { a.ssl_mode };
    // The sqlx defaults change how the server reads user SQL (|| as concat, UTC), so they are turned off.
    let mut o = MySqlConnectOptions::new().host(&host).port(port).username(&a.user).ssl_mode(ssl_mode(ssl)).pipes_as_concat(false).no_engine_substitution(false).timezone(None);
    // An IAM sign-in has no password: the server asks for the token as a cleartext password, and the proxy supplies it.
    if net.cloud_sql.as_ref().is_some_and(|c| c.iam) {
        o = o.enable_cleartext_plugin(true);
    }
    if let Some(s) = tunnel.and_then(|t| t.socket.as_ref()) {
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
    o = with_client_cert(o, &a)?;
    // The pool retries a connection whose setup failed until its timeout, so one plain connection runs the setup
    // first and a failing setting or init command is reported at once, with its statement.
    let first = async {
        let mut c = sqlx::MySqlConnection::connect_with(&o).await.map_err(|x| crate::db::mysql_name_note(se(x), a.ssl_mode == SslMode::VerifyCa))?;
        let r = session(&mut c, ms, &setup).await;
        let _ = c.close().await;
        r.map_err(|err| setup.failed_text().unwrap_or_else(|| e(err)))
    };
    let way = carrier.or(cloud.map(crate::db::Way::CloudSql));
    tokio::time::timeout(timeout, first).await.map_err(|_| format!("Could not connect and set up the session within {} s", timeout.as_secs()))?.map_err(|m| crate::db::Way::blame_proxy(way.as_ref(), m))?;
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
    Ok((db, dbs, way, ti))
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
/// (a domain, an enum), so its kinds are read from `pg_type`. A MySQL table leaves `kinds` empty, except for the MariaDB
/// columns that are JSON in all but name.
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
    // A unique key lets several rows hold NULL, so a NULL key value addresses no single row.
    let idx: Option<Vec<usize>> = pk.iter().map(|p| row.col_pos(p)).collect();
    if let Some(idx) = idx.filter(|i| !i.is_empty() && i.iter().all(|&i| vals[i] != Bson::Null)) {
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
    let res = super::pg::read::in_read_only(&mut conn, db, "SET LOCAL search_path = pg_catalog", async |c| sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).persistent(false).bind(schema).bind(table).fetch_all(c).await).await;
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

/// On MariaDB, the LONGTEXT columns declared JSON, as (table, column) pairs for the whole schema or for one table. They read as
/// `Kind::Json` everywhere a column kind comes from. A server without CHECK_CONSTRAINTS (before 10.2) or a login that cannot
/// read it just leaves them as text.
pub(super) async fn json_alias_cols(db: &Db, schema: &str, table: Option<&str>) -> HashSet<(String, String)> {
    if db.dialect != Dialect::MariaDb {
        return HashSet::new();
    }
    let rows = match table {
        Some(t) => db.meta(schema, db.dialect.json_checks(true), [schema, t]).await,
        None => db.meta(schema, db.dialect.json_checks(false), [schema]).await,
    };
    super::structure::json_check_pairs(&rows.unwrap_or_default()).into_iter().collect()
}

/// Column types and the primary key, from information_schema.
pub(super) async fn table_meta(db: &Db, schema: &str, table: &str) -> Res<(HashMap<String, (String, bool, String)>, HashMap<String, Kind>, Vec<String>)> {
    let d = db.dialect;
    let cols = async { if d == Dialect::Postgres { pg_columns(db, schema, table).await } else { db.meta(schema, d.columns(), [schema, table]).await } };
    let (cols, idx, json) = tokio::try_join!(cols, db.meta(schema, d.indexes(), [schema, table]), async { Ok::<_, String>(json_alias_cols(db, schema, Some(table)).await) })?;
    let meta = cols
        .iter()
        .filter_map(|r| {
            let nullable = r.int("nullable").map(|n| n != 0).unwrap_or(true);
            let extra = if d == Dialect::Postgres { pg_extra(r) } else { text(r, "extra").unwrap_or_default() };
            Some((text(r, "name")?, (text(r, "full_type")?, nullable, extra)))
        })
        .collect();
    let kinds = if d == Dialect::Postgres {
        cols.iter().filter_map(|r| Some((text(r, "name")?, pg_kind(r)))).collect()
    } else {
        cols.iter().filter(|r| text(r, "full_type").is_some_and(|t| t.eq_ignore_ascii_case("longtext"))).filter_map(|r| text(r, "name")).filter(|n| json.contains(&(table.to_string(), n.clone()))).map(|n| (n, Kind::Json)).collect()
    };
    let pk = row_key(d, &idx);
    Ok((meta, kinds, pk))
}

/// The columns that address one row: the primary key, else the best unique index on plain columns (`unique_key`).
pub(super) fn row_key(d: Dialect, idx: &[SqlRow]) -> Vec<String> {
    // A Postgres key is named by the table, so the index query says which one is the primary key. Read straight from the
    // rows: this runs on every page, and most tables have one, so the full index list is built only without it.
    let pk: Vec<String> = idx.iter().filter(|r| if d == Dialect::Postgres { r.int("is_primary") == Some(1) } else { text(r, "name").as_deref() == Some("PRIMARY") }).filter_map(|r| text(r, "col")).collect();
    if !pk.is_empty() {
        return pk;
    }
    let all = super::structure::indexes(idx);
    let nullable: HashSet<String> = idx.iter().filter(|r| text(r, "nullable").is_some_and(|n| !n.is_empty())).filter_map(|r| text(r, "col")).collect();
    // A failed CREATE UNIQUE INDEX CONCURRENTLY leaves an invalid index that may sit over duplicates.
    let invalid: HashSet<String> = idx.iter().filter(|r| r.int("valid") == Some(0)).filter_map(|r| text(r, "name")).collect();
    unique_key(all.iter().filter(|i| !invalid.contains(&i.name)), &nullable)
}

/// The key of a table without a primary key: a unique index on plain columns, NOT NULL ones first because UNIQUE lets
/// several rows hold NULL, and those rows cannot be told apart. Then the fewest columns, then the name. A Postgres index
/// that is `complex` may compare with another collation or operator class than `=` on the column, so it is skipped.
fn unique_key<'a>(all: impl Iterator<Item = &'a super::structure::Index>, nullable: &HashSet<String>) -> Vec<String> {
    let plain = |i: &&super::structure::Index| i.unique && !i.complex && i.predicate.is_empty() && i.cols.iter().all(|c| !c.name.is_empty() && c.expr.is_none() && c.prefix.is_none());
    all.filter(plain).min_by_key(|i| (i.cols.iter().any(|c| nullable.contains(&c.name)), i.cols.len())).map(|i| i.cols.iter().map(|c| c.name.clone()).collect()).unwrap_or_default()
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

/// The kind of a catalog column on either engine. MariaDB keeps JSON in a longtext column, so a longtext that
/// has a JSON_VALID check reads as JSON.
pub(super) fn column_kind<R: Cells>(d: Dialect, r: &R, table: &str, name: &str, full: &str, json: &HashSet<(String, String)>) -> Kind {
    if d == Dialect::Postgres { pg_kind(r) } else if full.eq_ignore_ascii_case("longtext") && json.contains(&(table.to_string(), name.to_string())) { Kind::Json } else { kind_of_full(full) }
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
    let json = json_alias_cols(db, schema, None).await;
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
        let kind = column_kind(d, r, &t, &name, &ty, &json);
        table.cols.push(SchemaCol { fk: fk.contains(&(t, name.clone())), kind, key: text(r, "col_key").unwrap_or_default(), nullable, name, ty });
    }
    Ok(Schema { tables: out })
}

/// Below this many rows a table is cheap enough to group in full while someone types.
pub(super) const VALUES_SMALL: u64 = 100_000;

/// What a lookup may afford on a table: `small` when it can be scanned in full, `leads` when an index leads with the
/// column (always false for an empty column).
pub(super) struct Gate {
    pub small: bool,
    pub leads: bool,
}

/// Reads metadata only, except on a PostgreSQL table never analyzed: its estimate is NULL, so a bounded count with a
/// 300 ms limit stands in for it, and a table too big or too slow to count is not small.
pub(super) async fn gate(db: &Db, schema: &str, table: &str, column: &str) -> Res<Option<Gate>> {
    let d = db.dialect;
    let row = if d == Dialect::Postgres {
        db.meta(schema, d.value_gate(), [column, schema, table]).await?.into_iter().next()
    } else {
        sqlx::query(d.value_gate()).bind(column).bind(schema).bind(table).fetch_optional(db.my_pool()?).await.map_err(se)?.map(SqlRow::My)
    };
    let Some(row) = row else { return Ok(None) };
    let est = super::structure::num(&row, "est");
    let leads = super::structure::num(&row, "leads").unwrap_or(0) > 0;
    let small = match est {
        Some(n) => n < VALUES_SMALL,
        None if d == Dialect::Postgres && !leads => pg_small(db, schema, table).await?,
        None => false,
    };
    Ok(Some(Gate { small, leads }))
}

/// A count of at most VALUES_SMALL + 1 rows on a connection of its own.
async fn pg_small(db: &Db, schema: &str, table: &str) -> Res<bool> {
    let mut c = db.detached().await?;
    let Conn::Pg(pc) = &mut c else { return Err("Not a PostgreSQL connection".to_string()) };
    // A SET LOCAL in the read-only transaction, never a session value, because a pooler would hand the session on.
    let short = super::pg::set_timeout("300");
    let sql = format!("SELECT count(*)::text FROM (SELECT 1 FROM {} LIMIT {}) AS q", db.dialect.qualified(schema, table), VALUES_SMALL + 1);
    let n = super::pg::read::in_read_only(pc, db, &short, async |c| c.fetch_all(sqlx::AssertSqlSafe(sql.as_str())).await).await;
    let _ = c.close().await;
    Ok(matches!(n, Ok(rows) if rows.first().and_then(|r| r.try_get::<String, _>(0).ok()).and_then(|n| n.parse::<u64>().ok()).is_some_and(|n| n <= VALUES_SMALL)))
}

/// A column's most frequent values, or None where looking would scan a big table (see `gate`). The lookup runs on a
/// connection of its own with a 300 ms limit, so a slow one never holds the pool.
async fn field_values(db: &Db, schema: &str, table: &str, column: &str, prefix: Option<&str>) -> Res<Option<Vec<(String, u64)>>> {
    let d = db.dialect;
    let Some(g) = gate(db, schema, table, column).await? else { return Ok(None) };
    if !g.small && !g.leads {
        return Ok(None);
    }
    if d == Dialect::Postgres {
        return pg_field_values(db, schema, table, column, prefix).await;
    }
    let mut c = super::run::side_conn(db, "Could not reach the server").await?.into_my()?;
    let res = async {
        // The pool's own setup, so values read in the same zone and charset as the grid shows them.
        session(&mut c, 300, &db.setup).await.map_err(|x| db.setup.failed_text().unwrap_or_else(|| e(x)))?;
        // The init command may raise the statement limit, and this lookup must stay short.
        let t = d.session_timeout(300);
        let _ = c.execute(sqlx::AssertSqlSafe(t.as_str())).await;
        let sql = d.value_counts(schema, table, column, prefix.is_some(), 201);
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
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

/// `field_values` on PostgreSQL, once `gate` allowed it.
async fn pg_field_values(db: &Db, schema: &str, table: &str, column: &str, prefix: Option<&str>) -> Res<Option<Vec<(String, u64)>>> {
    let d = db.dialect;
    let mut c = db.detached().await?;
    let res = async {
        let Conn::Pg(pc) = &mut c else { return Err("Not a PostgreSQL connection".to_string()) };
        // The lookup must stay short, so a slow one gives up instead of holding a connection. The limit is a SET LOCAL in
        // the read-only transaction of each read, never a session value, because a pooler would hand the session on.
        let short = super::pg::set_timeout("300");
        let sql = d.value_counts(schema, table, column, prefix.is_some(), 201);
        let pattern = prefix.map(|p| format!("{}%", super::rules::like_bang(p)));
        let t = Instant::now();
        let rows = super::pg::read::in_read_only(pc, db, &short, async |c| {
            let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).persistent(false);
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
                    Some(crate::db::Way::Proxy(_)) => " through the SOCKS5 proxy".to_string(),
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
        let (tx, wake) = (self.tx.clone(), self.wake.clone());
        self.rt.spawn(async move {
            let res = match page(&db, &schema, &table, &q, limit, offset, true).await {
                Ok((cols, pk, rows, count, estimated, ms, matched)) => {
                    let _ = tx.send(Msg::SqlCols { req, cols, pk, estimated, matched });
                    Ok((rows, count, ms))
                }
                Err(err) => Err(err),
            };
            let _ = tx.send(Msg::Docs { req, res, approx: false });
            wake();
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

    /// Runs the explain `script` on a session of its own, the way a Query run does, so the read-only guard, the statement
    /// timeout and the pooler rules apply to it as they do there.
    /// `h` is the handle `sql_kill` takes, because an ANALYZE runs the statement and can hold locks until it ends.
    pub fn sql_explain(&self, req: u64, db: Db, schema: String, script: String, read_only: bool, h: RunHandle) {
        self.spawn(async move {
            let sets = run(&db, &schema, &script, read_only, &h.conn, &h.cancel, 0).await;
            h.conn.store(0, Ordering::SeqCst);
            Msg::SqlExplained { req, sets }
        });
    }

    /// What the catalog says about the tables an explained plan scans, for its index suggestions. Never fails: a part that
    /// cannot be read is left empty and the dialog keeps the plan's own findings.
    pub fn sql_explain_evidence(&self, req: u64, db: Db, schema: String, wants: Vec<super::plan::evidence::Want>) {
        self.spawn(async move { Msg::SqlExplainEvidence { req, ev: super::plan::evidence::load(&db, &schema, &wants).await } });
    }

    /// Plans `stmt` again with the candidate index `create` as a HypoPG hypothetical index, on a connection of its own
    /// that is closed afterwards. Nothing is created; see `pg::hypo`.
    pub fn sql_explain_hypo(&self, req: u64, db: Db, schema: String, create: String, stmt: String, read_only: bool) {
        self.spawn(async move { Msg::SqlExplainHypo { req, res: super::pg::hypo::trial(&db, &schema, &create, &stmt, read_only).await } });
    }

    /// The row count of a paged statement, read in the background on a connection of its own under the
    /// statement timeout. `idx` is the result tab it belongs to.
    pub fn sql_count(&self, req: u64, idx: usize, db: Db, schema: String, stmt: String) {
        self.spawn(async move {
            let res = super::run::count_rows(&db, &schema, &stmt).await;
            Msg::SqlCount { req, idx, res }
        });
    }

    /// Parses `stmt` in `schema` without running it, for the Query editor's marks. Never logged: it runs on every pause in typing.
    pub fn sql_lint(&self, req: u64, db: Db, schema: String, stmt: String) {
        self.spawn(async move { Msg::SqlLint { req, lint: super::lint::check(&db, &schema, &stmt).await } });
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
#[path = "worker_tests.rs"]
mod tests;
