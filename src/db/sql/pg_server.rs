//! The PostgreSQL Server window's reads and its two actions. Every read runs in a `BEGIN READ ONLY` that is rolled back, on a
//! pooled connection, and is not put in the query log because the window polls. Cancel and Terminate go through the writer:
//! the read-only transaction would not stop pg_cancel_backend or pg_terminate_backend, so only an unlocked connection may send
//! them, and the statement that sends one checks in the same breath that the pid is still the backend the list showed.

use super::exec::{Cells, SqlRow};
use super::pg_read::in_read_only;
use super::worker::{num_at, text_at};
use super::value::Param;
use super::{se, write, Advanced, Db, Dialect, Pool};
use crate::db::{Msg, Res, Worker};
use sqlx::Executor;
use std::collections::HashMap;

pub use super::server::one_line;

/// Rows of one statement of Quarry's own over the simple protocol, so every value arrives as text.
async fn rows(c: &mut sqlx::PgConnection, sql: &str) -> Result<Vec<sqlx::postgres::PgRow>, sqlx::Error> {
    c.fetch_all(sql).await
}

fn txt<R: Cells>(r: &R, i: usize) -> String {
    text_at(r, i).unwrap_or_default()
}

fn secs<R: Cells>(r: &R, i: usize) -> Option<f64> {
    text_at(r, i)?.trim().parse().ok()
}

/// Runs `work` in a rolled-back read-only transaction on a pooled connection of the database `db` points at. A connection that
/// failed is closed instead of going back to the pool, since it may still sit inside the transaction.
async fn read<T>(db: &Db, work: impl AsyncFnOnce(&mut sqlx::PgConnection) -> Result<T, sqlx::Error>) -> Res<T> {
    if db.dialect != Dialect::Postgres {
        return Err("Not a PostgreSQL connection".into());
    }
    let mut conn = db.pg_pool()?.acquire().await.map_err(se)?;
    let r = in_read_only(&mut conn, db, "", work).await;
    if r.is_err() {
        conn.close_on_drop();
    }
    r.map_err(se)
}

/// One row of pg_stat_activity as the window shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Session {
    pub pid: u64,
    pub user: String,
    /// The client's address, "local" for a unix socket, empty for a background process or a hidden row.
    pub client: String,
    pub db: String,
    pub app: String,
    /// "active", "idle", "idle in transaction" and so on; empty for a background process or a hidden row.
    pub state: String,
    pub wait_type: String,
    pub wait: String,
    /// backend_type: "client backend", "autovacuum worker", "checkpointer"...
    pub kind: String,
    pub xact_secs: Option<f64>,
    pub state_secs: Option<f64>,
    pub query_secs: Option<f64>,
    /// As the server prints it. Empty when the role may not see the row, which then cannot be checked before a signal.
    pub backend_start: String,
    pub query_start: String,
    pub query: String,
    /// The query on one line, cut once per answer.
    pub line: String,
    /// Lower case user, database, application and the start of the query, for the filter.
    pub search: String,
    /// The role lacks pg_read_all_stats and the row is another role's: state, client and query read insufficient privilege.
    pub hidden: bool,
    /// The sessions this one waits behind (pg_blocking_pids), only for a session waiting on a lock.
    pub blockers: Vec<u64>,
    /// The lock it waits for and on what, empty when it waits for none.
    pub lock_mode: String,
    pub lock_on: String,
    /// The role may send this session a signal (its own role, pg_signal_backend for a non-superuser's, or a superuser).
    pub signal: bool,
    /// The connection that read this list, or another connection of Quarry's own on this server.
    pub ours: bool,
}

impl Session {
    #[allow(clippy::too_many_arguments)]
    pub fn new(pid: u64, user: &str, client: &str, db: &str, app: &str, state: &str, query: &str, kind: &str) -> Session {
        let hidden = query == "<insufficient privilege>";
        let search = format!("{user} {db} {app} {}", one_line(query, 4000)).to_lowercase();
        Session { pid, user: user.into(), client: client.into(), db: db.into(), app: app.into(), state: state.into(), kind: kind.into(), query: query.into(), line: if hidden { "insufficient privilege".into() } else { one_line(query, 300) }, search, hidden, ..Default::default() }
    }

    pub fn active(&self) -> bool {
        self.state == "active" || self.state == "fastpath function call"
    }

    /// Open transaction with nothing running: holds its locks and snapshot until the client goes on.
    pub fn idle_in_tx(&self) -> bool {
        self.state.starts_with("idle in transaction")
    }

    /// What "Hide idle" hides: an idle client, and a background process that runs nothing. A hidden row stays, since its state
    /// is unknown.
    pub fn idle(&self) -> bool {
        !self.hidden && (self.state == "idle" || (self.state.is_empty() && self.query.is_empty()))
    }

    /// A client session or an autovacuum worker; the server's other processes do not take these signals.
    pub fn signalable(&self) -> bool {
        matches!(self.kind.as_str(), "client backend" | "autovacuum worker")
    }

    /// Whether a pending confirmation still points at this row: the same backend in the same state running the same text.
    pub fn same(&self, o: &Session) -> bool {
        self.pid == o.pid && self.backend_start == o.backend_start && self.user == o.user && self.state == o.state && self.query == o.query
    }
}

pub struct Activity {
    pub rows: Vec<Session>,
    pub user: String,
    /// The role may read every session (pg_read_all_stats or a superuser).
    pub read_all: bool,
}

/// pg_blocking_pids as printed by array_to_string.
pub fn pids(s: &str) -> Vec<u64> {
    s.split(',').filter_map(|p| p.trim().parse().ok()).collect()
}

/// Every session with what it waits for. pg_locks is read by any role, so the lock columns are known even for a session whose
/// state is hidden; pg_blocking_pids is asked only for a session that waits, since it takes the lock manager's locks.
const ACTIVITY: &str = "WITH w AS (SELECT DISTINCT ON (pid) pid, mode, locktype, database, relation, transactionid FROM pg_locks WHERE NOT granted AND pid IS NOT NULL ORDER BY pid), \
me AS (SELECT r.rolsuper AS su, pg_has_role('pg_signal_backend', 'USAGE') AS sig FROM pg_roles r WHERE r.rolname = current_user) \
SELECT a.pid::text, COALESCE(a.usename::text, ''), CASE WHEN a.client_port = -1 THEN 'local' ELSE COALESCE(host(a.client_addr), '') END, \
COALESCE(a.datname::text, ''), COALESCE(a.application_name, ''), COALESCE(a.state, ''), COALESCE(a.wait_event_type, ''), COALESCE(a.wait_event, ''), COALESCE(a.backend_type, ''), \
extract(epoch FROM now() - a.xact_start)::text, extract(epoch FROM now() - a.state_change)::text, extract(epoch FROM now() - a.query_start)::text, \
COALESCE(a.backend_start::text, ''), COALESCE(a.query_start::text, ''), COALESCE(a.query, ''), \
CASE WHEN w.pid IS NOT NULL THEN array_to_string(pg_blocking_pids(a.pid), ',') END, COALESCE(w.mode, ''), \
CASE WHEN w.relation IS NOT NULL THEN COALESCE((SELECT n.nspname || '.' || c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.oid = w.relation AND w.database = (SELECT oid FROM pg_database WHERE datname = current_database())), 'relation ' || w.relation::text) \
WHEN w.locktype = 'transactionid' THEN 'transaction ' || w.transactionid::text WHEN w.locktype IS NOT NULL THEN w.locktype END, \
COALESCE(CASE WHEN (SELECT rolsuper FROM pg_roles WHERE oid = a.usesysid) THEN me.su ELSE me.su OR pg_has_role(a.usesysid, 'USAGE') OR me.sig END, false)::int::text, \
(a.pid = pg_backend_pid())::int::text \
FROM pg_stat_activity a LEFT JOIN w ON w.pid = a.pid CROSS JOIN me ORDER BY a.pid";

const WHO: &str = "SELECT current_user::text, pg_has_role('pg_read_all_stats', 'USAGE')::int::text";

fn session_of<R: Cells>(r: &R) -> Option<Session> {
    let mut s = Session::new(num_at(r, 0)?, &txt(r, 1), &txt(r, 2), &txt(r, 3), &txt(r, 4), &txt(r, 5), &txt(r, 14), &txt(r, 8));
    (s.wait_type, s.wait) = (txt(r, 6), txt(r, 7));
    (s.xact_secs, s.state_secs, s.query_secs) = (secs(r, 9), secs(r, 10), secs(r, 11));
    (s.backend_start, s.query_start) = (txt(r, 12), txt(r, 13));
    s.blockers = pids(&txt(r, 15));
    (s.lock_mode, s.lock_on) = (txt(r, 16), txt(r, 17));
    s.signal = txt(r, 18) == "1";
    s.ours = txt(r, 19) == "1";
    Some(s)
}

async fn activity(db: &Db) -> Res<Activity> {
    let (who, list) = read(db, async |c| Ok((rows(c, WHO).await?, rows(c, ACTIVITY).await?))).await?;
    let w = who.first().ok_or("The server did not say who is signed in")?;
    let mut rows: Vec<Session> = list.iter().filter_map(session_of).collect();
    if let Pool::Pg(p) = &db.pool {
        if !db.pooler() {
            p.keep_own(|pid, start| rows.iter().any(|r| r.pid == pid && r.backend_start == start));
            for r in rows.iter_mut() {
                r.ours |= p.is_own(r.pid, &r.backend_start);
            }
        }
    }
    Ok(Activity { rows, user: txt(w, 0), read_all: txt(w, 1) == "1" })
}

/// A blocking chain for the Locks tab: the session at its head and those queued behind it, each with its depth.
#[derive(Debug, PartialEq)]
pub struct Chain {
    pub head: u64,
    pub waiting: Vec<(u64, usize)>,
}

/// The blocking chains of a list, heads in pid order. A waiter goes under a blocker that does not wait itself when it has one, so a
/// session that holds the lock it needs leads; one blocked only by other waiters queues behind them and goes a level deeper.
/// Waiters in a cycle with no head (a deadlock the server has not broken yet) lead a chain of their own.
pub fn chains(rows: &[Session]) -> Vec<Chain> {
    let waits: HashMap<u64, &Session> = rows.iter().filter(|r| !r.blockers.is_empty()).map(|r| (r.pid, r)).collect();
    let parent = |r: &Session| r.blockers.iter().copied().find(|b| !waits.contains_key(b)).unwrap_or(r.blockers[0]);
    let mut kids: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut order: Vec<&&Session> = waits.values().collect();
    order.sort_by_key(|r| r.pid);
    for r in &order {
        kids.entry(parent(r)).or_default().push(r.pid);
    }
    let mut heads: Vec<u64> = kids.keys().copied().filter(|p| !waits.contains_key(p)).collect();
    heads.sort();
    let mut placed = std::collections::HashSet::new();
    let mut out = Vec::new();
    fn walk(p: u64, depth: usize, kids: &HashMap<u64, Vec<u64>>, placed: &mut std::collections::HashSet<u64>, into: &mut Vec<(u64, usize)>) {
        for k in kids.get(&p).into_iter().flatten() {
            if placed.insert(*k) {
                into.push((*k, depth));
                walk(*k, depth + 1, kids, placed, into);
            }
        }
    }
    for h in heads {
        let mut waiting = Vec::new();
        placed.insert(h);
        walk(h, 1, &kids, &mut placed, &mut waiting);
        out.push(Chain { head: h, waiting });
    }
    for r in order {
        if placed.insert(r.pid) {
            let mut waiting = Vec::new();
            walk(r.pid, 1, &kids, &mut placed, &mut waiting);
            out.push(Chain { head: r.pid, waiting });
        }
    }
    out
}

/// One setting of pg_settings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Setting {
    pub name: String,
    /// As SHOW prints it, with its unit ("4GB", "500ms").
    pub value: String,
    pub unit: String,
    /// pg_settings.source: default, configuration file, database, user, session, client...
    pub source: String,
    /// "file:line", empty when the role may not read where it was set.
    pub file: String,
    pub pending: bool,
    /// The value the configuration file holds for after the restart, when the role may read pg_file_settings.
    pub after_restart: Option<String>,
    pub context: String,
    pub desc: String,
    pub min: String,
    pub max: String,
    pub boot: String,
    /// Lower case name, for the filter.
    pub lower: String,
}

impl Setting {
    /// Not at its built-in default: set in a file, for a database or role, by a client or by the session.
    pub fn changed(&self) -> bool {
        self.source != "default"
    }
}

pub struct Settings {
    pub rows: Vec<Setting>,
    pub user: String,
    pub database: String,
}

const SETTINGS: &str = "SELECT name::text, COALESCE(current_setting(name, true), setting)::text, COALESCE(unit, ''), source::text, \
CASE WHEN sourcefile IS NOT NULL THEN sourcefile || ':' || sourceline::text ELSE '' END, pending_restart::int::text, context::text, COALESCE(short_desc, ''), \
COALESCE(min_val, ''), COALESCE(max_val, ''), COALESCE(boot_val, ''), current_user::text, current_database()::text FROM pg_settings ORDER BY name";

/// pg_file_settings is for superusers unless granted, so it is asked only when the role may read it.
const FILE_READABLE: &str = "SELECT (has_table_privilege('pg_catalog.pg_file_settings', 'SELECT') AND has_function_privilege('pg_catalog.pg_show_all_file_settings()', 'EXECUTE'))::int::text";
const FILE_PENDING: &str = "SELECT name::text, COALESCE(setting, '') FROM pg_file_settings WHERE error IS NOT NULL OR NOT applied ORDER BY seqno";

async fn settings(db: &Db) -> Res<Settings> {
    let (list, pending) = read(db, async |c| {
        let list = rows(c, SETTINGS).await?;
        let any = list.iter().any(|r| txt(r, 5) == "1");
        let pending = if any && rows(c, FILE_READABLE).await?.first().is_some_and(|r| txt(r, 0) == "1") { rows(c, FILE_PENDING).await? } else { Vec::new() };
        Ok((list, pending))
    })
    .await?;
    let after: HashMap<String, String> = pending.iter().map(|r| (txt(r, 0).to_lowercase(), txt(r, 1))).collect();
    let (user, database) = list.first().map(|r| (txt(r, 11), txt(r, 12))).unwrap_or_default();
    let rows = list
        .iter()
        .map(|r| {
            let name = txt(r, 0);
            let pending = txt(r, 5) == "1";
            let lower = name.to_lowercase();
            Setting { after_restart: if pending { after.get(&lower).cloned() } else { None }, value: txt(r, 1), unit: txt(r, 2), source: txt(r, 3), file: txt(r, 4), pending, context: txt(r, 6), desc: txt(r, 7), min: txt(r, 8), max: txt(r, 9), boot: txt(r, 10), lower, name }
        })
        .collect();
    Ok(Settings { rows, user, database })
}

/// The settings Quarry gives every session of a PostgreSQL connection, lower case: the startup options of `pg::startup_with`, what
/// sqlx sends itself, and the Advanced tab's rows. The window marks them only where the session's source says they are in effect.
pub fn set_by_quarry(adv: &Advanced) -> Vec<String> {
    let mut v: Vec<String> = ["application_name", "bytea_output", "client_encoding", "datestyle", "default_transaction_read_only", "extra_float_digits", "intervalstyle", "statement_timeout", "timezone"].map(String::from).to_vec();
    if let Ok(s) = super::pg_advanced::PgSession::new(adv, super::advanced::system_zone_name()) {
        v.extend(s.settings.iter().map(|(n, _)| n.to_lowercase()));
    }
    v.sort();
    v.dedup();
    v
}

/// One standby the primary streams to.
#[derive(Clone, Debug, PartialEq)]
pub struct Standby {
    pub name: String,
    pub addr: String,
    pub state: String,
    pub sync: String,
    pub lag: Option<f64>,
}

/// What the server is in replication.
#[derive(Clone, Debug, PartialEq)]
pub enum Role {
    /// A primary and its standbys, empty when none streams from it. `visible` is false when the role may not read their state.
    Primary { standbys: Vec<Standby>, visible: bool },
    /// A standby: its upstream as pg_stat_wal_receiver names it, the receiver's status and how far behind its replay is.
    Replica { from: String, status: String, lag: Option<f64> },
}

/// What the Overview shows, read in one transaction.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Overview {
    pub version: String,
    pub short_version: String,
    pub uptime: Option<u64>,
    pub started: String,
    pub connections: Option<f64>,
    pub max_connections: Option<f64>,
    pub active: Option<f64>,
    pub waiting: Option<f64>,
    pub commits: Option<f64>,
    pub rollbacks: Option<f64>,
    pub hits: Option<f64>,
    pub reads: Option<f64>,
    pub shared_buffers: Option<f64>,
    pub role: Option<Role>,
    pub host: String,
    pub port: String,
    /// Empty when the role may not read it (pg_read_all_settings).
    pub data_dir: String,
    pub encoding: String,
    pub collate: String,
    pub time_zone: String,
    /// TLS version and cipher of this session, empty when it is not encrypted.
    pub tls: String,
    pub read_all: bool,
    pub user: String,
    /// Each database with its size, None for one the role may not size; only on the reads that asked for them.
    pub sizes: Option<Vec<(String, Option<u64>)>>,
}

const OVERVIEW: &str = "SELECT split_part(version(), ' on ', 1), current_setting('server_version'), \
extract(epoch FROM now() - pg_postmaster_start_time())::bigint::text, to_char(pg_postmaster_start_time() AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI') || ' UTC', \
(SELECT count(*) FROM pg_stat_activity WHERE backend_type = 'client backend' OR (backend_type IS NULL AND usesysid IS NOT NULL))::text, current_setting('max_connections'), \
(SELECT count(*) FROM pg_stat_activity WHERE state = 'active' AND backend_type = 'client backend')::text, (SELECT count(*) FROM pg_stat_activity WHERE wait_event_type = 'Lock')::text, \
(SELECT sum(xact_commit)::text FROM pg_stat_database), (SELECT sum(xact_rollback)::text FROM pg_stat_database), (SELECT sum(blks_hit)::text FROM pg_stat_database), (SELECT sum(blks_read)::text FROM pg_stat_database), \
(SELECT (setting::bigint * current_setting('block_size')::bigint)::text FROM pg_settings WHERE name = 'shared_buffers'), pg_is_in_recovery()::int::text, \
extract(epoch FROM now() - pg_last_xact_replay_timestamp())::text, COALESCE(host(inet_server_addr()), ''), COALESCE(inet_server_port()::text, ''), \
COALESCE((SELECT setting FROM pg_settings WHERE name = 'data_directory'), ''), current_setting('server_encoding'), \
COALESCE((SELECT datcollate::text FROM pg_database WHERE datname = current_database()), ''), current_setting('log_timezone'), \
COALESCE((SELECT version || ', ' || cipher FROM pg_stat_ssl WHERE pid = pg_backend_pid() AND ssl), ''), pg_has_role('pg_read_all_stats', 'USAGE')::int::text, current_user::text";

const STANDBYS: &str = "SELECT COALESCE(application_name, ''), COALESCE(host(client_addr), ''), COALESCE(state, ''), COALESCE(sync_state, ''), extract(epoch FROM replay_lag)::text FROM pg_stat_replication ORDER BY 1";
const RECEIVER: &str = "SELECT COALESCE(status, ''), COALESCE(sender_host || ':' || sender_port::text, '') FROM pg_stat_wal_receiver";
/// A database the role cannot connect to is sized only with pg_read_all_stats; the server refuses the rest.
const SIZES: &str = "SELECT datname::text, CASE WHEN has_database_privilege(oid, 'CONNECT') OR pg_has_role('pg_read_all_stats', 'USAGE') THEN pg_database_size(oid)::text END FROM pg_database WHERE datallowconn AND NOT datistemplate";

fn num<R: Cells>(r: &R, i: usize) -> Option<f64> {
    secs(r, i)
}

async fn overview(db: &Db, with_sizes: bool) -> Res<Overview> {
    let (main, standbys, receiver, sizes) = read(db, async |c| {
        let main = rows(c, OVERVIEW).await?;
        let replica = main.first().is_some_and(|r| txt(r, 13) == "1");
        let (standbys, receiver) = if replica { (Vec::new(), rows(c, RECEIVER).await?) } else { (rows(c, STANDBYS).await?, Vec::new()) };
        let sizes = if with_sizes { Some(rows(c, SIZES).await?) } else { None };
        Ok((main, standbys, receiver, sizes))
    })
    .await?;
    let r = main.first().ok_or("The server did not answer the overview")?;
    let replica = txt(r, 13) == "1";
    let read_all = txt(r, 22) == "1";
    let role = if replica {
        let (status, from) = receiver.first().map(|w| (txt(w, 0), txt(w, 1))).unwrap_or_default();
        Role::Replica { from, status, lag: secs(r, 14) }
    } else {
        let standbys: Vec<Standby> = standbys.iter().map(|s| Standby { name: txt(s, 0), addr: txt(s, 1), state: txt(s, 2), sync: txt(s, 3), lag: secs(s, 4) }).collect();
        // Without the privilege the rows are there but their state is not.
        let visible = read_all || standbys.iter().all(|s| !s.state.is_empty());
        Role::Primary { standbys, visible }
    };
    let sizes = sizes.map(|list| {
        let mut v: Vec<(String, Option<u64>)> = list.iter().map(|s| (txt(s, 0), num_at(s, 1))).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    });
    Ok(Overview {
        version: txt(r, 0),
        short_version: txt(r, 1).split_whitespace().next().unwrap_or_default().to_string(),
        uptime: num_at(r, 2),
        started: txt(r, 3),
        connections: num(r, 4),
        max_connections: num(r, 5),
        active: num(r, 6),
        waiting: num(r, 7),
        commits: num(r, 8),
        rollbacks: num(r, 9),
        hits: num(r, 10),
        reads: num(r, 11),
        shared_buffers: num(r, 12),
        role: Some(role),
        host: txt(r, 15),
        port: txt(r, 16),
        data_dir: txt(r, 17),
        encoding: txt(r, 18),
        collate: txt(r, 19),
        time_zone: txt(r, 20),
        tls: txt(r, 21),
        read_all,
        user: txt(r, 23),
        sizes,
    })
}

/// The tiles' numbers from a sample and the one before it, `secs` apart.
#[derive(Debug, Default, PartialEq)]
pub struct Metrics {
    pub tps: Option<f64>,
    /// 0 to 1: of the transactions between the samples, or since the statistics were reset on the first one.
    pub commit: Option<f64>,
    pub hit: Option<f64>,
}

pub fn metrics(cur: &Overview, prev: Option<&(f64, Overview)>) -> Metrics {
    let total = |o: &Overview| Some(o.commits? + o.rollbacks?);
    let delta = prev.and_then(|(s, p)| {
        let (c, r) = (cur.commits? - p.commits?, cur.rollbacks? - p.rollbacks?);
        // A statistics reset or a restart makes the counters go down.
        (*s > 0.0 && c >= 0.0 && r >= 0.0).then_some((c + r, c, *s))
    });
    let tps = delta.map(|(n, _, s)| n / s);
    let commit = match delta {
        Some((n, c, _)) if n > 0.0 => Some(c / n),
        _ => total(cur).filter(|t| *t > 0.0).and_then(|t| Some(cur.commits? / t)),
    };
    let hit = match (cur.hits, cur.reads) {
        (Some(h), Some(r)) if h + r > 0.0 => Some(h / (h + r)),
        _ => None,
    };
    Metrics { tps, commit, hit }
}

/// What the window read about the session it is about to signal, to check again on the connection that sends it.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub pid: u64,
    pub backend_start: String,
    pub user: String,
    pub query_start: String,
}

impl Target {
    pub fn of(s: &Session) -> Target {
        Target { pid: s.pid, backend_start: s.backend_start.clone(), user: s.user.clone(), query_start: s.query_start.clone() }
    }
}

/// The statement that checks and signals in one go. pg_stat_activity is a snapshot taken at the first read of a transaction, so the
/// check and the signal must be one statement for the check to see the backend as it is now. A cancel also needs the same running
/// statement (query_start); so does a terminate behind a pooler, where a server backend serves one client per transaction.
fn signal_sql(terminate: bool, same_statement: bool) -> String {
    let f = if terminate { "pg_terminate_backend" } else { "pg_cancel_backend" };
    let stmt = if same_statement { " AND COALESCE(a.query_start::text, '') = $4" } else { "" };
    format!("SELECT COALESCE(a.backend_start::text, ''), COALESCE(a.usename::text, ''), COALESCE(a.query_start::text, ''), (a.pid = pg_backend_pid())::int::text, CASE WHEN a.pid <> pg_backend_pid() AND a.backend_start::text = $2 AND a.usename::text = $3{stmt} THEN {f}(a.pid)::text END FROM pg_stat_activity a WHERE a.pid = $1")
}

fn blind(pid: u64) -> String {
    format!("Quarry cannot tell whether process {pid} is still the session the list showed (the role lacks pg_read_all_stats), so nothing was sent.")
}

/// What the check of `signal_sql` found, in words, or None when the signal went out.
pub fn signal_outcome(t: &Target, terminate: bool, same_statement: bool, row: Option<(&str, &str, &str, bool, Option<&str>)>) -> Result<String, String> {
    let Some((start, user, query_start, mine, sent)) = row else { return Err(format!("Session {} is already gone.", t.pid)) };
    if mine {
        return Err("That is the connection Quarry sends this on, so nothing was sent.".into());
    }
    if start.is_empty() {
        return Err(blind(t.pid));
    }
    if start != t.backend_start || user != t.user {
        return Err(format!("Process {} is now another session than the list showed (the server reused its pid), so nothing was sent. Refresh and try again.", t.pid));
    }
    if same_statement && query_start != t.query_start {
        return Err(format!("Session {} is running another statement now, so nothing was sent. Refresh and try again.", t.pid));
    }
    match sent {
        Some("true") => Ok(if terminate { format!("Ended session {}", t.pid) } else { format!("Cancelled the query of session {}", t.pid) }),
        _ => Err(format!("The server could not signal process {}: it is not a session it takes signals for.", t.pid)),
    }
}

/// Cancel or Terminate on the writer, after the checks: an unlocked connection, a row the role could see, and not one of Quarry's own
/// connections. The statement goes in the query log.
async fn signal(db: &Db, t: &Target, terminate: bool, read_only: bool) -> Res<String> {
    if db.dialect != Dialect::Postgres {
        return Err("Not a PostgreSQL connection".into());
    }
    // The window asks to unlock instead; this keeps a stale or forged request from getting through.
    if read_only {
        return Err("Cancel and Terminate are locked on this connection. Unlock writes first.".into());
    }
    if t.backend_start.is_empty() {
        return Err(blind(t.pid));
    }
    if let Pool::Pg(p) = &db.pool {
        if p.is_own(t.pid, &t.backend_start) {
            return Err("That is one of Quarry's own connections. Stop its work where it runs instead.".into());
        }
    }
    let same_statement = !terminate || db.pooler();
    let sql = signal_sql(terminate, same_statement);
    let mut params = vec![Param::Int(t.pid as i64), Param::Text(t.backend_start.clone()), Param::Text(t.user.clone())];
    if same_statement {
        params.push(Param::Text(t.query_start.clone()));
    }
    let mut c = write::writer_conn(db, "", read_only).await?;
    let started = std::time::Instant::now();
    let r: Result<Vec<SqlRow>, sqlx::Error> = async {
        let mut tx = c.begin_write(db).await?;
        let rows = tx.fetch_bound(&sql, &params).await?;
        tx.commit().await?;
        Ok(rows)
    }
    .await;
    db.note("", &sql, started, &r);
    let _ = c.close().await;
    let rows = r.map_err(|x| if super::exec::sql_state(&x).as_deref() == Some("42501") { crate::db::pg_signal_refused(terminate) } else { se(x) })?;
    let row = rows.first().map(|r| (txt(r, 0), txt(r, 1), txt(r, 2), txt(r, 3) == "1", text_at(r, 4)));
    signal_outcome(t, terminate, same_statement, row.as_ref().map(|(a, b, q, m, s)| (a.as_str(), b.as_str(), q.as_str(), *m, s.as_deref())))
}

/// What the PostgreSQL Server window asked for and got.
pub enum Answer {
    Overview(Res<Overview>),
    Settings(Res<Settings>),
    Activity(Res<Activity>),
    /// How a Cancel or Terminate ended.
    Signal(Res<String>),
}

impl Worker {
    pub fn pg_server_overview(&self, db: Db, req: u64, sizes: bool) {
        self.spawn(async move { Msg::PgServer { req, ans: Answer::Overview(overview(&db, sizes).await) } });
    }

    pub fn pg_server_settings(&self, db: Db, req: u64) {
        self.spawn(async move { Msg::PgServer { req, ans: Answer::Settings(settings(&db).await) } });
    }

    pub fn pg_server_activity(&self, db: Db, req: u64) {
        self.spawn(async move { Msg::PgServer { req, ans: Answer::Activity(activity(&db).await) } });
    }

    pub fn pg_server_signal(&self, db: Db, req: u64, target: Target, terminate: bool, read_only: bool) {
        self.spawn(async move {
            let res = signal(&db, &target, terminate, read_only).await;
            Msg::PgServer { req, ans: Answer::Signal(res) }
        });
    }
}

#[cfg(test)]
#[path = "pg_server_tests.rs"]
mod tests;
