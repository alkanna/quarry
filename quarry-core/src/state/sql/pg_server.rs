//! State behind src/app/sql/pg_server.rs, which draws it.

use crate::state::sql::server::{prev_of, retry_after};
use crate::db::sql::pg::server::{carry_blockers, Activity, Answer, Counters, Overview, Role, Session, Setting, Settings, Sizes};
use std::collections::HashMap;
use std::time::Instant;
use crate::db;

#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub enum PgTab {
    #[default]
    Overview,
    Settings,
    Activity,
    Locks,
    Counters,
}

/// A Cancel or Terminate waiting for its second click, with the row as the list showed it.
#[derive(Clone)]
pub struct PgConfirm {
    pub s: Session,
    pub terminate: bool,
    /// `arm_time` when it was armed: the confirm ignores clicks for a moment after.
    pub at: f64,
}

#[derive(Default)]
pub struct PgServerWin {
    pub conn: u64,
    pub tab: PgTab,
    /// The settings Quarry gives every session of this connection, lower case.
    pub quarry: Vec<String>,
    /// When to ask for the Overview, and for Activity and Locks, again; None asks on the next frame. One per read, so an
    /// answer of one tab never delays the other's polling.
    pub ov_next: Option<Instant>,
    pub act_next: Option<Instant>,
    pub refresh: bool,
    pub ov_req: u64,
    pub ov_wait: bool,
    pub ov: Option<db::Res<(Instant, Overview)>>,
    pub ov_prev: Option<(f64, Overview)>,
    /// The last sizes that came in; a failed read keeps them and says why next to them.
    pub sizes: Option<Sizes>,
    pub sizes_err: Option<String>,
    pub sizes_at: Option<Instant>,
    pub sz_req: u64,
    pub sz_wait: bool,
    pub set_req: u64,
    pub set_wait: bool,
    pub settings: Option<db::Res<Settings>>,
    pub cnt_req: u64,
    pub cnt_wait: bool,
    pub counters: Option<db::Res<Vec<Counters>>>,
    pub set_filter: String,
    pub changed_only: bool,
    pub set_sel: Option<String>,
    pub act_req: u64,
    pub act_wait: bool,
    pub act: Option<db::Res<Activity>>,
    /// When blockers were last asked for (BLOCKERS_EVERY).
    pub blk_at: Option<Instant>,
    /// Copy query of a cut query: the request for its whole text, and the text to put on the clipboard on the next frame.
    pub copy_req: u64,
    pub copy: Option<String>,
    pub auto: bool,
    pub hide_idle: bool,
    pub filter: String,
    pub sel: Option<u64>,
    pub confirm: Option<PgConfirm>,
    /// Whether the connection is unlocked, read each frame.
    pub writable: bool,
    /// Quarry's sessions that Stop ends (a Query run, an export, an import, the shell's relays) by pid, read each frame.
    pub own: HashMap<u64, String>,
    pub unlock: bool,
}

impl PgServerWin {
    pub fn new(conn: u64, quarry: Vec<String>) -> Self {
        Self { conn, quarry, auto: true, hide_idle: true, changed_only: true, ..Default::default() }
    }

    /// Canned answers for the offline demo (QUARRY_TEST_PG=server...), with the numbers of the design.
    pub fn demo(conn: u64, which: &str) -> Self {
        use db::sql::pg::server::Standby;
        let mut s = Self::new(conn, vec!["application_name".into(), "statement_timeout".into()]);
        s.tab = match which {
            "server-settings" => PgTab::Settings,
            "server-locks" => PgTab::Locks,
            "server-counters" => PgTab::Counters,
            "server" => PgTab::Overview,
            _ => PgTab::Activity,
        };
        let gb = |x: f64| Some((x * 1024.0 * 1024.0 * 1024.0) as u64);
        let sample = |n: f64| Overview {
            version: "PostgreSQL 18.1 (Debian 18.1-1.pgdg13+1)".into(),
            short_version: "18.1".into(),
            uptime: Some(41 * 86_400 + 3 * 3600),
            started: "2026-10-02 06:14 UTC".into(),
            connections: Some(38.0),
            max_connections: Some(200.0),
            active: Some(6.0),
            waiting: Some(2.0),
            commits: Some(88_412_000.0 - n * 2470.0),
            rollbacks: Some(354_000.0 - n * 10.0),
            hits: Some(994.0e6),
            reads: Some(6.0e6),
            shared_buffers: Some(4.0 * 1024.0 * 1024.0 * 1024.0),
            role: Some(Role::Primary { standbys: vec![Standby { name: "replica-a".into(), addr: "10.0.4.21".into(), state: "streaming".into(), sync: "async".into(), lag: Some(0.4) }, Standby { name: "replica-b".into(), addr: "10.0.4.22".into(), state: "streaming".into(), sync: "sync".into(), lag: Some(0.0) }], visible: true }),
            host: "10.0.4.20".into(),
            port: "5432".into(),
            data_dir: "/var/lib/postgresql/18/main".into(),
            encoding: "UTF8".into(),
            collate: "en_US.UTF-8".into(),
            time_zone: "UTC".into(),
            tls: "TLSv1.3, TLS_AES_256_GCM_SHA384".into(),
            read_all: true,
            user: "analyst".into(),
        };
        s.ov = Some(Ok((Instant::now(), sample(0.0))));
        s.ov_prev = Some((2.0, sample(1.0)));
        s.sizes = Some(vec![("analytics".into(), gb(61.8)), ("shop".into(), gb(18.9)), ("staging".into(), gb(3.4)), ("postgres".into(), Some(7_969_177))]);
        s.sizes_at = Some(Instant::now());
        let set = |name: &str, value: &str, source: &str, pending: bool| Setting { name: name.into(), lower: name.to_lowercase(), value: value.into(), source: source.into(), pending, context: "user".into(), ..Default::default() };
        let mut rows = vec![
            set("application_name", "Quarry", "client", false),
            set("idle_in_transaction_session_timeout", "10min", "configuration file", false),
            set("log_min_duration_statement", "500ms", "configuration file", false),
            Setting { context: "postmaster".into(), desc: "Sets the maximum number of concurrent connections.".into(), min: "1".into(), max: "262143".into(), boot: "100".into(), file: "/etc/postgresql/18/main/postgresql.conf:64".into(), after_restart: Some("250".into()), ..set("max_connections", "200", "configuration file", true) },
            set("search_path", "reporting, public", "user", false),
            Setting { unit: "8kB".into(), boot: "16384".into(), ..set("shared_buffers", "4GB", "configuration file", false) },
            set("statement_timeout", "30s", "client", false),
            set("work_mem", "64MB", "database", false),
        ];
        for n in ["autovacuum", "checkpoint_timeout", "default_statistics_target", "effective_cache_size", "jit", "lock_timeout", "log_line_prefix", "maintenance_work_mem", "max_wal_size", "random_page_cost", "temp_buffers", "wal_level"] {
            rows.push(set(n, "on", "default", false));
        }
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        s.settings = Some(Ok(Settings { rows, user: "reporting".into(), database: "analytics".into() }));
        s.set_sel = Some("max_connections".into());
        let cnt = |db: &str, n: [u64; 10]| Counters { db: db.into(), sessions: Some(n[0]), commits: Some(n[1]), rollbacks: Some(n[2]), reads: Some(n[3]), hits: Some(n[4]), inserted: Some(n[5]), updated: Some(n[6]), deleted: Some(n[7]), deadlocks: Some(n[8]), temp_bytes: Some(n[9]), reset: if db == "shop" { "2026-11-01".into() } else { String::new() } };
        s.counters = Some(Ok(vec![
            cnt("shop", [34, 18_204_311, 4_120, 2_310_544, 912_004_118, 3_401_220, 1_288_419, 40_118, 2, 734_003_200]),
            cnt("analytics", [6, 2_481_007, 311, 48_220_931, 401_118_220, 12_004, 2_104, 88, 0, 18_253_611_008]),
            cnt("postgres", [1, 40_112, 3, 1_204, 2_210_871, 0, 0, 0, 0, 0]),
        ]));
        let limited = which == "server-limited";
        let sess = |pid: u64, user: &str, client: &str, db: &str, app: &str, state: &str, query: &str, xact: Option<f64>| {
            let mut x = Session::new(pid, user, client, db, app, state, query, "client backend");
            (x.backend_start, x.query_start, x.signal, x.xact_secs, x.query_secs, x.state_secs) = ("2026-11-12 09:00:00+00".into(), "2026-11-12 09:30:00+00".into(), true, xact, xact, xact);
            x
        };
        let mut act = vec![
            Session { wait_type: "Client".into(), wait: "ClientRead".into(), ..sess(41877, "app_writer", "10.0.2.31", "shop", "orders-api", "idle in transaction", "UPDATE orders SET status = 'paid' WHERE id = 90211", Some(724.0)) },
            Session { wait_type: "Lock".into(), wait: "transactionid".into(), blockers: vec![41877], lock_mode: "ShareLock".into(), lock_on: "transaction 88412".into(), ..sess(41915, "app_writer", "10.0.2.32", "shop", "orders-api", "active", "UPDATE orders SET status = 'shipped' WHERE id = 90211", Some(48.0)) },
            Session { wait_type: "IO".into(), wait: "DataFileRead".into(), ..sess(41888, "reporting", "10.0.4.12", "analytics", "metabase", "active", "SELECT customer_id, sum(total) FROM reporting.daily_sales GROUP BY 1", Some(142.0)) },
            sess(41930, "jane", "10.0.8.5", "analytics", "psql", "active", "SELECT * FROM reporting.daily_sales WHERE day > now() - interval '7 days'", Some(0.0)),
            Session { ours: true, ..sess(41950, "app_reader", "10.0.2.40", "analytics", "Quarry", "active", "SELECT pid, usename, datname, application_name FROM pg_stat_activity", Some(0.0)) },
            Session { kind: "autovacuum worker".into(), client: String::new(), ..sess(312, "autovacuum", "", "analytics", "", "active", "autovacuum: VACUUM public.events", Some(4.0)) },
        ];
        act.extend((0..30).map(|i| sess(42000 + i, "app_writer", &format!("10.0.2.{}", 50 + i), "shop", "orders-api", "idle", "COMMIT", None)));
        if which == "server-locks" {
            act.push(Session { wait_type: "Lock".into(), wait: "relation".into(), blockers: vec![41877, 41915], lock_mode: "AccessExclusiveLock".into(), lock_on: "public.orders".into(), ..sess(41940, "jane", "10.0.8.5", "shop", "psql", "active", "ALTER TABLE orders ADD COLUMN note text", Some(31.0)) });
            act.push(Session { wait_type: "Lock".into(), wait: "relation".into(), blockers: vec![41940], lock_mode: "AccessShareLock".into(), lock_on: "public.orders".into(), ..sess(41944, "reporting", "10.0.4.12", "shop", "metabase", "active", "SELECT count(*) FROM orders", Some(29.0)) });
            act.push(Session { wait_type: "Lock".into(), wait: "relation".into(), blockers: vec![41888], lock_mode: "AccessExclusiveLock".into(), lock_on: "reporting.daily_sales".into(), ..sess(41961, "jane", "10.0.8.5", "analytics", "psql", "active", "TRUNCATE reporting.daily_sales", Some(65.0)) });
        }
        if limited {
            // Without pg_read_all_stats other roles keep user, database and application only.
            for x in act.iter_mut().filter(|x| !x.ours) {
                *x = Session { signal: false, ..Session::new(x.pid, &x.user, "", &x.db, &x.app, "", "<insufficient privilege>", "") };
            }
        }
        act.sort_by_key(|x| match x.pid {
            41877 => 0,
            41915 => 1,
            41888 => 2,
            p => p,
        });
        s.act = Some(Ok(Activity { rows: act, user: if limited { "app_reader".into() } else { "analyst".into() }, read_all: !limited, server: String::new(), own_app: None, blockers: true }));
        s.sel = Some(match which {
            "server-confirm" => 41877,
            "server-limited" => 41950,
            _ => 41888,
        });
        if which == "server-confirm" {
            s.confirm = s.rows().iter().find(|r| r.pid == 41877).map(|r| PgConfirm { s: r.clone(), terminate: true, at: f64::NEG_INFINITY });
        }
        s
    }

    /// The pid of a running session whose query carries `tag`, for the UI test.
    #[cfg(any(test, feature = "test-support"))]
    pub fn listed(&self, tag: &str) -> Option<u64> {
        self.rows().iter().find(|r| r.query.contains(tag) && r.active()).map(|r| r.pid)
    }

    /// Asks for the list on the next frame, past the poll interval and the backoff after a failed read, for a UI test that
    /// waits on a session the server may not have started yet.
    #[cfg(any(test, feature = "test-support"))]
    pub fn refresh_now(&mut self) {
        (self.refresh, self.act_next) = (true, None);
    }

    /// What the Activity tab holds, for a UI test that waits on it: the error of the last read, or how many sessions it listed.
    #[cfg(any(test, feature = "test-support"))]
    pub fn activity_state(&self) -> String {
        match &self.act {
            None => format!("no answer yet (tab {:?}, waiting {})", self.tab, self.act_wait),
            Some(Err(e)) => format!("the last read failed: {e:?}"),
            Some(Ok(a)) => format!("{} sessions listed, {} running", a.rows.len(), a.rows.iter().filter(|r| r.active()).count()),
        }
    }

    /// Whether a Cancel or Terminate waits for its second click, for the UI test.
    #[cfg(any(test, feature = "test-support"))]
    pub fn armed(&self) -> bool {
        self.confirm.is_some()
    }

    pub fn server(&self) -> &str {
        match &self.act {
            Some(Ok(a)) => &a.server,
            _ => "",
        }
    }

    /// Applies an answer to a read of the window, and returns why a Copy query failed. Signals are told by the App, since they
    /// outlive the window.
    pub fn take(&mut self, req: u64, ans: Answer, now: Instant) -> Option<String> {
        match ans {
            Answer::Overview(r) if req == self.ov_req => {
                self.ov_wait = false;
                self.ov_next = Some(now + retry_after(r.is_ok()));
                self.ov_prev = prev_of(self.ov.take(), now);
                self.ov = Some(r.map(|o| (now, o)));
            }
            Answer::Sizes(r) if req == self.sz_req => {
                self.sz_wait = false;
                match r {
                    Ok(v) => (self.sizes, self.sizes_err) = (Some(v), None),
                    Err(e) => self.sizes_err = Some(e),
                }
            }
            Answer::Counters(r) if req == self.cnt_req => {
                self.cnt_wait = false;
                self.counters = Some(r);
            }
            Answer::Settings(r) if req == self.set_req => {
                self.set_wait = false;
                self.settings = Some(r);
            }
            Answer::Activity(mut r) if req == self.act_req => {
                self.act_wait = false;
                self.act_next = Some(now + retry_after(r.is_ok()));
                match &mut r {
                    Ok(a) => {
                        if !a.blockers && let Some(Ok(old)) = &self.act {
                            carry_blockers(&mut a.rows, &old.rows);
                        }
                        self.sel = self.sel.filter(|p| a.rows.iter().any(|x| x.pid == *p));
                        self.confirm = self.confirm.take().filter(|c| confirm_ok(c, &a.rows));
                    }
                    Err(_) => {
                        // Nothing is left to carry blockers from, so the next read asks for them.
                        (self.confirm, self.blk_at) = (None, None);
                    }
                }
                self.act = Some(r);
            }
            Answer::Query(r) if req == self.copy_req => {
                self.copy_req = 0;
                match r {
                    Ok(q) => self.copy = Some(q),
                    Err(e) => return Some(e),
                }
            }
            _ => {}
        }
        None
    }

    pub fn overview(&self) -> Option<&Overview> {
        match &self.ov {
            Some(Ok((_, o))) => Some(o),
            _ => None,
        }
    }

    pub fn rows(&self) -> &[Session] {
        match &self.act {
            Some(Ok(a)) => &a.rows,
            _ => &[],
        }
    }
}

/// Whether a pending confirmation still points at the row it was armed on.
pub fn confirm_ok(c: &PgConfirm, rows: &[Session]) -> bool {
    rows.iter().any(|r| c.s.same(r))
}
