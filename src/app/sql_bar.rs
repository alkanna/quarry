//! The Content query bar of a SQL tab (design F): a Rules / SQL switch, typed rules or WHERE text,
//! and the clause chips under it. The text in `Tab::filter_txt` and `Tab::sort_txt` stays the one
//! source of truth; rules, popovers and chips read and write it.

use super::*;
use crate::db::sql::{
    lex,
    rules::{self, Grain, Input, Op, Rule},
    value::Kind,
    Col, Dialect, PageQuery,
};

/// The clauses a tab keeps besides the WHERE and ORDER BY text, saved with the open tabs.
#[derive(Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct Clauses {
    /// SQL mode of the bar instead of Rules mode.
    pub(super) sql: bool,
    pub(super) select: String,
    pub(super) group: String,
    pub(super) having: String,
    pub(super) limit: String,
    pub(super) offset: String,
    pub(super) hint: String,
    pub(super) distinct: bool,
}

/// One aggregate of the GROUP BY editor: function index into `FUNCS`, column ("" is `*`) and alias.
#[derive(Clone, Default)]
struct Agg {
    func: usize,
    col: String,
    alias: String,
}

const FUNCS: [&str; 5] = ["COUNT", "SUM", "AVG", "MIN", "MAX"];

/// The state of the GROUP BY editor. It is what the last Apply wrote, so reopening shows it again;
/// text edited by hand in SQL mode is not read back (ponytail: parse it if someone asks).
#[derive(Default)]
struct GroupDlg {
    keys: Vec<(String, Option<Grain>)>,
    aggs: Vec<Agg>,
    having: Option<Rule>,
}

/// Runtime state of the bar. Only `c` is saved.
#[derive(Default)]
pub(super) struct Bar {
    pub(super) c: Clauses,
    rules: Vec<Rule>,
    any: bool,
    draft: Option<Rule>,
    /// The WHERE text the rules were last written as; a different `filter_txt` came from elsewhere.
    src: String,
    /// The ORDER BY text last seen, to tell a header click from an edit made here.
    sort_seen: String,
    /// Shift was down during the last frame, which turns a header sort into an added key.
    pub(super) shift: bool,
    fold_open: bool,
    gdlg: GroupDlg,
    /// The table's own columns, kept when a grouped answer replaces the result columns.
    pub(super) table_cols: Vec<Col>,
}

/// Splits at commas outside quotes and parentheses: select lists and GROUP BY lists.
pub(super) fn split_top(text: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth, mut quote) = (Vec::new(), String::new(), 0, None::<char>);
    for c in text.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"' | '`') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => depth -= 1,
            (None, ',') if depth == 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur);
    out.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

/// A plain column name from `col` or a bare word, or None for an expression.
fn plain_ident(s: &str) -> Option<String> {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix('`').and_then(|x| x.strip_suffix('`')) {
        return (!inner.contains('`')).then(|| inner.to_string());
    }
    (!s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$')).then(|| s.to_string())
}

/// ORDER BY text as (column, descending) keys, or None when it holds anything but plain columns.
fn parse_order(text: &str) -> Option<Vec<(String, bool)>> {
    split_top(text)
        .iter()
        .map(|k| {
            let up = k.to_ascii_uppercase();
            let (col, desc) = if up.ends_with(" DESC") { (&k[..k.len() - 5], true) } else if up.ends_with(" ASC") { (&k[..k.len() - 4], false) } else { (k.as_str(), false) };
            Some((plain_ident(col)?, desc))
        })
        .collect()
}

fn order_text(keys: &[(String, bool)], d: Dialect) -> String {
    keys.iter().map(|(c, desc)| format!("{} {}", d.quote_ident(c), if *desc { "DESC" } else { "ASC" })).collect::<Vec<_>>().join(", ")
}

/// What the worker is asked for, from the tab's texts.
pub(super) fn page_query(t: &Tab) -> PageQuery {
    let c = t.sql.as_ref().map(|s| s.bar.c.clone()).unwrap_or_default();
    PageQuery { filter: t.filter_txt.clone(), order: t.sort_txt.clone(), select: split_top(&c.select), distinct: c.distinct, group: split_top(&c.group), having: c.having, limit: c.limit.trim().parse().ok(), skip: c.offset.trim().parse().unwrap_or(0), index_hint: c.hint }
}

/// SQL text colored by token: keywords, numbers and strings. Plugs into `TextEdit::layouter`.
pub(super) fn sql_layout(ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32) -> Arc<egui::Galley> {
    let mut job = sql_job(buf.as_str());
    job.wrap.max_width = wrap;
    ui.fonts_mut(|f| f.layout_job(job))
}

pub(super) fn sql_job(text: &str) -> LayoutJob {
    let mut job = LayoutJob::default();
    for (tok, r) in lex::tokens(text) {
        let s = &text[r];
        let color = match tok {
            lex::Tok::Word if db::sql::KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(s)) => C_KEYWORD(),
            lex::Tok::Word if s.chars().next().is_some_and(|c| c.is_ascii_digit()) => C_NUMBER(),
            lex::Tok::Str => C_STRING(),
            lex::Tok::Comment => FAINT(),
            lex::Tok::Punct => MUTED(),
            _ => TEXT(),
        };
        job.append(s, 0.0, mono_fmt(TXT_SMALL, color, 400.0));
    }
    job
}

fn type_name(c: &Col) -> String {
    c.ty.split('(').next().unwrap_or(&c.ty).split_whitespace().next().unwrap_or("").to_ascii_uppercase()
}

/// A painted check mark.
fn paint_check(p: &egui::Painter, c: egui::Pos2, color: Color32) {
    let pts = [c + egui::vec2(-4.0, 0.0), c + egui::vec2(-1.5, 3.0), c + egui::vec2(4.0, -3.5)];
    p.line_segment([pts[0], pts[1]], Stroke::new(1.6, color));
    p.line_segment([pts[1], pts[2]], Stroke::new(1.6, color));
}

/// One row of a menu: a check column, the label in `color`, a faint note on the right.
fn menu_row(ui: &mut egui::Ui, label: &str, color: Color32, checked: bool, note: &str) -> egui::Response {
    let w = ui.available_width().max(190.0);
    let (rect, r) = ui.allocate_exact_size(egui::vec2(w, 24.0), Sense::click());
    let state = state_of(&r);
    if state > 0 {
        ui.painter().rect_filled(rect, R_SM, if state == 2 { menu_pressed() } else { MENU_HOVER() });
    }
    if checked {
        paint_check(ui.painter(), egui::pos2(rect.left() + 12.0, rect.center().y), TEXT());
    }
    let g = ui.painter().layout_no_wrap(label.to_string(), FontId::monospace(TXT_SMALL), color);
    let dy = optical_dy(&g);
    ui.painter().galley(egui::pos2(rect.left() + 26.0, rect.center().y - g.size().y / 2.0 + dy), g, color);
    if !note.is_empty() {
        let n = ui.painter().layout_no_wrap(note.to_string(), FontId::monospace(TXT_CAPTION), FAINT());
        let dy = optical_dy(&n);
        ui.painter().galley(egui::pos2(rect.right() - 8.0 - n.size().x, rect.center().y - n.size().y / 2.0 + dy), n, FAINT());
    }
    focus_ring(ui, &r, rect, R_SM as f32);
    let label = label.to_string();
    let enabled = ui.is_enabled();
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::MenuItem, enabled, checked, label.clone()));
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// An input-looking button that opens a menu: `main` in mono, `sub` faint (the column type), a chevron.
fn pill_combo(ui: &mut egui::Ui, main: &str, sub: &str, w: f32, main_color: Color32) -> egui::Response {
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
fn combo_w(ui: &egui::Ui, main: &str, sub: &str, min: f32) -> f32 {
    (text_w(ui, main, FontId::monospace(TXT_SMALL)) + if sub.is_empty() { 0.0 } else { text_w(ui, sub, FontId::monospace(TXT_CAPTION)) + 6.0 } + 34.0).max(min)
}

/// A column picker. Returns the picked name.
fn col_pick(ui: &mut egui::Ui, cur: &str, cols: &[Col], min: f32) -> Option<String> {
    let ty = cols.iter().find(|c| c.name == cur).map(type_name).unwrap_or_default();
    let w = combo_w(ui, if cur.is_empty() { "column" } else { cur }, &ty, min);
    let r = pill_combo(ui, if cur.is_empty() { "column" } else { cur }, &ty, w, if cur.is_empty() { FAINT() } else { TEXT() });
    let mut pick = None;
    egui::Popup::menu(&r).width(220.0).show(|ui| {
        menu_look(ui);
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            for c in cols {
                if menu_row(ui, &c.name, TEXT(), c.name == cur, &type_name(c)).clicked() {
                    pick = Some(c.name.clone());
                    ui.close();
                }
            }
        });
    });
    pick
}

/// The menu group of an operator, to draw a rule between groups like the design.
fn op_cat(op: Op) -> u8 {
    use Op::*;
    match op {
        Eq | Ne | Gt | Lt | Ge | Le => 0,
        Contains | NotContains | StartsWith | NotStartsWith | EndsWith | NotEndsWith => 1,
        Like | NotLike | Regexp | NotRegexp => 2,
        In | NotIn | NotInOrNull | Between => 3,
        Empty | NotEmpty => 4,
        IsNull | NotNull => 5,
        LenEq | LenGt | LenLt => 6,
        Today | Last | ThisWeek | ThisMonth => 7,
        AfterCol => 8,
        SetContains => 9,
        JsonPathEq | JsonContains | JsonHasPath => 10,
    }
}

/// The operator picker for a column type.
fn op_pick(ui: &mut egui::Ui, kind: Kind, cur: Op, min: f32) -> Option<Op> {
    let label = rules::label(cur, kind);
    let w = combo_w(ui, label, "", min);
    let r = pill_combo(ui, label, "", w, if rules::is_keyword(cur) { C_KEYWORD() } else { TEXT() });
    let mut pick = None;
    egui::Popup::menu(&r).width(190.0).show(|ui| {
        menu_look(ui);
        let mut last = None;
        for &op in rules::ops_for(kind) {
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

/// A round "+" button, painted because the glyph sits off center.
fn plus_button(ui: &mut egui::Ui, enabled: bool) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(BUTTON_H + 4.0, BUTTON_H), Sense::click());
    let state = if enabled { state_of(&r) } else { 0 };
    ui.painter().rect(rect, rect.height() / 2.0, hover_fill(state), Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
    let color = if !enabled { FAINT() } else if state > 0 { BRIGHT() } else { MUTED() };
    let c = rect.center();
    ui.painter().line_segment([c - egui::vec2(4.0, 0.0), c + egui::vec2(4.0, 0.0)], Stroke::new(1.3, color));
    ui.painter().line_segment([c - egui::vec2(0.0, 4.0), c + egui::vec2(0.0, 4.0)], Stroke::new(1.3, color));
    focus_ring(ui, &r, rect, rect.height() / 2.0);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, "Add rule"));
    r.on_hover_cursor(if enabled { egui::CursorIcon::PointingHand } else { egui::CursorIcon::Default })
}

/// The text of a rule's values for its token.
fn value_text(r: &Rule) -> String {
    let v = |i: usize| r.values.get(i).cloned().unwrap_or_default();
    match rules::input(r.op) {
        Input::None => String::new(),
        Input::Two => format!("{} and {}", v(0), v(1)),
        Input::Last => format!("{} {}", v(0), v(1)),
        Input::Path => format!("{} = {}", v(0), v(1)),
        _ => r.values.iter().filter(|s| !s.trim().is_empty()).cloned().collect::<Vec<_>>().join(", "),
    }
}

/// A finished rule as a token: column, operator and values in their colors, with a cross.
/// Returns (clicked to edit, removed).
fn rule_token(ui: &mut egui::Ui, r: &Rule) -> (bool, bool) {
    let mut job = LayoutJob::default();
    job.append(&r.col, 0.0, mono_fmt(TXT_SMALL, TEXT(), 400.0));
    let op = rules::label(r.op, r.kind);
    job.append(&format!("  {op}"), 0.0, mono_fmt(TXT_SMALL, C_KEYWORD(), 400.0));
    let vt = value_text(r);
    if !vt.is_empty() {
        let c = if rules::group_of(r.kind) == rules::Group::Number { C_NUMBER() } else { C_STRING() };
        job.append(&format!("  {vt}"), 0.0, mono_fmt(TXT_SMALL, c, 400.0));
    }
    let g = ui.fonts_mut(|f| f.layout_job(job));
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(g.size().x + 14.0 + 4.0 + CLOSE_W + 4.0, BUTTON_H), Sense::click());
    let state = state_of(&resp);
    ui.painter().rect_filled(rect, R_SM, [tint(PRIMARY(), 22), tint(PRIMARY(), 34), tint(PRIMARY(), 46)][state]);
    let dy = optical_dy(&g);
    ui.painter().galley(egui::pos2(rect.left() + 8.0, rect.center().y - g.size().y / 2.0 + dy), g, TEXT());
    focus_ring(ui, &resp, rect, R_SM as f32);
    let close = egui::Rect::from_center_size(egui::pos2(rect.right() - CLOSE_W / 2.0 - 4.0, rect.center().y), egui::vec2(CLOSE_W, CLOSE_W));
    let x = ui.scope_builder(egui::UiBuilder::new().max_rect(close), |ui| close_x(ui).on_hover_text("Remove rule")).inner;
    (resp.on_hover_text("Edit this rule").clicked() && !x.clicked(), x.clicked())
}

/// A removable value chip of an IN list.
fn value_chip(ui: &mut egui::Ui, text: &str, kind: Kind) -> bool {
    let color = if rules::group_of(kind) == rules::Group::Number { C_NUMBER() } else { C_STRING() };
    let g = ui.painter().layout_no_wrap(text.to_string(), FontId::monospace(TXT_SMALL), color);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(g.size().x + 12.0 + CLOSE_W + 2.0, BUTTON_H - 4.0), Sense::hover());
    ui.painter().rect_filled(rect, rect.height() / 2.0, tint(color, 26));
    let dy = optical_dy(&g);
    ui.painter().galley(egui::pos2(rect.left() + 9.0, rect.center().y - g.size().y / 2.0 + dy), g, color);
    let close = egui::Rect::from_center_size(egui::pos2(rect.right() - CLOSE_W / 2.0 - 2.0, rect.center().y), egui::vec2(CLOSE_W, CLOSE_W));
    ui.scope_builder(egui::UiBuilder::new().max_rect(close), |ui| close_x(ui).on_hover_text("Remove value").clicked()).inner
}

fn mono_edit<'a>(text: &'a mut String, w: f32, hint: &str) -> egui::TextEdit<'a> {
    egui::TextEdit::singleline(text).font(FontId::monospace(TXT_SMALL)).hint_text(RichText::new(hint).color(FAINT())).desired_width(w).margin(egui::Margin::symmetric(7, 4))
}

/// The value editors of a rule. Returns (changed, Enter pressed).
fn value_inputs(ui: &mut egui::Ui, r: &mut Rule, cols: &[Col], salt: egui::Id) -> (bool, bool) {
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
            if let Some(p) = col_pick(ui, &r.values[0], &dates, 120.0) {
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
fn rule_editor(ui: &mut egui::Ui, r: &mut Rule, cols: &[Col], salt: egui::Id) -> (bool, bool) {
    let mut changed = false;
    if let Some(p) = col_pick(ui, &r.col, cols, 130.0) {
        if p != r.col {
            let kind = cols.iter().find(|c| c.name == p).map(|c| c.kind).unwrap_or(Kind::Text);
            r.col = p;
            // Keep the operator when the new column offers it, else start over with its first one.
            if !rules::ops_for(kind).contains(&r.op) || rules::group_of(kind) != rules::group_of(r.kind) {
                r.op = rules::ops_for(kind)[0];
                r.values = rules::default_values(r.op);
            }
            r.kind = kind;
            changed = true;
        }
    }
    if let Some(op) = op_pick(ui, r.kind, r.op, 90.0) {
        if op != r.op {
            let keep: Vec<String> = r.values.iter().filter(|v| !v.is_empty()).cloned().collect();
            r.op = op;
            r.values = rules::default_values(op);
            if let (Some(first), true) = (keep.first(), matches!(rules::input(op), Input::One)) {
                r.values[0] = first.clone();
            }
            changed = true;
        }
    }
    let (c, e) = value_inputs(ui, r, cols, salt);
    (changed || c, e)
}

/// A clause pill that opens a popover: keyword, summary and a cross. Returns (response, cleared).
fn clause_chip(ui: &mut egui::Ui, name: &str, summary: &str, open: bool) -> (egui::Response, bool) {
    let gn = ui.painter().layout_no_wrap(name.to_string(), FontId::monospace(TXT_CAPTION), FAINT());
    let gs = ui.painter().layout_no_wrap(summary.to_string(), FontId::monospace(TXT_CAPTION), TEXT());
    let w = 10.0 + gn.size().x + 8.0 + gs.size().x + 6.0 + CLOSE_W + 3.0;
    let (rect, r) = ui.allocate_exact_size(egui::vec2(w, BUTTON_H), Sense::click());
    let state = state_of(&r);
    let edge = if open { PRIMARY() } else if state > 0 { BORDER() } else { LINE() };
    ui.painter().rect(rect, BUTTON_H / 2.0, if state == 2 { PRESSED() } else { PANEL() }, Stroke::new(1.0, edge), egui::StrokeKind::Inside);
    let (dn, ds) = (optical_dy(&gn), optical_dy(&gs));
    let nw = gn.size().x;
    ui.painter().galley(egui::pos2(rect.left() + 10.0, rect.center().y - gn.size().y / 2.0 + dn), gn, FAINT());
    ui.painter().galley(egui::pos2(rect.left() + 18.0 + nw, rect.center().y - gs.size().y / 2.0 + ds), gs, TEXT());
    focus_ring(ui, &r, rect, BUTTON_H / 2.0);
    let close = egui::Rect::from_center_size(egui::pos2(rect.right() - CLOSE_W / 2.0 - 3.0, rect.center().y), egui::vec2(CLOSE_W, CLOSE_W));
    let x = ui.scope_builder(egui::UiBuilder::new().max_rect(close), |ui| close_x(ui).on_hover_text(format!("Remove {name}")).clicked()).inner;
    (r.on_hover_cursor(egui::CursorIcon::PointingHand), x)
}

/// The popover for ORDER BY: one row per key. Returns whether the text changed.
fn order_popover(ui: &mut egui::Ui, text: &mut String, cols: &[Col], d: Dialect) -> bool {
    menu_look(ui);
    ui.set_min_width(320.0);
    ui.add_space(4.0);
    caption(ui, "ORDER BY");
    ui.add_space(4.0);
    let Some(mut keys) = parse_order(text) else {
        // Free text (expressions) cannot be shown as keys.
        let r = ui.add(mono_edit(text, 300.0, "created_at DESC").layouter(&mut sql_layout));
        return r.changed();
    };
    let mut changed = false;
    let mut remove = None;
    for (i, (col, desc)) in keys.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            if let Some(p) = col_pick(ui, col, cols, 170.0) {
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
        if quiet(ui, RichText::new("+ Sort key").color(PRIMARY())).clicked() {
            if let Some(c) = cols.iter().find(|c| !keys.iter().any(|(k, _)| *k == c.name)) {
                keys.push((c.name.clone(), false));
                changed = true;
            }
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
    let on = |c: &Col| all || items.iter().any(|i| plain_ident(i).is_some_and(|n| n == c.name));
    let mut checked: Vec<bool> = cols.iter().map(on).collect();
    // The expression fields keep their own buffers, since text typed before Enter is not in `select` yet.
    let ex_id = ui.scope_id().with("exprs");
    let (mut exprs, seen) = ui.data(|d| d.get_temp::<(Vec<String>, String)>(ex_id)).unwrap_or_default();
    if seen != *select {
        exprs = if all { Vec::new() } else { items.iter().filter(|i| plain_ident(i).is_none_or(|n| !cols.iter().any(|c| c.name == n))).cloned().collect() };
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
                let r = ui.add(mono_edit(e, 250.0, "total * 1.2 AS gross").layouter(&mut sql_layout));
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
        if let Some(p) = col_pick(ui, "", cols, 90.0) {
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
    let aliases: Vec<Col> = g.aggs.iter().filter(|a| !a.alias.trim().is_empty()).map(|a| Col { name: a.alias.trim().to_string(), ty: "DECIMAL".into(), kind: Kind::Decimal, nullable: true }).collect();
    ui.horizontal(|ui| {
        if g.having.is_none() {
            if quiet(ui, RichText::new("+ Condition").color(PRIMARY())).clicked() {
                if let Some(a) = aliases.first() {
                    g.having = Some(Rule::new(&a.name, Kind::Decimal));
                }
            }
        } else if let Some(r) = g.having.as_mut() {
            if !aliases.iter().any(|a| a.name == r.col) {
                r.col = aliases.first().map(|a| a.name.clone()).unwrap_or_default();
            }
            rule_editor(ui, r, &aliases, ui.scope_id().with("having"));
            if close_x(ui).on_hover_text("Remove condition").clicked() {
                g.having = None;
            }
        }
    });
    ui.add_space(6.0);
    let (sel, grp) = group_texts(g, d);
    let hav = g.having.as_ref().filter(|r| r.complete()).map(|r| rules::rule_sql(r, d)).unwrap_or_default();
    let mut job = sql_job(&format!("SELECT {sel}\n  GROUP BY {grp}{}", if hav.is_empty() { String::new() } else { format!(" HAVING {hav}") }));
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

/// Draws the bar and the clause chips. Returns true when the query should run.
pub(super) fn bar(ui: &mut egui::Ui, t: &mut Tab, d: Dialect) -> bool {
    let Some(s) = t.sql.as_mut() else { return false };
    let cols = if s.bar.table_cols.is_empty() { s.cols.clone() } else { s.bar.table_cols.clone() };
    let pairs: Vec<(String, Kind)> = cols.iter().map(|c| (c.name.clone(), c.kind)).collect();
    let b = &mut s.bar;
    let mut run = false;
    // A header sort with Shift held adds a key instead of replacing the others.
    if t.sort_txt != b.sort_seen {
        if b.shift && !b.sort_seen.is_empty() && !t.sort_txt.is_empty() {
            if let (Some(mut old), Some(new)) = (parse_order(&b.sort_seen), parse_order(&t.sort_txt)) {
                old.retain(|(c, _)| !new.iter().any(|(n, _)| n == c));
                old.extend(new);
                t.sort_txt = order_text(&old, d);
                run = true;
            }
        }
        b.sort_seen = t.sort_txt.clone();
    }
    // The WHERE text changed from elsewhere (Filter to these, a restored tab): read it back into rules.
    if !b.c.sql && t.filter_txt != b.src && (!pairs.is_empty() || t.filter_txt.trim().is_empty()) {
        match rules::parse(&t.filter_txt, &pairs) {
            Some((mut r, any)) => {
                b.draft = r.pop();
                b.rules = r;
                b.any = any;
            }
            None => b.c.sql = true,
        }
        b.src = t.filter_txt.clone();
    }
    if !b.c.sql && b.draft.is_none() {
        if let Some(c) = cols.first() {
            b.draft = Some(Rule::new(&c.name, c.kind));
        }
    }
    let size = ui.available_width();
    let mono = FontId::monospace(TXT_SMALL);
    let fid = query_id("SQL-WHERE");
    let focused = ui.memory(|m| m.has_focus(fid));
    let hovered = ui.ctx().read_response(fid).is_some_and(|r| r.hovered());
    let edge = if focused { PRIMARY() } else if hovered { MUTED() } else { BORDER() };
    let frame = egui::Frame::new().fill(PANEL()).stroke(Stroke::new(1.0, edge)).corner_radius(R_BAR).inner_margin(egui::Margin { left: 8, right: 8, top: 7, bottom: 7 }).show(ui, |ui| {
        ui.set_width(size - 18.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
            let run_w = 112.0;
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width() - run_w, BUTTON_H), egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true), |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
                // Rules mode needs text this module wrote; anything else stays in SQL mode.
                let can_rules = rules::parse(&t.filter_txt, &pairs).is_some() || pairs.is_empty();
                let mut sql = b.c.sql;
                let r0 = ui.cursor().min;
                segmented(ui, &mut sql, &[(false, "Rules"), (true, "SQL")]);
                let seg = egui::Rect::from_min_size(r0, egui::vec2(ui.cursor().min.x - r0.x, BUTTON_H));
                if b.c.sql && !can_rules {
                    ui.interact(seg, ui.scope_id().with("mode-why"), Sense::hover()).on_hover_text("Rules mode reads a flat AND or OR list of column comparisons. This WHERE has something else (a function, a subquery, or AND mixed with OR), so it stays SQL.");
                }
                if sql != b.c.sql && (sql || can_rules) {
                    b.c.sql = sql;
                    if !sql {
                        b.src.clear();
                    }
                }
                ui.add_sized([0.0, BUTTON_H], egui::Label::new(RichText::new("WHERE").font(mono.clone()).color(C_KEYWORD())));
                if b.c.sql {
                    let w = (ui.max_rect().right() - ui.cursor().left() - 24.0).max(160.0);
                    let r = ui.add(egui::TextEdit::singleline(&mut t.filter_txt).id(fid).frame(editor_margin()).font(mono.clone()).hint_text(RichText::new("status = 'paid' AND total > 50").color(FAINT())).desired_width(w).layouter(&mut sql_layout));
                    run |= r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                } else {
                    let mut edit: Option<usize> = None;
                    let mut remove: Option<usize> = None;
                    for (i, r) in b.rules.iter().enumerate() {
                        if i > 0 {
                            let w = if b.any { "OR" } else { "AND" };
                            if quiet(ui, RichText::new(w).font(FontId::monospace(TXT_CAPTION)).color(C_KEYWORD())).on_hover_text("Click to switch between AND and OR").clicked() {
                                b.any = !b.any;
                                run = true;
                            }
                        }
                        let (click, rm) = rule_token(ui, r);
                        if click {
                            edit = Some(i);
                        }
                        if rm {
                            remove = Some(i);
                        }
                    }
                    if let Some(i) = remove {
                        b.rules.remove(i);
                        run = true;
                    } else if let Some(i) = edit {
                        // The rule being edited goes back to a token when it is complete, else it is dropped.
                        let picked = b.rules.remove(i);
                        if let Some(old) = b.draft.replace(picked).filter(|r| r.complete()) {
                            b.rules.insert(i.min(b.rules.len()), old);
                        }
                    }
                    if let Some(mut dr) = b.draft.take() {
                        if !b.rules.is_empty() {
                            ui.label(RichText::new(if b.any { "OR" } else { "AND" }).font(FontId::monospace(TXT_CAPTION)).color(C_KEYWORD()));
                        }
                        let (changed, enter) = rule_editor(ui, &mut dr, &cols, ui.scope_id().with("draft"));
                        run |= enter || (changed && !matches!(rules::input(dr.op), Input::One | Input::Two | Input::Path | Input::List | Input::Last));
                        if plus_button(ui, dr.complete()).on_hover_text("Add another rule").clicked() && dr.complete() {
                            let next = cols.first().map(|c| Rule::new(&c.name, c.kind));
                            b.rules.push(dr);
                            b.draft = next;
                        } else {
                            b.draft = Some(dr);
                        }
                    }
                    // Write the rules out as the WHERE text, so Run, Copy as SQL and the fold line see them.
                    let mut all = b.rules.clone();
                    all.extend(b.draft.clone());
                    let text = rules::to_sql(&all, b.any, d);
                    if text != t.filter_txt {
                        t.filter_txt = text;
                    }
                    b.src = t.filter_txt.clone();
                }
            });
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), BUTTON_H), egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if run_button(ui, "Run", "↵").on_hover_text("Enter").clicked() {
                    run = true;
                }
            });
        });
    });
    if focused {
        ui.painter().rect_stroke(frame.response.rect.expand(1.0), R_BAR + 1, Stroke::new(2.0, tint(PRIMARY(), 60)), egui::StrokeKind::Outside);
    }
    ui.add_space(8.0);
    run |= clauses(ui, t, d, &cols);
    run
}

/// The chips under the bar, and the Copy as SQL and Explain buttons.
fn clauses(ui: &mut egui::Ui, t: &mut Tab, d: Dialect, cols: &[Col]) -> bool {
    let (db_name, table) = (t.sel.db.clone(), t.sel.coll.clone());
    let pk = t.sql.as_ref().map(|s| s.pk.clone()).unwrap_or_default();
    let matched = t.sql.as_ref().and_then(|s| s.matched).unwrap_or(0);
    let rows = t.count;
    let stmt = page_query(t).select_sql(d, &db_name, &table, &pk, None);
    let Some(s) = t.sql.as_mut() else { return false };
    let b = &mut s.bar;
    let mut run = false;
    let sql = b.c.sql;
    let mono = FontId::monospace(TXT_SMALL);
    let grouped = !b.c.group.trim().is_empty();
    ui.horizontal_top(|ui| {
        let w = ui.available_width() - 190.0;
        ui.allocate_ui_with_layout(egui::vec2(w, BUTTON_H), egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true), |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            // ORDER BY
            if sql {
                let id = query_id("SQL-ORDER");
                let tw = (text_w(ui, if t.sort_txt.is_empty() { "created_at DESC" } else { &t.sort_txt }, mono.clone()) + 8.0).clamp(24.0, 420.0);
                run |= opt_chip(ui, "ORDER BY", id, &mut t.sort_txt, |ui, text| {
                    let r = ui.add(egui::TextEdit::singleline(text).id(id).frame(editor_margin()).font(mono.clone()).hint_text("created_at DESC").desired_width(tw).layouter(&mut sql_layout));
                    r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
                });
                let id = query_id("SQL-GROUP");
                let tw = (text_w(ui, if b.c.group.is_empty() { "status, currency" } else { &b.c.group }, mono.clone()) + 8.0).clamp(24.0, 420.0);
                run |= opt_chip(ui, "GROUP BY", id, &mut b.c.group, |ui, text| {
                    let r = ui.add(egui::TextEdit::singleline(text).id(id).frame(editor_margin()).font(mono.clone()).hint_text("status, currency").desired_width(tw).layouter(&mut sql_layout));
                    r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
                });
            } else {
                let keys = parse_order(&t.sort_txt);
                let summary = match &keys {
                    Some(k) => k.iter().map(|(c, d)| format!("{c} {}", if *d { "DESC" } else { "ASC" })).collect::<Vec<_>>().join(", "),
                    None => t.sort_txt.clone(),
                };
                if t.sort_txt.is_empty() {
                    let r = add_chip(ui, "ORDER BY");
                    egui::Popup::menu(&r).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).width(340.0).show(|ui| {
                        if order_popover(ui, &mut t.sort_txt, cols, d) {
                            run = true;
                        }
                    });
                    if r.clicked() && t.sort_txt.is_empty() {
                        if let Some(c) = cols.first() {
                            t.sort_txt = order_text(&[(c.name.clone(), false)], d);
                            run = true;
                        }
                    }
                } else {
                    let (r, cleared) = clause_chip(ui, "ORDER BY", &summary, false);
                    egui::Popup::menu(&r).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).width(340.0).show(|ui| {
                        if order_popover(ui, &mut t.sort_txt, cols, d) {
                            run = true;
                        }
                    });
                    if cleared {
                        t.sort_txt.clear();
                        run = true;
                    }
                }
                if grouped {
                    let (r, cleared) = clause_chip(ui, "GROUP BY", &b.c.group, false);
                    let mut applied = false;
                    egui::Popup::menu(&r).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).width(580.0).show(|ui| applied = group_popover(ui, &mut b.gdlg, &mut b.c, cols, d, &table, matched.max(rows)));
                    run |= applied;
                    if cleared {
                        b.c.group.clear();
                        b.c.having.clear();
                        b.c.select.clear();
                        run = true;
                    }
                    if !b.c.having.trim().is_empty() {
                        let (r2, cleared) = clause_chip(ui, "HAVING", &b.c.having.replace('`', ""), false);
                        egui::Popup::menu(&r2).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).width(580.0).show(|ui| applied = group_popover(ui, &mut b.gdlg, &mut b.c, cols, d, &table, matched.max(rows)));
                        run |= applied;
                        if cleared {
                            b.c.having.clear();
                            run = true;
                        }
                    }
                }
            }
            if sql && grouped {
                let id = query_id("SQL-HAVING");
                let tw = (text_w(ui, if b.c.having.is_empty() { "orders > 10" } else { &b.c.having }, mono.clone()) + 8.0).clamp(24.0, 420.0);
                run |= opt_chip(ui, "HAVING", id, &mut b.c.having, |ui, text| {
                    let r = ui.add(egui::TextEdit::singleline(text).id(id).frame(editor_margin()).font(mono.clone()).hint_text("orders > 10").desired_width(tw).layouter(&mut sql_layout));
                    r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
                });
            }
            // LIMIT
            let id = query_id("SQL-LIMIT");
            run |= opt_chip(ui, "LIMIT", id, &mut b.c.limit, |ui, text| {
                let r = ui.add(egui::TextEdit::singleline(text).id(id).frame(editor_margin()).font(mono.clone()).hint_text("100").desired_width(44.0));
                r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
            });
            // Columns: a checklist in Rules mode, the select list text in SQL mode. Grouped results write their own.
            if !grouped {
                if sql {
                    let id = query_id("SQL-COLUMNS");
                    let tw = (text_w(ui, if b.c.select.is_empty() { "id, status, total" } else { &b.c.select }, mono.clone()) + 8.0).clamp(24.0, 420.0);
                    run |= opt_chip(ui, "Columns", id, &mut b.c.select, |ui, text| {
                        let r = ui.add(egui::TextEdit::singleline(text).id(id).frame(editor_margin()).font(mono.clone()).hint_text("id, status, total").desired_width(tw).layouter(&mut sql_layout));
                        r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
                    });
                } else {
                    let n = split_top(&b.c.select).len();
                    let (r, cleared) = if n == 0 { (add_chip(ui, "Columns"), false) } else { clause_chip(ui, "Columns", &format!("{n} of {}", cols.len()), false) };
                    egui::Popup::menu(&r).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).width(320.0).show(|ui| run |= columns_popover(ui, &mut b.c.select, cols, d));
                    if cleared {
                        b.c.select.clear();
                        run = true;
                    }
                }
            }
            if !sql && !grouped {
                let r = add_chip(ui, "GROUP BY");
                let mut applied = false;
                egui::Popup::menu(&r).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).width(580.0).show(|ui| applied = group_popover(ui, &mut b.gdlg, &mut b.c, cols, d, &table, rows));
                run |= applied;
            }
            // DISTINCT, a toggle
            if b.c.distinct {
                let (r, cleared) = clause_chip(ui, "DISTINCT", "", false);
                if cleared || r.clicked() {
                    b.c.distinct = false;
                    run = true;
                }
            } else if add_chip(ui, "DISTINCT").clicked() {
                b.c.distinct = true;
                run = true;
            }
            let id = query_id("SQL-OFFSET");
            run |= opt_chip(ui, "OFFSET", id, &mut b.c.offset, |ui, text| {
                let r = ui.add(egui::TextEdit::singleline(text).id(id).frame(editor_margin()).font(mono.clone()).hint_text("0").desired_width(44.0));
                r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
            });
            // ponytail: the index names are free text; list them from SHOW INDEX when someone asks for a picker.
            let id = query_id("SQL-INDEX");
            run |= opt_chip(ui, "USE INDEX", id, &mut b.c.hint, |ui, text| {
                let r = ui.add(egui::TextEdit::singleline(text).id(id).frame(editor_margin()).font(mono.clone()).hint_text("index_name").desired_width(90.0));
                r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
            });
        });
        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), BUTTON_H), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if quiet(ui, "Copy as SQL").on_hover_text("Copy the whole SELECT").clicked() {
                ui.ctx().copy_text(format!("{stmt};"));
            }
            // Step 8 adds Explain.
            quiet_enabled(ui, false, "Explain").on_disabled_hover_text("Coming with the Query view");
        });
    });
    // The full SELECT, folded under the bar in SQL mode.
    if sql {
        ui.add_space(8.0);
        let flat = stmt.split_whitespace().collect::<Vec<_>>().join(" ");
        let text = if b.fold_open { stmt.clone() } else { flat };
        let mut job = sql_job(&text);
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
        let tri = if b.fold_open { vec![c + egui::vec2(-4.0, -2.5), c + egui::vec2(4.0, -2.5), c + egui::vec2(0.0, 3.0)] } else { vec![c + egui::vec2(-2.5, -4.0), c + egui::vec2(-2.5, 4.0), c + egui::vec2(3.0, 0.0)] };
        ui.painter().add(egui::Shape::convex_polygon(tri, MUTED(), Stroke::NONE));
        ui.painter().galley(egui::pos2(rect.left() + 30.0, rect.top() + 7.0), g, TEXT());
        focus_ring(ui, &r, rect, R_MD as f32);
        if r.on_hover_text(if b.fold_open { "Fold the statement" } else { "Show the whole statement" }).clicked() {
            b.fold_open = !b.fold_open;
        }
    }
    run
}

/// The note above grouped results, with the Ungroup link. Returns true on Ungroup.
pub(super) fn grouped_note(ui: &mut egui::Ui, groups: u64, rows: u64) -> bool {
    let mut ungroup = false;
    egui::Frame::new().fill(tint(PRIMARY(), 14)).corner_radius(R_MD).inner_margin(egui::Margin { left: 12, right: 12, top: 7, bottom: 7 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} groups from {} rows. Double-click a group to see its rows. Grouped results are read-only.", group(&groups.to_string()), group(&rows.to_string()))).size(TXT_SMALL).color(MUTED()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if quiet(ui, RichText::new("Ungroup").color(PRIMARY())).clicked() {
                    ungroup = true;
                }
            });
        });
    });
    ungroup
}

fn tcols(s: &SqlTab) -> &[Col] {
    if s.bar.table_cols.is_empty() { &s.cols } else { &s.bar.table_cols }
}

/// The table's own sort can name an aggregate, which an ungrouped query does not have.
fn keep_sort(sort: &str, cols: &[Col]) -> bool {
    parse_order(sort).is_some_and(|k| k.iter().all(|(c, _)| cols.iter().any(|x| x.name == *c)))
}

fn bson_text(v: &Bson) -> Option<String> {
    match v {
        Bson::String(s) => Some(s.clone()),
        Bson::Int32(n) => Some(n.to_string()),
        Bson::Int64(n) => Some(n.to_string()),
        Bson::Double(n) => Some(n.to_string()),
        Bson::Boolean(b) => Some((*b as i32).to_string()),
        _ => None,
    }
}

/// Clears GROUP BY, HAVING and the generated select list; the sort stays when it names real columns.
pub(super) fn ungroup(t: &mut Tab) {
    let Some(s) = t.sql.as_mut() else { return };
    s.bar.c.group.clear();
    s.bar.c.having.clear();
    s.bar.c.select.clear();
    if !keep_sort(&t.sort_txt, tcols(s)) {
        t.sort_txt.clear();
    }
    t.page = 0;
}

/// Turns the double-clicked group into conditions on the WHERE text and ungroups.
pub(super) fn drill(t: &mut Tab, row: usize, d: Dialect) {
    let Some(doc) = t.docs.get(row).and_then(|r| r.decode().ok()) else { return };
    let Some(s) = t.sql.as_ref() else { return };
    let (select, group_list) = (split_top(&s.bar.c.select), split_top(&s.bar.c.group));
    let mut conds: Vec<String> = Vec::new();
    for g in &group_list {
        let Some(name) = plain_ident(g) else { continue };
        let Some(v) = doc.get(&name) else { continue };
        // A group named after a select alias is an expression such as DATE(col).
        let expr = select.iter().find_map(|i| {
            let up = i.to_ascii_uppercase();
            let at = up.rfind(" AS ")?;
            (plain_ident(&i[at + 4..])? == name).then(|| i[..at].trim().to_string())
        });
        let lhs = match (&expr, tcols(s).iter().find(|c| c.name == name)) {
            (Some(e), _) => e.clone(),
            (None, Some(_)) => d.quote_ident(&name),
            _ => continue,
        };
        conds.push(match bson_text(v) {
            Some(text) => match tcols(s).iter().find(|c| c.name == name && expr.is_none()) {
                Some(c) => rules::rule_sql(&Rule { col: name.clone(), kind: c.kind, op: Op::Eq, values: vec![text] }, d),
                None => format!("{lhs} = {}", rules::lit(&text)),
            },
            None => format!("{lhs} IS NULL"),
        });
    }
    let existing = t.filter_txt.trim().to_string();
    let existing = if existing.is_empty() { None } else if matches!(rules::parse(&existing, &tcols(s).iter().map(|c| (c.name.clone(), c.kind)).collect::<Vec<_>>()), Some((_, false))) { Some(existing) } else { Some(format!("({existing})")) };
    t.filter_txt = existing.into_iter().chain(conds).collect::<Vec<_>>().join(" AND ");
    ungroup(t);
}
