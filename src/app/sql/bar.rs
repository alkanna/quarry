//! The Content query bar of a SQL tab (design F): a Rules / SQL switch, typed rules or WHERE text,
//! and the clause chips under it. The text in `Tab::filter_txt` and `Tab::sort_txt` stays the one
//! source of truth; rules, popovers and chips read and write it.

use crate::app::*;
use super::filter_cells::{col_pick, combo_w, mono_edit, pill_combo, rule_editor, type_name, Hints};
use crate::db::sql::{
    filter::{self, Match},
    rules::{self, Grain, Rule},
    value::Kind,
    Col, Dialect, };
use crate::ui::sql_complete::{open_list, sql_ac_field, Scope, Wants};
pub(crate) use quarry_core::state::sql::bar::*;
use super::filter_panel::{Panel, PanelFold};

const FUNCS: [&str; 5] = ["COUNT", "SUM", "AVG", "MIN", "MAX"];

/// A one line SQL input with highlighting and suggestions. Returns whether Enter ended the edit, which runs the query.
#[allow(clippy::too_many_arguments)]
fn sql_input(ui: &mut egui::Ui, id: egui::Id, text: &mut String, ac: &mut Ac, wants: &mut Wants, scope: &Scope, values: &Fields, hint: impl Into<egui::WidgetText>, w: f32) -> bool {
    let mono = FontId::monospace(TXT_SMALL);
    let (out, _) = sql_ac_field(ui, id, text, ac, scope, values, wants, |ui, text, _| egui::TextEdit::singleline(text).id(id).frame(editor_margin()).font(mono).hint_text(hint).desired_width(w).layouter(&mut sql_layout).show(ui));
    crate::ui::editor::paint_pair(ui, &out);
    out.response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
}

/// Shift-click on a header: the column becomes the last sort key, or flips direction when it already
/// is one. Text that is not plain keys (expressions) is left as it is.
pub(in crate::app) fn add_sort_key(text: &str, col: &str, d: Dialect) -> String {
    let Some(mut keys) = parse_order(text, Some(d)) else { return text.to_string() };
    match keys.iter_mut().find(|(c, _)| c == col) {
        Some(k) => k.1 = !k.1,
        None => keys.push((col.to_string(), false)),
    }
    order_text(&keys, d)
}

/// The names of the GROUP BY columns of a grouped result, as they head its columns. It only marks
/// headers, so it reads either quote: the tab does not know its dialect here.
pub(in crate::app) fn group_names(s: &SqlTab) -> Vec<String> {
    split_top(&s.bar.c.group).iter().filter_map(|g| plain_ident(g, None)).collect()
}

pub(in crate::app) fn order_text(keys: &[(String, bool)], d: Dialect) -> String {
    keys.iter().map(|(c, desc)| format!("{} {}", d.quote_ident(c), if *desc { "DESC" } else { "ASC" })).collect::<Vec<_>>().join(", ")
}

/// SQL text colored by token: keywords, numbers and strings. Plugs into `TextEdit::layouter`.
pub(in crate::app) fn sql_layout(ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32) -> Arc<egui::Galley> {
    crate::ui::editor::memo_galley(ui, 0, buf.as_str(), wrap, || sql_job(buf.as_str(), TXT_SMALL))
}

/// A round "+" button, painted because the glyph sits off center.
fn plus_named(ui: &mut egui::Ui, enabled: bool, name: &str) -> egui::Response {
    round_button(ui, enabled, name, |p, c, color| paint_plus(p, c, 4.0, color))
}

fn round_button(ui: &mut egui::Ui, enabled: bool, name: &str, glyph: impl FnOnce(&egui::Painter, egui::Pos2, Color32)) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(BUTTON_H + 4.0, BUTTON_H), Sense::click());
    let state = if enabled { state_of(&r) } else { 0 };
    ui.painter().rect(rect, rect.height() / 2.0, hover_fill(state), Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
    let color = if !enabled { FAINT() } else if state > 0 { BRIGHT() } else { MUTED() };
    glyph(ui.painter(), rect.center(), color);
    focus_ring(ui, &r, rect, rect.height() / 2.0);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, name));
    r.on_hover_cursor(if enabled { egui::CursorIcon::PointingHand } else { egui::CursorIcon::Default })
}

/// A clause pill that opens a popover: keyword, summary and a cross. Returns (response, cleared).
fn clause_chip(ui: &mut egui::Ui, name: &str, summary: &str, open: bool) -> (egui::Response, bool) {
    let kw = chip_name_color(name);
    let gn = ui.painter().layout_no_wrap(name.to_string(), FontId::monospace(TXT_CAPTION), kw);
    let gs = ui.painter().layout_no_wrap(summary.to_string(), FontId::monospace(TXT_CAPTION), TEXT());
    let w = 10.0 + gn.size().x + 8.0 + gs.size().x + 6.0 + CLOSE_W + 3.0;
    let (rect, r) = ui.allocate_exact_size(egui::vec2(w, BUTTON_H), Sense::click());
    let state = state_of(&r);
    let edge = if open { PRIMARY() } else if state > 0 { BORDER() } else { LINE() };
    ui.painter().rect(rect, BUTTON_H / 2.0, if state == 2 { PRESSED() } else { PANEL() }, Stroke::new(1.0, edge), egui::StrokeKind::Inside);
    let (dn, ds) = (optical_dy(&gn), optical_dy(&gs));
    let nw = gn.size().x;
    ui.painter().galley(egui::pos2(rect.left() + 10.0, rect.center().y - gn.size().y / 2.0 + dn), gn, kw);
    ui.painter().galley(egui::pos2(rect.left() + 18.0 + nw, rect.center().y - gs.size().y / 2.0 + ds), gs, TEXT());
    focus_ring(ui, &r, rect, BUTTON_H / 2.0);
    let close = egui::Rect::from_center_size(egui::pos2(rect.right() - CLOSE_W / 2.0 - 3.0, rect.center().y), egui::vec2(CLOSE_W, CLOSE_W));
    // A child ui, not a scope: a scope would also claim room in the row and push the chips out of line.
    let mut cross = ui.new_child(egui::UiBuilder::new().max_rect(close));
    let x = close_x(&mut cross).on_hover_text(format!("Remove {name}")).clicked();
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{name} clause")));
    (r.on_hover_cursor(egui::CursorIcon::PointingHand), x)
}

/// The popover for ORDER BY: one row per key. Returns whether the text changed.
fn order_popover(ui: &mut egui::Ui, text: &mut String, cols: &[Col], d: Dialect, mut pick_text: Option<String>) -> bool {
    menu_look(ui);
    ui.set_min_width(320.0);
    ui.add_space(4.0);
    caption(ui, "ORDER BY");
    ui.add_space(4.0);
    let Some(mut keys) = parse_order(text, Some(d)) else {
        // Free text (expressions) cannot be shown as keys.
        return crate::ui::editor::show_paired(ui, mono_edit(text, 300.0, "created_at DESC").layouter(&mut sql_layout)).response.changed();
    };
    let mut changed = false;
    let mut remove = None;
    for (i, (col, desc)) in keys.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            if let Some(p) = col_pick(ui, ui.next_auto_id(), col, cols, 170.0, pick_text.take()) {
                *col = p;
                changed = true;
            }
            let before = *desc;
            segmented(ui, desc, &[(false, "ASC"), (true, "DESC")]);
            changed |= before != *desc;
            if close_x(ui).on_hover_text("Remove key").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        keys.remove(i);
        changed = true;
    }
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        if quiet(ui, RichText::new("+ Sort key").color(PRIMARY())).clicked() && let Some(c) = cols.iter().find(|c| !keys.iter().any(|(k, _)| *k == c.name)) {
            keys.push((c.name.clone(), false));
            changed = true;
        }
        ui.label(RichText::new("Shift-click a column header adds one too").size(TXT_SMALL).color(FAINT()));
    });
    if changed {
        *text = order_text(&keys, d);
    }
    changed
}

/// The popover for the SELECT list: a checklist of columns and expressions.
fn columns_popover(ui: &mut egui::Ui, select: &mut String, cols: &[Col], d: Dialect) -> bool {
    menu_look(ui);
    ui.set_min_width(300.0);
    ui.add_space(4.0);
    caption(ui, "COLUMNS");
    ui.label(RichText::new("The SELECT list. Unchecked columns are not fetched.").size(TXT_SMALL).color(FAINT()));
    ui.add_space(4.0);
    let items = split_top(select);
    let all = items.is_empty() || items.iter().any(|i| i == "*");
    let on = |c: &Col| all || items.iter().any(|i| plain_ident(i, Some(d)).is_some_and(|n| n == c.name));
    let mut checked: Vec<bool> = cols.iter().map(on).collect();
    // The expression fields keep their own buffers, since text typed before Enter is not in `select` yet.
    let ex_id = ui.scope_id().with("exprs");
    let (mut exprs, seen) = ui.data(|d| d.get_temp::<(Vec<String>, String)>(ex_id)).unwrap_or_default();
    if seen != *select {
        exprs = if all { Vec::new() } else { items.iter().filter(|i| plain_ident(i, Some(d)).is_none_or(|n| !cols.iter().any(|c| c.name == n))).cloned().collect() };
    }
    let (mut changed, mut enter) = (false, false);
    let filter_id = ui.scope_id().with("col-filter");
    let mut filter = ui.data(|d| d.get_temp::<String>(filter_id)).unwrap_or_default();
    ui.add(mono_edit(&mut filter, 300.0, "Filter columns"));
    ui.add_space(2.0);
    egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
        for (i, c) in cols.iter().enumerate() {
            if !filter.is_empty() && !c.name.to_lowercase().contains(&filter.to_lowercase()) {
                continue;
            }
            ui.horizontal(|ui| {
                let was = checked[i];
                ui.checkbox(&mut checked[i], RichText::new(&c.name).font(FontId::monospace(TXT_SMALL)).color(if was { TEXT() } else { FAINT() }));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(RichText::new(type_name(c)).font(FontId::monospace(TXT_CAPTION)).color(FAINT())));
            });
        }
        let mut remove = None;
        for (i, e) in exprs.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                let r = crate::ui::editor::show_paired(ui, mono_edit(e, 250.0, "total * 1.2 AS gross").layouter(&mut sql_layout)).response;
                enter |= r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if close_x(ui).on_hover_text("Remove expression").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            exprs.remove(i);
            changed = true;
        }
    });
    ui.data_mut(|d| d.insert_temp(filter_id, filter));
    ui.horizontal(|ui| {
        if quiet(ui, RichText::new("+ Expression").color(PRIMARY())).clicked() {
            exprs.push(String::new());
        }
        if quiet(ui, "Show all").clicked() {
            checked.iter_mut().for_each(|c| *c = true);
            changed = true;
        }
    });
    let was: Vec<bool> = cols.iter().map(on).collect();
    changed |= was != checked && checked.iter().any(|c| *c);
    if changed || enter {
        // Nothing checked would select nothing, so the last column stays.
        if !checked.iter().any(|c| *c) {
            checked = was;
        }
        *select = if checked.iter().all(|c| *c) && exprs.iter().all(|e| e.trim().is_empty()) {
            String::new()
        } else {
            cols.iter().zip(&checked).filter(|(_, c)| **c).map(|(c, _)| d.quote_ident(&c.name)).chain(exprs.iter().filter(|e| !e.trim().is_empty()).cloned()).collect::<Vec<_>>().join(", ")
        };
    }
    ui.data_mut(|d| d.insert_temp(ex_id, (exprs, select.clone())));
    changed || enter
}

/// The GROUP BY editor. Returns true when Group was pressed (and the texts were written).
fn group_popover(ui: &mut egui::Ui, g: &mut GroupDlg, c: &mut Clauses, cols: &[Col], d: Dialect, table: &str, rows: u64) -> bool {
    menu_look(ui);
    ui.set_width(560.0);
    ui.add_space(4.0);
    ui.label(semibold(RichText::new("Group rows").size(TXT_HEADING - 2.0)).color(BRIGHT()));
    ui.label(RichText::new(format!("{table} · {} rows match the filter", group(&rows.to_string()))).size(TXT_SMALL).color(FAINT()));
    ui.add_space(6.0);
    caption(ui, "GROUP BY");
    ui.horizontal_wrapped(|ui| {
        let mut remove = None;
        for (i, (col, grain)) in g.keys.iter_mut().enumerate() {
            let is_date = cols.iter().find(|c| c.name == *col).is_some_and(|c| rules::group_of(c.kind) == rules::Group::Date);
            let label = match grain {
                Some(gr) if is_date => format!("{col} by {}", gr.name()),
                _ => col.clone(),
            };
            let w = combo_w(ui, &label, "", 60.0);
            let r = pill_combo(ui, &label, "", w, if grain.is_some() { PRIMARY() } else { TEXT() });
            egui::Popup::menu(&r).width(160.0).show(|ui| {
                menu_look(ui);
                if is_date {
                    if menu_row(ui, "no grain", TEXT(), grain.is_none(), "").clicked() {
                        *grain = None;
                        ui.close();
                    }
                    for (gr, name) in Grain::ALL {
                        if menu_row(ui, &format!("by {name}"), TEXT(), *grain == Some(gr), "").clicked() {
                            *grain = Some(gr);
                            ui.close();
                        }
                    }
                    ui.separator();
                }
                if menu_row(ui, "Remove", ALERT(), false, "").clicked() {
                    remove = Some(i);
                    ui.close();
                }
            });
        }
        if let Some(i) = remove {
            g.keys.remove(i);
        }
        if let Some(p) = col_pick(ui, ui.next_auto_id(), "", cols, 90.0, None) {
            // A date column starts grouped by day, since raw timestamps never repeat.
            let date = cols.iter().find(|c| c.name == p).is_some_and(|c| rules::group_of(c.kind) == rules::Group::Date);
            g.keys.push((p, date.then_some(Grain::Day)));
        }
    });
    ui.label(RichText::new("Dates group by hour, day, week, month or year.").size(TXT_SMALL).color(FAINT()));
    ui.add_space(6.0);
    caption(ui, "VALUES");
    let mut remove = None;
    for (i, a) in g.aggs.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            let r = pill_combo(ui, FUNCS[a.func], "", 86.0, C_KEYWORD());
            egui::Popup::menu(&r).width(120.0).show(|ui| {
                menu_look(ui);
                for (k, f) in FUNCS.iter().enumerate() {
                    if menu_row(ui, f, C_KEYWORD(), a.func == k, "").clicked() {
                        a.func = k;
                        ui.close();
                    }
                }
            });
            let ty = cols.iter().find(|c| c.name == a.col).map(type_name).unwrap_or_default();
            let (main, color) = if a.col.is_empty() { ("* rows", FAINT()) } else { (a.col.as_str(), TEXT()) };
            let w = combo_w(ui, main, &ty, 150.0);
            let r = pill_combo(ui, main, &ty, w, color);
            egui::Popup::menu(&r).width(220.0).show(|ui| {
                menu_look(ui);
                if a.func == 0 && menu_row(ui, "* rows", TEXT(), a.col.is_empty(), "").clicked() {
                    a.col.clear();
                    ui.close();
                }
                for c in cols {
                    if menu_row(ui, &c.name, TEXT(), a.col == c.name, &type_name(c)).clicked() {
                        a.col = c.name.clone();
                        ui.close();
                    }
                }
            });
            ui.label(RichText::new("AS").font(FontId::monospace(TXT_CAPTION)).color(C_KEYWORD()));
            ui.add(mono_edit(&mut a.alias, 120.0, "alias"));
            if close_x(ui).on_hover_text("Remove value").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        g.aggs.remove(i);
    }
    if quiet(ui, RichText::new("+ Value").color(PRIMARY())).clicked() {
        g.aggs.push(Agg { func: 0, col: String::new(), alias: if g.aggs.is_empty() { "rows".into() } else { String::new() } });
    }
    ui.add_space(6.0);
    caption(ui, "HAVING");
    let aliases: Vec<Col> = g.aggs.iter().filter(|a| !a.alias.trim().is_empty()).map(|a| Col::plain(a.alias.trim(), "DECIMAL", Kind::Decimal, true)).collect();
    let bs = g.bs;
    ui.horizontal(|ui| {
        if g.having.is_none() {
            if quiet(ui, RichText::new("+ Condition").color(PRIMARY())).clicked() && let Some(a) = aliases.first() {
                g.having = Some(Rule::new(&a.name, Kind::Decimal, d));
            }
        } else if let Some(r) = g.having.as_mut() {
            if !aliases.iter().any(|a| a.name == r.col) {
                r.col = aliases.first().map(|a| a.name.clone()).unwrap_or_default();
            }
            rule_editor(ui, r, &aliases, ui.scope_id().with("having"), d, bs, None, false, &mut Hints::default());
            if close_x(ui).on_hover_text("Remove condition").clicked() {
                g.having = None;
            }
        }
    });
    ui.add_space(6.0);
    let (sel, grp) = group_texts(g, d);
    let hav = g.having.as_ref().filter(|r| r.complete()).map(|r| rules::rule_sql(r, d, g.bs)).unwrap_or_default();
    let mut job = sql_job(&format!("SELECT {sel}\n  GROUP BY {grp}{}", if hav.is_empty() { String::new() } else { format!(" HAVING {hav}") }), TXT_SMALL);
    job.wrap.max_width = 520.0;
    egui::Frame::new().fill(BG()).corner_radius(R_SM).inner_margin(8).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(job);
    });
    ui.add_space(6.0);
    let mut done = false;
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let ok = !g.keys.is_empty();
            if ui.add_enabled(ok, primary_button(ui, "Group", PRIMARY())).clicked() {
                c.select = sel.clone();
                c.group = grp.clone();
                c.having = hav.clone();
                done = true;
                ui.close();
            }
            if quiet(ui, "Cancel").clicked() {
                ui.close();
            }
        });
    });
    done
}

/// The select list and GROUP BY list the editor writes: a date key becomes `DATE(col) AS day`.
fn group_texts(g: &GroupDlg, d: Dialect) -> (String, String) {
    let (mut sel, mut grp) = (Vec::new(), Vec::new());
    for (col, grain) in &g.keys {
        match grain {
            Some(gr) => {
                let alias = gr.name().to_string();
                sel.push(format!("{} AS {}", gr.expr(col, d), d.quote_ident(&alias)));
                grp.push(d.quote_ident(&alias));
            }
            None => {
                sel.push(d.quote_ident(col));
                grp.push(d.quote_ident(col));
            }
        }
    }
    for a in &g.aggs {
        let alias = if a.alias.trim().is_empty() { if a.col.is_empty() { "rows".to_string() } else { format!("{}_{}", FUNCS[a.func].to_lowercase(), a.col) } } else { a.alias.trim().to_string() };
        let arg = if a.col.is_empty() { "*".to_string() } else { d.quote_ident(&a.col) };
        sel.push(format!("{}({arg}) AS {}", FUNCS[a.func], d.quote_ident(&alias)));
    }
    (sel.join(", "), grp.join(", "))
}

/// What the bar shows beside Run: the table's filter history and its saved filters, and what was picked in them.
pub(in crate::app) struct Side<'a> {
    pub(in crate::app) hist: &'a QueryHist,
    pub(in crate::app) pick: &'a crate::app::library::picker::PickData,
    pub(in crate::app) qacts: Vec<QAct>,
    pub(in crate::app) pick_acts: Vec<crate::app::library::picker::PickAct>,
    /// The ? button was clicked.
    pub(in crate::app) help: bool,
    /// A dialog or window is over the page, so the panel leaves Esc and Cmd+Enter to it.
    pub(in crate::app) covered: bool,
    /// What is known of the table's columns for the value cells, by column name.
    pub(in crate::app) facts: Option<&'a HashMap<String, db::sql::facts::ColFacts>>,
    /// The mode switch and the filter panel's structural edits, run once the bar is drawn.
    pub(in crate::app) acts: Vec<quarry_core::action::SqlA>,
    /// The Save as view menu item was clicked; `sql_content` opens the dialog once the bar is drawn.
    pub(in crate::app) save_view: bool,
    /// The Explain button was clicked; `sql_content` runs the same entry point as Cmd+E.
    pub(in crate::app) explain: bool,
    /// The panel's Save as filter was clicked; `sql_content` opens the Save dialog.
    pub(in crate::app) save_filter: bool,
    /// What the frame changed in the bar's texts, clauses, tree and panel, sent before the other actions.
    pub(in crate::app) edit: Option<BarEdit>,
}

/// A row of the "+" menu: a condition or a clause. `on` marks the ones already in the query.
pub(in crate::app) struct AddItem {
    pub(in crate::app) label: &'static str,
    pub(in crate::app) note: &'static str,
    pub(in crate::app) group: &'static str,
    pub(in crate::app) on: bool,
    /// SQL clause names read as keywords; the Mongo option names stay plain.
    pub(in crate::app) keyword: bool,
}

/// The one "+" at the end of a query line.
pub(in crate::app) fn add_button(ui: &mut egui::Ui) -> egui::Response {
    plus_named(ui, true, "Add a condition or clause")
}

/// The searchable menu of the "+": what can be added to the query, with a check on what is in it, and Query help last. Returns
/// the label picked. It stays open while the search is typed in; each row closes it.
pub(in crate::app) fn add_menu(plus: &egui::Response, items: &[AddItem], help: &mut bool) -> Option<&'static str> {
    let needle_id = plus.id.with("add-needle");
    let field = needle_id.with("field");
    let mut pick = None;
    egui::Popup::menu(plus).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).width(250.0).show(|ui| {
        menu_list(ui);
        let mut needle: String = ui.data(|d| d.get_temp(needle_id)).unwrap_or_default();
        well_field(ui, field, FieldIcon::Search, None, |ui| ui.add(field_edit(&mut needle, field, "Add a condition or clause", ui.available_width(), egui::TextStyle::Body)));
        // Typing starts at once; nothing else in the menu takes keyboard focus until it is clicked.
        if ui.memory(|m| m.focused().is_none()) {
            ui.memory_mut(|m| m.request_focus(field));
        }
        ui.add_space(4.0);
        let q = needle.trim().to_lowercase();
        let mut group = "";
        let mut picks = Vec::new();
        for it in items.iter().filter(|i| q.is_empty() || i.label.to_lowercase().contains(&q)) {
            if it.group != group {
                group = it.group;
                caption(ui, group);
            }
            menu_act(ui, &mut picks, it.label, |ui| add_item(ui, it, needle.trim()));
        }
        popover_divider(ui);
        let mut helps = Vec::new();
        menu_act(ui, &mut helps, (), |ui| ui.button(RichText::new("Query help").color(MUTED())));
        *help |= !helps.is_empty();
        pick = picks.pop();
        if pick.is_some() {
            needle.clear();
        }
        ui.data_mut(|d| d.insert_temp(needle_id, needle));
    });
    pick
}

/// A row of the "+" menu: the label with the typed part in the accent, its note or a check on the right. Named by its bare label,
/// so the note does not become part of the name.
fn add_item(ui: &mut egui::Ui, it: &AddItem, hl: &str) -> egui::Response {
    let color = if it.keyword { C_KEYWORD() } else { TEXT() };
    let fmt = |c: Color32| egui::TextFormat::simple(FontId::monospace(TXT_SMALL), c);
    let mut job = LayoutJob::default();
    match find_ci(it.label, hl) {
        Some((i, j)) => {
            job.append(&it.label[..i], 0.0, fmt(color));
            job.append(&it.label[i..j], 0.0, fmt(PRIMARY()));
            job.append(&it.label[j..], 0.0, fmt(color));
        }
        None => job.append(it.label, 0.0, fmt(color)),
    }
    let mut b = egui::Button::new(job);
    if !it.note.is_empty() {
        b = b.right_text(RichText::new(it.note).font(FontId::monospace(TXT_CAPTION)).color(FAINT()));
    }
    let r = ui.add(b);
    if it.on {
        // Painted, since the check glyph comes from a fallback font and sits off center.
        paint_check(ui.painter(), egui::pos2(r.rect.right() - 14.0, r.rect.center().y), PRIMARY());
    }
    let (enabled, on, label) = (ui.is_enabled(), it.on, it.label);
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::MenuItem, enabled, on, label));
    r
}

/// A row of a Run menu, named by its bare label so the shortcut hint does not become part of the name.
pub(in crate::app) fn run_item(ui: &mut egui::Ui, label: &str, keys: &str) -> egui::Response {
    let mut b = egui::Button::new(label);
    if !keys.is_empty() {
        b = b.right_text(RichText::new(keys).size(TXT_CAPTION).color(FAINT()));
    }
    let r = ui.add(b);
    let enabled = ui.is_enabled();
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::MenuItem, enabled, label));
    r
}

/// Gap between the items of a query line.
pub(in crate::app) const LINE_GAP: egui::Vec2 = egui::vec2(8.0, 6.0);

/// The frame of a query line, `left` and `right` from its content, and whether its main field `fid` has the focus. The line
/// reads the field's focus and hover from the last frame for its outline: alert on `err`, the accent while edited or focused,
/// muted on hover.
pub(in crate::app) fn line_frame(ui: &egui::Ui, fid: egui::Id, err: bool, dirty: bool, left: i8, right: i8) -> (egui::Frame, bool) {
    let focused = ui.memory(|m| m.has_focus(fid));
    let hovered = ui.ctx().read_response(fid).is_some_and(|r| r.hovered());
    let edge = if err { ALERT() } else if dirty || focused { PRIMARY() } else if hovered { MUTED() } else { BORDER() };
    (egui::Frame::new().fill(PANEL()).stroke(Stroke::new(1.0, edge)).corner_radius(R_BAR).inner_margin(egui::Margin { left, right, top: 7, bottom: 7 }), focused)
}

/// The focus ring around a query line whose field has the focus.
pub(in crate::app) fn line_ring(ui: &egui::Ui, rect: egui::Rect) {
    ui.painter().rect_stroke(rect.expand(1.0), R_BAR + 1, Stroke::new(2.0, tint(PRIMARY(), 60)), egui::StrokeKind::Outside);
}

/// Run and its tip on a query line, and the menu the tip opens: Run, a divider, then what `more` adds. True on Run.
pub(in crate::app) fn line_run(ui: &mut egui::Ui, dirty: bool, more: impl FnOnce(&mut egui::Ui)) -> bool {
    let (main, tip) = run_split(ui, "Run", "↵", dirty, "Run options");
    let mut run = main.on_hover_text("Enter").clicked();
    egui::Popup::menu(&tip).width(220.0).show(|ui| {
        menu_list(ui);
        if run_item(ui, "Run", "↵").clicked() {
            run = true;
            ui.close();
        }
        popover_divider(ui);
        more(ui);
    });
    run
}

/// The note under a query line whose text is not what the rows below came from. Returns true on its Run link.
pub(in crate::app) fn stale_note(ui: &mut egui::Ui) -> bool {
    let mut run = false;
    egui::Frame::new().fill(tint(WARNING(), 14)).corner_radius(R_MD).inner_margin(egui::Margin { left: 12, right: 12, top: 5, bottom: 5 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new("Showing the results of the previous query.").size(TXT_SMALL).color(WARNING()));
            run = quiet(ui, RichText::new("Run ↵").size(TXT_SMALL).color(PRIMARY())).clicked();
        });
    });
    run
}

/// What only drawing the bar needs, kept per tab by uid in `App.ui`, with the copies of the clauses, tree and panel a frame edits.
/// The frame copies them from Core and sends what differs as one `BarEdit`.
#[derive(Default)]
pub(in crate::app) struct BarUi {
    pub(in crate::app) c: Clauses,
    pub(in crate::app) tree: filter::Tree,
    pub(in crate::app) panel: Panel,
    /// The open suggestion list of whichever bar input has it, and what the inputs asked the app to look up.
    pub(in crate::app) ac: Ac,
    pub(in crate::app) wants: Wants,
    /// A column whose facts a value cell asked for, and its kind; `sql_content` sends the request.
    pub(in crate::app) want_facts: Option<(String, Kind)>,
    /// The clause whose popover the "+" opened, because a clause with no chip has nothing else to hang it on.
    adding: Option<Add>,
    gdlg: GroupDlg,
    fold_open: bool,
    /// Whether the WHERE text read as a tree, for the text, the column pairs and the mode it was read with.
    parsable: Option<(String, Arc<Vec<(String, Kind)>>, bool, bool)>,
    /// What the demo opened the bar with, copied from Core once; the picker's text is used once.
    preset: Preset,
    init: bool,
}

/// Draws the query line and what hangs under it. Returns true when the query should run. Core has read the text as rules
/// already (`SqlA::BarSync`); what the frame edits goes to `side.edit`.
#[allow(clippy::too_many_arguments)]
pub(in crate::app) fn bar(ui: &mut egui::Ui, t: &Tab, b: &mut BarUi, d: Dialect, bs: bool, mut scope: Scope, values: &Fields, side: &mut Side) -> bool {
    let Some(s) = t.sql.as_ref() else { return false };
    let Some(memo) = s.bar.memo.as_ref() else { return false };
    let matched = s.matched.unwrap_or(0);
    let cols = if s.bar.table_cols.is_empty() { s.cols.clone() } else { s.bar.table_cols.clone() };
    // The WHERE text has no FROM, so completion reads its columns from the table of the tab. Until the schema is loaded the result's own columns serve.
    let table = t.sel.coll.clone();
    scope.table = Some(table.clone());
    scope.fallback = memo.fallback.clone();
    let pairs = memo.pairs.clone();
    let (rows, tab) = (t.count, t.uid);
    if !b.init {
        b.init = true;
        b.preset = Preset { ac: s.bar.preset.ac, order: s.bar.preset.order, pick: s.bar.preset.pick.clone() };
    }
    (b.c, b.tree, b.panel) = (s.bar.c.clone(), s.bar.tree.clone(), s.bar.panel.clone());
    let (mut filter, mut sort) = (t.filter_txt.clone(), t.sort_txt.clone());
    let mut run = false;
    b.gdlg.bs = bs;
    let dirty = s.bar.dirty;
    let (mut copy, mut save_view) = (false, false);
    let mut written = false;
    let size = ui.available_width();
    let mono = FontId::monospace(TXT_SMALL);
    let fid = query_id("SQL-WHERE");
    let (frame, focused) = line_frame(ui, fid, false, dirty, 8, 8);
    let frame = frame.show(ui, |ui| {
        ui.set_width(size - 18.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing = LINE_GAP;
            // The edited note, the history and library icons, Run with its tip, and the chip of a bound script when there is one.
            // Drawn first, in a child over the first row that claims no room, so the conditions get the width it leaves.
            let row = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), BUTTON_H));
            let mut right = ui.new_child(egui::UiBuilder::new().max_rect(row).layout(egui::Layout::right_to_left(egui::Align::Min)));
            right.scope(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                run |= line_run(ui, dirty, |ui| {
                    if run_item(ui, "Explain", "⌘E").on_hover_text("Explain the SELECT of the rows shown in a new Query tab").clicked() {
                        side.explain = true;
                        ui.close();
                    }
                    if run_item(ui, "Copy as SQL", "").on_hover_text("Copy the whole SELECT").clicked() {
                        copy = true;
                        ui.close();
                    }
                    if run_item(ui, "Save as view…", "").on_hover_text("Create a view from the SELECT of the rows shown").clicked() {
                        save_view = true;
                        ui.close();
                    }
                });
                vsep(ui, 6.0);
                picker_button(ui, side.pick, true, &mut side.pick_acts);
                history_menu(ui, side.hist, &[], Some(d), &mut side.qacts);
                edited_note(ui, dirty);
            });
            let right_w = right.min_rect().width();
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width() - right_w - 16.0, BUTTON_H + 2.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing = LINE_GAP;
                // Rules mode needs text this module wrote; anything else stays in SQL mode.
                let can_rules = pairs.is_empty()
                    || match &b.parsable {
                        Some((text, p, bs0, ok)) if *text == filter && Arc::ptr_eq(p, &pairs) && *bs0 == bs => *ok,
                        _ => {
                            let ok = filter::parse(&filter, &pairs, d, bs).is_some();
                            b.parsable = Some((filter.clone(), pairs.clone(), bs, ok));
                            ok
                        }
                    };
                let mut sql = b.c.sql;
                let r0 = ui.cursor().min;
                segmented(ui, &mut sql, &[(false, "Rules"), (true, "SQL")]);
                let seg = egui::Rect::from_min_size(r0, egui::vec2(ui.cursor().min.x - r0.x, BUTTON_H));
                if b.c.sql && !can_rules {
                    ui.interact(seg, ui.scope_id().with("mode-why"), Sense::hover()).on_hover_text("Rules mode reads column comparisons joined by AND or OR, in groups of any depth. This WHERE has something else (a function, a subquery, or AND mixed with OR without parentheses), so it stays SQL.");
                }
                if sql != b.c.sql && (sql || can_rules) {
                    side.acts.push(quarry_core::action::SqlA::WhereMode { tab, sql });
                }
                ui.add_sized([0.0, BUTTON_H], egui::Label::new(RichText::new("WHERE").font(mono.clone()).color(C_KEYWORD())));
                // The conditions and the clauses in use share rows that wrap under the first condition, and the "+" ends the last.
                ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), BUTTON_H + 2.0), egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true), |ui| {
                    ui.spacing_mut().item_spacing = LINE_GAP;
                    if b.c.sql {
                        // Leaves room for the field's side margins, the gap and the "+" that ends the row, or the "+" wraps under the text.
                        let w = (ui.max_rect().right() - ui.cursor().left() - (BUTTON_H + 4.0 + LINE_GAP.x + 8.0)).max(160.0);
                        if b.preset.ac {
                            open_list(ui, fid, &filter, &mut b.ac, &scope, values);
                        }
                        run |= sql_input(ui, fid, &mut filter, &mut b.ac, &mut b.wants, &scope, values, RichText::new("status = 'paid' AND total > 50").color(FAINT()), w);
                    } else {
                        let f = super::filter_line::filters_button(ui, &b.tree, b.panel.open);
                        if f.clicked() {
                            b.panel.toggle(ui.ctx());
                            // Space and Enter belong to the panel's rows, not to the button they would press again.
                            ui.memory_mut(|m| m.surrender_focus(f.id));
                        }
                        // ponytail: the summary gets a fixed share of the line, so the clause chips after it keep theirs.
                        let room = (ui.available_width() * 0.55).max(120.0);
                        if let Some(at) = super::filter_line::summary(ui, &b.tree, room) {
                            b.panel.show(ui.ctx(), Some(at));
                        }
                    }
                    run |= clause_chips(ui, &mut sort, b, d, &cols, &scope, values, &table, matched, rows, &mut side.help);
                });
            });
        });
    });
    if focused {
        line_ring(ui, frame.response.rect);
    }
    if !b.c.sql {
        let mut hints = Hints { table: &table, facts: side.facts, values: Some(values), ..Default::default() };
        let out = super::filter_panel::panel(ui, &mut b.tree, &mut b.panel, &cols, d, bs, !side.covered, &mut hints);
        if let Some(f) = hints.want_facts.take() {
            b.want_facts = Some(f);
        }
        if let Some(column) = hints.want_values.take() {
            b.wants.values = Some(crate::ui::sql_complete::Need { table: table.clone(), column, prefix: None });
        }
        written = out.changed;
        side.acts.extend(out.acts.into_iter().map(|a| quarry_core::action::SqlA::Filter { tab, a }));
        if out.as_sql {
            side.acts.push(quarry_core::action::SqlA::WhereMode { tab, sql: true });
            ui.memory_mut(|m| m.request_focus(fid));
        }
        side.save_filter |= out.save;
        run |= out.run;
    }
    fold_line(ui, t, b, &filter, &sort, d);
    if dirty && !b.panel.open {
        ui.add_space(8.0);
        run |= stale_note(ui);
    }
    ui.add_space(8.0);
    if copy {
        ui.ctx().copy_text(format!("{};", select_sql(t, &filter, &sort, &b.c, d)));
    }
    side.save_view = save_view;
    let sb = &s.bar;
    let view = |p: &Panel| PanelView { open: p.open, sel: p.sel, pick: p.pick, ops: p.ops, hover: p.hover };
    let edit = BarEdit {
        filter: (filter != t.filter_txt).then_some(filter),
        sort: (sort != t.sort_txt).then_some(sort),
        clauses: (b.c != sb.c).then(|| b.c.clone()),
        tree: (b.tree != sb.tree).then(|| b.tree.clone()),
        written,
        panel: (view(&b.panel) != view(&sb.panel)).then(|| view(&b.panel)),
    };
    side.edit = Some(edit).filter(|e| !e.is_empty());
    run
}

/// The SELECT of the bar as it reads now.
fn select_sql(t: &Tab, filter: &str, sort: &str, c: &Clauses, d: Dialect) -> String {
    query_of(filter, sort, c.clone()).select_sql(d, t.sel.space(), &t.sel.coll, t.sql.as_ref().map_or(&[][..], |s| s.pk.as_slice()), None)
}

/// The clauses in use as chips, then the "+" that adds one. Each chip is in a scope of its own so its popover keeps its place when
/// the chips before it come and go.
#[allow(clippy::too_many_arguments)]
fn clause_chips(ui: &mut egui::Ui, sort: &mut String, b: &mut BarUi, d: Dialect, cols: &[Col], scope: &Scope, values: &Fields, table: &str, matched: u64, rows: u64, help: &mut bool) -> bool {
    let mut run = false;
    let sql = b.c.sql;
    let mono = FontId::monospace(TXT_SMALL);
    let grouped = !b.c.group.trim().is_empty();
    // A clause shows as a chip while it is set or its editor has focus, which is how the "+" menu starts an empty one.
    let shown = |ui: &egui::Ui, id: egui::Id, text: &str| !text.is_empty() || ui.memory(|m| m.has_focus(id));
    let fit = |ui: &egui::Ui, text: &str, hint: &str| (text_w(ui, if text.is_empty() { hint } else { text }, mono.clone()) + 8.0).clamp(24.0, 420.0);
    if sql {
        let id = query_id("SQL-ORDER");
        if shown(ui, id, sort) {
            let tw = fit(ui, sort, "created_at DESC");
            run |= opt_chip(ui, "ORDER BY", id, sort, |ui, text| sql_input(ui, id, text, &mut b.ac, &mut b.wants, scope, values, "created_at DESC", tw));
        }
        let id = query_id("SQL-GROUP");
        if shown(ui, id, &b.c.group) {
            let tw = fit(ui, &b.c.group, "status, currency");
            run |= opt_chip(ui, "GROUP BY", id, &mut b.c.group, |ui, text| sql_input(ui, id, text, &mut b.ac, &mut b.wants, scope, values, "status, currency", tw));
        }
        let id = query_id("SQL-HAVING");
        if grouped && shown(ui, id, &b.c.having) {
            let tw = fit(ui, &b.c.having, "orders > 10");
            run |= opt_chip(ui, "HAVING", id, &mut b.c.having, |ui, text| sql_input(ui, id, text, &mut b.ac, &mut b.wants, scope, values, "orders > 10", tw));
        }
    } else {
        if !sort.is_empty() {
            let summary = match parse_order(sort, Some(d)) {
                Some(k) => k.iter().map(|(c, d)| format!("{c} {}", if *d { "DESC" } else { "ASC" })).collect::<Vec<_>>().join(", "),
                None => sort.clone(),
            };
            run |= flow_item(ui, "clause-order", |ui| {
                let mut run = false;
                let (r, cleared) = clause_chip(ui, "ORDER BY", &summary, false);
                popover(&r, 340.0, b.preset.order, |ui| {
                    if order_popover(ui, sort, cols, d, b.preset.pick.take()) {
                        run = true;
                    }
                });
                if cleared {
                    sort.clear();
                    run = true;
                }
                run
            });
        }
        if grouped {
            run |= flow_item(ui, "clause-group", |ui| {
                let mut run = false;
                let (r, cleared) = clause_chip(ui, "GROUP BY", &b.c.group, false);
                let mut applied = false;
                popover(&r, 580.0, false, |ui| applied = group_popover(ui, &mut b.gdlg, &mut b.c, cols, d, table, matched.max(rows)));
                run |= applied;
                if cleared {
                    b.c.group.clear();
                    b.c.having.clear();
                    b.c.select.clear();
                    run = true;
                }
                run
            });
            // A unit of its own, so it can wrap to the next row apart from GROUP BY.
            if !b.c.having.trim().is_empty() {
                run |= flow_item(ui, "clause-having", |ui| {
                    let mut applied = false;
                    let (r, cleared) = clause_chip(ui, "HAVING", &b.c.having.replace('`', ""), false);
                    popover(&r, 580.0, false, |ui| applied = group_popover(ui, &mut b.gdlg, &mut b.c, cols, d, table, matched.max(rows)));
                    if cleared {
                        b.c.having.clear();
                        applied = true;
                    }
                    applied
                });
            }
        }
    }
    let id = query_id("SQL-LIMIT");
    if shown(ui, id, &b.c.limit) {
        run |= opt_chip(ui, "LIMIT", id, &mut b.c.limit, |ui, text| {
            let r = ui.add(egui::TextEdit::singleline(text).id(id).frame(editor_margin()).font(mono.clone()).hint_text("100").desired_width(44.0));
            r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
        });
    }
    // Columns: a checklist in Rules mode, the select list text in SQL mode. Grouped results write their own.
    if !grouped {
        if sql {
            let id = query_id("SQL-COLUMNS");
            if shown(ui, id, &b.c.select) {
                let tw = fit(ui, &b.c.select, "id, status, total");
                run |= opt_chip(ui, "Columns", id, &mut b.c.select, |ui, text| sql_input(ui, id, text, &mut b.ac, &mut b.wants, scope, values, "id, status, total", tw));
            }
        } else {
            let n = split_top(&b.c.select).len();
            if n > 0 {
                run |= flow_item(ui, "clause-columns", |ui| {
                    let mut run = false;
                    let (r, cleared) = clause_chip(ui, "Columns", &format!("{n} of {}", cols.len()), false);
                    popover(&r, 320.0, false, |ui| run |= columns_popover(ui, &mut b.c.select, cols, d));
                    if cleared {
                        b.c.select.clear();
                        run = true;
                    }
                    run
                });
            }
        }
    }
    // DISTINCT, a toggle
    if b.c.distinct {
        let (r, cleared) = clause_chip(ui, "DISTINCT", "", false);
        if cleared || r.clicked() {
            b.c.distinct = false;
            run = true;
        }
    }
    let id = query_id("SQL-OFFSET");
    if shown(ui, id, &b.c.offset) {
        run |= opt_chip(ui, "OFFSET", id, &mut b.c.offset, |ui, text| {
            let r = ui.add(egui::TextEdit::singleline(text).id(id).frame(editor_margin()).font(mono.clone()).hint_text("0").desired_width(44.0));
            r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
        });
    }
    // ponytail: the index names are free text; list them from SHOW INDEX when someone asks for a picker.
    let hints = d.supports(db::sql::Feature::IndexHint);
    let id = query_id("SQL-INDEX");
    if hints && shown(ui, id, &b.c.hint) {
        run |= opt_chip(ui, "USE INDEX", id, &mut b.c.hint, |ui, text| {
            let r = ui.add(egui::TextEdit::singleline(text).id(id).frame(editor_margin()).font(mono.clone()).hint_text("index_name").desired_width(90.0));
            r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
        });
    }
    let plus = add_button(ui);
    let item = |label, note, group, on, keyword| AddItem { label, note, group, on, keyword };
    let word = match b.tree.root.mode {
        Match::All => "AND",
        Match::Any => "OR",
        Match::None => "NOT",
    };
    let mut items = vec![item("Column condition", word, "CONDITION", false, false), item("Group of conditions", "( )", "CONDITION", false, false), item("ORDER BY", "", "CLAUSES", !sort.is_empty(), true), item("LIMIT", "", "CLAUSES", !b.c.limit.is_empty(), true)];
    if !grouped {
        items.push(item("Columns", "", "CLAUSES", !b.c.select.is_empty(), false));
    }
    items.push(item("GROUP BY", "", "CLAUSES", grouped, true));
    if grouped {
        items.push(item("HAVING", "", "CLAUSES", !b.c.having.trim().is_empty(), true));
    }
    items.push(item("DISTINCT", "", "CLAUSES", b.c.distinct, true));
    items.push(item("OFFSET", "", "CLAUSES", !b.c.offset.is_empty(), true));
    if hints {
        items.push(item("USE INDEX", "", "CLAUSES", !b.c.hint.is_empty(), true));
    }
    let focus = |ui: &egui::Ui, id: &str| ui.memory_mut(|m| m.request_focus(query_id(id)));
    let popover_id = plus.id.with("clause-popover");
    match add_menu(&plus, &items, help) {
        Some("Column condition" | "Group of conditions") if sql => focus(ui, "SQL-WHERE"),
        Some(p @ ("Column condition" | "Group of conditions")) => {
            quarry_core::action::sql::filter::add(&mut b.tree, &mut b.panel, 0, p == "Group of conditions", cols, d);
            let at = b.panel.sel;
            b.panel.show(ui.ctx(), at);
        }
        Some("ORDER BY") if sql => focus(ui, "SQL-ORDER"),
        Some("ORDER BY") if sort.is_empty() => {
            if let Some(c) = cols.first() {
                *sort = order_text(&[(c.name.clone(), false)], d);
                run = true;
            }
        }
        Some("ORDER BY") => {
            b.adding = Some(Add::Order);
            ui.data_mut(|d| d.insert_temp(popover_id, true));
        }
        Some("LIMIT") => focus(ui, "SQL-LIMIT"),
        Some("Columns") if sql => focus(ui, "SQL-COLUMNS"),
        Some("Columns") => {
            b.adding = Some(Add::Columns);
            ui.data_mut(|d| d.insert_temp(popover_id, true));
        }
        Some("GROUP BY") if sql => focus(ui, "SQL-GROUP"),
        Some("HAVING") if sql => focus(ui, "SQL-HAVING"),
        Some("GROUP BY" | "HAVING") => {
            b.adding = Some(Add::Group);
            ui.data_mut(|d| d.insert_temp(popover_id, true));
        }
        Some("DISTINCT") => {
            b.c.distinct = !b.c.distinct;
            run = true;
        }
        Some("OFFSET") => focus(ui, "SQL-OFFSET"),
        Some("USE INDEX") => focus(ui, "SQL-INDEX"),
        _ => {}
    }
    // A Rules mode clause with no chip yet opens its popover on the "+", and is done once it closes.
    if let Some(a) = b.adding {
        let out = match a {
            Add::Order => popover(&plus, 340.0, true, |ui| order_popover(ui, sort, cols, d, None)),
            Add::Columns => popover(&plus, 320.0, true, |ui| columns_popover(ui, &mut b.c.select, cols, d)),
            Add::Group => popover(&plus, 580.0, true, |ui| group_popover(ui, &mut b.gdlg, &mut b.c, cols, d, table, rows)),
        };
        match out {
            Some(applied) => {
                if applied && a == Add::Group {
                    b.adding = None;
                }
                run |= applied;
            }
            None => b.adding = None,
        }
    }
    run
}

/// The full SELECT, folded under the bar in SQL mode.
fn fold_line(ui: &mut egui::Ui, t: &Tab, b: &mut BarUi, filter: &str, sort: &str, d: Dialect) {
    if !b.c.sql {
        return;
    }
    let stmt = select_sql(t, filter, sort, &b.c, d);
    let uid = t.uid;
    ui.add_space(8.0);
    // The folded line as a job, kept per tab with the statement text and the fold state it was built from.
    let memo = egui::Id::unique(("sql-fold", uid));
    let mut job = match ui.data(|d| d.get_temp::<(String, bool, LayoutJob)>(memo)) {
        Some((key, open, job)) if open == b.fold_open && key == stmt => job,
        _ => {
            let text = if b.fold_open { stmt.clone() } else { stmt.split_whitespace().collect::<Vec<_>>().join(" ") };
            let job = sql_job(&text, TXT_SMALL);
            ui.data_mut(|d| d.insert_temp(memo, (stmt.clone(), b.fold_open, job.clone())));
            job
        }
    };
    job.wrap.max_width = ui.available_width() - 46.0;
    if !b.fold_open {
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
    }
    let g = ui.fonts_mut(|f| f.layout_job(job));
    let h = (g.size().y + 14.0).max(30.0);
    let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), Sense::click());
    let state = state_of(&r);
    ui.painter().rect(rect, R_MD, if state > 0 { HOVER() } else { PANEL() }, Stroke::new(1.0, LINE()), egui::StrokeKind::Inside);
    let c = egui::pos2(rect.left() + 16.0, rect.top() + 15.0);
    chevron(ui.painter(), c, b.fold_open, MUTED());
    ui.painter().galley(egui::pos2(rect.left() + 30.0, rect.top() + 7.0), g, TEXT());
    focus_ring(ui, &r, rect, R_MD as f32);
    if r.on_hover_text(if b.fold_open { "Fold the statement" } else { "Show the whole statement" }).clicked() {
        b.fold_open = !b.fold_open;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_click_appends_or_flips_a_sort_key() {
        let d = Dialect::MySql;
        assert_eq!(add_sort_key("", "a", d), "`a` ASC");
        assert_eq!(add_sort_key("`a` ASC", "b", d), "`a` ASC, `b` ASC");
        // A key already there keeps its place and flips.
        assert_eq!(add_sort_key("`a` ASC, `b` DESC", "a", d), "`a` DESC, `b` DESC");
        assert_eq!(add_sort_key("a, b DESC", "b", d), "`a` ASC, `b` ASC");
        // Expressions are not rewritten.
        assert_eq!(add_sort_key("LOWER(a)", "b", d), "LOWER(a)");
        let pg = Dialect::Postgres;
        assert_eq!(add_sort_key("\"a\" ASC", "b", pg), "\"a\" ASC, \"b\" ASC");
        assert_eq!(add_sort_key("\"a\" ASC, \"b\" DESC", "a", pg), "\"a\" DESC, \"b\" DESC");
        // An unquoted name is the folded one on PostgreSQL.
        assert_eq!(add_sort_key("Total DESC", "total", pg), "\"total\" ASC");
    }

    #[test]
    fn plain_ident_reads_back_what_quote_ident_writes() {
        for d in [Dialect::MySql, Dialect::MariaDb, Dialect::Postgres] {
            for name in ["status", "Order Date", "a`b", "say \"hi\"", "a.b"] {
                assert_eq!(plain_ident(&d.quote_ident(name), Some(d)).as_deref(), Some(name), "{d:?} {name}");
                assert_eq!(plain_ident(&d.quote_ident(name), None).as_deref(), Some(name), "{d:?} {name}");
            }
        }
        // On MySQL a double-quoted word is a string, and on PostgreSQL a backtick is not a quote.
        assert_eq!(plain_ident("\"a\"", Some(Dialect::MySql)), None);
        assert_eq!(plain_ident("`a`", Some(Dialect::Postgres)), None);
        // Several names or an expression between the outer quotes.
        assert_eq!(plain_ident("\"a\" || \"b\"", Some(Dialect::Postgres)), None);
        assert_eq!(plain_ident("`a`.`b`", Some(Dialect::MySql)), None);
        assert_eq!(plain_ident("\"\"", Some(Dialect::Postgres)), None);
        assert_eq!(plain_ident("Status", Some(Dialect::Postgres)).as_deref(), Some("status"));
        assert_eq!(plain_ident("Status", Some(Dialect::MySql)).as_deref(), Some("Status"));
        assert_eq!(plain_ident("COUNT(*)", None), None);
    }

    #[test]
    fn the_tree_never_overwrites_a_where_text_it_was_not_read_from() {
        let d = Dialect::MySql;
        let pairs = [("id".to_string(), Kind::Int)];
        // A tab opened before its columns: the text stays until they arrive, then reads as a tree.
        let mut b = Bar::default();
        let mut text = "id = 5".to_string();
        read_tree(&mut b, &text, &[], d, true, false);
        write_tree(&mut b, &mut text, &[], d, true);
        assert_eq!(text, "id = 5");
        read_tree(&mut b, &text, &pairs, d, true, false);
        write_tree(&mut b, &mut text, &pairs, d, true);
        assert_eq!(text, "`id` = 5");
        // Edited in SQL mode, then back to Rules in the same frame: the new text is read, not the old tree written.
        b.c.sql = true;
        text = "`id` = 7".into();
        b.c.sql = false;
        b.src.clear();
        read_tree(&mut b, &text, &pairs, d, true, true);
        write_tree(&mut b, &mut text, &pairs, d, true);
        assert_eq!(text, "`id` = 7");
        // Emptied in SQL mode: the empty text equals the cleared src, and is still read.
        b.c.sql = true;
        text.clear();
        b.c.sql = false;
        b.src.clear();
        read_tree(&mut b, &text, &pairs, d, true, true);
        write_tree(&mut b, &mut text, &pairs, d, true);
        assert_eq!(text, "");
        assert!(b.tree.root.kids.is_empty());
    }

    #[test]
    fn off_rows_survive_sql_mode_and_a_restored_tab() {
        let d = Dialect::MySql;
        let pairs = [("id".to_string(), Kind::Int), ("n".to_string(), Kind::Int)];
        let mut b = Bar::default();
        let mut text = "`id` = 5 AND `n` = 1".to_string();
        read_tree(&mut b, &text, &pairs, d, true, false);
        let off = b.tree.rows_in_order()[1];
        b.tree.toggle(off);
        write_tree(&mut b, &mut text, &pairs, d, true);
        assert_eq!(text, "`id` = 5");
        // To SQL mode and back with the text unchanged keeps the off row.
        b.c.sql = true;
        b.c.sql = false;
        b.src.clear();
        read_tree(&mut b, &text, &pairs, d, true, true);
        assert_eq!(b.tree.rows_in_order().len(), 2);
        // A restored tab: the saved tree comes back once the columns are known, while it writes the same text.
        let json = filter::to_json(&b.tree);
        let mut r = Bar { pending: json.clone(), ..Default::default() };
        read_tree(&mut r, &text, &[], d, true, false);
        assert_eq!(r.pending, json, "waits for the columns");
        read_tree(&mut r, &text, &pairs, d, true, false);
        assert_eq!(r.tree.rows_in_order().len(), 2);
        // Every row off: the WHERE is empty, and the saved tree still comes back.
        let mut all = b.tree.clone();
        all.toggle(all.rows_in_order()[0]);
        let mut r = Bar { pending: filter::to_json(&all), ..Default::default() };
        read_tree(&mut r, "", &pairs, d, true, false);
        assert_eq!(r.tree.rows_in_order().len(), 2);
        // A text edited since the save is read afresh.
        let mut r = Bar { pending: json, ..Default::default() };
        read_tree(&mut r, "`id` = 6", &pairs, d, true, false);
        assert_eq!(r.tree.rows_in_order().len(), 1);
    }

    #[test]
    fn binary_groups_drill_with_a_hex_literal() {
        assert_eq!(bytes_lit(&[0x0a, 0xff], Dialect::MySql, true, true), "0x0AFF");
        assert_eq!(bytes_lit(&[0x0a, 0xff], Dialect::MySql, true, false), "X'0AFF'");
        assert_eq!(bytes_lit(&[], Dialect::MySql, true, true), "''");
        assert_eq!(bytes_lit(&[0x0a, 0xff], Dialect::Postgres, false, true), "E'\\\\x0aff'");
        assert_eq!(bytes_lit(&[], Dialect::Postgres, false, true), "E'\\\\x'");
        // The 0x form reads back as a rule on a binary column and writes out the same.
        let pairs = [("blob".to_string(), Kind::Binary)];
        let text = format!("`blob` = {}", bytes_lit(&[0x0a, 0xff], Dialect::MySql, true, true));
        let t = filter::parse(&text, &pairs, Dialect::MySql, true).unwrap();
        assert_eq!(filter::to_sql(&t, Dialect::MySql, true), text);
    }
}
