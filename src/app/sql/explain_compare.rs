//! The Compare tab of the explain dialog: what changed since the last plan of the same statement this session. A line
//! sums it up, a table gives each node's cost (or time, when both ran) before and after, and the findings show which
//! went away and which are new. The matching is in `plan::compare`.

use super::explain::{Pane, SqlExplain, XAct};
use super::explain_panel::{access_color, figure};
use crate::app::*;
use crate::db::sql::plan::compare::{Delta, Diff, State};
use crate::db::sql::plan::{findings, Plan, PlanNode};

const ROW: f32 = 32.0;
const ROW_SUB: f32 = 46.0;
const STEP: f32 = 18.0;
const NUM_W: f32 = 120.0;
const CHANGE_W: f32 = 90.0;

/// The statement of the demo's before and after plans, captured from PostgreSQL 18 before and after an index on
/// bookings (provider, status).
pub(in crate::app) const DEMO_STMT: &str = "SELECT b.id, s.name FROM bookings b JOIN services s ON s.id = b.service_id WHERE b.provider = 'ZEUS_AIR' AND b.status = 'DONE' ORDER BY b.created_at";
pub(in crate::app) const DEMO_BEFORE: &str = include_str!("../../db/sql/plan/fixtures/pg18-fix-before.json");
pub(in crate::app) const DEMO_AFTER: &str = include_str!("../../db/sql/plan/fixtures/pg18-fix-after.json");

impl App {
    /// QUARRY_TEST_PG=explain-compare and explain-flame: the join explained before the index, on Flame, or explained
    /// again after it, on Compare.
    pub(in crate::app) fn demo_explain_pane(&mut self, compare: bool) {
        self.demo_explain_offline(db::sql::Dialect::Postgres, DEMO_STMT, DEMO_BEFORE);
        if compare {
            self.demo_explain_offline(db::sql::Dialect::Postgres, DEMO_STMT, DEMO_AFTER);
        }
        if let Some(Modal::SqlExplain(x)) = &mut self.modal {
            x.pane = if compare { Pane::Compare } else { Pane::Flame };
        }
    }
}

/// The plan this one is compared with and when it was made.
pub(in crate::app) struct Prev {
    pub(in crate::app) plan: Plan,
    /// Unix seconds.
    pub(in crate::app) at: u64,
    pub(in crate::app) diff: Diff,
}

/// "16:42", local time.
pub(super) fn clock(secs: u64) -> String {
    let s = secs as i64;
    let off = db::sql::advanced::local_offset_at(s).unwrap_or(0) as i64;
    crate::db::dates::hms((s + off).rem_euclid(86_400))[..5].to_string()
}

pub(super) fn compare_view(ui: &mut egui::Ui, x: &mut SqlExplain, screen_h: f32, acts: &mut Vec<XAct>) {
    // A HypoPG trial compares the dialog's plan, as before, with the trial's, as after; else the last plan is before.
    let hypo = x.hypo.as_ref().filter(|_| x.hypo_cmp);
    let (after, before, d) = match (hypo, &x.plan, &x.prev) {
        (Some(h), Some(p), _) => (&h.plan, p, &h.diff),
        (None, Some(p), Some(prev)) => (p, &prev.plan, &prev.diff),
        _ => return,
    };
    let timed = d.time.is_some();
    summary_band(ui, d, after.analyzed, hypo.is_some());
    ui.add_space(16.0);
    // Measured after the band, as the tree is after the findings, so the table never runs past the dialog.
    let body_h = (screen_h - 330.0).clamp(260.0, 640.0).min(body_room(ui, 74.0));
    let mut show = None;
    egui::ScrollArea::vertical().id_salt("sql-explain-compare").max_height(body_h).auto_shrink([false, true]).show(ui, |ui| {
        // Header, as labels so screen readers and tests find the columns.
        let (hr, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0), Sense::hover());
        let head = |ui: &mut egui::Ui, text: &str, r: egui::Rect, right: bool| {
            let layout = egui::Layout::left_to_right(egui::Align::Center).with_main_align(if right { egui::Align::Max } else { egui::Align::Min });
            ui.scope_builder(egui::UiBuilder::new().max_rect(r).layout(layout), |ui| ui.label(RichText::new(text).size(TXT_SECTION).color(FAINT()).extra_letter_spacing(0.6)));
        };
        head(ui, "NODE", egui::Rect::from_min_max(hr.min + egui::vec2(8.0, 0.0), egui::pos2(hr.center().x, hr.bottom())), false);
        let mut cx = hr.right() - 8.0;
        for (text, w) in [("CHANGE", CHANGE_W), ("AFTER", NUM_W), ("BEFORE", NUM_W)] {
            head(ui, text, egui::Rect::from_min_max(egui::pos2(cx - w + 4.0, hr.top()), egui::pos2(cx, hr.bottom())), true);
            cx -= w;
        }
        ui.painter().hline(hr.x_range(), hr.bottom() - 0.5, Stroke::new(1.0, LINE()));
        ui.spacing_mut().item_spacing.y = 0.0;
        for r in &d.rows {
            // A node that went away is drawn from the old plan.
            let n = match (r.after, r.before) {
                (Some(a), _) => &after.nodes[a],
                (None, Some(b)) => &before.nodes[b],
                _ => continue,
            };
            let delta = if timed { r.time.unwrap_or_default() } else { r.cost };
            let sub = row_sub(r.was.as_deref(), r.after.is_none(), n);
            let name = format!("{} {}", n.label, n.name()).trim().to_string();
            // The tree shows the dialog's plan, which is the before side of a trial.
            let own = if hypo.is_some() { r.before } else { r.after };
            let resp = side_row(ui, if sub.is_some() { ROW_SUB } else { ROW }, 0.0, own.is_some() && own == x.sel, &name, |pt, rect| paint_row(pt, rect, n, r.depth, sub.as_deref(), delta, timed, r.after.is_none()));
            if resp.clicked() {
                show = own;
            }
        }
        ui.spacing_mut().item_spacing.y = 4.0;
        if !d.findings.is_empty() {
            ui.add_space(20.0);
            ui.label(caption_text("FINDINGS"));
            ui.add_space(6.0);
            let w = ui.available_width();
            let per_row = if w >= 900.0 { 2 } else { 1 };
            let card_w = (w - 12.0 * (per_row - 1) as f32) / per_row as f32;
            for row in d.findings.chunks(per_row) {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    for f in row {
                        ui.allocate_ui_with_layout(egui::vec2(card_w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                            ui.set_width(card_w);
                            finding_card(ui, f.state, &f.title, &f.body);
                        });
                    }
                });
                ui.add_space(8.0);
            }
        }
    });
    // A node of the new plan opens in the tree with its panel.
    if let Some(i) = show {
        (x.pane, x.hypo_cmp) = (Pane::Tree, false);
        acts.push(XAct::Select(i));
    }
}

/// The verdict in a band tinted by whether the plan got better.
fn summary_band(ui: &mut egui::Ui, d: &Diff, analyzed: bool, hypothetical: bool) {
    let better = d.better();
    let color = match better {
        Some(true) => SUCCESS(),
        Some(false) => ALERT(),
        None => MUTED(),
    };
    egui::Frame::new().fill(tint(color, 26)).stroke(Stroke::new(1.0, tint(color, 90))).corner_radius(R_BAR).inner_margin(egui::Margin { left: 14, right: 14, top: 10, bottom: 10 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_top(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), Sense::hover());
            match better {
                Some(true) => glyphs::check(ui.painter(), r.center(), color),
                Some(false) => glyphs::warn(ui.painter(), r.center(), color),
                None => glyphs::info(ui.painter(), r.center(), color),
            }
            ui.vertical(|ui| {
                ui.label(medium(RichText::new(&d.summary).color(BRIGHT())));
                if hypothetical {
                    ui.label(RichText::new("After is hypothetical: HypoPG planned the statement as if the candidate index existed. Nothing was created.").size(TXT_SMALL).color(MUTED()));
                } else if !analyzed {
                    ui.label(RichText::new("Run with ANALYZE to check the time as well as the estimate.").size(TXT_SMALL).color(MUTED()));
                }
            });
        });
    });
}

/// The second line of a row: what the node used to be, or that it went away.
fn row_sub(was: Option<&str>, gone: bool, n: &PlanNode) -> Option<String> {
    if gone {
        return Some("not in the new plan".into());
    }
    let was = was?;
    let mut s = format!("was {was}");
    if let Some(c) = n.index_cond_expr.as_ref().map(crate::db::sql::plan::cond::columns).filter(|c| !c.is_empty()) {
        s = format!("{s}  ·  Index Cond: {}", c.join(", "));
    }
    Some(s)
}

#[allow(clippy::too_many_arguments)]
fn paint_row(p: &egui::Painter, rect: egui::Rect, n: &PlanNode, depth: usize, sub: Option<&str>, d: Delta, timed: bool, gone: bool) {
    let top = if sub.is_some() { rect.top() + 16.0 } else { rect.center().y };
    let cols_w = NUM_W * 2.0 + CHANGE_W;
    let node_area = egui::Rect::from_min_max(rect.min, egui::pos2(rect.right() - cols_w - 8.0, rect.bottom()));
    let np = p.with_clip_rect(node_area.intersect(p.clip_rect()));
    for l in 0..depth {
        np.vline(rect.left() + l as f32 * STEP + 5.5, rect.y_range(), Stroke::new(1.0, LINE()));
    }
    let mut x = rect.left() + depth as f32 * STEP;
    let col = if gone { FAINT() } else { access_color(n.access) };
    let g = np.layout_job(egui::text::LayoutJob::single_section(n.label.clone(), mono_fmt(TXT_BODY, if col == TEXT() { BRIGHT() } else { col }, 500.0)));
    if col != TEXT() && !gone {
        let chip = egui::Rect::from_min_size(egui::pos2(x, top - 10.0), egui::vec2(g.size().x + 12.0, 20.0));
        np.rect_filled(chip, R_SM, tint(col, 36));
        galley_at(&np, x + 6.0, top, g.clone(), col);
        x = chip.right() + 8.0;
    } else {
        let w = g.size().x;
        galley_at(&np, x, top, g, col);
        x += w + 8.0;
    }
    let name = n.name();
    if !name.is_empty() {
        let g = np.layout_job(egui::text::LayoutJob::single_section(name, mono_fmt(TXT_BODY, if gone { FAINT() } else { TEXT() }, 400.0)));
        galley_at(&np, x, top, g, TEXT());
    }
    if let Some(s) = sub {
        let g = np.layout_job(egui::text::LayoutJob::single_section(s.to_string(), mono_fmt(TXT_SMALL, FAINT(), 400.0)));
        galley_at(&np, rect.left() + depth as f32 * STEP + 2.0, rect.top() + 34.0, g, FAINT());
    }
    let amount = |v: Option<f64>| v.map(|v| if timed { format!("{} ms", findings::ms_text(v)) } else { figure(v) }).unwrap_or_default();
    let right = rect.right();
    let at = |text: String, color: Color32, edge: f32| {
        let g = p.layout_job(egui::text::LayoutJob::single_section(text, mono_fmt(TXT_BODY, color, 400.0)));
        galley_at(p, edge - g.size().x, top, g, color);
    };
    at(amount(d.before), FAINT(), right - CHANGE_W - NUM_W);
    at(amount(d.after), TEXT(), right - CHANGE_W);
    // The change as a chip: green when it went down, red when it went up, quiet when it held.
    let text = d.text();
    if !text.is_empty() {
        let color = match d.change() {
            _ if d.same() || d.before.is_none() || d.after.is_none() => MUTED(),
            Some(c) if c < 0.0 => SUCCESS(),
            Some(_) => ALERT(),
            None => MUTED(),
        };
        let g = p.layout_job(egui::text::LayoutJob::single_section(text, mono_fmt(TXT_SMALL, color, 500.0)));
        let chip = egui::Rect::from_min_size(egui::pos2(right - g.size().x - 12.0, top - 10.0), egui::vec2(g.size().x + 12.0, 20.0));
        p.rect_filled(chip, R_SM, tint(color, 36));
        galley_at(p, chip.left() + 6.0, top, g, color);
    }
}

/// A finding of either plan with what became of it: struck through once resolved.
fn finding_card(ui: &mut egui::Ui, state: State, title: &str, body: &str) {
    let (color, tag) = match state {
        State::Resolved => (SUCCESS(), "resolved"),
        State::New => (WARNING(), "new"),
        State::Same => (MUTED(), "still there"),
    };
    egui::Frame::new().fill(RAISED()).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_LG).inner_margin(egui::Margin { left: 14, right: 12, top: 10, bottom: 10 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), Sense::hover());
            match state {
                State::Resolved => glyphs::check(ui.painter(), r.center(), color),
                State::New => glyphs::warn(ui.painter(), r.center(), color),
                State::Same => glyphs::info(ui.painter(), r.center(), color),
            }
            let t = semibold(RichText::new(title));
            ui.label(if state == State::Resolved { t.strikethrough().color(MUTED()) } else { t.color(BRIGHT()) });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::Frame::new().fill(tint(color, 36)).corner_radius(R_SM).inner_margin(egui::Margin::symmetric(6, 2)).show(ui, |ui| ui.label(RichText::new(tag).size(TXT_SMALL).color(color)));
            });
        });
        if !body.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(24.0);
                ui.label(RichText::new(body).size(TXT_SMALL).color(MUTED()));
            });
        }
    });
}
