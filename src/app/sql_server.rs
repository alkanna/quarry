//! The Server window of a SQL connection: the process list with Kill and the variables. It follows the Mongo
//! window (`server.rs`): it asks only while it is open and Quarry is focused, never piles up requests, and keeps
//! errors in the window so a missing privilege is not announced every 2 seconds. View only: no SET anywhere.

use super::*;
use db::sql::server::{metrics, rate, Answer, Metrics, Overview, Proc, Processes, Replica, Snap, StatusGroup, Var, Variables};
use std::time::Instant;

const POLL: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, PartialEq, Default)]
pub(super) enum SqlServerTab {
    Overview,
    Variables,
    Status,
    #[default]
    Processes,
}

pub(super) struct SqlServerWin {
    pub(super) conn: u64,
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
    confirm: Option<(u64, bool)>,
    /// Kill requests in flight.
    kills: Vec<u64>,
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
    st_auto: bool,
    st_filter: String,
    st_group: StatusGroup,
}

impl SqlServerWin {
    pub(super) fn new(conn: u64, quarry: Vec<String>) -> Self {
        Self { conn, tab: SqlServerTab::default(), quarry, uptime: None, procs_req: 0, vars_req: 0, procs_wait: false, vars_wait: false, next: None, refresh: false, procs: None, auto: true, hide_sleeping: true, filter: String::new(), sel: None, confirm: None, kills: Vec::new(), global: false, var_filter: String::new(), vars: None, ov_req: 0, ov_wait: false, ov: None, ov_prev: None, st_req: 0, st_wait: false, st: None, st_prev: None, st_auto: true, st_filter: String::new(), st_group: StatusGroup::All }
    }

    /// Canned answers for the offline demo: `variables` opens that tab, anything else the processes.
    pub(super) fn demo(conn: u64, tab: &str) -> Self {
        let p = |id: u64, user: &str, host: &str, db: &str, command: &str, secs: f64, state: &str, info: &str| Proc { id, user: user.into(), host: host.into(), db: db.into(), command: command.into(), secs, state: state.into(), info: info.into() };
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
        let replica = Replica { source: "db-prod-0.internal:3306".into(), io: "Yes".into(), sql: "Yes".into(), behind: Some(0), gtid: "3e11fa47-71ca:1-88412".into(), error: String::new() };
        s.ov = Some(Ok((Instant::now(), Overview { snap: sample(0), vars: ovars, tls: "TLSv1.3, TLS_AES_256_GCM_SHA384".into(), replica: Ok(Some(replica)) })));
        s.ov_prev = Some((2.5, sample(1)));
        s.st = Some(Ok((Instant::now(), sample(0))));
        s.st_prev = Some((2.5, sample(1)));
        s.uptime = Some(41 * 86_400 + 3 * 3600);
        s.sel = Some(8812);
        s.auto = true;
        s.procs = Some(Ok(Processes { rows, me: 8901, uptime: s.uptime }));
        s.var_filter = "timeout".into();
        s.vars = Some(Ok(Variables { rows: vars, global: false, defaults: true, uptime: s.uptime }));
        s
    }
}

fn up_text(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs / 3600, secs / 60);
    let plural = |n: u64, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
    if d > 0 { plural(d, "day") } else if h > 0 { plural(h, "hour") } else { plural(m, "minute") }
}

/// The process list rows the filter and "Hide sleeping" leave: the filter matches user, database or query.
fn shown<'a>(rows: &'a [Proc], filter: &str, hide_sleeping: bool) -> Vec<&'a Proc> {
    let f = filter.trim().to_lowercase();
    rows.iter().filter(|p| !(hide_sleeping && p.sleeping())).filter(|p| f.is_empty() || [&p.user, &p.db, &p.info].iter().any(|t| t.to_lowercase().contains(&f))).collect()
}

/// Variable rows by name, for the session or global view.
fn shown_vars<'a>(rows: &'a [Var], filter: &str) -> Vec<&'a Var> {
    let f = filter.trim().to_lowercase();
    rows.iter().filter(|v| f.is_empty() || v.name.contains(&f)).collect()
}

impl App {
    pub(super) fn sql_server_window(&mut self, ctx: &egui::Context) {
        let Some(mut s) = self.sql_server.take() else { return };
        let conn = s.conn;
        let db = self.conn(conn).and_then(|c| c.sql.clone());
        let (name, product) = self.conn(conn).map(|c| (c.meta.name.clone(), format!("{} {}", c.server.product, c.server.version))).unwrap_or_default();
        if let (Some(db), false) = (&db, self.demo) {
            self.sql_server_poll(ctx, &mut s, db);
        }
        let mut open = true;
        let mut kill = None;
        let screen = ctx.content_rect();
        let r = dialog(egui::Id::unique(("quarry-layer", "sql-server-window"))).show(ctx, |ui| {
            ui.set_width((screen.width() * 0.7).clamp(700.0, 1100.0));
            dialog_title(ui, "Server");
            let up = s.uptime.map(|u| format!(" · up {}", up_text(u))).unwrap_or_default();
            ui.label(RichText::new(format!("{name} · {}{up}", product.trim())).size(TXT_SMALL).color(MUTED()));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 16.0;
                for (tab, label) in [(SqlServerTab::Overview, "Overview"), (SqlServerTab::Variables, "Variables"), (SqlServerTab::Status, "Status"), (SqlServerTab::Processes, "Processes")] {
                    if view_tab(ui, label, s.tab == tab).clicked() && s.tab != tab {
                        s.tab = tab;
                        // Ask for the new tab's data on the next frame instead of when the old tab's timer ends.
                        s.next = None;
                    }
                }
            });
            ui.add_space(8.0);
            let max = screen.height() - 300.0;
            match s.tab {
                SqlServerTab::Overview => overview_view(ui, &mut s),
                SqlServerTab::Status => status_view(ui, &mut s, max),
                SqlServerTab::Processes => procs_view(ui, &mut s, max, &mut kill),
                SqlServerTab::Variables => vars_view(ui, &mut s, max),
            }
            open = !done(ui);
        });
        if let (Some((id, whole)), Some(db)) = (kill, db) {
            self.req += 1;
            s.kills.push(self.req);
            self.worker.sql_server_kill(db, self.req, id, whole);
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
                // A failed read is not asked again until Refresh, so a missing privilege costs one call.
                let failed = matches!(s.procs, Some(Err(_)));
                let due = s.refresh || s.procs.is_none() || (s.auto && !failed && s.next.is_none_or(|t| now >= t));
                if s.procs_wait || !due {
                    return;
                }
                self.req += 1;
                (s.procs_req, s.procs_wait, s.refresh) = (self.req, true, false);
                self.worker.sql_server_processes(db.clone(), self.req);
            }
            SqlServerTab::Overview | SqlServerTab::Status => {
                let overview = s.tab == SqlServerTab::Overview;
                let (auto, failed, empty, wait) = if overview { (true, matches!(s.ov, Some(Err(_))), s.ov.is_none(), s.ov_wait) } else { (s.st_auto, matches!(s.st, Some(Err(_))), s.st.is_none(), s.st_wait) };
                if auto {
                    ctx.request_repaint_after(POLL);
                }
                let due = s.refresh || empty || (auto && !failed && s.next.is_none_or(|t| now >= t));
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

    pub(super) fn sql_server_done(&mut self, req: u64, ans: Answer) {
        let Some(s) = self.sql_server.as_mut() else { return };
        let mut told: Option<Result<String, String>> = None;
        match ans {
            Answer::Procs(r) if req == s.procs_req => {
                s.procs_wait = false;
                s.next = Some(Instant::now() + POLL);
                if let Ok(p) = &r {
                    s.uptime = p.uptime.or(s.uptime);
                    // The selection and a pending confirmation follow the thread by id; a thread that ended drops both.
                    let has = |id: u64| p.rows.iter().any(|x| x.id == id);
                    s.sel = s.sel.filter(|id| has(*id));
                    s.confirm = s.confirm.filter(|(id, _)| has(*id));
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
                s.next = Some(Instant::now() + POLL);
                let now = Instant::now();
                if let Some(Ok((at, old))) = s.ov.take() {
                    s.ov_prev = Some((now.duration_since(at).as_secs_f64(), old.snap));
                }
                if let Ok(o) = &r {
                    s.uptime = o.snap.num("Uptime").map(|u| u as u64).or(s.uptime);
                }
                s.ov = Some(r.map(|o| (now, o)));
            }
            Answer::Status(r) if req == s.st_req => {
                s.st_wait = false;
                s.next = Some(Instant::now() + POLL);
                let now = Instant::now();
                if let Some(Ok((at, old))) = s.st.take() {
                    s.st_prev = Some((now.duration_since(at).as_secs_f64(), old));
                }
                if let Ok(v) = &r {
                    s.uptime = v.num("Uptime").map(|u| u as u64).or(s.uptime);
                }
                s.st = Some(r.map(|v| (now, v)));
            }
            Answer::Kill { whole, res } if s.kills.contains(&req) => {
                s.kills.retain(|k| *k != req);
                if res.is_ok() {
                    s.refresh = true;
                    s.next = None;
                }
                told = Some(res.map(|_| if whole { "Ended the connection".to_string() } else { "Stopped the statement".to_string() }));
            }
            _ => {}
        }
        match told {
            Some(Ok(m)) => self.ok(m),
            Some(Err(e)) => self.err(e),
            None => {}
        }
    }
}

const ID_W: f32 = 64.0;
const COLS: [(&str, f32); 7] = [("Id", ID_W), ("User", 120.0), ("Host", 140.0), ("Db", 100.0), ("Command", 74.0), ("Time", 76.0), ("State", 150.0)];
const PAD: f32 = 10.0;
const KILL_W: f32 = 72.0;
const CONFIRM_W: f32 = 290.0;

/// One cell of a painted row: a single line cut with an ellipsis at the cell's width.
fn paint_cell(ui: &egui::Ui, row: egui::Rect, x: f32, w: f32, text: &str, color: Color32, mono: bool, right: bool) {
    let font = if mono { FontId::monospace(TXT_SMALL) } else { FontId::proportional(TXT_SMALL) };
    let mut job = LayoutJob::single_section(text.to_string(), egui::TextFormat { font_id: font, color, ..Default::default() });
    job.wrap = egui::text::TextWrapping { max_width: (w - PAD).max(1.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let g = ui.painter().layout_job(job);
    let gx = if right { x + w - PAD - g.size().x } else { x + PAD };
    ui.painter().galley(egui::pos2(gx, row.center().y - g.size().y / 2.0 + optical_dy(&g)), g, color);
}

fn header(ui: &mut egui::Ui, cols: &[(&str, f32, bool)]) {
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

fn procs_view(ui: &mut egui::Ui, s: &mut SqlServerWin, max_h: f32, kill: &mut Option<(u64, bool)>) {
    let (me, total, running, killable_sel) = match &s.procs {
        Some(Ok(p)) => (p.me, p.rows.len(), p.rows.iter().filter(|r| r.running()).count(), s.sel.and_then(|id| p.rows.iter().find(|r| r.id == id)).is_some_and(|r| r.id != p.me && r.running())),
        _ => (0, 0, 0, false),
    };
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut s.filter).hint_text("Filter by user, database or query").desired_width(240.0));
        ui.checkbox(&mut s.hide_sleeping, "Hide sleeping");
        ui.checkbox(&mut s.auto, "Refresh every 2 s");
        if !s.auto && quiet(ui, "Refresh").clicked() {
            s.refresh = true;
        }
        if total > 0 {
            ui.label(RichText::new(format!("{total} connections, {running} running")).size(TXT_SMALL).color(MUTED()));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let r = ui.add_enabled_ui(killable_sel, |ui| danger(ui, "Kill query")).inner;
            if r.clicked() {
                s.confirm = s.sel.map(|id| (id, false));
            }
        });
    });
    ui.add_space(8.0);
    let rows = match &s.procs {
        None => return spin(ui),
        Some(Err(e)) => {
            ui.label(RichText::new(format!("Could not list the processes: {e}")).color(ALERT()));
            return;
        }
        Some(Ok(p)) => shown(&p.rows, &s.filter, s.hide_sleeping),
    };
    let w = ui.available_width();
    let info_w = (w - COLS.iter().map(|c| c.1).sum::<f32>()).max(120.0);
    let mut cols: Vec<(&str, f32, bool)> = COLS.iter().map(|(l, w)| (*l, *w, *l == "Time")).collect();
    cols.push(("Info", info_w, false));
    header(ui, &cols);
    if rows.is_empty() {
        ui.add_space(8.0);
        ui.label(RichText::new(if s.filter.trim().is_empty() { "Nothing is running." } else { "No process matches." }).color(MUTED()));
        return;
    }
    let mut sel = s.sel;
    let mut confirm = s.confirm;
    egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for (i, p) in rows.iter().enumerate() {
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::click());
            let selected = sel == Some(p.id);
            let fill = match (selected, state_of(&resp)) {
                (true, 0) => SEL(),
                (true, _) => SEL_HOVER(),
                (false, 0) => if i % 2 == 1 { PANEL() } else { BG() },
                (false, st) => hover_fill(st),
            };
            ui.painter().rect_filled(rect, 0.0, fill);
            let mine = p.id == me;
            let mut x = rect.left();
            let slow = p.running() && p.secs >= 60.0;
            let cells: [(String, Color32, bool, bool); 7] = [
                (p.id.to_string(), if p.daemon() { C_NUMBER().gamma_multiply(0.8) } else { C_NUMBER() }, true, true),
                (p.user.clone(), if p.daemon() { MUTED() } else { BRIGHT() }, true, false),
                (if mine { "quarry (you)".into() } else { p.host.clone() }, if mine { PRIMARY() } else { MUTED() }, true, false),
                (p.db.clone(), TEXT(), true, false),
                (p.command.clone(), if p.running() { WARNING() } else if p.sleeping() { FAINT() } else { MUTED() }, true, false),
                (format!("{} s", group(&(p.secs as i64).to_string())), if slow { ALERT() } else { BRIGHT() }, true, false),
                (p.state.clone(), if slow { WARNING() } else { TEXT() }, false, false),
            ];
            for ((_, w), (text, color, mono, right)) in COLS.iter().zip(&cells) {
                paint_cell(ui, rect, x, *w, text, *color, *mono, *right);
                x += w;
            }
            let info = p.info.split_whitespace().collect::<Vec<_>>().join(" ");
            let confirming = confirm.is_some_and(|(id, _)| id == p.id);
            let info_x = rect.left() + COLS.iter().map(|c| c.1).sum::<f32>();
            paint_cell(ui, rect, info_x, info_w - if confirming { CONFIRM_W } else { KILL_W }, &info, MUTED(), true, false);
            if resp.clicked() || resp.secondary_clicked() {
                sel = Some(p.id);
            }
            if resp.hovered() && !confirming && !p.info.is_empty() && resp.hover_pos().is_some_and(|h| h.x >= info_x) {
                let long: String = p.info.chars().take(1200).collect();
                resp.clone().on_hover_text(long);
            }
            let can_query = p.running() && !mine;
            let can_end = !mine && !p.daemon();
            resp.context_menu(|ui| {
                menu_look(ui);
                if ui.add_enabled(can_query, egui::Button::new("Kill query").frame_when_inactive(false)).clicked() {
                    confirm = Some((p.id, false));
                    ui.close();
                }
                if ui.add_enabled(can_end, egui::Button::new("Kill connection").frame_when_inactive(false)).clicked() {
                    confirm = Some((p.id, true));
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
                match confirm.filter(|(id, _)| *id == p.id) {
                    Some((_, whole)) => {
                        if k.button("Keep").clicked() {
                            confirm = None;
                        }
                        if danger(&mut k, if whole { "Kill connection" } else { "Kill it" }).clicked() {
                            *kill = Some((p.id, whole));
                            confirm = None;
                        }
                        k.label(RichText::new(if whole { "End the connection?" } else { "Stop the statement?" }).size(TXT_SMALL).color(MUTED()));
                    }
                    None => {
                        if danger(&mut k, "Kill").clicked() {
                            confirm = Some((p.id, false));
                            sel = Some(p.id);
                        }
                    }
                }
            }
        }
    });
    s.sel = sel;
    s.confirm = confirm;
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
            ui.label(RichText::new(format!("Could not read the variables: {e}")).color(ALERT()));
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

fn tile(ui: &mut egui::Ui, rect: egui::Rect, title: &str, big: &str, suffix: &str, sub: &str) {
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
fn kv_rows(ui: &mut egui::Ui, w: f32, rows: &[(&str, String, Color32)]) {
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
            ui.label(RichText::new(format!("Could not read the server status: {e}")).color(ALERT()));
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
            Ok(None) => {
                ui.label(RichText::new("Not a replica").color(MUTED()));
            }
            Ok(Some(r)) => {
                let w = ui.available_width();
                ui.spacing_mut().item_spacing.y = 0.0;
                let cw = [w * 0.4, w * 0.2, w * 0.2, w * 0.2];
                header(ui, &[("Source", cw[0], false), ("IO thread", cw[1], false), ("SQL thread", cw[2], false), ("Behind", cw[3], true)]);
                let (rect, _) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::hover());
                ui.painter().rect_filled(rect, 0.0, BG());
                let (io, io_c) = thread_state(&r.io);
                let (sql, sql_c) = thread_state(&r.sql);
                paint_cell(ui, rect, rect.left(), cw[0], &r.source, BRIGHT(), true, false);
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
            ui.label(RichText::new(format!("Could not read the server status: {e}")).color(ALERT()));
            return;
        }
        Some(Ok((_, v))) => v,
    };
    let f = s.st_filter.trim().to_lowercase();
    let rows: Vec<&(String, String)> = snap.vals.iter().filter(|(n, _)| s.st_group.has(n) && (f.is_empty() || n.to_lowercase().contains(&f))).collect();
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
        Proc { id, user: user.into(), host: String::new(), db: db.into(), command: command.into(), secs: 0.0, state: String::new(), info: info.into() }
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

    #[test]
    fn uptime_reads_in_the_largest_unit() {
        assert_eq!(up_text(41 * 86_400 + 5), "41 days");
        assert_eq!(up_text(86_400), "1 day");
        assert_eq!(up_text(7300), "2 hours");
        assert_eq!(up_text(90), "1 minute");
    }
}
