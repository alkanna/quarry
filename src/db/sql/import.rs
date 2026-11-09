//! Importing a CSV file into a table. Everything runs in one transaction on a connection of its own: rows go
//! in as multi-row INSERTs whose values are bound to placeholders, so no cell is ever read as SQL, and a
//! failure or Cancel rolls the whole file back. Undo needs the keys of the rows that went in, so it is only
//! recorded when they are certain.

use super::csv::{Mapping, Opts, Reader, Source, TableInfo};
use super::value::{Kind, Param};
use super::write::{action, bound, writer};
use super::{Col, Db, Dialect};
use crate::db::{e, Msg, Res, UndoAction, Worker};
use mongodb::bson::{Bson, Document};
use sqlx::{Connection, Executor};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// What to do with a row whose key is already in the table.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Dup {
    /// The import fails and nothing is imported.
    Stop,
    /// The existing row stays as it is.
    Skip,
    /// The existing row takes the file's values.
    Update,
}

/// An import past this many rows records no undo, so a huge file never fills memory with keys.
pub const UNDO_MAX_ROWS: u64 = 100_000;
/// Rows in one INSERT, and the most bound values the server accepts in one statement.
const BATCH_ROWS: usize = 500;
const MAX_PLACEHOLDERS: usize = 60_000;
/// A batch is sent early once its text is this big, so a table of large cells stays under max_allowed_packet.
const BATCH_BYTES: usize = 4 << 20;

#[derive(Clone)]
pub struct ImportPlan {
    pub source: Source,
    pub opts: Opts,
    pub header: bool,
    pub info: TableInfo,
    pub map: Mapping,
    pub dup: Dup,
    /// Whether to record an undo; false when the dialog already said it is not available.
    pub want_undo: bool,
}

/// What a running job shares with the status bar.
#[derive(Default)]
pub struct Progress {
    pub rows: AtomicU64,
    pub bytes: AtomicU64,
}

/// How a background write ended, for the status line, the undo list and what to reload.
pub struct JobDone {
    pub msg: String,
    pub undo: Option<UndoAction>,
    pub schema: String,
    /// The table whose open tabs are reloaded, when one changed.
    pub table: Option<String>,
    /// The list of tables of the database changed.
    pub tables_changed: bool,
}

/// Whether the rows of this import can be taken out again, or why not. The dialog shows the reason before
/// anything starts.
pub fn undo_plan(info: &TableInfo, map: &Mapping, dup: Dup, rows: u64) -> Result<(), String> {
    if dup != Dup::Stop {
        return Err("Undo is only offered when a duplicate key stops the import: a skipped or updated row cannot be told from a new one.".into());
    }
    if info.pk.is_empty() {
        return Err("This table has no primary key, so the new rows could not be found again. Undo is not available.".into());
    }
    if rows > UNDO_MAX_ROWS {
        return Err(format!("Over {UNDO_MAX_ROWS} rows: no undo is recorded for an import this large."));
    }
    let mapped: Vec<&str> = map.targets().into_iter().map(|(_, t)| info.cols[t].name.as_str()).collect();
    let loose: Vec<&String> = info.pk.iter().filter(|p| !mapped.contains(&p.as_str())).collect();
    match loose.as_slice() {
        [] => Ok(()),
        [one] if info.pk.len() == 1 && info.col(one).is_some_and(|c| c.auto_increment) => Ok(()),
        [one, ..] => Err(format!("The key column {one} is not in the file and does not count up by itself, so the new rows could not be found again. Undo is not available.")),
    }
}

/// `INSERT INTO t (a, b) VALUES (?, ?), (?, ?)` for `rows` rows, with the duplicate clause the choice needs.
pub fn insert_sql(d: Dialect, schema: &str, table: &str, cols: &[&str], rows: usize, dup: Dup) -> String {
    let list = cols.iter().map(|c| d.quote_ident(c)).collect::<Vec<_>>().join(", ");
    let one = format!("({})", vec!["?"; cols.len()].join(", "));
    let mut sql = format!("INSERT INTO {} ({list}) VALUES {}", d.qualified(schema, table), vec![one; rows].join(", "));
    match dup {
        Dup::Stop => {}
        // Assigning a column to itself skips the row and keeps every other error, which INSERT IGNORE would not.
        Dup::Skip => sql.push_str(&format!(" ON DUPLICATE KEY UPDATE {0} = {0}", d.quote_ident(cols[0]))),
        Dup::Update => sql.push_str(&format!(" ON DUPLICATE KEY UPDATE {}", cols.iter().map(|c| format!("{0} = VALUES({0})", d.quote_ident(c))).collect::<Vec<_>>().join(", "))),
    }
    sql
}

fn key_value(p: &Param) -> Option<Bson> {
    Some(match p {
        Param::Null => return None,
        Param::Int(i) => Bson::Int64(*i),
        Param::UInt(u) => Bson::String(u.to_string()),
        Param::Float(f) => Bson::Double(*f),
        Param::Text(s) => Bson::String(s.clone()),
        Param::Bytes(b) => Bson::Binary(mongodb::bson::Binary { subtype: mongodb::bson::spec::BinarySubtype::Generic, bytes: b.clone() }),
    })
}

fn size_of(p: &Param) -> usize {
    match p {
        Param::Text(s) => s.len(),
        Param::Bytes(b) => b.len(),
        _ => 8,
    }
}

/// Where the keys of new rows come from.
enum Keys {
    /// Not recorded.
    Off,
    /// Read off the values of these positions of each row.
    Given(Vec<(String, usize)>),
    /// The single auto-increment key the server hands out: the first id of a batch and the step.
    Counted(String, u64),
}

pub struct Done {
    /// Rows read from the file.
    pub rows: u64,
    pub inserted: u64,
    pub undo: Option<UndoAction>,
    pub why_not: Option<String>,
}

/// Runs the import. `read_only` is the connection's lock: a locked connection never gets this far.
#[allow(clippy::too_many_arguments)]
pub async fn run(db: &Db, schema: &str, table: &str, plan: &ImportPlan, conn: u64, read_only: bool, prog: &Progress, cancel: &AtomicBool, id: &AtomicU64) -> Res<Done> {
    let targets = plan.map.targets();
    if targets.is_empty() {
        return Err("No column is mapped, so there is nothing to import.".into());
    }
    if plan.map.duplicate_target().is_some() {
        return Err("Two columns of the file fill the same column of the table.".into());
    }
    let names: Vec<&str> = targets.iter().map(|(_, t)| plan.info.cols[*t].name.as_str()).collect();
    let d = db.dialect;
    let per = BATCH_ROWS.min(MAX_PLACEHOLDERS / names.len()).max(1);
    let mut c = writer(db, schema, read_only).await?;
    if let Ok(r) = c.fetch_one("SELECT CONNECTION_ID()").await {
        id.store(super::worker::raw(&r, 0).and_then(|b| std::str::from_utf8(b).ok()?.parse().ok()).unwrap_or(0), Ordering::SeqCst);
    }
    // A long import is not a slow query to cut off.
    let timeout = d.session_timeout(0);
    let _ = c.execute(timeout.as_str()).await;
    let res = import_on(&mut c, d, schema, table, plan, &names, per, conn, prog, cancel).await;
    let res = match res {
        Err(_) if cancel.load(Ordering::SeqCst) => Err("Import cancelled".to_string()),
        r => r,
    };
    let _ = c.close().await;
    res
}

// The last flush resets the counters of a batch that no longer exists.
#[allow(clippy::too_many_arguments, unused_assignments)]
async fn import_on(c: &mut sqlx::MySqlConnection, d: Dialect, schema: &str, table: &str, plan: &ImportPlan, names: &[&str], per: usize, conn: u64, prog: &Progress, cancel: &AtomicBool) -> Res<Done> {
    let mut tx = c.begin().await.map_err(e)?;
    let count_sql = format!("SELECT COUNT(*) FROM {}", d.qualified(schema, table));
    let before: i64 = if plan.dup == Dup::Stop { 0 } else { sqlx::query_scalar(&count_sql).fetch_one(&mut *tx).await.map_err(e)? };
    let mut keys = Keys::Off;
    let mut found: Vec<Document> = Vec::new();
    let mut why_not = None;
    if plan.want_undo {
        match undo_plan(&plan.info, &plan.map, plan.dup, 0) {
            Ok(()) => {
                let given: Vec<(String, usize)> = plan.info.pk.iter().filter_map(|p| names.iter().position(|n| n == p).map(|i| (p.clone(), i))).collect();
                keys = if given.len() == plan.info.pk.len() {
                    Keys::Given(given)
                } else {
                    // Another session with a bigger step (a Galera cluster) leaves gaps, so the step is read, not assumed.
                    let row = sqlx::query("SELECT CAST(@@auto_increment_increment AS CHAR)").fetch_one(&mut *tx).await.map_err(e)?;
                    let step: u64 = super::worker::raw(&row, 0).and_then(|b| std::str::from_utf8(b).ok()?.parse().ok()).unwrap_or(1);
                    Keys::Counted(plan.info.pk[0].clone(), step.max(1))
                };
            }
            Err(w) => why_not = Some(w),
        }
    }
    let r = plan.source.open().map_err(|x| format!("Could not read the file: {x}"))?;
    let mut rd = Reader::new(r, plan.opts);
    let width = plan.map.map.len();
    let (mut rows, mut flat, mut in_batch, mut batch_bytes, mut first_row) = (0u64, Vec::<Param>::new(), 0usize, 0usize, 0u64);
    // The undo keys of the rows in the batch being built.
    let mut pending: Vec<Document> = Vec::new();
    macro_rules! flush {
        () => {
            if in_batch > 0 {
                let sql = insert_sql(d, schema, table, names, in_batch, plan.dup);
                let r = bound(&sql, &flat).execute(&mut *tx).await.map_err(|x| format!("Rows {first_row} to {}: {}. Nothing was imported.", first_row + in_batch as u64 - 1, e(x)))?;
                let mut lost = None;
                match &keys {
                    Keys::Given(_) => found.append(&mut pending),
                    Keys::Counted(pk, step) => {
                        let first = r.last_insert_id();
                        if first == 0 {
                            lost = Some("The server did not report the new keys, so Undo is not available.".to_string());
                        } else {
                            for i in 0..in_batch as u64 {
                                let id = first + i * step;
                                found.push(Document::from_iter([(pk.clone(), i64::try_from(id).map_or_else(|_| Bson::String(id.to_string()), Bson::Int64))]));
                            }
                        }
                    }
                    Keys::Off => {}
                }
                if lost.is_some() {
                    (keys, why_not) = (Keys::Off, lost);
                }
                pending.clear();
                prog.rows.fetch_add(in_batch as u64, Ordering::Relaxed);
                prog.bytes.store(rd.bytes(), Ordering::Relaxed);
                flat.clear();
                (in_batch, batch_bytes) = (0, 0);
            }
        };
    }
    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = tx.rollback().await;
            return Err("Import cancelled".into());
        }
        let Some(rec) = rd.next_record()? else { break };
        if plan.header && rd.records == 1 {
            continue;
        }
        rows += 1;
        let row = rd.records;
        if rec.len() != width {
            return Err(format!("Row {row} has {} values, the first row has {width}. Nothing was imported.", rec.len()));
        }
        let params = plan.map.convert_record(&plan.info.cols, &rec).map_err(|(ci, why)| format!("Row {row}, column {}: {why} Nothing was imported.", ci + 1))?;
        if in_batch == 0 {
            first_row = row;
        }
        if let Keys::Given(at) = &keys {
            let key: Option<Document> = at.iter().map(|(n, i)| key_value(&params[*i]).map(|v| (n.clone(), v))).collect::<Option<Vec<_>>>().map(Document::from_iter);
            match key {
                Some(k) => pending.push(k),
                // A NULL in an auto-increment key is filled by the server with a number the file never held.
                None => {
                    keys = Keys::Off;
                    why_not = Some("A key value in the file is empty and was generated by the server, so Undo is not available.".to_string());
                }
            }
        }
        batch_bytes += params.iter().map(size_of).sum::<usize>();
        flat.extend(params);
        in_batch += 1;
        if in_batch >= per || batch_bytes >= BATCH_BYTES {
            flush!();
        }
        if found.len() as u64 > UNDO_MAX_ROWS {
            keys = Keys::Off;
            found.clear();
            why_not = Some(format!("Over {} rows: no undo is recorded for an import this large.", UNDO_MAX_ROWS));
        }
    }
    flush!();
    let inserted = if plan.dup == Dup::Stop {
        rows
    } else {
        let after: i64 = sqlx::query_scalar(&count_sql).fetch_one(&mut *tx).await.map_err(e)?;
        (after - before).max(0) as u64
    };
    tx.commit().await.map_err(e)?;
    prog.bytes.store(rd.bytes(), Ordering::Relaxed);
    let undo = match keys {
        Keys::Off => None,
        _ if found.is_empty() => None,
        _ => {
            let cols: Vec<Col> = plan.info.cols.iter().map(|c| Col { generated: c.generated, auto_increment: c.auto_increment, ..Col::plain(&c.name, &c.full_type, Kind::from_column_type(&c.full_type), c.nullable) }).collect();
            Some(action(conn, schema, table, &cols, super::write::SqlUndo::Insert(found)))
        }
    };
    Ok(Done { rows, inserted, undo, why_not })
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The status line for a finished import.
pub fn summary(table: &str, plan: &ImportPlan, d: &Done) -> String {
    let mut s = match plan.dup {
        Dup::Stop => format!("Imported {} into {table}", plural(d.rows, "row", "rows")),
        Dup::Skip => format!("Imported {} into {table}, skipped {} that already existed", plural(d.inserted, "new row", "new rows"), d.rows - d.inserted.min(d.rows)),
        Dup::Update => format!("Imported {} into {table}, updated {} that already existed", plural(d.inserted, "new row", "new rows"), d.rows - d.inserted.min(d.rows)),
    };
    match (&d.undo, &d.why_not) {
        (Some(_), _) => s.push_str(". Undo removes them again."),
        (None, Some(why)) => s.push_str(&format!(". {why}")),
        (None, None) => {}
    }
    s
}

impl Worker {
    /// The columns and key of a table, for the Import dialog. Answers `SqlTableInfo`.
    pub fn sql_table_info(&self, req: u64, db: Db, schema: String, table: String) {
        self.spawn(async move { Msg::SqlTableInfo { req, res: super::structure::load(&db, &schema, &table).await.map(|s| TableInfo::from_structure(&s)) } });
    }

    /// Imports a file into a table, answering with `SqlJob`. `id` and `cancel` work like a script's: `sql_kill` stops it.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_import(&self, req: u64, conn: u64, db: Db, schema: String, table: String, plan: ImportPlan, read_only: bool, prog: Arc<Progress>, id: Arc<AtomicU64>, cancel: Arc<AtomicBool>) {
        self.spawn(async move {
            let res = run(&db, &schema, &table, &plan, conn, read_only, &prog, &cancel, &id).await;
            id.store(0, Ordering::SeqCst);
            let res = res.map(|d| JobDone { msg: summary(&table, &plan, &d), undo: d.undo, schema: schema.clone(), table: Some(table.clone()), tables_changed: false });
            Msg::SqlJob { req, res }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sql::csv::{DateFmt, TCol};

    fn tcol(name: &str, ty: &str, nullable: bool, auto: bool) -> TCol {
        TCol { name: name.into(), full_type: ty.into(), kind: Kind::from_column_type(ty), nullable, auto_increment: auto, generated: false, fills_itself: nullable || auto, max_chars: None, choices: Vec::new() }
    }

    fn info() -> TableInfo {
        TableInfo { cols: vec![tcol("id", "int", false, true), tcol("name", "varchar(20)", true, false), tcol("qty", "int", true, false)], pk: vec!["id".into()] }
    }

    fn map(m: &[Option<usize>]) -> Mapping {
        Mapping { map: m.to_vec(), fmts: vec![DateFmt::DayFirst; m.len()], empty_null: true }
    }

    #[test]
    fn statements_hold_placeholders_and_never_the_values() {
        let sql = insert_sql(Dialect::MySql, "s", "t", &["a", "b`c"], 2, Dup::Stop);
        assert_eq!(sql, "INSERT INTO `s`.`t` (`a`, `b``c`) VALUES (?, ?), (?, ?)");
        assert_eq!(insert_sql(Dialect::MariaDb, "s", "t", &["a", "b"], 1, Dup::Skip), "INSERT INTO `s`.`t` (`a`, `b`) VALUES (?, ?) ON DUPLICATE KEY UPDATE `a` = `a`");
        assert_eq!(insert_sql(Dialect::MySql, "s", "t", &["a", "b"], 1, Dup::Update), "INSERT INTO `s`.`t` (`a`, `b`) VALUES (?, ?) ON DUPLICATE KEY UPDATE `a` = VALUES(`a`), `b` = VALUES(`b`)");
        // A name that tries to close the statement stays inside its backticks.
        let hostile = insert_sql(Dialect::MySql, "s", "t`; DROP TABLE x; --", &["a`) VALUES (1); --"], 1, Dup::Stop);
        assert!(hostile.contains("`t``; DROP TABLE x; --`") && hostile.contains("`a``) VALUES (1); --`"));
        assert_eq!(hostile.matches('?').count(), 1);
    }

    #[test]
    fn undo_is_promised_only_when_the_new_rows_can_be_found() {
        let all = map(&[Some(0), Some(1), Some(2)]);
        assert!(undo_plan(&info(), &all, Dup::Stop, 10).is_ok());
        // The auto-increment key is left to the server.
        assert!(undo_plan(&info(), &map(&[Some(1), Some(2)]), Dup::Stop, 10).is_ok());
        assert!(undo_plan(&info(), &all, Dup::Skip, 10).unwrap_err().contains("only offered when a duplicate key stops"));
        assert!(undo_plan(&info(), &all, Dup::Update, 10).is_err());
        assert!(undo_plan(&info(), &all, Dup::Stop, UNDO_MAX_ROWS + 1).unwrap_err().contains("Over"));
        let mut none = info();
        none.pk.clear();
        assert!(undo_plan(&none, &all, Dup::Stop, 10).unwrap_err().contains("no primary key"));
        // A key that is neither in the file nor generated.
        let mut plain = info();
        plain.cols[0].auto_increment = false;
        assert!(undo_plan(&plain, &map(&[Some(1), Some(2)]), Dup::Stop, 10).unwrap_err().contains("key column id"));
        let mut two = info();
        two.pk = vec!["id".into(), "name".into()];
        assert!(undo_plan(&two, &map(&[Some(1), Some(2)]), Dup::Stop, 10).is_err());
        assert!(undo_plan(&two, &all, Dup::Stop, 10).is_ok());
    }

    #[test]
    fn key_values_keep_their_exact_form() {
        assert_eq!(key_value(&Param::Int(5)), Some(Bson::Int64(5)));
        assert_eq!(key_value(&Param::UInt(u64::MAX)), Some(Bson::String(u64::MAX.to_string())));
        assert_eq!(key_value(&Param::Null), None);
        assert_eq!(key_value(&Param::Text("a'b".into())), Some(Bson::String("a'b".into())));
    }

    #[test]
    fn the_summary_says_what_happened_to_undo() {
        let plan = ImportPlan { source: Source::Mem(Arc::new(Vec::new())), opts: Opts { delim: ',', quote: Some('"'), enc: crate::db::sql::csv::Enc::Utf8 }, header: true, info: info(), map: map(&[Some(1)]), dup: Dup::Stop, want_undo: true };
        let done = Done { rows: 1, inserted: 1, undo: None, why_not: Some("Reason.".into()) };
        assert_eq!(summary("t", &plan, &done), "Imported 1 row into t. Reason.");
        let skip = ImportPlan { dup: Dup::Skip, ..plan };
        assert_eq!(summary("t", &skip, &Done { rows: 10, inserted: 7, undo: None, why_not: None }), "Imported 7 new rows into t, skipped 3 that already existed");
    }

    use crate::db::sql::testing::{every_server, live, Live, ENVS};

    fn plan_for(l: &Live, schema: &str, table: &str, csv: String, names: &[&str], dup: Dup) -> ImportPlan {
        let info = l.info(schema, table);
        let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        let map = Mapping { map: crate::db::sql::csv::auto_map(&names, &info.cols), fmts: vec![DateFmt::DayFirst; names.len()], empty_null: true };
        let opts = Opts { delim: ',', quote: Some('"'), enc: crate::db::sql::csv::Enc::Utf8 };
        ImportPlan { source: Source::Mem(Arc::new(csv.into_bytes())), opts, header: true, info, map, dup, want_undo: true }
    }

    fn run_import(l: &Live, schema: &str, table: &str, plan: &ImportPlan) -> (Res<Done>, Arc<Progress>) {
        let prog = Arc::new(Progress::default());
        let (cancel, id) = (AtomicBool::new(false), AtomicU64::new(0));
        let r = l.w.rt.block_on(run(&l.db, schema, table, plan, 1, false, &prog, &cancel, &id));
        (r, prog)
    }

    fn csv_quote(s: &str) -> String {
        format!("\"{}\"", s.replace('"', "\"\""))
    }

    /// A note that would end a statement, close a quote, escape a quote, break a line and hide a NULL.
    fn hostile(i: usize) -> String {
        format!("it's \"quoted\", ok\\; DROP TABLE big; -- {i}\nsecond line \\' \\N \u{1F600} `x`")
    }

    #[test]
    fn live_import_50k_rows_then_undo_removes_exactly_them() {
        every_server(|l, var| {
            l.sql("CREATE DATABASE IF NOT EXISTS quarry_ie_import CHARACTER SET utf8mb4");
            l.sql("DROP TABLE IF EXISTS quarry_ie_import.big");
            l.sql("CREATE TABLE quarry_ie_import.big (id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(40) NOT NULL, qty INT, price DECIMAL(10,2), note TEXT, seen DATETIME NULL) DEFAULT CHARSET=utf8mb4");
            l.sql("INSERT INTO quarry_ie_import.big (name, qty) VALUES ('old1', 1), ('old2', 2), ('old3', 3)");
            let mut csv = String::from("name,qty,price,note,seen\n");
            for i in 0..50_000usize {
                let note = if i % 997 == 0 { csv_quote(&hostile(i)) } else { format!("note {i}") };
                let seen = if i % 2 == 0 { format!("{:02}/03/2026 10:{:02}", 1 + i % 28, i % 60) } else { String::new() };
                csv.push_str(&format!("row{i},{},{}.{:02},{note},{seen}\n", i % 1000, i % 500, i % 100));
            }
            let mut plan = plan_for(l, "quarry_ie_import", "big", csv, &["name", "qty", "price", "note", "seen"], Dup::Stop);
            // Dates are written day first in this file.
            plan.map.fmts = vec![DateFmt::DayFirst; 5];
            assert!(undo_plan(&plan.info, &plan.map, plan.dup, 50_000).is_ok(), "{var}");
            let (r, prog) = run_import(l, "quarry_ie_import", "big", &plan);
            let done = r.unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!((done.rows, done.inserted), (50_000, 50_000), "{var}");
            assert_eq!(prog.rows.load(Ordering::Relaxed), 50_000);
            assert_eq!(l.count("quarry_ie_import.big"), 50_003, "{var}");
            // Values arrive exactly as written, hostile ones included.
            assert_eq!(l.one("SELECT note FROM quarry_ie_import.big WHERE name = 'row997'"), hostile(997), "{var}");
            assert_eq!(l.one("SELECT note FROM quarry_ie_import.big WHERE name = 'row12'"), "note 12");
            assert_eq!(l.one("SELECT price FROM quarry_ie_import.big WHERE name = 'row123'"), "123.23");
            assert_eq!(l.one("SELECT seen FROM quarry_ie_import.big WHERE name = 'row2'"), "2026-03-03 10:02:00");
            assert_eq!(l.one("SELECT COUNT(*) FROM quarry_ie_import.big WHERE seen IS NULL AND name LIKE 'row%'"), "25000");
            assert_eq!(l.one("SELECT COUNT(*) FROM quarry_ie_import.big WHERE name = 'big'"), "0");
            assert_eq!(l.one("SHOW TABLES FROM quarry_ie_import LIKE 'big'"), "big", "the table is still there");
            // Undo takes out the new rows and only them.
            let undo = done.undo.unwrap_or_else(|| panic!("{var}: no undo, {:?}", done.why_not));
            l.w.sql_undo(l.db.clone(), undo, vec!["id".into()], false);
            let Msg::Written { res, .. } = l.rx.recv_timeout(std::time::Duration::from_secs(120)).unwrap() else { panic!("written") };
            res.unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!(l.count("quarry_ie_import.big"), 3, "{var}");
            assert_eq!(l.one("SELECT GROUP_CONCAT(name ORDER BY id) FROM quarry_ie_import.big"), "old1,old2,old3");
        });
    }

    #[test]
    fn live_a_server_side_type_error_rolls_everything_back() {
        every_server(|l, var| {
            l.sql("CREATE DATABASE IF NOT EXISTS quarry_ie_import CHARACTER SET utf8mb4");
            l.sql("DROP TABLE IF EXISTS quarry_ie_import.strict");
            l.sql("CREATE TABLE quarry_ie_import.strict (id INT AUTO_INCREMENT PRIMARY KEY, qty INT NOT NULL)");
            let mut csv = String::from("qty\n");
            for i in 0..1500 {
                csv.push_str(&format!("{i}\n"));
            }
            csv.push_str("not a number\n");
            let mut plan = plan_for(l, "quarry_ie_import", "strict", csv, &["qty"], Dup::Stop);
            // The file check would catch this, so the column is declared as text to let it reach the server.
            plan.info.cols[1].kind = Kind::Text;
            let (r, prog) = run_import(l, "quarry_ie_import", "strict", &plan);
            let err = r.err().unwrap_or_else(|| panic!("{var}: the import should fail"));
            assert!(err.contains("Nothing was imported"), "{var}: {err}");
            assert_eq!(l.count("quarry_ie_import.strict"), 0, "{var}");
            assert!(prog.rows.load(Ordering::Relaxed) >= 1000, "two batches had been sent before the failure");
            // The same file with the type known is stopped before the first row is sent.
            let mut known = plan_for(l, "quarry_ie_import", "strict", "qty\n1\nabc\n".into(), &["qty"], Dup::Stop);
            known.info.cols[1].kind = Kind::Int;
            let (r, _) = run_import(l, "quarry_ie_import", "strict", &known);
            assert!(r.err().unwrap().contains("Row 3"), "{var}");
            assert_eq!(l.count("quarry_ie_import.strict"), 0);
        });
    }

    #[test]
    fn live_cancel_in_the_middle_leaves_nothing() {
        every_server(|l, var| {
            l.sql("CREATE DATABASE IF NOT EXISTS quarry_ie_import CHARACTER SET utf8mb4");
            l.sql("DROP TABLE IF EXISTS quarry_ie_import.slow");
            l.sql("CREATE TABLE quarry_ie_import.slow (id INT AUTO_INCREMENT PRIMARY KEY, a VARCHAR(40), b VARCHAR(40))");
            let mut csv = String::from("a,b\n");
            for i in 0..400_000 {
                csv.push_str(&format!("alpha {i},beta {i}\n"));
            }
            let plan = plan_for(l, "quarry_ie_import", "slow", csv, &["a", "b"], Dup::Stop);
            let prog = Arc::new(Progress::default());
            let (cancel, id) = (Arc::new(AtomicBool::new(false)), AtomicU64::new(0));
            let watcher = {
                let (prog, cancel) = (prog.clone(), cancel.clone());
                std::thread::spawn(move || {
                    while prog.rows.load(Ordering::Relaxed) < 5_000 {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    cancel.store(true, Ordering::SeqCst);
                })
            };
            let r = l.w.rt.block_on(run(&l.db, "quarry_ie_import", "slow", &plan, 1, false, &prog, &cancel, &id));
            watcher.join().unwrap();
            assert_eq!(r.err().as_deref(), Some("Import cancelled"), "{var}");
            assert!(prog.rows.load(Ordering::Relaxed) < 400_000, "it stopped early");
            assert_eq!(l.count("quarry_ie_import.slow"), 0, "{var}: a stopped import leaves no rows");
        });
    }

    #[test]
    fn live_duplicate_keys_stop_skip_or_update() {
        every_server(|l, var| {
            l.sql("CREATE DATABASE IF NOT EXISTS quarry_ie_import CHARACTER SET utf8mb4");
            l.sql("DROP TABLE IF EXISTS quarry_ie_import.dups");
            l.sql("CREATE TABLE quarry_ie_import.dups (code VARCHAR(10) PRIMARY KEY, label VARCHAR(20)) DEFAULT CHARSET=utf8mb4");
            l.sql("INSERT INTO quarry_ie_import.dups VALUES ('a', 'old a'), ('b', 'old b')");
            let file = "code,label\na,new a\nc,new c\nd,new d\n";
            let go = |dup| {
                let plan = plan_for(l, "quarry_ie_import", "dups", file.into(), &["code", "label"], dup);
                run_import(l, "quarry_ie_import", "dups", &plan).0
            };
            let e = go(Dup::Stop).err().unwrap_or_else(|| panic!("{var}: must stop"));
            assert!(e.contains("Duplicate"), "{var}: {e}");
            assert_eq!(l.count("quarry_ie_import.dups"), 2, "{var}: nothing was kept");
            let skipped = go(Dup::Skip).unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!((skipped.rows, skipped.inserted), (3, 2), "{var}");
            assert!(skipped.undo.is_none() && skipped.why_not.is_some(), "skipped rows cannot be told from new ones");
            assert_eq!(l.one("SELECT label FROM quarry_ie_import.dups WHERE code = 'a'"), "old a");
            l.sql("DELETE FROM quarry_ie_import.dups WHERE code IN ('c', 'd')");
            let updated = go(Dup::Update).unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!((updated.rows, updated.inserted), (3, 2), "{var}");
            assert_eq!(l.one("SELECT label FROM quarry_ie_import.dups WHERE code = 'a'"), "new a");
            assert_eq!(l.one("SELECT label FROM quarry_ie_import.dups WHERE code = 'b'"), "old b");
        });
    }

    #[test]
    fn live_values_survive_both_backslash_modes() {
        // The global mode is set only while a connection of its own is made, and put back after.
        static MODE: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = MODE.lock().unwrap_or_else(|p| p.into_inner());
        for var in ENVS {
            let Some(base) = live(var) else { continue };
            let old = base.one("SELECT @@GLOBAL.sql_mode");
            for nbe in [false, true] {
                let mode = if nbe { format!("{old},NO_BACKSLASH_ESCAPES") } else { old.replace("NO_BACKSLASH_ESCAPES", "") };
                base.sql(&format!("SET GLOBAL sql_mode = '{}'", mode.trim_matches(',')));
                let l = live(var).unwrap();
                assert_eq!(l.db.backslash_escapes(), !nbe, "{var}");
                l.sql("CREATE DATABASE IF NOT EXISTS quarry_ie_import CHARACTER SET utf8mb4");
                l.sql("DROP TABLE IF EXISTS quarry_ie_import.bs");
                l.sql("CREATE TABLE quarry_ie_import.bs (id INT AUTO_INCREMENT PRIMARY KEY, v TEXT) DEFAULT CHARSET=utf8mb4");
                let values = ["a\\", "\\'", "'; DROP TABLE bs; --", "x\\\\y", "it's", "\"; DELETE FROM bs; #", "back`tick", "\\0 \\n \\t", "caf\u{e9} \u{1F600}"];
                let mut csv = String::from("v\n");
                for v in values {
                    csv.push_str(&csv_quote(v));
                    csv.push('\n');
                }
                let plan = plan_for(&l, "quarry_ie_import", "bs", csv, &["v"], Dup::Stop);
                let (r, _) = run_import(&l, "quarry_ie_import", "bs", &plan);
                r.unwrap_or_else(|x| panic!("{var} nbe={nbe}: {x}"));
                assert_eq!(l.count("quarry_ie_import.bs"), values.len() as i64, "{var} nbe={nbe}");
                for (i, v) in values.iter().enumerate() {
                    // Hex hides the text from the mode, so what the server stored is compared byte for byte.
                    let want: String = v.bytes().map(|b| format!("{b:02X}")).collect();
                    assert_eq!(l.one(&format!("SELECT HEX(v) FROM quarry_ie_import.bs WHERE id = {}", i + 1)), want, "{var} nbe={nbe} value {v:?}");
                }
            }
            base.sql(&format!("SET GLOBAL sql_mode = '{}'", old.replace('\'', "")));
        }
    }
}
