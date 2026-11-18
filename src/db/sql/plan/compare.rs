//! What changed between two plans of the same statement: Explain again after creating an index and see which nodes
//! got cheaper, which table now reads through an index and which findings went away. Nodes are matched by the table
//! they read, else by their type and the tables beneath them, else by their place among nodes of the same type.

use super::findings::Kind;
use super::{count, lex, Access, Dialect, Plan};

/// A change under this share is "same": plans of an unchanged statement still move a little with the statistics.
pub const SAME: f64 = 0.01;
/// Plans kept for comparing, the oldest going first.
pub const KEEP: usize = 50;

/// The statement as a key: comments and spacing dropped and words in capitals, literals and quoted names kept as written.
pub fn normalize(stmt: &str, d: Dialect) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (t, r) in lex::tokens(stmt, d) {
        let s = &stmt[r];
        match t {
            lex::Tok::Space | lex::Tok::Comment => {}
            lex::Tok::Word => parts.push(s.to_uppercase()),
            _ => parts.push(s.to_string()),
        }
    }
    while parts.last().is_some_and(|p| p == ";") {
        parts.pop();
    }
    parts.join(" ")
}

/// The last plan of each statement explained this session, by connection, schema, normalized statement and whether
/// it ran under ANALYZE (an estimate compared with a run would mostly show the difference between the two).
#[derive(Default)]
pub struct History {
    kept: Vec<(Key, Plan, u64)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    pub conn: u64,
    pub schema: String,
    pub stmt: String,
    pub analyzed: bool,
}

impl History {
    /// Keeps `plan`, made at `at` (Unix seconds), as the last of `key`, and returns the one it replaces.
    pub fn swap(&mut self, key: Key, plan: Plan, at: u64) -> Option<(Plan, u64)> {
        let old = self.kept.iter().position(|(k, ..)| *k == key).map(|i| self.kept.remove(i));
        self.kept.push((key, plan, at));
        if self.kept.len() > KEEP {
            self.kept.remove(0);
        }
        old.map(|(_, p, at)| (p, at))
    }

    /// Forgets the plans of a connection that closed.
    pub fn forget(&mut self, conn: u64) {
        self.kept.retain(|(k, ..)| k.conn != conn);
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.kept.len()
    }
}

/// A number before and after.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Delta {
    pub before: Option<f64>,
    pub after: Option<f64>,
}

impl Delta {
    /// The change as a share of before: -0.81 when it went down 81%.
    pub fn change(&self) -> Option<f64> {
        let (b, a) = (self.before?, self.after?);
        if b <= 0.0 {
            return (a <= 0.0).then_some(0.0);
        }
        Some((a - b) / b)
    }

    pub fn same(&self) -> bool {
        self.change().is_some_and(|c| c.abs() < SAME)
    }

    /// "-81%", "+12%", "same", "new" or "gone".
    pub fn text(&self) -> String {
        match (self.before, self.after) {
            (None, Some(_)) => "new".into(),
            (Some(_), None) => "gone".into(),
            _ if self.same() => "same".into(),
            _ => match self.change() {
                Some(c) => format!("{}{}%", if c > 0.0 { "+" } else { "-" }, count((c * 100.0).abs().max(1.0))),
                None => String::new(),
            },
        }
    }
}

/// One node of either plan, matched to its counterpart when it has one.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub before: Option<usize>,
    pub after: Option<usize>,
    pub depth: usize,
    /// The node's type in the old plan, when it changed: "Seq Scan".
    pub was: Option<String>,
    /// With the nodes below it.
    pub cost: Delta,
    /// Only when both plans ran.
    pub time: Option<Delta>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Resolved,
    New,
    Same,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FindingChange {
    pub state: State,
    pub title: String,
    pub body: String,
    /// The node it is about in the new plan, or in the old one when it was resolved.
    pub node: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Diff {
    /// The new plan's nodes in order, with nodes that went away where they were.
    pub rows: Vec<Row>,
    pub cost: Delta,
    pub time: Option<Delta>,
    /// "Cost down 81%. bookings b now reads through bookings_provider_status_idx."
    pub summary: String,
    /// Resolved first, then new, then those still there.
    pub findings: Vec<FindingChange>,
    /// The plans differ in shape or by at least `SAME` of their cost. Times alone vary from run to run.
    pub differs: bool,
}

impl Diff {
    /// Whether the new plan is cheaper (or faster, when both ran), dearer, or about the same.
    pub fn better(&self) -> Option<bool> {
        let d = self.time.filter(|t| t.change().is_some()).unwrap_or(self.cost);
        if d.same() {
            return None;
        }
        d.change().map(|c| c < 0.0)
    }
}

/// What changed from `before` to `after`.
pub fn diff(before: &Plan, after: &Plan) -> Diff {
    let m = matches(before, after);
    let (cb, ca) = (super::tree::inclusive(before, false), super::tree::inclusive(after, false));
    let timed = before.analyzed && after.analyzed;
    let (tb, ta) = if timed { (super::tree::inclusive(before, true), super::tree::inclusive(after, true)) } else { (Vec::new(), Vec::new()) };
    let (cb, ca, tb, ta) = (cb.as_slice(), ca.as_slice(), tb.as_slice(), ta.as_slice());
    let delta = |b: Option<usize>, a: Option<usize>, vb: &[f64], va: &[f64]| Delta { before: b.map(|i| vb[i]), after: a.map(|i| va[i]) };
    let mut rows: Vec<Row> = after
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let b = m[i];
            let was = b.map(|j| &before.nodes[j].label).filter(|l| **l != n.label).cloned();
            Row { before: b, after: Some(i), depth: n.depth, was, cost: delta(b, Some(i), cb, ca), time: timed.then(|| delta(b, Some(i), tb, ta)) }
        })
        .collect();
    // Nodes that went away go back where they were: after the row of the old node just before them.
    for j in (0..before.nodes.len()).filter(|j| !m.contains(&Some(*j))) {
        let at = rows.iter().rposition(|r| r.before.is_some_and(|b| b < j)).map_or(0, |p| p + 1);
        rows.insert(at, Row { before: Some(j), after: None, depth: before.nodes[j].depth, was: None, cost: delta(Some(j), None, cb, ca), time: timed.then(|| delta(Some(j), None, tb, ta)) });
    }
    let root = |p: &Plan, v: &[f64]| (0..p.nodes.len()).filter(|&i| p.nodes[i].parent.is_none()).map(|i| v[i]).sum::<f64>();
    let cost = Delta { before: Some(root(before, cb)), after: Some(root(after, ca)) };
    let time = timed.then(|| match (before.exec_ms, after.exec_ms) {
        (Some(b), Some(a)) => Delta { before: Some(b), after: Some(a) },
        _ => Delta { before: Some(root(before, tb)), after: Some(root(after, ta)) },
    });
    let differs = before.nodes.len() != after.nodes.len() || rows.iter().any(|r| r.before.is_none() || r.after.is_none() || r.was.is_some()) || !cost.same();
    let summary = summary(before, after, &rows, cost, time);
    let findings = finding_changes(before, after, &rows, cb, ca);
    Diff { rows, cost, time, summary, findings, differs }
}

/// For each node of `after`, the node of `before` it is.
fn matches(before: &Plan, after: &Plan) -> Vec<Option<usize>> {
    let mut m: Vec<Option<usize>> = vec![None; after.nodes.len()];
    let mut used = vec![false; before.nodes.len()];
    let table = |p: &Plan, i: usize| {
        let n = &p.nodes[i];
        (!n.rel.is_empty() || !n.alias.is_empty()).then(|| (n.rel.clone(), n.alias.clone()))
    };
    // Each pass pairs the nth node of a key in one plan with the nth of the same key in the other, in plan order.
    let mut pass = |m: &mut Vec<Option<usize>>, key: &dyn Fn(&Plan, usize) -> Option<String>| {
        for i in 0..after.nodes.len() {
            if m[i].is_some() {
                continue;
            }
            let Some(k) = key(after, i) else { continue };
            if let Some(j) = (0..before.nodes.len()).find(|&j| !used[j] && key(before, j).as_ref() == Some(&k)) {
                used[j] = true;
                m[i] = Some(j);
            }
        }
    };
    // A table is the same table whatever reads it, so a scan that became an index scan stays one row.
    pass(&mut m, &|p, i| table(p, i).map(|(r, a)| format!("{r}\u{0}{a}")));
    // Joins, sorts and the like by their type and the tables beneath them.
    pass(&mut m, &|p, i| {
        if table(p, i).is_some() {
            return None;
        }
        let mut below: Vec<String> = super::findings::below(p, i).filter_map(|k| table(p, k)).map(|(r, a)| format!("{r} {a}")).collect();
        below.sort();
        Some(format!("{}\u{0}{}", p.nodes[i].label, below.join(",")))
    });
    // What is left, by type alone.
    pass(&mut m, &|p, i| table(p, i).is_none().then(|| p.nodes[i].label.clone()));
    m
}

/// The index a node reads through: its own, or one a node below it searches (a Bitmap Heap Scan's Bitmap Index Scan).
fn index_of(p: &Plan, i: usize) -> Option<String> {
    let n = &p.nodes[i];
    if !n.index.is_empty() {
        return Some(n.index.clone());
    }
    super::findings::below(p, i).map(|k| &p.nodes[k]).find(|k| !k.index.is_empty() && k.rel.is_empty() && k.alias.is_empty()).map(|k| k.index.clone())
}

fn summary(before: &Plan, after: &Plan, rows: &[Row], cost: Delta, time: Option<Delta>) -> String {
    let word = |what: &str, d: Delta| match d.change() {
        _ if d.same() => format!("{what} about the same."),
        Some(c) => format!("{what} {} {}%.", if c < 0.0 { "down" } else { "up" }, count((c * 100.0).abs().max(1.0))),
        None => String::new(),
    };
    let mut parts = vec![word("Cost", cost)];
    if let Some(t) = time {
        parts.push(word("Time", t));
    }
    for r in rows {
        let (Some(b), Some(a)) = (r.before, r.after) else { continue };
        let (nb, na) = (&before.nodes[b], &after.nodes[a]);
        if na.rel.is_empty() && na.alias.is_empty() {
            continue;
        }
        if nb.access == Access::Scan && na.access != Access::Scan {
            if let Some(ix) = index_of(after, a) {
                parts.push(format!("{} now reads through {ix}.", na.name()));
            }
        } else if nb.access != Access::Scan && na.access == Access::Scan {
            parts.push(format!("{} is now read in full.", na.name()));
        }
    }
    parts.retain(|p| !p.is_empty());
    parts.join("  ")
}

/// A finding's key: what it is and the table (or node) it is about.
fn key(p: &Plan, f: &super::Finding) -> (Kind, String) {
    (f.kind, f.node.map(|i| p.nodes[i].name()).filter(|n| !n.is_empty()).unwrap_or_else(|| f.title.clone()))
}

fn finding_changes(before: &Plan, after: &Plan, rows: &[Row], cb: &[f64], ca: &[f64]) -> Vec<FindingChange> {
    let kb: Vec<_> = before.findings.iter().map(|f| key(before, f)).collect();
    let ka: Vec<_> = after.findings.iter().map(|f| key(after, f)).collect();
    let mut out = Vec::new();
    for (f, k) in before.findings.iter().zip(&kb) {
        if ka.contains(k) {
            continue;
        }
        // What became of its node, when the node is still there.
        let row = f.node.and_then(|i| rows.iter().find(|r| r.before == Some(i)));
        let via = |a: usize| (before.nodes[f.node.unwrap_or(0)].access == Access::Scan && after.nodes[a].access != Access::Scan).then(|| index_of(after, a)).flatten();
        let body = match row.and_then(|r| r.after) {
            Some(a) => match via(a) {
                Some(ix) => format!("Now an index scan through {ix}, {} instead of {}.", count(ca[a]), count(cb[f.node.unwrap_or(0)])),
                None => "No longer reported.".into(),
            },
            None => "The node is gone from the new plan.".into(),
        };
        out.push(FindingChange { state: State::Resolved, title: f.title.clone(), body, node: f.node });
    }
    for (f, k) in after.findings.iter().zip(&ka) {
        let state = if kb.contains(k) { State::Same } else { State::New };
        out.push(FindingChange { state, title: f.title.clone(), body: f.body.join(" "), node: f.node });
    }
    out.sort_by_key(|c| match c.state {
        State::Resolved => 0,
        State::New => 1,
        State::Same => 2,
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sql::plan::parse;

    const BEFORE: &str = include_str!("fixtures/pg18-fix-before.json");
    const AFTER: &str = include_str!("fixtures/pg18-fix-after.json");
    const BEFORE_RUN: &str = include_str!("fixtures/pg18-fix-before-analyze.json");
    const AFTER_RUN: &str = include_str!("fixtures/pg18-fix-after-analyze.json");

    fn pg(text: &str) -> Plan {
        parse(Dialect::Postgres, text).unwrap()
    }

    #[test]
    fn normalize_drops_case_spacing_and_comments_and_keeps_literals() {
        let d = Dialect::Postgres;
        let a = normalize("select id from bookings where status = 'DONE';", d);
        assert_eq!(a, "SELECT ID FROM BOOKINGS WHERE STATUS = 'DONE'");
        assert_eq!(normalize("SELECT  id\n  FROM bookings -- the hot table\n WHERE status='DONE'", d), a);
        assert_eq!(normalize("/* report */ SELECT id FROM bookings WHERE status = 'DONE' ;;", d), a);
        // A different value is a different statement, and so is a quoted name in other case.
        assert_ne!(normalize("SELECT id FROM bookings WHERE status = 'done'", d), a);
        assert_ne!(normalize(r#"SELECT id FROM "Bookings" WHERE status = 'DONE'"#, d), normalize(r#"SELECT id FROM "bookings" WHERE status = 'DONE'"#, d));
        assert_eq!(normalize("SELECT `id` FROM t # note\nWHERE a=1", Dialect::MySql), "SELECT `id` FROM T WHERE A = 1");
    }

    #[test]
    fn the_captured_plan_before_the_index_has_a_full_scan_of_bookings() {
        let p = pg(BEFORE);
        assert!(p.findings.iter().any(|f| f.kind == Kind::FullScan && f.node.is_some_and(|i| p.nodes[i].name() == "bookings b")), "{:?}", p.findings);
        assert!(!pg(AFTER).findings.iter().any(|f| f.kind == Kind::FullScan), "the plan after the index still has a full scan");
    }

    #[test]
    fn a_scan_that_became_an_index_scan_stays_one_row_and_says_what_it_was() {
        let (b, a) = (pg(BEFORE), pg(AFTER));
        let d = diff(&b, &a);
        assert!(d.differs);
        let heap = a.nodes.iter().position(|n| n.label == "Bitmap Heap Scan").unwrap();
        let row = d.rows.iter().find(|r| r.after == Some(heap)).unwrap();
        assert_eq!(row.was.as_deref(), Some("Seq Scan"));
        assert_eq!(b.nodes[row.before.unwrap()].name(), "bookings b");
        assert!(row.cost.change().unwrap() < -0.5, "{:?}", row.cost);
        // The Bitmap Index Scan under it is new; services s, its Hash and the join are the same nodes.
        let bitmap = a.nodes.iter().position(|n| n.label == "Bitmap Index Scan").unwrap();
        let new = d.rows.iter().find(|r| r.after == Some(bitmap)).unwrap();
        assert_eq!(new.before, None);
        assert_eq!(new.cost.text(), "new");
        let s = d.rows.iter().find(|r| r.after.is_some_and(|i| a.nodes[i].alias == "s")).unwrap();
        assert_eq!(s.cost.text(), "same");
        assert!(s.was.is_none());
        for label in ["Sort", "Hash Join (Inner)", "Hash"] {
            let r = d.rows.iter().find(|r| r.after.is_some_and(|i| a.nodes[i].label == label)).unwrap_or_else(|| panic!("no {label} row"));
            assert!(r.before.is_some_and(|j| b.nodes[j].label == label), "{label} was not matched");
        }
        assert!(d.rows.iter().all(|r| r.after.is_some()), "a node of the old plan has no match");
        assert!(d.time.is_none(), "times are compared when neither plan ran");
    }

    #[test]
    fn the_summary_gives_the_cost_change_and_the_index_now_used() {
        let d = diff(&pg(BEFORE), &pg(AFTER));
        assert!(d.summary.starts_with("Cost down 4"), "{}", d.summary);
        assert!(d.summary.contains("bookings b now reads through bookings_provider_status_idx."), "{}", d.summary);
        assert_eq!(d.better(), Some(true));
        // The other way round, the table is read in full again.
        let back = diff(&pg(AFTER), &pg(BEFORE));
        assert!(back.summary.starts_with("Cost up "), "{}", back.summary);
        assert!(back.summary.contains("bookings b is now read in full."), "{}", back.summary);
        assert_eq!(back.rows.iter().filter(|r| r.after.is_none()).count(), 1, "the Bitmap Index Scan is not shown as gone");
        assert_eq!(back.better(), Some(false));
    }

    #[test]
    fn the_full_scan_finding_is_resolved_with_what_replaced_it() {
        let d = diff(&pg(BEFORE), &pg(AFTER));
        let f = d.findings.iter().find(|f| f.title == "Full scan of bookings b").unwrap();
        assert_eq!(f.state, State::Resolved);
        assert!(f.body.starts_with("Now an index scan through bookings_provider_status_idx, "), "{}", f.body);
        assert_eq!(d.findings[0].state, State::Resolved, "resolved findings come first");
        // Backwards, it is new.
        let back = diff(&pg(AFTER), &pg(BEFORE));
        assert!(back.findings.iter().any(|f| f.title == "Full scan of bookings b" && f.state == State::New));
    }

    #[test]
    fn the_same_plan_twice_does_not_differ() {
        for text in [BEFORE, AFTER, BEFORE_RUN] {
            let d = diff(&pg(text), &pg(text));
            assert!(!d.differs);
            assert!(d.rows.iter().all(|r| r.cost.text() == "same" && r.was.is_none()));
            assert!(d.findings.iter().all(|f| f.state == State::Same));
            assert_eq!(d.better(), None);
            assert!(d.summary.starts_with("Cost about the same."), "{}", d.summary);
        }
    }

    #[test]
    fn two_runs_compare_their_times_too() {
        let d = diff(&pg(BEFORE_RUN), &pg(AFTER_RUN));
        let t = d.time.expect("both plans ran");
        assert!(t.before.is_some() && t.after.is_some());
        assert!(d.summary.contains("Time "), "{}", d.summary);
        assert!(d.rows.iter().all(|r| r.time.is_some()));
    }

    #[test]
    fn duplicate_types_pair_in_order_and_unmatched_nodes_are_new_or_gone() {
        // Two Seq Scans and two Hash nodes in the join plan; the CTE plan has two Aggregates.
        for (d, text) in [(Dialect::Postgres, include_str!("fixtures/pg18-cte-analyze.json")), (Dialect::Postgres, include_str!("fixtures/pg18-agg-analyze.json")), (Dialect::MySql, include_str!("fixtures/mysql-join.json"))] {
            let p = parse(d, text).unwrap();
            let m = matches(&p, &p);
            assert_eq!(m, (0..p.nodes.len()).map(Some).collect::<Vec<_>>(), "a plan does not match itself node for node");
        }
        // Two joins with the same shape but the tables on other sides: tables pair by alias wherever they sit, and the
        // Hash nodes, each over a different table, still pair by type.
        let (b, a) = (pg(include_str!("fixtures/pg18-nested-analyze.json")), pg(include_str!("fixtures/pg18-exists-analyze.json")));
        let d = diff(&b, &a);
        for alias in ["b", "s"] {
            let r = d.rows.iter().find(|r| r.after.is_some_and(|i| a.nodes[i].alias == alias)).unwrap_or_else(|| panic!("no row for {alias}"));
            assert!(r.before.is_some_and(|j| b.nodes[j].alias == alias), "{alias} was not paired with itself");
            assert_ne!(r.before, r.after, "{alias} sits at another place in each plan, so its rows differ by index");
        }
        let hash = a.nodes.iter().position(|n| n.label == "Hash").unwrap();
        let r = d.rows.iter().find(|r| r.after == Some(hash)).unwrap();
        assert!(r.before.is_some_and(|j| b.nodes[j].label == "Hash"), "the Hash nodes were not paired");
        // A plan against a different one: every node of each shows exactly once.
        let (b, a) = (pg(include_str!("fixtures/pg18-cte-analyze.json")), pg(include_str!("fixtures/pg18-scan.json")));
        let d = diff(&b, &a);
        let befores: Vec<usize> = d.rows.iter().filter_map(|r| r.before).collect();
        let afters: Vec<usize> = d.rows.iter().filter_map(|r| r.after).collect();
        assert_eq!(befores.len(), b.nodes.len());
        assert_eq!(afters, (0..a.nodes.len()).collect::<Vec<_>>());
        assert!(d.rows.iter().any(|r| r.after.is_none() && r.cost.text() == "gone"));
        assert!(d.differs);
    }

    #[test]
    fn history_keeps_the_last_plan_per_statement_and_forgets_a_closed_connection() {
        let key = |conn: u64, stmt: &str| Key { conn, schema: "public".into(), stmt: stmt.into(), analyzed: false };
        let mut h = History::default();
        assert!(h.swap(key(1, "A"), pg(BEFORE), 10).is_none());
        let (old, at) = h.swap(key(1, "A"), pg(AFTER), 20).expect("the first plan is kept");
        assert_eq!((old.nodes.len(), at), (5, 10));
        assert!(h.swap(key(1, "A"), pg(AFTER), 30).is_some_and(|(_, at)| at == 20));
        // Analyzed plans are kept apart from estimates.
        assert!(h.swap(Key { analyzed: true, ..key(1, "A") }, pg(BEFORE_RUN), 40).is_none());
        for i in 0..KEEP as u64 + 5 {
            h.swap(key(2, &format!("S{i}")), pg(BEFORE), i);
        }
        assert_eq!(h.len(), KEEP);
        h.forget(2);
        assert_eq!(h.len(), 0);
    }
}
