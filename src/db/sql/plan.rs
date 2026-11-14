//! Readable explain plans for the SQL engines. The server's JSON (or the MySQL tree text of EXPLAIN ANALYZE) becomes a flat
//! list of nodes with a depth, so the dialog can draw a tree without knowing which engine wrote it.

use super::{lex, Dialect, ResultSet};
use serde_json::Value;

/// A scan that reads at least this many rows is worth a warning. Below it a full scan is often the cheapest plan.
pub const BIG_SCAN: f64 = 1000.0;
const FILESORT_WARNING: &str = "Sorts rows outside an index (filesort). An index that matches the ORDER BY would avoid it.";
const TEMP_TABLE_WARNING: &str = "Builds a temporary table, which costs memory and may spill to disk.";

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
    /// Estimated rows.
    pub rows: Option<f64>,
    pub cost: Option<f64>,
    pub actual_rows: Option<f64>,
    pub actual_ms: Option<f64>,
    pub loops: Option<f64>,
    /// Filters, conditions and keys, one line each.
    pub detail: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub nodes: Vec<PlanNode>,
    /// Every warning of every node, once each, for the top of the dialog.
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
    let mut seen = Vec::new();
    for n in &plan.nodes {
        for w in &n.warnings {
            if !seen.contains(w) {
                seen.push(w.clone());
            }
        }
    }
    plan.warnings = seen;
    Ok(plan)
}

/// Pretty JSON for the Raw view; other text stays as the server wrote it.
pub fn raw_text(text: &str) -> String {
    serde_json::from_str::<Value>(text).ok().and_then(|v| serde_json::to_string_pretty(&v).ok()).unwrap_or_else(|| text.to_string())
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
    let rel = text(n, "Relation Name");
    let alias = text(n, "Alias");
    node.target = match (text(n, "Schema"), rel) {
        (_, "") => text(n, "CTE Name").to_string(),
        ("", r) => r.to_string(),
        (s, r) => format!("{s}.{r}"),
    };
    if !rel.is_empty() && !alias.is_empty() && alias != rel {
        node.target = format!("{} as {alias}", node.target);
    }
    node.index = text(n, "Index Name").to_string();
    node.access = if ty == "Seq Scan" { Access::Scan } else if ty.contains("Index") { Access::Index } else { Access::Other };
    node.rows = num(n, "Plan Rows");
    node.cost = num(n, "Total Cost");
    node.actual_rows = num(n, "Actual Rows");
    node.actual_ms = num(n, "Actual Total Time");
    node.loops = num(n, "Actual Loops");
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
    let removed = num(n, "Rows Removed by Filter");
    if let Some(r) = removed {
        node.detail.push(format!("Rows removed by filter: {}", count(r)));
    }
    let hit = num(n, "Shared Hit Blocks");
    let read = num(n, "Shared Read Blocks");
    if hit.is_some() || read.is_some() {
        node.detail.push(format!("Buffers: {} hit, {} read", count(hit.unwrap_or(0.0)), count(read.unwrap_or(0.0))));
    }
    let what = if node.target.is_empty() { ty.to_string() } else { format!("{ty} on {}", node.target) };
    // PostgreSQL does not say how many rows a sequential scan reads, only what it returns, so its cost stands in for the size.
    let filtered = n.get("Filter").is_some();
    let removed_many = removed.is_some_and(|r| r >= BIG_SCAN && r >= 10.0 * node.actual_rows.unwrap_or(0.0));
    if ty == "Seq Scan" && filtered && (node.cost.unwrap_or(0.0) >= BIG_SCAN || removed.unwrap_or(0.0) >= BIG_SCAN || node.rows.unwrap_or(0.0) >= BIG_SCAN) {
        node.warnings.push(format!("{what} reads every row and filters afterwards. An index on the filtered columns could avoid the scan."));
    } else if removed_many {
        node.warnings.push(format!("{what} discards {} rows to keep {}.", count(removed.unwrap_or(0.0)), count(node.actual_rows.unwrap_or(0.0))));
    }
    if text(n, "Sort Method").starts_with("external") {
        node.warnings.push(format!("The sort spilled to disk ({} kB). More work_mem would keep it in memory.", count(num(n, "Sort Space Used").unwrap_or(0.0))));
    }
    if num(n, "Hash Batches").is_some_and(|b| b > 1.0) {
        node.warnings.push("The hash spilled to disk in several batches. More work_mem would keep it in memory.".into());
    }
    if let (Some(est), Some(act)) = (node.rows, node.actual_rows) {
        let (lo, hi) = (est.min(act).max(1.0), est.max(act));
        if hi >= BIG_SCAN && hi / lo >= 10.0 {
            node.warnings.push(format!("{what} was estimated at {} rows and returned {}. The table statistics may be stale.", count(est), count(act)));
        }
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
            "nested_loop" | "query_block" | "materialized_from_subquery" | "materialized" | "read_sorted_file" => my_walk(v, depth, out),
            "ordering_operation" => my_op("Sort", "", v, depth, out, &[]),
            "grouping_operation" => my_op("Group", "", v, depth, out, &[]),
            "duplicates_removal" => my_op("Distinct", "", v, depth, out, &[]),
            "buffer_result" => my_op("Buffer result", "", v, depth, out, &[]),
            "windowing" | "window_functions_computation" => my_op("Window functions", "", v, depth, out, &[]),
            "union_result" => my_op("Union", text(v, "table_name"), v, depth, out, &[]),
            // MariaDB names the operation and always means it.
            "filesort" => my_op("Filesort", &joined(v, "sort_key"), v, depth, out, &[FILESORT_WARNING]),
            "temporary_table" => my_op("Temporary table", "", v, depth, out, &[TEMP_TABLE_WARNING]),
            "block-nl-join" => my_op("Join buffer", text(v, "join_type"), v, depth, out, &["Joins through a join buffer because no index serves the join condition."]),
            "query_specifications" => v.as_array().into_iter().flatten().for_each(|s| my_walk(s, depth, out)),
            "attached_subqueries" | "optimized_away_subqueries" | "subqueries" => {
                for s in v.as_array().into_iter().flatten() {
                    let mut node = PlanNode { depth, label: "Subquery".into(), ..Default::default() };
                    if flag(s, "dependent") {
                        node.warnings.push("A dependent subquery runs once for every row of the outer query.".into());
                    }
                    out.push(node);
                    my_walk(s, depth + 1, out);
                }
            }
            _ => {}
        }
    }
}

/// A sort, group, union or buffer step: a node of its own, flagged when the server says it used a filesort or a temporary table.
fn my_op(label: &str, target: &str, v: &Value, depth: usize, out: &mut Vec<PlanNode>, always: &[&str]) {
    let mut node = PlanNode { depth, label: label.into(), target: target.into(), ..Default::default() };
    node.warnings.extend(always.iter().map(|w| w.to_string()));
    if flag(v, "using_filesort") {
        node.warnings.push(FILESORT_WARNING.into());
    }
    if flag(v, "using_temporary_table") {
        node.warnings.push(TEMP_TABLE_WARNING.into());
    }
    out.push(node);
    my_walk(v, depth + 1, out);
}

fn my_table(t: &Value, depth: usize, out: &mut Vec<PlanNode>) {
    let name = text(t, "table_name");
    let access = text(t, "access_type");
    let key = text(t, "key");
    let mut node = PlanNode { depth, label: if access.is_empty() { "table".into() } else { access.into() }, target: name.into(), index: key.into(), ..Default::default() };
    node.access = match access {
        "ALL" => Access::Scan,
        // A full index scan reads every entry, but of a narrower structure; it keeps the neutral color.
        "index" | "" => Access::Other,
        _ if key.is_empty() => Access::Other,
        _ => Access::Index,
    };
    node.rows = num(t, "rows_examined_per_scan").or_else(|| num(t, "rows"));
    node.cost = t.get("cost_info").and_then(|c| num(c, "prefix_cost"));
    node.actual_rows = num(t, "r_rows");
    node.actual_ms = num(t, "r_total_time_ms").or_else(|| num(t, "r_table_time_ms"));
    node.loops = num(t, "r_loops");
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
    let big = node.rows.is_some_and(|r| r >= BIG_SCAN);
    let rows = count(node.rows.unwrap_or(0.0));
    if access == "ALL" && big {
        node.warnings.push(format!("Full table scan on {name}: about {rows} rows are read."));
    } else if access == "index" && big {
        node.warnings.push(format!("Full index scan on {name}: about {rows} index entries are read."));
    }
    if access == "ALL" && key.is_empty() && !joined(t, "possible_keys").is_empty() {
        node.warnings.push(format!("{name} has usable indexes ({}) but the server chose none.", joined(t, "possible_keys")));
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
        let (est_part, act_part) = match body.find("(actual") {
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
            node.actual_rows = after(a, "rows=");
            node.loops = after(a, "loops=");
            node.actual_ms = a.find("time=").and_then(|i| a[i..].find("..").map(|j| &a[i + j + 2..])).and_then(|s| after(s, ""));
        }
        let l = node.label.as_str();
        node.access = if l.starts_with("Table scan") { Access::Scan } else if l.contains("index") || l.starts_with("Index") { Access::Index } else { Access::Other };
        if node.access == Access::Scan && node.rows.is_some_and(|r| r >= BIG_SCAN) {
            node.warnings.push(format!("{}: about {} rows are read.", node.label, count(node.rows.unwrap_or(0.0))));
        }
        if l.starts_with("Sort") {
            node.warnings.push(FILESORT_WARNING.into());
        }
        if l.contains("temporary table") || l.contains("Temporary table") {
            node.warnings.push(TEMP_TABLE_WARNING.into());
        }
        if plan.cost.is_none() {
            plan.cost = node.cost;
        }
        plan.nodes.push(node);
    }
    plan
}

/// The number after `key` in `s`. Stops at ".." so a range like 0.03..0.04 reads as 0.03.
fn after(s: &str, key: &str) -> Option<f64> {
    let rest = &s[s.find(key)? + key.len()..];
    let b = rest.as_bytes();
    let mut end = 0;
    while end < b.len() && (b[end].is_ascii_digit() || (b[end] == b'.' && b.get(end + 1).is_some_and(u8::is_ascii_digit)) || (end == 0 && b[end] == b'-')) {
        end += 1;
    }
    rest[..end].parse().ok()
}

#[cfg(test)]
mod tests {
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
        assert_eq!(p.warnings.len(), 1);
        assert!(p.warnings[0].starts_with("Seq Scan on orders as o reads every row"));
        let ix = &p.nodes[3];
        assert_eq!((ix.target.as_str(), ix.index.as_str(), ix.access), ("public.customers as c", "customers_pkey", Access::Index));
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
        assert!(p.warnings.iter().any(|w| w.contains("spilled to disk (4,096 kB)")));
        assert!(p.warnings.iter().any(|w| w.contains("estimated at 10 rows and returned 5,000")));
        let seq = &p.nodes[1];
        assert!(seq.detail.contains(&"Buffers: 10 hit, 2 read".to_string()));
        assert!(seq.warnings.iter().any(|w| w.contains("reads every row")));
    }

    #[test]
    fn postgres_small_seq_scan_is_not_flagged() {
        let json = r#"[{"Plan":{"Node Type":"Seq Scan","Relation Name":"tiny","Alias":"tiny","Total Cost":1.05,"Plan Rows":5,"Filter":"(a = 1)"}}]"#;
        assert!(parse(Dialect::Postgres, json).unwrap().warnings.is_empty());
    }

    const MYSQL: &str = r#"{"query_block":{"select_id":1,"cost_info":{"query_cost":"1250.40"},"ordering_operation":{"using_filesort":true,"nested_loop":[
        {"table":{"table_name":"o","access_type":"ALL","possible_keys":["idx_cust"],"rows_examined_per_scan":9000,"rows_produced_per_join":900,"filtered":"10.00","cost_info":{"read_cost":"900.00","eval_cost":"90.00","prefix_cost":"990.00","data_read_per_join":"1M"},"attached_condition":"(`o`.`total` > 100)"}},
        {"table":{"table_name":"c","access_type":"eq_ref","possible_keys":["PRIMARY"],"key":"PRIMARY","used_key_parts":["id"],"key_length":"4","ref":["shop.o.customer_id"],"rows_examined_per_scan":1,"rows_produced_per_join":900,"filtered":"100.00","using_index":true,"cost_info":{"prefix_cost":"1250.40"}}}]}}}"#;

    #[test]
    fn mysql_json_plan() {
        let p = parse(Dialect::MySql, MYSQL).unwrap();
        assert_eq!(p.nodes.iter().map(|n| (n.depth, n.label.as_str(), n.target.as_str())).collect::<Vec<_>>(), [(0, "Sort", ""), (1, "ALL", "o"), (1, "eq_ref", "c")]);
        assert_eq!(p.cost, Some(1250.4));
        assert_eq!(p.nodes[1].access, Access::Scan);
        assert_eq!(p.nodes[1].cost, Some(990.0));
        assert_eq!((p.nodes[2].access, p.nodes[2].index.as_str()), (Access::Index, "PRIMARY"));
        assert!(p.nodes[2].detail.contains(&"Lookup: shop.o.customer_id".to_string()));
        assert!(p.warnings.iter().any(|w| w.starts_with("Full table scan on o: about 9,000 rows")));
        assert!(p.warnings.iter().any(|w| w.contains("filesort")));
        assert!(p.warnings.iter().any(|w| w.contains("usable indexes (idx_cust)")));
    }

    #[test]
    fn mysql_temporary_table_union_and_subquery() {
        let json = r#"{"query_block":{"union_result":{"using_temporary_table":true,"table_name":"<union1,2>","access_type":"ALL","query_specifications":[
            {"dependent":false,"cacheable":true,"query_block":{"select_id":1,"table":{"table_name":"a","access_type":"ref","key":"k","rows_examined_per_scan":3}}},
            {"dependent":true,"cacheable":false,"query_block":{"select_id":2,"table":{"table_name":"b","access_type":"const","key":"PRIMARY","rows_examined_per_scan":1}}}]}}}"#;
        let p = parse(Dialect::MySql, json).unwrap();
        assert_eq!(p.nodes.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(), ["Union", "ref", "const"]);
        assert!(p.warnings.iter().any(|w| w.contains("temporary table")));
        let sub = r#"{"query_block":{"select_id":1,"table":{"table_name":"t","access_type":"ALL","rows_examined_per_scan":5,"attached_subqueries":[{"dependent":true,"cacheable":false,"query_block":{"select_id":2,"table":{"table_name":"u","access_type":"ref","key":"k","rows_examined_per_scan":1}}}]}}}"#;
        let p = parse(Dialect::MySql, sub).unwrap();
        assert_eq!(p.nodes.iter().map(|n| (n.depth, n.label.as_str())).collect::<Vec<_>>(), [(0, "ALL"), (1, "Subquery"), (2, "ref")]);
        assert!(p.warnings.iter().any(|w| w.contains("dependent subquery")));
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
        assert!(p.warnings.iter().any(|w| w.starts_with("Full table scan on t: about 20,000 rows")));
    }

    #[test]
    fn mariadb_nested_loop_with_join_buffer() {
        let json = r#"{"query_block":{"select_id":1,"nested_loop":[{"table":{"table_name":"a","access_type":"range","key":"k","rows":10}},{"block-nl-join":{"table":{"table_name":"b","access_type":"ALL","rows":50},"buffer_type":"flat","join_type":"BNL"}}]}}"#;
        let p = parse(Dialect::MariaDb, json).unwrap();
        assert_eq!(p.nodes.iter().map(|n| (n.depth, n.label.as_str())).collect::<Vec<_>>(), [(0, "range"), (0, "Join buffer"), (1, "ALL")]);
        assert!(p.warnings.iter().any(|w| w.contains("join buffer")));
    }

    #[test]
    fn mysql_analyze_tree_text() {
        let text = "-> Sort: o.total DESC  (cost=1.5 rows=10) (actual time=0.5..0.6 rows=10 loops=1)\n    -> Table scan on o  (cost=1.25 rows=5000) (actual time=0.03..0.40 rows=5000 loops=1)\n        -> Index lookup on c using PRIMARY (id=o.cid)  (cost=0.35 rows=1) (never executed)\n";
        let p = parse(Dialect::MySql, text).unwrap();
        assert!(p.analyzed);
        assert_eq!(p.nodes.iter().map(|n| (n.depth, n.label.as_str())).collect::<Vec<_>>(), [(0, "Sort: o.total DESC"), (1, "Table scan on o"), (2, "Index lookup on c using PRIMARY (id=o.cid)")]);
        let scan = &p.nodes[1];
        assert_eq!((scan.cost, scan.rows, scan.actual_rows, scan.actual_ms, scan.loops), (Some(1.25), Some(5000.0), Some(5000.0), Some(0.4), Some(1.0)));
        assert_eq!(scan.access, Access::Scan);
        assert_eq!(p.nodes[2].access, Access::Index);
        assert!(p.warnings.iter().any(|w| w.starts_with("Table scan on o: about 5,000 rows")));
        assert!(p.warnings.iter().any(|w| w.contains("filesort")));
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
}
