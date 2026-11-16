//! The cells of a filter condition: the column, operator and value pickers that the filter panel's rows and the HAVING
//! editor share.

use crate::app::*;
use crate::db::sql::{
    facts::ColFacts,
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

/// The menu group of an operator and its caption, so the menu reads by meaning.
pub(super) fn op_cat(op: Op) -> (u8, &'static str) {
    use Op::*;
    match op {
        Eq | Ne | Gt | Lt | Ge | Le | Distinct => (0, "Compare"),
        Contains | NotContains | StartsWith | NotStartsWith | EndsWith | NotEndsWith => (1, "Text"),
        Like | NotLike | Regexp | NotRegexp => (2, "Pattern"),
        In | NotIn | NotInOrNull | Between => (3, "Lists and ranges"),
        Empty | NotEmpty | IsNull | NotNull => (4, "Empty"),
        LenEq | LenGt | LenLt => (6, "Length"),
        Today | Yesterday | Last | ThisWeek | ThisMonth | LastMonth => (7, "Dates"),
        AfterCol => (8, "Columns"),
        SetContains => (9, "Sets"),
        JsonPathEq | JsonPathNe | JsonPathGt | JsonPathLt | JsonContains | JsonHasPath | JsonHasKey => (10, "JSON"),
        ArrayHasAll | ArrayHasAny | ArrayLenEq | ArrayLenGt | ArrayLenLt => (11, "Array"),
    }
}

/// The operator picker for a column type, grouped by meaning with the SQL each one sends on the right. A text column
/// reaches its regex operators through the cell's .* toggle instead.
pub(super) fn op_pick(ui: &mut egui::Ui, kind: Kind, cur: Op, case: bool, min: f32, d: Dialect, open: bool) -> Option<Op> {
    let label = rules::label(cur, kind);
    let w = combo_w(ui, label, "", min);
    let r = pill_combo(ui, label, "", w, if rules::is_keyword(cur) { C_KEYWORD() } else { TEXT() });
    if open {
        egui::Popup::open_id(ui.ctx(), egui::Popup::default_response_id(&r));
    }
    let mut pick = None;
    egui::Popup::menu(&r).width(380.0).show(|ui| {
        menu_look(ui);
        egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            let mut last = None;
            for &op in rules::ops_for(kind, d).iter().filter(|o| kind != Kind::Text || !matches!(o, Op::Regexp | Op::NotRegexp)) {
                let (cat, name) = op_cat(op);
                if last != Some(cat) {
                    if last.is_some() {
                        ui.add_space(2.0);
                    }
                    caption(ui, name);
                    last = Some(cat);
                }
                let hint = rules::op_sql_hint(op, kind, d, if rules::has_case(kind, op) { case } else { rules::default_case(op, d) });
                if menu_row(ui, rules::label(op, kind), if rules::is_keyword(op) { C_KEYWORD() } else { TEXT() }, op == cur, &hint).clicked() {
                    pick = Some(op);
                    ui.close();
                }
            }
        });
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

/// What the value cells can suggest, and what they ask the app to look up.
#[derive(Default)]
pub(super) struct Hints<'a> {
    pub(super) table: &'a str,
    /// What is known of this table's columns, by column name.
    pub(super) facts: Option<&'a HashMap<String, ColFacts>>,
    /// Frequent values, by `value_key(table, column)`.
    pub(super) values: Option<&'a Fields>,
    /// A column whose facts a cell wants and the app has not given.
    pub(super) want_facts: Option<(String, Kind)>,
    /// A column whose frequent values a list cell wants.
    pub(super) want_values: Option<String>,
}

impl Hints<'_> {
    fn facts(&mut self, col: &str, kind: Kind) -> Option<&ColFacts> {
        let f = self.facts.and_then(|f| f.get(col));
        if f.is_none() && self.facts.is_some() && self.want_facts.is_none() {
            self.want_facts = Some((col.to_string(), kind));
        }
        f
    }
}

/// Whether text reads as a uuid, with or without its hyphens.
fn is_uuid(s: &str) -> bool {
    let s = s.trim();
    let hex = |t: &str| t.chars().all(|c| c.is_ascii_hexdigit());
    match s.len() {
        32 => hex(s),
        36 => s.char_indices().all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() }),
        _ => false,
    }
}

/// A small toggle with a mono label, such as Aa and .*, accent filled while on.
fn text_toggle(ui: &mut egui::Ui, text: &str, on: bool, name: &str, tip: &str) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(26.0, BUTTON_H), Sense::click());
    let state = state_of(&r);
    let fill = if on { [tint(PRIMARY(), 40), tint(PRIMARY(), 52), tint(PRIMARY(), 64)][state] } else { hover_fill(state) };
    ui.painter().rect(rect, R_SM, fill, Stroke::new(1.0, if on { PRIMARY() } else { BORDER() }), egui::StrokeKind::Inside);
    let g = ui.painter().layout_no_wrap(text.to_string(), FontId::monospace(TXT_CAPTION), if on { PRIMARY() } else { MUTED() });
    let dy = optical_dy(&g);
    ui.painter().galley(rect.center() - g.size() / 2.0 + egui::vec2(0.0, dy), g, TEXT());
    focus_ring(ui, &r, rect, R_SM as f32);
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, on, name));
    r.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The text operator and its regex twin, which the .* toggle swaps.
fn regex_twin(op: Op) -> Option<Op> {
    match op {
        Op::Contains => Some(Op::Regexp),
        Op::NotContains => Some(Op::NotRegexp),
        Op::Regexp => Some(Op::Contains),
        Op::NotRegexp => Some(Op::NotContains),
        _ => None,
    }
}

/// The date shortcuts a date cell offers as pills, with the operator and values each sets.
const DATE_PILLS: [(&str, Op, &[&str]); 6] = [("today", Op::Today, &[]), ("yesterday", Op::Yesterday, &[]), ("last 7 days", Op::Last, &["7", "days"]), ("this month", Op::ThisMonth, &[]), ("last month", Op::LastMonth, &[]), ("range", Op::Between, &["", ""])];

/// A text field for one value, outlined in the alert color while `bad`.
fn value_field(ui: &mut egui::Ui, text: &mut String, w: f32, hint: &str, id: egui::Id, bad: bool) -> egui::Response {
    let r = ui.add(mono_edit(text, w, hint).id(id));
    if bad {
        ui.painter().rect_stroke(r.rect, R_SM, Stroke::new(1.0, ALERT()), egui::StrokeKind::Inside);
    }
    r
}

/// The suggestions a list or a single value can pick from: an enum's labels, an array's items, or the column's frequent values.
fn suggestions(r: &Rule, cols: &[Col], hints: &mut Hints) -> Vec<String> {
    let l = labels(r, cols, hints);
    if !l.is_empty() {
        return l;
    }
    if r.kind == Kind::Array {
        return hints.facts(&r.col, r.kind).map(|f| f.items.clone()).unwrap_or_default();
    }
    let key = crate::ui::sql_complete::value_key(hints.table, &r.col);
    match hints.values.and_then(|v| v.get(&key)) {
        Some(info) => info.values.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        None => {
            hints.want_values = Some(r.col.clone());
            Vec::new()
        }
    }
}

/// The value cells of a rule, by the column's type and the operator. Returns (changed, Enter pressed).
pub(super) fn value_inputs(ui: &mut egui::Ui, r: &mut Rule, cols: &[Col], salt: egui::Id, d: Dialect, hints: &mut Hints) -> (bool, bool) {
    let (mut changed, mut enter) = (false, false);
    let need = match rules::input(r.op) {
        Input::None => 0,
        Input::One | Input::List | Input::Column => 1,
        Input::Two | Input::Last | Input::Path => 2,
    };
    if r.values.len() < need {
        r.values.resize(need, String::new());
    }
    let group = rules::group_of(r.kind);
    let date = group == rules::Group::Date;
    let entered = |ui: &egui::Ui, r: &egui::Response| r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
    // Booleans pick true, false or null instead of typing.
    if r.kind == Kind::Bool && matches!(r.op, Op::Eq | Op::IsNull) {
        let mut v = if r.op == Op::IsNull { 2 } else if r.values.first().is_some_and(|v| v.trim().eq_ignore_ascii_case("false")) { 1 } else { 0 };
        let before = v;
        segmented(ui, &mut v, &[(0, "true"), (1, "false"), (2, "null")]);
        if v != before {
            if v == 2 {
                set_op(r, Op::IsNull, d);
            } else {
                if r.op != Op::Eq {
                    set_op(r, Op::Eq, d);
                }
                r.values = vec![if v == 0 { "true" } else { "false" }.into()];
            }
            changed = true;
        }
        return (changed, enter);
    }
    match rules::input(r.op) {
        Input::None => {
            if !date {
                ui.label(RichText::new("No value").size(TXT_SMALL).color(FAINT()));
            }
        }
        Input::One => {
            let labels = if matches!(r.op, Op::Eq | Op::Ne | Op::Distinct) && matches!(r.kind, Kind::Enum | Kind::Text) { labels(r, cols, hints) } else { Vec::new() };
            if !labels.is_empty() {
                // An enum picks one of its labels.
                let cur = r.values[0].clone();
                let shown = if cur.is_empty() { "value" } else { cur.as_str() };
                let w = combo_w(ui, shown, "", 110.0);
                let p = pill_combo(ui, shown, "", w, if cur.is_empty() { FAINT() } else { C_STRING() });
                egui::Popup::menu(&p).width(w.max(160.0)).show(|ui| {
                    menu_look(ui);
                    for l in &labels {
                        if menu_row(ui, l, TEXT(), *l == cur, "").clicked() {
                            r.values[0] = l.clone();
                            changed = true;
                            ui.close();
                        }
                    }
                });
            } else {
                let hint = if date { "2026-11-01 00:00" } else if group == rules::Group::Number { "number" } else if r.kind == Kind::Uuid { "uuid" } else { "value" };
                let bad = r.kind == Kind::Uuid && !r.values[0].trim().is_empty() && !is_uuid(&r.values[0]);
                let resp = value_field(ui, &mut r.values[0], 170.0, hint, salt.with("v0"), bad);
                if bad {
                    resp.clone().on_hover_text("Not a uuid: 32 hex digits, with or without hyphens");
                }
                changed |= resp.changed();
                enter |= entered(ui, &resp);
            }
        }
        Input::Two => {
            let a = ui.add(mono_edit(&mut r.values[0], 90.0, "from").id(salt.with("v0")));
            ui.label(RichText::new("and").size(TXT_SMALL).color(FAINT()));
            let b = ui.add(mono_edit(&mut r.values[1], 90.0, "to").id(salt.with("v1")));
            changed |= a.changed() || b.changed();
            enter |= entered(ui, &a) || entered(ui, &b);
            if matches!(group, rules::Group::Number | rules::Group::Date) && let Some((lo, hi, _)) = hints.facts(&r.col, r.kind).and_then(|f| f.range.clone()) {
                ui.label(RichText::new(format!("{lo} … {hi}")).font(FontId::monospace(TXT_CAPTION)).color(FAINT())).on_hover_text("The column's smallest and largest values");
            }
        }
        Input::Path => {
            let keys = hints.facts(&r.col, r.kind).map(|f| f.keys.clone()).unwrap_or_default();
            let a = ui.add(mono_edit(&mut r.values[0], 110.0, "$.path").id(salt.with("v0")));
            if !keys.is_empty() {
                let k = pill_combo(ui, "key", "", 52.0, MUTED());
                egui::Popup::menu(&k).width(180.0).show(|ui| {
                    menu_look(ui);
                    egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                        for key in &keys {
                            if menu_row(ui, key, TEXT(), false, "").clicked() {
                                // A key that is not a plain name is quoted, as jsonpath and MySQL's paths both read it.
                                let plain = key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !key.starts_with(|c: char| c.is_ascii_digit());
                                r.values[0] = if plain { format!("$.{key}") } else { format!("$.\"{}\"", key.replace('"', "\\\"")) };
                                changed = true;
                                ui.close();
                            }
                        }
                    });
                });
            }
            let mut op = r.op;
            segmented(ui, &mut op, &[(Op::JsonPathEq, "="), (Op::JsonPathNe, "≠"), (Op::JsonPathGt, ">"), (Op::JsonPathLt, "<")]);
            if op != r.op {
                r.op = op;
                changed = true;
            }
            let b = ui.add(mono_edit(&mut r.values[1], 100.0, "value").id(salt.with("v1")));
            changed |= a.changed() || b.changed();
            enter |= entered(ui, &a) || entered(ui, &b);
        }
        Input::Last => {
            let a = ui.add(mono_edit(&mut r.values[0], 44.0, "7").id(salt.with("v0")));
            changed |= a.changed();
            enter |= entered(ui, &a);
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
        Input::List => changed |= list_cell(ui, r, cols, salt, hints),
    }
    // Text search: match case, and the regex twin of contains.
    if rules::has_case(r.kind, r.op) {
        if text_toggle(ui, "Aa", r.case, "Match case", "Match case").clicked() {
            r.case = !r.case;
            changed = true;
        }
        if let Some(twin) = regex_twin(r.op).filter(|_| r.kind == Kind::Text)
            && text_toggle(ui, ".*", matches!(r.op, Op::Regexp | Op::NotRegexp), "Regular expression", "Regular expression").clicked()
        {
            // The value and the case flag stay: the same text, read as a pattern or not.
            r.op = twin;
            changed = true;
        }
    }
    // Dates: the common ranges as pills beside the operator's own cells.
    if date && matches!(r.op, Op::Today | Op::Yesterday | Op::Last | Op::ThisMonth | Op::LastMonth | Op::Between | Op::ThisWeek) {
        for (name, op, values) in DATE_PILLS {
            let on = r.op == op && (op != Op::Last || r.values.first().is_some_and(|v| v == "7") && r.values.get(1).is_some_and(|u| u == "days"));
            if text_pill(ui, name, on).clicked() && !on {
                set_op(r, op, d);
                r.values = values.iter().map(|v| v.to_string()).collect();
                changed = true;
            }
        }
    }
    (changed, enter)
}

/// The labels of an enum column: MySQL's from its type, PostgreSQL's from the catalog. Empty for other columns.
fn labels(r: &Rule, cols: &[Col], hints: &mut Hints) -> Vec<String> {
    let ty = cols.iter().find(|c| c.name == r.col).map(|c| c.ty.clone()).unwrap_or_default();
    let mine = crate::db::sql::schema::enum_values(&ty);
    if !mine.is_empty() {
        return mine;
    }
    hints.facts(&r.col, r.kind).map(|f| f.labels.clone()).unwrap_or_default()
}

/// A pill of the date shortcuts, filled while on.
fn text_pill(ui: &mut egui::Ui, text: &str, on: bool) -> egui::Response {
    let g = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(TXT_CAPTION), if on { PRIMARY() } else { MUTED() });
    let (rect, r) = ui.allocate_exact_size(egui::vec2(g.size().x + 14.0, BUTTON_H - 4.0), Sense::click());
    let state = state_of(&r);
    let fill = if on { [tint(PRIMARY(), 30), tint(PRIMARY(), 42), tint(PRIMARY(), 54)][state] } else { hover_fill(state) };
    ui.painter().rect(rect, rect.height() / 2.0, fill, Stroke::new(1.0, if on { PRIMARY() } else { BORDER() }), egui::StrokeKind::Inside);
    let dy = optical_dy(&g);
    ui.painter().galley(rect.center() - g.size() / 2.0 + egui::vec2(0.0, dy), g, TEXT());
    focus_ring(ui, &r, rect, rect.height() / 2.0);
    let name = text.to_string();
    r.widget_info(move || egui::WidgetInfo::selected(egui::Role::Button, true, on, &name));
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// How many value chips a list shows before a count stands for the rest, so a long list stays inside its row.
const SHOWN_CHIPS: usize = 3;

/// A list of values: removable chips, a field that takes a value or a pasted list, and suggestions while it has focus.
fn list_cell(ui: &mut egui::Ui, r: &mut Rule, cols: &[Col], salt: egui::Id, hints: &mut Hints) -> bool {
    let mut changed = false;
    let mut remove = None;
    let shown: Vec<(usize, String)> = r.values.iter().cloned().enumerate().filter(|(_, v)| !v.trim().is_empty()).collect();
    for (i, v) in shown.iter().take(SHOWN_CHIPS) {
        if value_chip(ui, v, r.kind) {
            remove = Some(*i);
        }
    }
    if shown.len() > SHOWN_CHIPS {
        let rest: Vec<&str> = shown[SHOWN_CHIPS..].iter().map(|(_, v)| v.as_str()).collect();
        ui.label(RichText::new(format!("+{}", rest.len())).font(FontId::monospace(TXT_CAPTION)).color(MUTED())).on_hover_text(rest.join(", "));
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
    if resp.has_focus() {
        let have: std::collections::HashSet<&String> = r.values.iter().collect();
        let q = buf.trim().to_lowercase();
        let picks: Vec<String> = suggestions(r, cols, hints).into_iter().filter(|s| !have.contains(s) && (q.is_empty() || s.to_lowercase().contains(&q))).take(40).collect();
        if !picks.is_empty() {
            let mut add = None;
            egui::Popup::new(id.with("sugg"), ui.ctx().clone(), resp.rect, ui.layer_id()).kind(egui::PopupKind::Menu).style(egui::containers::menu::menu_style).gap(2.0).open(true).width(200.0).show(|ui| {
                menu_look(ui);
                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                    for s in &picks {
                        if menu_row_in(ui, s, TEXT(), false, "", &buf, false).clicked() {
                            add = Some(s.clone());
                        }
                    }
                });
            });
            if let Some(a) = add {
                r.values.retain(|v| !v.trim().is_empty());
                r.values.push(a);
                buf.clear();
                changed = true;
                resp.request_focus();
            }
        }
    }
    if let Some(i) = remove {
        r.values.remove(i);
        changed = true;
    }
    ui.data_mut(|d| d.insert_temp(id, buf));
    changed
}

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
/// `ops` opens the operator menu, as the demo of the menu does.
#[allow(clippy::too_many_arguments)]
pub(super) fn rule_editor(ui: &mut egui::Ui, r: &mut Rule, cols: &[Col], salt: egui::Id, d: Dialect, pick: Option<String>, ops: bool, hints: &mut Hints) -> (bool, bool) {
    let mut changed = false;
    if let Some(p) = col_pick(ui, salt.with("col"), &r.col, cols, 130.0, pick) && p != r.col {
        let kind = cols.iter().find(|c| c.name == p).map(|c| c.kind).unwrap_or(Kind::Text);
        set_col(r, p, kind, d);
        changed = true;
    }
    if let Some(op) = op_pick(ui, r.kind, r.op, r.case, 90.0, d, ops) && op != r.op {
        set_op(r, op, d);
        changed = true;
    }
    let (c, e) = value_inputs(ui, r, cols, salt, d, hints);
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

    #[test]
    fn uuids_read_with_or_without_hyphens() {
        assert!(is_uuid("123e4567-e89b-12d3-a456-426614174000") && is_uuid("123e4567e89b12d3a456426614174000"));
        assert!(!is_uuid("123e4567-e89b-12d3-a456") && !is_uuid("zz3e4567-e89b-12d3-a456-426614174000"));
    }
}
