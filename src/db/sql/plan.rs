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
mod tree;

pub use evidence::Evidence;
pub use findings::{Finding, FixKind, Severity};

/// A scan that reads at least this many rows is worth a warning. Below it a full scan is often the cheapest plan.
pub const BIG_SCAN: f64 = 1000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Access {
    /// Reads the whole table.
    Scan,
    /// Reaches rows through an index.
    Index,
    #[default]
    Other,
}

#[derive(Clone, Debug, Default)]
pub struct PlanNode {
    pub depth: usize,
    /// The access type or node type: "ALL", "ref", "Seq Scan", "Hash Join".
    pub label: String,
    pub target: String,
    pub index: String,
    pub access: Access,
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
    /// The filter applied to the rows read, as the server wrote it, and the condition used to search an index or join.
    pub filter: String,
    pub index_cond: String,
    pub join_cond: String,
    /// The conditions read, when they read; parsed once so drawing a frame never lexes them.
    pub filter_expr: Option<cond::Expr>,
    pub index_cond_expr: Option<cond::Expr>,
    pub join_cond_expr: Option<cond::Expr>,
    /// Rows the filter removed, per loop.
    pub removed: Option<f64>,
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
    /// ANALYZE never reached this node.
    pub never_ran: bool,
    /// Filters, conditions and keys, one line each.
    pub detail: Vec<String>,
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

    /// How far the estimate was from what ran, as a factor of one or more, when both are known.
    /// Rows kept by the node's filter, per loop.
    pub fn kept_rows(&self) -> Option<f64> {
        self.kept.or(self.actual_rows)
    }

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
    /// Every finding as one line, for the agent.
    pub warnings: Vec<String>,
    pub cost: Option<f64>,
    pub planning_ms: Option<f64>,
    pub exec_ms: Option<f64>,
    /// The statement ran, so nodes carry actual rows and times.
    pub analyzed: bool,
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
    let mut plan = if t.starts_with('{') || t.starts_with('[') {
        let v: Value = serde_json::from_str(t).map_err(|e| format!("The plan is not valid JSON: {e}"))?;
        if d == Dialect::Postgres { parse_pg(&v)? } else { parse_my(&v)? }
    } else if d != Dialect::Postgres && t.contains("-> ") {
        parse_tree(t)
    } else {
        return Err("The plan is in a form this window cannot read. See the raw text.".into());
    };
    tree::link(&mut plan);
    tree::derive(&mut plan, d);
    for n in &mut plan.nodes {
        let read = |t: &str| if t.is_empty() { None } else { cond::parse(t, d) };
        (n.filter_expr, n.index_cond_expr, n.join_cond_expr) = (read(&n.filter), read(&n.index_cond), read(&n.join_cond));
    }
    plan.refind(d, None, false);
    Ok(plan)
}

impl Plan {
    /// Works the findings out again, with what the catalog said once it answers.
    pub fn refind(&mut self, d: Dialect, ev: Option<&Evidence>, pooler: bool) {
        self.findings = findings::find(self, d, ev, pooler);
        self.warnings = self.findings.iter().map(Finding::line).collect();
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

/// `1234567.0` as "1,234,567" and small estimates with a decimal, for warnings.
fn count(n: f64) -> String {
    let s = format!("{:.0}", n.max(0.0));
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

// PostgreSQL

fn parse_pg(v: &Value) -> Result<Plan, String> {
    let top = v.as_array().and_then(|a| a.first()).unwrap_or(v);
    let root = top.get("Plan").ok_or("The plan has no Plan node.")?;
    let mut plan = Plan { cost: num(root, "Total Cost"), planning_ms: num(top, "Planning Time"), exec_ms: num(top, "Execution Time"), ..Default::default() };
    plan.analyzed = plan.exec_ms.is_some();
    pg_node(root, 0, &mut plan.nodes);
    Ok(plan)
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
    node.rows = num(n, "Plan Rows");
    node.cost = num(n, "Total Cost");
    node.width = num(n, "Plan Width");
    node.actual_rows = num(n, "Actual Rows");
    node.actual_ms = num(n, "Actual Total Time");
    node.loops = num(n, "Actual Loops");
    node.never_ran = node.loops == Some(0.0);
    node.filter = text(n, "Filter").to_string();
    node.index_cond = [text(n, "Index Cond"), text(n, "Recheck Cond")].into_iter().find(|s| !s.is_empty()).unwrap_or_default().to_string();
    // A Join Filter stays a detail line of its own, so a join with both shows both.
    node.join_cond = ["Hash Cond", "Merge Cond"].into_iter().map(|k| text(n, k)).find(|s| !s.is_empty()).unwrap_or_default().to_string();
    node.parallel = flag(n, "Parallel Aware");
    node.workers = num(n, "Workers Launched").or_else(|| num(n, "Workers Planned"));
    for key in ["Index Cond", "Recheck Cond", "Filter", "Join Filter", "Hash Cond", "Merge Cond", "One-Time Filter"] {
        let t = text(n, key);
        if !t.is_empty() {
            node.detail.push(format!("{key}: {t}"));
        }
    }
    for key in ["Sort Key", "Group Key"] {
        let t = joined(n, key);
        if !t.is_empty() {
            node.detail.push(format!("{key}: {t}"));
        }
    }
    node.removed = num(n, "Rows Removed by Filter");
    if let Some(r) = node.removed {
        node.detail.push(format!("Rows removed by filter: {}", count(r)));
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
                out.push(PlanNode { depth, label: "Nested loop".into(), ..Default::default() });
                my_walk(v, depth + 1, out);
            }
            "nested_loop" | "query_block" | "materialized_from_subquery" | "materialized" | "read_sorted_file" => my_walk(v, depth, out),
            "ordering_operation" => my_op("Sort", "", v, depth, out, |_| {}),
            "grouping_operation" => my_op("Group", "", v, depth, out, |_| {}),
            "duplicates_removal" => my_op("Distinct", "", v, depth, out, |_| {}),
            "buffer_result" => my_op("Buffer result", "", v, depth, out, |_| {}),
            "windowing" | "window_functions_computation" => my_op("Window functions", "", v, depth, out, |_| {}),
            "union_result" => my_op("Union", text(v, "table_name"), v, depth, out, |_| {}),
            // MariaDB names the operation and always means it.
            "filesort" => my_op("Filesort", &joined(v, "sort_key"), v, depth, out, |n| n.filesort = true),
            "temporary_table" => my_op("Temporary table", "", v, depth, out, |n| n.temp_table = true),
            "block-nl-join" => my_op("Join buffer", text(v, "join_type"), v, depth, out, |n| n.join_buffer = true),
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
    node.filter = text(t, "attached_condition").to_string();
    node.index_cond = text(t, "index_condition").to_string();
    node.join_cond = joined(t, "ref");
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
            node.filter = c.to_string();
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

#[cfg(test)]
mod tests {
    use super::findings::Kind;
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
        assert_eq!(p.warnings, ["Full scan of orders o. Reads every row, then keeps those matching total."]);
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
        for name in ["pg18-join-analyze", "pg18-scan-analyze", "pg18-scan", "pg18-parallel-analyze", "pg18-spill-analyze", "pg18-limit-analyze", "pg18-cte-analyze", "pg18-subplan-analyze", "pg13-join-analyze"] {
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
        assert!(scan.filter_expr.is_some());
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
        assert_eq!(p.nodes[0].join_cond, "(o.cid = c.id)");
        assert!(p.nodes[0].detail.contains(&"Join Filter: (o.total > c.cap)".to_string()));
        assert!(p.nodes[0].join_cond_expr.is_some());
    }
}
