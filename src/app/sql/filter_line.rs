//! What the query line shows of the filter in Rules mode: the Filters button that opens the panel, and the conditions in
//! use as one line of text, cut with a +N when it runs out of room.

use crate::app::*;
use crate::db::sql::{
    filter::{live, Item, Match, Node, Tree},
    rules::{self, Input, Rule},
};

/// The rows that write SQL: on and complete.
pub(super) fn live_rows(t: &Tree) -> usize {
    t.rows_in_order().into_iter().filter(|id| matches!(t.find(*id).map(|n| &n.item), Some(Item::Cond(c)) if c.on && c.rule.complete())).count()
}

/// The Filters button: a funnel, the word and how many conditions are in use. Drawn pressed while the panel is open.
pub(super) fn filters_button(ui: &mut egui::Ui, t: &Tree, open: bool) -> egui::Response {
    let n = live_rows(t);
    let g = ui.painter().layout_no_wrap("Filters".into(), FontId::proportional(TXT_SMALL), TEXT());
    let count = (n > 0).then(|| ui.painter().layout_job(LayoutJob::single_section(n.to_string(), mono_fmt(TXT_MICRO, PRIMARY(), W_SEMIBOLD))));
    let badge_w = count.as_ref().map_or(0.0, |c| c.size().x + 10.0 + 6.0);
    let (rect, r) = ui.allocate_exact_size(egui::vec2(10.0 + 16.0 + 6.0 + g.size().x + badge_w + 10.0, BUTTON_H), Sense::click());
    let state = state_of(&r);
    let fill = if open { [tint(PRIMARY(), 30), tint(PRIMARY(), 42), tint(PRIMARY(), 54)][state] } else { hover_fill(state) };
    ui.painter().rect(rect, R_SM, fill, Stroke::new(1.0, if open { PRIMARY() } else { BORDER() }), egui::StrokeKind::Inside);
    let mut x = rect.left() + 10.0;
    crate::ui::glyphs::funnel(ui.painter(), egui::pos2(x + 8.0, rect.center().y), if open || state > 0 { BRIGHT() } else { MUTED() });
    x += 16.0 + 6.0;
    let dy = optical_dy(&g);
    let gw = g.size().x;
    ui.painter().galley(egui::pos2(x, rect.center().y - g.size().y / 2.0 + dy), g, TEXT());
    x += gw + 6.0;
    if let Some(c) = count {
        // As tall as its digits and a little air, centered on the button.
        let b = egui::Rect::from_center_size(egui::pos2(x + (c.size().x + 10.0) / 2.0, rect.center().y), egui::vec2(c.size().x + 10.0, c.size().y + 4.0));
        paint_badge(ui.painter(), b, c, PRIMARY(), 31);
    }
    focus_ring(ui, &r, rect, R_SM as f32);
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, open, "Filters"));
    r.on_hover_text(if open { "Close the filter panel" } else { "Open the filter panel" }).on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The text of a rule's values, as the line shows them.
fn value_text(r: &Rule) -> String {
    let v = |i: usize| r.values.get(i).cloned().unwrap_or_default();
    match rules::input(r.op) {
        Input::None => String::new(),
        Input::Two => format!("{} and {}", v(0), v(1)),
        Input::Last => format!("{} {}", v(0), v(1)),
        Input::Path => format!("{} {}", v(0), v(1)),
        _ => r.values.iter().filter(|s| !s.trim().is_empty()).cloned().collect::<Vec<_>>().join(", "),
    }
}

/// One node of the summary in its colors: a condition as column, operator and value, a group in parentheses with its
/// own word, a none group after a not.
fn part(job: &mut LayoutJob, n: &Node) {
    let f = |c: Color32| mono_fmt(TXT_SMALL, c, 400.0);
    match &n.item {
        Item::Cond(c) => {
            let r = &c.rule;
            job.append(&r.col, 0.0, f(TEXT()));
            job.append(&format!(" {}", rules::label(r.op, r.kind)), 0.0, f(C_KEYWORD()));
            let v = value_text(r);
            if !v.is_empty() {
                job.append(&format!(" {v}"), 0.0, f(if rules::group_of(r.kind) == rules::Group::Number { C_NUMBER() } else { C_STRING() }));
            }
        }
        Item::Group(g) => {
            if g.mode == Match::None {
                job.append("not ", 0.0, f(C_KEYWORD()));
            }
            job.append("(", 0.0, f(MUTED()));
            let word = if g.mode == Match::All { " and " } else { " or " };
            for (i, k) in g.kids.iter().filter(|k| live(k)).enumerate() {
                if i > 0 {
                    job.append(word, 0.0, f(C_KEYWORD()));
                }
                part(job, k);
            }
            job.append(")", 0.0, f(MUTED()));
        }
    }
}

/// The conditions in use on one line of at most `room` points: the root's live children joined by its word, each a click
/// target that opens the panel on it, and a +N chip for those that do not fit. "All rows" when there are none. Returns
/// the node clicked.
pub(super) fn summary(ui: &mut egui::Ui, t: &Tree, room: f32) -> Option<u32> {
    let parts: Vec<&Node> = t.root.kids.iter().filter(|n| live(n)).collect();
    if parts.is_empty() {
        ui.add_sized([0.0, BUTTON_H], egui::Label::new(RichText::new("All rows").size(TXT_SMALL).color(FAINT())));
        return None;
    }
    let word = match t.root.mode {
        Match::All => "and",
        Match::Any => "or",
        Match::None => "nor",
    };
    let start = ui.cursor().min.x;
    let mut clicked = None;
    let none = t.root.mode == Match::None;
    if none {
        ui.add_sized([0.0, BUTTON_H], egui::Label::new(RichText::new("none of").font(FontId::monospace(TXT_SMALL)).color(C_KEYWORD())));
    }
    for (i, n) in parts.iter().enumerate() {
        let mut job = LayoutJob::default();
        part(&mut job, n);
        let g = ui.fonts_mut(|f| f.layout_job(job));
        let rest = parts.len() - i;
        let used = ui.cursor().min.x - start;
        // Room for this part, and for the +N chip when others follow.
        let need = g.size().x + 12.0 + if rest > 1 { 40.0 } else { 0.0 };
        if i > 0 && used + need > room {
            let more = format!("+{rest}");
            if badge(ui, &more, MUTED()).on_hover_text("More conditions in the filter panel").clicked() {
                clicked = Some(n.id);
            }
            break;
        }
        if i > 0 {
            ui.add_sized([0.0, BUTTON_H], egui::Label::new(RichText::new(word).font(FontId::monospace(TXT_CAPTION)).color(FAINT())));
        }
        let (rect, r) = ui.allocate_exact_size(egui::vec2(g.size().x + 12.0, BUTTON_H), Sense::click());
        let state = state_of(&r);
        if state > 0 {
            ui.painter().rect_filled(rect, R_SM, hover_fill(state));
        }
        let dy = optical_dy(&g);
        ui.painter().galley(egui::pos2(rect.left() + 6.0, rect.center().y - g.size().y / 2.0 + dy), g, TEXT());
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Filter condition"));
        if r.on_hover_text("Edit in the filter panel").on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            clicked = Some(n.id);
        }
    }
    clicked
}
