//! Readable explain plans for the SQL engines. The server's JSON (or the MySQL tree text of EXPLAIN ANALYZE) becomes a flat
//! list of nodes with a depth, so the dialog can draw a tree without knowing which engine wrote it. `tree` links the nodes
//! and works out what each one costs on its own, `cond` reads their conditions and `findings` says what is worth fixing.

use super::{lex, Dialect, ResultSet};
use serde_json::Value;

pub mod compare;
pub mod cond;
pub mod evidence;
pub mod findings;
pub mod flame;
pub mod index;
pub mod rewrite;
mod tree;

pub use evidence::Evidence;
pub use findings::{Finding, FixKind, Severity};

/// A scan that reads at least this many rows is worth a warning. Below it a full scan is often the cheapest plan.
const BIG_SCAN: f64 = 1000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Access {
    /// Reads the whole table.
    Scan,
    /// Reaches rows through an index.
    Index,
    #[default]
    Other,
}

/// What a node does, decided once by the parser from the engine's own names, so the findings and the dialog never match
/// label text. Only the kinds something reads are told apart. MySQL's tree text is recognized for its nested loops,
/// sorts, groups, hashes and full index scans; its hash joins and limits stay `Other`, as they always read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Op {
    /// MySQL's read of a whole index: the JSON's "index" access, the tree's "Index scan on" and "Covering index scan on".
    FullIndexScan,
    NestedLoop,
    HashJoin,
    MergeJoin,
    /// MySQL and MariaDB's block nested loop, matched in a join buffer.
    JoinBuffer,
    /// The hash table a Hash Join builds from its inner side.
    Hash,
    Gather,
    Sort,
    Aggregate,
    Window,
    Limit,
    /// PostgreSQL keeps the rows of a subquery it could not hash here, to read them again for every row it tests.
    Materialize,
    #[default]
    Other,
}

impl Op {
    /// A join that pairs rows on a condition of its own: PostgreSQL's Hash and Merge Join, MySQL's join buffer.
    pub fn is_join(self) -> bool {
        matches!(self, Op::HashJoin | Op::MergeJoin | Op::JoinBuffer)
    }
}

#[derive(Clone, Debug, Default)]
pub struct PlanNode {
    pub depth: usize,
    /// The access type or node type: "ALL", "ref", "Seq Scan", "Hash Join".
    pub label: String,
    pub target: String,
    pub index: String,
    pub access: Access,
    pub op: Op,
    /// The node above this one and those below it, by index into `Plan::nodes`.
    pub parent: Option<usize>,
    pub kids: Vec<usize>,
    /// The table it reads, as the plan names it, and the alias the statement gave it. MySQL plans give only the alias.
    pub rel: String,
    pub alias: String,
    /// What the node is to its parent: PostgreSQL's Outer, Inner, InitPlan or SubPlan.
    pub role: String,
    /// Estimated rows, per loop.
    pub rows: Option<f64>,
    /// Estimated cost. PostgreSQL's and MySQL's tree text include the nodes below; MySQL's JSON is the running total of the join.
    pub cost: Option<f64>,
    /// PostgreSQL's cost before the first row, which a subquery pays again on every call.
    pub startup_cost: Option<f64>,
    /// The node's own cost, when the engine gives it: MySQL's read and eval cost, MariaDB's cost.
    pub own_cost: Option<f64>,
    /// Estimated bytes per row.
    pub width: Option<f64>,
    pub actual_rows: Option<f64>,
    /// Time to the last row, per loop, including the nodes below.
    pub actual_ms: Option<f64>,
    pub loops: Option<f64>,
    /// MariaDB's time in this table over every loop.
    pub table_ms: Option<f64>,
    /// The filter applied to the rows read, and the condition used to search an index or join.
    pub filter: Cond,
    pub index_cond: Cond,
    pub join_cond: Cond,
    /// PostgreSQL's Join Filter: what each pair the join condition matched is checked against again.
    pub join_filter: Cond,
    /// PostgreSQL's One-Time Filter: tested once per run of the node, so a column from outside a subquery here makes it
    /// run again for every row.
    pub one_time: Cond,
    /// Rows the filter removed, per loop.
    pub removed: Option<f64>,
    /// Pairs the join matched and its Join Filter then removed, per loop.
    pub removed_join: Option<f64>,
    /// Rows the filter kept, per loop, when that is not `actual_rows`: MySQL's tree prints the filter as a node of its
    /// own above the scan, so the scan's actual rows are the rows it read.
    pub kept: Option<f64>,
    /// Shared buffers found in memory and read from disk.
    pub buffers: Option<(f64, f64)>,
    /// A sort that spilled, in kB on disk.
    pub sort_spill_kb: Option<f64>,
    /// A hash in several batches: how many it planned and how many it took, and its memory in kB.
    pub hash_batches: Option<(f64, f64)>,
    pub peak_kb: Option<f64>,
    /// Workers a Gather launched, and whether this node runs in them.
    pub workers: Option<f64>,
    pub parallel: bool,
    /// MySQL's guess at the share of rows the filter keeps, in percent.
    pub filtered_pct: Option<f64>,
    pub possible_keys: Vec<String>,
    /// MySQL and MariaDB flags the findings read.
    pub filesort: bool,
    pub temp_table: bool,
    pub join_buffer: bool,
    pub dependent: bool,
    /// The node's own cost and time, without the nodes below, and its share of the plan's (time when it ran, else cost).
    pub self_cost: Option<f64>,
    pub self_ms: Option<f64>,
    pub share: Option<f64>,
    /// The node's share of the plan's cost, and of its time when it ran, worked out once for the findings.
    cost_share: Option<f64>,
    time_share: Option<f64>,
    /// How a subquery runs and where the node above uses it, decided once from the plan's text.
    sub: Option<tree::Sub>,
    /// ANALYZE never reached this node.
    pub never_ran: bool,
    /// PostgreSQL's Sort Key and Group Key, each as one line.
    pub sort_key: String,
    pub group_key: String,
    /// Filters, conditions and keys, one line each.
    pub detail: Vec<String>,
}

/// A condition as the server wrote it, and read, when it reads. Parsed once so drawing a frame never lexes it.
#[derive(Clone, Debug, Default)]
pub struct Cond {
    pub text: String,
    pub expr: Option<cond::Expr>,
}

impl Cond {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

impl PlanNode {
    /// "bookings b", "orders" or "o": what the node reads, for findings and the inspector.
    pub fn name(&self) -> String {
        match (self.rel.as_str(), self.alias.as_str()) {
            ("", "") => self.target.clone(),
            (r, a) if a.is_empty() || a == r => r.to_string(),
            ("", a) => a.to_string(),
            (r, a) => format!("{r} {a}"),
        }
    }

    /// Rows kept by the node's filter, per loop.
    pub fn kept_rows(&self) -> Option<f64> {
        self.kept.or(self.actual_rows)
    }

    /// How far the estimate was from what ran, as a factor of one or more, when both are known.
    pub fn off(&self) -> Option<f64> {
        if self.never_ran {
            return None;
        }
        let (est, act) = (self.rows?, self.actual_rows?);
        Some(est.max(act) / est.min(act).max(1.0))
    }
}

#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub nodes: Vec<PlanNode>,
    /// What is worth fixing, most important first.
    pub findings: Vec<Finding>,
    pub cost: Option<f64>,
    pub planning_ms: Option<f64>,
    pub exec_ms: Option<f64>,
    /// The statement ran, so nodes carry actual rows and times.
    pub analyzed: bool,
    /// PostgreSQL's JIT compilation, when the statement ran with it.
    pub jit: Option<Jit>,
}

/// What PostgreSQL's JIT did for a run: the functions it compiled and the time that took. The time is counted inside the
/// nodes that first ran compiled code, not beside them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Jit {
    pub functions: f64,
    pub total_ms: f64,
    pub inlining: bool,
    pub optimization: bool,
}

/// How an ANALYZE run is made safe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Analyze {
    /// The statement is a read and runs as it is.
    Direct,
    /// PostgreSQL: inside BEGIN and ROLLBACK, so a write leaves no trace.
    Rollback,
}

/// The first word of a statement in capitals, past comments.
fn head(stmt: &str, d: Dialect) -> String {
    lex::head_words(stmt, d, 1).into_iter().next().unwrap_or_default()
}

/// Why `stmt` cannot be explained, when it cannot.
pub fn explainable(stmt: &str, d: Dialect) -> Result<(), String> {
    let h = head(stmt, d);
    match h.as_str() {
        "SELECT" | "WITH" | "TABLE" | "VALUES" | "INSERT" | "UPDATE" | "DELETE" | "REPLACE" | "MERGE" => Ok(()),
        "EXPLAIN" | "DESC" | "DESCRIBE" | "ANALYZE" => Err("This statement is already an explain. Remove the EXPLAIN word to see its plan here.".into()),
        "" => Err("Nothing to explain.".into()),
        _ => Err(format!("{h} has no plan. Explain works on SELECT, INSERT, UPDATE, DELETE and REPLACE.")),
    }
}

/// Whether ANALYZE may run `stmt`, and how. ANALYZE executes the statement, so only reads run as they are; PostgreSQL
/// also takes a write, inside a transaction it never commits. Behind a pooler the BEGIN is refused, so there only a read goes.
pub fn analyze_mode(stmt: &str, d: Dialect, pooler: bool) -> Result<Analyze, String> {
    explainable(stmt, d)?;
    let read = lex::pageable(stmt, d);
    match d {
        Dialect::Postgres if pooler && !read => Err("ANALYZE runs the statement. Behind a connection pooler only a SELECT can be analyzed, because the rollback that makes a write safe is not available.".into()),
        Dialect::Postgres if pooler => Ok(Analyze::Direct),
        Dialect::Postgres => Ok(Analyze::Rollback),
        _ if read => Ok(Analyze::Direct),
        _ => Err("ANALYZE runs the statement, so on this server it is offered only for SELECT.".into()),
    }
}

/// `"8.0.18"` or `"10.11.6-MariaDB"` as numbers, for a version check.
fn version_of(v: &str) -> (u32, u32, u32) {
    let mut n = v.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).map(|p| p.parse().unwrap_or(0));
    (n.next().unwrap_or(0), n.next().unwrap_or(0), n.next().unwrap_or(0))
}

/// Why ANALYZE is not offered on this server version, when it is not.
pub fn analyze_needs(d: Dialect, version: &str) -> Option<&'static str> {
    match d {
        Dialect::MySql if version_of(version) < (8, 0, 18) => Some("EXPLAIN ANALYZE needs MySQL 8.0.18 or newer."),
        Dialect::MariaDb if version_of(version) < (10, 1, 0) => Some("ANALYZE needs MariaDB 10.1 or newer."),
        _ => None,
    }
}

/// The script that makes the plan of `stmt`. A write on PostgreSQL goes between BEGIN and ROLLBACK.
pub fn explain_script(d: Dialect, stmt: &str, analyze: Option<Analyze>) -> String {
    let stmt = stmt.trim().trim_end_matches(';').trim_end();
    match (d, analyze) {
        (Dialect::Postgres, None) => format!("EXPLAIN (FORMAT JSON) {stmt}"),
        (Dialect::Postgres, Some(Analyze::Direct)) => format!("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {stmt}"),
        (Dialect::Postgres, Some(Analyze::Rollback)) => format!("BEGIN;\nEXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {stmt};\nROLLBACK"),
        (Dialect::MariaDb, None) => format!("EXPLAIN FORMAT=JSON {stmt}"),
        (Dialect::MariaDb, Some(_)) => format!("ANALYZE FORMAT=JSON {stmt}"),
        // The tree is the only form EXPLAIN ANALYZE has, and it is what the server prints without a FORMAT.
        (_, None) => format!("EXPLAIN FORMAT=JSON {stmt}"),
        (_, Some(_)) => format!("EXPLAIN ANALYZE {stmt}"),
    }
}

/// The plan text out of the sets a run returned: the first cell of the first result that has rows, or the first error.
pub fn plan_text(sets: &[ResultSet]) -> Result<String, String> {
    if let Some(e) = sets.iter().find_map(|s| s.error.clone()) {
        return Err(e);
    }
    for s in sets {
        let (Some(col), Some(row)) = (s.cols.first(), s.rows.first()) else { continue };
        return match row.doc().get(&col.name) {
            Some(mongodb::bson::Bson::String(t)) => Ok(t.clone()),
            _ => Err("The server returned a plan that is not text.".into()),
        };
    }
    Err("The server returned no plan.".into())
}

/// The plan in `text`, as the engine `d` writes it.
pub fn parse(d: Dialect, text: &str) -> Result<Plan, String> {
    let t = text.trim();
    let json = t.starts_with('{') || t.starts_with('[');
    let mut plan = if json {
        let v: Value = serde_json::from_str(t).map_err(|e| format!("The plan is not valid JSON: {e}"))?;
        if d == Dialect::Postgres { parse_pg(&v)? } else { parse_my(&v)? }
    } else if d != Dialect::Postgres && t.contains("-> ") {
        parse_tree(t)
    } else {
        return Err("The plan is in a form this window cannot read. See the raw text.".into());
    };
    tree::link(&mut plan);
    if !json {
        tree_filters(&mut plan);
    }
    // Parsed before the costs, since a subquery that reads columns from outside it is charged per row.
    for n in &mut plan.nodes {
        for c in [&mut n.filter, &mut n.index_cond, &mut n.join_cond, &mut n.join_filter, &mut n.one_time] {
            c.expr = if c.text.is_empty() { None } else { cond::parse(&c.text, d) };
        }
    }
    tree::derive(&mut plan, d);
    plan.refind(d, None, false);
    Ok(plan)
}

impl Plan {
    /// Works the findings out again, with what the catalog said once it answers.
    pub fn refind(&mut self, d: Dialect, ev: Option<&Evidence>, pooler: bool) {
        self.findings = findings::find(self, d, ev, pooler);
    }
}

/// Pretty JSON for the Raw view; other text stays as the server wrote it.
pub fn raw_text(text: &str) -> String {
    serde_json::from_str::<Value>(text).ok().and_then(|v| serde_json::to_string_pretty(&v).ok()).unwrap_or_else(|| text.to_string())
}

/// The planner's estimate of the rows a one-table read returns, from EXPLAIN's JSON: PostgreSQL's top node, MySQL's rows
/// produced (or examined, cut by the filtered share) and MariaDB's rows cut by its filtered share. A WHERE the optimizer
/// proved false has a message instead of numbers and matches nothing.
pub fn est_rows(d: Dialect, text: &str) -> Option<u64> {
    let v: Value = serde_json::from_str(text).ok()?;
    let n = if d == Dialect::Postgres {
        num(v.get(0)?.get("Plan")?, "Plan Rows")?
    } else {
        let qb = v.get("query_block")?;
        // MariaDB 11 puts even one table in a nested_loop.
        let t = qb.get("table").or_else(|| qb.get("nested_loop")?.get(0)?.get("table")).unwrap_or(qb);
        let share = num(t, "filtered").unwrap_or(100.0) / 100.0;
        match num(t, "rows_produced_per_join").or_else(|| Some(num(t, "rows_examined_per_scan").or_else(|| num(t, "rows"))? * share)) {
            Some(n) => n,
            None if t.get("message").is_some() || qb.get("message").is_some() => 0.0,
            None => return None,
        }
    };
    Some(n.max(0.0).round() as u64)
}

fn num(v: &Value, key: &str) -> Option<f64> {
    match v.get(key)? {
        Value::Number(n) => n.as_f64(),
        // MySQL writes its costs as strings.
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// A key that is a string, or a list of strings, as one line.
fn joined(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(str::to_string).or_else(|| x.as_f64().map(|n| n.to_string()))).collect::<Vec<_>>().join(", "),
        _ => String::new(),
    }
}

/// `1234567.0` as "1,234,567", for findings.
fn count(n: f64) -> String {
    crate::db::group(&format!("{:.0}", n.max(0.0)))
}

// PostgreSQL

fn parse_pg(v: &Value) -> Result<Plan, String> {
    let top = v.as_array().and_then(|a| a.first()).unwrap_or(v);
    let root = top.get("Plan").ok_or("The plan has no Plan node.")?;
    let mut plan = Plan { cost: num(root, "Total Cost"), planning_ms: num(top, "Planning Time"), exec_ms: num(top, "Execution Time"), ..Default::default() };
    plan.analyzed = plan.exec_ms.is_some();
    plan.jit = top.get("JIT").and_then(pg_jit);
    pg_node(root, 0, &mut plan.nodes);
    Ok(plan)
}

/// The JIT block of an analyzed plan. PostgreSQL 17 split Generation into Deform and Total, so a timing is a number or
/// an object with a Total.
fn pg_jit(j: &Value) -> Option<Jit> {
    let timing = j.get("Timing")?;
    let total = match timing.get("Total")? {
        Value::Object(_) => num(timing.get("Total")?, "Total"),
        _ => num(timing, "Total"),
    }?;
    let opts = j.get("Options");
    let on = |k: &str| opts.is_some_and(|o| flag(o, k));
    Some(Jit { functions: num(j, "Functions").unwrap_or(0.0), total_ms: total, inlining: on("Inlining"), optimization: on("Optimization") })
}

fn pg_node(n: &Value, depth: usize, out: &mut Vec<PlanNode>) {
    let ty = text(n, "Node Type");
    let join = text(n, "Join Type");
    let mut node = PlanNode { depth, label: if join.is_empty() { ty.to_string() } else { format!("{ty} ({join})") }, ..Default::default() };
    node.rel = text(n, "Relation Name").to_string();
    node.alias = text(n, "Alias").to_string();
    // Schema is only in VERBOSE plans.
    node.target = match (text(n, "Schema"), node.rel.as_str()) {
        (_, "") => text(n, "CTE Name").to_string(),
        ("", r) => r.to_string(),
        (s, r) => format!("{s}.{r}"),
    };
    if node.rel.is_empty() {
        node.alias.clear();
    } else if !node.alias.is_empty() && node.alias != node.rel {
        node.target = format!("{} as {}", node.target, node.alias);
    }
    node.role = match text(n, "Subplan Name") {
        "" => text(n, "Parent Relationship").to_string(),
        s => s.to_string(),
    };
    node.index = text(n, "Index Name").to_string();
    node.access = if ty == "Seq Scan" { Access::Scan } else if ty.contains("Index") { Access::Index } else { Access::Other };
    node.op = match ty {
        "Nested Loop" => Op::NestedLoop,
        "Hash Join" => Op::HashJoin,
        "Merge Join" => Op::MergeJoin,
        "Hash" => Op::Hash,
        "Gather" | "Gather Merge" => Op::Gather,
        "Sort" | "Incremental Sort" => Op::Sort,
        "Aggregate" | "Group" => Op::Aggregate,
        "WindowAgg" => Op::Window,
        "Limit" => Op::Limit,
        "Materialize" => Op::Materialize,
        _ => Op::Other,
    };
    node.rows = num(n, "Plan Rows");
    node.cost = num(n, "Total Cost");
    node.startup_cost = num(n, "Startup Cost");
    node.width = num(n, "Plan Width");
    node.actual_rows = num(n, "Actual Rows");
    node.actual_ms = num(n, "Actual Total Time");
    node.loops = num(n, "Actual Loops");
    node.never_ran = node.loops == Some(0.0);
    node.filter.text = text(n, "Filter").to_string();
    node.index_cond.text = [text(n, "Index Cond"), text(n, "Recheck Cond")].into_iter().find(|s| !s.is_empty()).unwrap_or_default().to_string();
    // A Join Filter stays a detail line of its own, so a join with both shows both.
    node.join_cond.text = ["Hash Cond", "Merge Cond"].into_iter().map(|k| text(n, k)).find(|s| !s.is_empty()).unwrap_or_default().to_string();
    node.join_filter.text = text(n, "Join Filter").to_string();
    node.one_time.text = text(n, "One-Time Filter").to_string();
    node.parallel = flag(n, "Parallel Aware");
    node.workers = num(n, "Workers Launched").or_else(|| num(n, "Workers Planned"));
    for key in ["Index Cond", "Recheck Cond", "Filter", "Join Filter", "Hash Cond", "Merge Cond", "One-Time Filter"] {
        let t = text(n, key);
        if !t.is_empty() {
            node.detail.push(format!("{key}: {t}"));
        }
    }
    (node.sort_key, node.group_key) = (joined(n, "Sort Key"), joined(n, "Group Key"));
    for (key, t) in [("Sort Key", &node.sort_key), ("Group Key", &node.group_key)] {
        if !t.is_empty() {
            node.detail.push(format!("{key}: {t}"));
        }
    }
    node.removed = num(n, "Rows Removed by Filter");
    if let Some(r) = node.removed {
        node.detail.push(format!("Rows removed by filter: {}", count(r)));
    }
    node.removed_join = num(n, "Rows Removed by Join Filter");
    if let Some(r) = node.removed_join {
        node.detail.push(format!("Rows removed by join filter: {}", count(r)));
    }
    let hit = num(n, "Shared Hit Blocks");
    let read = num(n, "Shared Read Blocks");
    if hit.is_some() || read.is_some() {
        node.buffers = Some((hit.unwrap_or(0.0), read.unwrap_or(0.0)));
        node.detail.push(format!("Buffers: {} hit, {} read", count(hit.unwrap_or(0.0)), count(read.unwrap_or(0.0))));
    }
    if text(n, "Sort Method").starts_with("external") {
        node.sort_spill_kb = num(n, "Sort Space Used");
    }
    if let Some(b) = num(n, "Hash Batches") {
        node.hash_batches = Some((num(n, "Original Hash Batches").unwrap_or(b), b));
        node.peak_kb = num(n, "Peak Memory Usage");
    }
    out.push(node);
    if let Some(kids) = n.get("Plans").and_then(Value::as_array) {
        for k in kids {
            pg_node(k, depth + 1, out);
        }
    }
}

// MySQL and MariaDB

fn parse_my(v: &Value) -> Result<Plan, String> {
    let qb = v.get("query_block").ok_or("The plan has no query_block.")?;
    let mut plan = Plan { cost: qb.get("cost_info").and_then(|c| num(c, "query_cost")).or_else(|| num(qb, "cost")), exec_ms: num(qb, "r_total_time_ms"), ..Default::default() };
    plan.analyzed = qb.get("r_loops").is_some() || plan.exec_ms.is_some();
    my_walk(qb, 0, &mut plan.nodes);
    Ok(plan)
}

/// Visits the keys of one operation object. MySQL and MariaDB nest the same ideas under different names, so each name
/// that carries a table or another operation has an arm and every other key (conditions, flags, costs) is left to its owner.
fn my_walk(o: &Value, depth: usize, out: &mut Vec<PlanNode>) {
    if let Some(a) = o.as_array() {
        a.iter().for_each(|x| my_walk(x, depth, out));
        return;
    }
    let Some(obj) = o.as_object() else { return };
    for (k, v) in obj {
        match k.as_str() {
            "table" => my_table(v, depth, out),
            // Tables joined in a row hang under one node, so the join reads as one step with its tables below it.
            "nested_loop" if v.as_array().is_some_and(|a| a.len() > 1) => {
                out.push(PlanNode { depth, label: "Nested loop".into(), op: Op::NestedLoop, ..Default::default() });
                my_walk(v, depth + 1, out);
            }
            "nested_loop" | "query_block" | "materialized_from_subquery" | "materialized" | "read_sorted_file" => my_walk(v, depth, out),
            "ordering_operation" => my_op("Sort", "", v, depth, out, |n| n.op = Op::Sort),
            "grouping_operation" => my_op("Group", "", v, depth, out, |n| n.op = Op::Aggregate),
            "duplicates_removal" => my_op("Distinct", "", v, depth, out, |_| {}),
            "buffer_result" => my_op("Buffer result", "", v, depth, out, |_| {}),
            "windowing" | "window_functions_computation" => my_op("Window functions", "", v, depth, out, |_| {}),
            "union_result" => my_op("Union", text(v, "table_name"), v, depth, out, |_| {}),
            // MariaDB names the operation and always means it.
            "filesort" => my_op("Filesort", &joined(v, "sort_key"), v, depth, out, |n| (n.filesort, n.op) = (true, Op::Sort)),
            "temporary_table" => my_op("Temporary table", "", v, depth, out, |n| n.temp_table = true),
            // The join's own condition sits here, apart from the filter of the table below.
            "block-nl-join" => my_op("Join buffer", text(v, "join_type"), v, depth, out, |n| (n.join_buffer, n.op, n.join_cond.text) = (true, Op::JoinBuffer, text(v, "attached_condition").to_string())),
            "query_specifications" => v.as_array().into_iter().flatten().for_each(|s| my_walk(s, depth, out)),
            "attached_subqueries" | "optimized_away_subqueries" | "subqueries" => {
                for s in v.as_array().into_iter().flatten() {
                    out.push(PlanNode { depth, label: "Subquery".into(), dependent: flag(s, "dependent"), ..Default::default() });
                    my_walk(s, depth + 1, out);
                }
            }
            _ => {}
        }
    }
}

/// A sort, group, union or buffer step: a node of its own, flagged when the server says it used a filesort or a temporary table.
fn my_op(label: &str, target: &str, v: &Value, depth: usize, out: &mut Vec<PlanNode>, mark: impl FnOnce(&mut PlanNode)) {
    let mut node = PlanNode { depth, label: label.into(), target: target.into(), ..Default::default() };
    mark(&mut node);
    node.filesort |= flag(v, "using_filesort");
    node.temp_table |= flag(v, "using_temporary_table");
    out.push(node);
    my_walk(v, depth + 1, out);
}

fn my_table(t: &Value, depth: usize, out: &mut Vec<PlanNode>) {
    let name = text(t, "table_name");
    let access = text(t, "access_type");
    let key = text(t, "key");
    let mut node = PlanNode { depth, label: if access.is_empty() { "table".into() } else { access.into() }, target: name.into(), index: key.into(), alias: name.into(), ..Default::default() };
    node.access = match access {
        "ALL" => Access::Scan,
        // A full index scan reads every entry, but of a narrower structure; it keeps the neutral color.
        "index" | "" => Access::Other,
        _ if key.is_empty() => Access::Other,
        _ => Access::Index,
    };
    if access == "index" {
        node.op = Op::FullIndexScan;
    }
    node.rows = num(t, "rows_examined_per_scan").or_else(|| num(t, "rows"));
    let ci = t.get("cost_info");
    // MySQL gives the running total of the join and this table's read and eval cost; MariaDB gives this table's cost.
    node.cost = ci.and_then(|c| num(c, "prefix_cost")).or_else(|| num(t, "cost"));
    node.own_cost = match ci.map(|c| (num(c, "read_cost"), num(c, "eval_cost"))) {
        Some((Some(r), Some(e))) => Some(r + e),
        _ => num(t, "cost"),
    };
    node.actual_rows = num(t, "r_rows");
    // MariaDB's ANALYZE counts the rows read and the share its condition kept, so the rows kept and removed follow.
    if let (Some(read), Some(pct)) = (node.actual_rows, num(t, "r_filtered").filter(|_| t.get("attached_condition").is_some())) {
        let kept = read * pct / 100.0;
        (node.kept, node.removed) = (Some(kept), Some((read - kept).max(0.0)));
    }
    node.loops = num(t, "r_loops");
    node.never_ran = node.loops == Some(0.0);
    node.table_ms = match (num(t, "r_table_time_ms"), num(t, "r_other_time_ms")) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
    };
    node.actual_ms = num(t, "r_total_time_ms").or(node.table_ms);
    node.filter.text = text(t, "attached_condition").to_string();
    node.index_cond.text = text(t, "index_condition").to_string();
    node.join_cond.text = joined(t, "ref");
    node.filtered_pct = num(t, "filtered");
    node.possible_keys = match t.get("possible_keys") {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        _ => Vec::new(),
    };
    node.join_buffer = t.get("using_join_buffer").is_some();
    let mut line = |label: &str, v: String| {
        if !v.is_empty() {
            node.detail.push(format!("{label}: {v}"));
        }
    };
    line("Filter", text(t, "attached_condition").to_string());
    line("Index condition", text(t, "index_condition").to_string());
    line("Lookup", joined(t, "ref"));
    line("Key parts", joined(t, "used_key_parts"));
    line("Possible keys", joined(t, "possible_keys"));
    line("Note", text(t, "message").to_string());
    if let Some(p) = num(t, "rows_produced_per_join") {
        line("Produces", format!("{} rows", count(p)));
    }
    if let Some(f) = num(t, "filtered") {
        line("Filtered", format!("{f:.1}%"));
    }
    if flag(t, "using_index") {
        line("Covering index", "no table read needed".into());
    }
    if let Some(j) = t.get("using_join_buffer").and_then(Value::as_str) {
        line("Join buffer", j.to_string());
    }
    out.push(node);
    // A derived table or a subquery attached to the table.
    my_walk(t, depth + 1, out);
}

/// The tree text of MySQL EXPLAIN ANALYZE: one "-> " line per node, four spaces of indent per level.
fn parse_tree(text: &str) -> Plan {
    let mut plan = Plan::default();
    for line in text.lines() {
        let trimmed = line.trim_start();
        let Some(body) = trimmed.strip_prefix("-> ") else { continue };
        let depth = (line.len() - trimmed.len()) / 4;
        let (est_part, act_part) = match body.find("(actual").or_else(|| body.find("(never executed)")) {
            Some(i) => (&body[..i], Some(&body[i..])),
            None => (body, None),
        };
        let cut = est_part.find("  (cost=").or_else(|| est_part.find(" (cost=")).unwrap_or(est_part.trim_end().len());
        let label = body[..cut.min(body.len())].trim().to_string();
        let mut node = PlanNode { depth, label, ..Default::default() };
        node.cost = after(est_part, "cost=");
        node.rows = after(est_part, "rows=");
        if let Some(a) = act_part {
            plan.analyzed = true;
            node.never_ran = a.starts_with("(never executed)");
            node.actual_rows = after(a, "rows=");
            node.loops = after(a, "loops=");
            node.actual_ms = a.find("time=").and_then(|i| a[i..].find("..").map(|j| &a[i + j + 2..])).and_then(|s| after(s, ""));
        }
        let l = node.label.clone();
        node.access = if l.starts_with("Table scan") { Access::Scan } else if l.contains("index") || l.starts_with("Index") { Access::Index } else { Access::Other };
        node.op = if l.to_ascii_lowercase().starts_with("nested loop") {
            Op::NestedLoop
        } else if l.starts_with("Sort") {
            Op::Sort
        } else if l.starts_with("Aggregate") || l.starts_with("Group") {
            Op::Aggregate
        } else if l == "Hash" {
            Op::Hash
        } else if l.starts_with("Covering index scan on ") || l.starts_with("Index scan on ") {
            Op::FullIndexScan
        } else {
            Op::Other
        };
        // "Table scan on o", "Index lookup on c using PRIMARY (id=o.cid)", "Index range scan on o using idx over (..)".
        if let Some(rest) = l.split_once(" on ").map(|(_, r)| r).filter(|_| l.starts_with("Table scan") || l.contains("ndex ")) {
            let mut words = rest.split_whitespace();
            node.alias = words.next().unwrap_or_default().to_string();
            node.target = node.alias.clone();
            if words.next() == Some("using") {
                node.index = words.next().unwrap_or_default().to_string();
            }
        }
        if let Some(c) = l.strip_prefix("Filter: ") {
            node.filter.text = c.to_string();
        }
        node.filesort = l.starts_with("Sort");
        node.temp_table = l.contains("temporary table") || l.contains("Temporary table");
        node.dependent = l.contains("dependent)");
        if plan.cost.is_none() {
            plan.cost = node.cost;
        }
        plan.nodes.push(node);
    }
    plan
}

/// The tree prints "Filter: (cond)" as a node over the "Table scan on o" or full index scan it filters; the scan is what
/// the filter slows down, so it takes the filter and the rows it removed. Runs once the nodes are linked.
fn tree_filters(p: &mut Plan) {
    for i in 0..p.nodes.len() {
        let Some(j) = p.nodes[i].parent else { continue };
        let n = &p.nodes[i];
        if (n.label.starts_with("Table scan") || n.op == Op::FullIndexScan) && n.filter.is_empty() && !p.nodes[j].filter.is_empty() && p.nodes[j].kids.len() == 1 {
            p.nodes[i].filter.text = p.nodes[j].filter.text.clone();
            if let (Some(read), Some(kept)) = (p.nodes[i].actual_rows, p.nodes[j].actual_rows) {
                p.nodes[i].removed = Some((read - kept).max(0.0));
                p.nodes[i].kept = Some(kept);
            }
        }
    }
}

/// The number after `key` in `s`, in plain or exponent form (662e-6). Stops at ".." so a range like 0.03..0.04 reads as 0.03.
fn after(s: &str, key: &str) -> Option<f64> {
    let rest = &s[s.find(key)? + key.len()..];
    let b = rest.as_bytes();
    let mut end = 0;
    while end < b.len() && (b[end].is_ascii_digit() || (b[end] == b'.' && b.get(end + 1).is_some_and(u8::is_ascii_digit)) || (end == 0 && b[end] == b'-')) {
        end += 1;
    }
    if end > 0 && matches!(b.get(end), Some(b'e' | b'E')) {
        let mut e = end + 1;
        if matches!(b.get(e), Some(b'-' | b'+')) {
            e += 1;
        }
        if b.get(e).is_some_and(u8::is_ascii_digit) {
            while b.get(e).is_some_and(u8::is_ascii_digit) {
                e += 1;
            }
            end = e;
        }
    }
    rest[..end].parse().ok()
}

/// A plan a user brought from PostgreSQL 18.6, rebuilt from the numbers the dialog showed: a Sort over a Hash Join of
/// service_inductors and bookings whose join filter tests a subquery on bookings again. Only the estimate was known.
#[cfg(test)]
pub(crate) fn user_plan(join_filter: &str) -> String {
    let scan = |rel: &str, alias: &str, cost: f64, rows: f64, extra: &str| format!(r#"{{"Node Type":"Seq Scan","Relation Name":"{rel}","Alias":"{alias}","Startup Cost":0,"Total Cost":{cost},"Plan Rows":{rows}{extra}}}"#);
    let si = scan("service_inductors", "si", 3442.0, 12622.0, r#","Parent Relationship":"Outer""#);
    let b = scan("bookings", "b", 2670.0, 5009.0, r#","Parent Relationship":"Outer","Filter":"((created_at >= '2026-10-13 00:00:00+02'::timestamp with time zone) AND (provider = ANY ('{ZEUS_TAXI,ZEUS_VLOC,ZEUS_TRAIN}'::provider[])))""#);
    let c = scan("bookings", "c", 2418.0, 1500.0, r#","Parent Relationship":"SubPlan","Subplan Name":"SubPlan 1","Filter":"(status = 'CONFIRMED'::booking_status)""#);
    let hash = format!(r#"{{"Node Type":"Hash","Parent Relationship":"Inner","Startup Cost":2670,"Total Cost":2670,"Plan Rows":5009,"Plans":[{b}]}}"#);
    let join = format!(r#"{{"Node Type":"Hash Join","Join Type":"Inner","Parent Relationship":"Outer","Startup Cost":2700,"Total Cost":59357,"Plan Rows":5009,"Hash Cond":"(si.service_id = b.service_id)","Join Filter":"{join_filter}","Plans":[{si},{hash},{c}]}}"#);
    format!(r#"[{{"Plan":{{"Node Type":"Sort","Startup Cost":59600,"Total Cost":59677,"Plan Rows":5009,"Sort Key":["b.created_at"],"Plans":[{join}]}}}}]"#)
}

#[cfg(test)]
mod tests {
    use super::findings::{below, Kind};
    use super::*;

    const PG: &str = r#"[{"Plan":{"Node Type":"Hash Join","Join Type":"Inner","Startup Cost":10.5,"Total Cost":2500.25,"Plan Rows":50000,"Plan Width":40,"Hash Cond":"(o.customer_id = c.id)","Plans":[
        {"Node Type":"Seq Scan","Parent Relationship":"Outer","Relation Name":"orders","Alias":"o","Startup Cost":0,"Total Cost":2100.0,"Plan Rows":50000,"Plan Width":20,"Filter":"(total > 100)"},
        {"Node Type":"Hash","Startup Cost":5,"Total Cost":5,"Plan Rows":100,"Plan Width":20,"Plans":[
            {"Node Type":"Index Scan","Relation Name":"customers","Schema":"public","Alias":"c","Index Name":"customers_pkey","Startup Cost":0.15,"Total Cost":4.2,"Plan Rows":100,"Plan Width":20,"Index Cond":"(id > 5)"}]}]},
        "Planning Time":0.2}]"#;

    #[test]
    fn postgres_plan_tree_and_warnings() {
        let p = parse(Dialect::Postgres, PG).unwrap();
        assert_eq!(p.nodes.iter().map(|n| (n.depth, n.label.as_str())).collect::<Vec<_>>(), [(0, "Hash Join (Inner)"), (1, "Seq Scan"), (1, "Hash"), (2, "Index Scan")]);
        assert_eq!(p.cost, Some(2500.25));
        assert!(!p.analyzed);
        let seq = &p.nodes[1];
        assert_eq!((seq.target.as_str(), seq.access, seq.rows), ("orders as o", Access::Scan, Some(50000.0)));
        assert!(seq.detail.contains(&"Filter: (total > 100)".to_string()));
        assert_eq!((seq.parent, p.nodes[0].kids.clone(), seq.name()), (Some(0), vec![1, 2], "orders o".to_string()));
        assert_eq!(p.findings.len(), 1);
        assert_eq!((p.findings[0].kind, p.findings[0].node, p.findings[0].title.as_str()), (Kind::FullScan, Some(1), "Full scan of orders o"));
        assert_eq!(p.findings.iter().map(Finding::line).collect::<Vec<_>>(), ["Full scan of orders o. Reads every row, then keeps those matching total."]);
        let ix = &p.nodes[3];
        assert_eq!((ix.target.as_str(), ix.index.as_str(), ix.access), ("public.customers as c", "customers_pkey", Access::Index));
        // Own costs add up to the plan's: the join's 2,500 less the 2,100 and 5 below it.
        assert_eq!(p.nodes.iter().map(|n| (n.self_cost.unwrap() * 100.0).round() / 100.0).collect::<Vec<_>>(), [395.25, 2100.0, 0.8, 4.2]);
    }

    #[test]
    fn postgres_analyze_reads_actuals_and_spills() {
        let json = r#"[{"Plan":{"Node Type":"Sort","Total Cost":900,"Plan Rows":10,"Actual Rows":5000,"Actual Loops":1,"Actual Total Time":12.5,"Sort Key":["a","b"],"Sort Method":"external merge","Sort Space Used":4096,"Plans":[
            {"Node Type":"Seq Scan","Relation Name":"t","Alias":"t","Total Cost":100,"Plan Rows":5000,"Actual Rows":20,"Actual Loops":1,"Actual Total Time":3.0,"Filter":"(x = 1)","Rows Removed by Filter":49980,"Shared Hit Blocks":10,"Shared Read Blocks":2}]},"Planning Time":0.1,"Execution Time":13.0}]"#;
        let p = parse(Dialect::Postgres, json).unwrap();
        assert!(p.analyzed);
        assert_eq!((p.exec_ms, p.planning_ms), (Some(13.0), Some(0.1)));
        let sort = &p.nodes[0];
        assert_eq!((sort.actual_rows, sort.actual_ms, sort.loops), (Some(5000.0), Some(12.5), Some(1.0)));
        assert!(sort.detail.contains(&"Sort Key: a, b".to_string()));
        assert_eq!((sort.self_ms, p.nodes[1].self_ms), (Some(9.5), Some(3.0)));
        let kinds = p.findings.iter().map(|f| (f.kind, f.node)).collect::<Vec<_>>();
        assert!(kinds.contains(&(Kind::SortSpill, Some(0))));
        let spill = p.findings.iter().find(|f| f.kind == Kind::SortSpill).unwrap();
        assert_eq!((spill.weight.as_str(), spill.fix.as_ref().unwrap().sql.as_str()), ("4.0 MB", "SET work_mem = '8MB';"));
        // The scan is where the estimate goes wrong; the sort above only inherits it.
        assert!(kinds.contains(&(Kind::Misestimate, Some(1))) && !kinds.contains(&(Kind::Misestimate, Some(0))));
        assert!(kinds.contains(&(Kind::FullScan, Some(1))));
        let scan = p.findings.iter().find(|f| f.kind == Kind::FullScan).unwrap();
        assert_eq!(scan.body, ["Read 50,000 rows to keep 20 (under 1%)."]);
        let seq = &p.nodes[1];
        assert!(seq.detail.contains(&"Buffers: 10 hit, 2 read".to_string()));
        assert_eq!(seq.buffers, Some((10.0, 2.0)));
    }

    #[test]
    fn postgres_small_seq_scan_is_not_flagged() {
        let json = r#"[{"Plan":{"Node Type":"Seq Scan","Relation Name":"tiny","Alias":"tiny","Total Cost":1.05,"Plan Rows":5,"Filter":"(a = 1)"}}]"#;
        assert!(parse(Dialect::Postgres, json).unwrap().findings.is_empty());
    }

    const MYSQL: &str = r#"{"query_block":{"select_id":1,"cost_info":{"query_cost":"1250.40"},"ordering_operation":{"using_filesort":true,"nested_loop":[
        {"table":{"table_name":"o","access_type":"ALL","possible_keys":["idx_cust"],"rows_examined_per_scan":9000,"rows_produced_per_join":900,"filtered":"10.00","cost_info":{"read_cost":"900.00","eval_cost":"90.00","prefix_cost":"990.00","data_read_per_join":"1M"},"attached_condition":"(`o`.`total` > 100)"}},
        {"table":{"table_name":"c","access_type":"eq_ref","possible_keys":["PRIMARY"],"key":"PRIMARY","used_key_parts":["id"],"key_length":"4","ref":["shop.o.customer_id"],"rows_examined_per_scan":1,"rows_produced_per_join":900,"filtered":"100.00","using_index":true,"cost_info":{"prefix_cost":"1250.40"}}}]}}}"#;

    #[test]
    fn row_estimates_per_engine() {
        assert_eq!(est_rows(Dialect::Postgres, PG), Some(50000));
        assert_eq!(est_rows(Dialect::MySql, r#"{"query_block":{"select_id":1,"table":{"table_name":"t","access_type":"ALL","rows_examined_per_scan":9000,"rows_produced_per_join":900,"filtered":"10.00"}}}"#), Some(900));
        assert_eq!(est_rows(Dialect::MySql, r#"{"query_block":{"select_id":1,"table":{"table_name":"t","rows_examined_per_scan":9000,"filtered":"50.00"}}}"#), Some(4500));
        assert_eq!(est_rows(Dialect::MariaDb, r#"{"query_block":{"select_id":1,"table":{"table_name":"t","access_type":"ALL","rows":20000,"filtered":25,"attached_condition":"t.a > 1"}}}"#), Some(5000));
        assert_eq!(est_rows(Dialect::MariaDb, r#"{"query_block":{"select_id":1,"cost":0.003,"nested_loop":[{"table":{"table_name":"t","rows":40,"filtered":50}}]}}"#), Some(20));
        assert_eq!(est_rows(Dialect::MySql, r#"{"query_block":{"select_id":1,"message":"Impossible WHERE"}}"#), Some(0));
        assert_eq!(est_rows(Dialect::MariaDb, r#"{"query_block":{"select_id":1,"table":{"message":"Impossible WHERE noticed after reading const tables"}}}"#), Some(0));
        assert_eq!(est_rows(Dialect::MySql, "not json"), None);
    }

    #[test]
    fn mysql_json_plan() {
        let p = parse(Dialect::MySql, MYSQL).unwrap();
        assert_eq!(p.nodes.iter().map(|n| (n.depth, n.label.as_str(), n.target.as_str())).collect::<Vec<_>>(), [(0, "Sort", ""), (1, "Nested loop", ""), (2, "ALL", "o"), (2, "eq_ref", "c")]);
        assert_eq!(p.cost, Some(1250.4));
        let o = &p.nodes[2];
        assert_eq!((o.access, o.cost, o.own_cost, o.self_cost), (Access::Scan, Some(990.0), Some(990.0), Some(990.0)));
        assert_eq!((p.nodes[3].access, p.nodes[3].index.as_str()), (Access::Index, "PRIMARY"));
        assert!(p.nodes[3].detail.contains(&"Lookup: shop.o.customer_id".to_string()));
        let kinds = p.findings.iter().map(|f| f.kind).collect::<Vec<_>>();
        assert!(kinds.contains(&Kind::FullScan) && kinds.contains(&Kind::UnusedIndex));
        // 9,000 rows sort quickly enough.
        assert!(!kinds.contains(&Kind::Filesort));
    }

    #[test]
    fn mysql_temporary_table_union_and_subquery() {
        let json = r#"{"query_block":{"union_result":{"using_temporary_table":true,"table_name":"<union1,2>","access_type":"ALL","query_specifications":[
            {"dependent":false,"cacheable":true,"query_block":{"select_id":1,"table":{"table_name":"a","access_type":"ref","key":"k","rows_examined_per_scan":3}}},
            {"dependent":true,"cacheable":false,"query_block":{"select_id":2,"table":{"table_name":"b","access_type":"const","key":"PRIMARY","rows_examined_per_scan":1}}}]}}}"#;
        let p = parse(Dialect::MySql, json).unwrap();
        assert_eq!(p.nodes.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(), ["Union", "ref", "const"]);
        assert!(p.nodes[0].temp_table);
        let sub = r#"{"query_block":{"select_id":1,"table":{"table_name":"t","access_type":"ALL","rows_examined_per_scan":5,"attached_subqueries":[{"dependent":true,"cacheable":false,"query_block":{"select_id":2,"table":{"table_name":"u","access_type":"ref","key":"k","rows_examined_per_scan":1}}}]}}}"#;
        let p = parse(Dialect::MySql, sub).unwrap();
        assert_eq!(p.nodes.iter().map(|n| (n.depth, n.label.as_str())).collect::<Vec<_>>(), [(0, "ALL"), (1, "Subquery"), (2, "ref")]);
        assert!(p.findings.iter().any(|f| f.kind == Kind::DependentSubquery && f.node == Some(1)));
    }

    #[test]
    fn mariadb_json_plan_with_filesort_and_analyze() {
        let json = r#"{"query_block":{"select_id":1,"r_loops":1,"r_total_time_ms":4.2,"filesort":{"sort_key":"t.created","temporary_table":{"function":"buffer","table":{"table_name":"t","access_type":"ALL","rows":20000,"r_rows":20000,"r_loops":1,"r_total_time_ms":3.1,"filtered":100,"r_filtered":100,"attached_condition":"t.a > 1"}}}}}"#;
        let p = parse(Dialect::MariaDb, json).unwrap();
        assert!(p.analyzed);
        assert_eq!(p.exec_ms, Some(4.2));
        assert_eq!(p.nodes.iter().map(|n| (n.depth, n.label.as_str())).collect::<Vec<_>>(), [(0, "Filesort"), (1, "Temporary table"), (2, "ALL")]);
        assert_eq!(p.nodes[0].target, "t.created");
        let t = &p.nodes[2];
        assert_eq!((t.rows, t.actual_rows, t.actual_ms, t.loops), (Some(20000.0), Some(20000.0), Some(3.1), Some(1.0)));
        assert!(p.findings.iter().any(|f| f.kind == Kind::Filesort));
    }

    #[test]
    fn mariadb_nested_loop_with_join_buffer() {
        let json = r#"{"query_block":{"select_id":1,"nested_loop":[{"table":{"table_name":"a","access_type":"range","key":"k","rows":10}},{"block-nl-join":{"table":{"table_name":"b","access_type":"ALL","rows":50},"buffer_type":"flat","join_type":"BNL"}}]}}"#;
        let p = parse(Dialect::MariaDb, json).unwrap();
        assert_eq!(p.nodes.iter().map(|n| (n.depth, n.label.as_str())).collect::<Vec<_>>(), [(0, "Nested loop"), (1, "range"), (1, "Join buffer"), (2, "ALL")]);
        assert!(p.nodes[2].join_buffer);
    }

    #[test]
    fn mysql_analyze_tree_text() {
        let text = "-> Sort: o.total DESC  (cost=1.5 rows=10) (actual time=0.5..0.6 rows=10 loops=1)\n    -> Table scan on o  (cost=1.25 rows=5000) (actual time=0.03..0.40 rows=5000 loops=1)\n        -> Index lookup on c using PRIMARY (id=o.cid)  (cost=0.35 rows=1) (never executed)\n";
        let p = parse(Dialect::MySql, text).unwrap();
        assert!(p.analyzed);
        assert_eq!(p.nodes.iter().map(|n| (n.depth, n.label.as_str())).collect::<Vec<_>>(), [(0, "Sort: o.total DESC"), (1, "Table scan on o"), (2, "Index lookup on c using PRIMARY (id=o.cid)")]);
        let scan = &p.nodes[1];
        assert_eq!((scan.cost, scan.rows, scan.actual_rows, scan.actual_ms, scan.loops), (Some(1.25), Some(5000.0), Some(5000.0), Some(0.4), Some(1.0)));
        assert_eq!((scan.access, scan.alias.as_str()), (Access::Scan, "o"));
        assert_eq!((p.nodes[2].access, p.nodes[2].alias.as_str(), p.nodes[2].index.as_str(), p.nodes[2].never_ran), (Access::Index, "c", "PRIMARY", true));
        assert!(p.nodes[0].filesort);
    }

    #[test]
    fn unreadable_plans_are_errors() {
        assert!(parse(Dialect::Postgres, "[]").is_err());
        assert!(parse(Dialect::MySql, "{\"nothing\":1}").is_err());
        assert!(parse(Dialect::MySql, "not a plan").is_err());
        assert!(parse(Dialect::Postgres, "{").is_err());
    }

    #[test]
    fn scripts_per_engine() {
        let sel = "SELECT 1;";
        assert_eq!(explain_script(Dialect::MySql, sel, None), "EXPLAIN FORMAT=JSON SELECT 1");
        assert_eq!(explain_script(Dialect::MariaDb, sel, None), "EXPLAIN FORMAT=JSON SELECT 1");
        assert_eq!(explain_script(Dialect::Postgres, sel, None), "EXPLAIN (FORMAT JSON) SELECT 1");
        assert_eq!(explain_script(Dialect::MySql, sel, Some(Analyze::Direct)), "EXPLAIN ANALYZE SELECT 1");
        assert_eq!(explain_script(Dialect::MariaDb, sel, Some(Analyze::Direct)), "ANALYZE FORMAT=JSON SELECT 1");
        assert_eq!(explain_script(Dialect::Postgres, "DELETE FROM t", Some(Analyze::Rollback)), "BEGIN;\nEXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) DELETE FROM t;\nROLLBACK");
    }

    #[test]
    fn analyze_only_runs_what_is_safe() {
        assert_eq!(analyze_mode("SELECT * FROM t", Dialect::MySql, false), Ok(Analyze::Direct));
        assert_eq!(analyze_mode("WITH c AS (SELECT 1) SELECT * FROM c", Dialect::MariaDb, false), Ok(Analyze::Direct));
        assert!(analyze_mode("DELETE FROM t", Dialect::MySql, false).is_err());
        assert!(analyze_mode("SELECT * FROM t FOR UPDATE", Dialect::MySql, false).is_err());
        assert_eq!(analyze_mode("DELETE FROM t", Dialect::Postgres, false), Ok(Analyze::Rollback));
        assert_eq!(analyze_mode("SELECT 1", Dialect::Postgres, true), Ok(Analyze::Direct));
        assert!(analyze_mode("UPDATE t SET a = 1", Dialect::Postgres, true).is_err());
        assert!(analyze_mode("EXPLAIN SELECT 1", Dialect::MySql, false).is_err());
        assert!(analyze_mode("DROP TABLE t", Dialect::Postgres, false).is_err());
    }

    #[test]
    fn analyze_needs_a_new_enough_server() {
        assert!(analyze_needs(Dialect::MySql, "5.7.44").is_some());
        assert!(analyze_needs(Dialect::MySql, "8.0.17").is_some());
        assert!(analyze_needs(Dialect::MySql, "8.0.18").is_none());
        assert!(analyze_needs(Dialect::MySql, "8.4.3").is_none());
        assert!(analyze_needs(Dialect::MariaDb, "10.11.6-MariaDB").is_none());
        assert!(analyze_needs(Dialect::Postgres, "16.2").is_none());
    }

    fn fixture(name: &str) -> &'static str {
        match name {
            "pg18-join-analyze" => include_str!("plan/fixtures/pg18-join-analyze.json"),
            "pg18-scan-analyze" => include_str!("plan/fixtures/pg18-scan-analyze.json"),
            "pg18-scan" => include_str!("plan/fixtures/pg18-scan.json"),
            "pg18-parallel-analyze" => include_str!("plan/fixtures/pg18-parallel-analyze.json"),
            "pg18-spill-analyze" => include_str!("plan/fixtures/pg18-spill-analyze.json"),
            "pg18-stale-analyze" => include_str!("plan/fixtures/pg18-stale-analyze.json"),
            "pg18-limit-analyze" => include_str!("plan/fixtures/pg18-limit-analyze.json"),
            "pg18-cte-analyze" => include_str!("plan/fixtures/pg18-cte-analyze.json"),
            "pg18-subplan-analyze" => include_str!("plan/fixtures/pg18-subplan-analyze.json"),
            "pg13-join-analyze" => include_str!("plan/fixtures/pg13-join-analyze.json"),
            "mysql-join" => include_str!("plan/fixtures/mysql-join.json"),
            "mysql-join-analyze" => include_str!("plan/fixtures/mysql-join-analyze.txt"),
            "mysql-joinbuf-analyze" => include_str!("plan/fixtures/mysql-joinbuf-analyze.txt"),
            "mysql-dep-analyze" => include_str!("plan/fixtures/mysql-dep-analyze.txt"),
            "mysql-lower" => include_str!("plan/fixtures/mysql-lower.json"),
            "mariadb-joinbuf-analyze" => include_str!("plan/fixtures/mariadb-joinbuf-analyze.json"),
            "mariadb-scan" => include_str!("plan/fixtures/mariadb-scan.json"),
            "pg18-subjoin" => include_str!("plan/fixtures/pg18-subjoin.json"),
            "pg18-subjoin-analyze" => include_str!("plan/fixtures/pg18-subjoin-analyze.json"),
            "pg18-subhash" => include_str!("plan/fixtures/pg18-subhash.json"),
            "pg18-subhash-analyze" => include_str!("plan/fixtures/pg18-subhash-analyze.json"),
            "pg18-subscalar" => include_str!("plan/fixtures/pg18-subscalar.json"),
            "pg18-subscalar-analyze" => include_str!("plan/fixtures/pg18-subscalar-analyze.json"),
            "pg18-subexists" => include_str!("plan/fixtures/pg18-subexists.json"),
            "pg18-submat" => include_str!("plan/fixtures/pg18-submat.json"),
            "pg18-jitscan-analyze" => include_str!("plan/fixtures/pg18-jitscan-analyze.json"),
            "pg18-slowrow-analyze" => include_str!("plan/fixtures/pg18-slowrow-analyze.json"),
            "pg18-pairs" => include_str!("plan/fixtures/pg18-pairs.json"),
            "pg18-pairs-analyze" => include_str!("plan/fixtures/pg18-pairs-analyze.json"),
            "pg13-subalt" => include_str!("plan/fixtures/pg13-subalt.json"),
            "pg13-submat" => include_str!("plan/fixtures/pg13-submat.json"),
            "pg18-jitpar-analyze" => include_str!("plan/fixtures/pg18-jitpar-analyze.json"),
            "pg18-subout" => include_str!("plan/fixtures/pg18-subout.json"),
            "pg18-subout-analyze" => include_str!("plan/fixtures/pg18-subout-analyze.json"),
            "pg18-subcorr" => include_str!("plan/fixtures/pg18-subcorr.json"),
            "pg18-subcorr-analyze" => include_str!("plan/fixtures/pg18-subcorr-analyze.json"),
            "pg18-subin" => include_str!("plan/fixtures/pg18-subin.json"),
            "pg18-subin-analyze" => include_str!("plan/fixtures/pg18-subin-analyze.json"),
            "pg18-user-subplan" => include_str!("plan/fixtures/pg18-user-subplan.json"),
            "pg18-user-subplan-analyze" => include_str!("plan/fixtures/pg18-user-subplan-analyze.json"),
            _ => panic!("no fixture {name}"),
        }
    }

    fn sum_close(p: &Plan) {
        let total: f64 = p.nodes.iter().filter_map(|n| n.self_cost).sum();
        assert!(p.nodes.iter().all(|n| n.self_cost.unwrap_or(0.0) >= 0.0 && n.self_ms.unwrap_or(0.0) >= 0.0));
        if let Some(c) = p.cost {
            assert!((total - c).abs() <= c * 0.01 + 0.01, "own costs add to {total}, the plan costs {c}");
        }
    }

    #[test]
    fn real_postgres_plans_add_up_and_link() {
        for name in ["pg18-join-analyze", "pg18-scan-analyze", "pg18-scan", "pg18-parallel-analyze", "pg18-spill-analyze", "pg18-limit-analyze", "pg18-cte-analyze", "pg18-subplan-analyze", "pg13-join-analyze", "pg18-subjoin", "pg18-subjoin-analyze", "pg18-subhash", "pg18-subhash-analyze", "pg18-subscalar", "pg18-subscalar-analyze", "pg18-subexists", "pg18-submat", "pg18-jitscan-analyze", "pg18-slowrow-analyze", "pg18-pairs", "pg18-pairs-analyze", "pg13-subalt", "pg13-submat", "pg18-jitpar-analyze", "pg18-subout", "pg18-subout-analyze", "pg18-subcorr", "pg18-subcorr-analyze", "pg18-subin", "pg18-subin-analyze", "pg18-user-subplan", "pg18-user-subplan-analyze"] {
            let p = parse(Dialect::Postgres, fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
            sum_close(&p);
            for (i, n) in p.nodes.iter().enumerate() {
                assert!(n.kids.iter().all(|&k| p.nodes[k].parent == Some(i)), "{name}");
            }
        }
    }

    #[test]
    fn a_parallel_scan_counts_its_time_once() {
        let p = parse(Dialect::Postgres, fixture("pg18-parallel-analyze")).unwrap();
        let scan = p.nodes.iter().find(|n| n.label == "Seq Scan").unwrap();
        assert!(scan.parallel);
        // Three processes each report about 8.7 ms; side by side that is 8.7 ms, not 26.
        assert!((scan.self_ms.unwrap() - 8.686).abs() < 0.01, "{:?}", scan.self_ms);
        let total: f64 = p.nodes.iter().filter_map(|n| n.self_ms).sum();
        assert!((total - 13.021).abs() < 0.05, "{total}");
    }

    #[test]
    fn real_plans_give_the_findings_that_matter() {
        let kinds = |p: &Plan| p.findings.iter().map(|f| f.kind).collect::<Vec<_>>();
        let p = parse(Dialect::Postgres, fixture("pg18-scan-analyze")).unwrap();
        let f = &p.findings[0];
        assert_eq!((f.kind, f.title.as_str()), (Kind::FullScan, "Full scan of bookings"));
        assert!(f.body[0].starts_with("Read 133,333 rows to keep"), "{:?}", f.body);
        let p = parse(Dialect::Postgres, fixture("pg18-stale-analyze")).unwrap();
        let f = p.findings.iter().find(|f| f.kind == Kind::Misestimate).unwrap();
        assert_eq!((f.title.as_str(), f.weight.as_str()), ("Estimate off 199,000x on events", "1 vs 199,000"));
        let p = parse(Dialect::Postgres, fixture("pg18-spill-analyze")).unwrap();
        assert!(kinds(&p).contains(&Kind::SortSpill));
        // The join reads bookings through an index on created_at; nothing there is a full scan worth fixing.
        assert!(!kinds(&parse(Dialect::Postgres, fixture("pg18-join-analyze")).unwrap()).contains(&Kind::FullScan));
        let p = parse(Dialect::MySql, fixture("mysql-join")).unwrap();
        assert_eq!(p.findings[0].kind, Kind::FullScan);
        assert_eq!(p.findings[0].title, "Full scan of o");
        assert!(kinds(&parse(Dialect::MySql, fixture("mysql-lower")).unwrap()).contains(&Kind::FullIndexScan));
        assert!(kinds(&parse(Dialect::MariaDb, fixture("mariadb-scan")).unwrap()).contains(&Kind::FullScan));
    }

    #[test]
    fn mysql_tree_text_links_filters_and_reads_tiny_times() {
        let p = parse(Dialect::MySql, fixture("mysql-join-analyze")).unwrap();
        let scan = p.nodes.iter().find(|n| n.label == "Table scan on o").unwrap();
        assert_eq!((scan.alias.as_str(), scan.removed), ("o", Some(200000.0)));
        assert!(scan.filter.expr.is_some());
        let p = parse(Dialect::MySql, fixture("mysql-joinbuf-analyze")).unwrap();
        let f = p.nodes.iter().find(|n| n.label.starts_with("Filter")).unwrap();
        // "actual time=662e-6..662e-6" is under a millisecond, not 662.
        assert!(f.actual_ms.unwrap() < 0.001, "{:?}", f.actual_ms);
        let p = parse(Dialect::MySql, fixture("mysql-dep-analyze")).unwrap();
        assert!(p.findings.iter().any(|f| f.kind == Kind::DependentSubquery));
        let p = parse(Dialect::MariaDb, fixture("mariadb-joinbuf-analyze")).unwrap();
        let o = p.nodes.iter().find(|n| n.alias == "o").unwrap();
        assert!((o.self_ms.unwrap() - 43.22).abs() < 0.01 && o.self_cost.is_some());
    }

    #[test]
    fn review_cases_count_rows_once_and_keep_old_mariadb_findings() {
        // MySQL's tree: the scan read 200,000 rows and its filter kept none.
        let p = parse(Dialect::MySql, fixture("mysql-join-analyze")).unwrap();
        let f = p.findings.iter().find(|f| f.kind == Kind::FullScan).unwrap();
        assert_eq!(f.body, ["Read 200,000 rows to keep 0 (0%)."]);
        // The inner side of MySQL's nested loop runs once per outer row, so the loop itself costs little of its own.
        let p = parse(Dialect::MySql, fixture("mysql-joinbuf-analyze")).unwrap();
        let nl = &p.nodes[0];
        assert!(nl.label.starts_with("Nested loop") && nl.self_cost.unwrap() < 0.3 * nl.cost.unwrap(), "{:?} of {:?}", nl.self_cost, nl.cost);
        // MariaDB before 11 gives no cost per table; a big filtered scan is still a finding.
        let old = r#"{"query_block":{"select_id":1,"table":{"table_name":"orders","access_type":"ALL","rows":200000,"filtered":100,"attached_condition":"orders.`status` = 'x'"}}}"#;
        assert!(parse(Dialect::MariaDb, old).unwrap().findings.iter().any(|f| f.kind == Kind::FullScan));
        // A join's filter stays visible beside its hash condition.
        let pg = r#"[{"Plan":{"Node Type":"Hash Join","Join Type":"Inner","Total Cost":10,"Plan Rows":5,"Hash Cond":"(o.cid = c.id)","Join Filter":"(o.total > c.cap)"}}]"#;
        let p = parse(Dialect::Postgres, pg).unwrap();
        assert_eq!(p.nodes[0].join_cond.text, "(o.cid = c.id)");
        assert!(p.nodes[0].detail.contains(&"Join Filter: (o.total > c.cap)".to_string()));
        assert!(p.nodes[0].join_cond.expr.is_some());
        assert_eq!(p.nodes[0].join_filter.text, "(o.total > c.cap)");
        assert!(p.nodes[0].join_filter.expr.is_some());
    }

    fn of(p: &Plan, kind: Kind) -> Option<&Finding> {
        p.findings.iter().find(|f| f.kind == kind)
    }

    #[test]
    fn a_join_that_is_most_of_the_plan_is_explained() {
        // The shape of the user's plan: a Hash Join whose filter tests a hashed SubPlan on a third table. PostgreSQL priced
        // the join with the per-row form of the subquery it then dropped, so the join is nearly all of the cost.
        let p = parse(Dialect::Postgres, fixture("pg18-subjoin")).unwrap();
        let join = p.nodes.iter().position(|n| n.label == "Hash Join (Inner)").unwrap();
        assert!(p.nodes[join].share.unwrap() > 0.9, "{:?}", p.nodes[join].share);
        assert!(p.nodes[join].join_filter.text.contains("hashed SubPlan 2") && p.nodes[join].join_filter.expr.is_some());
        let f = of(&p, Kind::Dominant).expect("the join is not explained");
        assert_eq!((f.title.as_str(), f.severity, f.node), ("Hash Join does most of the work", Severity::Warn, Some(join)));
        assert_eq!(f.body[0], "Matches 4,846 rows of bookings b against 12,622 rows of service_inductors si on b.service_id = si.service_id.");
        assert_eq!(f.body[1], "Each match is then checked again against the join filter, which runs SubPlan 2.");
        assert!(f.body.iter().any(|b| b.starts_with("The planner priced the subquery in the join filter as if it ran once per row, but PostgreSQL runs it once, hashed as SubPlan 2")), "{:?}", f.body);
        assert!(f.fix.is_none());
        // The hashed SubPlan runs once, so it costs its total once and gets no per-row finding.
        let sub = p.nodes.iter().find(|n| n.role == "SubPlan 2").unwrap();
        assert!((sub.self_cost.unwrap() - 10334.0).abs() < 1.0, "{:?}", sub.self_cost);
        assert!(of(&p, Kind::SubPlanPerRow).is_none());
    }

    #[test]
    fn a_jit_compile_that_is_most_of_the_run_is_a_finding_with_a_setting() {
        let p = parse(Dialect::Postgres, fixture("pg18-subjoin-analyze")).unwrap();
        let jit = p.jit.as_ref().unwrap();
        assert_eq!((jit.functions, jit.total_ms, jit.inlining, jit.optimization), (33.0, 148.646, true, true));
        let join = p.nodes.iter().find(|n| n.label == "Hash Join (Inner)").unwrap();
        assert_eq!(join.removed_join, Some(4946.0));
        assert!(join.detail.contains(&"Rows removed by join filter: 4,946".to_string()));
        let f = of(&p, Kind::Jit).expect("no JIT finding");
        assert_eq!(f.title, "JIT compilation took 149 ms of 190");
        // PostgreSQL counted the compile inside the scan of bookings b, the first node to run compiled code.
        assert_eq!(p.nodes[f.node.unwrap()].alias, "b");
        assert_eq!(f.fix.as_ref().unwrap().sql, "SET jit = off;");
        assert!(f.body.iter().any(|b| b.starts_with("The rest of the run took 41.4 ms")), "{:?}", f.body);
        // The scan holding the compile is explained by it, not called slow or most of the plan.
        assert!(!p.findings.iter().any(|g| g.node == f.node && matches!(g.kind, Kind::SlowRows | Kind::Dominant)));
        // A short scan that compiled: JIT again, and no note that the full scan is right.
        let p = parse(Dialect::Postgres, fixture("pg18-jitscan-analyze")).unwrap();
        assert!(of(&p, Kind::Jit).is_some());
        assert!(of(&p, Kind::ScanIsRight).is_none() && of(&p, Kind::SlowRows).is_none());
    }

    #[test]
    fn a_scan_slow_per_row_is_a_finding_and_not_called_right() {
        // md5 over a long string for every row: 37 ms for 12,622 rows in memory.
        let p = parse(Dialect::Postgres, fixture("pg18-slowrow-analyze")).unwrap();
        let f = of(&p, Kind::SlowRows).expect("no slow scan finding");
        assert_eq!(f.title, "Reading service_inductors si is slow per row");
        assert!(f.body[0].contains("2.9 µs a row"), "{:?}", f.body);
        assert!(f.body[1].starts_with("Something runs for every row"), "{:?}", f.body);
        assert!(f.fix.is_none());
        assert!(of(&p, Kind::ScanIsRight).is_none() && of(&p, Kind::Dominant).is_none());
        // Plain scans in the captured plans stay well under the threshold.
        for name in ["pg18-scan-analyze", "pg18-join-analyze", "pg18-subhash-analyze", "pg18-parallel-analyze", "pg13-join-analyze"] {
            assert!(of(&parse(Dialect::Postgres, fixture(name)).unwrap(), Kind::SlowRows).is_none(), "{name}");
        }
        // At half its time it is still far over the page bound, so noise between runs does not hide it.
        let half = fixture("pg18-slowrow-analyze").replace(r#""Actual Total Time":36.951"#, r#""Actual Total Time":18.4755"#);
        assert!(of(&parse(Dialect::Postgres, &half).unwrap(), Kind::SlowRows).is_some());
    }

    /// The user's join of bookings b and service_inductors si as PostgreSQL 18 ran it, in the captured shape and costs
    /// of `user_plan` without its subquery: b is 4.5% of the cost and si 5.8%. Every page was in memory: b read 13,530
    /// rows of about 1 kB in 2,273 pages and kept 4,911, si read all 13,895 rows of about 2 kB in 3,316.
    fn user_runs(b_ms: f64, si_ms: f64, total_ms: f64) -> Plan {
        let b = format!(r#"{{"Node Type":"Seq Scan","Parent Relationship":"Outer","Parallel Aware":false,"Relation Name":"bookings","Alias":"b","Startup Cost":0,"Total Cost":2670,"Plan Rows":5009,"Plan Width":40,"Actual Startup Time":0.02,"Actual Total Time":{b_ms},"Actual Rows":4911,"Actual Loops":1,"Filter":"((created_at >= '2026-10-13 00:00:00+02'::timestamp with time zone) AND (provider = ANY ('{{ZEUS_TAXI,ZEUS_VLOC,ZEUS_TRAIN}}'::provider[])))","Rows Removed by Filter":8619,"Shared Hit Blocks":2273,"Shared Read Blocks":0}}"#);
        let si = format!(r#"{{"Node Type":"Seq Scan","Parent Relationship":"Outer","Parallel Aware":false,"Relation Name":"service_inductors","Alias":"si","Startup Cost":0,"Total Cost":3442,"Plan Rows":12622,"Plan Width":60,"Actual Startup Time":0.01,"Actual Total Time":{si_ms},"Actual Rows":13895,"Actual Loops":1,"Shared Hit Blocks":3316,"Shared Read Blocks":0}}"#);
        let hash = format!(r#"{{"Node Type":"Hash","Parent Relationship":"Inner","Startup Cost":2670,"Total Cost":2670,"Plan Rows":5009,"Actual Startup Time":{h},"Actual Total Time":{h},"Actual Rows":4911,"Actual Loops":1,"Plans":[{b}]}}"#, h = b_ms + 0.5);
        let join = format!(r#"{{"Node Type":"Hash Join","Join Type":"Inner","Parent Relationship":"Outer","Startup Cost":2700,"Total Cost":59357,"Plan Rows":5009,"Actual Startup Time":{h},"Actual Total Time":{j},"Actual Rows":4911,"Actual Loops":1,"Hash Cond":"(si.service_id = b.service_id)","Plans":[{si},{hash}]}}"#, h = b_ms + 0.6, j = total_ms - 1.0);
        let plan = format!(r#"[{{"Plan":{{"Node Type":"Sort","Startup Cost":59600,"Total Cost":59677,"Plan Rows":5009,"Actual Startup Time":{total_ms},"Actual Total Time":{total_ms},"Actual Rows":4911,"Actual Loops":1,"Sort Key":["b.created_at"],"Plans":[{join}]}},"Planning Time":0.4,"Execution Time":{e}}}]"#, e = total_ms + 0.2);
        parse(Dialect::Postgres, &plan).unwrap()
    }

    #[test]
    fn the_same_plan_run_again_gives_the_same_findings() {
        let set = |p: &Plan| {
            let mut s: Vec<(String, Kind, Severity)> = p.findings.iter().map(|f| (f.node.map(|i| p.nodes[i].name()).unwrap_or_default(), f.kind, f.severity)).collect();
            s.sort_by(|a, b| a.0.cmp(&b.0).then(a.2.cmp(&b.2)));
            s
        };
        let (b, si) = ("bookings b".to_string(), "service_inductors si".to_string());
        // Four runs the user made, then one where b took only 15% of the time: still well over the time bound, while its
        // cost alone, under 5%, would never make it worth a finding.
        for (b_ms, si_ms, total) in [(8.7, 11.0, 40.5), (13.7, 14.5, 60.7), (9.2, 11.6, 42.0), (15.9, 11.9, 50.8), (7.5, 11.9, 50.0)] {
            let mut p = user_runs(b_ms, si_ms, total);
            let i = p.nodes.iter().position(|n| n.alias == "b").unwrap();
            assert!(p.nodes[i].self_cost.unwrap() / p.nodes.iter().filter_map(|n| n.self_cost).sum::<f64>() < 0.05, "{:?}", p.nodes[i].self_cost);
            if b_ms == 7.5 {
                assert!((p.nodes[i].share.unwrap() - 0.15).abs() < 0.001, "{:?}", p.nodes[i].share);
            }
            assert_eq!(set(&p), [(b.clone(), Kind::FullScan, Severity::Warn), (si.clone(), Kind::ScanIsRight, Severity::Note)], "{b_ms} ms: {:?}", p.findings);
            assert!(p.findings.iter().all(|f| f.evidence.is_empty()));
            // The catalog shows the filter keeps 36%: no index reads less, so the full scan is a note that says so.
            let mut ev = Evidence::demo(&p, Dialect::Postgres);
            ev.tables[0].rows = Some(13_530.0);
            p.refind(Dialect::Postgres, Some(&ev), false);
            assert_eq!(set(&p), [(b.clone(), Kind::FullScan, Severity::Note), (si.clone(), Kind::ScanIsRight, Severity::Note)], "{b_ms} ms: {:?}", p.findings);
            let f = of(&p, Kind::FullScan).unwrap();
            assert!(!f.evidence.is_empty() && f.no_fix.is_none(), "{f:?}");
            assert!(f.body.iter().any(|l| l.starts_with("A full scan is reasonable here: the filter keeps 36%")), "{:?}", f.body);
        }
        // Half again slower than their slowest run, at 10 µs a page, these scans are still read at a normal pace.
        let p = user_runs(23.9, 21.8, 75.0);
        assert!(of(&p, Kind::SlowRows).is_none() && of(&p, Kind::FullScan).is_some_and(|f| f.body.len() == 1), "{:?}", p.findings);
    }

    #[test]
    fn a_slow_filtered_scan_is_one_warning_whatever_the_catalog_says() {
        // The md5 scan with a filter that keeps 37%: one full scan finding that says it is slow, not two.
        let text = fixture("pg18-slowrow-analyze").replace(r#""Actual Rows":12622.0,"#, r#""Actual Rows":4622.0,"Rows Removed by Filter":8000,"Filter":"(service_id = 5)","#);
        let mut p = parse(Dialect::Postgres, &text).unwrap();
        assert!(of(&p, Kind::SlowRows).is_none(), "{:?}", p.findings);
        let f = of(&p, Kind::FullScan).unwrap();
        assert_eq!(f.severity, Severity::Warn);
        assert!(f.body.iter().any(|l| l.contains("2.9 µs a row")), "{:?}", f.body);
        // Keeping too many rows for an index does not make a slow scan reasonable.
        let ev = Evidence::demo(&p, Dialect::Postgres);
        p.refind(Dialect::Postgres, Some(&ev), false);
        let f = of(&p, Kind::FullScan).unwrap();
        assert_eq!(f.severity, Severity::Warn);
        assert!(f.no_fix.as_deref().is_some_and(|w| w.contains("too many for an index")), "{:?}", f.no_fix);
    }

    #[test]
    fn a_reasonable_full_scan_that_is_most_of_a_costly_plan_stays_a_note() {
        // Not run, cost over 10,000, and the estimate keeps 36% of 400,000 rows.
        let text = r#"[{"Plan":{"Node Type":"Seq Scan","Relation Name":"bookings","Alias":"b","Startup Cost":0,"Total Cost":12000,"Plan Rows":144000,"Plan Width":40,"Filter":"(status = 'DONE'::text)"}}]"#;
        let mut p = parse(Dialect::Postgres, text).unwrap();
        assert_eq!(of(&p, Kind::FullScan).map(|f| f.severity), Some(Severity::Warn));
        let ev = Evidence::demo(&p, Dialect::Postgres);
        p.refind(Dialect::Postgres, Some(&ev), false);
        assert_eq!(of(&p, Kind::FullScan).map(|f| f.severity), Some(Severity::Note), "{:?}", p.findings);
        assert!(!p.findings.iter().any(|f| f.severity == Severity::Warn), "{:?}", p.findings);
        // A filter that wraps an indexed column gets no warning about the index either: at 36% it would read no less.
        let mut p = parse(Dialect::Postgres, &text.replace("(status = 'DONE'::text)", "(lower(email) = 'a@b.c'::text)")).unwrap();
        let mut ev = Evidence::demo(&p, Dialect::Postgres);
        ev.tables[0].indexes.as_mut().unwrap().push(evidence::IndexEv { name: "bookings_email_idx".into(), cols: vec!["email".into()], btree: true, ..Default::default() });
        p.refind(Dialect::Postgres, Some(&ev), false);
        assert!(!p.findings.iter().any(|f| f.severity == Severity::Warn), "{:?}", p.findings);
    }

    #[test]
    fn a_subplan_run_per_row_is_charged_per_call_and_is_a_finding() {
        // A correlated scalar subquery in the join filter: an Aggregate over bookings c for every pair.
        let p = parse(Dialect::Postgres, fixture("pg18-subscalar")).unwrap();
        let sub = p.nodes.iter().position(|n| n.role == "SubPlan 1").unwrap();
        assert_eq!(tree::sub_run(&p, sub), Some(tree::SubRun::Expr));
        // 125.73 a call, at least once for each of the 1,615 rows the join returns.
        let charged = p.nodes[sub].self_cost.unwrap() + below(&p, sub).filter_map(|k| p.nodes[k].self_cost).sum::<f64>();
        assert!((charged - 125.73 * 1615.0).abs() < 50.0, "{charged}");
        let f = of(&p, Kind::SubPlanPerRow).expect("no per-row finding");
        assert_eq!(f.node, Some(sub));
        assert_eq!(f.body[0], "SubPlan 1 (Aggregate on bookings c) runs again for every row Hash Join tests, at least 1,615 times, once for each row it returns.");
        // After ANALYZE the count is what ran.
        let p = parse(Dialect::Postgres, fixture("pg18-subscalar-analyze")).unwrap();
        let f = of(&p, Kind::SubPlanPerRow).unwrap();
        assert!(f.body[0].starts_with("SubPlan 1 (Aggregate on bookings c) ran 6,252 times"), "{:?}", f.body);
        // EXISTS stops at its first row, and a plain NOT IN under a Materialize reads half on average.
        let p = parse(Dialect::Postgres, fixture("pg18-subexists")).unwrap();
        let sub = p.nodes.iter().position(|n| n.role == "SubPlan 1").unwrap();
        assert_eq!(tree::sub_run(&p, sub), Some(tree::SubRun::Exists));
        assert!(of(&p, Kind::SubPlanPerRow).is_some());
        let p = parse(Dialect::Postgres, fixture("pg18-submat")).unwrap();
        let sub = p.nodes.iter().position(|n| n.role == "SubPlan 1").unwrap();
        assert_eq!(tree::sub_run(&p, sub), Some(tree::SubRun::Any));
        assert!(p.nodes[sub].self_cost.unwrap() + below(&p, sub).filter_map(|k| p.nodes[k].self_cost).sum::<f64>() > 10_000.0 * 100.0);
        // A hashed NOT IN runs once.
        let p = parse(Dialect::Postgres, fixture("pg18-subhash")).unwrap();
        let sub = p.nodes.iter().position(|n| n.role == "SubPlan 1").unwrap();
        assert_eq!(tree::sub_run(&p, sub), Some(tree::SubRun::Once));
        assert!(of(&p, Kind::SubPlanPerRow).is_none());
    }

    #[test]
    fn the_users_plan_explains_its_join_whichever_way_the_subquery_ran() {
        // Hashed, the subquery runs once and the join keeps about 51,000 of 59,677.
        let p = parse(Dialect::Postgres, &user_plan("(NOT (ANY ((si.id = (hashed SubPlan 1).col1) AND (b.id = (hashed SubPlan 1).col2))))")).unwrap();
        sum_close(&p);
        let join = p.nodes.iter().position(|n| n.label == "Hash Join (Inner)").unwrap();
        assert!((p.nodes[join].share.unwrap() - 0.85).abs() < 0.01, "{:?}", p.nodes[join].share);
        let f = of(&p, Kind::Dominant).expect("the join is not explained");
        assert_eq!((f.title.as_str(), f.node), ("Hash Join does most of the work", Some(join)));
        assert_eq!(f.body[0], "Matches 12,622 rows of service_inductors si against 5,009 rows of bookings b on si.service_id = b.service_id.");
        assert_eq!(f.body[2], "The planner charges 50,827 for the join itself, 6x the cost of the nodes below it (8,530).");
        assert!(f.body[3].starts_with("It expects each row to match many on the other side"), "{:?}", f.body);
        // Per row, EXISTS stops at its first row: 2,418 over 1,500 rows a call, at least once for each of the 5,009 rows.
        let p = parse(Dialect::Postgres, &user_plan("((b.total > '890'::numeric) OR (NOT EXISTS(SubPlan 1)))")).unwrap();
        sum_close(&p);
        let sub = p.nodes.iter().position(|n| n.alias == "c").unwrap();
        assert!((p.nodes[sub].self_cost.unwrap() - 2418.0 / 1500.0 * 5009.0).abs() < 1.0, "{:?}", p.nodes[sub].self_cost);
        assert!(p.nodes[join].share.unwrap() > 0.7);
        assert_eq!(of(&p, Kind::Dominant).unwrap().node, Some(join));
        assert_eq!(of(&p, Kind::SubPlanPerRow).unwrap().node, Some(sub));
    }

    #[test]
    fn a_hashed_subquery_in_the_output_list_is_not_called_per_row() {
        // EXISTS in the SELECT list, correlated on si and b: PostgreSQL 18 plans it per row as SubPlan 1 and hashed as
        // SubPlan 2, prices the join with the first and keeps the second. Without VERBOSE nothing names it.
        let p = parse(Dialect::Postgres, fixture("pg18-subout")).unwrap();
        sum_close(&p);
        let join = p.nodes.iter().position(|n| n.label == "Hash Join (Inner)").unwrap();
        let sub = p.nodes.iter().position(|n| n.role == "SubPlan 2").unwrap();
        assert!(p.nodes[join].join_filter.is_empty() && !p.nodes.iter().any(|n| n.role == "SubPlan 1"));
        assert_eq!(tree::sub_run(&p, sub), Some(tree::SubRun::Once));
        assert!(of(&p, Kind::SubPlanPerRow).is_none(), "{:?}", p.findings);
        let f = of(&p, Kind::Dominant).expect("the join is not explained");
        assert_eq!((f.title.as_str(), f.node), ("Hash Join does most of the work", Some(join)));
        assert!(f.body.iter().any(|b| b == "The planner priced the subquery in the columns it returns (shown only with VERBOSE) as if it ran once per row, but PostgreSQL runs it once, hashed as SubPlan 2, so most of this cost is never spent. Run with ANALYZE to see the real time."), "{:?}", f.body);
        assert!(!f.body.iter().any(|b| b.starts_with("It expects each row") || b.contains("join filter")), "{:?}", f.body);
        // It ran once.
        let p = parse(Dialect::Postgres, fixture("pg18-subout-analyze")).unwrap();
        let sub = p.nodes.iter().position(|n| n.role == "SubPlan 2").unwrap();
        assert_eq!(p.nodes[sub].loops, Some(1.0));
        assert!(of(&p, Kind::SubPlanPerRow).is_none(), "{:?}", p.findings);
        // An uncorrelated IN in the SELECT list is hashed with no other form: SubPlan 1, unnamed, run once.
        for name in ["pg18-subin", "pg18-subin-analyze"] {
            let p = parse(Dialect::Postgres, fixture(name)).unwrap();
            let sub = p.nodes.iter().position(|n| n.role == "SubPlan 1").unwrap();
            assert_eq!(tree::sub_run(&p, sub), Some(tree::SubRun::Once), "{name}");
            assert!(of(&p, Kind::SubPlanPerRow).is_none(), "{name}: {:?}", p.findings);
            assert!(!p.findings.iter().any(|f| f.body.iter().any(|b| b.contains("hashed as"))), "{name}");
        }
        // The user's plan from PostgreSQL 18.6: a lone SubPlan 2 that no condition names, and no join filter.
        let p = parse(Dialect::Postgres, fixture("pg18-user-subplan")).unwrap();
        sum_close(&p);
        let join = p.nodes.iter().position(|n| n.label == "Hash Join (Inner)").unwrap();
        assert!(p.nodes[join].join_filter.is_empty() && p.nodes.iter().any(|n| n.role == "SubPlan 2"));
        assert!(of(&p, Kind::SubPlanPerRow).is_none(), "{:?}", p.findings);
        let f = of(&p, Kind::Dominant).expect("the join is not explained");
        assert_eq!(f.node, Some(join));
        assert!(f.body.iter().any(|b| b.starts_with("The planner priced the subquery in the columns it returns (shown only with VERBOSE) as if it ran once per row")), "{:?}", f.body);
        assert!(!f.body.iter().any(|b| b.starts_with("It expects each row")), "{:?}", f.body);
        // After ANALYZE it ran once, and the join itself took about 11 ms of 47.
        let p = parse(Dialect::Postgres, fixture("pg18-user-subplan-analyze")).unwrap();
        sum_close(&p);
        let sub = p.nodes.iter().position(|n| n.role == "SubPlan 2").unwrap();
        assert_eq!(p.nodes[sub].loops, Some(1.0));
        assert!(of(&p, Kind::SubPlanPerRow).is_none(), "{:?}", p.findings);
        let own = p.nodes[join].self_ms.unwrap();
        assert!((10.0..13.0).contains(&own), "{own}");
        assert!(!p.findings.iter().any(|f| f.body.iter().any(|b| b.starts_with("It expects each row") || b.contains("hashed as"))), "{:?}", p.findings);
    }

    #[test]
    fn a_correlated_subquery_in_the_output_list_runs_per_row() {
        // A scalar count over bookings c for each row: its Index Cond reads si.id, so it cannot be hashed.
        let p = parse(Dialect::Postgres, fixture("pg18-subcorr")).unwrap();
        sum_close(&p);
        let sub = p.nodes.iter().position(|n| n.role == "SubPlan 1").unwrap();
        assert!(p.nodes.iter().any(|n| n.index_cond.text == "(inductor_id = si.id)"));
        assert_eq!(tree::sub_run(&p, sub), Some(tree::SubRun::Exists));
        let f = of(&p, Kind::SubPlanPerRow).expect("no per-row finding");
        assert_eq!(f.node, Some(sub));
        assert!(f.body[0].starts_with("SubPlan 1 (Aggregate on bookings c) runs again for every row Hash Join tests, at least 5,865 times"), "{:?}", f.body);
        let p = parse(Dialect::Postgres, fixture("pg18-subcorr-analyze")).unwrap();
        sum_close(&p);
        let f = of(&p, Kind::SubPlanPerRow).expect("no per-row finding after ANALYZE");
        assert!(f.body[0].starts_with("SubPlan 1 (Aggregate on bookings c) ran 7,500 times"), "{:?}", f.body);
    }

    #[test]
    fn a_join_on_a_column_with_few_values_says_so() {
        // Six service ids on both sides: 4,632 bookings meet 12,622 inductors in 4.9 million pairs.
        let p = parse(Dialect::Postgres, fixture("pg18-pairs")).unwrap();
        let f = of(&p, Kind::Dominant).expect("the join is not explained");
        assert_eq!(f.title, "Hash Join does most of the work");
        assert!(f.body.iter().any(|b| b == "It expects 4,872,097 rows out of it. Each row pairs with many on the other side; check that b.service_id = si.service_id is the join condition you meant."), "{:?}", f.body);
        assert!(!f.body.iter().any(|b| b.starts_with("It expects each row")), "{:?}", f.body);
        let p = parse(Dialect::Postgres, fixture("pg18-pairs-analyze")).unwrap();
        let f = of(&p, Kind::Dominant).expect("the join is not explained after ANALYZE");
        assert!(f.body.contains(&"It built 13,151,707 pairs and kept 13,151,707 (100%).".to_string()), "{:?}", f.body);
        assert!(f.body.iter().any(|b| b.starts_with("Each row pairs with many on the other side")), "{:?}", f.body);
        assert!(f.weight.ends_with(" ms"), "{}", f.weight);
    }

    #[test]
    fn parallel_jit_is_summed_over_the_processes_and_old_servers_print_subplans_their_way() {
        // Three processes each compiled: 462 ms in total, about 154 side by side, of a 204 ms run.
        let p = parse(Dialect::Postgres, fixture("pg18-jitpar-analyze")).unwrap();
        let f = of(&p, Kind::Jit).expect("no JIT finding");
        assert_eq!(f.title, "JIT compilation took about 154 ms of 204");
        assert!(f.weight == "76% of the run" && f.body.iter().any(|b| b.starts_with("Its 462 ms are summed over the 3 processes")), "{} {:?}", f.weight, f.body);
        // PostgreSQL 13 keeps both forms of a subquery and picks one as it runs: each counts once, and nothing claims
        // which one ran.
        let p = parse(Dialect::Postgres, fixture("pg13-subalt")).unwrap();
        for k in p.nodes.iter().enumerate().filter(|(_, n)| n.role.starts_with("SubPlan")).map(|(k, _)| k) {
            assert_eq!(tree::sub_run(&p, k), Some(tree::SubRun::Once));
        }
        assert!(!p.findings.iter().any(|f| f.body.iter().any(|b| b.contains("hashed as"))));
        // Its plain NOT IN prints as "(NOT (SubPlan 1))" over a Materialize, searched again for every row: half of its
        // 10,728 a call, about 5,414, for each of the 970 rows the join returns. Read as a scalar it would be 0.27 a call.
        let p = parse(Dialect::Postgres, fixture("pg13-submat")).unwrap();
        let sub = p.nodes.iter().position(|n| n.role == "SubPlan 1").unwrap();
        assert_eq!(tree::sub_run(&p, sub), Some(tree::SubRun::Any));
        let charged = p.nodes[sub].self_cost.unwrap() + below(&p, sub).filter_map(|k| p.nodes[k].self_cost).sum::<f64>();
        assert!((charged - 5413.8 * 970.0).abs() < 1000.0, "{charged}");
        assert!(of(&p, Kind::SubPlanPerRow).is_some());
    }

    #[test]
    fn a_plan_with_a_node_that_is_most_of_it_never_has_no_findings() {
        for name in ["pg18-subjoin", "pg18-subjoin-analyze", "pg18-subhash", "pg18-subhash-analyze", "pg18-subscalar", "pg18-subscalar-analyze", "pg18-subexists", "pg18-submat", "pg18-jitscan-analyze", "pg18-slowrow-analyze", "pg18-join-analyze", "pg18-cte-analyze", "pg18-subout", "pg18-subcorr"] {
            let p = parse(Dialect::Postgres, fixture(name)).unwrap();
            // A fast plan may be one node without anything wrong.
            let slow = if p.analyzed { p.exec_ms.unwrap() >= 100.0 } else { p.cost.unwrap() >= 10_000.0 };
            for (i, n) in p.nodes.iter().enumerate() {
                if slow && n.share.is_some_and(|s| s >= findings::SHARE_MOST) {
                    assert!(p.findings.iter().any(|f| f.node == Some(i)), "{name}: {} is {:?} of the plan and has no finding", n.label, n.share);
                }
            }
        }
    }

    #[test]
    fn a_scan_the_planner_prices_high_weighs_by_its_cost_on_every_run() {
        let has = |p: &Plan, alias: &str, kind: Kind| p.findings.iter().any(|f| f.kind == kind && f.node.is_some_and(|i| p.nodes[i].alias == alias));
        // b filters 400,000 rows to 6,252 at 45% of the cost; its 12% of the time comes from the hashed subquery.
        assert!(has(&parse(Dialect::Postgres, fixture("pg18-subhash-analyze")).unwrap(), "b", Kind::FullScan));
        // c filters 1.2 million rows at 46% of the cost while JIT takes the time.
        assert!(has(&parse(Dialect::Postgres, fixture("pg18-jitpar-analyze")).unwrap(), "c", Kind::FullScan));
        // All 400,000 rows go into the CTE at 73% of the cost and 18% of the time.
        let p = parse(Dialect::Postgres, fixture("pg18-cte-analyze")).unwrap();
        assert!(p.findings.iter().any(|f| f.kind == Kind::ScanIsRight && f.node.is_some_and(|i| p.nodes[i].rel == "bookings")), "{:?}", p.findings);
        // MySQL leaves the dependent subquery out of its costs, so c is all of them while it took almost none of the time.
        assert!(!has(&parse(Dialect::MySql, fixture("mysql-dep-analyze")).unwrap(), "c", Kind::FullScan));
    }

    #[test]
    fn a_scan_that_took_a_tenth_of_the_time_is_worth_a_finding() {
        let has = |p: &Plan, alias: &str, kind: Kind| p.findings.iter().any(|f| f.kind == kind && f.node.is_some_and(|i| p.nodes[i].alias == alias));
        // 400,000 rows filtered to 53% in 10% of the time, beside the sort that spilled.
        assert!(has(&parse(Dialect::Postgres, fixture("pg18-spill-analyze")).unwrap(), "bookings", Kind::FullScan));
        // 400,000 rows filtered to 10% in 11% of the time, while JIT takes most of it.
        assert!(has(&parse(Dialect::Postgres, fixture("pg18-subjoin-analyze")).unwrap(), "c", Kind::FullScan));
        // All 100,439 rows go into the join buffer in 15% of the time.
        assert!(has(&parse(Dialect::MariaDb, fixture("mariadb-joinbuf-analyze")).unwrap(), "i", Kind::ScanIsRight));
    }

    #[test]
    fn a_subplan_under_the_inner_side_of_a_nested_loop_runs_per_row() {
        // The inner index scan runs 5,000 times and tests one row each time, so its SubPlan ran as often as it did.
        let text = r#"[{"Plan":{"Node Type":"Nested Loop","Join Type":"Inner","Startup Cost":0,"Total Cost":40100,"Plan Rows":5000,"Actual Startup Time":0.1,"Actual Total Time":300,"Actual Rows":5000,"Actual Loops":1,"Plans":[
            {"Node Type":"Seq Scan","Parent Relationship":"Outer","Relation Name":"orders","Alias":"o","Startup Cost":0,"Total Cost":100,"Plan Rows":5000,"Actual Startup Time":0.01,"Actual Total Time":2,"Actual Rows":5000,"Actual Loops":1},
            {"Node Type":"Index Scan","Parent Relationship":"Inner","Index Name":"customers_pkey","Relation Name":"customers","Alias":"c","Startup Cost":0.3,"Total Cost":8,"Plan Rows":1,"Actual Startup Time":0.01,"Actual Total Time":0.05,"Actual Rows":1,"Actual Loops":5000,"Index Cond":"(id = o.customer_id)","Filter":"(NOT (SubPlan 1))","Plans":[
                {"Node Type":"Index Only Scan","Parent Relationship":"SubPlan","Subplan Name":"SubPlan 1","Index Name":"bans_pkey","Relation Name":"bans","Alias":"x","Startup Cost":0.3,"Total Cost":4,"Plan Rows":1,"Actual Startup Time":0.01,"Actual Total Time":0.02,"Actual Rows":0,"Actual Loops":5000,"Index Cond":"(customer_id = c.id)"}]}]},"Planning Time":0.1,"Execution Time":301}]"#;
        let p = parse(Dialect::Postgres, text).unwrap();
        let f = of(&p, Kind::SubPlanPerRow).expect("no per-row finding");
        assert!(f.body[0].starts_with("SubPlan 1 (Index Only Scan on bans x) ran 5,000 times"), "{:?}", f.body);
        // A hashed SubPlan built once in each of three processes ran once.
        for name in ["pg18-subhash-analyze", "pg18-jitpar-analyze"] {
            assert!(of(&parse(Dialect::Postgres, fixture(name)).unwrap(), Kind::SubPlanPerRow).is_none(), "{name}");
        }
    }

    #[test]
    fn a_hashed_subplan_rebuilt_on_each_rescan_is_not_per_row() {
        // The inner index scan tests a hashed NOT IN: its 5,000 loops are one hash table built per rescan, not a call per row.
        let text = r#"[{"Plan":{"Node Type":"Nested Loop","Join Type":"Inner","Startup Cost":0,"Total Cost":40100,"Plan Rows":5000,"Actual Startup Time":0.1,"Actual Total Time":300,"Actual Rows":5000,"Actual Loops":1,"Plans":[
            {"Node Type":"Seq Scan","Parent Relationship":"Outer","Relation Name":"orders","Alias":"o","Startup Cost":0,"Total Cost":100,"Plan Rows":5000,"Actual Startup Time":0.01,"Actual Total Time":2,"Actual Rows":5000,"Actual Loops":1},
            {"Node Type":"Index Scan","Parent Relationship":"Inner","Index Name":"customers_pkey","Relation Name":"customers","Alias":"c","Startup Cost":0.3,"Total Cost":8,"Plan Rows":1,"Actual Startup Time":0.01,"Actual Total Time":0.05,"Actual Rows":1,"Actual Loops":5000,"Index Cond":"(id = o.customer_id)","Filter":"(NOT (hashed SubPlan 1))","Plans":[
                {"Node Type":"Seq Scan","Parent Relationship":"SubPlan","Subplan Name":"SubPlan 1","Relation Name":"bans","Alias":"x","Startup Cost":0,"Total Cost":4,"Plan Rows":10,"Actual Startup Time":0.01,"Actual Total Time":0.02,"Actual Rows":10,"Actual Loops":5000}]}]},"Planning Time":0.1,"Execution Time":301}]"#;
        let p = parse(Dialect::Postgres, text).unwrap();
        assert!(of(&p, Kind::SubPlanPerRow).is_none(), "{:?}", p.findings);
    }

    #[test]
    fn a_subquery_the_leader_runs_alone_under_a_gather_keeps_its_whole_time() {
        // A hashed SubPlan that only the leader built, under a parallel scan: its Nested Loop ran once, its inner side
        // 30,000 times, and neither shares its time with the workers.
        let text = r#"[{"Plan":{"Node Type":"Gather","Startup Cost":1000,"Total Cost":20000,"Plan Rows":30000,"Workers Planned":2,"Workers Launched":2,"Actual Startup Time":1,"Actual Total Time":40,"Actual Rows":30000,"Actual Loops":1,"Plans":[
            {"Node Type":"Seq Scan","Parent Relationship":"Outer","Parallel Aware":true,"Relation Name":"events","Alias":"e","Startup Cost":0,"Total Cost":19000,"Plan Rows":10000,"Actual Startup Time":0.01,"Actual Total Time":31.7,"Actual Rows":10000,"Actual Loops":3,"Filter":"(NOT (hashed SubPlan 1))","Plans":[
                {"Node Type":"Nested Loop","Parent Relationship":"SubPlan","Subplan Name":"SubPlan 1","Join Type":"Inner","Startup Cost":0,"Total Cost":9000,"Plan Rows":30000,"Actual Startup Time":0.01,"Actual Total Time":80,"Actual Rows":30000,"Actual Loops":1,"Plans":[
                    {"Node Type":"Seq Scan","Parent Relationship":"Outer","Relation Name":"logs","Alias":"l","Startup Cost":0,"Total Cost":3000,"Plan Rows":30000,"Plan Width":40,"Actual Startup Time":0.01,"Actual Total Time":60,"Actual Rows":30000,"Actual Loops":1,"Shared Hit Blocks":600,"Shared Read Blocks":0},
                    {"Node Type":"Index Scan","Parent Relationship":"Inner","Index Name":"users_pkey","Relation Name":"users","Alias":"u","Startup Cost":0.2,"Total Cost":0.2,"Plan Rows":1,"Actual Startup Time":0.0004,"Actual Total Time":0.0005,"Actual Rows":1,"Actual Loops":30000,"Index Cond":"(id = l.user_id)"}]}]}]},"Planning Time":0.1,"Execution Time":41}]"#;
        let p = parse(Dialect::Postgres, text).unwrap();
        let ms = |alias: &str| p.nodes.iter().find(|n| n.alias == alias).and_then(|n| n.self_ms).unwrap();
        assert!((ms("l") - 60.0).abs() < 0.01 && (ms("u") - 15.0).abs() < 0.01, "{} {}", ms("l"), ms("u"));
        let f = of(&p, Kind::SlowRows).expect("no slow scan finding");
        assert!(f.body[0].starts_with("It took 60 ms for 30,000 rows"), "{:?}", f.body);
        // A per-row SubPlan in the Gather's own output list runs in the leader too, however many times it ran.
        let text = r#"[{"Plan":{"Node Type":"Gather","Startup Cost":1000,"Total Cost":20000,"Plan Rows":30000,"Workers Planned":2,"Workers Launched":2,"Actual Startup Time":1,"Actual Total Time":70,"Actual Rows":30000,"Actual Loops":1,"Plans":[
            {"Node Type":"Seq Scan","Parent Relationship":"Outer","Parallel Aware":true,"Relation Name":"events","Alias":"e","Startup Cost":0,"Total Cost":19000,"Plan Rows":10000,"Actual Startup Time":0.01,"Actual Total Time":5,"Actual Rows":10000,"Actual Loops":3},
            {"Node Type":"Index Scan","Parent Relationship":"SubPlan","Subplan Name":"SubPlan 1","Index Name":"users_pkey","Relation Name":"users","Alias":"u","Startup Cost":0.2,"Total Cost":0.3,"Plan Rows":1,"Actual Startup Time":0.001,"Actual Total Time":0.002,"Actual Rows":1,"Actual Loops":30000,"Index Cond":"(id = e.user_id)"}]},"Planning Time":0.1,"Execution Time":71}]"#;
        let p = parse(Dialect::Postgres, text).unwrap();
        let u = p.nodes.iter().find(|n| n.alias == "u").unwrap();
        assert!((u.self_ms.unwrap() - 60.0).abs() < 0.01, "{:?}", u.self_ms);
    }

    #[test]
    fn a_plain_scan_in_parallel_workers_is_judged_in_their_time() {
        // Each of three processes read the whole table in 20 ms: 30,000 rows and 600 pages in 60 ms of work, 2 µs a row.
        let text = r#"[{"Plan":{"Node Type":"Gather","Startup Cost":1000,"Total Cost":20000,"Plan Rows":30000,"Workers Planned":2,"Workers Launched":2,"Actual Startup Time":1,"Actual Total Time":25,"Actual Rows":30000,"Actual Loops":1,"Plans":[
            {"Node Type":"Seq Scan","Parent Relationship":"Outer","Parallel Aware":false,"Relation Name":"events","Alias":"e","Startup Cost":0,"Total Cost":19000,"Plan Rows":10000,"Plan Width":40,"Actual Startup Time":0.01,"Actual Total Time":20,"Actual Rows":10000,"Actual Loops":3,"Shared Hit Blocks":600,"Shared Read Blocks":0}]},"Planning Time":0.1,"Execution Time":26}]"#;
        let p = parse(Dialect::Postgres, text).unwrap();
        let f = of(&p, Kind::SlowRows).expect("no slow scan finding");
        assert!(f.body[0].contains("side by side in 3 processes") && f.body[0].contains("2 µs a row"), "{:?}", f.body);
        assert!(of(&p, Kind::ScanIsRight).is_none(), "{:?}", p.findings);
    }

    /// The plan in `text` with the leaf node of SubPlan `sub` moved under a new node `top`, which becomes the SubPlan.
    fn under(text: &str, sub: &str, top: &str) -> String {
        let head = format!(r#""Parent Relationship":"SubPlan","Subplan Name":"{sub}","#);
        let at = text.find(&head).unwrap();
        let (start, end) = (text[..at].rfind('{').unwrap(), at + text[at..].find('}').unwrap() + 1);
        let node = text[start..end].replace(&head, r#""Parent Relationship":"Outer","#);
        format!(r#"{}{{{top},{head}"Plans":[{node}]}}{}"#, &text[..start], &text[end..])
    }

    #[test]
    fn a_subquery_that_cannot_be_hashed_or_reads_outside_in_its_keys_runs_per_row() {
        let hashed = |p: &Plan| p.findings.iter().any(|f| f.body.iter().any(|b| b.contains("hashed as")));
        // An IN PostgreSQL could not hash: a plain SubPlan over a Materialize, read again for every row.
        let p = parse(Dialect::Postgres, &under(fixture("pg18-subout"), "SubPlan 2", r#""Node Type":"Materialize","Startup Cost":0.0,"Total Cost":10533.0,"Plan Rows":39813,"Plan Width":8"#)).unwrap();
        sum_close(&p);
        let sub = p.nodes.iter().position(|n| n.role == "SubPlan 2").unwrap();
        assert_ne!(tree::sub_run(&p, sub), Some(tree::SubRun::Once));
        assert!(of(&p, Kind::SubPlanPerRow).is_some() && !hashed(&p), "{:?}", p.findings);
        // Correlated only in a One-Time Filter or a Sort Key, which no condition of the scan shows.
        for top in [r#""Node Type":"Result","Startup Cost":0.0,"Total Cost":10334.0,"Plan Rows":39813,"Plan Width":8,"One-Time Filter":"(si.active)""#, r#""Node Type":"Sort","Startup Cost":0.0,"Total Cost":10334.0,"Plan Rows":39813,"Plan Width":8,"Sort Key":["(c.total - si.cap)"]"#] {
            let p = parse(Dialect::Postgres, &under(fixture("pg18-subout"), "SubPlan 2", top)).unwrap();
            let sub = p.nodes.iter().position(|n| n.role == "SubPlan 2").unwrap();
            assert_ne!(tree::sub_run(&p, sub), Some(tree::SubRun::Once), "{top}");
            assert!(of(&p, Kind::SubPlanPerRow).is_some() && !hashed(&p), "{top}: {:?}", p.findings);
        }
    }

    #[test]
    fn a_join_never_says_its_cost_includes_a_subquery_charged_apart() {
        // pg18-subcorr with a cheap correlated subquery: charged per row out of the join, which stays most of the plan.
        let p = parse(Dialect::Postgres, &fixture("pg18-subcorr").replace(r#""Startup Cost":125.72,"Total Cost":125.73"#, r#""Startup Cost":1.0,"Total Cost":1.01"#)).unwrap();
        let f = of(&p, Kind::Dominant).expect("the join is not explained");
        assert!(p.nodes[f.node.unwrap()].label.starts_with("Hash Join"), "{f:?}");
        assert!(!f.body.iter().any(|b| b.contains("likely includes")), "{:?}", f.body);
    }

    #[test]
    fn a_filtered_scan_holding_the_jit_compile_stays_a_warning() {
        // b keeps 60,000 of 400,000 rows, too many for an index, but its time is the JIT compile, not a reasonable scan.
        let text = fixture("pg18-subjoin-analyze").replace(r#""Actual Rows":6252.0,"#, r#""Actual Rows":60000.0,"#).replace(r#""Rows Removed by Filter":393748"#, r#""Rows Removed by Filter":340000"#);
        let mut p = parse(Dialect::Postgres, &text).unwrap();
        let b = p.nodes.iter().position(|n| n.alias == "b").unwrap();
        assert_eq!(of(&p, Kind::Jit).and_then(|f| f.node), Some(b));
        let ev = Evidence::demo(&p, Dialect::Postgres);
        p.refind(Dialect::Postgres, Some(&ev), false);
        let f = p.findings.iter().find(|f| f.kind == Kind::FullScan && f.node == Some(b)).expect("no full scan finding");
        assert_eq!(f.severity, Severity::Warn, "{f:?}");
        assert!(!f.body.iter().any(|l| l.starts_with("A full scan is reasonable")), "{:?}", f.body);
        assert!(f.no_fix.as_deref().is_some_and(|w| w.contains("too many for an index")), "{:?}", f.no_fix);
    }
}
