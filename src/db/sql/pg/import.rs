//! Importing a CSV file into a PostgreSQL table. The file is read and checked as on MySQL (`csv`), and the rows go in as
//! multi-row INSERTs inside one transaction on a connection of its own, so a bad row, an error or Stop leaves the table as it
//! was. Every value is bound as text and its placeholder is written `CAST($n AS <type>)`, because the server refuses a
//! text value for an int column but never a cast from text. The type is the column's without its length or precision
//! (`TCol::cast_type`): an explicit cast to `varchar(3)` cuts a longer value, while the INSERT that follows applies the length
//! by assignment and refuses it, as a plain INSERT does. The types come from the structure read under the table's schema
//! as search_path and the transaction runs under the same path, so a type outside the schema means the same in both.
//!
//! Duplicate keys map to ON CONFLICT on the primary key. Undo is exact in every mode: the keys and values of new rows come
//! from RETURNING, together with `xmax = 0`, which says whether the statement inserted the row or updated it. The rows an
//! update overwrites are read first inside the transaction with FOR UPDATE. A row that was updated without such a snapshot
//! (its key is not in the file, or a concurrent insert took it) cannot be put back, so no undo is recorded then. The undo
//! compares every non-key value as text, the form RETURNING gave, because the equality of a type can be coarser than its
//! text (`1.5` and `1.50`, `1 mon` and `30 days`) or missing (point, json[]), and refuses, with nothing changed, when a row
//! no longer holds what the import left.

use crate::db::sql::csv::{Mapping, TCol, TableInfo};
use crate::db::sql::exec::{Cells, Conn, SqlRow, Tx};
use crate::db::sql::import::{Done, Dup, ImportPlan, Progress, UNDO_MAX_ROWS};
use crate::db::sql::value::Param;
use crate::db::sql::write::{action, writer_conn, SqlUndo, CONFLICT};
use crate::db::sql::{se, Col, Db, Dialect};
use crate::db::Res;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

const BATCH_ROWS: usize = 500;
/// The most values one statement can bind: the protocol counts them in 16 bits.
const MAX_PARAMS: usize = 65_535;
/// A batch is sent early once its text is this big, so a table of large cells never builds a huge statement.
const BATCH_BYTES: usize = 4 << 20;
/// What an undo may hold in memory, so a file of large cells records none instead of filling it.
const UNDO_MAX_BYTES: usize = 64 << 20;

pub(in crate::db::sql) const NEEDS_KEY: &str = "Update needs a primary key, and this table has none. Choose Skip or Stop.";
pub(in crate::db::sql) const DEFERRABLE_KEY: &str = "The primary key is DEFERRABLE, which ON CONFLICT does not support, so Skip and Update cannot work on this table. Choose Stop.";

/// How the duplicate choice is written in the statement.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Clause {
    None,
    /// No key to name, so any unique constraint passes the row over.
    NothingAny,
    Nothing,
    Update,
}

impl Clause {
    /// `has_set`: the file fills a column that is not part of the key, so an update has something to set.
    pub fn of(dup: Dup, has_pk: bool, has_set: bool) -> Clause {
        match dup {
            Dup::Stop => Clause::None,
            Dup::Skip | Dup::Update if !has_pk => Clause::NothingAny,
            Dup::Skip => Clause::Nothing,
            Dup::Update if !has_set => Clause::Nothing,
            Dup::Update => Clause::Update,
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            Clause::None => "INSERT, STOPS AT A DUPLICATE",
            Clause::NothingAny => "INSERT ... ON CONFLICT DO NOTHING",
            Clause::Nothing => "INSERT ... ON CONFLICT (KEY) DO NOTHING",
            Clause::Update => "INSERT ... ON CONFLICT (KEY) DO UPDATE",
        }
    }
}

/// Rows in one INSERT: 500, fewer when the columns would bind more than the protocol allows.
pub fn batch_rows(cols: usize) -> usize {
    BATCH_ROWS.min(MAX_PARAMS / cols.max(1)).max(1)
}

/// How the duplicate choice reads in the dialog for a PostgreSQL table: what the statement does with this mapping.
pub fn clause_text(info: &TableInfo, map: &Mapping, dup: Dup) -> &'static str {
    let has_set = map.targets().iter().any(|(_, t)| !info.pk.contains(&info.cols[*t].name));
    Clause::of(dup, !info.pk.is_empty(), has_set).text()
}

/// A row as the server printed it: the key columns, then the other columns the file fills, NULL as None.
type Cellz = Vec<Option<String>>;

/// One column of what an undo compares and writes.
#[derive(Clone, Debug, PartialEq)]
pub struct UCol {
    pub name: String,
    /// The type as format_type wrote it, which a key and an old value are cast to.
    pub ty: String,
}

/// What undoing a PostgreSQL import does: remove the rows it inserted and put back the old values of the rows an update
/// overwrote, each only while it still holds what the import left.
pub struct ImportUndo {
    /// The key columns, as many as the leading entries of `cols`.
    pub pk: Vec<String>,
    /// The key columns first, then the columns the file fills.
    pub cols: Vec<UCol>,
    /// The table is an inheritance parent: its own rows are meant, not those of its children.
    pub only: bool,
    /// New rows, with the values the server stored.
    pub inserted: Vec<Cellz>,
    /// Overwritten rows: what they held before, and what the import left.
    pub updated: Vec<(Cellz, Cellz)>,
}

impl ImportUndo {
    pub fn describe(&self) -> String {
        let p = |n: usize| format!("{n} row{}", if n == 1 { "" } else { "s" });
        match (self.inserted.len(), self.updated.len()) {
            (n, 0) => format!("imported {}", p(n)),
            (0, u) => format!("imported over {}", p(u)),
            (n, u) => format!("imported {} and updated {}", p(n), p(u)),
        }
    }
}

fn ucol(c: &TCol) -> UCol {
    UCol { name: c.name.clone(), ty: c.full_type.clone() }
}

/// The table in a statement. ONLY keeps the rows of an inheritance child out of what is read, updated or deleted.
fn target(schema: &str, table: &str, only: bool) -> String {
    format!("{}{}", if only { "ONLY " } else { "" }, Dialect::Postgres.qualified(schema, table))
}

fn cells_of(r: &SqlRow, n: usize) -> Cellz {
    (0..n).map(|i| r.cell(i).map(|b| String::from_utf8_lossy(b).into_owned())).collect()
}

fn size_of(row: &Cellz) -> usize {
    row.iter().map(|c| c.as_ref().map_or(1, |s| s.len() + 1)).sum()
}

/// `INSERT INTO "s"."t" ("a", "b") VALUES (CAST($1 AS integer), DEFAULT), ... ON CONFLICT ... RETURNING "a"::text, ...` for `rows`
/// rows whose values are `flat`, row by row. A NULL for a column that counts up by itself is written DEFAULT, as MySQL takes
/// a NULL there, so the server hands out the number. `pk` is the table's primary key, the target of ON CONFLICT. `flag` adds
/// `xmax = 0` as the last RETURNING value: true for a row the statement inserted, false for one it updated.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn insert_sql(schema: &str, table: &str, cols: &[&TCol], flat: Vec<Param>, rows: usize, dup: Dup, pk: &[String], returning: &[String], flag: bool) -> (String, Vec<Param>) {
    let (params, defaults) = bind_values(cols, flat, rows);
    (insert_text(schema, table, cols, &defaults, rows, dup, pk, returning, flag), params)
}

/// The values that are bound, and which cells are written DEFAULT instead (a NULL for a column that counts up by itself, or a
/// cell the row lacks). The second is empty when no column counts up by itself and no cell is missing, so a batch of the usual
/// shape costs no list.
fn bind_values(cols: &[&TCol], flat: Vec<Param>, rows: usize) -> (Vec<Param>, Vec<bool>) {
    let auto = cols.iter().any(|c| c.auto_increment);
    if !auto && flat.len() == rows * cols.len() {
        return (flat, Vec::new());
    }
    let (mut params, mut defaults) = (Vec::with_capacity(flat.len()), Vec::with_capacity(rows * cols.len()));
    let mut it = flat.into_iter();
    for _ in 0..rows {
        for c in cols {
            match it.next() {
                Some(Param::Null) if c.auto_increment => defaults.push(true),
                Some(p) => {
                    params.push(p);
                    defaults.push(false);
                }
                None => defaults.push(true),
            }
        }
    }
    (params, defaults)
}

/// The statement text of `insert_sql`, which depends only on the shape of the batch: how many rows, which cells are DEFAULT
/// (`defaults`, empty for none), the duplicate choice and what RETURNING gives. An import writes batches of one shape over and
/// over, so it builds the text once for each (`Imp::sql_for`).
#[allow(clippy::too_many_arguments)]
fn insert_text(schema: &str, table: &str, cols: &[&TCol], defaults: &[bool], rows: usize, dup: Dup, pk: &[String], returning: &[String], flag: bool) -> String {
    let d = Dialect::Postgres;
    let list = cols.iter().map(|c| d.quote_ident(&c.name)).collect::<Vec<_>>().join(", ");
    let (mut n, mut tuples) = (0, Vec::with_capacity(rows));
    for r in 0..rows {
        let cells: Vec<String> = cols
            .iter()
            .enumerate()
            .map(|(j, c)| {
                if defaults.get(r * cols.len() + j).copied().unwrap_or(false) {
                    return "DEFAULT".to_string();
                }
                n += 1;
                format!("CAST(${n} AS {})", c.cast_type)
            })
            .collect();
        tuples.push(format!("({})", cells.join(", ")));
    }
    let mut sql = format!("INSERT INTO {} ({list}) VALUES {}", d.qualified(schema, table), tuples.join(", "));
    let key = pk.iter().map(|k| d.quote_ident(k)).collect::<Vec<_>>().join(", ");
    let sets: Vec<String> = cols.iter().filter(|c| !pk.contains(&c.name)).map(|c| format!("{0} = EXCLUDED.{0}", d.quote_ident(&c.name))).collect();
    // Only a row whose key exists is passed over. A clash on another unique constraint stays an error, so no row
    // vanishes for a reason the dialog did not say.
    match Clause::of(dup, !pk.is_empty(), !sets.is_empty()) {
        Clause::None => {}
        Clause::NothingAny => sql.push_str(" ON CONFLICT DO NOTHING"),
        Clause::Nothing => sql.push_str(&format!(" ON CONFLICT ({key}) DO NOTHING")),
        Clause::Update => sql.push_str(&format!(" ON CONFLICT ({key}) DO UPDATE SET {}", sets.join(", "))),
    }
    let mut ret: Vec<String> = returning.iter().map(|n| format!("{}::text", d.quote_ident(n))).collect();
    if flag {
        ret.push("(xmax = 0)::text".into());
    }
    if !ret.is_empty() {
        sql.push_str(&format!(" RETURNING {}", ret.join(", ")));
    }
    sql
}

/// The rows of the batch that already exist, read with their old values and locked until the import ends, so what is saved
/// for Undo is what the update overwrites. `keys` are the key values of the rows, one entry per key column.
fn read_old_sql(schema: &str, table: &str, only: bool, cols: &[&TCol], names: &[String], keys: Vec<Vec<Param>>) -> (String, Vec<Param>) {
    let d = Dialect::Postgres;
    let list = names.iter().map(|n| format!("{}::text", d.quote_ident(n))).collect::<Vec<_>>().join(", ");
    let key = cols.iter().map(|c| d.quote_ident(&c.name)).collect::<Vec<_>>().join(", ");
    let mut params = Vec::new();
    let tuples: Vec<String> = keys
        .into_iter()
        .map(|k| {
            let cells: Vec<String> = k
                .into_iter()
                .zip(cols)
                .map(|(p, c)| {
                    params.push(p);
                    format!("CAST(${} AS {})", params.len(), c.full_type)
                })
                .collect();
            format!("({})", cells.join(", "))
        })
        .collect();
    (format!("SELECT {list} FROM {} WHERE ({key}) IN ({}) FOR UPDATE", target(schema, table, only), tuples.join(", ")), params)
}

/// What an import records for Undo while it runs.
struct Rec {
    on: bool,
    /// Update mode: a key can come back in a later batch, so where each one was recorded is kept.
    track: bool,
    n_pk: usize,
    inserted: Vec<Cellz>,
    ins_at: HashMap<Cellz, usize>,
    updated: Vec<(Cellz, Cellz)>,
    upd_at: HashMap<Cellz, usize>,
    bytes: usize,
}

impl Rec {
    /// A row the statement returned: new when `old` is None, else the row it overwrote.
    fn add(&mut self, row: Cellz, old: Option<&Cellz>) {
        let key: Cellz = row[..self.n_pk].to_vec();
        self.bytes += size_of(&row) + old.map_or(0, size_of);
        // A row this import inserted and a later line of the file overwrote: undo deletes it, holding the last values.
        if let Some(&i) = self.ins_at.get(&key) {
            self.inserted[i] = row;
            return;
        }
        match old {
            None => {
                if self.track {
                    self.ins_at.insert(key, self.inserted.len());
                }
                self.inserted.push(row);
            }
            Some(old) => {
                if let Some(&i) = self.upd_at.get(&key) {
                    self.updated[i].1 = row;
                } else if *old != row {
                    self.upd_at.insert(key, self.updated.len());
                    self.updated.push((old.clone(), row));
                }
            }
        }
    }

    fn full(&self) -> bool {
        (self.inserted.len() + self.updated.len()) as u64 > UNDO_MAX_ROWS || self.bytes > UNDO_MAX_BYTES
    }

    fn drop_all(&mut self) {
        (self.on, self.inserted, self.ins_at, self.updated, self.upd_at, self.bytes) = (false, Vec::new(), HashMap::new(), Vec::new(), HashMap::new(), 0);
    }
}

/// The batch being built and what the import carries from batch to batch.
struct Imp<'a> {
    db: &'a Db,
    schema: &'a str,
    table: &'a str,
    plan: &'a ImportPlan,
    mapped: Vec<&'a TCol>,
    /// The key columns and where each sits among the mapped ones, when the file fills all of them.
    key_at: Option<Vec<usize>>,
    /// The columns RETURNING and the read of old rows give: the key, then the other mapped columns.
    rec_names: Vec<String>,
    /// Whether an update has anything to set, that is a mapped column that is not part of the key.
    has_set: bool,
    /// A plain table that may have inheritance children: its rows are read and written with ONLY.
    only: bool,
    rec: Rec,
    why_not: Option<String>,
    flat: Vec<Param>,
    /// The statement text of each batch shape seen so far: rows, DEFAULT cells and whether RETURNING carries the insert flag.
    texts: HashMap<(usize, Vec<bool>, bool), Arc<str>>,
    in_batch: usize,
    batch_bytes: usize,
    first_row: u64,
    /// Key texts of the rows in the batch (update mode): a key twice in one INSERT is an error to ON CONFLICT DO UPDATE.
    seen: HashSet<Vec<String>>,
    inserted_n: u64,
}

fn message(range: String, x: sqlx::Error) -> String {
    let m = se(x);
    let hint = if m.contains("cannot affect row a second time") { " The file holds the same key twice in these rows." } else { "" };
    format!("{range}: {}.{hint} Nothing was imported.", m.trim_end_matches('.'))
}

impl Imp<'_> {
    fn range(&self) -> String {
        format!("Rows {} to {}", self.first_row, self.first_row + self.in_batch as u64 - 1)
    }

    async fn flush(&mut self, tx: &mut Tx<'_>, prog: &Progress, bytes: u64) -> Res<()> {
        let n = self.in_batch;
        if n == 0 {
            return Ok(());
        }
        let (w, plan) = (self.mapped.len(), self.plan);
        let update = plan.dup == Dup::Update && self.has_set;
        // Only an update can touch a row that was not inserted, and only it needs the statement to say which was which.
        let flag = self.rec.on || update;
        let mut old: HashMap<Cellz, Cellz> = HashMap::new();
        if let (true, Some(at)) = (update, &self.key_at) {
            let keys: Vec<Vec<Param>> = (0..n).map(|r| at.iter().map(|&j| self.flat[r * w + j].clone()).collect::<Vec<_>>()).filter(|k| !k.iter().any(|p| *p == Param::Null)).collect();
            if !keys.is_empty() {
                let key_cols: Vec<&TCol> = at.iter().map(|&j| self.mapped[j]).collect();
                let (sql, params) = read_old_sql(self.schema, self.table, self.only, &key_cols, &self.rec_names, keys);
                let rows = tx.fetch_bound(&sql, &params).await.map_err(|x| message(self.range(), x))?;
                for r in &rows {
                    let row = cells_of(r, self.rec_names.len());
                    old.insert(row[..plan.info.pk.len()].to_vec(), row);
                }
            }
        }
        let returning: &[String] = if self.rec.on { &self.rec_names } else { &[] };
        let (params, defaults) = bind_values(&self.mapped, std::mem::take(&mut self.flat), n);
        let sql = match self.texts.get(&(n, defaults.clone(), flag)) {
            Some(t) => Arc::clone(t),
            None => {
                let t: Arc<str> = insert_text(self.schema, self.table, &self.mapped, &defaults, n, plan.dup, &plan.info.pk, returning, flag).into();
                self.texts.insert((n, defaults, flag), Arc::clone(&t));
                t
            }
        };
        let started = Instant::now();
        let rows = if flag {
            let rows = tx.fetch_bound(&sql, &params).await;
            self.db.note(self.schema, &sql, started, &rows);
            rows.map_err(|x| message(self.range(), x))?
        } else {
            let r = tx.exec_bound(&sql, &params).await;
            self.db.note(self.schema, &sql, started, &r);
            self.inserted_n += r.map_err(|x| message(self.range(), x))?.rows_affected();
            Vec::new()
        };
        let width = if self.rec.on { self.rec_names.len() } else { 0 };
        for r in &rows {
            // The server's own word on whether it inserted the row or updated one that was there.
            let new = r.cell(width).is_some_and(|b| b == b"true");
            self.inserted_n += new as u64;
            if !self.rec.on {
                continue;
            }
            let row = cells_of(r, width);
            match (new, old.get(&row[..plan.info.pk.len()])) {
                (true, _) => self.rec.add(row, None),
                (false, Some(was)) => self.rec.add(row, Some(was)),
                // An existing row was overwritten that this import had not read: its key is not in the file, is empty there and
                // the sequence ran into it, or another session inserted it meanwhile. Its old values are lost, so no undo.
                (false, None) => {
                    self.rec.drop_all();
                    self.why_not = Some("A row of the file overwrote an existing row whose old values the import could not read first (its key is empty in the file or not in it, or another session added it meanwhile), so Undo could not put it back. No undo is recorded.".into());
                }
            }
        }
        if self.rec.on && self.rec.full() {
            self.rec.drop_all();
            self.why_not = Some(format!("Over {UNDO_MAX_ROWS} rows or too much data: no undo is recorded for an import this large."));
        }
        prog.rows.fetch_add(n as u64, Ordering::Relaxed);
        prog.bytes.store(bytes, Ordering::Relaxed);
        (self.in_batch, self.batch_bytes) = (0, 0);
        self.seen.clear();
        Ok(())
    }
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
    if plan.dup == Dup::Update && plan.info.pk.is_empty() {
        return Err(NEEDS_KEY.into());
    }
    if plan.dup != Dup::Stop && plan.info.pk_deferrable {
        return Err(DEFERRABLE_KEY.into());
    }
    let mut c = writer_conn(db, schema, read_only).await?;
    // The pid lets Stop cancel a statement that is running; behind a pooler there is none and Stop acts between batches.
    if let Conn::Pg(pc) = &mut c {
        crate::db::sql::pg::prepare(pc, true, id, db).await.map_err(se)?;
    }
    let res = import_on(&mut c, db, schema, table, plan, conn, prog, cancel).await;
    let res = match res {
        Err(_) if cancel.load(Ordering::SeqCst) => Err("Import cancelled".to_string()),
        r => r,
    };
    let _ = c.close().await;
    res
}

#[allow(clippy::too_many_arguments)]
async fn import_on(c: &mut Conn, db: &Db, schema: &str, table: &str, plan: &ImportPlan, conn: u64, prog: &Progress, cancel: &AtomicBool) -> Res<Done> {
    let mapped: Vec<&TCol> = plan.map.targets().iter().map(|(_, t)| &plan.info.cols[*t]).collect();
    let w = mapped.len();
    let pk = &plan.info.pk;
    let key_at: Option<Vec<usize>> = pk.iter().map(|p| mapped.iter().position(|m| m.name == *p)).collect();
    let mut rec_names: Vec<String> = pk.clone();
    rec_names.extend(mapped.iter().filter(|m| !pk.contains(&m.name)).map(|m| m.name.clone()));
    let has_set = mapped.iter().any(|m| !pk.contains(&m.name));
    let per = batch_rows(w);
    let mut why_not = None;
    let mut on = plan.want_undo;
    if on {
        if let Err(w) = crate::db::sql::import::undo_plan(&plan.info, &plan.map, plan.dup, 0) {
            (on, why_not) = (false, Some(w));
        }
    }
    let rec = Rec { on, track: plan.dup == Dup::Update, n_pk: pk.len(), inserted: Vec::new(), ins_at: HashMap::new(), updated: Vec::new(), upd_at: HashMap::new(), bytes: 0 };
    let mut tx = c.begin_write(db).await.map_err(se)?;
    // The types in the statements were read under this path, so they resolve to the same types here.
    crate::db::sql::pg::ddl::set_path(&mut tx, db, schema).await.map_err(se)?;
    let only = is_plain_table(&mut tx, schema, table).await?;
    let mut imp = Imp { db, schema, table, plan, mapped, key_at, rec_names, has_set, only, rec, why_not, flat: Vec::new(), texts: HashMap::new(), in_batch: 0, batch_bytes: 0, first_row: 0, seen: HashSet::new(), inserted_n: 0 };
    let done = read_file(&mut tx, &mut imp, prog, cancel, per).await;
    let rows = match done {
        Ok(rows) => rows,
        Err(x) => {
            let _ = tx.rollback().await;
            return Err(x);
        }
    };
    // Read before the COMMIT, so the oids are those of the table this import wrote to.
    let ids = if imp.rec.on && !(imp.rec.inserted.is_empty() && imp.rec.updated.is_empty()) { crate::db::sql::pg::ddl::ids_in(&mut tx, schema, table).await } else { crate::db::sql::write::PgIds::default() };
    // An error the server sends for COMMIT (a deferred constraint, a cancel) means the transaction was rolled back. Only a lost
    // connection leaves it open whether the commit went through.
    tx.commit().await.map_err(|x| match x.as_database_error() {
        Some(_) => format!("The server refused the import when it was committed ({}). It was rolled back: nothing was imported.", se(x)),
        None => format!("The server did not confirm the import ({}). Check the table: it may or may not have been applied.", se(x)),
    })?;
    let Imp { rec, why_not, inserted_n, mapped, rec_names, only, .. } = imp;
    let undo = (rec.on && !(rec.inserted.is_empty() && rec.updated.is_empty())).then(|| {
        let by_name = |n: &str| mapped.iter().copied().chain(plan.info.cols.iter()).find(|c| c.name == n).map(ucol);
        let cols: Vec<UCol> = rec_names.iter().filter_map(|n| by_name(n)).collect();
        let u = ImportUndo { pk: pk.clone(), cols, only, inserted: rec.inserted, updated: rec.updated };
        let all: Vec<Col> = plan.info.cols.iter().map(|c| Col { generated: c.generated, auto_increment: c.auto_increment, src: c.name.clone(), ..Col::plain(&c.name, &c.full_type, c.kind, c.nullable) }).collect();
        action(db.pg_database(), ids, conn, schema, table, &all, SqlUndo::Import(Arc::new(u)))
    });
    Ok(Done { rows, inserted: inserted_n, undo, why_not })
}

/// Whether the table is a plain one (not partitioned, not a view), the only kind that has inheritance children to leave out.
async fn is_plain_table(tx: &mut Tx<'_>, schema: &str, table: &str) -> Res<bool> {
    let sql = "SELECT c.relkind::text FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relname = $2";
    let rows = tx.fetch_bound(sql, &[Param::Text(schema.into()), Param::Text(table.into())]).await.map_err(se)?;
    Ok(rows.first().and_then(|r| r.cell(0)).is_some_and(|b| b == b"r"))
}

/// Reads the file and sends it in batches. Returns the rows read.
async fn read_file(tx: &mut Tx<'_>, imp: &mut Imp<'_>, prog: &Progress, cancel: &AtomicBool, per: usize) -> Res<u64> {
    let plan = imp.plan;
    let r = plan.source.open().map_err(|x| format!("Could not read the file: {x}"))?;
    let mut rd = crate::db::sql::csv::Reader::new(r, plan.opts);
    let width = plan.map.map.len();
    let mut rows = 0u64;
    loop {
        if cancel.load(Ordering::SeqCst) {
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
        if plan.dup == Dup::Update && imp.has_set {
            if let Some(at) = &imp.key_at {
                let key: Option<Vec<String>> = at.iter().map(|&j| if let Param::Text(s) = &params[j] { Some(s.clone()) } else { None }).collect();
                if let Some(k) = key {
                    // ON CONFLICT DO UPDATE cannot touch a row twice in one statement, so a repeated key starts a new batch.
                    if imp.seen.contains(&k) {
                        imp.flush(tx, prog, rd.bytes()).await?;
                    }
                    imp.seen.insert(k);
                }
            }
        }
        if imp.in_batch == 0 {
            imp.first_row = row;
        }
        imp.batch_bytes += params.iter().map(|p| if let Param::Text(s) = p { s.len() } else { 8 }).sum::<usize>();
        imp.flat.extend(params);
        imp.in_batch += 1;
        if imp.in_batch >= per || imp.batch_bytes >= BATCH_BYTES {
            imp.flush(tx, prog, rd.bytes()).await?;
        }
    }
    imp.flush(tx, prog, rd.bytes()).await?;
    prog.bytes.store(rd.bytes(), Ordering::Relaxed);
    Ok(rows)
}

/// A VALUES list with each column bound as the type given for it. The old and new values of a row travel as text, the form the
/// server printed them in, and are compared as text.
fn values(types: &[&str], rows: &[Vec<&Option<String>>]) -> (String, Vec<Param>) {
    let mut params = Vec::new();
    let tuples: Vec<String> = rows
        .iter()
        .map(|r| {
            let cells: Vec<String> = r
                .iter()
                .zip(types)
                .map(|(v, ty)| {
                    params.push(v.as_ref().map_or(Param::Null, |s| Param::Text(s.clone())));
                    format!("CAST(${} AS {ty})", params.len())
                })
                .collect();
            format!("({})", cells.join(", "))
        })
        .collect();
    (tuples.join(", "), params)
}

/// The UPDATE that puts back the old values of rows, each only while it holds what the import left.
pub(in crate::db::sql) fn restore_sql(schema: &str, table: &str, u: &ImportUndo, rows: &[(Cellz, Cellz)]) -> (String, Vec<Param>) {
    let d = Dialect::Postgres;
    let n_pk = u.pk.len();
    let rest: Vec<&UCol> = u.cols[n_pk..].iter().collect();
    // The columns of the VALUES list: the key as its type, then the old values and the values the import left, both as text.
    let types: Vec<&str> = u.cols[..n_pk].iter().map(|c| c.ty.as_str()).chain(std::iter::repeat_n("text", 2 * rest.len())).collect();
    let list: Vec<Vec<&Option<String>>> = rows.iter().map(|(old, new)| old.iter().chain(new[n_pk..].iter()).collect()).collect();
    let (tuples, params) = values(&types, &list);
    let names: Vec<String> = (0..types.len()).map(|i| format!("v{i}")).collect();
    let sets = rest.iter().enumerate().map(|(i, c)| format!("{} = CAST(v.v{} AS {})", d.quote_ident(&c.name), n_pk + i, c.ty)).collect::<Vec<_>>().join(", ");
    let mut conds: Vec<String> = u.cols[..n_pk].iter().enumerate().map(|(i, c)| format!("t.{} = v.v{i}", d.quote_ident(&c.name))).collect();
    conds.extend(rest.iter().enumerate().map(|(i, c)| format!("t.{}::text IS NOT DISTINCT FROM v.v{}", d.quote_ident(&c.name), n_pk + rest.len() + i)));
    (format!("UPDATE {} AS t SET {sets} FROM (VALUES {tuples}) AS v({}) WHERE {}", target(schema, table, u.only), names.join(", "), conds.join(" AND ")), params)
}

/// The DELETE that removes inserted rows, each only while every value is still the one the import stored.
pub(in crate::db::sql) fn remove_sql(schema: &str, table: &str, u: &ImportUndo, rows: &[Cellz]) -> (String, Vec<Param>) {
    let d = Dialect::Postgres;
    let n_pk = u.pk.len();
    let types: Vec<&str> = u.cols.iter().enumerate().map(|(i, c)| if i < n_pk { c.ty.as_str() } else { "text" }).collect();
    let list: Vec<Vec<&Option<String>>> = rows.iter().map(|r| r.iter().collect()).collect();
    let (tuples, params) = values(&types, &list);
    let names: Vec<String> = (0..types.len()).map(|i| format!("v{i}")).collect();
    let conds: Vec<String> = u.cols.iter().enumerate().map(|(i, c)| if i < n_pk { format!("t.{} = v.v{i}", d.quote_ident(&c.name)) } else { format!("t.{}::text IS NOT DISTINCT FROM v.v{i}", d.quote_ident(&c.name)) }).collect();
    (format!("DELETE FROM {} AS t USING (VALUES {tuples}) AS v({}) WHERE {}", target(schema, table, u.only), names.join(", "), conds.join(" AND ")), params)
}

/// Undoes an import in one transaction. A row that no longer holds what the import left, or is gone, refuses the whole undo
/// with nothing changed, so an edit made since is never overwritten and a row that is not the import's is never deleted.
pub async fn undo(db: &Db, schema: &str, table: &str, u: &ImportUndo, read_only: bool) -> Res<String> {
    let mut c = writer_conn(db, schema, read_only).await?;
    let mut tx = c.begin_write(db).await.map_err(se)?;
    crate::db::sql::pg::ddl::set_path(&mut tx, db, schema).await.map_err(se)?;
    let res = undo_in(db, schema, table, u, &mut tx).await;
    match res {
        Ok(()) => tx.commit().await.map_err(se)?,
        Err(x) => {
            let _ = tx.rollback().await;
            return Err(x);
        }
    }
    let _ = c.close().await;
    Ok(match (u.inserted.len(), u.updated.len()) {
        (n, 0) => format!("Removed {n} imported {}", if n == 1 { "row" } else { "rows" }),
        (n, m) => format!("Removed {n} imported rows and restored {m} overwritten {}", if m == 1 { "row" } else { "rows" }),
    })
}

async fn undo_in(db: &Db, schema: &str, table: &str, u: &ImportUndo, tx: &mut Tx<'_>) -> Res<()> {
    let n_pk = u.pk.len();
    let step = (MAX_PARAMS / (n_pk + 2 * (u.cols.len() - n_pk)).max(1)).max(1);
    for chunk in u.updated.chunks(step) {
        let (sql, params) = restore_sql(schema, table, u, chunk);
        let t = Instant::now();
        let r = tx.exec_bound(&sql, &params).await;
        db.note(schema, &sql, t, &r);
        if r.map_err(se)?.rows_affected() != chunk.len() as u64 {
            return Err(CONFLICT.into());
        }
    }
    let step = (MAX_PARAMS / u.cols.len().max(1)).max(1);
    for chunk in u.inserted.chunks(step) {
        let (sql, params) = remove_sql(schema, table, u, chunk);
        let t = Instant::now();
        let r = tx.exec_bound(&sql, &params).await;
        db.note(schema, &sql, t, &r);
        if r.map_err(se)?.rows_affected() != chunk.len() as u64 {
            return Err(CONFLICT.into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::db::sql::value::Kind;
    use super::*;

    fn col(name: &str, ty: &str, auto: bool) -> TCol {
        // The cast type is the bare type: `character varying(20)` is cast as `character varying`.
        let cast = ty.split('(').next().unwrap_or(ty);
        TCol { name: name.into(), full_type: ty.into(), cast_type: cast.into(), kind: Kind::Text, nullable: true, auto_increment: auto, generated: false, fills_itself: true, max_chars: None, choices: Vec::new(), pg: true }
    }

    fn t(s: &str) -> Param {
        Param::Text(s.into())
    }

    #[test]
    fn a_batch_never_binds_more_than_the_protocol_takes() {
        assert_eq!(batch_rows(1), 500);
        assert_eq!(batch_rows(131), 500);
        assert_eq!(batch_rows(132), 496);
        assert_eq!(batch_rows(200), 327);
        assert_eq!(batch_rows(1600), 40);
        assert_eq!(batch_rows(70_000), 1);
        for cols in 1..2000 {
            assert!(batch_rows(cols) * cols <= MAX_PARAMS || cols > MAX_PARAMS, "{cols}");
        }
    }

    #[test]
    fn statements_cast_every_placeholder_and_quote_every_name() {
        let (a, b) = (col("id", "integer", true), col("na\"me", "character varying(20)", false));
        let key = vec!["id".to_string()];
        let (sql, params) = insert_sql("my s", "t\"x", &[&a, &b], vec![t("1"), t("x'; DROP TABLE t; --"), Param::Null, Param::Null], 2, Dup::Stop, &key, &[], false);
        // The length is left out of the cast, so the INSERT checks it.
        assert_eq!(sql, "INSERT INTO \"my s\".\"t\"\"x\" (\"id\", \"na\"\"me\") VALUES (CAST($1 AS integer), CAST($2 AS character varying)), (DEFAULT, CAST($3 AS character varying))");
        assert_eq!(params, vec![t("1"), t("x'; DROP TABLE t; --"), Param::Null]);
        let ret = vec!["id".to_string(), "na\"me".to_string()];
        let (sql, _) = insert_sql("s", "t", &[&a, &b], vec![t("1"), t("x")], 1, Dup::Skip, &key, &ret, true);
        assert_eq!(sql, "INSERT INTO \"s\".\"t\" (\"id\", \"na\"\"me\") VALUES (CAST($1 AS integer), CAST($2 AS character varying)) ON CONFLICT (\"id\") DO NOTHING RETURNING \"id\"::text, \"na\"\"me\"::text, (xmax = 0)::text");
        let (sql, _) = insert_sql("s", "t", &[&a, &b], vec![t("1"), t("x")], 1, Dup::Update, &key, &[], false);
        assert!(sql.ends_with(" ON CONFLICT (\"id\") DO UPDATE SET \"na\"\"me\" = EXCLUDED.\"na\"\"me\""), "{sql}");
        // Only the key is filled: nothing to set, so the conflicting row stays.
        let (sql, _) = insert_sql("s", "t", &[&a], vec![t("1")], 1, Dup::Update, &key, &[], false);
        assert!(sql.ends_with(" ON CONFLICT (\"id\") DO NOTHING"), "{sql}");
        // A table without a key skips on any unique constraint.
        let (sql, _) = insert_sql("s", "t", &[&b], vec![t("1")], 1, Dup::Skip, &[], &[], false);
        assert!(sql.ends_with(" ON CONFLICT DO NOTHING"), "{sql}");
    }

    #[test]
    fn old_rows_are_read_by_their_typed_key_and_locked() {
        let (a, b) = (col("a", "integer", false), col("b", "text", false));
        let names = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let (sql, params) = read_old_sql("s", "t", false, &[&a, &b], &names, vec![vec![t("1"), t("x")], vec![t("2"), t("y")]]);
        assert_eq!(sql, "SELECT \"a\"::text, \"b\"::text, \"c\"::text FROM \"s\".\"t\" WHERE (\"a\", \"b\") IN ((CAST($1 AS integer), CAST($2 AS text)), (CAST($3 AS integer), CAST($4 AS text))) FOR UPDATE");
        assert_eq!(params.len(), 4);
    }

    #[test]
    fn an_undo_compares_every_value_and_binds_them_all() {
        let u = ImportUndo {
            pk: vec!["id".into()],
            cols: vec![UCol { name: "id".into(), ty: "integer".into() }, UCol { name: "doc".into(), ty: "json".into() }, UCol { name: "n".into(), ty: "numeric(10,2)".into() }],
            only: false,
            inserted: vec![vec![Some("1".into()), None, Some("1.50".into())]],
            updated: vec![(vec![Some("2".into()), Some("{}".into()), Some("2.00".into())], vec![Some("2".into()), Some("[1]".into()), Some("3.00".into())])],
        };
        let (sql, params) = remove_sql("s", "t", &u, &u.inserted);
        // The key is compared by its own equality, every other value as the text the server printed.
        assert_eq!(sql, "DELETE FROM \"s\".\"t\" AS t USING (VALUES (CAST($1 AS integer), CAST($2 AS text), CAST($3 AS text))) AS v(v0, v1, v2) WHERE t.\"id\" = v.v0 AND t.\"doc\"::text IS NOT DISTINCT FROM v.v1 AND t.\"n\"::text IS NOT DISTINCT FROM v.v2");
        assert_eq!(params, vec![t("1"), Param::Null, t("1.50")]);
        let (sql, params) = restore_sql("s", "t", &u, &u.updated);
        assert_eq!(sql, "UPDATE \"s\".\"t\" AS t SET \"doc\" = CAST(v.v1 AS json), \"n\" = CAST(v.v2 AS numeric(10,2)) FROM (VALUES (CAST($1 AS integer), CAST($2 AS text), CAST($3 AS text), CAST($4 AS text), CAST($5 AS text))) AS v(v0, v1, v2, v3, v4) WHERE t.\"id\" = v.v0 AND t.\"doc\"::text IS NOT DISTINCT FROM v.v3 AND t.\"n\"::text IS NOT DISTINCT FROM v.v4");
        // An inheritance parent is read, updated and deleted without its children.
        let only = ImportUndo { pk: u.pk.clone(), cols: u.cols.clone(), only: true, inserted: Vec::new(), updated: Vec::new() };
        assert!(remove_sql("s", "t", &only, &u.inserted).0.starts_with("DELETE FROM ONLY \"s\".\"t\" AS t"));
        assert!(restore_sql("s", "t", &only, &u.updated).0.starts_with("UPDATE ONLY \"s\".\"t\" AS t"));
        assert!(read_old_sql("s", "t", true, &[&col("a", "integer", false)], &["a".to_string()], vec![vec![t("1")]]).0.contains("FROM ONLY \"s\".\"t\" WHERE"));
        assert_eq!(params, vec![t("2"), t("{}"), t("2.00"), t("[1]"), t("3.00")]);
        assert_eq!(u.describe(), "imported 1 row and updated 1 row");
    }

    #[test]
    fn recording_follows_a_key_through_the_file() {
        let row = |k: &str, v: &str| vec![Some(k.to_string()), Some(v.to_string())];
        let mut r = Rec { on: true, track: true, n_pk: 1, inserted: Vec::new(), ins_at: HashMap::new(), updated: Vec::new(), upd_at: HashMap::new(), bytes: 0 };
        // A new row, then a later line of the file overwrites it: undo deletes it holding the last values.
        r.add(row("1", "a"), None);
        r.add(row("1", "b"), Some(&row("1", "a")));
        assert_eq!((r.inserted.clone(), r.updated.len()), (vec![row("1", "b")], 0));
        // An existing row overwritten twice: the first old values are kept, the last new ones are checked.
        r.add(row("2", "n1"), Some(&row("2", "o")));
        r.add(row("2", "n2"), Some(&row("2", "n1")));
        assert_eq!(r.updated, vec![(row("2", "o"), row("2", "n2"))]);
        // An overwrite that changes nothing records nothing.
        r.add(row("3", "same"), Some(&row("3", "same")));
        assert_eq!(r.updated.len(), 1);
    }

    #[test]
    fn the_dialog_text_says_what_the_statement_does() {
        assert_eq!(Clause::of(Dup::Skip, false, true).text(), "INSERT ... ON CONFLICT DO NOTHING");
        assert_eq!(Clause::of(Dup::Skip, true, true).text(), "INSERT ... ON CONFLICT (KEY) DO NOTHING");
        assert_eq!(Clause::of(Dup::Update, true, false).text(), "INSERT ... ON CONFLICT (KEY) DO NOTHING");
        assert_eq!(Clause::of(Dup::Update, true, true).text(), "INSERT ... ON CONFLICT (KEY) DO UPDATE");
        assert_eq!(Clause::of(Dup::Stop, true, true).text(), "INSERT, STOPS AT A DUPLICATE");
    }

    #[test]
    fn undo_needs_a_key_and_a_file_that_fits() {
        let mut info = TableInfo { cols: vec![col("id", "integer", true)], pk: vec!["id".into()], engine: String::new(), pg: true, pk_deferrable: false };
        let map = Mapping { map: vec![Some(0)], fmts: Vec::new(), empty_null: true };
        let undo_plan = |info: &TableInfo, dup, rows| crate::db::sql::import::undo_plan(info, &map, dup, rows);
        assert!(undo_plan(&info, Dup::Update, 10).is_ok() && undo_plan(&info, Dup::Skip, 10).is_ok());
        assert!(undo_plan(&info, Dup::Stop, UNDO_MAX_ROWS + 1).unwrap_err().contains("Over"));
        info.pk.clear();
        assert!(undo_plan(&info, Dup::Stop, 10).unwrap_err().contains("no primary key"));
    }
}
