//! A readable summary of explain output.

use super::*;

#[derive(Default)]
pub struct ExplainRow {
    pub depth: usize,
    pub stage: String,
    pub index: String,
    pub keys: Option<i64>,
    pub docs: Option<i64>,
    pub returned: Option<i64>,
    pub ms: Option<i64>,
}

#[derive(Default)]
pub struct ExplainSummary {
    pub rows: Vec<ExplainRow>,
    pub keys: i64,
    pub docs: i64,
    pub returned: i64,
    pub ms: i64,
    pub warnings: Vec<String>,
}

pub(crate) fn int(d: &Document, k: &str) -> Option<i64> {
    match d.get(k)? {
        Bson::Int32(n) => Some(*n as i64),
        Bson::Int64(n) => Some(*n),
        Bson::Double(n) => Some(*n as i64),
        _ => None,
    }
}

/// The parts of an explain reply that carry `executionStats`: the reply itself, the `$cursor`
/// stage of an aggregate, or one per shard.
pub(crate) fn explain_bodies<'a>(d: &'a Document, out: &mut Vec<&'a Document>) {
    if d.contains_key("executionStats") {
        out.push(d);
        return;
    }
    if let Ok(shards) = d.get_document("shards") {
        shards.values().filter_map(Bson::as_document).for_each(|s| explain_bodies(s, out));
    }
    if let Ok(stages) = d.get_array("stages") {
        stages.iter().filter_map(Bson::as_document).filter_map(|s| s.get_document("$cursor").ok()).for_each(|c| explain_bodies(c, out));
    }
}

pub(crate) fn explain_walk(n: &Document, depth: usize, rows: &mut Vec<ExplainRow>) {
    let stage = n.get_str("stage").unwrap_or("?");
    // The slot-based engine reports reads on its fetch and scan stages instead of docsExamined.
    let docs = int(n, "docsExamined").or_else(|| if matches!(stage, "fetch" | "scan") { int(n, "numReads") } else { None });
    rows.push(ExplainRow {
        depth,
        stage: stage.to_string(),
        index: n.get_str("indexName").unwrap_or_default().to_string(),
        keys: int(n, "keysExamined"),
        docs,
        returned: int(n, "nReturned"),
        ms: int(n, "executionTimeMillisEstimate"),
    });
    for (k, v) in n {
        match v {
            Bson::Document(c) if k.ends_with("Stage") => explain_walk(c, depth + 1, rows),
            Bson::Array(a) if k == "inputStages" => a.iter().filter_map(Bson::as_document).for_each(|c| explain_walk(c, depth + 1, rows)),
            // A sharded find lists each shard's own plan.
            Bson::Array(a) if k == "shards" => a.iter().filter_map(Bson::as_document).filter_map(|s| s.get_document("executionStages").ok()).for_each(|c| explain_walk(c, depth + 1, rows)),
            _ => {}
        }
    }
}

/// Flattens an explain reply (find or aggregate, classic or slot-based engine, sharded or not)
/// into plan stages, totals and warnings.
pub fn explain_summary(d: &Document) -> ExplainSummary {
    let mut bodies = Vec::new();
    explain_bodies(d, &mut bodies);
    let mut s = ExplainSummary::default();
    for b in bodies {
        let Ok(st) = b.get_document("executionStats") else { continue };
        s.keys += int(st, "totalKeysExamined").unwrap_or(0);
        s.docs += int(st, "totalDocsExamined").unwrap_or(0);
        s.returned += int(st, "nReturned").unwrap_or(0);
        s.ms += int(st, "executionTimeMillis").unwrap_or(0);
        if let Ok(root) = st.get_document("executionStages") {
            explain_walk(root, 0, &mut s.rows);
        }
    }
    let has = |names: &[&str]| s.rows.iter().any(|r| names.contains(&r.stage.to_lowercase().as_str()));
    if has(&["collscan", "scan"]) {
        s.warnings.push("COLLSCAN: every document is read. An index on the filtered fields would avoid it.".into());
    }
    if has(&["sort"]) {
        s.warnings.push("SORT in memory: no index provides the order. An index ending in the sort fields avoids it.".into());
    }
    let examined = s.docs.max(s.keys);
    // Grouping and counting shrink the output by design, so few results there are not a sign of a weak filter.
    if examined >= 1000 && examined > s.returned * 10 && !has(&["group", "count", "count_scan"]) {
        s.warnings.push(format!("Examined {examined} to return {}. The filter is not selective for the indexes in use.", s.returned));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explain_summary_find_with_index() {
        let d = parse_doc(r#"{queryPlanner: {winningPlan: {stage: "LIMIT"}}, executionStats: {nReturned: 50, executionTimeMillis: 0, totalKeysExamined: 50, totalDocsExamined: 50,
            executionStages: {stage: "LIMIT", nReturned: 50, executionTimeMillisEstimate: 0, inputStage: {stage: "FETCH", nReturned: 50, executionTimeMillisEstimate: 0, docsExamined: 50,
            inputStage: {stage: "IXSCAN", nReturned: 50, executionTimeMillisEstimate: 0, indexName: "status_1_createdAt_-1", keysExamined: 50}}}}}"#).unwrap();
        let s = explain_summary(&d);
        let stages: Vec<_> = s.rows.iter().map(|r| (r.depth, r.stage.as_str())).collect();
        assert_eq!(stages, [(0, "LIMIT"), (1, "FETCH"), (2, "IXSCAN")]);
        assert_eq!(s.rows[2].index, "status_1_createdAt_-1");
        assert_eq!((s.rows[2].keys, s.rows[1].docs, s.keys, s.docs, s.returned), (Some(50), Some(50), 50, 50, 50));
        assert!(s.warnings.is_empty());
    }

    #[test]
    fn explain_summary_collscan_and_ratio() {
        let d = parse_doc(r#"{executionStats: {nReturned: 3, executionTimeMillis: 41, totalKeysExamined: 0, totalDocsExamined: 20000,
            executionStages: {stage: "COLLSCAN", nReturned: 3, executionTimeMillisEstimate: 40, docsExamined: 20000}}}"#).unwrap();
        let s = explain_summary(&d);
        assert_eq!((s.rows.len(), s.ms), (1, 41));
        assert_eq!(s.warnings.len(), 2);
        assert!(s.warnings[0].starts_with("COLLSCAN"));
    }

    #[test]
    fn explain_summary_slot_based_aggregate() {
        // MongoDB 8 reports lowercase slot-based stages for aggregates, with reads on fetch.
        let d = parse_doc(r#"{explainVersion: "2", queryPlanner: {winningPlan: {queryPlan: {stage: "GROUP"}}},
            executionStats: {nReturned: 125, executionTimeMillis: 8, totalKeysExamined: 5000, totalDocsExamined: 5000,
            executionStages: {stage: "project", nReturned: 125, executionTimeMillisEstimate: 6, inputStage: {stage: "group", nReturned: 125, executionTimeMillisEstimate: 6,
            inputStage: {stage: "fetch", nReturned: 5000, executionTimeMillisEstimate: 6, numReads: 5000,
            inputStage: {stage: "ixseek", nReturned: 5000, executionTimeMillisEstimate: 0, indexName: "status_1_createdAt_-1", keysExamined: 5000}}}}}}"#).unwrap();
        let s = explain_summary(&d);
        let stages: Vec<_> = s.rows.iter().map(|r| r.stage.as_str()).collect();
        assert_eq!(stages, ["project", "group", "fetch", "ixseek"]);
        assert_eq!((s.rows[2].docs, s.rows[3].keys, s.rows[3].index.as_str()), (Some(5000), Some(5000), "status_1_createdAt_-1"));
        // A $group returns few results by design, so no selectivity warning.
        assert!(s.warnings.is_empty());
    }

    #[test]
    fn explain_summary_cursor_wrapper_and_shards() {
        let cursor = r#"{stages: [{$cursor: {executionStats: {nReturned: 2, executionTimeMillis: 1, totalKeysExamined: 2, totalDocsExamined: 2,
            executionStages: {stage: "FETCH", nReturned: 2, docsExamined: 2, inputStage: {stage: "IXSCAN", nReturned: 2, indexName: "a_1", keysExamined: 2}}}}}, {$group: {}}]}"#;
        let one = explain_summary(&parse_doc(cursor).unwrap());
        assert_eq!(one.rows.len(), 2);
        let c = parse_doc(cursor).unwrap();
        let sharded = doc! { "shards": { "a": c.clone(), "b": c } };
        let s = explain_summary(&sharded);
        assert_eq!((s.rows.len(), s.keys, s.docs, s.returned, s.ms), (4, 4, 4, 4, 2));
    }

    #[test]
    fn explain_summary_sharded_find_and_sort() {
        let d = parse_doc(r#"{executionStats: {nReturned: 4, totalDocsExamined: 4, executionStages: {stage: "SHARD_MERGE", nReturned: 4, shards: [
            {shardName: "s1", executionStages: {stage: "SORT", nReturned: 2, inputStage: {stage: "COLLSCAN", nReturned: 2, docsExamined: 2}}},
            {shardName: "s2", executionStages: {stage: "FETCH", nReturned: 2, inputStage: {stage: "SORT_KEY_GENERATOR", nReturned: 2}}}]}}}"#).unwrap();
        let s = explain_summary(&d);
        assert_eq!(s.rows.len(), 5);
        assert_eq!(s.warnings.len(), 2);
        assert!(explain_summary(&doc! { "ok": 1 }).rows.is_empty());
    }
}
