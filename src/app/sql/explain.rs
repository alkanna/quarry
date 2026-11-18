//! The explain dialog of a SQL statement: what is worth fixing at the top, the plan as a tree on the left and the
//! selected node on the right, the raw plan, and an optional ANALYZE run that shows what the server actually did. The
//! tree and the node panel are drawn in `explain_tree` and `explain_panel`; the reading of the plan is in `db::sql::plan`.

use super::explain_compare::{clock, compare_view, Prev};
use super::explain_flame::flame_view;
use super::explain_hypo::HypoRun;
use super::explain_panel::{findings_strip, inspector};
use super::explain_tree::plan_tree;
use super::query::{run_db, run_target};
use crate::app::*;
use crate::db::sql::plan::{self, compare, Analyze, Plan};
use crate::db::sql::{Db, Dialect};

/// The state of one open SQL explain dialog.
pub(in crate::app) struct SqlExplain {
    /// The id of the request in flight, so an old answer never fills a newer dialog.
    req: u64,
    busy: bool,
    pub(super) stmt: String,
    conn: u64,
    /// The tab's database, for the Query tab the classic rows open in.
    tab_db: String,
    /// The database the plan was made in: the run picker's when one is picked, else the tab's.
    database: String,
    pub(super) dialect: Dialect,
    pub(super) db: Db,
    pub(in crate::app) schema: String,
    pub(in crate::app) plan: Option<Plan>,
    /// Why the plan could not be drawn, when the server's text could not be read.
    unread: Option<String>,
    raw: String,
    /// The head tab: the tree or the raw text.
    pub(super) raw_tab: bool,
    /// The node the panel shows, and one the tree should scroll to.
    pub(in crate::app) sel: Option<usize>,
    pub(super) scroll_to: Option<usize>,
    /// The findings strip shows all of them, not the first two.
    pub(super) more: bool,
    analyzed: bool,
    /// The statement ran inside a transaction that was rolled back.
    rolled_back: bool,
    /// How ANALYZE may run this statement, or why it may not.
    analyze: Result<Analyze, String>,
    /// What the request in flight asked for.
    asked: Option<Analyze>,
    /// The handle Stop and closing the dialog use to end the run on the server.
    run: db::sql::RunHandle,
    /// Stop was pressed, so the answer that follows is no error to show.
    stopped: bool,
    /// What the head shows when Raw is not chosen.
    pub(in crate::app) pane: Pane,
    /// The flame chart is as wide as each node's time, not its cost.
    pub(in crate::app) flame_time: bool,
    /// The last plan of this statement this session, when it differs from this one.
    pub(in crate::app) prev: Option<Prev>,
    /// The catalog's evidence for the index suggestions, kept across an ANALYZE rerun of the same statement, and the
    /// request reading it, apart from `req` so a rerun does not drop it.
    pub(super) ev: Option<plan::Evidence>,
    ev_req: u64,
    pub(super) ev_busy: bool,
    /// The offline demo and the tests: made-up evidence instead of a catalog read.
    pub(super) demo: bool,
    /// The connection is locked, as of the last frame drawn.
    pub(super) locked: bool,
    /// The HypoPG trial of a candidate index: the request in flight and the candidate it tries, its answer, and whether
    /// Compare shows the trial's plan instead of the last one. Apart from `req`, like `ev_req`.
    pub(super) hypo_req: u64,
    pub(super) hypo_busy: bool,
    pub(super) hypo_fix: String,
    pub(in crate::app) hypo: Option<HypoRun>,
    pub(in crate::app) hypo_cmp: bool,
}

/// The views of a readable plan.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::app) enum Pane {
    Tree,
    Flame,
    Compare,
}

/// What a click in the dialog asks for, done once the frame is drawn.
pub(super) enum XAct {
    Select(usize),
    Copy(String),
    /// A fix as an editable draft in a new Query tab. Nothing runs.
    Draft(String),
    /// Try this candidate index with HypoPG.
    Hypo(plan::findings::Fix),
    /// Show the trial's plan in Compare.
    HypoCompare,
}

impl SqlExplain {
    #[allow(clippy::too_many_arguments)]
    fn new(stmt: String, conn: u64, tab_db: String, database: String, dialect: Dialect, db: Db, schema: String, analyze: Result<Analyze, String>) -> Self {
        SqlExplain { req: 0, busy: false, stmt, conn, tab_db, database, dialect, db, schema, plan: None, unread: None, raw: String::new(), raw_tab: false, sel: None, scroll_to: None, more: false, analyzed: false, rolled_back: false, analyze, asked: None, run: Default::default(), stopped: false, pane: Pane::Tree, flame_time: false, prev: None, ev: None, ev_req: 0, ev_busy: false, demo: false, locked: true, hypo_req: 0, hypo_busy: false, hypo_fix: String::new(), hypo: None, hypo_cmp: false }
    }
}

impl App {
    /// Shows the plan of `text` in a dialog. `text` is the statement under the caret of a Query view or the last page's
    /// SELECT of a Content grid.
    pub(in crate::app) fn sql_explain(&mut self, text: String) {
        let Some(t) = self.t() else { return };
        let conn = t.sel.conn;
        let d = self.dialect_of(conn);
        if text.trim().is_empty() {
            return self.warn("Nothing to explain.");
        }
        if let Err(e) = plan::explainable(&text, d) {
            return self.warn(e);
        }
        let Some(db) = self.sql_db_for(conn, Some("Connect first.")) else { return };
        let t = &self.tabs[self.cur];
        let (tab_db, database) = (t.sel.db.clone(), run_db(t));
        let (db, schema) = run_target(t, db);
        let writable = self.conn_writable(conn);
        let analyze = plan::analyze_needs(d, &db.version).map(str::to_string).map_or_else(|| plan::analyze_mode(&text, d, db.pooler()), Err).and_then(|a| match (d, writable) {
            // The read-only guard refuses ANALYZE on these engines, so it needs the unlock the user would otherwise be asked for.
            (Dialect::Postgres, false) => Err("ANALYZE on PostgreSQL runs the statement inside a transaction it rolls back, which needs writes unlocked on this connection.".into()),
            (Dialect::MariaDb, false) => Err("ANALYZE on MariaDB is a statement the read-only guard refuses. Unlock writes on this connection to use it.".into()),
            _ => Ok(a),
        });
        let mut x = SqlExplain::new(text, conn, tab_db, database, d, db, schema, analyze);
        self.sql_explain_send(&mut x, None);
        self.modal = Some(Modal::SqlExplain(Box::new(x)));
    }

    /// The offline demo's dialog: ANALYZE would send the statement to a server that is not there, so it is turned off.
    pub(in crate::app) fn demo_explain_offline(&mut self, d: Dialect, text: &str, plan: &str) {
        self.demo_explain(d, text, plan);
        if let Some(Modal::SqlExplain(x)) = &mut self.modal {
            x.analyze = Err("The offline demo has no server to run the statement on.".into());
        }
    }

    /// Opens the dialog on `plan` as if the server had answered, over a pool that never connects: the offline demo and
    /// the tests have no server for `sql_explain` to ask.
    pub(in crate::app) fn demo_explain(&mut self, d: Dialect, text: &str, plan: &str) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
        let _guard = rt.enter();
        let opts = "mysql://root@127.0.0.1:1/shop".parse::<sqlx::mysql::MySqlConnectOptions>().expect("demo address");
        // The pool is never used; the dialect and version are what the dialog reads.
        let version = if d == Dialect::Postgres { "18.6" } else if d == Dialect::MariaDb { "11.4.3-MariaDB" } else { "8.4.3" };
        let db = Db { pool: crate::db::sql::Pool::My(sqlx::mysql::MySqlPoolOptions::new().connect_lazy_with(opts.clone())), dialect: d, opts: crate::db::sql::exec::Opts::My(opts), version: version.into(), tls: false, sql_mode: String::new(), setup: Default::default(), conn: 0 };
        let (conn, tab_db, database, schema) = self.t().map_or(Default::default(), |t| (t.sel.conn, t.sel.db.clone(), run_db(t), if d == Dialect::Postgres { t.sel.schema.clone() } else { t.sel.db.clone() }));
        let analyze = plan::analyze_mode(text, d, false);
        let mut x = SqlExplain::new(text.into(), conn, tab_db, database, d, db, schema, analyze);
        (x.req, x.busy, x.demo) = (1, true, true);
        self.modal = Some(Modal::SqlExplain(Box::new(x)));
        let col = db::sql::Col::plain("EXPLAIN", "TEXT", db::sql::value::Kind::Text, false);
        let set = db::sql::ResultSet { cols: vec![col], rows: db::Row::from_doc(&mongodb::bson::doc! { "EXPLAIN": plan }).into_iter().collect(), ..db::sql::ResultSet::new(text) };
        self.sql_explained(1, vec![set]);
    }

    fn sql_explain_send(&mut self, x: &mut SqlExplain, analyze: Option<Analyze>) {
        self.req += 1;
        // A fresh handle per run, so a Stop for an old run never reaches this one.
        (x.req, x.busy, x.asked, x.run, x.stopped) = (self.req, true, analyze, Default::default(), false);
        let script = plan::explain_script(x.dialect, &x.stmt, analyze);
        self.worker.sql_explain(self.req, x.db.clone(), x.schema.clone(), script, !self.conn_writable(x.conn), x.run.clone());
    }

    /// Ends the run in flight on the server, which also rolls back an ANALYZE of a write and frees its locks.
    pub(in crate::app) fn sql_explain_stop(&mut self, x: &mut SqlExplain) {
        if x.busy {
            x.stopped = true;
            self.worker.sql_kill(x.db.clone(), x.run.conn.load(std::sync::atomic::Ordering::SeqCst), x.run.cancel.clone());
        }
    }

    /// The answer of a request of the open dialog.
    pub(in crate::app) fn sql_explained(&mut self, req: u64, sets: Vec<db::sql::ResultSet>) {
        let Some(Modal::SqlExplain(x)) = &mut self.modal else { return };
        if x.req != req || !x.busy {
            return;
        }
        x.busy = false;
        let text = match plan::plan_text(&sets) {
            Ok(t) => t,
            Err(e) => {
                if x.stopped {
                    if x.plan.is_none() && x.unread.is_none() {
                        self.modal = None;
                    }
                    return;
                }
                // A failed first read leaves nothing to show; a failed ANALYZE keeps the plan it already had.
                if x.plan.is_none() && x.unread.is_none() {
                    self.modal = None;
                }
                return self.err(e);
            }
        };
        x.raw = plan::raw_text(&text);
        x.analyzed = x.asked.is_some();
        x.rolled_back = x.asked == Some(Analyze::Rollback) && !crate::db::sql::lex::pageable(&x.stmt, x.dialect);
        match plan::parse(x.dialect, &text) {
            Ok(mut p) => {
                let ready = if x.demo {
                    x.ev = Some(plan::Evidence::demo(&p, x.dialect));
                    true
                } else {
                    let wants = plan::evidence::wants(&p, &x.stmt, x.dialect);
                    // A rerun of the same statement scans the same tables, so the evidence already read still holds.
                    let held = !wants.is_empty() && x.ev.as_ref().is_some_and(|e| e.covers(&wants));
                    if !wants.is_empty() && !held {
                        self.req += 1;
                        (x.ev_req, x.ev_busy) = (self.req, true);
                        self.worker.sql_explain_evidence(self.req, x.db.clone(), x.schema.clone(), wants);
                    }
                    held
                };
                if ready {
                    p.refind(x.dialect, x.ev.as_ref(), x.db.pooler());
                }
                // The panel opens on what matters most: the node of the top finding, else the one that costs the most.
                (x.hypo, x.hypo_busy, x.hypo_cmp) = (None, false, false);
                x.sel = p.findings.iter().find_map(|f| f.node).or_else(|| (0..p.nodes.len()).max_by(|&a, &b| p.nodes[a].share.unwrap_or(0.0).total_cmp(&p.nodes[b].share.unwrap_or(0.0))));
                x.scroll_to = x.sel;
                // The last plan of the same statement, kept for the session, is what Compare shows when this one differs.
                let now = crate::library::now_secs();
                x.prev = self.explain_prev.swap(history_key(x, p.analyzed), p.clone(), now).map(|(plan, at)| Prev { diff: compare::diff(&plan, &p), plan, at }).filter(|v| v.diff.differs);
                if x.prev.is_none() && x.pane == Pane::Compare {
                    x.pane = Pane::Tree;
                }
                (x.plan, x.unread) = (Some(p), None);
            }
            // The raw text is still worth reading.
            Err(e) => (x.plan, x.unread, x.raw_tab) = (None, Some(e), true),
        }
    }

    /// The catalog answered: the findings are worked out again with it, and the selection and scroll stay where they are.
    pub(in crate::app) fn sql_explain_evidence(&mut self, req: u64, ev: plan::Evidence) {
        let Some(Modal::SqlExplain(x)) = &mut self.modal else { return };
        if x.ev_req != req || !x.ev_busy {
            return;
        }
        x.ev_busy = false;
        let pooler = x.db.pooler();
        let key = history_key(x, x.plan.as_ref().is_some_and(|p| p.analyzed));
        if let Some(p) = &mut x.plan {
            p.refind(x.dialect, Some(&ev), pooler);
            // Each plan is compared with the findings it showed: the copy kept for the next Compare and this Compare's
            // new side follow the strip. `differs` reads no findings, so the Compare tab stays as it was.
            self.explain_prev.update(&key, p);
            if let Some(prev) = &mut x.prev {
                prev.diff = compare::diff(&prev.plan, p);
            }
        }
        x.ev = Some(ev);
    }

    /// The catalog's answer as if the server had sent it, for the open dialog.
    #[cfg(test)]
    pub(in crate::app) fn demo_explain_evidence(&mut self, ev: plan::Evidence) {
        let Some(Modal::SqlExplain(x)) = &mut self.modal else { return };
        self.req += 1;
        (x.ev_req, x.ev_busy) = (self.req, true);
        let req = self.req;
        self.sql_explain_evidence(req, ev);
    }

    pub(in crate::app) fn sql_explain_dialog(&mut self, ui: &mut egui::Ui, x: &mut SqlExplain, keep: &mut bool) {
        let ctx = ui.ctx().clone();
        let screen = ctx.content_rect();
        ui.set_width(fit_width(&ctx, (screen.width() * 0.85).clamp(760.0, 1160.0)));
        x.locked = !self.conn_writable(x.conn);
        let mut names = vec!["Tree"];
        if x.prev.is_some() || x.hypo_cmp {
            names.push("Compare");
        }
        names.extend(["Flame", "Raw"]);
        let mut head = Head::new(if x.analyzed { "Explain analyze" } else { "Explain" }).sub(head_line(x)).glyph(glyphs::list);
        let shown = if x.raw_tab { "Raw" } else { pane_name(x.pane) };
        if x.plan.is_some() {
            head = head.tabs(&names, names.iter().position(|n| *n == shown).unwrap_or(0));
        }
        if let Some(t) = dialog_head(ui, &head) {
            x.raw_tab = names[t] == "Raw";
            // Compare goes back to the last plan once left; Show in Compare brings the trial back.
            x.hypo_cmp &= names[t] == "Compare";
            x.pane = match names[t] {
                "Flame" => Pane::Flame,
                "Compare" => Pane::Compare,
                "Raw" => x.pane,
                _ => Pane::Tree,
            };
        }
        let mut acts = Vec::new();
        if x.raw_tab || x.plan.is_none() {
            let mut b = body_begin(ui);
            {
                let ui = &mut b.content_ui;
                status_lines(ui, x);
                let find = Find::bar(ui, egui::Id::unique("sql-explain-raw"), &x.raw);
                egui::ScrollArea::vertical().max_height(body_room(ui, 90.0)).auto_shrink([false, true]).show(ui, |ui| {
                    let mut text = x.raw.as_str();
                    let out = egui::TextEdit::multiline(&mut text).code_editor().desired_width(f32::INFINITY).layouter(&mut find.layouter(true)).show(ui);
                    find.reveal(ui, &x.raw, &out);
                });
            }
            b.end(ui);
        } else if x.pane == Pane::Compare && (x.prev.is_some() || x.hypo_cmp) {
            dialog_flush(ui, |ui| {
                egui::Frame::new().inner_margin(egui::Margin { left: 20, right: 20, top: 16, bottom: 16 }).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    status_lines(ui, x);
                    compare_view(ui, x, screen.height(), &mut acts);
                });
            });
        } else {
            dialog_flush(ui, |ui| {
                egui::Frame::new().inner_margin(egui::Margin { left: 20, right: 20, top: 16, bottom: 0 }).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    status_lines(ui, x);
                    findings_strip(ui, x, &mut acts);
                });
                let body_h = (screen.height() - 330.0).clamp(260.0, 640.0).min(body_room(ui, 74.0));
                let w = ui.available_width();
                let panel_w = (w * 0.4).clamp(300.0, 460.0);
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    // Frames in a row inherit its left to right layout, so each pane lays out top down itself.
                    egui::Frame::new().inner_margin(egui::Margin { left: 20, right: 8, top: 12, bottom: 16 }).show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.set_width(w - panel_w - 28.0 - 20.0);
                            if x.pane == Pane::Flame {
                                flame_view(ui, x, body_h, &mut acts);
                            } else {
                                plan_tree(ui, x, body_h, &mut acts);
                            }
                        });
                    });
                    egui::Frame::new().inner_margin(egui::Margin { left: 8, right: 20, top: 12, bottom: 16 }).show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.set_width(panel_w - 8.0);
                            egui::ScrollArea::vertical().id_salt("sql-explain-node").max_height(body_h).auto_shrink([false, true]).show(ui, |ui| {
                                inspector(ui, x, &mut acts);
                            });
                        });
                    });
                });
            });
        }
        let mut rows = false;
        let mut again = None;
        let mut stop = false;
        dialog_foot(ui, |ui| {
            match &x.analyze {
                Ok(a) => {
                    let label = if x.analyzed { "Run again" } else { "Run with ANALYZE" };
                    let r = ui.add_enabled(!x.busy, primary_button(ui, label, PRIMARY()));
                    let tip = if *a == Analyze::Rollback { "Runs the statement for real inside a transaction that is rolled back" } else { "Runs the statement for real to count rows and time" };
                    if r.on_hover_text(tip).clicked() {
                        again = Some(*a);
                    }
                }
                Err(why) => {
                    ui.add_enabled(false, egui::Button::new("Run with ANALYZE")).on_disabled_hover_text(why.as_str());
                }
            }
            if x.busy {
                stop = ui.button("Stop").on_hover_text("End the statement on the server").clicked();
            }
            if ui.button("Close").on_hover_text("Esc").clicked() {
                *keep = false;
            }
            if ui.button("Copy raw").clicked() {
                acts.push(XAct::Copy(x.raw.clone()));
            }
            rows = ui.button("Show rows in Query tab").on_hover_text("Run the classic EXPLAIN and show the server's rows").clicked();
            let note = match (&x.prev, x.pane) {
                _ if x.raw_tab || x.plan.is_none() => None,
                (_, Pane::Compare) if x.hypo_cmp => Some("Compared with the plan the server would choose with the hypothetical index. Nothing was created.".to_string()),
                (Some(prev), Pane::Compare) => Some(format!("Compared with the plan from {}.", clock(prev.at))),
                (_, Pane::Flame) => Some("Each bar is as wide as its share of the plan. Click one to see it.".to_string()),
                _ => None,
            };
            foot_note(ui, note.as_deref().unwrap_or(if x.analyzed { "Ran the statement. Times are each node's own, without the nodes below it." } else { "Estimated by the planner. Nothing ran." }));
        });
        for a in acts {
            match a {
                XAct::Select(i) => (x.sel, x.scroll_to) = (Some(i), Some(i)),
                XAct::Copy(text) => {
                    ctx.copy_text(text);
                    self.toast_ok("Copied.".into(), None);
                }
                XAct::Hypo(fix) => self.sql_explain_hypo_try(x, fix),
                XAct::HypoCompare => self.sql_explain_hypo_compare(x),
                XAct::Draft(sql) => {
                    *keep = false;
                    self.sql_explain_stop(x);
                    self.sql_new_query(x.conn, x.tab_db.clone());
                    if let Some(s) = self.tabs[self.cur].sql.as_mut() {
                        s.query = sql;
                        // The draft runs where the plan was made, not where the sidebar happens to be.
                        if x.dialect == Dialect::Postgres {
                            s.run_schema = x.schema.clone();
                            // Left empty for the tab's own database, as the picker leaves it.
                            if x.database != x.tab_db {
                                s.run_db = x.database.clone();
                            }
                        } else {
                            s.run_db = x.schema.clone();
                        }
                    }
                    return;
                }
            }
        }
        if stop {
            self.sql_explain_stop(x);
        }
        if let Some(a) = again {
            self.sql_explain_send(x, Some(a));
        }
        if rows {
            *keep = false;
            self.sql_explain_stop(x);
            self.sql_explain_rows(x);
        }
    }

    /// The classic EXPLAIN rows, in a Query tab of their own.
    fn sql_explain_rows(&mut self, x: &SqlExplain) {
        self.sql_new_query(x.conn, x.tab_db.clone());
        if let Some(s) = self.tabs[self.cur].sql.as_mut() {
            s.query = format!("EXPLAIN {}", x.stmt.trim().trim_end_matches(';'));
        }
        self.sql_run(true, false);
    }
}

/// Where this session keeps the last plan of the dialog's statement: two databases of one connection are two places.
fn history_key(x: &SqlExplain, analyzed: bool) -> compare::Key {
    compare::Key { conn: x.conn, db: x.database.clone(), schema: x.schema.clone(), stmt: compare::normalize(&x.stmt, x.dialect), analyzed }
}

fn pane_name(p: Pane) -> &'static str {
    match p {
        Pane::Tree => "Tree",
        Pane::Flame => "Flame",
        Pane::Compare => "Compare",
    }
}

/// The line under the title: the engine, then the size of the plan or what the run took.
fn head_line(x: &SqlExplain) -> String {
    let mut sub = x.db.label();
    let Some(p) = &x.plan else { return sub };
    let root = p.nodes.first();
    if p.analyzed {
        if let Some(ms) = p.exec_ms {
            sub = format!("{sub}  ·  ran in {} ms", plan::findings::ms_text(ms));
        }
        if let Some(ms) = p.planning_ms {
            sub = format!("{sub}  ·  planned in {} ms", plan::findings::ms_text(ms));
        }
        if let Some(r) = root.and_then(|n| n.actual_rows) {
            sub = format!("{sub}  ·  {} rows", group(&format!("{r:.0}")));
        }
    } else {
        sub = format!("{sub}  ·  {} node{}", p.nodes.len(), if p.nodes.len() == 1 { "" } else { "s" });
        if let Some(c) = p.cost {
            sub = format!("{sub}  ·  cost {}", group(&format!("{c:.0}")));
            if let Some(was) = x.prev.as_ref().and_then(|v| v.plan.cost).filter(|w| (w - c).abs() >= 0.5) {
                sub = format!("{sub}, was {}", group(&format!("{was:.0}")));
            }
        }
        if let Some(r) = root.and_then(|n| n.rows) {
            sub = format!("{sub}  ·  est. {} rows", group(&format!("{r:.0}")));
        }
    }
    sub
}

/// The run in flight and what a finished one left to say, above the plan.
fn status_lines(ui: &mut egui::Ui, x: &SqlExplain) {
    if x.busy {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new(if x.asked.is_some() { "Running the statement under ANALYZE" } else { "Reading the plan" }).color(MUTED()));
        });
    }
    if x.rolled_back {
        banner(ui, Notice::Info, "Rolled back.", "ANALYZE ran this statement inside a transaction and rolled it back. Nothing was kept.", None);
    }
    if let Some(e) = &x.unread {
        banner(ui, Notice::Warn, "Plan not readable.", e, None);
    }
}
