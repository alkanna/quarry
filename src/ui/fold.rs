//! Folders that open and close with an animation, shared by the Connections panel, the Library tree, the rail and the
//! explorer: how far open a folder is drawn, the clipped body that grows under its header, and the chevron that turns.
//! A fold animates only from `start`, called where a person toggles it. A folder that a filter, a search or a reveal
//! opens shows open at once, and one that is not drawn while it folds never plays a stale animation later.

use super::theme::FOLD_S;
use eframe::egui::{self, Color32, Stroke};
use std::collections::HashMap;

/// A fold in flight: when it started, from what openness, and which way.
#[derive(Clone, Copy, Debug)]
struct Fold {
    t0: f64,
    from: f32,
    open: bool,
}

#[derive(Clone, Default)]
struct Folds(HashMap<egui::Id, Fold>);

fn folds_id() -> egui::Id {
    egui::Id::unique("quarry-folds")
}

/// The easing of every fold and of a dropped row landing: fast at first, slowing to rest.
pub(crate) fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Where a fold that started at `from` stands `dt` seconds later on its way to `open`, or None once it has arrived.
fn at(from: f32, open: bool, dt: f32) -> Option<f32> {
    let goal = if open { 1.0 } else { 0.0 };
    // A fold reversed half way takes half the time back, so it never moves slower than a full one.
    let span = FOLD_S * (goal - from).abs();
    if span <= 0.0 || dt >= span {
        return None;
    }
    Some(from + (goal - from) * ease_out(dt.max(0.0) / span))
}

/// Starts the folder `id` moving towards `open` from wherever it is drawn now. Nothing starts while egui's animations
/// are off: the screenshot mode and the UI tests turn them off, and their pictures and clicks want the settled layout.
pub(crate) fn start(ctx: &egui::Context, id: egui::Id, open: bool) {
    let now = ctx.input(|i| i.time);
    let on = ctx.global_style().animation_time > 0.0;
    ctx.data_mut(|d| {
        let f = d.get_temp_mut_or_default::<Folds>(folds_id());
        let from = f.0.get(&id).and_then(|g| at(g.from, g.open, (now - g.t0) as f32)).unwrap_or(if open { 0.0 } else { 1.0 });
        if on {
            f.0.insert(id, Fold { t0: now, from, open });
        } else {
            f.0.remove(&id);
        }
    });
    if on {
        ctx.request_repaint();
    }
}

/// How far open the folder `id` is drawn, from 0 closed to 1 open, when it is `open` now. A fold heading the other way
/// was overtaken, by a filter opening the folder say, and is dropped so the folder shows its state at once.
pub(crate) fn openness(ctx: &egui::Context, id: egui::Id, open: bool) -> f32 {
    let now = ctx.input(|i| i.time);
    let v = ctx.data_mut(|d| {
        let f = d.get_temp_mut_or_default::<Folds>(folds_id());
        let g = *f.0.get(&id)?;
        let v = (g.open == open).then(|| at(g.from, g.open, (now - g.t0) as f32)).flatten();
        if v.is_none() {
            f.0.remove(&id);
        }
        v
    });
    match v {
        Some(v) => {
            ctx.request_repaint();
            v
        }
        None if open => 1.0,
        None => 0.0,
    }
}

/// Draws a folder's body under its header, `t` open (from `openness`). It is clipped to that share of its height, so it
/// grows and shrinks and what follows moves with it, and it fades with it. Bodies are drawn through here when settled
/// open too, so their rows keep their widget ids when an animation ends. `guess` is the height until the body has been
/// measured once. A closing body is not `live`: it is drawn but takes no clicks. Nothing is drawn once `t` is 0.
pub(crate) fn body<R>(ui: &mut egui::Ui, id: egui::Id, t: f32, guess: f32, live: bool, add: impl FnOnce(&mut egui::Ui) -> R) -> Option<R> {
    // Every path takes the same two auto ids (the child and the space after it), so the rows after the body keep their
    // ids when a closing fold settles, and a press made just before then still counts as a click when it is let go.
    if t <= 0.0 {
        ui.skip_ahead_auto_ids(2);
        return None;
    }
    let gap = ui.spacing().item_spacing.y;
    let hid = id.with("fold-height");
    let full = ui.data(|d| d.get_temp::<f32>(hid)).unwrap_or(guess);
    // The gap after the body shrinks with it, or what follows would jump by a gap as the fold starts and ends.
    let shown = if t >= 1.0 { f32::INFINITY } else { t * (full + gap) - gap };
    let top = ui.cursor().min;
    let w = ui.available_width();
    let max = egui::Rect::from_min_size(top, egui::vec2(w, ui.available_height().max(full)));
    let mut child = ui.new_child(egui::UiBuilder::new().scope_id(id.with("fold-body")).max_rect(max).layout(*ui.layout()));
    if t < 1.0 {
        let mut clip = child.clip_rect();
        clip.max.y = clip.max.y.min(top.y + shown.max(0.0));
        child.set_clip_rect(clip);
        child.multiply_opacity(t);
    }
    if !live {
        // Disabling also dims, which would make the rows jump to half their opacity before they fade.
        let o = child.opacity();
        child.disable();
        child.set_opacity(o);
    }
    let r = add(&mut child);
    let h = child.min_rect().height();
    ui.data_mut(|d| d.insert_temp(hid, h));
    if shown < 0.0 {
        ui.add_space(shown);
        ui.skip_ahead_auto_ids(1);
    } else {
        ui.advance_cursor_after_rect(egui::Rect::from_min_size(top, egui::vec2(w, shown.min(h))));
    }
    Some(r)
}

/// The three points of a folder's chevron `t` open around `c`: pointing right when closed and down when open.
fn chevron_points(c: egui::Pos2, t: f32) -> [egui::Pos2; 3] {
    // The closed shape turned a quarter clockwise is exactly the open one.
    let (s, co) = if t >= 1.0 { (1.0, 0.0) } else { (t.max(0.0) * std::f32::consts::FRAC_PI_2).sin_cos() };
    let turn = |x: f32, y: f32| c + egui::vec2(x * co - y * s, x * s + y * co);
    [turn(-2.0, -4.0), turn(3.0, 0.0), turn(-2.0, 4.0)]
}

/// A folder's chevron `t` open, painted as a shape since the glyph sits off center. It turns as the folder opens.
pub(crate) fn chevron(p: &egui::Painter, c: egui::Pos2, t: f32, color: Color32) {
    p.add(egui::Shape::convex_polygon(chevron_points(c, t).to_vec(), color, Stroke::NONE));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_easing_starts_fast_and_rests_at_its_ends() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert_eq!(ease_out(-1.0), 0.0);
        assert_eq!(ease_out(2.0), 1.0);
        assert!(ease_out(0.5) > 0.5, "ahead of a linear move half way");
        let steps: Vec<f32> = (0..=10).map(|i| ease_out(i as f32 / 10.0)).collect();
        assert!(steps.windows(2).all(|w| w[1] >= w[0]), "never moves back");
    }

    #[test]
    fn a_fold_arrives_within_its_time_and_a_reversed_one_takes_what_is_left() {
        assert!(at(0.0, true, 0.0).is_some_and(|v| v == 0.0));
        let mid = at(0.0, true, FOLD_S / 2.0).unwrap();
        assert!(mid > 0.5 && mid < 1.0, "{mid}");
        assert_eq!(at(0.0, true, FOLD_S), None);
        let closing = at(1.0, false, FOLD_S / 2.0).unwrap();
        assert!(closing > 0.0 && closing < 0.5, "{closing}");
        // From half open back to closed is half the distance and half the time.
        assert!(at(0.5, false, FOLD_S * 0.4).is_some());
        assert_eq!(at(0.5, false, FOLD_S * 0.5), None);
        assert_eq!(at(1.0, true, 0.0), None, "nothing to do");
    }

    /// Runs one frame at `time` seconds and returns what `f` gives.
    fn frame<R>(ctx: &egui::Context, time: f64, mut f: impl FnMut(&egui::Context) -> R) -> R {
        let mut out = None;
        let input = egui::RawInput { time: Some(time), predicted_dt: 1.0 / 60.0, ..Default::default() };
        // The font atlas comes out as a texture delta, which egui asserts is handled before it is dropped.
        ctx.run_ui(input, |ui| out = Some(f(ui.ctx()))).textures_delta.clear();
        out.unwrap()
    }

    #[test]
    fn a_started_fold_eases_open_and_settles_and_off_animations_settle_at_once() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|s| s.animation_time = 0.2);
        let id = egui::Id::unique("fold-test");
        assert_eq!(frame(&ctx, 0.0, |c| openness(c, id, false)), 0.0);
        frame(&ctx, 0.0, |c| start(c, id, true));
        let mid = frame(&ctx, 0.05, |c| openness(c, id, true));
        assert!(mid > 0.0 && mid < 1.0, "{mid}");
        assert_eq!(frame(&ctx, 0.05 + FOLD_S as f64, |c| openness(c, id, true)), 1.0);
        // Reversed half way, it closes from where it was drawn.
        frame(&ctx, 1.0, |c| start(c, id, false));
        let back = frame(&ctx, 1.02, |c| openness(c, id, false));
        assert!(back > 0.0 && back < 1.0, "{back}");
        frame(&ctx, 1.02, |c| start(c, id, true));
        let again = frame(&ctx, 1.03, |c| openness(c, id, true));
        assert!(again >= back && again < 1.0, "{back} then {again}");
        // A filter that opens a closing folder overtakes the fold.
        frame(&ctx, 2.0, |c| start(c, id, false));
        assert_eq!(frame(&ctx, 2.01, |c| openness(c, id, true)), 1.0);

        ctx.all_styles_mut(|s| s.animation_time = 0.0);
        frame(&ctx, 3.0, |c| start(c, id, false));
        assert_eq!(frame(&ctx, 3.0, |c| openness(c, id, false)), 0.0);
    }

    #[test]
    fn the_chevron_turns_from_right_to_down() {
        let c = egui::pos2(10.0, 10.0);
        let set = |pts: [egui::Pos2; 3]| {
            let mut v: Vec<(i32, i32)> = pts.iter().map(|p| ((p.x * 100.0).round() as i32, (p.y * 100.0).round() as i32)).collect();
            v.sort();
            v
        };
        let p = |x: f32, y: f32| c + egui::vec2(x, y);
        assert_eq!(set(chevron_points(c, 0.0)), set([p(-2.0, -4.0), p(3.0, 0.0), p(-2.0, 4.0)]));
        assert_eq!(set(chevron_points(c, 1.0)), set([p(-4.0, -2.0), p(4.0, -2.0), p(0.0, 3.0)]));
        let half = chevron_points(c, 0.5)[1];
        assert!(half.x > c.x && half.y > c.y, "the tip points down and right half way: {half:?}");
    }
}
