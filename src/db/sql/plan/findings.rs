//! What is worth fixing in a plan, one finding per problem, each tied to the node it is about. A finding says what was
//! seen, with the numbers that show it, and offers a fix only when the plan is enough to back it; the rest is left to
//! the evidence the catalog adds later. Precision over recall: a finding that is wrong teaches people to ignore them all.

use super::evidence::Evidence;
use super::index::{self, Advice};
use super::{count, Access, Dialect, Plan, PlanNode, BIG_SCAN};

/// A node below this share of the plan's cost or time is not worth a finding about its scan.
pub const SHARE_MIN: f64 = 0.20;
/// An estimate this many times off is worth a finding.
pub const OFF_WARN: f64 = 5.0;
/// A sort or temporary table over fewer rows than this costs too little to mention.
const BIG_SORT: f64 = 10_000.0;
/// A node with this share of the plan is what the plan's cost or time is, so it always gets a finding.
pub const SHARE_MOST: f64 = 0.5;
/// A plan this slow, or this costly when it did not run, explains its largest node even below SHARE_MOST, so it never
/// reads as if nothing stood out.
const SLOW_MS: f64 = 100.0;
const SLOW_COST: f64 = 10_000.0;
/// Microseconds per row read above which a scan of pages already in memory is slow. Plain scans in the captured plans
/// take 0.03 to 0.07 µs a row; a function on every row took 2.9, and JIT counted in the scan 3 to 8.
const SLOW_US: f64 = 1.0;
/// Below this, a slow scan or a JIT compile is too short to be worth a finding.
const SLOW_MIN_MS: f64 = 10.0;
/// A subquery run fewer times than this costs too little to mention.
const MANY_CALLS: f64 = 100.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    FullScan,
    ScanIsRight,
    FullIndexScan,
    Discards,
    Misestimate,
    SortSpill,
    HashSpill,
    Filesort,
    TempTable,
    JoinBuffer,
    DependentSubquery,
    UnusedIndex,
    /// The node that takes most of the plan, said in the plan's own numbers when nothing else explains it.
    Dominant,
    /// A PostgreSQL SubPlan that runs again for every row its parent tests.
    SubPlanPerRow,
    /// A scan that spends far longer per row than reading a row takes.
    SlowRows,
    /// JIT compilation that took a large part of the run.
    Jit,
}

/// A warning is something to fix; a note explains something that looks bad and is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Warn,
    Note,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixKind {
    /// A session setting run before the statement, so the draft holds both.
    Setting,
    /// An index to create, from the filter and the catalog.
    Index,
    /// ANALYZE of a table whose statistics are stale.
    Analyze,
    /// The same filter written so an existing index can serve it. `sql` is the condition, not a statement.
    Rewrite,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fix {
    pub kind: FixKind,
    /// The button that offers it: "SET work_mem".
    pub label: String,
    pub sql: String,
    pub note: String,
    /// The bare CREATE INDEX a HypoPG trial of an index fix takes, set by the one index builder. None when HypoPG cannot
    /// simulate it (GIN, several indexes) or the fix is no index.
    pub trial: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub kind: Kind,
    pub severity: Severity,
    pub node: Option<usize>,
    pub title: String,
    /// What makes it matter, for the chip beside the title: "49% of cost", "134 ms", "3.2 MB".
    pub weight: String,
    /// The node's share of the plan, which orders the findings.
    pub share: f64,
    pub body: Vec<String>,
    pub fix: Option<Fix>,
    /// What the catalog showed, one fact a line: the table's size, how much the filter keeps, the indexes it has.
    pub evidence: Vec<String>,
    /// Why no fix is offered where one could be expected, as "No index suggested: ...".
    pub no_fix: Option<String>,
}

impl Finding {
    /// The finding as one sentence or two, for the agent.
    pub fn line(&self) -> String {
        let mut s = self.title.clone();
        for b in &self.body {
            s.push_str(". ");
            s.push_str(b.trim_end_matches('.'));
        }
        s.push('.');
        s
    }
}

/// The findings of a parsed plan, most important first. With the catalog's `ev`, filtered full scans also get an index
/// suggestion or the reason there is none; `pooler` says the connection goes through one, which changes how an index is built.
pub fn find(p: &Plan, d: Dialect, ev: Option<&Evidence>, pooler: bool) -> Vec<Finding> {
    let mut out: Vec<Finding> = Vec::new();
    let by_time = p.analyzed && p.nodes.iter().any(|n| n.self_ms.is_some());
    let jit_node = jit(p, &mut out);
    for (i, n) in p.nodes.iter().enumerate() {
        let share = n.share.unwrap_or(0.0);
        // MariaDB before 11 gives no cost per table, so no share either: size alone decides there.
        let weighty = n.share.is_none_or(|s| s >= SHARE_MIN);
        let weight = weight_of(n, by_time);
        let mut push = |kind, severity, title: String, weight: String, body: Vec<String>, fix: Option<Fix>| out.push(Finding { kind, severity, node: Some(i), title, weight, share, body, fix, evidence: Vec::new(), no_fix: None });
        let name = n.name();
        let read = rows_read(n);
        // PostgreSQL's estimate gives only the rows a scan returns, so its own cost stands in for its size.
        let big = read.is_some_and(|r| r >= BIG_SCAN) || n.removed.is_some_and(|r| r >= BIG_SCAN) || (d == Dialect::Postgres && n.self_cost.is_some_and(|c| c >= BIG_SCAN));
        let filtered = !n.filter.is_empty();
        let slow_any = slow_per_row(n).filter(|_| n.access == Access::Scan && weighty);
        // The scan that holds a JIT compile is explained by the JIT finding.
        let slow = slow_any.filter(|_| jit_node != Some(i));
        if let Some(us) = slow {
            let rows = ran_rows(n).unwrap_or(0.0);
            let pages = n.buffers.map_or(0.0, |(hit, read)| hit + read);
            // Over about 1 kB a row the row itself is wide, so unpacking it comes first among the causes.
            let wide = n.width.is_some_and(|w| w >= 1024.0) || (rows > 0.0 && pages * 8192.0 / rows >= 1024.0);
            let mut body = vec![format!("It took {} ms for {} rows, {} µs a row, with its pages already in memory. A plain scan takes well under 0.1 µs a row.", ms_text(n.self_ms.unwrap_or(0.0)), count(rows), us_text(us))];
            body.push(match &p.jit {
                Some(j) => format!("This run used JIT, which took {} ms to compile {} functions, and PostgreSQL counts that time in the nodes that first ran the compiled code.", ms_text(j.total_ms), count(j.functions)),
                None if wide => format!("Its rows are wide, about {} each in {} pages, so unpacking wide or compressed columns is the likely cost; a function in the output or the filter, or a row-level security policy, would also run for every row.", size(pages * 8.0 / rows.max(1.0)), count(pages)),
                None => "Something runs for every row: a function in the output or the filter, a row-level security policy, or wide or compressed columns being unpacked.".into(),
            });
            push(Kind::SlowRows, Severity::Warn, format!("Reading {name} is slow per row"), weight.clone(), body, None);
        }
        // A scan that is most of the plan, slow, or holding a JIT compile is explained by its own finding, not called right.
        let plain = slow_any.is_none() && share < SHARE_MOST && jit_node != Some(i);
        if n.access == Access::Scan && big && weighty {
            if filtered {
                push(Kind::FullScan, Severity::Warn, format!("Full scan of {name}"), weight.clone(), vec![scan_body(n)], None);
            } else if n.join_cond.is_empty() && plain {
                let rows = read.map(count).unwrap_or_default();
                push(Kind::ScanIsRight, Severity::Note, format!("{name} is read in full"), weight.clone(), vec![format!("Every one of its {rows} rows is used and nothing filters them, so a full scan is the right plan here.")], None);
            }
        } else if ((n.label == "index" && n.access == Access::Other) || super::evidence::full_index_scan(&n.label)) && big && weighty {
            push(Kind::FullIndexScan, Severity::Warn, format!("Full index scan of {name}"), weight.clone(), vec![format!("Reads every entry of {}, about {} of them, instead of searching it.", if n.index.is_empty() { "the index" } else { &n.index }, count(read.unwrap_or(0.0)))], None);
        } else if let (Some(r), Some(k)) = (n.removed, n.kept_rows()) {
            if r >= BIG_SCAN && r >= 10.0 * k && weighty {
                push(Kind::Discards, Severity::Warn, format!("{name} discards most rows it reads"), weight.clone(), vec![kept_line(r, k)], None);
            }
        }
        if n.access == Access::Scan && n.index.is_empty() && !n.possible_keys.is_empty() && big {
            push(Kind::UnusedIndex, Severity::Warn, format!("{name} has an index the server did not use"), weight.clone(), unused_body(n), None);
        }
        if let Some(off) = n.off().filter(|&o| o >= OFF_WARN) {
            let (est, act) = (n.rows.unwrap_or(0.0), n.actual_rows.unwrap_or(0.0));
            // Only where the error starts: the nodes above inherit it.
            if est.max(act) >= BIG_SCAN && !below(p, i).any(|k| p.nodes[k].off().is_some_and(|o| o >= OFF_WARN)) {
                let what = if n.rel.is_empty() && n.alias.is_empty() { n.label.clone() } else { name.clone() };
                push(
                    Kind::Misestimate,
                    Severity::Warn,
                    format!("Estimate off {} on {what}", factor(off)),
                    format!("{} vs {}", count(est), count(act)),
                    vec![format!("The planner expected {} rows and got {}. Joins and memory are sized on that guess.", count(est), count(act))],
                    None,
                );
            }
        }
        if let Some(kb) = n.sort_spill_kb {
            let rows = n.actual_rows.map(|r| format!("Sorting {} rows", count(r * n.loops.unwrap_or(1.0)))).unwrap_or_else(|| "The sort".into());
            push(Kind::SortSpill, Severity::Warn, "Sort spilled to disk".into(), size(kb), vec![format!("{rows} needed {} on disk.", size(kb))], Some(work_mem(kb)));
        }
        if let Some((planned, took)) = n.hash_batches.filter(|&(_, b)| b > 1.0) {
            let need = n.peak_kb.unwrap_or(0.0) * took;
            let note = if took > planned { format!("It planned {} batch{} and needed {}.", count(planned), if planned == 1.0 { "" } else { "es" }, count(took)) } else { format!("It ran in {} batches.", count(took)) };
            push(Kind::HashSpill, Severity::Warn, "Hash spilled to disk".into(), format!("{} batches", count(took)), vec![note], (need > 0.0).then(|| work_mem(need)));
        }
        if d != Dialect::Postgres {
            let fed = below(p, i).filter_map(|k| p.nodes[k].rows).fold(0.0, f64::max);
            if n.filesort && fed >= BIG_SORT {
                push(Kind::Filesort, Severity::Warn, "Sort outside an index".into(), format!("{} rows", count(fed)), vec!["The rows are sorted after they are read (filesort). An index in the ORDER BY order would return them sorted.".into()], None);
            }
            if n.temp_table && fed >= BIG_SORT {
                push(Kind::TempTable, Severity::Warn, "Temporary table".into(), format!("{} rows", count(fed)), vec!["The rows go through a temporary table, which takes memory and may spill to disk.".into()], None);
            }
            if n.join_buffer && (read.unwrap_or(0.0) >= BIG_SCAN || fed >= BIG_SCAN) {
                push(Kind::JoinBuffer, Severity::Warn, "Join through a buffer".into(), String::new(), vec!["No index serves the join condition, so the rows are matched in a join buffer.".into()], None);
            }
        }
        if n.dependent {
            push(Kind::DependentSubquery, Severity::Warn, "Dependent subquery".into(), String::new(), vec!["The subquery runs once for every row of the outer query.".into()], None);
        }
        if let (Some((calls, ran)), Some(j)) = (super::tree::sub_calls(p, i), n.parent) {
            if calls >= MANY_CALLS {
                let parent = kind_of(&p.nodes[j].label);
                let what = format!("{} ({}{})", n.role, kind_of(&n.label), below_name(p, i).map(|s| format!(" on {s}")).unwrap_or_default());
                let first = if ran { format!("{what} ran {} times, once for every row {parent} tested.", count(calls)) } else { format!("{what} runs again for every row {parent} tests, at least {} times, once for each row it returns.", count(calls)) };
                let body = vec![first, "It is neither hashed nor turned into a join, so each call reads it again. MySQL calls this a dependent subquery.".into()];
                push(Kind::SubPlanPerRow, Severity::Warn, "A subquery runs once per row".into(), weight.clone(), body, None);
            }
        }
    }
    if let Some(ev) = ev {
        with_evidence(p, d, ev, pooler, by_time, &mut out);
        super::rewrite::add(p, d, ev, pooler, by_time, &mut out);
    }
    // MySQL's tree text labels a node with its whole line, which does not read as a node type.
    if d == Dialect::Postgres {
        dominant(p, by_time, &mut out);
    }
    out.sort_by(|a, b| a.severity.cmp(&b.severity).then(b.share.total_cmp(&a.share)));
    let mut seen = Vec::new();
    out.retain(|f| {
        let key = (f.kind, f.node);
        let fresh = !seen.contains(&key);
        seen.push(key);
        fresh
    });
    out
}

/// What the catalog adds: on each filtered full scan an index to create, the index that already serves it, stale
/// statistics to refresh, or why none of that is offered. A scan with no finding of its own gets one only when it runs
/// many times inside a nested loop and an index is the fix.
fn with_evidence(p: &Plan, d: Dialect, ev: &Evidence, pooler: bool, by_time: bool, out: &mut Vec<Finding>) {
    for (i, n) in p.nodes.iter().enumerate() {
        if n.access != Access::Scan || n.filter.is_empty() {
            continue;
        }
        let at = |out: &Vec<Finding>, k: Kind| out.iter().position(|f| f.node == Some(i) && f.kind == k);
        let (scan, mis) = (at(out, Kind::FullScan), at(out, Kind::Misestimate));
        let repeated = index::repeated(p, i);
        if scan.is_none() && mis.is_none() && !repeated {
            continue;
        }
        let Some(t) = ev.table(n) else {
            if let Some(k) = scan {
                out[k].no_fix = Some(if ev.failed { "No suggestion: the catalog could not be read." } else { "No index suggested: the table could not be matched to one table of the statement." }.into());
            }
            continue;
        };
        let advice = index::advise(p, i, d, t, pooler);
        if let (Some(k), false) = (mis, matches!(advice, Advice::Stale { .. })) {
            if d == Dialect::Postgres && index::fresh(t) {
                out[k].body.push("Statistics are recent, so the estimate is off for another reason.".into());
            }
        }
        let name = n.name();
        match advice {
            Advice::Stale { why, fix, lines } => {
                if let Some(k) = mis {
                    out[k].body.push(format!("Its statistics are stale: {why}."));
                    (out[k].fix, out[k].evidence) = (Some(fix.clone()), lines.clone());
                }
                if let Some(k) = scan {
                    out[k].no_fix = Some(format!("No index suggested: {why}. Refresh the statistics first."));
                    out[k].evidence = lines;
                    if mis.is_none() {
                        out[k].fix = Some(fix);
                    }
                }
            }
            Advice::Create { fix, lines } => match scan {
                Some(k) => (out[k].fix, out[k].evidence) = (Some(fix), lines),
                None if repeated => {
                    let loops = n.loops.map(|l| format!("Runs {} times", count(l))).unwrap_or_else(|| "Runs once per row of the side above".into());
                    out.push(Finding { kind: Kind::FullScan, severity: Severity::Warn, node: Some(i), title: format!("Repeated full scan of {name}"), weight: weight_of(n, by_time), share: n.share.unwrap_or(0.0), body: vec![format!("{loops} and reads every row each time.")], fix: Some(fix), evidence: lines, no_fix: None });
                }
                None => {}
            },
            Advice::Unused { index: ix, cause, lines } => {
                let Some(k) = scan else { continue };
                out[k].no_fix = Some(format!("No index suggested: {ix} already serves this filter."));
                out.retain(|f| !(f.node == Some(i) && f.kind == Kind::UnusedIndex));
                let body = cause.unwrap_or_else(|| format!("{ix} serves this filter, yet the planner chose a full scan. Run with ANALYZE to see whether its row estimate is why."));
                out.push(Finding { kind: Kind::UnusedIndex, severity: Severity::Warn, node: Some(i), title: format!("{name} has an index the planner did not use"), weight: weight_of(n, by_time), share: n.share.unwrap_or(0.0), body: vec![body], fix: None, evidence: lines, no_fix: None });
            }
            Advice::No { why, lines } => {
                if let Some(k) = scan {
                    (out[k].no_fix, out[k].evidence) = (Some(format!("No index suggested: {why}.")), lines);
                }
            }
        }
    }
}

/// A JIT compile that took a large part of the run, tied to the node that took the most time, where PostgreSQL counted
/// it. Returns that node.
fn jit(p: &Plan, out: &mut Vec<Finding>) -> Option<usize> {
    let (j, exec) = (p.jit.as_ref()?, p.exec_ms?);
    // Each parallel worker compiles its own copy and the plan prints the sum, so side by side it took about the sum
    // over the processes.
    let procs = p.nodes.iter().filter(|n| n.label.starts_with("Gather")).filter_map(|n| n.workers).fold(0.0, f64::max) + 1.0;
    let took = j.total_ms / procs;
    if took < SLOW_MIN_MS || took < 0.2 * exec {
        return None;
    }
    let ms = |i: usize| p.nodes[i].self_ms.unwrap_or(0.0);
    let i = (0..p.nodes.len()).filter(|&i| p.nodes[i].self_ms.is_some()).max_by(|&a, &b| ms(a).total_cmp(&ms(b)))?;
    let n = &p.nodes[i];
    let how = match (j.inlining, j.optimization) {
        (true, true) => " with inlining and optimization",
        (true, false) => " with inlining",
        (false, true) => " with optimization",
        _ => "",
    };
    let mut body = vec![format!(
        "PostgreSQL compiled {} functions{how} and counts that time inside the nodes that first ran them; {} took {} ms here.",
        count(j.functions),
        format!("{} {}", kind_of(&n.label), n.name()).trim(),
        ms_text(n.self_ms.unwrap_or(0.0))
    )];
    if procs > 1.0 {
        body.push(format!("Its {} ms are summed over the {} processes that ran the plan, each compiling its own copy, so side by side it took about {} ms.", ms_text(j.total_ms), count(procs), ms_text(took)));
    }
    let rest = exec - took;
    if rest < took {
        body.push(format!("The rest of the run took {} ms, so compiled code cannot win back what compiling it cost.", ms_text(rest.max(0.0))));
    }
    let fix = Fix {
        kind: FixKind::Setting,
        label: "SET jit".into(),
        sql: "SET jit = off;".into(),
        note: "For this session only. PostgreSQL compiles plans that cost more than jit_above_cost, 100,000 by default; raising it on the server keeps JIT for the statements it helps.".into(),
        trial: None,
    };
    let share = (took / exec).min(1.0);
    let about = if procs > 1.0 { "about " } else { "" };
    out.push(Finding { kind: Kind::Jit, severity: Severity::Warn, node: Some(i), title: format!("JIT compilation took {about}{} ms of {}", ms_text(took), ms_text(exec)), weight: format!("{}% of the run", (share * 100.0).round()), share: n.share.unwrap_or(share).max(share), body, fix: Some(fix), evidence: Vec::new(), no_fix: None });
    Some(i)
}

/// Microseconds per row a scan spent of its own time, when it ran long enough to tell and its pages were in memory.
/// A scan that read most of its pages from disk is waiting on the disk, which is not slow per row in this sense.
fn slow_per_row(n: &PlanNode) -> Option<f64> {
    let ms = n.self_ms.filter(|&m| m >= SLOW_MIN_MS)?;
    let rows = ran_rows(n)?;
    let (hit, read) = n.buffers?;
    if rows < 1.0 || read > 0.1 * (hit + read) {
        return None;
    }
    let us = ms * 1000.0 / rows;
    (us >= SLOW_US).then_some(us)
}

/// The rows a scan read over every loop, from what ran: the rows it returned and those its filter removed.
fn ran_rows(n: &PlanNode) -> Option<f64> {
    Some((n.kept_rows()? + n.removed.unwrap_or(0.0)) * n.loops.unwrap_or(1.0))
}

fn us_text(us: f64) -> String {
    if us >= 10.0 { format!("{us:.0}") } else { format!("{us:.1}").trim_end_matches(".0").to_string() }
}

/// "Hash Join" out of "Hash Join (Inner)".
fn kind_of(label: &str) -> &str {
    label.split(" (").next().unwrap_or(label)
}

/// The first table read at or below node `i`: what a join side or a subquery is about.
fn below_name(p: &Plan, i: usize) -> Option<String> {
    std::iter::once(i).chain(below_in_order(p, i)).map(|k| p.nodes[k].name()).find(|s| !s.is_empty())
}

/// Nodes below `i` in plan order, first child first.
fn below_in_order(p: &Plan, i: usize) -> impl Iterator<Item = usize> + '_ {
    let mut stack: Vec<usize> = p.nodes[i].kids.iter().rev().copied().collect();
    std::iter::from_fn(move || {
        let k = stack.pop()?;
        stack.extend(p.nodes[k].kids.iter().rev().copied());
        Some(k)
    })
}

/// The rows node `k` hands up over every loop: what ran when it ran, else the estimate.
fn rows_out(p: &Plan, k: usize) -> Option<f64> {
    let n = &p.nodes[k];
    match (p.analyzed, n.actual_rows) {
        (true, Some(a)) => Some(a * n.loops.unwrap_or(1.0)),
        _ => n.rows,
    }
}

/// The node that is most of the plan gets a finding when no warning explains it already, so the largest cost is
/// always said in the plan's own numbers. A slow plan explains its largest node even when no node is most of it.
fn dominant(p: &Plan, by_time: bool, out: &mut Vec<Finding>) {
    let slow = if p.analyzed { p.exec_ms.is_some_and(|m| m >= SLOW_MS) } else { p.cost.is_some_and(|c| c >= SLOW_COST) };
    // A fast plan is mostly one node without anything being wrong, like a lookup by primary key.
    if !slow {
        return;
    }
    let warned = |out: &Vec<Finding>, i: usize| out.iter().any(|f| f.node == Some(i) && f.severity == Severity::Warn);
    let mut picks: Vec<usize> = (0..p.nodes.len()).filter(|&i| p.nodes[i].share.is_some_and(|s| s >= SHARE_MOST)).collect();
    if picks.is_empty() && !out.iter().any(|f| f.severity == Severity::Warn) {
        let s = |i: usize| p.nodes[i].share.unwrap_or(0.0);
        picks.extend((0..p.nodes.len()).filter(|&i| p.nodes[i].share.is_some()).max_by(|&a, &b| s(a).total_cmp(&s(b))));
    }
    for i in picks {
        if warned(out, i) {
            continue;
        }
        let n = &p.nodes[i];
        let share = n.share.unwrap_or(0.0);
        let what = kind_of(&n.label).to_string();
        let most = if share >= SHARE_MOST { "does most of the work" } else { "takes the largest part" };
        let kids: Vec<usize> = n.kids.iter().copied().filter(|&k| super::tree::sub_run(p, k).is_none()).collect();
        let fed = kids.iter().filter_map(|&k| rows_out(p, k)).sum::<f64>();
        let key = |k: &str| n.detail.iter().find_map(|d| d.strip_prefix(k)).map(str::trim).map(str::to_string);
        let (severity, title, mut body) = if n.label.contains("Join") || super::tree::is_nested_loop(&n.label) {
            (Severity::Warn, format!("{what} {most}"), join_body(p, i))
        } else if what.contains("Sort") {
            let by = key("Sort Key:").map(|k| format!(" by {k}")).unwrap_or_default();
            (Severity::Warn, format!("{what} {most}"), vec![format!("Sorts {} rows{by}.", count(fed))])
        } else if what.contains("Aggregate") || what == "Group" || what == "WindowAgg" {
            let line = match key("Group Key:") {
                Some(k) => format!("Groups {} rows by {k}.", count(fed)),
                None => format!("Aggregates {} rows.", count(fed)),
            };
            (Severity::Warn, format!("{what} {most}"), vec![line])
        } else if n.access == Access::Scan && n.filter.is_empty() {
            let rows = rows_out(p, i).unwrap_or(0.0);
            (Severity::Note, format!("Reading {} {most}", n.name()), vec![format!("Reads all {} rows of {} and nothing filters them, so a full scan is the right way to read them.", count(rows), n.name())])
        } else if n.access != Access::Other {
            let read = rows_read(n).unwrap_or(0.0) * if p.analyzed { n.loops.unwrap_or(1.0) } else { 1.0 };
            let cols = n.filter_expr.as_ref().map(super::cond::columns).unwrap_or_default();
            let how = if n.index.is_empty() { String::new() } else { format!(" through {}", n.index) };
            let keep = if cols.is_empty() { String::new() } else { format!(" and keeps those matching {}", and_list(&cols)) };
            let sev = if n.access == Access::Scan { Severity::Warn } else { Severity::Note };
            (sev, format!("Reading {} {most}", n.name()), vec![format!("Reads {} rows{how}{keep}.", count(read))])
        } else {
            let line = if kids.is_empty() { format!("Returns {} rows.", count(rows_out(p, i).unwrap_or(0.0))) } else { format!("Takes {} rows from below and returns {}.", count(fed), count(rows_out(p, i).unwrap_or(0.0))) };
            (Severity::Note, format!("{what} {most}"), vec![line])
        };
        if !by_time && p.analyzed {
            body.push("Run times were not reported for it, so this share is of the planner's cost.".into());
        }
        out.push(Finding { kind: Kind::Dominant, severity, node: Some(i), title, weight: weight_of(n, by_time), share, body, fix: None, evidence: Vec::new(), no_fix: None });
    }
}

/// What a join that is most of the plan does, with the numbers that say why.
fn join_body(p: &Plan, i: usize) -> Vec<String> {
    let n = &p.nodes[i];
    let sides: Vec<usize> = n.kids.iter().copied().filter(|&k| super::tree::sub_run(p, k).is_none()).collect();
    let side = |k: usize| {
        // A Hash Join's inner side is the Hash built from the node below it.
        let k = if p.nodes[k].label == "Hash" { p.nodes[k].kids.first().copied().unwrap_or(k) } else { k };
        (rows_out(p, k).unwrap_or(0.0), below_name(p, k).unwrap_or_else(|| kind_of(&p.nodes[k].label).to_string()))
    };
    let cond = n.join_cond_expr.as_ref().map(|e| super::cond::one_line(e, 3)).unwrap_or_else(|| n.join_cond.trim_start_matches('(').trim_end_matches(')').to_string());
    let mut body = Vec::new();
    let mut sides_rows = 0.0;
    if let [outer, inner, ..] = sides[..] {
        let ((or, on), (ir, inn)) = (side(outer), side(inner));
        sides_rows = or.max(ir);
        let on_cond = if cond.is_empty() { String::new() } else { format!(" on {cond}") };
        body.push(format!("Matches {} rows of {on} against {} rows of {inn}{on_cond}.", count(or), count(ir)));
    }
    let subs: Vec<String> = n.kids.iter().filter(|&&k| super::tree::sub_run(p, k).is_some()).map(|&k| p.nodes[k].role.clone()).filter(|r| !r.is_empty()).collect();
    if !n.join_filter.is_empty() {
        let runs = if subs.is_empty() { String::new() } else { format!(", which runs {}", and_list(&subs)) };
        body.push(format!("Each match is then checked again against the join filter{runs}."));
    }
    // A join whose pairs far outnumber the rows on either side pairs each row with many on the other: the join
    // condition is on a column with few distinct values, which is worth checking against the intent.
    let check = format!("Each row pairs with many on the other side; check that {} is the join condition you meant.", if cond.is_empty() { "the condition".to_string() } else { cond.clone() });
    if p.analyzed {
        let loops = n.loops.unwrap_or(1.0);
        let kept = n.actual_rows.unwrap_or(0.0) * loops;
        let pairs = kept + n.removed_join.unwrap_or(0.0) * loops;
        if n.removed_join.is_some() {
            let pct = if pairs > 0.0 { kept / pairs * 100.0 } else { 0.0 };
            body.push(format!("It built {} pairs and kept {} ({}).", count(pairs), count(kept), pct_text(pct)));
        }
        if sides_rows > 0.0 && pairs >= 10.0 * sides_rows {
            body.push(check);
        }
    } else {
        let inc = super::tree::inclusive(p, false);
        let reading: f64 = n.kids.iter().map(|&k| inc[k]).sum();
        let own = n.self_cost.unwrap_or(0.0);
        let heavy = own >= 2.0 * reading && reading > 0.0;
        let out = n.rows.unwrap_or(0.0);
        let fans = sides_rows > 0.0 && out >= 10.0 * sides_rows;
        if fans {
            body.push(format!("It expects {} rows out of it. {check}", count(out)));
        }
        if heavy {
            let times = if own >= 20.0 * reading { "over 20 times".to_string() } else { factor(own / reading) };
            body.push(format!("The planner charges {} for the join itself, {times} the cost of the nodes below it ({}).", count(own), count(reading)));
        }
        if let Some(k) = super::tree::dropped_alternative(p, i).filter(|_| heavy) {
            body.push(format!("PostgreSQL planned the subquery in the join filter both per row and hashed, priced the join with the per-row form, then chose the hashed {}, so much of this cost is never spent. Run with ANALYZE to see the real time.", p.nodes[k].role));
        } else if heavy && !fans {
            body.push("It expects each row to match many on the other side, or a costly join filter. Run with ANALYZE to see the real time.".into());
        }
    }
    body
}

/// Every node below `i`.
pub fn below(p: &Plan, i: usize) -> impl Iterator<Item = usize> + '_ {
    let mut stack = p.nodes[i].kids.clone();
    std::iter::from_fn(move || {
        let k = stack.pop()?;
        stack.extend(p.nodes[k].kids.iter().copied());
        Some(k)
    })
}

/// The rows a scan reads per loop: what it returned and what its filter removed, or the estimate.
pub fn rows_read(n: &PlanNode) -> Option<f64> {
    match (n.kept_rows(), n.removed) {
        (Some(k), Some(r)) => Some(k + r),
        _ => n.rows.map(|r| r + n.removed.unwrap_or(0.0)),
    }
}

/// MySQL's possible keys without a key: the names and the server's guess at what the filter keeps, which is all the plan says.
fn unused_body(n: &PlanNode) -> Vec<String> {
    let mut body = vec![format!("Possible keys: {}. The server judged a full scan cheaper, or the filter cannot use them.", n.possible_keys.join(", "))];
    if let Some(f) = n.filtered_pct {
        body.push(format!("The server guesses the filter keeps {} of the rows it reads.", pct_text(f)));
    }
    body
}

pub(super) fn weight_of(n: &PlanNode, by_time: bool) -> String {
    match (by_time, n.self_ms, n.share) {
        (true, Some(ms), _) => format!("{} ms", ms_text(ms)),
        (_, _, Some(s)) => format!("{}% of cost", (s * 100.0).round()),
        _ => String::new(),
    }
}

fn scan_body(n: &PlanNode) -> String {
    let cols = n.filter_expr.as_ref().map(super::cond::columns).unwrap_or_default();
    match (n.kept_rows(), n.removed) {
        (Some(k), Some(r)) if r > 0.0 => kept_line(r, k),
        _ if !cols.is_empty() => format!("Reads every row, then keeps those matching {}.", and_list(&cols)),
        _ => "Reads every row, then filters them.".into(),
    }
}

fn kept_line(removed: f64, kept: f64) -> String {
    let read = removed + kept;
    let pct = if read > 0.0 { kept / read * 100.0 } else { 0.0 };
    format!("Read {} rows to keep {} ({}).", count(read), count(kept), pct_text(pct))
}

/// "created_at, provider and status".
pub fn and_list(xs: &[String]) -> String {
    match xs {
        [] => String::new(),
        [a] => a.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// 8.5x, 12x, 1,200x.
pub fn factor(f: f64) -> String {
    if f < 10.0 { format!("{:.1}x", f).replace(".0x", "x") } else { format!("{}x", count(f)) }
}

pub fn pct_text(p: f64) -> String {
    if p > 0.0 && p < 1.0 { "under 1%".into() } else { format!("{}%", p.round()) }
}

pub fn ms_text(ms: f64) -> String {
    if ms >= 100.0 {
        count(ms)
    } else if ms >= 1.0 {
        format!("{ms:.1}").trim_end_matches(".0").to_string()
    } else {
        format!("{ms:.2}")
    }
}

/// 6,024 kB as "5.9 MB".
pub fn size(kb: f64) -> String {
    if kb >= 1024.0 * 1024.0 {
        format!("{:.1} GB", kb / 1024.0 / 1024.0)
    } else if kb >= 1024.0 {
        format!("{:.1} MB", kb / 1024.0)
    } else {
        format!("{} kB", count(kb))
    }
}

/// A work_mem that holds `kb` with room: the next power of two above twice it, between 4 MB and 1 GB.
fn work_mem(kb: f64) -> Fix {
    let mb = ((kb * 2.0 / 1024.0).max(4.0)).min(1024.0);
    let mb = (mb.log2().ceil()).exp2().min(1024.0) as u64;
    Fix {
        kind: FixKind::Setting,
        label: "SET work_mem".into(),
        sql: format!("SET work_mem = '{mb}MB';"),
        note: "For this session only. A higher server-wide value multiplies by every sort and hash running at once.".into(),
        trial: None,
    }
}
