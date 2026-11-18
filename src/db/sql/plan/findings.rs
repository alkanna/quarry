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
        if n.access == Access::Scan && big && weighty {
            if filtered {
                push(Kind::FullScan, Severity::Warn, format!("Full scan of {name}"), weight.clone(), vec![scan_body(n)], None);
            } else if n.join_cond.is_empty() {
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
    }
    if let Some(ev) = ev {
        with_evidence(p, d, ev, pooler, by_time, &mut out);
        super::rewrite::add(p, d, ev, pooler, by_time, &mut out);
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
    }
}
