//! The SQL Server window's reads: the process list, the variables and Kill. Each is one function that
//! answers through `Msg::SqlServer`, tagged with the window's request id so a slow old answer is dropped.
//! View only: nothing here runs a SET.

use super::worker::text;
use super::{Advanced, Db, Dialect};
use crate::db::{e, Msg, Res, Worker};
use sqlx::{Connection, Executor};
use std::collections::HashMap;

/// One row of SHOW FULL PROCESSLIST.
#[derive(Clone, Debug, PartialEq)]
pub struct Proc {
    pub id: u64,
    pub user: String,
    pub host: String,
    pub db: String,
    pub command: String,
    pub secs: f64,
    pub state: String,
    pub info: String,
    /// The query on one line and cut for display, made once per answer so a megabyte INSERT costs nothing per frame.
    pub line: String,
    /// Lower case user, database and the start of the query, for the filter.
    pub search: String,
}

/// The query as one line of at most `n` characters.
pub fn one_line(info: &str, n: usize) -> String {
    let mut out = String::new();
    for w in info.split_whitespace() {
        if out.chars().count() >= n {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    out.chars().take(n).collect()
}

impl Proc {
    #[allow(clippy::too_many_arguments)]
    pub fn new(id: u64, user: &str, host: &str, db: &str, command: &str, secs: f64, state: &str, info: &str) -> Proc {
        let search = format!("{} {} {}", user, db, one_line(info, 4000)).to_lowercase();
        Proc { id, user: user.into(), host: host.into(), db: db.into(), command: command.into(), secs, state: state.into(), info: info.into(), line: one_line(info, 300), search }
    }

    /// Whether `other` is still the same statement on the same connection: an id can move to another client or
    /// another statement between two reads, and a Kill must not follow it there.
    pub fn same_thread(&self, other: &Proc) -> bool {
        self.id == other.id && self.user == other.user && self.host == other.host && self.command == other.command && self.info == other.info
    }

    /// A thread that is only waiting, which "Hide sleeping" hides.
    pub fn sleeping(&self) -> bool {
        self.command == "Sleep"
    }

    /// Background threads of the server (the event scheduler, replication and binlog senders) cannot be killed
    /// like a client's, and are not counted as running.
    pub fn daemon(&self) -> bool {
        // MySQL runs replication threads as "system user" with Command Connect; MariaDB names them by role.
        matches!(self.command.as_str(), "Daemon" | "Binlog Dump" | "Binlog Dump GTID" | "Slave_IO" | "Slave_SQL" | "Slave_worker") || self.user == "event_scheduler" || self.user == "system user"
    }

    pub fn running(&self) -> bool {
        !self.sleeping() && !self.daemon()
    }
}

pub struct Processes {
    pub rows: Vec<Proc>,
    /// The connection that ran the SHOW, so the window can mark it as Quarry's own.
    pub me: u64,
    pub uptime: Option<u64>,
    /// Which server answered (its UUID, or hostname and id on MariaDB), so a Kill that lands on another backend
    /// behind a proxy is refused.
    pub identity: String,
}

/// A row from the first of `names` the server gave. MySQL names the database column `db`, MariaDB too, but a
/// proxy may change the case, so each name is tried as sent and in the other cases.
fn pick(get: &dyn Fn(&str) -> Option<String>, name: &str) -> Option<String> {
    get(name).or_else(|| get(&name.to_ascii_lowercase())).or_else(|| get(&name.to_ascii_uppercase()))
}

/// Reads a process list row by column name, so MariaDB's extra Progress column and any future one do not
/// shift anything.
pub fn proc_from(get: &dyn Fn(&str) -> Option<String>) -> Option<Proc> {
    let s = |n: &str| pick(get, n).unwrap_or_default();
    Some(Proc::new(pick(get, "Id")?.trim().parse().ok()?, &s("User"), &s("Host"), &s("db"), &s("Command"), pick(get, "Time").and_then(|t| t.trim().parse().ok()).unwrap_or(0.0), &s("State"), &s("Info")))
}

/// One variable as the window shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Var {
    pub name: String,
    pub value: String,
    /// Blank when the server cannot say, never guessed.
    pub default: Option<String>,
    /// What the server knows about it: Dynamic, Read only, or where it was set.
    pub note: String,
}

impl Var {
    /// Whether the value differs from a known default. Compared without case, since ON and on are one value.
    pub fn changed(&self) -> bool {
        self.default.as_ref().is_some_and(|d| !d.trim().eq_ignore_ascii_case(self.value.trim()))
    }
}

pub struct Variables {
    pub rows: Vec<Var>,
    pub global: bool,
    /// False when defaults could not be read (performance_schema off or denied on MySQL), so the window says why
    /// the column is blank.
    pub defaults: bool,
    pub uptime: Option<u64>,
}

/// MariaDB: one row of information_schema.SYSTEM_VARIABLES. A global-only variable has no session value, and
/// the session view shows its global one, as SHOW SESSION VARIABLES does.
pub fn maria_var(name: &str, session: Option<&str>, global: Option<&str>, default: Option<&str>, read_only: Option<&str>, want_global: bool) -> Var {
    let value = if want_global { global } else { session.or(global) }.unwrap_or_default();
    let note = if read_only.is_some_and(|r| r.eq_ignore_ascii_case("YES")) { "Read only" } else { "Dynamic" };
    Var { name: name.to_ascii_lowercase(), value: value.to_string(), default: default.map(str::to_string), note: note.into() }
}

/// MySQL: a value from SHOW VARIABLES with what performance_schema.variables_info says about it. COMPILED means
/// the global value was never changed, so it is the default; anything else leaves the default unknown.
pub fn mysql_var(name: &str, value: &str, global_value: Option<&str>, source: Option<&str>) -> Var {
    let (default, note) = match source.map(str::to_ascii_uppercase).as_deref() {
        Some("COMPILED") => (global_value.map(str::to_string), ""),
        Some("COMMAND_LINE") => (None, "From the command line"),
        Some("EXPLICIT" | "GLOBAL" | "SERVER") => (None, "From an option file"),
        Some("DYNAMIC") => (None, "Changed at runtime"),
        Some("PERSISTED" | "PERSISTED_ONLY") => (None, "Persisted"),
        _ => (None, ""),
    };
    Var { name: name.to_ascii_lowercase(), value: value.to_string(), default, note: note.into() }
}

/// The variables Quarry sets on every session of a connection, lower case, so the window can say so. The charset
/// ones come from SET NAMES, which only a non-default charset or collation sends.
pub fn set_by_quarry(adv: &Advanced, dialect: Dialect, system_offset: Option<i32>) -> Vec<String> {
    // sqlx sends SET NAMES utf8mb4 COLLATE utf8mb4_unicode_ci on every connect, so these are always Quarry's.
    let mut v: Vec<String> = ["autocommit", "transaction_read_only", "tx_read_only", "character_set_client", "character_set_connection", "character_set_results", "collation_connection"].map(String::from).to_vec();
    v.push(if dialect == Dialect::MySql { "max_execution_time" } else { "max_statement_time" }.into());
    for q in adv.session_statements(system_offset).unwrap_or_default() {
        let l = q.to_ascii_lowercase();
        if l.contains("time_zone") {
            v.push("time_zone".into());
        } else if l.contains("sql_mode") {
            v.push("sql_mode".into());
        }
    }
    for q in super::advanced::init_statements(&adv.init).unwrap_or_default() {
        v.extend(init_names(&q));
    }
    v.sort();
    v.dedup();
    v
}

/// The session variables an init statement assigns. User variables (@x) are not server variables.
fn init_names(stmt: &str) -> Vec<String> {
    let body = stmt.trim().trim_end_matches(';');
    let body = body.get(3..).unwrap_or_default();
    let (mut parts, mut depth, mut quote, mut cur) = (Vec::new(), 0i32, None::<char>, String::new());
    for c in body.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '\'' | '"') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => depth -= 1,
            (None, ',') if depth == 0 => {
                parts.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    parts.push(cur);
    parts
        .iter()
        .filter_map(|p| {
            let lhs = p.split([':', '=']).next()?.to_ascii_lowercase();
            // "SET SESSION   x", "SET SESSION\nx" and "@@session.x" all name x.
            let mut words = lhs.split_whitespace().skip_while(|w| matches!(*w, "session" | "local"));
            let w = words.next()?;
            let name: String = w.trim_start_matches("@@session.").trim_start_matches("@@local.").trim_start_matches("@@").chars().filter(|c| *c != '`' && *c != '"').collect();
            (!name.is_empty() && !name.starts_with('@')).then_some(name)
        })
        .collect()
}

/// What a failed KILL means to the person who pressed it, from the server's error number: 1095 is "not owner
/// of thread" (it needs CONNECTION_ADMIN or SUPER), 1094 an id that is already gone.
pub fn kill_error(err: &sqlx::Error) -> String {
    let n = match err {
        sqlx::Error::Database(d) => d.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>().map(|m| m.number()),
        _ => None,
    };
    match n {
        Some(1095 | 1227) => "Your account cannot kill this one: stopping another user's statement or connection needs the CONNECTION_ADMIN privilege (or SUPER).".into(),
        Some(1094) => "That connection is already gone.".into(),
        _ => e(err),
    }
}


/// SHOW GLOBAL STATUS as read: in server order for the table, and by name for rates and the overview.
#[derive(Clone, Debug, Default)]
pub struct Snap {
    pub vals: Vec<(String, String)>,
    map: HashMap<String, String>,
}

impl Snap {
    pub fn new(vals: Vec<(String, String)>) -> Snap {
        let map = vals.iter().cloned().collect();
        Snap { vals, map }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.map.get(name).map(String::as_str)
    }

    pub fn num(&self, name: &str) -> Option<f64> {
        self.get(name)?.trim().parse().ok()
    }
}

/// Status variables that only count up since start, so a difference over time is a rate. Everything else is a
/// level (threads, open tables, cache sizes) or text, which has no per-second value.
pub fn is_counter(name: &str, value: &str) -> bool {
    if value.trim().parse::<f64>().is_err() {
        return false;
    }
    const COUNTERS_IN_LEVEL_GROUPS: [&str; 6] = ["Threads_created", "Innodb_buffer_pool_pages_flushed", "Qcache_hits", "Qcache_inserts", "Qcache_lowmem_prunes", "Qcache_not_cached"];
    if COUNTERS_IN_LEVEL_GROUPS.contains(&name) {
        return true;
    }
    const LEVEL_PREFIXES: [&str; 8] = ["Threads_", "Open_", "Qcache_", "Max_", "Last_", "Ssl_", "Uptime", "Rpl_"];
    const LEVEL_PARTS: [&str; 5] = ["pending", "current", "_pages_", "_bytes_", "history_list"];
    !(LEVEL_PREFIXES.iter().any(|p| name.starts_with(p)) || LEVEL_PARTS.iter().any(|p| name.contains(p)) || matches!(name, "Innodb_page_size" | "Innodb_num_open_files" | "Compression" | "Slave_running"))
}

/// Per second between two samples `secs` apart, or None when it is not a counter, was not in the earlier sample,
/// went down (a FLUSH STATUS or restart) or the time is zero.
pub fn rate(cur: &Snap, prev: Option<&(f64, Snap)>, name: &str) -> Option<f64> {
    let (secs, p) = prev?;
    let (c, o) = (cur.num(name)?, p.num(name)?);
    (is_counter(name, cur.get(name)?) && *secs > 0.0 && c >= o).then(|| (c - o) / secs)
}

/// The groups of the Status tab's switch.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum StatusGroup {
    #[default]
    All,
    Connections,
    Queries,
    InnoDb,
    Network,
}

impl StatusGroup {
    pub const ALL: [(StatusGroup, &'static str); 5] = [(StatusGroup::All, "All"), (StatusGroup::Connections, "Connections"), (StatusGroup::Queries, "Queries"), (StatusGroup::InnoDb, "InnoDB"), (StatusGroup::Network, "Network")];

    pub fn has(self, name: &str) -> bool {
        let any = |p: &[&str]| p.iter().any(|x| name.starts_with(x));
        match self {
            StatusGroup::All => true,
            StatusGroup::Connections => any(&["Connections", "Aborted_", "Threads_", "Max_used_connections", "Connection_errors", "Locked_connects"]),
            StatusGroup::Queries => any(&["Questions", "Queries", "Com_", "Slow_queries", "Select_", "Sort_", "Handler_", "Created_tmp", "Qcache_"]),
            StatusGroup::InnoDb => any(&["Innodb_"]),
            StatusGroup::Network => any(&["Bytes_"]),
        }
    }
}

/// What the Overview tiles show, from a sample, the global variables and the sample before it.
#[derive(Debug, Default, PartialEq)]
pub struct Metrics {
    pub connected: Option<f64>,
    pub max_connections: Option<f64>,
    pub running: Option<f64>,
    pub qps: Option<f64>,
    pub questions: Option<f64>,
    pub slow: Option<f64>,
    /// 0 to 1, since start.
    pub hit: Option<f64>,
    pub pool_used: Option<f64>,
    pub pool_size: Option<f64>,
    pub net_in: Option<f64>,
    pub net_out: Option<f64>,
}

pub fn metrics(cur: &Snap, vars: &HashMap<String, String>, prev: Option<&(f64, Snap)>) -> Metrics {
    let var = |n: &str| vars.get(n).and_then(|v| v.trim().parse::<f64>().ok());
    let hit = match (cur.num("Innodb_buffer_pool_reads"), cur.num("Innodb_buffer_pool_read_requests")) {
        (Some(r), Some(q)) if q > 0.0 => Some((1.0 - r / q).clamp(0.0, 1.0)),
        _ => None,
    };
    let pool_used = cur.num("Innodb_buffer_pool_pages_data").map(|p| p * cur.num("Innodb_page_size").unwrap_or(16384.0));
    Metrics {
        connected: cur.num("Threads_connected"),
        max_connections: var("max_connections"),
        running: cur.num("Threads_running"),
        qps: rate(cur, prev, "Questions"),
        questions: cur.num("Questions"),
        slow: cur.num("Slow_queries"),
        hit,
        pool_used,
        pool_size: var("innodb_buffer_pool_size"),
        net_in: rate(cur, prev, "Bytes_received"),
        net_out: rate(cur, prev, "Bytes_sent"),
    }
}

/// The server's own view of a replica: SHOW REPLICA STATUS in MySQL 8.4's names, SHOW SLAVE STATUS in older
/// MySQL's and MariaDB's.
#[derive(Clone, Debug, PartialEq)]
pub struct Replica {
    /// The channel of a multi-source replica, or the named connection on MariaDB; empty for the default one.
    pub channel: String,
    pub source: String,
    pub io: String,
    pub sql: String,
    pub behind: Option<u64>,
    pub gtid: String,
    pub error: String,
}

pub fn replica_from(get: &dyn Fn(&str) -> Option<String>) -> Replica {
    let first = |names: &[&str]| names.iter().filter_map(|n| get(n)).find(|v| !v.is_empty()).unwrap_or_default();
    let host = first(&["Source_Host", "Master_Host"]);
    let port = first(&["Source_Port", "Master_Port"]);
    Replica {
        channel: first(&["Channel_Name", "Connection_name"]),
        source: if port.is_empty() { host } else { format!("{host}:{port}") },
        io: first(&["Replica_IO_Running", "Slave_IO_Running"]),
        sql: first(&["Replica_SQL_Running", "Slave_SQL_Running"]),
        behind: first(&["Seconds_Behind_Source", "Seconds_Behind_Master"]).parse().ok(),
        gtid: first(&["Executed_Gtid_Set", "Gtid_IO_Pos", "Gtid_Slave_Pos"]),
        error: first(&["Last_IO_Error", "Last_SQL_Error", "Last_Error"]),
    }
}

pub struct Overview {
    pub snap: Snap,
    pub vars: HashMap<String, String>,
    pub tls: String,
    /// None is "Not a replica"; an error is the server's words, with a plain one for a missing privilege.
    pub replica: Res<Vec<Replica>>,
}

/// What a missing replication privilege is called: MariaDB 10.5.9 and later asks for REPLICA MONITOR, which
/// SLAVE MONITOR and REPLICATION CLIENT stood for before.
pub fn replica_denied(d: Dialect) -> String {
    match d {
        Dialect::MySql => "Not available for this account: it needs the REPLICATION CLIENT privilege.".into(),
        Dialect::MariaDb => "Not available for this account: it needs the REPLICA MONITOR privilege (SLAVE MONITOR or REPLICATION CLIENT on older MariaDB).".into(),
    }
}

/// Variables the Overview reads.
const OVERVIEW_VARS: &str = "'max_connections','version','version_comment','hostname','port','datadir','character_set_server','collation_server','time_zone','system_time_zone','innodb_buffer_pool_size'";

/// What the Server window asked for and got.
pub enum Answer {
    Procs(Res<Processes>),
    Vars(Res<Variables>),
    Status(Res<Snap>),
    Overview(Res<Overview>),
    /// A KILL: whether it was of the whole connection, and the server's answer.
    Kill { whole: bool, res: Res<String> },
}

/// Text-protocol rows from one statement. A plain function, because inside a spawned block the executor
/// lifetimes of a borrowed connection do not infer.
async fn rows(c: &mut sqlx::MySqlConnection, q: &str) -> Result<Vec<sqlx::mysql::MySqlRow>, sqlx::Error> {
    c.fetch_all(q).await
}

async fn uptime(c: &mut sqlx::MySqlConnection) -> Option<u64> {
    let rows = rows(c, "SHOW GLOBAL STATUS LIKE 'Uptime'").await.ok()?;
    text(rows.first()?, "Value")?.trim().parse().ok()
}

async fn processes(db: &Db) -> Res<Processes> {
    let mut c = db.acquire().await?;
    let (me, identity) = me_and_identity(&mut c, db.dialect).await?;
    let list = rows(&mut c, "SHOW FULL PROCESSLIST").await.map_err(|x| {
        let denied = matches!(&x, sqlx::Error::Database(d) if d.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>().is_some_and(|m| m.number() == 1227));
        if denied { "Your account cannot list the processes: it needs the PROCESS privilege.".to_string() } else { e(x) }
    })?;
    let rows = list.iter().filter_map(|r| proc_from(&|n| text(r, n))).collect();
    Ok(Processes { rows, me, uptime: uptime(&mut c).await, identity })
}

async fn variables(db: &Db, global: bool) -> Res<Variables> {
    let mut c = db.acquire().await?;
    let up = uptime(&mut c).await;
    if db.dialect == Dialect::MariaDb {
        let q = "SELECT VARIABLE_NAME, SESSION_VALUE, GLOBAL_VALUE, DEFAULT_VALUE, READ_ONLY FROM information_schema.SYSTEM_VARIABLES ORDER BY VARIABLE_NAME";
        let list = rows(&mut c, q).await.map_err(e)?;
        let rows = list.iter().filter_map(|r| Some(maria_var(&text(r, "VARIABLE_NAME")?, text(r, "SESSION_VALUE").as_deref(), text(r, "GLOBAL_VALUE").as_deref(), text(r, "DEFAULT_VALUE").as_deref(), text(r, "READ_ONLY").as_deref(), global))).collect();
        return Ok(Variables { rows, global, defaults: true, uptime: up });
    }
    let pairs = |rows: Vec<sqlx::mysql::MySqlRow>| -> Vec<(String, String)> { rows.iter().filter_map(|r| Some((text(r, "Variable_name")?, text(r, "Value").unwrap_or_default()))).collect() };
    let vals = pairs(rows(&mut c, if global { "SHOW GLOBAL VARIABLES" } else { "SHOW SESSION VARIABLES" }).await.map_err(e)?);
    // Either read can be refused or missing (performance_schema off, an old server), and the values stand alone then.
    let sources: Option<HashMap<String, String>> = rows(&mut c, "SELECT VARIABLE_NAME, VARIABLE_SOURCE FROM performance_schema.variables_info").await.ok().map(|list| list.iter().filter_map(|r| Some((text(r, "VARIABLE_NAME")?.to_ascii_lowercase(), text(r, "VARIABLE_SOURCE")?))).collect()).filter(|m: &HashMap<_, _>| !m.is_empty());
    let globals: Option<HashMap<String, String>> = match (global, &sources) {
        (true, _) | (_, None) => None,
        _ => match rows(&mut c, "SHOW GLOBAL VARIABLES").await {
            Ok(rows) => Some(pairs(rows).into_iter().map(|(k, v)| (k.to_ascii_lowercase(), v)).collect()),
            Err(_) => None,
        },
    };
    let defaults = sources.is_some();
    let rows = vals
        .iter()
        .map(|(n, v)| {
            let key = n.to_ascii_lowercase();
            let gv = if global { Some(v.as_str()) } else { globals.as_ref().and_then(|g| g.get(&key)).map(String::as_str) };
            mysql_var(n, v, gv, sources.as_ref().and_then(|s| s.get(&key)).map(String::as_str))
        })
        .collect();
    Ok(Variables { rows, global, defaults, uptime: up })
}

async fn status_snap(c: &mut sqlx::MySqlConnection) -> Res<Snap> {
    let list = rows(c, "SHOW GLOBAL STATUS").await.map_err(e)?;
    Ok(Snap::new(list.iter().filter_map(|r| Some((text(r, "Variable_name")?, text(r, "Value").unwrap_or_default()))).collect()))
}

fn mysql_number(x: &sqlx::Error) -> Option<u16> {
    match x {
        sqlx::Error::Database(d) => d.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>().map(|m| m.number()),
        _ => None,
    }
}

/// Every replication channel. MySQL 8.4 only knows REPLICA, older MySQL only SLAVE; MariaDB's plain statement shows
/// the default connection only, so ALL SLAVES is what lists the named ones of a multi-source replica.
async fn replicas(c: &mut sqlx::MySqlConnection, d: Dialect) -> Res<Vec<Replica>> {
    let qs: &[&str] = if d == Dialect::MariaDb { &["SHOW ALL SLAVES STATUS"] } else { &["SHOW REPLICA STATUS", "SHOW SLAVE STATUS"] };
    let mut last = None;
    for q in qs {
        match rows(c, q).await {
            Ok(list) => return Ok(list.iter().map(|r| replica_from(&|n| text(r, n))).collect()),
            // 1064 is the syntax error of a server that lacks the other spelling; any other error is the answer.
            Err(x) if mysql_number(&x) == Some(1064) => last = Some("This server does not report replication status.".to_string()),
            Err(x) if matches!(mysql_number(&x), Some(1227 | 1045)) => return Err(replica_denied(d)),
            Err(x) => return Err(e(x)),
        }
    }
    Err(last.unwrap_or_default())
}

async fn overview(db: &Db) -> Res<Overview> {
    let mut c = db.acquire().await?;
    let snap = status_snap(&mut c).await?;
    let q = format!("SHOW GLOBAL VARIABLES WHERE Variable_name IN ({OVERVIEW_VARS})");
    let vars = rows(&mut c, &q).await.map_err(e)?.iter().filter_map(|r| Some((text(r, "Variable_name")?.to_ascii_lowercase(), text(r, "Value").unwrap_or_default()))).collect();
    // The connection's own TLS state, which is what Quarry's session uses.
    let tls = match rows(&mut c, "SHOW SESSION STATUS WHERE Variable_name IN ('Ssl_version','Ssl_cipher')").await {
        Ok(l) => {
            let g = |n: &str| l.iter().find(|r| text(r, "Variable_name").as_deref() == Some(n)).and_then(|r| text(r, "Value")).unwrap_or_default();
            [g("Ssl_version"), g("Ssl_cipher")].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(", ")
        }
        Err(_) => String::new(),
    };
    let replica = replicas(&mut c, db.dialect).await;
    Ok(Overview { snap, vars, tls, replica })
}

async fn status(db: &Db) -> Res<Snap> {
    let mut c = db.acquire().await?;
    status_snap(&mut c).await
}

/// What the window read about the thread it is about to kill, to check again on the connection that kills.
#[derive(Clone, Debug, PartialEq)]
pub struct KillTarget {
    pub id: u64,
    pub user: String,
    pub host: String,
    pub info: String,
    pub identity: String,
}

impl KillTarget {
    pub fn of(p: &Proc, identity: &str) -> KillTarget {
        KillTarget { id: p.id, user: p.user.clone(), host: p.host.clone(), info: p.info.clone(), identity: identity.to_string() }
    }
}

/// Which server a connection reached. MySQL has a UUID; MariaDB's hostname and server id are what tell a
/// replica or a cluster node apart.
async fn identity(c: &mut sqlx::MySqlConnection, d: Dialect) -> String {
    match rows(c, identity_columns(d)).await {
        Ok(r) => r.first().map(|r| identity_of(r, 0, d)).unwrap_or_default(),
        Err(_) => String::new(),
    }
}

fn identity_columns(d: Dialect) -> &'static str {
    if d == Dialect::MariaDb { "SELECT @@hostname, @@server_id" } else { "SELECT @@server_uuid" }
}

/// The identity columns of `identity_columns` starting at column `from`, joined.
fn identity_of(r: &sqlx::mysql::MySqlRow, from: usize, d: Dialect) -> String {
    (from..from + if d == Dialect::MariaDb { 2 } else { 1 }).map(|i| super::worker::text_at(r, i).unwrap_or_default()).collect::<Vec<_>>().join("/")
}

/// This connection's id and the server's identity in one round trip. A server that refuses the identity columns
/// still answers with the id alone, as the two reads did apart, so the id never depends on the identity. Both are
/// read on the connection that then lists the processes, and again on every poll, since a changed identity is how
/// a failover shows.
async fn me_and_identity(c: &mut sqlx::MySqlConnection, d: Dialect) -> Res<(u64, String)> {
    let q = identity_columns(d).replacen("SELECT ", "SELECT CONNECTION_ID(), ", 1);
    if let Ok(r) = rows(c, &q).await {
        if let Some(r) = r.first() {
            return Ok((super::worker::num_at(r, 0).unwrap_or(0), identity_of(r, 1, d)));
        }
    }
    let r = rows(c, "SELECT CONNECTION_ID()").await.map_err(e)?;
    Ok((r.first().and_then(|r| super::worker::num_at(r, 0)).unwrap_or(0), String::new()))
}

/// Whether the thread the server now has under `t.id` is the one the list showed.
pub fn target_matches(t: &KillTarget, user: &str, host: &str, info: &str) -> bool {
    t.user == user && t.host == host && t.info.trim() == info.trim()
}

/// KILL QUERY or KILL on a connection of its own (a busy or exhausted pool never blocks it), after checking on
/// that same connection that it reached the same server and that the thread is still the one the list showed.
async fn kill_checked(db: &Db, t: &KillTarget, whole: bool, read_only: bool) -> Res<String> {
    // The window hides Kill on a locked connection; this keeps a stale or forged request from getting through.
    if read_only {
        return Err("Kill is locked on this connection. Unlock writes first.".into());
    }
    let mut c = super::worker::side_conn(db, "Could not reach the server to kill it").await?;
    let r = kill_on(&mut c, db.dialect, t, whole).await;
    let _ = c.close().await;
    r
}

async fn kill_on(c: &mut sqlx::MySqlConnection, d: Dialect, t: &KillTarget, whole: bool) -> Res<String> {
    if identity(c, d).await != t.identity {
        return Err("The connection now reaches a different server than the list was read from, so nothing was killed. Refresh and try again.".into());
    }
    let r = rows(c, &format!("SELECT USER, HOST, INFO FROM information_schema.PROCESSLIST WHERE ID = {}", t.id)).await.map_err(e)?;
    let Some(r) = r.first() else { return Err("That connection is already gone.".into()) };
    if !target_matches(t, &text(r, "USER").unwrap_or_default(), &text(r, "HOST").unwrap_or_default(), &text(r, "INFO").unwrap_or_default()) {
        return Err("That connection is now running something else, so nothing was killed. Refresh and try again.".into());
    }
    let q = if whole { d.kill_connection(t.id) } else { d.cancel(t.id) };
    c.execute(q.as_str()).await.map(|_| if whole { "Ended the connection" } else { "Stopped the statement" }.to_string()).map_err(|x| kill_error(&x))
}

impl Worker {
    pub fn sql_server_processes(&self, db: Db, req: u64) {
        self.spawn(async move { Msg::SqlServer { req, ans: Answer::Procs(processes(&db).await) } });
    }

    pub fn sql_server_variables(&self, db: Db, req: u64, global: bool) {
        self.spawn(async move { Msg::SqlServer { req, ans: Answer::Vars(variables(&db, global).await) } });
    }

    pub fn sql_server_overview(&self, db: Db, req: u64) {
        self.spawn(async move { Msg::SqlServer { req, ans: Answer::Overview(overview(&db).await) } });
    }

    pub fn sql_server_status(&self, db: Db, req: u64) {
        self.spawn(async move { Msg::SqlServer { req, ans: Answer::Status(status(&db).await) } });
    }

    /// `whole` ends the connection (KILL), otherwise only its running statement (KILL QUERY).
    pub fn sql_server_kill(&self, db: Db, req: u64, target: KillTarget, whole: bool, read_only: bool) {
        self.spawn(async move {
            let res = kill_checked(&db, &target, whole, read_only).await;
            Msg::SqlServer { req, ans: Answer::Kill { whole, res } }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sql::testing::every_server;
    use crate::db::sql::worker::kill_query;
    use std::collections::HashMap;
    use std::time::Duration;

    fn row(cols: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = cols.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |n| m.get(n).cloned()
    }

    #[test]
    fn mysql_processlist_row() {
        let g = row(&[("Id", "8812"), ("User", "report"), ("Host", "10.0.4.12:51522"), ("db", "analytics"), ("Command", "Query"), ("Time", "142"), ("State", "Sending data"), ("Info", "SELECT 1")]);
        let p = proc_from(&g).unwrap();
        assert_eq!((p.id, p.user.as_str(), p.db.as_str(), p.secs, p.state.as_str()), (8812, "report", "analytics", 142.0, "Sending data"));
        assert!(p.running() && !p.sleeping());
    }

    #[test]
    fn mariadb_processlist_row_has_progress_and_nulls() {
        // MariaDB adds Progress; a daemon row has no db, no Info and a NULL State.
        let g = row(&[("Id", "5"), ("User", "event_scheduler"), ("Host", "localhost"), ("Command", "Daemon"), ("Time", "90211"), ("Progress", "0.000")]);
        let p = proc_from(&g).unwrap();
        assert_eq!((p.id, p.db.as_str(), p.info.as_str()), (5, "", ""));
        assert!(p.daemon() && !p.running());
        let sleeping = proc_from(&row(&[("Id", "9"), ("Command", "Sleep"), ("Time", "3.5"), ("Progress", "0.000")])).unwrap();
        assert!(sleeping.sleeping() && !sleeping.running());
        assert_eq!(sleeping.secs, 3.5);
        assert!(proc_from(&row(&[("User", "x")])).is_none());
    }

    #[test]
    fn mysql_defaults_come_only_from_compiled() {
        let v = mysql_var("Wait_Timeout", "600", Some("28800"), Some("COMPILED"));
        assert_eq!((v.name.as_str(), v.default.as_deref(), v.changed()), ("wait_timeout", Some("28800"), true));
        let v = mysql_var("connect_timeout", "10", Some("10"), Some("COMPILED"));
        assert!(!v.changed());
        let v = mysql_var("max_connections", "500", Some("500"), Some("EXPLICIT"));
        assert_eq!((v.default, v.note.as_str()), (None, "From an option file"));
        let v = mysql_var("x", "1", None, None);
        assert!(v.default.is_none() && !v.changed());
    }

    #[test]
    fn mariadb_variable_columns() {
        let v = maria_var("WAIT_TIMEOUT", Some("600"), Some("28800"), Some("28800"), Some("NO"), false);
        assert_eq!((v.value.as_str(), v.default.as_deref(), v.note.as_str(), v.changed()), ("600", Some("28800"), "Dynamic", true));
        let g = maria_var("WAIT_TIMEOUT", Some("600"), Some("28800"), Some("28800"), Some("NO"), true);
        assert!(!g.changed());
        // A global-only variable has no session value; the session view shows the global one.
        let ro = maria_var("DATADIR", None, Some("/var/lib/mysql/"), Some("/var/lib/mysql/"), Some("YES"), false);
        assert_eq!((ro.value.as_str(), ro.note.as_str()), ("/var/lib/mysql/", "Read only"));
        assert!(!maria_var("OLD_MODE", Some("ON"), Some("ON"), Some("on"), Some("NO"), false).changed());
    }

    #[test]
    fn what_quarry_sets_per_session() {
        let base = set_by_quarry(&Advanced::default(), Dialect::MySql, None);
        assert!(base.contains(&"max_execution_time".to_string()) && base.contains(&"autocommit".to_string()));
        assert!(!base.contains(&"time_zone".to_string()) && !base.contains(&"max_statement_time".to_string()));
        assert!(set_by_quarry(&Advanced::default(), Dialect::MariaDb, None).contains(&"max_statement_time".to_string()));
        let adv = Advanced { zone: crate::db::sql::Zone::Utc, sql_mode: Some(vec!["ANSI_QUOTES".into()]), init: "SET SESSION wait_timeout = 60, @x = 1; SET @@session.group_concat_max_len = 2000".into(), ..Default::default() };
        let v = set_by_quarry(&adv, Dialect::MySql, None);
        for n in ["time_zone", "sql_mode", "wait_timeout", "group_concat_max_len"] {
            assert!(v.contains(&n.to_string()), "{n} in {v:?}");
        }
        assert!(!v.iter().any(|n| n.starts_with('@')));
        let adv = Advanced { charset: crate::db::sql::Charset::Utf8mb3, ..Default::default() };
        assert!(set_by_quarry(&adv, Dialect::MySql, None).contains(&"collation_connection".to_string()));
    }

    #[test]
    fn system_and_replication_threads_are_not_client_threads() {
        for (user, cmd) in [("system user", "Connect"), ("system user", "Query"), ("root", "Slave_IO"), ("root", "Slave_SQL"), ("root", "Slave_worker"), ("repl", "Binlog Dump GTID"), ("event_scheduler", "Daemon")] {
            let p = proc_from(&row(&[("Id", "7"), ("User", user), ("Command", cmd), ("Time", "10")])).unwrap();
            assert!(p.daemon() && !p.running(), "{user} {cmd}");
        }
        assert!(proc_from(&row(&[("Id", "7"), ("User", "app"), ("Command", "Query")])).unwrap().running());
    }

    #[test]
    fn a_thread_that_moved_is_not_the_same_thread() {
        let p = |id, user: &str, cmd: &str, info: &str| Proc::new(id, user, "10.0.0.1:5", "shop", cmd, 1.0, "", info);
        let a = p(9, "app", "Query", "SELECT 1");
        assert!(a.same_thread(&p(9, "app", "Query", "SELECT 1")));
        assert!(!a.same_thread(&p(9, "app", "Query", "SELECT 2")));
        assert!(!a.same_thread(&p(9, "app", "Sleep", "")));
        assert!(!a.same_thread(&p(9, "other", "Query", "SELECT 1")));
        assert!(!a.same_thread(&p(10, "app", "Query", "SELECT 1")));
        let t = KillTarget::of(&a, "uuid");
        assert!(target_matches(&t, "app", "10.0.0.1:5", "SELECT 1 ") && !target_matches(&t, "app", "10.0.0.1:5", "SELECT 3"));
    }

    #[test]
    fn long_queries_are_cut_once_for_display() {
        let long = format!("INSERT INTO t VALUES {}", "(1, 'a'),\n\t".repeat(100_000));
        let p = Proc::new(1, "u", "h", "d", "Query", 0.0, "", &long);
        assert!(p.line.chars().count() <= 300 && !p.line.contains('\n') && p.line.starts_with("INSERT INTO t VALUES (1, 'a'),"));
        assert!(p.search.len() <= 4100 && p.search.contains("insert into"));
        assert_eq!(one_line("  SELECT\n  1  ", 300), "SELECT 1");
    }

    #[test]
    fn init_names_ignore_odd_spacing_and_charset_is_always_quarrys() {
        assert_eq!(init_names("SET SESSION   wait_timeout = 1"), ["wait_timeout"]);
        assert_eq!(init_names("SET SESSION\nwait_timeout=1, @@local.lock_wait_timeout := 2, @u = 3"), ["wait_timeout", "lock_wait_timeout"]);
        assert_eq!(init_names("SET\tLOCAL  `group_concat_max_len` = 5;"), ["group_concat_max_len"]);
        let v = set_by_quarry(&Advanced::default(), Dialect::MariaDb, None);
        for n in ["character_set_client", "character_set_connection", "character_set_results", "collation_connection"] {
            assert!(v.contains(&n.to_string()), "{n}");
        }
    }

    #[test]
    fn replication_channels_and_privilege_names() {
        let r = replica_from(&row(&[("Channel_Name", "east"), ("Source_Host", "h")]));
        assert_eq!(r.channel, "east");
        assert_eq!(replica_from(&row(&[("Connection_name", "west"), ("Master_Host", "m")])).channel, "west");
        assert!(replica_denied(Dialect::MySql).contains("REPLICATION CLIENT"));
        assert!(replica_denied(Dialect::MariaDb).contains("REPLICA MONITOR"));
    }

    #[test]
    fn live_replication_reads_on_both_servers_and_a_plain_one_has_none() {
        every_server(|l, _| {
            let r = l.w.rt.block_on(async {
                let mut c = l.db.acquire().await.unwrap();
                replicas(&mut c, l.db.dialect).await
            });
            assert!(r.unwrap().is_empty());
        });
    }

    fn snap(v: &[(&str, &str)]) -> Snap {
        Snap::new(v.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
    }

    #[test]
    fn rates_come_from_two_samples_and_only_for_counters() {
        let a = snap(&[("Questions", "1000"), ("Threads_connected", "14"), ("Bytes_received", "0"), ("Version_x", "abc"), ("Uptime", "100")]);
        let b = snap(&[("Questions", "1824"), ("Threads_connected", "9"), ("Bytes_received", "2048"), ("Version_x", "abd"), ("Uptime", "102")]);
        let prev = Some((2.0, a));
        assert_eq!(rate(&b, prev.as_ref(), "Questions"), Some(412.0));
        assert_eq!(rate(&b, prev.as_ref(), "Bytes_received"), Some(1024.0));
        // Levels, text and the first sample have no rate.
        assert_eq!(rate(&b, prev.as_ref(), "Threads_connected"), None);
        assert_eq!(rate(&b, prev.as_ref(), "Uptime"), None);
        assert_eq!(rate(&b, prev.as_ref(), "Version_x"), None);
        assert_eq!(rate(&b, None, "Questions"), None);
        // A counter that went down was reset by FLUSH STATUS or a restart.
        assert_eq!(rate(&prev.as_ref().unwrap().1, Some(&(2.0, b.clone())), "Questions"), None);
        assert_eq!(rate(&b, Some(&(0.0, b.clone())), "Questions"), None);
        assert!(is_counter("Threads_created", "5") && !is_counter("Threads_running", "5") && !is_counter("Innodb_buffer_pool_pages_free", "5") && is_counter("Innodb_buffer_pool_reads", "5") && !is_counter("Innodb_row_lock_current_waits", "0"));
    }

    #[test]
    fn overview_metrics() {
        let a = snap(&[("Questions", "1000"), ("Bytes_received", "0"), ("Bytes_sent", "0")]);
        let b = snap(&[("Threads_connected", "14"), ("Threads_running", "3"), ("Questions", "1824"), ("Slow_queries", "3"), ("Innodb_buffer_pool_reads", "8"), ("Innodb_buffer_pool_read_requests", "1000"), ("Innodb_buffer_pool_pages_data", "65536"), ("Innodb_page_size", "16384"), ("Bytes_received", "2000"), ("Bytes_sent", "6000")]);
        let vars: HashMap<String, String> = [("max_connections", "151"), ("innodb_buffer_pool_size", "8589934592")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let m = metrics(&b, &vars, Some(&(2.0, a)));
        assert_eq!((m.connected, m.max_connections, m.running, m.qps, m.slow), (Some(14.0), Some(151.0), Some(3.0), Some(412.0), Some(3.0)));
        assert_eq!((m.net_in, m.net_out, m.pool_used, m.pool_size), (Some(1000.0), Some(3000.0), Some(1073741824.0), Some(8589934592.0)));
        assert!((m.hit.unwrap() - 0.992).abs() < 1e-9);
        // The first sample has counts since start but no rates.
        let first = metrics(&b, &vars, None);
        assert_eq!((first.qps, first.questions, first.net_in), (None, Some(1824.0), None));
        assert_eq!(metrics(&Snap::default(), &HashMap::new(), None), Metrics::default());
    }

    #[test]
    fn status_groups() {
        use StatusGroup::*;
        for (g, yes, no) in [(Connections, "Threads_running", "Questions"), (Queries, "Com_select", "Bytes_sent"), (InnoDb, "Innodb_row_lock_waits", "Threads_running"), (Network, "Bytes_received", "Innodb_data_read")] {
            assert!(g.has(yes) && !g.has(no), "{g:?} {yes} {no}");
            assert!(All.has(no));
        }
    }

    #[test]
    fn replica_columns_of_both_servers() {
        let my = replica_from(&row(&[("Source_Host", "db0"), ("Source_Port", "3306"), ("Replica_IO_Running", "Yes"), ("Replica_SQL_Running", "Yes"), ("Seconds_Behind_Source", "0"), ("Executed_Gtid_Set", "3e11:1-5"), ("Last_IO_Error", "")]));
        assert_eq!((my.source.as_str(), my.io.as_str(), my.behind, my.gtid.as_str(), my.error.as_str()), ("db0:3306", "Yes", Some(0), "3e11:1-5", ""));
        let maria = replica_from(&row(&[("Master_Host", "m1"), ("Master_Port", "3306"), ("Slave_IO_Running", "Connecting"), ("Slave_SQL_Running", "Yes"), ("Seconds_Behind_Master", ""), ("Gtid_IO_Pos", "0-1-7"), ("Last_IO_Error", "error connecting")]));
        assert_eq!((maria.source.as_str(), maria.io.as_str(), maria.behind, maria.gtid.as_str(), maria.error.as_str()), ("m1:3306", "Connecting", None, "0-1-7", "error connecting"));
    }

    #[test]
    fn live_status_and_overview_read_on_both_servers() {
        every_server(|l, _| {
            let st = l.w.rt.block_on(status(&l.db)).unwrap();
            assert!(st.num("Questions").is_some_and(|q| q > 0.0) && st.num("Uptime").is_some_and(|u| u > 0.0) && st.vals.len() > 100);
            let ov = l.w.rt.block_on(overview(&l.db)).unwrap();
            assert!(ov.vars.get("max_connections").is_some_and(|v| v.parse::<u32>().is_ok()) && ov.vars.contains_key("version") && ov.vars.contains_key("datadir"));
            let m = metrics(&ov.snap, &ov.vars, None);
            assert!(m.connected.is_some() && m.max_connections.is_some() && m.hit.is_some() && m.pool_size.is_some());
            // A fresh container is not a replica.
            assert!(ov.replica.unwrap().is_empty());
            // Two samples apart give a rate for a counter that moved.
            l.one("SELECT 1");
            std::thread::sleep(Duration::from_millis(1100));
            let st2 = l.w.rt.block_on(status(&l.db)).unwrap();
            assert!(rate(&st2, Some(&(1.1, st)), "Questions").is_some_and(|r| r > 0.0));
        });
    }

    /// A SELECT SLEEP on its own connection, and that connection's id.
    fn sleeper(l: &super::super::testing::Live) -> (std::sync::mpsc::Receiver<(Result<String, String>, bool)>, u64) {
        let opts = l.db.opts.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let (idtx, idrx) = std::sync::mpsc::channel();
        l.w.rt.spawn(async move {
            let mut c = sqlx::MySqlConnection::connect_with(&opts).await.unwrap();
            let id: u64 = sqlx::query_scalar("SELECT CONNECTION_ID()").fetch_one(&mut c).await.unwrap();
            idtx.send(id).unwrap();
            // SLEEP itself answers 1 when interrupted, so a second row is what turns the kill into an error.
            let r = sqlx::query_scalar::<_, i64>("SELECT SLEEP(30) FROM (SELECT 1 UNION ALL SELECT 2) t").fetch_all(&mut c).await.map(|n| format!("slept {n:?}")).map_err(|x| e(x));
            let alive = sqlx::query_scalar::<_, i64>("SELECT 1").fetch_one(&mut c).await.is_ok();
            let _ = tx.send((r, alive));
        });
        (rx, idrx.recv_timeout(Duration::from_secs(10)).unwrap())
    }

    fn wait_listed(l: &super::super::testing::Live, id: u64, secs: u64) -> Option<Proc> {
        for _ in 0..secs * 10 {
            let p = l.w.rt.block_on(processes(&l.db)).unwrap();
            // sqlx prepares first, so the text shows for a moment under Command "Prepare" before it runs.
            if let Some(r) = p.rows.into_iter().find(|r| r.id == id && r.info.contains("SLEEP") && r.command != "Prepare") {
                return Some(r);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        None
    }

    fn target(l: &super::super::testing::Live, p: &Proc) -> KillTarget {
        let ident = l.w.rt.block_on(processes(&l.db)).unwrap().identity;
        KillTarget::of(p, &ident)
    }

    #[test]
    fn live_processlist_lists_a_running_statement_and_marks_ours() {
        every_server(|l, _| {
            let (_rx, id) = sleeper(l);
            let p = wait_listed(l, id, 10).expect("the sleeper is listed");
            assert!(p.running() && matches!(p.command.as_str(), "Query" | "Execute") && p.info.contains("SLEEP(30)"), "{p:?}");
            let all = l.w.rt.block_on(processes(&l.db)).unwrap();
            assert!(all.rows.iter().any(|r| r.id == all.me && r.info.contains("PROCESSLIST")));
            assert!(all.uptime.is_some_and(|u| u > 0) && !all.identity.is_empty());
            l.w.rt.block_on(kill_checked(&l.db, &target(l, &p), false, false)).unwrap();
        });
    }

    #[test]
    fn live_kill_query_interrupts_and_keeps_the_connection() {
        every_server(|l, _| {
            let (rx, id) = sleeper(l);
            let p = wait_listed(l, id, 10).expect("listed");
            l.w.rt.block_on(kill_checked(&l.db, &target(l, &p), false, false)).unwrap();
            let (out, alive) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
            assert!(alive, "the connection answers the next statement after KILL QUERY");
            // The error is the statement's own.
            let err = out.unwrap_err();
            assert!(err.contains("interrupted") || err.contains("1317"), "{err}");
        });
    }

    #[test]
    fn live_kill_connection_drops_it() {
        every_server(|l, _| {
            let (rx, id) = sleeper(l);
            let p = wait_listed(l, id, 10).expect("listed");
            let t = target(l, &p);
            l.w.rt.block_on(kill_checked(&l.db, &t, true, false)).unwrap();
            let _ = rx.recv_timeout(Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(300));
            let after = l.w.rt.block_on(processes(&l.db)).unwrap();
            assert!(!after.rows.iter().any(|r| r.id == id), "the connection is gone after KILL");
            // A second kill of the same id says it is gone instead of a raw code.
            assert_eq!(l.w.rt.block_on(kill_checked(&l.db, &t, true, false)).unwrap_err(), "That connection is already gone.");
            assert_eq!(l.w.rt.block_on(kill_query(&l.db, 4_000_000_000)).unwrap_err(), "That connection is already gone.");
        });
    }

    #[test]
    fn live_a_kill_on_a_locked_connection_is_refused_and_kills_nothing() {
        every_server(|l, _| {
            let (rx, id) = sleeper(l);
            let p = wait_listed(l, id, 10).expect("listed");
            let t = target(l, &p);
            for whole in [false, true] {
                let err = l.w.rt.block_on(kill_checked(&l.db, &t, whole, true)).unwrap_err();
                assert!(err.contains("locked"), "{err}");
            }
            assert!(rx.recv_timeout(Duration::from_millis(500)).is_err(), "the statement still runs");
            l.w.rt.block_on(kill_checked(&l.db, &t, false, false)).unwrap();
            let _ = rx.recv_timeout(Duration::from_secs(10));
        });
    }

    #[test]
    fn live_a_kill_of_something_else_is_refused() {
        every_server(|l, _| {
            let (rx, id) = sleeper(l);
            let p = wait_listed(l, id, 10).expect("listed");
            // The id now runs another statement, another client, or lives on another server: nothing dies.
            let ok = target(l, &p);
            for bad in [KillTarget { info: "SELECT 1".into(), ..ok.clone() }, KillTarget { user: "someone".into(), ..ok.clone() }, KillTarget { host: "10.9.9.9:1".into(), ..ok.clone() }] {
                let err = l.w.rt.block_on(kill_checked(&l.db, &bad, true, false)).unwrap_err();
                assert!(err.contains("running something else"), "{err}");
            }
            let err = l.w.rt.block_on(kill_checked(&l.db, &KillTarget { identity: "other-server".into(), ..ok.clone() }, false, false)).unwrap_err();
            assert!(err.contains("different server"), "{err}");
            assert!(rx.recv_timeout(Duration::from_millis(500)).is_err(), "the statement still runs");
            l.w.rt.block_on(kill_checked(&l.db, &ok, false, false)).unwrap();
            let _ = rx.recv_timeout(Duration::from_secs(10));
        });
    }

    #[test]
    fn live_killing_another_users_thread_without_the_privilege_says_so() {
        every_server(|l, _| {
            let (rx, id) = sleeper(l);
            wait_listed(l, id, 10).expect("listed");
            l.sql("DROP USER IF EXISTS 'q_nokill'@'%'");
            l.sql("CREATE USER 'q_nokill'@'%' IDENTIFIED BY 'nokill-pw'");
            let opts = l.db.opts.clone().username("q_nokill").password("nokill-pw");
            let low = Db { opts, ..l.db.clone() };
            let err = l.w.rt.block_on(kill_query(&low, id)).unwrap_err();
            l.sql("DROP USER 'q_nokill'@'%'");
            assert!(err.contains("CONNECTION_ADMIN"), "{err}");
            assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "the statement still runs");
            l.w.rt.block_on(kill_query(&l.db, id)).unwrap();
            let _ = rx.recv_timeout(Duration::from_secs(10));
        });
    }

    #[test]
    fn live_variables_have_values_and_mariadb_defaults() {
        every_server(|l, var| {
            for global in [false, true] {
                let v = l.w.rt.block_on(variables(&l.db, global)).unwrap();
                let one = v.rows.iter().find(|r| r.name == "max_allowed_packet").expect("a known variable");
                assert!(one.value.parse::<u64>().is_ok(), "{one:?}");
                if var == "QUARRY_TEST_MARIADB" {
                    assert!(one.default.is_some() && !one.note.is_empty(), "{one:?}");
                    assert!(v.defaults);
                }
                assert_eq!(v.global, global);
            }
            // The session view carries the statement timeout Quarry set.
            let s = l.w.rt.block_on(variables(&l.db, false)).unwrap();
            let n = if var == "QUARRY_TEST_MARIADB" { "max_statement_time" } else { "max_execution_time" };
            assert!(s.rows.iter().any(|r| r.name == n));
        });
    }
}
