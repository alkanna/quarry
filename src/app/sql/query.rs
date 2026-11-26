//! The Query view of a SQL tab: a script editor, Run, Stop and Explain, and one result tab per statement.

use crate::app::*;
use quarry_core::action::{Action, Pref, PrefsA, SqlA};
use crate::app::column_menu::{query_col_popover, HeadAct};
use crate::app::sql::edit::CellAct;
use crate::app::library::snippet_strip::{literal_at, Src};
use crate::ui::dnd;
use crate::db::sql::{lex, value::Kind, Col, ResultSet};
use db::copy::{CopyCol, Fmt};
use crate::ui::sql_complete::{open_list, sql_ac_field};
use egui::text::{CCursor, CCursorRange};
use std::ops::Range;
pub(crate) use quarry_core::state::sql::query::*;
use quarry_core::state::sql::query::byte_at;

/// Height of the editor's footer line.
const FOOTER_H: f32 = 24.0;

/// A fixed line height, so the gutter numbers and the statement band line up with the text.
const LINE_H: f32 = 20.0;

/// The statement under a caret (a char index) as a byte range, with its 1-based number and the count of statements.
#[cfg(test)]
fn statement_pos(text: &str, caret: usize, d: db::sql::Dialect) -> Option<(Range<usize>, usize, usize)> {
    let all: Vec<Range<usize>> = lex::split_statements(text, d).into_iter().map(|(r, _)| r).collect();
    statement_in(&all, text, caret, d)
}

/// Wavy lines under what the live check marked, red for a syntax error and amber for a name the server does not know,
/// with the server's sentence when the pointer rests on one.
fn paint_lints(ui: &egui::Ui, out: &egui::text_edit::TextEditOutput, query: &str, stmts: &[Range<usize>], lints: &LintState) {
    if lints.seen.is_empty() {
        return;
    }
    let (g, gp, hover) = (&out.galley, out.galley_pos, out.response.hover_pos());
    for r in stmts {
        let Some(l) = lints.mark(&query[r.clone()]) else { continue };
        let at = |b: usize| CCursor::new(query[..r.start + b].chars().count());
        let (a, z) = (g.pos_from_cursor(at(l.span.start)), g.pos_from_cursor(at(l.span.end)));
        let (x0, y) = (gp.x + a.left(), gp.y + a.bottom() - 2.0);
        let x1 = (gp.x + z.left()).max(x0 + 6.0);
        crate::ui::widgets::wavy_line(ui.painter(), x0..=x1, y, if l.warn { WARNING() } else { ALERT() });
        let rect = egui::Rect::from_min_max(egui::pos2(x0, gp.y + a.top()), egui::pos2(x1, gp.y + a.bottom()));
        if hover.is_some_and(|p| rect.contains(p)) {
            let _ = egui::Response::clone(&out.response).on_hover_text_at_pointer(l.msg.as_str());
        }
    }
}

/// One marker per statement in the gutter, at its first line: a gray play mark, or the accent with a play mark for the statement
/// under the caret. A click returns the text of the statement to run.
fn gutter_markers(ui: &mut egui::Ui, out: &egui::text_edit::TextEditOutput, query: &str, stmts: &[Range<usize>], current: Option<usize>, busy: bool) -> Option<String> {
    // The galley row each logical line starts on.
    let mut line_rows = Vec::new();
    let mut fresh = true;
    for (i, row) in out.galley.rows.iter().enumerate() {
        if fresh {
            line_rows.push(i);
        }
        fresh = row.ends_with_newline;
    }
    let (gp, rect) = (out.galley_pos, out.response.rect);
    let mut run = None;
    for (i, r) in stmts.iter().enumerate() {
        let text = &query[r.clone()];
        // A statement's range can open on the blank lines after the previous one, so its line is its first code.
        let start = r.start + (query[r.start..].len() - query[r.start..].trim_start().len());
        let line = query[..start].matches('\n').count();
        let Some(row) = line_rows.get(line).and_then(|&ri| out.galley.rows.get(ri)) else { continue };
        let c = egui::pos2(rect.left() + 12.0, gp.y + row.pos.y + row.height() / 2.0);
        let hit = egui::Rect::from_center_size(c, egui::vec2(16.0, 16.0));
        let resp = ui.interact(hit, out.response.id.with(("gutter-run", i)), if busy { Sense::hover() } else { Sense::click() });
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, !busy, format!("Run statement {} from the gutter", i + 1)));
        let hot = resp.hovered() && !busy;
        let p = ui.painter();
        if current == Some(i + 1) {
            p.circle_filled(c, 6.0, if hot { shade(PRIMARY(), 0.10) } else { PRIMARY() });
            play_mark(p, c - egui::vec2(1.5, 0.0), 4.0, 2.5, ON_PRIMARY());
        } else {
            // A gray play mark, not a ring: a ring read as a radio button that selects the statement, not one that runs it.
            if hot {
                p.circle_filled(c, 7.0, HOVER());
            }
            play_mark(p, c - egui::vec2(2.5, 0.0), 6.0, 3.5, if hot { TEXT() } else { FAINT() });
        }
        if !busy {
            let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(format!("Run statement {}", i + 1));
            if resp.clicked() {
                run = Some(text.to_string());
            }
        }
    }
    run
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
    /// The column a plain click landed in, as a place among the columns shown, for the Vim cursor.
    col: Option<usize>,
    /// The column names in the order shown.
    shown: Vec<String>,
}

/// The rows of one result set as a grid with a checkbox column. Checked rows are tinted, and the menu of a
/// checked row acts on every checked row. `record` draws the selected row's record under it.
/// `prefs` are the column choices of this result, and `filter` whether the result came from one table, which
/// turns a cell into a filter on it.
/// `cursor` is the Vim cursor's place among the columns shown, scrolled into view with `scroll`, and whether the record
/// has the keys, which draws it faint.
#[allow(clippy::too_many_arguments)]
fn grid(ui: &mut egui::Ui, set: &ResultSet, prefs: &ColPrefs, checked: &BTreeSet<usize>, sel: Option<usize>, scroll: bool, cursor: Option<(usize, bool)>, filter: bool, mut record: Option<&mut dyn FnMut(&mut egui::Ui)>) -> GridOut {
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
    let mut out = GridOut { shown: if cursor.is_some() { order.iter().map(|&ci| names[ci].clone()).collect() } else { Vec::new() }, ..Default::default() };
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
                        // After the row's own scroll, so the cell's wins sideways.
                        if let Some((k, false)) = cursor.filter(|&(k, _)| k < order.len()) {
                            let x = rr.left() + lead + order[..k].iter().map(|&ci| widths[ci]).sum::<f32>();
                            ui.scroll_to_rect(egui::Rect::from_min_size(egui::pos2(x, rr.top()), egui::vec2(widths[order[k]], ROW_H)), None);
                        }
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
                        for (k, &ci) in order.iter().enumerate() {
                            let (c, w) = (&set.cols[ci], &widths[ci]);
                            let cell = egui::Rect::from_min_size(egui::pos2(x, rr.top()), egui::vec2(*w, ROW_H));
                            x += w;
                            let here = sel == Some(i) && cursor.map(|(c, _)| c.min(order.len() - 1)) == Some(k);
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
                            if here {
                                table::paint_cursor(ui, cell, if cursor.is_some_and(|(_, rec)| rec) { FAINT() } else { PRIMARY() });
                            }
                            if let Some(v) = table::view_button(ui, cell, 0.0, v, ("eye", i, ci), || format!("View {} row {}", c.name, i + 1)) {
                                out.view = Some((c.name.clone(), v));
                            }
                        }
                        // Cmd or Shift on a row checks it like in the Content grid; a plain click selects it.
                        // The place among the shown columns of the cell under x, for the Vim cursor and the cell menu alike.
                        let col_at = |px: f32| {
                            let mut x = rr.left() + lead;
                            order.iter().position(|&ci| {
                                x += widths[ci];
                                px < x
                            })
                        };
                        if resp.clicked() {
                            if shift || ui.input(|i| i.modifiers.command) {
                                out.check = Some((i, shift));
                            } else {
                                out.select = Some(i);
                                out.col = resp.interact_pointer_pos().and_then(|p| col_at(p.x));
                            }
                        }
                        // The menu knows its cell only from where the right click landed, so that is kept for it.
                        let cell_id = ui.scope_id().with("menu-cell");
                        if resp.secondary_clicked() && let Some(p) = resp.interact_pointer_pos() {
                            let hit = col_at(p.x).map(|k| order[k]);
                            ui.ctx().data_mut(|d| d.insert_temp(cell_id, hit.unwrap_or(usize::MAX)));
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
        let Some(t) = self.core.tabs.get(i) else { return String::new() };
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
            return format!("Query {}", self.core.tabs[..=i].iter().filter(|x| x.sql.is_some() && x.sel.coll.is_empty()).count());
        }
        t.sel.coll.clone()
    }

    /// Opens a Query tab for a database. It has no table, so its collection name stays empty.
    pub(in crate::app) fn sql_new_query(&mut self, conn: u64, db: String) {
        self.exec(Action::Tab(TabA::NewQuery { conn, db, schema: String::new() }))
    }

    pub(in crate::app) fn sql_sort_page(&mut self, keys: Vec<(String, bool)>) {
        if let Some(tab) = self.uid_of(self.core.cur) {
            self.exec(Action::Sql(SqlA::SortPage { tab, keys }));
        }
    }

    pub(in crate::app) fn sql_run(&mut self, all: bool, explain: bool) {
        if let Some(tab) = self.uid_of(self.core.cur) {
            self.exec(Action::Sql(SqlA::Run { tab, all, explain }));
        }
    }

    /// The text selected in the editor, if any, for Run selection.
    fn editor_selection(&self, ctx: &egui::Context) -> Option<String> {
        let q = &self.t()?.sql.as_ref()?.query;
        let r = egui::TextEdit::load_state(ctx, Self::sql_editor_id(self.core.cur))?.cursor.char_range().filter(|r| r.primary != r.secondary)?;
        let (p, q2) = (usize::from(r.primary.index), usize::from(r.secondary.index));
        let (a, b) = (byte_at(q, p.min(q2)), byte_at(q, p.max(q2)));
        Some(q[a..b].to_string()).filter(|t| !t.trim().is_empty())
    }

    /// The statements of the script, one line each under a separator, for the Run button's tip. Draws nothing for an empty script. Returns the one picked.
    fn sql_statement_menu(&self, ui: &mut egui::Ui) -> Option<String> {
        let s = self.t()?.sql.as_ref()?;
        let d = self.sql_dialect();
        let info = s.caret_info(d);
        if info.stmts.is_empty() {
            return None;
        }
        let here = info.stmt.map(|(r, _, _)| r);
        let mut pick = None;
        ui.separator();
        egui::ScrollArea::vertical().id_salt("sql-run-pick").max_height(360.0).show(ui, |ui| {
            for (i, r) in info.stmts.iter().cloned().enumerate() {
                let text = &s.query[r.clone()];
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

    /// A cell menu pick in the shown result.
    pub(in crate::app) fn sql_query_cell_act(&mut self, ctx: &egui::Context, row: usize, col: &str, act: CellAct) {
        let Some(s) = self.t().and_then(|t| t.sql.as_ref()) else { return };
        let Some(set) = s.cur_set() else { return };
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
        let Some(set) = s.cur_set() else { return };
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
        let text = if v == Bson::Null { format!("{q} IS NULL") } else { format!("{q} = {}", db::copy::sql_literal(d, Some(&v), numeric, self.sql_bs())) };
        // The editor belongs to this tab's rows; it would stage into the wrong table once the tab changes.
        if self.core.sql_cell.is_some() {
            return self.warn("Finish or cancel the cell edit first.");
        }
        let base = Sel { conn: t.sel.conn, db: db_name, schema: String::new(), coll: String::new() };
        let sel = super::ns::with_ns(d, &base, &ns, table);
        self.sql_open_filtered(sel, text);
    }

    /// Opens a table's Content in a tab of its own, filtered by the WHERE text `filter`.
    pub(in crate::app) fn sql_open_filtered(&mut self, sel: Sel, filter: String) {
        self.exec(Action::Tab(TabA::FilterText { sel: Some(sel), text: filter }));
    }

    /// Show the other row of a duplicate-key error on table `sel`. The error names an index (a PostgreSQL unique
    /// constraint has its index's name), so the table's indexes are read first: the index must be there and cover one
    /// whole column, and that column is filtered on. Reading it from the table also gives the column's own name,
    /// which PostgreSQL's message quotes and MySQL's does not give at all.
    pub(in crate::app) fn sql_show_dup(&mut self, sel: Sel, index: String, value: String) {
        if self.core.sql_cell.is_some() {
            return self.warn("Finish or cancel the cell edit first.");
        }
        self.exec(Action::SqlAsk(SqlAskA::ShowDup { sel, index, value }));
    }

    /// The table's indexes are in: the duplicate's index must be on this table and be one whole column.
    pub(in crate::app) fn sql_dup_index(&mut self, sel: Sel, index: &str, value: String, res: db::Res<db::sql::Structure>) {
        let st = match res {
            Ok(st) => st,
            Err(e) => return self.err(e),
        };
        let Some(ix) = st.indexes.iter().find(|i| i.name.eq_ignore_ascii_case(index)) else {
            return self.warn(format!("The key {index} is not on {}, so its other row is in another table.", sel.coll));
        };
        let [c] = ix.cols.as_slice() else { return self.warn("This key spans several columns, so its value cannot be filtered on.") };
        if c.prefix.is_some() || c.expr.is_some() {
            return self.warn("This key covers part of a column or an expression, so its value cannot be filtered on.");
        }
        let filter = self.dup_filter(sel.conn, &c.name, value);
        self.sql_open_filtered(sel, filter);
    }

    /// `col = 'value'` in the dialect of connection `conn`. PostgreSQL prints a NULL key value as null, the same as the
    /// text 'null', so that one matches both.
    fn dup_filter(&self, conn: u64, col: &str, value: String) -> String {
        let d = self.dialect_of(conn);
        let bs = self.sql_bs_of(conn);
        let q = d.quote_ident(col);
        let eq = format!("{q} = {}", db::copy::sql_literal(d, Some(&Bson::String(value.clone())), false, bs));
        if d == db::sql::Dialect::Postgres && value == "null" { format!("({q} IS NULL OR {eq})") } else { eq }
    }

    pub(in crate::app) fn sql_query_view(&mut self, ui: &mut egui::Ui) {
        let Some(t) = self.t() else { return };
        let (conn, free, schema, tab) = (t.sel.conn, t.sel.coll.is_empty(), run_db(t), t.uid);
        let running = t.sql.as_ref().is_some_and(|s| s.run_req != 0);
        let lib_pick = self.pick_data(ui.ctx(), PickKind::Sql);
        let mut pick_acts = Vec::new();
        let mut load = None;
        let dialect = self.sql_dialect();
        // The tab, the title bar crumbs and the editor footer name the script and where it runs, so the view has no header.
        // A table's Query view gets these from the view tabs above it; a Query tab of its own has none.
        if free {
            self.script_banners(ui);
        }
        let (mut run, mut run_all, mut stop, mut explain, mut format, mut run_pick, mut save_view, mut run_sel) = (false, false, false, false, false, None, false, None);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            // As tall as History, Library and Run, so the clip on the tools below does not cut their bottom edge.
            ui.set_min_height(PICK_H);
            // The right side is laid out first, so a narrow window clips the tools and never History or Library.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // The first one added sits at the right edge: the library, then the history beside it.
                let button = picker_button(ui, &lib_pick, false, &mut pick_acts);
                self.demo_open_library(ui.ctx(), &button);
                self.demo_open_picker(ui.ctx(), &button, "picker");
                ui.add_space(4.0);
                sticky_menu(ui, (icon_atom(paint_clock), "History"), |ui| {
                    ui.set_min_width(360.0);
                    ui.set_max_width(520.0);
                    let hist = self.core.prefs.history.get(&format!("{conn}/{schema}/#query")).map(|h| h.selector.as_slice()).unwrap_or_default();
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
                ui.add_space(4.0);
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                    // Stop takes the place of Run statement while a run is in flight, so the toolbar never shows a dead button.
                    if running {
                        stop = danger(ui, "Stop").on_hover_text("Interrupt the running statement and skip the rest").clicked();
                    } else {
                        let (main, tip) = run_split(ui, "Run statement", "\u{2318}\u{21b5}", true, "Choose what to run");
                        run = main.on_hover_text("Run the statement under the caret (Cmd+Enter)").clicked();
                        let tip = tip.on_hover_text("Choose what to run");
                        egui::Popup::menu(&tip).width(420.0).show(|ui| {
                            menu_look(ui);
                            // Read only while the menu is open, since it copies the selected text.
                            let selection = self.editor_selection(ui.ctx());
                            if selection.is_some() && ui.add(egui::Button::new("Run selection")).on_hover_text("Run only the selected text").clicked() {
                                run_sel = selection;
                                ui.close();
                            }
                            if ui.add(egui::Button::new("Explain")).on_hover_text("EXPLAIN the statement under the caret (Cmd+E)").clicked() {
                                explain = true;
                                ui.close();
                            }
                            run_pick = self.sql_statement_menu(ui);
                        });
                    }
                    ui.add_enabled_ui(!running, |ui| {
                        run_all = ui.add(egui::Button::new("Run all").right_text(RichText::new("\u{21e7}\u{2318}\u{21b5}").size(TXT_CAPTION).color(FAINT()))).on_hover_text("Run every statement (Cmd+Shift+Enter)").clicked();
                    });
                    explain |= quiet_enabled(ui, !running, "Explain").on_hover_text("EXPLAIN the statement under the caret (Cmd+E)").clicked();
                    format = quiet_enabled(ui, !running, "Format").on_hover_text("Lay out the selection, or the whole script when nothing is selected (Shift+Alt+F). A statement that would read differently stays as it is.").clicked();
                    save_view = quiet_enabled(ui, !running, "Save as view\u{2026}").on_hover_text("Create a view from the statement under the caret").clicked();
                });
            });
        });
        ui.add_space(8.0);
        let ctx = ui.ctx().clone();
        if let Some(text) = load {
            self.sql_replace(&ctx, text, None);
        }
        // Not while a run is on, as the button: the statement ranges it reports point into the text it ran.
        if format || !running && crate::ui::editor::format_key(ui, Self::sql_editor_id(self.core.cur)) {
            self.sql_format(&ctx);
        }
        if self.snippet_strip(ui, Src::Query) {
            self.snippet_preview(ui);
        } else {
            self.sql_editor(ui);
        }
        ui.add_space(4.0);
        self.sql_result_pane(ui, running);
        if stop {
            self.exec(Action::Sql(SqlA::Stop { tab }));
        }
        if let Some(text) = run_pick.or(run_sel) {
            self.exec(Action::Sql(SqlA::RunText { tab, text }));
        }
        if run || run_all || explain {
            self.exec(Action::Sql(SqlA::Run { tab, all: run_all, explain }));
        }
        if save_view {
            self.exec(Action::Ddl(quarry_core::action::DdlA::QueryAsView { tab }));
        }
        // Last, since opening a script may change the tab this frame was drawn for.
        self.pick_acts(pick_acts);
    }

    /// Sends the statement under the caret (`stmt`, a byte range of the script) to be checked once typing has rested, one
    /// request at a time per tab; Core decides, the app only waits out the pause.
    fn sql_lint_tick(&mut self, ctx: &egui::Context, stmt: Option<Range<usize>>) {
        let Some(stmt) = stmt else { return };
        self.exec(Action::SqlAsk(SqlAskA::LintTick { now: ctx.input(|i| i.time), stmt }));
        self.take_repaint(ctx);
    }

    /// QUARRY_TEST_SQL=lint and QUARRY_TEST_PG=lint: a statement with a syntax error under the caret and one naming an
    /// unknown table, marked as the live check marks them.
    pub(in crate::app) fn demo_lint(&mut self, pg: bool) {
        let core = self.core.setup();
        let Some(s) = core.tabs.get_mut(core.cur).and_then(|t| t.sql.as_mut()) else { return };
        let (bad, unknown) = ("SELCT ( FRO orders", "SELECT * FROM shipment WHERE order_id = 4405");
        let lint = |span: Range<usize>, msg: &str, warn: bool| Ok(Some(db::sql::Lint { span, msg: db::lint_wording(msg, warn), warn }));
        let (err, warn) = if pg {
            (lint(0..5, "syntax error at or near \"SELCT\"", false), lint(14..22, "relation \"shipment\" does not exist", true))
        } else {
            (lint(0..5, "You have an error in your SQL syntax; check the manual that corresponds to your MySQL server version for the right syntax to use near 'SELCT ( FRO orders' at line 1", false), lint(14..22, "Table 'shop.shipment' doesn't exist", true))
        };
        (s.query, s.caret) = (format!("{bad};\n{unknown};"), 5);
        s.lint.put(bad.into(), err);
        s.lint.put(unknown.into(), warn);
    }

    pub(in crate::app) fn sql_editor_id(cur: usize) -> egui::Id {
        egui::Id::unique(("sql-editor", cur))
    }

    /// Puts `new` in the editor so that Cmd+Z brings the old text back. egui's editor keeps its undo history as whole
    /// states, so the text before the change is added as a point first; changing the string alone would only become
    /// one after the text had rested for a second. `select` is the range of the new text to leave selected.
    pub(in crate::app) fn sql_replace(&mut self, ctx: &egui::Context, new: String, select: Option<Range<usize>>) {
        let id = Self::sql_editor_id(self.core.cur);
        let Some((tab, mut text)) = self.core.tabs.get(self.core.cur).and_then(|t| t.sql.as_ref().map(|s| (t.uid, s.query.clone()))) else { return };
        if text == new {
            return;
        }
        let range = crate::ui::editor::replace_with_undo(ctx, id, &mut text, new, |before, _, new| {
            let end = new.chars().count();
            match select {
                Some(r) => CCursorRange::two(CCursor::new(r.start.min(end)), CCursor::new(r.end.min(end))),
                None => CCursorRange::one(CCursor::new(usize::from(before.primary.index).min(end))),
            }
        });
        self.exec(Action::Sql(SqlA::SetQuery { tab, text, run_db: None, run_schema: None, caret: Some(usize::from(range.primary.index)), show: false }));
    }

    /// Format: the selection, or the whole script when nothing is selected.
    fn sql_format(&mut self, ctx: &egui::Context) {
        let Some(s) = self.core.tabs.get(self.core.cur).and_then(|t| t.sql.as_ref()) else { return };
        let text = s.query.clone();
        let picked = egui::TextEdit::load_state(ctx, Self::sql_editor_id(self.core.cur)).and_then(|st| st.cursor.char_range()).filter(|r| r.primary != r.secondary);
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
        let note = if new == text && f.untouched() == 0 { "Already formatted".to_string() } else { f.summary() };
        self.sql_replace(ctx, new, select);
        if f.refused > 0 { self.warn(note) } else { self.ok(note) }
    }

    /// The script editor with line numbers, token colors and a faint band behind the statement under the caret.
    fn sql_editor(&mut self, ui: &mut egui::Ui) {
        let id = Self::sql_editor_id(self.core.cur);
        let focused = ui.memory(|m| m.has_focus(id));
        let avail = ui.available_height();
        let Some((conn, db, schema)) = self.core.tabs.get(self.core.cur).map(|t| (t.sel.conn, run_db(t), if self.is_pg(t.sel.conn) { Some(run_schema_of(t)).filter(|s| !s.is_empty()).unwrap_or_else(|| self.sel_schema(&t.sel)) } else { String::new() })) else { return };
        let scope = Self::sql_scope(&self.core.conns, &self.core.sql_schema, &self.core.views, conn, &db, &schema, None);
        let dialect = scope.dialect;
        let none = Fields::new();
        let values = self.core.sql_values.get(&(conn, ns_key(&db, &schema))).unwrap_or(&none);
        // The footer's words are worked out before the tab is borrowed for the editor.
        let engine_db = self.conn(conn).map(|c| format!("{}  \u{b7}  {db}", super::super::sidebar::conn_product(c))).unwrap_or_default();
        let saved_in = self.core.tabs.get(self.core.cur).and_then(|t| t.script.as_ref()).map(|b| self.chip_text(&b.r)).and_then(|t| t.split_once("  \u{b7}  ").map(|(name, place)| format!("Saved in {place} / {name}")));
        let mut gutter_run = None;
        let mut lint_stmt = None;
        let mut make_value = None;
        let tab = self.cur_uid().unwrap_or_default();
        // The editor edits copies, which are sent as one action once it is drawn: the script, and its suggestion list.
        let mut ac = self.ui.sql_ac.remove(&tab).unwrap_or_default();
        let Some(s) = self.core.tabs.get(self.core.cur).and_then(|t| t.sql.as_ref()) else { return };
        let mut query = s.query.clone();
        let mut wants = crate::ui::sql_complete::Wants::default();
        let mut new_caret = s.caret;
        let lines = s.query.split('\n').count();
        // The editor keeps three lines, and the results below it their tabs, header and three rows.
        let (lo, hi) = (3.0 * LINE_H + 24.0, avail - 3.0 * ROW_H - TABLE_HEAD_H - 110.0);
        let custom = self.core.prefs.query_editor_h > 0.0;
        let h = if custom { self.core.prefs.query_editor_h.clamp(lo, hi.max(lo)) } else { (lines as f32 * LINE_H + 24.0).clamp(96.0, (avail * 0.5).max(96.0)) };
        let mut layout = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
            crate::ui::editor::memo_galley(ui, 1, buf.as_str(), wrap, || {
                let mut job = crate::ui::editor::sql_job_in(buf.as_str(), TXT_SMALL, dialect);
                for sec in &mut job.sections {
                    sec.format.line_height = Some(LINE_H);
                }
                let holes: Vec<_> = crate::library::snippet::parse(buf.as_str(), dialect).holes.into_iter().map(|h| h.span).collect();
                crate::ui::editor::tint_holes(&mut job, &holes);
                job
            })
        };
        let framed = egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, if focused { PRIMARY() } else { BORDER() })).corner_radius(R_MD).show(ui, |ui| {
            let mut caret = None;
            let shown = egui::ScrollArea::vertical().id_salt("sql-ed").max_height(h).auto_shrink([false, true]).show(ui, |ui| {
                // Reserved first so the band is drawn under the text.
                let band = ui.painter().add(egui::Shape::Noop);
                if s.bar.preset.ac {
                    open_list(ui, id, &query, &mut ac, &scope, values);
                }
                // The suggestions work on the statement under the caret, with the tail after it for aliases.
                let (out, _) = crate::ui::vim::wrap_with(ui, id, &mut query, |ui, query| {
                    sql_ac_field(ui, id, query, &mut ac, &scope, values, &mut wants, |ui, text, _| {
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
                    new_caret = usize::from(r.primary.index);
                }
                // Make it a value... turns the literal under the selection into a {{...}} placeholder. The selection is read
                // from the editor's stored state when the menu draws, as Run selection does.
                out.response.context_menu(|ui| {
                    menu_list(ui);
                    let picked = egui::TextEdit::load_state(ui.ctx(), id).and_then(|st| st.cursor.char_range()).and_then(|r| {
                        let (a, b) = (usize::from(r.primary.index), usize::from(r.secondary.index));
                        let (a, b) = (byte_at(&query, a.min(b)), byte_at(&query, a.max(b)));
                        literal_at(&query, a..b.max(a + 1), dialect.into())
                    });
                    let mut acts = Vec::new();
                    if let Some(r) = picked {
                        menu_act(ui, &mut acts, r, |ui| ui.button("Make it a value\u{2026}"));
                    } else {
                        menu_act(ui, &mut acts, 0..0, |ui| ui.add_enabled(false, egui::Button::new("Make it a value\u{2026}")).on_disabled_hover_text("Select a string or a number"));
                    }
                    if let Some(r) = acts.pop() {
                        make_value = Some(r);
                    }
                });
                let (g, gp, rect) = (&out.galley, out.galley_pos, out.response.rect);
                let info = s.caret_info(dialect);
                if let Some((r, _, _)) = info.stmt.as_ref().filter(|p| p.2 > 1) {
                    let at = |b: usize| s.query[..b].chars().count();
                    let (a, z) = (g.pos_from_cursor(CCursor::new(at(r.start))), g.pos_from_cursor(CCursor::new(at(r.end))));
                    let band_rect = egui::Rect::from_min_max(egui::pos2(rect.left(), gp.y + a.top()), egui::pos2(rect.right(), gp.y + z.bottom()));
                    ui.painter().set(band, egui::Shape::rect_filled(band_rect, 0.0, tint(PRIMARY(), 22)));
                }
                paint_lints(ui, &out, &s.query, &info.stmts, &s.lint);
                lint_stmt = info.stmt.as_ref().map(|(r, _, _)| r.clone());
                paint_gutter(ui, &out, Some(info.line_col.0));
                gutter_run = gutter_markers(ui, &out, &s.query, &info.stmts, info.stmt.as_ref().map(|p| p.1), s.run_req != 0);
                caret = Some(info);
            });
            // Inside the frame, so the position reads as part of the editor and not of the toolbar.
            ui.horizontal(|ui| {
                ui.set_height(FOOTER_H);
                let info = caret.take().unwrap_or_else(|| s.caret_info(dialect));
                let place = info.stmt.as_ref().map_or(String::new(), |(_, i, n)| format!("statement {i} of {n}  \u{b7}  "));
                let faint = |t: String| RichText::new(t).font(FontId::monospace(TXT_CAPTION)).color(FAINT());
                ui.add_space(10.0);
                ui.label(faint(format!("{place}Ln {}, Col {}  \u{b7}  {engine_db}", info.line_col.0, info.line_col.1)));
                if let Some(l) = info.stmt.as_ref().and_then(|(r, _, _)| s.lint.mark(&s.query[r.clone()])) {
                    ui.add(egui::Label::new(RichText::new(l.msg.as_str()).font(FontId::monospace(TXT_CAPTION)).color(if l.warn { WARNING() } else { ALERT() })).truncate());
                }
                if let Some(t) = &saved_in {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(10.0);
                        ui.label(faint(t.clone()));
                    });
                }
            });
            // The editor's own height, which a drag starts from. The frame also holds the stroke, the spacing and the footer.
            // inner_rect is the room offered before the area shrank, so the content is read instead.
            shown.content_size.y.min(h)
        });
        self.ui.sql_ac.insert(tab, ac);
        let edited = self.core.tabs.get(self.core.cur).and_then(|t| t.sql.as_ref()).is_some_and(|s| s.query != query || s.caret != new_caret);
        if edited {
            self.exec(Action::Sql(SqlA::SetQuery { tab, text: query, run_db: None, run_schema: None, caret: Some(new_caret), show: false }));
        }
        self.sql_serve(conn, &db, &schema, wants);
        // What was under the caret belongs to the script before this frame's typing, so the check waits for the next frame.
        self.sql_lint_tick(ui.ctx(), lint_stmt.filter(|_| !edited));
        if let Some(r) = make_value {
            self.ask_make_value(r, Src::Query);
        }
        let bar = splitter(ui);
        let mut editor_h = self.core.prefs.query_editor_h;
        if resize_editor(&bar, &mut editor_h, framed.inner, lo, hi) {
            self.exec(Action::Prefs(PrefsA::Set(Pref::QueryEditorH(editor_h))));
        }
        // The same path as picking the statement from the Run menu.
        if let Some(text) = gutter_run {
            let tab = self.core.tabs[self.core.cur].uid;
            self.exec(Action::Sql(SqlA::RunText { tab, text }));
        }
    }

    /// The result tabs, the Table or Text switch and the selected statement's outcome.
    fn sql_result_pane(&mut self, ui: &mut egui::Ui, running: bool) {
        let cur = self.core.cur;
        let tab = self.core.tabs[cur].uid;
        let mut acts: Vec<SqlA> = Vec::new();
        let hidden = self.modal.is_some() || self.core.insert.is_some();
        let dialect = self.sql_dialect();
        let record_open = self.core.tabs.get(cur).is_some_and(|t| t.record);
        // The Text view is built by Core, once per result, before the frame reads it.
        if self.core.tabs.get(cur).and_then(|t| t.sql.as_ref()).is_some_and(|s| s.text_mode && !s.sets.is_empty() && s.text_cache.as_ref().is_none_or(|c| c.0 != s.set_sel.min(s.sets.len() - 1))) {
            self.exec(Action::Sql(SqlA::BuildText { tab }));
        }
        let Some(s) = self.core.tabs.get(cur).and_then(|t| t.sql.as_ref()) else { return };
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
        if text_mode != s.text_mode {
            acts.push(SqlA::TextMode { tab, on: text_mode });
        }
        if let Some(set) = pick {
            acts.push(SqlA::PickSet { tab, set });
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
            let nshown = table::visible_cols(self.ui.qprefs.get(&tab).and_then(|m| m.get(&colnames)), &colnames, usize::MAX).len();
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
                        acts.push(SqlA::ClearChecks { tab });
                    }
                    match act {
                        Some(DocsAct::CopyAs(f)) => copy = Some((f, Some(s.qchecked.iter().copied().collect()))),
                        Some(DocsAct::Export) => export = Some((s.qchecked.iter().copied().collect(), false)),
                        _ => {}
                    }
                }
                // The record draws through `self`, so the results leave the tab while the grid draws, as in `results`.
                let qsel = s.qsel.filter(|&i| i < s.sets[sel].rows.len());
                let (sets, checked) = (s.sets.clone(), s.qchecked.clone());
                let prefs = self.ui.qprefs.get(&tab).and_then(|m| m.get(&colnames)).cloned().unwrap_or_default();
                // Shown columns first, in grid order, so Tab puts the record's cursor on the grid cursor's column.
                let names = if record_open { table::record_names(&table::visible_cols(Some(&prefs), &colnames, usize::MAX), &colnames) } else { Vec::new() };
                // Only a result read from one table has a table to filter. The lexer reads a short statement per frame.
                let filter = sets[sel].kind == "SELECT" && sets[sel].stmt.len() < 20_000 && db::sql::source::single_table(&sets[sel].stmt, dialect).is_some();
                let vim = self.core.prefs.vim && !record_open;
                let cursor = self.core.prefs.vim.then_some((self.vcol(), record_open));
                let scroll = std::mem::take(&mut self.ui.scroll_sel) | (vim && std::mem::take(&mut self.vnav.scroll));
                self.vnav.page = (ui.available_height() / ROW_H) as usize;
                let mut rec = crate::app::sql::record::RecordOut::default();
                let mut draw = |ui: &mut egui::Ui| self.sql_record(ui, Rows::Raw(&sets[sel].rows), qsel.unwrap_or(0), true, &names, &[], RowMenu::None, &checked, &mut rec);
                let out = grid(ui, &sets[sel], &prefs, &checked, qsel, scroll, cursor, filter, record_open.then_some(&mut draw as &mut dyn FnMut(&mut egui::Ui)));
                if !record_open {
                    self.vnav.cols = out.shown.len();
                }
                // The record walks every column, the grid only those shown.
                let shown = if record_open { &names } else { &out.shown };
                let at = if record_open { self.vline() } else { self.vcol() };
                let vim_act = self.vim_cell_act(ui.ctx(), qsel, at, shown, Rows::Raw(&sets[sel].rows));
                if let Some(c) = out.col {
                    self.set_vcol(c);
                }
                let Some(s) = self.core.tabs[cur].sql.as_ref() else { return };
                view = out.view.or(rec.view);
                head_acts = out.head;
                cell_act = out.cell.or(vim_act);
                if let Some(row) = out.select {
                    acts.push(SqlA::SelectRow { tab, row });
                }
                if let Some(on) = out.all {
                    acts.push(SqlA::CheckAll { tab, on });
                }
                if let Some((i, a)) = out.act {
                    let targets: Vec<usize> = if s.qchecked.contains(&i) { s.qchecked.iter().copied().collect() } else { vec![i] };
                    match a {
                        DocsAct::CopyAs(f) => copy = Some((f, Some(targets))),
                        DocsAct::Export => export = Some((targets, false)),
                        _ => {}
                    }
                }
                if let Some((row, range)) = out.check {
                    acts.push(SqlA::Check { tab, row, range });
                }
            }
        }
        let set_rows = |app: &App, idx: &Option<Vec<usize>>| -> Vec<Document> {
            let Some(s) = app.t().and_then(|t| t.sql.as_ref()) else { return Vec::new() };
            let rows = &s.sets[sel.min(s.sets.len().saturating_sub(1))].rows;
            rows.iter().enumerate().filter(|(i, _)| idx.as_ref().is_none_or(|v| v.contains(i))).map(|(_, r)| r.owned()).collect()
        };
        if let Some((c, v)) = view {
            self.sql_view_value(&c, &v);
        }
        if unsort {
            acts.push(SqlA::SortPage { tab, keys: Vec::new() });
        }
        if let Some(names) = show_cols {
            let p = self.ui.qprefs.entry(tab).or_default().entry(names.clone()).or_default();
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
            let cols: Vec<CopyCol> = self.core.tabs[cur].sql.as_ref().map(|s| s.sets[sel].cols.iter().map(|c| CopyCol { name: c.name.clone(), src: c.src.clone(), numeric: is_numeric(c.kind) }).collect()).unwrap_or_default();
            let table = self.insert_table(super::super::export::Src::Query);
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
        if let Some((set, page)) = goto {
            acts.push(SqlA::GotoPage { tab, set, page });
        }
        for a in acts {
            self.exec(Action::Sql(a));
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
            let lib = &mut self.core.setup().library;
            let id = lib.next_id();
            lib.scripts.push(crate::library::Script { id, ..saved("Late shipments", "SELECT o.id, o.created_at, s.carrier\nFROM orders o\nLEFT JOIN shipments s ON s.order_id = o.id\nWHERE o.status = 'paid' AND s.id IS NULL;") });
            let id = lib.next_id();
            lib.scripts.push(crate::library::Script { id, ..saved("Revenue by tier", "SELECT tier, SUM(total) AS revenue FROM orders GROUP BY tier ORDER BY revenue DESC;") });
            lib.recent = lib.scripts.iter().map(|s| crate::library::ScriptRef::Saved(s.id)).collect();
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
            (s.query, s.sets) = (query.into(), vec![first.into(), refused.into(), last.into()]);
            // QUARRY_TEST_CHECKED=1 shows three rows checked with the selection bar.
            if std::env::var("QUARRY_TEST_CHECKED").is_ok() {
                s.qchecked = [0, 2, 3].into();
            }
        }
        let core = self.core.setup();
        core.tabs.push(t);
        core.cur = core.tabs.len() - 1;
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
