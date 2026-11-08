//! Pure analysis of fetched documents: compare, relations, schema stats and index advice.

use super::*;

/// Two collections compared by `_id`.
#[derive(Default)]
pub struct Compared {
    pub only_src: Vec<Document>,
    pub only_dst: Vec<Document>,
    /// (source, target) pairs with the same `_id` and different content.
    pub changed: Vec<(Document, Document)>,
    /// One row per differing field of `changed`: `_id`, `field`, and `target`/`source` when that side has it.
    pub diffs: Vec<Document>,
    pub same: usize,
}

/// Columns of `Compared::diffs`, in the order the table shows them.
pub const DIFF_FIELDS: [&str; 4] = ["_id", "field", "target", "source"];

/// The original skips collections above this many documents (maxDocuments, renderer line 132512).
pub const COMPARE_MAX: u64 = 1_000_000;

/// Ids are copied and deleted in chunks of this size (chunkSize, renderer line 132513).
pub const SYNC_CHUNK: usize = 1000;

/// Field order is ignored, as the original compares document hashes of sorted fields.
pub fn compare_docs(src: Vec<Document>, dst: Vec<Document>) -> Compared {
    let mut other: std::collections::HashMap<String, Document> = dst.into_iter().map(|d| (compact(d.get("_id").unwrap_or(&Bson::Null)), d)).collect();
    let mut r = Compared::default();
    for d in src {
        match other.remove(&compact(d.get("_id").unwrap_or(&Bson::Null))) {
            None => r.only_src.push(d),
            Some(o) if o == d => r.same += 1,
            Some(o) => r.changed.push((d, o)),
        }
    }
    r.only_dst = other.into_values().collect();
    r.only_dst.sort_by_key(|d| compact(d.get("_id").unwrap_or(&Bson::Null)));
    // Built here, on the worker, because diffing every changed document can take a while.
    for (src, dst) in &r.changed {
        let id = src.get("_id").cloned().unwrap_or(Bson::Null);
        for (path, old, new) in diff(dst, src) {
            let mut d = doc! { "_id": id.clone(), "field": path };
            // A side without the field leaves the cell empty rather than showing null.
            if let Some(v) = old {
                d.insert("target", v);
            }
            if let Some(v) = new {
                d.insert("source", v);
            }
            r.diffs.push(d);
        }
    }
    r
}

/// First top-level text field that is not itself an id, like the original's getFirstStringField
/// (renderer line 105602); `_id` when there is none.
pub fn preview_field(d: &Document) -> String {
    let id_like = |s: &str| s.len() == 24 && s.chars().all(|c| c.is_ascii_hexdigit());
    d.iter().find(|(k, v)| *k != "_id" && matches!(v, Bson::String(s) if !id_like(s))).map(|(k, _)| k.clone()).unwrap_or_else(|| "_id".into())
}

/// Every value at `path`, looking inside arrays at any level, for looking up referenced documents.
pub fn values_at<'a>(docs: impl IntoIterator<Item = &'a RawDocument>, path: &str) -> Vec<Bson> {
    // Read from the BSON bytes, so a page of large documents is not decoded to find a few ids.
    fn walk(v: RawBsonRef, segs: &[&str], out: &mut Vec<Bson>) {
        match (v, segs.split_first()) {
            (RawBsonRef::Array(a), _) => a.into_iter().flatten().for_each(|x| walk(x, segs, out)),
            (RawBsonRef::Document(d), Some((k, rest))) => {
                if let Ok(Some(x)) = d.get(k) {
                    walk(x, rest, out);
                }
            }
            (v, None) => out.extend(Bson::try_from(v).ok()),
            _ => {}
        }
    }
    let segs: Vec<&str> = path.split('.').collect();
    let mut out = Vec::new();
    for d in docs {
        walk(RawBsonRef::Document(d), &segs, &mut out);
    }
    out.sort_by_cached_key(compact);
    out.dedup();
    out
}

/// Fields that look like references, with sample ids (the original's _analyzeDocument, renderer
/// line 105562): ObjectIds, or alphanumeric strings as long as this collection's own string ids.
/// Arrays are looked into through their first element; objects with 100 fields or more are skipped.
pub fn proto_relations(docs: &[Document], id_len: usize) -> Vec<(String, Vec<Bson>)> {
    fn is_key(v: &Bson, id_len: usize) -> bool {
        match v {
            Bson::ObjectId(_) => true,
            Bson::String(s) => id_len > 0 && s.len() == id_len && s.chars().all(|c| c.is_ascii_alphanumeric()),
            _ => false,
        }
    }
    fn walk(d: &Document, prefix: &str, id_len: usize, out: &mut Vec<(String, Vec<Bson>)>) {
        for (k, v) in d {
            let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
            match v {
                Bson::Array(a) => match a.first() {
                    Some(f) if is_key(f, id_len) => out.push((path, a.clone())),
                    Some(Bson::Document(x)) => walk(x, &path, id_len, out),
                    _ => {}
                },
                v if is_key(v, id_len) => out.push((path, vec![v.clone()])),
                Bson::Document(x) if x.len() < 100 => walk(x, &path, id_len, out),
                _ => {}
            }
        }
    }
    let mut out: Vec<(String, Vec<Bson>)> = Vec::new();
    for d in docs {
        let mut d = d.clone();
        d.remove("_id");
        let mut found = Vec::new();
        walk(&d, "", id_len, &mut found);
        for (p, ids) in found {
            if !out.iter().any(|(q, _)| *q == p) {
                out.push((p, ids));
            }
        }
    }
    out
}

/// One field path across the analysed documents.
#[derive(Clone, Debug)]
pub struct FieldStat {
    pub path: String,
    pub depth: usize,
    /// Documents that have this field at least once.
    pub docs: usize,
    /// Type name and how many times it was seen.
    pub types: Vec<(&'static str, usize)>,
    pub examples: Vec<String>,
}

/// Field paths in first-seen order, how many documents have each, their types and a few
/// examples. Objects inside arrays are merged under the array's path.
pub fn analyze_schema(docs: &[Document]) -> Vec<FieldStat> {
    use std::collections::{HashMap, HashSet};
    fn walk(d: &Document, prefix: &str, depth: usize, stats: &mut Vec<FieldStat>, at: &mut HashMap<String, usize>, seen: &mut HashSet<usize>) {
        for (k, v) in d {
            let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
            let i = *at.entry(path.clone()).or_insert_with(|| {
                stats.push(FieldStat { path: path.clone(), depth, docs: 0, types: Vec::new(), examples: Vec::new() });
                stats.len() - 1
            });
            if seen.insert(i) {
                stats[i].docs += 1;
            }
            let tn = type_name(v);
            match stats[i].types.iter_mut().find(|(t, _)| *t == tn) {
                Some(t) => t.1 += 1,
                None => stats[i].types.push((tn, 1)),
            }
            if !matches!(v, Bson::Document(_) | Bson::Array(_)) && stats[i].examples.len() < 3 {
                let shown = match v {
                    Bson::ObjectId(o) => o.to_hex(),
                    Bson::DateTime(d) => d.try_to_rfc3339_string().unwrap_or_default(),
                    other => compact(other),
                };
                let ex: String = shown.chars().take(40).collect();
                if !stats[i].examples.contains(&ex) {
                    stats[i].examples.push(ex);
                }
            }
            match v {
                Bson::Document(c) => walk(c, &path, depth + 1, stats, at, seen),
                Bson::Array(a) => {
                    for x in a.iter().take(50) {
                        if let Bson::Document(c) = x {
                            walk(c, &path, depth + 1, stats, at, seen);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let (mut stats, mut at) = (Vec::new(), HashMap::new());
    for d in docs {
        let mut seen = HashSet::new();
        walk(d, "", 0, &mut stats, &mut at, &mut seen);
    }
    for s in &mut stats {
        s.types.sort_by(|a, b| b.1.cmp(&a.1));
    }
    stats
}

/// A `$jsonSchema` validator describing the analysed documents. A field is required when every
/// sampled document has it. Children are nested only under object values, because `properties`
/// does not constrain array elements.
pub fn schema_from_stats(stats: &[FieldStat], sampled: usize) -> Document {
    fn bson_type(t: &str) -> Option<&'static str> {
        Some(match t {
            "double" => "double",
            "string" => "string",
            "object" => "object",
            "array" => "array",
            "bool" => "bool",
            "null" => "null",
            "int" => "int",
            "long" => "long",
            "objectId" => "objectId",
            "date" => "date",
            "decimal" => "decimal",
            "uuid" | "binary" => "binData",
            "regex" => "regex",
            "timestamp" => "timestamp",
            _ => return None,
        })
    }
    fn object(stats: &[FieldStat], prefix: &str, depth: usize, present: usize) -> Document {
        let (mut props, mut required) = (Document::new(), Vec::new());
        for f in stats.iter().filter(|f| f.depth == depth && f.path.strip_prefix(prefix).is_some_and(|r| !r.contains('.'))) {
            let name = &f.path[prefix.len()..];
            let mut types: Vec<&str> = f.types.iter().filter_map(|(t, _)| bson_type(t)).collect();
            let all_mapped = types.len() == f.types.len();
            types.sort_unstable();
            types.dedup();
            let mut schema = Document::new();
            // An unmappable type leaves the field unconstrained rather than rejecting its documents.
            if all_mapped {
                match types.as_slice() {
                    [one] => schema.insert("bsonType", *one),
                    many => schema.insert("bsonType", many.iter().map(|t| Bson::String(t.to_string())).collect::<Vec<_>>()),
                };
            }
            // When the path is also an array its children are merged from elements, so their
            // counts say nothing about presence inside each object.
            if f.types.iter().any(|(t, _)| *t == "object") {
                let plain = !f.types.iter().any(|(t, _)| *t == "array");
                schema.extend(object(stats, &format!("{}.", f.path), depth + 1, if plain { f.docs } else { 0 }));
            }
            if present > 0 && f.docs == present {
                required.push(Bson::String(name.to_string()));
            }
            props.insert(name, schema);
        }
        let mut out = Document::new();
        if !props.is_empty() {
            out.insert("properties", props);
        }
        // MongoDB rejects an empty required array.
        if !required.is_empty() {
            out.insert("required", required);
        }
        out
    }
    let mut schema = doc! { "bsonType": "object" };
    schema.extend(object(stats, "", 0, sampled));
    doc! { "$jsonSchema": schema }
}

pub struct IndexStat {
    pub name: String,
    pub ops: i64,
    /// When the counter started, in ms since the epoch.
    pub since: Option<i64>,
    pub hidden: bool,
    pub size: Option<usize>,
}

pub struct IndexInfo {
    pub stats: Vec<IndexStat>,
    /// Why usage counts are missing: $indexStats was refused, as Atlas does on admin's system collections.
    pub stats_err: Option<String>,
    /// Profiler level: 0 off, 1 slow operations, 2 all.
    pub level: i32,
    pub slowms: i64,
    pub shapes: Vec<Shape>,
}

/// Profiled queries that share filter fields and sort.
pub struct Shape {
    pub filter: Document,
    pub sort: Document,
    pub count: usize,
    pub avg_ms: f64,
    pub suggestion: Document,
}

/// Unused for a week; a younger counter proves nothing because it resets on restart.
pub fn is_unused(ops: i64, since_ms: Option<i64>, now_ms: i64) -> bool {
    ops == 0 && since_ms.is_some_and(|s| now_ms - s > 7 * 24 * 3600 * 1000)
}

/// Compound index for a query in the usual order: equality fields, then sort fields, then ranges.
pub fn suggest_index(filter: &Document, sort: &Document) -> Document {
    let mut eq = Vec::new();
    let mut range = Vec::new();
    for (k, v) in filter.iter().filter(|(k, _)| !k.starts_with('$')) {
        let is_eq = match v {
            Bson::Document(d) if d.keys().any(|k| k.starts_with('$')) => d.keys().any(|k| matches!(k.as_str(), "$eq" | "$in" | "$elemMatch")),
            _ => true,
        };
        if is_eq { eq.push(k) } else { range.push(k) }
    }
    let mut out = Document::new();
    for k in eq {
        out.insert(k, 1);
    }
    for (k, v) in sort {
        let dir = match v {
            Bson::Int32(n) if *n < 0 => -1,
            Bson::Int64(n) if *n < 0 => -1,
            Bson::Double(n) if *n < 0.0 => -1,
            _ => 1,
        };
        if !out.contains_key(k) {
            out.insert(k, dir);
        }
    }
    for k in range {
        if !out.contains_key(k) {
            out.insert(k, 1);
        }
    }
    out
}

/// Groups profile entries (`filter`, `sort`, `millis`) by query shape, busiest first.
pub fn group_profile(entries: &[Document]) -> Vec<Shape> {
    let mut groups: Vec<(String, Shape, f64)> = Vec::new();
    for en in entries {
        let filter = en.get_document("filter").cloned().unwrap_or_default();
        let sort = en.get_document("sort").cloned().unwrap_or_default();
        let suggestion = suggest_index(&filter, &sort);
        // A bare full scan has no field to index.
        if suggestion.is_empty() {
            continue;
        }
        let ms = match en.get("millis") {
            Some(Bson::Int32(n)) => *n as f64,
            Some(Bson::Int64(n)) => *n as f64,
            _ => 0.0,
        };
        let mut fields: Vec<&String> = filter.keys().collect();
        fields.sort();
        let key = format!("{fields:?}{}", compact(&Bson::Document(sort.clone())));
        match groups.iter_mut().find(|g| g.0 == key) {
            Some(g) => {
                g.1.count += 1;
                g.2 += ms;
            }
            None => groups.push((key, Shape { filter, sort, count: 1, avg_ms: 0.0, suggestion }, ms)),
        }
    }
    let mut out: Vec<Shape> = groups
        .into_iter()
        .map(|(_, mut s, total)| {
            s.avg_ms = total / s.count as f64;
            s
        })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then(b.avg_ms.total_cmp(&a.avg_ms)));
    out
}

/// A column data action from the table header.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ColOp {
    Copy,
    CopyDistinct,
    CopyDistinctJs,
    CountDistinct,
    Sum,
    Avg,
    Median,
    Range,
    Group,
}

impl ColOp {
    /// In menu order.
    pub const ALL: [ColOp; 9] = [ColOp::Copy, ColOp::CopyDistinct, ColOp::CopyDistinctJs, ColOp::CountDistinct, ColOp::Sum, ColOp::Avg, ColOp::Median, ColOp::Range, ColOp::Group];

    pub fn label(self) -> &'static str {
        match self {
            ColOp::Copy => "Copy values",
            ColOp::CopyDistinct => "Copy distinct values",
            ColOp::CopyDistinctJs => "Copy distinct values as JS array",
            ColOp::CountDistinct => "Count distinct values",
            ColOp::Sum => "Sum of values",
            ColOp::Avg => "Average of values",
            ColOp::Median => "Median of values",
            ColOp::Range => "Range of values",
            ColOp::Group => "Group by and count values",
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum ColOut {
    /// Text for the clipboard and how many values it holds.
    Copy(String, usize),
    Notice(String),
    /// `{ _id, count }` rows, most frequent first.
    Groups(Vec<Document>),
}

/// Numeric strings count too, since imported data often stores numbers as text.
/// Leading zeros mark codes like "007", which import also keeps as text.
pub(crate) fn num(v: &Bson) -> Option<f64> {
    match v {
        Bson::Int32(n) => Some(*n as f64),
        Bson::Int64(n) => Some(*n as f64),
        Bson::Double(f) => Some(*f),
        Bson::Decimal128(d) => d.to_string().parse().ok(),
        Bson::String(s) => {
            let t = s.trim();
            let code = t.len() > 1 && t.starts_with('0') && !t.starts_with("0.");
            if code { None } else { t.parse().ok().filter(|f: &f64| f.is_finite()) }
        }
        _ => None,
    }
}

fn date_ms(v: &Bson) -> Option<i64> {
    match v {
        Bson::DateTime(t) => Some(t.timestamp_millis()),
        Bson::Timestamp(t) => Some(t.time as i64 * 1000),
        _ => None,
    }
}

fn fmt_date(ms: i64) -> String {
    mongodb::bson::DateTime::from_millis(ms).try_to_rfc3339_string().unwrap_or_else(|_| ms.to_string())
}

fn fmt_span(ms: i64) -> String {
    if ms < 1000 {
        return format!("{ms} ms");
    }
    let s = ms / 1000;
    let parts = [(s / 86_400, "d"), (s / 3600 % 24, "h"), (s / 60 % 60, "m"), (s % 60, "s")];
    parts.iter().filter(|p| p.0 > 0).map(|(n, u)| format!("{n}{u}")).collect::<Vec<_>>().join(" ")
}

/// Sum, average, median and range over numbers or dates, whichever the column holds more of.
fn stat_result(op: ColOp, values: &[Bson]) -> String {
    let total = values.len();
    // Each epoch milli is exact as f64 (below 2^53), so dates share the number path.
    let dates: Vec<f64> = values.iter().filter_map(date_ms).map(|ms| ms as f64).collect();
    let nums: Vec<f64> = values.iter().filter_map(num).collect();
    let is_date = dates.len() > nums.len();
    let (mut xs, kind) = if is_date { (dates, "dates") } else { (nums, "numbers") };
    let n = xs.len();
    if n == 0 {
        return format!("None of the {total} values are numbers or dates");
    }
    let show = |x: f64| if is_date { fmt_date(x as i64) } else { x.to_string() };
    let body = match op {
        ColOp::Sum if is_date => "Dates cannot be summed, try average, median or range".into(),
        ColOp::Sum => show(xs.iter().sum()),
        // Summed exactly: 100k epoch millis add up past 2^53, where f64 starts rounding.
        ColOp::Avg if is_date => show((xs.iter().map(|&x| x as i128).sum::<i128>() / n as i128) as f64),
        ColOp::Avg => show(xs.iter().sum::<f64>() / n as f64),
        ColOp::Median => {
            xs.sort_by(f64::total_cmp);
            show(if n % 2 == 1 { xs[n / 2] } else { (xs[n / 2 - 1] + xs[n / 2]) / 2.0 })
        }
        ColOp::Range => {
            let (lo, hi) = xs.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &x| (lo.min(x), hi.max(x)));
            let span = if is_date { fmt_span((hi - lo) as i64) } else { (hi - lo).to_string() };
            format!("{} to {}\nSpan {span}", show(lo), show(hi))
        }
        _ => unreachable!("not a stat op"),
    };
    format!("{body}\n\n{n} of {total} values are {kind}")
}

pub fn col_op_result(op: ColOp, values: &[Bson]) -> ColOut {
    use std::collections::{HashMap, HashSet};
    let distinct = || {
        let mut seen = HashSet::new();
        values.iter().filter(|v| seen.insert(compact(v))).cloned().collect::<Vec<_>>()
    };
    match op {
        ColOp::Copy => ColOut::Copy(values.iter().map(copy_text).collect::<Vec<_>>().join("\n"), values.len()),
        ColOp::CopyDistinct => {
            let d = distinct();
            ColOut::Copy(d.iter().map(copy_text).collect::<Vec<_>>().join("\n"), d.len())
        }
        ColOp::CopyDistinctJs => {
            let d = distinct();
            let n = d.len();
            ColOut::Copy(shell_text(&Bson::Array(d)), n)
        }
        ColOp::CountDistinct => ColOut::Notice(format!("{} distinct values among {}", distinct().len(), values.len())),
        ColOp::Sum | ColOp::Avg | ColOp::Median | ColOp::Range => ColOut::Notice(stat_result(op, values)),
        ColOp::Group => {
            let mut rows: Vec<(Bson, i64)> = Vec::new();
            let mut at: HashMap<String, usize> = HashMap::new();
            for v in values {
                let i = *at.entry(compact(v)).or_insert_with(|| {
                    rows.push((v.clone(), 0));
                    rows.len() - 1
                });
                rows[i].1 += 1;
            }
            rows.sort_by_key(|r| -r.1);
            // Same limit as the server-side group, so the dialog stays quick to draw.
            rows.truncate(1000);
            ColOut::Groups(rows.into_iter().map(|(v, n)| doc! { "_id": v, "count": n }).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(d: &Document) -> Vec<(String, i32)> {
        d.iter().map(|(k, v)| (k.clone(), v.as_i32().unwrap())).collect()
    }

    fn kv(v: &[(&str, i32)]) -> Vec<(String, i32)> {
        v.iter().map(|(k, d)| (k.to_string(), *d)).collect()
    }

    fn stat(path: &str, docs: usize, types: &[&'static str]) -> FieldStat {
        FieldStat { path: path.into(), depth: path.matches('.').count(), docs, types: types.iter().map(|t| (*t, 1)).collect(), examples: Vec::new() }
    }

    #[test]
    fn suggests_equality_only() {
        assert_eq!(keys(&suggest_index(&doc! { "a": 1, "b": "x" }, &doc! {})), kv(&[("a", 1), ("b", 1)]));
    }

    #[test]
    fn suggests_equality_then_sort() {
        assert_eq!(keys(&suggest_index(&doc! { "status": "a" }, &doc! { "createdAt": -1 })), kv(&[("status", 1), ("createdAt", -1)]));
    }

    #[test]
    fn suggests_equality_sort_range() {
        let f = doc! { "total": { "$lt": 50 }, "customer.tier": "gold" };
        assert_eq!(keys(&suggest_index(&f, &doc! { "createdAt": 1 })), kv(&[("customer.tier", 1), ("createdAt", 1), ("total", 1)]));
    }

    #[test]
    fn in_and_elem_match_count_as_equality() {
        let f = doc! { "n": { "$gt": 1 }, "t": { "$in": ["a", "b"] }, "l": { "$elemMatch": { "x": 1 } }, "$or": [{ "z": 1 }] };
        assert_eq!(keys(&suggest_index(&f, &doc! {})), kv(&[("t", 1), ("l", 1), ("n", 1)]));
    }

    #[test]
    fn sort_on_an_equality_field_keeps_its_place() {
        assert_eq!(keys(&suggest_index(&doc! { "a": 1 }, &doc! { "a": -1, "b": -1 })), kv(&[("a", 1), ("b", -1)]));
    }

    #[test]
    fn groups_profile_by_shape() {
        let e = |f: Document, s: Option<Document>, ms: i32| {
            let mut d = doc! { "filter": f, "millis": ms };
            if let Some(s) = s {
                d.insert("sort", s);
            }
            d
        };
        let entries = vec![e(doc! { "a": 1 }, None, 10), e(doc! { "a": 2 }, None, 30), e(doc! { "a": 1 }, Some(doc! { "b": 1 }), 5), e(doc! {}, None, 99)];
        let g = group_profile(&entries);
        assert_eq!(g.len(), 2);
        assert_eq!((g[0].count, g[0].avg_ms), (2, 20.0));
        assert_eq!(keys(&g[1].suggestion), kv(&[("a", 1), ("b", 1)]));
    }

    #[test]
    fn unused_needs_an_old_zero_counter() {
        let week = 7 * 24 * 3600 * 1000;
        assert!(is_unused(0, Some(0), week + 1));
        assert!(!is_unused(0, Some(0), week));
        assert!(!is_unused(3, Some(0), week * 2));
        assert!(!is_unused(0, None, week * 2));
    }

    #[test]
    fn schema_from_stats_maps_types_and_required() {
        let s = vec![stat("_id", 10, &["objectId"]), stat("age", 10, &["int", "string"]), stat("nick", 4, &["string"]), stat("tags", 10, &["array"])];
        let js = schema_from_stats(&s, 10);
        let js = js.get_document("$jsonSchema").unwrap();
        let names = |a: &Vec<Bson>| a.iter().map(|b| b.as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(names(js.get_array("required").unwrap()), ["_id", "age", "tags"]);
        let p = js.get_document("properties").unwrap();
        assert_eq!(names(p.get_document("age").unwrap().get_array("bsonType").unwrap()), ["int", "string"]);
        assert_eq!(p.get_document("tags").unwrap().get_str("bsonType").unwrap(), "array");
        assert_eq!(p.get_document("nick").unwrap().get_str("bsonType").unwrap(), "string");
    }

    #[test]
    fn schema_from_stats_nests_objects_but_not_arrays() {
        let s = vec![stat("addr", 5, &["object"]), stat("addr.city", 5, &["string"]), stat("addr.zip", 2, &["string"]), stat("list", 5, &["array"]), stat("list.x", 5, &["int"])];
        let js = schema_from_stats(&s, 10);
        let top = js.get_document("$jsonSchema").unwrap();
        assert!(top.get("required").is_none());
        let p = top.get_document("properties").unwrap();
        let addr = p.get_document("addr").unwrap();
        assert_eq!(addr.get_array("required").unwrap(), &vec![Bson::String("city".into())]);
        assert!(addr.get_document("properties").unwrap().get_document("zip").is_ok());
        assert!(p.get_document("list").unwrap().get("properties").is_none());
    }

    #[test]
    fn column_data_ops() {
        let v = [Bson::Int32(3), Bson::Int64(1), Bson::String("a".into()), Bson::Double(2.5), Bson::Int32(1)];
        let notice = |op| match col_op_result(op, &v) {
            ColOut::Notice(s) => s.lines().next().unwrap().to_string(),
            _ => panic!(),
        };
        assert_eq!(notice(ColOp::Sum), "7.5");
        assert_eq!(notice(ColOp::Avg), "1.875");
        assert_eq!(notice(ColOp::Median), "1.75");
        assert_eq!(notice(ColOp::CountDistinct), "4 distinct values among 5");
        let odd = [Bson::Int32(5), Bson::Int32(1), Bson::Int32(2)];
        assert_eq!(col_op_result(ColOp::Median, &odd), ColOut::Notice("2\n\n3 of 3 values are numbers".into()));
        assert_eq!(col_op_result(ColOp::CopyDistinct, &v), ColOut::Copy("3\n1\na\n2.5".into(), 4));
        let ColOut::Copy(js, 4) = col_op_result(ColOp::CopyDistinctJs, &v) else { panic!() };
        assert!(js.starts_with('[') && js.contains("\"a\""), "{js}");
        let ColOut::Groups(g) = col_op_result(ColOp::Group, &v) else { panic!() };
        assert_eq!(g[0], doc! { "_id": 1i64, "count": 2i64 });
        assert_eq!(notice(ColOp::Range), "1 to 3");
        let text = [Bson::String(" 4 ".into()), Bson::String("x".into()), Bson::String("NaN".into()), Bson::String("007".into())];
        assert_eq!(col_op_result(ColOp::Sum, &text), ColOut::Notice("4\n\n1 of 4 values are numbers".into()));
        let day = 86_400_000;
        let d = |ms| Bson::DateTime(mongodb::bson::DateTime::from_millis(ms));
        let dates = [d(0), d(3 * day + 3_661_000), d(day), Bson::Int32(1)];
        assert_eq!(col_op_result(ColOp::Median, &dates), ColOut::Notice("1970-01-02T00:00:00Z\n\n3 of 4 values are dates".into()));
        assert_eq!(col_op_result(ColOp::Range, &dates), ColOut::Notice("1970-01-01T00:00:00Z to 1970-01-04T01:01:01Z\nSpan 3d 1h 1m 1s\n\n3 of 4 values are dates".into()));
        assert!(matches!(col_op_result(ColOp::Sum, &dates), ColOut::Notice(s) if s.starts_with("Dates cannot")));
    }
}
