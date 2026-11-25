//! The Export dialog: the rows of a result written to a file in any Copy as format, and the status
//! bar readout of a Query result streaming out.

use super::*;
use db::copy::{self, CopyCol, Fmt, Sheet};
use db::sql::export::{Done, Plan, Sink};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
pub(crate) use quarry_core::state::export::*;

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
            Src::Query => t.sql.as_ref().and_then(|s| s.cur_set()).map(|x| x.rows.iter().map(db::Row::owned).collect()).unwrap_or_default(),
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
        // The Shell pages its rows; its checks are positions in the page on screen. Only those are decoded, since a
        // Shell result can hold a gigabyte.
        if src == Src::Shell {
            let base = t.shell.page * PAGE_SIZE as usize;
            return self.checks().iter().filter_map(|i| t.shell.docs.get(base + i)).map(db::Row::owned).collect();
        }
        let all = self.shown_rows(src);
        self.checks().iter().filter_map(|i| all.get(*i).cloned()).collect()
    }

    /// Columns, table name and key for writing `rows` of `src` as text. The table is `insert_table`'s, so Copy as and
    /// Export agree on it.
    fn export_sheet(&self, src: Src, rows: &[Document]) -> (Vec<CopyCol>, String, Vec<String>) {
        let Some(t) = self.t() else { return Default::default() };
        let table = self.insert_table(src);
        match src {
            Src::Query => {
                let set = t.sql.as_ref().and_then(|s| s.cur_set());
                let cols = set.map(|s| s.cols.iter().map(|c| CopyCol { name: c.name.clone(), src: c.src.clone(), numeric: is_numeric(c.kind) }).collect()).unwrap_or_default();
                (cols, table, Vec::new())
            }
            Src::Shell => (keys_of(rows).into_iter().map(|n| CopyCol { numeric: infer_numeric(rows, &n), src: n.clone(), name: n }).collect(), table, Vec::new()),
            Src::Content => (self.copy_cols(rows), table, t.sql.as_ref().map(|s| s.pk.clone()).unwrap_or_default()),
        }
    }

    /// The table a SQL INSERT of `src`'s rows writes to. A Content grid writes its own table. A Query result writes the
    /// base table of the saved script the tab is bound to, else the first table its statement reads, else the tab's table.
    pub(in crate::app) fn insert_table(&self, src: Src) -> String {
        let Some(t) = self.t() else { return "result".into() };
        if src == Src::Content && !t.sel.coll.is_empty() {
            return t.sel.coll.clone();
        }
        let base = match t.script.as_ref().map(|b| &b.r) {
            Some(crate::library::ScriptRef::Saved(id)) => self.library.scripts.iter().find(|s| s.id == *id && s.kind == "sql").map(|s| s.coll.clone()),
            _ => None,
        };
        let stmt = t.sql.as_ref().filter(|_| src == Src::Query).and_then(|s| s.cur_set()).map(|x| x.stmt.as_str()).unwrap_or_default();
        [base, db::sql::source::first_table(stmt, self.sql_dialect()), Some(t.sel.coll.clone())].into_iter().flatten().find(|n| !n.trim().is_empty()).unwrap_or_else(|| "result".into())
    }

    /// Opens the dialog for the current grid. `selected` are the rows to offer as "Selected rows".
    pub(super) fn open_export(&mut self, selected: Vec<Document>, all_pages: bool) {
        let src = self.grid_src();
        let Some(t) = self.t() else { return };
        let sql = t.sql.is_some();
        let set = t.sql.as_ref().filter(|_| src == Src::Query).and_then(|s| s.cur_set());
        let shown = shown_count(t, src);
        let paged = set.is_some_and(|x| x.page.is_some());
        let all = set.filter(|x| x.error.is_none() && !x.stmt.trim().is_empty()).and_then(|x| {
            let db = self.conn(t.sel.conn).and_then(|c| c.sql.clone())?;
            let refused = db::sql::lex::read_only_check(&x.stmt, db.dialect).err();
            // The same database and schema the statement ran in, so it reads what the grid showed.
            let (db, schema) = super::sql::query::ran_target(t, db);
            Some(AllRows { db, schema, stmt: x.stmt.clone(), total: x.page.and_then(|p| p.total), paged, refused })
        });
        let all = if src == Src::Content { self.content_all(t) } else { all };
        let script = t.sql.as_ref().filter(|_| src == Src::Query).and_then(|s| {
            let db = self.conn(t.sel.conn).and_then(|c| c.sql.clone())?;
            let (db, schema) = super::sql::query::run_target(t, db);
            // A snippet exports what Run sends, its values filled in, and nothing while one does not read.
            let query = self.snippet_fill_run(s.query.clone()).ok()?;
            let stmts = db::sql::export::check_script(&query, db.dialect);
            Some(Script { db, schema, stmts })
        });
        let scope = pick_scope(Ask { all_pages, all_ok: all.as_ref().is_some_and(|a| a.refused.is_none()), has_selected: !selected.is_empty() });
        let fmt = if sql { Format::T(Fmt::Csv) } else { Format::MongoJson };
        self.modal = Some(Modal::Export(Box::new(ExportForm { src, sql, fmt, header: true, scope, selected, shown, all, script, single: false, paged, about: false, table: self.insert_table(src) })));
    }

    /// Every row of a SQL Content grid: the SELECT of the last page asked for, never the bar text being typed, with its WHERE,
    /// ORDER BY, columns and grouping and without the paging. The LIMIT and OFFSET chips still apply, so it is the count the
    /// grid shows.
    fn content_all(&self, t: &Tab) -> Option<AllRows> {
        let s = t.sql.as_ref().filter(|_| !t.sel.coll.is_empty())?;
        let (db, schema) = self.sql_target(&t.sel)?;
        let pk: &[String] = if s.ran.grouped() { &[] } else { &s.pk };
        let stmt = s.ran.select_sql(db.dialect, &schema, &t.sel.coll, pk, None);
        let refused = db::sql::lex::read_only_check(&stmt, db.dialect).err();
        let total = (!s.estimated).then_some(t.count);
        Some(AllRows { db, schema, stmt, total, paged: true, refused })
    }

    /// Demo only: gives the open dialog the editor script as a source with a pool that never connects, so the scope can be drawn offline.
    /// `text` replaces the editor's text when given, to show a script that passes the check.
    pub(super) fn demo_script_scope(&mut self, text: Option<&str>) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
        let _guard = rt.enter();
        let opts = "mysql://root@127.0.0.1:1/shop".parse::<sqlx::mysql::MySqlConnectOptions>().expect("demo address");
        let db = db::sql::Db { pool: db::sql::Pool::My(sqlx::mysql::MySqlPoolOptions::new().connect_lazy_with(opts.clone())), dialect: db::sql::Dialect::MySql, opts: db::sql::exec::Opts::My(opts), version: "8.4.3".into(), tls: false, sql_mode: String::new(), setup: Default::default(), conn: 0 };
        let query = text.map(str::to_string).or_else(|| self.t().and_then(|t| t.sql.as_ref().map(|s| s.query.clone()))).unwrap_or_default();
        if let Some(Modal::Export(f)) = &mut self.modal {
            let stmts = db::sql::export::check_script(&query, db.dialect);
            f.script = Some(Script { db, schema: "shop".into(), stmts });
            f.scope = Scope::Script;
        }
    }

    pub(super) fn export_dialog(&mut self, ui: &mut egui::Ui, f: &mut ExportForm, keep: &mut bool) {
        ui.set_width(520.0);
        // Opened from a row menu or the selection bar, the dialog was made while the grid had its rows out of the tab,
        // so it counted none; they are back by the time it draws.
        if f.shown == 0 && let Some(t) = self.t() {
            f.shown = shown_count(t, f.src);
        }
        let where_ = self.t().map(|t| t.sel.ns()).unwrap_or_default();
        dialog_head(ui, &Head::new("Export rows").sub(where_).glyph(glyphs::download));
        let mut b = body_begin(ui);
        let outer = ui;
        let ui = &mut b.content_ui;
        let formats: Vec<Format> = if f.sql {
            let mut v = vec![Format::T(Fmt::SqlInsert)];
            // A Query result or a Shell result has no key to write an UPDATE by, and neither has a streamed result.
            if f.src == Src::Content && f.scope != Scope::All {
                v.push(Format::T(Fmt::SqlUpdate));
            }
            v.push(Format::T(Fmt::Csv));
            v.push(Format::Xlsx);
            v.extend([Fmt::Tsv, Fmt::Json, Fmt::JsonLines, Fmt::Markdown, Fmt::Html, Fmt::Values].map(Format::T));
            v
        } else {
            vec![Format::MongoJs, Format::MongoJson, Format::MongoLines, Format::T(Fmt::Csv), Format::Xlsx, Format::T(Fmt::Tsv), Format::T(Fmt::Markdown), Format::T(Fmt::Html)]
        };
        let noun = if f.sql { "rows" } else { "documents" };
        // Which rows: one radio card each, with why a choice is not available under it.
        card_caption(ui, "What to export", "");
        let w = ui.available_width();
        let radio = |ui: &mut egui::Ui, f: &mut ExportForm, on: bool, scope: Scope, title: &str, desc: &str| {
            if radio_card(ui, w, on, title, desc).clicked() {
                f.scope = scope;
            }
            ui.add_space(6.0);
        };
        let table = matches!(f.fmt, Format::T(_) | Format::Xlsx);
        ui.add_enabled_ui(!f.selected.is_empty(), |ui| radio(ui, f, f.scope == Scope::Selected, Scope::Selected, &format!("Selected {noun}"), &format!("{} picked in the grid", group(&f.selected.len().to_string()))));
        let shown = if f.paged { "This page" } else { "Shown" };
        radio(ui, f, f.scope == Scope::Shown, Scope::Shown, &format!("{shown} {noun}"), &format!("{} as they are on screen", group(&f.shown.to_string())));
        if let Some((paged, total, refused)) = f.all.as_ref().map(|a| (a.paged, a.total, a.refused.clone())) {
            let what = match (f.src, paged, total) {
                (Src::Content, _, Some(n)) => format!("All matching rows, {}", group(&n.to_string())),
                (Src::Content, _, None) => "All matching rows".to_string(),
                (_, true, Some(n)) => format!("All pages, {} rows", group(&n.to_string())),
                (_, true, None) => "All pages".to_string(),
                _ => "All rows".to_string(),
            };
            let again = if f.src == Src::Content { "Runs the SELECT again without paging and streams to the file" } else { "Runs the statement again and streams to the file" };
            let why = refused.clone().map_or_else(|| again.to_string(), |w| format!("Not available: {w}"));
            ui.add_enabled_ui(refused.is_none() && table, |ui| radio(ui, f, f.scope == Scope::All, Scope::All, &what, &why));
        }
        if let Some(sc) = &f.script {
            let n = sc.stmts.as_ref().map_or(0, Vec::len);
            let ok = sc.stmts.is_ok();
            let why = match &sc.stmts {
                Ok(_) => "Runs them in order and streams each result".to_string(),
                Err(w) => format!("Not available: {w}"),
            };
            ui.add_enabled_ui(ok && table, |ui| radio(ui, f, f.scope == Scope::Script, Scope::Script, &format!("All statements in the editor ({n})"), &why));
        }
        ui.add_space(10.0);
        form_card(ui, "Format", "", |rows| {
            rows.label_width(140.0);
            rows.row("Format", "", |ui| {
                egui::ComboBox::from_id_salt("export-fmt").width(220.0).selected_text(f.fmt.label()).show_ui(ui, |ui| {
                    menu_look(ui);
                    for x in formats {
                        ui.selectable_value(&mut f.fmt, x, x.label());
                    }
                });
            });
            let has_header = !matches!(f.fmt, Format::MongoJs | Format::MongoJson | Format::MongoLines | Format::Xlsx);
            let hint = match f.fmt {
                Format::Xlsx => "The table's header row, always there",
                _ if has_header => "A header row first",
                _ => "This format has no header row",
            };
            rows.row("Column names", hint, |ui| {
                // A spreadsheet table always has its header, so the toggle shows on and cannot change.
                let mut on = f.header || f.fmt == Format::Xlsx;
                ui.add_enabled_ui(has_header, |ui| toggle(ui, if has_header { &mut f.header } else { &mut on }));
            });
            if matches!(f.fmt, Format::T(Fmt::SqlInsert | Fmt::SqlUpdate)) {
                let bound = f.src == Src::Query && self.t().is_some_and(|t| matches!(t.script.as_ref().map(|b| &b.r), Some(crate::library::ScriptRef::Saved(_))));
                rows.row("Table", if bound { "Kept as the script's base table" } else { "The statements write to it" }, |ui| {
                    let r = ui.add(egui::TextEdit::singleline(&mut f.table).desired_width(ui.available_width()).margin(egui::Margin { left: 10, right: 10, top: 5, bottom: 5 }));
                    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Insert table"));
                });
            }
            if f.fmt == Format::Xlsx {
                rows.row("About sheet", "A second sheet with the source, filter and statement", |ui| {
                    let on = f.about;
                    toggle(ui, &mut f.about).widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, on, "About sheet"));
                });
            }
            if f.scope == Scope::Script {
                let one_ok = matches!(f.fmt, Format::T(x) if db::sql::export::can_single(x));
                if !one_ok {
                    f.single = false;
                }
                let hint = match f.fmt {
                    _ if one_ok => "A section for each statement, instead of name-1, name-2…",
                    // ponytail: one workbook per statement; a sheet per statement in one workbook if asked for.
                    Format::Xlsx => "Excel writes a workbook for each statement",
                    _ => "CSV, TSV and Values only: the others have no place for a second header",
                };
                rows.row("One file", hint, |ui| {
                    ui.add_enabled_ui(one_ok, |ui| toggle(ui, &mut f.single));
                });
            }
        });
        // A format only the Shell and Documents know cannot come from a statement.
        if matches!(f.scope, Scope::All | Scope::Script) && !table {
            f.scope = Scope::Shown;
        }
        if f.scope == Scope::All && f.fmt == Format::T(Fmt::SqlUpdate) {
            f.fmt = Format::T(Fmt::SqlInsert);
        }
        let ready = match f.scope {
            Scope::Selected => !f.selected.is_empty(),
            Scope::Shown => f.shown > 0,
            Scope::All => f.all.as_ref().is_some_and(|a| a.refused.is_none()),
            Scope::Script => f.script.as_ref().is_some_and(|s| s.stmts.is_ok()),
        };
        b.end(outer);
        let ui = outer;
        dialog_foot(ui, |ui| {
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
            if f.sql && f.src == Src::Content && foot_left(ui, |ui| ui.button("SQL dump…").on_hover_text("The table with its structure, or the whole database, as a script").clicked()) {
                *keep = false;
                if let Some(s) = self.t().map(|t| t.sel.clone()) {
                    self.open_sql_dump_in(s.conn, s.db, s.schema, Some(s.coll));
                }
            }
        });
    }

    fn export_name(&self, src: Src) -> String {
        // A tab bound to a saved script exports that script's view under the script's name, which says more than the table's.
        let view = match src {
            Src::Query => View::Sql,
            Src::Shell => View::Shell,
            Src::Content => View::Documents,
        };
        if let Some(name) = self.t().and_then(|t| t.script.as_ref()).filter(|b| b.view == view).map(|b| match &b.r {
            crate::library::ScriptRef::File { rel, .. } => rel.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            r => self.script_name(r),
        }) && !name.trim().is_empty()
        {
            return crate::library::disk::file_name(&name, "");
        }
        let coll = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
        match (src, coll.is_empty()) {
            (Src::Query, true) => "query-result".into(),
            (Src::Shell, true) => "shell-result".into(),
            (Src::Shell, false) => format!("{coll}-shell"),
            (_, true) => "export".into(),
            (_, false) => coll,
        }
    }

    /// The sheet name and About lines of an xlsx export.
    fn xlsx_opts(&self, f: &ExportForm) -> db::xlsx::Opts {
        db::xlsx::Opts { sheet: self.export_name(f.src), about: f.about.then(|| self.xlsx_about(f.src)) }
    }

    /// Where the rows come from, for the About sheet: the connection, its server, the database and table or collection,
    /// and the filter and sort a Content grid ran with.
    fn xlsx_about(&self, src: Src) -> Vec<(String, String)> {
        let Some(t) = self.t() else { return Vec::new() };
        let mut out = Vec::new();
        let mut put = |k: &str, v: &str| {
            if !v.trim().is_empty() {
                out.push((k.to_string(), v.trim().to_string()));
            }
        };
        if let Some(c) = self.conn(t.sel.conn) {
            put("Connection", &c.meta.name);
            let (_, product) = super::rail::engine_mark(c.meta.engine, &c.server.product);
            put("Server", &format!("{product} {}", c.server.version));
        }
        put("Database", &t.sel.db);
        put("Schema", &t.sel.schema);
        put(if t.sql.is_some() { "Table" } else { "Collection" }, &t.sel.coll);
        // What the rows ran with, never the bar's text, which may hold edits that did not run.
        if src == Src::Content {
            put("Filter", &t.ran_txt[0]);
            put("Sort", &t.ran_txt[1]);
            if t.sql.is_none() {
                put("Projection", &t.ran_txt[2]);
            }
        }
        out
    }

    /// The columns of an xlsx export with their SQL kinds, which say how to write text such as dates and DECIMAL. Mongo
    /// columns have none and are written by each value's own type.
    fn xlsx_cols(&self, src: Src, cols: &[CopyCol]) -> Vec<db::xlsx::XCol> {
        let t = self.t();
        let sql_cols: &[db::sql::Col] = match (src, t.and_then(|t| t.sql.as_ref())) {
            (Src::Query, Some(s)) => s.cur_set().map_or(&[], |x| x.cols.as_slice()),
            (Src::Content, Some(s)) => s.cols.as_slice(),
            _ => &[],
        };
        cols.iter().map(|c| db::xlsx::XCol { name: c.name.clone(), kind: sql_cols.iter().find(|s| s.name == c.name).map(|s| s.kind) }).collect()
    }

    fn export_run(&mut self, f: &ExportForm, path: PathBuf) {
        let table = if f.table.trim().is_empty() { self.insert_table(f.src) } else { f.table.trim().to_string() };
        if matches!(f.fmt, Format::T(Fmt::SqlInsert | Fmt::SqlUpdate)) {
            self.keep_base_table(f.src, &table);
        }
        // The text format to stream, or for a workbook the CSV slot it leaves unused.
        use db::sql::export::Target;
        let target = match f.fmt {
            Format::T(fmt) => Some(Target::Text(fmt)),
            Format::Xlsx => Some(Target::Xlsx(self.xlsx_opts(f))),
            _ => None,
        };
        if f.scope == Scope::All {
            let (Some(a), Some(target)) = (&f.all, target) else { return };
            let plan = Plan { target, header: f.header, table, bs: self.sql_bs() };
            self.start_stream(a, plan, Sink::File(path), false);
            return;
        }
        if f.scope == Scope::Script {
            let (Some(sc), Some(target)) = (&f.script, target) else { return };
            let Ok(stmts) = &sc.stmts else { return };
            let plan = Plan { target, header: f.header, table, bs: self.sql_bs() };
            // Only text formats with room for several results go in one file.
            let single = f.single && plan.text().is_some_and(db::sql::export::can_single);
            return self.start_script(sc, stmts.clone(), db::sql::export::AllPlan { plan, single }, path);
        }
        // Shell rows can reach a gigabyte, so they are written a batch at a time off the UI thread, in the same bytes as
        // the whole list. Markdown pads its table to the widest cell, which only the whole list knows, so a small one
        // keeps that path.
        if f.src == Src::Shell && f.scope == Scope::Shown && !(f.fmt == Format::T(Fmt::Markdown) && self.t().is_some_and(|t| t.shell.docs_bytes() <= db::sql::export::CLIP_MAX)) {
            return self.start_shell_export(f, table, path);
        }
        let rows = if f.scope == Scope::Selected { f.selected.clone() } else { self.shown_rows(f.src) };
        if let Some(Target::Xlsx(opts)) = target {
            let (cols, _, _) = self.export_sheet(f.src, &rows);
            // A Query result's statement goes on the About sheet; a Content grid's filter is there already.
            let stmt = self.t().and_then(|t| t.sql.as_ref()).filter(|_| f.src == Src::Query).and_then(|s| s.cur_set()).map(|x| x.stmt.clone());
            let mut bytes = Vec::new();
            let book = db::xlsx::Book::new(&opts.sheet, self.xlsx_cols(f.src, &cols), opts.about).and_then(|mut b| {
                b.rows(&rows)?;
                b.finish(stmt.as_deref(), &mut bytes)
            });
            if let Err(e) = book {
                return self.err(e);
            }
            return self.write_export(&path, &bytes, rows.len());
        }
        let text = match f.fmt {
            Format::T(fmt) => {
                let (cols, _, pk) = self.export_sheet(f.src, &rows);
                let text = copy::format(self.sql_dialect(), fmt, &Sheet { rows: &rows, cols: &cols, table: &table, pk: &pk, bs: self.sql_bs() }, f.header);
                if text.is_empty() {
                    return self.warn(if fmt == Fmt::SqlUpdate { "SQL UPDATE needs a table with a primary key." } else { "Nothing to export." });
                }
                text
            }
            Format::MongoJs => db::pretty_shell(&Bson::Array(rows.iter().cloned().map(Bson::Document).collect())),
            Format::MongoJson => db::to_edit_value(&Bson::Array(rows.iter().cloned().map(Bson::Document).collect())),
            Format::MongoLines => rows.iter().map(|d| db::to_edit_line(&Bson::Document(d.clone()))).collect::<Vec<_>>().join("\n"),
            Format::Xlsx => return,
        };
        self.write_export(&path, format!("{text}\n").as_bytes(), rows.len());
    }

    /// A Query result exported as SQL to another table than its script's base table makes that table the new base table,
    /// so the next Copy as SQL INSERT writes to it.
    fn keep_base_table(&mut self, src: Src, table: &str) {
        let Some(Some(crate::library::ScriptRef::Saved(id))) = self.t().filter(|_| src == Src::Query).map(|t| t.script.as_ref().map(|b| b.r.clone())) else { return };
        if let Some(s) = self.library.scripts.iter_mut().find(|s| s.id == id && s.kind == "sql" && s.coll != table) {
            s.coll = table.to_string();
            self.save_library();
        }
    }

    fn write_export(&mut self, path: &std::path::Path, bytes: &[u8], n: usize) {
        let (dir, name) = (path.parent().map(|p| p.to_path_buf()).unwrap_or_default(), path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        match write_atomic(&dir, &name, bytes) {
            Ok(()) => self.ok(format!("Exported {n} {} to {}", if n == 1 { "row" } else { "rows" }, path.display())),
            Err(e) => self.err(format!("Could not write {}: {e}", path.display())),
        }
    }

    /// Streams a Query result's statement to `sink` in a worker task, tracked in the status bar.
    fn start_stream(&mut self, a: &AllRows, plan: Plan, sink: Sink, copy: bool) {
        if self.export_job.is_some() {
            return self.warn("An export is already running.");
        }
        self.req += 1;
        let job = ExportJob { req: self.req, count: Arc::new(AtomicU64::new(0)), cancel: Arc::new(AtomicBool::new(false)), id: Arc::new(AtomicU64::new(0)), db: Some(a.db.clone()), copy, step: Arc::new(AtomicU64::new(0)), of: 0 };
        self.worker.sql_export(job.req, a.db.clone(), a.schema.clone(), a.stmt.clone(), plan, sink, job.count.clone(), job.id.clone(), job.cancel.clone());
        self.export_job = Some(job);
    }

    /// Writes every Shell row to `path` on a blocking thread, tracked in the status bar like a stream.
    fn start_shell_export(&mut self, f: &ExportForm, table: String, path: PathBuf) {
        if self.export_job.is_some() {
            return self.warn("An export is already running.");
        }
        let out = match f.fmt {
            // Shell rows have no primary key, which UPDATE needs, as the whole-list export says too.
            Format::T(Fmt::SqlUpdate) => return self.warn("SQL UPDATE needs a table with a primary key."),
            Format::T(fmt) => db::RowsOut::Text { fmt, header: f.header, table, dialect: self.sql_dialect(), bs: self.sql_bs() },
            Format::Xlsx => db::RowsOut::Xlsx(self.xlsx_opts(f)),
            Format::MongoJs => db::RowsOut::MongoJs,
            Format::MongoJson => db::RowsOut::MongoJson,
            Format::MongoLines => db::RowsOut::MongoLines,
        };
        let Some(t) = self.t() else { return };
        // A copy of the bytes, freed when the export ends, so the result stays on screen meanwhile.
        let rows: Vec<mongodb::bson::RawDocumentBuf> = t.shell.docs.iter().map(|r| r.raw().to_owned()).collect();
        self.req += 1;
        let job = ExportJob { req: self.req, count: Arc::new(AtomicU64::new(0)), cancel: Arc::new(AtomicBool::new(false)), id: Arc::new(AtomicU64::new(0)), db: None, copy: false, step: Arc::new(AtomicU64::new(0)), of: 0 };
        let (req, count, cancel) = (job.req, job.count.clone(), job.cancel.clone());
        self.worker.blocking(move || Msg::SqlExport { req, res: db::export_rows(&rows, &out, &path, &count, &cancel) });
        self.export_job = Some(job);
    }

    /// Runs the editor's statements in a worker task and streams their results to files.
    fn start_script(&mut self, sc: &Script, stmts: Vec<String>, ap: db::sql::export::AllPlan, path: PathBuf) {
        if self.export_job.is_some() {
            return self.warn("An export is already running.");
        }
        self.req += 1;
        let job = ExportJob { req: self.req, count: Arc::new(AtomicU64::new(0)), cancel: Arc::new(AtomicBool::new(false)), id: Arc::new(AtomicU64::new(0)), db: Some(sc.db.clone()), copy: false, step: Arc::new(AtomicU64::new(0)), of: stmts.len() };
        self.worker.sql_export_all(job.req, sc.db.clone(), sc.schema.clone(), stmts, ap, path, job.count.clone(), job.step.clone(), job.id.clone(), job.cancel.clone());
        self.export_job = Some(job);
    }

    /// Copy as for every page of the selected Query result: the text is built in a worker task.
    pub(super) fn copy_all_pages(&mut self, fmt: Fmt, header: bool) {
        let Some(t) = self.t() else { return };
        let Some(s) = t.sql.as_ref() else { return };
        let Some(set) = s.cur_set() else { return };
        if let Err(why) = lex_check_msg(&set.stmt, self.sql_dialect()) {
            return self.warn(why);
        }
        let Some(db) = self.conn(t.sel.conn).and_then(|c| c.sql.clone()) else { return self.warn_offline() };
        let (db, schema) = super::sql::query::ran_target(t, db);
        let a = AllRows { db, schema, stmt: set.stmt.clone(), total: None, paged: true, refused: None };
        let table = self.export_sheet(Src::Query, &[]).1;
        let plan = Plan { target: crate::db::sql::export::Target::Text(fmt), header, table, bs: self.sql_bs() };
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
        if quiet(ui, RichText::new("Cancel").size(TXT_CAPTION)).on_hover_text("Stop and discard the output").clicked() && let Some(j) = &self.export_job {
            match &j.db {
                Some(db) => self.worker.sql_kill(db.clone(), j.id.load(Ordering::SeqCst), j.cancel.clone()),
                None => j.cancel.store(true, Ordering::Relaxed),
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

/// The rows of `src` on screen: the selected Query result, the Shell's results or the Content page.
fn shown_count(t: &Tab, src: Src) -> usize {
    match src {
        Src::Query => t.sql.as_ref().and_then(|s| s.cur_set()).map_or(0, |x| x.rows.len()),
        Src::Shell => t.shell.docs.len(),
        Src::Content => t.docs.len(),
    }
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

fn lex_check_msg(stmt: &str, d: db::sql::Dialect) -> Result<(), String> {
    db::sql::lex::read_only_check(stmt, d).map_err(|why| format!("Only statements that read can be copied in full: {why}"))
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
            assert!(lex_check_msg(w, db::sql::Dialect::MySql).is_err(), "{w}");
        }
        for r in ["SELECT * FROM t", "WITH c AS (SELECT 1) SELECT * FROM c", "SHOW TABLES"] {
            assert!(lex_check_msg(r, db::sql::Dialect::MySql).is_ok(), "{r}");
        }
    }

    #[test]
    fn all_matching_rows_repeat_the_page_select_without_its_paging() {
        use db::sql::{Dialect, PageQuery};
        let pk = ["id".to_string()];
        let q = PageQuery { filter: "a = 1 OR b = 2".into(), order: "a DESC".into(), select: vec!["a".into(), "b".into()], ..Default::default() };
        assert_eq!(q.select_sql(Dialect::MySql, "shop", "t", &pk, None), "SELECT a,\n  b\nFROM `shop`.`t`\nWHERE (\na = 1 OR b = 2\n)\nORDER BY a DESC\n, `id`");
        assert_eq!(q.select_sql(Dialect::MariaDb, "shop", "t", &pk, None), "SELECT a,\n  b\nFROM `shop`.`t`\nWHERE (\na = 1 OR b = 2\n)\nORDER BY a DESC\n, `id`");
        assert_eq!(q.select_sql(Dialect::Postgres, "public", "t", &pk, None), "SELECT a,\n  b\nFROM \"public\".\"t\"\nWHERE (\na = 1 OR b = 2\n)\nORDER BY a DESC\n, \"id\"");
        // No WHERE reads every row; the LIMIT chip still caps the export as it caps the grid's count.
        assert_eq!(PageQuery::default().select_sql(Dialect::MySql, "shop", "t", &pk, None), "SELECT *\nFROM `shop`.`t`\nORDER BY `id`");
        assert_eq!(PageQuery { limit: Some(5), ..Default::default() }.select_sql(Dialect::Postgres, "public", "t", &pk, None), "SELECT *\nFROM \"public\".\"t\"\nORDER BY \"id\"\nLIMIT 5");
        // A grouped page exports its groups.
        let g = PageQuery { select: vec!["status".into(), "COUNT(*) AS n".into()], group: vec!["status".into()], ..Default::default() };
        assert_eq!(g.select_sql(Dialect::MySql, "shop", "t", &[], None), "SELECT status,\n  COUNT(*) AS n\nFROM `shop`.`t`\nGROUP BY status\nORDER BY status");
        for d in [Dialect::MySql, Dialect::Postgres] {
            assert!(db::sql::lex::read_only_check(&q.select_sql(d, "s", "t", &pk, None), d).is_ok());
        }
    }

    #[test]
    fn row_counts_read_short_from_a_million() {
        assert_eq!(human_rows(999), "999");
        assert_eq!(human_rows(1_234_567), "1.2M");
    }
}
