//! Editing SQL rows from the grid: cell edits staged as drafts, the staged-edits bar, row inserts,
//! deletes, Set value, and the "Copy as" actions that Mongo documents share.

use super::*;
use db::copy::{self, CopyCol, Fmt, Sheet};
use db::sql::value::{self, Kind};
use db::sql::write::{self, NewRow, RowEdit};
use db::sql::Col;

/// A cell being edited in place. `done` is set by the editor: Some(true) to commit, Some(false) to cancel.
pub(super) struct CellEdit {
    /// The row's key, because a reload while the editor is open moves rows around.
    pub(super) id: Option<Bson>,
    pub(super) col: String,
    pub(super) text: String,
    pub(super) err: Option<String>,
    pub(super) focus: bool,
    pub(super) done: Option<bool>,
}

/// The "Set value on N rows" dialog.
pub(super) struct SetForm {
    pub(super) rows: Vec<Document>,
    pub(super) col: String,
    pub(super) text: String,
    pub(super) null: bool,
    pub(super) err: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Mode {
    Default,
    Null,
    Value,
}

pub(super) struct RowField {
    col: Col,
    mode: Mode,
    text: String,
}

/// The "+ Row" dialog: one line per column.
pub(super) struct RowForm {
    title: String,
    fields: Vec<RowField>,
    err: Option<String>,
}

/// What a cell's context menu asked for, besides the row actions.
#[derive(Clone, Copy)]
pub(super) enum CellAct {
    CopyValue,
    SetNull,
    FilterValue,
}

pub(super) fn is_numeric(k: Kind) -> bool {
    matches!(k, Kind::Int | Kind::UInt | Kind::Float | Kind::Decimal)
}

/// The option list of an ENUM column from its type text, `enum('a','it''s')`.
fn enum_values(ty: &str) -> Vec<String> {
    let Some(start) = ty.find('(') else { return Vec::new() };
    if !ty[..start].eq_ignore_ascii_case("enum") {
        return Vec::new();
    }
    let (mut out, mut cur, mut inq) = (Vec::new(), String::new(), false);
    let mut chars = ty[start + 1..].chars().peekable();
    while let Some(c) = chars.next() {
        match (c, inq) {
            ('\'', false) => inq = true,
            ('\'', true) if chars.peek() == Some(&'\'') => {
                cur.push('\'');
                chars.next();
            }
            ('\'', true) => {
                inq = false;
                out.push(std::mem::take(&mut cur));
            }
            (c, true) => cur.push(c),
            _ => {}
        }
    }
    out
}

/// Text for the editor: what the cell shows, as typed back it parses to the same value.
fn edit_text(b: &Bson) -> String {
    match b {
        Bson::Null => "NULL".into(),
        Bson::String(s) => s.clone(),
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => n.to_string(),
        Bson::Double(f) => f.to_string(),
        Bson::Binary(x) => format!("0x{}", x.bytes.iter().map(|b| format!("{b:02X}")).collect::<String>()),
        other => db::short(other),
    }
}

/// The menu of a SQL cell: the value actions first. Returns the one picked.
pub(super) fn sql_cell_menu(ui: &mut egui::Ui, nullable: bool, editable: bool) -> Option<CellAct> {
    let mut act = None;
    if ui.button("Copy value").clicked() {
        act = Some(CellAct::CopyValue);
    }
    if ui.add_enabled(nullable && editable, egui::Button::new("Set NULL")).on_disabled_hover_text(if nullable { "This cell cannot be edited" } else { "This column does not allow NULL" }).clicked() {
        act = Some(CellAct::SetNull);
    }
    if ui.button("Filter to this value").clicked() {
        act = Some(CellAct::FilterValue);
    }
    act
}

/// "Copy as" for the document menus. SQL rows add SQL INSERT and UPDATE; Mongo documents keep their JS and
/// JSON forms. Both get Markdown and HTML tables and the column names switch.
pub(super) fn copy_as_menu(ui: &mut egui::Ui, sql: bool) -> Option<DocsAct> {
    menu_look(ui);
    let mut act = None;
    let mut item = |ui: &mut egui::Ui, label: &str, hint: &str, a: DocsAct| {
        if ui.add(egui::Button::new(label).right_text(RichText::new(hint).size(TXT_CAPTION).color(FAINT()))).clicked() {
            act = Some(a);
        }
    };
    if sql {
        item(ui, "SQL INSERT", "INSERT INTO t …", DocsAct::CopyAs(Fmt::SqlInsert));
        item(ui, "SQL UPDATE", "UPDATE … WHERE id = …", DocsAct::CopyAs(Fmt::SqlUpdate));
        ui.separator();
        item(ui, "CSV", "with header", DocsAct::CopyAs(Fmt::Csv));
        item(ui, "TSV", "pastes into spreadsheets", DocsAct::CopyAs(Fmt::Tsv));
        item(ui, "JSON", "array of objects", DocsAct::CopyAs(Fmt::Json));
        item(ui, "JSON lines", "one object per line", DocsAct::CopyAs(Fmt::JsonLines));
        ui.separator();
    } else {
        item(ui, "JS", "shell syntax", DocsAct::CopyJs);
        item(ui, "JSON", "extended JSON", DocsAct::CopyJson);
        item(ui, "JSON lines", "one document per line", DocsAct::CopyLines);
        ui.separator();
    }
    item(ui, "Markdown table", "| id | status |", DocsAct::CopyAs(Fmt::Markdown));
    item(ui, "HTML table", "<table> with <th>", DocsAct::CopyAs(Fmt::Html));
    if sql {
        item(ui, "Values only", "one per line", DocsAct::CopyAs(Fmt::Values));
    }
    ui.separator();
    // A stable id: the copy action reads the switch back by it.
    let id = egui::Id::unique("copy-with-header");
    let on = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(true);
    let (row, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ui.spacing().interact_size.y), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(row, R_SM, MENU_HOVER());
    }
    let boxed = check_box(ui, egui::pos2(row.left() + 14.0, row.center().y), id.with("box"), on, false);
    ui.painter().text(egui::pos2(row.left() + 30.0, row.center().y), egui::Align2::LEFT_CENTER, "Include column names", FontId::proportional(TXT_BODY), TEXT());
    if boxed.clicked() || resp.clicked() {
        ui.data_mut(|d| d.insert_temp(id, !on));
    }
    act
}

/// Rows of a Markdown or CSV copy are numeric when every filled value of the field is a number.
pub(super) fn infer_numeric(rows: &[Document], name: &str) -> bool {
    let vals: Vec<&Bson> = rows.iter().filter_map(|d| db::get_path(d, name)).filter(|v| !matches!(v, Bson::Null)).collect();
    !vals.is_empty() && vals.iter().all(|v| matches!(v, Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_)))
}

#[cfg(target_os = "macos")]
fn put_html(html: &str, plain: &str) -> bool {
    arboard::Clipboard::new().and_then(|mut c| c.set_html(html, Some(plain))).is_ok()
}

#[cfg(not(target_os = "macos"))]
fn put_html(_html: &str, _plain: &str) -> bool {
    false
}

impl App {
    /// The table, columns and key of the open SQL tab.
    fn sql_ctx(&self) -> Option<(Sel, Arc<Vec<Col>>, Vec<String>)> {
        let t = self.t()?;
        let s = t.sql.as_ref()?;
        Some((t.sel.clone(), s.cols.clone(), s.pk.clone()))
    }

    /// Whether the open connection's server reads backslash escapes in strings.
    pub(super) fn sql_bs(&self) -> bool {
        self.t().and_then(|t| self.conn(t.sel.conn)).and_then(|c| c.sql.as_ref()).map_or(true, |d| d.backslash_escapes())
    }

    /// Why the open table cannot be edited at all, if so.
    pub(super) fn sql_edit_block(&self) -> Option<&'static str> {
        let s = self.t()?.sql.as_ref()?;
        if s.matched.is_some() {
            Some("Grouped rows are summaries, not table rows. Ungroup to edit.")
        } else if self.sel_is_view() {
            Some("Views cannot be edited.")
        } else if s.pk.is_empty() {
            Some("This table has no usable primary key, so its rows cannot be edited.")
        } else {
            None
        }
    }

    /// Why one column cannot be edited in place, if so.
    pub(super) fn sql_col_block(&self, col: &str) -> Option<&'static str> {
        let s = self.t()?.sql.as_ref()?;
        if s.pk.iter().any(|p| p == col) {
            return Some("Primary key columns are not edited here: undo could no longer find the row.");
        }
        let c = s.cols.iter().find(|c| c.name == col)?;
        if c.generated {
            return Some("Generated columns are computed by the server and cannot be written.");
        }
        (c.kind == Kind::Geometry).then_some("Geometry values cannot be edited as text yet.")
    }

    /// Everything a write needs, or None with the reason shown. The one gate for SQL writes.
    fn writable_sql(&mut self) -> Option<(u64, db::sql::Db, Sel, Arc<Vec<Col>>, Vec<String>)> {
        let (sel, cols, pk) = self.sql_ctx()?;
        if self.demo {
            self.ok("Offline demo: no database is connected.");
            return None;
        }
        if !self.sel_writable() {
            self.warn_ro();
            return None;
        }
        let Some(db) = self.conn(sel.conn).and_then(|c| c.sql.clone()) else {
            self.warn_offline();
            return None;
        };
        Some((sel.conn, db, sel, cols, pk))
    }

    /// Double-click on a cell: opens the editor, after asking for write access when it is locked.
    pub(super) fn sql_open_cell(&mut self, i: usize, col: String) {
        if let Some(why) = self.sql_edit_block().or_else(|| self.sql_col_block(&col)) {
            return self.warn(why);
        }
        if !self.sel_writable() {
            return self.ask_unlock_then(move |a| a.sql_open_cell(i, col));
        }
        let Some(t) = self.t() else { return };
        let rows = Rows::Raw(&t.docs);
        let v = self.shown_row(rows, i, false).and_then(|d| d.get(&col).cloned()).or_else(|| rows.value(i, &col).map(|v| v.bson().into_owned())).unwrap_or(Bson::Null);
        self.sql_cell = Some(CellEdit { id: rows.id(i), col, text: edit_text(&v), err: None, focus: true, done: None });
    }

    /// Draws the editor over a cell and records Enter, Esc and a click elsewhere.
    pub(super) fn sql_cell_editor(&mut self, ui: &mut egui::Ui, cell: egui::Rect) {
        let Some(mut e) = self.sql_cell.take() else { return };
        let id = egui::Id::unique("sql-cell-editor");
        let rect = cell.shrink(1.0);
        ui.painter().rect(rect, 0, BG(), Stroke::new(1.5, if e.err.is_some() { ALERT() } else { PRIMARY() }), egui::StrokeKind::Inside);
        let out = ui
            .scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                egui::TextEdit::singleline(&mut e.text).id(id).font(FontId::monospace(TXT_SMALL)).frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(6, 0))).desired_width(rect.width()).show(ui)
            })
            .inner;
        if e.focus {
            out.response.request_focus();
            let mut st = out.state.clone();
            st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(e.text.chars().count()))));
            st.store(ui.ctx(), id);
            e.focus = false;
        }
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
            e.done = Some(false);
        } else if out.response.lost_focus() {
            e.done = Some(true);
        }
        if let Some(m) = &e.err {
            egui::Area::new(id.with("err")).order(egui::Order::Tooltip).fixed_pos(rect.left_bottom() + egui::vec2(0.0, 4.0)).show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| ui.label(RichText::new(m).size(TXT_SMALL).color(ALERT())));
            });
        }
        self.sql_cell = Some(e);
    }

    /// After the table is drawn: stage the edit, or keep the editor open with its error.
    pub(super) fn sql_cell_finish(&mut self) {
        let Some(mut e) = self.sql_cell.take() else { return };
        match e.done {
            None => self.sql_cell = Some(e),
            Some(false) => {}
            Some(true) => {
                let kind = self.t().and_then(|t| t.sql.as_ref()).and_then(|s| s.cols.iter().find(|c| c.name == e.col)).map_or(Kind::Text, |c| c.kind);
                let row = self.t().and_then(|t| (0..t.docs.len()).find(|&j| Rows::Raw(&t.docs).id(j) == e.id));
                match value::parse_edit(kind, &e.text) {
                    Ok(_) if row.is_none() => self.warn("That row is no longer in the table, so the edit was cancelled."),
                    Ok(v) => self.sql_set_cell(row.unwrap_or_default(), &e.col, v),
                    Err(m) => {
                        (e.err, e.done, e.focus) = (Some(m), None, true);
                        self.sql_cell = Some(e);
                    }
                }
            }
        }
    }

    /// Stages `v` as the value of a cell of the open page.
    pub(super) fn sql_set_cell(&mut self, i: usize, col: &str, v: Bson) {
        let Some(t) = self.t() else { return };
        let rows = Rows::Raw(&t.docs);
        let Some(orig) = rows.owned(i) else { return };
        let cur = self.shown_row(rows, i, false).unwrap_or(orig);
        let mut new = cur.clone();
        new.insert(col, v);
        self.sql_stage(&cur, new);
    }

    fn sql_stage(&mut self, d: &Document, new: Document) {
        let key = self.draft_key(d);
        let Some(sel) = self.t().map(|t| t.sel.clone()) else { return };
        let n = stage_draft(&mut self.drafts, &key, &sel, d, new);
        if n == 0 {
            self.ok("No changes left on this row");
        } else if self.prefs.auto_save && self.sel_writable() {
            self.write_sql_drafts(vec![key]);
        } else {
            self.ok(format!("{n} cell{} staged. Save writes them, Discard drops them.", if n == 1 { "" } else { "s" }));
        }
    }

    /// The columns of row `i` that have unsaved edits, for tinting.
    pub(super) fn sql_staged_cols(&self, rows: Rows, i: usize) -> Vec<String> {
        let Some(dr) = self.drafts.get(&self.draft_key_of_row(rows, i)) else { return Vec::new() };
        db::diff(&dr.original, &dr.current).into_iter().map(|c| c.0).filter(|p| p != "_id").collect()
    }

    fn draft_key_of_row(&self, rows: Rows, i: usize) -> String {
        let s = self.t().map(Tab::coll_key).unwrap_or_default();
        format!("{s}#{}", rows.id(i).map(|v| db::compact(&v)).unwrap_or_default())
    }

    /// One transaction for the given drafts. A failure keeps every one of them staged.
    pub(super) fn write_sql_drafts(&mut self, keys: Vec<String>) {
        let drafts: Vec<(String, Draft)> = keys.into_iter().filter_map(|k| self.drafts.remove(&k).map(|d| (k, d))).collect();
        let Some(first) = drafts.first().map(|d| Sel { conn: d.1.conn, db: d.1.db.clone(), coll: d.1.coll.clone() }) else { return };
        let put_back = |a: &mut App, drafts: Vec<(String, Draft)>, why: &str| {
            a.drafts.extend(drafts);
            a.err(why.to_string());
        };
        let Some(db) = self.conn(first.conn).and_then(|c| c.sql.clone()) else { return put_back(self, drafts, "Not connected.") };
        let read_only = !self.conn_writable(first.conn);
        let Some((cols, pk)) = self.tabs.iter().find(|t| t.sel == first).and_then(|t| t.sql.as_ref()).map(|s| (s.cols.to_vec(), s.pk.clone())) else { return put_back(self, drafts, "Open the table to save its edits.") };
        // A row without its key could address any row, so it is never sent; its edits stay staged.
        let (keyed, keyless): (Vec<_>, Vec<_>) = drafts.into_iter().partition(|(_, d)| d.current.get_document("_id").is_ok_and(|k| !k.is_empty()));
        let skipped = keyless.len();
        self.drafts.extend(keyless);
        if skipped > 0 {
            self.err(format!("{skipped} row{} without a primary key {} not saved.", if skipped == 1 { "" } else { "s" }, if skipped == 1 { "was" } else { "were" }));
        }
        let drafts = keyed;
        if drafts.is_empty() {
            return;
        }
        let edits: Vec<RowEdit> = drafts
            .iter()
            .map(|(_, d)| RowEdit {
                pk: d.current.get_document("_id").cloned().unwrap_or_default(),
                changes: db::diff(&d.original, &d.current).into_iter().filter(|c| c.0 != "_id").map(|(p, a, b)| (p, a.unwrap_or(Bson::Null), b.unwrap_or(Bson::Null))).collect(),
            })
            .collect();
        let tags: Vec<String> = drafts.iter().map(|d| d.0.clone()).collect();
        self.saving.extend(drafts);
        self.worker.sql_save(tags, first.conn, db, first.db, first.coll, cols, pk, edits, read_only);
    }

    /// Cmd+S, the bar's Save: asks for write access first, then confirms like Mongo's Save all when the setting says so.
    pub(super) fn sql_save_all(&mut self, keys: Vec<String>) {
        if !self.sel_writable() {
            return self.ask_unlock_then(move |a| a.sql_save_all(keys));
        }
        if self.prefs.confirm_save && !self.prefs.always_writable {
            self.modal = Some(Modal::SaveAll(keys));
        } else {
            self.write_sql_drafts(keys);
        }
    }

    /// The results with the staged-edits bar under them; the table shrinks to leave room.
    pub(super) fn sql_results(&mut self, ui: &mut egui::Ui) {
        let keys = self.tab_drafts();
        let room = if keys.is_empty() { 0.0 } else { STAGED_H + 8.0 };
        ui.scope(|ui| {
            ui.set_max_height((ui.available_height() - room).max(120.0));
            self.results(ui, false);
        });
        if !keys.is_empty() {
            ui.add_space(8.0);
            self.sql_staged_bar(ui, keys);
        }
    }

    fn sql_staged_bar(&mut self, ui: &mut egui::Ui, keys: Vec<String>) {
        let Some((sel, cols, _)) = self.sql_ctx() else { return };
        let (d, bs) = self.sql_dialect();
        let (mut cells, mut summary) = (0, String::new());
        for k in &keys {
            let Some(dr) = self.drafts.get(k) else { continue };
            let ch: Vec<_> = db::diff(&dr.original, &dr.current).into_iter().filter(|c| c.0 != "_id").collect();
            cells += ch.len();
            if summary.is_empty() {
                let lit = |n: &str, v: Option<&Bson>| copy::sql_literal(v, cols.iter().find(|c| c.name == n).is_some_and(|c| is_numeric(c.kind)), bs);
                let set: Vec<String> = ch.iter().take(2).map(|(p, _, b)| format!("{} = {}", d.quote_ident(p), lit(p, b.as_ref()))).collect();
                let wh: Vec<String> = dr.current.get_document("_id").map(|k| k.iter().map(|(n, v)| format!("{} = {}", d.quote_ident(n), lit(n, Some(v)))).collect()).unwrap_or_default();
                summary = format!("UPDATE {} SET {}{} WHERE {}", d.quote_ident(&sel.coll), set.join(", "), if ch.len() > 2 { ", …" } else { "" }, wh.join(" AND "));
            }
        }
        let rows = keys.len();
        let (mut save, mut discard, mut show) = (false, false, false);
        egui::Frame::new().fill(tint(WARNING(), 22)).stroke(Stroke::new(1.0, tint(WARNING(), 90))).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(12, 6)).show(ui, |ui| {
            ui.set_height(STAGED_H - 12.0);
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), Sense::hover());
                ui.painter().circle_filled(dot.center(), 3.5, WARNING());
                ui.label(semibold(RichText::new(format!("{cells} cell{} changed in {rows} row{}", if cells == 1 { "" } else { "s" }, if rows == 1 { "" } else { "s" })).color(BRIGHT())));
                let more = if rows > 1 { format!("  (+{} more row{})", rows - 1, if rows == 2 { "" } else { "s" }) } else { String::new() };
                let right = 280.0;
                ui.add_sized([(ui.available_width() - right).max(0.0), BUTTON_H], egui::Label::new(RichText::new(format!("{summary}{more}")).font(FontId::monospace(TXT_CAPTION)).color(MUTED())).truncate());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    save = primary(ui, "Save", PRIMARY()).on_hover_text("Write every staged edit in one transaction (Cmd+S)").clicked();
                    discard = quiet(ui, "Discard").on_hover_text("Drop the staged edits").clicked();
                    show = quiet(ui, "Preview SQL").on_hover_text("The statements Save will run").clicked();
                });
            });
        });
        let free = self.modal.is_none() && self.insert.is_none() && self.json_edit.is_none() && self.sql_cell.is_none();
        save |= free && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::S));
        if show {
            let mut body = Vec::new();
            for k in &keys {
                let Some(dr) = self.drafts.get(k) else { continue };
                let edit = RowEdit {
                    pk: dr.current.get_document("_id").cloned().unwrap_or_default(),
                    changes: db::diff(&dr.original, &dr.current).into_iter().filter(|c| c.0 != "_id").map(|(p, a, b)| (p, a.unwrap_or(Bson::Null), b.unwrap_or(Bson::Null))).collect(),
                };
                let (sql, params) = write::update_sql(d, &sel.db, &sel.coll, &cols, &edit);
                body.push(format!("{};", write::preview(&sql, &params, bs)));
            }
            self.modal = Some(Modal::Text { title: "SQL that Save will run".into(), body: body.join("\n"), code: false });
        }
        if discard {
            for k in &keys {
                self.drafts.remove(k);
            }
            self.ok("Discarded the staged edits");
        }
        if save {
            self.sql_save_all(keys);
        }
    }

    /// + Row, Duplicate and Delete in the results line.
    pub(super) fn sql_row_tools(&mut self, ui: &mut egui::Ui) {
        let n = self.t().map_or(0, |t| t.checked.len());
        let why = self.sql_edit_block();
        let none = if n == 0 { "Check rows first" } else { "" };
        if quiet_enabled(ui, n > 0 && why.is_none(), "Delete").on_hover_text(why.unwrap_or(if n == 0 { none } else { "Delete the checked rows" })).on_disabled_hover_text(why.unwrap_or(none)).clicked() {
            let rows = self.checked_sql_rows();
            self.docs_act(&ui.ctx().clone(), DocsAct::Delete, rows);
        }
        if quiet_enabled(ui, n > 0 && why.is_none(), "Duplicate").on_hover_text("Insert copies of the checked rows").on_disabled_hover_text(why.unwrap_or(none)).clicked() {
            let rows = self.checked_sql_rows();
            self.docs_act(&ui.ctx().clone(), DocsAct::Duplicate, rows);
        }
        if quiet_enabled(ui, why.is_none(), "+ Row").on_hover_text("Insert a row").on_disabled_hover_text(why.unwrap_or_default()).clicked() {
            self.sql_new_row(None);
        }
        let view = self.sel_is_view();
        if quiet_enabled(ui, !view, "Import…").on_hover_text("Add the rows of a CSV file to this table").on_disabled_hover_text("A view has no rows to add").clicked() {
            if let Some(s) = self.t().map(|t| t.sel.clone()) {
                self.open_sql_import(s.conn, s.db, s.coll);
            }
        }
        vsep(ui, 4.0);
    }

    fn checked_sql_rows(&self) -> Vec<Document> {
        let Some(t) = self.t() else { return Vec::new() };
        App::checked_docs(&t.checked, Rows::Raw(&t.docs))
    }

    /// Opens the insert form, filled from `from` for a duplicate.
    fn sql_new_row(&mut self, from: Option<Document>) {
        if !self.sel_writable() {
            return self.ask_unlock_then(move |a| a.sql_new_row(from));
        }
        let Some((_, cols, pk)) = self.sql_ctx() else { return };
        // A single integer key is almost always generated, so a copy leaves it to the server.
        let auto = pk.len() == 1 && cols.iter().any(|c| c.src == pk[0] && matches!(c.kind, Kind::Int | Kind::UInt));
        let fields = cols
            .iter()
            .map(|col| {
                let v = from.as_ref().and_then(|d| d.get(&col.name)).filter(|_| !(auto && col.src == pk[0]));
                let (mode, text) = match v {
                    None => (Mode::Default, String::new()),
                    Some(Bson::Null) => (Mode::Null, String::new()),
                    Some(v) => (Mode::Value, edit_text(v)),
                };
                RowField { col: col.clone(), mode, text }
            })
            .collect();
        let title = if from.is_some() { "Duplicate row" } else { "Insert a row" };
        self.modal = Some(Modal::SqlRow(Box::new(RowForm { title: title.into(), fields, err: None })));
    }

    pub(super) fn sql_row_dialog(&mut self, ui: &mut egui::Ui, f: &mut RowForm, keep: &mut bool) {
        let table = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
        ui.set_width(560.0);
        dialog_title(ui, format!("{} in {table}", f.title));
        ui.label(RichText::new("Default leaves the column to the server: its default, or the next auto-increment number.").size(TXT_SMALL).color(MUTED()));
        ui.add_space(6.0);
        egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
            egui::Grid::new("sql-row").num_columns(3).spacing([12.0, 6.0]).show(ui, |ui| {
                for fld in f.fields.iter_mut() {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&fld.col.name).monospace().color(BRIGHT()));
                        ui.label(RichText::new(&fld.col.ty).size(TXT_CAPTION).color(FAINT()));
                    });
                    let opts: Vec<(Mode, &str)> = if fld.col.nullable { vec![(Mode::Default, "Default"), (Mode::Null, "NULL"), (Mode::Value, "Value")] } else { vec![(Mode::Default, "Default"), (Mode::Value, "Value")] };
                    segmented(ui, &mut fld.mode, &opts);
                    let enabled = fld.mode == Mode::Value;
                    ui.add_enabled(enabled, egui::TextEdit::singleline(&mut fld.text).font(egui::TextStyle::Monospace).desired_width(220.0));
                    ui.end_row();
                }
            });
        });
        if let Some(m) = &f.err {
            ui.label(RichText::new(m).size(TXT_SMALL).color(ALERT()));
        }
        dialog_actions(ui, |ui| {
            if primary(ui, "Insert", PRIMARY()).clicked() {
                let mut row: NewRow = Vec::new();
                let mut bad = None;
                for fld in &f.fields {
                    match fld.mode {
                        Mode::Default => {}
                        Mode::Null => row.push((fld.col.name.clone(), Bson::Null)),
                        Mode::Value => match value::parse_edit(fld.col.kind, &fld.text) {
                            Ok(v) => row.push((fld.col.name.clone(), v)),
                            Err(m) => {
                                bad = Some(format!("{}: {m}", fld.col.name));
                                break;
                            }
                        },
                    }
                }
                f.err = bad;
                if f.err.is_none() {
                    self.sql_insert_rows(vec![row]);
                    *keep = false;
                }
            }
            if ui.button("Cancel").clicked() {
                *keep = false;
            }
        });
    }

    fn sql_insert_rows(&mut self, rows: Vec<NewRow>) {
        let Some((conn, db, sel, cols, pk)) = self.writable_sql() else { return };
        let read_only = !self.conn_writable(conn);
        self.worker.sql_insert(conn, db, sel.db, sel.coll, cols.to_vec(), pk, rows, read_only);
    }

    /// Rows are the loaded documents of the checked or clicked rows, each with its key in `_id`.
    pub(super) fn sql_docs_act(&mut self, ctx: &egui::Context, act: DocsAct, rows: Vec<Document>) {
        if rows.is_empty() {
            return;
        }
        let n = rows.len();
        let writes = matches!(act, DocsAct::Duplicate | DocsAct::SetValue | DocsAct::Delete);
        if writes {
            if let Some(why) = self.sql_edit_block() {
                return self.warn(why);
            }
            if !self.sel_writable() {
                let ctx = ctx.clone();
                return self.ask_unlock_then(move |a| a.sql_docs_act(&ctx, act, rows));
            }
        }
        let (sel, cols, pk) = self.sql_ctx().unwrap_or_else(|| (Sel { conn: 0, db: String::new(), coll: String::new() }, Arc::new(Vec::new()), Vec::new()));
        let (d, bs) = self.sql_dialect();
        // The key names are the server's, which differ from the shown name of a renamed column.
        let numeric = |name: &str| cols.iter().find(|c| c.src == name).is_some_and(|c| is_numeric(c.kind));
        let key_lit = |r: &Document, p: &String| copy::sql_literal(r.get_document("_id").ok().and_then(|k| k.get(p)), numeric(p), bs);
        // `pk IN (...)` for one key column, a row-value list for several.
        let key_list = || -> String {
            if pk.len() == 1 {
                format!("{} IN ({})", d.quote_ident(&pk[0]), rows.iter().map(|r| key_lit(r, &pk[0])).collect::<Vec<_>>().join(", "))
            } else {
                format!("({}) IN ({})", pk.iter().map(|p| d.quote_ident(p)).collect::<Vec<_>>().join(", "), rows.iter().map(|r| format!("({})", pk.iter().map(|p| key_lit(r, p)).collect::<Vec<_>>().join(", "))).collect::<Vec<_>>().join(", "))
            }
        };
        match act {
            DocsAct::CopyAs(f) => self.copy_rows_as(ctx, f, &rows),
            DocsAct::Filter => {
                if pk.is_empty() {
                    return self.warn("This table has no usable primary key to filter by.");
                }
                let text = key_list();
                let t = self.tm().unwrap();
                t.filter_txt = text;
                t.page = 0;
                self.reload();
            }
            DocsAct::EditQuery => {
                if pk.is_empty() {
                    return self.warn("This table has no usable primary key to address these rows.");
                }
                let text = format!("UPDATE {}\nSET \nWHERE {};", d.qualified(&sel.db, &sel.coll), key_list());
                let t = self.tm().unwrap();
                if let Some(s) = t.sql.as_mut() {
                    s.query = text;
                }
                t.view = View::Sql;
            }
            DocsAct::Export => self.open_export(rows, false),
            DocsAct::Duplicate => {
                if let [one] = rows.as_slice() {
                    return self.sql_new_row(Some(one.clone()));
                }
                let pk_shown = cols.iter().find(|c| pk.len() == 1 && c.src == pk[0]).map(|c| (c.name.clone(), matches!(c.kind, Kind::Int | Kind::UInt)));
                let auto = pk_shown.as_ref().is_some_and(|p| p.1);
                let pk_name = pk_shown.map(|p| p.0).unwrap_or_default();
                let copies: Vec<NewRow> = rows.iter().map(|r| r.iter().filter(|(k, _)| *k != "_id" && !(auto && **k == pk_name)).map(|(k, v)| (k.clone(), v.clone())).collect()).collect();
                self.sql_insert_rows(copies);
            }
            DocsAct::SetValue => {
                let first = cols.iter().find(|c| !pk.contains(&c.src) && c.kind != Kind::Geometry).map(|c| c.name.clone());
                let Some(col) = first else { return self.warn("No column of this table can be set.") };
                self.modal = Some(Modal::SqlSet(Box::new(SetForm { rows, col, text: String::new(), null: false, err: None })));
            }
            DocsAct::Delete => {
                if n > 1 {
                    self.modal = Some(Modal::Typed { title: format!("Delete {n} rows of {}?", sel.coll), expect: sel.coll.clone(), typed: String::new(), action: Danger::DeleteRows(sel.clone(), rows) });
                } else {
                    self.modal = Some(Modal::Delete(sel.clone(), rows));
                }
            }
            DocsAct::CopyJs | DocsAct::CopyJson | DocsAct::CopyLines | DocsAct::Update => {}
        }
    }

    /// Deletes rows by key once confirmed. Their staged edits go with them.
    pub(super) fn sql_delete(&mut self, rows: Vec<Document>) {
        let Some((conn, db, sel, cols, pk)) = self.writable_sql() else { return };
        for r in &rows {
            let key = self.draft_key(r);
            self.drafts.remove(&key);
        }
        if let Some(t) = self.tm() {
            t.selected = None;
        }
        let read_only = !self.conn_writable(conn);
        self.worker.sql_delete_rows(conn, db, sel.db, sel.coll, cols.to_vec(), pk, rows, read_only);
    }

    pub(super) fn sql_set_dialog(&mut self, ui: &mut egui::Ui, f: &mut SetForm, keep: &mut bool) {
        let Some((sel, cols, pk)) = self.sql_ctx() else { return *keep = false };
        let (d, bs) = self.sql_dialect();
        ui.set_width(560.0);
        let n = f.rows.len();
        dialog_title(ui, format!("Set value on {n} row{}", if n == 1 { "" } else { "s" }));
        let ids: Vec<String> = f.rows.iter().take(4).map(|r| r.get_document("_id").map(|k| k.values().map(db::short).collect::<Vec<_>>().join("-")).unwrap_or_default()).collect();
        ui.label(RichText::new(format!("{} · rows {}{}", sel.coll, ids.join(", "), if n > 4 { ", …" } else { "" })).color(MUTED()));
        ui.add_space(6.0);
        let editable: Vec<&Col> = cols.iter().filter(|c| !pk.contains(&c.src) && c.kind != Kind::Geometry).collect();
        let col = editable.iter().find(|c| c.name == f.col).copied().or(editable.first().copied());
        let Some(col) = col.cloned() else { return *keep = false };
        egui::Grid::new("sql-set").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
            form_label(ui, "Column");
            egui::ComboBox::from_id_salt("sql-set-col").width(300.0).selected_text(format!("{}  {}", col.name, col.ty.to_ascii_uppercase())).show_ui(ui, |ui| {
                menu_look(ui);
                for c in &editable {
                    if ui.selectable_label(c.name == col.name, format!("{}  {}", c.name, c.ty.to_ascii_uppercase())).clicked() {
                        (f.col, f.text, f.null, f.err) = (c.name.clone(), String::new(), false, None);
                    }
                }
            });
            ui.end_row();
            form_label(ui, "Value");
            ui.vertical(|ui| {
                let opts = enum_values(&col.ty);
                if col.kind == Kind::Enum && !opts.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        for o in &opts {
                            if chip(ui, &format!("'{o}'"), PRIMARY(), !f.null && f.text == *o).clicked() {
                                (f.text, f.null) = (o.clone(), false);
                            }
                        }
                    });
                } else {
                    ui.add_enabled(!f.null, egui::TextEdit::singleline(&mut f.text).font(egui::TextStyle::Monospace).desired_width(300.0));
                }
                if col.nullable {
                    ui.checkbox(&mut f.null, "NULL");
                }
            });
            ui.end_row();
        });
        let parsed = if f.null { Ok(Bson::Null) } else if col.kind == Kind::Enum { Ok(Bson::String(f.text.clone())) } else { value::parse_edit(col.kind, &f.text) };
        ui.add_space(4.0);
        ui.label(RichText::new(format!("Staged as {n} cell edit{}. Nothing runs until Save, and Undo reverts them together.", if n == 1 { "" } else { "s" })).size(TXT_SMALL).color(FAINT()));
        ui.add_space(6.0);
        let lit = match &parsed {
            Ok(v) => copy::sql_literal(Some(v), is_numeric(col.kind), bs),
            Err(_) => "…".into(),
        };
        let keys = if pk.len() == 1 { format!("{} IN ({})", d.quote_ident(&pk[0]), f.rows.iter().map(|r| copy::sql_literal(r.get_document("_id").ok().and_then(|k| k.get(&pk[0])), cols.iter().any(|c| c.src == pk[0] && is_numeric(c.kind)), bs)).collect::<Vec<_>>().join(", ")) } else { format!("{} composite keys", n) };
        egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(10).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("UPDATE {}\n  SET {} = {lit}\n  WHERE {keys};", d.quote_ident(&sel.coll), d.quote_ident(&col.src))).monospace().size(TXT_SMALL).color(TEXT()));
        });
        if let (Some(m), false) = (&f.err, f.null) {
            ui.label(RichText::new(m).size(TXT_SMALL).color(ALERT()));
        }
        dialog_actions(ui, |ui| {
            if primary(ui, &format!("Stage {n} edit{}", if n == 1 { "" } else { "s" }), PRIMARY()).clicked() {
                match parsed {
                    Ok(v) => {
                        for r in &f.rows {
                            let key = self.draft_key(r);
                            let sel = sel.clone();
                            // Staged on top of any earlier edit of the same row, which keeps its original.
                            let cur = self.drafts.get(&key).map(|x| x.current.clone()).unwrap_or_else(|| r.clone());
                            let mut new = cur.clone();
                            new.insert(col.name.clone(), v.clone());
                            stage_draft(&mut self.drafts, &key, &sel, &cur, new);
                        }
                        self.ok(format!("Staged {n} edit{}. Save writes them.", if n == 1 { "" } else { "s" }));
                        *keep = false;
                    }
                    Err(m) => f.err = Some(m),
                }
            }
            if ui.button("Cancel").clicked() {
                *keep = false;
            }
        });
    }

    /// The visible columns in the order shown, for the copy formats.
    pub(super) fn copy_cols(&self, rows: &[Document]) -> Vec<CopyCol> {
        let Some(t) = self.t() else { return Vec::new() };
        let prefs = self.prefs.cols.get(&self.col_key(false)).cloned().unwrap_or_default();
        table::visible_cols(Some(&prefs), &t.fields, self.default_cols())
            .into_iter()
            .map(|name| {
                let col = t.sql.as_ref().and_then(|s| s.cols.iter().find(|c| c.name == name));
                let numeric = match t.sql.as_ref() {
                    Some(_) => col.is_some_and(|c| is_numeric(c.kind)),
                    None => infer_numeric(rows, &name),
                };
                let src = col.map_or_else(|| name.clone(), |c| c.src.clone());
                CopyCol { name, src, numeric }
            })
            .collect()
    }

    /// Copies rows in `fmt`. HTML also goes to the clipboard as rich text on macOS, so it pastes as a table.
    pub(super) fn copy_rows_as(&mut self, ctx: &egui::Context, fmt: Fmt, rows: &[Document]) {
        let cols = self.copy_cols(rows);
        let (table, pk) = match self.t() {
            Some(t) => (t.sel.coll.clone(), t.sql.as_ref().map(|s| s.pk.clone()).unwrap_or_default()),
            None => return,
        };
        let bs = self.sql_bs();
        self.copy_sheet(ctx, fmt, rows, &cols, &table, &pk, bs);
    }

    /// `copy_rows_as` for rows whose columns and table are given, such as a Query result.
    pub(super) fn copy_sheet(&mut self, ctx: &egui::Context, fmt: Fmt, rows: &[Document], cols: &[CopyCol], table: &str, pk: &[String], bs: bool) {
        let header = ctx.data(|d| d.get_temp::<bool>(egui::Id::unique("copy-with-header"))).unwrap_or(true);
        let d = self.sql_dialect().0;
        let sheet = Sheet { rows, cols, table, pk, bs };
        let text = copy::format_in(d, fmt, &sheet, header);
        if text.is_empty() {
            return self.warn(if fmt == Fmt::SqlUpdate { "SQL UPDATE needs a table with a primary key." } else { "Nothing to copy." });
        }
        let rich = fmt == Fmt::Html && put_html(&text, &copy::format_in(d, Fmt::Tsv, &sheet, header));
        if !rich {
            ctx.copy_text(text);
        }
        let name = match fmt {
            Fmt::SqlInsert => "SQL INSERT",
            Fmt::SqlUpdate => "SQL UPDATE",
            Fmt::Csv => "CSV",
            Fmt::Tsv => "TSV",
            Fmt::Json => "JSON",
            Fmt::JsonLines => "JSON lines",
            Fmt::Markdown => "a Markdown table",
            Fmt::Html => "an HTML table",
            Fmt::Values => "values",
        };
        let n = rows.len();
        self.ok(format!("Copied {n} {} as {name}", if n == 1 { "row" } else { "rows" }));
    }

    /// The cell menu's actions, once the table is drawn.
    pub(super) fn sql_cell_acts(&mut self, ctx: &egui::Context, acts: Vec<(usize, String, CellAct)>) {
        for (i, col, a) in acts {
            let Some(t) = self.t() else { return };
            let rows = Rows::Raw(&t.docs);
            let v = self.shown_row(rows, i, false).and_then(|d| d.get(&col).cloned()).or_else(|| rows.value(i, &col).map(|v| v.bson().into_owned())).unwrap_or(Bson::Null);
            match a {
                CellAct::CopyValue => {
                    ctx.copy_text(match &v {
                        Bson::Null => String::new(),
                        other => edit_text(other),
                    });
                    self.ok("Copied the value");
                }
                CellAct::SetNull => {
                    if let Some(why) = self.sql_edit_block().or_else(|| self.sql_col_block(&col)) {
                        return self.warn(why);
                    }
                    if !self.sel_writable() {
                        return self.ask_unlock_then(move |a| a.sql_set_cell(i, &col, Bson::Null));
                    }
                    self.sql_set_cell(i, &col, Bson::Null);
                }
                CellAct::FilterValue => {
                    let numeric = self.t().and_then(|t| t.sql.as_ref()).and_then(|s| s.cols.iter().find(|c| c.name == col)).is_some_and(|c| is_numeric(c.kind));
                    let q = self.sql_dialect().0.quote_ident(&col);
                    let text = if v == Bson::Null { format!("{q} IS NULL") } else { format!("{q} = {}", copy::sql_literal(Some(&v), numeric, self.sql_bs())) };
                    let t = self.tm().unwrap();
                    t.filter_txt = text;
                    t.page = 0;
                    self.reload();
                }
            }
        }
    }

    /// The offline demo of `QUARRY_TEST_SQL=edit`: two cells of row 3 staged.
    pub(super) fn sql_demo_stage(&mut self) {
        let Some(t) = self.tabs.get_mut(0) else { return };
        let Some(orig) = t.docs.get(2).map(db::Row::owned) else { return };
        let mut old = orig.clone();
        old.insert("status", "pending");
        old.insert("note", Bson::Null);
        if let Ok(r) = db::Row::from_doc(&old) {
            t.docs[2] = r;
        }
        let sel = t.sel.clone();
        let key = format!("{}#{}", t.coll_key(), db::compact(old.get("_id").unwrap_or(&Bson::Null)));
        stage_draft(&mut self.drafts, &key, &sel, &old, orig);
    }
}

/// Height of the staged-edits bar, with its padding.
const STAGED_H: f32 = 40.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_options_are_read_from_the_type() {
        assert_eq!(enum_values("enum('pending','paid','it''s')"), ["pending", "paid", "it's"]);
        assert_eq!(enum_values("ENUM('a,b','c')"), ["a,b", "c"]);
        assert!(enum_values("varchar(20)").is_empty());
        assert!(enum_values("ENUM").is_empty());
    }

    #[test]
    fn edit_text_round_trips_through_parse_edit() {
        for (k, v) in [(Kind::Int, Bson::Int64(-5)), (Kind::Decimal, Bson::String("12.50".into())), (Kind::Text, Bson::String("a b".into())), (Kind::Binary, Bson::Binary(mongodb::bson::Binary { subtype: mongodb::bson::spec::BinarySubtype::Generic, bytes: vec![1, 2] }))] {
            assert_eq!(value::parse_edit(k, &edit_text(&v)), Ok(v.clone()), "{k:?}");
        }
        assert_eq!(value::parse_edit(Kind::Int, &edit_text(&Bson::Null)), Ok(Bson::Null));
    }

    #[test]
    fn numbers_are_inferred_for_documents() {
        let rows = vec![doc! { "a": 1, "b": "x", "c": Bson::Null }, doc! { "a": 2.5, "b": 3, "c": Bson::Null }];
        assert!(infer_numeric(&rows, "a"));
        assert!(!infer_numeric(&rows, "b"));
        assert!(!infer_numeric(&rows, "c"));
    }
}
