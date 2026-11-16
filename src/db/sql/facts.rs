//! What the filter panel asks of a table beside its rows: how many rows a WHERE matches while it is being built, and what
//! a column holds (enum labels, its range, JSON keys, array items) for the value cells. Every read is short, on a
//! connection of its own, and never in the query log, because it runs on its own while someone types.

use super::exec::{Conn, SqlRow};
use super::value::Kind;
use super::worker::{gate, session, text_at};
use super::{se_at, Db, Dialect, PageQuery};
use crate::db::{e, Msg, Res, Worker};
use sqlx::{Connection, Executor};

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct FilterCount {
    pub n: u64,
    /// The planner's estimate, for a table too big to count while someone types.
    pub estimated: bool,
}

#[derive(Clone, PartialEq, Debug)]
pub struct FilterErr {
    pub msg: String,
    /// A byte offset into the WHERE text, when the server says where.
    pub at: Option<usize>,
}

/// What the value cells can offer for a column. Each part is empty when reading it would cost too much.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct ColFacts {
    /// An enum's labels in their order (PostgreSQL; MySQL's are in the column type).
    pub labels: Vec<String>,
    /// MIN and MAX as text, and AVG for a number column.
    pub range: Option<(String, String, Option<String>)>,
    /// Top-level keys of JSON objects, from a sample.
    pub keys: Vec<String>,
    /// Array items, from a sample (PostgreSQL).
    pub items: Vec<String>,
}

/// An exact count gives up after this long and the estimate stands in.
const COUNT_MS: u64 = 2000;
/// Facts are suggestions, so they may not hold anything up.
const FACTS_MS: u64 = 300;

/// Runs `sqls` one after the other on a connection of its own with a statement limit of `ms`, in the session the grid
/// reads in (zone and sql_mode on MySQL, a read-only transaction on PostgreSQL), and gives each its own answer.
async fn short_reads(db: &Db, ms: u64, sqls: &[String]) -> Res<Vec<Result<Vec<SqlRow>, sqlx::Error>>> {
    let mut out = Vec::new();
    if db.dialect == Dialect::Postgres {
        let mut c = db.detached().await?;
        let Conn::Pg(pc) = &mut c else { return Err("Not a PostgreSQL connection".into()) };
        // A SET LOCAL inside each read-only transaction, never a session value, because a pooler hands the session on.
        let limit = super::pg::set_timeout(&ms.to_string());
        for sql in sqls {
            out.push(super::pg::read::in_read_only(pc, db, &limit, async |c| c.fetch_all(sqlx::AssertSqlSafe(sql.as_str())).await).await.map(SqlRow::wrap_pg));
        }
        let _ = c.close().await;
    } else {
        let mut c = super::run::side_conn(db, "Could not reach the server").await?.into_my()?;
        session(&mut c, ms, &db.setup).await.map_err(|x| db.setup.failed_text().unwrap_or_else(|| e(x)))?;
        // The init command may raise the statement limit.
        let t = db.dialect.session_timeout(ms);
        let _ = c.execute(sqlx::AssertSqlSafe(t.as_str())).await;
        for sql in sqls {
            out.push(c.fetch_all(sqlx::AssertSqlSafe(sql.as_str())).await.map(SqlRow::wrap));
        }
        let _ = c.close().await;
    }
    Ok(out)
}

/// The server stopped the statement at its time limit.
fn timed_out(err: &sqlx::Error) -> bool {
    let Some(d) = err.as_database_error() else { return false };
    d.code().is_some_and(|c| c == "57014") || d.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>().is_some_and(|m| matches!(m.number(), 3024 | 1969))
}

/// `err` worded for the WHERE text, which starts `pre` bytes into `sql`, with where it points when the server says.
fn filter_err(err: &sqlx::Error, sql: &str, pre: usize, filter: &str) -> FilterErr {
    let d = err.as_database_error();
    let full = if let Some(pg) = d.and_then(|d| d.try_downcast_ref::<sqlx::postgres::PgDatabaseError>()) {
        match pg.position() {
            // Characters from 1.
            Some(sqlx::postgres::PgErrorPosition::Original(p)) => sql.char_indices().nth(p.saturating_sub(1)).map(|(i, _)| i),
            _ => None,
        }
    } else if let Some(d) = d.filter(|d| d.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>().is_some_and(|m| m.number() == 1064)) {
        Some(super::lint::near_at(sql, d.message()))
    } else {
        None
    };
    let at = full.and_then(|a| a.checked_sub(pre)).filter(|a| *a <= filter.len());
    FilterErr { msg: se_at(err, filter, sql[..pre].chars().count()), at }
}

/// How many rows match `filter`: exact on a table small enough to count while someone types, else the planner's
/// estimate, which is also what a count that runs past its limit falls back to.
pub(super) async fn filter_count(db: &Db, schema: &str, table: &str, filter: &str) -> Result<FilterCount, FilterErr> {
    let d = db.dialect;
    let plain = |msg: String| FilterErr { msg, at: None };
    let f = filter.trim();
    let count = PageQuery { filter: f.to_string(), ..Default::default() }.count_sql(d, schema, table);
    super::worker::one_read(&count, d).map_err(plain)?;
    let small = gate(db, schema, table, "").await.map_err(plain)?.is_some_and(|g| g.small);
    // Both statements end with the WHERE text and its closing line.
    let pre = |sql: &str| sql.len() - f.len() - if f.is_empty() { 0 } else { 2 };
    if small {
        let mut res = short_reads(db, COUNT_MS, std::slice::from_ref(&count)).await.map_err(plain)?;
        match res.remove(0) {
            Ok(rows) => return Ok(FilterCount { n: rows.first().and_then(|r| text_at(r, 0)).and_then(|n| n.parse().ok()).unwrap_or(0), estimated: false }),
            Err(x) if timed_out(&x) => {}
            Err(x) => return Err(filter_err(&x, &count, pre(&count), f)),
        }
    }
    let head = if d == Dialect::Postgres { "EXPLAIN (FORMAT JSON)" } else { "EXPLAIN FORMAT=JSON" };
    let mut sql = format!("{head} SELECT 1 FROM {}", d.qualified(schema, table));
    if !f.is_empty() {
        sql += &format!("\nWHERE (\n{f}\n)");
    }
    let mut res = short_reads(db, COUNT_MS, std::slice::from_ref(&sql)).await.map_err(plain)?;
    match res.remove(0) {
        Ok(rows) => {
            let plan = rows.first().and_then(|r| text_at(r, 0)).unwrap_or_default();
            let n = super::plan::est_rows(d, &plan).ok_or_else(|| plain("The server's plan gave no row estimate".into()))?;
            Ok(FilterCount { n, estimated: true })
        }
        Err(x) => Err(filter_err(&x, &sql, pre(&sql), f)),
    }
}

/// A PostgreSQL column's enum labels in their order, through a domain over an enum too.
const ENUM_LABELS: &str = "SELECT e.enumlabel::text AS v FROM pg_attribute a \
    JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
    JOIN pg_type t ON t.oid = a.atttypid \
    JOIN pg_enum e ON e.enumtypid = CASE WHEN t.typtype = 'd' THEN t.typbasetype ELSE t.oid END \
    WHERE a.attname = $1 AND n.nspname = $2 AND c.relname = $3 ORDER BY e.enumsortorder";

/// Rows read in a sample for JSON keys and array items: enough to suggest, few enough to stay quick on any table.
const SAMPLE: u32 = 1000;

pub(super) async fn col_facts(db: &Db, schema: &str, table: &str, column: &str, kind: Kind) -> Res<ColFacts> {
    let d = db.dialect;
    let pg = d == Dialect::Postgres;
    let mut f = ColFacts::default();
    if pg && matches!(kind, Kind::Enum | Kind::Text) {
        f.labels = db.meta(schema, ENUM_LABELS, [column, schema, table]).await?.iter().filter_map(|r| text_at(r, 0)).collect();
    }
    let c = d.quote_ident(column);
    let t = d.qualified(schema, table);
    let as_text = |x: &str| if pg { format!("({x})::text") } else { format!("CAST({x} AS CHAR)") };
    let group = super::rules::group_of(kind);
    let mut sqls = Vec::new();
    let ranged = matches!(group, super::rules::Group::Number | super::rules::Group::Date);
    if ranged && let Some(g) = gate(db, schema, table, column).await? {
        // MIN and MAX read the ends of an index that leads with the column; AVG reads every row.
        let avg = g.small && group == super::rules::Group::Number;
        if g.small || g.leads {
            let mut s = format!("SELECT {}, {}", as_text(&format!("MIN({c})")), as_text(&format!("MAX({c})")));
            if avg {
                s += &format!(", {}", as_text(&format!("AVG({c})")));
            }
            sqls.push(("range", s + &format!(" FROM {t}")));
        }
    }
    if kind == Kind::Json {
        sqls.push(("keys", if pg {
            format!("SELECT DISTINCT k FROM (SELECT jsonb_object_keys({c}::jsonb) AS k FROM (SELECT {c} FROM {t} WHERE jsonb_typeof({c}::jsonb) = 'object' LIMIT {SAMPLE}) s) q ORDER BY 1 LIMIT 200")
        } else {
            // Merged here: MySQL has no set-returning form of JSON_KEYS before JSON_TABLE, which MariaDB reads differently.
            format!("SELECT JSON_KEYS({c}) FROM {t} WHERE JSON_TYPE({c}) = 'OBJECT' LIMIT {SAMPLE}")
        }));
    }
    if pg && kind == Kind::Array {
        sqls.push(("items", format!("SELECT DISTINCT x FROM (SELECT unnest({c}::text[]) AS x FROM (SELECT {c} FROM {t} LIMIT {SAMPLE}) s) q ORDER BY 1 LIMIT 200")));
    }
    if sqls.is_empty() {
        return Ok(f);
    }
    let texts: Vec<String> = sqls.iter().map(|(_, s)| s.clone()).collect();
    for ((what, _), res) in sqls.iter().zip(short_reads(db, FACTS_MS, &texts).await?) {
        // A part that times out or fails is a suggestion less, not an error.
        let Ok(rows) = res else { continue };
        match *what {
            "range" => {
                f.range = rows.first().and_then(|r| Some((text_at(r, 0)?, text_at(r, 1)?, text_at(r, 2))));
            }
            "keys" if pg => f.keys = rows.iter().filter_map(|r| text_at(r, 0)).collect(),
            "keys" => {
                let mut keys: Vec<String> = rows.iter().filter_map(|r| serde_json::from_str::<Vec<String>>(&text_at(r, 0)?).ok()).flatten().collect();
                keys.sort();
                keys.dedup();
                keys.truncate(200);
                f.keys = keys;
            }
            _ => f.items = rows.iter().filter_map(|r| text_at(r, 0)).collect(),
        }
    }
    Ok(f)
}

impl Worker {
    /// How many rows `filter` matches on a table, for the filter panel. `req` matches the answer to the ask.
    pub fn sql_filter_count(&self, req: u64, db: Db, schema: String, table: String, filter: String) {
        self.spawn(async move { Msg::SqlFilterCount { req, res: filter_count(&db, &schema, &table, &filter).await } });
    }

    /// What the value cells can suggest for a column.
    pub fn sql_col_facts(&self, conn: u64, db: Db, schema: String, table: String, column: String, kind: Kind) {
        self.spawn(async move {
            let database = db.pg_database();
            let res = col_facts(&db, &schema, &table, &column, kind).await;
            Msg::SqlFacts { conn, database, schema, table, column, res }
        });
    }
}
