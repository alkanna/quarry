//! Drag and drop for rows: a row that can be dragged, the ghost that follows the pointer, and the marks a target shows.
//! Rows keep their own click, right-click and focus, so a press that moves less than egui's drag threshold is still a click.
//! The payload travels through `egui::DragAndDrop`; targets read it with `Response::dnd_hover_payload` and
//! `dnd_release_payload`. The `table.rs` grip pattern does not fit rows that are also clickable.

use super::theme::*;
use super::widgets::{focus_ring, menu_look, side_row_sense};
use eframe::egui::{self, Sense, Stroke};
use std::time::Duration;

/// How long a drag must rest on a closed folder before it opens.
const HOVER_OPEN_S: f64 = 0.6;

/// `side_row` that can also be dragged.
pub(crate) fn side_row_drag(ui: &mut egui::Ui, height: f32, indent: f32, active: bool, label: &str, paint: impl FnOnce(&egui::Painter, egui::Rect)) -> egui::Response {
    side_row_sense(ui, Sense::click_and_drag(), height, indent, active, label, paint)
}

/// The insertion line between two rows: 2 px in the accent, across `x`.
pub(crate) fn drop_line(p: &egui::Painter, x: egui::Rangef, y: f32) {
    p.hline(x, y, Stroke::new(2.0, PRIMARY()));
}

/// The mark of a drop that lands inside a row or folder: a fill and an outline.
pub(crate) fn drop_fill(p: &egui::Painter, rect: egui::Rect) {
    p.rect(rect, R_SM, tint(PRIMARY(), 28), Stroke::new(1.0, PRIMARY()), egui::StrokeKind::Inside);
}

/// True once a drag has rested on the target `id` for 0.6 s of egui time, to open a closed folder under it. Call it every
/// frame with whether a drag is over the target; leaving the target starts the wait again. A repaint is scheduled for the
/// moment it is due, since nothing else redraws a held pointer.
pub(crate) fn hover_open(ctx: &egui::Context, id: egui::Id, hovering: bool) -> bool {
    if !hovering {
        ctx.data_mut(|d| d.remove_temp::<f64>(id));
        return false;
    }
    let now = ctx.input(|i| i.time);
    let since = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(id, || now));
    let left = since + HOVER_OPEN_S - now;
    if left > 0.0 {
        ctx.request_repaint_after(Duration::from_secs_f64(left));
    }
    left <= 0.0
}

// The reorder engine. One call draws a whole list whose items can be dragged to a new place, along one axis.
//
// Each item gets a drag-only sense under whatever `draw` adds, registered before any item is drawn: egui reports a
// press to both a click widget on top and a drag widget below it, so the item's own click, right-click and focus still
// work, and a handle that senses clicks and drags itself (a column's resize edge) stays on top and wins. The lift only
// starts once the press is decidedly a drag, past egui's click distance or click time.

/// Within this distance of a scroll area's edge a drag scrolls it.
const EDGE: f32 = 24.0;
/// Auto-scroll speed in points per second with the pointer on the edge or past it.
const SCROLL_MAX: f32 = 900.0;
/// How long the other items take to slide aside.
const SLIDE_S: f32 = 0.12;
/// How long a dropped or cancelled item takes to land in its slot.
const SETTLE_S: f64 = 0.16;
/// How far across the axis a release may stray from the list and still drop.
const CROSS_SLACK: f32 = 40.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Axis {
    Vertical,
    Horizontal,
}

impl Axis {
    fn main(self, v: egui::Vec2) -> f32 {
        match self {
            Axis::Vertical => v.y,
            Axis::Horizontal => v.x,
        }
    }

    fn vec(self, main: f32, cross: f32) -> egui::Vec2 {
        match self {
            Axis::Vertical => egui::vec2(cross, main),
            Axis::Horizontal => egui::vec2(main, cross),
        }
    }
}

/// What `reorder` draws: the items' sizes along the axis and the rules for where they may go.
#[derive(Clone, Copy)]
pub(crate) struct Items<'a> {
    axis: Axis,
    sizes: &'a [f32],
    cross: Option<f32>,
    keys: Option<&'a [egui::Id]>,
    blocks: &'a [usize],
    into: Option<&'a [bool]>,
    menu: bool,
}

impl<'a> Items<'a> {
    /// A column of items as wide as the space left.
    pub(crate) fn vertical(sizes: &'a [f32]) -> Self {
        Self { axis: Axis::Vertical, sizes, cross: None, keys: None, blocks: &[], into: None, menu: false }
    }

    /// A row of items `height` tall.
    pub(crate) fn horizontal(sizes: &'a [f32], height: f32) -> Self {
        Self { axis: Axis::Horizontal, sizes, cross: Some(height), keys: None, blocks: &[], into: None, menu: false }
    }

    /// Gives each item a right-click menu with Move up and Move down (left and right for a row), for a list whose items
    /// have no menu of their own. A list with one calls `move_items` inside it instead.
    pub(crate) fn menu(mut self) -> Self {
        self.menu = true;
        self
    }

    /// One stable key per item, such as a column name or a connection id. Each item's `Ui` is keyed by it, so its
    /// widgets keep their ids and focus when it moves. Without keys the index is the key.
    pub(crate) fn keys(mut self, keys: &'a [egui::Id]) -> Self {
        self.keys = Some(keys);
        self
    }

    /// Indices where a block ends and the next starts: an item only moves inside its own block, and a drop past the
    /// edge stops at it. Pinned columns are `&[pinned_n]`.
    pub(crate) fn blocks(mut self, blocks: &'a [usize]) -> Self {
        self.blocks = blocks;
        self
    }

    /// Which items take a drop into them, such as a folder or a rail tile that makes a group. While the pointer is on
    /// the middle half of one the gap holds still, the item shows the drop fill, and a release returns a `Move` with
    /// `into` set.
    pub(crate) fn into(mut self, into: &'a [bool]) -> Self {
        self.into = Some(into);
        self
    }
}

/// How `reorder` asks an item to draw itself.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ItemState {
    /// This is the dragged item, drawn floating over the page. Skip its menus, popups and buttons: they would follow
    /// the pointer.
    pub lifted: bool,
    /// Some item of the list is being dragged or landing; hover marks and per-row buttons read better hidden.
    pub busy: bool,
    /// How far the item is slid from its slot along the axis to open the gap.
    pub offset: f32,
}

/// A finished drag or keyboard move. `to` is the item's index once moved (remove at `from`, insert at `to`), or with
/// `into` the index of the item it was dropped into, in the order before the move.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Move {
    pub from: usize,
    pub to: usize,
    pub into: bool,
}

impl Move {
    /// `to` as an insert-before index into the list before the item is taken out, for APIs that read it that way
    /// (`ColAct::MoveTo`).
    pub(crate) fn before(self) -> usize {
        if self.to > self.from { self.to + 1 } else { self.to }
    }

    /// Moves the item in `v`. A move with an index past the end does nothing: the list changed since the move was made,
    /// so its indexes name other items now. A move into an item is the caller's to apply.
    pub(crate) fn apply<T>(self, v: &mut Vec<T>) {
        if !self.into && self.from < v.len() && self.to < v.len() {
            let x = v.remove(self.from);
            v.insert(self.to, x);
        }
    }
}

/// "Move up" and "Move down" (or left and right) for item `k` of `n` in a menu, with the Alt+arrow keys that do the
/// same as their shortcut text, so a list that drags to reorder can also be reordered without a drag or a key. Returns
/// the move picked.
pub(crate) fn move_items(ui: &mut egui::Ui, n: usize, k: usize, blocks: &[usize], horizontal: bool) -> Option<Move> {
    let steps = if horizontal { [("Move left", "Alt+Left", -1), ("Move right", "Alt+Right", 1)] } else { [("Move up", "Alt+Up", -1), ("Move down", "Alt+Down", 1)] };
    let mut out = None;
    for (label, key, step) in steps {
        let to = key_move(blocks, n, k, step);
        if ui.add_enabled(to.is_some(), egui::Button::new(label).shortcut_text(key)).clicked() {
            out = to.map(|to| Move { from: k, to, into: false });
            ui.close();
        }
    }
    out
}

/// One id per item from its name, telling apart items that share a name (two columns of one name in a result or an
/// index) by how many came before, so `reorder` never gives two items one id.
pub(crate) fn unique_keys<T: std::hash::Hash + Eq + std::fmt::Debug>(salt: &str, items: &[T]) -> Vec<egui::Id> {
    let mut seen: std::collections::HashMap<&T, usize> = std::collections::HashMap::new();
    items
        .iter()
        .map(|t| {
            let n = seen.entry(t).or_insert(0);
            *n += 1;
            egui::Id::unique((salt, t, *n))
        })
        .collect()
}

#[derive(Clone, Copy)]
struct Lift {
    from: usize,
    /// Where on the item the press was, along the axis, so it does not jump to the pointer.
    grab: f32,
    cancelled: bool,
    to: usize,
}

#[derive(Clone, Copy)]
struct Settle {
    index: usize,
    /// Where it was let go, along the axis from the list's start.
    at: f32,
    t0: f64,
}

#[derive(Clone, Copy, Default)]
struct Mem {
    lift: Option<Lift>,
    settle: Option<Settle>,
    /// Part of every slide animation's id. It changes when a move is committed, so the items start the next frame in
    /// their new slots instead of sliding back from the old gap.
    epoch: u64,
    /// The frame it was last drawn in. A list missing for a frame (a closed popover, another tab) drops its drag, so
    /// a lift left over from then is not read as a release when the list comes back.
    frame: u64,
}

/// The index the item lifted from `from` lands at when its center is at `center` along the list: how many of the other
/// items have their midpoint before it. The midpoints are those of the layout before the lift, so the answer does not
/// move as the gap opens and the gap never flickers. A center exactly on a midpoint goes before that item.
pub(crate) fn drop_index(sizes: &[f32], from: usize, center: f32) -> usize {
    let mut start = 0.0;
    let mut n = 0;
    for (k, s) in sizes.iter().enumerate() {
        if k != from && start + s / 2.0 < center {
            n += 1;
        }
        start += s;
    }
    n
}

/// The first index of `i`'s block and the end of it.
fn block_of(blocks: &[usize], n: usize, i: usize) -> (usize, usize) {
    let lo = blocks.iter().copied().filter(|&b| b <= i).max().unwrap_or(0);
    let hi = blocks.iter().copied().filter(|&b| b > i).min().unwrap_or(n).min(n);
    (lo, hi)
}

/// `to` held inside the block of `from`.
pub(crate) fn clamp_to_block(blocks: &[usize], n: usize, from: usize, to: usize) -> usize {
    let (lo, hi) = block_of(blocks, n, from);
    to.clamp(lo, hi.saturating_sub(1).max(lo))
}

/// Where Alt+arrow moves item `from` by `step`, or None at the end of the list or of its block.
pub(crate) fn key_move(blocks: &[usize], n: usize, from: usize, step: i32) -> Option<usize> {
    let to = from as i64 + step as i64;
    if to < 0 || to >= n as i64 {
        return None;
    }
    let to = clamp_to_block(blocks, n, from, to as usize);
    (to != from).then_some(to)
}

/// Auto-scroll speed in points per second with the pointer `dist` inside a scroll area's edge: none past `EDGE`,
/// growing with the square of the closeness so a pointer just inside creeps and one on the edge races.
pub(crate) fn scroll_speed(dist: f32) -> f32 {
    if dist >= EDGE {
        return 0.0;
    }
    let t = ((EDGE - dist) / EDGE).clamp(0.0, 1.0);
    SCROLL_MAX * t * t
}

/// How far item `k` slides to open the gap for a move from `from` to `to`.
fn slide(sizes: &[f32], from: usize, to: usize, k: usize) -> f32 {
    if from < to && k > from && k <= to {
        -sizes[from]
    } else if to < from && k >= to && k < from {
        sizes[from]
    } else {
        0.0
    }
}

/// Which part of an item a drop lands in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Zone {
    Before,
    Into,
    After,
}

/// The zone of an item `len` long that a point `at` from its start falls in: the middle half is `Into` when the item
/// takes drops into it, and either side of the middle are `Before` and `After`.
pub(crate) fn zone_at(len: f32, at: f32, into: bool) -> Zone {
    if into {
        zone_edges(len, at, 0.25)
    } else if at < len / 2.0 {
        Zone::Before
    } else {
        Zone::After
    }
}

/// The zone with bands of `edge` of the length at each end for `Before` and `After` and the rest `Into`: 0.25 is the
/// middle half of `zone_at`, 1/3 the thirds of a rail tile.
pub(crate) fn zone_edges(len: f32, at: f32, edge: f32) -> Zone {
    if at < len * edge {
        Zone::Before
    } else if at > len * (1.0 - edge) {
        Zone::After
    } else {
        Zone::Into
    }
}

/// How far down `rect` the pointer at `pos` is, from 0.0 at its top edge to 1.0 at its bottom edge; the middle without one.
pub(crate) fn y_frac(rect: egui::Rect, pos: Option<egui::Pos2>) -> f32 {
    pos.map_or(0.5, |p| ((p.y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0))
}

/// Where a drop on a tree row lands: before, into or after it, and the depth an insertion line sits at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Spot {
    pub zone: Zone,
    pub depth: usize,
}

/// Paints the mark of a tree drop on the row at `rect`: the drop fill for `Into`, else an insertion line on the row's
/// top or bottom edge that starts at the depth's indent with a small ring.
pub(crate) fn paint_spot(p: &egui::Painter, rect: egui::Rect, spot: Spot, x0: f32, indent: f32) {
    match spot.zone {
        Zone::Into => drop_fill(p, rect),
        z => {
            let y = if z == Zone::Before { rect.top() } else { rect.bottom() };
            let x = (x0 + spot.depth as f32 * indent).min(rect.right() - 8.0);
            p.circle_stroke(egui::pos2(x + 3.0, y), 3.0, Stroke::new(1.5, PRIMARY()));
            drop_line(p, egui::Rangef::new(x + 6.0, rect.right()), y);
        }
    }
}

/// Draws a list whose items the user can drag to reorder, and returns the move once one is let go or made with
/// Alt+arrow. `draw` paints item `i` to fill its `Ui`'s `max_rect` and returns the item's main response, which gets the
/// grab cursor on hover and the keyboard move while it has focus. Apply the move to the data the same frame or the next.
///
/// While dragging, the item floats on the tooltip layer, held to the axis, with a shadow; the others slide aside to
/// open a gap where it would land, taken from the item midpoints of the whole list. Near the edge of the enclosing
/// scroll area the list scrolls, faster the closer the pointer. Esc, or a release away from the list, cancels and the
/// item lands back in its slot.
pub(crate) fn reorder(ui: &mut egui::Ui, id: egui::Id, items: Items, mut draw: impl FnMut(&mut egui::Ui, usize, ItemState) -> egui::Response) -> Option<Move> {
    let Items { axis, sizes, cross, keys, blocks, into, menu } = items;
    let n = sizes.len();
    let ctx = ui.ctx().clone();
    let key = |k: usize| keys.and_then(|v| v.get(k).copied()).unwrap_or_else(|| egui::Id::unique(k));
    let takes = |k: usize| into.is_some_and(|v| v.get(k).copied().unwrap_or(false));
    let cross = cross.unwrap_or_else(|| ui.available_width());
    let total: f32 = sizes.iter().sum();
    let (list, _) = ui.allocate_exact_size(axis.vec(total, cross), Sense::hover());
    let mut starts = Vec::with_capacity(n);
    let mut s = 0.0;
    for z in sizes {
        starts.push(s);
        s += z;
    }
    let slot = |k: usize| egui::Rect::from_min_size(list.min + axis.vec(starts[k], 0.0), axis.vec(sizes[k], cross));
    let rel = |p: egui::Pos2| axis.main(p - list.min);
    let mut m: Mem = ctx.data(|d| d.get_temp(id)).unwrap_or_default();
    let frame = ctx.cumulative_frame_nr();
    if frame > m.frame + 1 {
        m.lift = None;
        m.settle = None;
    }
    m.frame = frame;
    if m.lift.is_some_and(|l| l.from >= n) {
        m.lift = None;
    }
    if m.settle.is_some_and(|s| s.index >= n) {
        m.settle = None;
    }
    // Every drag sense first, so all that the items draw sits above them.
    let handles: Vec<egui::Response> = (0..n).map(|k| ui.interact(slot(k), id.with(("drag", key(k))), Sense::drag())).collect();
    let (pointer, decided, now, dt) = ctx.input(|i| (i.pointer.latest_pos(), i.pointer.is_decidedly_dragging(), i.time, i.stable_dt.min(0.1)));
    let clip = ui.clip_rect();
    if m.lift.is_none() && decided && let Some(k) = handles.iter().position(|h| h.dragged_by(egui::PointerButton::Primary)) {
        let origin = ctx.input(|i| i.pointer.press_origin()).or(pointer).unwrap_or(slot(k).min);
        m.lift = Some(Lift { from: k, grab: rel(origin) - starts[k], cancelled: false, to: k });
        m.settle = None;
        m.epoch = m.epoch.wrapping_add(1);
    }
    // The lifted item stays in the visible part of the list, so it never floats over the page around a scroll area.
    let (lo, hi) = (axis.main(clip.min - list.min).max(0.0), axis.main(clip.max - list.min).min(total));
    let mut out = None;
    let mut lifted: Option<(usize, f32)> = None;
    let mut gap: Option<(usize, usize)> = None;
    let mut target: Option<usize> = None;
    let epoch = m.epoch;
    if let Some(l) = m.lift {
        // The drop index reads where the pointer is, unclamped: held inside the list, the last item's center could only reach the
        // midpoint of the item before it, and a midpoint tie stays before, so nothing could be dropped last.
        let raw = pointer.map_or(starts[l.from], |p| rel(p) - l.grab);
        let pos = raw.clamp(lo, (hi - sizes[l.from]).max(lo));
        let held = handles[l.from].dragged();
        if l.cancelled {
            if !held {
                m.lift = None;
            }
        } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) || drag_cancelled(&ctx) {
            // A list whose lifted item also carries a DragAndDrop payload (the rail) never sees the Esc key: egui's
            // plugin eats it to drop the payload, which drag_cancelled notices instead.
            m.lift = Some(Lift { cancelled: true, ..l });
            m.settle = Some(Settle { index: l.from, at: pos, t0: now });
        } else {
            // On the middle of an item that takes drops the gap holds still, so the item under the pointer stays put.
            // The test is against where the items stand with the current gap, which only the midpoints move.
            let over = pointer.and_then(|p| {
                let at = rel(p);
                (0..n).filter(|&k| k != l.from && takes(k)).find(|&k| {
                    let s = starts[k] + slide(sizes, l.from, l.to, k);
                    at >= s && at <= s + sizes[k] && zone_at(sizes[k], at - s, true) == Zone::Into
                })
            });
            let to = if over.is_some() { l.to } else { clamp_to_block(blocks, n, l.from, drop_index(sizes, l.from, raw + sizes[l.from] / 2.0)) };
            m.lift = Some(Lift { to, ..l });
            lifted = Some((l.from, pos));
            gap = Some((l.from, to));
            target = over;
            if !held {
                m.lift = None;
                let near = list.intersect(clip).expand2(axis.vec(EDGE, CROSS_SLACK));
                let inside = pointer.is_some_and(|p| near.contains(p));
                match over {
                    Some(t) if inside => {
                        out = Some(Move { from: l.from, to: t, into: true });
                        m.epoch = m.epoch.wrapping_add(1);
                    }
                    _ if inside && to != l.from => {
                        out = Some(Move { from: l.from, to, into: false });
                        m.epoch = m.epoch.wrapping_add(1);
                        m.settle = Some(Settle { index: to, at: pos, t0: now });
                    }
                    _ => m.settle = Some(Settle { index: l.from, at: pos, t0: now }),
                }
            } else {
                ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
                if let Some(p) = pointer {
                    let pm = axis.main(p.to_vec2());
                    let (cmin, cmax) = (axis.main(clip.min.to_vec2()), axis.main(clip.max.to_vec2()));
                    let mut d = 0.0;
                    if axis.main(list.min.to_vec2()) < cmin - 0.5 {
                        d += scroll_speed(pm - cmin);
                    }
                    if axis.main(list.max.to_vec2()) > cmax + 0.5 {
                        d -= scroll_speed(cmax - pm);
                    }
                    if d != 0.0 {
                        ui.scroll_with_delta_animation(axis.vec(d * dt, 0.0), egui::style::ScrollAnimation::none());
                        ctx.request_repaint();
                    }
                }
            }
        }
    }
    // A dropped or cancelled item floats into its slot.
    if lifted.is_none() && let Some(s) = m.settle {
        let t = ((now - s.t0) / SETTLE_S).clamp(0.0, 1.0) as f32;
        if t >= 1.0 {
            m.settle = None;
        } else {
            let e = super::fold::ease_out(t);
            lifted = Some((s.index, s.at + (starts[s.index] - s.at) * e));
            ctx.request_repaint();
        }
    }
    let busy = lifted.is_some();
    let (back, fwd) = match axis {
        Axis::Vertical => (egui::Key::ArrowUp, egui::Key::ArrowDown),
        Axis::Horizontal => (egui::Key::ArrowLeft, egui::Key::ArrowRight),
    };
    for k in 0..n {
        if lifted.is_some_and(|(i, _)| i == k) {
            continue;
        }
        let goal = gap.map_or(0.0, |(f, t)| slide(sizes, f, t, k));
        let off = ctx.animate_value_with_time(id.with(("slide", epoch, key(k))), goal, SLIDE_S);
        let rect = slot(k).translate(axis.vec(off, 0.0));
        let mut child = ui.new_child(egui::UiBuilder::new().scope_id(id.with(("item", key(k)))).max_rect(rect));
        let r = draw(&mut child, k, ItemState { lifted: false, busy, offset: off });
        if target == Some(k) {
            drop_fill(ui.painter(), rect);
        }
        if busy {
            continue;
        }
        focus_ring(ui, &r, r.rect, R_SM as f32);
        if r.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::Grab);
        }
        if menu {
            r.context_menu(|ui| {
                menu_look(ui);
                if let Some(mv) = move_items(ui, n, k, blocks, matches!(axis, Axis::Horizontal))
                    && out.is_none()
                {
                    out = Some(mv);
                    m.epoch = m.epoch.wrapping_add(1);
                }
            });
        }
        if r.has_focus() && out.is_none() {
            let step = ctx.input_mut(|i| if i.consume_key(egui::Modifiers::ALT, back) { -1 } else if i.consume_key(egui::Modifiers::ALT, fwd) { 1 } else { 0 });
            if let Some(to) = (step != 0).then(|| key_move(blocks, n, k, step)).flatten() {
                out = Some(Move { from: k, to, into: false });
                m.epoch = m.epoch.wrapping_add(1);
            }
        }
    }
    if let Some((k, at)) = lifted {
        let rect = egui::Rect::from_min_size(list.min + axis.vec(at, 0.0), axis.vec(sizes[k], cross));
        let layer = egui::LayerId::new(egui::Order::Tooltip, id.with("lifted"));
        // Room for the shadow around the visible part of the list.
        let room = clip.expand(16.0);
        let p = ctx.layer_painter(layer).with_clip_rect(room);
        p.add(ui.visuals().popup_shadow.as_shape(rect, R_SM));
        p.rect(rect, R_SM, RAISED(), Stroke::new(1.0, if target.is_some() { PRIMARY() } else { BORDER() }), egui::StrokeKind::Inside);
        // In an area that takes no pointer: widgets the item draws there would otherwise cover the targets under the
        // pointer, and a lifted item that also carries a DragAndDrop payload (`carry`) needs those targets to see it.
        let layout = *ui.layout();
        let style = ui.style().clone();
        egui::Area::new(layer.id).order(egui::Order::Tooltip).interactable(false).constrain(false).fixed_pos(rect.min).show(&ctx, |area| {
            let mut child = area.new_child(egui::UiBuilder::new().scope_id(id.with(("item", key(k)))).layout(layout).max_rect(rect));
            child.set_style(style);
            child.set_clip_rect(room);
            draw(&mut child, k, ItemState { lifted: true, busy: true, offset: 0.0 });
        });
    }
    ctx.data_mut(|d| d.insert_temp(id, m));
    out
}

/// Makes `r` the source of a drag: sets `payload` when the drag starts and, while it goes on, draws a ghost that `paint`
/// draws: the source's own look (a tile, a connection row, a script row) at its size, floating beside the pointer with a
/// shadow. The ghost is painted on the tooltip layer without a widget, so it never catches the pointer that the targets
/// below it need. Esc drops the payload (see `drag_cancelled`) and the ghost goes.
pub(crate) fn drag_source_with<T: std::any::Any + Send + Sync>(ui: &egui::Ui, r: &egui::Response, payload: T, paint: impl FnOnce(&egui::Painter, egui::Rect)) {
    if r.drag_started() {
        egui::DragAndDrop::set_payload(ui.ctx(), payload);
    }
    if drag_cancelled(ui.ctx()) || !r.dragged() {
        return;
    }
    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    let Some(pos) = ui.ctx().pointer_interact_pos() else { return };
    let p = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("quarry-drag-ghost")));
    let rect = egui::Rect::from_min_size(pos + egui::vec2(12.0, 8.0), r.rect.size());
    p.add(ui.visuals().popup_shadow.as_shape(rect, R_SM));
    p.rect(rect, R_SM, RAISED(), Stroke::new(1.0, PRIMARY()), egui::StrokeKind::Inside);
    paint(&p.with_clip_rect(rect), rect);
}

/// Lets an item lifted by `reorder` also travel as an `egui::DragAndDrop` payload, so targets outside its list (another
/// list, a tree) can take it. Call it from `draw` with the item's state: the payload is set while it is lifted. The
/// list itself cancels a release outside it, so the target that took the payload is the one that acts.
pub(crate) fn carry<T: std::any::Any + Send + Sync>(ctx: &egui::Context, st: ItemState, payload: impl FnOnce() -> T) {
    if st.lifted && !drag_cancelled(ctx) && ctx.input(|i| i.pointer.any_down()) {
        egui::DragAndDrop::set_payload(ctx, payload());
    }
}

/// Scrolls the scroll area `ui` is inside while an `egui::DragAndDrop` payload is held near its edges, on both axes,
/// faster the closer the pointer. Call it inside the area's content each frame.
pub(crate) fn auto_scroll_while_dragging(ui: &egui::Ui) {
    let ctx = ui.ctx();
    if !egui::DragAndDrop::has_any_payload(ctx) || drag_cancelled(ctx) {
        return;
    }
    let (Some(p), dt) = ctx.input(|i| (i.pointer.latest_pos(), i.stable_dt.min(0.1))) else { return };
    let clip = ui.clip_rect();
    if !clip.expand(EDGE).contains(p) {
        return;
    }
    let d = egui::vec2(scroll_speed(p.x - clip.left()) - scroll_speed(clip.right() - p.x), scroll_speed(p.y - clip.top()) - scroll_speed(clip.bottom() - p.y));
    if d != egui::Vec2::ZERO {
        ui.scroll_with_delta_animation(d * dt, egui::style::ScrollAnimation::none());
        ctx.request_repaint();
    }
}

#[derive(Clone, Copy, Default)]
struct Seen {
    frame: u64,
    had: bool,
    cancelled: bool,
}

/// True while a drag whose `egui::DragAndDrop` payload Esc dropped is still held. egui clears the payload on Esc at the
/// start of the frame and eats the key, so this notices the payload gone with the button still down. Call it every frame
/// a drag may be going on (`drag_source_with` does); a source should stop its ghost and targets their marks.
pub(crate) fn drag_cancelled(ctx: &egui::Context) -> bool {
    let key = egui::Id::unique("quarry-drag-cancelled");
    let frame = ctx.cumulative_frame_nr();
    let mut s: Seen = ctx.data(|d| d.get_temp(key)).unwrap_or_default();
    if s.frame == frame {
        return s.cancelled;
    }
    let has = egui::DragAndDrop::has_any_payload(ctx);
    let down = ctx.input(|i| i.pointer.any_down());
    let had = s.had && s.frame + 1 == frame;
    s.cancelled = down && (s.cancelled || (had && !has));
    s.had = has;
    s.frame = frame;
    ctx.data_mut(|d| d.insert_temp(key, s));
    s.cancelled
}

/// The six dots of a drag grip centered on `c`, painted without a widget so the whole row stays the handle.
pub(crate) fn grip_dots(p: &egui::Painter, c: egui::Pos2, color: egui::Color32) {
    for dx in [-2.5, 2.5] {
        for dy in [-5.0, 0.0, 5.0] {
            p.circle_filled(c + egui::vec2(dx, dy), 1.3, color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_index_counts_midpoints_before_the_center() {
        let s = [30.0; 5];
        // Lifted from 1 and barely moved: still 1.
        assert_eq!(drop_index(&s, 1, 45.0), 1);
        // Past the midpoint of item 2 (75) it goes after it.
        assert_eq!(drop_index(&s, 1, 76.0), 2);
        assert_eq!(drop_index(&s, 1, 74.0), 1);
        // Exactly on a midpoint it stays before that item.
        assert_eq!(drop_index(&s, 1, 75.0), 1);
        assert_eq!(drop_index(&s, 3, -50.0), 0);
        assert_eq!(drop_index(&s, 0, 500.0), 4);
    }

    #[test]
    fn drop_index_with_uneven_sizes() {
        // Midpoints at 10, 70 and 125.
        let s = [20.0, 100.0, 10.0];
        assert_eq!(drop_index(&s, 2, 69.0), 1);
        assert_eq!(drop_index(&s, 2, 71.0), 2);
        assert_eq!(drop_index(&s, 0, 71.0), 1);
    }

    #[test]
    fn blocks_hold_a_move_inside_its_block() {
        // Pinned 0..2, the rest 2..5.
        let b = [2];
        assert_eq!(clamp_to_block(&b, 5, 0, 4), 1);
        assert_eq!(clamp_to_block(&b, 5, 1, 0), 0);
        assert_eq!(clamp_to_block(&b, 5, 3, 0), 2);
        assert_eq!(clamp_to_block(&b, 5, 3, 4), 4);
        assert_eq!(clamp_to_block(&[], 5, 3, 0), 0);
        // A boundary at either end changes nothing.
        assert_eq!(clamp_to_block(&[0], 5, 3, 0), 0);
        assert_eq!(clamp_to_block(&[5], 5, 3, 0), 0);
    }

    #[test]
    fn keyboard_moves_stop_at_ends_and_blocks() {
        assert_eq!(key_move(&[], 3, 0, -1), None);
        assert_eq!(key_move(&[], 3, 2, 1), None);
        assert_eq!(key_move(&[], 3, 1, 1), Some(2));
        assert_eq!(key_move(&[], 3, 1, -1), Some(0));
        assert_eq!(key_move(&[2], 4, 1, 1), None);
        assert_eq!(key_move(&[2], 4, 2, -1), None);
        assert_eq!(key_move(&[2], 4, 2, 1), Some(3));
    }

    #[test]
    fn scroll_speed_grows_towards_the_edge() {
        assert_eq!(scroll_speed(EDGE), 0.0);
        assert_eq!(scroll_speed(100.0), 0.0);
        assert_eq!(scroll_speed(0.0), SCROLL_MAX);
        assert_eq!(scroll_speed(-30.0), SCROLL_MAX);
        let (far, mid, near) = (scroll_speed(20.0), scroll_speed(12.0), scroll_speed(4.0));
        assert!(0.0 < far && far < mid && mid < near && near < SCROLL_MAX);
    }

    #[test]
    fn the_gap_opens_where_the_item_lands() {
        let s = [10.0, 20.0, 30.0, 40.0];
        assert_eq!((0..4).map(|k| slide(&s, 0, 2, k)).collect::<Vec<_>>(), vec![0.0, -10.0, -10.0, 0.0]);
        assert_eq!((0..4).map(|k| slide(&s, 3, 1, k)).collect::<Vec<_>>(), vec![0.0, 40.0, 40.0, 0.0]);
    }

    #[test]
    fn moves_apply_and_read_as_insert_before() {
        let mut v = vec!['a', 'b', 'c', 'd'];
        let m = Move { from: 0, to: 2, into: false };
        m.apply(&mut v);
        assert_eq!(v, vec!['b', 'c', 'a', 'd']);
        assert_eq!(m.before(), 3);
        assert_eq!(Move { from: 3, to: 1, into: false }.before(), 1);
    }

    /// Drags item `from` of four 30 pt rows to `at` points below the list's top and returns what `reorder` let go of.
    fn drag_in_list(from: usize, at: f32, into: &[bool]) -> Option<Move> {
        use std::cell::{Cell, RefCell};
        let (out, top) = (Cell::new(None), RefCell::new(None));
        let sizes = [30.0; 4];
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(200.0, 200.0)).build_ui(|ui| {
            *top.borrow_mut() = Some(ui.cursor().min);
            if let Some(m) = reorder(ui, egui::Id::unique("dnd-test"), Items::vertical(&sizes).into(into), |ui, _, _| ui.response()) {
                out.set(Some(m));
            }
        });
        h.step();
        let top = top.borrow().unwrap();
        let start = top + egui::vec2(50.0, from as f32 * 30.0 + 15.0);
        let end = egui::pos2(start.x, top.y + at);
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        h.hover_at(start);
        h.step();
        h.input_mut().events.push(button(start, true));
        h.step();
        for i in 1..=6 {
            h.hover_at(start.lerp(end, i as f32 / 6.0));
            h.step();
        }
        h.input_mut().events.push(button(end, false));
        h.run_steps(3);
        drop(h);
        out.get()
    }

    #[test]
    fn a_drag_reorders_and_a_drop_on_a_middle_goes_into_it() {
        // Down past the middle of row 2: lands after it.
        assert_eq!(drag_in_list(0, 80.0, &[false; 4]), Some(Move { from: 0, to: 2, into: false }));
        // Same spot on a row that takes drops: into it, since the gap left row 2 where it stood.
        assert_eq!(drag_in_list(0, 75.0, &[false, false, true, false]), Some(Move { from: 0, to: 2, into: true }));
        // On the edge of that row it is a plain move again.
        assert_eq!(drag_in_list(0, 88.0, &[false, false, true, false]), Some(Move { from: 0, to: 2, into: false }));
        // Back where it started: nothing.
        assert_eq!(drag_in_list(1, 46.0, &[false; 4]), None);
    }

    #[test]
    fn a_stale_move_changes_nothing_and_names_alike_get_their_own_keys() {
        let mut v = vec!['b', 'c', 'a'];
        Move { from: 0, to: 7, into: false }.apply(&mut v);
        assert_eq!(v, ['b', 'c', 'a']);
        Move { from: 0, to: 1, into: true }.apply(&mut v);
        assert_eq!(v, ['b', 'c', 'a']);
        Move { from: 5, to: 0, into: false }.apply(&mut v);
        assert_eq!(v, ['b', 'c', 'a']);
        let k = unique_keys("cols", &["id", "name", "id"]);
        assert!(k[0] != k[2] && k[0] != k[1]);
        assert_eq!(k[0], unique_keys("cols", &["id"])[0], "the first of a name keeps its key");
    }

    #[test]
    fn a_list_that_shrinks_during_a_drag_lets_go_without_a_move() {
        use std::cell::{Cell, RefCell};
        let (out, top, n) = (Cell::new(None), RefCell::new(None), Cell::new(4usize));
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(200.0, 200.0)).build_ui(|ui| {
            *top.borrow_mut() = Some(ui.cursor().min);
            let sizes = vec![30.0; n.get()];
            if let Some(m) = reorder(ui, egui::Id::unique("dnd-shrink"), Items::vertical(&sizes), |ui, _, _| ui.response()) {
                out.set(Some(m));
            }
        });
        h.step();
        let top = top.borrow().unwrap();
        let start = top + egui::vec2(50.0, 3.0 * 30.0 + 15.0);
        let end = egui::pos2(start.x, top.y + 10.0);
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        h.hover_at(start);
        h.step();
        h.input_mut().events.push(button(start, true));
        h.step();
        for i in 1..=6 {
            h.hover_at(start.lerp(end, i as f32 / 6.0));
            h.step();
        }
        // A scan or a poll took rows away while the last one was held.
        n.set(2);
        h.step();
        h.input_mut().events.push(button(end, false));
        h.run_steps(3);
        drop(h);
        assert_eq!(out.get(), None);
    }

    #[test]
    fn zones_split_an_item_by_length() {
        assert_eq!(zone_at(28.0, 3.0, true), Zone::Before);
        assert_eq!(zone_at(28.0, 14.0, true), Zone::Into);
        assert_eq!(zone_at(28.0, 25.0, true), Zone::After);
        assert_eq!(zone_at(28.0, 13.0, false), Zone::Before);
        assert_eq!(zone_at(28.0, 15.0, false), Zone::After);
        assert_eq!([5.0, 15.0, 25.0].map(|at| zone_edges(30.0, at, 1.0 / 3.0)), [Zone::Before, Zone::Into, Zone::After]);
    }
}
