//! The Server window of a PostgreSQL connection (design A on the "Server window" page of docs/sql/postgres.figcraft): Overview,
//! Settings, Activity and Locks. It follows the MySQL window (`sql_server.rs`): it asks only while it is open and Quarry is
//! focused, keeps errors in the window, and offers Cancel and Terminate only on an unlocked connection, after a second click.

use crate::app::sql::server::{arm_time, confirmed, error_row, header, kv_rows, paint_cell, prev_of, retry_after, tile, up_text, PAD, POLL};
use crate::app::*;
use db::sql::pg::server::{carry_blockers, chains, metrics, Activity, Answer, Overview, Role, Session, Setting, Settings, Sizes, Target};
use std::collections::HashMap;
use std::time::Instant;

/// Database sizes walk the data directory, so the Overview asks for them this often and not on every poll.
const SIZES_EVERY: Duration = Duration::from_secs(30);
/// Each pg_blocking_pids call takes every lock manager partition lock, so blockers are asked this often; the polls between
/// keep the last ones for sessions that still wait.
const BLOCKERS_EVERY: Duration = Duration::from_secs(5);
const ROW2_H: f32 = 40.0;
const HEAD_H: f32 = 50.0;

#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub(in crate::app) enum PgTab {
    #[default]
    Overview,
    Settings,
    Activity,
    Locks,
}

/// A Cancel or Terminate waiting for its second click, with the row as the list showed it.
#[derive(Clone)]
pub(in crate::app) struct PgConfirm {
    s: Session,
    terminate: bool,
    /// `arm_time` when it was armed: the confirm ignores clicks for a moment after.
    at: f64,
}

#[derive(Default)]
pub(in crate::app) struct PgServerWin {
    pub(in crate::app) conn: u64,
    tab: PgTab,
    /// The settings Quarry gives every session of this connection, lower case.
    quarry: Vec<String>,
    /// When to ask for the Overview, and for Activity and Locks, again; None asks on the next frame. One per read, so an
    /// answer of one tab never delays the other's polling.
    ov_next: Option<Instant>,
    act_next: Option<Instant>,
    refresh: bool,
    ov_req: u64,
    ov_wait: bool,
    ov: Option<db::Res<(Instant, Overview)>>,
    ov_prev: Option<(f64, Overview)>,
    /// The last sizes that came in; a failed read keeps them and says why next to them.
    sizes: Option<Sizes>,
    sizes_err: Option<String>,
    sizes_at: Option<Instant>,
    sz_req: u64,
    sz_wait: bool,
    set_req: u64,
    set_wait: bool,
    settings: Option<db::Res<Settings>>,
    set_filter: String,
    changed_only: bool,
    set_sel: Option<String>,
    act_req: u64,
    act_wait: bool,
    act: Option<db::Res<Activity>>,
    /// When blockers were last asked for (BLOCKERS_EVERY).
    blk_at: Option<Instant>,
    /// Copy query of a cut query: the request for its whole text, and the text to put on the clipboard on the next frame.
    copy_req: u64,
    copy: Option<String>,
    auto: bool,
    hide_idle: bool,
    filter: String,
    sel: Option<u64>,
    confirm: Option<PgConfirm>,
    /// Whether the connection is unlocked, read each frame.
    writable: bool,
    /// Quarry's sessions that Stop ends (a Query run, an export, an import, the shell's relays) by pid, read each frame.
    own: HashMap<u64, String>,
    unlock: bool,
}

impl PgServerWin {
    pub(in crate::app) fn new(conn: u64, quarry: Vec<String>) -> Self {
        Self { conn, quarry, auto: true, hide_idle: true, changed_only: true, ..Default::default() }
    }

    /// Canned answers for the offline demo (QUARRY_TEST_PG=server...), with the numbers of the design.
    pub(in crate::app) fn demo(conn: u64, which: &str) -> Self {
        use db::sql::pg::server::Standby;
        let mut s = Self::new(conn, vec!["application_name".into(), "statement_timeout".into()]);
        s.tab = match which {
            "server-settings" => PgTab::Settings,
            "server-locks" => PgTab::Locks,
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
    #[cfg(test)]
    pub(in crate::app) fn listed(&self, tag: &str) -> Option<u64> {
        self.rows().iter().find(|r| r.query.contains(tag) && r.active()).map(|r| r.pid)
    }

    /// Whether a Cancel or Terminate waits for its second click, for the UI test.
    #[cfg(test)]
    pub(in crate::app) fn armed(&self) -> bool {
        self.confirm.is_some()
    }

    fn server(&self) -> &str {
        match &self.act {
            Some(Ok(a)) => &a.server,
            _ => "",
        }
    }

    /// Applies an answer to a read of the window, and returns why a Copy query failed. Signals are told by the App, since they
    /// outlive the window.
    fn take(&mut self, req: u64, ans: Answer, now: Instant) -> Option<String> {
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

    fn overview(&self) -> Option<&Overview> {
        match &self.ov {
            Some(Ok((_, o))) => Some(o),
            _ => None,
        }
    }

    fn rows(&self) -> &[Session] {
        match &self.act {
            Some(Ok(a)) => &a.rows,
            _ => &[],
        }
    }
}

/// Whether the row may get a Cancel (`terminate` false) or a Terminate: a session the role may signal and could see, of a kind
/// that takes the signal, never one of Quarry's own; a cancel only while a statement runs.
fn can_signal(own: &HashMap<u64, String>, s: &Session, terminate: bool) -> bool {
    s.signal && !s.ours && !own.contains_key(&s.pid) && !s.hidden && !s.backend_start.is_empty() && s.signalable() && (terminate || s.active())
}

/// Whether a pending confirmation still points at the row it was armed on.
fn confirm_ok(c: &PgConfirm, rows: &[Session]) -> bool {
    rows.iter().any(|r| c.s.same(r))
}

/// The Activity rows the filter and "Hide idle" leave.
fn shown<'a>(rows: &'a [Session], filter: &str, hide_idle: bool) -> Vec<&'a Session> {
    let f = filter.trim().to_lowercase();
    rows.iter().filter(|s| !(hide_idle && s.idle())).filter(|s| f.is_empty() || s.search.contains(&f)).collect()
}

/// Settings by name, all or only the changed ones.
fn shown_settings<'a>(rows: &'a [Setting], filter: &str, changed_only: bool) -> Vec<&'a Setting> {
    let f = filter.trim().to_lowercase();
    rows.iter().filter(|s| !changed_only || s.changed()).filter(|s| f.is_empty() || s.lower.contains(&f)).collect()
}

/// "12:04" for an age in seconds, "1:02:04" past an hour.
fn mss(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// "12 min", "2 min 22 s", "40 s", "3 h 5 min".
fn span(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    match s {
        0..60 => format!("{s} s"),
        60..600 if !s.is_multiple_of(60) => format!("{} min {} s", s / 60, s % 60),
        60..3600 => format!("{} min", s / 60),
        _ => format!("{} h {} min", s / 3600, s / 60 % 60),
    }
}

/// The source of a setting as the window words it, naming the database or role it came from.
fn source_text(s: &Setting, user: &str, database: &str) -> String {
    match s.source.as_str() {
        "client" | "session" => "session".into(),
        "database" => format!("database {database}"),
        "user" => format!("user {user}"),
        "database user" => format!("database {database}, user {user}"),
        other => other.into(),
    }
}

/// Whether Quarry set this setting for the session that read the list.
fn set_by_quarry(s: &Setting, quarry: &[String]) -> bool {
    matches!(s.source.as_str(), "client" | "session") && quarry.contains(&s.lower)
}

/// A pg_settings number with its unit as SHOW would print it: "16384" in units of "8kB" is "128MB".
fn with_unit(v: &str, unit: &str) -> String {
    let Ok(n) = v.trim().parse::<i64>() else { return v.to_string() };
    let (k, base) = match unit {
        "B" => (1, "B"),
        "kB" => (1, "kB"),
        "8kB" => (8, "kB"),
        "MB" => (1, "MB"),
        "ms" | "s" | "min" => return format!("{n}{unit}"),
        _ => return format!("{v}{}{unit}", if unit.is_empty() { "" } else { " " }),
    };
    if n <= 0 {
        return format!("{n}{base}");
    }
    let mut x = n * k;
    let units = ["B", "kB", "MB", "GB", "TB"];
    let mut i = units.iter().position(|u| *u == base).unwrap_or(0);
    while x % 1024 == 0 && i + 1 < units.len() {
        x /= 1024;
        i += 1;
    }
    format!("{x}{}", units[i])
}

/// A painted one-line text from `x` to `x + w` at the vertical center `cy`, cut with an ellipsis.
fn text_at(ui: &egui::Ui, x: f32, w: f32, cy: f32, h: f32, s: &str, color: Color32, mono: bool, size: f32, right: bool) {
    let font = if mono { FontId::monospace(size) } else { FontId::proportional(size) };
    place(ui, egui::Rect::from_min_max(egui::pos2(x, cy - h / 2.0), egui::pos2(x + w, cy + h / 2.0)), s, font, color, right, PAD);
}

/// A note above the list: a tinted box with the text and an optional button, true when the button was clicked.
fn banner(ui: &mut egui::Ui, color: Color32, add: impl FnOnce(&mut egui::Ui), button: Option<&str>) -> bool {
    let mut clicked = false;
    egui::Frame::new().fill(tint(color, 20)).stroke(Stroke::new(1.0, tint(color, 84))).corner_radius(R_BAR).inner_margin(egui::Margin { left: 12, right: 8, top: 7, bottom: 7 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.set_min_height(26.0);
            add(ui);
            if let Some(b) = button {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| clicked = ui.button(b).clicked());
            }
        });
    });
    ui.add_space(14.0);
    clicked
}

/// The read-only note of the Activity and Locks tabs, true when Unlock was clicked.
fn locked_banner(ui: &mut egui::Ui) -> bool {
    crate::ui::dialog::banner(ui, Notice::Locked, "Read-only.", "Unlock writes to cancel queries or terminate sessions.", Some("Unlock…"))
}

/// What a click on Cancel or Terminate does on a locked connection: a popover that offers Unlock instead of acting. True when
/// Unlock was clicked.
fn ask_unlock(ui: &egui::Ui, r: &egui::Response, conn_name: &str, what: &str) -> bool {
    let mut unlock = false;
    let mut close = false;
    popover(r, 340.0, false, |ui| {
        menu_look(ui);
        popover_body(ui, 320.0, |ui| {
            ui.label(semibold(RichText::new(format!("{conn_name} is read-only")).size(TXT_BODY).color(BRIGHT())));
            ui.add_space(4.0);
            ui.label(RichText::new(format!("Unlock writes to {what}.")).size(TXT_SMALL).color(MUTED()));
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if primary(ui, "Unlock…", PRIMARY()).clicked() {
                    (unlock, close) = (true, true);
                }
                if ui.button("Not now").clicked() {
                    close = true;
                }
            });
        });
    });
    if close {
        ui.ctx().data_mut(|d| d.insert_temp(r.id.with("clause-popover"), false));
    }
    unlock
}

/// What the person asked for this frame.
#[derive(Default)]
struct Acts {
    /// A second click went through: send it.
    send: Option<PgConfirm>,
    unlock: bool,
    /// Copy query on a row whose text the list cut: ask for the whole of it.
    fetch: Option<Session>,
}

impl App {
    /// Asks for the tab on screen. Overview, Activity and Locks repeat every POLL while Quarry is focused; Settings is read once.
    fn pg_server_poll(&mut self, ctx: &egui::Context, s: &mut PgServerWin, db: &db::sql::Db) {
        if !ctx.input(|i| i.focused) {
            return;
        }
        let now = Instant::now();
        match s.tab {
            PgTab::Overview => {
                ctx.request_repaint_after(POLL);
                let sizes = !s.sz_wait && (s.refresh || s.sizes_at.is_none_or(|t| now.duration_since(t) >= SIZES_EVERY));
                if sizes {
                    self.req += 1;
                    (s.sz_req, s.sz_wait, s.sizes_at) = (self.req, true, Some(now));
                    self.worker.pg_server_sizes(db.clone(), self.req);
                }
                let due = s.refresh || s.ov.is_none() || s.ov_next.is_none_or(|t| now >= t);
                if s.ov_wait || !due {
                    return;
                }
                self.req += 1;
                (s.ov_req, s.ov_wait, s.refresh) = (self.req, true, false);
                self.worker.pg_server_overview(db.clone(), self.req);
            }
            PgTab::Settings => {
                if s.set_wait || !(s.refresh || s.settings.is_none()) {
                    return;
                }
                self.req += 1;
                (s.set_req, s.set_wait, s.refresh) = (self.req, true, false);
                self.worker.pg_server_settings(db.clone(), self.req);
            }
            PgTab::Activity | PgTab::Locks => {
                if s.auto {
                    ctx.request_repaint_after(POLL);
                }
                let due = s.refresh || s.act.is_none() || (s.auto && s.act_next.is_none_or(|t| now >= t));
                if s.act_wait || !due {
                    return;
                }
                let blockers = s.refresh || s.blk_at.is_none_or(|t| now.duration_since(t) >= BLOCKERS_EVERY);
                if blockers {
                    s.blk_at = Some(now);
                }
                self.req += 1;
                (s.act_req, s.act_wait, s.refresh) = (self.req, true, false);
                self.worker.pg_server_activity(db.clone(), self.req, blockers);
            }
        }
    }

    pub(in crate::app) fn pg_server_done(&mut self, req: u64, ans: Answer) {
        // A Cancel or Terminate is told even when the window is gone, and only once.
        if let Answer::Signal(res) = &ans {
            if !self.sql_kills.remove(&req) {
                return;
            }
            if let (Ok(_), Some(s)) = (res, self.pg_server.as_mut()) {
                (s.refresh, s.act_next) = (true, None);
            }
            match res.clone() {
                Ok(m) => self.ok(m),
                Err(e) => self.err(e),
            }
            return;
        }
        let Some(s) = self.pg_server.as_mut() else { return };
        if let Some(e) = s.take(req, ans, Instant::now()) {
            self.err(e);
        }
    }

    pub(in crate::app) fn pg_server_window(&mut self, ctx: &egui::Context) {
        let Some(mut s) = self.pg_server.take() else { return };
        let conn = s.conn;
        let db = self.conn(conn).and_then(|c| c.sql.clone());
        let name = self.conn(conn).map(|c| c.meta.name.clone()).unwrap_or_default();
        let product = self.conn(conn).map(|c| format!("{} {}", c.server.product, c.server.version)).unwrap_or_default();
        if let (Some(db), false) = (&db, self.demo) {
            self.pg_server_poll(ctx, &mut s, db);
        }
        let live = db.is_some() || self.demo;
        s.writable = self.conn(conn).map_or(self.demo, |c| c.writable);
        s.own = self.own_sessions(conn);
        // A confirmation armed before the lock is dropped.
        if !s.writable {
            s.confirm = None;
        }
        let mut acts = Acts::default();
        let screen = ctx.content_rect();
        let r = banded(egui::Id::unique(("quarry-layer", "pg-server-window"))).show(ctx, |ui| {
            ui.set_width((screen.width() * 0.7).clamp(760.0, 1100.0));
            const TABS: [(PgTab, &str); 4] = [(PgTab::Overview, "Overview"), (PgTab::Settings, "Settings"), (PgTab::Activity, "Activity"), (PgTab::Locks, "Locks")];
            let labels = TABS.map(|t| t.1);
            let active = TABS.iter().position(|t| t.0 == s.tab).unwrap_or(0);
            let picked = dialog_head(ui, &Head::new("Server").sub(subtitle(&s, &name, product.trim())).glyph(glyphs::server).tabs(&labels, active));
            {
                for (tab, _) in TABS.iter().copied().filter(|t| picked.is_some_and(|i| TABS[i].0 == t.0)) {
                    if s.tab != tab {
                        s.tab = tab;
                        match tab {
                            PgTab::Overview => s.ov_next = None,
                            PgTab::Activity | PgTab::Locks => s.act_next = None,
                            PgTab::Settings => {}
                        }
                        // Locks is about the blockers, so it asks for them at once.
                        if tab == PgTab::Locks {
                            s.blk_at = None;
                        }
                        s.confirm = None;
                    }
                }
            }
            let max = screen.height() - 400.0;
            dialog_pane(ui, |ui| {
                if !live {
                    ui.label(RichText::new("This connection is closed. Reconnect to see the server again.").color(MUTED()));
                } else {
                    match s.tab {
                        PgTab::Overview => overview_view(ui, &mut s),
                        PgTab::Settings => settings_view(ui, &mut s, max),
                        PgTab::Activity => activity_view(ui, &mut s, max, &name, &mut acts),
                        PgTab::Locks => locks_view(ui, &mut s, max, &name, &mut acts),
                    }
                }
            });
            if dialog_done(ui) {
                s.unlock = false;
                acts.send = None;
                return false;
            }
            true
        });
        let open = r.inner;
        if acts.unlock || std::mem::take(&mut s.unlock) {
            self.modal = Some(Modal::Unlock(conn, None));
        }
        if let Some(t) = s.copy.take() {
            ctx.copy_text(t);
            self.ok("Copied the query");
        }
        if let (Some(r), Some(db)) = (acts.fetch, &db) {
            self.req += 1;
            s.copy_req = self.req;
            self.worker.pg_server_query(db.clone(), self.req, &r);
        }
        if let (Some(c), Some(db)) = (acts.send, db) {
            self.req += 1;
            self.sql_kills.insert(self.req);
            self.worker.pg_server_signal(db, self.req, Target::of(&c.s, s.server()), c.terminate, !s.writable);
        }
        if open && !(r.should_close() && self.modal.is_none()) {
            self.pg_server = Some(s);
        }
    }
}

fn subtitle(s: &PgServerWin, name: &str, product: &str) -> String {
    let rows = s.rows();
    let clients = || rows.iter().filter(|r| r.kind == "client backend" || r.hidden).count();
    match (s.tab, &s.act) {
        (PgTab::Activity, Some(Ok(a))) if !a.read_all => format!("{name} · signed in as {} · {} sessions", a.user, clients()),
        (PgTab::Activity, Some(Ok(_))) => format!("{name} · {} sessions, {} active, {} idle in transaction", clients(), rows.iter().filter(|r| r.active() && r.kind == "client backend").count(), rows.iter().filter(|r| r.idle_in_tx()).count()),
        (PgTab::Locks, Some(Ok(_))) => {
            let ch = chains(rows);
            let waiting: usize = ch.iter().map(|c| c.waiting.len()).sum();
            if ch.is_empty() { format!("{name} · no session waits for a lock") } else { format!("{name} · {waiting} {} waiting behind {} {}", if waiting == 1 { "session" } else { "sessions" }, ch.len(), if ch.len() == 1 { "blocker" } else { "blockers" }) }
        }
        _ => {
            let (version, up) = match s.overview() {
                Some(o) => (format!("PostgreSQL {}", o.short_version), o.uptime),
                None => (product.to_string(), None),
            };
            let up = up.map(|u| format!(" · up {}", up_text(u))).unwrap_or_default();
            format!("{name} · {version}{up}")
        }
    }
}

fn overview_view(ui: &mut egui::Ui, s: &mut PgServerWin) {
    let o = match &s.ov {
        None => return spin(ui),
        Some(Err(e)) => {
            if error_row(ui, format!("Could not read the server status: {e}")) {
                s.refresh = true;
            }
            return;
        }
        Some(Ok((_, o))) => o,
    };
    let m = metrics(o, s.ov_prev.as_ref());
    let int = |v: Option<f64>| v.map(|v| group(&format!("{v:.0}"))).unwrap_or_else(|| "…".into());
    let gap = 12.0;
    let w = (ui.available_width() - gap * 4.0) / 5.0;
    let (strip, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 86.0), Sense::hover());
    let at = |i: usize| egui::Rect::from_min_size(egui::pos2(strip.left() + i as f32 * (w + gap), strip.top()), egui::vec2(w, strip.height()));
    let of = o.max_connections.map(|v| format!("of {}", group(&format!("{v:.0}")))).unwrap_or_default();
    tile(ui, at(0), "Connections", &int(o.connections), &of, &format!("{} active, {} waiting", int(o.active), int(o.waiting)));
    let commit = m.commit.map(|c| format!("{:.1}% commit", c * 100.0)).unwrap_or_default();
    tile(ui, at(1), "Transactions per second", &int(m.tps), "", &commit);
    let buffers = o.shared_buffers.map(|b| format!("shared_buffers {}", human_bytes(b as usize))).unwrap_or_default();
    tile(ui, at(2), "Cache hit ratio", &m.hit.map(|h| format!("{:.1}%", h * 100.0)).unwrap_or_else(|| "…".into()), "", &buffers);
    let (total, count) = match &s.sizes {
        Some(v) => (Some(v.iter().filter_map(|(_, b)| *b).sum::<u64>()), format!("{} {}", v.len(), if v.len() == 1 { "database" } else { "databases" })),
        None => (None, String::new()),
    };
    tile(ui, at(3), "Database size", &total.map(|t| human_bytes(t as usize)).unwrap_or_else(|| "…".into()), "", &count);
    let lag = |l: Option<f64>| l.map(|l| format!("{l:.1} s").replace(".0 s", " s")).unwrap_or_else(|| "none yet".into());
    let (big, sub) = match &o.role {
        Some(Role::Primary { standbys, visible }) if standbys.is_empty() => ("primary".to_string(), if *visible { "no replicas".to_string() } else { String::new() }),
        Some(Role::Primary { standbys, visible }) => {
            let n = standbys.len();
            let worst = standbys.iter().filter_map(|x| x.lag).fold(None, |a: Option<f64>, l| Some(a.map_or(l, |a| a.max(l))));
            ("primary".into(), if *visible { format!("{n} {}, lag {}", if n == 1 { "replica" } else { "replicas" }, lag(worst)) } else { format!("{n} {}", if n == 1 { "replica" } else { "replicas" }) })
        }
        Some(Role::Replica { lag: l, .. }) => ("replica".into(), format!("lag {}", lag(*l))),
        None => ("…".into(), String::new()),
    };
    tile(ui, at(4), "Replication", &big, "", &sub);
    ui.add_space(16.0);
    let dir = if o.data_dir.is_empty() { "Not readable by this role (pg_read_all_settings)".to_string() } else { o.data_dir.clone() };
    let server = [
        ("Version", o.version.clone(), BRIGHT()),
        ("Host", if o.host.is_empty() { "local socket".into() } else { o.host.clone() }, BRIGHT()),
        ("Port", o.port.clone(), C_NUMBER()),
        ("TLS", if o.tls.is_empty() { "Not encrypted".into() } else { o.tls.clone() }, BRIGHT()),
        ("Data directory", dir, if o.data_dir.is_empty() { MUTED() } else { BRIGHT() }),
        ("Encoding", [o.encoding.as_str(), o.collate.as_str()].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(", "), BRIGHT()),
        ("Time zone", o.time_zone.clone(), BRIGHT()),
        ("Started", o.started.clone(), BRIGHT()),
    ];
    let (read_all, user) = (o.read_all, o.user.clone());
    let role = o.role.clone();
    let sizes = s.sizes.clone();
    let sizes_err = s.sizes_err.clone();
    ui.columns(2, |cols| {
        let ui = &mut cols[0];
        caption(ui, "SERVER");
        ui.add_space(4.0);
        let w = ui.available_width();
        ui.spacing_mut().item_spacing.y = 0.0;
        kv_rows(ui, w, &server);
        let ui = &mut cols[1];
        caption(ui, "DATABASE SIZES");
        ui.add_space(4.0);
        let w = ui.available_width();
        ui.spacing_mut().item_spacing.y = 0.0;
        size_bars(ui, w, sizes.as_deref(), sizes_err.as_deref());
        ui.add_space(16.0);
        caption(ui, "REPLICATION");
        ui.add_space(4.0);
        replication(ui, w, role.as_ref());
    });
    if !read_all {
        ui.add_space(10.0);
        ui.label(RichText::new(format!("Active and waiting count only the sessions {user} may see: the role lacks pg_read_all_stats (pg_monitor grants it).")).size(TXT_SMALL).color(MUTED()));
    }
}

/// The size bars, or why they could not be read; a failure after a good read keeps the last values and says so under them.
fn size_bars(ui: &mut egui::Ui, w: f32, sizes: Option<&[(String, Option<u64>)]>, err: Option<&str>) {
    let note = |ui: &mut egui::Ui, t: String| {
        ui.label(RichText::new(t).size(TXT_SMALL).color(MUTED()));
    };
    let Some(sizes) = sizes else {
        return match err {
            Some(e) => note(ui, format!("Could not read the sizes: {e}")),
            None => spin(ui),
        };
    };
    if let Some(e) = err {
        note(ui, format!("Last read sizes; the latest read failed: {e}"));
    }
    let max = sizes.iter().filter_map(|(_, b)| *b).max().unwrap_or(1).max(1) as f32;
    let (name_w, size_w) = (w * 0.3, 90.0);
    for (i, (name, bytes)) in sizes.iter().take(8).enumerate() {
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::hover());
        ui.painter().rect_filled(rect, 0.0, if i % 2 == 1 { PANEL() } else { BG() });
        paint_cell(ui, rect, rect.left(), name_w, name, BRIGHT(), true, false);
        let bar = egui::Rect::from_min_max(egui::pos2(rect.left() + name_w, rect.center().y - 3.0), egui::pos2(rect.right() - size_w - PAD, rect.center().y + 3.0));
        ui.painter().rect_filled(bar, 3.0, LINE());
        match bytes {
            Some(b) => {
                let fill = egui::Rect::from_min_size(bar.min, egui::vec2((bar.width() * *b as f32 / max).max(4.0), bar.height()));
                ui.painter().rect_filled(fill, 3.0, C_NUMBER());
                paint_cell(ui, rect, rect.right() - size_w, size_w, &human_bytes(*b as usize), C_NUMBER(), true, true);
            }
            None => {
                paint_cell(ui, rect, rect.right() - size_w, size_w, "hidden", FAINT(), true, true);
                resp.on_hover_text("This role may not connect to it, and sizing it needs pg_read_all_stats.");
            }
        }
    }
    if sizes.len() > 8 {
        ui.label(RichText::new(format!("and {} more", sizes.len() - 8)).size(TXT_SMALL).color(MUTED()));
    }
}

fn replication(ui: &mut egui::Ui, w: f32, role: Option<&Role>) {
    match role {
        None => {}
        Some(Role::Primary { standbys, .. }) if standbys.is_empty() => {
            ui.label(RichText::new("No replica streams from this server").color(MUTED()));
        }
        Some(Role::Primary { standbys, visible }) => {
            let cw = [w * 0.42, w * 0.2, w * 0.16, w * 0.22];
            header(ui, &[("Replica", cw[0], false), ("State", cw[1], false), ("Sync", cw[2], false), ("Replay lag", cw[3], true)]);
            for (i, r) in standbys.iter().enumerate() {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::hover());
                ui.painter().rect_filled(rect, 0.0, if i % 2 == 1 { PANEL() } else { BG() });
                let who = [r.name.as_str(), r.addr.as_str()].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join("  ");
                paint_cell(ui, rect, rect.left(), cw[0], &who, BRIGHT(), true, false);
                paint_cell(ui, rect, rect.left() + cw[0], cw[1], &r.state, if r.state == "streaming" { SUCCESS() } else { WARNING() }, true, false);
                paint_cell(ui, rect, rect.left() + cw[0] + cw[1], cw[2], &r.sync, TEXT(), true, false);
                let lag = r.lag.map(|l| format!("{l:.1} s").replace(".0 s", " s")).unwrap_or_default();
                paint_cell(ui, rect, rect.left() + cw[0] + cw[1] + cw[2], cw[3], &lag, C_NUMBER(), true, true);
            }
            if !visible {
                ui.add_space(4.0);
                ui.label(RichText::new("Their state and lag are hidden: the role needs pg_read_all_stats (pg_monitor grants it).").size(TXT_SMALL).color(MUTED()));
            }
        }
        Some(Role::Replica { from, status, lag }) => {
            let lag = lag.map(|l| format!("{l:.1} s")).unwrap_or_else(|| "No transaction replayed yet".into());
            let hidden = "Hidden: needs pg_read_all_stats".to_string();
            kv_rows(ui, w, &[("Role", "replica".into(), BRIGHT()), ("Primary", if from.is_empty() { hidden.clone() } else { from.clone() }, if from.is_empty() { MUTED() } else { BRIGHT() }), ("Receiver", if status.is_empty() { hidden } else { status.clone() }, if status == "streaming" { SUCCESS() } else { MUTED() }), ("Replay lag", lag, C_NUMBER())]);
        }
    }
}

fn settings_view(ui: &mut egui::Ui, s: &mut PgServerWin, max_h: f32) {
    let total = match &s.settings {
        Some(Ok(v)) => v.rows.len(),
        _ => 0,
    };
    let count = match &s.settings {
        Some(Ok(v)) => shown_settings(&v.rows, &s.set_filter, s.changed_only).len(),
        _ => 0,
    };
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut s.set_filter).hint_text("Filter settings").desired_width(260.0));
        ui.checkbox(&mut s.changed_only, "Changed only");
        ui.add_space(8.0);
        let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), Sense::hover());
        ui.painter().circle_filled(dot.center(), 3.5, WARNING());
        ui.label(RichText::new("Needs a restart").color(MUTED()));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if total > 0 {
                ui.label(RichText::new(format!("{count} of {total} settings")).color(FAINT()));
            }
        });
    });
    ui.add_space(8.0);
    let v = match &s.settings {
        None => return spin(ui),
        Some(Err(e)) => {
            if error_row(ui, format!("Could not read the settings: {e}")) {
                s.refresh = true;
            }
            return;
        }
        Some(Ok(v)) => v,
    };
    let rows = shown_settings(&v.rows, &s.set_filter, s.changed_only);
    let w = ui.available_width();
    let (name_w, val_w, src_w) = (w * 0.34, w * 0.17, w * 0.18);
    let note_w = w - name_w - val_w - src_w;
    header(ui, &[("Name", name_w, false), ("Value", val_w, false), ("Source", src_w, false), ("Note", note_w, false)]);
    if rows.is_empty() {
        ui.add_space(8.0);
        ui.label(RichText::new("No setting matches.").color(MUTED()));
        return;
    }
    let mut sel = s.set_sel.clone();
    let detail_h = if sel.is_some() { 110.0 } else { 0.0 };
    // show_rows steps by the row height plus the parent's item spacing, which would leave a gap per row.
    let gap = std::mem::replace(&mut ui.spacing_mut().item_spacing.y, 0.0);
    egui::ScrollArea::vertical().id_salt("pg-settings").max_height((max_h - detail_h).max(120.0)).auto_shrink([false, true]).show_rows(ui, ROW_H, rows.len(), |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for i in range {
            let r = rows[i];
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::click());
            let selected = sel.as_deref() == Some(r.name.as_str());
            let fill = match (selected, state_of(&resp)) {
                (true, 0) => SEL(),
                (true, _) => SEL_HOVER(),
                (false, 0) => if i % 2 == 1 { PANEL() } else { BG() },
                (false, st) => hover_fill(st),
            };
            ui.painter().rect_filled(rect, 0.0, fill);
            let ours = set_by_quarry(r, &s.quarry);
            paint_cell(ui, rect, rect.left(), name_w, &r.name, BRIGHT(), true, false);
            paint_cell(ui, rect, rect.left() + name_w, val_w, &r.value, C_NUMBER(), true, false);
            paint_cell(ui, rect, rect.left() + name_w + val_w, src_w, &source_text(r, &v.user, &v.database), if ours { C_LINK() } else if r.changed() { TEXT() } else { MUTED() }, false, false);
            let nx = rect.left() + name_w + val_w + src_w;
            if ours {
                paint_cell(ui, rect, nx, note_w, "Set by Quarry for this session", C_LINK(), true, false);
            } else if r.pending {
                ui.painter().circle_filled(egui::pos2(nx + PAD + 3.5, rect.center().y), 3.5, WARNING());
                let note = match &r.after_restart {
                    Some(a) => format!("{} after a restart", with_unit(a, "")),
                    None => "Needs a restart".into(),
                };
                paint_cell(ui, rect, nx + 12.0, note_w - 12.0, &note, WARNING(), false, false);
            }
            if resp.clicked() {
                sel = if selected { None } else { Some(r.name.clone()) };
            }
            if r.value.chars().count() > 24 {
                resp.on_hover_text(&r.value);
            }
        }
    });
    ui.spacing_mut().item_spacing.y = gap;
    if let Some(r) = sel.as_deref().and_then(|n| v.rows.iter().find(|r| r.name == n)) {
        ui.add_space(10.0);
        setting_detail(ui, r);
    }
    s.set_sel = sel;
}

fn setting_detail(ui: &mut egui::Ui, r: &Setting) {
    egui::Frame::new().fill(BG()).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 4.0;
        let k = |t: &str| RichText::new(t).size(TXT_SMALL).color(MUTED());
        let v = |t: &str| RichText::new(t).size(TXT_SMALL).color(TEXT());
        ui.horizontal(|ui| {
            ui.label(RichText::new(&r.name).monospace().color(BRIGHT()));
            ui.label(RichText::new(&r.desc).color(TEXT()));
        });
        ui.horizontal(|ui| {
            ui.label(k("Context"));
            ui.label(v(&r.context));
            if !r.min.is_empty() || !r.max.is_empty() {
                ui.add_space(8.0);
                ui.label(k("Range"));
                ui.label(v(&format!("{} to {}", with_unit(&r.min, &r.unit), with_unit(&r.max, &r.unit))));
            }
            ui.add_space(8.0);
            ui.label(k("Default"));
            ui.label(v(&with_unit(&r.boot, &r.unit)));
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Set in").monospace().size(TXT_SMALL).color(MUTED()));
            let file = if r.file.is_empty() { if r.source == "configuration file" { "not readable by this role" } else { "" } } else { r.file.as_str() };
            ui.label(RichText::new(file).monospace().size(TXT_SMALL).color(TEXT()));
            ui.add_space(12.0);
            ui.label(RichText::new("pending_restart").monospace().size(TXT_SMALL).color(MUTED()));
            ui.label(RichText::new(if r.pending { "true" } else { "false" }).monospace().size(TXT_SMALL).color(if r.pending { WARNING() } else { TEXT() }));
        });
    });
}

/// The right end of the Activity toolbar: Cancel query and Terminate for the selected row, or the second step of one.
fn act_buttons(ui: &mut egui::Ui, s: &mut PgServerWin, sel: Option<&Session>, name: &str, acts: &mut Acts) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if let Some(c) = s.confirm.clone() {
            // Drawn where Terminate was, so the guard keeps a double click from sending it.
            let r = primary(ui, if c.terminate { "Terminate" } else { "Cancel query" }, ALERT());
            if confirmed(ui, &r, c.at) {
                acts.send = Some(c.clone());
                s.confirm = None;
            }
            if ui.button("Keep").clicked() {
                s.confirm = None;
            }
            ui.label(RichText::new(if c.terminate { format!("End session {}?", c.s.pid) } else { format!("Cancel the query of {}?", c.s.pid) }).color(ALERT()));
            return;
        }
        let (can_t, can_c) = sel.map_or((false, false), |r| (can_signal(&s.own, r, true), can_signal(&s.own, r, false)));
        let term = ui.add_enabled_ui(can_t, |ui| danger(ui, "Terminate")).inner;
        let cancel = ui.add_enabled(can_c, egui::Button::new("Cancel query"));
        if let Some(r) = sel {
            for (resp, terminate) in [(&term, true), (&cancel, false)] {
                if s.writable {
                    if resp.clicked() {
                        s.confirm = Some(PgConfirm { s: r.clone(), terminate, at: arm_time(ui) });
                    }
                } else if ask_unlock(ui, resp, name, &if terminate { format!("terminate session {}", r.pid) } else { format!("cancel the query of {}", r.pid) }) {
                    acts.unlock = true;
                }
            }
        }
    });
}

const PID_W: f32 = 64.0;
const ACT_COLS: [(&str, f32); 6] = [("Pid", PID_W), ("User · client", 150.0), ("Database · app", 150.0), ("State", 180.0), ("Wait event", 180.0), ("Xact age", 80.0)];

fn activity_view(ui: &mut egui::Ui, s: &mut PgServerWin, max_h: f32, name: &str, acts: &mut Acts) {
    let (read_all, user, own_app) = match &s.act {
        Some(Ok(a)) => (a.read_all, a.user.clone(), a.own_app.clone()),
        _ => (true, String::new(), None),
    };
    if !s.writable && locked_banner(ui) {
        acts.unlock = true;
    }
    let hidden = s.rows().iter().any(|r| r.hidden);
    if !read_all && hidden {
        banner(ui, PRIMARY(), |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.label(RichText::new("Other roles’ sessions read ").color(TEXT()));
            ui.label(RichText::new("insufficient privilege").color(FAINT()));
            ui.label(RichText::new(format!(": {user} lacks pg_read_all_stats, so their state, client and query are hidden.")).color(TEXT()));
        }, None);
    }
    let rows: Vec<Session> = shown(s.rows(), &s.filter, s.hide_idle).into_iter().cloned().collect();
    let idle_hidden = if s.hide_idle { s.rows().iter().filter(|r| r.idle()).count() } else { 0 };
    // A pending action whose row the filter hid or that changed is dropped.
    if s.confirm.as_ref().is_some_and(|c| !confirm_ok(c, &rows)) {
        s.confirm = None;
    }
    let sel_row = s.sel.and_then(|p| rows.iter().find(|r| r.pid == p)).cloned();
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut s.filter).hint_text("Filter by user, database or query").desired_width(240.0));
        ui.checkbox(&mut s.hide_idle, "Hide idle");
        ui.checkbox(&mut s.auto, "Refresh every 2 s");
        if !s.auto && quiet(ui, "Refresh").clicked() {
            s.refresh = true;
        }
        act_buttons(ui, s, sel_row.as_ref(), name, acts);
    });
    ui.add_space(8.0);
    match &s.act {
        None => return spin(ui),
        Some(Err(e)) => {
            if error_row(ui, format!("Could not list the sessions: {e}")) {
                s.refresh = true;
            }
            return;
        }
        Some(Ok(_)) => {}
    }
    let w = ui.available_width();
    let fixed: f32 = ACT_COLS.iter().map(|c| c.1).sum();
    let query_w = (w - fixed).max(140.0);
    let mut cols: Vec<(&str, f32, bool)> = ACT_COLS.iter().map(|(l, w)| (*l, *w, matches!(*l, "Pid" | "Xact age"))).collect();
    cols.push(("Query", query_w, false));
    header(ui, &cols);
    if rows.is_empty() {
        ui.add_space(8.0);
        ui.label(RichText::new(if s.filter.trim().is_empty() { "No session is running." } else { "No session matches." }).color(MUTED()));
    } else {
        let mut sel = s.sel;
        let mut arm: Option<PgConfirm> = None;
        let mut fetch: Option<Session> = None;
        let own = s.own.clone();
        let writable = s.writable;
        let now = arm_time(ui);
        // show_rows steps by the row height plus the parent's item spacing, which would leave a gap per row.
        let gap = std::mem::replace(&mut ui.spacing_mut().item_spacing.y, 0.0);
        egui::ScrollArea::vertical().id_salt("pg-activity").max_height(max_h - 30.0).auto_shrink([false, true]).show_rows(ui, ROW2_H, rows.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for i in range {
                let r = &rows[i];
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW2_H), Sense::click());
                let resp = resp.on_hover_cursor(egui::CursorIcon::Default);
                let selected = sel == Some(r.pid);
                session_row(ui, rect, &resp, r, selected, i, &own, query_w);
                let r2 = resp.clone();
                r2.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Session {}", r.pid)));
                if resp.clicked() || resp.secondary_clicked() {
                    sel = Some(r.pid);
                }
                resp.context_menu(|ui| {
                    menu_look(ui);
                    if ui.add_enabled(writable && can_signal(&own, r, false), egui::Button::new("Cancel query").frame_when_inactive(false)).clicked() {
                        arm = Some(PgConfirm { s: r.clone(), terminate: false, at: now });
                        ui.close();
                    }
                    if ui.add_enabled_ui(writable && can_signal(&own, r, true), |ui| danger_item(ui, "Terminate session")).inner.clicked() {
                        arm = Some(PgConfirm { s: r.clone(), terminate: true, at: now });
                        ui.close();
                    }
                    if ui.add_enabled(!r.query.is_empty() && !r.hidden, egui::Button::new("Copy query").frame_when_inactive(false)).clicked() {
                        if r.cut {
                            fetch = Some(r.clone());
                        } else {
                            ui.ctx().copy_text(r.query.clone());
                        }
                        ui.close();
                    }
                });
            }
        });
        ui.spacing_mut().item_spacing.y = gap;
        s.sel = sel;
        if arm.is_some() {
            s.confirm = arm;
        }
        acts.fetch = fetch;
    }
    ui.add_space(8.0);
    // Ownership is per server: every Quarry connection to it is known, or behind a pooler only the application name is.
    let ours = match &own_app {
        Some(app) => format!("Behind a pooler Quarry knows its own sessions on this server only by their name, {app}, and never offers them."),
        None => "Quarry’s own sessions on this server, from any of its connections, are never offered.".into(),
    };
    let note = if !read_all {
        format!("Cancel and Terminate work on {user}’s own sessions, or on others with pg_signal_backend. {ours}")
    } else {
        let idle = if idle_hidden > 0 { format!("{idle_hidden} idle {} hidden. ", if idle_hidden == 1 { "session" } else { "sessions" }) } else { String::new() };
        format!("{idle}Cancel stops the running statement (pg_cancel_backend); Terminate ends the session and rolls back its transaction. {ours}")
    };
    ui.label(RichText::new(note).size(TXT_SMALL).color(FAINT()));
}

#[allow(clippy::too_many_arguments)]
fn session_row(ui: &egui::Ui, rect: egui::Rect, resp: &egui::Response, r: &Session, selected: bool, i: usize, own: &HashMap<u64, String>, query_w: f32) {
    let warn = r.idle_in_tx();
    let fill = match (selected, state_of(resp)) {
        (true, 0) => SEL(),
        (true, _) => SEL_HOVER(),
        (false, 0) if warn => tint(WARNING(), 20),
        (false, 0) => if i % 2 == 1 { PANEL() } else { BG() },
        (false, st) => hover_fill(st),
    };
    let p = ui.painter();
    p.rect_filled(rect, 0.0, fill);
    if warn && !selected {
        p.rect_filled(egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())), 0.0, WARNING());
    }
    let (y1, y2, h) = (rect.top() + 12.0, rect.top() + 28.0, 16.0);
    let mut x = rect.left();
    let col = |k: usize| ACT_COLS[k].1;
    text_at(ui, x, col(0), y1, h, &r.pid.to_string(), C_NUMBER(), true, TXT_SMALL, true);
    x += col(0);
    text_at(ui, x, col(1), y1, h, &r.user, if r.kind == "client backend" || r.hidden { BRIGHT() } else { MUTED() }, true, TXT_SMALL, false);
    if !r.client.is_empty() {
        text_at(ui, x, col(1), y2, h, &r.client, FAINT(), true, TXT_CAPTION, false);
    }
    x += col(1);
    text_at(ui, x, col(2), y1, h, &r.db, BRIGHT(), true, TXT_SMALL, false);
    let mine = r.ours || own.contains_key(&r.pid);
    if mine {
        text_at(ui, x, col(2), y2, h, "quarry (you)", C_LINK(), true, TXT_CAPTION, false);
    } else if !r.app.is_empty() {
        text_at(ui, x, col(2), y2, h, &r.app, FAINT(), true, TXT_CAPTION, false);
    }
    x += col(2);
    let (state, state_c) = match r.state.as_str() {
        _ if r.hidden => (String::new(), MUTED()),
        "active" => ("active".into(), SUCCESS()),
        st if st.starts_with("idle in transaction") => (st.to_string(), WARNING()),
        "idle" => ("idle".into(), FAINT()),
        "" => (r.kind.clone(), FAINT()),
        st => (st.to_string(), MUTED()),
    };
    text_at(ui, x, col(3), y1, h, &state, state_c, false, TXT_SMALL, false);
    if warn && let Some(t) = r.state_secs {
        text_at(ui, x, col(3), y2, h, &format!("for {}", span(t)), WARNING(), false, TXT_SMALL, false);
    }
    x += col(3);
    if !r.wait_type.is_empty() {
        let lock = r.wait_type == "Lock";
        text_at(ui, x, col(4), y1, h, &format!("{}: {}", r.wait_type, r.wait), if lock { WARNING() } else { TEXT() }, false, TXT_SMALL, false);
        if !r.blockers.is_empty() {
            let by = r.blockers.iter().map(|b| b.to_string()).collect::<Vec<_>>().join(", ");
            text_at(ui, x, col(4), y2, h, &format!("blocked by {by}"), C_LINK(), false, TXT_SMALL, false);
        }
    }
    x += col(4);
    if let Some(a) = r.xact_secs {
        text_at(ui, x, col(5), y1, h, &mss(a), if warn { WARNING() } else { C_NUMBER() }, true, TXT_SMALL, true);
    }
    x += col(5);
    text_at(ui, x, query_w, y1, h, &r.line, if r.hidden { FAINT() } else { TEXT() }, true, TXT_SMALL, false);
    if resp.hovered() && !r.hidden && !r.query.is_empty() && resp.hover_pos().is_some_and(|h| h.x >= x) {
        let long: String = r.query.chars().take(1200).collect();
        resp.clone().on_hover_text(long);
    } else if mine && resp.hover_pos().is_some_and(|h| h.x >= rect.left() + col(0) + col(1) && h.x < rect.left() + col(0) + col(1) + col(2)) {
        let what = own.get(&r.pid).map(String::as_str).unwrap_or("the Server window or another of its connections to this server");
        resp.clone().on_hover_text(format!("Quarry's own session ({what}). Cancel and Terminate are not offered for it."));
    }
}

const LOCK_COLS: [(&str, f32); 5] = [("Pid", 120.0), ("User", 120.0), ("Database", 110.0), ("Waits for", 200.0), ("Relation", 200.0)];
const WAIT_W: f32 = 70.0;

fn locks_view(ui: &mut egui::Ui, s: &mut PgServerWin, max_h: f32, name: &str, acts: &mut Acts) {
    if !s.writable && locked_banner(ui) {
        acts.unlock = true;
    }
    ui.horizontal(|ui| {
        ui.checkbox(&mut s.auto, "Refresh every 2 s");
        if !s.auto && quiet(ui, "Refresh").clicked() {
            s.refresh = true;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new("From pg_locks and pg_blocking_pids()").color(FAINT()));
        });
    });
    ui.add_space(8.0);
    match &s.act {
        None => return spin(ui),
        Some(Err(e)) => {
            if error_row(ui, format!("Could not read the locks: {e}")) {
                s.refresh = true;
            }
            return;
        }
        Some(Ok(_)) => {}
    }
    let rows = s.rows().to_vec();
    let ch = chains(&rows);
    if s.confirm.as_ref().is_some_and(|c| !confirm_ok(c, &rows) || !ch.iter().any(|x| x.head == c.s.pid)) {
        s.confirm = None;
    }
    let w = ui.available_width();
    let fixed: f32 = LOCK_COLS.iter().map(|c| c.1).sum::<f32>() + WAIT_W;
    let query_w = (w - fixed).max(140.0);
    let mut cols: Vec<(&str, f32, bool)> = LOCK_COLS.iter().map(|(l, w)| (*l, *w, false)).collect();
    cols.push(("Waiting", WAIT_W, true));
    cols.push(("Query", query_w, false));
    header(ui, &cols);
    if ch.is_empty() {
        ui.add_space(8.0);
        ui.label(RichText::new("No session waits for a lock.").color(MUTED()));
        return;
    }
    let by_pid: HashMap<u64, &Session> = rows.iter().map(|r| (r.pid, r)).collect();
    let mut deeper: Option<(u64, u64, u64)> = None;
    egui::ScrollArea::vertical().id_salt("pg-locks").max_height(max_h - 30.0).auto_shrink([false, true]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for c in &ch {
            let gone = Session { pid: c.head, ..Default::default() };
            let head = by_pid.get(&c.head).copied().unwrap_or(&gone);
            blocker_row(ui, s, head, w, query_w, name, acts);
            let mut parents: Vec<u64> = vec![c.head];
            for (pid, depth) in &c.waiting {
                parents.truncate(*depth);
                if *depth > 1 && deeper.is_none() {
                    deeper = Some((*pid, parents[depth - 1], c.head));
                }
                if let Some(r) = by_pid.get(pid) {
                    waiter_row(ui, r, *depth, w, query_w);
                }
                parents.push(*pid);
            }
        }
    });
    ui.add_space(8.0);
    let lead = deeper.map(|(p, q, h)| format!("{p} queues behind {q}, not behind {h} directly. ")).unwrap_or_default();
    ui.label(RichText::new(format!("{lead}Terminating a blocker rolls back its transaction and frees every session under it.")).size(TXT_SMALL).color(FAINT()));
}

fn blocker_row(ui: &mut egui::Ui, s: &mut PgServerWin, r: &Session, w: f32, query_w: f32, name: &str, acts: &mut Acts) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, HEAD_H), Sense::hover());
    let warn = r.idle_in_tx();
    let p = ui.painter();
    p.rect_filled(rect, 0.0, if warn { tint(WARNING(), 20) } else { PANEL() });
    if warn {
        p.rect_filled(egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())), 0.0, WARNING());
    }
    let (y1, y2, h) = (rect.top() + 15.0, rect.top() + 35.0, 18.0);
    let mut x = rect.left();
    text_at(ui, x, LOCK_COLS[0].1, y1, h, &r.pid.to_string(), C_NUMBER(), true, TXT_SMALL, false);
    x += LOCK_COLS[0].1;
    text_at(ui, x, LOCK_COLS[1].1, y1, h, &r.user, BRIGHT(), true, TXT_SMALL, false);
    let qx = x;
    x += LOCK_COLS[1].1;
    text_at(ui, x, LOCK_COLS[2].1, y1, h, &r.db, BRIGHT(), true, TXT_SMALL, false);
    x += LOCK_COLS[2].1;
    let g = ui.painter().layout_no_wrap("Blocker".into(), FontId::proportional(TXT_SMALL), ALERT());
    let bw = g.size().x;
    ui.painter().galley(egui::pos2(x + PAD, y1 - g.size().y / 2.0 + optical_dy(&g)), g, ALERT());
    let (what, color) = if r.hidden {
        (String::new(), MUTED())
    } else if warn {
        (format!("{} for {}", r.state, span(r.state_secs.unwrap_or(0.0))), WARNING())
    } else if r.active() {
        (format!("active for {}", span(r.query_secs.unwrap_or(0.0))), SUCCESS())
    } else {
        (r.state.clone(), MUTED())
    };
    let buttons_w = 260.0;
    text_at(ui, x + bw + 4.0, (rect.right() - buttons_w - x - bw).max(40.0), y1, h, &what, color, false, TXT_SMALL, false);
    text_at(ui, qx, (rect.right() - buttons_w - qx).max(60.0), y2, h, &r.line, if r.hidden { FAINT() } else { MUTED() }, true, TXT_SMALL, false);
    let _ = query_w;
    let area = egui::Rect::from_min_max(egui::pos2(rect.right() - buttons_w, rect.top() + 9.0), egui::pos2(rect.right() - 8.0, rect.bottom() - 9.0));
    let mut k = ui.new_child(egui::UiBuilder::new().max_rect(area).layout(egui::Layout::right_to_left(egui::Align::Center)));
    match s.confirm.clone().filter(|c| c.s.pid == r.pid) {
        Some(c) => {
            // Drawn where Terminate was, so the guard keeps a double click from sending it.
            let b = primary(&mut k, if c.terminate { "Terminate" } else { "Cancel query" }, ALERT());
            if confirmed(&k, &b, c.at) {
                acts.send = Some(c.clone());
                s.confirm = None;
            }
            if k.button("Keep").clicked() {
                s.confirm = None;
            }
            k.label(RichText::new(if c.terminate { format!("End session {}?", r.pid) } else { format!("Cancel the query of {}?", r.pid) }).size(TXT_SMALL).color(ALERT()));
        }
        None => {
            let term = k.add_enabled_ui(can_signal(&s.own, r, true), |ui| danger(ui, "Terminate")).inner;
            let cancel = k.add_enabled(can_signal(&s.own, r, false), egui::Button::new("Cancel query"));
            for (resp, terminate) in [(&term, true), (&cancel, false)] {
                if s.writable {
                    if resp.clicked() {
                        s.confirm = Some(PgConfirm { s: r.clone(), terminate, at: arm_time(&k) });
                        s.sel = Some(r.pid);
                    }
                } else if ask_unlock(&k, resp, name, &if terminate { format!("terminate session {}", r.pid) } else { format!("cancel the query of {}", r.pid) }) {
                    acts.unlock = true;
                }
            }
        }
    }
}

fn waiter_row(ui: &mut egui::Ui, r: &Session, depth: usize, w: f32, query_w: f32) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 0.0, BG());
    // The tree line: a stem from the row above and a tick into this one, one step in per level.
    let indent = 14.0 * depth as f32;
    let tx = rect.left() + PAD + indent - 8.0;
    let stroke = Stroke::new(1.0, FAINT());
    p.line_segment([egui::pos2(tx, rect.top()), egui::pos2(tx, rect.center().y)], stroke);
    p.line_segment([egui::pos2(tx, rect.center().y), egui::pos2(tx + 6.0, rect.center().y)], stroke);
    let cy = rect.center().y;
    let mut x = rect.left();
    text_at(ui, x + indent, LOCK_COLS[0].1 - indent, cy, ROW_H, &r.pid.to_string(), C_NUMBER(), true, TXT_SMALL, false);
    x += LOCK_COLS[0].1;
    text_at(ui, x, LOCK_COLS[1].1, cy, ROW_H, &r.user, BRIGHT(), true, TXT_SMALL, false);
    x += LOCK_COLS[1].1;
    text_at(ui, x, LOCK_COLS[2].1, cy, ROW_H, &r.db, BRIGHT(), true, TXT_SMALL, false);
    x += LOCK_COLS[2].1;
    let strong = matches!(r.lock_mode.as_str(), "AccessExclusiveLock" | "ExclusiveLock");
    text_at(ui, x, LOCK_COLS[3].1, cy, ROW_H, &r.lock_mode, if strong { WARNING() } else { TEXT() }, true, TXT_SMALL, false);
    x += LOCK_COLS[3].1;
    text_at(ui, x, LOCK_COLS[4].1, cy, ROW_H, &r.lock_on, TEXT(), true, TXT_SMALL, false);
    x += LOCK_COLS[4].1;
    text_at(ui, x, WAIT_W, cy, ROW_H, &r.query_secs.map(mss).unwrap_or_default(), C_NUMBER(), true, TXT_SMALL, true);
    x += WAIT_W;
    text_at(ui, x, query_w, cy, ROW_H, &r.line, if r.hidden { FAINT() } else { MUTED() }, true, TXT_SMALL, false);
    if resp.hovered() && !r.hidden && !r.query.is_empty() && resp.hover_pos().is_some_and(|h| h.x >= x) {
        let long: String = r.query.chars().take(1200).collect();
        resp.on_hover_text(long);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: u64, user: &str, state: &str, query: &str) -> Session {
        let mut s = Session::new(pid, user, "10.0.0.1", "shop", "app", state, query, "client backend");
        (s.backend_start, s.signal) = ("2026-11-12 10:00:00+00".into(), true);
        s
    }

    #[test]
    fn signals_are_offered_only_where_they_can_land() {
        let none = HashMap::new();
        let active = row(7, "app", "active", "SELECT pg_sleep(9)");
        let idle_tx = row(8, "app", "idle in transaction", "UPDATE t SET a = 1");
        assert!(can_signal(&none, &active, false) && can_signal(&none, &active, true));
        // An idle session has no statement to cancel but can be ended.
        assert!(!can_signal(&none, &idle_tx, false) && can_signal(&none, &idle_tx, true));
        // Quarry's own (the reading connection, a pooled one, or a Query run Stop ends) is never offered.
        let ours = Session { ours: true, ..active.clone() };
        assert!(!can_signal(&none, &ours, true) && !can_signal(&none, &ours, false));
        let own: HashMap<u64, String> = [(7, "Query tab".to_string())].into();
        assert!(!can_signal(&own, &active, true));
        // A row the role may not see or signal, and a server process, get neither.
        let hidden = Session::new(9, "other", "", "shop", "app", "", "<insufficient privilege>", "");
        assert!(hidden.hidden && !can_signal(&none, &Session { signal: true, ..hidden }, true));
        assert!(!can_signal(&none, &Session { signal: false, ..active.clone() }, true));
        assert!(!can_signal(&none, &Session { kind: "checkpointer".into(), ..active.clone() }, true));
    }

    #[test]
    fn a_pending_signal_dies_with_any_change_of_its_row() {
        let c = PgConfirm { s: row(9, "app", "active", "SELECT 1"), terminate: true, at: 0.0 };
        assert!(confirm_ok(&c, &[row(9, "app", "active", "SELECT 1")]));
        for other in [row(9, "app", "active", "SELECT 2"), row(9, "app", "idle", "SELECT 1"), row(9, "x", "active", "SELECT 1"), row(10, "app", "active", "SELECT 1"), Session { backend_start: "2026-11-12 11:00:00+00".into(), ..row(9, "app", "active", "SELECT 1") }] {
            assert!(!confirm_ok(&c, &[other]));
        }
    }

    #[test]
    fn an_overview_answer_never_moves_the_activity_poll() {
        let now = Instant::now();
        let mut s = PgServerWin::new(1, Vec::new());
        let due = now + POLL;
        (s.tab, s.act_next, s.ov_req, s.ov_wait) = (PgTab::Activity, Some(due), 5, true);
        // A failed Overview read that lands after the switch to Activity backs off the Overview only.
        s.take(5, Answer::Overview(Err("timed out".into())), now);
        assert_eq!(s.act_next, Some(due));
        assert!(s.ov_next.is_some_and(|t| t > due) && !s.ov_wait);
        // And the other way round.
        (s.act_req, s.act_wait) = (6, true);
        let ov = s.ov_next;
        s.take(6, Answer::Activity(Err("timed out".into())), now);
        assert_eq!(s.ov_next, ov);
    }

    #[test]
    fn a_failed_size_read_keeps_the_last_sizes() {
        let mut s = PgServerWin::new(1, Vec::new());
        s.sz_req = 3;
        s.take(3, Answer::Sizes(Ok(vec![("shop".into(), Some(10))])), Instant::now());
        s.sz_req = 4;
        s.take(4, Answer::Sizes(Err("canceling statement due to statement timeout".into())), Instant::now());
        assert_eq!(s.sizes, Some(vec![("shop".to_string(), Some(10))]));
        assert!(s.sizes_err.as_deref().is_some_and(|e| e.contains("timeout")));
    }

    #[test]
    fn hide_idle_keeps_open_transactions_and_hidden_rows() {
        let rows = vec![row(1, "app", "idle", ""), row(2, "app", "idle in transaction", "UPDATE t"), row(3, "app", "active", "SELECT 1"), Session::new(4, "other", "", "shop", "", "", "<insufficient privilege>", ""), Session::new(5, "", "", "", "", "", "", "checkpointer")];
        assert_eq!(shown(&rows, "", true).iter().map(|r| r.pid).collect::<Vec<_>>(), [2, 3, 4]);
        assert_eq!(shown(&rows, "", false).len(), 5);
        assert_eq!(shown(&rows, "UPDATE", false).iter().map(|r| r.pid).collect::<Vec<_>>(), [2]);
    }

    #[test]
    fn times_read_like_the_design() {
        assert_eq!(mss(724.4), "12:04");
        assert_eq!(mss(48.0), "0:48");
        assert_eq!(mss(3725.0), "1:02:05");
        assert_eq!(span(720.0), "12 min");
        assert_eq!(span(142.0), "2 min 22 s");
        assert_eq!(span(40.0), "40 s");
        assert_eq!(span(3900.0), "1 h 5 min");
    }

    #[test]
    fn units_and_sources_read_like_show() {
        assert_eq!(with_unit("16384", "8kB"), "128MB");
        assert_eq!(with_unit("4096", "kB"), "4MB");
        assert_eq!(with_unit("100", ""), "100");
        assert_eq!(with_unit("500", "ms"), "500ms");
        assert_eq!(with_unit("-1", "kB"), "-1kB");
        let s = |source: &str, name: &str| Setting { source: source.into(), lower: name.into(), ..Default::default() };
        assert_eq!(source_text(&s("database", "work_mem"), "jane", "analytics"), "database analytics");
        assert_eq!(source_text(&s("user", "search_path"), "reporting", "analytics"), "user reporting");
        assert_eq!(source_text(&s("client", "application_name"), "u", "d"), "session");
        let q = vec!["statement_timeout".to_string()];
        assert!(set_by_quarry(&s("session", "statement_timeout"), &q) && !set_by_quarry(&s("configuration file", "statement_timeout"), &q));
        let rows = vec![s("default", "a"), s("configuration file", "b_mem")];
        assert_eq!(shown_settings(&rows, "", true).len(), 1);
        assert_eq!(shown_settings(&rows, "MEM", false).len(), 1);
    }
}
