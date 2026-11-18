//! Links a plan's nodes to their parents and children and works out what each one costs and takes on its own. The
//! engines print totals that include the nodes below (PostgreSQL, MySQL's tree text) or a running total of the join
//! (MySQL's JSON), so a node's own share has to be taken apart here.

use super::{Dialect, Plan};

/// Sets `parent` and `kids` from the depths, and gives a MySQL tree-text scan the filter its parent applies to it.
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
    // MySQL's tree prints "Filter: (cond)" over the "Table scan on o" or full index scan it filters; the scan is what the
    // filter slows down.
    for i in 0..p.nodes.len() {
        let Some(j) = p.nodes[i].parent else { continue };
        let l = &p.nodes[i].label;
        if (l.starts_with("Table scan") || super::evidence::full_index_scan(l)) && p.nodes[i].filter.is_empty() && !p.nodes[j].filter.is_empty() && p.nodes[j].kids.len() == 1 {
            p.nodes[i].filter = p.nodes[j].filter.clone();
            if let (Some(read), Some(kept)) = (p.nodes[i].actual_rows, p.nodes[j].actual_rows) {
                p.nodes[i].removed = Some((read - kept).max(0.0));
                p.nodes[i].kept = Some(kept);
            }
        }
    }
}

/// Fills `self_cost`, `self_ms` and `share` on every node.
pub(super) fn derive(p: &mut Plan, d: Dialect) {
    let n = p.nodes.len();
    if n == 0 {
        return;
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
        x.share = if by_time { x.self_ms.map(|m| m / total_ms) } else if total_cost > 0.0 { x.self_cost.map(|c| c / total_cost) } else { None };
    }
}

/// How many times the planner expects each node to run: the inner side of a nested loop once per row of the outer side.
fn est_loops(p: &Plan) -> Vec<f64> {
    let mut loops = vec![1.0; p.nodes.len()];
    for i in 0..p.nodes.len() {
        let Some(j) = p.nodes[i].parent else { continue };
        let parent = &p.nodes[j];
        let mut l = loops[j];
        if is_nested_loop(&parent.label) && is_inner(p, j, i) {
            let outer = parent.kids.iter().find(|&&k| !is_inner(p, j, k)).and_then(|&k| p.nodes[k].rows).unwrap_or(1.0);
            l *= outer.max(1.0);
        }
        loops[i] = l;
    }
    loops
}

/// PostgreSQL's "Nested Loop" and MySQL's "Nested loop inner join".
pub(super) fn is_nested_loop(label: &str) -> bool {
    label.to_ascii_lowercase().starts_with("nested loop")
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
pub enum SubRun {
    /// An InitPlan, a CTE or a hashed SubPlan: run once and kept.
    Once,
    /// A SubPlan run again for every row the node above tests. `EXISTS(SubPlan 1)` stops at its first row, `ANY` reads
    /// half of it on average and anything else, a scalar subquery, reads all of it.
    Exists,
    Any,
    Expr,
}

/// How node `k` runs, when it is a subquery.
pub fn sub_run(p: &Plan, k: usize) -> Option<SubRun> {
    let n = &p.nodes[k];
    if !n.role.starts_with("SubPlan") {
        return (n.role.starts_with("InitPlan") || n.role.starts_with("CTE")).then_some(SubRun::Once);
    }
    let Some(j) = n.parent else { return Some(SubRun::Once) };
    let parent = &p.nodes[j];
    let text = parent.detail.join("\n");
    let named = |pre: &str, post: &str| mentions(&text, &format!("{pre}{}", n.role), post);
    // PostgreSQL 16 and older may print "alternatives: SubPlan 1 or hashed SubPlan 2" and pick one as it runs.
    Some(if named("hashed ", "") || text.contains("alternatives:") {
        SubRun::Once
    } else if named("EXISTS(", ")") {
        SubRun::Exists
    } else if named("(", ").col") {
        SubRun::Any
    } else if named("(", ")") && !text.contains("EXISTS") && !text.contains("ANY") && !text.contains("ALL") {
        SubRun::Expr
    } else {
        // Older servers print every kind as "(SubPlan 1)", and a SubPlan in the output list is not printed without
        // VERBOSE; the cheapest reading keeps the charge a lower bound.
        SubRun::Exists
    })
}

/// Whether `text` holds `name` then `post`, with no other digit right after the name, so "SubPlan 1" does not match
/// "SubPlan 12".
fn mentions(text: &str, name: &str, post: &str) -> bool {
    text.match_indices(name).any(|(at, m)| {
        let rest = &text[at + m.len()..];
        !rest.starts_with(|c: char| c.is_ascii_digit()) && rest.starts_with(post)
    })
}

/// The hashed SubPlan the node `i` tests in place of a per-row one PostgreSQL dropped. The planner may plan a subquery
/// both ways, prices its parent with the first (per row) and only then picks the hash, so the parent's cost still holds
/// a per-row subquery that never runs. The dropped one is the SubPlan numbered just before the hashed one.
pub fn dropped_alternative(p: &Plan, i: usize) -> Option<usize> {
    let n = &p.nodes[i];
    n.kids.iter().copied().find(|&k| {
        let role = &p.nodes[k].role;
        let Some(num) = role.strip_prefix("SubPlan ").and_then(|s| s.parse::<u32>().ok()) else { return false };
        let before = format!("SubPlan {}", num.saturating_sub(1));
        // InitPlans and CTEs take numbers from the same count, so with any of them the gap proves nothing.
        let others = p.nodes.iter().any(|x| x.role.starts_with("InitPlan") || x.role.starts_with("CTE") || x.label == "CTE Scan");
        num >= 2 && !others && sub_run(p, k) == Some(SubRun::Once) && !p.nodes.iter().any(|x| x.role == before) && !n.detail.iter().any(|d| mentions(d, &before, ""))
    })
}

/// What the planner charges a per-row SubPlan inside its parent for one run of the parent: its cost per call, worked out
/// as PostgreSQL's cost_subplan does, times the rows the parent returns. Every row the parent returns passed the test,
/// so that is the fewest calls there can have been. A join tests every pair its condition matched, which an estimate
/// does not give, so what the subquery cost beyond that stays with the parent.
fn per_row_charge(p: &Plan, k: usize) -> Option<f64> {
    let n = &p.nodes[k];
    let run = sub_run(p, k)?;
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
pub fn sub_calls(p: &Plan, k: usize) -> Option<(f64, bool)> {
    let n = &p.nodes[k];
    if matches!(sub_run(p, k), None | Some(SubRun::Once)) {
        return None;
    }
    match n.loops {
        Some(l) if p.analyzed => Some((l, true)),
        _ => Some((p.nodes[n.parent?].rows.unwrap_or(1.0).max(1.0), false)),
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

/// The processes sharing a node's work: those of the Gather above it, when there is one.
fn parallel_procs(p: &Plan, i: usize) -> Option<f64> {
    let mut j = p.nodes[i].parent?;
    loop {
        let x = &p.nodes[j];
        if x.label.starts_with("Gather") {
            return Some(x.workers.unwrap_or(0.0) + 1.0);
        }
        j = x.parent?;
    }
}

/// Each node's cost, or time, with the nodes below it, built up from the own values `derive` worked out. For PostgreSQL
/// and MySQL's tree text that is the loop-scaled cost `spread` gives; MySQL's JSON has only a running total, so its own
/// costs are summed. Building up from own values keeps every parent at least as heavy as its children, which the
/// times alone do not (parallel workers, loops).
pub(crate) fn inclusive(p: &Plan, time: bool) -> Vec<f64> {
    let mut w = vec![0.0; p.nodes.len()];
    // Children follow their parent, so walking back sees every child first.
    for i in (0..p.nodes.len()).rev() {
        let x = &p.nodes[i];
        let own = if time { x.self_ms } else { x.self_cost }.unwrap_or(0.0).max(0.0);
        w[i] = own + x.kids.iter().map(|&k| w[k]).sum::<f64>();
    }
    w
}
