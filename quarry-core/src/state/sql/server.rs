//! State behind src/app/sql/server.rs, which draws it.

use crate::db::sql::server::locks::Locks;
use crate::db::sql::server::{Overview, Proc, Processes, Replica, Snap, StatusGroup, Var, Variables};
use std::collections::HashMap;
use std::time::Instant;
use crate::db;
use std::time::Duration;

/// A Kill waiting for its second click, with everything the list showed about its target.
#[derive(Clone)]
pub struct Confirm {
    pub p: Proc,
    pub identity: String,
    /// The whole connection ends (KILL) instead of only its statement (KILL QUERY).
    pub whole: bool,
    /// `arm_time` when it was armed.
    pub at: f64,
}

#[derive(Clone, Copy, PartialEq, Default)]
pub enum SqlServerTab {
    Overview,
    Variables,
    Status,
    #[default]
    Processes,
    Locks,
}

#[derive(Default)]
pub struct SqlServerWin {
    pub conn: u64,
    pub tab: SqlServerTab,
    /// The variables Quarry sets on every session of this connection, for the Variables note.
    pub quarry: Vec<String>,
    pub uptime: Option<u64>,
    /// Each read has its own request id, so an answer to an older one, or to a closed window, is dropped.
    pub procs_req: u64,
    pub vars_req: u64,
    pub procs_wait: bool,
    pub locks_req: u64,
    pub locks_wait: bool,
    pub locks: Option<db::Res<std::sync::Arc<Locks>>>,
    /// The blocker chains of `locks`, built once per answer and not on every frame.
    pub chains: std::sync::Arc<Vec<db::sql::server::locks::Chain>>,
    pub vars_wait: bool,
    /// When to ask for the processes again; None asks on the next frame.
    pub next: Option<Instant>,
    pub refresh: bool,
    pub procs: Option<db::Res<Processes>>,
    pub auto: bool,
    pub hide_sleeping: bool,
    pub filter: String,
    /// The selected thread by id, so a refresh keeps it.
    pub sel: Option<u64>,
    /// The thread waiting for its second click, and whether the whole connection or only its statement ends.
    pub confirm: Option<Confirm>,
    /// Whether the connection is unlocked, read each frame. Kill needs it.
    pub writable: bool,
    /// Quarry's own sessions on this server by thread id, with what each one is, read each frame. The list
    /// itself marks only the connection that ran SHOW PROCESSLIST.
    pub own: HashMap<u64, String>,
    /// Unlock was clicked in the note above the list.
    pub unlock: bool,
    pub global: bool,
    pub var_filter: String,
    pub vars: Option<db::Res<Variables>>,
    pub ov_req: u64,
    pub ov_wait: bool,
    pub ov: Option<db::Res<(Instant, Overview)>>,
    /// The sample before the latest and the seconds between them, for per second values.
    pub ov_prev: Option<(f64, Snap)>,
    pub sz_req: u64,
    pub sz_wait: bool,
    /// The last sizes that came in; a failed read keeps them and says why.
    pub sizes: Option<Vec<(String, Option<u64>)>>,
    pub sizes_err: Option<String>,
    pub sizes_at: Option<Instant>,
    pub st_req: u64,
    pub st_wait: bool,
    pub st: Option<db::Res<(Instant, Snap)>>,
    pub st_prev: Option<(f64, Snap)>,
    /// The names of the latest status sample in lowercase, in its order, so the filter does not fold every name each frame.
    pub st_lower: Vec<String>,
    pub st_auto: bool,
    pub st_filter: String,
    pub st_group: StatusGroup,
}

impl SqlServerWin {
    pub fn new(conn: u64, quarry: Vec<String>) -> Self {
        Self { conn, quarry, auto: true, hide_sleeping: true, writable: true, st_auto: true, ..Default::default() }
    }

    pub fn set_lower(&mut self) {
        self.st_lower = match &self.st {
            Some(Ok((_, snap))) => snap.vals.iter().map(|(n, _)| n.to_lowercase()).collect(),
            _ => Vec::new(),
        };
    }

    /// Canned answers for the offline demo: `variables` opens that tab, anything else the processes.
    pub fn demo(conn: u64, tab: &str) -> Self {
        let p = Proc::new;
        let mut rows = vec![
            p(8812, "report", "10.0.4.12:51522", "analytics", "Query", 142.0, "Sending data", "SELECT customer_id, SUM(total) FROM orders GROUP BY customer_id"),
            p(8790, "app", "10.0.2.31:40118", "shop", "Query", 3.0, "updating", "UPDATE payments SET state = 'captured' WHERE id = 90211"),
            p(8766, "event_scheduler", "localhost", "", "Daemon", 90_211.0, "Waiting on empty queue", ""),
            p(8901, "app", "10.0.2.40:52044", "shop", "Query", 0.0, "executing", "SHOW FULL PROCESSLIST"),
        ];
        rows.extend((0..10).map(|i| p(8700 + i, "app", &format!("10.0.2.{}:4{}", 20 + i, 1000 + i), "shop", "Sleep", 5.0 + i as f64 * 7.0, "", "")));
        rows.sort_by_key(|r| r.id);
        let var = |n: &str, v: &str, d: Option<&str>, note: &str| Var { name: n.into(), value: v.into(), default: d.map(str::to_string), note: note.into() };
        let vars: Vec<Var> = [("connect_timeout", "10", "10"), ("innodb_lock_wait_timeout", "15", "50"), ("interactive_timeout", "28800", "28800"), ("lock_wait_timeout", "31536000", "31536000"), ("max_execution_time", "30000", "0"), ("net_read_timeout", "30", "30"), ("wait_timeout", "600", "28800")].iter().map(|(n, v, d)| var(n, v, Some(d), "Dynamic")).collect();
        let mut s = Self::new(conn, vec!["max_execution_time".into()]);
        s.tab = match tab {
            "variables" => SqlServerTab::Variables,
            "overview" => SqlServerTab::Overview,
            "status" => SqlServerTab::Status,
            "locks" => SqlServerTab::Locks,
            _ => SqlServerTab::Processes,
        };
        // Two samples 2.5 s apart, so the demo goes through the same rate math as a live server.
        let sample = |n: i64| -> Snap {
            let d = |full: i64, step: i64| (full - step * n).to_string();
            Snap::new(
                [
                    ("Aborted_connects", d(12, 0)), ("Bytes_received", d(8_697_308_774, 3_145_728)), ("Bytes_sent", d(33_714_349_670, 8_912_896)), ("Innodb_buffer_pool_pages_data", "393216".into()), ("Innodb_buffer_pool_read_requests", d(388_500_000, 0)),
                    ("Innodb_buffer_pool_reads", d(3108, 1)), ("Innodb_page_size", "16384".into()), ("Innodb_row_lock_waits", d(27, 0)), ("Questions", d(1_204_881, 1030)), ("Slow_queries", d(3, 0)), ("Threads_connected", "14".into()), ("Threads_running", "3".into()), ("Uptime", d(3_542_400, 2)),
                ]
                .map(|(k, v)| (k.to_string(), v))
                .to_vec(),
            )
        };
        let ovars: HashMap<String, String> = [("max_connections", "151"), ("version", "8.4.3"), ("version_comment", "MySQL Community Server (GPL)"), ("hostname", "db-prod-1.internal"), ("port", "3306"), ("datadir", "/var/lib/mysql/"), ("character_set_server", "utf8mb4"), ("collation_server", "utf8mb4_0900_ai_ci"), ("time_zone", "SYSTEM"), ("system_time_zone", "UTC"), ("innodb_buffer_pool_size", "8589934592")].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let replica = Replica { channel: String::new(), source: "db-prod-0.internal:3306".into(), io: "Yes".into(), sql: "Yes".into(), behind: Some(0), gtid: "3e11fa47-71ca:1-88412".into(), error: String::new() };
        s.ov = Some(Ok((Instant::now(), Overview { snap: sample(0), vars: ovars, tls: "TLSv1.3, TLS_AES_256_GCM_SHA384".into(), replica: Ok(vec![replica]) })));
        s.ov_prev = Some((2.5, sample(1)));
        let gb = |g: f64| Some((g * 1024.0 * 1024.0 * 1024.0) as u64);
        (s.sizes, s.sizes_at) = (Some(vec![("shop".into(), gb(42.6)), ("analytics".into(), gb(11.3)), ("mysql".into(), gb(0.01))]), Some(Instant::now()));
        s.st = Some(Ok((Instant::now(), sample(0))));
        s.set_lower();
        s.st_prev = Some((2.5, sample(1)));
        s.uptime = Some(41 * 86_400 + 3 * 3600);
        s.sel = Some(8812);
        s.auto = true;
        let demo = demo_locks(&rows);
        s.chains = std::sync::Arc::new(db::sql::server::locks::chains(&demo.waits));
        s.locks = Some(Ok(std::sync::Arc::new(demo)));
        s.procs = Some(Ok(Processes { rows, me: 8901, uptime: s.uptime, identity: String::new() }));
        s.var_filter = "timeout".into();
        s.vars = Some(Ok(Variables { rows: vars, global: false, defaults: true, uptime: s.uptime }));
        s
    }
}

pub const POLL: Duration = Duration::from_secs(2);

pub fn retry_after(ok: bool) -> Duration {
    if ok { POLL } else { BACKOFF }
}

/// Whether the confirmation still points at the same statement of the same connection on the same server.
pub fn confirm_ok(c: &Confirm, p: &Processes) -> bool {
    c.identity == p.identity && p.rows.iter().any(|r| c.p.same_thread(r))
}

/// The sample before the latest and the seconds between them, from the two instants themselves; a failed read
/// in between leaves no sample, so a rate never spans an unknown gap.
pub fn prev_of<T>(old: Option<db::Res<(Instant, T)>>, now: Instant) -> Option<(f64, T)> {
    match old {
        Some(Ok((at, t))) => Some((now.duration_since(at).as_secs_f64(), t)),
        _ => None,
    }
}

/// Canned locks for the offline demo: an idle transaction on `orders` holds the row two writers wait for, and a third
/// queues behind one of them, so the tab shows a chain two deep.
pub fn demo_locks(procs: &[Proc]) -> Locks {
    use db::sql::server::locks::{LockWait, Trx};
    let p = Proc::new;
    let mut all: Vec<Proc> = procs.to_vec();
    all.push(p(8830, "app", "10.0.2.33:40990", "shop", "Query", 41.0, "updating", "UPDATE orders SET status = 'shipped' WHERE id = 90211"));
    all.push(p(8841, "jane", "10.0.8.5:52210", "shop", "Query", 33.0, "Waiting for table metadata lock", "ALTER TABLE orders ADD COLUMN note TEXT"));
    all.push(p(8850, "report", "10.0.4.12:51710", "shop", "Query", 29.0, "Waiting for table metadata lock", "SELECT COUNT(*) FROM orders"));
    all.retain(|x| x.id != 8790);
    all.push(p(8790, "app", "10.0.2.31:40118", "shop", "Sleep", 118.0, "", ""));
    let wait = |waiter: u64, blocker: u64, kind: &str, on: &str, mode: &str| LockWait { waiter, blocker, kind: kind.into(), on: on.into(), mode: mode.into() };
    let trx = |id: u64, open: f64, waiting: Option<f64>, q: &str| (id, Trx { id, open_secs: Some(open), wait_secs: waiting, query: q.into() });
    Locks {
        waits: vec![wait(8830, 8790, "Row", "shop.orders (PRIMARY)", "X,REC_NOT_GAP"), wait(8841, 8790, "Metadata", "shop.orders", "EXCLUSIVE"), wait(8850, 8841, "Metadata", "shop.orders", "SHARED_READ")],
        procs: all.into_iter().map(|x| (x.id, x)).collect(),
        trx: [trx(8790, 124.0, None, ""), trx(8830, 41.0, Some(41.0), "UPDATE orders SET status = 'shipped' WHERE id = 90211")].into_iter().collect(),
        me: 8901,
        identity: String::new(),
        notes: Vec::new(),
        source: "performance_schema.data_lock_waits",
    }
}

/// After a failed read a polling tab tries again this often, so one dropped packet does not end the polling and a
/// missing privilege is not asked about every 2 seconds.
pub const BACKOFF: Duration = Duration::from_secs(10);
