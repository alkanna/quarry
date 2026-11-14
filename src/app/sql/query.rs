//! The Query view of a SQL tab: a script editor, Run, Stop and Explain, and one result tab per statement.

use crate::app::*;
use crate::app::column_menu::{query_col_popover, HeadAct};
use crate::app::sql::edit::CellAct;
use crate::ui::dnd;
use crate::db::sql::{lex, value::Kind, Col, ResultSet};
use db::copy::{CopyCol, Fmt};
use crate::ui::sql_complete::{open_list, sql_ac_field};
use egui::text::{CCursor, CCursorRange};
use std::ops::Range;
use std::sync::atomic::Ordering;

/// A fixed line height, so the gutter numbers and the statement band line up with the text.
const LINE_H: f32 = 20.0;

/// 1-based line and column of a caret given as a char index.
pub(in crate::app) fn line_col(text: &str, caret: usize) -> (usize, usize) {
    text.chars().take(caret).fold((1, 1), |(l, c), ch| if ch == '\n' { (l + 1, 1) } else { (l, c + 1) })
}

fn byte_at(text: &str, caret: usize) -> usize {
    text.char_indices().nth(caret).map_or(text.len(), |(b, _)| b)
}

/// Where the caret stands: line and column, and the statement under it.
#[derive(Clone)]
pub(in crate::app) struct CaretInfo {
    key: (usize, u64, usize),
    pub(in crate::app) line_col: (usize, usize),
    pub(in crate::app) stmt: Option<(Range<usize>, usize, usize)>,
}

impl SqlTab {
    /// The caret's place in the script. Lexing the script is costly next to a frame, so the answer is kept
    /// until the text or the caret changes.
    pub(in crate::app) fn caret_info(&self, d: db::sql::Dialect) -> CaretInfo {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.query.hash(&mut h);
        let key = (self.query.len(), h.finish(), self.caret);
        let mut slot = self.caret_cache.borrow_mut();
        match &*slot {
            Some(c) if c.key == key => c.clone(),
            _ => {
                let c = CaretInfo { key, line_col: line_col(&self.query, self.caret), stmt: statement_pos(&self.query, self.caret, d) };
                *slot = Some(c.clone());
                c
            }
        }
    }
}

/// The statement under a caret (a char index) as a byte range, with its 1-based number and the count of statements.
pub(in crate::app) fn statement_pos(text: &str, caret: usize, d: db::sql::Dialect) -> Option<(Range<usize>, usize, usize)> {
    let r = lex::statement_at(text, byte_at(text, caret), d)?;
    let all = lex::split_statements(text, d);
    let i = all.iter().position(|(a, _)| *a == r)?;
    Some((r, i + 1, all.len()))
}

/// A statement on one line for a menu, from its first word of code: a leading comment would color the whole line as one.
fn preview(text: &str, d: db::sql::Dialect) -> String {
    let text = text.trim_start();
    // Most statements start with code, and the History menu builds every line each frame, so only those that open
    // with a comment are lexed.
    if !(text.starts_with("--") || text.starts_with("/*") || text.starts_with('#')) {
        return db::sql::server::one_line(text, 90);
    }
    let start = lex::tokens(text, d).into_iter().find(|(t, _)| !matches!(t, lex::Tok::Space | lex::Tok::Comment)).map_or(0, |(_, r)| r.start);
    db::sql::server::one_line(&text[start..], 90)
}

/// The database a run uses: the picked one, else the tab's own.
fn run_db(t: &Tab) -> String {
    t.sql.as_ref().map(|s| s.run_db.clone()).filter(|d| !d.is_empty()).unwrap_or_else(|| t.sel.db.clone())
}

/// The schema a PostgreSQL Query view runs in: the one typed in its schema field, else the tab's own when the run is in
/// the tab's own database. Another database has other schemas, so the tab's schema is never carried over to it and the
/// answer stays empty until that database's schemas arrive.
pub(in crate::app) fn run_schema_choice(typed: &str, run_db: &str, sel_db: &str, sel_schema: &str) -> String {
    let typed = typed.trim();
    if !typed.is_empty() {
        typed.to_string()
    } else if run_db.is_empty() || run_db == sel_db {
        sel_schema.to_string()
    } else {
        String::new()
    }
}

fn run_schema_of(t: &Tab) -> String {
    run_schema_choice(t.sql.as_ref().map_or("", |s| s.run_schema.as_str()), t.sql.as_ref().map_or("", |s| s.run_db.as_str()), &t.sel.db, &t.sel.schema)
}

/// The database a run connects to and the namespace inside it. A PostgreSQL connection is bound to one database, so
/// the `Db` points at it and the namespace is the tab's schema; MySQL picks the database per statement. Export of a
/// result uses it too, so the statement reads the same tables it ran against.
pub(in crate::app) fn run_target(t: &Tab, db: db::sql::Db) -> (db::sql::Db, String) {
    let (rd, rs) = t.sql.as_ref().map_or(("", ""), |s| (s.run_db.as_str(), s.run_schema.as_str()));
    target_of(t, db, rd, rs)
}

/// `run_target` of the run whose results are shown, for statements read again from them.
pub(in crate::app) fn ran_target(t: &Tab, db: db::sql::Db) -> (db::sql::Db, String) {
    match t.sql.as_ref().and_then(|s| s.ran_in.as_ref()) {
        Some((rd, rs)) => target_of(t, db, rd, rs),
        None => run_target(t, db),
    }
}

/// The target for a picked database `rd` (empty for the tab's own) and typed schema `rs`.
fn target_of(t: &Tab, db: db::sql::Db, rd: &str, rs: &str) -> (db::sql::Db, String) {
    let d = if rd.is_empty() { t.sel.db.clone() } else { rd.to_string() };
    if db.dialect == db::sql::Dialect::Postgres {
        return (db.at(&d), run_schema_choice(rs, rd, &t.sel.db, &t.sel.schema));
    }
    (db, d)
}

fn t_schema(t: Option<&Tab>) -> String {
    t.and_then(|t| t.sql.as_ref()).map(|s| s.run_schema.clone()).unwrap_or_default()
}

/// One result tab: the statement number, its kind and the outcome in color.
fn set_tab(ui: &mut egui::Ui, i: usize, set: &ResultSet, on: bool) -> egui::Response {
    let (detail, color) = if set.error.is_some() {
        (if set.refused { "refused: read-only" } else { "error" }.to_string(), ALERT())
    } else {
        let what = match set.affected {
            Some(n) => format!("{} affected", group(&n.to_string())),
            None => format!("{} {}", group(&set.rows.len().to_string()), if set.rows.len() == 1 { "row" } else { "rows" }),
        };
        (format!("{what} · {} ms", set.ms), if on { SUCCESS() } else { FAINT() })
    };
    let mut job = egui::text::LayoutJob::default();
    job.append(&(i + 1).to_string(), 0.0, mono_fmt(TXT_CAPTION, FAINT(), 400.0));
    job.append(&set.kind, 8.0, ui_fmt(TXT_SMALL, if on { BRIGHT() } else { MUTED() }, if on { W_MEDIUM } else { 400.0 }, 0.0));
    job.append(&detail, 8.0, mono_fmt(TXT_CAPTION, color, 400.0));
    let g = ui.painter().layout_job(job);
    let (rect, r) = ui.allocate_exact_size(egui::vec2(g.size().x + 20.0, BUTTON_H), Sense::click());
    let state = state_of(&r);
    ui.painter().rect(rect, R_MD, if on { PANEL() } else { hover_fill(state) }, Stroke::new(1.0, if on { BORDER() } else { Color32::TRANSPARENT }), egui::StrokeKind::Inside);
    ui.painter().galley(rect.center() - g.size() / 2.0 + egui::vec2(0.0, optical_dy(&g)), g, TEXT());
    focus_ring(ui, &r, rect, R_MD as f32);
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, on, "Result"));
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The key each column of a result goes by in its column choices. A join can return two columns of one name, so the
/// second and later ones carry their position after a separator the SQL names cannot hold.
pub(in crate::app) fn result_keys(cols: &[Col]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(cols.len());
    for (i, c) in cols.iter().enumerate() {
        out.push(if out.contains(&c.name) { format!("{}\u{1}{i}", c.name) } else { c.name.clone() });
    }
    out
}

/// What the grid asks for: a row-menu action with its row, a check click (row and whether Shift extends a
/// range) and the header's select all.
#[derive(Default)]
struct GridOut {
    act: Option<(usize, DocsAct)>,
    check: Option<(usize, bool)>,
    all: Option<bool>,
    /// A plain click on a row, which selects it.
    select: Option<usize>,
    /// A value to open in full, from a double click on its cell or the eye shown over it.
    view: Option<(String, Bson)>,
    /// Column menu picks, by column name. They change the page sort or this tab's column setup, never the query.
    head: Vec<(String, HeadAct)>,
    /// A cell menu pick: the row, the column name and the action.
    cell: Option<(usize, String, CellAct)>,
}

/// The rows of one result set as a grid with a checkbox column. Checked rows are tinted, and the menu of a
/// checked row acts on every checked row. `record` draws the selected row's record under it.
/// `prefs` are the column choices of this result, and `filter` whether the result came from one table, which
/// turns a cell into a filter on it.
fn grid(ui: &mut egui::Ui, set: &ResultSet, prefs: &ColPrefs, checked: &BTreeSet<usize>, sel: Option<usize>, scroll: bool, filter: bool, mut record: Option<&mut dyn FnMut(&mut egui::Ui)>) -> GridOut {
    let mono = FontId::monospace(TXT_SMALL);
    let cw = ui.fonts_mut(|f| f.glyph_width(&mono, '0'));
    // Widths from the headers and the first rows, so a wide cell further down is cut with an ellipsis.
    let wide = set.widest.get_or_init(|| {
        set.cols.iter().map(|c| set.rows.iter().take(60).filter_map(|r| r.value(&c.name)).map(|v| if let mongodb::bson::raw::RawBsonRef::String(s) = v { s.chars().count() + 2 } else { 10 }).max().unwrap_or(0)).collect()
    });
    let names = result_keys(&set.cols);
    // A chosen width wins over the measured one.
    let widths: Vec<f32> = set.cols.iter().zip(wide).enumerate().map(|(ci, (c, &longest))| prefs.widths.get(&names[ci]).copied().unwrap_or_else(|| ((longest.max(c.name.chars().count()) as f32) * cw + 28.0).clamp(80.0, 360.0))).collect();
    // The order the columns show in, as indexes into `set.cols`: pinned first, hidden ones left out. Headers drag to
    // reorder, and the choices live in `prefs`, kept for results with the same columns so the next page keeps them.
    let shown = table::visible_cols(Some(prefs), &names, usize::MAX);
    let order: Vec<usize> = shown.iter().filter_map(|n| names.iter().position(|c| c == n)).collect();
    let lead = CHECK_W + NUM_W;
    let total = lead + order.iter().map(|&ci| widths[ci]).sum::<f32>();
    let aligns: Vec<u8> = set.cols.iter().enumerate().map(|(ci, c)| prefs.align.get(&names[ci]).copied().unwrap_or(if is_numeric(c.kind) { 2 } else { 0 })).collect();
    let pins = shown.iter().filter(|c| prefs.pinned.contains(*c)).count();
    let mut out = GridOut::default();
    egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(1).show(ui, |ui| {
        let view_w = ui.available_width();
        let (head, _) = ui.allocate_exact_size(egui::vec2(view_w, TABLE_HEAD_H), Sense::hover());
        let area = ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::ScrollArea::both().id_salt("sql-grid").auto_shrink(false).show(ui, |ui| {
                let shift = ui.input(|i| i.modifiers.shift);
                let row_w = total.max(view_w - 2.0);
                for i in 0..set.rows.len() {
                    // Every row takes its room so the scroll stays true, but only rows on screen are drawn.
                    let (rr, resp) = ui.allocate_exact_size(egui::vec2(row_w, ROW_H), Sense::click());
                    if scroll && sel == Some(i) {
                        resp.scroll_to_me(None);
                    }
                    if ui.is_rect_visible(rr) {
                        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Row, true, format!("Result row {}", i + 1)));
                        let hovered = ui.rect_contains_pointer(rr) && !ui.ctx().any_popup_open();
                        table::paint_row(ui, rr, sel == Some(i), checked.contains(&i), hovered, resp.is_pointer_button_down_on(), i % 2 == 1);
                        ui.painter().text(egui::pos2(rr.left() + CHECK_W + 18.0, rr.center().y), egui::Align2::LEFT_CENTER, (i + 1).to_string(), FontId::monospace(TXT_CAPTION), FAINT());
                        if check_box(ui, egui::pos2(rr.left() + 13.0, rr.center().y), ui.scope_id().with(("check", i)), checked.contains(&i), false).clicked() {
                            out.check = Some((i, shift));
                        }
                        let mut x = rr.left() + lead;
                        for &ci in &order {
                            let (c, w) = (&set.cols[ci], &widths[ci]);
                            let cell = egui::Rect::from_min_size(egui::pos2(x, rr.top()), egui::vec2(*w, ROW_H));
                            x += w;
                            if !ui.is_rect_visible(cell) {
                                continue;
                            }
                            // Read by the whole name: a column name can hold a dot, which a path would split.
                            let v = set.rows[i].raw().get(&c.name).ok().flatten().map(Val::R);
                            if prefs.highlight.contains(&db::strip_indices(&names[ci])) {
                                ui.painter().rect_filled(cell, 0, HIGHLIGHT());
                            }
                            // The row takes the clicks, so a double click finds its cell by where it landed.
                            if resp.double_clicked() && resp.interact_pointer_pos().is_some_and(|p| cell.x_range().contains(p.x)) {
                                out.view = Some((c.name.clone(), Val::owned(v)));
                            }
                            let (mut job, color) = match v {
                                Some(v) => (sql_cell_job(v, c.kind, false), TEXT()),
                                None => (egui::text::LayoutJob::single_section("—".into(), mono_fmt(TXT_SMALL, FAINT(), 400.0)), FAINT()),
                            };
                            job.wrap = egui::text::TextWrapping::truncate_at_width(w - 14.0);
                            let g = ui.fonts_mut(|f| f.layout_job(job));
                            let gx = table::align_x(aligns[ci], cell.left(), *w, g.size().x);
                            ui.painter().with_clip_rect(cell.shrink2(egui::vec2(6.0, 0.0))).galley(egui::pos2(gx, cell.center().y - g.size().y / 2.0), g, color);
                            if let Some(v) = table::view_button(ui, cell, 0.0, v, ("eye", i, ci), || format!("View {} row {}", c.name, i + 1)) {
                                out.view = Some((c.name.clone(), v));
                            }
                        }
                        // Cmd or Shift on a row checks it like in the Content grid; a plain click selects it.
                        if resp.clicked() {
                            if shift || ui.input(|i| i.modifiers.command) {
                                out.check = Some((i, shift));
                            } else {
                                out.select = Some(i);
                            }
                        }
                        // The menu knows its cell only from where the right click landed, so that is kept for it.
                        let cell_id = ui.scope_id().with("menu-cell");
                        if resp.secondary_clicked() && let Some(p) = resp.interact_pointer_pos() {
                            let mut x = rr.left() + lead;
                            let hit = order.iter().find(|&&ci| {
                                x += widths[ci];
                                p.x < x
                            });
                            ui.ctx().data_mut(|d| d.insert_temp(cell_id, hit.copied().unwrap_or(usize::MAX)));
                        }
                        resp.context_menu(|ui| {
                            menu_look(ui);
                            let at: usize = ui.ctx().data(|d| d.get_temp(cell_id)).unwrap_or(usize::MAX);
                            if let Some(c) = set.cols.get(at) {
                                if ui.button("Copy value").clicked() {
                                    out.cell = Some((i, c.name.clone(), CellAct::CopyValue));
                                    ui.close();
                                }
                                if ui.button("View value…").clicked() {
                                    out.cell = Some((i, c.name.clone(), CellAct::View));
                                    ui.close();
                                }
                                // Only a result read from one table has a table to filter.
                                if filter && ui.button("Filter to this value").on_hover_text("Open the table with this value as its filter").clicked() {
                                    out.cell = Some((i, c.name.clone(), CellAct::FilterValue));
                                    ui.close();
                                }
                                ui.separator();
                            }
                            if let Some(a) = results_menu(ui, if checked.contains(&i) { checked.len() } else { 1 }, true, true) {
                                out.act = Some((i, a));
                                ui.close();
                            }
                        });
                    }
                    if sel == Some(i) && let Some(rec) = record.as_mut() {
                        table::record_card(ui, row_w, view_w, |ui| rec(ui));
                    }
                }
            })
        });
        // Painted after the area so it follows the sideways scroll of that same frame.
        let off = area.inner.state.clamped_offset().x;
        let p = ui.painter().with_clip_rect(head);
        p.rect_filled(head, egui::CornerRadius { nw: R_MD - 1, ne: R_MD - 1, sw: 0, se: 0 }, HDR());
        let all = !set.rows.is_empty() && checked.len() == set.rows.len();
        if check_box(ui, egui::pos2(head.left() + 13.0 - off, head.center().y), ui.scope_id().with("check-all"), all, !all && !checked.is_empty()).on_hover_text("Select every row on this page").clicked() {
            out.all = Some(!all);
        }
        p.text(egui::pos2(head.left() + CHECK_W + 18.0 - off, head.center().y), egui::Align2::LEFT_CENTER, "#", FontId::monospace(TXT_CAPTION), FAINT());
        p.hline(head.x_range(), head.bottom() - 0.5, Stroke::new(1.0, LINE()));
        // Headers drag sideways to reorder the columns, or Alt+Left and Alt+Right on the focused one. A click opens the
        // column menu and Shift-click adds a sort key, as in Content. Sorting only reorders this page of rows.
        let ow: Vec<f32> = order.iter().map(|&ci| widths[ci]).collect();
        let keys: Vec<egui::Id> = order.iter().map(|&ci| egui::Id::unique(("sql-query-col", ci))).collect();
        let blocks = [pins];
        let strip = egui::Rect::from_min_size(egui::pos2(head.left() + lead - off, head.top()), egui::vec2(ow.iter().sum(), TABLE_HEAD_H));
        let room = egui::Rect::from_min_max(egui::pos2(head.left() + lead, head.top()), head.max);
        let mv = put_at(ui, strip, |ui| {
            ui.set_clip_rect(room.intersect(ui.clip_rect()));
            dnd::reorder(ui, egui::Id::unique(("sql-query-head", &names)), dnd::Items::horizontal(&ow, TABLE_HEAD_H).keys(&keys).blocks(&blocks), |ui, k, st| {
                let ci = order[k];
                let c = &set.cols[ci];
                let cell = ui.max_rect();
                let r = ui.interact(cell, ui.scope_id().with("head"), Sense::click());
                r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ColumnHeader, true, format!("Result column {}", c.name)));
                let state = if st.busy { 0 } else { state_of(&r) };
                if state > 0 {
                    ui.painter().rect_filled(cell, 0, hover_fill(state));
                }
                let dir = set.sort.iter().find(|(n, _)| *n == c.name).map(|(_, desc)| *desc);
                let pinned = prefs.pinned.contains(&names[ci]);
                let color = if dir.is_some() { PRIMARY() } else if pinned { WARNING() } else if st.lifted || state > 0 { BRIGHT() } else { MUTED() };
                let label = match dir {
                    Some(false) => format!("{} ↑", c.name),
                    Some(true) => format!("{} ↓", c.name),
                    None => c.name.clone(),
                };
                let g = ui.painter().layout_no_wrap(label, FontId::proportional(TXT_SMALL), color);
                let gx = table::align_x(aligns[ci], cell.left(), widths[ci], g.size().x);
                ui.painter().with_clip_rect(cell.shrink2(egui::vec2(4.0, 0.0)).intersect(ui.clip_rect())).galley(egui::pos2(gx, cell.center().y - g.size().y / 2.0 + optical_dy(&g)), g, color);
                if st.lifted || st.busy {
                    return r;
                }
                let r = r.on_hover_text("Click for column options. Shift-click adds a sort key. Sorting only orders the rows on this page.");
                let shift_sort = r.clicked() && ui.input(|i| i.modifiers.shift);
                if shift_sort {
                    out.head.push((names[ci].clone(), HeadAct::AddSort));
                }
                let open = if r.secondary_clicked() { Some(egui::SetOpenCommand::Bool(true)) } else if r.clicked() && !shift_sort { Some(egui::SetOpenCommand::Toggle) } else { None };
                let mut acts = Vec::new();
                // Clicks inside, such as the Data submenu, keep it open; each action closes it itself.
                egui::Popup::menu(&r).open_memory(open).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).width(190.0).show(|ui| query_col_popover(ui, &names[ci], dir, prefs, &mut acts));
                out.head.extend(acts.into_iter().map(|a| (names[ci].clone(), a)));
                r
            })
        });
        if let Some(m) = mv {
            out.head.push((shown[m.from].clone(), HeadAct::Col(ColAct::MoveTo(shown[m.from].clone(), m.before()))));
        }
    });
    out
}

/// Previous and next buttons for a result whose length is not known yet.
fn open_pager(ui: &mut egui::Ui, page: usize, next: bool) -> Option<usize> {
    let mut go = None;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        quiet_look(ui);
        let nav = |t: &str| quiet_button(RichText::new(t).font(FontId::monospace(TXT_BODY))).min_size(egui::vec2(22.0, BUTTON_H));
        if ui.add_enabled(page > 0, nav("«")).on_hover_text("First page").clicked() {
            go = Some(0);
        }
        if ui.add_enabled(page > 0, nav("‹")).on_hover_text("Previous page").clicked() {
            go = Some(page - 1);
        }
        ui.label(RichText::new(format!("Page {}", page + 1)).font(FontId::monospace(TXT_BODY)).color(MUTED()));
        if ui.add_enabled(next, nav("›")).on_hover_text("Next page").clicked() {
            go = Some(page + 1);
        }
    });
    go
}

impl App {
    /// The name a tab shows: its table, or "Query N" for a Query tab that belongs to no table.
    pub(in crate::app) fn tab_name(&self, i: usize) -> String {
        let Some(t) = self.tabs.get(i) else { return String::new() };
        // A tab opened from the library, or saved to it, is named for its script or file, so Cmd+S writing it back silently
        // is never a surprise.
        if let Some(b) = &t.script {
            let name = self.script_name(&b.r);
            if !name.is_empty() {
                return name;
            }
        }
        // A Shell script opened from the library has no collection: the tab is named for its database.
        if t.sel.coll.is_empty() && t.view == View::Shell {
            return format!("Shell · {}", t.sel.db);
        }
        if t.sql.is_some() && t.sel.coll.is_empty() {
            return format!("Query {}", self.tabs[..=i].iter().filter(|x| x.sql.is_some() && x.sel.coll.is_empty()).count());
        }
        t.sel.coll.clone()
    }

    /// Opens a Query tab for a database. It has no table, so its collection name stays empty.
    pub(in crate::app) fn sql_new_query(&mut self, conn: u64, db: String) {
        let engine = self.conn(conn).map(|c| c.meta.engine).unwrap_or_default();
        // A PostgreSQL Query tab starts in the schema the sidebar shows for its database.
        let mut t = Tab::new(self.sel_in(conn, &db, String::new())).for_engine(engine);
        // A database never expanded has no schema list yet; the tab fills its schema when the list arrives.
        if self.is_pg(conn) && !self.sql_asked.contains(&(conn, db.clone())) {
            self.pg_ask_schemas(conn, &db);
        }
        t.view = View::Sql;
        self.tabs.push(t);
        self.set_cur(self.tabs.len() - 1);
        self.reset_doc_state();
    }

    /// Runs the statement under the caret, or the whole script. `explain` puts EXPLAIN in front of the statement.
    pub(in crate::app) fn sql_run(&mut self, all: bool, explain: bool) {
        let dialect = self.sql_dialect();
        let Some(s) = self.t().and_then(|t| t.sql.as_ref()) else { return };
        let text = if all { s.query.clone() } else { s.caret_info(dialect).stmt.map(|(r, _, _)| s.query[r].to_string()).unwrap_or_default() };
        self.sql_run_text(text, explain);
    }

    /// The statements of the script, one line each, for the Run button's tip. Returns the one picked.
    fn sql_statement_menu(&self, ui: &mut egui::Ui) -> Option<String> {
        let s = self.t()?.sql.as_ref()?;
        let d = self.sql_dialect();
        let here = s.caret_info(d).stmt.map(|(r, _, _)| r);
        let mut pick = None;
        egui::ScrollArea::vertical().id_salt("sql-run-pick").max_height(360.0).show(ui, |ui| {
            for (i, (r, text)) in lex::split_statements(&s.query, d).into_iter().enumerate() {
                // The number keeps two statements that read the same apart; the one under the caret is in the accent.
                let mut job = egui::text::LayoutJob::single_section(format!("{:>2}  ", i + 1), mono_fmt(TXT_SMALL, if here.as_ref() == Some(&r) { PRIMARY() } else { FAINT() }, 400.0));
                let sql = sql_job_in(&preview(text, d), TXT_SMALL, d);
                for sec in sql.sections {
                    job.append(&sql.text[sec.byte_range.start.0..sec.byte_range.end.0], 0.0, sec.format);
                }
                job.wrap = egui::text::TextWrapping::truncate_at_width(396.0);
                let r = ui.add(quiet_button(job)).on_hover_text(text);
                r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Run statement {}", i + 1)));
                if r.clicked() {
                    pick = Some(text.to_string());
                    ui.close();
                }
            }
        });
        pick
    }

    /// Runs `text` in the current Query tab, as picked from the Run button's tip or found by `sql_run`.
    fn sql_run_text(&mut self, text: String, explain: bool) {
        let Some(t) = self.t() else { return };
        let (conn, schema) = (t.sel.conn, run_db(t));
        if t.sql.as_ref().is_none_or(|s| s.run_req != 0) {
            return;
        }
        if text.trim().is_empty() {
            return self.warn("Nothing to run.");
        }
        let Some(db) = self.sql_db_for(conn, Some("Connect first.")) else { return };
        let (db, ns) = run_target(&self.tabs[self.cur], db);
        let read_only = !self.conn_writable(conn);
        let key = format!("{conn}/{schema}/#query");
        // History is written to ui.json and listed in the menu, so a password never goes in.
        push_history(&mut self.prefs.history.entry(key).or_default().selector, &db::redact_sql(&text).0);
        self.save_prefs();
        let text = if explain { format!("EXPLAIN {text}") } else { text };
        self.req += 1;
        let req = self.req;
        let Some(s) = self.tabs[self.cur].sql.as_mut() else { return };
        // Fresh handles per run, so a Stop pressed for an old run never reaches this one.
        (s.run_req, s.run, s.ran_in) = (req, Default::default(), Some((s.run_db.clone(), s.run_schema.clone())));
        self.worker.sql_run(req, db, ns, text, read_only, 0, s.run.clone());
    }

    /// Stops the running script: the statement in flight is interrupted and the rest never starts.
    fn sql_stop(&mut self) {
        let Some(t) = self.t() else { return };
        let Some(s) = t.sql.as_ref().filter(|s| s.run_req != 0) else { return };
        if let Some(db) = self.conn(t.sel.conn).and_then(|c| c.sql.clone()) {
            self.worker.sql_kill(db, s.run.conn.load(Ordering::SeqCst), s.run.cancel.clone());
        }
    }

    /// Applies a finished run to its tab; a run the tab no longer waits for is dropped. A page read replaces
    /// only its result tab and keeps the row count; a new run starts the counts of its paged results.
    pub(in crate::app) fn sql_ran(&mut self, req: u64, mut sets: Vec<ResultSet>) {
        let Some(ti) = self.tabs.iter().position(|t| t.sql.as_ref().is_some_and(|s| s.run_req == req)) else { return };
        let conn = self.tabs[ti].sel.conn;
        let Some(s) = self.tabs[ti].sql.as_mut() else { return };
        // Checks belong to the page they were made on.
        (s.run_req, s.text_cache, s.qanchor, s.qsel) = (0, None, None, None);
        s.qchecked.clear();
        if let Some(idx) = s.paging.take() {
            let failed = match (s.sets.get_mut(idx), sets.pop()) {
                (Some(old), Some(mut new)) if new.error.is_none() => {
                    if let (Some(o), Some(n)) = (old.page, new.page.as_mut()) {
                        n.total = o.total;
                    }
                    *old = new;
                    None
                }
                (_, Some(new)) => new.error,
                _ => None,
            };
            if let Some(e) = failed {
                self.err(e);
            }
            return;
        }
        s.sets_req = req;
        (s.set_sel, s.sets) = (0, sets);
        // A last page already knows the total; the others ask the server.
        let wants: Vec<(usize, String)> = s.sets.iter().enumerate().filter(|(_, x)| x.page.is_some_and(|p| p.next)).map(|(i, x)| (i, x.stmt.clone())).collect();
        if let Some((db, ns)) = self.conn(conn).and_then(|c| c.sql.clone()).map(|db| ran_target(&self.tabs[ti], db)) {
            for (i, stmt) in wants {
                self.worker.sql_count(req, i, db.clone(), ns.clone(), stmt);
            }
        }
    }

    /// The row count of a paged result arrived.
    pub(in crate::app) fn sql_counted(&mut self, req: u64, idx: usize, res: db::Res<u64>) {
        let p = self.tabs.iter_mut().find_map(|t| t.sql.as_mut().filter(|s| s.sets_req == req)).and_then(|s| s.sets.get_mut(idx)).and_then(|x| x.page.as_mut());
        if let (Some(p), Ok(n)) = (p, res) {
            p.total = Some(n);
        }
    }

    /// Sorts the shown result by `keys` (column and whether descending) on the client, which only orders the rows
    /// already fetched. No keys restores the server's order. Checks and the selected row follow their rows.
    pub(in crate::app) fn sql_sort_page(&mut self, keys: Vec<(String, bool)>) {
        let Some(s) = self.tm().and_then(|t| t.sql.as_mut()) else { return };
        let i = s.set_sel.min(s.sets.len().saturating_sub(1));
        let Some(set) = s.sets.get_mut(i) else { return };
        let moved = db::sql::sort::sort_rows(set, &keys);
        s.qchecked = s.qchecked.iter().filter_map(|&r| moved.get(r).copied()).collect();
        s.qanchor = s.qanchor.and_then(|r| moved.get(r).copied());
        s.qsel = s.qsel.and_then(|r| moved.get(r).copied());
        s.text_cache = None;
    }

    /// A cell menu pick in the shown result.
    pub(in crate::app) fn sql_query_cell_act(&mut self, ctx: &egui::Context, row: usize, col: &str, act: CellAct) {
        let Some(s) = self.t().and_then(|t| t.sql.as_ref()) else { return };
        let Some(set) = s.sets.get(s.set_sel.min(s.sets.len().saturating_sub(1))) else { return };
        let v = Rows::Raw(&set.rows).key(row, col).unwrap_or(Bson::Null);
        match act {
            CellAct::CopyValue => {
                ctx.copy_text(super::view::plain_text(&v));
                self.ok("Copied the value");
            }
            CellAct::View => self.sql_view_value(col, &v),
            CellAct::FilterValue => self.sql_query_filter(col, v),
            // Results are never edited.
            CellAct::SetNull => {}
        }
    }

    /// Opens the table a one-table result was read from, filtered to `v` in column `col`.
    fn sql_query_filter(&mut self, col: &str, v: Bson) {
        let Some(t) = self.t() else { return };
        let Some(s) = t.sql.as_ref() else { return };
        let Some(set) = s.sets.get(s.set_sel.min(s.sets.len().saturating_sub(1))) else { return };
        let d = self.sql_dialect();
        let Some((ns, table)) = db::sql::source::single_table(&set.stmt, d) else { return self.warn("This result did not come from a single table.") };
        // The namespace the run used, which the person may have changed since.
        let (rd, rs) = s.ran_in.clone().unwrap_or_else(|| (s.run_db.clone(), s.run_schema.clone()));
        let db_name = if rd.is_empty() { t.sel.db.clone() } else { rd.clone() };
        let ns = ns.unwrap_or_else(|| {
            if d != db::sql::Dialect::Postgres {
                return db_name.clone();
            }
            let schema = run_schema_choice(&rs, &rd, &t.sel.db, &t.sel.schema);
            if schema.is_empty() { PG_PUBLIC.to_string() } else { schema }
        });
        let found = set.cols.iter().find(|c| c.name == col);
        let numeric = found.is_some_and(|c| is_numeric(c.kind));
        // The server's own column name, which a statement must use.
        let q = d.quote_ident(found.map_or(col, |c| c.src.as_str()));
        let text = if v == Bson::Null { format!("{q} IS NULL") } else { format!("{q} = {}", db::copy::sql_literal(Some(&v), numeric, self.sql_bs())) };
        // The editor belongs to this tab's rows; it would stage into the wrong table once the tab changes.
        if self.sql_cell.is_some() {
            return self.warn("Finish or cancel the cell edit first.");
        }
        let base = Sel { conn: t.sel.conn, db: db_name, schema: String::new(), coll: String::new() };
        let sel = super::ns::with_ns(d, &base, &ns, table);
        self.open_tab(sel, Open::Link);
        let Some(t) = self.tm() else { return };
        t.view = View::Documents;
        t.filter_txt = text;
        t.page = 0;
        self.reload();
    }

    /// Reads result tab `idx` again at page `n` on a fresh connection, a run of its own that Stop reaches.
    fn sql_goto_page(&mut self, idx: usize, n: usize) {
        let Some(t) = self.t() else { return };
        let conn = t.sel.conn;
        let Some(stmt) = t.sql.as_ref().filter(|s| s.run_req == 0).and_then(|s| s.sets.get(idx)).map(|x| x.stmt.clone()) else { return };
        let Some(db) = self.sql_db_for(conn, Some("Connect first.")) else { return };
        let (db, schema) = ran_target(&self.tabs[self.cur], db);
        let read_only = !self.conn_writable(conn);
        self.req += 1;
        let req = self.req;
        let Some(s) = self.tabs[self.cur].sql.as_mut() else { return };
        (s.run_req, s.paging, s.run) = (req, Some(idx), Default::default());
        self.worker.sql_run(req, db, schema, stmt, read_only, n, s.run.clone());
    }

    pub(in crate::app) fn sql_query_view(&mut self, ui: &mut egui::Ui) {
        let Some(t) = self.t() else { return };
        let (conn, free, schema) = (t.sel.conn, t.sel.coll.is_empty(), run_db(t));
        let running = t.sql.as_ref().is_some_and(|s| s.run_req != 0);
        let title = if free { self.tab_name(self.cur) } else { "Query".into() };
        let lib_pick = self.pick_data(ui.ctx(), PickKind::Sql);
        let mut pick_acts = Vec::new();
        let (mut pick, mut load, mut picked) = (schema.clone(), None, false);
        let dialect = self.sql_dialect();
        let mut pg_schema = t_schema(self.t());
        let tab_schema = self.t().map(|t| t.sel.schema.clone()).unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label(semibold(RichText::new(&title).size(TXT_HEADING).color(BRIGHT())));
            ui.add_space(6.0);
            ui.scope(|ui| {
                quiet_look(ui);
                egui::ComboBox::from_id_salt("sql-db").selected_text(RichText::new(&schema).font(FontId::monospace(TXT_SMALL))).show_ui(ui, |ui| {
                    menu_look(ui);
                    for (d, _) in self.conn(conn).map(|c| c.dbs.as_slice()).unwrap_or_default() {
                        picked |= ui.selectable_value(&mut pick, d.clone(), d).clicked();
                    }
                });
            });
            if dialect == db::sql::Dialect::Postgres {
                ui.add_space(4.0);
                ui.add(egui::TextEdit::singleline(&mut pg_schema).font(FontId::monospace(TXT_SMALL)).hint_text(RichText::new(&tab_schema).color(FAINT())).desired_width(100.0)).on_hover_text("The schema put first in search_path for this tab. Empty uses the schema this tab was opened in.");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // The first one added sits at the right edge: the history, then the library beside it.
                icon_menu(ui, paint_clock, true, |ui| {
                    ui.set_min_width(360.0);
                    ui.set_max_width(520.0);
                    let hist = self.prefs.history.get(&format!("{conn}/{schema}/#query")).map(|h| h.selector.as_slice()).unwrap_or_default();
                    if hist.is_empty() {
                        ui.label(RichText::new("Statements you run appear here.").color(MUTED()));
                    }
                    egui::ScrollArea::vertical().id_salt("sql-hist").max_height(300.0).show(ui, |ui| {
                        for h in hist {
                            let mut job = sql_job_in(&preview(h, dialect), TXT_SMALL, dialect);
                            job.wrap = egui::text::TextWrapping::truncate_at_width(496.0);
                            if ui.add(quiet_button(job)).on_hover_text(h.as_str()).clicked() {
                                load = Some(h.clone());
                                ui.close();
                            }
                        }
                    });
                })
                .on_hover_text("History");
                ui.add_space(6.0);
                let button = picker_button(ui, &lib_pick, false, &mut pick_acts);
                self.demo_open_library(ui.ctx(), &button);
                self.demo_open_picker(ui.ctx(), &button, "picker");
            });
        });
        if let Some(s) = self.tm().and_then(|t| t.sql.as_mut()) {
            // Kept as typed: trimming here would eat a space the user is still typing. Every reader trims.
            s.run_schema = pg_schema;
        }
        ui.add_space(4.0);
        // A table's Query view gets these from the view tabs above it; a Query tab of its own has none.
        if free {
            self.script_banners(ui);
        }
        let (mut run, mut run_all, mut stop, mut explain, mut format, mut run_pick, mut save_view) = (false, false, false, false, false, None, false);
        let caret = self.t().and_then(|t| t.sql.as_ref()).map(|s| s.caret_info(self.sql_dialect()));
        let many = caret.as_ref().and_then(|c| c.stmt.as_ref()).is_some_and(|(_, _, n)| *n > 1);
        let status = caret.map(|CaretInfo { line_col: (l, c), stmt, .. }| {
            let n = stmt.map_or(String::new(), |(_, i, n)| format!("  ·  statement {i} of {n}"));
            format!("Ln {l}, Col {c}{n}")
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.add_enabled_ui(!running, |ui| {
                let (main, tip) = run_split(ui, "Run statement", "⌘↵", many);
                run = main.on_hover_text("Run the statement under the caret (Cmd+Enter)").clicked();
                let tip = if many { tip.on_hover_text("Choose the statement to run") } else { tip };
                egui::Popup::menu(&tip).width(420.0).show(|ui| {
                    menu_look(ui);
                    run_pick = self.sql_statement_menu(ui);
                });
                run_all = ui.add(egui::Button::new("Run all").right_text(RichText::new("⇧⌘↵").size(TXT_CAPTION).color(FAINT()))).on_hover_text("Run every statement (Cmd+Shift+Enter)").clicked();
            });
            stop = quiet_enabled(ui, running, "Stop").on_hover_text("Interrupt the running statement and skip the rest").clicked();
            let (r, _) = ui.allocate_exact_size(egui::vec2(1.0, 16.0), Sense::hover());
            ui.painter().vline(r.center().x, r.y_range(), Stroke::new(1.0, LINE()));
            explain = quiet_enabled(ui, !running, "Explain").on_hover_text("EXPLAIN the statement under the caret (Cmd+E)").clicked();
            format = quiet_enabled(ui, !running, "Format").on_hover_text("Lay out the selection, or the whole script when nothing is selected. A statement that would read differently stays as it is.").clicked();
            save_view = quiet_enabled(ui, !running, "Save as view…").on_hover_text("Create a view from the statement under the caret").clicked();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(s) = &status {
                    ui.label(RichText::new(s).font(FontId::monospace(TXT_CAPTION)).color(FAINT()));
                }
            });
        });
        ui.add_space(8.0);
        if picked {
            // Another PostgreSQL database has its own schemas, so the tab moves to the one its sidebar shows.
            let schema = self.ns_of(conn, &pick).to_string();
            if self.is_pg(conn) && !self.sql_asked.contains(&(conn, pick.clone())) {
                self.pg_ask_schemas(conn, &pick);
            }
            if let Some(t) = self.tm() {
                match (free, t.sql.as_mut()) {
                    (true, _) => (t.sel.db, t.sel.schema) = (pick, schema),
                    (false, Some(s)) => (s.run_db, s.run_schema) = (pick, schema),
                    _ => {}
                }
            }
        }
        let ctx = ui.ctx().clone();
        if let Some(text) = load {
            self.sql_replace(&ctx, text, None);
        }
        if format {
            self.sql_format(&ctx);
        }
        self.sql_editor(ui);
        ui.add_space(4.0);
        self.sql_result_pane(ui, running);
        if stop {
            self.sql_stop();
        }
        if let Some(text) = run_pick {
            self.sql_run_text(text, false);
        }
        if run || run_all || explain {
            self.sql_run(run_all, explain);
        }
        if save_view {
            self.save_query_as_view();
        }
        // Last, since opening a script may change the tab this frame was drawn for.
        self.pick_acts(pick_acts);
    }

    pub(in crate::app) fn sql_editor_id(cur: usize) -> egui::Id {
        egui::Id::unique(("sql-editor", cur))
    }

    /// Puts `new` in the editor so that Cmd+Z brings the old text back. egui's editor keeps its undo history as whole
    /// states, so the text before the change is added as a point first; changing the string alone would only become
    /// one after the text had rested for a second. `select` is the range of the new text to leave selected.
    pub(in crate::app) fn sql_replace(&mut self, ctx: &egui::Context, new: String, select: Option<Range<usize>>) {
        let id = Self::sql_editor_id(self.cur);
        let Some(s) = self.tabs.get_mut(self.cur).and_then(|t| t.sql.as_mut()) else { return };
        if s.query == new {
            return;
        }
        let mut st = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        let before = st.cursor.char_range().unwrap_or_else(|| CCursorRange::one(CCursor::new(s.query.chars().count())));
        let mut undoer = st.undoer();
        undoer.add_undo(&(before, std::mem::replace(&mut s.query, new)));
        st.set_undoer(undoer);
        let end = s.query.chars().count();
        let range = match select {
            Some(r) => CCursorRange::two(CCursor::new(r.start.min(end)), CCursor::new(r.end.min(end))),
            None => CCursorRange::one(CCursor::new(usize::from(before.primary.index).min(end))),
        };
        st.cursor.set_char_range(Some(range));
        s.caret = usize::from(range.primary.index);
        st.store(ctx, id);
        ctx.request_repaint();
    }

    /// Format: the selection, or the whole script when nothing is selected.
    fn sql_format(&mut self, ctx: &egui::Context) {
        let Some(s) = self.tabs.get(self.cur).and_then(|t| t.sql.as_ref()) else { return };
        let text = s.query.clone();
        let picked = egui::TextEdit::load_state(ctx, Self::sql_editor_id(self.cur)).and_then(|st| st.cursor.char_range()).filter(|r| r.primary != r.secondary);
        let (new, select, f) = match picked {
            None => {
                let f = db::sql::format::format_script(&text, self.sql_dialect());
                (f.text.clone(), None, f)
            }
            Some(r) => {
                let (p, q) = (usize::from(r.primary.index), usize::from(r.secondary.index));
                let (a, b) = (p.min(q), p.max(q));
                let (a, b) = (byte_at(&text, a), byte_at(&text, b));
                // A cut through a string or comment would be formatted as something it is not.
                if lex::tokens(&text, self.sql_dialect()).iter().any(|(t, tr)| matches!(t, lex::Tok::Str | lex::Tok::Quoted | lex::Tok::Comment) && ((tr.start < a && a < tr.end) || (tr.start < b && b < tr.end))) {
                    return self.warn("The selection starts or ends inside a string or comment. Select whole statements, or nothing to format the script.");
                }
                let f = db::sql::format::format_script(&text[a..b], self.sql_dialect());
                let new = format!("{}{}{}", &text[..a], f.text, &text[b..]);
                let from = text[..a].chars().count();
                (new, Some(from..from + f.text.chars().count()), f)
            }
        };
        let note = if new == text && f.special == 0 && f.refused == 0 { "Already formatted".to_string() } else { f.summary() };
        self.sql_replace(ctx, new, select);
        if f.refused > 0 { self.warn(note) } else { self.ok(note) }
    }

    /// The script editor with line numbers, token colors and a faint band behind the statement under the caret.
    fn sql_editor(&mut self, ui: &mut egui::Ui) {
        let id = Self::sql_editor_id(self.cur);
        let focused = ui.memory(|m| m.has_focus(id));
        let avail = ui.available_height();
        let Some((conn, db, schema)) = self.tabs.get(self.cur).map(|t| (t.sel.conn, run_db(t), if self.is_pg(t.sel.conn) { Some(run_schema_of(t)).filter(|s| !s.is_empty()).unwrap_or_else(|| self.sel_schema(&t.sel)) } else { String::new() })) else { return };
        let scope = Self::sql_scope(&self.conns, &self.sql_schema, &self.views, conn, &db, &schema, None);
        let dialect = scope.dialect;
        let none = Fields::new();
        let values = self.sql_values.get(&(conn, ns_key(&db, &schema))).unwrap_or(&none);
        let Some(s) = self.tabs.get_mut(self.cur).and_then(|t| t.sql.as_mut()) else { return };
        let lines = s.query.split('\n').count();
        // The editor keeps three lines, and the results below it their tabs, header and three rows.
        let (lo, hi) = (3.0 * LINE_H + 24.0, avail - 3.0 * ROW_H - TABLE_HEAD_H - 110.0);
        let custom = self.prefs.query_editor_h > 0.0;
        let h = if custom { self.prefs.query_editor_h.clamp(lo, hi.max(lo)) } else { (lines as f32 * LINE_H + 24.0).clamp(96.0, (avail * 0.5).max(96.0)) };
        let mut layout = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
            crate::ui::editor::memo_galley(ui, 1, buf.as_str(), wrap, || {
                let mut job = crate::ui::editor::sql_job_in(buf.as_str(), TXT_SMALL, dialect);
                for sec in &mut job.sections {
                    sec.format.line_height = Some(LINE_H);
                }
                job
            })
        };
        let framed = egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, if focused { PRIMARY() } else { BORDER() })).corner_radius(R_MD).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("sql-ed").max_height(h).auto_shrink([false, true]).show(ui, |ui| {
                // Reserved first so the band is drawn under the text.
                let band = ui.painter().add(egui::Shape::Noop);
                if s.bar.preset.ac {
                    open_list(ui, id, &s.query, &mut s.bar.ac, &scope, values);
                }
                // The suggestions work on the statement under the caret, with the tail after it for aliases.
                let (out, _) = crate::ui::vim::wrap_with(ui, id, &mut s.query, |ui, query| {
                    sql_ac_field(ui, id, query, &mut s.bar.ac, &scope, values, &mut s.bar.wants, |ui, text, _| {
                    egui::TextEdit::multiline(text)
                        .id(id)
                        .font(FontId::monospace(TXT_SMALL))
                        .frame(gutter_frame())
                        .desired_width(f32::INFINITY)
                        .min_size(egui::vec2(0.0, if custom { h } else { 96.0 }))
                        .lock_focus(true)
                        .hint_text(RichText::new("SELECT * FROM orders;").font(FontId::monospace(TXT_SMALL)).color(FAINT()).line_height(Some(LINE_H)))
                        .layouter(&mut layout)
                        .show(ui)
                    })
                });
                if let Some(r) = out.cursor_range {
                    s.caret = usize::from(r.primary.index);
                }
                let (g, gp, rect) = (&out.galley, out.galley_pos, out.response.rect);
                let info = s.caret_info(dialect);
                if let Some((r, _, _)) = info.stmt.filter(|p| p.2 > 1) {
                    let at = |b: usize| s.query[..b].chars().count();
                    let (a, z) = (g.pos_from_cursor(CCursor::new(at(r.start))), g.pos_from_cursor(CCursor::new(at(r.end))));
                    let band_rect = egui::Rect::from_min_max(egui::pos2(rect.left(), gp.y + a.top()), egui::pos2(rect.right(), gp.y + z.bottom()));
                    ui.painter().set(band, egui::Shape::rect_filled(band_rect, 0.0, tint(PRIMARY(), 22)));
                }
                paint_gutter(ui, &out, Some(info.line_col.0));
            });
        });
        let wants = self.tabs.get_mut(self.cur).and_then(|t| t.sql.as_mut()).map(|s| std::mem::take(&mut s.bar.wants)).unwrap_or_default();
        self.sql_serve(conn, &db, &schema, wants);
        let bar = splitter(ui);
        if resize_editor(&bar, &mut self.prefs.query_editor_h, framed.response.rect.height(), lo, hi) {
            self.save_prefs();
        }
    }

    /// The result tabs, the Table or Text switch and the selected statement's outcome.
    fn sql_result_pane(&mut self, ui: &mut egui::Ui, running: bool) {
        let cur = self.cur;
        let hidden = self.modal.is_some() || self.insert.is_some();
        let dialect = self.sql_dialect();
        let record_open = self.tabs.get(cur).is_some_and(|t| t.record);
        let Some(s) = self.tabs.get_mut(cur).and_then(|t| t.sql.as_mut()) else { return };
        if s.sets.is_empty() {
            ui.horizontal(|ui| {
                if running {
                    spin(ui);
                }
                ui.label(RichText::new(if running { "Running…" } else { "Run a statement to see its result." }).color(MUTED()));
            });
            return;
        }
        let (mut pick, mut text_mode) = (None, s.text_mode);
        // What the frame asked for, done after `s` is let go: rows to copy (None is the whole page), rows to
        // export with whether the dialog starts on every page, copying every page, and a page to read.
        let mut view = None;
        // Column menu and cell menu picks, an unsort and a show-all-columns, done once `s` is let go.
        let (mut head_acts, mut cell_act, mut unsort, mut show_cols): (Vec<(String, HeadAct)>, Option<(usize, String, CellAct)>, bool, Option<Vec<String>>) = (Vec::new(), None, false, None);
        let (mut copy, mut export, mut copy_all, mut goto): (Option<(Fmt, Option<Vec<usize>>)>, Option<(Vec<usize>, bool)>, Option<Fmt>, Option<(usize, usize)>) = (None, None, None, None);
        let sel = s.set_sel.min(s.sets.len() - 1);
        let paged = s.sets[sel].page.filter(|p| p.next || p.n > 0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let w = (ui.available_width() - 260.0).max(120.0);
            ui.allocate_ui(egui::vec2(w, BUTTON_H + 8.0), |ui| {
                egui::ScrollArea::horizontal().id_salt("sql-sets").show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for (i, set) in s.sets.iter().enumerate() {
                            if set_tab(ui, i, set, i == sel).clicked() {
                                pick = Some(i);
                            }
                        }
                        if running {
                            spin(ui);
                        }
                    });
                });
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                dots_menu(ui, |ui| {
                    menu_look(ui);
                    if let Some(DocsAct::CopyAs(f)) = ui.menu_button("Copy as", |ui| copy_as_menu(ui, true)).inner.flatten() {
                        copy = Some((f, None));
                        ui.close();
                    }
                    if paged.is_some() && let Some(DocsAct::CopyAs(f)) = ui.menu_button("Copy all pages as", |ui| copy_as_menu(ui, true)).inner.flatten() {
                        copy_all = Some(f);
                        ui.close();
                    }
                    if ui.button("Export…").clicked() {
                        export = Some((s.qchecked.iter().copied().collect(), false));
                        ui.close();
                    }
                });
                if quiet(ui, "Export…").on_hover_text("Write this result to a file in any Copy as format").clicked() {
                    export = Some((s.qchecked.iter().copied().collect(), false));
                }
                segmented(ui, &mut text_mode, &[(false, "Table"), (true, "Text")]);
            });
        });
        ui.add_space(6.0);
        s.text_mode = text_mode;
        if let Some(i) = pick {
            (s.set_sel, s.text_cache) = (i, None);
            s.qchecked.clear();
            (s.qanchor, s.qsel) = (None, None);
        }
        let (error, stmt, affected, ms, capped, page) = {
            let x = &s.sets[sel];
            (x.error.clone(), x.stmt.clone(), x.affected, x.ms, x.capped, x.page)
        };
        // The page bar: how many rows there are, the pager and a note that unordered rows can move.
        if let (Some(p), None) = (page, &error) {
            let known = p.total.or_else(|| (!p.next).then(|| (p.n * db::sql::RUN_ROWS_MAX + s.sets[sel].rows.len()) as u64));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let count = match known {
                    Some(n) => format!("{} {}", group(&n.to_string()), if n == 1 { "row" } else { "rows" }),
                    None => format!("{}+ rows", group(&((p.n + 1) * db::sql::RUN_ROWS_MAX).to_string())),
                };
                ui.label(semibold(RichText::new(count).font(FontId::monospace(TXT_SMALL)).color(BRIGHT())));
                if !p.ordered && (p.next || p.n > 0) {
                    ui.label(RichText::new("No ORDER BY: rows may move between pages").size(TXT_CAPTION).color(FAINT()));
                }
                if p.next || p.n > 0 {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // The pager reads left to right, so it gets its own layout at last frame's width, as in Content.
                        let id = ui.scope_id().with("pager-w");
                        let w = ui.data(|d| d.get_temp::<f32>(id)).unwrap_or(150.0);
                        let r = ui.allocate_ui_with_layout(egui::vec2(w, BUTTON_H), egui::Layout::left_to_right(egui::Align::Center), |ui| match known {
                            Some(n) => pager(ui, p.n, (n as usize).div_ceil(db::sql::RUN_ROWS_MAX).max(1)),
                            None => open_pager(ui, p.n, p.next),
                        });
                        if r.response.rect.width() != w {
                            ui.data_mut(|d| d.insert_temp(id, r.response.rect.width()));
                        }
                        goto = r.inner.filter(|n| *n != p.n && !running).map(|n| (sel, n));
                    });
                }
            });
            ui.add_space(4.0);
        }
        if let Some(e) = &error {
            egui::Frame::new().fill(tint(ALERT(), 22)).stroke(Stroke::new(1.0, tint(ALERT(), 90))).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(&stmt).font(FontId::monospace(TXT_SMALL)).color(MUTED()));
                ui.label(RichText::new(e).color(ALERT()));
            });
        } else if let Some(n) = affected {
            ui.label(RichText::new(format!("{} {} affected in {} ms.", group(&n.to_string()), if n == 1 { "row" } else { "rows" }, ms)).color(MUTED()));
        } else {
            if capped {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.label(RichText::new(format!("Showing the first {} rows. Export all to get every row.", group(&db::sql::RUN_ROWS_MAX.to_string()))).font(FontId::monospace(TXT_SMALL)).color(WARNING()));
                    if quiet(ui, "Export all…").on_hover_text("Run the statement again and stream every row to a file").clicked() {
                        export = Some((s.qchecked.iter().copied().collect(), true));
                    }
                });
                ui.add_space(4.0);
            }
            let checked_n = s.qchecked.len();
            let colnames = result_keys(&s.sets[sel].cols);
            let nshown = table::visible_cols(s.qprefs.get(&colnames), &colnames, usize::MAX).len();
            let sorted_by = s.sets[sel].sort.clone();
            if !s.sets[sel].rows.is_empty() && !text_mode && (!sorted_by.is_empty() || nshown < colnames.len()) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if !sorted_by.is_empty() {
                        let keys = sorted_by.iter().map(|(c, desc)| format!("{c} {}", if *desc { "descending" } else { "ascending" })).collect::<Vec<_>>().join(", ");
                        ui.label(RichText::new(format!("Sorted on this page by {keys}. The query did not run again.")).font(FontId::monospace(TXT_SMALL)).color(WARNING()));
                        unsort = quiet(ui, "Unsort").on_hover_text("Back to the order the server sent").clicked();
                    }
                    if nshown < colnames.len() {
                        ui.label(RichText::new(format!("{} hidden", group(&(colnames.len() - nshown).to_string()))).font(FontId::monospace(TXT_SMALL)).color(MUTED()));
                        if quiet(ui, "Show all columns").clicked() {
                            show_cols = Some(colnames.clone());
                        }
                    }
                });
                ui.add_space(4.0);
            }
            if s.sets[sel].rows.is_empty() {
                ui.label(RichText::new("No rows").color(MUTED()));
            } else if text_mode {
                if s.text_cache.as_ref().is_none_or(|c| c.0 != sel) {
                    let docs: Vec<Document> = s.sets[sel].rows.iter().map(db::Row::owned).collect();
                    s.text_cache = Some((sel, db::copy::text_table(&s.sets[sel].cols.iter().map(|c| c.name.clone()).collect::<Vec<_>>(), &docs)));
                }
                let text = s.text_cache.as_ref().map(|c| c.1.as_str()).unwrap_or_default();
                egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(8).show(ui, |ui| {
                    egui::ScrollArea::both().id_salt("sql-text").auto_shrink(false).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(text).font(FontId::monospace(TXT_SMALL)).color(TEXT())).extend());
                    });
                });
            } else {
                // The bar floats over the bottom of the grid, so it is placed before the grid takes the room.
                if !hidden {
                    let (cancel, act) = selection_area(ui, checked_n, false, true, true, false);
                    if cancel {
                        s.qchecked.clear();
                    }
                    match act {
                        Some(DocsAct::CopyAs(f)) => copy = Some((f, Some(s.qchecked.iter().copied().collect()))),
                        Some(DocsAct::Export) => export = Some((s.qchecked.iter().copied().collect(), false)),
                        _ => {}
                    }
                }
                // The record draws through `self`, so the results leave the tab while the grid draws, as in `results`.
                let qsel = s.qsel.filter(|&i| i < s.sets[sel].rows.len());
                let (sets, checked) = (std::mem::take(&mut s.sets), std::mem::take(&mut s.qchecked));
                let names: Vec<String> = if record_open { colnames.clone() } else { Vec::new() };
                let prefs = s.qprefs.get(&colnames).cloned().unwrap_or_default();
                // Only a result read from one table has a table to filter. The lexer reads a short statement per frame.
                let filter = sets[sel].kind == "SELECT" && sets[sel].stmt.len() < 20_000 && db::sql::source::single_table(&sets[sel].stmt, dialect).is_some();
                let scroll = std::mem::take(&mut self.scroll_sel);
                let mut rec = crate::app::sql::record::RecordOut::default();
                let mut draw = |ui: &mut egui::Ui| self.sql_record(ui, Rows::Raw(&sets[sel].rows), qsel.unwrap_or(0), true, &names, &[], RowMenu::None, &checked, &mut rec);
                let out = grid(ui, &sets[sel], &prefs, &checked, qsel, scroll, filter, record_open.then_some(&mut draw as &mut dyn FnMut(&mut egui::Ui)));
                let Some(s) = self.tabs[cur].sql.as_mut() else { return };
                (s.sets, s.qchecked) = (sets, checked);
                view = out.view.or(rec.view);
                head_acts = out.head;
                cell_act = out.cell;
                if let Some(i) = out.select {
                    s.qsel = Some(i);
                }
                if let Some(on) = out.all {
                    s.qchecked = if on { (0..s.sets[sel].rows.len()).collect() } else { BTreeSet::new() };
                }
                if let Some((i, a)) = out.act {
                    let targets: Vec<usize> = if s.qchecked.contains(&i) { s.qchecked.iter().copied().collect() } else { vec![i] };
                    match a {
                        DocsAct::CopyAs(f) => copy = Some((f, Some(targets))),
                        DocsAct::Export => export = Some((targets, false)),
                        _ => {}
                    }
                }
                if let Some((i, range)) = out.check {
                    match s.qanchor.filter(|_| range) {
                        Some(a) => s.qchecked.extend(a.min(i)..=a.max(i)),
                        None => {
                            if !s.qchecked.insert(i) {
                                s.qchecked.remove(&i);
                            }
                        }
                    }
                    s.qanchor = Some(i);
                }
            }
        }
        let tab_coll = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
        let set_rows = |app: &App, idx: &Option<Vec<usize>>| -> Vec<Document> {
            let Some(s) = app.t().and_then(|t| t.sql.as_ref()) else { return Vec::new() };
            let rows = &s.sets[sel.min(s.sets.len().saturating_sub(1))].rows;
            rows.iter().enumerate().filter(|(i, _)| idx.as_ref().is_none_or(|v| v.contains(i))).map(|(_, r)| r.owned()).collect()
        };
        if let Some((c, v)) = view {
            self.sql_view_value(&c, &v);
        }
        if unsort {
            self.sql_sort_page(Vec::new());
        }
        if let Some(names) = show_cols && let Some(s) = self.tm().and_then(|t| t.sql.as_mut()) {
            let p = s.qprefs.entry(names.clone()).or_default();
            let mut all = table::visible_cols(Some(p), &names, usize::MAX);
            all.extend(names.iter().filter(|n| !all.contains(n)).cloned().collect::<Vec<_>>());
            p.shown = Some(all);
        }
        for (c, a) in head_acts {
            self.query_head_act(ui, &c, a);
        }
        if let Some((i, col, a)) = cell_act {
            self.sql_query_cell_act(ui.ctx(), i, &col, a);
        }
        if let Some((fmt, idx)) = copy {
            let rows = set_rows(self, &idx);
            let cols: Vec<CopyCol> = self.tabs[cur].sql.as_ref().map(|s| s.sets[sel].cols.iter().map(|c| CopyCol { name: c.name.clone(), src: c.src.clone(), numeric: is_numeric(c.kind) }).collect()).unwrap_or_default();
            let table = if tab_coll.is_empty() { "result".to_string() } else { tab_coll };
            let ctx = ui.ctx().clone();
            self.copy_sheet(&ctx, fmt, &rows, &cols, &table, &[], self.sql_bs());
        }
        if let Some((idx, all)) = export {
            let rows = set_rows(self, &Some(idx));
            self.open_export(rows, all);
        }
        if let Some(fmt) = copy_all {
            let header = ui.ctx().data(|d| d.get_temp::<bool>(egui::Id::unique("copy-with-header"))).unwrap_or(true);
            self.copy_all_pages(fmt, header);
        }
        if let Some((idx, n)) = goto {
            self.sql_goto_page(idx, n);
        }
    }

    /// QUARRY_TEST_SQL=query: a Query tab with the multi-statement script of the design and its three outcomes.
    pub(in crate::app) fn sql_query_demo(&mut self) {
        let mut t = Tab::new(Sel { conn: 3, db: "shop".into(), coll: String::new(), ..Default::default() }).for_engine(db::sql::Engine::MySql);
        t.view = View::Sql;
        let col = |name: &str, ty: &str| Col::plain(name, ty, Kind::from_type_name(ty), false);
        let people: [(&str, &str, i64, &str); 7] = [
            ("Ada Byron", "ada@example.com", 6, "1412.40"),
            ("Grace Hopper", "grace@example.com", 5, "988.15"),
            ("Ken Thompson", "ken@example.com", 4, "712.00"),
            ("Barbara Liskov", "barbara@example.com", 4, "540.75"),
            ("Edsger Dijkstra", "edsger@example.com", 3, "301.20"),
            ("Margaret Hamilton", "margaret@example.com", 3, "288.00"),
            ("Dennis Ritchie", "dennis@example.com", 2, "190.50"),
        ];
        let raw = "-- Paid orders over the last week, by customer\nSELECT c.name, c.email, COUNT(*) AS orders, SUM(o.total) AS spent\nFROM orders o\nJOIN customers c ON c.id = o.customer_id\nWHERE o.status = 'paid'\n  AND o.created_at >= CURDATE() - INTERVAL 7 DAY\nGROUP BY c.id\nORDER BY spent DESC;\n\nUPDATE orders SET status = 'shipped' WHERE id IN (4405, 4407);\nSELECT * FROM shipments WHERE order_id = 4405;";
        // QUARRY_TEST_SQL=format shows the same script after Format.
        // QUARRY_TEST_SQL=library has two saved scripts and the menu open.
        if std::env::var("QUARRY_TEST_SQL").as_deref() == Ok("library") {
            let saved = |name: &str, sql: &str| crate::library::Script { name: name.into(), kind: "sql".into(), body: sql.into(), conn: 3, ..Default::default() };
            let id = self.library.next_id();
            self.library.scripts.push(crate::library::Script { id, ..saved("Late shipments", "SELECT o.id, o.created_at, s.carrier\nFROM orders o\nLEFT JOIN shipments s ON s.order_id = o.id\nWHERE o.status = 'paid' AND s.id IS NULL;") });
            let id = self.library.next_id();
            self.library.scripts.push(crate::library::Script { id, ..saved("Revenue by tier", "SELECT tier, SUM(total) AS revenue FROM orders GROUP BY tier ORDER BY revenue DESC;") });
            self.library.recent = self.library.scripts.iter().map(|s| crate::library::ScriptRef::Saved(s.id)).collect();
        }
        let query = if std::env::var("QUARRY_TEST_SQL").as_deref() == Ok("format") { db::sql::format::format_script(raw, db::sql::Dialect::MySql).text } else { raw.to_string() };
        let query = query.as_str();
        let stmt = query.lines().skip(1).take(7).collect::<Vec<_>>().join("\n");
        let first = ResultSet {
            cols: vec![col("name", "VARCHAR"), col("email", "VARCHAR"), col("orders", "BIGINT"), col("spent", "DECIMAL")],
            rows: people.iter().filter_map(|&(n, e, o, s)| db::Row::from_doc(&doc! { "name": n, "email": e, "orders": o, "spent": s }).ok()).collect(),
            ms: 31,
            ..ResultSet::new(&stmt)
        };
        let refused = ResultSet { error: Some("This statement is not allowed on a read-only connection. Unlock writes to run it.".into()), refused: true, ..ResultSet::new("UPDATE orders SET status = 'shipped' WHERE id IN (4405, 4407)") };
        let stmt = "SELECT * FROM shipments WHERE order_id = 4405";
        let last = ResultSet {
            cols: vec![col("id", "INT"), col("order_id", "INT"), col("carrier", "VARCHAR")],
            rows: db::Row::from_doc(&doc! { "id": 912_i64, "order_id": 4405_i64, "carrier": "DHL" }).into_iter().collect(),
            ms: 4,
            ..ResultSet::new(stmt)
        };
        if let Some(s) = t.sql.as_mut() {
            s.caret = query.lines().take(6).map(|l| l.chars().count() + 1).sum::<usize>() - 1;
            (s.query, s.sets) = (query.into(), vec![first, refused, last]);
            // QUARRY_TEST_CHECKED=1 shows three rows checked with the selection bar.
            if std::env::var("QUARRY_TEST_CHECKED").is_ok() {
                s.qchecked = [0, 2, 3].into();
            }
        }
        self.tabs.push(t);
        self.cur = self.tabs.len() - 1;
        // QUARRY_TEST_EXPORT=1 opens the Export dialog on the checked rows.
        // QUARRY_TEST_EXPORT=script shows the scope "All statements in the editor" on the demo script, which has a write in it
        // and so is refused; =script-ok shows it on a script of reads.
        if let Ok(v) = std::env::var("QUARRY_TEST_EXPORT") {
            let rows = self.checked_rows();
            self.open_export(rows, false);
            match v.as_str() {
                "script" => self.demo_script_scope(None),
                "script-ok" => self.demo_script_scope(Some("USE shop;\nSET @since = '2024-01-01';\nSELECT id, status, total FROM orders WHERE created_at > @since;\nSELECT carrier, COUNT(*) AS n FROM shipments GROUP BY carrier")),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn another_database_never_inherits_the_tabs_schema() {
        // Typed wins, and the tab's own schema serves only its own database.
        assert_eq!(run_schema_choice(" audit ", "b", "a", "public"), "audit");
        assert_eq!(run_schema_choice("sales data ", "b", "a", "public"), "sales data");
        assert_eq!(run_schema_choice("  ", "a", "a", "public"), "public");
        assert_eq!(run_schema_choice("", "", "a", "public"), "public");
        assert_eq!(run_schema_choice("", "a", "a", "public"), "public");
        assert_eq!(run_schema_choice("", "b", "a", "public"), "");
    }

    #[test]
    fn caret_line_and_column() {
        let t = "SELECT 1;\n  SELECT é;";
        assert_eq!(line_col(t, 0), (1, 1));
        assert_eq!(line_col(t, 10), (2, 1));
        // The caret counts chars, not bytes.
        assert_eq!(line_col(t, t.chars().count()), (2, 12));
    }

    #[test]
    fn statement_under_the_caret() {
        const D: db::sql::Dialect = db::sql::Dialect::MySql;
        let t = "SELECT 1;\n\nUPDATE a SET b = 2;\nSELECT 'x;y';";
        let (r, i, n) = statement_pos(t, 3, D).unwrap();
        assert_eq!((&t[r], i, n), ("SELECT 1", 1, 3));
        let (_, i, _) = statement_pos(t, 14, D).unwrap();
        assert_eq!(i, 2);
        // A caret past the end stays with the last statement.
        assert_eq!(statement_pos(t, 999, D).unwrap().1, 3);
        assert!(statement_pos("  ", 0, D).is_none());
    }

    #[test]
    fn the_menu_preview_starts_at_the_code() {
        assert_eq!(preview("-- Paid orders\n/* by customer */\nSELECT  c.name\nFROM c", db::sql::Dialect::MySql), "SELECT c.name FROM c");
        assert_eq!(preview("-- only a comment", db::sql::Dialect::MySql), "-- only a comment");
    }

    #[test]
    fn kind_skips_comments() {
        assert_eq!(ResultSet::new("-- hi\n  select 1").kind, "SELECT");
        assert_eq!(ResultSet::new("").kind, "");
    }
}
