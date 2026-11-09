//! The Export dialog: the rows of a result written to a file in any Copy as format, and the status
//! bar readout of a Query result streaming out.

use super::*;
use db::copy::{self, CopyCol, Fmt, Sheet};
use db::sql::export::{Done, Plan, Sink};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// A format of the dialog: the table formats Copy as shares, or Mongo's own text forms.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Format {
    T(Fmt),
    MongoJs,
    MongoJson,
    MongoLines,
}

impl Format {
    fn label(self) -> &'static str {
        match self {
            Format::T(Fmt::SqlInsert) => "SQL INSERT",
            Format::T(Fmt::SqlUpdate) => "SQL UPDATE",
            Format::T(Fmt::Csv) => "CSV",
            Format::T(Fmt::Tsv) => "TSV",
            Format::T(Fmt::Json) | Format::MongoJson => "JSON",
            Format::T(Fmt::JsonLines) | Format::MongoLines => "JSON lines",
            Format::T(Fmt::Markdown) => "Markdown table",
            Format::T(Fmt::Html) => "HTML table",
            Format::T(Fmt::Values) => "Values only",
            Format::MongoJs => "JS",
        }
    }

    fn ext(self) -> &'static str {
        match self {
            Format::T(Fmt::SqlInsert | Fmt::SqlUpdate) => "sql",
            Format::T(Fmt::Csv) => "csv",
            Format::T(Fmt::Tsv) => "tsv",
            Format::T(Fmt::Json) | Format::MongoJson => "json",
            Format::T(Fmt::JsonLines) | Format::MongoLines => "jsonl",
            Format::T(Fmt::Markdown) => "md",
            Format::T(Fmt::Html) => "html",
            Format::T(Fmt::Values) => "txt",
            Format::MongoJs => "js",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Scope {
    Selected,
    Shown,
    All,
    /// Every statement of the Query editor, run in order on one connection.
    Script,
}

/// Which grid the rows come from.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Src {
    Query,
    Shell,
    Content,
}

/// What it takes to run the statement of a Query result again for every row.
pub(super) struct AllRows {
    pub(super) db: db::sql::Db,
    pub(super) schema: String,
    pub(super) stmt: String,
    pub(super) total: Option<u64>,
    pub(super) paged: bool,
    /// Why this statement may not be streamed, when it may not.
    pub(super) refused: Option<String>,
}

/// The Query editor's whole text as an export source.
pub(super) struct Script {
    pub(super) db: db::sql::Db,
    pub(super) schema: String,
    /// The statements, or why the script may not be exported.
    pub(super) stmts: Result<Vec<String>, String>,
}

pub(super) struct ExportForm {
    pub(super) src: Src,
    pub(super) sql: bool,
    pub(super) fmt: Format,
    pub(super) header: bool,
    pub(super) scope: Scope,
    pub(super) selected: Vec<Document>,
    pub(super) shown: usize,
    pub(super) all: Option<AllRows>,
    pub(super) script: Option<Script>,
    /// For the script: one file instead of one per result.
    pub(super) single: bool,
    /// The result is one page of a longer one.
    pub(super) paged: bool,
}

/// A Query result streaming to a file or the clipboard, with what Cancel needs.
pub(super) struct ExportJob {
    pub(super) req: u64,
    pub(super) count: Arc<AtomicU64>,
    pub(super) cancel: Arc<AtomicBool>,
    pub(super) id: Arc<AtomicU64>,
    pub(super) db: db::sql::Db,
    pub(super) copy: bool,
    /// Progress of a script: the statement being run and how many there are. `of` is 0 for a single statement.
    pub(super) step: Arc<AtomicU64>,
    pub(super) of: usize,
}

/// "1.2M" from a million up, the exact count below.
pub(super) fn human_rows(n: u64) -> String {
    if n >= 1_000_000 { format!("{:.1}M", n as f64 / 1e6) } else { group(&n.to_string()) }
}

/// Keys of the rows in order of first appearance, for results that have no column list.
fn keys_of(rows: &[Document]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for d in rows.iter().take(500) {
        for k in d.keys() {
            if !out.contains(k) {
                out.push(k.clone());
            }
        }
    }
    out
}

impl App {
    /// The rows a grid shows, decoded, and for the Shell the offset of the checked page in them.
    fn shown_rows(&self, src: Src) -> Vec<Document> {
        let Some(t) = self.t() else { return Vec::new() };
        match src {
            Src::Query => t.sql.as_ref().and_then(|s| s.sets.get(s.set_sel.min(s.sets.len().saturating_sub(1)))).map(|x| x.rows.iter().map(db::Row::owned).collect()).unwrap_or_default(),
            Src::Shell => t.shell.docs.iter().map(db::Row::owned).collect(),
            Src::Content => t.docs.iter().map(db::Row::owned).collect(),
        }
    }

    /// The grid of the current view.
    pub(super) fn grid_src(&self) -> Src {
        match self.t().map(|t| (t.view, t.sql.is_some())) {
            Some((View::Shell, _)) => Src::Shell,
            Some((View::Sql, true)) => Src::Query,
            _ => Src::Content,
        }
    }

    /// The checked rows of the current grid, decoded.
    pub(super) fn checked_rows(&self) -> Vec<Document> {
        let src = self.grid_src();
        let Some(t) = self.t() else { return Vec::new() };
        let all = self.shown_rows(src);
        // The Shell pages its rows; its checks are positions in the page on screen.
        let base = if src == Src::Shell { t.shell.page * PAGE_SIZE as usize } else { 0 };
        self.checks().iter().filter_map(|i| all.get(base + i).cloned()).collect()
    }

    /// Columns, table name and key for writing `rows` of `src` as text.
    fn export_sheet(&self, src: Src, rows: &[Document]) -> (Vec<CopyCol>, String, Vec<String>) {
        let Some(t) = self.t() else { return Default::default() };
        let named = |c: &str| if c.is_empty() { "result".to_string() } else { c.to_string() };
        match src {
            Src::Query => {
                let set = t.sql.as_ref().and_then(|s| s.sets.get(s.set_sel.min(s.sets.len().saturating_sub(1))));
                let cols = set.map(|s| s.cols.iter().map(|c| CopyCol { name: c.name.clone(), src: c.src.clone(), numeric: is_numeric(c.kind) }).collect()).unwrap_or_default();
                (cols, named(&t.sel.coll), Vec::new())
            }
            Src::Shell => (keys_of(rows).into_iter().map(|n| CopyCol { numeric: infer_numeric(rows, &n), src: n.clone(), name: n }).collect(), named(&t.sel.coll), Vec::new()),
            Src::Content => (self.copy_cols(rows), t.sel.coll.clone(), t.sql.as_ref().map(|s| s.pk.clone()).unwrap_or_default()),
        }
    }

    /// Opens the dialog for the current grid. `selected` are the rows to offer as "Selected rows".
    pub(super) fn open_export(&mut self, selected: Vec<Document>, all_pages: bool) {
        let src = self.grid_src();
        let Some(t) = self.t() else { return };
        let sql = t.sql.is_some();
        let set = t.sql.as_ref().filter(|_| src == Src::Query).and_then(|s| s.sets.get(s.set_sel.min(s.sets.len().saturating_sub(1))));
        let shown = match src {
            Src::Query => set.map_or(0, |x| x.rows.len()),
            Src::Shell => t.shell.docs.len(),
            Src::Content => t.docs.len(),
        };
        let paged = set.is_some_and(|x| x.page.is_some());
        let all = set.filter(|x| x.error.is_none() && !x.stmt.trim().is_empty()).and_then(|x| {
            let db = self.conn(t.sel.conn).and_then(|c| c.sql.clone())?;
            let refused = db::sql::lex::read_only_check(&x.stmt).err();
            Some(AllRows { db, schema: t.sql.as_ref().map(|s| s.run_db.clone()).filter(|d| !d.is_empty()).unwrap_or_else(|| t.sel.db.clone()), stmt: x.stmt.clone(), total: x.page.and_then(|p| p.total), paged, refused })
        });
        let script = t.sql.as_ref().filter(|_| src == Src::Query).and_then(|s| {
            let db = self.conn(t.sel.conn).and_then(|c| c.sql.clone())?;
            let schema = Some(s.run_db.clone()).filter(|d| !d.is_empty()).unwrap_or_else(|| t.sel.db.clone());
            Some(Script { db, schema, stmts: db::sql::export::check_script(&s.query) })
        });
        let scope = pick_scope(Ask { all_pages, all_ok: all.as_ref().is_some_and(|a| a.refused.is_none()), has_selected: !selected.is_empty() });
        let fmt = if sql { Format::T(Fmt::Csv) } else { Format::MongoJson };
        self.modal = Some(Modal::Export(Box::new(ExportForm { src, sql, fmt, header: true, scope, selected, shown, all, script, single: false, paged })));
    }

    /// Demo only: gives the open dialog the editor script as a source with a pool that never connects, so the scope can be drawn offline.
    /// `text` replaces the editor's text when given, to show a script that passes the check.
    pub(super) fn demo_script_scope(&mut self, text: Option<&str>) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
        let _guard = rt.enter();
        let opts = "mysql://root@127.0.0.1:1/shop".parse::<sqlx::mysql::MySqlConnectOptions>().expect("demo address");
        let db = db::sql::Db { pool: db::sql::Pool::My(sqlx::mysql::MySqlPoolOptions::new().connect_lazy_with(opts.clone())), dialect: db::sql::Dialect::MySql, opts, version: "8.4.3".into(), tls: false, sql_mode: String::new() };
        let query = text.map(str::to_string).or_else(|| self.t().and_then(|t| t.sql.as_ref().map(|s| s.query.clone()))).unwrap_or_default();
        if let Some(Modal::Export(f)) = &mut self.modal {
            f.script = Some(Script { db, schema: "shop".into(), stmts: db::sql::export::check_script(&query) });
            f.scope = Scope::Script;
        }
    }

    pub(super) fn export_dialog(&mut self, ui: &mut egui::Ui, f: &mut ExportForm, keep: &mut bool) {
        ui.set_width(520.0);
        dialog_title(ui, "Export rows");
        let formats: Vec<Format> = if f.sql {
            let mut v = vec![Format::T(Fmt::SqlInsert)];
            // A Query result or a Shell result has no key to write an UPDATE by.
            if f.src == Src::Content {
                v.push(Format::T(Fmt::SqlUpdate));
            }
            v.extend([Fmt::Csv, Fmt::Tsv, Fmt::Json, Fmt::JsonLines, Fmt::Markdown, Fmt::Html, Fmt::Values].map(Format::T));
            v
        } else {
            vec![Format::MongoJs, Format::MongoJson, Format::MongoLines, Format::T(Fmt::Csv), Format::T(Fmt::Tsv), Format::T(Fmt::Markdown), Format::T(Fmt::Html)]
        };
        let noun = if f.sql { "rows" } else { "documents" };
        egui::Grid::new("export-form").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
            form_label(ui, "Format");
            egui::ComboBox::from_id_salt("export-fmt").width(220.0).selected_text(f.fmt.label()).show_ui(ui, |ui| {
                menu_look(ui);
                for x in formats {
                    ui.selectable_value(&mut f.fmt, x, x.label());
                }
            });
            ui.end_row();
            form_label(ui, "Column names");
            let table = !matches!(f.fmt, Format::MongoJs | Format::MongoJson | Format::MongoLines);
            ui.add_enabled_ui(table, |ui| toggle(ui, &mut f.header)).response.on_disabled_hover_text("This format has no header row");
            ui.end_row();
            form_label(ui, "Rows");
            ui.vertical(|ui| {
                ui.add_enabled_ui(!f.selected.is_empty(), |ui| ui.radio_value(&mut f.scope, Scope::Selected, format!("Selected {noun} ({})", group(&f.selected.len().to_string()))));
                let shown = if f.paged { format!("This page ({})", group(&f.shown.to_string())) } else { format!("Shown {noun} ({})", group(&f.shown.to_string())) };
                ui.radio_value(&mut f.scope, Scope::Shown, shown);
                if let Some(a) = &f.all {
                    let what = match (a.paged, a.total) {
                        (true, Some(n)) => format!("All pages ({} rows)", group(&n.to_string())),
                        (true, None) => "All pages".to_string(),
                        _ => "All rows".to_string(),
                    };
                    let table = matches!(f.fmt, Format::T(_));
                    ui.add_enabled_ui(a.refused.is_none() && table, |ui| ui.radio_value(&mut f.scope, Scope::All, format!("{what}: runs the statement again and streams to the file")));
                    if let Some(why) = &a.refused {
                        ui.label(RichText::new(format!("Not available: {why}")).size(TXT_SMALL).color(ALERT()));
                    }
                }
                if let Some(sc) = &f.script {
                    let n = sc.stmts.as_ref().map_or(0, Vec::len);
                    let table = matches!(f.fmt, Format::T(_));
                    ui.add_enabled_ui(sc.stmts.is_ok() && table, |ui| ui.radio_value(&mut f.scope, Scope::Script, format!("All statements in the editor ({n}): runs them in order and streams each result")));
                    if let Err(why) = &sc.stmts {
                        ui.label(RichText::new(format!("Not available: {why}")).size(TXT_SMALL).color(ALERT()));
                    }
                }
            });
            ui.end_row();
            if f.scope == Scope::Script {
                form_label(ui, "Output");
                let one_ok = matches!(f.fmt, Format::T(x) if db::sql::export::can_single(x));
                if !one_ok {
                    f.single = false;
                }
                ui.vertical(|ui| {
                    ui.radio_value(&mut f.single, false, "One file per result (name-1, name-2, and so on)");
                    ui.add_enabled_ui(one_ok, |ui| ui.radio_value(&mut f.single, true, "One file, a section for each statement")).response.on_disabled_hover_text("CSV, TSV and Values only: they have no place for a second header");
                });
                ui.end_row();
            }
        });
        // A format only the Shell and Documents know cannot come from a statement.
        if matches!(f.scope, Scope::All | Scope::Script) && !matches!(f.fmt, Format::T(_)) {
            f.scope = Scope::Shown;
        }
        let ready = match f.scope {
            Scope::Selected => !f.selected.is_empty(),
            Scope::Shown => f.shown > 0,
            Scope::All => f.all.as_ref().is_some_and(|a| a.refused.is_none()),
            Scope::Script => f.script.as_ref().is_some_and(|s| s.stmts.is_ok()),
        };
        dialog_actions(ui, |ui| {
            if ui.add_enabled(ready, primary_button(ui, "Export…", PRIMARY())).clicked() {
                let file = rfd::FileDialog::new().set_file_name(format!("{}.{}", self.export_name(f.src), f.fmt.ext())).add_filter(f.fmt.label(), &[f.fmt.ext()]).save_file();
                if let Some(path) = file {
                    self.export_run(f, path);
                    *keep = false;
                }
            }
            if ui.button("Cancel").clicked() {
                *keep = false;
            }
            // A table of the Content view can also leave as a dump with its structure.
            if f.sql && f.src == Src::Content {
                if quiet(ui, "SQL dump…").on_hover_text("The table with its structure, or the whole database, as a script").clicked() {
                    *keep = false;
                    if let Some(s) = self.t().map(|t| t.sel.clone()) {
                        self.open_sql_dump(s.conn, s.db, Some(s.coll));
                    }
                }
            }
        });
    }

    fn export_name(&self, src: Src) -> String {
        let coll = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
        match (src, coll.is_empty()) {
            (Src::Query, true) => "query-result".into(),
            (Src::Shell, true) => "shell-result".into(),
            (Src::Shell, false) => format!("{coll}-shell"),
            (_, true) => "export".into(),
            (_, false) => coll,
        }
    }

    fn export_run(&mut self, f: &ExportForm, path: PathBuf) {
        let table = self.export_sheet(f.src, &[]).1;
        if f.scope == Scope::All {
            let (Some(a), Format::T(fmt)) = (&f.all, f.fmt) else { return };
            let plan = db::sql::export::Plan { fmt, header: f.header, table, bs: self.sql_bs() };
            self.start_stream(a, plan, Sink::File(path), false);
            return;
        }
        if f.scope == Scope::Script {
            let (Some(sc), Format::T(fmt)) = (&f.script, f.fmt) else { return };
            let Ok(stmts) = &sc.stmts else { return };
            let ap = db::sql::export::AllPlan { plan: Plan { fmt, header: f.header, table, bs: self.sql_bs() }, single: f.single };
            return self.start_script(sc, stmts.clone(), ap, path);
        }
        let rows = if f.scope == Scope::Selected { f.selected.clone() } else { self.shown_rows(f.src) };
        let text = match f.fmt {
            Format::T(fmt) => {
                let (cols, table, pk) = self.export_sheet(f.src, &rows);
                let text = copy::format(fmt, &Sheet { rows: &rows, cols: &cols, table: &table, pk: &pk, bs: self.sql_bs() }, f.header);
                if text.is_empty() {
                    return self.err(if fmt == Fmt::SqlUpdate { "SQL UPDATE needs a table with a primary key." } else { "Nothing to export." });
                }
                text
            }
            Format::MongoJs => db::pretty_shell(&Bson::Array(rows.iter().cloned().map(Bson::Document).collect())),
            Format::MongoJson => db::to_edit_value(&Bson::Array(rows.iter().cloned().map(Bson::Document).collect())),
            Format::MongoLines => rows.iter().map(|d| db::to_edit_line(&Bson::Document(d.clone()))).collect::<Vec<_>>().join("\n"),
        };
        let (dir, name) = (path.parent().map(|p| p.to_path_buf()).unwrap_or_default(), path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        match write_atomic(&dir, &name, format!("{text}\n").as_bytes()) {
            Ok(()) => self.ok(format!("Exported {} {} to {}", rows.len(), if rows.len() == 1 { "row" } else { "rows" }, path.display())),
            Err(e) => self.err(format!("Could not write {}: {e}", path.display())),
        }
    }

    /// Streams a Query result's statement to `sink` in a worker task, tracked in the status bar.
    fn start_stream(&mut self, a: &AllRows, plan: Plan, sink: Sink, copy: bool) {
        if self.export_job.is_some() {
            return self.err("An export is already running.");
        }
        self.req += 1;
        let job = ExportJob { req: self.req, count: Arc::new(AtomicU64::new(0)), cancel: Arc::new(AtomicBool::new(false)), id: Arc::new(AtomicU64::new(0)), db: a.db.clone(), copy, step: Arc::new(AtomicU64::new(0)), of: 0 };
        self.worker.sql_export(job.req, a.db.clone(), a.schema.clone(), a.stmt.clone(), plan, sink, job.count.clone(), job.id.clone(), job.cancel.clone());
        self.export_job = Some(job);
    }

    /// Runs the editor's statements in a worker task and streams their results to files.
    fn start_script(&mut self, sc: &Script, stmts: Vec<String>, ap: db::sql::export::AllPlan, path: PathBuf) {
        if self.export_job.is_some() {
            return self.err("An export is already running.");
        }
        self.req += 1;
        let job = ExportJob { req: self.req, count: Arc::new(AtomicU64::new(0)), cancel: Arc::new(AtomicBool::new(false)), id: Arc::new(AtomicU64::new(0)), db: sc.db.clone(), copy: false, step: Arc::new(AtomicU64::new(0)), of: stmts.len() };
        self.worker.sql_export_all(job.req, sc.db.clone(), sc.schema.clone(), stmts, ap, path, job.count.clone(), job.step.clone(), job.id.clone(), job.cancel.clone());
        self.export_job = Some(job);
    }

    /// Copy as for every page of the selected Query result: the text is built in a worker task.
    pub(super) fn copy_all_pages(&mut self, fmt: Fmt, header: bool) {
        let Some(t) = self.t() else { return };
        let Some(s) = t.sql.as_ref() else { return };
        let Some(set) = s.sets.get(s.set_sel.min(s.sets.len().saturating_sub(1))) else { return };
        if let Err(why) = lex_check_msg(&set.stmt) {
            return self.err(why);
        }
        let Some(db) = self.conn(t.sel.conn).and_then(|c| c.sql.clone()) else { return self.err("Connect first.") };
        let a = AllRows { db, schema: if s.run_db.is_empty() { t.sel.db.clone() } else { s.run_db.clone() }, stmt: set.stmt.clone(), total: None, paged: true, refused: None };
        let table = self.export_sheet(Src::Query, &[]).1;
        let plan = Plan { fmt, header, table, bs: self.sql_bs() };
        self.start_stream(&a, plan, Sink::Clipboard, true);
    }

    /// The finished stream: a file is reported, clipboard text is copied.
    pub(super) fn export_done(&mut self, ctx: &egui::Context, req: u64, res: db::Res<Done>) {
        if self.export_job.as_ref().is_none_or(|j| j.req != req) {
            return;
        }
        self.export_job = None;
        match res {
            Ok(Done { rows, path: Some(p), files, .. }) if files > 1 => self.ok(format!("Exported {} rows to {files} files, from {}", human_rows(rows), p.display())),
            Ok(Done { rows, path: Some(p), .. }) => self.ok(format!("Exported {} rows to {}", human_rows(rows), p.display())),
            Ok(Done { rows, text: Some(t), .. }) => {
                ctx.copy_text(t);
                self.ok(format!("Copied {} rows", human_rows(rows)));
            }
            Ok(_) => {}
            Err(e) if e == "Export cancelled" => self.ok("Export stopped"),
            Err(e) => self.err(e),
        }
    }

    /// The status bar's readout of a running stream, with Cancel. True while one runs.
    pub(super) fn export_progress(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(j) = &self.export_job else { return false };
        let (n, copy) = (j.count.load(Ordering::Relaxed), j.copy);
        spin(ui);
        let text = if j.of > 0 { format!("Statement {} of {}, {} rows", j.step.load(Ordering::Relaxed), j.of, human_rows(n)) } else { format!("{} {} rows", if copy { "Copied" } else { "Exported" }, human_rows(n)) };
        ui.label(RichText::new(text).size(TXT_CAPTION).color(MUTED()));
        if quiet(ui, RichText::new("Cancel").size(TXT_CAPTION)).on_hover_text("Stop and discard the output").clicked() {
            if let Some(j) = &self.export_job {
                self.worker.sql_kill(j.db.clone(), j.id.load(Ordering::SeqCst), j.cancel.clone());
            }
        }
        ui.ctx().request_repaint_after(Duration::from_millis(200));
        true
    }
}

/// What decides the dialog's first scope.
struct Ask {
    all_pages: bool,
    all_ok: bool,
    has_selected: bool,
}

/// The scope the dialog starts on: every page when that was asked for and is allowed, else the checked rows, else what is shown.
fn pick_scope(a: Ask) -> Scope {
    if a.all_pages && a.all_ok {
        Scope::All
    } else if a.has_selected {
        Scope::Selected
    } else {
        Scope::Shown
    }
}

fn lex_check_msg(stmt: &str) -> Result<(), String> {
    db::sql::lex::read_only_check(stmt).map_err(|why| format!("Only statements that read can be copied in full: {why}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dialog_starts_on_the_widest_scope_asked_for_that_is_allowed() {
        let ask = |all_pages, all_ok, has_selected| pick_scope(Ask { all_pages, all_ok, has_selected });
        assert!(ask(true, true, true) == Scope::All);
        // Refused statements fall back to the checked rows, then to what is shown.
        assert!(ask(true, false, true) == Scope::Selected);
        assert!(ask(true, false, false) == Scope::Shown);
        assert!(ask(false, true, true) == Scope::Selected);
        assert!(ask(false, true, false) == Scope::Shown);
    }

    #[test]
    fn all_rows_export_refuses_what_writes() {
        for w in ["DELETE FROM t", "UPDATE t SET a = 1", "SELECT * FROM t INTO OUTFILE '/tmp/x'", "SET SESSION transaction_read_only = 0", "DROP TABLE t"] {
            assert!(lex_check_msg(w).is_err(), "{w}");
        }
        for r in ["SELECT * FROM t", "WITH c AS (SELECT 1) SELECT * FROM c", "SHOW TABLES"] {
            assert!(lex_check_msg(r).is_ok(), "{r}");
        }
    }

    #[test]
    fn row_counts_read_short_from_a_million() {
        assert_eq!(human_rows(999), "999");
        assert_eq!(human_rows(1_234_567), "1.2M");
    }
}
