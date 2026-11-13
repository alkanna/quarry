//! The selected SQL row as a record, one line per column, under the row in the grid. Tab opens and closes it, Esc closes
//! it. Its values edit, open in full and link like the grid's cells, and JSON values fold out as a tree.

use crate::app::sql::view::read_menu;
use crate::app::table::{fk_link, view_button, FK_W};
use crate::app::*;
use crate::ui::json_tree;
use db::sql::structure::ForeignKey;
use db::sql::value::Kind;
use db::sql::Col;

/// What the record asked for while it was drawn. Edits and cell actions are done by `sql_after` once the page is back in
/// the tab, like the grid's own.
#[derive(Default)]
pub(in crate::app) struct RecordOut {
    pub(in crate::app) edit: Option<String>,
    pub(in crate::app) acts: Vec<(String, CellAct)>,
    pub(in crate::app) view: Option<(String, Bson)>,
    pub(in crate::app) row_act: Option<(usize, RowAct)>,
    pub(in crate::app) fk: Option<usize>,
}

/// One parse cache per column, not per row, so moving through rows does not pile up parsed values.
fn tree_id(col: &str) -> egui::Id {
    egui::Id::unique(("sql-record-json", col))
}

impl App {
    /// Row `i` of `docs` as a record. `agg` rows are a query's answer, which is only read: a double click opens the value.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn sql_record(&mut self, ui: &mut egui::Ui, docs: Rows, i: usize, agg: bool, names: &[String], fks: &[ForeignKey], menu: RowMenu, checked: &BTreeSet<usize>, out: &mut RecordOut) {
        let drafted = self.shown_row(docs, i, agg);
        let staged = if drafted.is_some() { self.sql_staged_cols(docs, i) } else { Vec::new() };
        // A query's answer is not the open table, so a column of the same name tells nothing about its type.
        let table_cols = if agg { None } else { self.t().and_then(|t| t.sql.as_ref()).map(|s| s.cols.clone()) };
        let cols: Vec<Option<&Col>> = names.iter().map(|c| table_cols.as_ref().and_then(|t| t.iter().find(|k| &k.name == c))).collect();
        let font = FontId::monospace(TXT_SMALL);
        let small = FontId::monospace(TXT_CAPTION);
        // Monospaced, so the widest text is the longest one, measured once per font.
        let width = |f: &FontId, chars: usize| chars as f32 * ui.fonts_mut(|x| x.glyph_width(f, '0'));
        let name_w = (width(&font, names.iter().map(|n| n.chars().count()).max().unwrap_or(0)) + 24.0).clamp(110.0, 280.0);
        let type_w = (width(&small, cols.iter().flatten().map(|c| c.ty.chars().count()).max().unwrap_or(0)) + 16.0).clamp(40.0, 160.0);
        let rid = ui.scope_id().with(("sql-record", i));
        let key = docs.id(i);
        for (c, col) in names.iter().zip(&cols) {
            let (line, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::hover());
            // Lines scrolled out of view keep their space but skip the work, like the grid's rows.
            if !ui.is_rect_visible(line) {
                continue;
            }
            let kind = col.as_ref().map_or(Kind::Text, |c| c.kind);
            let val = egui::Rect::from_min_max(egui::pos2(line.left() + name_w, line.top()), egui::pos2(line.right() - type_w, line.bottom()));
            if !agg && self.sql_cell.as_ref().is_some_and(|e| e.is_at(key.clone(), c)) {
                self.sql_cell_editor(ui, val);
                continue;
            }
            let v = match &drafted {
                Some(d) => d.get(c).map(Val::B),
                None => docs.value(i, c),
            };
            // Editing is the grid's: a column it refuses, or a query's answer, opens the value to read instead.
            let blocked = agg || self.sql_edit_block().or_else(|| self.sql_col_block(c)).is_some();
            let r = ui.interact(val, rid.with(("v", c)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
            r.widget_info(|| {
                let mut info = egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{c} record"));
                info.current_text_value = v.map(|v| cell_text(&v.bson()));
                info
            });
            let p = ui.painter();
            p.rect_filled(line, R_SM, hover_fill(state_of(&r)));
            focus_ring(ui, &r, val, R_SM as f32);
            let label = |text: String, f: &FontId, color: Color32| p.layout_no_wrap(text, f.clone(), color);
            let g = label(c.clone(), &font, MUTED());
            p.with_clip_rect(egui::Rect::from_min_size(line.min, egui::vec2(name_w - 8.0, line.height()))).galley(egui::pos2(line.left() + 6.0, line.center().y - g.size().y / 2.0 + optical_dy(&g)), g, MUTED());
            if let Some(col) = col {
                let g = label(col.ty.to_lowercase(), &small, FAINT());
                p.galley(egui::pos2(line.right() - 6.0 - g.size().x, line.center().y - g.size().y / 2.0 + optical_dy(&g)), g, FAINT());
            }
            if staged.iter().any(|s| s == c) {
                p.rect_filled(val, 0, tint(WARNING(), 34));
                p.rect_filled(egui::Rect::from_min_size(val.min, egui::vec2(2.0, val.height())), 0, WARNING());
            }
            let text = v.and_then(Val::as_str);
            let json = text.is_some_and(|s| kind == Kind::Json || json_tree::looks_like(s));
            let open_id = rid.with(("json", c));
            let mut open = json && ui.data(|d| d.get_temp(open_id)).unwrap_or(false);
            let fk = v.filter(|v| !v.is_null()).and_then(|_| fks.iter().position(|f| col.as_ref().is_some_and(|k| f.cols.contains(&k.src))));
            let text_x = val.left() + if json { 20.0 } else { 6.0 };
            let clip = p.with_clip_rect(val);
            match v {
                Some(v) => {
                    let mut job = sql_cell_job(v, kind, true);
                    job.wrap = egui::text::TextWrapping::truncate_at_width(val.right() - text_x - 6.0 - if fk.is_some() { FK_W } else { 0.0 });
                    let g = ui.fonts_mut(|f| f.layout_job(job));
                    clip.galley(egui::pos2(text_x, val.center().y - g.size().y / 2.0), g, TEXT());
                }
                None => {
                    clip.text(egui::pos2(text_x, val.center().y), egui::Align2::LEFT_CENTER, "—", font.clone(), FAINT());
                }
            }
            let r = r.on_hover_text(if blocked { "Double-click to view, right-click for more" } else { "Double-click to edit, right-click for more" });
            if r.double_clicked() {
                if blocked {
                    out.view = Some((c.to_string(), Val::owned(v)));
                } else {
                    out.edit = Some(c.to_string());
                }
            }
            r.context_menu(|ui| {
                menu_look(ui);
                if agg {
                    if read_menu(ui, v) {
                        out.view = Some((c.to_string(), Val::owned(v)));
                    }
                } else if !checked.contains(&i) {
                    if let Some(a) = sql_cell_menu(ui, col.is_some_and(|c| c.nullable), !blocked) {
                        out.acts.push((c.to_string(), a));
                        ui.close();
                    }
                }
                if menu != RowMenu::None {
                    ui.separator();
                    out.row_act = any_row_menu(ui, menu, i, docs, checked).or(out.row_act.take());
                }
            });
            if json {
                let tr = ui.interact(egui::Rect::from_min_size(val.min, egui::vec2(20.0, val.height())), rid.with(("fold", c)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
                tr.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{c} tree")));
                let state = state_of(&tr);
                chevron(ui.painter(), egui::pos2(val.left() + 10.0, val.center().y), open, if state > 0 { BRIGHT() } else { MUTED() });
                focus_ring(ui, &tr, tr.rect, R_SM as f32);
                if tr.on_hover_text(if open { "Hide the tree" } else { "Show as a tree" }).clicked() {
                    open = !open;
                    ui.data_mut(|d| d.insert_temp(open_id, open));
                    if !open {
                        json_tree::forget(ui.ctx(), tree_id(c));
                    }
                }
            }
            if let Some(k) = fk && fk_link(ui, val, rid.with(("fk", c)), &fks[k]).clicked() {
                out.fk = Some(k);
            }
            if let Some(b) = view_button(ui, val, if fk.is_some() { FK_W } else { 0.0 }, v, ("sql-record-eye", i, c.as_str()), || format!("View {c} record")) {
                out.view = Some((c.to_string(), b));
            }
            if let (true, Some(s)) = (open, text) {
                ui.horizontal(|ui| {
                    ui.add_space(name_w);
                    ui.vertical(|ui| json_tree::show_text(ui, tree_id(c), s));
                });
            }
        }
    }

    /// Tab on a selected SQL row opens or closes its record. Read before anything draws: egui has already turned the Tab
    /// into a focus move, which would give the focus to the first button of the top bar.
    pub(in crate::app) fn record_key(&mut self, ctx: &egui::Context) {
        // The record belongs to a selected row: whatever let the row go (a view switch, a reload, a delete) ends it too.
        if let Some(t) = self.tm().filter(|t| t.record && !t.record_row()) {
            t.record = false;
        }
        if self.window_open() || self.sql_cell.is_some() || ctx.memory(|m| m.focused().is_some()) || ctx.any_popup_open() {
            return;
        }
        let Some(t) = self.tm().filter(|t| t.sql.is_some() && t.record_row()) else { return };
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Tab)) {
            t.record = !t.record;
            ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
        }
    }
}
