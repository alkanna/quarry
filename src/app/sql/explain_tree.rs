//! The plan tree of the explain dialog: one painted row per node with indent guides, the node's type colored by how it
//! reads (red for a full scan, green through an index), what it reads, a mark when a finding is about it, and columns
//! for its own share, rows and cost. After ANALYZE the columns turn to time and the estimate against what ran.

use super::explain::{SqlExplain, XAct};
use super::explain_panel::{access_color, amount, figure, meter, paint_mark, severity_color, share_color, tag_chip, HEAD_H};
use crate::app::*;
use crate::db::sql::plan::{cond, findings, Op, Plan, PlanNode, Severity};

/// A row's height, with a second line, and the indent of one level.
pub(super) const ROW: f32 = 32.0;
pub(super) const ROW_SUB: f32 = 46.0;
const STEP: f32 = 18.0;

#[derive(Clone, Copy, PartialEq)]
enum Col {
    SelfCost,
    Rows,
    Kept,
    Cost,
    Time,
    EstActual,
    Off,
}

impl Col {
    fn head(self) -> &'static str {
        match self {
            Col::SelfCost => "SELF COST",
            Col::Rows => "ROWS",
            Col::Kept => "KEPT",
            Col::Cost => "COST",
            Col::Time => "TIME",
            Col::EstActual => "EST. > ACTUAL",
            Col::Off => "OFF",
        }
    }

    fn width(self) -> f32 {
        match self {
            Col::SelfCost | Col::Time => 130.0,
            Col::EstActual => 150.0,
            Col::Kept | Col::Off => 60.0,
            Col::Rows | Col::Cost => 84.0,
        }
    }

    fn tip(self) -> Option<&'static str> {
        match self {
            Col::SelfCost => Some("The node's own cost, without the nodes below it, as a share of the plan. Approximate for subplans and nodes that stop early."),
            Col::Time => Some("The node's own time over all its loops, without the nodes below it."),
            Col::Kept => Some("The server's guess at the share of rows its filter keeps."),
            Col::Off => Some("How many times the estimate was off from the rows that came back."),
            _ => None,
        }
    }
}

/// The columns that fit `w`, the least useful going first when the dialog is narrow.
fn columns(p: &Plan, w: f32) -> Vec<Col> {
    let timed = p.analyzed && p.nodes.iter().any(|n| n.self_ms.is_some());
    let mut cols = if timed { vec![Col::Time, Col::EstActual, Col::Off] } else { vec![Col::SelfCost, Col::Rows, Col::Kept, Col::Cost] };
    if !p.nodes.iter().any(|n| n.filtered_pct.is_some()) {
        cols.retain(|c| *c != Col::Kept);
    }
    if w < 520.0 {
        cols.retain(|c| !matches!(c, Col::Rows | Col::Kept | Col::EstActual));
    }
    if w < 440.0 {
        cols.retain(|c| !matches!(c, Col::Cost | Col::Off));
    }
    cols
}

pub(super) fn plan_tree(ui: &mut egui::Ui, x: &mut SqlExplain, body_h: f32, acts: &mut Vec<XAct>) {
    let Some(p) = &x.plan else { return };
    if p.nodes.is_empty() {
        ui.label(RichText::new("The server returned no plan nodes. See Raw.").color(MUTED()));
        return;
    }
    let cols = columns(p, ui.available_width());
    let marks: Vec<Option<Severity>> = (0..p.nodes.len()).map(|i| p.findings.iter().filter(|f| f.node == Some(i)).map(|f| f.severity).min()).collect();
    // Header.
    let (hr, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), HEAD_H), Sense::hover());
    // Labels rather than painted text, so screen readers and tests find the columns.
    let head = |ui: &mut egui::Ui, text: &str, r: egui::Rect, right: bool| {
        // A main align of Max does not move a single label in a horizontal layout; right to left does.
        let layout = if right { egui::Layout::right_to_left(egui::Align::Center) } else { egui::Layout::left_to_right(egui::Align::Center) };
        ui.scope_builder(egui::UiBuilder::new().max_rect(r).layout(layout), |ui| ui.label(RichText::new(text).size(TXT_SECTION).color(FAINT()).extra_letter_spacing(0.6))).inner
    };
    head(ui, "NODE", egui::Rect::from_min_max(hr.min + egui::vec2(8.0, 0.0), egui::pos2(hr.center().x, hr.bottom())), false);
    let mut cx = hr.right() - 8.0;
    for c in cols.iter().rev() {
        let right = !matches!(c, Col::SelfCost | Col::Time);
        let r = egui::Rect::from_min_max(egui::pos2(cx - c.width() + 4.0, hr.top()), egui::pos2(cx, hr.bottom()));
        let l = head(ui, c.head(), r, right);
        if let Some(tip) = c.tip() {
            l.on_hover_text(tip);
        }
        cx -= c.width();
    }
    ui.painter().hline(hr.x_range(), hr.bottom() - 0.5, Stroke::new(1.0, LINE()));
    let scroll_to = x.scroll_to.take();
    egui::ScrollArea::vertical().id_salt("sql-explain-tree").max_height((body_h - HEAD_H).max(120.0)).auto_shrink([false, true]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for (i, n) in p.nodes.iter().enumerate() {
            let sub = node_sub(n);
            let name = format!("{} {}", n.label, n.name()).trim().to_string();
            let r = side_row(ui, if sub.is_some() { ROW_SUB } else { ROW }, 0.0, x.sel == Some(i), &name, |pt, rect| paint_row(pt, rect, n, sub.as_deref(), marks[i], &cols));
            if scroll_to == Some(i) {
                r.scroll_to_me(Some(egui::Align::Center));
            }
            if r.clicked() {
                acts.push(XAct::Select(i));
            }
        }
    });
}

/// The second line of a row: the columns its filter tests, or the condition it searches or joins on.
fn node_sub(n: &PlanNode) -> Option<String> {
    if let Some(e) = &n.filter.expr {
        let c = cond::columns(e);
        if !c.is_empty() {
            return Some(format!("Filter: {}", c.join(", ")));
        }
    }
    if !n.filter.is_empty() {
        return Some("Filter".into());
    }
    if let Some(c) = n.index_cond.expr.as_ref().map(cond::columns).filter(|c| !c.is_empty()) {
        return Some(format!("Index Cond: {}", c.join(", ")));
    }
    if !n.join_cond.is_empty() {
        let line = n.join_cond.expr.as_ref().map(|e| cond::one_line(e, 3)).unwrap_or_else(|| n.join_cond.text.trim_start_matches('(').trim_end_matches(')').to_string());
        let short: String = line.chars().take(60).collect();
        return Some(if n.op.is_join() || n.op == Op::Hash { short } else { format!("Lookup: {short}") });
    }
    if !n.sort_key.is_empty() {
        return Some(format!("Sort Key: {}", n.sort_key));
    }
    (!n.group_key.is_empty()).then(|| format!("Group Key: {}", n.group_key))
}

fn paint_row(p: &egui::Painter, rect: egui::Rect, n: &PlanNode, sub: Option<&str>, mark: Option<Severity>, cols: &[Col]) {
    let top = if sub.is_some() { rect.top() + 16.0 } else { rect.center().y };
    let cols_w: f32 = cols.iter().map(|c| c.width()).sum();
    let node_area = egui::Rect::from_min_max(rect.min, egui::pos2(rect.right() - cols_w - 8.0, rect.bottom()));
    let np = p.with_clip_rect(node_area.intersect(p.clip_rect()));
    let x = paint_node(&np, rect, top, n.depth, n, sub, false);
    if let Some(sev) = mark {
        paint_mark(&np, egui::pos2(x + 6.0, top), sev.into(), severity_color(sev));
    }
    // Columns, right to left.
    let mut cx = rect.right();
    for c in cols.iter().rev() {
        let left = cx - c.width();
        let right_text = |text: String, color: Color32| {
            let g = p.layout_job(egui::text::LayoutJob::single_section(text, mono_fmt(TXT_BODY, color, 400.0)));
            galley_at(p, cx - g.size().x, top, g, color);
        };
        match c {
            Col::SelfCost | Col::Time => {
                let (share, text) = match c {
                    Col::Time => (n.share, n.self_ms.map(|m| amount(m, true))),
                    _ => (n.share, n.share.map(|s| format!("{}%", (s * 100.0).round()))),
                };
                if let (Some(s), Some(t)) = (share, text) {
                    let color = share_color(s);
                    let g = p.layout_job(egui::text::LayoutJob::single_section(t, mono_fmt(TXT_SMALL, if s >= 0.2 { color } else { MUTED() }, 500.0)));
                    let tw = g.size().x;
                    galley_at(p, cx - tw, top, g, color);
                    let track = egui::Rect::from_min_max(egui::pos2(left + 4.0, top - 3.0), egui::pos2(cx - tw - 10.0, top + 3.0));
                    if track.width() > 12.0 {
                        meter(p, track, Some(s), 3.0, color);
                    }
                }
            }
            Col::Rows => right_text(n.rows.map(figure).unwrap_or_default(), TEXT()),
            Col::Kept => right_text(n.filtered_pct.map(|f| format!("{f:.0}%")).unwrap_or_default(), if n.filtered_pct.is_some_and(|f| f <= 20.0) { WARNING() } else { MUTED() }),
            Col::Cost => right_text(n.own_cost.or(n.cost).map(figure).unwrap_or_default(), TEXT()),
            Col::EstActual => {
                if n.never_ran {
                    right_text("never ran".into(), FAINT());
                } else if let (Some(e), Some(a)) = (n.rows, n.actual_rows) {
                    let mut job = egui::text::LayoutJob::default();
                    job.append(&figure(e), 0.0, mono_fmt(TXT_BODY, FAINT(), 400.0));
                    job.append(" > ", 0.0, mono_fmt(TXT_BODY, FAINT(), 400.0));
                    job.append(&figure(a), 0.0, mono_fmt(TXT_BODY, TEXT(), 400.0));
                    let g = p.layout_job(job);
                    galley_at(p, cx - g.size().x, top, g, TEXT());
                }
            }
            Col::Off => {
                if let Some(off) = n.off() {
                    if off >= 2.0 {
                        let color = if off >= findings::OFF_WARN { WARNING() } else { MUTED() };
                        let g = p.layout_job(egui::text::LayoutJob::single_section(findings::factor(off), mono_fmt(TXT_SMALL, color, 500.0)));
                        tag_chip(p, cx - g.size().x - 12.0, top, g, color);
                    } else {
                        // The check's ink ends 4.5 px right of its center; keep it on the column's right edge like the chips.
                        glyphs::check(p, egui::pos2(cx - 4.5, top), SUCCESS());
                    }
                }
            }
        }
        cx = left;
    }
}

/// The left part of a plan row, shared with Compare: indent guides, the node's type in a chip tinted by how it reads,
/// what it reads, and the second line. A node `gone` from the new plan is drawn faint, without its chip. Returns where a
/// mark after the name goes.
pub(super) fn paint_node(np: &egui::Painter, rect: egui::Rect, top: f32, depth: usize, n: &PlanNode, sub: Option<&str>, gone: bool) -> f32 {
    // Indent guides, one per level above the node.
    for l in 0..depth {
        np.vline(rect.left() + l as f32 * STEP + 5.5, rect.y_range(), Stroke::new(1.0, LINE()));
    }
    let mut x = rect.left() + depth as f32 * STEP;
    let col = if gone { FAINT() } else { access_color(n.access) };
    let g = np.layout_job(egui::text::LayoutJob::single_section(n.label.clone(), mono_fmt(TXT_BODY, if col == TEXT() { BRIGHT() } else { col }, 500.0)));
    if col != TEXT() && !gone {
        x = tag_chip(np, x, top, g, col).right() + 8.0;
    } else {
        let w = g.size().x;
        galley_at(np, x, top, g, col);
        x += w + 8.0;
    }
    let name = n.name();
    if !name.is_empty() {
        let g = np.layout_job(egui::text::LayoutJob::single_section(name, mono_fmt(TXT_BODY, if gone { FAINT() } else { TEXT() }, 400.0)));
        let w = g.size().x;
        galley_at(np, x, top, g, TEXT());
        x += w + 8.0;
    }
    if let Some(s) = sub {
        let g = np.layout_job(egui::text::LayoutJob::single_section(s.to_string(), mono_fmt(TXT_SMALL, FAINT(), 400.0)));
        galley_at(np, rect.left() + depth as f32 * STEP + 2.0, rect.top() + 34.0, g, FAINT());
    }
    x
}
