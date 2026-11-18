//! What the catalog says about the tables a plan scans, so an index suggestion rests on more than the plan: the table's
//! size, how fresh its statistics are, the indexes it already has and what its filtered columns hold. A plan names a
//! table without its schema (PostgreSQL without VERBOSE) or only by its alias (MySQL), so the table is matched to the
//! statement's own names first; one that does not match exactly one table gets no evidence and so no suggestion.

use super::super::exec::{Cells, SqlRow};
use super::super::lex::{self, Tok};
use super::super::worker::text;
use super::super::{Db, Dialect};
use super::index::same;
use super::{Access, Plan, PlanNode};
use std::time::Duration;

/// At most this many tables are read, the ones the plan scans first.
const MAX_TABLES: usize = 4;
/// The whole read gives up after this long; the dialog then shows the plan's findings alone.
const LOAD_MS: u64 = 3000;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Evidence {
    pub tables: Vec<TableEv>,
    /// PostgreSQL extensions later suggestions need: HypoPG to try an index, pg_trgm for a leading wildcard.
    pub hypopg: bool,
    pub trgm: bool,
    /// Nothing could be read: the catalog refused or the read ran out of time.
    pub failed: bool,
    /// The server's version, for suggestions only newer servers take, such as MySQL's index on an expression.
    pub version: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TableEv {
    /// The names the plan reads it by: PostgreSQL's relation and alias, MySQL's alias with an empty relation.
    pub names: Vec<(String, String)>,
    pub schema: String,
    pub table: String,
    /// The planner's row count (reltuples, MySQL's TABLE_ROWS), and the size with indexes in bytes.
    pub rows: Option<f64>,
    pub bytes: Option<f64>,
    /// Seconds since the last ANALYZE, manual or automatic.
    pub analyzed_ago_s: Option<f64>,
    pub never_analyzed: bool,
    /// Rows written since the last ANALYZE (PostgreSQL).
    pub mods_since: Option<f64>,
    /// None when they could not be read, which is not the same as a table without indexes.
    pub indexes: Option<Vec<IndexEv>>,
    pub cols: Vec<ColEv>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct IndexEv {
    pub name: String,
    /// Key columns in order; an expression part is an empty name.
    pub cols: Vec<String>,
    pub unique: bool,
    pub btree: bool,
    /// A partial index (PostgreSQL WHERE) serves only the rows its condition names.
    pub partial: bool,
    /// PostgreSQL: left by a CREATE INDEX CONCURRENTLY that failed, and never used.
    pub invalid: bool,
    /// MySQL INVISIBLE, MariaDB IGNORED.
    pub invisible: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ColEv {
    pub name: String,
    pub ty: String,
    pub null_frac: Option<f64>,
    /// Distinct values; below zero it is minus a fraction of the rows, as pg_stats writes it.
    pub n_distinct: Option<f64>,
    /// Most common values and their share of the rows. Never sent to an agent.
    pub mcv: Vec<(String, f64)>,
    pub correlation: Option<f64>,
}

impl Evidence {
    /// The table a plan node reads, when the catalog was read for it.
    pub fn table(&self, n: &PlanNode) -> Option<&TableEv> {
        self.tables.iter().find(|t| t.names.iter().any(|(r, a)| *r == n.rel && *a == n.alias))
    }

    /// Whether every table `wants` asks for is already here, so a new plan of the same statement needs no new read. A
    /// read that failed, perhaps only out of time, is tried again.
    pub fn covers(&self, wants: &[Want]) -> bool {
        !self.failed && wants.iter().all(|w| w.names.iter().all(|(r, a)| self.tables.iter().any(|t| t.names.iter().any(|(tr, ta)| tr == r && ta == a))))
    }

    /// Made-up evidence for the offline demo and the tests, which have no catalog to read: every filtered scan of the plan
    /// on a table of 400,000 rows analyzed two hours ago, with a primary key and an index on created_at.
    pub fn demo(p: &Plan, d: Dialect) -> Evidence {
        let mut tables: Vec<TableEv> = Vec::new();
        for n in p.nodes.iter().filter(|n| reads_all(n) && !n.filter.is_empty()) {
            let table = if n.rel.is_empty() { n.alias.clone() } else { n.rel.clone() };
            if let Some(t) = tables.iter_mut().find(|t| t.table == table) {
                t.names.push((n.rel.clone(), n.alias.clone()));
                continue;
            }
            let ix = |name: String, cols: &[&str], unique| IndexEv { name, cols: cols.iter().map(|c| c.to_string()).collect(), unique, btree: true, ..Default::default() };
            let cols = n.filter_expr.as_ref().map(super::cond::columns).unwrap_or_default().into_iter().map(|name| ColEv { ty: demo_type(&name, d).into(), name, null_frac: Some(0.0), ..Default::default() }).collect();
            tables.push(TableEv {
                names: vec![(n.rel.clone(), n.alias.clone())],
                schema: if d == Dialect::Postgres { "public".into() } else { "shop".into() },
                rows: Some(400_000.0),
                bytes: Some(52.0 * 1024.0 * 1024.0),
                analyzed_ago_s: Some(7200.0),
                mods_since: Some(1200.0),
                indexes: Some(vec![ix(if d == Dialect::Postgres { format!("{table}_pkey") } else { "PRIMARY".into() }, &["id"], true), ix(format!("{table}_created_at_idx"), &["created_at"], false)]),
                cols,
                table,
                ..Default::default()
            });
        }
        let version = if d == Dialect::Postgres { "18.6" } else if d == Dialect::MariaDb { "11.4.3-MariaDB" } else { "8.4.3" };
        Evidence { tables, version: version.into(), ..Default::default() }
    }
}

/// A made-up column's type from its name, so the demo's dates and keys are dates and numbers to the suggestions.
fn demo_type(name: &str, d: Dialect) -> &'static str {
    let pg = d == Dialect::Postgres;
    if name.ends_with("_at") || name.ends_with("date") {
        if pg { "timestamp with time zone" } else { "datetime" }
    } else if name == "id" || name.ends_with("_id") {
        if pg { "bigint" } else { "int" }
    } else if pg {
        "text"
    } else {
        "varchar(64)"
    }
}

/// A node that reads every row of its table: a full scan, or MySQL's full index scan, which a filter on an indexed
/// column it cannot search also leaves reading everything. EXPLAIN ANALYZE's tree names the latter in its label.
pub fn reads_all(n: &PlanNode) -> bool {
    n.access == Access::Scan || (n.access == Access::Other && n.label == "index" && !n.alias.is_empty()) || full_index_scan(&n.label)
}

/// MySQL's tree text for a read of a whole index: "Covering index scan on o using idx", "Index scan on o using idx".
pub fn full_index_scan(label: &str) -> bool {
    label.starts_with("Covering index scan on ") || label.starts_with("Index scan on ")
}

impl TableEv {
    pub fn col(&self, name: &str) -> Option<&ColEv> {
        self.cols.iter().find(|c| c.name.eq_ignore_ascii_case(name))
    }
}

/// A table to read the catalog for: where the statement says it is, and the plan's names for it.
#[derive(Clone, Debug, PartialEq)]
pub struct Want {
    /// The schema the statement writes before the table, when it writes one; else the run's search_path or database.
    pub schema: Option<String>,
    pub table: String,
    pub names: Vec<(String, String)>,
    /// The columns its filters test, whose statistics are read.
    pub cols: Vec<String>,
}

/// A table the statement names in FROM, JOIN, UPDATE or INTO, with the schema it writes and its alias.
#[derive(Clone, Debug, PartialEq)]
pub struct Ref {
    pub schema: Option<String>,
    pub table: String,
    pub alias: Option<String>,
}

/// Words that end a table reference where an alias could stand.
const NOT_ALIAS: &[&str] = &[
    "from", "where", "join", "inner", "left", "right", "full", "outer", "cross", "natural", "on", "using", "group", "order", "limit", "offset", "having", "window", "union", "except", "intersect", "for", "lateral", "set", "values", "select", "straight_join", "use", "force", "ignore", "partition", "tablesample", "returning", "fetch", "into", "as", "when", "then", "with", "default",
];

/// The tables `stmt` names, with WITH names left out: those are the statement's own and no catalog table.
pub fn refs(stmt: &str, d: Dialect) -> Vec<Ref> {
    let toks: Vec<(Tok, &str)> = lex::tokens(stmt, d).into_iter().filter(|(t, _)| !matches!(t, Tok::Space | Tok::Comment | Tok::ExecOpen | Tok::ExecClose)).map(|(t, r)| (t, &stmt[r])).collect();
    let word = |i: usize, w: &str| toks.get(i).is_some_and(|(t, s)| *t == Tok::Word && s.eq_ignore_ascii_case(w));
    let punct = |i: usize, p: &str| toks.get(i).is_some_and(|(t, s)| *t == Tok::Punct && *s == p);
    let name = |i: usize| toks.get(i).and_then(|&(t, s)| ident(t, s, d));
    // `name AS (`, `name AS MATERIALIZED (` and `name (cols) AS (` open a WITH query.
    let mut ctes: Vec<String> = Vec::new();
    for i in 0..toks.len() {
        let Some(n) = name(i) else { continue };
        let mut j = i + 1;
        if punct(j, "(") {
            while j < toks.len() && !punct(j, ")") {
                j += 1;
            }
            j += 1;
        }
        if word(j, "as") && (punct(j + 1, "(") || (word(j + 1, "materialized") && punct(j + 2, "(")) || (word(j + 1, "not") && word(j + 2, "materialized"))) {
            ctes.push(n);
        }
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let from = word(i, "from");
        if !(from || word(i, "join") || word(i, "update") || word(i, "into")) {
            i += 1;
            continue;
        }
        i += 1;
        if word(i, "only") || word(i, "lateral") {
            i += 1;
        }
        while let Some(first) = name(i) {
            if NOT_ALIAS.contains(&first.to_ascii_lowercase().as_str()) && toks[i].0 == Tok::Word {
                break;
            }
            i += 1;
            let (schema, table) = match name(i + 1) {
                Some(t) if punct(i, ".") => {
                    i += 2;
                    (Some(first), t)
                }
                _ => (None, first),
            };
            // A function in FROM, such as generate_series(..), is no table.
            if punct(i, "(") {
                break;
            }
            let mut alias = None;
            if word(i, "as") {
                alias = name(i + 1);
                i += 2;
            } else if let Some(&(t, s)) = toks.get(i) {
                if t != Tok::Word || !NOT_ALIAS.contains(&s.to_ascii_lowercase().as_str()) {
                    alias = ident(t, s, d);
                    if alias.is_some() {
                        i += 1;
                    }
                }
            }
            if !(schema.is_none() && ctes.iter().any(|c| same(c, &table, d))) {
                out.push(Ref { schema, table, alias });
            }
            if from && punct(i, ",") {
                i += 1;
                continue;
            }
            break;
        }
    }
    out
}

/// A name token as the server reads it: PostgreSQL folds a bare name to lower case.
fn ident(t: Tok, s: &str, d: Dialect) -> Option<String> {
    match t {
        Tok::Word if !s.starts_with(|c: char| c.is_ascii_digit() || c == '$' || c == '@') => Some(if d == Dialect::Postgres { s.to_lowercase() } else { s.to_string() }),
        Tok::Quoted => Some(lex::unquote_ident(s)),
        Tok::Str if d == Dialect::Postgres && s.starts_with('"') => Some(lex::unquote_ident(s)),
        _ => None,
    }
}

/// The table the statement means by a node: exactly one reference with the node's name. None when it names none (a
/// view's table, a partition) or several different ones.
pub fn resolve(n: &PlanNode, refs: &[Ref], d: Dialect) -> Option<(Option<String>, String)> {
    let alias_of = |r: &Ref| r.alias.clone().unwrap_or_else(|| r.table.clone());
    let hits: Vec<&Ref> = if d == Dialect::Postgres {
        let alias = if n.alias.is_empty() { &n.rel } else { &n.alias };
        refs.iter().filter(|r| same(&r.table, &n.rel, d) && same(&alias_of(r), alias, d)).collect()
    } else {
        refs.iter().filter(|r| same(&alias_of(r), &n.alias, d)).collect()
    };
    let first = *hits.first()?;
    let differ = |a: &Option<String>, b: &Option<String>| match (a, b) {
        (Some(x), Some(y)) => !same(x, y, d),
        (None, None) => false,
        _ => true,
    };
    if hits.iter().any(|r| differ(&r.schema, &first.schema) || !same(&r.table, &first.table, d)) {
        return None;
    }
    Some((first.schema.clone(), first.table.clone()))
}

/// The MariaDB join buffer step whose condition is about node `i`, the table it buffers.
fn buffered_by(p: &Plan, i: usize) -> Option<&PlanNode> {
    p.nodes[i].parent.map(|j| &p.nodes[j]).filter(|b| b.join_buffer && b.alias.is_empty() && !b.join_cond.is_empty() && b.kids.first() == Some(&i))
}

/// The tables worth reading for `p`: those its filtered full scans read, matched to the statement, at most four. A full
/// scan under a MariaDB join buffer counts too, since the join's condition sits on the buffer step above it.
pub fn wants(p: &Plan, stmt: &str, d: Dialect) -> Vec<Want> {
    let refs = refs(stmt, d);
    let mut out: Vec<Want> = Vec::new();
    for (i, n) in p.nodes.iter().enumerate() {
        let buf = buffered_by(p, i);
        if !reads_all(n) || (n.filter.is_empty() && n.join_cond.is_empty() && buf.is_none()) {
            continue;
        }
        let Some((schema, table)) = resolve(n, &refs, d) else { continue };
        let mut cols: Vec<String> = Vec::new();
        for e in [n.filter_expr.as_ref(), n.join_cond_expr.as_ref(), buf.and_then(|b| b.join_cond_expr.as_ref())].into_iter().flatten() {
            for c in super::cond::columns(e) {
                if !cols.contains(&c) {
                    cols.push(c);
                }
            }
        }
        let name = (n.rel.clone(), n.alias.clone());
        if let Some(w) = out.iter_mut().find(|w| w.schema == schema && same(&w.table, &table, d)) {
            if !w.names.contains(&name) {
                w.names.push(name);
            }
            for c in cols {
                if !w.cols.contains(&c) {
                    w.cols.push(c);
                }
            }
            continue;
        }
        if out.len() == MAX_TABLES {
            break;
        }
        out.push(Want { schema, table, names: vec![name], cols });
    }
    out
}

/// Reads the catalog for `wants`. `schema` is where the statement ran: PostgreSQL resolves an unqualified table on its
/// search_path, MySQL in that database. A part that fails is left empty; nothing here is an error to show.
pub async fn load(db: &Db, schema: &str, wants: &[Want]) -> Evidence {
    let work = async {
        let ext = async {
            if db.dialect != Dialect::Postgres {
                return (false, false);
            }
            let rows = db.meta(schema, super::super::pg::catalog::EXTENSIONS, []).await.unwrap_or_default();
            let has = |n: &str| rows.iter().any(|r| text(r, "name").as_deref() == Some(n));
            (has("hypopg"), has("pg_trgm"))
        };
        let tables = futures_util::future::join_all(wants.iter().map(|w| table_ev(db, schema, w)));
        let ((hypopg, trgm), tables) = tokio::join!(ext, tables);
        // Only every read failing counts as the catalog failing; a view or a missing table just has no evidence.
        let failed = !tables.is_empty() && tables.iter().all(Result::is_err);
        Evidence { tables: tables.into_iter().flatten().flatten().collect(), hypopg, trgm, failed, version: db.version.clone() }
    };
    tokio::time::timeout(Duration::from_millis(LOAD_MS), work).await.unwrap_or(Evidence { failed: true, version: db.version.clone(), ..Default::default() })
}

fn f(r: &SqlRow, name: &str) -> Option<f64> {
    text(r, name).and_then(|s| s.trim().parse().ok())
}

/// The table's evidence; None when the name is not a table there, Err when the catalog could not be read.
async fn table_ev(db: &Db, schema: &str, w: &Want) -> Result<Option<TableEv>, ()> {
    if db.dialect == Dialect::Postgres { pg_table(db, schema, w).await } else { my_table(db, schema, w).await }
}

async fn pg_table(db: &Db, run_schema: &str, w: &Want) -> Result<Option<TableEv>, ()> {
    use super::super::pg::catalog::{EXPLAIN_COLS, EXPLAIN_TABLE};
    let given = w.schema.clone().unwrap_or_default();
    // Under the run's search_path, so an unqualified name resolves to the table the statement read.
    let rows = db.meta_with(run_schema, EXPLAIN_TABLE, [given.as_str(), w.table.as_str()], true).await.map_err(|_| ())?;
    let Some(r) = rows.first() else { return Ok(None) };
    let (Some(schema), Some(table)) = (text(r, "schema"), text(r, "name")) else { return Ok(None) };
    if !matches!(text(r, "kind").as_deref(), Some("r" | "m")) {
        return Ok(None);
    }
    let cols = serde_json::to_string(&w.cols).unwrap_or_else(|_| "[]".into());
    let (ix, cs) = tokio::join!(db.meta(&schema, Dialect::Postgres.indexes(), [schema.as_str(), table.as_str()]), db.meta(&schema, EXPLAIN_COLS, [schema.as_str(), table.as_str(), cols.as_str()]));
    let never = r.int("never") == Some(1);
    Ok(Some(TableEv {
        names: w.names.clone(),
        rows: if never { None } else { f(r, "reltuples") },
        bytes: f(r, "bytes"),
        analyzed_ago_s: f(r, "ago"),
        never_analyzed: never,
        mods_since: f(r, "mods"),
        indexes: ix.ok().map(|rows| super::super::structure::indexes(&rows).iter().map(index_ev).collect()),
        cols: cs.unwrap_or_default().iter().map(pg_col).collect(),
        schema,
        table,
    }))
}

fn pg_col(r: &SqlRow) -> ColEv {
    let list = |k: &str| text(r, k).and_then(|j| serde_json::from_str::<Vec<serde_json::Value>>(&j).ok()).unwrap_or_default();
    let vals: Vec<String> = list("mcv").into_iter().map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).collect();
    let freqs: Vec<f64> = list("mcf").into_iter().filter_map(|v| v.as_f64()).collect();
    ColEv { name: text(r, "name").unwrap_or_default(), ty: text(r, "ty").unwrap_or_default(), null_frac: f(r, "null_frac"), n_distinct: f(r, "n_distinct"), mcv: vals.into_iter().zip(freqs).collect(), correlation: f(r, "corr") }
}

fn index_ev(ix: &super::super::structure::Index) -> IndexEv {
    IndexEv {
        name: ix.name.clone(),
        cols: ix.cols.iter().map(|c| if c.expr.is_some() { String::new() } else { c.name.clone() }).collect(),
        unique: ix.unique,
        btree: ix.kind.eq_ignore_ascii_case("btree"),
        partial: !ix.predicate.is_empty(),
        invalid: ix.invalid,
        invisible: ix.invisible,
    }
}

const MY_TABLE: &str = "SELECT TABLE_TYPE AS kind, TABLE_ROWS AS row_estimate, DATA_LENGTH + INDEX_LENGTH AS bytes FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?";
/// InnoDB's own statistics table, which needs a grant on mysql; without it the age is just not shown.
const MY_STATS_AGE: &str = "SELECT TIMESTAMPDIFF(SECOND, last_update, NOW()) AS ago FROM mysql.innodb_table_stats WHERE database_name = ? AND table_name = ?";
const MY_HIDDEN: &str = "SELECT INDEX_NAME AS name, IS_VISIBLE = 'NO' AS hidden FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND SEQ_IN_INDEX = 1";
const MARIA_HIDDEN: &str = "SELECT INDEX_NAME AS name, IGNORED = 'YES' AS hidden FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND SEQ_IN_INDEX = 1";
/// MySQL 8's histograms, which exist only for columns someone ran ANALYZE TABLE .. UPDATE HISTOGRAM on.
const MY_HISTOGRAMS: &str = "SELECT COLUMN_NAME AS name, HISTOGRAM AS h FROM information_schema.COLUMN_STATISTICS WHERE SCHEMA_NAME = ? AND TABLE_NAME = ?";

async fn my_table(db: &Db, run_db: &str, w: &Want) -> Result<Option<TableEv>, ()> {
    let d = db.dialect;
    let schema = w.schema.clone().unwrap_or_else(|| run_db.to_string());
    let p = [schema.as_str(), w.table.as_str()];
    let rows = db.meta(&schema, MY_TABLE, p).await.map_err(|_| ())?;
    let Some(r) = rows.first() else { return Ok(None) };
    if !matches!(text(r, "kind").as_deref(), Some("BASE TABLE" | "SYSTEM VERSIONED")) {
        return Ok(None);
    }
    let hidden_sql = if d == Dialect::MariaDb { MARIA_HIDDEN } else { MY_HIDDEN };
    let hist = async {
        if d == Dialect::MySql { db.meta(&schema, MY_HISTOGRAMS, p).await.unwrap_or_default() } else { Vec::new() }
    };
    let (ix, hidden, cols, age, hist) = tokio::join!(db.meta(&schema, d.indexes(), p), db.meta(&schema, hidden_sql, p), db.meta(&schema, d.columns(), p), db.meta(&schema, MY_STATS_AGE, p), hist);
    let hidden: Vec<String> = hidden.unwrap_or_default().iter().filter(|r| r.int("hidden").is_some_and(|n| n != 0)).filter_map(|r| text(r, "name")).collect();
    let ix = ix.ok().map(|rows| super::super::structure::indexes(&rows));
    let mut cols: Vec<ColEv> = cols.unwrap_or_default().iter().map(|r| ColEv { name: text(r, "name").unwrap_or_default(), ty: text(r, "full_type").unwrap_or_default(), ..Default::default() }).collect();
    // The first part of an index counts its distinct values, which is all MySQL knows of a column without a histogram.
    for c in &mut cols {
        c.n_distinct = ix.iter().flatten().filter(|i| i.cols.first().is_some_and(|k| k.name.eq_ignore_ascii_case(&c.name) && k.prefix.is_none())).filter_map(|i| i.cardinality).map(|n| n as f64).fold(None, |a: Option<f64>, b| Some(a.map_or(b, |a| a.max(b))));
    }
    for r in &hist {
        let (Some(name), Some(h)) = (text(r, "name"), text(r, "h")) else { continue };
        if let Some(c) = cols.iter_mut().find(|c| c.name.eq_ignore_ascii_case(&name)) {
            histogram(c, &h);
        }
    }
    Ok(Some(TableEv {
        names: w.names.clone(),
        schema,
        table: w.table.clone(),
        rows: f(r, "row_estimate"),
        bytes: f(r, "bytes"),
        analyzed_ago_s: age.ok().and_then(|rows| rows.first().and_then(|r| f(r, "ago"))),
        never_analyzed: false,
        mods_since: None,
        indexes: ix.map(|ix| {
            ix.iter()
                .map(|i| {
                    let mut e = index_ev(i);
                    e.invisible |= hidden.iter().any(|h| *h == i.name);
                    e
                })
                .collect()
        }),
        cols,
    }))
}

/// A MySQL histogram's null share and distinct count: a singleton histogram has one bucket per value, an equi-height
/// one counts the values of each bucket.
fn histogram(c: &mut ColEv, h: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(h) else { return };
    c.null_frac = v.get("null-values").and_then(|x| x.as_f64());
    let buckets = v.get("buckets").and_then(|b| b.as_array()).cloned().unwrap_or_default();
    c.n_distinct = match v.get("histogram-type").and_then(|t| t.as_str()) {
        Some("singleton") => Some(buckets.len() as f64),
        Some("equi-height") => Some(buckets.iter().filter_map(|b| b.get(3).and_then(|n| n.as_f64())).sum()),
        _ => c.n_distinct,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statement_names_with_schemas_and_aliases() {
        let r = refs("SELECT b.id FROM app.bookings b, Events JOIN \"Svc\" AS s ON s.id = b.service_id WHERE x = 1", Dialect::Postgres);
        assert_eq!(r, [Ref { schema: Some("app".into()), table: "bookings".into(), alias: Some("b".into()) }, Ref { schema: None, table: "events".into(), alias: None }, Ref { schema: None, table: "Svc".into(), alias: Some("s".into()) }]);
        let r = refs("SELECT o.id FROM `shop`.`orders` o LEFT JOIN customers c ON c.id = o.customer_id WHERE o.status = 'paid'", Dialect::MySql);
        assert_eq!(r[0], Ref { schema: Some("shop".into()), table: "orders".into(), alias: Some("o".into()) });
        assert_eq!(r[1].alias.as_deref(), Some("c"));
        // A WITH name is the statement's own, and a function in FROM is no table.
        let r = refs("WITH hot AS (SELECT * FROM bookings WHERE total > 5) SELECT * FROM hot, generate_series(1, 3) g", Dialect::Postgres);
        assert_eq!(r.iter().map(|r| r.table.as_str()).collect::<Vec<_>>(), ["bookings"]);
    }

    fn node(rel: &str, alias: &str) -> PlanNode {
        PlanNode { rel: rel.into(), alias: alias.into(), ..Default::default() }
    }

    #[test]
    fn a_node_resolves_to_one_table_or_none() {
        let pg = refs("SELECT * FROM app.bookings b JOIN bookings c ON c.id = b.id", Dialect::Postgres);
        assert_eq!(resolve(&node("bookings", "b"), &pg, Dialect::Postgres), Some((Some("app".into()), "bookings".into())));
        assert_eq!(resolve(&node("bookings", "c"), &pg, Dialect::Postgres), Some((None, "bookings".into())));
        // A table only a view reads is not in the statement.
        assert_eq!(resolve(&node("orders", "orders"), &pg, Dialect::Postgres), None);
        // The same alias for two tables in two parts of a MySQL statement: the plan's alias could be either.
        let my = refs("SELECT * FROM orders o WHERE o.id IN (SELECT o.id FROM archive.orders o)", Dialect::MySql);
        assert_eq!(resolve(&node("", "o"), &my, Dialect::MySql), None);
        let my = refs("SELECT * FROM orders o JOIN customers c ON c.id = o.customer_id", Dialect::MySql);
        assert_eq!(resolve(&node("", "o"), &my, Dialect::MySql), Some((None, "orders".into())));
    }

    #[test]
    fn a_select_into_reads_the_table_after_from() {
        let r = refs("SELECT b.id INTO newtab FROM bookings b WHERE b.status = 'DONE'", Dialect::Postgres);
        assert_eq!(r, [Ref { schema: None, table: "newtab".into(), alias: None }, Ref { schema: None, table: "bookings".into(), alias: Some("b".into()) }]);
    }

    #[test]
    fn a_failed_read_covers_nothing_so_a_rerun_tries_again() {
        let want = Want { schema: None, table: "orders".into(), names: vec![(String::new(), "o".into())], cols: Vec::new() };
        let ev = Evidence { tables: vec![TableEv { names: want.names.clone(), table: "orders".into(), ..Default::default() }], ..Default::default() };
        assert!(ev.covers(std::slice::from_ref(&want)));
        assert!(!Evidence { failed: true, ..ev }.covers(&[want]));
    }

    #[test]
    fn mariadb_asks_for_the_table_under_a_join_buffer() {
        let stmt = "SELECT o.id FROM items i JOIN orders o ON o.ref = i.sku WHERE o.status = 'paid' AND o.total > 899";
        let json = include_str!("fixtures/mariadb-bufjoin.json");
        // The buffered table's own filter dropped: only the buffer step above it holds a condition about it.
        let bare = super::super::parse(Dialect::MariaDb, &json.replace(r#""o.`status` = 'paid' and o.total > 899""#, r#""""#)).unwrap();
        assert!(bare.nodes.iter().all(|n| n.alias != "o" || n.filter.is_empty()), "the fixture's filter was not dropped");
        let w = wants(&bare, stmt, Dialect::MariaDb);
        assert_eq!(w.iter().map(|w| (w.table.as_str(), w.names.clone())).collect::<Vec<_>>(), [("orders", vec![(String::new(), "o".to_string())])]);
        assert!(w[0].cols.iter().any(|c| c == "ref"), "{:?}", w[0].cols);
        // With its filter too, the join column is read as well as the filter's.
        let w = wants(&super::super::parse(Dialect::MariaDb, json).unwrap(), stmt, Dialect::MariaDb);
        let o = w.iter().find(|w| w.table == "orders").unwrap();
        assert!(["status", "total", "ref"].iter().all(|c| o.cols.iter().any(|x| x == c)), "{:?}", o.cols);
    }

    #[test]
    fn a_histogram_gives_nulls_and_distinct_values() {
        let mut c = ColEv::default();
        histogram(&mut c, r#"{"buckets": [[1, 0.2], [2, 0.5], [3, 0.9]], "null-values": 0.1, "histogram-type": "singleton"}"#);
        assert_eq!((c.null_frac, c.n_distinct), (Some(0.1), Some(3.0)));
        histogram(&mut c, r#"{"buckets": [[1, 10, 0.5, 10], [11, 40, 1.0, 25]], "null-values": 0.0, "histogram-type": "equi-height"}"#);
        assert_eq!(c.n_distinct, Some(35.0));
    }
}
