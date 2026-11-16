//! The filter panel under the query line (design B in docs/sql/rules-builder.figcraft): the conditions in nested all, any
//! and none groups, the SQL they write, and Apply. It edits `Bar::tree`; the bar writes the WHERE text from it.

use super::filter_cells::{rule_editor, Hints};
use crate::app::*;
use crate::db::sql::{
    facts::{FilterCount, FilterErr},
    filter::{self, Block, Cond, Item, Match, Tree},
    rules::Rule,
    Col, Dialect,
};
use crate::ui::dnd::{drag_cancelled, drag_source_with, grip_dots, paint_spot, y_frac, zone_at, Spot, Zone};
use crate::ui::fold;

/// The pause after the last edit before a count is asked again, as long as the Query editor's live check waits.
const PAUSE: f64 = 0.6;
/// Counts of single rows kept while the panel is used, so hovering back and forth asks once per row.
const HOVER_CACHE: usize = 64;

pub(in crate::app) type Counted = Result<FilterCount, FilterErr>;

/// What a count slot should do this frame.
pub(in crate::app) enum Tick {
    Ask,
    /// Look again after this many seconds.
    Wait(f32),
    Idle,
}

/// One count the panel shows: the text last asked about under which request, and the answer with the text it is for.
#[derive(Default)]
pub(in crate::app) struct Count {
    pub(in crate::app) req: u64,
    pub(in crate::app) asked: Option<String>,
    /// The text seen last and since when, for the pause before asking.
    seen: String,
    since: f64,
    pub(in crate::app) res: Option<(String, Counted)>,
}

impl Count {
    /// Whether to ask about `text` now: once it has rested PAUSE seconds and was not asked yet.
    pub(in crate::app) fn tick(&mut self, text: &str, now: f64) -> Tick {
        if text != self.seen {
            self.seen = text.to_string();
            self.since = now;
        }
        if self.asked.as_deref() == Some(text) {
            return Tick::Idle;
        }
        let left = PAUSE - (now - self.since);
        if left > 0.0 { Tick::Wait(left as f32) } else { Tick::Ask }
    }

    pub(in crate::app) fn ask(&mut self, req: u64, text: &str) {
        self.req = req;
        self.asked = Some(text.to_string());
    }

    /// Keeps the answer to request `req` when it is this slot's latest, and says whether it was.
    pub(in crate::app) fn answer(&mut self, req: u64, res: &Counted) -> bool {
        match &self.asked {
            Some(text) if req == self.req => {
                self.res = Some((text.clone(), res.clone()));
                true
            }
            _ => false,
        }
    }

    /// The answer for `text`, never one for an older text.
    fn shown(&self, text: &str) -> Option<&Counted> {
        self.res.as_ref().filter(|(t, _)| t == text).map(|(_, r)| r)
    }
}

#[derive(Default)]
pub(in crate::app) struct Panel {
    pub(in crate::app) open: bool,
    /// The selected row or group.
    pub(in crate::app) sel: Option<u32>,
    /// A row just added, whose column picker opens on its first frame.
    pub(in crate::app) pick: Option<u32>,
    /// A row whose operator menu opens on its first frame; only the demo of the menu sets it.
    pub(in crate::app) ops: Option<u32>,
    /// How many rows the whole WHERE matches.
    pub(in crate::app) count: Count,
    /// The row under the pointer, whose own count shows at its end.
    pub(in crate::app) hover: Option<u32>,
    pub(in crate::app) hover_count: Count,
    pub(in crate::app) hover_cache: HashMap<String, Counted>,
    /// The table the counts are for, since a tab can move to another one.
    pub(in crate::app) table: String,
}

impl Panel {
    /// Keeps the answer to request `req` in the slot that asked. False when neither did.
    pub(in crate::app) fn counted(&mut self, req: u64, res: &Counted) -> bool {
        if self.count.answer(req, res) {
            return true;
        }
        match self.hover_count.asked.clone().filter(|_| self.hover_count.req == req) {
            Some(text) => {
                if self.hover_cache.len() >= HOVER_CACHE {
                    self.hover_cache.clear();
                }
                self.hover_cache.insert(text, res.clone());
                true
            }
            None => false,
        }
    }
}

/// A count with its thousands separated.
fn grouped(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The line under the SQL: how many rows match, an estimate, or why the server refused it.
fn count_line(c: &Counted) -> (String, Color32) {
    match c {
        Ok(c) if c.estimated => (format!("\u{2248} {} rows match", grouped(c.n)), MUTED()),
        Ok(c) => (format!("{} {} match", grouped(c.n), if c.n == 1 { "row" } else { "rows" }), TEXT()),
        Err(e) => (crate::db::explain(&e.msg, &crate::db::ErrCtx { what: "count the rows".into(), ns: String::new(), host: String::new() }).title, ALERT()),
    }
}

fn fold_id() -> egui::Id {
    egui::Id::unique("sql-filter-panel")
}

impl Panel {
    pub(in crate::app) fn toggle(&mut self, ctx: &egui::Context) {
        self.open = !self.open;
        fold::start(ctx, fold_id(), self.open);
        // The rows may have changed while it was closed, so counts are asked again.
        if self.open {
            self.forget_counts();
        }
    }

    pub(in crate::app) fn forget_counts(&mut self) {
        (self.count, self.hover_count) = (Count::default(), Count::default());
        self.hover_cache.clear();
    }

    /// Opens the panel, if it is not, on node `at`.
    pub(in crate::app) fn show(&mut self, ctx: &egui::Context, at: Option<u32>) {
        if !self.open {
            self.toggle(ctx);
        }
        self.sel = at;
    }

    fn close(&mut self, ctx: &egui::Context) {
        if self.open {
            self.toggle(ctx);
        }
    }
}

/// What the panel asks of the bar after a frame.
#[derive(Default)]
pub(super) struct Out {
    pub(super) run: bool,
    /// The tree changed, so the WHERE text is written again.
    pub(super) changed: bool,
    /// Save as filter was clicked.
    pub(super) save: bool,
    /// Edit as SQL was clicked.
    pub(super) as_sql: bool,
    /// Where a dragged node may land, gathered while the rows draw.
    targets: Vec<Target>,
}

/// What every row of one frame reads: the columns, the server's dialect and escapes, and the row an error points into.
struct Cx<'a> {
    cols: &'a [Col],
    d: Dialect,
    bs: bool,
    bad: Option<u32>,
}

/// A row or group dragged in the panel, by id.
#[derive(Clone, Copy)]
struct FilterDrag(u32);

/// A place a drop can land: a row (before or after it), a group's header (before it, or at the top inside it), or the
/// end of a group's list.
#[derive(Clone, Copy)]
struct Target {
    rect: egui::Rect,
    parent: u32,
    index: usize,
    depth: usize,
    kind: Place,
}

#[derive(Clone, Copy, PartialEq)]
enum Place {
    Row,
    Head(u32),
    End,
}

enum Act {
    Remove(u32),
    Toggle(u32),
    /// A row, or a group holding one, added at the end of a group (0 the root).
    Add(u32, bool),
    /// A row added right after this one.
    After(u32),
    Duplicate(u32),
    Negate(u32),
    Wrap(u32),
    Ungroup(u32),
    MoveBy(u32, isize),
    /// A row taken out of its group to just after it.
    Unwrap(u32),
    /// A dragged node dropped into group `parent` before the child at `index`.
    Move(u32, u32, usize),
}

/// The word a new group takes inside `parent`: the other one than its parent's, so wrapping says something.
fn other_word(tree: &Tree, parent: u32) -> Match {
    let outer = if parent == 0 { Some(tree.root.mode) } else { tree.find(parent).and_then(|n| if let Item::Group(g) = &n.item { Some(g.mode) } else { None }) };
    if outer == Some(Match::All) { Match::Any } else { Match::All }
}

const MODES: [(Match, &str); 3] = [(Match::All, "all"), (Match::Any, "any"), (Match::None, "none")];
/// The SQL pane's width beside the conditions; under `WIDE` it goes below them.
const PANE: f32 = 300.0;
const WIDE: f32 = 760.0;

/// A new row on the table's first column, or a group of the other word than `parent` holding one, at the end of `parent`
/// (0 the root), selected with its column picker open.
pub(super) fn add(tree: &mut Tree, p: &mut Panel, parent: u32, group: bool, cols: &[Col], d: Dialect) {
    let Some(c) = cols.first() else { return };
    let row = Item::Cond(Cond { rule: Rule::new(&c.name, c.kind, d), on: true });
    let id = if group {
        let mode = other_word(tree, parent);
        tree.push(parent, Item::Group(Block { mode, kids: vec![] })).and_then(|g| tree.push(g, row))
    } else {
        tree.push(parent, row)
    };
    p.sel = id;
    p.pick = id;
}

/// Draws the panel while it is open or folding, and handles its keys. The tree is edited in place; structural edits are
/// queued while it is drawn and made after.
/// `keys` is false while a dialog is over the page, which then owns Esc and Cmd+Enter.
pub(super) fn panel(ui: &mut egui::Ui, tree: &mut Tree, p: &mut Panel, cols: &[Col], d: Dialect, bs: bool, keys: bool, hints: &mut Hints) -> Out {
    let mut out = Out::default();
    let (text, spans) = filter::spans(tree, d, bs);
    // The row a server error points into: the narrowest span that holds the position.
    let bad = match p.count.shown(&text) {
        Some(Err(FilterErr { at: Some(at), .. })) => spans.iter().filter(|(_, r)| r.contains(at) || r.end == *at).min_by_key(|(_, r)| r.len()).map(|(id, _)| *id),
        _ => None,
    };
    p.hover = None;
    // Sampled before anything draws: a field that gives up its focus on Enter this frame still had it.
    let typing = ui.ctx().egui_wants_keyboard_input();
    let drag = egui::DragAndDrop::payload::<FilterDrag>(ui.ctx()).filter(|_| !drag_cancelled(ui.ctx())).map(|d| d.0);
    let cx = Cx { cols, d, bs, bad };
    let t = fold::openness(ui.ctx(), fold_id(), p.open);
    let mut acts = Vec::new();
    let mut apply = false;
    fold::body(ui, fold_id(), t, 220.0, p.open, |ui| {
        egui::Frame::new().fill(PANEL()).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_BAR).inner_margin(egui::Margin::same(12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if ui.available_width() >= WIDE {
                ui.horizontal_top(|ui| {
                    let left = ui.available_width() - PANE - 16.0;
                    ui.allocate_ui_with_layout(egui::vec2(left, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.set_width(left);
                        conditions(ui, tree, p, &cx, &mut acts, &mut out, hints);
                    });
                    ui.add_space(16.0);
                    ui.vertical(|ui| {
                        ui.set_width(PANE);
                        sql_pane(ui, tree, p, &text, d, bs, &mut out);
                    });
                });
            } else {
                conditions(ui, tree, p, &cx, &mut acts, &mut out, hints);
                ui.add_space(10.0);
                sql_pane(ui, tree, p, &text, d, bs, &mut out);
            }
            ui.add_space(8.0);
            popover_divider(ui);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                out.save |= quiet(ui, RichText::new("Save as filter…").size(TXT_SMALL).color(PRIMARY())).on_hover_text("Save the WHERE to the script library").clicked();
                if quiet(ui, RichText::new("Clear").size(TXT_SMALL).color(MUTED())).on_hover_text("Remove every condition").clicked() {
                    tree.root.kids.clear();
                    (p.sel, p.pick) = (None, None);
                    out.changed = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    apply |= primary(ui, "Apply", PRIMARY()).on_hover_text("Run the filter and close the panel (⌘↵)").clicked();
                    ui.label(RichText::new("Esc closes, keeps the edits").size(TXT_CAPTION).color(FAINT()));
                });
            });
        });
    });
    if let Some(id) = drag {
        drop_at(ui, &out.targets, tree, id, &mut acts);
    }
    out.targets.clear();
    if p.open && keys {
        // A menu or a picker of the panel owns Esc while it is open.
        let popup = egui::Popup::is_any_open(ui.ctx());
        let (enter, esc) = ui.input_mut(|i| (i.consume_key(egui::Modifiers::COMMAND, Key::Enter), !popup && i.consume_key(egui::Modifiers::NONE, Key::Escape)));
        apply |= enter;
        if esc {
            p.close(ui.ctx());
        }
        if !typing && !popup {
            row_keys(ui, tree, p, &mut acts);
        }
    }
    for a in acts {
        match a {
            Act::Remove(id) => {
                // The selection moves on to the next row, so Cmd+Backspace can be pressed again.
                let order = tree.rows_in_order();
                let next = order.iter().position(|x| *x == id).and_then(|i| order.get(i + 1).or_else(|| i.checked_sub(1).and_then(|j| order.get(j)))).copied();
                tree.remove(id);
                if p.sel == Some(id) {
                    p.sel = next.filter(|n| tree.find(*n).is_some());
                }
            }
            Act::Toggle(id) => {
                tree.toggle(id);
            }
            Act::Add(parent, group) => add(tree, p, parent, group, cols, d),
            Act::After(id) => {
                if let Some(c) = cols.first() {
                    let new = tree.insert_after(id, Item::Cond(Cond { rule: Rule::new(&c.name, c.kind, d), on: true }));
                    (p.sel, p.pick) = (new, new);
                }
            }
            Act::Duplicate(id) => p.sel = tree.duplicate(id).or(p.sel),
            Act::Negate(id) => {
                tree.negate(id, d);
            }
            Act::Wrap(id) => {
                let mode = other_word(tree, tree.parent_of(id).unwrap_or(0));
                p.sel = tree.wrap(id, mode).or(p.sel);
            }
            Act::Ungroup(id) => {
                tree.ungroup(id);
            }
            Act::MoveBy(id, by) => {
                tree.move_by(id, by);
            }
            Act::Unwrap(id) => {
                tree.unwrap(id);
            }
            Act::Move(id, parent, index) => {
                tree.move_to(id, parent, index);
                p.sel = Some(id);
            }
        }
        out.changed = true;
    }
    if apply {
        out.run = true;
        p.close(ui.ctx());
    }
    if out.as_sql {
        p.close(ui.ctx());
    }
    out
}

/// The root's word, its rows and groups, and the buttons that add to it.
#[allow(clippy::too_many_arguments)]
fn conditions(ui: &mut egui::Ui, tree: &mut Tree, p: &mut Panel, cx: &Cx, acts: &mut Vec<Act>, out: &mut Out, hints: &mut Hints) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(RichText::new("Match").size(TXT_SMALL).color(MUTED()));
        let before = tree.root.mode;
        segmented(ui, &mut tree.root.mode, &MODES);
        out.changed |= tree.root.mode != before;
        ui.label(RichText::new("of these conditions").size(TXT_SMALL).color(MUTED()));
    });
    ui.add_space(6.0);
    if tree.root.kids.is_empty() {
        ui.label(RichText::new("No conditions yet: every row shows.").size(TXT_SMALL).color(FAINT()));
    }
    block(ui, &mut tree.root, 0, 0, p, cx, acts, out, hints);
    let n = tree.root.kids.len();
    add_buttons(ui, 0, n, 0, acts, out);
}

/// The buttons that add to group `parent`, whose row also takes drops at the end of its `n` children.
fn add_buttons(ui: &mut egui::Ui, parent: u32, n: usize, depth: usize, acts: &mut Vec<Act>, out: &mut Out) {
    let r = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if quiet(ui, RichText::new("+ Condition").size(TXT_SMALL).color(PRIMARY())).clicked() {
            acts.push(Act::Add(parent, false));
        }
        if quiet(ui, RichText::new("+ Group").size(TXT_SMALL).color(PRIMARY())).clicked() {
            acts.push(Act::Add(parent, true));
        }
    });
    out.targets.push(Target { rect: r.response.rect.with_max_x(ui.max_rect().right()), parent, index: n, depth, kind: Place::End });
}

/// The children of group `parent` (0 the root), `depth` groups deep.
#[allow(clippy::too_many_arguments)]
fn block(ui: &mut egui::Ui, b: &mut Block, parent: u32, depth: usize, p: &mut Panel, cx: &Cx, acts: &mut Vec<Act>, out: &mut Out, hints: &mut Hints) {
    for (index, n) in b.kids.iter_mut().enumerate() {
        let id = n.id;
        let at = Target { rect: egui::Rect::NOTHING, parent, index, depth, kind: Place::Row };
        ui.push_id(id, |ui| match &mut n.item {
            Item::Cond(c) => row(ui, id, c, at, p, cx, acts, out, hints),
            Item::Group(g) => card(ui, id, g, at, p, cx, acts, out, hints),
        });
    }
}

/// Marks where the node `id` being dragged would land under the pointer, and moves it there on release. A group never
/// lands in itself or in a group inside it.
fn drop_at(ui: &egui::Ui, targets: &[Target], tree: &Tree, id: u32, acts: &mut Vec<Act>) {
    let Some(pos) = ui.ctx().pointer_interact_pos() else { return };
    // Rows and headers do not overlap, so the last one holding the pointer is the one under it.
    let Some(t) = targets.iter().rev().find(|t| t.rect.contains(pos)) else { return };
    let frac = y_frac(t.rect, Some(pos));
    let (parent, index, zone) = match t.kind {
        Place::Row => {
            let z = zone_at(1.0, frac, false);
            (t.parent, if z == Zone::Before { t.index } else { t.index + 1 }, z)
        }
        Place::Head(g) => match zone_at(1.0, frac, true) {
            Zone::Before => (t.parent, t.index, Zone::Before),
            z => (g, 0, z),
        },
        Place::End => (t.parent, t.index, Zone::Before),
    };
    if parent != 0 && tree.holds(id, parent) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::NotAllowed);
        return;
    }
    let depth = if matches!(t.kind, Place::Head(_)) && zone != Zone::Before { t.depth + 1 } else { t.depth };
    paint_spot(ui.painter(), t.rect, Spot { zone, depth }, t.rect.left() + 8.0, 16.0);
    if ui.input(|i| i.pointer.any_released()) {
        acts.push(Act::Move(id, parent, index));
        egui::DragAndDrop::clear_payload(ui.ctx());
    }
}

/// The keys of the panel while no field has the focus: Up and Down select, Enter adds a row below, Space turns a row on
/// or off, Cmd+G groups, Cmd+Shift+G takes out of the group, Cmd+Backspace removes.
fn row_keys(ui: &egui::Ui, tree: &Tree, p: &mut Panel, acts: &mut Vec<Act>) {
    let none = egui::Modifiers::NONE;
    let (down, up, enter, space, unwrap, wrap, del) = ui.input_mut(|i| {
        (
            i.consume_key(none, Key::ArrowDown),
            i.consume_key(none, Key::ArrowUp),
            i.consume_key(none, Key::Enter),
            i.consume_key(none, Key::Space),
            // Checked first: Cmd+G would also match Cmd+Shift+G.
            i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, Key::G),
            i.consume_key(egui::Modifiers::COMMAND, Key::G),
            i.consume_key(egui::Modifiers::COMMAND, Key::Backspace),
        )
    });
    let order = tree.rows_in_order();
    let at = p.sel.and_then(|s| order.iter().position(|x| *x == s));
    if down || up {
        p.sel = match (at, down) {
            (Some(i), true) => order.get(i + 1).or(order.get(i)).copied(),
            (Some(i), false) => order.get(i.saturating_sub(1)).copied(),
            (None, true) => order.first().copied(),
            (None, false) => order.last().copied(),
        };
    }
    let Some(sel) = p.sel.filter(|s| tree.find(*s).is_some()) else {
        if enter {
            acts.push(Act::Add(0, false));
        }
        return;
    };
    let group = matches!(tree.find(sel).map(|n| &n.item), Some(Item::Group(_)));
    if enter {
        // A row is followed by a new one; a group gets one at its end.
        acts.push(if group { Act::Add(sel, false) } else { Act::After(sel) });
    }
    if space && !group {
        acts.push(Act::Toggle(sel));
    }
    if unwrap {
        acts.push(if group { Act::Ungroup(sel) } else { Act::Unwrap(sel) });
    } else if wrap {
        acts.push(Act::Wrap(sel));
    }
    if del {
        acts.push(Act::Remove(sel));
    }
}

/// The id a row's cells keep wherever the row moves.
pub(in crate::app) fn row_salt(id: u32) -> egui::Id {
    egui::Id::unique(("sql-filter-row", id))
}

/// A condition: its on box, column, operator, values and a cross. A click on the row's background selects it.
#[allow(clippy::too_many_arguments)]
fn row(ui: &mut egui::Ui, id: u32, c: &mut Cond, at: Target, p: &mut Panel, cx: &Cx, acts: &mut Vec<Act>, out: &mut Out, hints: &mut Hints) {
    let h = BUTTON_H + 8.0;
    let rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), h));
    out.targets.push(Target { rect, ..at });
    // Registered before the cells, so they take the clicks and drags they are under; the rest of the row is the handle.
    let bg = ui.interact(rect, row_salt(id).with("row"), Sense::click_and_drag());
    let name = format!("Condition {}", c.rule.col);
    bg.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Row, true, &name));
    let ghost = format!("{} {}", c.rule.col, crate::db::sql::rules::label(c.rule.op, c.rule.kind));
    drag_source_with(ui, &bg, FilterDrag(id), |p, r| {
        p.text(r.left_center() + egui::vec2(10.0, 0.0), egui::Align2::LEFT_CENTER, &ghost, FontId::monospace(TXT_SMALL), TEXT());
    });
    if bg.hovered() || bg.dragged() {
        grip_dots(ui.painter(), egui::pos2(rect.left() + 5.0, rect.center().y), FAINT());
    }
    let sel = p.sel == Some(id);
    if sel || bg.hovered() {
        ui.painter().rect_filled(rect, R_SM, if sel { tint(PRIMARY(), 22) } else { HOVER() });
    }
    if ui.rect_contains_pointer(rect) {
        p.hover = Some(id);
    }
    if cx.bad == Some(id) {
        ui.painter().rect_stroke(rect, R_SM, Stroke::new(1.0, ALERT()), egui::StrokeKind::Inside);
    }
    if bg.clicked() {
        p.sel = Some(id);
    }
    let pick = (p.pick == Some(id)).then(String::new);
    if pick.is_some() {
        p.pick = None;
    }
    let ops = p.ops == Some(id);
    if ops {
        p.ops = None;
    }
    // The menu at the right end is drawn in a child that takes no room, and the cells stop before it.
    let end = egui::Rect::from_min_max(egui::pos2(rect.right() - 6.0 - BUTTON_H, rect.top()), egui::pos2(rect.right() - 6.0, rect.bottom()));
    let mut menu = ui.new_child(egui::UiBuilder::new().max_rect(end).layout(egui::Layout::right_to_left(egui::Align::Center)));
    dots_menu(&mut menu, |ui| {
        menu_list(ui);
        let mut picks = Vec::new();
        menu_act(ui, &mut picks, Act::Duplicate(id), |ui| ui.button("Duplicate"));
        menu_act(ui, &mut picks, Act::Negate(id), |ui| ui.button("Negate"));
        menu_act(ui, &mut picks, Act::Wrap(id), |ui| ui.button("Convert to group"));
        menu_act(ui, &mut picks, Act::MoveBy(id, -1), |ui| ui.button("Move up"));
        menu_act(ui, &mut picks, Act::MoveBy(id, 1), |ui| ui.button("Move down"));
        popover_divider(ui);
        menu_act(ui, &mut picks, Act::Remove(id), |ui| danger_item(ui, "Delete"));
        acts.extend(picks);
    })
    .on_hover_text("Condition actions");
    // The hovered row's own count, faint before the menu.
    if p.hover == Some(id) && c.on && c.rule.complete() && let Some(Ok(n)) = p.hover_cache.get(&crate::db::sql::rules::rule_sql(&c.rule, cx.d, cx.bs)) {
        let label = format!("{}{}", if n.estimated { "\u{2248} " } else { "" }, grouped(n.n));
        let g = ui.painter().layout_no_wrap(label, FontId::monospace(TXT_CAPTION), FAINT());
        let dy = optical_dy(&g);
        ui.painter().galley(egui::pos2(end.left() - 6.0 - g.size().x, rect.center().y - g.size().y / 2.0 + dy), g, FAINT());
    }
    let cells = egui::Rect::from_min_max(rect.min + egui::vec2(6.0, 0.0), egui::pos2(end.left() - 4.0, rect.bottom()));
    ui.scope_builder(egui::UiBuilder::new().max_rect(cells).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
        ui.set_min_height(h);
        ui.spacing_mut().item_spacing.x = 6.0;
        // Room for the grip dots.
        ui.add_space(6.0);
        let (cb, _) = ui.allocate_exact_size(egui::vec2(16.0, BUTTON_H), Sense::hover());
        let on = check_box(ui, cb.center(), row_salt(id).with("on"), c.on, false);
        let state = c.on;
        on.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, state, "Condition on"));
        if on.on_hover_text(if c.on { "Leave this condition out" } else { "Use this condition" }).clicked() {
            acts.push(Act::Toggle(id));
        }
        if !c.on {
            ui.multiply_opacity(0.5);
        }
        let (changed, enter) = rule_editor(ui, &mut c.rule, cx.cols, row_salt(id), cx.d, pick, ops, hints);
        out.changed |= changed;
        out.run |= enter;
    });
}

/// A group as a tinted card: its own word, its rows and groups, and the buttons that add to it.
#[allow(clippy::too_many_arguments)]
fn card(ui: &mut egui::Ui, id: u32, g: &mut Block, at: Target, p: &mut Panel, cx: &Cx, acts: &mut Vec<Act>, out: &mut Out, hints: &mut Hints) {
    let (fill, edge) = match g.mode {
        Match::Any => (tint(C_LINK(), 18), tint(C_LINK(), 70)),
        Match::None => (tint(ALERT(), 18), tint(ALERT(), 70)),
        Match::All => (tint(TEXT(), 8), BORDER()),
    };
    let edge = if p.sel == Some(id) { PRIMARY() } else { edge };
    ui.add_space(2.0);
    egui::Frame::new().fill(fill).stroke(Stroke::new(1.0, edge)).corner_radius(R_MD).inner_margin(egui::Margin { left: 10, right: 6, top: 6, bottom: 4 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let head = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), BUTTON_H));
        out.targets.push(Target { rect: head, kind: Place::Head(id), ..at });
        // The header's empty part drags the group and selects it, under its controls like a row's background.
        let bg = ui.interact(head, row_salt(id).with("head"), Sense::click_and_drag());
        bg.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Row, true, "Group"));
        drag_source_with(ui, &bg, FilterDrag(id), |p, r| {
            p.text(r.left_center() + egui::vec2(10.0, 0.0), egui::Align2::LEFT_CENTER, "group", FontId::monospace(TXT_SMALL), TEXT());
        });
        if bg.clicked() {
            p.sel = Some(id);
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let before = g.mode;
            segmented(ui, &mut g.mode, &MODES);
            out.changed |= g.mode != before;
            ui.label(RichText::new("of these").size(TXT_SMALL).color(MUTED()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                dots_menu(ui, |ui| {
                    menu_list(ui);
                    let mut picks = Vec::new();
                    menu_act(ui, &mut picks, Act::Duplicate(id), |ui| ui.button("Duplicate"));
                    menu_act(ui, &mut picks, Act::Ungroup(id), |ui| ui.button("Ungroup"));
                    menu_act(ui, &mut picks, Act::MoveBy(id, -1), |ui| ui.button("Move up"));
                    menu_act(ui, &mut picks, Act::MoveBy(id, 1), |ui| ui.button("Move down"));
                    popover_divider(ui);
                    menu_act(ui, &mut picks, Act::Remove(id), |ui| danger_item(ui, "Delete group"));
                    acts.extend(picks);
                })
                .on_hover_text("Group actions");
            });
        });
        block(ui, g, id, at.depth + 1, p, cx, acts, out, hints);
        let n = g.kids.len();
        add_buttons(ui, id, n, at.depth + 1, acts, out);
    });
    ui.add_space(2.0);
}

/// The SQL the tree writes, with off rows as comments, and the way to edit it as text.
fn sql_pane(ui: &mut egui::Ui, tree: &Tree, p: &Panel, text: &str, d: Dialect, bs: bool, out: &mut Out) {
    caption(ui, "SQL");
    let shown = filter::preview(tree, d, bs);
    let mut job = sql_job(if shown.is_empty() { "-- every row" } else { &shown }, TXT_SMALL);
    job.wrap.max_width = ui.available_width() - 20.0;
    let g = ui.fonts_mut(|f| f.layout_job(job));
    egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(g);
    });
    ui.add_space(4.0);
    match p.count.shown(text) {
        Some(c) => {
            let (line, color) = count_line(c);
            let r = ui.label(RichText::new(line).size(TXT_SMALL).color(color));
            if let Err(e) = c {
                r.on_hover_text(&e.msg);
            }
        }
        // Asked before, so a count is on its way; the demo never asks and shows none.
        None if p.count.asked.is_some() => {
            ui.label(RichText::new("Counting\u{2026}").size(TXT_SMALL).color(FAINT()));
        }
        None => {}
    }
    out.as_sql |= quiet(ui, RichText::new("Edit as SQL").size(TXT_SMALL).color(PRIMARY())).on_hover_text("Switch the line to SQL with this WHERE").clicked();
}
