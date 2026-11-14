//! The Import dialog of a SQL table: a CSV, JSON or JSON Lines file (or pasted text), how to read it, which column goes
//! where, and what the check of every value found. The check reads the whole file in a thread of its own, so the dialog
//! stays quick on a big file. JSON is flattened to CSV once, when it is opened, and everything after reads that CSV.

use crate::app::*;
use db::sql::csv::{self, DateFmt, Enc, Mapping, Opts, Report, Source, TableInfo};
use db::sql::json;
use db::sql::import::{self, Dup, ImportPlan};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

/// The file being imported and everything read from it.
pub(in crate::app) struct FileState {
    /// What the checks and the import read: the file itself for CSV, the CSV a JSON file was flattened to.
    src: Source,
    /// What the person gave, which Back and an unlock open again.
    origin: Source,
    name: String,
    /// Bytes of `src`, which the progress is counted in, and of `origin`, which the dialog shows.
    size: u64,
    shown_size: u64,
    /// Set for JSON and JSON Lines.
    json: Option<JsonFile>,
    opts: Opts,
    header: bool,
    /// What the encoding looked like when the file was opened, shown beside the choice.
    detected: Enc,
    names: Vec<String>,
    /// The first records after the header, for the values column and the date reading.
    sample: Vec<Vec<String>>,
    map: Mapping,
    /// Hash of what the sample was read with, and of what the check was run for.
    read_key: u64,
    check_key: u64,
    report: Option<Report>,
    rx: Option<(u64, mpsc::Receiver<(u64, Report)>)>,
    cancel: Arc<AtomicBool>,
    epoch: u64,
}

pub(in crate::app) struct ImportForm {
    conn: u64,
    /// The database on MySQL, the PostgreSQL database of the connection otherwise.
    db: String,
    /// The PostgreSQL schema of the table, empty on MySQL where the database is the whole namespace.
    schema: String,
    table: String,
    req: u64,
    info: Option<Result<TableInfo, String>>,
    file: FileState,
    dup: Dup,
    /// The mapping chosen before an unlock reopened the form, put back once the table's columns are read again.
    restore: Option<csv::Mapping>,
}

fn hash_of(v: &impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

/// What a JSON file said about itself when it was flattened.
struct JsonFile {
    /// One object per line rather than one array.
    lines: bool,
    /// By key: whether any row holds an object or array there, which is written as JSON text.
    nested: Vec<bool>,
}

impl FileState {
    /// `name` is the file's name; None for pasted text, which is named by what it turned out to be.
    fn open(origin: Source, name: Option<String>) -> Result<FileState, String> {
        let head = origin.head(64 << 10).map_err(|e| format!("Could not read the file: {e}"))?;
        let is_json = json::looks_like_json(name.as_deref().unwrap_or(""), &head);
        let name = name.unwrap_or_else(|| if is_json { "Pasted JSON" } else { "Pasted CSV" }.into());
        let blank = |src: Source, opts: Opts, header: bool, json: Option<JsonFile>| FileState { size: src.len(), shown_size: origin.len(), origin: origin.clone(), src, json, name: name.clone(), opts, header, detected: opts.enc, names: Vec::new(), sample: Vec::new(), map: Mapping { map: Vec::new(), fmts: Vec::new(), empty_null: true }, read_key: u64::MAX, check_key: u64::MAX, report: None, rx: None, cancel: Arc::new(AtomicBool::new(false)), epoch: 0 };
        let mut f = if is_json {
            let flat = json::flatten(&json::read_text(&origin)?)?;
            // The flattened text is plain UTF-8 CSV, so the reading options are fixed and the dialog does not offer them.
            let opts = Opts { delim: ',', quote: Some('"'), enc: Enc::Utf8 };
            blank(Source::Mem(Arc::new(flat.csv)), opts, true, Some(JsonFile { lines: flat.lines, nested: flat.nested }))
        } else {
            let (opts, header) = csv::sniff(&origin)?;
            blank(origin.clone(), opts, header, None)
        };
        f.resample()?;
        Ok(f)
    }

    /// Reads the first records again with the options as they are now.
    fn resample(&mut self) -> Result<(), String> {
        let rows = csv::sample(&self.src, self.opts, 200)?;
        let width = rows.first().map_or(0, Vec::len);
        (self.names, self.sample) = if self.header && !rows.is_empty() { (rows[0].clone(), rows[1..].to_vec()) } else { ((1..=width).map(|i| format!("Column {i}")).collect(), rows) };
        self.map.map = vec![None; width];
        self.map.fmts = vec![DateFmt::DayFirst; width];
        self.read_key = hash_of(&(self.opts, self.header));
        Ok(())
    }

    fn values(&self, ci: usize) -> Vec<&str> {
        self.sample.iter().filter_map(|r| r.get(ci).map(String::as_str)).collect()
    }

    /// Matches columns by name and reads each date column the way its values say.
    fn remap(&mut self, info: &TableInfo) {
        self.map.map = csv::auto_map(&self.names, &info.cols);
        for ci in 0..self.map.map.len() {
            self.refmt(ci, info);
        }
    }

    fn refmt(&mut self, ci: usize, info: &TableInfo) {
        if let Some(t) = self.map.map[ci] && matches!(info.cols[t].kind, db::sql::value::Kind::Date | db::sql::value::Kind::DateTime) {
            self.map.fmts[ci] = csv::guess_date_fmt(&self.values(ci));
        }
    }

    /// Starts the check again when anything it depends on changed.
    fn recheck(&mut self, info: &TableInfo, ctx: &egui::Context) {
        let key = hash_of(&(self.opts, self.header, &self.map.map, &self.map.fmts, self.map.empty_null));
        if key == self.check_key {
            return;
        }
        self.check_key = key;
        self.cancel.store(true, Ordering::Relaxed);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.report = None;
        self.epoch += 1;
        let (tx, rx) = mpsc::channel();
        self.rx = Some((self.epoch, rx));
        let (src, opts, header, cols, map, width, cancel, epoch, ctx) = (self.src.clone(), self.opts, self.header, info.cols.clone(), self.map.clone(), self.names.len(), self.cancel.clone(), self.epoch, ctx.clone());
        std::thread::spawn(move || {
            let rep = csv::analyze(&src, opts, header, &cols, &map, width, &cancel);
            if !rep.cancelled {
                let _ = tx.send((epoch, rep));
                ctx.request_repaint();
            }
        });
    }

    fn poll(&mut self) {
        if let Some((epoch, rx)) = &self.rx && let Ok((g, rep)) = rx.try_recv() && g == *epoch {
            self.report = Some(rep);
            self.rx = None;
        }
    }
}

impl Drop for FileState {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The text on the clipboard as an import source, or why there is none.
fn clipboard_source() -> Result<Source, String> {
    let text = arboard::Clipboard::new().and_then(|mut c| c.get_text()).map_err(|e| format!("Could not read the clipboard: {e}"))?;
    if text.trim().is_empty() {
        return Err("The clipboard holds no text.".into());
    }
    Ok(Source::Mem(Arc::new(text.into_bytes())))
}

fn pick_csv() -> Option<std::path::PathBuf> {
    rfd::FileDialog::new().add_filter("CSV, JSON and JSON Lines", &["csv", "tsv", "txt", "json", "jsonl", "ndjson"]).pick_file()
}

fn quote_label(q: Option<char>) -> &'static str {
    match q {
        Some('"') => "\"",
        Some('\'') => "'",
        _ => "None",
    }
}

impl App {
    /// Opens the Import dialog for `table`, asking for the file first.
    pub(in crate::app) fn open_sql_import(&mut self, conn: u64, db: String, schema: String, table: String) {
        self.open_sql_import_with(conn, db, schema, table, None);
    }

    /// Opens the Import dialog for `table` over the text on the clipboard, CSV or JSON.
    pub(in crate::app) fn open_sql_import_paste(&mut self, conn: u64, db: String, schema: String, table: String) {
        match clipboard_source() {
            Ok(src) => self.open_sql_import_with(conn, db, schema, table, Some(src)),
            Err(e) => self.warn(e),
        }
    }

    /// `src` is the file to read; None asks for one in the system dialog, so a test hands it in.
    pub(in crate::app) fn open_sql_import_with(&mut self, conn: u64, db: String, schema: String, table: String, src: Option<Source>) {
        // The pool of the table's database and the namespace a worker call takes: the schema on PostgreSQL, the database on MySQL.
        let Some((sql, ns)) = self.sql_target(&Sel { conn, db: db.clone(), schema: schema.clone(), coll: table.clone() }) else { return self.warn_offline() };
        if self.demo {
            return self.ok("Offline demo: no database is connected.");
        }
        let src = match src {
            Some(s) => s,
            None => match pick_csv() {
                Some(p) => Source::File(p),
                None => return,
            },
        };
        let name = match &src {
            Source::File(p) => Some(p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
            Source::Mem(_) => None,
        };
        let file = match FileState::open(src, name) {
            Ok(f) => f,
            Err(e) => return self.err(e),
        };
        self.req += 1;
        self.worker.sql_table_info(self.req, sql, ns, table.clone());
        self.modal = Some(Modal::SqlImport(Box::new(ImportForm { conn, db, schema, table, req: self.req, info: None, file, dup: Dup::Stop, restore: None })));
    }

    /// The columns of the table arrived.
    pub(in crate::app) fn import_info(&mut self, req: u64, res: db::Res<TableInfo>) {
        let Some(Modal::SqlImport(f)) = &mut self.modal else { return };
        if f.req != req {
            return;
        }
        f.info = Some(match res {
            Ok(info) => {
                f.file.remap(&info);
                if let Some(m) = f.restore.take().filter(|m| m.map.len() == f.file.map.map.len()) {
                    f.file.map = m;
                }
                Ok(info)
            }
            Err(e) => Err(e),
        });
    }

    pub(in crate::app) fn sql_import_dialog(&mut self, ui: &mut egui::Ui, f: &mut ImportForm, keep: &mut bool) {
        ui.set_width(720.0);
        dialog_head(ui, &Head::new(format!("Import into {}", f.table)).glyph(glyphs::upload));
        let mut b = body_begin(ui);
        let outer = ui;
        let ui = &mut b.content_ui;
        let writable = self.conn_writable(f.conn);
        f.file.poll();
        let ctx = ui.ctx().clone();
        // A changed separator, quote, encoding or header row reads the first records again.
        if hash_of(&(f.file.opts, f.file.header)) != f.file.read_key {
            if let Err(e) = f.file.resample() {
                f.file.report = Some(Report { error: Some(e), ..Default::default() });
            }
            if let Some(Ok(info)) = &f.info {
                f.file.remap(info);
            }
        }
        if let Some(Ok(info)) = &f.info {
            f.file.recheck(info, &ctx);
        }
        let rep_rows = f.file.report.as_ref().map(|r| r.rows);
        let sub = match rep_rows {
            Some(n) => format!("{} \u{b7} {} rows \u{b7} {}", f.file.name, group(&n.to_string()), human_bytes(f.file.shown_size as usize)),
            None => format!("{} \u{b7} {}", f.file.name, human_bytes(f.file.shown_size as usize)),
        };
        ui.label(RichText::new(sub).size(TXT_SMALL).color(MUTED()));
        ui.add_space(8.0);
        match &f.info {
            None => {
                ui.horizontal(|ui| {
                    spin(ui);
                    ui.label(RichText::new("Reading the columns of the table").color(MUTED()));
                });
                b.end(outer);
                if dialog_foot(outer, |ui| ui.button("Cancel").clicked()) {
                    *keep = false;
                }
                return;
            }
            Some(Err(e)) => {
                ui.label(RichText::new(e.as_str()).color(ALERT()));
                b.end(outer);
                let ui = outer;
                if dialog_foot(ui, |ui| ui.button("Close").clicked()) {
                    *keep = false;
                }
                return;
            }
            Some(Ok(_)) => {}
        }
        let Some(Ok(info)) = f.info.clone() else { return };
        egui::Grid::new("imp-opts").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W + 10.0).show(ui, |ui| {
            form_label(ui, if f.file.json.is_some() { "Format" } else { "Separator" });
            ui.horizontal(|ui| {
                if let Some(j) = &f.file.json {
                    ui.label(RichText::new(if j.lines { "JSON Lines, one object per line" } else { "JSON array of objects" }).size(TXT_SMALL).color(MUTED()));
                    ui.add_space(8.0);
                    ui.label(RichText::new("Each key is a column. Nested objects and arrays are written as JSON text.").size(TXT_CAPTION).color(FAINT()));
                } else {
                    segmented(ui, &mut f.file.opts.delim, &[(',', ","), (';', ";"), ('\t', "Tab"), ('|', "|")]);
                    ui.add_space(14.0);
                    ui.checkbox(&mut f.file.header, "First row is the header");
                }
            });
            ui.end_row();
            form_label(ui, "Duplicate keys");
            ui.horizontal(|ui| {
                segmented(ui, &mut f.dup, &[(Dup::Stop, "Stop"), (Dup::Skip, "Skip"), (Dup::Update, "Update")]);
                ui.add_space(14.0);
                let how = match (f.dup, info.pg) {
                    (Dup::Stop, _) => "INSERT, STOPS AT A DUPLICATE",
                    (Dup::Skip, false) => "INSERT ... ON DUPLICATE KEY, NO CHANGE",
                    (Dup::Update, false) => "INSERT ... ON DUPLICATE KEY UPDATE",
                    // The statement names the key only when the table has one, and updates only when the file fills a column besides it.
                    (dup, true) => db::sql::pg::import::clause_text(&info, &f.file.map, dup),
                };
                ui.label(RichText::new(how).font(FontId::monospace(TXT_CAPTION)).color(FAINT()));
            });
            ui.end_row();
            form_label(ui, "Reading");
            ui.horizontal(|ui| {
                // A JSON file was read once, with the encoding its bytes show, so only CSV has these to choose.
                if f.file.json.is_none() {
                    ui.label(RichText::new("Quote").size(TXT_SMALL).color(MUTED()));
                    segmented(ui, &mut f.file.opts.quote, &[(Some('"'), quote_label(Some('"'))), (Some('\''), quote_label(Some('\''))), (None, "None")]);
                    ui.add_space(8.0);
                    ui.label(RichText::new("Encoding").size(TXT_SMALL).color(MUTED()));
                    let det = f.file.detected;
                    egui::ComboBox::from_id_salt("imp-enc").width(130.0).selected_text(f.file.opts.enc.label()).show_ui(ui, |ui| {
                        menu_look(ui);
                        for (e, l) in Enc::ALL {
                            ui.selectable_value(&mut f.file.opts.enc, e, if e == det { format!("{l} (detected)") } else { l.to_string() });
                        }
                    });
                    ui.add_space(8.0);
                }
                let tip = if f.file.json.is_some() { "What a key an object lacks, and an empty string, become in a text column. Other columns always get NULL, and a JSON null is always NULL." } else { "What an empty value becomes in a text column. Other columns always get NULL." };
                ui.label(RichText::new("Empty is").size(TXT_SMALL).color(MUTED())).on_hover_text(tip);
                segmented(ui, &mut f.file.map.empty_null, &[(true, "NULL"), (false, "Empty text")]);
            });
            ui.end_row();
        });
        ui.add_space(4.0);
        caption(ui, "COLUMNS");
        ui.add_space(2.0);
        let mut set: Option<(usize, Option<usize>)> = None;
        let mut flip: Option<usize> = None;
        let rep = f.file.report.clone();
        egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("imp-cols").max_height(270.0).auto_shrink([false, true]).show(ui, |ui| {
                egui::Grid::new("imp-cols").num_columns(4).spacing([16.0, 7.0]).show(ui, |ui| {
                    for h in [if f.file.json.is_some() { "Key" } else { "CSV column" }, "Table column", "First values", "Check"] {
                        ui.label(RichText::new(h).size(TXT_SMALL).color(MUTED()));
                    }
                    ui.end_row();
                    for ci in 0..f.file.names.len() {
                        ui.allocate_ui_with_layout(egui::vec2(110.0, BUTTON_H), egui::Layout::left_to_right(egui::Align::Center), |ui| ui.add(egui::Label::new(RichText::new(&f.file.names[ci]).size(TXT_BODY).color(TEXT())).truncate()));
                        let cur = f.file.map.map[ci];
                        let (label, color) = match cur {
                            Some(t) => (info.cols[t].name.clone(), C_LINK()),
                            None => ("Skip".to_string(), FAINT()),
                        };
                        egui::ComboBox::from_id_salt(("imp-map", ci)).width(150.0).selected_text(RichText::new(label).font(FontId::monospace(TXT_SMALL)).color(color)).show_ui(ui, |ui| {
                            menu_look(ui);
                            if ui.selectable_label(cur.is_none(), "Skip").clicked() {
                                set = Some((ci, None));
                            }
                            for (t, c) in info.cols.iter().enumerate().filter(|(_, c)| !c.generated) {
                                if ui.selectable_label(cur == Some(t), format!("{}  {}", c.name, c.full_type)).clicked() {
                                    set = Some((ci, Some(t)));
                                }
                            }
                        });
                        let vals: Vec<String> = f.file.values(ci).into_iter().take(3).map(csv::short).collect();
                        ui.allocate_ui_with_layout(egui::vec2(190.0, BUTTON_H), egui::Layout::left_to_right(egui::Align::Center), |ui| ui.add(egui::Label::new(RichText::new(vals.join(", ")).font(FontId::monospace(TXT_SMALL)).color(FAINT())).truncate()));
                        let mono = |t: String, c: Color32| RichText::new(t).font(FontId::monospace(TXT_SMALL)).color(c);
                        match (cur, &rep) {
                            (None, _) => {
                                ui.label(mono("Not imported".into(), FAINT()));
                            }
                            (Some(_), None) => {
                                ui.label(mono("Checking".into(), FAINT()));
                            }
                            (Some(_), Some(r)) if r.cols.get(ci).is_some_and(|c| c.bad > 0) => {
                                let c = &r.cols[ci];
                                let tip = c.first.iter().map(|p| format!("Row {}: {} {}", p.row, if p.value.is_empty() { String::new() } else { format!("\"{}\"", p.value) }, p.msg)).collect::<Vec<_>>().join("\n");
                                ui.label(mono(format!("{} {}", group(&c.bad.to_string()), if c.bad == 1 { "problem" } else { "problems" }), ALERT())).on_hover_text(tip);
                            }
                            (Some(t), Some(_)) => {
                                let dates = matches!(info.cols[t].kind, db::sql::value::Kind::Date | db::sql::value::Kind::DateTime) && csv::has_local_dates(&f.file.values(ci));
                                if dates {
                                    let how = if f.file.map.fmts[ci] == DateFmt::DayFirst { "Read as day/month/year" } else { "Read as month/day/year" };
                                    if ui.add(egui::Label::new(mono(how.into(), WARNING())).sense(Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Click to read the dates the other way round").clicked() {
                                        flip = Some(ci);
                                    }
                                } else if f.file.json.as_ref().is_some_and(|j| j.nested.get(ci).copied().unwrap_or(false)) && info.cols[t].kind != db::sql::value::Kind::Json {
                                    ui.label(mono("Written as JSON text".into(), WARNING())).on_hover_text("This key holds objects or arrays, and the column is not a JSON column, so the value is stored as its JSON text.");
                                } else {
                                    ui.label(mono("OK".into(), SUCCESS()));
                                }
                            }
                        }
                        ui.end_row();
                    }
                });
            });
        });
        if let Some((ci, t)) = set {
            if let Some(t) = t {
                // A table column takes one file column, so the one that had it lets go.
                for m in f.file.map.map.iter_mut().filter(|m| **m == Some(t)) {
                    *m = None;
                }
            }
            f.file.map.map[ci] = t;
            f.file.refmt(ci, &info);
        }
        if let Some(ci) = flip {
            f.file.map.fmts[ci] = if f.file.map.fmts[ci] == DateFmt::DayFirst { DateFmt::MonthFirst } else { DateFmt::DayFirst };
        }
        // What the check found outside the column list.
        let rows = rep.as_ref().map_or(0, |r| r.rows);
        if let Some(r) = &rep {
            if let Some(e) = &r.error {
                ui.add_space(4.0);
                ui.label(RichText::new(e.as_str()).size(TXT_SMALL).color(ALERT()));
            }
            for p in r.first.iter().take(5) {
                let col = p.col.and_then(|c| f.file.names.get(c)).map_or(String::new(), |n| format!(", {n}"));
                let val = if p.value.is_empty() { String::new() } else { format!(" \"{}\"", p.value) };
                ui.label(RichText::new(format!("Row {}{col}:{val} {}", p.row, p.msg)).font(FontId::monospace(TXT_SMALL)).color(ALERT()));
            }
            if r.bad_rows > r.first.len().min(5) as u64 {
                ui.label(RichText::new(format!("{} rows have a problem in all.", group(&r.bad_rows.to_string()))).size(TXT_SMALL).color(MUTED()));
            }
        }
        let mapped = f.file.map.targets();
        let missing: Vec<&str> = info.cols.iter().enumerate().filter(|(i, c)| !c.generated && !c.fills_itself && !mapped.iter().any(|(_, t)| t == i)).map(|(_, c)| c.name.as_str()).collect();
        ui.add_space(6.0);
        let undo = import::undo_plan(&info, &f.file.map, f.dup, rows);
        let per = if info.pg { db::sql::pg::import::batch_rows(mapped.len()) } else { 500.min(60_000 / mapped.len().max(1)).max(1) };
        egui::Frame::new().fill(PANEL()).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let say = |ui: &mut egui::Ui, t: String, c: Color32| ui.add(egui::Label::new(RichText::new(t).size(TXT_SMALL).color(c)).wrap());
            say(ui, format!("Runs in one transaction in batches of {per}. {}", match &undo {
                Ok(()) if info.pg && f.dup == Dup::Update => "Undo removes the new rows and puts back what an update overwrote, if nothing changed since.".to_string(),
                Ok(()) if info.pg => "Undo removes the new rows again, if nothing changed since.".to_string(),
                Ok(()) => format!("Undo removes the {} rows again.", group(&rows.to_string())),
                Err(why) => why.clone(),
            }), MUTED());
            if !info.transactional() {
                say(ui, format!("This table's engine ({}) cannot roll back: rows written before an error or Cancel stay.", info.engine), WARNING());
            }
            if !missing.is_empty() {
                say(ui, format!("Not in the file and without a default: {}. The server may refuse the insert.", missing.join(", ")), WARNING());
            }
            if f.file.map.duplicate_target().is_some() {
                say(ui, "Two columns of the file fill the same column of the table.".into(), ALERT());
            }
            if !writable {
                say(ui, "Writes are locked on this connection. Allow writes to import.".into(), WARNING());
            }
        });
        let blocked = if !writable {
            Some("Allow writes on this connection first")
        } else if self.sql_job.is_some() {
            Some("Another import or script is running")
        } else if f.dup == Dup::Update && info.pg && info.pk.is_empty() {
            Some("Update needs a primary key")
        } else if f.dup != Dup::Stop && info.pk_deferrable {
            Some("The primary key is DEFERRABLE, which ON CONFLICT does not support. Choose Stop")
        } else if mapped.is_empty() {
            Some("Map at least one column")
        } else if f.file.map.duplicate_target().is_some() {
            Some("Two columns fill the same column")
        } else if rep.as_ref().is_none_or(|r| r.bad_rows > 0 || r.error.is_some()) {
            Some("Fix what the check found first")
        } else if rows == 0 {
            Some("The file has no rows")
        } else {
            None
        };
        let (mut start, mut back, mut paste, mut unlock) = (false, false, false, false);
        ui.add_space(8.0);
        // One button high: a bare right to left layout fills the height the dialog's scroll area offers, the whole window.
        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), ui.spacing().interact_size.y), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let label = if rep.is_some() { format!("Import {} rows", group(&rows.to_string())) } else { "Import".to_string() };
            let r = ui.add_enabled(blocked.is_none(), primary_button(ui, label, PRIMARY()));
            start = r.clicked();
            if let Some(why) = blocked {
                r.on_disabled_hover_text(why);
            }
            back = ui.button("Back").on_hover_text("Choose another file").clicked();
            paste = ui.button("Paste").on_hover_text("Import the text on the clipboard instead, CSV or JSON").clicked();
            if ui.button("Cancel").clicked() {
                *keep = false;
            }
            if !writable {
                unlock = quiet(ui, "Allow writes").clicked();
            }
        });
        if start {
            self.start_sql_import(f, &info, undo.is_ok());
            *keep = false;
        } else if back || paste {
            let next = if back { pick_csv().map(|p| (Source::File(p.clone()), p.file_name().map(|n| n.to_string_lossy().into_owned()))) } else { clipboard_source().map(|s| (s, None)).map_err(|e| self.warn(e)).ok() };
            if let Some((src, name)) = next {
                match FileState::open(src, name) {
                    Ok(mut file) => {
                        file.remap(&info);
                        f.file = file;
                    }
                    Err(e) => self.err(e),
                }
            }
        } else if unlock {
            *keep = false;
            let (conn, db, schema, table, src) = (f.conn, f.db.clone(), f.schema.clone(), f.table.clone(), f.file.origin.clone());
            // The form is built again after the unlock, with the choices made in this one.
            let (opts, header, map, dup) = (f.file.opts, f.file.header, f.file.map.clone(), f.dup);
            self.modal = Some(Modal::Unlock(conn, Some(Box::new(move |a: &mut App| {
                a.open_sql_import_with(conn, db, schema, table, Some(src));
                if let Some(Modal::SqlImport(nf)) = &mut a.modal {
                    (nf.file.opts, nf.file.header, nf.dup, nf.restore) = (opts, header, dup, Some(map));
                }
            }))));
        }
    }

    fn start_sql_import(&mut self, f: &ImportForm, info: &TableInfo, want_undo: bool) {
        let Some(writable) = self.conn(f.conn).map(|c| c.writable) else { return self.warn_offline() };
        let Some((sql, ns)) = self.sql_target(&Sel { conn: f.conn, db: f.db.clone(), schema: f.schema.clone(), coll: f.table.clone() }) else { return self.warn_offline() };
        let plan = ImportPlan { source: f.file.src.clone(), opts: f.file.opts, header: f.file.header, info: info.clone(), map: f.file.map.clone(), dup: f.dup, want_undo };
        self.req += 1;
        let mut job = crate::app::sql::dump::SqlJob::new(self.req, f.conn, sql.clone(), ns.clone(), crate::app::sql::dump::JobKind::Import, f.file.report.as_ref().map_or(0, |r| r.rows), f.file.size);
        job.rolls_back = info.transactional();
        self.worker.sql_import(job.req, f.conn, sql, ns, f.table.clone(), plan, !writable, job.prog.clone(), job.id.clone(), job.cancel.clone());
        self.sql_job = Some(job);
        self.ok(format!("Importing {} rows into {}", group(&job_rows(&self.sql_job).to_string()), f.table));
    }

    /// QUARRY_TEST_SQL=import: the dialog over the orders table with a file that has one bad value, built offline.
    pub(in crate::app) fn demo_sql_import(&mut self) {
        let mut csv = String::from("order_id,customer,state,amount,ccy,created,source\n");
        for i in 0..1240u32 {
            let amount = if i == 5 { "twelve".to_string() } else { format!("{}.{:02}", 20 + i % 90, (i * 7) % 100) };
            csv.push_str(&format!("{},{},{},{},{},{:02}/11/2026 12:41,{}\n", 4410 + i, 800 + i % 700, ["paid", "shipped", "paid"][i as usize % 3], amount, ["EUR", "EUR", "USD"][i as usize % 3], 1 + i % 28, ["web", "app", "web"][i as usize % 3]));
        }
        let src = Source::Mem(Arc::new(csv.into_bytes()));
        let Ok(mut file) = FileState::open(src, Some("orders-2026-11.csv".into())) else { return };
        file.size = 182 * 1024;
        file.shown_size = file.size;
        let col = |name: &str, ty: &str, nullable: bool, auto: bool| db::sql::csv::TCol::from_column(&db::sql::structure::Column { name: name.into(), full_type: ty.into(), nullable, extra: if auto { "auto_increment".into() } else { String::new() }, ..Default::default() }, false);
        let info = TableInfo { cols: vec![col("id", "int unsigned", false, true), col("customer_id", "int unsigned", false, false), col("status", "enum('pending','paid','shipped','refunded')", false, false), col("total", "decimal(10,2)", false, false), col("currency", "char(3)", false, false), col("created_at", "datetime", false, false)], pk: vec!["id".into()], engine: "InnoDB".into(), pg: false, pk_deferrable: false };
        file.remap(&info);
        // The design maps the file's names to these columns, which only differ in spelling.
        file.map.map = vec![Some(0), Some(1), Some(2), Some(3), Some(4), Some(5), None];
        file.refmt(5, &info);
        let conn = self.t().map_or(0, |t| t.sel.conn);
        self.modal = Some(Modal::SqlImport(Box::new(ImportForm { conn, db: "shop".into(), schema: String::new(), table: "orders".into(), req: 0, info: Some(Ok(info)), file, dup: Dup::Stop, restore: None })));
    }
}

fn job_rows(j: &Option<crate::app::sql::dump::SqlJob>) -> u64 {
    j.as_ref().map_or(0, |j| j.total_rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(t: &str) -> Source {
        Source::Mem(Arc::new(t.as_bytes().to_vec()))
    }

    fn strings(rows: &[&[&str]]) -> Vec<Vec<String>> {
        rows.iter().map(|r| r.iter().map(|c| c.to_string()).collect()).collect()
    }

    #[test]
    fn json_text_opens_as_columns_named_by_its_keys() {
        let f = FileState::open(mem(r#"[{"id":1,"tags":["a"],"name":"x"},{"id":2,"name":"y"}]"#), None).unwrap();
        assert_eq!(f.names, ["id", "tags", "name"]);
        assert_eq!(f.sample, strings(&[&["1", r#"["a"]"#, "x"], &["2", "", "y"]]));
        assert_eq!(f.name, "Pasted JSON");
        let j = f.json.as_ref().unwrap();
        assert!(!j.lines && j.nested == [false, true, false]);
        // The progress is counted in the text that is read, which is the flattened one.
        assert_eq!(f.size, f.src.len());
    }

    #[test]
    fn json_lines_and_csv_text_are_told_apart_without_a_file_name() {
        let lines = FileState::open(mem("{\"a\":1}\n{\"a\":2}\n"), None).unwrap();
        assert!(lines.json.as_ref().is_some_and(|j| j.lines));
        let csv = FileState::open(mem("a,b\n1,2\n"), None).unwrap();
        assert!(csv.json.is_none());
        assert_eq!((csv.names.clone(), csv.name.as_str()), (vec!["a".to_string(), "b".to_string()], "Pasted CSV"));
    }

    #[test]
    fn flattened_json_is_checked_with_missing_keys_null_and_nested_values_as_text() {
        let col = |name: &str, ty: &str, nullable: bool| csv::TCol::from_column(&db::sql::structure::Column { name: name.into(), full_type: ty.into(), nullable, ..Default::default() }, false);
        let cols = vec![col("id", "int", false), col("name", "varchar(10)", true), col("doc", "json", true)];
        let f = FileState::open(mem(r#"[{"id":1,"name":"a","doc":{"k":[1]}},{"id":2,"doc":null},{"id":3,"name":null}]"#), None).unwrap();
        let map = Mapping { map: csv::auto_map(&f.names, &cols), fmts: vec![DateFmt::DayFirst; f.names.len()], empty_null: true };
        let rep = csv::analyze(&f.src, f.opts, f.header, &cols, &map, f.names.len(), &AtomicBool::new(false));
        assert_eq!((rep.rows, rep.bad_rows, rep.error), (3, 0, None));
        // A key the first object lacks, for a NOT NULL column, is a problem the check reports as an empty CSV value is.
        let f = FileState::open(mem(r#"[{"name":"a"},{"id":2,"name":"b"}]"#), None).unwrap();
        let map = Mapping { map: csv::auto_map(&f.names, &cols), fmts: vec![DateFmt::DayFirst; f.names.len()], empty_null: true };
        let rep = csv::analyze(&f.src, f.opts, f.header, &cols, &map, f.names.len(), &AtomicBool::new(false));
        assert_eq!(rep.bad_rows, 1);
    }
}
