//! Changing every row the Content bar matches: one UPDATE or DELETE built from the WHERE of the page the grid shows.
//! The rows are counted first, the write must match exactly that count inside its transaction, and the old rows are kept
//! for undo when there are no more than the import keeps.

use super::import::UNDO_MAX_ROWS;
use super::value::Param;
use super::worker::{make_cols_kinds, num_at, one_read, table_meta, to_row};
use super::write::{action, pk_of, writer_conn, PgIds, RowEdit, SqlUndo};
use super::{lex, se, Col, Db, Dialect, PageQuery};
use crate::db::{Msg, Res, Worker};
use mongodb::bson::{Bson, Document};
use std::time::Instant;

/// What happens to the matching rows. Update holds each column by its `Col::name` with the value it takes.
#[derive(Clone, Debug, PartialEq)]
pub enum BulkOp {
    Update(Vec<(String, Bson)>),
    Delete,
}

/// A confirmed change, with everything the statement is built from, so the worker sends the text the dialog showed.
#[derive(Clone)]
pub struct BulkPlan {
    pub op: BulkOp,
    pub filter: String,
    /// The table's columns and key as the count read them, which name the SET columns and type their literals.
    pub cols: Vec<Col>,
    pub pk: Vec<String>,
    /// Whether the server reads backslash escapes, for MySQL string literals.
    pub bs: bool,
    /// The rows the count found. A write that matches another number is rolled back.
    pub expected: u64,
    /// Keep the old rows so the change can be undone. The dialog only asks for it at or under `UNDO_MAX_ROWS`.
    pub undo: bool,
    /// The MySQL storage engine the count read, empty on PostgreSQL. One without transactions keeps what a write did.
    pub engine: String,
}

/// What the dialog learns before it can be confirmed: the exact count, and the table's columns and key read fresh, since the
/// grid's own columns may be a select list or aliases.
#[derive(Clone, Debug)]
pub struct BulkPrep {
    pub count: u64,
    pub cols: Vec<Col>,
    pub pk: Vec<String>,
    /// The table's storage engine on MySQL and MariaDB, empty on PostgreSQL.
    pub engine: String,
}

impl BulkPrep {
    /// Whether a write that matches another count than this one can still be taken back.
    pub fn rolls_back(&self) -> bool {
        super::csv::engine_rolls_back(&self.engine)
    }

    /// Whether the old rows can be kept for undo. They are found again by their key, so a table without one, or whose key
    /// is not in SELECT *, changes without undo; the page's own key may have been read before the table lost it.
    pub fn keeps_undo(&self) -> bool {
        !self.pk.is_empty() && self.count <= UNDO_MAX_ROWS
    }
}

/// How a run ended. `kept` is set when a write that should have been refused stayed, on an engine without transactions:
/// the message then says what happened, and is reported as a failure with its undo.
struct Ran {
    msg: String,
    undo: Option<(SqlUndo, PgIds)>,
    cols: Vec<Col>,
    kept: bool,
}

/// The WHERE as a page read splices it: on lines of its own inside parentheses, so an OR in it cannot escape whatever
/// follows and a trailing `-- comment` cannot swallow it. An empty filter is no WHERE at all, which is every row.
pub fn where_clause(filter: &str) -> String {
    let f = filter.trim();
    if f.is_empty() { String::new() } else { format!("\nWHERE (\n{f}\n)") }
}

/// The literal a column takes. MySQL reuses the Preview SQL spelling of a bound value; PostgreSQL wraps the literal in a
/// cast to the column's type, the same shape the row editor binds its values with.
fn literal(d: Dialect, col: &Col, v: &Bson, bs: bool) -> Res<String> {
    if d != Dialect::Postgres {
        return Ok(super::write::preview(d, "?", &[Param::for_kind(col.kind, v)], bs));
    }
    let p = Param::for_pg(v);
    let lit = super::relay::literal_pg(&p)?;
    if matches!(p, Param::Null) || col.ty.trim().is_empty() {
        return Ok(lit);
    }
    Ok(format!("CAST({lit} AS {})", col.ty))
}

/// `a = 1, b = 'x'` for the pairs, each column by its server name.
pub fn set_list(d: Dialect, cols: &[Col], sets: &[(String, Bson)], bs: bool) -> Res<String> {
    if sets.is_empty() {
        return Err("Pick at least one column to set.".into());
    }
    let mut out = Vec::new();
    for (name, v) in sets {
        let col = cols.iter().find(|c| c.name == *name).ok_or_else(|| format!("{name} is not a column of this table"))?;
        out.push(format!("{} = {}", d.quote_ident(&col.src), literal(d, col, v, bs)?));
    }
    Ok(out.join(",\n    "))
}

/// The statement the plan runs, as the dialog shows it.
pub fn statement(d: Dialect, schema: &str, table: &str, p: &BulkPlan) -> Res<String> {
    let t = d.qualified(schema, table);
    let w = where_clause(&p.filter);
    Ok(match &p.op {
        BulkOp::Update(sets) => format!("UPDATE {t}\nSET {}{w}", set_list(d, &p.cols, sets, p.bs)?),
        BulkOp::Delete => format!("DELETE FROM {t}{w}"),
    })
}

/// The exact count of the rows the WHERE matches. Never the table estimate a plain page shows.
pub fn count_sql(d: Dialect, schema: &str, table: &str, filter: &str) -> String {
    PageQuery { filter: filter.to_string(), ..Default::default() }.count_sql(d, schema, table)
}

/// The rows about to change, locked for the rest of the transaction. One more than counted is asked for, so a table that
/// grew since the count is seen without reading all of it.
fn capture_sql(d: Dialect, schema: &str, table: &str, filter: &str, limit: u64) -> String {
    format!("SELECT *\nFROM {}{}\nLIMIT {limit}\nFOR UPDATE", d.qualified(schema, table), where_clause(filter))
}

/// The filter must read like a page's WHERE, and the statement built around it must be one statement.
pub fn check(d: Dialect, schema: &str, table: &str, filter: &str, stmt: &str) -> Res<()> {
    one_read(&count_sql(d, schema, table, filter), d)?;
    if lex::split_statements(stmt, d).len() != 1 {
        return Err("The filter must not contain another statement".into());
    }
    Ok(())
}

fn row_count(n: u64) -> String {
    format!("{n} {}", if n == 1 { "row" } else { "rows" })
}

/// Why nothing was written when the rows no longer match the count.
fn moved(now: &str, expected: u64) -> String {
    format!("Nothing was changed: {now} match now, not {} as counted. Review the new count and confirm again.", row_count(expected))
}

async fn prep(db: &Db, schema: &str, table: &str, filter: &str) -> Res<BulkPrep> {
    let d = db.dialect;
    let sql = count_sql(d, schema, table, filter);
    one_read(&sql, d)?;
    let count = async { db.read_text(schema, &sql, true).await.map(|r| r.first().and_then(|r| num_at(r, 0)).unwrap_or(0)) };
    let shape = async {
        let engine = if d == Dialect::Postgres { String::new() } else { db.meta(schema, d.table_status(), [schema, table]).await?.first().and_then(|r| super::worker::text(r, "engine")).unwrap_or_default() };
        let (meta, kinds, mut pk) = table_meta(db, schema, table).await?;
        let rcols = db.describe_text(&format!("SELECT * FROM {}", d.qualified(schema, table))).await?;
        // An invisible key column is not in SELECT *, so the rows could not be told apart.
        if !pk.iter().all(|p| rcols.iter().any(|c| c.name == *p)) {
            pk.clear();
        }
        Ok::<_, String>((make_cols_kinds(&rcols, &meta, &kinds, !pk.is_empty()), pk, engine))
    };
    let (count, (cols, pk, engine)) = tokio::try_join!(count, shape)?;
    Ok(BulkPrep { count, cols, pk, engine })
}

/// Runs the plan in one transaction. Returns what to say, the undo when one was kept, and the columns the undo reads.
async fn run(db: &Db, schema: &str, table: &str, p: &BulkPlan, read_only: bool) -> Res<Ran> {
    let d = db.dialect;
    let mut stmt = statement(d, schema, table, p)?;
    check(d, schema, table, &p.filter, &stmt)?;
    if p.expected == 0 {
        return Err("No rows match, so nothing was changed.".into());
    }
    let (meta, kinds, pk) = table_meta(db, schema, table).await?;
    // The undo finds its rows again by the key the dialog was built with; another key could match rows it never touched.
    if p.undo && (pk.is_empty() || pk != p.pk) {
        return Err(format!("Nothing was changed: the primary key of {table} is not the one the count read. Count again."));
    }
    let mut c = writer_conn(db, schema, read_only).await?;
    let mut tx = c.begin_write(db).await.map_err(se)?;
    // The literals were spelled for the sql_mode read when the connection opened. This session may read backslashes the
    // other way, and then the same values are spelled for it, so what is stored is what the dialog showed.
    if d != Dialect::Postgres && matches!(p.op, BulkOp::Update(_)) {
        let r = tx.fetch_text("SELECT @@SESSION.sql_mode").await.map_err(se)?;
        if let Some(mode) = r.first().and_then(|r| super::worker::text_at(r, 0)) {
            let bs = !mode.to_ascii_uppercase().contains("NO_BACKSLASH_ESCAPES");
            if bs != p.bs {
                stmt = statement(d, schema, table, &BulkPlan { bs, ..p.clone() })?;
                check(d, schema, table, &p.filter, &stmt)?;
            }
        }
    }
    let mut old: Vec<Document> = Vec::new();
    let mut cols = p.cols.clone();
    if p.undo {
        let sql = capture_sql(d, schema, table, &p.filter, p.expected + 1);
        let t = Instant::now();
        let r = tx.fetch_text(&sql).await;
        db.note(schema, &sql, t, &r);
        let rows = r.map_err(se)?;
        let n = rows.len() as u64;
        if n != p.expected {
            let _ = tx.rollback().await;
            return Err(moved(&if n > p.expected { format!("more than {}", row_count(p.expected)) } else { row_count(n) }, p.expected));
        }
        if let Some(first) = rows.first() {
            cols = make_cols_kinds(&first.cols(), &meta, &kinds, true);
        }
        for r in &rows {
            let doc = to_row(&cols, r, &pk)?.owned();
            if pk_of(&doc).len() != pk.len() {
                let _ = tx.rollback().await;
                return Err("Nothing was changed: a row could not be read with its whole key, so undo could not be kept.".into());
            }
            old.push(doc);
        }
    }
    // Bound with no values: a prepared statement is one statement to the server, whatever the lexer read.
    let t = Instant::now();
    let r = tx.exec_bound(&stmt, &[]).await;
    db.note(schema, &stmt, t, &r);
    let n = r.map_err(|x| super::se_at(&x, &stmt, 0))?.rows_affected();
    // Without transactions the rows changed the moment the statement ran, so a rollback would only hide that.
    let kept = n != p.expected && !super::csv::engine_rolls_back(&p.engine);
    if n != p.expected && !kept {
        let _ = tx.rollback().await;
        return Err(moved(&row_count(n), p.expected));
    }
    let ids = if p.undo { super::pg::ddl::ids_in(&mut tx, schema, table).await } else { PgIds::default() };
    tx.commit().await.map_err(se)?;
    let undo = p.undo.then(|| match &p.op {
        BulkOp::Delete => SqlUndo::Delete(old),
        BulkOp::Update(sets) => SqlUndo::Update(old.iter().map(|r| RowEdit { pk: pk_of(r), changes: sets.iter().map(|(name, new)| (name.clone(), r.get(name).cloned().unwrap_or(Bson::Null), new.clone())).collect() }).collect()),
    });
    let what = match p.op {
        BulkOp::Update(_) => format!("Updated {} in {table}", row_count(n)),
        BulkOp::Delete => format!("Deleted {} from {table}", row_count(n)),
    };
    let msg = if kept {
        format!("{what}, not {} as counted: the {} engine of this table cannot roll back, so the change stays.{}", row_count(p.expected), p.engine, if p.undo { " Undo puts back the rows read before it." } else { "" })
    } else if p.undo {
        what
    } else if p.pk.is_empty() {
        format!("{what}. No undo was kept: the table has no primary key.")
    } else {
        format!("{what}. No undo was kept for a change this large.")
    };
    Ok(Ran { msg, undo: undo.map(|u| (u, ids)), cols, kept })
}

impl Worker {
    /// Counts the rows the filter matches and reads the table's columns and key, for the bulk dialog. Answers `SqlBulkPrep`.
    pub fn sql_bulk_prep(&self, req: u64, db: Db, schema: String, table: String, filter: String) {
        self.spawn(async move {
            let res = prep(&db, &schema, &table, &filter).await;
            Msg::SqlBulkPrep { req, res }
        });
    }

    /// Runs a confirmed bulk UPDATE or DELETE. Answers `Written`, with the undo when one was kept.
    pub fn sql_bulk(&self, conn: u64, db: Db, schema: String, table: String, plan: BulkPlan, read_only: bool) {
        self.spawn(async move {
            let at = (schema.clone(), table.clone());
            let (res, undo) = match run(&db, &schema, &table, &plan, read_only).await {
                Ok(r) => (if r.kept { Err(r.msg) } else { Ok(r.msg) }, r.undo.map(|(u, ids)| action(db.pg_database(), ids, conn, &schema, &table, &r.cols, u))),
                Err(err) => (Err(err), None),
            };
            Msg::Written { at, res, undo, reload: true, retry: None }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::value::Kind;

    fn cols() -> Vec<Col> {
        vec![Col::plain("id", "int", Kind::Int, false), Col::plain("name", "varchar(50)", Kind::Text, true), Col::plain("qty", "int", Kind::Int, true)]
    }

    fn plan(op: BulkOp, filter: &str) -> BulkPlan {
        BulkPlan { op, filter: filter.into(), cols: cols(), pk: vec!["id".into()], bs: true, expected: 2, undo: true, engine: String::new() }
    }

    #[test]
    fn an_or_filter_stays_inside_its_parentheses() {
        let p = plan(BulkOp::Delete, "qty > 15 OR name = 'alpha'");
        assert_eq!(statement(Dialect::MySql, "shop", "t", &p).unwrap(), "DELETE FROM `shop`.`t`\nWHERE (\nqty > 15 OR name = 'alpha'\n)");
        assert_eq!(statement(Dialect::MariaDb, "shop", "t", &p).unwrap(), "DELETE FROM `shop`.`t`\nWHERE (\nqty > 15 OR name = 'alpha'\n)");
        assert_eq!(statement(Dialect::Postgres, "public", "t", &p).unwrap(), "DELETE FROM \"public\".\"t\"\nWHERE (\nqty > 15 OR name = 'alpha'\n)");
        // The capture that undo reads locks the same rows and nothing past the OR.
        assert_eq!(capture_sql(Dialect::Postgres, "public", "t", &p.filter, 3), "SELECT *\nFROM \"public\".\"t\"\nWHERE (\nqty > 15 OR name = 'alpha'\n)\nLIMIT 3\nFOR UPDATE");
        assert_eq!(count_sql(Dialect::MySql, "shop", "t", &p.filter), "SELECT COUNT(*) FROM `shop`.`t`\nWHERE (\nqty > 15 OR name = 'alpha'\n)");
    }

    #[test]
    fn a_trailing_comment_cannot_swallow_the_closing_parenthesis() {
        let p = plan(BulkOp::Delete, "qty > 15 -- big ones");
        let s = statement(Dialect::MySql, "shop", "t", &p).unwrap();
        assert_eq!(s, "DELETE FROM `shop`.`t`\nWHERE (\nqty > 15 -- big ones\n)");
        assert!(check(Dialect::MySql, "shop", "t", &p.filter, &s).is_ok());
    }

    #[test]
    fn an_empty_filter_means_every_row() {
        for f in ["", "  \n "] {
            let p = plan(BulkOp::Delete, f);
            assert_eq!(statement(Dialect::MySql, "shop", "t", &p).unwrap(), "DELETE FROM `shop`.`t`");
            assert_eq!(statement(Dialect::Postgres, "public", "t", &p).unwrap(), "DELETE FROM \"public\".\"t\"");
            assert_eq!(count_sql(Dialect::Postgres, "public", "t", f), "SELECT COUNT(*) FROM \"public\".\"t\"");
        }
    }

    #[test]
    fn update_values_are_literals_of_each_dialect() {
        let sets = vec![("name".to_string(), Bson::String("o'brien \\ x".into())), ("qty".to_string(), Bson::Int64(7))];
        let p = plan(BulkOp::Update(sets.clone()), "a = 1 OR b = 2");
        assert_eq!(statement(Dialect::MySql, "shop", "t", &p).unwrap(), "UPDATE `shop`.`t`\nSET `name` = 'o''brien \\\\ x',\n    `qty` = 7\nWHERE (\na = 1 OR b = 2\n)");
        // Under NO_BACKSLASH_ESCAPES a backslash is a plain character and a quote is doubled.
        let plain = BulkPlan { bs: false, ..p.clone() };
        assert!(statement(Dialect::MySql, "shop", "t", &plain).unwrap().contains("`name` = 'o''brien \\ x'"));
        let pg = statement(Dialect::Postgres, "public", "t", &p).unwrap();
        assert_eq!(pg, "UPDATE \"public\".\"t\"\nSET \"name\" = CAST(E'o''brien \\\\ x' AS varchar(50)),\n    \"qty\" = CAST('7' AS int)\nWHERE (\na = 1 OR b = 2\n)");
        // NULL needs no cast, and a column is named by its server name.
        let null = plan(BulkOp::Update(vec![("name".into(), Bson::Null)]), "");
        assert_eq!(statement(Dialect::Postgres, "public", "t", &null).unwrap(), "UPDATE \"public\".\"t\"\nSET \"name\" = NULL");
        assert_eq!(statement(Dialect::MySql, "shop", "t", &null).unwrap(), "UPDATE `shop`.`t`\nSET `name` = NULL");
        assert!(statement(Dialect::MySql, "shop", "t", &plan(BulkOp::Update(Vec::new()), "")).is_err());
        assert!(statement(Dialect::MySql, "shop", "t", &plan(BulkOp::Update(vec![("nope".into(), Bson::Null)]), "")).is_err());
    }

    #[test]
    fn a_filter_that_writes_or_holds_a_second_statement_is_refused() {
        for (d, schema) in [(Dialect::MySql, "shop"), (Dialect::Postgres, "public")] {
            for f in ["1 = 1); DROP TABLE t; --", "id IN (SELECT 1); DELETE FROM u"] {
                let p = plan(BulkOp::Delete, f);
                let s = statement(d, schema, "t", &p).unwrap();
                assert!(check(d, schema, "t", f, &s).is_err(), "{d:?}: {f}");
            }
            let ok = plan(BulkOp::Delete, "name LIKE '%;%'");
            assert!(check(d, schema, "t", &ok.filter, &statement(d, schema, "t", &ok).unwrap()).is_ok(), "{d:?}");
        }
    }

    #[test]
    fn undo_is_kept_up_to_the_import_cap() {
        let prep = |count, pk: &[&str]| BulkPrep { count, cols: cols(), pk: pk.iter().map(|p| p.to_string()).collect(), engine: String::new() };
        assert!(prep(3, &["id"]).keeps_undo());
        assert!(!prep(UNDO_MAX_ROWS + 1, &["id"]).keeps_undo());
        // Rows without a key could never be found again, so a keyless table changes without undo instead of being refused.
        assert!(!prep(3, &[]).keeps_undo());
    }

    #[test]
    fn live_a_write_that_matches_another_count_than_the_dialog_showed_changes_nothing() {
        super::super::testing::every_server(|l, var| {
            l.sql("CREATE DATABASE IF NOT EXISTS quarry_bulk");
            l.sql("DROP TABLE IF EXISTS quarry_bulk.moved");
            l.sql("CREATE TABLE quarry_bulk.moved (id INT PRIMARY KEY, name VARCHAR(20), qty INT)");
            l.sql("INSERT INTO quarry_bulk.moved VALUES (1, 'alpha', 10), (2, 'beta', 20), (3, 'gamma', 30)");
            let filter = "qty > 15 OR name = 'alpha'";
            let p = l.w.rt.block_on(prep(&l.db, "quarry_bulk", "moved", filter)).unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!((p.count, p.pk.clone()), (3, vec!["id".to_string()]), "{var}");
            // A count of 2 stands for rows that changed between the count and the confirm: with or without the capture, the
            // write is rolled back.
            for undo in [true, false] {
                let plan = BulkPlan { op: BulkOp::Delete, filter: filter.into(), cols: p.cols.clone(), pk: p.pk.clone(), bs: true, expected: 2, undo, engine: p.engine.clone() };
                let err = l.w.rt.block_on(run(&l.db, "quarry_bulk", "moved", &plan, false)).err().unwrap_or_else(|| panic!("{var}: the write ran"));
                assert!(err.starts_with("Nothing was changed"), "{var}: {err}");
                assert_eq!(l.count("quarry_bulk.moved"), 3, "{var}, undo {undo}");
            }
            // A locked connection writes nothing either.
            // A filter that carries a second statement is refused before anything is sent.
            let sneaky = BulkPlan { op: BulkOp::Delete, filter: "1 = 1); DELETE FROM quarry_bulk.moved; --".into(), cols: p.cols.clone(), pk: p.pk.clone(), bs: true, expected: 3, undo: false, engine: p.engine.clone() };
            assert!(l.w.rt.block_on(run(&l.db, "quarry_bulk", "moved", &sneaky, false)).is_err(), "{var}");
            assert_eq!(l.count("quarry_bulk.moved"), 3, "{var}");
            let plan = BulkPlan { op: BulkOp::Delete, filter: filter.into(), cols: p.cols.clone(), pk: p.pk.clone(), bs: true, expected: 3, undo: true, engine: p.engine.clone() };
            assert!(l.w.rt.block_on(run(&l.db, "quarry_bulk", "moved", &plan, true)).is_err(), "{var}");
            assert_eq!(l.count("quarry_bulk.moved"), 3, "{var}");
            let r = l.w.rt.block_on(run(&l.db, "quarry_bulk", "moved", &plan, false)).unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!((r.msg.as_str(), r.kept), ("Deleted 3 rows from moved", false), "{var}");
            assert!(matches!(r.undo, Some((SqlUndo::Delete(ref rows), _)) if rows.len() == 3), "{var}");
            assert_eq!(l.count("quarry_bulk.moved"), 0, "{var}");
            l.sql("DROP TABLE quarry_bulk.moved");
        });
    }

    #[test]
    fn live_a_table_without_a_key_changes_without_undo() {
        super::super::testing::every_server(|l, var| {
            l.sql("CREATE DATABASE IF NOT EXISTS quarry_bulk");
            l.sql("DROP TABLE IF EXISTS quarry_bulk.keyless");
            l.sql("CREATE TABLE quarry_bulk.keyless (name VARCHAR(20), qty INT)");
            l.sql("INSERT INTO quarry_bulk.keyless VALUES ('alpha', 10), ('beta', 20), ('gamma', 30)");
            let filter = "qty > 15";
            let p = l.w.rt.block_on(prep(&l.db, "quarry_bulk", "keyless", filter)).unwrap_or_else(|x| panic!("{var}: {x}"));
            assert!(p.pk.is_empty() && p.count == 2 && !p.keeps_undo(), "{var}");
            // The plan the dialog builds from this count runs; one that asked for undo would be refused.
            let plan = |undo| BulkPlan { op: BulkOp::Update(vec![("name".into(), Bson::String("zeta".into()))]), filter: filter.into(), cols: p.cols.clone(), pk: p.pk.clone(), bs: true, expected: p.count, undo, engine: p.engine.clone() };
            assert!(l.w.rt.block_on(run(&l.db, "quarry_bulk", "keyless", &plan(true), false)).is_err(), "{var}");
            let r = l.w.rt.block_on(run(&l.db, "quarry_bulk", "keyless", &plan(p.keeps_undo()), false)).unwrap_or_else(|x| panic!("{var}: {x}"));
            assert!(r.undo.is_none() && !r.kept, "{var}");
            assert_eq!(l.one("SELECT GROUP_CONCAT(name ORDER BY qty) FROM quarry_bulk.keyless"), "alpha,zeta,zeta", "{var}");
            l.sql("DROP TABLE quarry_bulk.keyless");
        });
    }
}
