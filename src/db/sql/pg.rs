//! PostgreSQL connections: one pool per database, the session guard, the version gate, Stop and the database list.
//! A Postgres connection belongs to one database and there is no USE, so the pool is picked by the database the
//! `Db`'s options name (see `Db::at`).

use super::se;
use super::exec::{Cells, Conn, Opts};
use super::worker::{num_at, text_at};
use super::{Addr, Db, Dialect, Pool, SslMode, TestInfo};
use crate::db::{e, Net, Res};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions, PgSslMode};
use sqlx::{Connection, Executor};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const NOT_YET: &str = "not available for PostgreSQL yet";

/// Older servers lack features the catalog queries rely on, and 13 is the oldest Quarry is tested on.
const MIN_VERSION: u32 = 130_000;
/// Several pools per connection count against the server's max_connections, so both caps stay small.
const PER_DB: u32 = 3;
const MAX_POOLS: usize = 4;
const IDLE: Duration = Duration::from_secs(300);
/// The database every server has, used when the address names none.
const FALLBACK_DB: &str = "postgres";

/// Most start times kept per connection. A run that ends without Stop leaves its entry, so the map is cleared past this
/// instead of growing for the life of the connection.
const MAX_STARTS: usize = 256;

/// The pools of one Postgres connection.
pub struct PgPools {
    /// The login without a database; each pool adds its own.
    opts: PgConnectOptions,
    default_db: String,
    acquire: Duration,
    idle: Duration,
    per_db: u32,
    max_pools: usize,
    /// Set when the server refused startup options (a pooler such as PgBouncer). A session-level SET would land on
    /// whichever backend answered and could leak to another client of the pooler, so nothing is set on the session:
    /// every read and write runs in a transaction that starts with these SET LOCAL statements instead.
    locals: Option<String>,
    /// The statement_timeout the server gave the session at connect, spelled as the server spells it ("0", "30s"). A
    /// transaction applies it again with SET LOCAL, because behind a pooler a session value would not hold.
    timeout: String,
    /// A pool wraps what a connection hook failed with in a bare timeout, so the real message is kept here.
    failure: Arc<Mutex<Option<String>>>,
    live: Mutex<Live>,
    /// What the first line of a Stop needs and a pid alone cannot give: the time each backend started, so a pid the server
    /// reused for another session is never cancelled. Kept per connection because two servers hand out the same pids.
    starts: Mutex<HashMap<u64, String>>,
}

#[derive(Default)]
struct Live {
    tick: u64,
    pools: HashMap<String, (PgPool, u64)>,
}

impl PgPools {
    fn new(opts: PgConnectOptions, default_db: String, acquire: Duration, locals: Option<String>, timeout: String) -> PgPools {
        PgPools { opts, default_db, acquire, idle: IDLE, per_db: PER_DB, max_pools: MAX_POOLS, locals, timeout, failure: Default::default(), live: Default::default(), starts: Default::default() }
    }

    /// The SET LOCAL statements every transaction of a pooler-fallback connection starts with, or an empty string when
    /// the startup options carry the session.
    pub fn locals(&self) -> &str {
        self.locals.as_deref().unwrap_or_default()
    }

    /// The session's statement_timeout as read at connect.
    pub fn timeout(&self) -> &str {
        &self.timeout
    }

    pub(super) fn remember_start(&self, pid: u64, start: String) {
        let mut m = self.starts.lock().unwrap();
        if m.len() >= MAX_STARTS {
            m.clear();
        }
        m.insert(pid, start);
    }

    pub(super) fn recall_start(&self, pid: u64) -> Option<String> {
        self.starts.lock().unwrap().get(&pid).cloned()
    }

    /// The pool of `database`, made on first use. Making it opens no connection. Past the cap the pool used least
    /// recently leaves the map but is not closed: closing makes every acquire on a clone someone still holds fail with
    /// PoolClosed. The last handle and the idle timeout reap it.
    pub fn pool(&self, database: Option<&str>) -> PgPool {
        let name = database.filter(|d| !d.is_empty()).unwrap_or(&self.default_db).to_string();
        let mut live = self.live.lock().unwrap();
        live.tick += 1;
        let tick = live.tick;
        if let Some((p, used)) = live.pools.get_mut(&name) {
            *used = tick;
            return p.clone();
        }
        while live.pools.len() >= self.max_pools {
            let Some(oldest) = live.pools.iter().min_by_key(|(_, (_, used))| *used).map(|(k, _)| k.clone()) else { break };
            live.pools.remove(&oldest);
        }
        let (locals, failure) = (self.locals.clone(), self.failure.clone());
        let pool = PgPoolOptions::new()
            .max_connections(self.per_db)
            .min_connections(0)
            .idle_timeout(self.idle)
            .acquire_timeout(self.acquire)
            .after_connect(move |c, _| {
                let (locals, failure) = (locals.clone(), failure.clone());
                Box::pin(async move { session(c, locals.as_deref(), &failure).await })
            })
            .connect_lazy_with(self.opts.clone().database(&name));
        live.pools.insert(name, (pool.clone(), tick));
        pool
    }

    /// Names of the databases that have a live pool.
    #[cfg(test)]
    fn live_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.live.lock().unwrap().pools.keys().cloned().collect();
        v.sort();
        v
    }

    /// A pooled connection of `database`. When new connections fail the session guard the error says so instead of
    /// a bare timeout.
    pub async fn acquire(&self, database: Option<&str>) -> Res<sqlx::pool::PoolConnection<sqlx::Postgres>> {
        self.pool(database).acquire().await.map_err(|x| match x {
            sqlx::Error::PoolTimedOut => self.failure.lock().unwrap().clone().unwrap_or_else(|| e(x)),
            x => e(x),
        })
    }

    /// A connection of its own, outside the pools, with the same session as theirs.
    pub async fn detached(&self, opts: &Opts) -> Res<Conn> {
        let Opts::Pg(o) = opts else { return Err("Not a PostgreSQL connection".into()) };
        let mut c = sqlx::PgConnection::connect_with(o).await.map_err(se)?;
        session(&mut c, self.locals.as_deref(), &self.failure).await.map_err(|x| self.failure.lock().unwrap().clone().unwrap_or_else(|| e(x)))?;
        Ok(Conn::Pg(c))
    }
}

impl Db {
    /// The same server with the options pointing at `database`, which is what picks the pool. A MySQL connection
    /// selects its database per statement, so it is returned as it is.
    pub fn at(&self, database: &str) -> Db {
        match &self.opts {
            Opts::Pg(o) if !database.is_empty() => Db { opts: Opts::Pg(o.clone().database(database)), ..self.clone() },
            _ => self.clone(),
        }
    }

    /// The database a PostgreSQL `Db` points at, empty on MySQL. Answers that name it tell the app which database of a
    /// connection they belong to.
    pub fn pg_database(&self) -> String {
        match &self.opts {
            Opts::Pg(o) => o.get_database().unwrap_or_default().to_string(),
            Opts::My(_) => String::new(),
        }
    }

    /// The pool of the database this `Db` points at.
    pub fn pg_pool(&self) -> Res<PgPool> {
        match (&self.pool, &self.opts) {
            (Pool::Pg(p), Opts::Pg(o)) => Ok(p.pool(o.get_database())),
            _ => Err("Not a PostgreSQL connection".into()),
        }
    }

    /// Whether the server refused startup options, which is what a connection pooler such as PgBouncer does. Nothing
    /// may then be set on a session and Stop cannot cancel by pid.
    pub fn pooler(&self) -> bool {
        matches!(&self.pool, Pool::Pg(p) if p.locals.is_some())
    }

    /// What every transaction of one script run adds behind a pooler, on top of what `pg_begin` sends: a `SET LOCAL
    /// search_path` for the tab's schema, because a session search_path would land on whichever backend answered and leak
    /// to other clients. The name is written as the quoted identifier `quote_ident` makes, not as a string literal, because
    /// SET reads a literal as one schema named with the quotes in it. Empty when startup options carry the session (normal
    /// mode sets the search_path once per run).
    pub fn pg_run_locals(&self, schema: &str) -> String {
        if !self.pooler() || schema.is_empty() {
            return String::new();
        }
        format!("SET LOCAL search_path = {}", Dialect::Postgres.quote_ident(schema))
    }

    /// The same server as `self` with its pool built the way a pooler that refused startup options leaves it, for
    /// tests of the fallback mode.
    #[cfg(test)]
    pub fn forced_pooler(&self, uri: &str, ms: u64) -> Db {
        use std::str::FromStr;
        let base = PgConnectOptions::from_str(uri).unwrap();
        let Opts::Pg(o) = &self.opts else { panic!("not postgres") };
        let p = PgPools::new(base.clone(), "postgres".into(), Duration::from_secs(10), Some(locals_for(ms)), "0".into());
        Db { pool: Pool::Pg(Arc::new(p)), opts: Opts::Pg(base.database(o.get_database().unwrap_or("postgres"))), ..self.clone() }
    }
}

/// The output formats the read path expects. sqlx sends DateStyle "ISO, MDY", TimeZone UTC and extra_float_digits 2 itself;
/// these are the ones it leaves out. One list feeds the startup options, the pooler fallback and every write transaction,
/// so a setting added here cannot go missing behind a pooler.
const FORMATS: [(&str, &str); 3] = [("bytea_output", "hex"), ("DateStyle", "ISO"), ("IntervalStyle", "postgres")];

fn set_local(k: &str, v: &str) -> String {
    format!("SET LOCAL {k} = '{v}'")
}

/// The one spelling of a transaction's statement timeout. A bare number is milliseconds and "0" turns the limit off.
pub(super) fn set_timeout(v: &str) -> String {
    set_local("statement_timeout", v)
}

/// Startup parameters the session needs. `statement_timeout` is left out when the limit is off.
fn startup(o: PgConnectOptions, ms: u64) -> PgConnectOptions {
    let mut opts: Vec<(&str, String)> = vec![("default_transaction_read_only", "on".into())];
    if ms > 0 {
        opts.push(("statement_timeout", ms.to_string()));
    }
    opts.extend(FORMATS.map(|(k, v)| (k, v.to_string())));
    o.application_name("Quarry").options(opts)
}

/// The settings of `startup` that are not the read-only guard, as SET LOCAL statements for a server that refuses startup
/// options. The guard itself is the `BEGIN READ ONLY` every read opens.
fn locals_for(ms: u64) -> String {
    let mut v = Vec::new();
    if ms > 0 {
        v.push(set_timeout(&ms.to_string()));
    }
    v.extend(FORMATS.map(|(k, v)| set_local(k, v)));
    v.join("; ")
}

/// A `Db` that behaves as behind a pooler that refused startup options, so tests of other modules can force that mode.
#[cfg(test)]
pub(super) fn pooled_for_test(base: &Db, uri: &str) -> Db {
    use std::str::FromStr;
    let p = PgPools::new(PgConnectOptions::from_str(uri).unwrap(), "postgres".into(), Duration::from_secs(10), Some(locals_for(30_000)), "0".into());
    Db { pool: Pool::Pg(Arc::new(p)), ..base.clone() }
}

/// What every write transaction starts with, whatever the session carries: no statement timeout, because an ALTER or a
/// big update must not be cut by the limit meant for reads, a lock timeout so a write never queues behind another
/// session's lock for ever, and the output formats the read path expects.
pub(super) fn write_locals() -> String {
    let mut v = vec![set_timeout("0"), set_local("lock_timeout", "10s")];
    v.extend(FORMATS.map(|(k, x)| set_local(k, x)));
    v.join("; ")
}

/// A pooler answers a startup `options` parameter with "unsupported startup parameter".
fn options_refused(err: &sqlx::Error) -> bool {
    let m = err.to_string().to_ascii_lowercase();
    m.contains("startup parameter") || m.contains("unsupported") && m.contains("options")
}

const SCS_OFF: &str = "The server has standard_conforming_strings off, which Quarry cannot read safely, so the connection was refused";
const NOT_READ_ONLY: &str = "The session was not read-only, so the connection was refused";

/// A session whose strings read a backslash as an escape would make the lexer's reading of user text wrong.
fn scs_check(v: Option<&[u8]>) -> Result<(), String> {
    if v == Some(b"on") { Ok(()) } else { Err(SCS_OFF.into()) }
}

/// Reads the two settings the read-only guard depends on, for a connection of a pool. A session that could write, or
/// whose strings read a backslash as an escape, refuses the connection.
pub(super) async fn readback(c: &mut sqlx::PgConnection) -> Result<(), String> {
    let rows = c.fetch_all("SELECT current_setting('default_transaction_read_only'), current_setting('standard_conforming_strings')").await.map_err(se)?;
    let row = rows.first().ok_or("The server did not report its session settings")?;
    if row.cell(0) != Some(b"on") {
        return Err(NOT_READ_ONLY.into());
    }
    scs_check(row.cell(1))
}

const POOLED: &str = "This server sits behind a connection pooler that gives each statement to a different backend (statement pooling). Quarry needs session or transaction pooling, so the connection was refused";

/// The check of a connection behind a pooler that refused startup options, in one transaction because nothing may be set on
/// the session. It refuses statement pooling, where BEGIN is not allowed or the statements of one transaction are answered
/// by different backends, and reads `standard_conforming_strings`. The read-only guard is not read here, since each read
/// opens its own `BEGIN READ ONLY`. The first read rides in the same message as BEGIN and the SET LOCALs, and a second one
/// has to come from the same backend.
async fn pooler_check(c: &mut sqlx::PgConnection, locals: &str) -> Result<(), String> {
    let sql = format!("BEGIN READ ONLY; {locals}; SELECT pg_backend_pid()::text, current_setting('standard_conforming_strings')");
    let rows = c.fetch_all(sql.as_str()).await.map_err(|x| format!("{POOLED} ({})", e(x)))?;
    let row = rows.first().ok_or("The server did not report its session settings")?;
    scs_check(row.cell(1))?;
    let first = row.cell(0).map(<[u8]>::to_vec);
    let again = c.fetch_all("SELECT pg_backend_pid()::text").await.map_err(se)?;
    if again.first().and_then(|r| r.cell(0)) != first.as_deref() {
        return Err(POOLED.into());
    }
    let _ = c.execute("ROLLBACK").await;
    Ok(())
}

/// What every connection of a pool gets after it opens. With startup options the guard is read back; behind a pooler
/// (`locals` is set) nothing is set or read on the session and the pooler is checked instead.
pub(super) async fn session(c: &mut sqlx::PgConnection, locals: Option<&str>, failure: &Mutex<Option<String>>) -> Result<(), sqlx::Error> {
    let fail = |m: String| {
        *failure.lock().unwrap() = Some(m.clone());
        sqlx::Error::Protocol(m)
    };
    match locals {
        Some(l) => pooler_check(c, l).await.map_err(fail),
        None => readback(c).await.map_err(fail),
    }
}

fn ssl_mode(m: SslMode) -> PgSslMode {
    match m {
        SslMode::Disabled => PgSslMode::Disable,
        SslMode::Preferred => PgSslMode::Prefer,
        SslMode::Required => PgSslMode::Require,
        SslMode::VerifyCa => PgSslMode::VerifyCa,
        SslMode::VerifyIdentity => PgSslMode::VerifyFull,
    }
}

/// The server's version as read from `server_version_num` (170002 is 17.2), or None when it is not a number.
fn gate(num: &str) -> Option<u32> {
    num.trim().parse().ok()
}

/// What the connect statement learns in one round trip.
struct Facts {
    version: String,
    tls: bool,
    timeout: String,
}

/// The guard settings, the version, the TLS state and the statement timeout in one SELECT. Behind a pooler the session
/// carries none of Quarry's settings (`guarded` is false), so only the pooler check vouches for the connection.
async fn probe(c: &mut sqlx::PgConnection, guarded: bool) -> Res<Facts> {
    let rows = c
        .fetch_all(
            "SELECT current_setting('server_version_num'), split_part(current_setting('server_version'), ' ', 1), COALESCE((SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()), false)::text, current_setting('statement_timeout'), current_setting('default_transaction_read_only'), current_setting('standard_conforming_strings')",
        )
        .await
        .map_err(se)?;
    let r = rows.first().ok_or("The server did not report its version")?;
    if guarded && r.cell(4) != Some(b"on") {
        return Err(NOT_READ_ONLY.into());
    }
    scs_check(r.cell(5))?;
    let text = |i| text_at(r, i).unwrap_or_default();
    let (num, version) = (text(0), text(1));
    match gate(&num) {
        Some(n) if n >= MIN_VERSION => {}
        _ => return Err(format!("PostgreSQL {} is too old. Quarry needs PostgreSQL 13 or later.", if version.is_empty() { "of this version".to_string() } else { version })),
    }
    // Only a sane value is kept, because it is written back into SET LOCAL statements.
    let timeout = Some(text(3)).filter(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_alphanumeric())).unwrap_or_else(|| "0".into());
    Ok(Facts { version, tls: text(2) == "true", timeout })
}

/// Connects the way `worker::open_full` does for MySQL: one plain connection first, so a refusal (the guard, the
/// version, a login) is reported at once, then the pools that open their connections on first use.
pub(super) async fn open(a: Addr, timeout: Duration, net: &Net, adv: &super::Advanced, _info: bool) -> Res<(Db, Vec<String>, Option<crate::ssh::Tunnel>, TestInfo)> {
    let (timeout, ms) = adv.timeouts(timeout);
    let tunnel = match &net.ssh {
        Some((conf, secrets)) => Some(crate::ssh::open(conf, secrets, timeout, Some((a.host.clone(), a.port))).await?),
        None => None,
    };
    // The certificate names the real host, not the tunnel's 127.0.0.1, so the name check cannot pass.
    let (host, port, mode) = match &tunnel {
        Some(t) => ("127.0.0.1".to_string(), t.port, if a.ssl_mode == SslMode::VerifyIdentity { SslMode::VerifyCa } else { a.ssl_mode }),
        None => (a.host.clone(), a.port, a.ssl_mode),
    };
    let default_db = if a.database.is_empty() { FALLBACK_DB.to_string() } else { a.database.clone() };
    // The environment (PGPASSWORD, PGSSLROOTCERT and so on) and ~/.pgpass would change what a saved connection
    // does depending on where Quarry was started, so neither is read.
    let mut base = PgConnectOptions::new_without_pgpass().host(&host).port(port).username(&a.user).ssl_mode(ssl_mode(mode));
    base = base.password(&a.password);
    if !a.ssl_ca.is_empty() {
        base = base.ssl_root_cert(&a.ssl_ca);
    }
    let first = async {
        let mut locals = None;
        let (mut c, opts) = match sqlx::PgConnection::connect_with(&startup(base.clone().database(&default_db), ms)).await {
            Ok(c) => (c, startup(base.clone(), ms)),
            Err(x) if options_refused(&x) => {
                locals = Some(locals_for(ms));
                (sqlx::PgConnection::connect_with(&base.clone().database(&default_db)).await.map_err(se)?, base.clone())
            }
            Err(x) => return Err(e(x)),
        };
        let res = async {
            // Behind a pooler the check runs in a transaction first; otherwise one probe reads the guard with the rest.
            if let Some(l) = &locals {
                pooler_check(&mut c, l).await?;
            }
            let f = probe(&mut c, locals.is_none()).await?;
            // The same visibility rule the sidebar needs: databases that cannot be connected to are left out.
            let rows = c
                .fetch_all("SELECT datname::text FROM pg_database WHERE datallowconn AND NOT datistemplate AND has_database_privilege(datname, 'CONNECT') ORDER BY 1")
                .await
                .map_err(se)?;
            let dbs: Vec<String> = rows.iter().filter_map(|r| text_at(r, 0)).collect();
            Ok::<_, String>((f, dbs))
        }
        .await;
        let _ = c.close().await;
        let (f, dbs) = res?;
        Ok::<_, String>((f, dbs, opts, locals))
    };
    let (f, dbs, opts, locals) = tokio::time::timeout(timeout, first).await.map_err(|_| format!("Could not connect and set up the session within {} s", timeout.as_secs()))??;
    let pools = PgPools::new(opts.clone(), default_db.clone(), timeout, locals, f.timeout.clone());
    let db = Db {
        pool: Pool::Pg(Arc::new(pools)),
        dialect: Dialect::Postgres,
        opts: Opts::Pg(opts.database(&default_db)),
        version: f.version,
        tls: f.tls,
        sql_mode: String::new(),
        setup: Default::default(),
        conn: 0,
    };
    Ok((db, dbs, tunnel, TestInfo::default()))
}

/// Readies a connection of its own for a script: the backend's pid for Stop, and write access when unlocked. Behind a
/// pooler (`Db::pooler`) nothing is set on the session, the pid is not kept (the backend may serve another client by the
/// time Stop is pressed) and write access is a `BEGIN READ WRITE` of each statement, because a session setting would land
/// on whichever backend answered and leak to other clients.
pub(super) async fn prepare(c: &mut sqlx::PgConnection, read_only: bool, id: &AtomicU64, db: &Db) -> Result<(), sqlx::Error> {
    if db.pooler() {
        return Ok(());
    }
    // The start time travels as text so no timestamp type has to round trip, and is compared as text on Stop.
    let rows = c.fetch_all("SELECT pg_backend_pid()::text, (SELECT backend_start::text FROM pg_stat_activity WHERE pid = pg_backend_pid())").await?;
    if let Some(r) = rows.first() {
        let pid = num_at(r, 0).unwrap_or(0);
        if let (Some(start), Pool::Pg(p)) = (text_at(r, 1), &db.pool) {
            p.remember_start(pid, start);
        }
        id.store(pid, Ordering::SeqCst);
    }
    if !read_only {
        c.execute(Dialect::Postgres.session_read_only(false)).await?;
    }
    Ok(())
}

/// Stops the statement running on backend `pid` from a connection of its own to the same database. The start time
/// is part of the match, so a pid the server handed to another session meanwhile is left alone.
pub(super) async fn cancel(db: &Db, pid: u64) -> Res<String> {
    // Behind a pooler the backend may be serving another client by now, so Stop closes the connection instead.
    if db.pooler() {
        return Ok("Stopped the script".into());
    }
    let Pool::Pg(pools) = &db.pool else { return Err("Not a PostgreSQL connection".into()) };
    let Some(start) = pools.recall_start(pid) else { return Err("That connection is already gone.".into()) };
    let mut c = super::run::side_conn(db, "Could not reach the server to stop the statement").await?;
    let Conn::Pg(pc) = &mut c else { return Err("Not a PostgreSQL connection".into()) };
    let q = sqlx::query("SELECT pg_cancel_backend(pid)::text FROM pg_stat_activity WHERE pid = $1 AND backend_start::text = $2").bind(pid as i32).bind(start);
    let r = q.fetch_all(&mut *pc).await.map_err(se);
    let _ = c.close().await;
    match r? {
        rows if rows.is_empty() => Err("That connection is already gone.".into()),
        _ => Ok("Stopped the running statement".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
    }

    fn pools(max: usize) -> PgPools {
        let mut p = PgPools::new(PgConnectOptions::new_without_pgpass().host("127.0.0.1").port(1), "postgres".into(), Duration::from_secs(1), None, "0".into());
        p.max_pools = max;
        p
    }

    #[test]
    fn default_database_and_cap() {
        rt().block_on(async {
            let p = pools(4);
            // None and an empty name both mean the default database.
            p.pool(None);
            p.pool(Some(""));
            assert_eq!(p.live_names(), ["postgres"]);
            for d in ["a", "b", "c"] {
                p.pool(Some(d));
            }
            assert_eq!(p.live_names().len(), 4);
            // The same name is the same pool, not a new one.
            p.pool(Some("a"));
            assert_eq!(p.live_names().len(), 4);
        });
    }

    #[test]
    fn the_pool_used_least_recently_is_evicted() {
        rt().block_on(async {
            let p = pools(4);
            for d in ["a", "b", "c", "d"] {
                p.pool(Some(d));
            }
            // Touching a makes b the oldest.
            p.pool(Some("a"));
            p.pool(Some("e"));
            assert_eq!(p.live_names(), ["a", "c", "d", "e"]);
            p.pool(Some("f"));
            assert_eq!(p.live_names(), ["a", "d", "e", "f"]);
        });
    }

    #[test]
    fn a_pool_holds_three_connections_and_none_idle() {
        rt().block_on(async {
            let p = pools(4);
            let pool = p.pool(Some("a"));
            assert_eq!(pool.options().get_max_connections(), 3);
            assert_eq!(pool.options().get_min_connections(), 0);
            assert_eq!(pool.options().get_idle_timeout(), Some(Duration::from_secs(300)));
            assert_eq!(pool.size(), 0);
        });
    }

    #[test]
    fn two_connections_keep_their_own_start_times_for_one_pid() {
        let (a, b) = (pools(4), pools(4));
        a.remember_start(4242, "2024-01-01 00:00:00+00".into());
        b.remember_start(4242, "2025-06-01 00:00:00+00".into());
        assert_eq!(a.recall_start(4242).as_deref(), Some("2024-01-01 00:00:00+00"));
        assert_eq!(b.recall_start(4242).as_deref(), Some("2025-06-01 00:00:00+00"));
        assert_eq!(a.recall_start(7), None);
        for pid in 0..(MAX_STARTS as u64 * 2) {
            a.remember_start(pid, "x".into());
        }
        assert!(a.starts.lock().unwrap().len() <= MAX_STARTS);
    }

    #[test]
    fn at_changes_only_the_database() {
        rt().block_on(async {
            let o = PgConnectOptions::new_without_pgpass().host("127.0.0.1").port(1).database("postgres");
            let db = Db { pool: Pool::Pg(Arc::new(pools(4))), dialect: Dialect::Postgres, opts: Opts::Pg(o), version: String::new(), tls: false, sql_mode: String::new(), setup: Default::default(), conn: 0 };
            assert_eq!(db.at("shop").opts.database(), Some("shop"));
            assert_eq!(db.opts.database(), Some("postgres"));
            assert_eq!(db.at("").opts.database(), Some("postgres"));
            // MySQL-only paths answer with an error instead of panicking.
            assert_eq!(db.my_pool().err().as_deref(), Some(NOT_YET));
            assert_eq!(db.opts.my().err().as_deref(), Some(NOT_YET));
        });
    }

    #[test]
    fn version_gate_reads_server_version_num() {
        assert_eq!(gate("130023"), Some(130_023));
        assert!(gate("120022").is_some_and(|n| n < MIN_VERSION));
        assert!(gate("180000").is_some_and(|n| n >= MIN_VERSION));
        assert_eq!(gate("beta"), None);
    }

    #[test]
    fn a_pooler_refusing_options_is_recognised() {
        assert!(options_refused(&sqlx::Error::Protocol("unsupported startup parameter: options".into())));
        assert!(!options_refused(&sqlx::Error::Protocol("password authentication failed".into())));
    }

    #[test]
    fn startup_and_fallback_carry_the_same_settings() {
        let o = startup(PgConnectOptions::new_without_pgpass(), 15_000);
        let opts = o.get_options().unwrap();
        for want in ["default_transaction_read_only=on", "statement_timeout=15000", "bytea_output=hex", "DateStyle=ISO", "IntervalStyle=postgres"] {
            assert!(opts.contains(want), "{opts}");
        }
        assert_eq!(o.get_application_name(), Some("Quarry"));
        assert!(!startup(PgConnectOptions::new_without_pgpass(), 0).get_options().unwrap().contains("statement_timeout"));
        // The fallback is the same settings as SET LOCAL, and never a session-level SET.
        let locals = locals_for(15_000);
        for want in ["SET LOCAL statement_timeout = 15000", "SET LOCAL bytea_output = 'hex'", "SET LOCAL DateStyle = 'ISO'", "SET LOCAL IntervalStyle = 'postgres'"] {
            assert!(locals.contains(want), "{locals}");
        }
        assert!(!locals.contains("default_transaction_read_only"), "the guard is the read-only transaction, not a setting");
        assert!(!locals_for(0).contains("statement_timeout"));
        let write = write_locals();
        for stmt in locals.split("; ").chain(write.split("; ")) {
            assert!(stmt.starts_with("SET LOCAL "), "{stmt}");
        }
        assert!(write.contains("statement_timeout = 0") && write.contains("lock_timeout = '10s'"));
    }

    #[test]
    fn the_three_places_agree_on_the_output_formats() {
        let startup_opts = startup(PgConnectOptions::new_without_pgpass(), 0).get_options().unwrap().to_string();
        let (locals, write) = (locals_for(0), write_locals());
        for (k, v) in FORMATS {
            assert!(startup_opts.contains(&format!("{k}={v}")), "startup lacks {k}");
            assert!(locals.split("; ").any(|s| s == set_local(k, v)), "fallback lacks {k}");
            assert!(write.split("; ").any(|s| s == set_local(k, v)), "write lacks {k}");
        }
        // Nothing but the formats is shared: the fallback has no more format settings than the list.
        assert_eq!(locals.split("; ").count(), FORMATS.len());
    }

    /// Live tests, run on every server named by QUARRY_TEST_POSTGRES and QUARRY_TEST_POSTGRES13.
    mod live {
        use super::super::*;
        use crate::db::sql::testing::{every_pg, LivePg};
        use std::str::FromStr;

        fn code(x: &sqlx::Error) -> Option<String> {
            x.as_database_error().and_then(|d| d.code().map(|c| c.to_string()))
        }

        fn pools_of(l: &LivePg, locals: Option<String>, acquire: Duration) -> PgPools {
            // Without fallback statements the startup options carry the session, as in the app.
            let o = PgConnectOptions::from_str(&l.uri).unwrap();
            PgPools::new(if locals.is_none() { startup(o, 30_000) } else { o }, "postgres".into(), acquire, locals, "0".into())
        }

        /// Pools whose startup options are exactly `opts`, as a server that ignored or changed the guard would leave them.
        fn pools_with(l: &LivePg, opts: &[(&str, &str)], acquire: Duration) -> PgPools {
            PgPools::new(PgConnectOptions::from_str(&l.uri).unwrap().options(opts.iter().copied()), "postgres".into(), acquire, None, "0".into())
        }

        #[test]
        fn connects_and_lists_the_databases_it_may_use() {
            every_pg(|l, var| {
                assert_eq!(l.db.dialect, Dialect::Postgres, "{var}");
                assert!(l.db.version.chars().next().unwrap().is_ascii_digit() && !l.db.version.contains(' '), "{}", l.db.version);
                assert_eq!(l.db.label(), format!("PostgreSQL {}", l.db.version));
                assert!(l.dbs.iter().any(|d| d == "postgres"), "{:?}", l.dbs);
                assert!(!l.dbs.iter().any(|d| d.starts_with("template")), "{:?}", l.dbs);
                let mut sorted = l.dbs.clone();
                sorted.sort();
                assert_eq!(l.dbs, sorted);
                // The default database is the one the address names, and `at` moves to another.
                assert_eq!(l.db.opts.database(), Some("postgres"));
                let other = l.quarry_test();
                let mut c = l.w.rt.block_on(other.acquire_conn()).unwrap();
                let rows = l.w.rt.block_on(c.fetch_all("SELECT current_database()")).unwrap();
                assert_eq!(rows[0].cell(0), Some(&b"quarry_test"[..]), "{var}");
                assert!(l.db.my_pool().is_err() && l.db.opts.my().is_err());
            });
        }

        #[test]
        fn the_session_is_read_only_and_reads_text() {
            every_pg(|l, var| {
                let mut c = l.w.rt.block_on(l.db.acquire_conn()).unwrap();
                let name = format!("t_pg2_ro_{}", var.to_ascii_lowercase());
                let err = l.w.rt.block_on(c.execute(&format!("CREATE TABLE {name} (id int)"))).unwrap_err();
                assert_eq!(code(&err).as_deref(), Some("25006"), "{var}: {err}");
                for (setting, want) in [("default_transaction_read_only", "on"), ("standard_conforming_strings", "on"), ("bytea_output", "hex"), ("DateStyle", "ISO, MDY"), ("IntervalStyle", "postgres"), ("application_name", "Quarry")] {
                    let rows = l.w.rt.block_on(c.fetch_all(&format!("SHOW {setting}"))).unwrap();
                    assert_eq!(rows[0].cell(0), Some(want.as_bytes()), "{var} {setting}");
                }
                // The statement timeout is the connect timeout the worker was given (30 s).
                let rows = l.w.rt.block_on(c.fetch_all("SHOW statement_timeout")).unwrap();
                assert_eq!(rows[0].cell(0), Some(&b"30s"[..]), "{var}");
            });
        }

        #[test]
        fn readback_refuses_a_session_that_can_write() {
            every_pg(|l, var| {
                // Dropping a pooled connection needs the runtime of the worker.
                let _rt = l.w.rt.enter();
                // A pool whose connections turn the guard off, as a server or pooler that ignores the startup
                // options would leave them.
                let off = pools_with(l, &[("default_transaction_read_only", "off")], Duration::from_secs(3));
                let err = l.w.rt.block_on(off.acquire(None)).err().expect("must be refused");
                assert!(err.contains("not read-only"), "{var}: {err}");
                let scs = pools_with(l, &[("default_transaction_read_only", "on"), ("standard_conforming_strings", "off")], Duration::from_secs(3));
                let err = l.w.rt.block_on(scs.acquire(None)).err().expect("must be refused");
                assert!(err.contains("standard_conforming_strings"), "{var}: {err}");
                // Direct readback on a connection that was switched.
                let mut raw = l.w.rt.block_on(sqlx::PgConnection::connect(&l.uri)).unwrap();
                l.w.rt.block_on(raw.execute("SET default_transaction_read_only = on")).unwrap();
                assert!(l.w.rt.block_on(readback(&mut raw)).is_ok());
                l.w.rt.block_on(raw.execute("SET default_transaction_read_only = off")).unwrap();
                assert!(l.w.rt.block_on(readback(&mut raw)).is_err());
            });
        }

        #[test]
        fn the_version_is_at_least_13() {
            every_pg(|l, var| {
                let mut c = l.w.rt.block_on(l.db.acquire_conn()).unwrap();
                let f = l.w.rt.block_on(async {
                    let Conn::Pg(pc) = &mut c else { panic!() };
                    probe(pc, true).await
                })
                .unwrap();
                let num = gate(&l.one("SHOW server_version_num")).unwrap();
                assert!(num >= MIN_VERSION, "{var}");
                assert_eq!(f.version, l.db.version);
                assert_eq!(f.version.split('.').next().unwrap(), (num / 10_000).to_string(), "{var}");
            });
        }

        #[test]
        fn stop_cancels_the_running_statement_and_spares_a_reused_pid() {
            every_pg(|l, var| {
                let db = l.db.clone();
                let id = Arc::new(AtomicU64::new(0));
                let (tx, rx) = std::sync::mpsc::channel();
                let run_id = id.clone();
                l.w.rt.spawn(async move {
                    let mut c = db.acquire_conn().await.unwrap();
                    crate::db::sql::run::prepare(&mut c, &db, "", true, &run_id).await.unwrap();
                    let t = std::time::Instant::now();
                    let r = c.execute("SELECT pg_sleep(30)").await;
                    let _ = tx.send((r.err().and_then(|x| code(&x)), t.elapsed()));
                });
                let t = std::time::Instant::now();
                while id.load(Ordering::SeqCst) == 0 {
                    assert!(t.elapsed() < Duration::from_secs(10), "{var}: no backend pid");
                    std::thread::sleep(Duration::from_millis(50));
                }
                std::thread::sleep(Duration::from_millis(300));
                let pid = id.load(Ordering::SeqCst);
                // The server gave this pid to another session in the meantime: the start time differs, so nothing is hit.
                let Pool::Pg(pools) = &l.db.pool else { unreachable!() };
                let real = pools.recall_start(pid).unwrap();
                pools.remember_start(pid, "2001-01-01 00:00:00+00".into());
                let err = l.w.rt.block_on(cancel(&l.db, pid)).unwrap_err();
                assert_eq!(err, "That connection is already gone.", "{var}");
                assert!(rx.recv_timeout(Duration::from_millis(800)).is_err(), "{var}: a reused pid was cancelled");
                pools.remember_start(pid, real);
                assert_eq!(l.w.rt.block_on(cancel(&l.db, pid)).unwrap(), "Stopped the running statement", "{var}");
                let (state, took) = rx.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|_| panic!("{var}: still running after Stop"));
                assert_eq!(state.as_deref(), Some("57014"), "{var}");
                assert!(took < Duration::from_secs(10), "{var}: {took:?}");
            });
        }

        #[test]
        fn an_evicted_pool_stays_usable_for_its_holders() {
            every_pg(|l, var| {
                let _rt = l.w.rt.enter();
                let a = "postgres";
                let da = l.db.at(a);
                let mut held = l.w.rt.block_on(da.acquire_conn()).unwrap();
                // A clone handed out before the eviction, as a restored tab holds it.
                let before = da.pg_pool().unwrap();
                for d in ["pg11_e1", "pg11_e2", "pg11_e3", "pg11_e4"] {
                    l.db.at(d).pg_pool().unwrap();
                }
                let Pool::Pg(pools) = &l.db.pool else { unreachable!() };
                assert!(!pools.live_names().contains(&a.to_string()), "{var}: the pool was not evicted");
                let Conn::Pg(c) = &mut held else { unreachable!() };
                l.w.rt.block_on(c.execute("SELECT pg_sleep(0.2)")).unwrap_or_else(|x| panic!("{var}: in-flight query failed: {x}"));
                std::thread::sleep(Duration::from_millis(300));
                l.w.rt.block_on(before.acquire()).unwrap_or_else(|x| panic!("{var}: an evicted pool refused an acquire: {x}"));
                drop(held);
                assert!(l.w.rt.block_on(da.acquire_conn()).is_ok(), "{var}");
            });
        }

        #[test]
        fn a_database_pool_holds_three_connections() {
            every_pg(|l, var| {
                // Dropping a pooled connection needs the runtime of the worker.
                let _rt = l.w.rt.enter();
                let p = pools_of(l, None, Duration::from_secs(1));
                let held: Vec<_> = (0..3).map(|_| l.w.rt.block_on(p.acquire(None)).unwrap()).collect();
                assert!(l.w.rt.block_on(p.acquire(None)).is_err(), "{var}: a fourth connection was handed out");
                drop(held);
                assert!(l.w.rt.block_on(p.acquire(None)).is_ok(), "{var}");
            });
        }

        #[test]
        fn tls_is_reported_as_the_server_has_it() {
            every_pg(|l, var| {
                let ssl = l.one("SHOW ssl");
                eprintln!("{var}: ssl={ssl} reported tls={}", l.db.tls);
                assert_eq!(l.db.tls, ssl == "on", "{var}");
                // A mode that demands TLS either connects over it or is refused, and Off never uses it.
                for (mode, want) in [("require", true), ("disable", false)] {
                    let o = PgConnectOptions::from_str(&format!("{}?sslmode={mode}", l.uri)).unwrap();
                    match l.w.rt.block_on(sqlx::PgConnection::connect_with(&o)) {
                        Ok(mut c) => {
                            let rows = l.w.rt.block_on(c.fetch_all("SELECT COALESCE((SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()), false)::text")).unwrap();
                            assert_eq!(rows[0].cell(0), Some(if want { &b"true"[..] } else { &b"false"[..] }), "{var} {mode}");
                        }
                        Err(x) => assert!(mode == "require" && ssl != "on", "{var} {mode}: {x}"),
                    }
                }
            });
        }

        #[test]
        fn the_writer_writes_and_the_pooled_session_stays_read_only() {
            every_pg(|l, var| {
                let db = l.quarry_test();
                let name = format!("t_pg2_w_{}", var.to_ascii_lowercase());
                l.w.rt.block_on(async {
                    assert_eq!(crate::db::sql::write::writer_conn(&db, "", true).await.err().as_deref(), Some("Writes are locked on this connection. Unlock them first."));
                    let mut w = crate::db::sql::write::writer_conn(&db, "", false).await.unwrap();
                    // The writer's session is the pool's, read-only; a write opens its own READ WRITE transaction.
                    let rows = w.fetch_all("SHOW default_transaction_read_only").await.unwrap();
                    assert_eq!(rows[0].cell(0), Some(&b"on"[..]), "{var}");
                    let err = w.execute(&format!("CREATE TABLE {name} (id int)")).await.unwrap_err();
                    assert_eq!(code(&err).as_deref(), Some("25006"), "{var}: {err}");
                    let mut tx = w.begin_write().await.unwrap();
                    tx.exec_bound(&format!("DROP TABLE IF EXISTS {name}"), &[]).await.unwrap();
                    tx.exec_bound(&format!("CREATE TABLE {name} (id int)"), &[]).await.unwrap();
                    tx.exec_bound(&format!("INSERT INTO {name} VALUES (1), (2)"), &[]).await.unwrap();
                    let rows = tx.fetch_bound(&format!("SELECT count(*)::text, current_setting('statement_timeout'), current_setting('lock_timeout'), current_setting('transaction_read_only') FROM {name}"), &[]).await.unwrap();
                    let cells: Vec<_> = (0..4).map(|i| String::from_utf8_lossy(rows[0].cell(i).unwrap()).into_owned()).collect();
                    assert_eq!(cells, ["2", "0", "10s", "off"], "{var}");
                    tx.commit().await.unwrap();
                    // The session default was never changed, and the next pooled session is read-only too.
                    let rows = w.fetch_all("SHOW default_transaction_read_only").await.unwrap();
                    assert_eq!(rows[0].cell(0), Some(&b"on"[..]), "{var}");
                    let rows = w.fetch_all("SHOW lock_timeout").await.unwrap();
                    assert_eq!(rows[0].cell(0), Some(&b"0"[..]), "{var}");
                    let mut r = db.acquire_conn().await.unwrap();
                    let err = r.execute(&format!("INSERT INTO {name} VALUES (3)")).await.unwrap_err();
                    assert_eq!(code(&err).as_deref(), Some("25006"), "{var}: {err}");
                    let mut tx = w.begin_write().await.unwrap();
                    tx.exec_bound(&format!("DROP TABLE {name}"), &[]).await.unwrap();
                    tx.commit().await.unwrap();
                });
            });
        }

        #[test]
        fn behind_a_pooler_nothing_is_set_on_the_session() {
            every_pg(|l, var| {
                let _rt = l.w.rt.enter();
                // No startup options, as a pooler that refuses them leaves the connection.
                let p = pools_of(l, Some(locals_for(30_000)), Duration::from_secs(10));
                let mut c = l.w.rt.block_on(p.acquire(None)).unwrap();
                // The check ran in a rolled-back transaction: statement_timeout, DateStyle, bytea_output and the read-only
                // default are all what the server hands every session.
                let server_default = |name: &str| l.one(&format!("SHOW {name}"));
                for name in ["statement_timeout", "default_transaction_read_only", "bytea_output", "IntervalStyle", "application_name"] {
                    let rows = l.w.rt.block_on(c.fetch_all(format!("SHOW {name}").as_str())).unwrap();
                    assert_eq!(String::from_utf8_lossy(rows[0].cell(0).unwrap()), server_default(name), "{var} {name}");
                }
                assert_eq!(p.locals(), locals_for(30_000));
                // Inside a read transaction the locals apply and the guard holds.
                let db = Db { pool: Pool::Pg(Arc::new(p)), ..l.db.clone() };
                let rows = l.w.rt.block_on(crate::db::sql::pg_read::read_only(&db, false, "", "SELECT current_setting('statement_timeout'), current_setting('transaction_read_only'), current_setting('DateStyle')")).unwrap();
                assert_eq!(rows[0].cell(0), Some(&b"30s"[..]), "{var}");
                assert_eq!(rows[0].cell(1), Some(&b"on"[..]), "{var}");
                let err = l.w.rt.block_on(crate::db::sql::pg_read::read_only(&db, false, "", "CREATE TABLE t_pg5_pooled (id int)")).unwrap_err();
                assert_eq!(code(&err).as_deref(), Some("25006"), "{var}: {err}");
                // The session is untouched afterwards as well.
                let rows = l.w.rt.block_on(c.fetch_all("SHOW statement_timeout")).unwrap();
                assert_eq!(String::from_utf8_lossy(rows[0].cell(0).unwrap()), server_default("statement_timeout"), "{var}");
            });
        }

        #[test]
        fn behind_a_pooler_pages_and_writes_run_in_transactions() {
            every_pg(|l, var| {
                let _rt = l.w.rt.enter();
                let base = l.quarry_test();
                let pool = l.fixture_pool();
                let name = format!("t5_pooled_{}", var.to_ascii_lowercase());
                l.qsql(&pool, &format!("DROP SCHEMA IF EXISTS {name} CASCADE; CREATE SCHEMA {name}; CREATE TABLE {name}.t (id int PRIMARY KEY, v int, b bytea); INSERT INTO {name}.t VALUES (1, 1, '\\x0102')"));
                let p = PgPools::new(PgConnectOptions::from_str(&l.uri).unwrap(), "postgres".into(), Duration::from_secs(10), Some(locals_for(30_000)), "0".into());
                let db = Db { pool: Pool::Pg(Arc::new(p)), ..base };
                // A page and the metadata behind it are read in READ ONLY transactions with the locals applied.
                l.w.sql_page(7, db.clone(), name.clone(), "t".into(), Default::default(), 100, 0);
                let crate::db::Msg::SqlCols { cols, pk, .. } = l.next() else { panic!("expected SqlCols") };
                let crate::db::Msg::Docs { res: Ok((rows, _, _)), .. } = l.next() else { panic!("expected Docs") };
                let docs: Vec<_> = rows.iter().map(|r| r.decode().unwrap()).collect();
                assert_eq!(pk, ["id"], "{var}");
                assert_eq!(docs[0].get_i64("v").unwrap(), 1);
                assert!(matches!(docs[0].get("b"), Some(mongodb::bson::Bson::Binary(b)) if b.bytes == [1, 2]), "{var}: bytea is read as hex");
                // A write goes through its own READ WRITE transaction, and no session setting changed anywhere.
                let ed = crate::db::sql::write::RowEdit { pk: docs[0].get_document("_id").unwrap().clone(), changes: vec![("v".into(), mongodb::bson::Bson::Int64(1), mongodb::bson::Bson::Int64(2))] };
                l.w.sql_save(vec!["k".into()], 1, db.clone(), name.clone(), "t".into(), cols, vec!["id".into()], vec![ed], false);
                let crate::db::Msg::Saved { res, .. } = l.next() else { panic!("expected Saved") };
                assert!(res.is_ok(), "{var}: {res:?}");
                assert_eq!(l.qone(&pool, &format!("SELECT v::text FROM {name}.t")), "2");
                l.w.rt.block_on(async {
                    let mut c = db.acquire_conn().await.unwrap();
                    for (setting, want) in [("default_transaction_read_only", "off"), ("statement_timeout", "0"), ("lock_timeout", "0"), ("bytea_output", "hex")] {
                        let rows = c.fetch_all(format!("SHOW {setting}").as_str()).await.unwrap();
                        assert_eq!(String::from_utf8_lossy(rows[0].cell(0).unwrap()), want, "{var} {setting}");
                    }
                });
                l.qsql(&pool, &format!("DROP SCHEMA {name} CASCADE"));
            });
        }

        #[test]
        fn a_backend_that_changes_inside_one_transaction_is_refused() {
            every_pg(|l, var| {
                // A real server answers every statement of a transaction from one backend; statement pooling does not.
                // The pid is compared across the statements of the check, so a stand-in that reports another value for the
                // second one is what the refusal reads.
                let schema = format!("t5_flip_{}", var.to_ascii_lowercase());
                let mut raw = l.w.rt.block_on(sqlx::PgConnection::connect(&l.uri)).unwrap();
                l.w.rt.block_on(raw.execute(format!("DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema}; CREATE FUNCTION {schema}.pg_backend_pid() RETURNS int LANGUAGE sql VOLATILE AS 'SELECT (random() * 1000000000)::int'").as_str())).unwrap();
                // An unqualified call finds the stand-in before pg_catalog's own.
                l.w.rt.block_on(raw.execute(format!("SET search_path = {schema}, pg_catalog").as_str())).unwrap();
                let err = l.w.rt.block_on(pooler_check(&mut raw, "SET LOCAL statement_timeout = 0")).unwrap_err();
                assert!(err.contains("statement pooling"), "{var}: {err}");
                // Without the stand-in the same connection passes.
                l.w.rt.block_on(raw.execute("SET search_path = pg_catalog")).unwrap();
                assert!(l.w.rt.block_on(pooler_check(&mut raw, "SET LOCAL statement_timeout = 0")).is_ok(), "{var}");
                l.w.rt.block_on(raw.execute(format!("DROP SCHEMA {schema} CASCADE").as_str())).unwrap();
            });
        }
    }
}
