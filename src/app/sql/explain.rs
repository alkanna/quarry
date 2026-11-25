//! The explain dialog of a SQL statement: what is worth fixing at the top, the plan as a tree on the left and the
//! selected node on the right, the raw plan, and an optional ANALYZE run that shows what the server actually did. The
//! tree and the node panel are drawn in `explain_tree` and `explain_panel`; the reading of the plan is in `db::sql::plan`.

use super::explain_compare::compare_view;
use super::explain_flame::flame_view;
use super::explain_panel::{findings_strip, inspector};
use super::explain_tree::plan_tree;
use super::query::run_db;
use crate::app::*;
use crate::db::sql::plan::{self, Analyze};
use crate::db::sql::{Db, Dialect};
pub(crate) use quarry_core::state::sql::explain::*;

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

impl App {
    /// Shows the plan of `text` in a dialog. `text` is the statement under the caret of a Query view or the last page's
    /// SELECT of a Content grid.
    pub(in crate::app) fn sql_explain(&mut self, text: String) {
        if let Some(x) = self.core.sql_explain_new(text) {
            self.modal = Some(Modal::SqlExplain(Box::new(x)));
        }
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

    /// The answer of a request of the open dialog.
    pub(in crate::app) fn sql_explained(&mut self, req: u64, sets: Vec<db::sql::ResultSet>) {
        let Some(Modal::SqlExplain(x)) = &mut self.modal else { return };
        if !self.core.sql_explained(x, req, sets) {
            self.modal = None;
        }
    }

    /// The catalog answered: the findings are worked out again with it, and the selection and scroll stay where they are.
    pub(in crate::app) fn sql_explain_evidence(&mut self, req: u64, ev: plan::Evidence) {
        if let Some(Modal::SqlExplain(x)) = &mut self.modal {
            self.core.sql_explain_evidence(x, req, ev);
        }
    }

    /// The catalog's answer as if the server had sent it, for the open dialog.
    #[cfg(test)]
    pub(in crate::app) fn demo_explain_evidence(&mut self, ev: plan::Evidence) {
        let Some(Modal::SqlExplain(x)) = &mut self.modal else { return };
        self.core.req += 1;
        x.ev_req = Some(self.core.req);
        let req = self.core.req;
        self.sql_explain_evidence(req, ev);
    }

    pub(in crate::app) fn sql_explain_dialog(&mut self, ui: &mut egui::Ui, x: &mut SqlExplain, keep: &mut bool) {
        let ctx = ui.ctx().clone();
        let screen = ctx.content_rect();
        ui.set_width(fit_width(&ctx, (screen.width() * 0.85).clamp(760.0, 1160.0)));
        x.locked = !self.conn_writable(x.conn);
        if x.dialect == Dialect::Postgres && x.hypo_refused.as_ref().is_none_or(|(locked, _)| *locked != x.locked) {
            x.hypo_refused = Some((x.locked, crate::db::sql::pg::hypo::refused(&x.stmt, x.locked)));
        }
        let mut panes = vec![Pane::Tree];
        if x.prev.is_some() || x.hypo_cmp {
            panes.push(Pane::Compare);
        }
        panes.extend([Pane::Flame, Pane::Raw]);
        let names: Vec<&str> = panes.iter().map(|p| p.name()).collect();
        let mut head = Head::new(if x.analyzed { "Explain analyze" } else { "Explain" }).sub(head_line(x)).glyph(glyphs::list);
        if x.plan.is_some() {
            head = head.tabs(&names, panes.iter().position(|p| *p == x.pane).unwrap_or(0));
        }
        if let Some(t) = dialog_head(ui, &head) {
            x.pane = panes[t];
            // Compare goes back to the last plan once left; Show in Compare brings the trial back.
            x.hypo_cmp &= x.pane == Pane::Compare;
        }
        let mut acts = Vec::new();
        if x.pane == Pane::Raw || x.plan.is_none() {
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
                _ if x.plan.is_none() => None,
                (_, Pane::Compare) if x.hypo_cmp => Some("Compared with the plan the server would choose with the hypothetical index. Nothing was created.".to_string()),
                (Some(prev), Pane::Compare) => Some(format!("Compared with the plan from {}.", &crate::db::dates::local_hms(prev.at)[..5])),
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
                XAct::Hypo(fix) => self.core.sql_explain_hypo_try(x, fix),
                XAct::HypoCompare => self.sql_explain_hypo_compare(x),
                XAct::Draft(sql) => {
                    *keep = false;
                    self.core.sql_explain_stop(x);
                    self.sql_new_query(x.conn, x.tab_db.clone());
                    if let Some(s) = self.core.tabs[self.core.cur].sql.as_mut() {
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
            self.core.sql_explain_stop(x);
        }
        if let Some(a) = again {
            self.core.sql_explain_send(x, Some(a));
        }
        if rows {
            *keep = false;
            self.core.sql_explain_stop(x);
            self.sql_explain_rows(x);
        }
    }

    /// The classic EXPLAIN rows, in a Query tab of their own.
    fn sql_explain_rows(&mut self, x: &SqlExplain) {
        self.sql_new_query(x.conn, x.tab_db.clone());
        if let Some(s) = self.core.tabs[self.core.cur].sql.as_mut() {
            s.query = format!("EXPLAIN {}", x.stmt.trim().trim_end_matches(';'));
        }
        self.sql_run(true, false);
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
