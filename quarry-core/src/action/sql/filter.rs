//! The filter panel's structural edits of the Content bar's condition tree: add, remove, move, wrap and the rest. A row's
//! column, operator and value are edited in place while the panel draws (src/app/sql/filter_panel.rs).

use crate::core::Core;
use crate::db::sql::filter::{self, Block, Cond, Item, Match, Tree};
use crate::db::sql::rules::{default_case, group_of, input, rule_sql, Group, Input, Op, Rule};
use crate::db::sql::{Col, Dialect};
use crate::state::sql::bar::{and_onto, write_tree};
use crate::state::sql::filter_panel::Panel;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum FilterA {
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
    /// Clear: every condition goes.
    Clear,
    /// The word of group `id` (0 the root).
    Mode(u32, Match),
    /// Cmd+F in Rules mode: the panel opens, with a new row when it has none. The WHERE text is not rewritten.
    Open,
    /// A column header's funnel: a rule on `col` joins the filter with AND and the first page is read again. `value` is
    /// ignored by operators that take none.
    Quick { col: String, op: Op, value: String },
}

/// The word a new group takes inside `parent`: the other one than its parent's, so wrapping says something.
pub fn other_word(tree: &Tree, parent: u32) -> Match {
    let outer = if parent == 0 { Some(tree.root.mode) } else { tree.find(parent).and_then(|n| if let Item::Group(g) = &n.item { Some(g.mode) } else { None }) };
    if outer == Some(Match::All) { Match::Any } else { Match::All }
}

/// A new row on the table's first column, or a group of the other word than `parent` holding one, at the end of `parent`
/// (0 the root), selected with its column picker open.
pub fn add(tree: &mut Tree, p: &mut Panel, parent: u32, group: bool, cols: &[Col], d: Dialect) {
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

impl Core {
    /// One structural edit of the open tab's filter tree, then the WHERE text written from it, as the bar does after its panel.
    pub(super) fn sql_filter(&mut self, a: FilterA) {
        let (d, bs) = (self.sql_dialect(), self.sql_bs());
        let t = &mut self.tabs[self.cur];
        let Some(s) = t.sql.as_mut() else { return };
        let cols = if s.bar.table_cols.is_empty() { s.cols.clone() } else { s.bar.table_cols.clone() };
        // SQL mode, or a text the tree was not read from yet, which writing the tree would not reach.
        let text_only = s.bar.c.sql || s.bar.src != t.filter_txt;
        let pairs: Vec<_> = cols.iter().map(|c| (c.name.clone(), c.kind)).collect();
        let mut reload = false;
        let (tree, p) = (&mut s.bar.tree, &mut s.bar.panel);
        match a {
            FilterA::Quick { col, op, value } => {
                let Some(c) = cols.iter().find(|c| c.name == col) else { return };
                // Spaces around a text value can be what it matches; around a number or a date they are slips.
                let value = if group_of(c.kind) == Group::Text { value } else { value.trim().to_string() };
                let values = if input(op) == Input::One { vec![value] } else { Vec::new() };
                let rule = Rule { col, kind: c.kind, op, values, case: default_case(op, d) };
                if !rule.complete() {
                    return;
                }
                (t.page, reload) = (0, true);
                if text_only {
                    // The text keeps its spelling; write_tree below skips a text the tree was not read from.
                    t.filter_txt = and_onto(&t.filter_txt, [rule_sql(&rule, d, bs)], &pairs, d, bs);
                } else if tree.and(Item::Cond(Cond { rule, on: true })) {
                    // The old rows got new ids, so a selection would name another row.
                    (p.sel, p.pick) = (None, None);
                }
            }
            FilterA::Open => {
                if tree.root.kids.is_empty() {
                    add(tree, p, 0, false, &cols, d);
                }
                if !p.open {
                    p.open = true;
                    p.forget_counts();
                }
                return;
            }
            FilterA::Remove(id) => {
                // The selection moves on to the next row, so Cmd+Backspace can be pressed again.
                let order = tree.rows_in_order();
                let next = order.iter().position(|x| *x == id).and_then(|i| order.get(i + 1).or_else(|| i.checked_sub(1).and_then(|j| order.get(j)))).copied();
                tree.remove(id);
                if p.sel == Some(id) {
                    p.sel = next.filter(|n| tree.find(*n).is_some());
                }
            }
            FilterA::Toggle(id) => {
                tree.toggle(id);
            }
            FilterA::Add(parent, group) => add(tree, p, parent, group, &cols, d),
            FilterA::After(id) => {
                if let Some(c) = cols.first() {
                    let new = tree.insert_after(id, Item::Cond(Cond { rule: Rule::new(&c.name, c.kind, d), on: true }));
                    (p.sel, p.pick) = (new, new);
                }
            }
            FilterA::Duplicate(id) => p.sel = tree.duplicate(id).or(p.sel),
            FilterA::Negate(id) => {
                tree.negate(id, d);
            }
            FilterA::Wrap(id) => {
                let mode = other_word(tree, tree.parent_of(id).unwrap_or(0));
                p.sel = tree.wrap(id, mode).or(p.sel);
            }
            FilterA::Ungroup(id) => {
                tree.ungroup(id);
            }
            FilterA::MoveBy(id, by) => {
                tree.move_by(id, by);
            }
            FilterA::Unwrap(id) => {
                tree.unwrap(id);
            }
            FilterA::Move(id, parent, index) => {
                tree.move_to(id, parent, index);
                p.sel = Some(id);
            }
            FilterA::Clear => {
                tree.root.kids.clear();
                (p.sel, p.pick) = (None, None);
            }
            FilterA::Mode(0, mode) => tree.root.mode = mode,
            FilterA::Mode(id, mode) => {
                if let Some(Item::Group(g)) = tree.find_mut(id).map(|n| &mut n.item) {
                    g.mode = mode;
                }
            }
        }
        write_tree(&mut s.bar, &mut t.filter_txt, &pairs, d, bs);
        s.bar.tree_json = filter::to_json(&s.bar.tree);
        if reload {
            self.sql_reload();
        }
    }
}
