//! The flame view of the explain dialog: an icicle with the plan's root across the top and each node below its parent,
//! as wide as its share of the cost, or of the time once the statement ran. Bars are colored like the tree's types (a
//! full scan red, an index green) and a click selects the node for the panel beside it. The layout is `plan::flame`.

use super::explain::{SqlExplain, XAct};
use super::explain_panel::figure;
use crate::app::*;
use crate::db::sql::plan::{findings, flame, Access, PlanNode};

/// A bar holds two lines, the node and its weight, as in the design.
const BAR_H: f32 = 36.0;
const GAP: f32 = 4.0;
/// Between bars side by side.
const SPLIT: f32 = 2.0;
/// Text inside a bar starts this far from its edge.
const PAD: f32 = 10.0;
/// The narrowest bar drawn for a node that weighs something, so even a tiny one can be pointed at and clicked.
const MIN_BAR: f32 = 6.0;
/// The space under a parent its children leave is its own work; below this width it is not worth marking.
const MIN_GAP: f32 = 6.0;

pub(super) fn flame_view(ui: &mut egui::Ui, x: &mut SqlExplain, body_h: f32, acts: &mut Vec<XAct>) {
    let Some(p) = &x.plan else { return };
    let timed = flame::timed(p);
    if !timed {
        x.flame_time = false;
    }
    ui.horizontal(|ui| {
        ui.label(caption_text("WIDTH BY"));
        ui.add_space(8.0);
        segmented_on(ui, &mut x.flame_time, &[(false, "Cost", true), (true, "Time", timed)]);
        if !timed {
            ui.add_space(8.0);
            ui.label(RichText::new("Time needs ANALYZE").size(TXT_SMALL).color(FAINT()));
        }
    });
    ui.add_space(12.0);
    let bars = flame::layout_min(p, x.flame_time, (MIN_BAR / ui.available_width().max(1.0)) as f64);
    if bars.is_empty() {
        ui.label(RichText::new("The plan gives no cost to draw. See Tree.").color(MUTED()));
        return;
    }
    // The figure on each bar is the weight its width comes from.
    let all = flame::weights(p, x.flame_time);
    let total: f64 = (0..p.nodes.len()).filter(|&k| p.nodes[k].parent.is_none()).map(|k| all[k]).sum();
    let rows = bars.iter().map(|b| b.row).max().unwrap_or(0) + 1;
    let scroll_to = x.scroll_to.take();
    let mut picked = None;
    egui::ScrollArea::vertical().id_salt("sql-explain-flame").max_height((body_h - 80.0).max(120.0)).auto_shrink([false, true]).show(ui, |ui| {
        let w = ui.available_width();
        let (area, _) = ui.allocate_exact_size(egui::vec2(w, rows as f32 * (BAR_H + GAP)), Sense::hover());
        for b in &bars {
            let left = left_of(area, w, b.x);
            let width = ((b.w * w as f64) as f32 - SPLIT).max(1.0);
            let rect = egui::Rect::from_min_size(egui::pos2(left, area.top() + b.row as f32 * (BAR_H + GAP)), egui::vec2(width, BAR_H));
            let n = &p.nodes[b.node];
            let name = format!("{} {}", n.label, n.name()).trim().to_string();
            let r = ui.interact(rect, ui.scope_id().with(("flame", b.node)), Sense::click());
            r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, x.sel == Some(b.node), &name));
            let (value, share) = (all[b.node], if total > 0.0 { all[b.node] / total } else { 0.0 });
            paint_bar(ui, rect, n, value, share, x.flame_time, state_of(&r), x.sel == Some(b.node));
            focus_ring(ui, &r, rect, R_SM as f32);
            if scroll_to == Some(b.node) {
                r.scroll_to_me(Some(egui::Align::Center));
            }
            let own = if x.flame_time { n.self_ms } else { n.self_cost }.unwrap_or(0.0);
            let tip = format!("{name}\n{}  ·  {}% of the plan\nOwn work: {}", amount(value, x.flame_time), (share * 100.0).round(), amount(own, x.flame_time));
            if r.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip).clicked() {
                picked = Some(b.node);
            }
            // What the children leave empty under a bar is the node's own work: a quiet fill makes it read as part of
            // the node, so a join that is its own cost does not look like an empty chart.
            let kids: Vec<&flame::Bar> = bars.iter().filter(|k| p.nodes[k.node].parent == Some(b.node)).collect();
            if !kids.is_empty() {
                let end = kids.iter().map(|k| left_of(area, w, k.x + k.w)).fold(rect.left(), f32::max);
                let gap = egui::Rect::from_min_max(egui::pos2(end, rect.bottom() + GAP), egui::pos2(rect.right(), rect.bottom() + GAP + BAR_H));
                if gap.width() >= MIN_GAP && own > 0.0 {
                    ui.painter().rect(gap, R_SM, tint(TRACK(), 40), Stroke::new(1.0, LINE()), egui::StrokeKind::Inside);
                    let own_share = if total > 0.0 { own / total } else { 0.0 };
                    ui.interact(gap, ui.scope_id().with(("flame-own", b.node)), Sense::hover()).on_hover_text(format!("{name}, its own work\n{}  ·  {}% of the plan", amount(own, x.flame_time), (own_share * 100.0).round()));
                }
            }
        }
    });
    if let Some(i) = picked {
        acts.push(XAct::Select(i));
    }
    ui.add_space(12.0);
    legend(ui);
}

/// The bar's fill: the type's color for a scan or an index, a quiet track for joins, sorts and the rest.
fn base(a: Access) -> (Color32, u8) {
    match a {
        Access::Scan => (ALERT(), 56),
        Access::Index => (SUCCESS(), 56),
        Access::Other => (TRACK(), 140),
    }
}

fn left_of(area: egui::Rect, w: f32, x: f64) -> f32 {
    area.left() + (x * w as f64) as f32
}

fn amount(value: f64, time: bool) -> String {
    if time { format!("{} ms", findings::ms_text(value)) } else { figure(value) }
}

/// Which of a bar's texts fit inside it. A text that does not fit whole is left out rather than cut, and the tooltip
/// carries it all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BarText {
    label: bool,
    name: bool,
    amount: bool,
}

fn bar_text(inner_w: f32, label_w: f32, name_w: f32, amount_w: f32) -> BarText {
    let label = label_w <= inner_w;
    BarText { label, name: label && name_w > 0.0 && label_w + name_w <= inner_w, amount: label && amount_w <= inner_w }
}

#[allow(clippy::too_many_arguments)]
fn paint_bar(ui: &egui::Ui, rect: egui::Rect, n: &PlanNode, value: f64, share: f64, time: bool, state: usize, selected: bool) {
    let (c, a) = base(n.access);
    // Translucent fills deepen on hover and press, as `state_fill` does for buttons.
    let fill = tint(c, [a, a.saturating_add(a / 2), a.saturating_mul(2)][state]);
    let p = ui.painter();
    p.rect_filled(rect, R_SM, fill);
    if selected {
        p.rect_stroke(rect, R_SM, Stroke::new(1.5, PRIMARY()), egui::StrokeKind::Inside);
    }
    let inner = rect.shrink2(egui::vec2(PAD, 0.0));
    let fg = if n.access == Access::Other { BRIGHT() } else { c };
    let label = p.layout_job(egui::text::LayoutJob::single_section(n.label.clone(), mono_fmt(TXT_BODY, fg, 500.0)));
    let name = n.name();
    let name_g = (!name.is_empty()).then(|| p.layout_job(egui::text::LayoutJob::single_section(format!("  {name}"), mono_fmt(TXT_BODY, if n.access == Access::Other { TEXT() } else { fg }, 400.0))));
    let line = format!("{}  ·  {}%", amount(value, time), (share * 100.0).round());
    let amount_g = p.layout_job(egui::text::LayoutJob::single_section(line, ui_fmt(TXT_SMALL, MUTED(), 400.0, 0.0)));
    let show = bar_text(inner.width(), label.size().x, name_g.as_ref().map_or(0.0, |g| g.size().x), amount_g.size().x);
    if !show.label {
        return;
    }
    let lw = label.size().x;
    galley_at(p, inner.left(), rect.top() + 11.0, label, fg);
    if let (true, Some(g)) = (show.name, name_g) {
        galley_at(p, inner.left() + lw, rect.top() + 11.0, g, fg);
    }
    if show.amount {
        galley_at(p, inner.left(), rect.top() + 26.0, amount_g, MUTED());
    }
}

fn legend(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (a, label) in [(Access::Scan, "Full scan"), (Access::Index, "Index"), (Access::Other, "Join, sort and other work")] {
            let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
            let (c, alpha) = base(a);
            ui.painter().rect_filled(r, R_SM / 2, tint(c, alpha.saturating_mul(2).max(150)));
            ui.label(RichText::new(label).size(TXT_SMALL).color(MUTED()));
            ui.add_space(16.0);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sql::plan::parse;
    use crate::db::sql::Dialect;

    #[test]
    fn narrow_bars_carry_no_label_and_none_is_cut() {
        // In a 700 px chart Sort and Hash Join span it, and the scans under them are slivers: the user's plan, then a
        // captured one of the same shape.
        let user = crate::db::sql::plan::user_plan("(NOT (ANY ((si.id = (hashed SubPlan 1).col1) AND (b.id = (hashed SubPlan 1).col2))))");
        for text in [user.as_str(), include_str!("../../db/sql/plan/fixtures/pg18-subjoin.json")] {
            check_labels(&parse(Dialect::Postgres, text).unwrap());
        }
    }

    fn check_labels(p: &crate::db::sql::plan::Plan) {
        let w = 700.0_f32;
        let bars = flame::layout_min(p, false, (MIN_BAR / w) as f64);
        // Text is about 8 px a character in the bar's mono font.
        let text_w = |s: &str| s.chars().count() as f32 * 8.0;
        let mut labelled = Vec::new();
        for b in &bars {
            let n = &p.nodes[b.node];
            let px = (b.w * w as f64) as f32 - SPLIT;
            let inner = px - 2.0 * PAD;
            let show = bar_text(inner, text_w(&n.label), text_w(&format!("  {}", n.name())), text_w("10,334  ·  2%"));
            if show.label {
                assert!(text_w(&n.label) <= inner, "{} is cut", n.label);
                labelled.push(n.label.clone());
            } else {
                assert!(!show.name && !show.amount, "{} shows a name or amount without its label", n.label);
            }
        }
        assert!(labelled.iter().any(|l| l.starts_with("Hash Join")), "the join has no label: {labelled:?}");
        assert!(!labelled.iter().any(|l| l == "Hash" || l == "Seq Scan"), "a sliver got a label: {labelled:?}");
        // Every node that weighs something has a bar to point at, however small its share.
        for (i, n) in p.nodes.iter().enumerate() {
            if n.self_cost.is_some_and(|c| c > 0.0) {
                let b = bars.iter().find(|b| b.node == i).unwrap_or_else(|| panic!("{} has no bar", n.label));
                assert!((b.w * w as f64) as f32 >= MIN_BAR - 0.01, "{} is a {} px sliver", n.label, b.w * w as f64);
            }
        }
    }
}
