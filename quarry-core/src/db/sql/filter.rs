//! The filter panel's tree: conditions in nested all, any and none groups, the WHERE text it writes, and the way back
//! from text it wrote. The WHERE text stays the only thing a filter is: the panel edits a tree, writes the text on every
//! edit, and reads any text from elsewhere back into a tree.

use super::rules::{self, close_of, lex, ops_for, rule_of, word, Rule, T};
use super::value::Kind;
use super::Dialect;
use serde::{Deserialize, Serialize};
use std::ops::Range;

/// How a group joins its children: all of them (AND), any (OR) or none (NOT (a OR b)).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Match {
    #[default]
    All,
    Any,
    None,
}

/// A condition row. An off row stays in the tree and writes no SQL.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Cond {
    pub rule: Rule,
    pub on: bool,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum Item {
    Cond(Cond),
    Group(Block),
}

/// A row or a group. The id names it while the panel is open and is not saved.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    #[serde(skip)]
    pub id: u32,
    pub item: Item,
}

impl PartialEq for Node {
    fn eq(&self, o: &Node) -> bool {
        self.item == o.item
    }
}

#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Block {
    pub mode: Match,
    pub kids: Vec<Node>,
}

/// Id 0 names the root.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Tree {
    pub root: Block,
    next: u32,
}

impl PartialEq for Tree {
    fn eq(&self, o: &Tree) -> bool {
        self.root == o.root
    }
}

/// Whether a node writes any SQL: an on, complete row, or a group holding one.
pub fn live(n: &Node) -> bool {
    match &n.item {
        Item::Cond(c) => c.on && c.rule.complete(),
        Item::Group(b) => b.kids.iter().any(live),
    }
}

/// Root children are joined bare and every child group is in parentheses, even with one child or the same word as its
/// parent, so text and tree read the same at any depth. Flattening is an edit, never something the writer does.
fn write(b: &Block, root: bool, d: Dialect, bs: bool, out: &mut String, spans: &mut Vec<(u32, Range<usize>)>) {
    let (open, close) = match (b.mode, root) {
        (Match::None, _) => ("NOT (", ")"),
        (_, true) => ("", ""),
        _ => ("(", ")"),
    };
    out.push_str(open);
    for (i, n) in b.kids.iter().filter(|n| live(n)).enumerate() {
        if i > 0 {
            out.push_str(if b.mode == Match::All { " AND " } else { " OR " });
        }
        let at = out.len();
        match &n.item {
            Item::Cond(c) => out.push_str(&rules::rule_sql(&c.rule, d, bs)),
            Item::Group(g) => write(g, false, d, bs, out, spans),
        }
        spans.push((n.id, at..out.len()));
    }
    out.push_str(close);
}

/// The WHERE text and the byte range each live row and group wrote, to pin a server error to its row.
pub fn spans(t: &Tree, d: Dialect, bs: bool) -> (String, Vec<(u32, Range<usize>)>) {
    let (mut out, mut spans) = (String::new(), Vec::new());
    if t.root.kids.iter().any(live) {
        write(&t.root, true, d, bs, &mut out, &mut spans);
    }
    (out, spans)
}

pub fn to_sql(t: &Tree, d: Dialect, bs: bool) -> String {
    spans(t, d, bs).0
}

/// The panel's SQL pane: the WHERE text, then each off row as a comment, so what they would add stays in view.
pub fn preview(t: &Tree, d: Dialect, bs: bool) -> String {
    let mut s = to_sql(t, d, bs);
    for id in t.rows_in_order() {
        if let Some(Node { item: Item::Cond(c), .. }) = t.find(id)
            && !c.on
            && c.rule.complete()
        {
            if !s.is_empty() {
                s.push('\n');
            }
            s += &format!("-- off: {}", rules::rule_sql(&c.rule, d, bs));
        }
    }
    s
}

/// Splits at top-level AND or OR. BETWEEN owns the AND after its first value. Mixed words at one level are refused:
/// their precedence is the reader's to guess, so such text stays SQL. The word is None for one part.
fn split(t: &[T]) -> Option<(Vec<&[T]>, Option<bool>)> {
    let (mut parts, mut start, mut depth, mut between) = (Vec::new(), 0, 0, false);
    let mut any: Option<bool> = None;
    for (i, x) in t.iter().enumerate() {
        match x {
            T::P('(') => depth += 1,
            T::P(')') => depth -= 1,
            T::Word(w) if depth == 0 && w == "BETWEEN" => between = true,
            T::Word(w) if depth == 0 && (w == "AND" || w == "OR") => {
                if w == "AND" && between {
                    between = false;
                    continue;
                }
                let or = w == "OR";
                if any.is_some_and(|j| j != or) {
                    return None;
                }
                any = Some(or);
                parts.push(&t[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&t[start..]);
    Some((parts, any))
}

/// Deeper text than this stays SQL rather than recursing without end.
const DEPTH: usize = 32;

fn node(item: Item) -> Node {
    Node { id: 0, item }
}

fn kids(parts: &[&[T]], cols: &[(String, Kind)], d: Dialect, depth: usize) -> Option<Vec<Node>> {
    parts.iter().map(|p| expr(p, cols, d, depth).map(node)).collect()
}

/// One operand: a rule first, because some rules have parentheses or a NOT of their own (NOT IN ... OR NULL, MySQL's
/// this month and NOT (c <=> v)), then a none group, then a group.
fn expr(t: &[T], cols: &[(String, Kind)], d: Dialect, depth: usize) -> Option<Item> {
    if depth > DEPTH {
        return None;
    }
    // A rule the column does not offer is a group the writer wrote, such as NOT IN and IS NULL in an any group on an enum.
    if let Some(rule) = rule_of(t, cols, d).filter(|r| ops_for(r.kind, d).contains(&r.op)) {
        return Some(Item::Cond(Cond { rule, on: true }));
    }
    if let [n, T::P('('), ..] = t
        && word(n, "NOT")
        && close_of(t, 1) == Some(t.len() - 1)
    {
        let (parts, any) = split(&t[2..t.len() - 1])?;
        // The writer joins a none group with OR only.
        if any == Some(false) {
            return None;
        }
        return Some(Item::Group(Block { mode: Match::None, kids: kids(&parts, cols, d, depth + 1)? }));
    }
    if t.first() == Some(&T::P('(')) && close_of(t, 0) == Some(t.len() - 1) {
        let (parts, any) = split(&t[1..t.len() - 1])?;
        let mode = if any == Some(true) { Match::Any } else { Match::All };
        return Some(Item::Group(Block { mode, kids: kids(&parts, cols, d, depth + 1)? }));
    }
    None
}

fn valid(b: &Block, d: Dialect, complete: bool) -> bool {
    b.kids.iter().all(|n| match &n.item {
        Item::Cond(c) => ops_for(c.rule.kind, d).contains(&c.rule.op) && (!complete || c.rule.complete()),
        Item::Group(g) => valid(g, d, complete),
    })
}

/// Reads text back into a tree when it is what `to_sql` writes. Anything else (subqueries, mixed AND and OR at one
/// level, other functions) gives None, which keeps the bar in SQL mode. The tree is checked by writing it out again, so
/// a successful parse never changes the query.
pub fn parse(text: &str, cols: &[(String, Kind)], d: Dialect, bs: bool) -> Option<Tree> {
    // The reader decodes backslash escapes, which this server does not have, so such text stays SQL.
    if !bs && text.contains('\\') {
        return None;
    }
    let toks = lex(text, cols, d)?;
    let mut t = Tree::default();
    if toks.is_empty() {
        return Some(t);
    }
    let (parts, any) = split(&toks)?;
    t.root = if parts.len() == 1 {
        match expr(&toks, cols, d, 0)? {
            // A whole text in NOT (...) is a none root.
            Item::Group(b) if b.mode == Match::None => b,
            item => Block { mode: Match::All, kids: vec![node(item)] },
        }
    } else {
        Block { mode: if any == Some(true) { Match::Any } else { Match::All }, kids: kids(&parts, cols, d, 1)? }
    };
    if !valid(&t.root, d, true) {
        return None;
    }
    t.renumber();
    (lex(&to_sql(&t, d, true), cols, d)? == toks).then_some(t)
}

/// A saved tree, the way a restored tab keeps its groups, order and off rows.
pub fn to_json(t: &Tree) -> String {
    serde_json::to_string(&t.root).unwrap_or_default()
}

/// A saved tree with each rule's kind read from the table's columns again. A column that is gone, or an operator this
/// server's columns do not offer, gives None.
pub fn from_json(json: &str, cols: &[(String, Kind)], d: Dialect) -> Option<Tree> {
    fn kinds(b: &mut Block, cols: &[(String, Kind)]) -> Option<()> {
        for n in &mut b.kids {
            match &mut n.item {
                Item::Cond(c) => c.rule.kind = cols.iter().find(|(name, _)| *name == c.rule.col)?.1,
                Item::Group(g) => kinds(g, cols)?,
            }
        }
        Some(())
    }
    let mut t = Tree { root: serde_json::from_str(json).ok()?, next: 0 };
    kinds(&mut t.root, cols)?;
    valid(&t.root, d, false).then_some(())?;
    t.renumber();
    Some(t)
}

/// The saved tree when it still writes `text`, so its groups and off rows come back; otherwise `text` read afresh, as
/// when the filter was edited as SQL or the table changed since.
pub fn reconcile(json: &str, text: &str, cols: &[(String, Kind)], d: Dialect, bs: bool) -> Option<Tree> {
    match from_json(json, cols, d) {
        Some(t) if to_sql(&t, d, bs) == text => Some(t),
        _ => parse(text, cols, d, bs),
    }
}

/// The opposite operator, where the column offers one.
fn opposite(op: rules::Op) -> Option<rules::Op> {
    use rules::Op::*;
    // NOT IN ... OR NULL is the opposite of IN, but IN's is plain NOT IN: like the other pairs it leaves NULL out, and a
    // column without NOT IN wraps the row in a none group instead.
    if op == NotInOrNull {
        return Some(In);
    }
    let pairs = [(Eq, Ne), (Gt, Le), (Lt, Ge), (In, NotIn), (IsNull, NotNull), (Contains, NotContains), (StartsWith, NotStartsWith), (EndsWith, NotEndsWith), (Like, NotLike), (Regexp, NotRegexp), (Empty, NotEmpty), (JsonPathEq, JsonPathNe)];
    pairs.iter().find_map(|&(a, b)| if a == op { Some(b) } else if b == op { Some(a) } else { None })
}

fn path_in(b: &Block, id: u32, out: &mut Vec<usize>) -> bool {
    for (i, n) in b.kids.iter().enumerate() {
        out.push(i);
        if n.id == id {
            return true;
        }
        if let Item::Group(g) = &n.item
            && path_in(g, id, out)
        {
            return true;
        }
        out.pop();
    }
    false
}

fn order(b: &Block, out: &mut Vec<u32>) {
    for n in &b.kids {
        out.push(n.id);
        if let Item::Group(g) = &n.item {
            order(g, out);
        }
    }
}

fn prune_in(b: &mut Block) {
    for n in &mut b.kids {
        if let Item::Group(g) = &mut n.item {
            prune_in(g);
        }
    }
    b.kids.retain(|n| !matches!(&n.item, Item::Group(g) if g.kids.is_empty()));
}

fn number(b: &mut Block, next: &mut u32) {
    for n in &mut b.kids {
        *next += 1;
        n.id = *next;
        if let Item::Group(g) = &mut n.item {
            number(g, next);
        }
    }
}

impl Tree {
    /// A tree of `root` with fresh ids.
    #[cfg(any(test, feature = "test-support"))]
    pub fn new(root: Block) -> Tree {
        let mut t = Tree { root, next: 0 };
        t.renumber();
        t
    }

    /// Gives every node a fresh id, after a parse or a load.
    fn renumber(&mut self) {
        number(&mut self.root, &mut self.next);
    }

    /// `item` as a node with fresh ids, its children's too.
    fn fresh(&mut self, item: Item) -> Node {
        let mut b = Block { mode: Match::All, kids: vec![Node { id: 0, item }] };
        number(&mut b, &mut self.next);
        b.kids.remove(0)
    }

    fn path(&self, id: u32) -> Option<Vec<usize>> {
        let mut p = Vec::new();
        path_in(&self.root, id, &mut p).then_some(p)
    }

    fn block_at(&mut self, p: &[usize]) -> Option<&mut Block> {
        let mut b = &mut self.root;
        for &i in p {
            match &mut b.kids.get_mut(i)?.item {
                Item::Group(g) => b = g,
                Item::Cond(_) => return None,
            }
        }
        Some(b)
    }

    /// The group with id `id`, 0 being the root.
    fn group_mut(&mut self, id: u32) -> Option<&mut Block> {
        if id == 0 {
            return Some(&mut self.root);
        }
        match &mut self.find_mut(id)?.item {
            Item::Group(g) => Some(g),
            Item::Cond(_) => None,
        }
    }

    pub fn find(&self, id: u32) -> Option<&Node> {
        let p = self.path(id)?;
        let mut b = &self.root;
        for &i in &p[..p.len() - 1] {
            let Item::Group(g) = &b.kids[i].item else { return None };
            b = g;
        }
        b.kids.get(*p.last()?)
    }

    pub fn find_mut(&mut self, id: u32) -> Option<&mut Node> {
        let p = self.path(id)?;
        let (last, up) = p.split_last()?;
        self.block_at(up)?.kids.get_mut(*last)
    }

    /// The id of the group holding `id`, 0 for the root.
    pub fn parent_of(&self, id: u32) -> Option<u32> {
        let p = self.path(id)?;
        if p.len() == 1 {
            return Some(0);
        }
        let mut b = &self.root;
        let mut parent = 0;
        for &i in &p[..p.len() - 1] {
            parent = b.kids[i].id;
            let Item::Group(g) = &b.kids[i].item else { return None };
            b = g;
        }
        Some(parent)
    }

    /// Whether `id` is `outer` or inside it.
    pub fn holds(&self, outer: u32, id: u32) -> bool {
        matches!((self.path(outer), self.path(id)), (Some(a), Some(b)) if b.starts_with(&a))
    }

    /// Every row and group in the order the panel draws them.
    pub fn rows_in_order(&self) -> Vec<u32> {
        let mut out = Vec::new();
        order(&self.root, &mut out);
        out
    }

    /// Removes groups left with nothing in them. Every edit ends with it.
    fn prune(&mut self) {
        prune_in(&mut self.root);
    }

    /// Adds `item` so it narrows the whole filter: at the end of an all root, or beside the old root moved into a group.
    /// Returns whether the old root moved, which gives its rows new ids.
    pub fn and(&mut self, item: Item) -> bool {
        // An any root of one row reads the same as an all root.
        let r = &self.root;
        let wrap = !r.kids.is_empty() && r.mode != Match::All && !(r.mode == Match::Any && r.kids.len() == 1);
        if wrap {
            let old = std::mem::take(&mut self.root);
            self.push(0, Item::Group(old));
        }
        self.root.mode = Match::All;
        self.push(0, item);
        wrap
    }

    /// Whether a condition in use is on column `col`.
    pub fn uses(&self, col: &str) -> bool {
        fn walk(b: &Block, col: &str) -> bool {
            b.kids.iter().any(|n| match &n.item {
                Item::Cond(c) => c.on && c.rule.col == col,
                Item::Group(g) => walk(g, col),
            })
        }
        walk(&self.root, col)
    }

    /// Adds `item` at the end of group `parent` (0 for the root) and returns its id.
    pub fn push(&mut self, parent: u32, item: Item) -> Option<u32> {
        let n = self.fresh(item);
        let id = n.id;
        self.group_mut(parent)?.kids.push(n);
        Some(id)
    }

    /// Adds `item` right after `id` in the same group and returns its id.
    pub fn insert_after(&mut self, id: u32, item: Item) -> Option<u32> {
        let p = self.path(id)?;
        let (last, up) = p.split_last()?;
        let n = self.fresh(item);
        let new = n.id;
        self.block_at(up)?.kids.insert(last + 1, n);
        Some(new)
    }

    pub fn remove(&mut self, id: u32) -> Option<Node> {
        let p = self.path(id)?;
        let (last, up) = p.split_last()?;
        let n = self.block_at(up)?.kids.remove(*last);
        self.prune();
        Some(n)
    }

    /// Puts `id` in a new group of its own where it stood, and returns the group's id.
    pub fn wrap(&mut self, id: u32, mode: Match) -> Option<u32> {
        let p = self.path(id)?;
        let (last, up) = p.split_last()?;
        let mut g = self.fresh(Item::Group(Block { mode, kids: Vec::new() }));
        let gid = g.id;
        let b = self.block_at(up)?;
        let n = std::mem::replace(&mut b.kids[*last], Node { id: 0, item: Item::Group(Block::default()) });
        if let Item::Group(inner) = &mut g.item {
            inner.kids.push(n);
        }
        b.kids[*last] = g;
        Some(gid)
    }

    /// Takes `id` out of its group to just after that group. A row of the root stays where it is.
    pub fn unwrap(&mut self, id: u32) -> Option<()> {
        let p = self.path(id)?;
        if p.len() < 2 {
            return None;
        }
        let n = self.block_at(&p[..p.len() - 1])?.kids.remove(p[p.len() - 1]);
        let (gi, up) = p[..p.len() - 1].split_last()?;
        self.block_at(up)?.kids.insert(gi + 1, n);
        self.prune();
        Some(())
    }

    /// Replaces group `id` with its children, in place.
    pub fn ungroup(&mut self, id: u32) -> Option<()> {
        let p = self.path(id)?;
        let (last, up) = p.split_last()?;
        let b = self.block_at(up)?;
        let Item::Group(g) = &b.kids[*last].item else { return None };
        let kids = g.kids.clone();
        b.kids.splice(*last..*last + 1, kids);
        Some(())
    }

    /// Moves `id` into group `parent` (0 for the root) before the child at `index`, counted as the list stood before the
    /// move. A group never moves into itself or a group inside it.
    pub fn move_to(&mut self, id: u32, parent: u32, index: usize) -> Option<()> {
        if parent != 0 && self.holds(id, parent) {
            return None;
        }
        // A target that is not a group is refused before anything moves.
        self.group_mut(parent)?;
        let from = self.path(id)?;
        let mut index = index;
        if self.parent_of(id)? == parent && from[from.len() - 1] < index {
            index -= 1;
        }
        let (last, up) = from.split_last()?;
        let n = self.block_at(up)?.kids.remove(*last);
        let to = self.group_mut(parent)?;
        let index = index.min(to.kids.len());
        to.kids.insert(index, n);
        self.prune();
        Some(())
    }

    /// Moves `id` up (-1) or down (+1) among its siblings.
    pub fn move_by(&mut self, id: u32, by: isize) -> Option<()> {
        let p = self.path(id)?;
        let (last, up) = p.split_last()?;
        let b = self.block_at(up)?;
        let to = last.checked_add_signed(by).filter(|t| *t < b.kids.len())?;
        b.kids.swap(*last, to);
        Some(())
    }

    /// Copies `id` with all it holds right after it, and returns the copy's id.
    pub fn duplicate(&mut self, id: u32) -> Option<u32> {
        let item = self.find(id)?.item.clone();
        self.insert_after(id, item)
    }

    pub fn toggle(&mut self, id: u32) -> Option<()> {
        match &mut self.find_mut(id)?.item {
            Item::Cond(c) => c.on = !c.on,
            Item::Group(_) => return None,
        }
        Some(())
    }

    /// The opposite of a row by its operator when the column offers one, else the row in a none group. A none group
    /// becomes an any group, an any group a none group, and an all group goes into a none group.
    pub fn negate(&mut self, id: u32, d: Dialect) -> Option<()> {
        let wrap = match &mut self.find_mut(id)?.item {
            Item::Cond(c) => match opposite(c.rule.op).filter(|o| ops_for(c.rule.kind, d).contains(o)) {
                Some(o) => {
                    c.rule.op = o;
                    false
                }
                None => true,
            },
            Item::Group(g) if g.mode == Match::None => {
                g.mode = Match::Any;
                false
            }
            Item::Group(g) if g.mode == Match::Any => {
                g.mode = Match::None;
                false
            }
            Item::Group(_) => true,
        };
        if wrap {
            self.wrap(id, Match::None)?;
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rules::Op;

    fn cols() -> Vec<(String, Kind)> {
        [("id", Kind::Int), ("total", Kind::Decimal), ("name", Kind::Text), ("created_at", Kind::DateTime), ("status", Kind::Enum)].iter().map(|(n, k)| (n.to_string(), *k)).collect()
    }

    fn cond(col: &str, kind: Kind, op: Op, v: &[&str], d: Dialect) -> Item {
        Item::Cond(Cond { rule: Rule { col: col.into(), kind, op, values: v.iter().map(|s| s.to_string()).collect(), case: rules::default_case(op, d) }, on: true })
    }

    fn group(mode: Match, kids: Vec<Item>) -> Item {
        Item::Group(Block { mode, kids: kids.into_iter().map(node).collect() })
    }

    fn tree(mode: Match, kids: Vec<Item>) -> Tree {
        let mut t = Tree { root: Block { mode, kids: kids.into_iter().map(node).collect() }, next: 0 };
        t.renumber();
        t
    }

    fn a(d: Dialect) -> Item {
        cond("total", Kind::Decimal, Op::Gt, &["50"], d)
    }
    fn b(d: Dialect) -> Item {
        cond("status", Kind::Enum, Op::Eq, &["paid"], d)
    }
    fn c(d: Dialect) -> Item {
        cond("id", Kind::Int, Op::Lt, &["9"], d)
    }

    const MY: Dialect = Dialect::MySql;

    #[test]
    fn the_writer_puts_every_group_in_parentheses() {
        let t = tree(Match::All, vec![a(MY), group(Match::Any, vec![b(MY), c(MY)])]);
        assert_eq!(to_sql(&t, MY, true), "`total` > 50 AND (`status` = 'paid' OR `id` < 9)");
        let t = tree(Match::Any, vec![a(MY), group(Match::Any, vec![b(MY)])]);
        assert_eq!(to_sql(&t, MY, true), "`total` > 50 OR (`status` = 'paid')");
        let t = tree(Match::All, vec![a(MY), group(Match::None, vec![b(MY), c(MY)])]);
        assert_eq!(to_sql(&t, MY, true), "`total` > 50 AND NOT (`status` = 'paid' OR `id` < 9)");
        let t = tree(Match::None, vec![a(MY), b(MY)]);
        assert_eq!(to_sql(&t, MY, true), "NOT (`total` > 50 OR `status` = 'paid')");
        // Empty groups, off rows, incomplete rows and groups of only those write nothing.
        let mut off = a(MY);
        if let Item::Cond(c) = &mut off {
            c.on = false;
        }
        let blank = cond("total", Kind::Decimal, Op::Gt, &[""], MY);
        let t = tree(Match::All, vec![off.clone(), group(Match::Any, vec![]), group(Match::Any, vec![off.clone(), blank.clone()]), b(MY)]);
        assert_eq!(to_sql(&t, MY, true), "`status` = 'paid'");
        assert_eq!(to_sql(&tree(Match::None, vec![off, blank]), MY, true), "");
    }

    #[test]
    fn the_preview_lists_off_rows_as_comments() {
        let mut t = tree(Match::All, vec![a(MY), b(MY)]);
        let id = t.rows_in_order()[1];
        t.toggle(id).unwrap();
        assert_eq!(preview(&t, MY, true), "`total` > 50\n-- off: `status` = 'paid'");
        t.toggle(t.rows_in_order()[0]).unwrap();
        assert_eq!(preview(&t, MY, true), "-- off: `total` > 50\n-- off: `status` = 'paid'");
    }

    #[test]
    fn nested_trees_round_trip_on_every_engine() {
        for d in [Dialect::MySql, Dialect::MariaDb, Dialect::Postgres] {
            let shapes = [
                tree(Match::All, vec![a(d), b(d)]),
                tree(Match::Any, vec![a(d), group(Match::All, vec![b(d), c(d)])]),
                tree(Match::All, vec![group(Match::Any, vec![a(d), group(Match::All, vec![b(d), group(Match::None, vec![c(d), a(d)])])]), c(d)]),
                tree(Match::None, vec![a(d), group(Match::All, vec![b(d), c(d)])]),
                tree(Match::All, vec![group(Match::Any, vec![a(d), b(d)])]),
                tree(Match::All, vec![a(d), group(Match::All, vec![b(d)])]),
                tree(Match::All, vec![a(d), group(Match::None, vec![b(d)])]),
                // Spelled like the NOT IN ... OR NULL rule, which an enum column does not offer.
                tree(Match::All, vec![a(d), group(Match::Any, vec![cond("status", Kind::Enum, Op::NotIn, &["a"], d), cond("status", Kind::Enum, Op::IsNull, &[], d)])]),
            ];
            for t in shapes {
                let sql = to_sql(&t, d, false);
                let back = parse(&sql, &cols(), d, false).unwrap_or_else(|| panic!("{d:?}: {sql}"));
                assert_eq!(back, t, "{d:?}: {sql}");
            }
        }
    }

    #[test]
    fn text_the_writer_would_not_write_stays_sql() {
        for text in [
            "total > 50 AND total < 5 OR id = 1",
            "total > 50 AND (status = 'paid' OR id = 1 AND id = 2)",
            "NOT (total > 50 AND id = 1)",
            "total > 50 AND id IN (SELECT 1)",
            "LOWER(name) = 'x'",
            "total > 50 AND",
        ] {
            assert!(parse(text, &cols(), MY, true).is_none(), "{text}");
        }
        // Deeper than the cap.
        let deep = format!("{}id = 1{}", "(".repeat(40), ")".repeat(40));
        assert!(parse(&deep, &cols(), MY, true).is_none());
        assert_eq!(parse("", &cols(), MY, true), Some(Tree::default()));
        // Bare names and lower case words read like what the writer sends.
        assert_eq!(parse("status = 'paid' and (total between 1 and 2 or id = 3)", &cols(), MY, true).unwrap().root.kids.len(), 2);
    }

    #[test]
    fn rules_with_parentheses_or_not_of_their_own_read_as_rules() {
        let nin = cond("name", Kind::Text, Op::NotInOrNull, &["a"], MY);
        let dist = cond("id", Kind::Int, Op::Distinct, &["5"], MY);
        let month = cond("created_at", Kind::DateTime, Op::ThisMonth, &[], MY);
        for t in [tree(Match::All, vec![nin.clone()]), tree(Match::All, vec![a(MY), dist.clone()]), tree(Match::Any, vec![month.clone(), nin.clone()]), tree(Match::All, vec![dist])] {
            let sql = to_sql(&t, MY, true);
            assert_eq!(parse(&sql, &cols(), MY, true), Some(t), "{sql}");
        }
        // A group of one such rule keeps its own parentheses. With one child its word reads back as all.
        let t = tree(Match::All, vec![a(MY), group(Match::All, vec![nin])]);
        let sql = to_sql(&t, MY, true);
        assert_eq!(parse(&sql, &cols(), MY, true), Some(t), "{sql}");
    }

    #[test]
    fn edits_keep_the_tree_whole() {
        let mut t = tree(Match::All, vec![a(MY), group(Match::Any, vec![b(MY), c(MY)])]);
        let ids = t.rows_in_order();
        let (ra, g, rb, rc) = (ids[0], ids[1], ids[2], ids[3]);
        assert_eq!((t.parent_of(ra), t.parent_of(rb)), (Some(0), Some(g)));
        // push and insert_after.
        let rd = t.push(g, a(MY)).unwrap();
        assert_eq!(t.parent_of(rd), Some(g));
        let re = t.insert_after(ra, c(MY)).unwrap();
        assert_eq!(t.rows_in_order(), vec![ra, re, g, rb, rc, rd]);
        // A group moves into neither itself nor a group inside it.
        let inner = t.wrap(rb, Match::None).unwrap();
        assert!(t.move_to(g, g, 0).is_none() && t.move_to(g, inner, 0).is_none());
        // Into a row: refused, and the row being moved stays.
        assert!(t.move_to(ra, rc, 0).is_none() && t.find(ra).is_some());
        // Across groups, and down in the same group counting the list before the move.
        t.move_to(ra, g, 1).unwrap();
        assert_eq!(t.parent_of(ra), Some(g));
        t.move_to(re, 0, 2).unwrap();
        assert_eq!(t.rows_in_order().last(), Some(&re));
        assert_eq!(t.parent_of(re), Some(0));
        // unwrap: the row goes right after its group; the emptied group is pruned.
        t.unwrap(rb).unwrap();
        assert!(t.find(inner).is_none());
        assert_eq!(t.parent_of(rb), Some(g));
        t.unwrap(rb).unwrap();
        assert_eq!(t.parent_of(rb), Some(0));
        assert!(t.unwrap(rb).is_none());
        // ungroup splices the children in place.
        t.ungroup(g).unwrap();
        assert!(t.find(g).is_none() && t.parent_of(ra) == Some(0));
        // move_by, duplicate, toggle, remove.
        let before = t.rows_in_order();
        t.move_by(before[0], 1).unwrap();
        assert_eq!(t.rows_in_order()[1], before[0]);
        assert!(t.move_by(before[0], 10).is_none());
        let dup = t.duplicate(rb).unwrap();
        assert_eq!(t.find(dup).unwrap().item, t.find(rb).unwrap().item);
        t.toggle(dup).unwrap();
        assert!(matches!(&t.find(dup).unwrap().item, Item::Cond(c) if !c.on));
        t.remove(dup).unwrap();
        assert!(t.find(dup).is_none());
        // Removing a group's last row removes the group.
        let solo = t.wrap(rb, Match::Any).unwrap();
        t.remove(rb).unwrap();
        assert!(t.find(solo).is_none());
    }

    #[test]
    fn negate_flips_the_operator_or_wraps() {
        let mut t = tree(Match::All, vec![a(MY), cond("created_at", Kind::DateTime, Op::Today, &[], MY), group(Match::Any, vec![b(MY)])]);
        let ids = t.rows_in_order();
        t.negate(ids[0], MY).unwrap();
        assert_eq!(to_sql(&t, MY, true), "`total` <= 50 AND DATE(`created_at`) = CURDATE() AND (`status` = 'paid')");
        t.negate(ids[1], MY).unwrap();
        t.negate(ids[2], MY).unwrap();
        assert_eq!(to_sql(&t, MY, true), "`total` <= 50 AND NOT (DATE(`created_at`) = CURDATE()) AND NOT (`status` = 'paid')");
        // IN turns into NOT IN where the column offers it, and into a none group where it does not.
        let mut t = tree(Match::All, vec![cond("status", Kind::Enum, Op::In, &["a"], MY), cond("id", Kind::Int, Op::In, &["1"], MY)]);
        let ids = t.rows_in_order();
        t.negate(ids[0], MY).unwrap();
        t.negate(ids[1], MY).unwrap();
        assert_eq!(to_sql(&t, MY, true), "`status` NOT IN ('a') AND NOT (`id` IN (1))");
    }

    #[test]
    fn a_saved_tree_comes_back_while_it_writes_the_same_text() {
        let mut t = tree(Match::All, vec![a(MY), group(Match::Any, vec![b(MY), c(MY)])]);
        let off = t.rows_in_order()[3];
        t.toggle(off).unwrap();
        let json = to_json(&t);
        let text = to_sql(&t, MY, true);
        // The off row survives only through the saved tree.
        assert_eq!(reconcile(&json, &text, &cols(), MY, true), Some(t.clone()));
        assert_ne!(parse(&text, &cols(), MY, true), Some(t.clone()));
        // Text edited since is read afresh, and a saved tree naming a dropped column is not used.
        let edited = "`total` > 60";
        assert_eq!(to_sql(&reconcile(&json, edited, &cols(), MY, true).unwrap(), MY, true), edited);
        let fewer: Vec<_> = cols().into_iter().filter(|(n, _)| n != "status").collect();
        assert!(from_json(&json, &fewer, MY).is_none());
        // Kinds come from the columns, not the save.
        let back = from_json(&json, &cols(), MY).unwrap();
        assert!(matches!(&back.find(back.rows_in_order()[0]).unwrap().item, Item::Cond(c) if c.rule.kind == Kind::Decimal));
    }

    #[test]
    fn spans_name_the_text_each_row_wrote() {
        let t = tree(Match::All, vec![a(MY), group(Match::Any, vec![b(MY), c(MY)])]);
        let (sql, spans) = spans(&t, MY, true);
        let ids = t.rows_in_order();
        let at = |id: u32| spans.iter().find(|(i, _)| *i == id).map(|(_, r)| &sql[r.clone()]).unwrap();
        assert_eq!(at(ids[0]), "`total` > 50");
        assert_eq!(at(ids[1]), "(`status` = 'paid' OR `id` < 9)");
        assert_eq!(at(ids[3]), "`id` < 9");
    }
}
