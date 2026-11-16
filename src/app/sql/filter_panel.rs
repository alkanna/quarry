//! The filter panel under the query line (design B in docs/sql/rules-builder.figcraft): the conditions in nested all, any
//! and none groups, the SQL they write, and Apply. It edits `Bar::tree`; the bar writes the WHERE text from it.

use super::filter_cells::rule_editor;
use crate::app::*;
use crate::db::sql::{
    filter::{self, Block, Cond, Item, Match, Tree},
    rules::Rule,
    Col, Dialect,
};
use crate::ui::fold;

#[derive(Default)]
pub(in crate::app) struct Panel {
    pub(in crate::app) open: bool,
    /// The selected row or group.
    pub(in crate::app) sel: Option<u32>,
    /// A row just added, whose column picker opens on its first frame.
    pub(in crate::app) pick: Option<u32>,
}

fn fold_id() -> egui::Id {
    egui::Id::unique("sql-filter-panel")
}

impl Panel {
    pub(in crate::app) fn toggle(&mut self, ctx: &egui::Context) {
        self.open = !self.open;
        fold::start(ctx, fold_id(), self.open);
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
}

enum Act {
    Remove(u32),
    Toggle(u32),
    /// A row, or a group holding one, added at the end of a group (0 the root).
    Add(u32, bool),
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
        let outer = if parent == 0 { Some(tree.root.mode) } else { tree.find(parent).and_then(|n| if let Item::Group(g) = &n.item { Some(g.mode) } else { None }) };
        let mode = if outer == Some(Match::All) { Match::Any } else { Match::All };
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
pub(super) fn panel(ui: &mut egui::Ui, tree: &mut Tree, p: &mut Panel, cols: &[Col], d: Dialect, bs: bool, keys: bool) -> Out {
    let mut out = Out::default();
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
                        conditions(ui, tree, p, cols, d, &mut acts, &mut out);
                    });
                    ui.add_space(16.0);
                    ui.vertical(|ui| {
                        ui.set_width(PANE);
                        sql_pane(ui, tree, d, bs, &mut out);
                    });
                });
            } else {
                conditions(ui, tree, p, cols, d, &mut acts, &mut out);
                ui.add_space(10.0);
                sql_pane(ui, tree, d, bs, &mut out);
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
    for a in acts {
        match a {
            Act::Remove(id) => {
                tree.remove(id);
                if p.sel == Some(id) {
                    p.sel = None;
                }
            }
            Act::Toggle(id) => {
                tree.toggle(id);
            }
            Act::Add(parent, group) => add(tree, p, parent, group, cols, d),
        }
        out.changed = true;
    }
    if p.open && keys {
        // A menu or a picker of the panel owns Esc while it is open.
        let popup = egui::Popup::is_any_open(ui.ctx());
        let (enter, esc) = ui.input_mut(|i| (i.consume_key(egui::Modifiers::COMMAND, Key::Enter), !popup && i.consume_key(egui::Modifiers::NONE, Key::Escape)));
        apply |= enter;
        if esc {
            p.close(ui.ctx());
        }
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
fn conditions(ui: &mut egui::Ui, tree: &mut Tree, p: &mut Panel, cols: &[Col], d: Dialect, acts: &mut Vec<Act>, out: &mut Out) {
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
    block(ui, &mut tree.root, p, cols, d, acts, out);
    add_buttons(ui, 0, acts);
}

fn add_buttons(ui: &mut egui::Ui, parent: u32, acts: &mut Vec<Act>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if quiet(ui, RichText::new("+ Condition").size(TXT_SMALL).color(PRIMARY())).clicked() {
            acts.push(Act::Add(parent, false));
        }
        if quiet(ui, RichText::new("+ Group").size(TXT_SMALL).color(PRIMARY())).clicked() {
            acts.push(Act::Add(parent, true));
        }
    });
}

fn block(ui: &mut egui::Ui, b: &mut Block, p: &mut Panel, cols: &[Col], d: Dialect, acts: &mut Vec<Act>, out: &mut Out) {
    for n in &mut b.kids {
        let id = n.id;
        ui.push_id(id, |ui| match &mut n.item {
            Item::Cond(c) => row(ui, id, c, p, cols, d, acts, out),
            Item::Group(g) => card(ui, id, g, p, cols, d, acts, out),
        });
    }
}

/// The id a row's cells keep wherever the row moves.
fn row_salt(id: u32) -> egui::Id {
    egui::Id::unique(("sql-filter-row", id))
}

/// A condition: its on box, column, operator, values and a cross. A click on the row's background selects it.
#[allow(clippy::too_many_arguments)]
fn row(ui: &mut egui::Ui, id: u32, c: &mut Cond, p: &mut Panel, cols: &[Col], d: Dialect, acts: &mut Vec<Act>, out: &mut Out) {
    let h = BUTTON_H + 8.0;
    let rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), h));
    // Registered before the cells, so they take the clicks they are under.
    let bg = ui.interact(rect, row_salt(id).with("row"), Sense::click());
    let sel = p.sel == Some(id);
    if sel || bg.hovered() {
        ui.painter().rect_filled(rect, R_SM, if sel { tint(PRIMARY(), 22) } else { HOVER() });
    }
    if bg.clicked() {
        p.sel = Some(id);
    }
    let pick = (p.pick == Some(id)).then(String::new);
    if pick.is_some() {
        p.pick = None;
    }
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(6.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
        ui.set_min_height(h);
        ui.spacing_mut().item_spacing.x = 6.0;
        let (cb, _) = ui.allocate_exact_size(egui::vec2(16.0, BUTTON_H), Sense::hover());
        let on = check_box(ui, cb.center(), row_salt(id).with("on"), c.on, false);
        let state = c.on;
        on.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, state, "Condition on"));
        if on.on_hover_text(if c.on { "Leave this condition out" } else { "Use this condition" }).clicked() {
            acts.push(Act::Toggle(id));
        }
        // The right end first, so the cells cannot push it out of the row.
        let end = egui::Rect::from_min_max(egui::pos2(rect.right() - 6.0 - CLOSE_W - 4.0, rect.top()), egui::pos2(rect.right() - 6.0, rect.bottom()));
        if ui.scope_builder(egui::UiBuilder::new().max_rect(end).layout(egui::Layout::right_to_left(egui::Align::Center)), |ui| close_x(ui).on_hover_text("Remove condition").clicked()).inner {
            acts.push(Act::Remove(id));
        }
        if !c.on {
            ui.multiply_opacity(0.5);
        }
        let (changed, enter) = rule_editor(ui, &mut c.rule, cols, row_salt(id), d, pick);
        out.changed |= changed;
        out.run |= enter;
    });
}

/// A group as a tinted card: its own word, its rows and groups, and the buttons that add to it.
#[allow(clippy::too_many_arguments)]
fn card(ui: &mut egui::Ui, id: u32, g: &mut Block, p: &mut Panel, cols: &[Col], d: Dialect, acts: &mut Vec<Act>, out: &mut Out) {
    let (fill, edge) = match g.mode {
        Match::Any => (tint(C_LINK(), 18), tint(C_LINK(), 70)),
        Match::None => (tint(ALERT(), 18), tint(ALERT(), 70)),
        Match::All => (tint(TEXT(), 8), BORDER()),
    };
    let edge = if p.sel == Some(id) { PRIMARY() } else { edge };
    ui.add_space(2.0);
    egui::Frame::new().fill(fill).stroke(Stroke::new(1.0, edge)).corner_radius(R_MD).inner_margin(egui::Margin { left: 10, right: 6, top: 6, bottom: 4 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let before = g.mode;
            segmented(ui, &mut g.mode, &MODES);
            out.changed |= g.mode != before;
            ui.label(RichText::new("of these").size(TXT_SMALL).color(MUTED()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if close_x(ui).on_hover_text("Remove the group and its conditions").clicked() {
                    acts.push(Act::Remove(id));
                }
            });
        });
        block(ui, g, p, cols, d, acts, out);
        add_buttons(ui, id, acts);
    });
    ui.add_space(2.0);
}

/// The SQL the tree writes, with off rows as comments, and the way to edit it as text.
fn sql_pane(ui: &mut egui::Ui, tree: &Tree, d: Dialect, bs: bool, out: &mut Out) {
    caption(ui, "SQL");
    let text = filter::preview(tree, d, bs);
    let mut job = sql_job(if text.is_empty() { "-- every row" } else { &text }, TXT_SMALL);
    job.wrap.max_width = ui.available_width() - 20.0;
    let g = ui.fonts_mut(|f| f.layout_job(job));
    egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(g);
    });
    ui.add_space(4.0);
    out.as_sql |= quiet(ui, RichText::new("Edit as SQL").size(TXT_SMALL).color(PRIMARY())).on_hover_text("Switch the line to SQL with this WHERE").clicked();
}
