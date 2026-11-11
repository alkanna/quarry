//! SQL dumps in and out: the Export dialog for tables and whole databases, the Run SQL file dialog, and the
//! status bar readout shared by every long write of this kind (a CSV import, a script file).

use super::*;
use db::sql::csv::Source;
use db::sql::dump::{DumpPlan, DumpTable};
use db::sql::import::{JobDone, Progress};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Clone, Copy, PartialEq)]
pub(super) enum JobKind {
    Import,
    Script,
}

/// A CSV import or a script file running, with what Cancel needs.
pub(super) struct SqlJob {
    pub(super) req: u64,
    pub(super) conn: u64,
    pub(super) db: db::sql::Db,
    pub(super) schema: String,
    pub(super) kind: JobKind,
    pub(super) prog: Arc<Progress>,
    pub(super) id: Arc<AtomicU64>,
    pub(super) cancel: Arc<AtomicBool>,
    pub(super) total_rows: u64,
    pub(super) total_bytes: u64,
    /// False for an import into a table whose engine has no transactions, where Cancel keeps what was written.
    pub(super) rolls_back: bool,
}

impl SqlJob {
    pub(super) fn new(req: u64, conn: u64, db: db::sql::Db, schema: String, kind: JobKind, total_rows: u64, total_bytes: u64) -> SqlJob {
        SqlJob { req, conn, db, schema, kind, prog: Arc::new(Progress::default()), id: Arc::new(AtomicU64::new(0)), cancel: Arc::new(AtomicBool::new(false)), total_rows, total_bytes, rolls_back: true }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum What {
    /// The rows the Content view shows, with no structure.
    Filtered,
    Selected,
    Whole,
}

struct Row {
    name: String,
    view: bool,
    on: bool,
    structure: bool,
    content: bool,
    drop: bool,
    rows: Option<u64>,
}

pub(super) struct DumpForm {
    conn: u64,
    db: String,
    conn_name: String,
    what: What,
    /// The open table with a filter: its name, the WHERE text and the rows it matches.
    filtered: Option<(String, String, u64)>,
    rows: Vec<Row>,
    extended: bool,
    snapshot: bool,
    extras: bool,
    path: String,
}

pub(super) struct RunForm {
    conn: u64,
    db: String,
    conn_name: String,
    src: Source,
    name: String,
}

/// Today as YYYY-MM-DD in UTC, for a default file name.
fn today() -> String {
    let days = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() / 86_400) as i64;
    let (y, m, d) = db::civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

impl App {
    /// Opens the dump dialog for a database. `table` is the open table whose filter the first choice offers.
    pub(super) fn open_sql_dump(&mut self, conn: u64, db: String, table: Option<String>) {
        let Some(c) = self.conn(conn) else { return };
        if c.sql.is_none() {
            return self.warn_offline();
        }
        let names: Vec<String> = c.dbs.iter().find(|d| d.0 == db).and_then(|d| d.1.clone()).unwrap_or_default();
        let schema = self.sql_schema.get(&(conn, db.clone()));
        let rows: Vec<Row> = names
            .iter()
            .map(|n| {
                let view = self.views.has(conn, &db, &n);
                Row { name: n.clone(), view, on: true, structure: true, content: !view, drop: false, rows: schema.and_then(|s| s.table(n)).and_then(|t| t.rows) }
            })
            .collect();
        // The table open in the Content view with a filter set is the first thing to export.
        let filtered = self.t().filter(|t| t.sel.conn == conn && t.sel.db == db && table.as_deref() == Some(t.sel.coll.as_str()) && !t.filter_txt.trim().is_empty()).map(|t| (t.sel.coll.clone(), t.filter_txt.clone(), t.count));
        let what = if filtered.is_some() { What::Filtered } else if table.is_some() { What::Selected } else { What::Whole };
        let mut rows = rows;
        if let (What::Selected, Some(t)) = (what, &table) {
            for r in &mut rows {
                r.on = r.name == *t;
            }
        }
        let conn_name = c.meta.name.clone();
        let path = dirs::download_dir().unwrap_or_default().join(format!("{db}-{}.sql", today())).display().to_string();
        self.modal = Some(Modal::SqlDump(Box::new(DumpForm { conn, db, conn_name, what, filtered, rows, extended: true, snapshot: true, extras: true, path })));
    }

    /// Asks for a script file, then opens the dialog that runs it.
    pub(super) fn open_sql_run_file(&mut self, conn: u64, db: String) {
        let Some(c) = self.conn(conn) else { return };
        if c.sql.is_none() {
            return self.warn_offline();
        }
        let conn_name = c.meta.name.clone();
        let Some(p) = rfd::FileDialog::new().add_filter("SQL", &["sql", "txt"]).pick_file() else { return };
        let src = Source::File(p.clone());
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.modal = Some(Modal::SqlRunFile(Box::new(RunForm { conn, db, conn_name, src, name })));
    }

    pub(super) fn sql_dump_dialog(&mut self, ui: &mut egui::Ui, f: &mut DumpForm, keep: &mut bool) {
        ui.set_width(610.0);
        dialog_title(ui, "Export");
        ui.label(RichText::new(format!("{} on {}", f.db, f.conn_name)).size(TXT_SMALL).color(MUTED()));
        ui.add_space(10.0);
        let mono = |t: String| RichText::new(t).font(FontId::monospace(TXT_SMALL)).color(FAINT());
        let selected = f.rows.iter().filter(|r| r.on).count();
        egui::Grid::new("dump-what").num_columns(2).spacing([14.0, 8.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
            form_label(ui, "What");
            ui.vertical(|ui| {
                if let Some((t, _, n)) = f.filtered.clone() {
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut f.what, What::Filtered, format!("Filtered rows of {t}"));
                        ui.label(mono(format!("{} rows", group(&n.to_string()))));
                    });
                }
                ui.horizontal(|ui| {
                    ui.radio_value(&mut f.what, What::Selected, "Selected tables");
                    ui.label(mono(format!("{selected} of {}", f.rows.len())));
                });
                if ui.radio_value(&mut f.what, What::Whole, "Whole database").clicked() {
                    for r in &mut f.rows {
                        (r.on, r.structure, r.content) = (true, true, !r.view);
                    }
                }
            });
            ui.end_row();
        });
        ui.add_space(8.0);
        let filtered = f.what == What::Filtered;
        let mut edited = false;
        ui.scope(|ui| {
            if filtered {
                ui.disable();
            }
            egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_MD).show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical().id_salt("dump-tables").max_height(190.0).auto_shrink([false, true]).show(ui, |ui| {
                    egui::Grid::new("dump-tables").num_columns(5).spacing([22.0, 7.0]).min_col_width(60.0).show(ui, |ui| {
                        let head = |ui: &mut egui::Ui, t: &str| ui.label(RichText::new(t).size(TXT_SMALL).color(MUTED()));
                        ui.horizontal(|ui| {
                            ui.add_space(8.0);
                            head(ui, "Table");
                        });
                        for h in ["Structure", "Content", "DROP first"] {
                            head(ui, h);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_space(8.0);
                            head(ui, "Rows");
                        });
                        ui.end_row();
                        for r in &mut f.rows {
                            let off = !r.on || (r.view && !f.extras);
                            ui.horizontal(|ui| {
                                ui.add_space(8.0);
                                edited |= ui.checkbox(&mut r.on, "").changed();
                                let name = if r.view { format!("{} (view)", r.name) } else { r.name.clone() };
                                ui.allocate_ui_with_layout(egui::vec2(170.0, BUTTON_H), egui::Layout::left_to_right(egui::Align::Center), |ui| ui.add(egui::Label::new(RichText::new(name).font(FontId::monospace(TXT_BODY)).color(if off { FAINT() } else { TEXT() })).truncate()));
                            });
                            ui.add_enabled_ui(!off, |ui| edited |= ui.checkbox(&mut r.structure, "").changed());
                            ui.add_enabled_ui(!off && !r.view, |ui| edited |= ui.checkbox(&mut r.content, "").changed());
                            ui.add_enabled_ui(!off && r.structure, |ui| edited |= ui.checkbox(&mut r.drop, "").changed());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.add_space(8.0);
                                if let Some(n) = r.rows.filter(|_| !r.view) {
                                    ui.label(RichText::new(group(&n.to_string())).font(FontId::monospace(TXT_SMALL)).color(if off { FAINT() } else { C_NUMBER() }));
                                }
                            });
                            ui.end_row();
                        }
                    });
                });
            });
        });
        if edited && f.what == What::Whole {
            f.what = What::Selected;
        }
        ui.add_space(8.0);
        caption(ui, "OPTIONS");
        ui.add_space(2.0);
        egui::Grid::new("dump-opts").num_columns(2).spacing([40.0, 8.0]).show(ui, |ui| {
            ui.checkbox(&mut f.extended, "Extended INSERTs (1,000 rows each)").on_hover_text("One INSERT for many rows is much faster to run. Off writes one INSERT per row.");
            ui.checkbox(&mut f.snapshot, "Consistent snapshot").on_hover_text("Reads every table from one point in time without locking anything (InnoDB).");
            ui.end_row();
            ui.checkbox(&mut f.extras, "Include views, triggers and routines");
            // The file name carries the choice, so the path and the checkbox cannot disagree.
            let mut gz = f.path.trim_end().ends_with(".gz");
            if ui.checkbox(&mut gz, "Compress with gzip").changed() {
                let p = f.path.trim_end();
                f.path = if gz { format!("{p}.gz") } else { p.strip_suffix(".gz").unwrap_or(p).to_string() };
            }
            ui.end_row();
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_sized([FORM_LABEL_W, BUTTON_H], egui::Label::new(RichText::new("Save to").color(MUTED())));
            let w = ui.available_width() - 100.0;
            ui.add(egui::TextEdit::singleline(&mut f.path).font(egui::TextStyle::Monospace).desired_width(w));
            if ui.button("Choose…").clicked() {
                let start = std::path::Path::new(&f.path);
                let mut dlg = rfd::FileDialog::new().add_filter("SQL", &["sql", "gz"]);
                if let Some(n) = start.file_name() {
                    dlg = dlg.set_file_name(n.to_string_lossy());
                }
                if let Some(p) = dlg.save_file() {
                    f.path = p.display().to_string();
                }
            }
        });
        let total: u64 = if filtered { f.filtered.as_ref().map_or(0, |x| x.2) } else { f.rows.iter().filter(|r| r.on && r.content && !r.view).filter_map(|r| r.rows).sum() };
        let ready = !f.path.trim().is_empty() && (filtered || f.rows.iter().any(|r| r.on && (r.structure || r.content) && !(r.view && !f.extras)));
        let mut go = false;
        ui.add_space(8.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let busy = self.export_job.is_some();
            go = ui.add_enabled(ready && !busy, primary_button(ui, if total > 0 { format!("Export {} rows", group(&total.to_string())) } else { "Export".to_string() }, PRIMARY())).on_disabled_hover_text(if busy { "An export is already running" } else { "Pick something to export" }).clicked();
            if ui.button("Cancel").clicked() {
                *keep = false;
            }
        });
        if go {
            self.start_dump(f, total);
            *keep = false;
        }
    }

    fn start_dump(&mut self, f: &DumpForm, total: u64) {
        let Some(sql) = self.conn(f.conn).and_then(|c| c.sql.clone()) else { return self.warn_offline() };
        let per = if f.extended { 1000 } else { 1 };
        let plan = match (f.what, &f.filtered) {
            (What::Filtered, Some((t, w, _))) => DumpPlan { schema: f.db.clone(), tables: vec![DumpTable { name: t.clone(), view: false, structure: false, content: true, drop: false }], create_db: false, extras: false, rows_per_insert: per, snapshot: f.snapshot, filter: Some(w.clone()) },
            _ => {
                let tables = f.rows.iter().filter(|r| r.on && !(r.view && !f.extras)).map(|r| DumpTable { name: r.name.clone(), view: r.view, structure: r.structure, content: r.content && !r.view, drop: r.drop && r.structure }).collect();
                DumpPlan { schema: f.db.clone(), tables, create_db: f.what == What::Whole, extras: f.extras, rows_per_insert: per, snapshot: f.snapshot, filter: None }
            }
        };
        self.req += 1;
        let job = super::export::ExportJob { req: self.req, count: Arc::new(AtomicU64::new(0)), cancel: Arc::new(AtomicBool::new(false)), id: Arc::new(AtomicU64::new(0)), db: sql.clone(), copy: false, step: Arc::new(AtomicU64::new(0)), of: 0 };
        self.worker.sql_dump(job.req, sql, plan, PathBuf::from(f.path.trim()), job.count.clone(), job.id.clone(), job.cancel.clone());
        self.export_job = Some(job);
        self.ok(format!("Exporting {} to {}", if total > 0 { format!("about {} rows", group(&total.to_string())) } else { "the structure".to_string() }, f.path.trim()));
    }

    pub(super) fn sql_run_file_dialog(&mut self, ui: &mut egui::Ui, f: &mut RunForm, keep: &mut bool) {
        ui.set_width(520.0);
        dialog_title(ui, "Run SQL file");
        ui.label(RichText::new(format!("{} \u{b7} {}", f.name, human_bytes(f.src.len() as usize))).size(TXT_SMALL).color(MUTED()));
        ui.add_space(8.0);
        let writable = self.conn_writable(f.conn);
        let say = |ui: &mut egui::Ui, t: String, c: Color32| ui.add(egui::Label::new(RichText::new(t).color(c)).wrap());
        say(ui, format!("Runs every statement of the file on {} in the database {}. A DELIMITER line and /*! ... */ comments are understood, and the file may switch database with USE.", f.conn_name, f.db), TEXT());
        ui.add_space(4.0);
        say(ui, "Statements run one by one and stop at the first error, which is shown with its number and text. The ones before it stay applied. There is no undo for a script file.".into(), WARNING());
        if !writable {
            ui.add_space(4.0);
            say(ui, "Writes are locked on this connection. Allow writes to run the file.".into(), ALERT());
        }
        let (mut go, mut unlock) = (false, false);
        ui.add_space(8.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let busy = self.sql_job.is_some();
            go = ui.add_enabled(writable && !busy, primary_button(ui, "Run file", ALERT())).on_disabled_hover_text(if busy { "Another import or script is running" } else { "Allow writes on this connection first" }).clicked();
            if ui.button("Cancel").clicked() {
                *keep = false;
            }
            if !writable {
                unlock = quiet(ui, "Allow writes").clicked();
            }
        });
        if go {
            let Some(sql) = self.conn(f.conn).and_then(|c| c.sql.clone()) else { return self.warn_offline() };
            self.req += 1;
            let job = SqlJob::new(self.req, f.conn, sql.clone(), f.db.clone(), JobKind::Script, 0, f.src.len());
            self.worker.sql_run_file(job.req, sql, f.db.clone(), f.src.clone(), !writable, job.prog.clone(), job.id.clone(), job.cancel.clone());
            self.sql_job = Some(job);
            self.ok(format!("Running {}", f.name));
            *keep = false;
        } else if unlock {
            *keep = false;
            let (conn, db, src, name, conn_name) = (f.conn, f.db.clone(), f.src.clone(), f.name.clone(), f.conn_name.clone());
            self.modal = Some(Modal::Unlock(conn, Some(Box::new(move |a: &mut App| a.modal = Some(Modal::SqlRunFile(Box::new(RunForm { conn, db, conn_name, src, name })))))));
        }
    }

    /// An import or a script finished: report it, keep its undo, and reload what it changed.
    pub(super) fn sql_job_done(&mut self, req: u64, res: db::Res<JobDone>) {
        let Some(j) = self.sql_job.take_if(|j| j.req == req) else { return };
        // A script can create or drop anything anywhere, and one that failed or was stopped kept what ran before.
        if j.kind == JobKind::Script {
            self.worker.sql_lists(j.conn, j.db.clone(), j.schema.clone());
        }
        match res {
            Ok(d) => {
                self.ok(d.msg);
                if let Some(u) = d.undo {
                    self.push_undo(u);
                }
                // A script's tabs and lists follow in `sql_lists_done`.
                if j.kind == JobKind::Import {
                    let tabs: Vec<usize> = (0..self.tabs.len()).filter(|&i| self.tabs[i].sel.conn == j.conn && self.tabs[i].sel.db == d.schema && d.table.as_ref().is_none_or(|t| self.tabs[i].sel.coll == *t)).collect();
                    for i in tabs {
                        self.reload_of(i);
                    }
                    if d.tables_changed {
                        self.side_act(j.conn, SideAct::Refresh(j.schema.clone()));
                    }
                }
            }
            Err(e) if e == "Import cancelled" => self.ok("Import stopped. Nothing was imported."),
            // A table that cannot roll back: the message counts the rows that stayed.
            Err(e) if e.starts_with("Import stopped.") => self.ok(e),
            Err(e) => self.err(e),
        }
    }

    /// After a script, finished or not: the database list, the tables of its database and the open tabs follow what
    /// the server has now. Tabs of a database or table that is gone close; the others of its database reload.
    pub(super) fn sql_lists_done(&mut self, conn: u64, schema: String, res: db::Res<(Vec<String>, Option<Vec<String>>)>) {
        let Ok((dbs, tables)) = res else { return };
        let mut dropped = Vec::new();
        if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
            let old = std::mem::take(&mut c.dbs);
            dropped = old.iter().filter(|o| !dbs.contains(&o.0)).map(|o| o.0.clone()).collect();
            c.dbs = dbs.iter().map(|d| (d.clone(), old.iter().find(|o| o.0 == *d).and_then(|o| o.1.clone()))).collect();
        }
        for d in &dropped {
            self.open_dbs.remove(&(conn, d.clone()));
            self.sql_asked.remove(&(conn, d.clone()));
            self.forget_schema(conn, d);
        }
        self.remove_tabs(|t| t.sel.conn == conn && tab_gone(&t.sel, &dbs, &schema, tables.as_deref()));
        let open: Vec<usize> = (0..self.tabs.len()).filter(|&i| self.tabs[i].sel.conn == conn && self.tabs[i].sel.db == schema && !self.tabs[i].sel.coll.is_empty()).collect();
        for i in open {
            self.reload_of(i);
        }
        if tables.is_some() {
            self.side_act(conn, SideAct::Refresh(schema));
        }
    }

    /// The status bar readout of a running import or script, with Cancel. True while one runs.
    pub(super) fn sql_job_progress(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(j) = &self.sql_job else { return false };
        spin(ui);
        let (rows, bytes) = (j.prog.rows.load(Ordering::Relaxed), j.prog.bytes.load(Ordering::Relaxed));
        let text = match j.kind {
            JobKind::Import => format!("Importing {} of {} rows", group(&rows.to_string()), group(&j.total_rows.to_string())),
            JobKind::Script => format!("Running statement {}, {}%", group(&rows.to_string()), if j.total_bytes == 0 { 0 } else { (bytes * 100 / j.total_bytes).min(100) }),
        };
        ui.label(RichText::new(text).size(TXT_CAPTION).color(MUTED()));
        if quiet(ui, RichText::new("Cancel").size(TXT_CAPTION)).on_hover_text(if j.kind == JobKind::Import && j.rolls_back { "Stop and roll everything back" } else if j.kind == JobKind::Import { "Stop. This table's engine cannot roll back, so rows written so far stay." } else { "Interrupt the running statement and stop. Statements that already ran stay applied." }).clicked() {
            self.worker.sql_kill(j.db.clone(), j.id.load(Ordering::SeqCst), j.cancel.clone());
        }
        ui.ctx().request_repaint_after(Duration::from_millis(200));
        true
    }

    /// QUARRY_TEST_SQL=dump: the Export dialog of the design with the shop database.
    pub(super) fn demo_sql_dump(&mut self) {
        let conn = self.t().map_or(0, |t| t.sel.conn);
        let row = |name: &str, on: bool, content: bool, rows: u64| Row { name: name.into(), view: false, on, structure: on, content, drop: false, rows: Some(rows) };
        let rows = vec![
            row("customers", true, true, 1284),
            row("order_items", true, true, 8902),
            row("orders", true, true, 4410),
            row("payments", true, false, 4377),
            row("products", false, false, 212),
            row("refunds", false, false, 96),
            row("shipments", false, false, 3120),
            row("suppliers", false, false, 38),
            row("tags", false, false, 54),
            row("users", false, false, 1190),
            Row { name: "revenue_by_day".into(), view: true, on: false, structure: false, content: false, drop: false, rows: None },
        ];
        self.modal = Some(Modal::SqlDump(Box::new(DumpForm { conn, db: "shop".into(), conn_name: "shop-prod".into(), what: What::Selected, filtered: Some(("orders".into(), "status = 'paid'".into(), 312)), rows, extended: true, snapshot: false, extras: true, path: "~/Downloads/shop-2026-11-09.sql".into() })));
    }
}

/// Whether a tab shows a database the server no longer lists, or a table of `schema` that is not among `tables`.
/// `tables` is None when they could not be read, which closes nothing on its own.
fn tab_gone(sel: &Sel, dbs: &[String], schema: &str, tables: Option<&[String]>) -> bool {
    if sel.db.is_empty() {
        return false;
    }
    if !dbs.contains(&sel.db) {
        return true;
    }
    sel.db == schema && !sel.coll.is_empty() && tables.is_some_and(|t| !t.contains(&sel.coll))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Final review finding 8: after a script the tabs of what it dropped close, wherever it was.
    #[test]
    fn tabs_of_what_a_script_dropped_close() {
        let sel = |db: &str, coll: &str| Sel { conn: 1, db: db.into(), coll: coll.into(), ..Default::default() };
        let dbs = vec!["shop".to_string(), "other".to_string()];
        let tables = vec!["orders".to_string()];
        let t = Some(tables.as_slice());
        assert!(!tab_gone(&sel("shop", "orders"), &dbs, "shop", t));
        assert!(tab_gone(&sel("shop", "items"), &dbs, "shop", t), "a table the script dropped");
        assert!(tab_gone(&sel("gone", "x"), &dbs, "shop", t), "a database the script dropped");
        assert!(tab_gone(&sel("gone", ""), &dbs, "shop", t), "a Query tab of a dropped database");
        assert!(!tab_gone(&sel("shop", ""), &dbs, "shop", t), "a Query tab of the database stays");
        assert!(!tab_gone(&sel("other", "items"), &dbs, "shop", t), "tables of other databases were not read");
        assert!(!tab_gone(&sel("shop", "items"), &dbs, "shop", None));
    }
}
