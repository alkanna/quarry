//! The explain dialog of a SQL statement: the plan as a tree of nodes with the warnings that matter, the raw plan, and
//! an optional ANALYZE run that shows what the server actually did.

use super::query::run_target;
use crate::app::*;
use crate::db::sql::plan::{self, Access, Analyze, Plan};
use crate::db::sql::{Db, Dialect};

/// The state of one open SQL explain dialog.
pub(in crate::app) struct SqlExplain {
    /// The id of the request in flight, so an old answer never fills a newer dialog.
    req: u64,
    busy: bool,
    stmt: String,
    conn: u64,
    /// The tab's database, for the Query tab the classic rows open in.
    tab_db: String,
    dialect: Dialect,
    db: Db,
    schema: String,
    plan: Option<Plan>,
    /// Why the plan could not be drawn, when the server's text could not be read.
    unread: Option<String>,
    raw: String,
    show_raw: bool,
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
        let tab_db = t.sel.db.clone();
        let (db, schema) = run_target(t, db);
        let writable = self.conn_writable(conn);
        let analyze = plan::analyze_needs(d, &db.version).map(str::to_string).map_or_else(|| plan::analyze_mode(&text, d, db.pooler()), Err).and_then(|a| match (d, writable) {
            // The read-only guard refuses ANALYZE on these engines, so it needs the unlock the user would otherwise be asked for.
            (Dialect::Postgres, false) => Err("ANALYZE on PostgreSQL runs the statement inside a transaction it rolls back, which needs writes unlocked on this connection.".into()),
            (Dialect::MariaDb, false) => Err("ANALYZE on MariaDB is a statement the read-only guard refuses. Unlock writes on this connection to use it.".into()),
            _ => Ok(a),
        });
        let mut x = SqlExplain { req: 0, busy: false, stmt: text, conn, tab_db, dialect: d, db, schema, plan: None, unread: None, raw: String::new(), show_raw: false, analyzed: false, rolled_back: false, analyze, asked: None, run: Default::default(), stopped: false };
        self.sql_explain_send(&mut x, None);
        self.modal = Some(Modal::SqlExplain(Box::new(x)));
    }

    /// Test only: opens the dialog on `plan` as if the server had answered, over a pool that never connects, because the demo has no
    /// server for `sql_explain` to ask.
    #[cfg(test)]
    pub(in crate::app) fn demo_explain(&mut self, text: &str, plan: &str) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
        let _guard = rt.enter();
        let opts = "mysql://root@127.0.0.1:1/shop".parse::<sqlx::mysql::MySqlConnectOptions>().expect("demo address");
        let db = Db { pool: crate::db::sql::Pool::My(sqlx::mysql::MySqlPoolOptions::new().connect_lazy_with(opts.clone())), dialect: Dialect::MySql, opts: crate::db::sql::exec::Opts::My(opts), version: "8.4.3".into(), tls: false, sql_mode: String::new(), setup: Default::default(), conn: 0 };
        let conn = self.t().map_or(0, |t| t.sel.conn);
        let analyze = plan::analyze_mode(text, Dialect::MySql, false);
        let x = SqlExplain { req: 1, busy: true, stmt: text.into(), conn, tab_db: "shop".into(), dialect: Dialect::MySql, db, schema: "shop".into(), plan: None, unread: None, raw: String::new(), show_raw: false, analyzed: false, rolled_back: false, analyze, asked: None, run: Default::default(), stopped: false };
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
            Ok(p) => (x.plan, x.unread) = (Some(p), None),
            // The raw text is still worth reading.
            Err(e) => (x.plan, x.unread, x.show_raw) = (None, Some(e), true),
        }
    }

    pub(in crate::app) fn sql_explain_dialog(&mut self, ui: &mut egui::Ui, x: &mut SqlExplain, keep: &mut bool) {
        ui.set_width(760.0);
        let mut sub = x.db.label();
        if let Some(p) = &x.plan {
            sub = format!("{sub}  ·  {} nodes", p.nodes.len());
            if let Some(c) = p.cost {
                sub = format!("{sub}  ·  cost {}", figure(c));
            }
            if let Some(ms) = p.exec_ms {
                sub = format!("{sub}  ·  {} ms to run", figure(ms));
            }
            if let Some(ms) = p.planning_ms {
                sub = format!("{sub}  ·  {} ms to plan", figure(ms));
            }
        }
        dialog_head(ui, &Head::new(if x.analyzed { "Explain analyze" } else { "Explain" }).sub(sub).glyph(glyphs::list));
        let mut b = body_begin(ui);
        let outer = ui;
        let ui = &mut b.content_ui;
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
        if let Some(p) = &x.plan {
            for w in &p.warnings {
                ui.label(RichText::new(w.as_str()).color(WARNING()));
            }
        }
        if x.plan.is_some() {
            ui.horizontal(|ui| {
                segmented(ui, &mut x.show_raw, &[(false, "Plan"), (true, "Raw")]);
            });
        }
        let find = if x.show_raw { Find::bar(ui, egui::Id::unique("sql-explain-raw"), &x.raw) } else { Find::default() };
        egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            if x.show_raw {
                let mut text = x.raw.as_str();
                let out = egui::TextEdit::multiline(&mut text).code_editor().desired_width(f32::INFINITY).layouter(&mut find.layouter(true)).show(ui);
                find.reveal(ui, &x.raw, &out);
            } else if let Some(p) = &x.plan {
                plan_grid(ui, p);
            }
        });
        b.end(outer);
        let ui = outer;
        let mut rows = false;
        let mut again = None;
        let mut stop = false;
        dialog_foot(ui, |ui| {
            if ui.button("Close").on_hover_text("Esc").clicked() {
                *keep = false;
            }
            if ui.button("Copy raw").clicked() {
                ui.ctx().copy_text(x.raw.clone());
            }
            rows = ui.button("Show rows in Query tab").on_hover_text("Run the classic EXPLAIN and show the server's rows").clicked();
            if x.busy {
                stop = ui.button("Stop").on_hover_text("End the statement on the server").clicked();
            }
            match &x.analyze {
                Ok(a) => {
                    let r = ui.add_enabled(!x.busy && !x.analyzed, egui::Button::new("Run with ANALYZE"));
                    let tip = if *a == Analyze::Rollback { "Runs the statement for real inside a transaction that is rolled back" } else { "Runs the statement for real to count rows and time" };
                    if r.on_hover_text(tip).on_disabled_hover_text("Already analyzed").clicked() {
                        again = Some(*a);
                    }
                }
                Err(why) => {
                    ui.add_enabled(false, egui::Button::new("Run with ANALYZE")).on_disabled_hover_text(why.as_str());
                }
            }
        });
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

/// A count or time for a column: whole numbers grouped, small ones with two decimals.
fn figure(n: f64) -> String {
    if n >= 100.0 { group(&format!("{n:.0}")) } else { format!("{n:.2}").trim_end_matches('0').trim_end_matches('.').to_string() }
}

fn plan_grid(ui: &mut egui::Ui, p: &Plan) {
    if p.nodes.is_empty() {
        ui.label(RichText::new("The server returned no plan nodes. See Raw.").color(MUTED()));
        return;
    }
    let heads: &[&str] = if p.analyzed { &["NODE", "ROWS", "COST", "ACTUAL ROWS", "LOOPS", "MS"] } else { &["NODE", "ROWS", "COST"] };
    egui::Grid::new("sql-explain-plan").num_columns(heads.len()).spacing([16.0, 6.0]).striped(true).show(ui, |ui| {
        for h in heads {
            caption(ui, *h);
        }
        ui.end_row();
        for n in &p.nodes {
            ui.vertical(|ui| {
                let c = match n.access {
                    Access::Scan => ALERT(),
                    Access::Index => SUCCESS(),
                    Access::Other => TEXT(),
                };
                let mut job = egui::text::LayoutJob::default();
                job.append(&format!("{}{}", "    ".repeat(n.depth), n.label), 0.0, mono_fmt(TXT_BODY, c, 400.0));
                for part in [n.target.as_str(), n.index.as_str()].into_iter().filter(|s| !s.is_empty()) {
                    job.append(&format!("  {part}"), 0.0, mono_fmt(TXT_BODY, MUTED(), 400.0));
                }
                ui.label(job);
                let pad = "    ".repeat(n.depth + 1);
                for d in &n.detail {
                    ui.label(RichText::new(format!("{pad}{d}")).monospace().size(TXT_SMALL).color(MUTED()));
                }
                for w in &n.warnings {
                    ui.label(RichText::new(format!("{pad}{w}")).size(TXT_SMALL).color(WARNING()));
                }
            });
            let cell = |v: Option<f64>| v.map(figure).unwrap_or_default();
            let vals = [n.rows, n.cost, n.actual_rows, n.loops, n.actual_ms];
            let shown = if p.analyzed { &vals[..] } else { &vals[..2] };
            for v in shown {
                ui.label(RichText::new(cell(*v)).monospace());
            }
            ui.end_row();
        }
    });
}
