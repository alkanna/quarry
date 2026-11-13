//! What AI agents read from SQL servers: databases, tables, a table's structure, rows through `select_rows`, and
//! read-only SQL through `query_sql`. Here rather than in `db::agent` because it reuses helpers private to `db::sql`.
//! Every answer is scrubbed by the caller's rules before it is turned into text.

use super::exec::Cells;
use super::run::{run, RunHandle};
use super::value::{self, Kind};
use super::worker::{pg_columns, pg_kind, text, to_row};
use super::{Col, Db, Dialect};
use crate::agents::config::{Limits, Scope};
use crate::agents::policy::visible;
use crate::agents::scrub::Rules;
use crate::agents::tools::{Call, Cond, CondOp};
use crate::db::agent::{failure, page_out, too_big, AgentJob, AgentUndo, Answer, Snapshot};
use crate::db::Res;
use mongodb::bson::{Bson, Document};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

/// Schemas a free SQL statement may never name, so they are not listed either. information_schema stays: it shows
/// names and types only, and the server limits it to what the login may see.
const MY_HIDDEN: [&str; 3] = ["mysql", "performance_schema", "sys"];

/// The databases of a MySQL server, or the schemas of the connected PostgreSQL database, that the scope shares.
pub(in crate::db) async fn databases(db: &Db, scope: &Scope) -> Res<Answer> {
    let rows = if db.dialect == Dialect::Postgres {
        db.meta("", db.dialect.schemas(), []).await?
    } else {
        db.meta("", "SELECT SCHEMA_NAME AS name FROM information_schema.SCHEMATA ORDER BY SCHEMA_NAME", []).await?
    };
    let names: Vec<String> = rows.iter().filter_map(|r| text(r, "name")).filter(|n| db.dialect == Dialect::Postgres || !MY_HIDDEN.contains(&n.to_ascii_lowercase().as_str())).filter(|n| visible(scope, n, None).unwrap_or(false)).collect();
    let key = if db.dialect == Dialect::Postgres { "schemas" } else { "databases" };
    let n = names.len();
    let mut v = json!({ key: names });
    if db.dialect == Dialect::Postgres {
        v["database"] = json!(db.pg_database());
    }
    Ok(Answer::new(v, format!("{n} {key}")))
}

/// Tables and views of one database (MySQL) or schema (PostgreSQL) that the scope shares.
pub(in crate::db) async fn tables(db: &Db, schema: &str, scope: &Scope) -> Res<Answer> {
    let rows = db.meta(schema, db.dialect.tables(), [schema]).await?;
    let mut tables = Vec::new();
    let mut views = Vec::new();
    for r in &rows {
        let Some(n) = text(r, "name") else { continue };
        if !visible(scope, schema, Some(&n)).unwrap_or(false) {
            continue;
        }
        if text(r, "kind").is_some_and(|k| k.contains("VIEW")) {
            views.push(n);
        } else {
            tables.push(n);
        }
    }
    let n = tables.len() + views.len();
    Ok(Answer::new(json!({ "tables": tables, "views": views }), format!("{n} tables and views")))
}

/// Columns with their types and whether rules hide or mask them, the primary key, foreign keys, indexes and the row
/// estimate. Defaults and index definitions are text a person wrote, so they pass through the value rules.
pub(in crate::db) async fn describe(db: &Db, schema: &str, table: &str, rules: &Rules) -> Res<Answer> {
    let nsr = rules.for_ns(schema, table);
    let st = super::structure::load(db, schema, table).await.map_err(|e| failure(&e, &nsr, "describe the table"))?;
    if st.columns.is_empty() {
        return Err(format!("No table {schema}.{table}, or this login cannot see its columns."));
    }
    let columns: Vec<Value> = st
        .columns
        .iter()
        .map(|c| {
            let mut v = json!({ "name": c.name, "type": c.full_type, "nullable": c.nullable });
            if let Some(d) = &c.default {
                v["default"] = json!(nsr.scrub_text(d));
            }
            if nsr.hides(&c.name) {
                v["hidden"] = json!(true);
            } else if nsr.masks(&c.name) {
                v["masked"] = json!(true);
            }
            v
        })
        .collect();
    let primary = st.indexes.iter().find(|i| i.primary || i.name == "PRIMARY").map(|i| i.cols.iter().map(|c| c.name.clone()).collect::<Vec<_>>()).unwrap_or_default();
    let fks: Vec<Value> = st.foreign_keys.iter().map(|f| json!({ "name": f.name, "columns": f.cols, "references": format!("{}.{}", f.ref_db, f.ref_table), "referenced_columns": f.ref_cols })).collect();
    let indexes: Vec<Value> = st
        .indexes
        .iter()
        .map(|i| {
            let mut v = json!({ "name": i.name, "unique": i.unique, "columns": i.cols.iter().map(|c| c.expr.clone().map(|e| nsr.scrub_text(&e)).unwrap_or_else(|| c.name.clone())).collect::<Vec<_>>() });
            if !i.def.is_empty() {
                // A partial index states its condition with literals from the data.
                v["definition"] = json!(nsr.scrub_text(&i.def));
            }
            v
        })
        .collect();
    let v = json!({ "table": format!("{schema}.{table}"), "columns": columns, "primary_key": primary, "foreign_keys": fks, "indexes": indexes, "estimated_rows": st.status.rows });
    Ok(Answer::new(v, format!("{} columns", st.columns.len())))
}

/// A literal Quarry writes itself: numbers as numbers, text quoted by the engine's own rules. The text protocol answers
/// in the server's text, which `to_row` reads; a bound statement would answer in the binary protocol.
fn lit(db: &Db, v: &Value) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.into(),
        Value::Number(n) => n.to_string(),
        Value::String(s) if db.dialect == Dialect::Postgres => value::pg_lit(s),
        Value::String(s) => value::lit(s, db.backslash_escapes()),
        other => value::lit(&other.to_string(), db.backslash_escapes()),
    }
}

/// The WHERE text of `select_rows`: each test on a quoted column, values written by `lit`.
pub(in crate::db) fn where_sql(db: &Db, wher: &[Cond]) -> String {
    let d = db.dialect;
    let tests: Vec<String> = wher
        .iter()
        .map(|c| {
            let col = d.quote_ident(&c.col);
            let op = match c.op {
                CondOp::Eq => "=",
                CondOp::Ne => "<>",
                CondOp::Lt => "<",
                CondOp::Le => "<=",
                CondOp::Gt => ">",
                CondOp::Ge => ">=",
                CondOp::IsNull => return format!("{col} IS NULL"),
                CondOp::NotNull => return format!("{col} IS NOT NULL"),
                CondOp::In => {
                    let list: Vec<String> = c.value.as_array().map(|a| a.iter().map(|v| lit(db, v)).collect()).unwrap_or_default();
                    return format!("{col} IN ({})", list.join(", "));
                }
                CondOp::Like if d == Dialect::Postgres => return format!("{col}::text LIKE {}", lit(db, &c.value)),
                CondOp::Like => "LIKE",
            };
            format!("{col} {op} {}", lit(db, &c.value))
        })
        .collect();
    if tests.is_empty() { String::new() } else { format!(" WHERE {}", tests.join(" AND ")) }
}

/// The columns of a table in their order, as result columns, from the catalog.
async fn table_cols(db: &Db, schema: &str, table: &str) -> Res<Vec<Col>> {
    let d = db.dialect;
    let rows = if d == Dialect::Postgres { pg_columns(db, schema, table).await? } else { db.meta(schema, d.columns(), [schema, table]).await? };
    Ok(rows
        .iter()
        .filter_map(|r| {
            let name = text(r, "name")?;
            let full = text(r, "full_type")?;
            let kind = if d == Dialect::Postgres { pg_kind(r) } else { Kind::from_column_type(&full) };
            Some(Col::plain(&name, &full, kind, r.int("nullable").map(|n| n != 0).unwrap_or(true)))
        })
        .collect())
}

/// Rows of one table from structured arguments: quoted names, literals Quarry writes, a row cap of its own.
#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn select_rows(db: &Db, schema: &str, table: &str, columns: &[String], wher: &[Cond], order: &[(String, bool)], limit: Option<u32>, offset: u64, rules: &Rules, limits: &Limits) -> Res<Answer> {
    let d = db.dialect;
    let nsr = rules.for_ns(schema, table);
    let all = table_cols(db, schema, table).await.map_err(|e| failure(&e, &nsr, "read the table's columns"))?;
    if all.is_empty() {
        return Err(format!("No table {schema}.{table}, or this login cannot see its columns."));
    }
    let known = |c: &str| all.iter().any(|x| x.name == c);
    if let Some(bad) = columns.iter().chain(wher.iter().map(|c| &c.col)).chain(order.iter().map(|o| &o.0)).find(|c| !known(c)) {
        return Err(format!("{schema}.{table} has no column `{bad}`. Call describe for its columns."));
    }
    let cols: Vec<Col> = if columns.is_empty() { all } else { columns.iter().filter_map(|c| all.iter().find(|x| x.name == *c).cloned()).collect() };
    let want = limit.unwrap_or(limits.rows).min(limits.rows).max(1) as u64;
    let list: Vec<String> = cols.iter().map(|c| d.quote_ident(&c.name)).collect();
    let mut sql = format!("SELECT {} FROM {}{}", list.join(", "), d.qualified(schema, table), where_sql(db, wher));
    if !order.is_empty() {
        let o: Vec<String> = order.iter().map(|(c, desc)| format!("{}{}", d.quote_ident(c), if *desc { " DESC" } else { "" })).collect();
        sql.push_str(&format!(" ORDER BY {}", o.join(", ")));
    }
    // One row past the cap says whether more match.
    sql.push_str(&format!(" {}", d.page(want + 1, offset)));
    // The text carries the agent's values, so on PostgreSQL its connection is closed afterwards like the user's own text.
    let rows = db.read_text(schema, &sql, true).await.map_err(|e| failure(&e, &nsr, "read the rows"))?;
    let docs: Vec<Document> = rows.iter().map(|r| to_row(&cols, r, &[]).and_then(|r| r.decode())).collect::<Res<_>>()?;
    Ok(page_out(docs, want as usize, &nsr, limits, "rows"))
}

/// Read-only SQL through the same path as the user's Custom Query on a locked connection: the lexer gate per statement,
/// and on PostgreSQL a read-only transaction rolled back after each one. Rows of free SQL belong to no one table, so
/// every rule applies to them.
pub(in crate::db) async fn query_sql(db: &Db, schema: &str, sql: &str, limit: Option<u32>, rules: &Rules, limits: &Limits, h: &RunHandle) -> Res<Answer> {
    free_sql(db, schema, sql, true, limit, rules, limits, h, "query_sql runs read-only statements only (SELECT, SHOW, EXPLAIN without ANALYZE), so this one was not run.").await
}

/// `run_sql`: statements that may write, through the user's own script path. On a locked connection it runs read-only
/// like the user's would, and a statement that writes is refused there. Nothing it does can be undone.
pub(in crate::db) async fn run_sql(db: &Db, schema: &str, sql: &str, read_only: bool, rules: &Rules, limits: &Limits, h: &RunHandle) -> Res<Answer> {
    let mut a = free_sql(db, schema, sql, read_only, None, rules, limits, h, "The connection is locked for writes in Quarry, so this statement was not run. Only the user can unlock it.").await?;
    a.value["undo"] = json!(false);
    Ok(a)
}

/// Free SQL, statement by statement on a connection of its own. Rows of free SQL belong to no one table, so every rule
/// applies to them, and statements and messages go through the value rules. `refused` replaces the lexer's own words,
/// which tell a person to unlock, something an agent cannot do.
#[allow(clippy::too_many_arguments)]
async fn free_sql(db: &Db, schema: &str, sql: &str, read_only: bool, limit: Option<u32>, rules: &Rules, limits: &Limits, h: &RunHandle, refused: &str) -> Res<Answer> {
    let nsr = rules.all();
    let sets = run(db, schema, sql, read_only, &h.conn, &h.cancel, 0).await;
    h.conn.store(0, Ordering::SeqCst);
    let multi = sets.len() > 1;
    if let Some((i, s, e)) = sets.iter().enumerate().find_map(|(i, s)| s.error.as_deref().map(|e| (i, s, e))) {
        let msg = if s.refused { refused.to_string() } else { failure(e, &nsr, "run the statement") };
        // Earlier statements ran and stay: free SQL has no undo.
        let before = if read_only { String::new() } else { ran_before(i) };
        return Err(if multi { format!("Statement {}: {msg}{before}", i + 1) } else { msg });
    }
    let want = limit.unwrap_or(limits.rows).min(limits.rows).max(1) as usize;
    let mut results = Vec::new();
    let mut summary = Vec::new();
    let (mut count, mut bytes) = (0u64, 0u64);
    for s in sets {
        if s.cols.is_empty() {
            results.push(json!({ "statement": nsr.scrub_text(&s.stmt), "affected": s.affected }));
            continue;
        }
        let more = s.capped || s.page.is_some_and(|p| p.next);
        let docs: Vec<Document> = s.rows.iter().map(|r| r.decode()).collect::<Res<_>>()?;
        let mut a = page_out(docs, want, &nsr, limits, "rows");
        if more && a.value["more"] == json!(false) {
            a.value["more"] = json!(true);
            a.value["note"] = json!(format!("More rows match than the {} returned. Add a LIMIT and OFFSET, or narrow the WHERE.", a.count.unwrap_or(0)));
        }
        count += a.count.unwrap_or(0);
        bytes += a.bytes;
        summary.push(a.summary.clone());
        let mut v = a.value;
        v["statement"] = json!(nsr.scrub_text(&s.stmt));
        v["columns"] = json!(s.cols.iter().map(|c| json!({ "name": c.name, "type": c.ty })).collect::<Vec<_>>());
        results.push(v);
    }
    let mut a = Answer::new(json!({ "results": results }), if summary.is_empty() { "ran".into() } else { summary.join(", ") });
    (a.count, a.bytes) = (Some(count), bytes);
    Ok(a)
}

/// What a failed statement leaves behind when earlier ones ran: they stay, since free SQL has no undo.
fn ran_before(i: usize) -> String {
    match i {
        0 => String::new(),
        1 => " The statement before it ran.".into(),
        n => format!(" The {n} statements before it ran."),
    }
}

/// A value from an agent's JSON as the write code expects it from the grid: text, whole numbers, decimals, booleans or
/// NULL. Each engine's binding converts it to the column's type.
fn json_bson(v: &Value) -> Bson {
    match v {
        Value::Null => Bson::Null,
        Value::Bool(b) => Bson::Boolean(*b),
        Value::Number(n) => n.as_i64().map(Bson::Int64).or_else(|| n.as_u64().map(|u| Bson::String(u.to_string()))).unwrap_or_else(|| Bson::Double(n.as_f64().unwrap_or(0.0))),
        Value::String(s) => Bson::String(s.clone()),
        other => Bson::String(other.to_string()),
    }
}

fn count_of(v: Option<&Bson>) -> u64 {
    match v {
        Some(Bson::Int32(n)) => *n as u64,
        Some(Bson::Int64(n)) => *n as u64,
        Some(Bson::String(s)) => s.parse().unwrap_or(0),
        Some(Bson::Double(f)) => *f as u64,
        _ => 0,
    }
}

/// The table and the arguments of a SQL row tool.
fn row_call(call: &Call) -> Option<(&str, &str)> {
    match call {
        Call::SqlInsertRows { ns, .. } | Call::SqlUpdateRows { ns, .. } | Call::SqlDeleteRows { ns, .. } => Some((&ns.db, &ns.coll)),
        _ => None,
    }
}

/// Reads what a SQL row tool would change: the table's columns and primary key, and for an update or delete the rows the
/// where matches, up to the undo cap and one more. A table without a primary key can't have its rows told apart, so its
/// changes can't be undone.
pub(in crate::db) async fn prepare(db: &Db, call: &Call, l: &Limits) -> Res<Snapshot> {
    let d = db.dialect;
    let Some((schema, table)) = row_call(call) else { return Err("Not a SQL row change.".into()) };
    let cols = table_cols(db, schema, table).await?;
    if cols.is_empty() {
        return Err(format!("No table {schema}.{table}, or this login cannot see its columns."));
    }
    let known = |c: &str| cols.iter().any(|x| x.name == c);
    let named: Vec<&String> = match call {
        Call::SqlInsertRows { rows, .. } => rows.iter().flat_map(|r| r.keys()).collect(),
        Call::SqlUpdateRows { set, wher, .. } => set.keys().chain(wher.iter().map(|c| &c.col)).collect(),
        Call::SqlDeleteRows { wher, .. } => wher.iter().map(|c| &c.col).collect(),
        _ => Vec::new(),
    };
    if let Some(bad) = named.into_iter().find(|c| !known(c)) {
        return Err(format!("{schema}.{table} has no column `{bad}`. Call describe for its columns."));
    }
    let pk = super::write::primary_key(db, schema, table).await?;
    let no_key = || format!("{schema}.{table} has no primary key, so Quarry can't tell its rows apart to undo this change.");
    if let Call::SqlInsertRows { rows, .. } = call {
        let matched = rows.len() as u64;
        let bytes = rows.iter().map(|r| Value::Object(r.clone()).to_string().len() as u64).sum();
        let no_undo = if pk.is_empty() { Some(no_key()) } else { too_big(matched, bytes, l, "rows") };
        return Ok(Snapshot { docs: None, matched, no_undo, table: Some((cols, pk)) });
    }
    let wher = match call {
        Call::SqlUpdateRows { wher, .. } | Call::SqlDeleteRows { wher, .. } => where_sql(db, wher),
        _ => String::new(),
    };
    let from = format!("{}{wher}", d.qualified(schema, table));
    let count = || async {
        let n = Col::plain("n", "bigint", Kind::Int, false);
        let rows = db.read_text(schema, &format!("SELECT COUNT(*) AS n FROM {from}"), true).await?;
        let d = rows.first().map(|r| to_row(std::slice::from_ref(&n), r, &[]).and_then(|r| r.decode())).transpose()?.unwrap_or_default();
        Ok::<u64, String>(count_of(d.get("n")))
    };
    if pk.is_empty() {
        return Ok(Snapshot { docs: None, matched: count().await?, no_undo: Some(no_key()), table: Some((cols, pk)) });
    }
    let cap = l.undo_docs as u64;
    let list: Vec<String> = cols.iter().map(|c| d.quote_ident(&c.name)).collect();
    let sql = format!("SELECT {} FROM {from} {}", list.join(", "), d.page(cap + 1, 0));
    let rows = db.read_text(schema, &sql, true).await?;
    let docs: Vec<Document> = rows.iter().map(|r| to_row(&cols, r, &pk).and_then(|r| r.decode())).collect::<Res<_>>()?;
    let matched = if docs.len() as u64 > cap { count().await? } else { docs.len() as u64 };
    let update = matches!(call, Call::SqlUpdateRows { .. });
    let bytes = docs.iter().map(crate::db::agent::doc_bytes).sum::<u64>() * if update { 2 } else { 1 };
    let no_undo = too_big(matched, bytes, l, "rows");
    Ok(Snapshot { docs: no_undo.is_none().then_some(docs), matched, no_undo, table: Some((cols, pk)) })
}

/// Runs a SQL row tool. With the snapshot it goes through the grid's own write code (`apply_updates`, `apply_deletes`,
/// `apply_inserts`), so it checks each row is unchanged and its undo is the grid's `SqlUndo`. Without it (no primary key,
/// or the user allowed it without undo) one statement runs on the where.
pub(in crate::db) async fn write(db: &Db, job: &AgentJob) -> Res<Answer> {
    use super::write::{action, apply_deletes, apply_inserts, apply_updates, pk_of, RowEdit, SqlUndo};
    let d = db.dialect;
    let Some((schema, table)) = row_call(&job.call) else { return Err("Not a SQL row change.".into()) };
    let nsr = job.rules.for_ns(schema, table);
    let read_only = !job.writable;
    let snap = job.snap.as_ref().ok_or("Quarry did not read the table before this change.")?;
    let (cols, pk) = snap.table.as_ref().ok_or("Quarry did not read the table before this change.")?;
    let rows = snap.docs.as_ref().filter(|_| job.keep_undo && !pk.is_empty());
    let entry = |u: SqlUndo, ids| {
        let mut a = action(db.pg_database(), ids, job.conn, schema, table, cols, u);
        a.agent = Some(job.activity);
        AgentUndo::Sql(a)
    };
    let kept = "Undo call in Quarry's Agent activity can revert this.";
    match (&job.call, rows) {
        (Call::SqlInsertRows { rows, .. }, _) => {
            let new: Vec<super::write::NewRow> = rows.iter().map(|r| r.iter().map(|(k, v)| (k.clone(), json_bson(v))).collect()).collect();
            let (keys, ids) = apply_inserts(db, schema, table, cols, pk, &new, read_only, false).await.map_err(|e| failure(&e, &nsr, "insert the rows"))?;
            let n = new.len();
            let known: Option<Vec<Document>> = keys.into_iter().collect();
            let undo = known.filter(|_| job.keep_undo && !pk.is_empty()).map(|k| entry(if d == Dialect::Postgres { SqlUndo::Inserted(k) } else { SqlUndo::Insert(k) }, ids));
            let mut v = json!({ "inserted": n, "undo": undo.is_some() });
            if undo.is_some() {
                v["note"] = json!(kept);
            }
            let mut a = Answer::new(v, format!("{n} inserted"));
            a.count = Some(n as u64);
            a.undo = undo;
            Ok(a)
        }
        (Call::SqlUpdateRows { set, .. }, Some(rows)) => {
            let edits: Vec<RowEdit> = rows.iter().map(|r| RowEdit { pk: pk_of(r), changes: set.iter().map(|(c, v)| (c.clone(), r.get(c).cloned().unwrap_or(Bson::Null), json_bson(v))).collect() }).collect();
            let n = edits.len();
            let undo = if n == 0 { None } else { Some(entry(SqlUndo::Update(edits.clone()), apply_updates(db, schema, table, cols, pk, &edits, read_only).await.map_err(|e| failure(&e, &nsr, "update the rows"))?)) };
            let mut a = Answer::new(json!({ "updated": n, "undo": n > 0, "note": if n > 0 { kept } else { "" } }), format!("{n} changed"));
            a.count = Some(n as u64);
            a.undo = undo;
            Ok(a)
        }
        (Call::SqlDeleteRows { .. }, Some(rows)) => {
            let pks: Vec<Document> = rows.iter().map(pk_of).collect();
            let (gone, ids) = if pks.is_empty() { (Vec::new(), Default::default()) } else { apply_deletes(db, schema, table, cols, pk, &pks, read_only).await.map_err(|e| failure(&e, &nsr, "delete the rows"))? };
            let n = gone.len();
            let skipped = rows.len() - n;
            let kept_rows: Vec<Document> = rows.iter().filter(|r| gone.contains(&pk_of(r))).cloned().collect();
            let mut v = json!({ "deleted": n, "undo": n > 0 });
            let mut note = Vec::new();
            if skipped > 0 {
                note.push(format!("{skipped} rows were already gone when Quarry deleted."));
            }
            if n > 0 {
                note.push(kept.to_string());
            }
            v["note"] = json!(note.join(" "));
            let mut a = Answer::new(v, format!("{n} deleted"));
            a.count = Some(n as u64);
            a.undo = (n > 0).then(|| entry(SqlUndo::Delete(kept_rows), ids));
            Ok(a)
        }
        (Call::SqlUpdateRows { set, wher, .. }, None) => {
            let sets: Vec<String> = set.iter().map(|(c, v)| format!("{} = {}", d.quote_ident(c), lit(db, v))).collect();
            let sql = format!("UPDATE {} SET {}{}", d.qualified(schema, table), sets.join(", "), where_sql(db, wher));
            let n = run_write(db, schema, &sql, read_only).await.map_err(|e| failure(&e, &nsr, "update the rows"))?;
            let mut a = Answer::new(json!({ "updated": n, "undo": false }), format!("{n} changed, no undo"));
            a.count = Some(n);
            Ok(a)
        }
        (Call::SqlDeleteRows { wher, .. }, None) => {
            let sql = format!("DELETE FROM {}{}", d.qualified(schema, table), where_sql(db, wher));
            let n = run_write(db, schema, &sql, read_only).await.map_err(|e| failure(&e, &nsr, "delete the rows"))?;
            let mut a = Answer::new(json!({ "deleted": n, "undo": false }), format!("{n} deleted, no undo"));
            a.count = Some(n);
            Ok(a)
        }
        _ => Err("Not a SQL row change.".into()),
    }
}

fn descending(v: &Bson) -> Option<bool> {
    match v {
        Bson::Int32(n) if *n != 0 => Some(*n < 0),
        Bson::Int64(n) if *n != 0 => Some(*n < 0),
        Bson::Double(f) if *f != 0.0 => Some(*f < 0.0),
        _ => None,
    }
}

/// The statement of a SQL structure tool, written by Quarry from the structured arguments: names quoted, column types
/// held to the tool parser's grammar, and a CREATE TABLE checked to be that one statement.
pub(in crate::db) fn structure_sql(d: Dialect, call: &Call) -> Res<(String, String)> {
    let q = |s: &str| d.quote_ident(s);
    Ok(match call {
        Call::CreateIndex { ns, keys, name, unique } => {
            let mut cols = Vec::new();
            for (k, v) in keys {
                let desc = descending(v).ok_or_else(|| format!("On a SQL table each index key is 1 (ascending) or -1 (descending), not {v} for `{k}`."))?;
                cols.push(format!("{}{}", q(k), if desc { " DESC" } else { "" }));
            }
            let name = name.clone().unwrap_or_else(|| format!("{}_{}_idx", ns.coll, keys.keys().cloned().collect::<Vec<_>>().join("_")).chars().take(60).collect());
            (format!("CREATE {}INDEX {} ON {} ({})", if *unique { "UNIQUE " } else { "" }, q(&name), d.qualified(&ns.db, &ns.coll), cols.join(", ")), format!("Created index {name}"))
        }
        Call::DropIndex { ns, name } if d == Dialect::Postgres => (format!("DROP INDEX {}.{}", q(&ns.db), q(name)), format!("Dropped index {name}")),
        Call::DropIndex { ns, name } => (format!("DROP INDEX {} ON {}", q(name), d.qualified(&ns.db, &ns.coll)), format!("Dropped index {name}")),
        Call::DropTable { ns } => (format!("DROP TABLE {}", d.qualified(&ns.db, &ns.coll)), format!("Dropped table {}.{}", ns.db, ns.coll)),
        Call::CreateTable { ns, columns, primary_key } => {
            // Checked again here: the type is the one part written into the statement unquoted.
            for c in columns {
                crate::agents::tools::col_type(&c.ty).map_err(|e| e.0)?;
            }
            let mut parts: Vec<String> = columns.iter().map(|c| format!("{} {}{}", q(&c.name), c.ty.trim(), if c.nullable { "" } else { " NOT NULL" })).collect();
            if !primary_key.is_empty() {
                parts.push(format!("PRIMARY KEY ({})", primary_key.iter().map(|k| q(k)).collect::<Vec<_>>().join(", ")));
            }
            let sql = format!("CREATE TABLE {} ({})", d.qualified(&ns.db, &ns.coll), parts.join(", "));
            one_create_table(&sql, d)?;
            (sql, format!("Created table {}.{}", ns.db, ns.coll))
        }
        _ => return Err("Not a structure change.".into()),
    })
}

/// Whether `sql` is one CREATE TABLE whose column list closes at its very end, under every way a server may lex it.
/// MySQL runs `CREATE TABLE t (c int) SELECT ...` as one statement, so counting statements alone would pass that.
fn one_create_table(sql: &str, d: Dialect) -> Res<()> {
    use super::lex::{every_reading, Tok};
    let refuse = || Err::<(), String>("Quarry could not write this table as one plain CREATE TABLE, so nothing was run.".into());
    for reading in every_reading(sql, d) {
        let toks: Vec<(Tok, &str)> = reading.into_iter().filter(|(t, _)| *t != Tok::Space).map(|(t, r)| (t, &sql[r])).collect();
        if toks.iter().any(|(t, s)| matches!(t, Tok::Comment | Tok::ExecOpen | Tok::ExecClose) || (*t == Tok::Punct && *s == ";")) {
            return refuse();
        }
        let lead: Vec<String> = toks.iter().take(2).map(|(_, s)| s.to_ascii_uppercase()).collect();
        if lead != ["CREATE", "TABLE"] {
            return refuse();
        }
        let Some(open) = toks.iter().position(|(t, s)| *t == Tok::Punct && *s == "(") else { return refuse() };
        let mut depth = 0i32;
        let mut close = None;
        for (i, (t, s)) in toks.iter().enumerate().skip(open) {
            match (t, *s) {
                (Tok::Punct, "(") => depth += 1,
                (Tok::Punct, ")") => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                close = Some(i);
                break;
            }
        }
        if close != Some(toks.len() - 1) {
            return refuse();
        }
    }
    Ok(())
}

/// A SQL structure change from an agent, run like the user's own script. It has no undo, which the approval said.
pub(in crate::db) async fn structure(db: &Db, call: &Call, writable: bool, rules: &Rules, limits: &Limits, h: &RunHandle) -> Res<Answer> {
    let (sql, done) = structure_sql(db.dialect, call)?;
    let schema = call.ns().map(|n| n.db.clone()).unwrap_or_default();
    free_sql(db, &schema, &sql, !writable, None, rules, limits, h, "The connection is locked for writes in Quarry, so this was not run. Only the user can unlock it.").await?;
    Ok(Answer::new(json!({ "ok": true, "statement": sql, "undo": false }), done))
}

/// One statement Quarry wrote itself, in a write transaction on a connection of its own; the count of rows it changed.
async fn run_write(db: &Db, schema: &str, sql: &str, read_only: bool) -> Res<u64> {
    let mut c = super::write::writer_conn(db, schema, read_only).await?;
    let mut tx = c.begin_write(db).await.map_err(super::se)?;
    let t = std::time::Instant::now();
    let r = tx.exec_bound(sql, &[]).await;
    db.note(schema, sql, t, &r);
    let n = r.map_err(super::se)?.rows_affected();
    tx.commit().await.map_err(super::se)?;
    Ok(n)
}

/// Stops a running `query_sql` from a connection of its own, as Stop does for the user's script.
pub(in crate::db) async fn stop(db: &Db, h: &RunHandle) {
    h.cancel.store(true, Ordering::SeqCst);
    let id = h.conn.load(Ordering::SeqCst);
    if id != 0 {
        let _ = db.stop(id).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::config::{Action, Matcher, PresetMode, PresetOverride, Presets, RuleSpec};
    use crate::db::sql::testing::{every_pg, every_server, PgFixture};

    fn rules() -> Rules {
        let pw = RuleSpec { on: true, matcher: Matcher::FieldName("^password$".into()), scope: Vec::new(), action: Action::Replace("hidden".into()), mode: PresetMode::Hide };
        Rules::compile(&Presets::default(), &PresetOverride::default(), &[pw], &[], &[3; 32]).unwrap()
    }

    fn limits(rows: u32, bytes: u32) -> Limits {
        Limits { rows, bytes, ..Limits::default() }
    }

    fn cond(col: &str, op: CondOp, value: Value) -> Cond {
        Cond { col: col.into(), op, value }
    }

    /// A table of five users on each MySQL family server, in a database of this test's own.
    fn my_fixture(l: &crate::db::sql::testing::Live, name: &str) -> String {
        let db = crate::db::sql::testing::unique_name(name);
        l.sql(&format!("DROP DATABASE IF EXISTS {db}; CREATE DATABASE {db}; CREATE TABLE {db}.users (id INT PRIMARY KEY, email VARCHAR(80), password VARCHAR(40), age INT, note TEXT); INSERT INTO {db}.users VALUES (1, 'ana@example.org', 's3cret', 31, 'call +33 6 12 34 56 78'), (2, 'bob@example.org', 'hunter2', 40, 'x'), (3, 'cy@example.org', 'pw', 22, 'y'), (4, 'di@example.org', 'pw', 55, 'z'), (5, 'ed@example.org', 'pw', 60, '{{}}')"));
        db
    }

    fn rows(a: &Answer) -> Vec<Value> {
        a.value["rows"].as_array().cloned().unwrap_or_default()
    }

    #[test]
    fn live_my_select_rows_scrubs_and_caps() {
        every_server(|l, var| {
            let db = my_fixture(l, "agent_sel");
            let r = rules();
            let rt = &l.w.rt;
            let a = rt.block_on(select_rows(&l.db, &db, "users", &[], &[cond("age", CondOp::Ge, json!(30))], &[("id".into(), false)], None, 0, &r, &limits(200, 1 << 20))).unwrap();
            let got = rows(&a);
            assert_eq!(got.len(), 4, "{var}");
            assert_eq!(got[0]["id"], json!(1));
            assert_eq!(got[0]["password"], json!("hidden"), "{var}");
            assert_ne!(got[0]["email"], json!("ana@example.org"), "{var}: email is masked");
            assert!(!got[0]["note"].as_str().unwrap().contains("12 34"), "{var}: a phone number in text is masked");
            // Quotes in a value stay a value.
            let a = rt.block_on(select_rows(&l.db, &db, "users", &["id".into()], &[cond("email", CondOp::Eq, json!("x' OR '1'='1"))], &[], None, 0, &r, &limits(200, 1 << 20))).unwrap();
            assert!(rows(&a).is_empty(), "{var}");
            let a = rt.block_on(select_rows(&l.db, &db, "users", &["id".into()], &[cond("id", CondOp::In, json!([2, 3]))], &[("id".into(), true)], None, 0, &r, &limits(200, 1 << 20))).unwrap();
            assert_eq!(rows(&a), vec![json!({ "id": 3 }), json!({ "id": 2 })]);
            // The row cap: two of five, and the agent is told more match.
            let a = rt.block_on(select_rows(&l.db, &db, "users", &["id".into()], &[], &[("id".into(), false)], None, 0, &r, &limits(2, 1 << 20))).unwrap();
            assert_eq!(rows(&a).len(), 2);
            assert_eq!(a.value["more"], json!(true));
            // The byte cap cuts on a row boundary and says how many rows were left out.
            let a = rt.block_on(select_rows(&l.db, &db, "users", &[], &[], &[("id".into(), false)], None, 0, &r, &limits(200, 200))).unwrap();
            let n = rows(&a).len();
            assert!(n >= 1 && n < 5, "{var}: {n}");
            assert_eq!(a.value["left_out"], json!(5 - n));
            assert!(rt.block_on(select_rows(&l.db, &db, "users", &["nope".into()], &[], &[], None, 0, &r, &limits(200, 1 << 20))).unwrap_err().contains("no column"));
            l.sql(&format!("DROP DATABASE {db}"));
        });
    }

    #[test]
    fn live_my_query_sql_is_read_only_and_errors_are_scrubbed() {
        every_server(|l, var| {
            let db = my_fixture(l, "agent_q");
            let r = rules();
            let rt = &l.w.rt;
            let h = RunHandle::default();
            let a = rt.block_on(query_sql(&l.db, &db, "SELECT id, password, email FROM users ORDER BY id", Some(3), &r, &limits(200, 1 << 20), &h)).unwrap();
            let res = &a.value["results"][0];
            assert_eq!(res["rows"].as_array().unwrap().len(), 3, "{var}");
            assert_eq!(res["rows"][0]["password"], json!("hidden"));
            assert_eq!(res["more"], json!(true));
            let e = rt.block_on(query_sql(&l.db, &db, "UPDATE users SET age = 0", None, &r, &limits(200, 1 << 20), &h)).unwrap_err();
            assert!(e.to_lowercase().contains("read"), "{var}: {e}");
            assert_eq!(l.one(&format!("SELECT COUNT(*) FROM {db}.users WHERE age = 0")), "0");
            // A real duplicate key error, made outside the agent path, loses the value it quotes.
            let raw = l.w.rt.block_on(async { sqlx::raw_sql(&format!("INSERT INTO {db}.users (id, email) VALUES (1, 'dup@example.org')")).execute(&l.pool).await.unwrap_err() });
            let e = failure(&crate::db::sql::se(raw), &r.all(), "insert");
            assert!(!e.contains("dup@example.org") && !e.contains("'1'"), "{var}: {e}");
            let a = rt.block_on(describe(&l.db, &db, "users", &r)).unwrap();
            assert_eq!(a.value["primary_key"], json!(["id"]));
            let pw = a.value["columns"].as_array().unwrap().iter().find(|c| c["name"] == "password").unwrap().clone();
            assert_eq!(pw["hidden"], json!(true));
            let t = rt.block_on(tables(&l.db, &db, &Scope::default())).unwrap();
            assert_eq!(t.value["tables"], json!(["users"]));
            let dbs = rt.block_on(databases(&l.db, &Scope::default())).unwrap();
            let list = dbs.value["databases"].as_array().unwrap();
            assert!(list.contains(&json!(db)) && !list.contains(&json!("mysql")), "{var}");
            l.sql(&format!("DROP DATABASE {db}"));
        });
    }

    #[test]
    fn live_pg_reads_scrub_rows_and_errors() {
        every_pg(|l, var| {
            let f = PgFixture::new(l, "agent_pg", "CREATE TABLE users (id int PRIMARY KEY, email text UNIQUE, password text, active bool, CHECK (id < 100)); INSERT INTO users VALUES (1, 'ana@example.org', 's3cret', true), (2, 'bob@example.org', 'pw', false), (3, 'cy@example.org', 'pw', true);");
            let r = rules();
            let rt = &l.w.rt;
            let a = rt.block_on(select_rows(&f.db, "agent_pg", "users", &[], &[cond("active", CondOp::Eq, json!(true)), cond("id", CondOp::Ge, json!("1"))], &[("id".into(), false)], None, 0, &r, &limits(200, 1 << 20))).unwrap();
            let got = rows(&a);
            assert_eq!(got.len(), 2, "{var}");
            assert_eq!(got[0]["password"], json!("hidden"));
            assert_eq!(got[0]["active"], json!(true));
            let a = rt.block_on(select_rows(&f.db, "agent_pg", "users", &["id".into()], &[cond("email", CondOp::Like, json!("bob%"))], &[], None, 0, &r, &limits(200, 1 << 20))).unwrap();
            assert_eq!(rows(&a), vec![json!({ "id": 2 })], "{var}");
            let h = RunHandle::default();
            let e = rt.block_on(query_sql(&f.db, "agent_pg", "UPDATE users SET password = 'x'", None, &r, &limits(200, 1 << 20), &h)).unwrap_err();
            assert!(e.to_lowercase().contains("read"), "{var}: {e}");
            assert_eq!(f.one("SELECT count(*)::text FROM users WHERE password = 'x'"), "0");
            // A failed cast quotes the value in its message, and bad JSON quotes it in DETAIL.
            let e = rt.block_on(query_sql(&f.db, "agent_pg", "SELECT 'ana@example.org'::int", None, &r, &limits(200, 1 << 20), &h)).unwrap_err();
            assert!(!e.contains("ana@example.org"), "{var}: {e}");
            let e = rt.block_on(query_sql(&f.db, "agent_pg", "SELECT '{s3cretvalue}'::json", None, &r, &limits(200, 1 << 20), &h)).unwrap_err();
            assert!(!e.contains("s3cretvalue"), "{var}: {e}");
            // A duplicate key's DETAIL, from a real insert outside the agent path.
            let raw = l.w.rt.block_on(async { sqlx::raw_sql("INSERT INTO agent_pg.users (id, email) VALUES (9, 'ana@example.org')").execute(&f.pool).await.unwrap_err() });
            let e = failure(&crate::db::sql::se(raw), &r.all(), "insert");
            assert!(!e.contains("ana@example.org") && e.contains("23505"), "{var}: {e}");
            let a = rt.block_on(query_sql(&f.db, "agent_pg", "SELECT id, password FROM users ORDER BY id", None, &r, &limits(2, 1 << 20), &h)).unwrap();
            assert_eq!(a.value["results"][0]["rows"].as_array().unwrap().len(), 2);
            assert_eq!(a.value["results"][0]["rows"][0]["password"], json!("hidden"));
            let d = rt.block_on(describe(&f.db, "agent_pg", "users", &r)).unwrap();
            assert_eq!(d.value["primary_key"], json!(["id"]), "{var}");
            let s = rt.block_on(databases(&f.db, &Scope::default())).unwrap();
            assert!(s.value["schemas"].as_array().unwrap().contains(&json!("agent_pg")), "{var}");
        });
    }

    fn tns(schema: &str, table: &str) -> crate::agents::tools::Ns {
        crate::agents::tools::Ns { conn: crate::agents::tools::ConnRef::Id(1), db: schema.into(), coll: table.into() }
    }

    fn obj(v: Value) -> serde_json::Map<String, Value> {
        v.as_object().cloned().unwrap()
    }

    fn job(db: &Db, call: Call, snap: Snapshot, keep: bool) -> AgentJob {
        AgentJob { activity: 9, conn: 1, target: crate::db::AgentTarget::Sql(db.clone()), call, rules: std::sync::Arc::new(rules()), limits: Limits::default(), scope: Scope::default(), snap: Some(snap), keep_undo: keep, writable: true, shell: None }
    }

    /// Runs an agent's SQL undo through the same code as the Undo call does.
    fn undo(rt: &tokio::runtime::Runtime, db: &Db, u: AgentUndo) -> Res<String> {
        let AgentUndo::Sql(a) = u else { panic!("a SQL undo") };
        assert_eq!(a.agent, Some(9), "the action names its Activity entry");
        let pk = match &a.kind {
            crate::db::UndoKind::Sql { undo, .. } => undo.pk_names(),
            _ => Vec::new(),
        };
        match rt.block_on(super::super::write::undo_rows(db.clone(), a, pk, false)) {
            crate::db::Msg::Written { res, undo, .. } => {
                assert!(res.is_ok() || undo.is_some_and(|u| u.agent == Some(9)), "a failed undo comes back to its entry");
                res
            }
            _ => panic!("a row undo answers Written"),
        }
    }

    /// Update, delete and insert through the row tools, each undone; then a table without a primary key, whose changes
    /// have no undo. `one` reads one value as text.
    fn row_tools(rt: &tokio::runtime::Runtime, db: &Db, schema: &str, one: &dyn Fn(&str) -> String, var: &str) {
        let q = |s: &str| s.replace("$T", &db.dialect.qualified(schema, "users")).replace("$L", &db.dialect.qualified(schema, "logs"));
        let up = Call::SqlUpdateRows { ns: tns(schema, "users"), set: obj(json!({ "age": 99, "note": "set by an agent" })), wher: vec![cond("age", CondOp::Lt, json!(45))] };
        let snap = rt.block_on(prepare(db, &up, &Limits::default())).unwrap();
        assert_eq!((snap.matched, snap.docs.as_ref().map(Vec::len), snap.no_undo.is_none()), (3, Some(3), true), "{var}");
        let a = rt.block_on(write(db, &job(db, up, snap, true))).unwrap();
        assert_eq!(a.value["updated"], json!(3), "{var}");
        assert_eq!(one(&q("SELECT COUNT(*) FROM $T WHERE age = 99")), "3", "{var}");
        undo(rt, db, a.undo.expect("an undo")).unwrap();
        assert_eq!(one(&q("SELECT COUNT(*) FROM $T WHERE age = 99")), "0", "{var}");
        assert_eq!(one(&q("SELECT age FROM $T WHERE id = 1")), "31", "{var}");
        let del = Call::SqlDeleteRows { ns: tns(schema, "users"), wher: vec![cond("id", CondOp::In, json!([4, 5]))] };
        let snap = rt.block_on(prepare(db, &del, &Limits::default())).unwrap();
        let a = rt.block_on(write(db, &job(db, del, snap, true))).unwrap();
        assert_eq!(a.value["deleted"], json!(2), "{var}");
        assert_eq!(one(&q("SELECT COUNT(*) FROM $T")), "3", "{var}");
        undo(rt, db, a.undo.expect("an undo")).unwrap();
        assert_eq!(one(&q("SELECT COUNT(*) FROM $T")), "5", "{var}");
        assert_eq!(one(&q("SELECT email FROM $T WHERE id = 5")), "ed@example.org", "{var}");
        let ins = Call::SqlInsertRows { ns: tns(schema, "users"), rows: vec![obj(json!({ "id": 7, "email": "g@example.org", "age": 1 }))] };
        let snap = rt.block_on(prepare(db, &ins, &Limits::default())).unwrap();
        assert!(snap.no_undo.is_none(), "{var}");
        let a = rt.block_on(write(db, &job(db, ins, snap, true))).unwrap();
        assert_eq!(a.value["inserted"], json!(1), "{var}");
        undo(rt, db, a.undo.expect("an undo")).unwrap();
        assert_eq!(one(&q("SELECT COUNT(*) FROM $T WHERE id = 7")), "0", "{var}");
        // Over the undo cap the snapshot keeps nothing and says why.
        let wide = Call::SqlDeleteRows { ns: tns(schema, "users"), wher: vec![cond("id", CondOp::Gt, json!(0))] };
        let snap = rt.block_on(prepare(db, &wide, &Limits { undo_docs: 2, ..Limits::default() })).unwrap();
        assert_eq!((snap.matched, snap.docs.is_none()), (5, true), "{var}");
        // Without a primary key rows can't be told apart, so the change has no undo.
        let del = Call::SqlDeleteRows { ns: tns(schema, "logs"), wher: vec![cond("msg", CondOp::Eq, json!("a"))] };
        let snap = rt.block_on(prepare(db, &del, &Limits::default())).unwrap();
        assert_eq!((snap.matched, snap.docs.is_none()), (1, true), "{var}");
        assert!(snap.no_undo.as_deref().unwrap().contains("no primary key"), "{var}");
        let a = rt.block_on(write(db, &job(db, del, snap, false))).unwrap();
        assert_eq!((a.value["deleted"].clone(), a.undo.is_none()), (json!(1), true), "{var}");
        assert_eq!(one(&q("SELECT COUNT(*) FROM $L")), "1", "{var}");
        // A column the table does not have is refused before anything runs.
        let bad = Call::SqlUpdateRows { ns: tns(schema, "users"), set: obj(json!({ "nope": 1 })), wher: vec![cond("id", CondOp::Eq, json!(1))] };
        assert!(rt.block_on(prepare(db, &bad, &Limits::default())).unwrap_err().contains("no column"), "{var}");
    }

    #[test]
    fn live_my_row_tools_write_with_undo() {
        every_server(|l, var| {
            let db = my_fixture(l, "agent_w");
            l.sql(&format!("CREATE TABLE {db}.logs (msg VARCHAR(20)); INSERT INTO {db}.logs VALUES ('a'), ('b')"));
            row_tools(&l.w.rt, &l.db, &db, &|s| l.one(s), var);
            l.sql(&format!("DROP DATABASE {db}"));
        });
    }

    #[test]
    fn live_pg_row_tools_write_with_undo() {
        every_pg(|l, var| {
            let f = PgFixture::new(l, "agent_pgw", "CREATE TABLE users (id int PRIMARY KEY, email text, password text, age int, note text); INSERT INTO users VALUES (1, 'ana@example.org', 's3cret', 31, 'n'), (2, 'bob@example.org', 'pw', 40, 'x'), (3, 'cy@example.org', 'pw', 22, 'y'), (4, 'di@example.org', 'pw', 55, 'z'), (5, 'ed@example.org', 'pw', 60, 'w'); CREATE TABLE logs (msg text); INSERT INTO logs VALUES ('a'), ('b');");
            row_tools(&l.w.rt, &f.db, "agent_pgw", &|s| f.one(&format!("SELECT ({s})::text")), var);
        });
    }

    /// run_sql refuses a write on a locked connection and runs it once unlocked; the structure tools make and drop an
    /// index and a table.
    fn scripts_and_structure(rt: &tokio::runtime::Runtime, db: &Db, schema: &str, one: &dyn Fn(&str) -> String, var: &str) {
        let r = rules();
        let h = RunHandle::default();
        let t = db.dialect.qualified(schema, "users");
        let e = rt.block_on(run_sql(db, schema, &format!("UPDATE {t} SET age = 0"), true, &r, &Limits::default(), &h)).unwrap_err();
        assert!(e.contains("locked"), "{var}: {e}");
        assert_eq!(one(&format!("SELECT COUNT(*) FROM {t} WHERE age = 0")), "0", "{var}");
        let a = rt.block_on(run_sql(db, schema, &format!("UPDATE {t} SET age = 0 WHERE id = 1; SELECT id, password FROM {t} WHERE id = 1"), false, &r, &Limits::default(), &h)).unwrap();
        assert_eq!(a.value["results"][1]["rows"][0]["password"], json!("hidden"), "{var}: {}", a.value);
        assert_eq!(a.value["undo"], json!(false));
        assert_eq!(one(&format!("SELECT COUNT(*) FROM {t} WHERE age = 0")), "1", "{var}");
        let ns = || tns(schema, "users");
        let run = |c: Call| rt.block_on(structure(db, &c, true, &r, &Limits::default(), &h));
        run(Call::CreateIndex { ns: ns(), keys: mongodb::bson::doc! { "age": -1, "email": 1 }, name: Some("by_age".into()), unique: false }).unwrap();
        let idx = rt.block_on(describe(db, schema, "users", &r)).unwrap();
        assert!(idx.value["indexes"].as_array().unwrap().iter().any(|i| i["name"] == "by_age"), "{var}: {}", idx.value);
        run(Call::DropIndex { ns: ns(), name: "by_age".into() }).unwrap();
        let idx = rt.block_on(describe(db, schema, "users", &r)).unwrap();
        assert!(!idx.value["indexes"].as_array().unwrap().iter().any(|i| i["name"] == "by_age"), "{var}");
        let cols = vec![crate::agents::tools::ColumnDef { name: "k".into(), ty: "int".into(), nullable: false }, crate::agents::tools::ColumnDef { name: "v".into(), ty: "varchar(20)".into(), nullable: true }];
        run(Call::CreateTable { ns: tns(schema, "made"), columns: cols, primary_key: vec!["k".into()] }).unwrap();
        let d = rt.block_on(describe(db, schema, "made", &r)).unwrap();
        assert_eq!(d.value["primary_key"], json!(["k"]), "{var}");
        run(Call::DropTable { ns: tns(schema, "made") }).unwrap();
        assert!(rt.block_on(describe(db, schema, "made", &r)).is_err(), "{var}");
        let inject = vec![crate::agents::tools::ColumnDef { name: "v".into(), ty: format!("int) SELECT password AS v FROM {t} WHERE (1"), nullable: true }];
        assert!(run(Call::CreateTable { ns: tns(schema, "copied"), columns: inject, primary_key: Vec::new() }).is_err(), "{var}");
        assert!(rt.block_on(describe(db, schema, "copied", &r)).is_err(), "{var}: no table was made");
        // A locked connection refuses structure too.
        let e = rt.block_on(structure(db, &Call::DropTable { ns: ns() }, false, &r, &Limits::default(), &h)).unwrap_err();
        assert!(e.contains("locked"), "{var}: {e}");
        assert!(structure_sql(db.dialect, &Call::CreateIndex { ns: ns(), keys: mongodb::bson::doc! { "a": "text" }, name: None, unique: false }).is_err());
    }

    #[test]
    fn live_my_run_sql_and_structure() {
        every_server(|l, var| {
            let db = my_fixture(l, "agent_s");
            scripts_and_structure(&l.w.rt, &l.db, &db, &|s| l.one(s), var);
            l.sql(&format!("DROP DATABASE {db}"));
        });
    }

    #[test]
    fn live_pg_run_sql_and_structure() {
        every_pg(|l, var| {
            let f = PgFixture::new(l, "agent_pgs", "CREATE TABLE users (id int PRIMARY KEY, email text, password text, age int); INSERT INTO users VALUES (1, 'ana@example.org', 's3cret', 31), (2, 'bob@example.org', 'pw', 40);");
            scripts_and_structure(&l.w.rt, &f.db, "agent_pgs", &|s| f.one(&format!("SELECT ({s})::text")), var);
        });
    }

    /// A column type that closes the column list early would make MySQL run CREATE TABLE ... SELECT, copying rows of
    /// another table, hidden columns included, into one the agent can read.
    #[test]
    fn create_table_refuses_types_that_add_a_clause() {
        let col = |ty: &str| crate::agents::tools::ColumnDef { name: "v".into(), ty: ty.into(), nullable: true };
        let make = |ty: &str| Call::CreateTable { ns: tns("shop", "copy"), columns: vec![col("int"), col(ty)], primary_key: Vec::new() };
        for d in [Dialect::MySql, Dialect::Postgres] {
            let e = structure_sql(d, &make("int) SELECT password AS v FROM users WHERE (1")).unwrap_err();
            assert!(e.contains("is not allowed"), "{d:?}: {e}");
            let (sql, _) = structure_sql(d, &make("numeric(10, 2)")).unwrap();
            assert!(sql.ends_with("numeric(10, 2))"), "{sql}");
            assert!(one_create_table(&sql, d).is_ok(), "{sql}");
            for bad in ["CREATE TABLE t (c int) SELECT password FROM users WHERE (1)", "CREATE TABLE t (c int); DROP TABLE u", "CREATE TABLE t (c int) -- x", "CREATE TABLE t (c int /* x */)", "DROP TABLE t (c)", "CREATE TABLE t (c int"] {
                assert!(one_create_table(bad, d).is_err(), "{d:?}: {bad}");
            }
        }
    }

    #[test]
    fn statements_before_a_failure_are_counted_in_words() {
        assert_eq!(ran_before(0), "");
        assert_eq!(ran_before(1), " The statement before it ran.");
        assert_eq!(ran_before(3), " The 3 statements before it ran.");
    }

    #[test]
    fn pg_detail_is_scrubbed() {
        let r = rules();
        let raw = "error returned from database: 23505: duplicate key value violates unique constraint \"users_email_key\"\nDetail: Key (email)=(ana@example.org) already exists.";
        let e = failure(raw, &r.all(), "run the statement");
        assert!(!e.contains("ana@example.org"), "{e}");
    }
}
