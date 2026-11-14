//! Editing SQL rows from the grid: cell edits staged as drafts, the staged-edits bar, row inserts,
//! deletes, Set value, and the "Copy as" actions that Mongo documents share.

use crate::app::*;
use db::copy::{self, CopyCol, Fmt, Sheet};
use db::sql::check;
use db::sql::value::{self, Kind};
use db::sql::write::{self, NewRow, RowEdit};
use db::sql::Col;

/// A cell being edited in place. `done` is set by the editor: Some(true) to commit, Some(false) to cancel.
#[derive(Default)]
pub(in crate::app) struct CellEdit {
    /// The row's key, because a reload while the editor is open moves rows around.
    pub(in crate::app) id: Option<Bson>,
    pub(in crate::app) col: String,
    pub(in crate::app) text: String,
    pub(in crate::app) err: Option<String>,
    pub(in crate::app) focus: bool,
    pub(in crate::app) done: Option<bool>,
    /// Shown in the large editor instead of over the cell.
    pub(in crate::app) big: bool,
    /// Edited since it opened, so the side panel must not drop it for another value.
    pub(in crate::app) dirty: bool,
    /// The large editor's live check of a JSON column: None until the text is checked again after an edit.
    pub(in crate::app) json_err: Option<Option<String>>,
    /// The calendar is open, holding the text from before it opened for Cancel.
    pub(in crate::app) pick: Option<String>,
    /// The row of the value list the arrows moved to, None until they are used.
    pub(in crate::app) hl: Option<usize>,
    /// What the helpers under the cell covered last frame, so a click in them is not a click outside the editor.
    pub(in crate::app) pop: Option<egui::Rect>,
    /// Give the text box focus back without selecting its text, after a helper button took it.
    pub(in crate::app) refocus: bool,
    /// A helper reads plain keys (arrows, Space, letters) with no text box focused, so the page must not.
    pub(in crate::app) grabs_keys: bool,
    pub(in crate::app) keys: Option<KeyLook>,
}

/// Rows of the table a foreign key column points at, looked up as the key is typed.
#[derive(Default)]
pub(in crate::app) struct KeyLook {
    /// The single-column foreign key of the edited column, found once the table structure is loaded.
    pub(in crate::app) fk: Option<db::sql::structure::ForeignKey>,
    pub(in crate::app) req: u64,
    /// The text the last lookup was for, and whether its answer is still on its way.
    pub(in crate::app) asked: Option<String>,
    pub(in crate::app) busy: bool,
    /// What the first answer learned about the referenced table, sent with the next lookups.
    pub(in crate::app) table: Option<db::sql::KeyTable>,
    pub(in crate::app) rows: Vec<(String, String)>,
}

impl CellEdit {
    /// Whether this editor belongs to the cell. A missing key never matches: two keyless rows are not the same row.
    pub(in crate::app) fn is_at(&self, id: Option<Bson>, col: &str) -> bool {
        self.id.is_some() && self.id == id && self.col == col
    }
}

/// The "Set value on N rows" dialog.
pub(in crate::app) struct SetForm {
    pub(in crate::app) rows: Vec<Document>,
    pub(in crate::app) col: String,
    pub(in crate::app) text: String,
    pub(in crate::app) null: bool,
    pub(in crate::app) err: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
pub(in crate::app) enum Mode {
    Default,
    Null,
    Value,
}

pub(in crate::app) struct RowField {
    col: Col,
    mode: Mode,
    text: String,
}

/// The "+ Row" dialog: one line per column.
pub(in crate::app) struct RowForm {
    title: String,
    fields: Vec<RowField>,
    err: Option<String>,
}

/// What a cell's context menu asked for, besides the row actions.
#[derive(Clone, Copy)]
pub(in crate::app) enum CellAct {
    CopyValue,
    View,
    SetNull,
    FilterValue,
}

pub(in crate::app) fn is_numeric(k: Kind) -> bool {
    matches!(k, Kind::Int | Kind::UInt | Kind::Float | Kind::Decimal)
}

/// Text for the editor of a `kind` column: what the cell shows, as typed back it parses to the same value.
pub(in crate::app) fn edit_text(b: &Bson, kind: Kind) -> String {
    match b {
        Bson::Null => "NULL".into(),
        Bson::String(s) if value::text_like(kind) && value::null_like(s) => format!("'{s}'"),
        Bson::String(s) if kind == Kind::Json && s.trim() == "null" => "'null'".into(),
        Bson::String(s) => s.clone(),
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => n.to_string(),
        Bson::Double(f) => f.to_string(),
        Bson::Binary(x) => format!("0x{}", x.bytes.iter().map(|b| format!("{b:02X}")).collect::<String>()),
        other => db::short(other),
    }
}

/// The menu of a SQL cell: the value actions first. Returns the one picked.
pub(in crate::app) fn sql_cell_menu(ui: &mut egui::Ui, nullable: bool, editable: bool) -> Option<CellAct> {
    let mut act = None;
    if ui.button("Copy value").clicked() {
        act = Some(CellAct::CopyValue);
    }
    if ui.button("View value…").clicked() {
        act = Some(CellAct::View);
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
pub(in crate::app) fn copy_as_menu(ui: &mut egui::Ui, sql: bool) -> Option<DocsAct> {
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
pub(in crate::app) fn infer_numeric(rows: &[Document], name: &str) -> bool {
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

/// The zone line of the calendar for a date and time column.
fn zone_note(d: db::sql::Dialect, col: &Col) -> String {
    if col.kind != Kind::DateTime {
        return String::new();
    }
    let zoned = check::zoned(d, col);
    let name = match (d == db::sql::Dialect::Postgres, zoned) {
        (true, true) => "TIMESTAMPTZ",
        (_, false) if d != db::sql::Dialect::Postgres => "DATETIME",
        _ => "TIMESTAMP",
    };
    if zoned { format!("{name} · read and written in the session time zone") } else { format!("{name} · saved as typed, no time zone") }
}

/// A boolean cell: the value in the cell and a true, false and NULL switch under it. TINYINT(1) columns save 1 and 0.
fn sql_bool_cell(ui: &mut egui::Ui, e: &mut CellEdit, col: &Col, rect: egui::Rect, below: egui::Pos2, click_out: bool) {
    let tiny = col.kind != Kind::Bool;
    let cur = match e.text.trim().to_ascii_lowercase().as_str() {
        "null" => None,
        "1" | "true" | "t" | "yes" => Some(true),
        _ => Some(false),
    };
    let word = |v: Option<bool>| match v {
        None => "NULL",
        Some(true) => if tiny { "1" } else { "true" },
        Some(false) => if tiny { "0" } else { "false" },
    };
    let color = match cur {
        None => C_NULL(),
        Some(true) => C_TRUE(),
        Some(false) => C_FALSE(),
    };
    place(ui, rect, word(cur), FontId::monospace(TXT_SMALL), color, false, 6.0);
    let keyed = pickers::bool_keys(ui, cur, col.nullable);
    let label = format!("{} · {}", col.src, col.ty.to_uppercase());
    let (clicked, r) = pickers::bool_pick(ui.ctx(), egui::Id::unique("sql-bool-cell"), below, cur, col.nullable, &label);
    e.pop = Some(r);
    e.grabs_keys = true;
    if let Some(m) = &e.err {
        pickers::callout(ui.ctx(), egui::Id::unique("sql-bool-err"), rect.left_top() - egui::vec2(0.0, 4.0), egui::Align2::LEFT_BOTTOM, m, Some(crate::app::errors::paint_alert as fn(&egui::Painter, egui::Pos2, Color32)), None);
    }
    if let Some(v) = keyed.or(clicked) {
        (e.text, e.dirty) = (word(v).into(), true);
    }
    if clicked.is_some() {
        e.done = Some(true);
    } else if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
        e.done = Some(false);
    } else if click_out || ui.input(|i| i.key_pressed(Key::Enter)) {
        e.done = Some(true);
    }
}

impl App {
    /// The table, columns and key of the open SQL tab.
    fn sql_ctx(&self) -> Option<(Sel, Arc<Vec<Col>>, Vec<String>)> {
        let t = self.t()?;
        let s = t.sql.as_ref()?;
        Some((t.sel.clone(), s.cols.clone(), s.pk.clone()))
    }

    /// Whether the open connection's server reads backslash escapes in strings.
    pub(in crate::app) fn sql_bs(&self) -> bool {
        self.t().and_then(|t| self.conn(t.sel.conn)).and_then(|c| c.sql.as_ref()).is_none_or(|d| d.backslash_escapes())
    }

    /// Why the open table cannot be edited at all, if so.
    pub(in crate::app) fn sql_edit_block(&self) -> Option<&'static str> {
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

    /// A column of the open SQL table, by its key in the row documents.
    pub(in crate::app) fn sql_col(&self, name: &str) -> Option<&Col> {
        self.t()?.sql.as_ref()?.cols.iter().find(|c| c.name == name)
    }

    /// Why one column cannot be edited in place, if so.
    pub(in crate::app) fn sql_col_block(&self, col: &str) -> Option<&'static str> {
        let s = self.t()?.sql.as_ref()?;
        if s.pk.iter().any(|p| p == col) {
            return Some("Primary key columns can't be edited inline, because changing the key would break undo for this row. Use a query instead.");
        }
        let c = self.sql_col(col)?;
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
        let Some((db, _)) = self.sql_target(&sel) else {
            self.warn_offline();
            return None;
        };
        Some((sel.conn, db, sel, cols, pk))
    }

    /// Double-click on a cell: opens the editor, after asking for write access when it is locked.
    pub(in crate::app) fn sql_open_cell(&mut self, i: usize, col: String) {
        if let Some(why) = self.sql_edit_block().or_else(|| self.sql_col_block(&col)) {
            return self.warn(why);
        }
        if !self.sel_writable() {
            return self.ask_unlock_then(move |a| a.sql_open_cell(i, col));
        }
        if self.panel_dirty() {
            return self.warn("Apply or cancel the value in the side panel first.");
        }
        let Some(t) = self.t() else { return };
        let rows = Rows::Raw(&t.docs);
        // A row without a key would match every keyless row in the grid and could never be written back.
        if rows.id(i).is_none() {
            return self.warn("This row has no key, so it cannot be edited here.");
        }
        let v = self.shown_row(rows, i, false).and_then(|d| d.get(&col).cloned()).or_else(|| rows.value(i, &col).map(|v| v.bson().into_owned())).unwrap_or(Bson::Null);
        let kind = self.sql_col(&col).map_or(Kind::Text, |c| c.kind);
        let text = edit_text(&v, kind);
        // With the panel open, a double-click loads the new cell into it, so the panel follows the selection.
        let big = value_edit::is_long(&text) || self.value_in_panel();
        self.sql_cell = Some(CellEdit { json_err: None, id: rows.id(i), col, text, dirty: false, err: None, focus: true, done: None, big, ..Default::default() });
    }

    /// Draws the editor over a cell and records Enter, Esc and a click elsewhere. Under it goes the helper the column
    /// type wants: the calendar for dates, a list for ENUM, SET and foreign key columns, a switch for booleans, and
    /// otherwise the line that names a problem as it is typed.
    pub(in crate::app) fn sql_cell_editor(&mut self, ui: &mut egui::Ui, cell: egui::Rect) {
        let Some(mut e) = self.sql_cell.take() else { return };
        let id = egui::Id::unique("sql-cell-editor");
        let rect = cell.shrink(1.0);
        let d = self.sql_dialect();
        let col = self.sql_col(&e.col).cloned().unwrap_or_else(|| Col::plain(&e.col, "", Kind::Text, true));
        let clock = self.sql_clock(&col);
        let now = clock.map_or_else(pickers::local_now, pickers::Civil::from_ms);
        let checked = check::check(d, &col, &e.text, clock);
        let bad = e.err.is_some() || checked.value.is_err();
        ui.painter().rect(rect, 0, BG(), Stroke::new(1.5, if bad { ALERT() } else { PRIMARY() }), egui::StrokeKind::Inside);
        // The large editor has the text, so the cell only keeps its outline.
        if e.big {
            self.sql_cell = Some(e);
            return;
        }
        let ctx = ui.ctx().clone();
        let below = rect.left_bottom() + egui::vec2(0.0, 4.0);
        let last_pop = e.pop.take();
        let ours = |p: egui::Pos2| rect.contains(p) || last_pop.is_some_and(|r| r.contains(p));
        let click = ui.input(|i| i.pointer.any_pressed().then(|| i.pointer.interact_pos()).flatten());
        let click_out = click.is_some_and(|p| !ours(p));
        let nullable = col.nullable || col.auto_increment;
        if check::is_bool(d, &col) {
            sql_bool_cell(ui, &mut e, &col, rect, below, click_out);
            self.sql_cell = Some(e);
            return;
        }
        let date = matches!(col.kind, Kind::Date | Kind::DateTime);
        let choices = if matches!(col.kind, Kind::Enum | Kind::Set) { db::sql::schema::enum_values(&col.ty) } else { Vec::new() };
        let set = col.kind == Kind::Set && !choices.is_empty();
        let typed = e.text.trim().to_lowercase();
        let (list, title, foot): (Vec<(String, String)>, Option<String>, String) = if col.kind == Kind::Enum {
            // Unedited, the list shows every option; typing narrows it.
            (choices.iter().filter(|c| !e.dirty || c.to_lowercase().contains(&typed)).map(|c| (c.clone(), String::new())).collect(), None, String::new())
        } else if date || set {
            (Vec::new(), None, String::new())
        } else {
            self.sql_key_lookup(&mut e, &col).unwrap_or_default()
        };
        let list_null = nullable && !list.is_empty();
        let list_n = list.len() + usize::from(list_null);
        if e.pick.is_some() {
            let mut v = pickers::parse_sql(&e.text);
            if pickers::date_keys(ui, id, &mut v, now) && let Some(c) = v {
                (e.text, e.dirty, e.err) = (pickers::sql_text_like(c, col.kind == Kind::DateTime, &e.text), true, None);
            }
        } else if list_n > 0 {
            pickers::list_keys(ui, &mut e.hl, list_n);
        }
        let alt_down = date && ui.input_mut(|i| i.consume_key(egui::Modifiers::ALT, Key::ArrowDown));
        let pad = (rect.height() - EXPAND_BTN) / 2.0;
        let btn = egui::Rect::from_min_size(egui::pos2(rect.right() - pad - EXPAND_BTN, rect.top() + pad), egui::vec2(EXPAND_BTN, EXPAND_BTN));
        // Taken before the text box sees it, where Enter alone would commit.
        let expand = expand_key(ui, id);
        // One text row centered in the cell, where the value sat before the double click.
        let font = FontId::monospace(TXT_SMALL);
        let row_h = ui.fonts_mut(|f| f.row_height(&font));
        let text_rect = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.center().y - row_h / 2.0), egui::pos2(btn.left(), rect.center().y + row_h / 2.0));
        let out = ui
            .scope_builder(egui::UiBuilder::new().max_rect(text_rect), |ui| {
                egui::TextEdit::singleline(&mut e.text).id(id).font(font).frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(6, 0))).desired_width(btn.left() - rect.left() - 16.0).show(ui)
            })
            .inner;
        let (cal, b) = if date {
            (pickers::glyph_button(ui, btn, id.with("cal"), pickers::paint_calendar, e.pick.is_some(), "Pick a date (Alt+Down)").clicked(), false)
        } else {
            (false, ui.put(btn, expand_button()).clicked())
        };
        if e.focus {
            out.response.request_focus();
            let mut st = out.state.clone();
            st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(e.text.chars().count()))));
            st.store(ui.ctx(), id);
            e.focus = false;
        }
        if std::mem::take(&mut e.refocus) {
            out.response.request_focus();
        }
        if out.response.changed() {
            (e.dirty, e.err, e.hl) = (true, None, None);
        }
        // Without the arrows, Enter takes the first row only for text the column could not save as typed, and only
        // from a list that answers this text, never an older one still on screen.
        let fresh = e.keys.as_ref().is_none_or(|k| !k.busy && k.asked.as_deref() == Some(if typed == "null" { "" } else { e.text.trim() }));
        let hl = e.hl.or((e.dirty && fresh && checked.value.is_err() && list_n > 0).then_some(0));
        let mut pop: Option<egui::Rect> = None;
        let mut commit = false;
        if let Some(orig) = e.pick.clone() {
            let mut v = pickers::parse_sql(&e.text);
            let o = pickers::DateOpts { time: col.kind == Kind::DateTime, millis: false, zone: pickers::Zone::Note(zone_note(d, &col)), nullable, now, now_button: clock.is_some() };
            let (changed, act, r) = pickers::date_picker(&ctx, id.with("date"), below, &mut v, o);
            pop = Some(r);
            if changed && let Some(c) = v {
                (e.text, e.dirty, e.err) = (pickers::sql_text_like(c, col.kind == Kind::DateTime, &e.text), true, None);
            }
            match act {
                Some(pickers::PickOut::Apply) => (e.pick, commit) = (None, true),
                Some(pickers::PickOut::Null) => (e.text, e.pick, commit) = ("NULL".into(), None, true),
                Some(pickers::PickOut::Cancel) => (e.text, e.pick, e.refocus) = (orig, None, true),
                None => {}
            }
        } else if set {
            let parts: Vec<String> = e.text.split(',').map(|x| x.trim().to_lowercase()).collect();
            let mut on: Vec<bool> = choices.iter().map(|c| parts.contains(&c.to_lowercase())).collect();
            let (t, r) = pickers::check_list(&ctx, id.with("set"), below, rect.width().max(200.0), &choices, &on);
            pop = Some(r);
            if let Some(i) = t {
                on[i] = !on[i];
                // Written in the column's own order, which is how the server reads it back.
                e.text = choices.iter().zip(&on).filter(|x| *x.1).map(|x| x.0.as_str()).collect::<Vec<_>>().join(",");
                (e.dirty, e.err, e.refocus) = (true, None, true);
            }
        } else if list_n > 0 || title.is_some() {
            let saved = (!typed.eq_ignore_ascii_case("null")).then_some(e.text.trim());
            let (picked, r) = pickers::choice_list(&ctx, id.with("list"), below, rect.width().max(220.0), title.as_deref(), &list, saved, hl, list_null, &foot);
            pop = Some(r);
            if let Some(i) = picked {
                (e.text, commit) = (list.get(i).map_or("NULL".into(), |x| x.0.clone()), true);
            }
        }
        // The problem line goes under the cell, or over it when a helper fills the space under; a note only goes under.
        let shown = match checked.value.as_ref().err().or(e.err.as_ref()) {
            Some(m) => Some((m.clone(), true)),
            None => checked.note.clone().map(|n| (n, false)),
        };
        if let Some((m, is_err)) = shown.filter(|s| pop.is_none() || s.1) {
            let (at, pivot) = if pop.is_none() { (below, egui::Align2::LEFT_TOP) } else { (rect.left_top() - egui::vec2(0.0, 4.0), egui::Align2::LEFT_BOTTOM) };
            let (r, fixed) = pickers::callout(&ctx, id.with("check"), at, pivot, &m, is_err.then_some(crate::app::errors::paint_alert as fn(&egui::Painter, egui::Pos2, Color32)), checked.fix.as_ref().map(|f| format!("Use {f}")).as_deref());
            pop = Some(pop.map_or(r, |p| p.union(r)));
            if fixed && let Some(f) = &checked.fix {
                (e.text, e.dirty, e.err, e.refocus) = (f.clone(), true, None, true);
            }
        }
        let esc = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
        let (enter, tab) = ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Tab)));
        if alt_down || cal {
            e.pick = if e.pick.is_some() { None } else { Some(e.text.clone()) };
            e.refocus = true;
        } else if expand || b {
            (e.big, e.focus, e.pick) = (true, true, None);
        } else if esc {
            // Esc closes the calendar first, and the editor only when nothing is open over it.
            match e.pick.take() {
                Some(o) => (e.text, e.refocus) = (o, true),
                None => e.done = Some(false),
            }
        } else if commit {
            e.done = Some(true);
        } else if click_out {
            // A press outside the cell and its helpers, seen in the frame it happens. The text box's lost focus is not
            // used for clicks: egui reports it on the release or later, when the press is gone.
            e.done = Some(true);
        } else if (enter || tab) && out.response.lost_focus() || enter && !out.response.has_focus() {
            // Enter or Tab from the text box, or Enter after a helper (a day, a time field, a checkbox) took the focus.
            // Enter on a highlighted row saves that row rather than the typed text.
            if let Some(h) = hl.filter(|h| enter && *h < list_n) {
                e.text = list.get(h).map_or("NULL".into(), |x| x.0.clone());
            }
            e.done = Some(true);
        }
        e.pop = pop;
        e.grabs_keys = e.pick.is_some() || list_n > 0;
        self.sql_cell = Some(e);
    }

    /// The clock in the zone `col` is read in, as milliseconds of that wall clock. None for a TIMESTAMP column under
    /// the server's own zone, which Quarry does not know.
    fn sql_clock(&self, col: &Col) -> Option<i64> {
        if !check::zoned(self.sql_dialect(), col) {
            return Some(pickers::local_now().to_ms());
        }
        let conn = self.conn(self.t()?.sel.conn)?;
        let off = conn.meta.advanced.offset_secs(db::sql::advanced::local_offset_secs())?;
        Some(pickers::utc_now_ms() + i64::from(off) * 1000)
    }

    /// The rows a single-column foreign key can point at, looked up as the key is typed: the list, its title and its
    /// footer. None for other columns.
    fn sql_key_lookup(&mut self, e: &mut CellEdit, col: &Col) -> Option<(Vec<(String, String)>, Option<String>, String)> {
        if e.keys.is_none() {
            // Found once, after the structure the grid loads for its key links has arrived.
            self.t().and_then(|t| t.sql.as_ref()).filter(|s| s.st.data.is_some())?;
            let fk = self.sql_fks().into_iter().find(|k| k.cols.len() == 1 && k.cols[0] == col.src);
            e.keys = Some(KeyLook { fk, ..Default::default() });
        }
        let look = e.keys.as_mut()?;
        let fk = look.fk.clone()?;
        let key = fk.ref_cols.first()?.clone();
        let t = e.text.trim();
        let q = if t.eq_ignore_ascii_case("null") { "" } else { t };
        // One lookup at a time; the answer for older text asks again with the newest.
        if !look.busy && look.asked.as_deref() != Some(q) && !self.demo && let Some((db, space)) = self.t().map(|t| t.sel.clone()).and_then(|s| self.sql_target(&s)) {
            self.req += 1;
            (look.req, look.busy, look.asked) = (self.req, true, Some(q.to_string()));
            let schema = if fk.ref_db.is_empty() { space } else { fk.ref_db.clone() };
            self.worker.sql_key_search(self.req, db, schema, fk.ref_table.clone(), key.clone(), q.to_string(), look.table.clone());
        }
        let title = format!("{} references {} ({key})", col.src, fk.ref_table);
        let saves = look.rows.get(e.hl.unwrap_or(0)).map(|r| format!(" Picking one saves {}.", r.0)).unwrap_or_default();
        let foot = match (look.table.as_ref().and_then(|t| t.label.as_ref()), look.rows.is_empty()) {
            _ if look.asked.is_none() => String::new(),
            (_, true) if look.busy => "Looking…".into(),
            (_, true) => format!("No {} row matches.", fk.ref_table),
            (Some(l), false) => format!("Matches {key} or {l}.{saves}"),
            (None, false) => format!("Matches {key}.{saves}"),
        };
        Some((look.rows.clone(), look.asked.is_some().then_some(title), foot))
    }

    pub(in crate::app) fn sql_keys_done(&mut self, req: u64, res: db::Res<(db::sql::KeyTable, Vec<(String, String)>)>) {
        let Some(k) = self.sql_cell.as_mut().and_then(|e| e.keys.as_mut()).filter(|k| k.req == req) else { return };
        k.busy = false;
        // A lookup the server refuses leaves the list empty; the key can still be typed and saved.
        match res {
            Ok((t, rows)) => (k.table, k.rows) = (Some(t), rows),
            // Treated as a big table with no name column, so later lookups skip the catalog and only match exact keys.
            Err(_) => (k.table, k.rows) = (Some(db::sql::KeyTable { label: None, big: true }), Vec::new()),
        }
    }

    /// After the table is drawn: stage the edit, or keep the editor open with its error.
    pub(in crate::app) fn sql_cell_finish(&mut self) {
        let Some(mut e) = self.sql_cell.take() else { return };
        match e.done {
            None => self.sql_cell = Some(e),
            Some(false) => {}
            Some(true) => {
                let col = self.sql_col(&e.col).cloned().unwrap_or_else(|| Col::plain(&e.col, "", Kind::Text, true));
                let row = self.t().and_then(|t| (0..t.docs.len()).find(|&j| Rows::Raw(&t.docs).id(j) == e.id));
                match check::check(self.sql_dialect(), &col, &e.text, self.sql_clock(&col)).value {
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
    pub(in crate::app) fn sql_set_cell(&mut self, i: usize, col: &str, v: Bson) {
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
    pub(in crate::app) fn sql_staged_cols(&self, rows: Rows, i: usize) -> Vec<String> {
        let Some(dr) = self.drafts.get(&self.draft_key_of_row(rows, i)) else { return Vec::new() };
        db::diff(&dr.original, &dr.current).into_iter().map(|c| c.0).filter(|p| p != "_id").collect()
    }

    fn draft_key_of_row(&self, rows: Rows, i: usize) -> String {
        let s = self.t().map(Tab::coll_key).unwrap_or_default();
        format!("{s}#{}", rows.id(i).map(|v| db::compact(&v)).unwrap_or_default())
    }

    /// One transaction for the given drafts. A failure keeps every one of them staged.
    pub(in crate::app) fn write_sql_drafts(&mut self, keys: Vec<String>) {
        let drafts: Vec<(String, Draft)> = keys.into_iter().filter_map(|k| self.drafts.remove(&k).map(|d| (k, d))).collect();
        let Some(first) = drafts.first().map(|d| Sel { conn: d.1.conn, db: d.1.db.clone(), schema: d.1.schema.clone(), coll: d.1.coll.clone() }) else { return };
        let put_back = |a: &mut App, drafts: Vec<(String, Draft)>, why: &str| {
            a.drafts.extend(drafts);
            a.err(why.to_string());
        };
        let Some((db, space)) = self.sql_target(&first) else { return put_back(self, drafts, "Not connected.") };
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
        self.worker.sql_save(tags, first.conn, db, space, first.coll, cols, pk, edits, read_only);
    }

    /// Cmd+S, the bar's Save: asks for write access first, then confirms like Mongo's Save all when the setting says so.
    pub(in crate::app) fn sql_save_all(&mut self, keys: Vec<String>) {
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
    pub(in crate::app) fn sql_results(&mut self, ui: &mut egui::Ui) {
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
        let (d, bs) = (self.sql_dialect(), self.sql_bs());
        let (mut cells, mut summary) = (0, String::new());
        for k in &keys {
            let Some(dr) = self.drafts.get(k) else { continue };
            let ch: Vec<_> = db::diff(&dr.original, &dr.current).into_iter().filter(|c| c.0 != "_id").collect();
            cells += ch.len();
            if summary.is_empty() {
                let lit = |n: &str, v: Option<&Bson>| copy::sql_literal(d, v, cols.iter().find(|c| c.name == n).is_some_and(|c| is_numeric(c.kind)), bs);
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
        // Any window over the page owns Cmd+S, as do the value and cell editors.
        let free = !self.window_open() && self.json_edit.is_none() && self.sql_cell.is_none();
        save |= free && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::S));
        if show {
            let mut body = Vec::new();
            for k in &keys {
                let Some(dr) = self.drafts.get(k) else { continue };
                let edit = RowEdit {
                    pk: dr.current.get_document("_id").cloned().unwrap_or_default(),
                    changes: db::diff(&dr.original, &dr.current).into_iter().filter(|c| c.0 != "_id").map(|(p, a, b)| (p, a.unwrap_or(Bson::Null), b.unwrap_or(Bson::Null))).collect(),
                };
                let (sql, params) = write::update_sql(d, sel.space(), &sel.coll, &cols, &edit);
                body.push(format!("{};", write::preview(d, &sql, &params, bs)));
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
    pub(in crate::app) fn sql_row_tools(&mut self, ui: &mut egui::Ui) {
        let n = self.t().map_or(0, |t| t.checked.len());
        let why = self.sql_edit_block();
        let none = if n == 0 { "Check rows first" } else { "" };
        // Every row the page's WHERE matches, beyond the page on screen. Drawn first, so it sits rightmost.
        let mut bulk = None;
        dots_menu(ui, |ui| {
            menu_look(ui);
            if ui.add_enabled(why.is_none(), egui::Button::new("Update matching…")).on_hover_text("Set columns on every row the WHERE matches").on_disabled_hover_text(why.unwrap_or_default()).clicked() {
                bulk = Some(true);
                ui.close();
            }
            ui.separator();
            if ui.add_enabled(why.is_none(), egui::Button::new(RichText::new("Delete all matching rows…").color(ALERT()))).on_disabled_hover_text(why.unwrap_or_default()).clicked() {
                bulk = Some(false);
                ui.close();
            }
        })
        .on_hover_text("More actions");
        if let Some(update) = bulk {
            self.open_bulk(update);
        }
        // Checked rows start the dialog on them; otherwise it starts on every row the WHERE matches.
        if quiet(ui, "Export…").on_hover_text("Write the checked rows, this page or every matching row to a file").clicked() {
            let rows = self.checked_sql_rows();
            self.open_export(rows, n == 0);
        }
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
        {
            if quiet_enabled(ui, !view, "Import…").on_hover_text("Add the rows of a CSV file to this table").on_disabled_hover_text("A view has no rows to add").clicked() && let Some(s) = self.t().map(|t| t.sel.clone()) {
                let schema = self.sel_schema(&s);
                self.open_sql_import(s.conn, s.db, schema, s.coll);
            }
        }
        vsep(ui, 4.0);
    }

    fn checked_sql_rows(&self) -> Vec<Document> {
        let Some(t) = self.t() else { return Vec::new() };
        App::checked_docs(&t.checked, Rows::Raw(&t.docs))
    }

    /// File > Insert Document on a SQL tab: the row form lives on the Content grid, so the tab goes there first.
    #[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
    pub(in crate::app) fn sql_insert_row_menu(&mut self) {
        if self.t().is_none_or(|t| t.sel.coll.is_empty()) {
            return self.warn("Open a table first to insert a row.");
        }
        self.set_view(View::Documents);
        if let Some(why) = self.sql_edit_block() {
            return self.warn(why);
        }
        self.sql_new_row(None);
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
                    Some(v) => (Mode::Value, edit_text(v, col.kind)),
                };
                RowField { col: col.clone(), mode, text }
            })
            .collect();
        let title = if from.is_some() { "Duplicate row" } else { "Insert a row" };
        self.modal = Some(Modal::SqlRow(Box::new(RowForm { title: title.into(), fields, err: None })));
    }

    pub(in crate::app) fn sql_row_dialog(&mut self, ui: &mut egui::Ui, f: &mut RowForm, keep: &mut bool) {
        let table = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
        let dialect = self.sql_dialect();
        ui.set_width(560.0);
        dialog_head(ui, &Head::new(format!("{} in {table}", f.title)).sub("Default leaves a column to the server: its default or the next number").glyph(glyphs::plus));
        let mut b = body_begin(ui);
        let outer = ui;
        let ui = &mut b.content_ui;
        plain_card(ui, |ui| egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
            egui::Grid::new("sql-row").num_columns(3).spacing([12.0, 6.0]).show(ui, |ui| {
                for fld in f.fields.iter_mut() {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&fld.col.name).monospace().color(BRIGHT()));
                        ui.label(RichText::new(&fld.col.ty).size(TXT_CAPTION).color(FAINT()));
                    });
                    let opts: Vec<(Mode, &str)> = if fld.col.nullable { vec![(Mode::Default, "Default"), (Mode::Null, "NULL"), (Mode::Value, "Value")] } else { vec![(Mode::Default, "Default"), (Mode::Value, "Value")] };
                    segmented(ui, &mut fld.mode, &opts);
                    let enabled = fld.mode == Mode::Value;
                    ui.add_enabled(enabled, egui::TextEdit::singleline(&mut fld.text).font(egui::TextStyle::Monospace).desired_width(220.0)).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, enabled, format!("Value of {}", fld.col.name)));
                    ui.end_row();
                }
            });
        }));
        if let Some(m) = &f.err {
            field_note(ui, m, ALERT());
        }
        b.end(outer);
        let ui = outer;
        dialog_foot(ui, |ui| {
            if primary(ui, "Insert", PRIMARY()).clicked() {
                let mut row: NewRow = Vec::new();
                let mut bad = None;
                for fld in &f.fields {
                    match fld.mode {
                        Mode::Default => {}
                        Mode::Null => row.push((fld.col.name.clone(), Bson::Null)),
                        Mode::Value => match value::parse_col(dialect, &fld.col, &fld.text) {
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
        self.worker.sql_insert(conn, db, sel.space().to_string(), sel.coll, cols.to_vec(), pk, rows, read_only);
    }

    /// Rows are the loaded documents of the checked or clicked rows, each with its key in `_id`.
    pub(in crate::app) fn sql_docs_act(&mut self, ctx: &egui::Context, act: DocsAct, rows: Vec<Document>) {
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
        let (sel, cols, pk) = self.sql_ctx().unwrap_or_else(|| (Sel { conn: 0, db: String::new(), coll: String::new(), ..Default::default() }, Arc::new(Vec::new()), Vec::new()));
        let (d, bs) = (self.sql_dialect(), self.sql_bs());
        // The key names are the server's, which differ from the shown name of a renamed column.
        let numeric = |name: &str| cols.iter().find(|c| c.src == name).is_some_and(|c| is_numeric(c.kind));
        let key_lit = |r: &Document, p: &String| copy::sql_literal(d, r.get_document("_id").ok().and_then(|k| k.get(p)), numeric(p), bs);
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
                let text = format!("UPDATE {}\nSET \nWHERE {};", d.qualified(sel.space(), &sel.coll), key_list());
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
    pub(in crate::app) fn sql_delete(&mut self, rows: Vec<Document>) {
        let Some((conn, db, sel, cols, pk)) = self.writable_sql() else { return };
        for r in &rows {
            let key = self.draft_key(r);
            self.drafts.remove(&key);
        }
        if let Some(t) = self.tm() {
            t.selected = None;
        }
        let read_only = !self.conn_writable(conn);
        self.worker.sql_delete_rows(conn, db, sel.space().to_string(), sel.coll, cols.to_vec(), pk, rows, read_only);
    }

    pub(in crate::app) fn sql_set_dialog(&mut self, ui: &mut egui::Ui, f: &mut SetForm, keep: &mut bool) {
        let Some((sel, cols, pk)) = self.sql_ctx() else { return *keep = false };
        let (d, bs) = (self.sql_dialect(), self.sql_bs());
        ui.set_width(560.0);
        let n = f.rows.len();
        let ids: Vec<String> = f.rows.iter().take(4).map(|r| r.get_document("_id").map(|k| k.values().map(db::short).collect::<Vec<_>>().join("-")).unwrap_or_default()).collect();
        dialog_head(ui, &Head::new(format!("Set value on {n} row{}", if n == 1 { "" } else { "s" })).sub(format!("{}  ·  rows {}{}", sel.coll, ids.join(", "), if n > 4 { ", …" } else { "" })).glyph(glyphs::pencil));
        let mut b = body_begin(ui);
        let outer = ui;
        let ui = &mut b.content_ui;
        let editable: Vec<&Col> = cols.iter().filter(|c| !pk.contains(&c.src) && c.kind != Kind::Geometry).collect();
        let col = editable.iter().find(|c| c.name == f.col).copied().or(editable.first().copied());
        let Some(col) = col.cloned() else { return *keep = false };
        form_grid(ui, "sql-set", |ui| {
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
                let opts = db::sql::schema::enum_values(&col.ty);
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
        let parsed = if f.null { Ok(Bson::Null) } else if col.kind == Kind::Enum { Ok(Bson::String(f.text.clone())) } else { value::parse_col(d, &col, &f.text) };
        ui.add_space(4.0);
        ui.label(RichText::new(format!("Staged as {n} cell edit{}. Nothing runs until Save, and Undo reverts them together.", if n == 1 { "" } else { "s" })).size(TXT_SMALL).color(FAINT()));
        ui.add_space(6.0);
        let lit = match &parsed {
            Ok(v) => copy::sql_literal(d, Some(v), is_numeric(col.kind), bs),
            Err(_) => "…".into(),
        };
        let keys = if pk.len() == 1 { format!("{} IN ({})", d.quote_ident(&pk[0]), f.rows.iter().map(|r| copy::sql_literal(d, r.get_document("_id").ok().and_then(|k| k.get(&pk[0])), cols.iter().any(|c| c.src == pk[0] && is_numeric(c.kind)), bs)).collect::<Vec<_>>().join(", ")) } else { format!("{} composite keys", n) };
        egui::Frame::new().fill(WELL()).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_BAR).inner_margin(10).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("UPDATE {}\n  SET {} = {lit}\n  WHERE {keys};", d.quote_ident(&sel.coll), d.quote_ident(&col.src))).monospace().size(TXT_SMALL).color(TEXT()));
        });
        if let (Some(m), false) = (&f.err, f.null) {
            field_note(ui, m, ALERT());
        }
        b.end(outer);
        let ui = outer;
        dialog_foot(ui, |ui| {
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
    pub(in crate::app) fn copy_cols(&self, rows: &[Document]) -> Vec<CopyCol> {
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
    pub(in crate::app) fn copy_rows_as(&mut self, ctx: &egui::Context, fmt: Fmt, rows: &[Document]) {
        let cols = self.copy_cols(rows);
        let (table, pk) = match self.t() {
            Some(t) => (t.sel.coll.clone(), t.sql.as_ref().map(|s| s.pk.clone()).unwrap_or_default()),
            None => return,
        };
        let bs = self.sql_bs();
        self.copy_sheet(ctx, fmt, rows, &cols, &table, &pk, bs);
    }

    /// `copy_rows_as` for rows whose columns and table are given, such as a Query result.
    pub(in crate::app) fn copy_sheet(&mut self, ctx: &egui::Context, fmt: Fmt, rows: &[Document], cols: &[CopyCol], table: &str, pk: &[String], bs: bool) {
        let header = ctx.data(|d| d.get_temp::<bool>(egui::Id::unique("copy-with-header"))).unwrap_or(true);
        let d = self.sql_dialect();
        let sheet = Sheet { rows, cols, table, pk, bs };
        let text = copy::format(d, fmt, &sheet, header);
        if text.is_empty() {
            return self.warn(if fmt == Fmt::SqlUpdate { "SQL UPDATE needs a table with a primary key." } else { "Nothing to copy." });
        }
        let rich = fmt == Fmt::Html && put_html(&text, &copy::format(d, Fmt::Tsv, &sheet, header));
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
    pub(in crate::app) fn sql_cell_acts(&mut self, ctx: &egui::Context, acts: Vec<(usize, String, CellAct)>) {
        for (i, col, a) in acts {
            let Some(t) = self.t() else { return };
            let rows = Rows::Raw(&t.docs);
            let v = self.shown_row(rows, i, false).and_then(|d| d.get(&col).cloned()).or_else(|| rows.value(i, &col).map(|v| v.bson().into_owned())).unwrap_or(Bson::Null);
            match a {
                CellAct::CopyValue => {
                    ctx.copy_text(super::view::plain_text(&v));
                    self.ok("Copied the value");
                }
                CellAct::View => self.sql_view_value(&col, &v),
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
                    let numeric = self.sql_col(&col).is_some_and(|c| is_numeric(c.kind));
                    let q = self.sql_dialect().quote_ident(&col);
                    let text = if v == Bson::Null { format!("{q} IS NULL") } else { format!("{q} = {}", copy::sql_literal(self.sql_dialect(), Some(&v), numeric, self.sql_bs())) };
                    let t = self.tm().unwrap();
                    t.filter_txt = text;
                    t.page = 0;
                    self.reload();
                }
            }
        }
    }

    /// The offline demo of `QUARRY_TEST_SQL=edit`: two cells of row 3 staged.
    pub(in crate::app) fn sql_demo_stage(&mut self) {
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
    use crate::db::sql::Dialect;


    #[test]
    fn edit_text_round_trips_through_parse_edit() {
        for (k, v) in [(Kind::Int, Bson::Int64(-5)), (Kind::Decimal, Bson::String("12.50".into())), (Kind::Text, Bson::String("a b".into())), (Kind::Binary, Bson::Binary(mongodb::bson::Binary { subtype: mongodb::bson::spec::BinarySubtype::Generic, bytes: vec![1, 2] }))] {
            assert_eq!(value::parse_edit(Dialect::MySql, k, &edit_text(&v, k)), Ok(v.clone()), "{k:?}");
        }
        assert_eq!(value::parse_edit(Dialect::MySql, Kind::Int, &edit_text(&Bson::Null, Kind::Int)), Ok(Bson::Null));
    }

    #[test]
    fn bare_null_is_null_and_quoted_null_is_text() {
        let note = Col::plain("note", "TEXT", Kind::Text, true);
        for t in ["NULL", "null", " Null "] {
            assert_eq!(value::parse_col(Dialect::Postgres, &note, t), Ok(Bson::Null), "{t}");
        }
        for t in ["'NULL'", "\"null\""] {
            assert_eq!(value::parse_col(Dialect::MySql, &note, t), Ok(Bson::String(t[1..t.len() - 1].into())), "{t}");
        }
        // The text NULL, with its quotes or without, survives an unchanged edit; JSON null stays valid JSON.
        for t in ["NULL", "'null'", "\"NULL\"", " null "] {
            let text = Bson::String(t.into());
            assert_eq!(value::parse_col(Dialect::MySql, &note, &edit_text(&text, Kind::Text)), Ok(text), "{t}");
        }
        let json = Bson::String("null".into());
        assert_eq!(value::parse_edit(Dialect::MySql, Kind::Json, &edit_text(&json, Kind::Json)), Ok(json));
        let id = Col { auto_increment: true, ..Col::plain("id", "INT", Kind::Int, false) };
        assert_eq!(value::parse_col(Dialect::MySql, &id, "NULL"), Ok(Bson::Null));
        let name = Col::plain("name", "VARCHAR(20)", Kind::Text, false);
        assert!(value::parse_col(Dialect::MySql, &name, "NULL").is_err());
        assert_eq!(value::parse_col(Dialect::MySql, &name, "'NULL'"), Ok(Bson::String("NULL".into())));
    }

    #[test]
    fn numbers_are_inferred_for_documents() {
        let rows = vec![doc! { "a": 1, "b": "x", "c": Bson::Null }, doc! { "a": 2.5, "b": 3, "c": Bson::Null }];
        assert!(infer_numeric(&rows, "a"));
        assert!(!infer_numeric(&rows, "b"));
        assert!(!infer_numeric(&rows, "c"));
    }

    #[test]
    fn keyless_rows_never_own_the_editor() {
        let e = |id: Option<Bson>| CellEdit { json_err: None, id, col: "last_name".into(), text: String::new(), err: None, focus: false, done: None, big: false, dirty: false, ..Default::default() };
        // A Shell result has no keys: every row reports None, and the editor must not appear in all of them.
        assert!(!e(None).is_at(None, "last_name"));
        let keyed = e(Some(Bson::Int32(7)));
        assert!(keyed.is_at(Some(Bson::Int32(7)), "last_name"));
        assert!(!keyed.is_at(Some(Bson::Int32(8)), "last_name"));
        assert!(!keyed.is_at(None, "last_name"));
        assert!(!keyed.is_at(Some(Bson::Int32(7)), "first_name"));
    }

    #[test]
    fn editor_opens_again_after_it_is_closed() {
        // Esc leaves done = Some(false); the finish step drops the editor, and a fresh one starts clean.
        let mut cell = Some(CellEdit { json_err: None, id: Some(Bson::Int32(1)), col: "a".into(), text: "x".into(), err: None, focus: true, done: Some(false), big: false, dirty: false, ..Default::default() });
        if cell.as_ref().is_some_and(|e| e.done == Some(false)) {
            cell = None;
        }
        assert!(cell.is_none());
        cell = Some(CellEdit { json_err: None, id: Some(Bson::Int32(1)), col: "a".into(), text: "x".into(), err: None, focus: true, done: None, big: false, dirty: false, ..Default::default() });
        assert!(cell.unwrap().is_at(Some(Bson::Int32(1)), "a"));
    }
}
