//! Links a plan's nodes to their parents and children and works out what each one costs and takes on its own. The
//! engines print totals that include the nodes below (PostgreSQL, MySQL's tree text) or a running total of the join
//! (MySQL's JSON), so a node's own share has to be taken apart here.

use super::findings::below;
use super::{Dialect, Op, Plan, PlanNode};
use std::collections::HashSet;

/// Sets `parent` and `kids` from the depths.
pub(super) fn link(p: &mut Plan) {
    let mut stack: Vec<usize> = Vec::new();
    for i in 0..p.nodes.len() {
        let d = p.nodes[i].depth;
        while stack.last().is_some_and(|&j| p.nodes[j].depth >= d) {
            stack.pop();
        }
        if let Some(&j) = stack.last() {
            p.nodes[i].parent = Some(j);
            p.nodes[j].kids.push(i);
        }
        stack.push(i);
    }
}

/// Fills `sub`, `self_cost`, `self_ms` and the shares on every node.
pub(super) fn derive(p: &mut Plan, d: Dialect) {
    let n = p.nodes.len();
    if n == 0 {
        return;
    }
    // Before the costs, since a subquery run per row is charged per call.
    let subs: Vec<Option<Sub>> = (0..n).map(|k| classify(p, k)).collect();
    for (x, s) in p.nodes.iter_mut().zip(subs) {
        x.sub = s;
    }
    let json_my = d != Dialect::Postgres && p.nodes.iter().any(|x| x.own_cost.is_some() || x.table_ms.is_some());
    // Cost.
    if json_my {
        for x in &mut p.nodes {
            x.self_cost = x.own_cost;
        }
    } else {
        let loops = est_loops(p);
        let mut eff = vec![0.0; n];
        for root in (0..n).filter(|&i| p.nodes[i].parent.is_none()) {
            spread(p, root, 1.0, &loops, &mut eff);
        }
        for i in 0..n {
            if p.nodes[i].cost.is_none() {
                continue;
            }
            let below: f64 = p.nodes[i].kids.iter().map(|&k| eff[k]).sum();
            p.nodes[i].self_cost = Some((eff[i] - below).max(0.0));
        }
    }
    // Time.
    if p.analyzed {
        if json_my {
            for x in &mut p.nodes {
                x.self_ms = x.table_ms;
            }
        } else {
            let wall: Vec<f64> = (0..n).map(|i| wall_ms(p, i)).collect();
            for i in 0..n {
                if p.nodes[i].actual_ms.is_none() || p.nodes[i].never_ran {
                    continue;
                }
                let below: f64 = p.nodes[i].kids.iter().map(|&k| wall[k]).sum();
                p.nodes[i].self_ms = Some((wall[i] - below).max(0.0));
            }
        }
    }
    // Share: of the time when the statement ran and the times are known, else of the cost.
    let total_ms: f64 = p.nodes.iter().filter_map(|x| x.self_ms).sum();
    let total_cost: f64 = p.nodes.iter().filter_map(|x| x.self_cost).sum();
    let by_time = p.analyzed && total_ms > 0.0;
    for x in &mut p.nodes {
        x.cost_share = x.self_cost.filter(|_| total_cost > 0.0).map(|c| c / total_cost);
        x.time_share = x.self_ms.filter(|_| by_time).map(|m| m / total_ms);
        x.share = if by_time { x.time_share } else { x.cost_share };
    }
}

/// How many times the planner expects each node to run: the inner side of a nested loop once per row of the outer side.
fn est_loops(p: &Plan) -> Vec<f64> {
    let mut loops = vec![1.0; p.nodes.len()];
    for i in 0..p.nodes.len() {
        let Some(j) = p.nodes[i].parent else { continue };
        let parent = &p.nodes[j];
        let mut l = loops[j];
        if parent.op == Op::NestedLoop && is_inner(p, j, i) {
            let outer = parent.kids.iter().find(|&&k| !is_inner(p, j, k)).and_then(|&k| p.nodes[k].rows).unwrap_or(1.0);
            l *= outer.max(1.0);
        }
        loops[i] = l;
    }
    loops
}

/// Whether `k` is the inner side of the join `j`: PostgreSQL names it, MySQL's tree puts the outer side first.
pub(super) fn is_inner(p: &Plan, j: usize, k: usize) -> bool {
    match p.nodes[k].role.as_str() {
        "Inner" => true,
        "" => p.nodes[j].kids.first() != Some(&k),
        _ => false,
    }
}

/// The cost each node carries in the whole plan. A parent that stops early (Limit, a semi join) costs less than the
/// totals below it, so the nodes below are scaled to fit inside it and the parts add up to the plan's cost. A subquery
/// run once per row is charged per call inside its parent, so it takes its share out of what the parent has left.
fn spread(p: &Plan, i: usize, scale: f64, loops: &[f64], eff: &mut [f64]) {
    let cost = |k: usize| p.nodes[k].cost.unwrap_or(0.0) * loops[k];
    let own = cost(i) * scale;
    eff[i] = own;
    let kids = &p.nodes[i].kids;
    let per: Vec<Option<f64>> = kids.iter().map(|&k| per_row_charge(p, k).map(|c| c * loops[k] * scale)).collect();
    let raw: f64 = kids.iter().zip(&per).filter(|(_, c)| c.is_none()).map(|(&k, _)| cost(k) * scale).sum();
    let fit = if raw > own && raw > 0.0 { own / raw } else { 1.0 };
    let mut room = (own - raw * fit).max(0.0);
    for (&k, c) in kids.iter().zip(per) {
        let target = match c {
            None => cost(k) * scale * fit,
            Some(c) => {
                let t = c.min(room);
                room -= t;
                t
            }
        };
        spread(p, k, if cost(k) > 0.0 { target / cost(k) } else { 0.0 }, loops, eff);
    }
}

/// How a PostgreSQL subquery runs for the node that tests it, read from how that node prints it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SubRun {
    /// An InitPlan, a CTE or a hashed SubPlan: run once and kept. Also a SubPlan the plan shows no sign of running per
    /// row, which is then charged nothing per row.
    Once,
    /// A SubPlan run again for every row the node above tests. `EXISTS(SubPlan 1)` stops at its first row, `ANY` reads
    /// half of it on average and anything else, a scalar subquery, reads all of it.
    Exists,
    Any,
    Expr,
}

/// Where the node above uses a subquery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SubAt {
    JoinFilter,
    /// Any other condition or key the node prints.
    Cond,
    /// Nothing the node prints names it, so it is in the columns the node returns, which only VERBOSE prints.
    Output,
}

impl SubAt {
    /// Where the subquery is, for a finding.
    pub(super) fn place(self) -> &'static str {
        match self {
            SubAt::JoinFilter => "in the join filter",
            SubAt::Cond => "in its condition",
            SubAt::Output => "in the columns it returns (shown only with VERBOSE)",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Sub {
    pub(super) run: SubRun,
    pub(super) at: SubAt,
}

/// How node `k` runs, when it is a subquery.
pub(super) fn sub_run(p: &Plan, k: usize) -> Option<SubRun> {
    p.nodes[k].sub.map(|s| s.run)
}

/// How node `k` runs and where its parent uses it, when it is a subquery.
fn classify(p: &Plan, k: usize) -> Option<Sub> {
    let n = &p.nodes[k];
    let sub = n.role.starts_with("SubPlan");
    if !sub && !n.role.starts_with("InitPlan") && !n.role.starts_with("CTE") {
        return None;
    }
    let parent = n.parent.map(|j| &p.nodes[j]);
    let at = match parent {
        Some(x) if mentions(&x.join_filter.text, &n.role, "") => SubAt::JoinFilter,
        Some(x) if x.detail.iter().any(|d| mentions(d, &n.role, "")) => SubAt::Cond,
        _ => SubAt::Output,
    };
    let run = match parent {
        Some(x) if sub => sub_plan_run(p, k, x),
        _ => SubRun::Once,
    };
    Some(Sub { run, at })
}

/// How the SubPlan `k` runs for its `parent`.
fn sub_plan_run(p: &Plan, k: usize, parent: &PlanNode) -> SubRun {
    let n = &p.nodes[k];
    let text = parent.detail.join("\n");
    let named = |pre: &str, post: &str| mentions(&text, &format!("{pre}{}", n.role), post);
    // PostgreSQL 16 and older may print "alternatives: SubPlan 1 or hashed SubPlan 2" and pick one as it runs.
    if named("hashed ", "") || text.contains("alternatives:") {
        SubRun::Once
    } else if named("EXISTS(", ")") {
        SubRun::Exists
    } else if named("(", ").col") {
        SubRun::Any
    } else if named("(", ")") && !text.contains("EXISTS") && !text.contains("ANY") && !text.contains("ALL") {
        SubRun::Expr
    } else if n.op == Op::Materialize {
        // An IN or ANY that PostgreSQL could not hash keeps its rows in a Materialize to search them for every row.
        SubRun::Any
    } else if mentions(&text, &n.role, "") || correlated(p, k) {
        // Older servers print every kind as "(SubPlan 1)"; the cheapest reading keeps the charge a lower bound. A
        // SubPlan the node does not name sits in its output list, printed only with VERBOSE, and one that reads a
        // column from outside can only run per row, since PostgreSQL never hashes a correlated subquery.
        SubRun::Exists
    } else {
        // Unnamed and uncorrelated, as a hashed subquery in the output list prints without VERBOSE. Calling it per row
        // would be a guess, so it counts once and ANALYZE's loops have the last word.
        SubRun::Once
    }
}

/// Whether the subquery at `k` reads a column of a table outside it. The server qualifies those with their alias and
/// leaves the subquery's own columns bare or qualified with aliases from inside it. Every condition and key a node
/// prints counts: a column from outside in a One-Time Filter or a Sort Key correlates it as much as one in its filter.
/// A Limit's count and a function's arguments are not printed without VERBOSE, so a correlation there goes unseen.
fn correlated(p: &Plan, k: usize) -> bool {
    let inside: HashSet<usize> = std::iter::once(k).chain(below(p, k)).collect();
    let own: HashSet<&str> = inside.iter().map(|&j| p.nodes[j].alias.as_str()).collect();
    let outer: HashSet<&str> = (0..p.nodes.len()).filter(|j| !inside.contains(j)).map(|j| p.nodes[j].alias.as_str()).filter(|a| !a.is_empty() && !own.contains(a)).collect();
    inside.iter().any(|&j| {
        let x = &p.nodes[j];
        [&x.filter.text, &x.index_cond.text, &x.join_cond.text, &x.join_filter.text, &x.one_time.text, &x.sort_key, &x.group_key].into_iter().any(|t| quals(t).iter().any(|q| outer.contains(q)))
    })
}

/// The names in front of a dot in `text`, past string literals: "c" and "si" out of "(c.total - si.cap)". Read from the
/// text rather than the parsed condition, so a column inside arithmetic or a key counts too.
fn quals(text: &str) -> Vec<&str> {
    let b = text.as_bytes();
    // Bytes of a multibyte character count as letters, so every cut falls on an ASCII byte.
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80;
    let starts = |c: u8| c == b'"' || c.is_ascii_alphabetic() || c == b'_' || c >= 0x80;
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'\'' {
            i += 1 + text[i + 1..].find('\'').map_or(b.len(), |e| e + 1);
        } else if starts(c) {
            let (from, to, next) = if c == b'"' {
                let to = text[i + 1..].find('"').map_or(b.len(), |e| i + 1 + e);
                (i + 1, to, to + 1)
            } else {
                let to = i + b[i..].iter().take_while(|&&c| word(c)).count();
                (i, to, to)
            };
            if b.get(next) == Some(&b'.') && b.get(next + 1).is_some_and(|&c| starts(c)) {
                out.push(&text[from..to]);
            }
            i = next;
        } else if c.is_ascii_digit() {
            // A number, decimals and all, is no name.
            i += b[i..].iter().take_while(|&&c| c.is_ascii_alphanumeric() || c == b'.' || c == b'_').count();
        } else {
            i += 1;
        }
    }
    out
}

/// Whether `text` holds `name` then `post`, with no other digit right after the name, so "SubPlan 1" does not match
/// "SubPlan 12".
pub(super) fn mentions(text: &str, name: &str, post: &str) -> bool {
    text.match_indices(name).any(|(at, m)| {
        let rest = &text[at + m.len()..];
        !rest.starts_with(|c: char| c.is_ascii_digit()) && rest.starts_with(post)
    })
}

/// The hashed SubPlan the node `i` tests in place of a per-row one PostgreSQL dropped. The planner may plan a subquery
/// both ways, prices its parent with the first (per row) and only then picks the hash, so the parent's cost still holds
/// a per-row subquery that never runs. The dropped one is the SubPlan numbered just before the hashed one. The claim
/// rests on that missing number and on nothing in the plan saying the SubPlan runs per row.
pub(super) fn dropped_alternative(p: &Plan, i: usize) -> Option<usize> {
    // InitPlans and CTEs take numbers from the same count, so with any of them the gap proves nothing.
    if p.nodes.iter().any(|x| x.role.starts_with("InitPlan") || x.role.starts_with("CTE") || x.label == "CTE Scan") {
        return None;
    }
    let n = &p.nodes[i];
    n.kids.iter().copied().find(|&k| {
        let x = &p.nodes[k];
        let Some(num) = x.role.strip_prefix("SubPlan ").and_then(|s| s.parse::<u32>().ok()) else { return false };
        let before = format!("SubPlan {}", num.saturating_sub(1));
        num >= 2 && x.sub.is_some_and(|s| s.run == SubRun::Once) && !p.nodes.iter().any(|x| x.role == before) && !n.detail.iter().any(|d| mentions(d, &before, ""))
    })
}

/// What the planner charges a per-row SubPlan inside its parent for one run of the parent: its cost per call, worked out
/// as PostgreSQL's cost_subplan does, times the rows the parent returns. Every row the parent returns passed the test,
/// so that is the fewest calls there can have been. A join tests every pair its condition matched, which an estimate
/// does not give, so what the subquery cost beyond that stays with the parent.
fn per_row_charge(p: &Plan, k: usize) -> Option<f64> {
    let n = &p.nodes[k];
    let run = n.sub?.run;
    if run == SubRun::Once {
        return None;
    }
    let (start, total, rows) = (n.startup_cost.unwrap_or(0.0), n.cost.unwrap_or(0.0), n.rows.unwrap_or(1.0).max(1.0));
    let body = (total - start).max(0.0);
    // A scalar subquery reads all its rows, but it nearly always returns one, and servers before 17 print EXISTS the
    // same way, so the first-row reading of EXISTS keeps the charge a lower bound for both.
    let call = start
        + match run {
            // Half the rows, and an operator per row looked at.
            SubRun::Any => 0.5 * body + 0.5 * rows * 0.0025,
            _ => body / rows,
        };
    Some(call * p.nodes[n.parent?].rows.unwrap_or(1.0).max(1.0))
}

/// How many times a per-row SubPlan ran, or at least runs: its loops after ANALYZE, else the rows its parent returns.
/// After ANALYZE the loops decide whatever the text says. A hashed SubPlan is built once in each process that runs its
/// parent, so loops beyond those processes are calls per row, even when they match the parent's own loops, as under
/// the inner side of a nested loop that tests one row a run.
pub(super) fn sub_calls(p: &Plan, k: usize) -> Option<(f64, bool)> {
    let n = &p.nodes[k];
    let run = n.sub.filter(|_| n.role.starts_with("SubPlan"))?.run;
    let parent = &p.nodes[n.parent?];
    match n.loops {
        Some(l) if p.analyzed => (l > parallel_procs(p, k).unwrap_or(1.0)).then_some((l, true)),
        _ if run == SubRun::Once => None,
        _ => Some((parent.rows.unwrap_or(1.0).max(1.0), false)),
    }
}

/// A node's time over all its loops. Nodes run by parallel workers report an average per process, which is already
/// close to the time they took side by side, so their loops are divided back out.
fn wall_ms(p: &Plan, i: usize) -> f64 {
    let x = &p.nodes[i];
    if x.never_ran {
        return 0.0;
    }
    let ms = x.actual_ms.unwrap_or(0.0) * x.loops.unwrap_or(1.0);
    match parallel_procs(p, i) {
        Some(procs) => ms / procs,
        None => ms,
    }
}

/// The Gather above node `i`, when there is one.
pub(super) fn gather_of(p: &Plan, i: usize) -> Option<usize> {
    let mut j = p.nodes[i].parent?;
    while p.nodes[j].op != Op::Gather {
        j = p.nodes[j].parent?;
    }
    Some(j)
}

/// The processes a Gather runs its plan in: its workers and the leader.
pub(super) fn gather_procs(g: &PlanNode) -> f64 {
    g.workers.unwrap_or(0.0) + 1.0
}

/// The processes sharing node `i`'s work: those of the Gather above it, whether the node splits its rows between them
/// (Parallel Aware) or each one runs all of it. Their times are an average per process and their rows and buffers a sum.
pub(super) fn parallel_procs(p: &Plan, i: usize) -> Option<f64> {
    gather_of(p, i).map(|g| gather_procs(&p.nodes[g]))
}

/// Each node's cost, or time, with the nodes below it, built up from the own values `derive` worked out. For PostgreSQL
/// and MySQL's tree text that is the loop-scaled cost `spread` gives; MySQL's JSON has only a running total, so its own
/// costs are summed. Building up from own values keeps every parent at least as heavy as its children, which the
/// times alone do not (parallel workers, loops).
pub(super) fn inclusive(p: &Plan, time: bool) -> Vec<f64> {
    let mut w = vec![0.0; p.nodes.len()];
    // Children follow their parent, so walking back sees every child first.
    for i in (0..p.nodes.len()).rev() {
        let x = &p.nodes[i];
        let own = if time { x.self_ms } else { x.self_cost }.unwrap_or(0.0).max(0.0);
        w[i] = own + x.kids.iter().map(|&k| w[k]).sum::<f64>();
    }
    w
}
