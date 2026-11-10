//! The SQL Server window's reads: the process list, the variables and Kill. Each is one function that
//! answers through `Msg::SqlServer`, tagged with the window's request id so a slow old answer is dropped.
//! View only: nothing here runs a SET.

use super::worker::{kill_query, text};
use super::{Advanced, Db, Dialect};
use crate::db::{e, Msg, Res, Worker};
use sqlx::{Connection, Executor};
use std::collections::HashMap;
use std::time::Duration;

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
}

impl Proc {
    /// A thread that is only waiting, which "Hide sleeping" hides.
    pub fn sleeping(&self) -> bool {
        self.command == "Sleep"
    }

    /// Background threads of the server (the event scheduler, replication and binlog senders) cannot be killed
    /// like a client's, and are not counted as running.
    pub fn daemon(&self) -> bool {
        matches!(self.command.as_str(), "Daemon" | "Binlog Dump" | "Binlog Dump GTID") || self.user == "event_scheduler"
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
    Some(Proc { id: pick(get, "Id")?.trim().parse().ok()?, user: s("User"), host: s("Host"), db: s("db"), command: s("Command"), secs: pick(get, "Time").and_then(|t| t.trim().parse().ok()).unwrap_or(0.0), state: s("State"), info: s("Info") })
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
    let mut v: Vec<String> = ["autocommit", "transaction_read_only", "tx_read_only"].map(String::from).to_vec();
    v.push(if dialect == Dialect::MySql { "max_execution_time" } else { "max_statement_time" }.into());
    for q in adv.session_statements(system_offset).unwrap_or_default() {
        let l = q.to_ascii_lowercase();
        if l.contains("time_zone") {
            v.push("time_zone".into());
        } else if l.starts_with("set names") {
            v.extend(["character_set_client", "character_set_connection", "character_set_results", "collation_connection"].map(String::from));
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
            let lhs = p.split([':', '=']).next()?.trim().to_ascii_lowercase();
            let lhs = lhs.trim_start_matches("session ").trim_start_matches("local ").trim_start_matches("@@session.").trim_start_matches("@@local.").trim_start_matches("@@");
            let lhs: String = lhs.chars().filter(|c| *c != '`' && *c != '"').collect();
            (!lhs.is_empty() && !lhs.starts_with('@')).then_some(lhs)
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

/// What the Server window asked for and got.
pub enum Answer {
    Procs(Res<Processes>),
    Vars(Res<Variables>),
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
    let me: u64 = rows(&mut c, "SELECT CONNECTION_ID()").await.map_err(e)?.first().and_then(|r| super::worker::raw(r, 0)).and_then(|b| std::str::from_utf8(b).ok()?.parse().ok()).unwrap_or(0);
    let list = rows(&mut c, "SHOW FULL PROCESSLIST").await.map_err(|x| {
        let denied = matches!(&x, sqlx::Error::Database(d) if d.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>().is_some_and(|m| m.number() == 1227));
        if denied { "Your account cannot list the processes: it needs the PROCESS privilege.".to_string() } else { e(x) }
    })?;
    let rows = list.iter().filter_map(|r| proc_from(&|n| text(r, n))).collect();
    Ok(Processes { rows, me, uptime: uptime(&mut c).await })
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

/// KILL <id> on a connection of its own, so a busy or exhausted pool never blocks it.
async fn kill_connection(db: &Db, id: u64) -> Res<String> {
    let mut c = tokio::time::timeout(Duration::from_secs(10), sqlx::MySqlConnection::connect_with(&db.opts)).await.map_err(|_| "Could not reach the server to end the connection".to_string())?.map_err(e)?;
    let r = c.execute(db.dialect.kill_connection(id).as_str()).await.map(|_| "Ended the connection".to_string()).map_err(|x| kill_error(&x));
    let _ = c.close().await;
    r
}

impl Worker {
    pub fn sql_server_processes(&self, db: Db, req: u64) {
        self.spawn(async move { Msg::SqlServer { req, ans: Answer::Procs(processes(&db).await) } });
    }

    pub fn sql_server_variables(&self, db: Db, req: u64, global: bool) {
        self.spawn(async move { Msg::SqlServer { req, ans: Answer::Vars(variables(&db, global).await) } });
    }

    /// `whole` ends the connection (KILL), otherwise only its running statement (KILL QUERY).
    pub fn sql_server_kill(&self, db: Db, req: u64, id: u64, whole: bool) {
        self.spawn(async move {
            let res = if whole { kill_connection(&db, id).await } else { kill_query(&db, id).await };
            Msg::SqlServer { req, ans: Answer::Kill { whole, res } }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sql::testing::every_server;
    use std::collections::HashMap;

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
            if let Some(r) = p.rows.into_iter().find(|r| r.id == id && r.info.contains("SLEEP")) {
                return Some(r);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        None
    }

    #[test]
    fn live_processlist_lists_a_running_statement_and_marks_ours() {
        every_server(|l, _| {
            let (_rx, id) = sleeper(l);
            let p = wait_listed(l, id, 10).expect("the sleeper is listed");
            assert!(p.running() && matches!(p.command.as_str(), "Query" | "Execute") && p.info.contains("SLEEP(30)"), "{p:?}");
            let all = l.w.rt.block_on(processes(&l.db)).unwrap();
            assert!(all.rows.iter().any(|r| r.id == all.me && r.info.contains("PROCESSLIST")));
            assert!(all.uptime.is_some_and(|u| u > 0));
            l.w.rt.block_on(kill_query(&l.db, id)).unwrap();
        });
    }

    #[test]
    fn live_kill_query_interrupts_and_keeps_the_connection() {
        every_server(|l, _| {
            let (rx, id) = sleeper(l);
            wait_listed(l, id, 10).expect("listed");
            l.w.rt.block_on(kill_query(&l.db, id)).unwrap();
            // MySQL answers the interrupted SLEEP with 1 (or error 1317 for a killed query); either way the same
            // connection answers the next statement.
            let (out, alive) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
            assert!(alive, "the connection answers the next statement after KILL QUERY");
            // The error is the statement's own; the connection answers the next one.
            let err = out.unwrap_err();
            assert!(err.contains("interrupted") || err.contains("1317"), "{err}");
        });
    }

    #[test]
    fn live_kill_connection_drops_it() {
        every_server(|l, _| {
            let (rx, id) = sleeper(l);
            wait_listed(l, id, 10).expect("listed");
            l.w.rt.block_on(kill_connection(&l.db, id)).unwrap();
            let _ = rx.recv_timeout(Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(300));
            let after = l.w.rt.block_on(processes(&l.db)).unwrap();
            assert!(!after.rows.iter().any(|r| r.id == id), "the connection is gone after KILL");
            // A second kill of the same id says it is gone instead of a raw code.
            let err = l.w.rt.block_on(kill_connection(&l.db, id)).unwrap_err();
            assert_eq!(err, "That connection is already gone.");
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
