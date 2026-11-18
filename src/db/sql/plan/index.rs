//! Whether an index would serve a filtered full scan, and which one, judged from the plan and the catalog together. The
//! candidate comes from the filter alone (equal columns by how few rows each keeps, then IN lists, then one range) and is
//! offered only when every gate holds: a table big enough, a filter that keeps few of its rows, a scan that weighs on
//! the plan, fresh statistics and no index that already serves it. Otherwise the advice says why not.

use super::cond::{self, ColRef, SargKind, Sargs, Val};
use super::evidence::{IndexEv, TableEv};
use super::findings::{and_list, pct_text, size, Fix, FixKind, SHARE_MIN};
use super::tree::is_inner;
use super::{count, Dialect, Op, Plan};

/// A table smaller than this reads whole about as fast as through an index.
pub(super) const MIN_ROWS: f64 = 10_000.0;
/// A filter that keeps more than this share of the rows reads most pages either way.
pub(super) const MAX_KEPT: f64 = 0.05;
/// A column that keeps more than this share narrows too little to be worth a place in the index.
const WEAK: f64 = 0.5;
const MAX_COLS: usize = 3;
/// Statistics are stale once this share of the rows changed since the last ANALYZE, and at least `STALE_MIN` rows did.
const STALE_SHARE: f64 = 0.20;
const STALE_MIN: f64 = 1000.0;
/// An inner scan of a nested loop that runs this many times is worth an index whatever its share.
const REPEAT_LOOPS: f64 = 10.0;

pub const NOTE: &str = "From the filter: equal columns first, then the range. Check it against the table's other queries before creating it.";

#[derive(Clone, Debug, PartialEq)]
pub enum Advice {
    Create { fix: Fix, lines: Vec<String> },
    /// An index that serves the filter already exists and the scan did not use it; the cause only when it is known.
    Unused { index: String, cause: Option<String>, lines: Vec<String> },
    /// The statistics are too old to judge the plan by; ANALYZE first.
    Stale { why: String, fix: Fix, lines: Vec<String> },
    No { why: String, lines: Vec<String> },
}

/// How much of the table the filter keeps, and how that is known.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Kept {
    /// ANALYZE counted it.
    Measured(f64),
    /// PostgreSQL's estimated rows over the table's.
    Estimated(f64),
    /// MySQL's filtered, which is a guess even when it looks exact.
    Guess(f64),
    Unknown,
}

/// What the catalog and the plan say about the filtered full scan `i` of table `t`.
pub(super) fn advise(p: &Plan, i: usize, d: Dialect, t: &TableEv, pooler: bool) -> Advice {
    let n = &p.nodes[i];
    let name = t.table.as_str();
    let mut lines = Vec::new();
    let rows = table_rows(p, i, t, d);
    lines.push(size_line(t, rows));
    if let Some(l) = stats_line(t) {
        lines.push(l);
    }
    if let Some(why) = stale(t) {
        return Advice::Stale { why, fix: analyze_fix(d, t), lines };
    }
    let no = |why: String, lines: Vec<String>| Advice::No { why, lines };
    let Some(rows) = rows else { return no(format!("the size of {name} is not known"), lines) };
    if rows < MIN_ROWS {
        return no(format!("{name} has about {} rows, few enough that reading it whole is cheap", count(rows)), lines);
    }
    let Some(e) = &n.filter.expr else { return no("the filter could not be read".into(), lines) };
    let kept = match kept(p, i, t, d) {
        Kept::Guess(f) => return no(format!("the {} of rows the server says the filter keeps is its guess. Run with ANALYZE to measure it", pct_text(f * 100.0)), lines),
        Kept::Unknown => return no("how many rows the filter keeps is not known".into(), lines),
        Kept::Measured(f) => {
            lines.push(format!("The filter keeps {} of the rows it reads, measured.", pct_text(f * 100.0)));
            f
        }
        Kept::Estimated(f) => {
            lines.push(format!("The filter keeps about {} of the rows, by the planner's estimate.", pct_text(f * 100.0)));
            f
        }
    };
    if kept > MAX_KEPT {
        return no(format!("the filter keeps {} of the rows, too many for an index to read less than the scan", pct_text(kept * 100.0)), lines);
    }
    let repeated = repeated(p, i);
    if let Some(s) = n.share.filter(|&s| s < SHARE_MIN && !repeated) {
        return no(format!("this scan is only {} of the plan", pct_text(s * 100.0)), lines);
    }
    let own = |c: &ColRef| c.qual.as_deref().is_none_or(|q| same(q, &n.alias, d) || same(q, &n.rel, d) || same(q, &t.table, d));
    let ty = |c: &str| t.col(c).map(|c| c.ty.clone());
    let s = cond::sargs(e, d, &own, repeated, &ty);
    let pick = pick(&s, t, rows, d);
    let Some(indexes) = &t.indexes else { return no(format!("the indexes of {name} could not be read"), lines) };
    if pick.cols.is_empty() {
        // An index the filter could have used, had it not wrapped the column.
        for b in &s.blockers {
            if let Some(ix) = indexes.iter().find(|ix| usable(ix) && ix.cols.first().is_some_and(|c| same(c, &b.col, d))) {
                let how = match b.kind {
                    cond::BlockKind::Wrapped => format!("wraps {} in {}()", b.col, b.what),
                    cond::BlockKind::Cast => format!("casts {} to {}", b.col, b.what),
                    cond::BlockKind::LeadingWild => format!("matches {} with a pattern that starts with a wildcard", b.col),
                    cond::BlockKind::Or => format!("tests {} inside an OR", b.col),
                };
                return Advice::Unused { index: ix.name.clone(), cause: Some(format!("The filter {how}, so {} cannot be searched.", ix.name)), lines };
            }
        }
        let why = if s.blockers.is_empty() { "nothing in the filter is a condition an index can search".to_string() } else { "the filter's columns sit inside functions, casts or an OR, which an index on them cannot search".to_string() };
        return no(why, lines);
    }
    if !pick.all {
        // The filter's other conditions do part of the narrowing, so the index has to narrow enough by itself.
        match pick.sel {
            Some(f) if f <= MAX_KEPT => {}
            Some(f) => return no(format!("an index could use only {}, which keep about {} of the rows", and_list(&pick.cols), pct_text(f * 100.0)), lines),
            None => return no(format!("an index could use only {}, and how much that narrows is not known", and_list(&pick.cols)), lines),
        }
    }
    let range = pick.range.as_deref();
    let lead: Vec<&str> = pick.cols.iter().map(String::as_str).filter(|c| Some(*c) != range).collect();
    // A valid, visible index wins over an invalid or hidden one with the same columns.
    let mut serving: Vec<&IndexEv> = indexes.iter().filter(|ix| serves(ix, &lead, range, d)).collect();
    serving.sort_by_key(|ix| (ix.invalid, ix.invisible));
    if let Some(ix) = serving.first() {
        lines.push(format!("{} already starts with {}.", ix.name, and_list(&pick.cols)));
        let cause = if ix.invalid {
            Some(format!("{} is invalid, as a CREATE INDEX CONCURRENTLY that failed leaves it, so the planner never uses it. Rebuild it with REINDEX.", ix.name))
        } else if ix.invisible {
            Some(format!("{} is {} to the optimizer.", ix.name, if d == Dialect::MariaDb { "ignored" } else { "invisible" }))
        } else {
            None
        };
        return Advice::Unused { index: ix.name.clone(), cause, lines };
    }
    for ix in indexes.iter().filter(|ix| usable(ix)) {
        let k = ix.cols.iter().take_while(|c| pick.cols.iter().any(|p| same(p, c, d))).count();
        if k > 0 {
            lines.push(format!("{} covers only {}.", ix.name, and_list(&ix.cols[..k])));
        }
    }
    let nd: Vec<String> = pick.cols.iter().filter_map(|c| distinct(t, c, rows).map(|n| format!("{c} {}", count(n)))).collect();
    if !nd.is_empty() {
        lines.push(format!("Distinct values: {}.", nd.join(", ")));
    }
    let taken: Vec<&str> = indexes.iter().map(|ix| ix.name.as_str()).collect();
    let ix = index_name(&t.table, &pick.cols, &taken, d.max_ident(), d);
    Advice::Create { fix: create_fix(d, t, &ix, &pick.cols, pooler), lines }
}

/// The candidate's columns in order, and whether they serve every condition of the filter.
struct Pick {
    cols: Vec<String>,
    range: Option<String>,
    /// Every top-level condition is served by the columns.
    all: bool,
    /// The share of rows the columns keep together, when each one's is known.
    sel: Option<f64>,
}

fn pick(s: &Sargs, t: &TableEv, rows: f64, d: Dialect) -> Pick {
    // A column of another table, or of no table (an output name), cannot be in this table's index, nor can one whose type
    // a plain B-tree does not take.
    let known = |c: &str| t.col(c).is_some_and(|c| indexable(&c.ty, d));
    let mut eq: Vec<(String, Option<f64>)> = Vec::new();
    let mut ins: Vec<(String, Option<f64>)> = Vec::new();
    let mut ranges: Vec<String> = Vec::new();
    for kind in [SargKind::Eq, SargKind::In, SargKind::Range, SargKind::Prefix] {
        for g in s.sargs.iter().filter(|g| g.kind == kind && known(&g.col)) {
            let taken = eq.iter().chain(&ins).any(|(c, _)| same(c, &g.col, d)) || ranges.iter().any(|c| same(c, &g.col, d));
            if taken {
                continue;
            }
            match kind {
                SargKind::Eq => eq.push((g.col.clone(), sel(t, &g.col, &g.values, rows))),
                SargKind::In => ins.push((g.col.clone(), sel(t, &g.col, &g.values, rows))),
                _ => ranges.push(g.col.clone()),
            }
        }
    }
    // Most selective first; a column with no statistics after those that have them, in the filter's order.
    let order = |a: &(String, Option<f64>), b: &(String, Option<f64>)| match (a.1, b.1) {
        (Some(x), Some(y)) => x.total_cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    };
    eq.sort_by(order);
    ins.sort_by(order);
    let mut cols: Vec<(String, Option<f64>)> = eq.into_iter().chain(ins).collect();
    let range = ranges.first().cloned();
    if let Some(r) = &range {
        cols.push((r.clone(), None));
    }
    // A weak column only widens the index, unless it is all there is. Leaving it out costs at most half the narrowing, so
    // it still counts as served below.
    let mut weak: Vec<String> = Vec::new();
    while cols.len() > 1 {
        let Some(k) = cols.iter().position(|(_, s)| s.is_some_and(|s| s > WEAK)) else { break };
        weak.push(cols.remove(k).0);
    }
    cols.truncate(MAX_COLS);
    let range = range.filter(|r| cols.iter().any(|(c, _)| c == r));
    let names: Vec<String> = cols.iter().map(|(c, _)| c.clone()).collect();
    let all = s.blockers.is_empty() && s.other == 0 && s.sargs.iter().all(|g| names.iter().chain(&weak).any(|c| same(c, &g.col, d)));
    let sel = cols.iter().try_fold(1.0, |acc, (_, s)| s.map(|s| acc * s));
    Pick { cols: names, range, all, sel }
}

/// Whether a plain index can hold the column whole: MySQL needs a prefix length for TEXT and BLOB and takes no JSON or
/// spatial column, and PostgreSQL's json and xml have no B-tree operator class.
pub(super) fn indexable(ty: &str, d: Dialect) -> bool {
    let t = ty.to_ascii_lowercase();
    let base = t.split(['(', ' ']).next().unwrap_or_default();
    if d == Dialect::Postgres {
        return !matches!(base, "json" | "xml" | "point" | "polygon" | "line" | "circle" | "box" | "path");
    }
    !(base.ends_with("text") || base.ends_with("blob") || matches!(base, "json" | "geometry" | "point" | "linestring" | "polygon" | "multipoint" | "multilinestring" | "multipolygon" | "geometrycollection"))
}

/// The share of rows `col` keeps for these values, from its statistics.
fn sel(t: &TableEv, col: &str, values: &[Val], rows: f64) -> Option<f64> {
    let c = t.col(col)?;
    let nulls = c.null_frac.unwrap_or(0.0);
    let nd = distinct(t, col, rows);
    let common: f64 = c.mcv.iter().map(|(_, f)| f).sum();
    let boolean = c.ty.eq_ignore_ascii_case("boolean");
    let one = |v: &Val| -> Option<f64> {
        match v {
            Val::Null => c.null_frac,
            Val::Lit(x) => {
                let x = match (boolean, x.as_str()) {
                    (true, "true") => "t",
                    (true, "false") => "f",
                    _ => x.as_str(),
                };
                if let Some((_, f)) = c.mcv.iter().find(|(v, _)| v == x) {
                    return Some(*f);
                }
                let rest = nd? - c.mcv.len() as f64;
                Some(((1.0 - common - nulls).max(0.0) / rest.max(1.0)).min(1.0))
            }
            Val::Unknown => Some((1.0 - nulls) / nd?.max(1.0)),
        }
    };
    let mut total = 0.0;
    for v in values {
        total += one(v)?;
    }
    (!values.is_empty()).then(|| total.min(1.0))
}

/// A column's distinct values as a count: pg_stats writes a share of the rows as a negative number.
pub(super) fn distinct(t: &TableEv, col: &str, rows: f64) -> Option<f64> {
    let n = t.col(col)?.n_distinct?;
    let n = if n < 0.0 { -n * rows } else { n };
    (n > 0.0).then_some(n)
}

/// A B-tree over plain columns of every row, which an equality or range on its leading columns can search.
pub(super) fn usable(ix: &IndexEv) -> bool {
    ix.btree && !ix.partial && !ix.invalid && !ix.invisible
}

/// Whether `ix` leads with the equal columns in any order and then the range, so it serves what the candidate would.
fn serves(ix: &IndexEv, lead: &[&str], range: Option<&str>, d: Dialect) -> bool {
    let m = lead.len();
    if !ix.btree || ix.partial || ix.cols.len() < m + usize::from(range.is_some()) {
        return false;
    }
    let head = &ix.cols[..m];
    let same_set = lead.iter().all(|c| head.iter().any(|h| same(h, c, d))) && head.iter().all(|h| !h.is_empty());
    same_set && range.is_none_or(|r| same(&ix.cols[m], r, d))
}

/// PostgreSQL names are exact once folded; MySQL compares names without case on most servers.
pub(super) fn same(a: &str, b: &str, d: Dialect) -> bool {
    if d == Dialect::Postgres { a == b } else { a.eq_ignore_ascii_case(b) }
}

/// Whether node `i` runs many times as the inner side of a nested loop.
pub fn repeated(p: &Plan, i: usize) -> bool {
    let n = &p.nodes[i];
    if n.loops.is_some_and(|l| l >= REPEAT_LOOPS) && n.parent.is_some_and(|j| p.nodes[j].op == Op::NestedLoop) {
        return true;
    }
    let Some(j) = n.parent else { return false };
    let parent = &p.nodes[j];
    if parent.op != Op::NestedLoop || !is_inner(p, j, i) {
        return false;
    }
    parent.kids.iter().find(|&&k| !is_inner(p, j, k)).and_then(|&k| p.nodes[k].rows).is_some_and(|r| r >= REPEAT_LOOPS)
}

/// The rows of the table: PostgreSQL's reltuples; on MySQL the larger of TABLE_ROWS, which can lag a day behind, and the
/// rows the full scan expects to read.
pub(super) fn table_rows(p: &Plan, i: usize, t: &TableEv, d: Dialect) -> Option<f64> {
    if d == Dialect::Postgres {
        return t.rows;
    }
    let scan = p.nodes[i].rows;
    match (t.rows, scan) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

pub(super) fn kept(p: &Plan, i: usize, t: &TableEv, d: Dialect) -> Kept {
    let n = &p.nodes[i];
    if p.analyzed && !n.never_ran {
        if let (Some(k), Some(r)) = (n.kept_rows(), n.removed) {
            if k + r > 0.0 {
                return Kept::Measured(k / (k + r));
            }
        }
    }
    if d == Dialect::Postgres {
        if let (Some(est), Some(total)) = (n.rows, t.rows.filter(|&r| r > 0.0)) {
            return Kept::Estimated((est * divisor(p, i) / total).min(1.0));
        }
    }
    match n.filtered_pct {
        Some(f) => Kept::Guess(f / 100.0),
        None => Kept::Unknown,
    }
}

/// A parallel scan's Plan Rows are per process. PostgreSQL divides by the workers plus what the leader adds
/// (1 - 0.3 per worker, when above zero), so the estimate for the whole scan multiplies that back.
fn divisor(p: &Plan, i: usize) -> f64 {
    if !p.nodes[i].parallel {
        return 1.0;
    }
    let mut j = p.nodes[i].parent;
    while let Some(k) = j {
        if p.nodes[k].op == Op::Gather {
            let w = p.nodes[k].workers.unwrap_or(0.0);
            let leader = 1.0 - 0.3 * w;
            return (w + leader.max(0.0)).max(1.0);
        }
        j = p.nodes[k].parent;
    }
    1.0
}

/// Why the statistics of `t` cannot be trusted, when they cannot.
pub(super) fn stale(t: &TableEv) -> Option<String> {
    if t.never_analyzed {
        return Some(format!("{} has never been analyzed", t.table));
    }
    let m = t.mods_since?;
    let base = t.rows.unwrap_or(0.0).max(1.0);
    (m >= STALE_MIN && m / base >= STALE_SHARE).then(|| format!("{} rows of {} changed since its last analyze, {} of the table", count(m), t.table, pct_text(m / base * 100.0)))
}

/// Whether the statistics of `t` are known and fresh, for the misestimate that has to look elsewhere.
pub fn fresh(t: &TableEv) -> bool {
    t.analyzed_ago_s.is_some() && stale(t).is_none()
}

pub(super) fn size_line(t: &TableEv, rows: Option<f64>) -> String {
    let mut s = format!("{}.{}", t.schema, t.table);
    if let Some(r) = rows {
        s += &format!(": about {} rows", count(r));
    }
    if let Some(b) = t.bytes {
        s += &format!(", {}", size(b / 1024.0));
    }
    s + "."
}

pub(super) fn stats_line(t: &TableEv) -> Option<String> {
    if t.never_analyzed {
        return Some("Never analyzed.".into());
    }
    let ago = t.analyzed_ago_s?;
    let mut s = format!("Last analyzed {} ago", ago_text(ago));
    if let Some(m) = t.mods_since {
        s += &format!(", {} rows changed since", count(m));
    }
    Some(s + ".")
}

/// 90 as "2 minutes", 7200 as "2 hours".
pub(super) fn ago_text(s: f64) -> String {
    let unit = |n: f64, w: &str| {
        let n = n.round().max(1.0);
        format!("{} {w}{}", count(n), if n == 1.0 { "" } else { "s" })
    };
    match s {
        s if s < 60.0 => "under a minute".into(),
        s if s < 3600.0 => unit(s / 60.0, "minute"),
        s if s < 86_400.0 => unit(s / 3600.0, "hour"),
        s => unit(s / 86_400.0, "day"),
    }
}

/// `{table}_{cols}_idx`, cut to the server's name limit on a character boundary and numbered past the names taken.
pub fn index_name(table: &str, cols: &[String], taken: &[&str], max: usize, d: Dialect) -> String {
    let stem = format!("{table}_{}", cols.join("_"));
    for k in 0.. {
        let suffix = if k == 0 { "_idx".to_string() } else { format!("_idx{k}") };
        let mut end = max.saturating_sub(suffix.len()).min(stem.len());
        while !stem.is_char_boundary(end) {
            end -= 1;
        }
        let name = format!("{}{suffix}", &stem[..end]);
        if !taken.iter().any(|t| same(t, &name, d)) {
            return name;
        }
    }
    unreachable!()
}

fn create_fix(d: Dialect, t: &TableEv, name: &str, cols: &[String], pooler: bool) -> Fix {
    let list = cols.iter().map(|c| d.quote_ident(c)).collect::<Vec<_>>().join(", ");
    let (sql, trial) = create(d, t, name, &list, How::Plain, pooler);
    Fix { kind: FixKind::Index, label: "Candidate index".into(), sql, note: NOTE.into(), trial }
}

/// What kind of index `create` writes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum How {
    Plain,
    /// PostgreSQL's GIN, for pg_trgm.
    Gin,
    /// An index on an expression, which MySQL adds as a hidden generated column.
    Expr,
}

/// `CREATE INDEX` of `parts` (what goes between the parentheses) on `t`, built without blocking writes where the server
/// can, and the bare statement a HypoPG trial takes: PostgreSQL's, without CONCURRENTLY, which only matters for a real
/// build, the comment and the semicolon. HypoPG simulates no GIN index, so a trigram index has none.
pub(super) fn create(d: Dialect, t: &TableEv, name: &str, parts: &str, how: How, pooler: bool) -> (String, Option<String>) {
    let target = d.qualified(&t.schema, &t.table);
    let ix = d.quote_ident(name);
    let using = if how == How::Gin { " USING gin" } else { "" };
    let bare = format!("CREATE INDEX {ix} ON {target}{using} ({parts})");
    let trial = (d == Dialect::Postgres && how != How::Gin).then(|| bare.clone());
    let sql = match d {
        // A pooler runs each statement in a transaction, where CONCURRENTLY is refused.
        Dialect::Postgres if pooler => format!("-- Blocks writes to {} while it builds: CONCURRENTLY cannot run through the connection pooler.\n{bare};", t.table),
        Dialect::Postgres => format!("CREATE INDEX CONCURRENTLY {ix} ON {target}{using} ({parts});"),
        // MySQL refuses LOCK=NONE for the virtual column behind an index on an expression.
        _ if how == How::Expr => format!("-- Blocks writes to {} while it builds: MySQL adds an index on an expression under a shared lock.\nALTER TABLE {target} ADD INDEX {ix} ({parts}), ALGORITHM=INPLACE, LOCK=SHARED;", t.table),
        _ => format!("ALTER TABLE {target} ADD INDEX {ix} ({parts}), ALGORITHM=INPLACE, LOCK=NONE;"),
    };
    (sql, trial)
}

pub(super) fn analyze_fix(d: Dialect, t: &TableEv) -> Fix {
    let target = d.qualified(&t.schema, &t.table);
    let sql = if d == Dialect::Postgres { format!("ANALYZE {target};") } else { format!("ANALYZE TABLE {target};") };
    Fix { kind: FixKind::Analyze, label: "ANALYZE".into(), sql, note: format!("Refreshes the planner's statistics of {} from a sample of its rows.", t.table), trial: None }
}

#[cfg(test)]
mod tests {
    use super::super::evidence::{ColEv, Evidence};
    use super::super::findings::Kind;
    use super::super::{parse, Plan};
    use super::*;

    /// A PostgreSQL scan of `bookings` with `filter`, `rows` estimated of the table's 400,000, carrying most of the cost.
    fn pg_scan(filter: &str, rows: f64) -> Plan {
        let json = format!(
            r#"[{{"Plan":{{"Node Type":"Seq Scan","Relation Name":"bookings","Alias":"bookings","Total Cost":7439.0,"Plan Rows":{rows},"Plan Width":63,"Filter":{}}}}}]"#,
            serde_json::to_string(filter).unwrap()
        );
        parse(Dialect::Postgres, &json).unwrap()
    }

    fn col(name: &str, nd: f64) -> ColEv {
        ColEv { name: name.into(), ty: "text".into(), null_frac: Some(0.0), n_distinct: Some(nd), ..Default::default() }
    }

    fn ix(name: &str, cols: &[&str]) -> IndexEv {
        IndexEv { name: name.into(), cols: cols.iter().map(|c| c.to_string()).collect(), btree: true, ..Default::default() }
    }

    fn bookings(cols: Vec<ColEv>, indexes: Vec<IndexEv>) -> TableEv {
        TableEv { names: vec![("bookings".into(), "bookings".into())], schema: "public".into(), table: "bookings".into(), rows: Some(400_000.0), bytes: Some(5e7), analyzed_ago_s: Some(3600.0), mods_since: Some(10.0), indexes: Some(indexes), cols, ..Default::default() }
    }

    fn std_cols() -> Vec<ColEv> {
        vec![col("provider", 5.0), col("status", 6.0), col("created_at", -0.9), col("email", -0.12), ColEv { ty: "boolean".into(), ..col("flagged", 2.0) }]
    }

    fn advice(p: &Plan, t: &TableEv) -> Advice {
        advise(p, 0, Dialect::Postgres, t, false)
    }

    fn sql(a: &Advice) -> &str {
        match a {
            Advice::Create { fix, .. } => &fix.sql,
            a => panic!("no candidate: {a:?}"),
        }
    }

    #[test]
    fn equal_columns_by_selectivity_then_in_then_the_range() {
        let p = pg_scan("((created_at >= '2026-10-01'::timestamp with time zone) AND (status = ANY ('{CONFIRMED,FAILED}'::booking_status[])) AND (provider = 'ZEUS_AIR'::provider) AND (flagged IS NULL))", 1000.0);
        let mut cols = std_cols();
        cols[4].null_frac = Some(0.001);
        let a = advice(&p, &bookings(cols, vec![ix("bookings_pkey", &["id"])]));
        // flagged IS NULL keeps 0.1%, provider 20%: both equal, so they lead, then the IN list, then the range; three at most.
        assert_eq!(sql(&a), r#"CREATE INDEX CONCURRENTLY "bookings_flagged_provider_status_idx" ON "public"."bookings" ("flagged", "provider", "status");"#);
    }

    #[test]
    fn a_weak_column_is_dropped_unless_alone() {
        // flagged = false keeps half the rows or more: it would only widen the index.
        let p = pg_scan("((NOT flagged) AND (status = 'DONE'::booking_status) AND (created_at < '2026-01-02'::timestamp with time zone))", 1000.0);
        let mut cols = std_cols();
        cols[4].mcv = vec![("f".into(), 0.99), ("t".into(), 0.01)];
        assert!(sql(&advice(&p, &bookings(cols, vec![]))).ends_with(r#"("status", "created_at");"#));
        // Alone, it stays: the gates on the whole filter already passed.
        let p = pg_scan("(provider = 'ZEUS_AIR'::provider)", 1000.0);
        let mut cols = std_cols();
        cols[0].mcv = vec![("ZEUS_AIR".into(), 0.6)];
        assert!(sql(&advice(&p, &bookings(cols, vec![]))).ends_with(r#"("provider");"#));
    }

    #[test]
    fn a_tiny_table_or_a_filter_that_keeps_much_gets_none() {
        let p = pg_scan("(provider = 'ZEUS_AIR'::provider)", 50.0);
        let mut t = bookings(std_cols(), vec![]);
        t.rows = Some(400.0);
        let Advice::No { why, .. } = advice(&p, &t) else { panic!() };
        assert!(why.contains("400 rows"), "{why}");
        // 80,000 of 400,000 rows is 20%.
        let p = pg_scan("(provider = 'ZEUS_AIR'::provider)", 80_000.0);
        let Advice::No { why, .. } = advice(&p, &bookings(std_cols(), vec![])) else { panic!() };
        assert!(why.contains("keeps 20%"), "{why}");
    }

    #[test]
    fn an_index_that_serves_the_filter_means_no_candidate() {
        let p = pg_scan("((provider = 'ZEUS_AIR'::provider) AND (status = 'DONE'::booking_status))", 1000.0);
        // The same columns in another order serve the same equalities.
        let a = advice(&p, &bookings(std_cols(), vec![ix("b_ps", &["provider", "status", "id"])]));
        assert!(matches!(&a, Advice::Unused { index, cause: None, .. } if index == "b_ps"), "{a:?}");
        // An invalid one is named with its cause, and no duplicate is offered.
        let mut bad = ix("b_sp", &["status", "provider"]);
        bad.invalid = true;
        let Advice::Unused { cause: Some(c), .. } = advice(&p, &bookings(std_cols(), vec![bad])) else { panic!() };
        assert!(c.contains("invalid") && c.contains("REINDEX"), "{c}");
        // An index that leads with only one of them leaves the candidate, with that as evidence.
        let Advice::Create { lines, fix } = advice(&p, &bookings(std_cols(), vec![ix("bookings_status_idx", &["status"])])) else { panic!() };
        assert!(lines.iter().any(|l| l == "bookings_status_idx covers only status."), "{lines:?}");
        assert!(fix.sql.contains(r#"("status", "provider")"#), "{}", fix.sql);
        // A partial index is no proof of anything.
        let mut part = ix("b_p", &["provider", "status"]);
        part.partial = true;
        assert!(matches!(advice(&p, &bookings(std_cols(), vec![part])), Advice::Create { .. }));
    }

    #[test]
    fn a_wrapped_column_with_an_index_names_the_blocker() {
        let p = pg_scan("(date(created_at) = '2026-03-01'::date)", 1000.0);
        let Advice::Unused { index, cause: Some(c), .. } = advice(&p, &bookings(std_cols(), vec![ix("bookings_created_at_idx", &["created_at"])])) else { panic!() };
        assert_eq!(index, "bookings_created_at_idx");
        assert!(c.contains("wraps created_at in date()"), "{c}");
    }

    #[test]
    fn stale_statistics_ask_for_analyze_first() {
        let p = pg_scan("(provider = 'ZEUS_AIR'::provider)", 1000.0);
        let mut t = bookings(std_cols(), vec![]);
        t.mods_since = Some(150_000.0);
        let Advice::Stale { why, fix, .. } = advice(&p, &t) else { panic!() };
        assert!(why.contains("150,000 rows"), "{why}");
        assert_eq!((fix.kind, fix.sql.as_str()), (FixKind::Analyze, r#"ANALYZE "public"."bookings";"#));
        t.never_analyzed = true;
        assert!(matches!(advice(&p, &t), Advice::Stale { .. }));
    }

    #[test]
    fn names_are_cut_to_the_limit_and_numbered_past_those_taken() {
        let cols = vec!["a_rather_long_column_name".to_string(), "another_long_column_name".to_string()];
        let n = index_name("événements_du_trimestre", &cols, &[], 63, Dialect::Postgres);
        assert!(n.len() <= 63 && n.ends_with("_idx"), "{n}");
        assert_eq!(index_name("t", &["a".into()], &["t_a_idx"], 63, Dialect::Postgres), "t_a_idx1");
        assert_eq!(index_name("t", &["a".into()], &["T_A_IDX"], 64, Dialect::MySql), "t_a_idx1");
    }

    #[test]
    fn behind_a_pooler_the_index_is_built_without_concurrently() {
        let p = pg_scan("(provider = 'ZEUS_AIR'::provider)", 1000.0);
        let Advice::Create { fix, .. } = advise(&p, 0, Dialect::Postgres, &bookings(std_cols(), vec![]), true) else { panic!() };
        assert!(fix.sql.starts_with("-- Blocks writes") && fix.sql.contains("\nCREATE INDEX \"bookings_provider_idx\""), "{}", fix.sql);
    }

    #[test]
    fn a_varchar_relabel_is_seen_through_and_other_casts_block() {
        let p = pg_scan("((email)::text = 'user42@example.com'::text)", 1.0);
        let mut cols = std_cols();
        cols[3].ty = "character varying(120)".into();
        assert!(sql(&advice(&p, &bookings(cols, vec![]))).ends_with(r#"("email");"#));
        // On a text column the same cast is a real one.
        let Advice::No { .. } = advice(&p, &bookings(std_cols(), vec![])) else { panic!() };
    }

    #[test]
    fn mysql_filtered_alone_is_a_guess() {
        let json = r#"{"query_block":{"select_id":1,"cost_info":{"query_cost":"20177.25"},"table":{"table_name":"orders","access_type":"ALL","rows_examined_per_scan":199290,"rows_produced_per_join":6642,"filtered":"3.33","cost_info":{"read_cost":"19513.02","eval_cost":"664.23","prefix_cost":"20177.25"},"attached_condition":"((`shop`.`orders`.`status` = 'failed') and (`shop`.`orders`.`total` > 800.00))"}}}"#;
        let p = parse(Dialect::MySql, json).unwrap();
        let my_col = |name: &str, ty: &str, nd: f64| ColEv { ty: ty.into(), ..col(name, nd) };
        let t = TableEv { names: vec![(String::new(), "orders".into())], schema: "shop".into(), table: "orders".into(), rows: Some(200_000.0), indexes: Some(vec![]), cols: vec![my_col("status", "varchar(16)", 5.0), my_col("total", "decimal(10,2)", 900.0)], ..Default::default() };
        let Advice::No { why, .. } = advise(&p, 0, Dialect::MySql, &t, false) else { panic!() };
        assert!(why.contains("ANALYZE"), "{why}");
        // Measured by EXPLAIN ANALYZE, the same filter gets its candidate.
        let p = parse(Dialect::MySql, include_str!("fixtures/mysql-scan-analyze.txt")).unwrap();
        let i = p.nodes.iter().position(|n| n.label.starts_with("Table scan")).unwrap();
        let t = TableEv { names: vec![(String::new(), "orders".into())], ..t };
        let Advice::Create { fix, .. } = advise(&p, i, Dialect::MySql, &t, false) else { panic!("{:?}", advise(&p, i, Dialect::MySql, &t, false)) };
        assert_eq!(fix.sql, "ALTER TABLE `shop`.`orders` ADD INDEX `orders_status_total_idx` (`status`, `total`), ALGORITHM=INPLACE, LOCK=NONE;");
        // A TEXT column takes no index without a prefix length, so the statement would fail: the range alone is left, and
        // how much it narrows is not known.
        let t = TableEv { cols: vec![my_col("status", "mediumtext", 5.0), my_col("total", "decimal(10,2)", 900.0)], ..t };
        let Advice::No { why, .. } = advise(&p, i, Dialect::MySql, &t, false) else { panic!() };
        assert!(why.contains("only total"), "{why}");
    }

    #[test]
    fn a_parallel_estimate_counts_every_process() {
        // 5,497 rows per process under two workers is about 13,200 in all: 3.3% of the table, not 1.4%.
        let p = parse(Dialect::Postgres, include_str!("fixtures/pg18-scan.json")).unwrap();
        let i = p.nodes.iter().position(|n| n.label == "Seq Scan").unwrap();
        let Advice::Create { lines, .. } = advise(&p, i, Dialect::Postgres, &bookings(std_cols(), vec![]), false) else { panic!() };
        assert!(lines.iter().any(|l| l.contains("about 3%")), "{lines:?}");
    }

    #[test]
    fn findings_carry_the_candidate_and_the_misestimate_its_cause() {
        let mut p = parse(Dialect::Postgres, include_str!("fixtures/pg18-scan.json")).unwrap();
        let ev = Evidence::demo(&p, Dialect::Postgres);
        p.refind(Dialect::Postgres, Some(&ev), false);
        let f = p.findings.iter().find(|f| f.kind == Kind::FullScan).unwrap();
        let fix = f.fix.as_ref().unwrap();
        assert_eq!((fix.kind, fix.label.as_str()), (FixKind::Index, "Candidate index"));
        assert!(fix.sql.contains(r#"("provider", "status")"#), "{}", fix.sql);
        assert!(f.evidence.iter().any(|l| l.starts_with("public.bookings: about 400,000 rows")), "{:?}", f.evidence);
        // The stale capture: events grew 200 times since its only ANALYZE.
        let mut p = parse(Dialect::Postgres, include_str!("fixtures/pg18-stale-analyze.json")).unwrap();
        let mut ev = Evidence::demo(&p, Dialect::Postgres);
        (ev.tables[0].rows, ev.tables[0].mods_since) = (Some(1000.0), Some(199_000.0));
        p.refind(Dialect::Postgres, Some(&ev), false);
        let m = p.findings.iter().find(|f| f.kind == Kind::Misestimate).unwrap();
        assert_eq!(m.fix.as_ref().map(|f| f.kind), Some(FixKind::Analyze));
        // Fresh statistics: the misestimate says the cause lies elsewhere.
        let mut p = parse(Dialect::Postgres, include_str!("fixtures/pg18-stale-analyze.json")).unwrap();
        let ev = Evidence::demo(&p, Dialect::Postgres);
        p.refind(Dialect::Postgres, Some(&ev), false);
        let m = p.findings.iter().find(|f| f.kind == Kind::Misestimate).unwrap();
        assert!(m.body.iter().any(|b| b.contains("Statistics are recent")), "{:?}", m.body);
        // A table the statement does not name, or a catalog that failed, says so instead of guessing.
        let mut p = parse(Dialect::Postgres, include_str!("fixtures/pg18-scan.json")).unwrap();
        p.refind(Dialect::Postgres, Some(&Evidence { failed: true, ..Default::default() }), false);
        let f = p.findings.iter().find(|f| f.kind == Kind::FullScan).unwrap();
        assert_eq!((f.fix.is_none(), f.no_fix.as_deref()), (true, Some("No suggestion: the catalog could not be read.")));
    }

    #[test]
    fn one_builder_writes_the_index_and_its_trial() {
        let t = bookings(std_cols(), Vec::new());
        let bare = r#"CREATE INDEX "b_idx" ON "public"."bookings" ("provider")"#;
        let (sql, trial) = create(Dialect::Postgres, &t, "b_idx", r#""provider""#, How::Plain, false);
        assert_eq!((sql.as_str(), trial.as_deref()), (r#"CREATE INDEX CONCURRENTLY "b_idx" ON "public"."bookings" ("provider");"#, Some(bare)));
        // Behind a pooler the build blocks writes and says so; the trial is the same statement.
        let (sql, trial) = create(Dialect::Postgres, &t, "b_idx", r#""provider""#, How::Plain, true);
        assert_eq!(sql, format!("-- Blocks writes to bookings while it builds: CONCURRENTLY cannot run through the connection pooler.\n{bare};"));
        assert_eq!(trial.as_deref(), Some(bare));
        assert!(create(Dialect::Postgres, &t, "e_idx", "(lower(\"email\"))", How::Expr, false).1.is_some());
        // HypoPG simulates no GIN index, and MySQL has no HypoPG.
        assert_eq!(create(Dialect::Postgres, &t, "t_idx", r#""email" gin_trgm_ops"#, How::Gin, false).1, None);
        assert_eq!(create(Dialect::MySql, &t, "b_idx", "`provider`", How::Plain, false).1, None);
    }
}
