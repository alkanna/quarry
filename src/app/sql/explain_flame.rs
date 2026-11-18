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
    let bars = flame::layout(p, x.flame_time);
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
            let left = area.left() + (b.x * w as f64) as f32;
            let width = ((b.w * w as f64) as f32 - SPLIT).max(1.0);
            let rect = egui::Rect::from_min_size(egui::pos2(left, area.top() + b.row as f32 * (BAR_H + GAP)), egui::vec2(width, BAR_H));
            let n = &p.nodes[b.node];
            let name = format!("{} {}", n.label, n.name()).trim().to_string();
            let r = ui.interact(rect, ui.id().with(("flame", b.node)), Sense::click());
            r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, x.sel == Some(b.node), &name));
            let (value, share) = (all[b.node], if total > 0.0 { all[b.node] / total } else { 0.0 });
            paint_bar(ui, rect, n, value, share, x.flame_time, state_of(&r), x.sel == Some(b.node));
            focus_ring(ui, &r, rect, R_SM as f32);
            if scroll_to == Some(b.node) {
                r.scroll_to_me(Some(egui::Align::Center));
            }
            if r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                picked = Some(b.node);
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
    if inner.width() < 8.0 {
        return;
    }
    let cp = p.with_clip_rect(inner.intersect(p.clip_rect()));
    let fg = if n.access == Access::Other { BRIGHT() } else { c };
    let mut job = egui::text::LayoutJob::default();
    job.append(&n.label, 0.0, mono_fmt(TXT_BODY, fg, 500.0));
    let name = n.name();
    if !name.is_empty() {
        job.append(&format!("  {name}"), 0.0, mono_fmt(TXT_BODY, if n.access == Access::Other { TEXT() } else { fg }, 400.0));
    }
    galley_at(&cp, inner.left(), rect.top() + 11.0, cp.layout_job(job), fg);
    let amount = if time { format!("{} ms", findings::ms_text(value)) } else { figure(value) };
    let line = format!("{amount}  ·  {}%", (share * 100.0).round());
    let g = cp.layout_job(egui::text::LayoutJob::single_section(line, ui_fmt(TXT_SMALL, MUTED(), 400.0, 0.0)));
    galley_at(&cp, inner.left(), rect.top() + 26.0, g, MUTED());
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
