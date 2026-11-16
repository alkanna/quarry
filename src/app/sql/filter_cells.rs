//! The cells of a filter condition: the column, operator and value pickers that the filter panel's rows and the HAVING
//! editor share.

use crate::app::*;
use crate::db::sql::{
    rules::{self, Input, Op, Rule},
    value::Kind,
    Col, Dialect,
};

pub(super) fn type_name(c: &Col) -> String {
    c.ty.split('(').next().unwrap_or(&c.ty).split_whitespace().next().unwrap_or("").to_ascii_uppercase()
}

/// An input-looking button that opens a menu: `main` in mono, `sub` faint (the column type), a chevron.
pub(super) fn pill_combo(ui: &mut egui::Ui, main: &str, sub: &str, w: f32, main_color: Color32) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(w, BUTTON_H), Sense::click());
    let state = state_of(&r);
    let fill = [BG(), HOVER(), PRESSED()][state];
    let edge = if state > 0 { MUTED() } else { BORDER() };
    ui.painter().rect(rect, R_SM, fill, Stroke::new(1.0, edge), egui::StrokeKind::Inside);
    let g = ui.painter().layout_no_wrap(main.to_string(), FontId::monospace(TXT_SMALL), main_color);
    let dy = optical_dy(&g);
    let mut x = rect.left() + 8.0;
    ui.painter().galley(egui::pos2(x, rect.center().y - g.size().y / 2.0 + dy), g.clone(), main_color);
    x += g.size().x + 6.0;
    if !sub.is_empty() {
        let s = ui.painter().layout_no_wrap(sub.to_string(), FontId::monospace(TXT_CAPTION), FAINT());
        let dy = optical_dy(&s);
        ui.painter().galley(egui::pos2(x, rect.center().y - s.size().y / 2.0 + dy), s, FAINT());
    }
    let c = egui::pos2(rect.right() - 11.0, rect.center().y);
    ui.painter().add(egui::Shape::convex_polygon(vec![c + egui::vec2(-3.5, -2.0), c + egui::vec2(3.5, -2.0), c + egui::vec2(0.0, 2.5)], if state > 0 { BRIGHT() } else { MUTED() }, Stroke::NONE));
    focus_ring(ui, &r, rect, R_SM as f32);
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Width of a pill combo for its texts.
pub(super) fn combo_w(ui: &egui::Ui, main: &str, sub: &str, min: f32) -> f32 {
    (text_w(ui, main, FontId::monospace(TXT_SMALL)) + if sub.is_empty() { 0.0 } else { text_w(ui, sub, FontId::monospace(TXT_CAPTION)) + 6.0 } + 34.0).max(min)
}

/// Indices of the columns that hold `q`, names starting with it first, each group in table order. An
/// empty `q` keeps every column.
pub(super) fn col_filter(cols: &[Col], q: &str) -> Vec<usize> {
    let q = q.to_ascii_lowercase();
    let (mut starts, mut inside) = (Vec::new(), Vec::new());
    for (i, c) in cols.iter().enumerate() {
        match c.name.to_ascii_lowercase().find(&q) {
            Some(0) => starts.push(i),
            Some(_) => inside.push(i),
            None => {}
        }
    }
    starts.extend(inside);
    starts
}

/// What a column picker holds while its field is being typed in.
#[derive(Clone, Default)]
struct ColBox {
    text: String,
    sel: usize,
    /// Where the list was drawn last frame, so a press on it is not read as leaving the field.
    list: Option<egui::Rect>,
    /// Frames shown, so the first ones can win the focus they asked for before losing it counts.
    age: u8,
}

/// A searchable column picker: a click turns the pill into a text field in place and typing filters
/// the list. `id` keeps its open state with its row when rows move. Returns the picked name.
pub(super) fn col_pick(ui: &mut egui::Ui, id: egui::Id, cur: &str, cols: &[Col], min: f32, start_text: Option<String>) -> Option<String> {
    let ty = cols.iter().find(|c| c.name == cur).map(type_name).unwrap_or_default();
    let shown = if cur.is_empty() { "column" } else { cur };
    let w = combo_w(ui, shown, &ty, min);
    let key = id.with("col-box");
    let edit_id = key.with("edit");
    let Some(mut st) = ui.data(|d| d.get_temp::<ColBox>(key)) else {
        let r = pill_combo(ui, shown, &ty, w, if cur.is_empty() { FAINT() } else { TEXT() });
        if r.clicked() || start_text.is_some() {
            ui.data_mut(|d| d.insert_temp(key, ColBox { text: start_text.unwrap_or_default(), ..Default::default() }));
            ui.memory_mut(|m| m.request_focus(edit_id));
            ui.ctx().request_repaint();
        }
        return None;
    };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, BUTTON_H), Sense::hover());
    ui.painter().rect(rect, R_SM, BG(), Stroke::new(1.0, PRIMARY()), egui::StrokeKind::Inside);
    let (down, up, enter, esc) = ui.input_mut(|i| (i.consume_key(egui::Modifiers::NONE, Key::ArrowDown), i.consume_key(egui::Modifiers::NONE, Key::ArrowUp), i.consume_key(egui::Modifiers::NONE, Key::Enter), i.consume_key(egui::Modifiers::NONE, Key::Escape)));
    let tab = ui.input(|i| i.key_pressed(Key::Tab));
    if st.age < 3 {
        st.age += 1;
        ui.memory_mut(|m| m.request_focus(edit_id));
    }
    let settled = st.age >= 3;
    let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
    let te = inner.add(egui::TextEdit::singleline(&mut st.text).id(edit_id).frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 0))).font(FontId::monospace(TXT_SMALL)).hint_text(RichText::new(shown).color(FAINT())).align(egui::Align2::LEFT_CENTER).desired_width(w));
    let hits = col_filter(cols, &st.text);
    if te.changed() {
        st.sel = 0;
    }
    if down {
        st.sel += 1;
    }
    if up {
        st.sel = st.sel.saturating_sub(1);
    }
    st.sel = st.sel.min(hits.len().saturating_sub(1));
    let mut pick = None;
    // The list is cheap to draw whole: rows off screen are skipped before any text is laid out.
    let width = {
        let longest = cols.iter().map(|c| text_w(ui, &c.name, FontId::monospace(TXT_SMALL)) + text_w(ui, &type_name(c), FontId::monospace(TXT_CAPTION))).fold(0.0, f32::max);
        (longest + 60.0).clamp(220.0, 420.0)
    };
    let mut open = true;
    let list = egui::Popup::new(key.with("list"), ui.ctx().clone(), rect, ui.layer_id()).kind(egui::PopupKind::Menu).layout(egui::Layout::top_down_justified(egui::Align::Min)).style(egui::containers::menu::menu_style).gap(0.0).open_bool(&mut open).close_behavior(egui::PopupCloseBehavior::IgnoreClicks).width(width).show(|ui| {
        menu_look(ui);
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            if hits.is_empty() {
                ui.label(RichText::new("No matching column").size(TXT_SMALL).color(FAINT()));
            }
            for (n, &i) in hits.iter().enumerate() {
                let c = &cols[i];
                let r = menu_row_in(ui, &c.name, TEXT(), c.name == cur, &type_name(c), &st.text, n == st.sel);
                if (down || up) && n == st.sel {
                    ui.scroll_to_rect(r.rect, None);
                }
                if r.clicked() {
                    pick = Some(c.name.clone());
                }
            }
        });
    });
    let in_list = list.as_ref().is_some_and(|l| ui.ctx().input(|i| i.pointer.interact_pos()).is_some_and(|p| l.response.rect.contains(p)) || Some(l.response.rect) == st.list);
    if let Some(l) = &list {
        st.list = Some(l.response.rect);
    }
    let keyed = enter || (tab && te.lost_focus());
    if keyed {
        pick = pick.or_else(|| hits.get(st.sel).map(|&i| cols[i].name.clone()));
    }
    // Leaving the field any other way, such as a click on the page, restores the previous value.
    let gone = esc || keyed || pick.is_some() || (settled && !te.has_focus() && !ui.ctx().input(|i| i.pointer.any_down() && i.pointer.interact_pos().is_some_and(|p| st.list.is_some_and(|l| l.contains(p)))) && !in_list);
    if gone {
        ui.memory_mut(|m| m.surrender_focus(edit_id));
        ui.data_mut(|d| d.remove::<ColBox>(key));
    } else {
        ui.data_mut(|d| d.insert_temp(key, st));
    }
    pick
}

/// The menu group of an operator, to draw a rule between groups like the design.
pub(super) fn op_cat(op: Op) -> u8 {
    use Op::*;
    match op {
        Eq | Ne | Gt | Lt | Ge | Le | Distinct => 0,
        Contains | NotContains | StartsWith | NotStartsWith | EndsWith | NotEndsWith => 1,
        Like | NotLike | Regexp | NotRegexp => 2,
        In | NotIn | NotInOrNull | Between => 3,
        Empty | NotEmpty => 4,
        IsNull | NotNull => 5,
        LenEq | LenGt | LenLt | ArrayLenEq | ArrayLenGt | ArrayLenLt => 6,
        Today | Yesterday | Last | ThisWeek | ThisMonth | LastMonth => 7,
        AfterCol => 8,
        SetContains => 9,
        JsonPathEq | JsonPathNe | JsonPathGt | JsonPathLt | JsonContains | JsonHasPath | JsonHasKey => 10,
        ArrayHasAll | ArrayHasAny => 11,
    }
}

/// The operator picker for a column type.
pub(super) fn op_pick(ui: &mut egui::Ui, kind: Kind, cur: Op, min: f32, d: Dialect) -> Option<Op> {
    let label = rules::label(cur, kind);
    let w = combo_w(ui, label, "", min);
    let r = pill_combo(ui, label, "", w, if rules::is_keyword(cur) { C_KEYWORD() } else { TEXT() });
    let mut pick = None;
    egui::Popup::menu(&r).width(190.0).show(|ui| {
        menu_look(ui);
        let mut last = None;
        for &op in rules::ops_for(kind, d) {
            if last.is_some_and(|l| l != op_cat(op)) {
                ui.separator();
            }
            last = Some(op_cat(op));
            if menu_row(ui, rules::label(op, kind), if rules::is_keyword(op) { C_KEYWORD() } else { TEXT() }, op == cur, "").clicked() {
                pick = Some(op);
                ui.close();
            }
        }
    });
    pick
}


/// A removable value chip of an IN list.
pub(super) fn value_chip(ui: &mut egui::Ui, text: &str, kind: Kind) -> bool {
    let color = if rules::group_of(kind) == rules::Group::Number { C_NUMBER() } else { C_STRING() };
    let g = ui.painter().layout_no_wrap(text.to_string(), FontId::monospace(TXT_SMALL), color);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(g.size().x + 12.0 + CLOSE_W + 2.0, BUTTON_H - 4.0), Sense::hover());
    ui.painter().rect_filled(rect, rect.height() / 2.0, tint(color, 26));
    let dy = optical_dy(&g);
    ui.painter().galley(egui::pos2(rect.left() + 9.0, rect.center().y - g.size().y / 2.0 + dy), g, color);
    let close = egui::Rect::from_center_size(egui::pos2(rect.right() - CLOSE_W / 2.0 - 2.0, rect.center().y), egui::vec2(CLOSE_W, CLOSE_W));
    ui.scope_builder(egui::UiBuilder::new().max_rect(close), |ui| close_x(ui).on_hover_text("Remove value").clicked()).inner
}

pub(super) fn mono_edit<'a>(text: &'a mut String, w: f32, hint: &str) -> egui::TextEdit<'a> {
    egui::TextEdit::singleline(text).font(FontId::monospace(TXT_SMALL)).hint_text(RichText::new(hint).color(FAINT())).desired_width(w).margin(egui::Margin::symmetric(7, 4))
}

/// The value editors of a rule. Returns (changed, Enter pressed).
pub(super) fn value_inputs(ui: &mut egui::Ui, r: &mut Rule, cols: &[Col], salt: egui::Id) -> (bool, bool) {
    let (mut changed, mut enter) = (false, false);
    let need = match rules::input(r.op) {
        Input::None => 0,
        Input::One | Input::List | Input::Column => 1,
        Input::Two | Input::Last | Input::Path => 2,
    };
    if r.values.len() < need {
        r.values.resize(need, String::new());
    }
    let date = rules::group_of(r.kind) == rules::Group::Date;
    match rules::input(r.op) {
        Input::None => {
            ui.label(RichText::new("No value").size(TXT_SMALL).color(FAINT()));
        }
        Input::One => {
            let hint = if date { "2026-11-01 00:00" } else if rules::group_of(r.kind) == rules::Group::Number { "number" } else { "value" };
            let resp = ui.add(mono_edit(&mut r.values[0], 170.0, hint).id(salt.with("v0")));
            changed |= resp.changed();
            enter |= resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        }
        Input::Two => {
            let a = ui.add(mono_edit(&mut r.values[0], 90.0, "from").id(salt.with("v0")));
            ui.label(RichText::new("and").size(TXT_SMALL).color(FAINT()));
            let b = ui.add(mono_edit(&mut r.values[1], 90.0, "to").id(salt.with("v1")));
            changed |= a.changed() || b.changed();
            enter |= (a.lost_focus() || b.lost_focus()) && ui.input(|i| i.key_pressed(Key::Enter));
        }
        Input::Path => {
            let a = ui.add(mono_edit(&mut r.values[0], 110.0, "$.path").id(salt.with("v0")));
            let b = ui.add(mono_edit(&mut r.values[1], 110.0, "value").id(salt.with("v1")));
            changed |= a.changed() || b.changed();
            enter |= (a.lost_focus() || b.lost_focus()) && ui.input(|i| i.key_pressed(Key::Enter));
        }
        Input::Last => {
            let a = ui.add(mono_edit(&mut r.values[0], 44.0, "7").id(salt.with("v0")));
            changed |= a.changed();
            enter |= a.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            let mut unit = rules::UNITS.iter().position(|u| *u == r.values[1]).unwrap_or(1);
            let before = unit;
            let opts: Vec<(usize, &str)> = rules::UNITS.iter().enumerate().map(|(i, u)| (i, *u)).collect();
            segmented(ui, &mut unit, &opts);
            if unit != before || !rules::UNITS.contains(&r.values[1].as_str()) {
                r.values[1] = rules::UNITS[unit].to_string();
                changed = true;
            }
        }
        Input::Column => {
            let dates: Vec<Col> = cols.iter().filter(|c| rules::group_of(c.kind) == rules::Group::Date && c.name != r.col).cloned().collect();
            if let Some(p) = col_pick(ui, salt.with("after"), &r.values[0], &dates, 120.0, None) {
                r.values[0] = p;
                changed = true;
            }
        }
        Input::List => {
            let mut remove = None;
            let shown: Vec<(usize, String)> = r.values.iter().cloned().enumerate().filter(|(_, v)| !v.trim().is_empty()).collect();
            for (i, v) in &shown {
                if value_chip(ui, v, r.kind) {
                    remove = Some(*i);
                }
            }
            let id = salt.with("entry");
            let mut buf = ui.data(|d| d.get_temp::<String>(id)).unwrap_or_default();
            let resp = ui.add(mono_edit(&mut buf, 110.0, "+ value, or paste a list").id(id.with("edit")));
            let commit = resp.lost_focus() && !buf.trim().is_empty() || buf.contains(',') || buf.contains('\n');
            if commit {
                let mut vals: Vec<String> = r.values.iter().filter(|v| !v.trim().is_empty()).cloned().collect();
                vals.extend(buf.split([',', '\n']).map(|s| s.trim().trim_matches('\'').to_string()).filter(|s| !s.is_empty()));
                r.values = vals;
                buf.clear();
                changed = true;
                if resp.has_focus() || ui.input(|i| i.key_pressed(Key::Enter)) {
                    resp.request_focus();
                }
            }
            if let Some(i) = remove {
                r.values.remove(i);
                changed = true;
            }
            ui.data_mut(|d| d.insert_temp(id, buf));
        }
    }
    (changed, enter)
}

/// An editable rule: column, operator and values. Returns (changed, Enter pressed).
/// Moves a rule to column `col`. The operator stays when the new column offers it, else the column's first one starts
/// over; either way case matching goes back to what a new rule has, since the flag means nothing on most columns.
pub(super) fn set_col(r: &mut Rule, col: String, kind: Kind, d: Dialect) {
    r.col = col;
    if !rules::ops_for(kind, d).contains(&r.op) || rules::group_of(kind) != rules::group_of(r.kind) {
        r.op = rules::ops_for(kind, d)[0];
        r.values = rules::default_values(r.op);
    }
    r.kind = kind;
    r.case = rules::default_case(r.op, d);
}

/// Gives a rule another operator, keeping a single value it had. Case matching goes back to the operator's own: on
/// PostgreSQL = matches case and text search does not.
pub(super) fn set_op(r: &mut Rule, op: Op, d: Dialect) {
    let keep: Vec<String> = r.values.iter().filter(|v| !v.is_empty()).cloned().collect();
    r.op = op;
    r.values = rules::default_values(op);
    if let (Some(first), true) = (keep.first(), matches!(rules::input(op), Input::One)) {
        r.values[0] = first.clone();
    }
    r.case = rules::default_case(op, d);
}

/// `pick` opens the column picker with that text typed, for a row just added.
pub(super) fn rule_editor(ui: &mut egui::Ui, r: &mut Rule, cols: &[Col], salt: egui::Id, d: Dialect, pick: Option<String>) -> (bool, bool) {
    let mut changed = false;
    if let Some(p) = col_pick(ui, salt.with("col"), &r.col, cols, 130.0, pick) && p != r.col {
        let kind = cols.iter().find(|c| c.name == p).map(|c| c.kind).unwrap_or(Kind::Text);
        set_col(r, p, kind, d);
        changed = true;
    }
    if let Some(op) = op_pick(ui, r.kind, r.op, 90.0, d) && op != r.op {
        set_op(r, op, d);
        changed = true;
    }
    let (c, e) = value_inputs(ui, r, cols, salt);
    (changed || c, e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_filter_puts_prefixes_first() {
        let col = |n: &str| Col::plain(n, "VARCHAR", Kind::Text, false);
        let cols = [col("created_at"), col("order_status"), col("status"), col("id"), col("Status_code")];
        assert_eq!(col_filter(&cols, ""), [0, 1, 2, 3, 4]);
        assert_eq!(col_filter(&cols, "stat"), [2, 4, 1]);
        assert_eq!(col_filter(&cols, "STAT"), [2, 4, 1]);
        assert!(col_filter(&cols, "zz").is_empty());
        assert_eq!(find_ci("order_status", "STAT"), Some((6, 10)));
    }

    #[test]
    fn a_new_postgres_row_switched_to_contains_ignores_case() {
        let d = Dialect::Postgres;
        let mut r = Rule::new("name", Kind::Text, d);
        assert!(r.op == Op::Eq && r.case);
        set_op(&mut r, Op::Contains, d);
        r.values = vec!["ann".into()];
        assert_eq!(rules::rule_sql(&r, d, false), "\"name\" ILIKE '%ann%'");
        set_op(&mut r, Op::Eq, d);
        assert_eq!(rules::rule_sql(&r, d, false), "\"name\" = 'ann'");
        // A uuid column has no case flag, and its rule reads back the way a new one would have it.
        set_col(&mut r, "u".into(), Kind::Uuid, d);
        assert_eq!(r.case, rules::default_case(r.op, d));
    }
}
