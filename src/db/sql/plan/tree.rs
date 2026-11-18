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
    // MySQL's tree prints "Filter: (cond)" over the "Table scan on o" it filters; the scan is what the filter slows down.
    for i in 0..p.nodes.len() {
        let Some(j) = p.nodes[i].parent else { continue };
        if p.nodes[i].label.starts_with("Table scan") && p.nodes[i].filter.is_empty() && !p.nodes[j].filter.is_empty() && p.nodes[j].kids.len() == 1 {
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
/// totals below it, so the nodes below are scaled to fit inside it and the parts add up to the plan's cost.
fn spread(p: &Plan, i: usize, scale: f64, loops: &[f64], eff: &mut [f64]) {
    let own = p.nodes[i].cost.unwrap_or(0.0) * loops[i] * scale;
    eff[i] = own;
    let raw: f64 = p.nodes[i].kids.iter().map(|&k| p.nodes[k].cost.unwrap_or(0.0) * loops[k] * scale).sum();
    let fit = if raw > own && raw > 0.0 { own / raw } else { 1.0 };
    for &k in &p.nodes[i].kids {
        spread(p, k, scale * fit, loops, eff);
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
