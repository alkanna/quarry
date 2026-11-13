//! SQL dumps in and out: the Export dialog for tables and whole databases, the Run SQL file dialog, and the
//! status bar readout shared by every long write of this kind (a CSV import, a script file).

use crate::app::*;
use db::sql::csv::Source;
use db::sql::dump::{DumpPlan, DumpTable};
use db::sql::import::{JobDone, Progress};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Clone, Copy, PartialEq)]
pub(in crate::app) enum JobKind {
    Import,
    Script,
}

/// A CSV import or a script file running, with what Cancel needs.
pub(in crate::app) struct SqlJob {
    pub(in crate::app) req: u64,
    pub(in crate::app) conn: u64,
    pub(in crate::app) db: db::sql::Db,
    pub(in crate::app) schema: String,
    pub(in crate::app) kind: JobKind,
    pub(in crate::app) prog: Arc<Progress>,
    pub(in crate::app) id: Arc<AtomicU64>,
    pub(in crate::app) cancel: Arc<AtomicBool>,
    pub(in crate::app) total_rows: u64,
    pub(in crate::app) total_bytes: u64,
    /// False for an import into a table whose engine has no transactions, where Cancel keeps what was written.
    pub(in crate::app) rolls_back: bool,
}

impl SqlJob {
    pub(in crate::app) fn new(req: u64, conn: u64, db: db::sql::Db, schema: String, kind: JobKind, total_rows: u64, total_bytes: u64) -> SqlJob {
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
    /// Data and indexes on disk, once the server said.
    bytes: Option<u64>,
}

pub(in crate::app) struct DumpForm {
    conn: u64,
    db: String,
    /// The schema of a PostgreSQL database the table list is of; empty on MySQL, where the database is the namespace.
    schema: String,
    conn_name: String,
    what: What,
    /// The open table with a filter: its name, the WHERE text and the rows it matches.
    filtered: Option<(String, String, u64)>,
    rows: Vec<Row>,
    extended: bool,
    snapshot: bool,
    extras: bool,
    /// PostgreSQL only. Both off by default: they name roles, and a restore where one is missing stops there.
    grants: bool,
    owners: bool,
    path: String,
    /// The request the table sizes answer.
    size_req: u64,
}

pub(in crate::app) struct RunForm {
    conn: u64,
    db: String,
    /// The schema a PostgreSQL file's unqualified names land in (its search_path); empty on MySQL.
    schema: String,
    conn_name: String,
    src: Source,
    name: String,
}

/// Today as YYYY-MM-DD in UTC, for a default file name.
/// "210 MB", in the largest unit that keeps a number of at least 1.
fn human_size(b: u64) -> String {
    let units = ["bytes", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{b} bytes") } else if v < 10.0 { format!("{v:.1} {}", units[i]) } else { format!("{v:.0} {}", units[i]) }
}

fn today() -> String {
    let days = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() / 86_400) as i64;
    let (y, m, d) = db::civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

impl App {
    /// Opens the dump dialog for a database, in the schema its switcher shows on PostgreSQL. `table` is the open table whose
    /// filter the first choice offers.
    pub(in crate::app) fn sql_sizes_done(&mut self, req: u64, res: db::Res<Vec<(String, u64)>>) {
        // Sizes are an extra: a refusal leaves the estimate out.
        let (Some(Modal::SqlDump(f)), Ok(sizes)) = (self.modal.as_mut(), res) else { return };
        if f.size_req != req {
            return;
        }
        for r in &mut f.rows {
            r.bytes = sizes.iter().find(|s| s.0 == r.name).map(|s| s.1);
        }
    }

    pub(in crate::app) fn open_sql_dump(&mut self, conn: u64, db: String, table: Option<String>) {
        let schema = self.ns_of(conn, &db).to_string();
        self.open_sql_dump_in(conn, db, schema, table)
    }

    /// `open_sql_dump` for a table of a given schema, which is not the one the sidebar shows when its tab was opened from
    /// another. The schema is empty on MySQL.
    pub(in crate::app) fn open_sql_dump_in(&mut self, conn: u64, db: String, schema: String, table: Option<String>) {
        let Some(c) = self.conn(conn) else { return };
        if c.sql.is_none() {
            return self.warn_offline();
        }
        // A PostgreSQL database lists the tables of the schema its switcher shows; the whole-database choice still takes every schema.
        let pg = c.meta.engine == db::sql::Engine::Postgres;
        let schema = if pg { Some(schema).filter(|s| !s.is_empty()).unwrap_or_else(|| c.pg_picked(&db).to_string()) } else { String::new() };
        let names: Vec<String> = if pg { c.tables_in(&db, &schema).cloned().unwrap_or_default() } else { c.dbs.iter().find(|d| d.0 == db).and_then(|d| d.1.clone()).unwrap_or_default() };
        // The estimates are kept per database by table name, which a second schema of the same name would confuse.
        let est = self.sql_schema.get(&(conn, db.clone())).filter(|_| !pg);
        let ns = ns_key(&db, &schema);
        let rows: Vec<Row> = names
            .iter()
            .map(|n| {
                let view = self.views.has(conn, &ns, n);
                Row { name: n.clone(), view, on: true, structure: true, content: !view, drop: false, rows: est.and_then(|s| s.table(n)).and_then(|t| t.rows), bytes: None }
            })
            .collect();
        // The table open in the Content view with a filter set is the first thing to export.
        let filtered = self.t().filter(|t| t.sel.conn == conn && t.sel.db == db && (!pg || t.sel.schema == schema) && table.as_deref() == Some(t.sel.coll.as_str()) && !t.filter_txt.trim().is_empty()).map(|t| (t.sel.coll.clone(), t.filter_txt.clone(), t.count));
        let what = if filtered.is_some() { What::Filtered } else if table.is_some() { What::Selected } else { What::Whole };
        let mut rows = rows;
        if let (What::Selected, Some(t)) = (what, &table) {
            for r in &mut rows {
                r.on = r.name == *t;
            }
        }
        let conn_name = c.meta.name.clone();
        let path = dirs::download_dir().unwrap_or_default().join(format!("{db}-{}.sql", today())).display().to_string();
        // The sizes come later; until then the footer counts rows only.
        self.req += 1;
        let size_req = self.req;
        if let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) {
            self.worker.sql_table_sizes(size_req, sql.at(&db), if pg { schema.clone() } else { db.clone() });
        }
        self.modal = Some(Modal::SqlDump(Box::new(DumpForm { conn, db, schema, conn_name, what, filtered, rows, extended: true, snapshot: true, extras: true, grants: false, owners: false, path, size_req })));
    }

    /// Asks for a script file, then opens the dialog that runs it.
    pub(in crate::app) fn open_sql_run_file(&mut self, conn: u64, db: String) {
        let Some(c) = self.conn(conn) else { return };
        if c.sql.is_none() {
            return self.warn_offline();
        }
        let Some(p) = rfd::FileDialog::new().add_filter("SQL", &["sql", "txt"]).pick_file() else { return };
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.open_run_form(conn, db, Source::File(p), name);
    }

    /// The Run SQL file dialog for a file already chosen, which the UI tests open without the native file dialog.
    pub(in crate::app) fn open_run_form(&mut self, conn: u64, db: String, src: Source, name: String) {
        let Some(c) = self.conn(conn) else { return };
        let schema = if c.meta.engine == db::sql::Engine::Postgres { c.pg_picked(&db).to_string() } else { String::new() };
        let conn_name = c.meta.name.clone();
        self.modal = Some(Modal::SqlRunFile(Box::new(RunForm { conn, db, schema, conn_name, src, name })));
    }

    /// Sets where the open dump dialog writes, which the UI tests do instead of using the native file dialog.
    #[cfg(test)]
    pub(in crate::app) fn set_dump_path(&mut self, path: &str) {
        if let Some(Modal::SqlDump(f)) = &mut self.modal {
            f.path = path.to_string();
        }
    }

    pub(in crate::app) fn sql_dump_dialog(&mut self, ui: &mut egui::Ui, f: &mut DumpForm, keep: &mut bool) {
        ui.set_width(660.0);
        let pg = self.is_pg(f.conn);
        dialog_head(ui, &Head::new("Export").sub(if pg { format!("{}, schema {} on {}", f.db, f.schema, f.conn_name) } else { format!("{} on {}", f.db, f.conn_name) }).glyph(glyphs::download));
        let mut b = body_begin(ui);
        let outer = ui;
        let ui = &mut b.content_ui;
        let selected = f.rows.iter().filter(|r| r.on).count();
        let views = f.rows.iter().filter(|r| r.view).count();
        card_caption(ui, "What to export", "");
        // Radio cards side by side: the whole database, the checked tables, and the filtered rows when there are some.
        let n_cards = if f.filtered.is_some() { 3.0 } else { 2.0 };
        let cw = (ui.available_width() - 10.0 * (n_cards - 1.0)) / n_cards;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let whole_desc = format!("{} tables{}", f.rows.len() - views, if views > 0 { format!(", {views} views") } else { String::new() });
            let r = radio_card(ui, cw, f.what == What::Whole, if pg { "Whole database" } else { "Whole database" }, &if pg { format!("{whole_desc}, every schema") } else { whole_desc });
            if r.on_hover_text(if pg { "Every table, view, type, sequence, function and extension of every schema, as the server has them now" } else { "Every table with its rows" }).clicked() {
                f.what = What::Whole;
                for r in &mut f.rows {
                    (r.on, r.structure, r.content) = (true, true, !r.view);
                }
            }
            if radio_card(ui, cw, f.what == What::Selected, "Selected tables", &format!("{selected} of {} checked below", f.rows.len())).clicked() {
                f.what = What::Selected;
            }
            if let Some((t, _, n)) = f.filtered.clone() && radio_card(ui, cw, f.what == What::Filtered, &format!("Filtered rows of {t}"), &format!("{} rows", group(&n.to_string()))).clicked() {
                f.what = What::Filtered;
            }
        });
        ui.add_space(12.0);
        let filtered = f.what == What::Filtered;
        let mut edited = false;
        ui.scope(|ui| {
            if filtered {
                ui.disable();
            }
            card_frame().inner_margin(egui::Margin::symmetric(4, 8)).show(ui, |ui| {
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
        // The file drops every routine of the schema, not only the ones the checked tables use, because a routine can name
        // a table's row type and block its DROP.
        if pg && f.extras && f.what != What::Whole && f.rows.iter().any(|r| r.on && r.structure && r.drop) {
            ui.add_space(4.0);
            ui.label(RichText::new("DROP first also drops and recreates every function and procedure of the schema, whichever tables are checked.").size(TXT_SMALL).color(MUTED()));
        }
        ui.add_space(16.0);
        // The options as switches in two columns of one card.
        let mut gz = f.path.trim_end().ends_with(".gz");
        let gz_was = gz;
        let mut always = true;
        form_card(ui, "Options", "", |rows| {
            rows.full(|ui| {
                ui.columns(2, |c| {
                    let opt = |ui: &mut egui::Ui, on: &mut bool, enabled: bool, title: &str, hint: &str, tip: &str| {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                ui.label(medium(RichText::new(title).size(TXT_BODY).color(BRIGHT())));
                                ui.label(RichText::new(hint).size(TXT_SMALL).color(MUTED()));
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let r = ui.add_enabled_ui(enabled, |ui| toggle(ui, on)).inner;
                                r.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, enabled, *on, title));
                                if !tip.is_empty() {
                                    r.on_hover_text(tip);
                                }
                            });
                        });
                        ui.add_space(10.0);
                    };
                    opt(&mut c[0], &mut f.extended, true, "Extended INSERTs", "1,000 rows per statement", "One INSERT for many rows is much faster to run. Off writes one INSERT per row.");
                    opt(&mut c[0], &mut f.extras, true, "Views, triggers, routines", "Written after the tables", "");
                    if pg {
                        // The dump is one REPEATABLE READ READ ONLY transaction, so there is nothing to choose.
                        opt(&mut c[1], &mut always, false, "Consistent snapshot", "Always, in one read-only transaction", "PostgreSQL always reads every table from one snapshot, in a read-only transaction that locks nothing but DDL.");
                    } else {
                        opt(&mut c[1], &mut f.snapshot, true, "Consistent snapshot", "One point in time, no locks", "Reads every table from one point in time without locking anything (InnoDB).");
                    }
                    opt(&mut c[1], &mut gz, true, "Compress with gzip", "Saves a .sql.gz file", "");
                    if pg {
                        opt(&mut c[0], &mut f.grants, !filtered, "Include grants", "GRANT and REVOKE for what it makes", "GRANT and REVOKE for the schemas, tables, columns, sequences, functions and types the file makes; a whole database also gets its default privileges and the grants on the database itself.");
                        opt(&mut c[1], &mut f.owners, !filtered, "Set owners", "ALTER ... OWNER TO for each object", "ALTER ... OWNER TO for everything the file makes. Off, everything belongs to the role that runs the file.");
                    }
                });
            });
        });
        // The file name carries the choice, so the path and the switch cannot disagree.
        if gz != gz_was {
            let p = f.path.trim_end();
            f.path = if gz { format!("{p}.gz") } else { p.strip_suffix(".gz").unwrap_or(p).to_string() };
        }
        if pg && !filtered {
            ui.add_space(4.0);
            let roles = if f.grants || f.owners { "The roles they name must exist where the file is run, or the run stops at the first missing one (the tables and rows are in by then). " } else { "" };
            ui.add(egui::Label::new(RichText::new(format!("{roles}Row security policies are always written; one for a named role needs that role too.")).size(TXT_SMALL).color(if roles.is_empty() { MUTED() } else { WARNING() })).wrap());
        }
        ui.add_space(4.0);
        card_caption(ui, "Save to", "");
        plain_card(ui, |ui| ui.horizontal(|ui| {
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
        }));
        let total: u64 = if filtered { f.filtered.as_ref().map_or(0, |x| x.2) } else { f.rows.iter().filter(|r| r.on && r.content && !r.view).filter_map(|r| r.rows).sum() };
        let ready = !f.path.trim().is_empty() && (filtered || f.rows.iter().any(|r| r.on && (r.structure || r.content) && !(r.view && !f.extras)));
        let mut go = false;
        ui.add_space(8.0);
        b.end(outer);
        let ui = outer;
        dialog_foot(ui, |ui| {
            let busy = self.export_job.is_some();
            go = ui.add_enabled(ready && !busy, primary_button(ui, if total > 0 { format!("Export {} rows", group(&total.to_string())) } else { "Export".to_string() }, PRIMARY())).on_disabled_hover_text(if busy { "An export is already running" } else { "Pick something to export" }).clicked();
            if ui.button("Cancel").clicked() {
                *keep = false;
            }
            let n = if filtered { 1 } else { f.rows.iter().filter(|r| r.on && !(r.view && !f.extras)).count() };
            // The tables as they sit on the server; the file is usually smaller than that, and much smaller compressed.
            let bytes: u64 = if filtered { 0 } else { f.rows.iter().filter(|r| r.on && r.content && !r.view).filter_map(|r| r.bytes).sum() };
            let size = if bytes > 0 { format!("  ·  {} on the server", human_size(bytes)) } else { String::new() };
            foot_note(ui, &format!("{}{}{size}", crate::app::sql::create::plural(n as u64, "table"), if total > 0 { format!("  ·  about {} rows", group(&total.to_string())) } else { String::new() }));
        });
        if go {
            self.start_dump(f, total);
            *keep = false;
        }
    }

    fn start_dump(&mut self, f: &DumpForm, total: u64) {
        let Some(sql) = self.conn(f.conn).and_then(|c| c.sql.clone()) else { return self.warn_offline() };
        // A PostgreSQL connection belongs to one database, so the database picks the pool, and the schema is the namespace.
        let sql = sql.at(&f.db);
        let space = if f.schema.is_empty() { f.db.clone() } else { f.schema.clone() };
        let per = if f.extended { 1000 } else { 1 };
        let pg = !f.schema.is_empty();
        let plan = match (f.what, &f.filtered) {
            (What::Filtered, Some((t, w, _))) => DumpPlan { schema: space.clone(), tables: vec![DumpTable { name: t.clone(), view: false, structure: false, content: true, drop: false }], create_db: false, extras: false, rows_per_insert: per, snapshot: f.snapshot, filter: Some(w.clone()), grants: false, owners: false },
            _ => {
                let tables = f.rows.iter().filter(|r| r.on && !(r.view && !f.extras)).map(|r| DumpTable { name: r.name.clone(), view: r.view, structure: r.structure, content: r.content && !r.view, drop: r.drop && r.structure }).collect();
                DumpPlan { schema: space.clone(), tables, create_db: f.what == What::Whole, extras: f.extras, rows_per_insert: per, snapshot: f.snapshot, filter: None, grants: pg && f.grants, owners: pg && f.owners }
            }
        };
        self.req += 1;
        let job = crate::app::export::ExportJob { req: self.req, count: Arc::new(AtomicU64::new(0)), cancel: Arc::new(AtomicBool::new(false)), id: Arc::new(AtomicU64::new(0)), db: sql.clone(), copy: false, step: Arc::new(AtomicU64::new(0)), of: 0 };
        self.worker.sql_dump(job.req, sql, plan, PathBuf::from(f.path.trim()), job.count.clone(), job.id.clone(), job.cancel.clone());
        self.export_job = Some(job);
        self.ok(format!("Exporting {} to {}", if total > 0 { format!("about {} rows", group(&total.to_string())) } else { "the structure".to_string() }, f.path.trim()));
    }

    pub(in crate::app) fn sql_run_file_dialog(&mut self, ui: &mut egui::Ui, f: &mut RunForm, keep: &mut bool) {
        ui.set_width(520.0);
        dialog_head(ui, &Head::new("Run SQL file").sub(format!("{} \u{b7} {}", f.name, human_bytes(f.src.len() as usize))).glyph(glyphs::upload));
        let mut b = body_begin(ui);
        let outer = ui;
        let ui = &mut b.content_ui;
        let writable = self.conn_writable(f.conn);
        let say = |ui: &mut egui::Ui, t: String, c: Color32| ui.add(egui::Label::new(RichText::new(t).color(c)).wrap());
        if f.schema.is_empty() {
            say(ui, format!("Runs every statement of the file on {} in the database {}. A DELIMITER line and /*! ... */ comments are understood, and the file may switch database with USE.", f.conn_name, f.db), TEXT());
        } else {
            say(ui, format!("Runs every statement of the file on {} in the database {}, with the schema {} as the search path unless the file sets its own. Dollar-quoted bodies, E'' strings and nested comments are understood. COPY ... FROM stdin and psql commands such as \\connect are refused, with their line.", f.conn_name, f.db, f.schema), TEXT());
        }
        ui.add_space(4.0);
        say(ui, "Statements run one by one and stop at the first error, which is shown with its number and text. The ones before it stay applied. There is no undo for a script file.".into(), WARNING());
        if !writable {
            ui.add_space(4.0);
            say(ui, "Writes are locked on this connection. Allow writes to run the file.".into(), ALERT());
        }
        let (mut go, mut unlock) = (false, false);
        ui.add_space(8.0);
        b.end(outer);
        let ui = outer;
        dialog_foot(ui, |ui| {
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
            let sql = sql.at(&f.db);
            let space = if f.schema.is_empty() { f.db.clone() } else { f.schema.clone() };
            self.req += 1;
            let job = SqlJob::new(self.req, f.conn, sql.clone(), space.clone(), JobKind::Script, 0, f.src.len());
            self.worker.sql_run_file(job.req, sql, space, f.src.clone(), !writable, job.prog.clone(), job.id.clone(), job.cancel.clone());
            self.sql_job = Some(job);
            self.ok(format!("Running {}", f.name));
            *keep = false;
        } else if unlock {
            *keep = false;
            let (conn, db, schema, src, name, conn_name) = (f.conn, f.db.clone(), f.schema.clone(), f.src.clone(), f.name.clone(), f.conn_name.clone());
            self.modal = Some(Modal::Unlock(conn, Some(Box::new(move |a: &mut App| a.modal = Some(Modal::SqlRunFile(Box::new(RunForm { conn, db, schema, conn_name, src, name })))))));
        }
    }

    /// An import or a script finished: report it, keep its undo, and reload what it changed.
    pub(in crate::app) fn sql_job_done(&mut self, req: u64, res: db::Res<JobDone>) {
        let Some(j) = self.sql_job.take_if(|j| j.req == req) else { return };
        // A script can create or drop anything anywhere, and one that failed or was stopped kept what ran before.
        if j.kind == JobKind::Script && j.db.dialect == db::sql::Dialect::Postgres {
            // The database list of a PostgreSQL server is not what a script changes; its schemas and tables are, in the database the
            // run was for, and the tabs of that database follow what the server has now.
            let database = j.db.pg_database();
            self.pg_refresh(j.conn, &database);
            let open: Vec<usize> = (0..self.tabs.len()).filter(|&i| self.tabs[i].sel.conn == j.conn && self.tabs[i].sel.db == database && !self.tabs[i].sel.coll.is_empty()).collect();
            for i in open {
                self.reload_of(i);
            }
        } else if j.kind == JobKind::Script {
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
                    // A tab is the table's when its database and namespace, resolved as a read of it resolves them, are the job's: another
                    // PostgreSQL database or schema can hold a table of the same name.
                    let tabs: Vec<usize> = (0..self.tabs.len()).filter(|&i| d.table.as_ref().is_some_and(|t| crate::app::sql::alter::answers_tab(&self.tabs[i].sel, self.sql_target(&self.tabs[i].sel).map(|(x, ns)| (x.pg_database(), ns)), (j.conn, &d.database, &d.schema, t)))).collect();
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
    pub(in crate::app) fn sql_lists_done(&mut self, conn: u64, schema: String, res: db::Res<(Vec<String>, Option<Vec<String>>)>) {
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
    pub(in crate::app) fn sql_job_progress(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(j) = &self.sql_job else { return false };
        spin(ui);
        let (rows, bytes) = (j.prog.rows.load(Ordering::Relaxed), j.prog.bytes.load(Ordering::Relaxed));
        let text = match j.kind {
            JobKind::Import => format!("Importing {} of {} rows", group(&rows.to_string()), group(&j.total_rows.to_string())),
            JobKind::Script => format!("Running statement {}, {}%", group(&rows.to_string()), (bytes * 100).checked_div(j.total_bytes).map_or(0, |p| p.min(100))),
        };
        ui.label(RichText::new(text).size(TXT_CAPTION).color(MUTED()));
        if quiet(ui, RichText::new("Cancel").size(TXT_CAPTION)).on_hover_text(if j.kind == JobKind::Import && j.rolls_back { "Stop and roll everything back" } else if j.kind == JobKind::Import { "Stop. This table's engine cannot roll back, so rows written so far stay." } else { "Interrupt the running statement and stop. Statements that already ran stay applied." }).clicked() {
            self.worker.sql_kill(j.db.clone(), j.id.load(Ordering::SeqCst), j.cancel.clone());
        }
        ui.ctx().request_repaint_after(Duration::from_millis(200));
        true
    }

    /// QUARRY_TEST_SQL=dump: the Export dialog of the design with the shop database.
    pub(in crate::app) fn demo_sql_dump(&mut self) {
        let conn = self.t().map_or(0, |t| t.sel.conn);
        let row = |name: &str, on: bool, content: bool, rows: u64| Row { name: name.into(), view: false, on, structure: on, content, drop: false, rows: Some(rows), bytes: Some(rows * 180) };
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
            Row { name: "revenue_by_day".into(), view: true, on: false, structure: false, content: false, drop: false, rows: None, bytes: None },
        ];
        self.modal = Some(Modal::SqlDump(Box::new(DumpForm { conn, db: "shop".into(), schema: String::new(), conn_name: "shop-prod".into(), what: What::Selected, filtered: Some(("orders".into(), "status = 'paid'".into(), 312)), rows, extended: true, snapshot: false, extras: true, grants: false, owners: false, path: "~/Downloads/shop-2026-11-09.sql".into(), size_req: 0 })));
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

#[cfg(test)]
mod size_tests {
    #[test]
    fn sizes_read_in_the_largest_unit() {
        assert_eq!(super::human_size(512), "512 bytes");
        assert_eq!(super::human_size(1536), "1.5 KB");
        assert_eq!(super::human_size(220 * 1024 * 1024), "220 MB");
    }
}
