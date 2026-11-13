//! The Server window of a SQL connection: the process list with Kill and the variables. It follows the Mongo
//! window (`server.rs`): it asks only while it is open and Quarry is focused, never piles up requests, and keeps
//! errors in the window so a missing privilege is not announced every 2 seconds. View only: no SET anywhere.

use crate::app::*;
use db::sql::server::{metrics, rate, Answer, KillTarget, Metrics, Overview, Proc, Processes, Replica, Snap, StatusGroup, Var, Variables};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::time::Instant;

pub(in crate::app) const POLL: Duration = Duration::from_secs(2);
/// After a failed read a polling tab tries again this often, so one dropped packet does not end the polling and a
/// missing privilege is not asked about every 2 seconds.
const BACKOFF: Duration = Duration::from_secs(10);

pub(in crate::app) fn retry_after(ok: bool) -> Duration {
    if ok { POLL } else { BACKOFF }
}

/// How long a confirm button ignores clicks after it was armed, in seconds. A confirm can be drawn where the button that
/// armed it was, so the second click of a double click would otherwise confirm at once.
pub(in crate::app) const ARM_GUARD: f64 = 0.5;

/// The time a confirm is armed at: egui's input time, which a test can set.
pub(in crate::app) fn arm_time(ui: &egui::Ui) -> f64 {
    ui.input(|i| i.time)
}

/// Whether a click on the confirm button armed at `at` counts: not within ARM_GUARD of arming.
pub(in crate::app) fn confirmed(ui: &egui::Ui, r: &egui::Response, at: f64) -> bool {
    r.clicked() && arm_time(ui) - at >= ARM_GUARD
}

/// A Kill waiting for its second click, with everything the list showed about its target.
#[derive(Clone)]
pub(in crate::app) struct Confirm {
    p: Proc,
    identity: String,
    /// The whole connection ends (KILL) instead of only its statement (KILL QUERY).
    whole: bool,
    /// `arm_time` when it was armed.
    at: f64,
}

/// Whether the confirmation still points at the same statement of the same connection on the same server.
fn confirm_ok(c: &Confirm, p: &Processes) -> bool {
    c.identity == p.identity && p.rows.iter().any(|r| c.p.same_thread(r))
}

/// Whether a Kill may be offered for a row: only on an unlocked connection (like Mongo's Operations tab), never on
/// one of Quarry's own sessions (the person stops those with Stop, which also sets the cancel flag), and a statement
/// kill only while something runs.
fn can_kill(writable: bool, own: &HashMap<u64, String>, p: &Proc, whole: bool) -> bool {
    writable && !own.contains_key(&p.id) && if whole { !p.daemon() } else { p.running() }
}

/// What to tell the person about a Kill answer, and only for a request that was really sent: the answer can arrive
/// after the Server window was closed, so it is matched by request id on App and not on the window.
fn kill_toast(kills: &mut HashSet<u64>, req: u64, ans: &Answer) -> Option<Result<String, String>> {
    match ans {
        Answer::Kill { whole, res } if kills.remove(&req) => Some(res.clone().map(|_| if *whole { "Ended the connection".to_string() } else { "Stopped the statement".to_string() })),
        _ => None,
    }
}

/// The sample before the latest and the seconds between them, from the two instants themselves; a failed read
/// in between leaves no sample, so a rate never spans an unknown gap.
pub(in crate::app) fn prev_of<T>(old: Option<db::Res<(Instant, T)>>, now: Instant) -> Option<(f64, T)> {
    match old {
        Some(Ok((at, t))) => Some((now.duration_since(at).as_secs_f64(), t)),
        _ => None,
    }
}

/// An error with the way to try again, true when Refresh was clicked.
pub(in crate::app) fn error_row(ui: &mut egui::Ui, text: String) -> bool {
    ui.horizontal(|ui| {
        ui.label(RichText::new(text).color(ALERT()));
        quiet(ui, "Refresh").clicked()
    })
    .inner
}

#[derive(Clone, Copy, PartialEq, Default)]
pub(in crate::app) enum SqlServerTab {
    Overview,
    Variables,
    Status,
    #[default]
    Processes,
}

#[derive(Default)]
pub(in crate::app) struct SqlServerWin {
    pub(in crate::app) conn: u64,
    tab: SqlServerTab,
    /// The variables Quarry sets on every session of this connection, for the Variables note.
    quarry: Vec<String>,
    uptime: Option<u64>,
    /// Each read has its own request id, so an answer to an older one, or to a closed window, is dropped.
    procs_req: u64,
    vars_req: u64,
    procs_wait: bool,
    vars_wait: bool,
    /// When to ask for the processes again; None asks on the next frame.
    next: Option<Instant>,
    refresh: bool,
    procs: Option<db::Res<Processes>>,
    auto: bool,
    hide_sleeping: bool,
    filter: String,
    /// The selected thread by id, so a refresh keeps it.
    sel: Option<u64>,
    /// The thread waiting for its second click, and whether the whole connection or only its statement ends.
    confirm: Option<Confirm>,
    /// Whether the connection is unlocked, read each frame. Kill needs it.
    writable: bool,
    /// Quarry's own sessions on this server by thread id, with what each one is, read each frame. The list
    /// itself marks only the connection that ran SHOW PROCESSLIST.
    own: HashMap<u64, String>,
    /// Unlock was clicked in the note above the list.
    unlock: bool,
    global: bool,
    var_filter: String,
    vars: Option<db::Res<Variables>>,
    ov_req: u64,
    ov_wait: bool,
    ov: Option<db::Res<(Instant, Overview)>>,
    /// The sample before the latest and the seconds between them, for per second values.
    ov_prev: Option<(f64, Snap)>,
    st_req: u64,
    st_wait: bool,
    st: Option<db::Res<(Instant, Snap)>>,
    st_prev: Option<(f64, Snap)>,
    /// The names of the latest status sample in lowercase, in its order, so the filter does not fold every name each frame.
    st_lower: Vec<String>,
    st_auto: bool,
    st_filter: String,
    st_group: StatusGroup,
}

impl SqlServerWin {
    pub(in crate::app) fn new(conn: u64, quarry: Vec<String>) -> Self {
        Self { conn, quarry, auto: true, hide_sleeping: true, writable: true, st_auto: true, ..Default::default() }
    }

    fn set_lower(&mut self) {
        self.st_lower = match &self.st {
            Some(Ok((_, snap))) => snap.vals.iter().map(|(n, _)| n.to_lowercase()).collect(),
            _ => Vec::new(),
        };
    }

    /// Canned answers for the offline demo: `variables` opens that tab, anything else the processes.
    pub(in crate::app) fn demo(conn: u64, tab: &str) -> Self {
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
        s.st = Some(Ok((Instant::now(), sample(0))));
        s.set_lower();
        s.st_prev = Some((2.5, sample(1)));
        s.uptime = Some(41 * 86_400 + 3 * 3600);
        s.sel = Some(8812);
        s.auto = true;
        s.procs = Some(Ok(Processes { rows, me: 8901, uptime: s.uptime, identity: String::new() }));
        s.var_filter = "timeout".into();
        s.vars = Some(Ok(Variables { rows: vars, global: false, defaults: true, uptime: s.uptime }));
        s
    }
}

pub(in crate::app) fn up_text(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs / 3600, secs / 60);
    let plural = |n: u64, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
    if d > 0 { plural(d, "day") } else if h > 0 { plural(h, "hour") } else { plural(m, "minute") }
}

/// The process list rows the filter and "Hide sleeping" leave: the filter matches user, database or query.
fn shown<'a>(rows: &'a [Proc], filter: &str, hide_sleeping: bool) -> Vec<&'a Proc> {
    let f = filter.trim().to_lowercase();
    rows.iter().filter(|p| !(hide_sleeping && p.sleeping())).filter(|p| f.is_empty() || p.search.contains(&f)).collect()
}

/// Variable rows by name, for the session or global view.
fn shown_vars<'a>(rows: &'a [Var], filter: &str) -> Vec<&'a Var> {
    let f = filter.trim().to_lowercase();
    rows.iter().filter(|v| f.is_empty() || v.name.contains(&f)).collect()
}

impl App {
    pub(in crate::app) fn sql_server_window(&mut self, ctx: &egui::Context) {
        let Some(mut s) = self.sql_server.take() else { return };
        let conn = s.conn;
        let db = self.conn(conn).and_then(|c| c.sql.clone());
        let (name, product) = self.conn(conn).map(|c| (c.meta.name.clone(), format!("{} {}", c.server.product, c.server.version))).unwrap_or_default();
        if let (Some(db), false) = (&db, self.demo) {
            self.sql_server_poll(ctx, &mut s, db);
        }
        let mut open = true;
        let mut kill: Option<Confirm> = None;
        let live = db.is_some() || self.demo;
        // A locked connection offers no Kill, and a confirmation armed before the lock is dropped.
        s.writable = self.conn(conn).map_or(self.demo, |c| c.writable);
        s.own = self.own_sessions(conn);
        if !s.writable {
            s.confirm = None;
        }
        let screen = ctx.content_rect();
        let r = banded(egui::Id::unique(("quarry-layer", "sql-server-window"))).show(ctx, |ui| {
            ui.set_width(fit_width(ctx, (screen.width() * 0.7).clamp(760.0, 1100.0)));
            let up = s.uptime.map(|u| format!("  ·  up {}", up_text(u))).unwrap_or_default();
            const TABS: [(SqlServerTab, &str); 4] = [(SqlServerTab::Overview, "Overview"), (SqlServerTab::Variables, "Variables"), (SqlServerTab::Status, "Status"), (SqlServerTab::Processes, "Processes")];
            let labels = TABS.map(|t| t.1);
            let active = TABS.iter().position(|t| t.0 == s.tab).unwrap_or(0);
            if let Some(i) = dialog_head(ui, &Head::new("Server").sub(format!("{name}  ·  {}{up}", product.trim())).glyph(glyphs::server).tabs(&labels, active)) && TABS[i].0 != s.tab {
                s.tab = TABS[i].0;
                // Ask for the new tab's data on the next frame instead of when the old tab's timer ends.
                s.next = None;
            }
            let max = screen.height() - 380.0;
            dialog_pane(ui, |ui| {
                if !live {
                    // The pool is gone, so nothing here can be read or killed any more.
                    ui.label(RichText::new("This connection is closed. Reconnect to see the server again.").color(MUTED()));
                } else {
                    match s.tab {
                        SqlServerTab::Overview => overview_view(ui, &mut s),
                        SqlServerTab::Status => status_view(ui, &mut s, max),
                        SqlServerTab::Processes => procs_view(ui, &mut s, max, &mut kill),
                        SqlServerTab::Variables => vars_view(ui, &mut s, max),
                    }
                }
            });
            let auto = s.tab == SqlServerTab::Processes && s.auto;
            open = !dialog_foot(ui, |ui| {
                let done = ui.button("Done").on_hover_text("Esc").clicked();
                if auto {
                    foot_left(ui, |ui| {
                        let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
                        ui.painter().circle_filled(r.center(), 4.0, SUCCESS());
                        ui.label(RichText::new("Live  ·  refreshing every 2 s").size(TXT_SMALL).color(MUTED()));
                    });
                }
                done
            });
        });
        if std::mem::take(&mut s.unlock) {
            self.modal = Some(Modal::Unlock(conn, None));
        }
        if let (Some(c), Some(db)) = (kill, db) {
            self.req += 1;
            self.sql_kills.insert(self.req);
            self.worker.sql_server_kill(db, self.req, KillTarget::of(&c.p, &c.identity), c.whole, !s.writable);
        }
        if open && !(r.should_close() && self.modal.is_none()) {
            self.sql_server = Some(s);
        }
    }

    /// Asks for the tab on screen. The processes repeat every POLL while Quarry is focused and the last answer is
    /// in; the variables are read once per tab, scope or Refresh, since they hardly change.
    fn sql_server_poll(&mut self, ctx: &egui::Context, s: &mut SqlServerWin, db: &db::sql::Db) {
        if !ctx.input(|i| i.focused) {
            return;
        }
        let now = Instant::now();
        match s.tab {
            SqlServerTab::Processes => {
                if s.auto {
                    ctx.request_repaint_after(POLL);
                }
                let due = s.refresh || s.procs.is_none() || (s.auto && s.next.is_none_or(|t| now >= t));
                if s.procs_wait || !due {
                    return;
                }
                self.req += 1;
                (s.procs_req, s.procs_wait, s.refresh) = (self.req, true, false);
                self.worker.sql_server_processes(db.clone(), self.req);
            }
            SqlServerTab::Overview | SqlServerTab::Status => {
                let overview = s.tab == SqlServerTab::Overview;
                let (auto, empty, wait) = if overview { (true, s.ov.is_none(), s.ov_wait) } else { (s.st_auto, s.st.is_none(), s.st_wait) };
                if auto {
                    ctx.request_repaint_after(POLL);
                }
                let due = s.refresh || empty || (auto && s.next.is_none_or(|t| now >= t));
                if wait || !due {
                    return;
                }
                self.req += 1;
                s.refresh = false;
                if overview {
                    (s.ov_req, s.ov_wait) = (self.req, true);
                    self.worker.sql_server_overview(db.clone(), self.req);
                } else {
                    (s.st_req, s.st_wait) = (self.req, true);
                    self.worker.sql_server_status(db.clone(), self.req);
                }
            }
            SqlServerTab::Variables => {
                if s.vars_wait || !(s.refresh || s.vars.is_none()) {
                    return;
                }
                self.req += 1;
                (s.vars_req, s.vars_wait, s.refresh) = (self.req, true, false);
                self.worker.sql_server_variables(db.clone(), self.req, s.global);
            }
        }
    }

    /// Quarry's own sessions on connection `conn` by server thread id, with what each one is: the ids kept for Stop
    /// (a Query run, a running export, an import or script file, the shell's relays). They get no Kill, because
    /// Stop also sets the cancel flag so the work ends instead of carrying on after the statement is cut.
    pub(in crate::app) fn own_sessions(&self, conn: u64) -> HashMap<u64, String> {
        let mut own = HashMap::new();
        let mut add = |id: u64, what: &str| {
            if id != 0 {
                own.insert(id, what.to_string());
            }
        };
        for t in &self.tabs {
            if let Some(s) = t.sql.as_ref().filter(|s| s.run_req != 0 && t.sel.conn == conn) {
                add(s.run.conn.load(Ordering::SeqCst), "Query tab");
            }
            if t.shell.cancel.is_some() {
                for (c, id) in &t.shell.relays {
                    if *c == conn {
                        add(id.load(Ordering::SeqCst), "Shell");
                    }
                }
            }
        }
        if let Some(j) = self.export_job.as_ref().filter(|j| j.db.conn == conn) {
            add(j.id.load(Ordering::SeqCst), "Export");
        }
        if let Some(j) = self.sql_job.as_ref().filter(|j| j.conn == conn) {
            add(j.id.load(Ordering::SeqCst), if j.kind == sql::dump::JobKind::Import { "Import" } else { "Script file" });
        }
        own
    }

    pub(in crate::app) fn sql_server_done(&mut self, req: u64, ans: Answer) {
        // A Kill answer is told even when the window is gone, so a failure is never silent.
        if let Some(told) = kill_toast(&mut self.sql_kills, req, &ans) {
            if let (Ok(_), Some(s)) = (&told, self.sql_server.as_mut()) {
                s.refresh = true;
                s.next = None;
            }
            match told {
                Ok(m) => self.ok(m),
                Err(e) => self.err(e),
            }
            return;
        }
        let Some(s) = self.sql_server.as_mut() else { return };
        match ans {
            Answer::Procs(r) if req == s.procs_req => {
                s.procs_wait = false;
                s.next = Some(Instant::now() + retry_after(r.is_ok()));
                match &r {
                    Ok(p) => {
                        s.uptime = p.uptime.or(s.uptime);
                        // The selection follows the thread by id. A pending Kill follows it only while it is the same
                        // statement on the same server: ids are reused, and a restart starts them over.
                        s.sel = s.sel.filter(|id| p.rows.iter().any(|x| x.id == *id));
                        s.confirm = s.confirm.take().filter(|c| confirm_ok(c, p));
                    }
                    Err(_) => s.confirm = None,
                }
                s.procs = Some(r);
            }
            Answer::Vars(r) if req == s.vars_req => {
                s.vars_wait = false;
                if let Ok(v) = &r {
                    s.uptime = v.uptime.or(s.uptime);
                }
                s.vars = Some(r);
            }
            Answer::Overview(r) if req == s.ov_req => {
                s.ov_wait = false;
                let now = Instant::now();
                s.next = Some(now + retry_after(r.is_ok()));
                s.ov_prev = prev_of(s.ov.take().map(|o| o.map(|(at, o)| (at, o.snap))), now);
                if let Ok(o) = &r {
                    s.uptime = o.snap.num("Uptime").map(|u| u as u64).or(s.uptime);
                }
                s.ov = Some(r.map(|o| (now, o)));
            }
            Answer::Status(r) if req == s.st_req => {
                s.st_wait = false;
                let now = Instant::now();
                s.next = Some(now + retry_after(r.is_ok()));
                s.st_prev = prev_of(s.st.take(), now);
                if let Ok(v) = &r {
                    s.uptime = v.num("Uptime").map(|u| u as u64).or(s.uptime);
                }
                s.st = Some(r.map(|v| (now, v)));
                s.set_lower();
            }
            _ => {}
        }
    }
}

const ID_W: f32 = 64.0;
const COLS: [(&str, f32); 7] = [("Id", ID_W), ("User", 120.0), ("Host", 140.0), ("Db", 100.0), ("Command", 74.0), ("Time", 76.0), ("State", 150.0)];
pub(in crate::app) const PAD: f32 = 10.0;
const KILL_W: f32 = 72.0;
const CONFIRM_W: f32 = 290.0;

/// One cell of a painted row: a single line cut with an ellipsis at the cell's width.
pub(in crate::app) fn paint_cell(ui: &egui::Ui, row: egui::Rect, x: f32, w: f32, text: &str, color: Color32, mono: bool, right: bool) {
    let font = if mono { FontId::monospace(TXT_SMALL) } else { FontId::proportional(TXT_SMALL) };
    place(ui, egui::Rect::from_min_max(egui::pos2(x, row.top()), egui::pos2(x + w, row.bottom())), text, font, color, right, PAD);
}

pub(in crate::app) fn header(ui: &mut egui::Ui, cols: &[(&str, f32, bool)]) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::hover());
    ui.painter().rect_filled(rect, R_SM, HDR());
    let mut x = rect.left();
    for (label, w, right) in cols {
        let g = ui.painter().layout_no_wrap(label.to_string(), FontId::proportional(TXT_CAPTION), MUTED());
        let gx = if *right { x + w - PAD - g.size().x } else { x + PAD };
        ui.painter().galley(egui::pos2(gx, rect.center().y - g.size().y / 2.0 + optical_dy(&g)), g, MUTED());
        x += w;
    }
}

fn procs_view(ui: &mut egui::Ui, s: &mut SqlServerWin, max_h: f32, kill: &mut Option<Confirm>) {
    let (filter, hide) = (s.filter.clone(), s.hide_sleeping);
    let (me, total, running, identity, rows): (u64, usize, usize, String, Vec<&Proc>) = match &s.procs {
        Some(Ok(p)) => (p.me, p.rows.len(), p.rows.iter().filter(|r| r.running()).count(), p.identity.clone(), shown(&p.rows, &filter, hide)),
        _ => (0, 0, 0, String::new(), Vec::new()),
    };
    // A Kill waiting for its second click is dropped when its row is hidden by the filter or has changed.
    if s.confirm.as_ref().is_some_and(|c| !rows.iter().any(|r| c.p.same_thread(r))) {
        s.confirm = None;
    }
    let (writable, mut own) = (s.writable, s.own.clone());
    // The thread that reads the list is Quarry's too.
    own.entry(me).or_insert_with(|| "Server window".to_string());
    let sel_row = s.sel.and_then(|id| rows.iter().find(|r| r.id == id)).filter(|r| can_kill(writable, &own, r, false));
    if !writable {
        s.unlock = banner(ui, Notice::Locked, "Read-only.", "Unlock writes to kill queries or connections.", Some("Unlock…"));
    }
    if let Some(Ok(p)) = &s.procs {
        // Summary tiles over the list, read from the rows already loaded.
        let sleeping = p.rows.iter().filter(|r| r.sleeping()).count();
        let longest = p.rows.iter().filter(|r| r.running()).map(|r| r.secs).fold(0.0, f64::max);
        let tile_w = (ui.available_width() - 36.0) / 4.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            stat_tile(ui, tile_w, "Connections", &group(&total.to_string()), None);
            stat_tile(ui, tile_w, "Running", &group(&running.to_string()), (running > 0).then(PRIMARY));
            stat_tile(ui, tile_w, "Sleeping", &group(&sleeping.to_string()), None);
            stat_tile(ui, tile_w, "Longest running", &format!("{} s", group(&(longest as i64).to_string())), (longest >= 60.0).then(WARNING));
        });
        ui.add_space(14.0);
    }
    ui.horizontal(|ui| {
        well_field(ui, egui::Id::unique("sql-procs-filter"), FieldIcon::Search, None, |ui| ui.add(field_edit(&mut s.filter, egui::Id::unique("sql-procs-filter"), "Filter by user, database or query", 260.0, egui::TextStyle::Body)));
        ui.add_space(4.0);
        if chip(ui, "Hide sleeping", PRIMARY(), s.hide_sleeping).clicked() {
            s.hide_sleeping = !s.hide_sleeping;
        }
        if chip(ui, "Refresh every 2 s", PRIMARY(), s.auto).clicked() {
            s.auto = !s.auto;
        }
        if !s.auto && quiet(ui, "Refresh").clicked() {
            s.refresh = true;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let r = ui.add_enabled_ui(sel_row.is_some(), |ui| danger(ui, "Kill query")).inner;
            if let (true, Some(p)) = (r.clicked(), sel_row) {
                s.confirm = Some(Confirm { p: (*p).clone(), identity: identity.clone(), whole: false, at: arm_time(ui) });
            }
        });
    });
    ui.add_space(8.0);
    match &s.procs {
        None => return spin(ui),
        Some(Err(e)) => {
            if error_row(ui, format!("Could not list the processes: {e}")) {
                s.refresh = true;
            }
            return;
        }
        Some(Ok(_)) => {}
    }
    let hidden = if hide && filter.trim().is_empty() { total - rows.len() } else { 0 };
    let mut show_all = false;
    card_frame().show(ui, |ui| {
    ui.set_width(ui.available_width());
    ui.spacing_mut().item_spacing.y = 0.0;
    let w = ui.available_width();
    let info_w = (w - COLS.iter().map(|c| c.1).sum::<f32>()).max(120.0);
    let mut cols: Vec<(&str, f32, bool)> = COLS.iter().map(|(l, w)| (*l, *w, *l == "Time")).collect();
    cols.push(("Info", info_w, false));
    header(ui, &cols);
    if rows.is_empty() {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(PAD);
            ui.label(RichText::new(if s.filter.trim().is_empty() { "Nothing is running." } else { "No process matches." }).color(MUTED()));
        });
        ui.add_space(10.0);
    }
    let mut sel = s.sel;
    let mut confirm = s.confirm.clone();
    egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show_rows(ui, ROW_H, rows.len(), |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for i in range {
            let p = rows[i];
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::click());
            let selected = sel == Some(p.id);
            let fill = match (selected, state_of(&resp)) {
                (true, 0) => SEL(),
                (true, _) => SEL_HOVER(),
                (false, 0) => if i % 2 == 1 { tint(TEXT(), 6) } else { Color32::TRANSPARENT },
                (false, st) => hover_fill(st),
            };
            ui.painter().rect_filled(rect, 0.0, fill);
            let mine = own.get(&p.id);
            let mut x = rect.left();
            let slow = p.running() && p.secs >= 60.0;
            let cells: [(String, Color32, bool, bool); 7] = [
                (p.id.to_string(), if p.daemon() { C_NUMBER().gamma_multiply(0.8) } else { C_NUMBER() }, true, true),
                (p.user.clone(), if p.daemon() { MUTED() } else { BRIGHT() }, true, false),
                (if mine.is_some() { "quarry (you)".into() } else { p.host.clone() }, if mine.is_some() { PRIMARY() } else { MUTED() }, true, false),
                (p.db.clone(), TEXT(), true, false),
                (p.command.clone(), if p.running() { WARNING() } else if p.sleeping() { FAINT() } else { MUTED() }, true, false),
                (format!("{} s", group(&(p.secs as i64).to_string())), if slow { ALERT() } else { BRIGHT() }, true, false),
                (p.state.clone(), if slow { WARNING() } else { TEXT() }, false, false),
            ];
            for ((_, w), (text, color, mono, right)) in COLS.iter().zip(&cells) {
                paint_cell(ui, rect, x, *w, text, *color, *mono, *right);
                x += w;
            }
            let confirming = confirm.as_ref().is_some_and(|c| c.p.id == p.id);
            let info_x = rect.left() + COLS.iter().map(|c| c.1).sum::<f32>();
            paint_cell(ui, rect, info_x, info_w - if confirming { CONFIRM_W } else { KILL_W }, &p.line, MUTED(), true, false);
            if resp.clicked() || resp.secondary_clicked() {
                sel = Some(p.id);
            }
            let host_x = rect.left() + COLS[..2].iter().map(|c| c.1).sum::<f32>();
            if let (Some(what), true) = (mine, resp.hover_pos().is_some_and(|h| h.x >= host_x && h.x < host_x + COLS[2].1)) {
                resp.clone().on_hover_text(format!("Quarry's own session ({what}). Stop it there: Kill is not offered for it."));
            } else if resp.hovered() && !confirming && !p.info.is_empty() && resp.hover_pos().is_some_and(|h| h.x >= info_x) {
                let long: String = p.info.chars().take(1200).collect();
                resp.clone().on_hover_text(long);
            }
            let can_query = can_kill(writable, &own, p, false);
            let can_end = can_kill(writable, &own, p, true);
            let now = arm_time(ui);
            let arm = |whole: bool| Confirm { p: p.clone(), identity: identity.clone(), whole, at: now };
            resp.context_menu(|ui| {
                menu_look(ui);
                if ui.add_enabled(can_query, egui::Button::new("Kill query").frame_when_inactive(false)).clicked() {
                    confirm = Some(arm(false));
                    ui.close();
                }
                if ui.add_enabled_ui(can_end, |ui| danger_item(ui, "Kill connection")).inner.clicked() {
                    confirm = Some(arm(true));
                    ui.close();
                }
                if ui.add_enabled(!p.info.is_empty(), egui::Button::new("Copy query").frame_when_inactive(false)).clicked() {
                    ui.ctx().copy_text(p.info.clone());
                    ui.close();
                }
            });
            // The right end of the row: a Kill button, or the second step of one.
            let width = if confirming { CONFIRM_W } else { KILL_W };
            let area = egui::Rect::from_min_max(egui::pos2(rect.right() - width, rect.top() + 2.0), egui::pos2(rect.right() - 6.0, rect.bottom() - 2.0));
            if confirming {
                ui.painter().rect_filled(area.expand2(egui::vec2(6.0, 2.0)).intersect(rect), 0.0, fill);
            }
            if confirming || can_query {
                let mut k = ui.new_child(egui::UiBuilder::new().max_rect(area).layout(egui::Layout::right_to_left(egui::Align::Center)));
                match confirm.clone().filter(|c| c.p.id == p.id) {
                    Some(c) => {
                        if quiet(&mut k, "Keep").clicked() {
                            confirm = None;
                        }
                        let r = danger(&mut k, if c.whole { "Kill connection" } else { "Kill it" });
                        if confirmed(&k, &r, c.at) {
                            *kill = Some(c.clone());
                            confirm = None;
                        }
                        k.label(RichText::new(if c.whole { "End the connection?" } else { "Stop the statement?" }).size(TXT_SMALL).color(MUTED()));
                    }
                    None => {
                        if danger(&mut k, "Kill").clicked() {
                            confirm = Some(arm(false));
                            sel = Some(p.id);
                        }
                    }
                }
            }
        }
    });
    s.sel = sel;
    s.confirm = confirm;
    if hidden > 0 {
        show_all = table_foot_row(ui, &format!("{} sleeping connection{} hidden.", group(&hidden.to_string()), if hidden == 1 { "" } else { "s" }), Some("Show them"));
    }
    });
    if show_all {
        s.hide_sleeping = false;
    }
    ui.add_space(14.0);
}

fn vars_view(ui: &mut egui::Ui, s: &mut SqlServerWin, max_h: f32) {
    let before = s.global;
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut s.var_filter).hint_text("Filter variables").desired_width(300.0));
        let mut scope = s.global;
        segmented(ui, &mut scope, &[(false, "Session"), (true, "Global")]);
        s.global = scope;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new("Values changed from the default are brighter").size(TXT_SMALL).color(FAINT()));
        });
    });
    // The other scope has other values, so its rows are asked for again.
    if s.global != before {
        s.vars = None;
        s.vars_req = 0;
        s.vars_wait = false;
    }
    ui.add_space(8.0);
    let v = match &s.vars {
        None => return spin(ui),
        Some(Err(e)) => {
            if error_row(ui, format!("Could not read the variables: {e}")) {
                s.refresh = true;
            }
            return;
        }
        Some(Ok(v)) => v,
    };
    let rows = shown_vars(&v.rows, &s.var_filter);
    let w = ui.available_width();
    let (name_w, val_w, def_w) = (300.0_f32.min(w * 0.3), 200.0_f32.min(w * 0.22), 200.0_f32.min(w * 0.22));
    header(ui, &[("Name", name_w, false), ("Value", val_w, true), ("Default", def_w, true), ("Note", (w - name_w - val_w - def_w).max(80.0), false)]);
    if rows.is_empty() {
        ui.add_space(8.0);
        ui.label(RichText::new("No variable matches.").color(MUTED()));
        return;
    }
    let (global, quarry) = (v.global, &s.quarry);
    egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show_rows(ui, ROW_H, rows.len(), |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for i in range {
            let v = rows[i];
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::hover());
            ui.painter().rect_filled(rect, 0.0, if i % 2 == 1 { PANEL() } else { BG() });
            let changed = v.changed();
            let ours = !global && quarry.contains(&v.name);
            let note: (String, Color32) = if ours { ("Set by Quarry for this session".into(), C_LINK()) } else { (v.note.clone(), MUTED()) };
            let value_color = if changed { C_NUMBER() } else { C_NUMBER().gamma_multiply(0.6) };
            paint_cell(ui, rect, rect.left(), name_w, &v.name, BRIGHT(), true, false);
            paint_cell(ui, rect, rect.left() + name_w, val_w, &v.value, value_color, true, true);
            paint_cell(ui, rect, rect.left() + name_w + val_w, def_w, v.default.as_deref().unwrap_or(""), FAINT(), true, true);
            paint_cell(ui, rect, rect.left() + name_w + val_w + def_w, w - name_w - val_w - def_w, &note.0, note.1, true, false);
            if v.value.chars().count() > 24 {
                resp.on_hover_text(&v.value);
            }
        }
    });
    if !v.defaults {
        ui.add_space(6.0);
        ui.label(RichText::new("Defaults are not shown: performance_schema is off or not readable by this account.").size(TXT_SMALL).color(MUTED()));
    }
}

/// A number of the status as the table shows it: byte counters in units, whole numbers with separators.
fn status_value(name: &str, v: &str) -> String {
    let int = v.strip_prefix('-').unwrap_or(v);
    if name.starts_with("Bytes_") && int.bytes().all(|b| b.is_ascii_digit()) && !int.is_empty() {
        human_bytes(v.parse::<u64>().unwrap_or(0) as usize)
    } else if !int.is_empty() && int.bytes().all(|b| b.is_ascii_digit()) {
        format!("{}{}", if v.starts_with('-') { "-" } else { "" }, group(int))
    } else {
        v.to_string()
    }
}

/// A per second value: bytes in units, others with one decimal below 100 and none from there, no ".0".
fn fmt_rate(name: &str, r: f64) -> String {
    if name.starts_with("Bytes_") {
        return format!("{}/s", human_bytes(r as usize));
    }
    if r >= 100.0 {
        return format!("{}/s", group(&format!("{r:.0}")));
    }
    let t = format!("{r:.1}");
    format!("{}/s", t.strip_suffix(".0").unwrap_or(&t))
}

/// The server's running state of a replication thread in words.
fn thread_state(v: &str) -> (&str, Color32) {
    match v {
        "Yes" | "ON" => ("Running", SUCCESS()),
        "No" | "OFF" => ("Stopped", ALERT()),
        "Connecting" => ("Connecting", WARNING()),
        "" => ("", MUTED()),
        other => (other, WARNING()),
    }
}

pub(in crate::app) fn tile(ui: &mut egui::Ui, rect: egui::Rect, title: &str, big: &str, suffix: &str, sub: &str) {
    ui.painter().rect(rect, R_MD, PANEL(), Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
    let x = rect.left() + 12.0;
    let t = ui.painter().layout_no_wrap(title.to_string(), FontId::proportional(TXT_SMALL), MUTED());
    ui.painter().galley(egui::pos2(x, rect.top() + 12.0), t, MUTED());
    let mut job = LayoutJob::single_section(big.to_string(), ui_fmt(20.0, C_NUMBER(), W_SEMIBOLD, 0.0));
    if !suffix.is_empty() {
        job.append(&format!(" {suffix}"), 0.0, mono_fmt(TXT_SMALL, MUTED(), 400.0));
    }
    let g = ui.painter().layout_job(job);
    ui.painter().galley(egui::pos2(x, rect.top() + 32.0), g, C_NUMBER());
    let sg = ui.painter().layout_no_wrap(sub.to_string(), FontId::proportional(TXT_SMALL), MUTED());
    ui.painter().galley(egui::pos2(x, rect.bottom() - 12.0 - sg.size().y), sg, MUTED());
}

/// Label and value rows of a key/value box.
pub(in crate::app) fn kv_rows(ui: &mut egui::Ui, w: f32, rows: &[(&str, String, Color32)]) {
    for (i, (k, v, c)) in rows.iter().enumerate() {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::hover());
        ui.painter().rect_filled(rect, 0.0, if i % 2 == 1 { PANEL() } else { BG() });
        paint_cell(ui, rect, rect.left(), 130.0, k, MUTED(), false, false);
        paint_cell(ui, rect, rect.left() + 130.0, w - 130.0, v, *c, true, false);
    }
}

fn overview_view(ui: &mut egui::Ui, s: &mut SqlServerWin) {
    let (o, m): (&Overview, Metrics) = match &s.ov {
        None => return spin(ui),
        Some(Err(e)) => {
            if error_row(ui, format!("Could not read the server status: {e}")) {
                s.refresh = true;
            }
            return;
        }
        Some(Ok((_, o))) => (o, metrics(&o.snap, &o.vars, s.ov_prev.as_ref())),
    };
    let int = |v: Option<f64>| v.map(|v| group(&format!("{v:.0}"))).unwrap_or_else(|| "…".into());
    let bytes = |v: Option<f64>| v.map(|v| human_bytes(v as usize)).unwrap_or_else(|| "…".into());
    let gap = 12.0;
    let w = (ui.available_width() - gap * 4.0) / 5.0;
    let (strip, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 86.0), Sense::hover());
    let at = |i: usize| egui::Rect::from_min_size(egui::pos2(strip.left() + i as f32 * (w + gap), strip.top()), egui::vec2(w, strip.height()));
    tile(ui, at(0), "Connections", &int(m.connected), &m.max_connections.map(|v| format!("of {}", group(&format!("{v:.0}")))).unwrap_or_default(), &format!("{} running", int(m.running)));
    tile(ui, at(1), "Queries per second", &int(m.qps), "", &format!("{} since start", int(m.questions)));
    tile(ui, at(2), "Slow queries", &int(m.slow), "", "since start");
    tile(ui, at(3), "Buffer pool", &m.hit.map(|h| format!("{:.1}%", h * 100.0)).unwrap_or_else(|| "…".into()), "hit", &match (m.pool_used, m.pool_size) {
        (Some(u), Some(z)) => format!("{} of {}", bytes(Some(u)), bytes(Some(z))),
        _ => String::new(),
    });
    tile(ui, at(4), "Network per second", &bytes(m.net_in), "in", &format!("{} out", bytes(m.net_out)));
    ui.add_space(16.0);
    let var = |n: &str| o.vars.get(n).cloned().unwrap_or_default();
    let tz = if var("time_zone") == "SYSTEM" { format!("{} (SYSTEM)", var("system_time_zone")) } else { var("time_zone") };
    let server = [
        ("Version", format!("{} {}", var("version"), var("version_comment")).trim().to_string(), BRIGHT()),
        ("Host", var("hostname"), BRIGHT()),
        ("Port", var("port"), C_NUMBER()),
        ("TLS", if o.tls.is_empty() { "Not encrypted".into() } else { o.tls.clone() }, BRIGHT()),
        ("Data directory", var("datadir"), BRIGHT()),
        ("Character set", format!("{}, {}", var("character_set_server"), var("collation_server")), BRIGHT()),
        ("Time zone", tz, BRIGHT()),
    ];
    ui.columns(2, |cols| {
        let ui = &mut cols[0];
        caption(ui, "SERVER");
        ui.add_space(4.0);
        let w = ui.available_width();
        ui.spacing_mut().item_spacing.y = 0.0;
        kv_rows(ui, w, &server);
        let ui = &mut cols[1];
        caption(ui, "REPLICATION");
        ui.add_space(4.0);
        match &o.replica {
            Err(e) => {
                ui.label(RichText::new(e).color(MUTED()));
            }
            Ok(list) if list.is_empty() => {
                ui.label(RichText::new("Not a replica").color(MUTED()));
            }
            Ok(list) => {
                let w = ui.available_width();
                ui.spacing_mut().item_spacing.y = 0.0;
                let cw = [w * 0.4, w * 0.2, w * 0.2, w * 0.2];
                header(ui, &[("Source", cw[0], false), ("IO thread", cw[1], false), ("SQL thread", cw[2], false), ("Behind", cw[3], true)]);
                // One block per channel, so a multi-source replica shows every source.
                for r in list {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::hover());
                    ui.painter().rect_filled(rect, 0.0, BG());
                    let (io, io_c) = thread_state(&r.io);
                    let (sql, sql_c) = thread_state(&r.sql);
                    let source = if r.channel.is_empty() { r.source.clone() } else { format!("{}: {}", r.channel, r.source) };
                    paint_cell(ui, rect, rect.left(), cw[0], &source, BRIGHT(), true, false);
                    paint_cell(ui, rect, rect.left() + cw[0], cw[1], io, io_c, true, false);
                    paint_cell(ui, rect, rect.left() + cw[0] + cw[1], cw[2], sql, sql_c, true, false);
                    paint_cell(ui, rect, rect.left() + cw[0] + cw[1] + cw[2], cw[3], &r.behind.map(|b| format!("{b} s")).unwrap_or_default(), C_NUMBER(), true, true);
                    let mut extra = vec![("GTID set", r.gtid.clone(), TEXT())];
                    if !r.error.is_empty() {
                        extra.push(("Last error", r.error.clone(), ALERT()));
                    }
                    kv_rows(ui, w, &extra.iter().filter(|(_, v, _)| !v.is_empty()).map(|(k, v, c)| (*k, v.clone(), *c)).collect::<Vec<_>>());
                }
            }
        }
    });
}

fn status_view(ui: &mut egui::Ui, s: &mut SqlServerWin, max_h: f32) {
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut s.st_filter).hint_text("Filter status names").desired_width(300.0));
        segmented(ui, &mut s.st_group, &StatusGroup::ALL);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.checkbox(&mut s.st_auto, "Refresh every 2 s");
            if !s.st_auto && quiet(ui, "Refresh").clicked() {
                s.refresh = true;
            }
        });
    });
    ui.add_space(8.0);
    let snap = match &s.st {
        None => return spin(ui),
        Some(Err(e)) => {
            if error_row(ui, format!("Could not read the server status: {e}")) {
                s.refresh = true;
            }
            return;
        }
        Some(Ok((_, v))) => v,
    };
    let f = s.st_filter.trim().to_lowercase();
    let rows: Vec<&(String, String)> = snap.vals.iter().enumerate().filter(|(i, (n, _))| s.st_group.has(n) && (f.is_empty() || s.st_lower.get(*i).is_some_and(|l| l.contains(&f)))).map(|(_, r)| r).collect();
    let w = ui.available_width();
    let (name_w, val_w) = (w * 0.4, w * 0.2);
    header(ui, &[("Name", name_w, false), ("Value", val_w, true), ("Per second", val_w, true)]);
    if rows.is_empty() {
        ui.add_space(8.0);
        ui.label(RichText::new("No status variable matches.").color(MUTED()));
        return;
    }
    let prev = s.st_prev.as_ref();
    egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show_rows(ui, ROW_H, rows.len(), |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for i in range {
            let (name, value) = rows[i];
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::hover());
            ui.painter().rect_filled(rect, 0.0, if i % 2 == 1 { PANEL() } else { BG() });
            let numeric = value.trim().parse::<f64>().is_ok();
            paint_cell(ui, rect, rect.left(), name_w, name, BRIGHT(), true, false);
            paint_cell(ui, rect, rect.left() + name_w, val_w, &status_value(name, value), if numeric { C_NUMBER() } else { TEXT() }, true, true);
            if let Some(r) = rate(snap, prev, name) {
                paint_cell(ui, rect, rect.left() + name_w + val_w, val_w, &fmt_rate(name, r), if r == 0.0 { FAINT() } else { C_NUMBER() }, true, true);
            }
            if value.chars().count() > 30 {
                resp.on_hover_text(value);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: u64, user: &str, db: &str, command: &str, info: &str) -> Proc {
        Proc::new(id, user, "", db, command, 0.0, "", info)
    }

    #[test]
    fn kill_needs_an_unlocked_connection_and_never_reaches_quarrys_own_sessions() {
        let running = p(7, "app", "shop", "Query", "SELECT SLEEP(9)");
        let sleeping = p(8, "app", "shop", "Sleep", "");
        let daemon = p(9, "event_scheduler", "", "Daemon", "");
        let none = HashMap::new();
        // A locked connection offers neither kind of Kill.
        assert!(!can_kill(false, &none, &running, false) && !can_kill(false, &none, &running, true));
        assert!(can_kill(true, &none, &running, false) && can_kill(true, &none, &running, true));
        // A sleeping thread has no statement to stop but its connection can end; a daemon cannot.
        assert!(!can_kill(true, &none, &sleeping, false) && can_kill(true, &none, &sleeping, true));
        assert!(!can_kill(true, &none, &daemon, true));
        // Quarry's own session (a Query run, the shell) is stopped with Stop, never with Kill.
        let own: HashMap<u64, String> = [(7, "Query tab".to_string())].into();
        assert!(!can_kill(true, &own, &running, false) && !can_kill(true, &own, &running, true));
        assert!(can_kill(true, &own, &sleeping, true));
    }

    #[test]
    fn a_kill_answer_is_told_without_a_window_and_only_once() {
        let mut kills: HashSet<u64> = [5].into();
        let ok = Answer::Kill { whole: false, res: Ok("x".into()) };
        let bad = Answer::Kill { whole: true, res: Err("nope".into()) };
        assert_eq!(kill_toast(&mut kills, 5, &ok), Some(Ok("Stopped the statement".to_string())));
        assert!(kills.is_empty());
        // The same request answered again, or one that was never sent, says nothing.
        assert_eq!(kill_toast(&mut kills, 5, &ok), None);
        kills.insert(6);
        assert_eq!(kill_toast(&mut kills, 6, &bad), Some(Err("nope".to_string())));
        assert_eq!(kill_toast(&mut kills, 6, &Answer::Procs(Err("x".into()))), None);
    }

    #[test]
    fn filter_matches_user_database_or_query_and_hides_sleeping() {
        let rows = vec![p(1, "report", "analytics", "Query", "SELECT 1"), p(2, "app", "shop", "Sleep", ""), p(3, "app", "shop", "Query", "UPDATE payments SET a = 1")];
        assert_eq!(shown(&rows, "", true).iter().map(|r| r.id).collect::<Vec<_>>(), [1, 3]);
        assert_eq!(shown(&rows, "", false).len(), 3);
        assert_eq!(shown(&rows, "ANALYTICS", false).iter().map(|r| r.id).collect::<Vec<_>>(), [1]);
        assert_eq!(shown(&rows, "payments", false).iter().map(|r| r.id).collect::<Vec<_>>(), [3]);
        assert_eq!(shown(&rows, "app", true).iter().map(|r| r.id).collect::<Vec<_>>(), [3]);
    }

    #[test]
    fn status_values_and_rates_read_like_the_design() {
        assert_eq!(status_value("Questions", "1204881"), "1,204,881");
        assert_eq!(status_value("Bytes_received", "8697308774"), "8.1 GB");
        assert_eq!(status_value("Version", "8.4.3"), "8.4.3");
        assert_eq!(status_value("Threads_running", "3"), "3");
        assert_eq!(fmt_rate("Questions", 412.0), "412/s");
        assert_eq!(fmt_rate("Questions", 0.0), "0/s");
        assert_eq!(fmt_rate("Innodb_buffer_pool_reads", 0.4), "0.4/s");
        assert_eq!(fmt_rate("Questions", 12_345.6), "12,346/s");
        assert_eq!(fmt_rate("Bytes_received", 1_258_291.0), "1.2 MB/s");
        assert_eq!(thread_state("Yes").0, "Running");
        assert_eq!(thread_state("No").0, "Stopped");
    }

    fn procs(rows: Vec<Proc>, identity: &str) -> Processes {
        Processes { rows, me: 1, uptime: None, identity: identity.into() }
    }

    #[test]
    fn a_pending_kill_dies_with_any_change_of_its_target() {
        let c = Confirm { p: p(9, "app", "shop", "Query", "SELECT 1"), identity: "uuid-a".into(), whole: false, at: 0.0 };
        assert!(confirm_ok(&c, &procs(vec![p(9, "app", "shop", "Query", "SELECT 1")], "uuid-a")));
        // The id went to another statement, to Sleep, to another user, or is gone.
        for rows in [vec![p(9, "app", "shop", "Query", "SELECT 2")], vec![p(9, "app", "shop", "Sleep", "")], vec![p(9, "other", "shop", "Query", "SELECT 1")], vec![p(10, "app", "shop", "Query", "SELECT 1")]] {
            assert!(!confirm_ok(&c, &procs(rows, "uuid-a")));
        }
        // The same row on another server (a restart, a failover): ids repeat but nothing may be killed.
        assert!(!confirm_ok(&c, &procs(vec![p(9, "app", "shop", "Query", "SELECT 1")], "uuid-b")));
    }

    #[test]
    fn a_hidden_row_is_not_killable_through_the_toolbar() {
        let rows = vec![p(1, "report", "analytics", "Query", "SELECT 1"), p(2, "app", "shop", "Query", "SELECT 2")];
        let vis = shown(&rows, "analytics", false);
        assert!(vis.iter().any(|r| r.id == 1) && !vis.iter().any(|r| r.id == 2));
    }

    #[test]
    fn rates_never_span_a_failed_read() {
        let t0 = Instant::now();
        let now = t0 + Duration::from_secs(2);
        assert_eq!(prev_of(Some(Ok((t0, 5))), now).map(|(s, v)| (s.round(), v)), Some((2.0, 5)));
        assert!(prev_of::<i32>(Some(Err("gone".into())), now).is_none());
        assert!(prev_of::<i32>(None, now).is_none());
    }

    #[test]
    fn a_failed_read_is_retried_after_a_backoff() {
        assert_eq!(retry_after(true), Duration::from_secs(2));
        assert_eq!(retry_after(false), Duration::from_secs(10));
    }

    #[test]
    fn uptime_reads_in_the_largest_unit() {
        assert_eq!(up_text(41 * 86_400 + 5), "41 days");
        assert_eq!(up_text(86_400), "1 day");
        assert_eq!(up_text(7300), "2 hours");
        assert_eq!(up_text(90), "1 minute");
    }
}
