//! The Import dialog of a SQL table: a CSV file, how to read it, which column goes where, and what the check of
//! every value found. The check reads the whole file in a thread of its own, so the dialog stays quick on a big file.

use super::*;
use db::sql::csv::{self, DateFmt, Enc, Mapping, Opts, Report, Source, TableInfo};
use db::sql::import::{self, Dup, ImportPlan};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

/// The file being imported and everything read from it.
pub(super) struct FileState {
    src: Source,
    name: String,
    size: u64,
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

pub(super) struct ImportForm {
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
}

fn hash_of(v: &impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

impl FileState {
    fn open(src: Source, name: String) -> Result<FileState, String> {
        let (opts, header) = csv::sniff(&src)?;
        let mut f = FileState { size: src.len(), src, name, opts, header, detected: opts.enc, names: Vec::new(), sample: Vec::new(), map: Mapping { map: Vec::new(), fmts: Vec::new(), empty_null: true }, read_key: u64::MAX, check_key: u64::MAX, report: None, rx: None, cancel: Arc::new(AtomicBool::new(false)), epoch: 0 };
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
        if let Some(t) = self.map.map[ci] {
            if matches!(info.cols[t].kind, db::sql::value::Kind::Date | db::sql::value::Kind::DateTime) {
                self.map.fmts[ci] = csv::guess_date_fmt(&self.values(ci));
            }
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
        if let Some((epoch, rx)) = &self.rx {
            if let Ok((g, rep)) = rx.try_recv() {
                if g == *epoch {
                    self.report = Some(rep);
                    self.rx = None;
                }
            }
        }
    }
}

impl Drop for FileState {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn pick_csv() -> Option<std::path::PathBuf> {
    rfd::FileDialog::new().add_filter("CSV", &["csv", "tsv", "txt"]).pick_file()
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
    pub(super) fn open_sql_import(&mut self, conn: u64, db: String, schema: String, table: String) {
        self.open_sql_import_with(conn, db, schema, table, None);
    }

    /// `src` is the file to read; None asks for one in the system dialog, so a test hands it in.
    pub(super) fn open_sql_import_with(&mut self, conn: u64, db: String, schema: String, table: String, src: Option<Source>) {
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
            Source::File(p) => p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            Source::Mem(_) => "data.csv".into(),
        };
        let file = match FileState::open(src, name) {
            Ok(f) => f,
            Err(e) => return self.err(e),
        };
        self.req += 1;
        self.worker.sql_table_info(self.req, sql, ns, table.clone());
        self.modal = Some(Modal::SqlImport(Box::new(ImportForm { conn, db, schema, table, req: self.req, info: None, file, dup: Dup::Stop })));
    }

    /// The columns of the table arrived.
    pub(super) fn import_info(&mut self, req: u64, res: db::Res<TableInfo>) {
        let Some(Modal::SqlImport(f)) = &mut self.modal else { return };
        if f.req != req {
            return;
        }
        f.info = Some(match res {
            Ok(info) => {
                f.file.remap(&info);
                Ok(info)
            }
            Err(e) => Err(e),
        });
    }

    pub(super) fn sql_import_dialog(&mut self, ui: &mut egui::Ui, f: &mut ImportForm, keep: &mut bool) {
        ui.set_width(720.0);
        dialog_title(ui, format!("Import into {}", f.table));
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
            Some(n) => format!("{} \u{b7} {} rows \u{b7} {}", f.file.name, group(&n.to_string()), human_bytes(f.file.size as usize)),
            None => format!("{} \u{b7} {}", f.file.name, human_bytes(f.file.size as usize)),
        };
        ui.label(RichText::new(sub).size(TXT_SMALL).color(MUTED()));
        ui.add_space(8.0);
        match &f.info {
            None => {
                ui.horizontal(|ui| {
                    spin(ui);
                    ui.label(RichText::new("Reading the columns of the table").color(MUTED()));
                });
                ui.add_space(8.0);
                if dialog_actions(ui, |ui| ui.button("Cancel").clicked()) {
                    *keep = false;
                }
                return;
            }
            Some(Err(e)) => {
                ui.label(RichText::new(e.as_str()).color(ALERT()));
                if dialog_actions(ui, |ui| ui.button("Close").clicked()) {
                    *keep = false;
                }
                return;
            }
            Some(Ok(_)) => {}
        }
        let Some(Ok(info)) = f.info.clone() else { return };
        egui::Grid::new("imp-opts").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W + 10.0).show(ui, |ui| {
            form_label(ui, "Separator");
            ui.horizontal(|ui| {
                segmented(ui, &mut f.file.opts.delim, &[(',', ","), (';', ";"), ('\t', "Tab"), ('|', "|")]);
                ui.add_space(14.0);
                ui.checkbox(&mut f.file.header, "First row is the header");
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
                    (dup, true) => db::sql::import_pg::clause_text(&info, &f.file.map, dup),
                };
                ui.label(RichText::new(how).font(FontId::monospace(TXT_CAPTION)).color(FAINT()));
            });
            ui.end_row();
            form_label(ui, "Reading");
            ui.horizontal(|ui| {
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
                ui.label(RichText::new("Empty is").size(TXT_SMALL).color(MUTED())).on_hover_text("What an empty value becomes in a text column. Other columns always get NULL.");
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
                    for h in ["CSV column", "Table column", "First values", "Check"] {
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
                        let vals: Vec<String> = f.file.values(ci).into_iter().take(3).map(|v| csv::short(v)).collect();
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
        let per = if info.pg { db::sql::import_pg::batch_rows(mapped.len()) } else { 500.min(60_000 / mapped.len().max(1)).max(1) };
        egui::Frame::new().fill(PANEL()).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let say = |ui: &mut egui::Ui, t: String, c: Color32| ui.add(egui::Label::new(RichText::new(t).size(TXT_SMALL).color(c)).wrap());
            say(ui, format!("Runs in one transaction in batches of {per}. {}", match &undo {
                Ok(()) if info.pg && f.dup == Dup::Update => format!("Undo removes the new rows and puts back what an update overwrote, if nothing changed since."),
                Ok(()) if info.pg => format!("Undo removes the new rows again, if nothing changed since."),
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
        let (mut start, mut back, mut unlock) = (false, false, false);
        ui.add_space(8.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let label = if rep.is_some() { format!("Import {} rows", group(&rows.to_string())) } else { "Import".to_string() };
            let r = ui.add_enabled(blocked.is_none(), primary_button(ui, label, PRIMARY()));
            start = r.clicked();
            if let Some(why) = blocked {
                r.on_disabled_hover_text(why);
            }
            back = ui.button("Back").on_hover_text("Choose another file").clicked();
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
        } else if back {
            if let Some(p) = pick_csv() {
                match FileState::open(Source::File(p.clone()), p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()) {
                    Ok(mut file) => {
                        file.remap(&info);
                        f.file = file;
                    }
                    Err(e) => self.err(e),
                }
            }
        } else if unlock {
            *keep = false;
            let (conn, db, schema, table, src) = (f.conn, f.db.clone(), f.schema.clone(), f.table.clone(), f.file.src.clone());
            self.modal = Some(Modal::Unlock(conn, Some(Box::new(move |a: &mut App| a.open_sql_import_with(conn, db, schema, table, Some(src))))));
        }
    }

    fn start_sql_import(&mut self, f: &ImportForm, info: &TableInfo, want_undo: bool) {
        let Some(writable) = self.conn(f.conn).map(|c| c.writable) else { return self.warn_offline() };
        let Some((sql, ns)) = self.sql_target(&Sel { conn: f.conn, db: f.db.clone(), schema: f.schema.clone(), coll: f.table.clone() }) else { return self.warn_offline() };
        let plan = ImportPlan { source: f.file.src.clone(), opts: f.file.opts, header: f.file.header, info: info.clone(), map: f.file.map.clone(), dup: f.dup, want_undo };
        self.req += 1;
        let mut job = super::sql_dump::SqlJob::new(self.req, f.conn, sql.clone(), ns.clone(), super::sql_dump::JobKind::Import, f.file.report.as_ref().map_or(0, |r| r.rows), f.file.size);
        job.rolls_back = info.transactional();
        self.worker.sql_import(job.req, f.conn, sql, ns, f.table.clone(), plan, !writable, job.prog.clone(), job.id.clone(), job.cancel.clone());
        self.sql_job = Some(job);
        self.ok(format!("Importing {} rows into {}", group(&job_rows(&self.sql_job).to_string()), f.table));
    }

    /// QUARRY_TEST_SQL=import: the dialog over the orders table with a file that has one bad value, built offline.
    pub(super) fn demo_sql_import(&mut self) {
        let mut csv = String::from("order_id,customer,state,amount,ccy,created,source\n");
        for i in 0..1240u32 {
            let amount = if i == 5 { "twelve".to_string() } else { format!("{}.{:02}", 20 + i % 90, (i * 7) % 100) };
            csv.push_str(&format!("{},{},{},{},{},{:02}/11/2026 12:41,{}\n", 4410 + i, 800 + i % 700, ["paid", "shipped", "paid"][i as usize % 3], amount, ["EUR", "EUR", "USD"][i as usize % 3], 1 + i % 28, ["web", "app", "web"][i as usize % 3]));
        }
        let src = Source::Mem(Arc::new(csv.into_bytes()));
        let Ok(mut file) = FileState::open(src, "orders-2026-11.csv".into()) else { return };
        file.size = 182 * 1024;
        let col = |name: &str, ty: &str, nullable: bool, auto: bool| db::sql::csv::TCol::from_column(&db::sql::structure::Column { name: name.into(), full_type: ty.into(), nullable, extra: if auto { "auto_increment".into() } else { String::new() }, ..Default::default() }, false);
        let info = TableInfo { cols: vec![col("id", "int unsigned", false, true), col("customer_id", "int unsigned", false, false), col("status", "enum('pending','paid','shipped','refunded')", false, false), col("total", "decimal(10,2)", false, false), col("currency", "char(3)", false, false), col("created_at", "datetime", false, false)], pk: vec!["id".into()], engine: "InnoDB".into(), pg: false, pk_deferrable: false };
        file.remap(&info);
        // The design maps the file's names to these columns, which only differ in spelling.
        file.map.map = vec![Some(0), Some(1), Some(2), Some(3), Some(4), Some(5), None];
        file.refmt(5, &info);
        let conn = self.t().map_or(0, |t| t.sel.conn);
        self.modal = Some(Modal::SqlImport(Box::new(ImportForm { conn, db: "shop".into(), schema: String::new(), table: "orders".into(), req: 0, info: Some(Ok(info)), file, dup: Dup::Stop })));
    }
}

fn job_rows(j: &Option<super::sql_dump::SqlJob>) -> u64 {
    j.as_ref().map_or(0, |j| j.total_rows)
}
