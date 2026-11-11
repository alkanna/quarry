//! Writing SQL rows: saving staged cell edits, deleting rows and inserting them, each in one
//! transaction on a connection of its own that is switched to READ WRITE. The pool stays read-only.

use super::se;
use super::value::{hex_digits, lit, Kind, Param};
use super::lex;
use super::exec::{Cells, Conn};
use super::{Col, Db, Dialect};
use crate::db::{Msg, Res, UndoAction, UndoKind, Worker};
use mongodb::bson::{Bson, Document};
use std::time::Instant;

pub const CONFLICT: &str = "Not saved: a row changed on the server since you opened it. Nothing was written. Reload and try again.";

/// PostgreSQL has no `LIMIT 1` on UPDATE and DELETE, so a key that is not unique (a column set the table does not
/// enforce) would change every row that shares it. The count is checked before the commit and the write is rolled back.
pub const SEVERAL: &str = "Nothing was written: the key matches more than one row, so the change would have reached rows that are not on screen. The table has no unique constraint on the columns used as its key.";

/// One row's staged change: the key, and each changed column with its value as read and the new one.
#[derive(Clone, Debug, PartialEq)]
pub struct RowEdit {
    pub pk: Document,
    pub changes: Vec<(String, Bson, Bson)>,
}

/// What undoing a SQL write does. Updates write the originals back, a delete puts the whole rows
/// back, an insert deletes the new keys.
#[derive(Clone)]
pub enum SqlUndo {
    Update(Vec<RowEdit>),
    Delete(Vec<Document>),
    Insert(Vec<Document>),
    /// PostgreSQL inserts: each document holds the key under `_id` and the values that were written, so the undo
    /// removes a row only while it still holds them and never one that was edited or replaced since.
    Inserted(Vec<Document>),
    /// A structure change: the ALTERs that reverse it and what they rely on.
    Alter(Box<super::ddl::AlterUndo>),
    /// A trigger change: what puts the trigger back and how the change left it.
    Trigger(Box<super::ddl::TrigUndo>),
    /// A database or table this app created: undone by dropping it while it is still empty.
    /// `oid` is the database's pg_database oid and `baseline` how many objects of a user it held right after the create (what its template gave it), PostgreSQL only; MySQL leaves both empty.
    CreatedDb { oid: String, baseline: i64 },
    /// `made` is the table's CREATE_TIME right after the create, so the undo can tell it is still that table.
    CreatedTable { made: Option<String> },
    /// A PostgreSQL schema this app created, by its oid so a schema dropped and made again is not taken for it.
    CreatedSchema { oid: String },
    /// A PostgreSQL table this app created: its oid and how its structure read right after, so the undo drops it only while it is that table, unchanged and empty.
    PgCreatedTable { oid: String, state: Vec<String> },
    /// A change to an account's privileges, or a user this app created; see `users`.
    Account(Box<super::users::AccountUndo>),
}

impl SqlUndo {
    /// Key column names, read off the first row: undo of a delete re-inserts the rows and needs them.
    pub fn pk_names(&self) -> Vec<String> {
        let first = match self {
            SqlUndo::Update(r) => r.first().map(|r| r.pk.clone()),
            SqlUndo::Delete(r) => r.first().map(pk_of),
            SqlUndo::Insert(r) => r.first().cloned(),
            SqlUndo::Inserted(r) => r.first().map(pk_of),
            SqlUndo::Alter(_) | SqlUndo::Trigger(_) | SqlUndo::Account(_) => None,
            SqlUndo::CreatedDb { .. } | SqlUndo::CreatedTable { .. } | SqlUndo::CreatedSchema { .. } | SqlUndo::PgCreatedTable { .. } => None,
        };
        first.map(|d| d.keys().cloned().collect()).unwrap_or_default()
    }

    /// For the Recent changes list.
    pub fn describe(&self) -> String {
        match self {
            SqlUndo::Update(r) => format!("edited {}", plural(r.len(), "row")),
            SqlUndo::Delete(r) => format!("deleted {}", plural(r.len(), "row")),
            SqlUndo::Insert(r) | SqlUndo::Inserted(r) => format!("inserted {}", plural(r.len(), "row")),
            SqlUndo::Alter(u) => u.label.clone(),
            SqlUndo::Trigger(u) => u.label.clone(),
            SqlUndo::CreatedDb { .. } => "created database".into(),
            SqlUndo::CreatedTable { .. } | SqlUndo::PgCreatedTable { .. } => "created table".into(),
            SqlUndo::CreatedSchema { .. } => "created schema".into(),
            SqlUndo::Account(u) => u.label.clone(),
        }
    }
}

/// A row to insert: only the columns given a value; the rest take the column default. `insert_sql`
/// wants server column names, `apply_inserts` accepts the row document's own.
pub type NewRow = Vec<(String, Bson)>;

/// Row documents are keyed by `Col::name`, statements must use `Col::src`. Keys of `_id` already use server names.
fn kind_of(cols: &[Col], name: &str) -> Kind {
    cols.iter().find(|c| c.name == name).map_or(Kind::Text, |c| c.kind)
}

fn kind_of_src(cols: &[Col], src: &str) -> Kind {
    cols.iter().find(|c| c.src == src).map_or(Kind::Text, |c| c.kind)
}

fn src_of(cols: &[Col], name: &str) -> String {
    cols.iter().find(|c| c.name == name).map_or_else(|| name.to_string(), |c| c.src.clone())
}

/// A write needs the whole key and nothing else: with a missing part it would address more rows than
/// the one shown, and an empty key would address every row.
fn check_key(pk: &[String], key: &Document) -> Res<()> {
    if pk.is_empty() || key.len() != pk.len() || !pk.iter().all(|p| key.contains_key(p)) {
        return Err("Not written: a row has no complete primary key.".into());
    }
    Ok(())
}

/// `col <=> ?` with the casts that keep exact numbers exact: a decimal or a big unsigned value sent as
/// text would be compared as a double and match the wrong row.
fn equals(d: Dialect, kind: Kind, name: &str) -> String {
    let ph = match kind {
        Kind::Decimal => "CAST(? AS DECIMAL(65,30))",
        Kind::UInt => "CAST(? AS UNSIGNED)",
        _ => "?",
    };
    format!("{} <=> {ph}", d.quote_ident(name))
}

/// Floats round on the way through text, JSON is normalized by the server, BIT reads back as bytes and
/// geometry is opaque, so none of them can be compared to what was read.
fn checkable(kind: Kind) -> bool {
    !matches!(kind, Kind::Float | Kind::Json | Kind::Bit | Kind::Geometry)
}

fn key_conds(d: Dialect, cols: &[Col], pk: &Document, conds: &mut Vec<String>, params: &mut Vec<Param>) {
    for (name, v) in pk {
        let kind = kind_of_src(cols, name);
        conds.push(equals(d, kind, name));
        params.push(Param::for_kind(kind, v));
    }
}

/// Joined conditions, never empty: a statement without a key matches nothing instead of every row.
fn where_of(conds: Vec<String>) -> String {
    format!(" WHERE {}", if conds.is_empty() { "1 = 0".to_string() } else { conds.join(" AND ") })
}

/// PostgreSQL refuses a text value for an int column but never a cast from text, so every value is bound as text and its
/// placeholder is written as `CAST($n AS <the column's type>)`, with the type as the server's `format_type` spells it
/// (enums, domains, arrays and sized types included). A column the statement cannot place keeps a bare placeholder.
fn pg_cast(cols: &[Col], src: &str, v: &Bson, params: &mut Vec<Param>) -> String {
    params.push(Param::for_pg(v));
    let n = params.len();
    match cols.iter().find(|c| c.src == src).map(|c| c.ty.as_str()).filter(|t| !t.is_empty()) {
        Some(ty) => format!("CAST(${n} AS {ty})"),
        None => format!("${n}"),
    }
}

/// `UPDATE` on PostgreSQL. No `LIMIT 1`, which it does not have: `apply_updates` counts the rows instead and rolls back
/// unless exactly one changed. Key columns compare with `=` so an index is used; the old values are checked with
/// `IS NOT DISTINCT FROM` because they may be NULL.
fn pg_update_sql(schema: &str, table: &str, cols: &[Col], edit: &RowEdit) -> (String, Vec<Param>) {
    let d = Dialect::Postgres;
    let mut params = Vec::new();
    let mut sets = Vec::new();
    for (n, _, new) in &edit.changes {
        let src = src_of(cols, n);
        let v = pg_cast(cols, &src, new, &mut params);
        sets.push(format!("{} = {v}", d.quote_ident(&src)));
    }
    let mut conds = pg_key_conds(cols, &edit.pk, &mut params);
    for (n, old, _) in &edit.changes {
        let src = src_of(cols, n);
        conds.push(pg_same(cols, &src, kind_of(cols, n), old, &mut params));
    }
    (format!("UPDATE {} SET {}{}", d.qualified(schema, table), sets.join(", "), where_of(conds)), params)
}

fn pg_key_conds(cols: &[Col], pk: &Document, params: &mut Vec<Param>) -> Vec<String> {
    let d = Dialect::Postgres;
    pk.iter()
        .map(|(name, v)| {
            let v = pg_cast(cols, name, v, params);
            format!("{} = {v}", d.quote_ident(name))
        })
        .collect()
}

/// The check that a column still holds the value that was read, `IS NOT DISTINCT FROM` because it may be NULL. Every kind
/// can be compared on PostgreSQL. jsonb compares as jsonb. json has no equality operator, and a bit string or a geometry
/// is only read in its text form, so those compare as text, which the server prints the same way it printed the value read.
/// A float is compared as its own type: the shortest text that reads back as the same number is what both sides use.
fn pg_same(cols: &[Col], src: &str, kind: Kind, v: &Bson, params: &mut Vec<Param>) -> String {
    let q = Dialect::Postgres.quote_ident(src);
    let ty = cols.iter().find(|c| c.src == src).map(|c| c.ty.as_str()).unwrap_or_default();
    if matches!(kind, Kind::Bit | Kind::Geometry) || (kind == Kind::Json && ty != "jsonb") {
        params.push(Param::for_pg(v));
        return format!("{q}::text IS NOT DISTINCT FROM ${}", params.len());
    }
    let v = pg_cast(cols, src, v, params);
    format!("{q} IS NOT DISTINCT FROM {v}")
}

fn pg_delete_sql(schema: &str, table: &str, cols: &[Col], pk: &Document) -> (String, Vec<Param>) {
    let d = Dialect::Postgres;
    let mut params = Vec::new();
    let conds = pg_key_conds(cols, pk, &mut params);
    (format!("DELETE FROM {}{}", d.qualified(schema, table), where_of(conds)), params)
}

/// How many rows the key matches, stopping at two. PostgreSQL has no `LIMIT 1` on UPDATE and DELETE, so this runs first
/// and a key that is not unique is refused before the write takes a single row lock.
fn pg_count_sql(schema: &str, table: &str, cols: &[Col], pk: &Document) -> (String, Vec<Param>) {
    let mut params = Vec::new();
    let conds = pg_key_conds(cols, pk, &mut params);
    (format!("SELECT count(*)::text FROM (SELECT 1 FROM {}{} LIMIT 2) s", Dialect::Postgres.qualified(schema, table), where_of(conds)), params)
}

/// The DELETE that undoes an insert: the key and every value that was written must still be there.
fn pg_delete_inserted_sql(schema: &str, table: &str, cols: &[Col], pk: &Document, values: &Document) -> (String, Vec<Param>) {
    let mut params = Vec::new();
    let mut conds = pg_key_conds(cols, pk, &mut params);
    for (name, v) in values {
        conds.push(pg_same(cols, name, kind_of_src(cols, name), v, &mut params));
    }
    (format!("DELETE FROM {}{}", Dialect::Postgres.qualified(schema, table), where_of(conds)), params)
}

/// Refuses a key that matches more than one row, in the transaction that is about to write.
async fn pg_one_row(tx: &mut super::exec::Tx<'_>, db: &Db, schema: &str, table: &str, cols: &[Col], pk: &Document) -> Res<()> {
    let (sql, params) = pg_count_sql(schema, table, cols, pk);
    let t = Instant::now();
    let r = tx.fetch_bound(&sql, &params).await;
    db.note(schema, &sql, t, &r);
    let n = r.map_err(se)?.first().and_then(|r| r.cell(0)).and_then(|b| std::str::from_utf8(b).ok()?.parse::<u64>().ok()).unwrap_or(0);
    if n > 1 {
        return Err(SEVERAL.into());
    }
    Ok(())
}

/// `INSERT` on PostgreSQL: `DEFAULT VALUES` for an empty row, and `RETURNING` the key columns as text so the new keys
/// are known without `last_insert_id`. `override_identity` writes values into a `GENERATED ALWAYS` identity column, which
/// only the undo of a delete needs to bring the old keys back.
fn pg_insert_sql(schema: &str, table: &str, cols: &[Col], row: &NewRow, pk: &[String], override_identity: bool) -> (String, Vec<Param>) {
    let d = Dialect::Postgres;
    let t = d.qualified(schema, table);
    let ret = if pk.is_empty() { String::new() } else { format!(" RETURNING {}", pk.iter().map(|p| format!("{}::text", d.quote_ident(p))).collect::<Vec<_>>().join(", ")) };
    if row.is_empty() {
        return (format!("INSERT INTO {t} DEFAULT VALUES{ret}"), Vec::new());
    }
    let mut params = Vec::new();
    let names: Vec<String> = row.iter().map(|(n, _)| d.quote_ident(n)).collect();
    let marks: Vec<String> = row.iter().map(|(n, v)| pg_cast(cols, n, v, &mut params)).collect();
    let over = if override_identity { " OVERRIDING SYSTEM VALUE" } else { "" };
    (format!("INSERT INTO {t} ({}){over} VALUES ({}){ret}", names.join(", "), marks.join(", ")), params)
}

/// The UPDATE of one row: SET the changed columns, WHERE the key and every checkable changed column
/// still hold the value that was read.
pub fn update_sql(d: Dialect, schema: &str, table: &str, cols: &[Col], edit: &RowEdit) -> (String, Vec<Param>) {
    if d == Dialect::Postgres {
        return pg_update_sql(schema, table, cols, edit);
    }
    let mut params = Vec::new();
    let sets: Vec<String> = edit
        .changes
        .iter()
        .map(|(n, _, new)| {
            params.push(Param::for_kind(kind_of(cols, n), new));
            format!("{} = ?", d.quote_ident(&src_of(cols, n)))
        })
        .collect();
    let mut sql = format!("UPDATE {} SET {}", d.qualified(schema, table), sets.join(", "));
    let mut conds = Vec::new();
    key_conds(d, cols, &edit.pk, &mut conds, &mut params);
    for (n, old, _) in &edit.changes {
        let kind = kind_of(cols, n);
        if checkable(kind) {
            conds.push(equals(d, kind, &src_of(cols, n)));
            params.push(Param::for_kind(kind, old));
        }
    }
    sql.push_str(&where_of(conds));
    sql.push_str(" LIMIT 1");
    (sql, params)
}

pub fn delete_sql(d: Dialect, schema: &str, table: &str, cols: &[Col], pk: &Document) -> (String, Vec<Param>) {
    if d == Dialect::Postgres {
        return pg_delete_sql(schema, table, cols, pk);
    }
    let mut params = Vec::new();
    let mut sql = format!("DELETE FROM {}", d.qualified(schema, table));
    let mut conds = Vec::new();
    key_conds(d, cols, pk, &mut conds, &mut params);
    sql.push_str(&where_of(conds));
    sql.push_str(" LIMIT 1");
    (sql, params)
}

pub fn insert_sql(d: Dialect, schema: &str, table: &str, cols: &[Col], row: &NewRow) -> (String, Vec<Param>) {
    if d == Dialect::Postgres {
        return pg_insert_sql(schema, table, cols, row, &[], false);
    }
    let t = d.qualified(schema, table);
    if row.is_empty() {
        return (format!("INSERT INTO {t} () VALUES ()"), Vec::new());
    }
    let names: Vec<String> = row.iter().map(|(n, _)| d.quote_ident(n)).collect();
    let marks = vec!["?"; row.len()].join(", ");
    let params = row.iter().map(|(n, v)| Param::for_kind(kind_of_src(cols, n), v)).collect();
    (format!("INSERT INTO {t} ({}) VALUES ({marks})", names.join(", ")), params)
}

/// The same statement as it reads with its values filled in, for the Preview SQL window. Never sent.
/// `bs` is whether the server reads backslash escapes (false under NO_BACKSLASH_ESCAPES). PostgreSQL's `$n` placeholders
/// are found by the lexer, so one inside a string, a comment, a quoted name or a dollar quote is left alone, and its
/// values are spelled as the shell relay spells them.
pub fn preview(d: Dialect, sql: &str, params: &[Param], bs: bool) -> String {
    let mut out = String::new();
    if d == Dialect::Postgres {
        let mut last = 0;
        for (t, r) in lex::tokens_with(sql, d, false, false) {
            let Some(n) = (t == lex::Tok::Word).then(|| lex::param_number(&sql[r.clone()])).flatten() else { continue };
            out.push_str(&sql[last..r.start]);
            match params.get(n.wrapping_sub(1)) {
                Some(p) => out.push_str(&super::relay::literal_pg(p).unwrap_or_else(|why| format!("/* {why} */"))),
                None => out.push_str("NULL"),
            }
            last = r.end;
        }
        out.push_str(&sql[last..]);
        return out;
    }
    let mut it = params.iter();
    for ch in sql.chars() {
        if ch == '?' {
            match it.next() {
                Some(Param::Null) | None => out.push_str("NULL"),
                Some(Param::Int(i)) => out.push_str(&i.to_string()),
                Some(Param::UInt(u)) => out.push_str(&u.to_string()),
                Some(Param::Float(f)) => out.push_str(&f.to_string()),
                Some(Param::Text(s)) => out.push_str(&lit(s, bs)),
                // A bare 0x is a syntax error, so an empty value has its own spelling.
                Some(Param::Bytes(b)) if b.is_empty() => out.push_str("X''"),
                Some(Param::Bytes(b)) => out.push_str(&format!("0x{}", hex_digits(b, true))),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// A connection of its own with write access, so the pool's read-only session is never changed.
pub(super) async fn writer_conn(db: &Db, schema: &str, read_only: bool) -> Res<Conn> {
    if read_only {
        return Err("Writes are locked on this connection. Unlock them first.".into());
    }
    let mut c = db.acquire_conn().await?;
    if db.dialect == Dialect::Postgres {
        // The session stays read-only: each write opens a READ WRITE transaction of its own (`Conn::begin_write`).
        return Ok(c);
    }
    if !schema.is_empty() {
        let q = format!("USE {}", db.dialect.quote_ident(schema));
        c.execute(q.as_str()).await.map_err(se)?;
    }
    c.execute(db.dialect.session_read_only(false)).await.map_err(se)?;
    // With autocommit 0 from the server's globals, a script, a dump run back or a DDL undo would report success
    // and roll back on close. sql_select_limit would cut the reads that check a table before a change.
    c.execute("SET SESSION autocommit = 1, SESSION sql_select_limit = DEFAULT").await.map_err(se)?;
    // MariaDB's max_statement_time limits every statement, so the app timeout would cut an ALTER of a big table
    // short. MySQL's only limits SELECT; it is reset too so both engines behave the same.
    let timeout = db.dialect.session_timeout(0);
    let _ = c.execute(timeout.as_str()).await;
    Ok(c)
}

pub(super) use super::exec::bound;

fn pk_of(row: &Document) -> Document {
    row.get_document("_id").cloned().unwrap_or_default()
}

async fn apply_updates(db: &Db, schema: &str, table: &str, cols: &[Col], pk: &[String], edits: &[RowEdit], read_only: bool) -> Res<()> {
    for ed in edits {
        check_key(pk, &ed.pk)?;
    }
    let mut c = writer_conn(db, schema, read_only).await?;
    let mut tx = c.begin_write().await.map_err(se)?;
    for ed in edits {
        if db.dialect == Dialect::Postgres {
            pg_one_row(&mut tx, db, schema, table, cols, &ed.pk).await?;
        }
        let (sql, params) = update_sql(db.dialect, schema, table, cols, ed);
        let t = Instant::now();
        let r = tx.exec_bound(&sql, &params).await;
        db.note(schema, &sql, t, &r);
        let r = r.map_err(se)?;
        // The session asks for matched rows, so zero means the key or an original value no longer matches.
        if r.rows_affected() == 0 {
            return Err(CONFLICT.into());
        }
        // MySQL's LIMIT 1 makes this impossible there; PostgreSQL has none, so the count is the promise.
        if r.rows_affected() > 1 {
            let _ = tx.rollback().await;
            return Err(SEVERAL.into());
        }
    }
    tx.commit().await.map_err(se)?;
    Ok(())
}

/// Deletes by key and returns the keys that really went: a row someone else deleted first is not
/// ours to bring back on undo.
async fn apply_deletes(db: &Db, schema: &str, table: &str, cols: &[Col], key_cols: &[String], pks: &[Document], read_only: bool) -> Res<Vec<Document>> {
    for pk in pks {
        check_key(key_cols, pk)?;
    }
    let mut c = writer_conn(db, schema, read_only).await?;
    let mut tx = c.begin_write().await.map_err(se)?;
    let mut gone = Vec::new();
    for pk in pks {
        if db.dialect == Dialect::Postgres {
            pg_one_row(&mut tx, db, schema, table, cols, pk).await?;
        }
        let (sql, params) = delete_sql(db.dialect, schema, table, cols, pk);
        let t = Instant::now();
        let r = tx.exec_bound(&sql, &params).await;
        db.note(schema, &sql, t, &r);
        let n = r.map_err(se)?.rows_affected();
        if n > 1 {
            let _ = tx.rollback().await;
            return Err(SEVERAL.into());
        }
        if n == 1 {
            gone.push(pk.clone());
        }
    }
    tx.commit().await.map_err(se)?;
    Ok(gone)
}

/// Inserts the rows and returns each one's key document when it is known: the values given for the key
/// columns, or LAST_INSERT_ID() for an auto-increment key left out.
async fn apply_inserts(db: &Db, schema: &str, table: &str, cols: &[Col], pk: &[String], rows: &[NewRow], read_only: bool, restore: bool) -> Res<Vec<Option<Document>>> {
    if db.dialect == Dialect::Postgres {
        return apply_inserts_pg(db, schema, table, cols, pk, rows, read_only, restore).await;
    }
    let mut c = writer_conn(db, schema, read_only).await?;
    let mut tx = c.begin_write().await.map_err(se)?;
    let mut keys = Vec::new();
    // Generated columns cannot be given a value, and a column renamed on read goes back under its own name.
    let rows: Vec<NewRow> = rows.iter().map(|r| r.iter().filter(|(n, _)| !cols.iter().any(|c| c.name == *n && c.generated)).map(|(n, v)| (src_of(cols, n), v.clone())).collect()).collect();
    for row in &rows {
        let (sql, params) = insert_sql(db.dialect, schema, table, cols, row);
        let t = Instant::now();
        let r = tx.exec_bound(&sql, &params).await;
        db.note(schema, &sql, t, &r);
        let r = r.map_err(se)?;
        let mut key = Document::new();
        for p in pk {
            let given = row.iter().find(|(n, _)| n == p).map(|(_, v)| v).filter(|v| **v != Bson::Null);
            match given {
                Some(v) => {
                    key.insert(p.clone(), v.clone());
                }
                // LAST_INSERT_ID() only belongs to the key when that column counts up by itself.
                None if pk.len() == 1 && cols.iter().any(|c| c.src == *p && c.auto_increment) && r.last_insert_id() > 0 => {
                    let id = r.last_insert_id();
                    key.insert(p.clone(), i64::try_from(id).map_or_else(|_| Bson::String(id.to_string()), Bson::Int64));
                }
                None => {}
            }
        }
        keys.push((key.len() == pk.len() && !pk.is_empty()).then_some(key));
    }
    tx.commit().await.map_err(se)?;
    Ok(keys)
}

/// `apply_inserts` on PostgreSQL. The keys come from `RETURNING` (as text, read back by kind), so a key the server made,
/// from an identity column or a default, is known and an insert can be undone. A computed column cannot be written and is
/// left out. An identity column that is always generated is left out too, except when `restore` puts back a deleted row:
/// then its old value is written with OVERRIDING SYSTEM VALUE so the row returns under the key it had.
#[allow(clippy::too_many_arguments)]
async fn apply_inserts_pg(db: &Db, schema: &str, table: &str, cols: &[Col], pk: &[String], rows: &[NewRow], read_only: bool, restore: bool) -> Res<Vec<Option<Document>>> {
    let mut c = writer_conn(db, schema, read_only).await?;
    let mut tx = c.begin_write().await.map_err(se)?;
    let mut keys = Vec::new();
    // Identity ALWAYS says both GENERATED and auto_increment in `extra`; a computed column says only GENERATED.
    let identity_always = |name: &str| cols.iter().any(|c| c.name == name && c.generated && c.auto_increment);
    let writable = |name: &str| !cols.iter().any(|c| c.name == name && c.generated) || (restore && identity_always(name));
    for row in rows {
        let row: NewRow = row.iter().filter(|(n, _)| writable(n)).map(|(n, v)| (src_of(cols, n), v.clone())).collect();
        let over = restore && row.iter().any(|(n, _)| cols.iter().any(|c| c.src == *n && c.generated && c.auto_increment));
        let (sql, params) = pg_insert_sql(schema, table, cols, &row, pk, over);
        let t = Instant::now();
        let r = tx.fetch_bound(&sql, &params).await;
        db.note(schema, &sql, t, &r);
        let r = r.map_err(se)?;
        let mut key = Document::new();
        if let Some(got) = r.first() {
            for (i, p) in pk.iter().enumerate() {
                if let Some(raw) = got.cell(i) {
                    key.insert(p.clone(), super::value::to_bson(db.dialect, kind_of_src(cols, p), Some(raw)));
                }
            }
        }
        // The values go with the key so the undo can tell the row is still the one that was written. A column called `_id`
        // would collide with the key's slot, so it is left out of the check.
        let doc = key.len() == pk.len() && !pk.is_empty();
        let mut kept = Document::new();
        if doc {
            kept.insert("_id", key);
            for (n, v) in &row {
                if n != "_id" {
                    kept.insert(n.clone(), v.clone());
                }
            }
        }
        keys.push(doc.then_some(kept));
    }
    tx.commit().await.map_err(se)?;
    Ok(keys)
}

/// The undo of `apply_inserts_pg`: each row goes only while it still holds the values that were written, so a row someone
/// edited since, or deleted and replaced under the same key, stays. A row that no longer matches is a conflict.
async fn apply_undo_inserted_pg(db: &Db, schema: &str, table: &str, cols: &[Col], key_cols: &[String], rows: &[Document], read_only: bool) -> Res<()> {
    for r in rows {
        check_key(key_cols, &pk_of(r))?;
    }
    let mut c = writer_conn(db, schema, read_only).await?;
    let mut tx = c.begin_write().await.map_err(se)?;
    for r in rows {
        let (pk, values) = (pk_of(r), r.iter().filter(|(k, _)| *k != "_id").map(|(k, v)| (k.clone(), v.clone())).collect::<Document>());
        pg_one_row(&mut tx, db, schema, table, cols, &pk).await?;
        let (sql, params) = pg_delete_inserted_sql(schema, table, cols, &pk, &values);
        let t = Instant::now();
        let res = tx.exec_bound(&sql, &params).await;
        db.note(schema, &sql, t, &res);
        if res.map_err(se)?.rows_affected() == 0 {
            return Err(CONFLICT.into());
        }
    }
    tx.commit().await.map_err(se)?;
    Ok(())
}

/// Whether the key columns a row undo recorded are still the table's primary key. Names compare without case, as
/// the server compares column names.
fn same_key(recorded: &[String], now: &[String]) -> bool {
    let low = |v: &[String]| {
        let mut v: Vec<String> = v.iter().map(|n| n.to_lowercase()).collect();
        v.sort();
        v
    };
    low(recorded) == low(now)
}

/// A row undo finds its rows by the key columns it recorded, `<=> ? LIMIT 1` each. Once the primary key is made of
/// other columns those may match several rows, and the undo could delete or overwrite one the change never touched.
async fn key_unchanged(db: &Db, schema: &str, table: &str, recorded: &[String]) -> Res<()> {
    if recorded.is_empty() {
        return Ok(());
    }
    if db.dialect == Dialect::Postgres {
        // The index query names the primary key by `is_primary`, not by an index called PRIMARY.
        let rows = db.meta(schema, Dialect::Postgres.indexes(), [schema, table]).await?;
        let now: Vec<String> = rows.iter().filter(|r| r.int("is_primary") == Some(1)).filter_map(|r| super::worker::text(r, "col")).collect();
        return key_changed_error(table, recorded, &now);
    }
    let pool = db.my_pool()?;
    let sql = format!("SELECT COLUMN_NAME FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND INDEX_NAME = 'PRIMARY' ORDER BY SEQ_IN_INDEX{}", super::structure::ALL_ROWS);
    let t = Instant::now();
    let rows = sqlx::query(&sql).bind(schema).bind(table).fetch_all(pool).await;
    db.note(schema, &sql, t, &rows);
    let rows = rows.map_err(se)?;
    let now: Vec<String> = rows.iter().filter_map(|r| super::worker::text_at(r, 0)).collect();
    key_changed_error(table, recorded, &now)
}

fn key_changed_error(table: &str, recorded: &[String], now: &[String]) -> Res<()> {
    if same_key(recorded, now) {
        return Ok(());
    }
    let now = if now.is_empty() { "none".to_string() } else { now.join(", ") };
    Err(format!("Undo refused: the primary key of {table} is now ({now}), not ({}) as when the change ran, so its rows cannot be told apart. Nothing was changed.", recorded.join(", ")))
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// `database` is the PostgreSQL database the write ran in (`Db::pg_database`), empty on MySQL.
pub(super) fn action(database: String, conn: u64, schema: &str, table: &str, cols: &[Col], undo: SqlUndo) -> UndoAction {
    UndoAction { at: Instant::now(), conn, db: schema.into(), coll: table.into(), kind: UndoKind::Sql { database, cols: cols.to_vec(), undo } }
}

impl Worker {
    /// Saves staged edits in one transaction. Each of `tags` (one per staged row, the Mongo draft keys)
    /// gets its own `Saved` so the app can settle every draft; only the first carries the undo.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_save(&self, tags: Vec<String>, conn: u64, db: Db, schema: String, table: String, cols: Vec<Col>, pk: Vec<String>, edits: Vec<RowEdit>, read_only: bool) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let res = apply_updates(&db, &schema, &table, &cols, &pk, &edits, read_only).await;
            let n = edits.len();
            let mut undo = res.is_ok().then(|| action(db.pg_database(), conn, &schema, &table, &cols, SqlUndo::Update(edits)));
            for tag in tags {
                let res = res.clone().map(|_| format!("Saved {}", plural(n, "row")));
                let _ = tx.send(Msg::Saved { tag, res, undo: undo.take() });
            }
            ctx.request_repaint();
        });
    }

    /// Deletes rows by key. `rows` are the full rows as read, kept so undo can put them back.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_delete_rows(&self, conn: u64, db: Db, schema: String, table: String, cols: Vec<Col>, pk: Vec<String>, rows: Vec<Document>, read_only: bool) {
        self.spawn(async move {
            let at = (schema.clone(), table.clone());
            let pks: Vec<Document> = rows.iter().map(pk_of).collect();
            let (res, undo) = match apply_deletes(&db, &schema, &table, &cols, &pk, &pks, read_only).await {
                Ok(gone) => {
                    let n = gone.len();
                    let rows: Vec<Document> = rows.into_iter().filter(|r| gone.contains(&pk_of(r))).collect();
                    (Ok(format!("Deleted {}", plural(n, "row"))), (n > 0).then(|| action(db.pg_database(), conn, &schema, &table, &cols, SqlUndo::Delete(rows))))
                }
                Err(err) => (Err(err), None),
            };
            Msg::Written { at, res, undo, reload: true, retry: None }
        });
    }

    /// Inserts rows in one transaction. Undo is offered when every new row's key is known.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_insert(&self, conn: u64, db: Db, schema: String, table: String, cols: Vec<Col>, pk: Vec<String>, rows: Vec<NewRow>, read_only: bool) {
        self.spawn(async move {
            let at = (schema.clone(), table.clone());
            let (res, undo) = match apply_inserts(&db, &schema, &table, &cols, &pk, &rows, read_only, false).await {
                Ok(keys) => {
                    let known: Option<Vec<Document>> = keys.into_iter().collect();
                    let undo = |k| if db.dialect == Dialect::Postgres { SqlUndo::Inserted(k) } else { SqlUndo::Insert(k) };
                    (Ok(format!("Inserted {}", plural(rows.len(), "row"))), known.map(|k| action(db.pg_database(), conn, &schema, &table, &cols, undo(k))))
                }
                Err(err) => (Err(err), None),
            };
            Msg::Written { at, res, undo, reload: true, retry: None }
        });
    }

    /// Reverts a SQL write. Updates need the new values to still be there, like the original save. When it
    /// fails the action comes back in the answer so the app can keep it in the list.
    pub fn sql_undo(&self, db: Db, a: UndoAction, pk: Vec<String>, read_only: bool) {
        if matches!(&a.kind, UndoKind::Sql { undo: SqlUndo::CreatedDb { .. } | SqlUndo::CreatedTable { .. } | SqlUndo::CreatedSchema { .. } | SqlUndo::PgCreatedTable { .. }, .. }) {
            return self.sql_undo_create(db, a, read_only);
        }
        self.spawn(async move {
            let at = (a.db.clone(), a.coll.clone());
            let UndoAction { at: when, conn, db: schema, coll: table, kind } = a;
            if let UndoKind::Sql { undo: SqlUndo::Alter(u), .. } = &kind {
                let u = (**u).clone();
                return super::ddl::undo(&db, UndoAction { at: when, conn, db: schema, coll: table, kind }, u, read_only).await;
            }
            if let UndoKind::Sql { undo: SqlUndo::Trigger(u), .. } = &kind {
                let u = (**u).clone();
                return super::ddl::undo_trigger(&db, UndoAction { at: when, conn, db: schema, coll: table, kind }, u, read_only).await;
            }
            if let UndoKind::Sql { undo: SqlUndo::Account(u), .. } = &kind {
                let u = (**u).clone();
                return crate::db::sql_scope(db.conn, "", super::users::undo(&db, UndoAction { at: when, conn, db: schema, coll: table, kind }, u, read_only)).await;
            }
            let UndoKind::Sql { database, cols, undo } = kind else { return Msg::Written { at, res: Err("Not a SQL change".into()), undo: None, reload: false, retry: None } };
            if let Err(err) = key_unchanged(&db, &schema, &table, &undo.pk_names()).await {
                let undo = Some(UndoAction { at: when, conn, db: schema, coll: table, kind: UndoKind::Sql { database, cols, undo } });
                return Msg::Written { at, res: Err(err), undo, reload: false, retry: None };
            }
            let res = match undo.clone() {
                SqlUndo::Update(edits) => {
                    let back: Vec<RowEdit> = edits.into_iter().map(|r| RowEdit { pk: r.pk, changes: r.changes.into_iter().map(|(n, old, new)| (n, new, old)).collect() }).collect();
                    apply_updates(&db, &schema, &table, &cols, &pk, &back, read_only).await.map(|_| "Undid the edit".to_string())
                }
                SqlUndo::Delete(rows) => {
                    // The whole row goes back, the key included, so auto-increment values come back as they were.
                    let rows: Vec<NewRow> = rows.iter().map(|r| r.iter().filter(|(k, _)| *k != "_id").map(|(k, v)| (k.clone(), v.clone())).collect()).collect();
                    apply_inserts(&db, &schema, &table, &cols, &pk, &rows, read_only, true).await.map(|_| "Restored the deleted rows".to_string())
                }
                SqlUndo::Insert(pks) => apply_deletes(&db, &schema, &table, &cols, &pk, &pks, read_only).await.map(|_| "Removed the inserted rows".to_string()),
                SqlUndo::Inserted(rows) => apply_undo_inserted_pg(&db, &schema, &table, &cols, &pk, &rows, read_only).await.map(|_| "Removed the inserted rows".to_string()),
                // Taken before this match, where the structure statements run.
                SqlUndo::Alter(_) | SqlUndo::Trigger(_) | SqlUndo::Account(_) => Err("Not a row change".into()),
                SqlUndo::CreatedDb { .. } | SqlUndo::CreatedTable { .. } | SqlUndo::CreatedSchema { .. } | SqlUndo::PgCreatedTable { .. } => Err("Handled by sql_undo_create".to_string()),
            };
            let undo = res.is_err().then(|| UndoAction { at: when, conn, db: schema, coll: table, kind: UndoKind::Sql { database, cols, undo } });
            Msg::Written { at, res, undo, reload: true, retry: None }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::doc;
    use std::sync::mpsc::Receiver;
    use std::time::Duration;

    fn col(name: &str, kind: Kind) -> Col {
        Col::plain(name, "", kind, true)
    }

    #[test]
    fn update_checks_key_and_changed_columns() {
        let cols = [col("a", Kind::Int), col("b", Kind::UInt), col("price", Kind::Decimal), col("f", Kind::Float), col("j", Kind::Json)];
        let ed = RowEdit {
            pk: doc! { "a": 1, "b": "18446744073709551615" },
            changes: vec![("price".into(), Bson::String("1.10".into()), Bson::String("2.20".into())), ("f".into(), Bson::Double(1.0), Bson::Double(2.0)), ("j".into(), Bson::String("{}".into()), Bson::Null)],
        };
        let (sql, p) = update_sql(Dialect::MySql, "s", "t", &cols, &ed);
        assert_eq!(sql, "UPDATE `s`.`t` SET `price` = ?, `f` = ?, `j` = ? WHERE `a` <=> ? AND `b` <=> CAST(? AS UNSIGNED) AND `price` <=> CAST(? AS DECIMAL(65,30)) LIMIT 1");
        assert_eq!(p.len(), 6);
        assert_eq!(p[4], Param::UInt(u64::MAX));
    }

    #[test]
    fn update_without_a_key_never_runs_wide() {
        // Only float and JSON changes have no value check, so an empty key used to leave no WHERE at all.
        let cols = [col("f", Kind::Float)];
        let ed = RowEdit { pk: Document::new(), changes: vec![("f".into(), Bson::Double(1.0), Bson::Double(2.0))] };
        let (sql, _) = update_sql(Dialect::MySql, "s", "t", &cols, &ed);
        assert_eq!(sql, "UPDATE `s`.`t` SET `f` = ? WHERE 1 = 0 LIMIT 1");
        // With a value check but no key the check must not start the WHERE with AND.
        let cols = [col("n", Kind::Int)];
        let ed = RowEdit { pk: Document::new(), changes: vec![("n".into(), Bson::Int64(1), Bson::Int64(2))] };
        assert_eq!(update_sql(Dialect::MySql, "s", "t", &cols, &ed).0, "UPDATE `s`.`t` SET `n` = ? WHERE `n` <=> ? LIMIT 1");
        assert_eq!(delete_sql(Dialect::MySql, "s", "t", &cols, &Document::new()).0, "DELETE FROM `s`.`t` WHERE 1 = 0 LIMIT 1");
    }

    #[test]
    fn keys_must_be_the_whole_table_key() {
        let pk = vec!["a".to_string(), "b".to_string()];
        assert!(check_key(&pk, &doc! { "a": 1, "b": 2 }).is_ok());
        assert!(check_key(&pk, &doc! { "a": 1 }).is_err());
        assert!(check_key(&pk, &doc! { "a": 1, "c": 2 }).is_err());
        assert!(check_key(&pk, &doc! { "a": 1, "b": 2, "c": 3 }).is_err());
        assert!(check_key(&pk, &Document::new()).is_err());
        assert!(check_key(&[], &Document::new()).is_err());
    }

    #[test]
    fn statements_use_server_names_for_renamed_columns() {
        let mut c = col("_id_2", Kind::Text);
        c.src = "_id".into();
        let cols = [c, col("a", Kind::Int)];
        let ed = RowEdit { pk: doc! { "a": 1 }, changes: vec![("_id_2".into(), Bson::String("x".into()), Bson::String("y".into()))] };
        assert_eq!(update_sql(Dialect::MySql, "s", "t", &cols, &ed).0, "UPDATE `s`.`t` SET `_id` = ? WHERE `a` <=> ? AND `_id` <=> ? LIMIT 1");
    }

    #[test]
    fn delete_and_insert_statements() {
        let cols = [col("id", Kind::Int)];
        let (sql, p) = delete_sql(Dialect::MySql, "s", "t", &cols, &doc! { "id": 3 });
        assert_eq!((sql.as_str(), p), ("DELETE FROM `s`.`t` WHERE `id` <=> ? LIMIT 1", vec![Param::Int(3)]));
        assert_eq!(insert_sql(Dialect::MySql, "s", "t", &cols, &vec![]).0, "INSERT INTO `s`.`t` () VALUES ()");
        let (sql, p) = insert_sql(Dialect::MySql, "s", "t", &cols, &vec![("id".into(), Bson::Null)]);
        assert_eq!((sql.as_str(), p), ("INSERT INTO `s`.`t` (`id`) VALUES (?)", vec![Param::Null]));
    }

    fn pg_col(name: &str, ty: &str, kind: Kind) -> Col {
        Col::plain(name, ty, kind, true)
    }

    #[test]
    fn postgres_statements_cast_every_placeholder() {
        let cols = [pg_col("id", "integer", Kind::Int), pg_col("name", "character varying(20)", Kind::Text), pg_col("tags", "text[]", Kind::Array), pg_col("at", "timestamp with time zone", Kind::DateTime), pg_col("blob", "bytea", Kind::Binary), pg_col("ok", "boolean", Kind::Bool), pg_col("f", "double precision", Kind::Float), pg_col("mood", "\"Mood\"", Kind::Enum)];
        let ed = RowEdit {
            pk: doc! { "id": 7_i64 },
            changes: vec![
                ("name".into(), Bson::String("a".into()), Bson::Null),
                ("ok".into(), Bson::Boolean(true), Bson::Boolean(false)),
                ("f".into(), Bson::Double(1.5), Bson::Double(2.5)),
                ("mood".into(), Bson::String("sad".into()), Bson::String("it's".into())),
            ],
        };
        let (sql, p) = update_sql(Dialect::Postgres, "S", "T", &cols, &ed);
        assert_eq!(sql, "UPDATE \"S\".\"T\" SET \"name\" = CAST($1 AS character varying(20)), \"ok\" = CAST($2 AS boolean), \"f\" = CAST($3 AS double precision), \"mood\" = CAST($4 AS \"Mood\") WHERE \"id\" = CAST($5 AS integer) AND \"name\" IS NOT DISTINCT FROM CAST($6 AS character varying(20)) AND \"ok\" IS NOT DISTINCT FROM CAST($7 AS boolean) AND \"f\" IS NOT DISTINCT FROM CAST($8 AS double precision) AND \"mood\" IS NOT DISTINCT FROM CAST($9 AS \"Mood\")");
        assert_eq!(p, [Param::Null, Param::Text("false".into()), Param::Text("2.5".into()), Param::Text("it's".into()), Param::Text("7".into()), Param::Text("a".into()), Param::Text("true".into()), Param::Text("1.5".into()), Param::Text("sad".into())]);
        assert!(!sql.contains("LIMIT") && !sql.contains("<=>"));
        // Bytes bind as their hex text and the cast reads them back.
        let b = RowEdit { pk: doc! { "id": 1_i64 }, changes: vec![("blob".into(), Bson::Null, bytes_doc(&[1, 255]))] };
        let (sql, p) = update_sql(Dialect::Postgres, "s", "t", &cols, &b);
        assert_eq!(sql, "UPDATE \"s\".\"t\" SET \"blob\" = CAST($1 AS bytea) WHERE \"id\" = CAST($2 AS integer) AND \"blob\" IS NOT DISTINCT FROM CAST($3 AS bytea)");
        assert_eq!(p[0], Param::Text("\\x01ff".into()));
        let (sql, p) = delete_sql(Dialect::Postgres, "s", "t", &cols, &doc! { "id": 3 });
        assert_eq!((sql.as_str(), p), ("DELETE FROM \"s\".\"t\" WHERE \"id\" = CAST($1 AS integer)", vec![Param::Text("3".into())]));
        assert_eq!(delete_sql(Dialect::Postgres, "s", "t", &cols, &Document::new()).0, "DELETE FROM \"s\".\"t\" WHERE 1 = 0");
        // An update with no key never runs wide either, and its value check still follows.
        let wide = RowEdit { pk: Document::new(), changes: vec![("f".into(), Bson::Double(1.5), Bson::Double(2.5))] };
        assert_eq!(update_sql(Dialect::Postgres, "s", "t", &cols, &wide).0, "UPDATE \"s\".\"t\" SET \"f\" = CAST($1 AS double precision) WHERE \"f\" IS NOT DISTINCT FROM CAST($2 AS double precision)");
    }

    #[test]
    fn postgres_checks_json_bit_and_geometry_as_the_server_compares_them() {
        let cols = [pg_col("id", "integer", Kind::Int), pg_col("jb", "jsonb", Kind::Json), pg_col("js", "json", Kind::Json), pg_col("b", "bit(4)", Kind::Bit), pg_col("g", "geometry", Kind::Geometry)];
        let ed = RowEdit {
            pk: doc! { "id": 1_i64 },
            changes: vec![("jb".into(), Bson::String("{}".into()), Bson::Null), ("js".into(), Bson::String("[]".into()), Bson::Null), ("b".into(), Bson::String("0101".into()), Bson::Null), ("g".into(), Bson::Null, Bson::Null)],
        };
        let sql = update_sql(Dialect::Postgres, "s", "t", &cols, &ed).0;
        assert!(sql.ends_with("WHERE \"id\" = CAST($5 AS integer) AND \"jb\" IS NOT DISTINCT FROM CAST($6 AS jsonb) AND \"js\"::text IS NOT DISTINCT FROM $7 AND \"b\"::text IS NOT DISTINCT FROM $8 AND \"g\"::text IS NOT DISTINCT FROM $9"), "{sql}");
        let (sql, p) = pg_delete_inserted_sql("s", "t", &cols, &doc! { "id": 1_i64 }, &doc! { "b": "0101" });
        assert_eq!((sql.as_str(), p), ("DELETE FROM \"s\".\"t\" WHERE \"id\" = CAST($1 AS integer) AND \"b\"::text IS NOT DISTINCT FROM $2", vec![Param::Text("1".into()), Param::Text("0101".into())]));
        assert_eq!(pg_count_sql("s", "t", &cols, &doc! { "id": 1_i64 }).0, "SELECT count(*)::text FROM (SELECT 1 FROM \"s\".\"t\" WHERE \"id\" = CAST($1 AS integer) LIMIT 2) s");
    }

    fn bytes_doc(b: &[u8]) -> Bson {
        Bson::Binary(mongodb::bson::Binary { subtype: mongodb::bson::spec::BinarySubtype::Generic, bytes: b.to_vec() })
    }

    #[test]
    fn postgres_inserts_use_default_values_and_return_the_key() {
        let cols = [pg_col("id", "integer", Kind::Int), pg_col("tags", "text[]", Kind::Array)];
        assert_eq!(insert_sql(Dialect::Postgres, "s", "t", &cols, &vec![]).0, "INSERT INTO \"s\".\"t\" DEFAULT VALUES");
        let (sql, p) = insert_sql(Dialect::Postgres, "s", "t", &cols, &vec![("tags".into(), Bson::String("{a,b}".into())), ("id".into(), Bson::Null)]);
        assert_eq!((sql.as_str(), p), ("INSERT INTO \"s\".\"t\" (\"tags\", \"id\") VALUES (CAST($1 AS text[]), CAST($2 AS integer))", vec![Param::Text("{a,b}".into()), Param::Null]));
        let pk = vec!["id".to_string(), "t\"x".to_string()];
        assert_eq!(pg_insert_sql("s", "t", &cols, &vec![], &pk, false).0, "INSERT INTO \"s\".\"t\" DEFAULT VALUES RETURNING \"id\"::text, \"t\"\"x\"::text");
        assert_eq!(pg_insert_sql("s", "t", &cols, &vec![("id".into(), Bson::Int64(4))], &pk[..1], true).0, "INSERT INTO \"s\".\"t\" (\"id\") OVERRIDING SYSTEM VALUE VALUES (CAST($1 AS integer)) RETURNING \"id\"::text");
    }

    #[test]
    fn postgres_previews_fill_the_numbered_placeholders() {
        let p = [Param::Text("it's $2".into()), Param::Null, Param::Text("\\x01".into())];
        // A `$2` inside a quoted identifier is a name, and a value that looks like a placeholder is not scanned again.
        assert_eq!(preview(Dialect::Postgres, "UPDATE \"a$1\" SET x = CAST($1 AS text), y = $2 WHERE z = CAST($3 AS bytea)", &p, false), "UPDATE \"a$1\" SET x = CAST('it''s $2' AS text), y = NULL WHERE z = CAST(E'\\\\x01' AS bytea)");
        assert_eq!(preview(Dialect::MySql, "a = ?", &[Param::Int(1)], true), "a = 1");
    }

    #[test]
    fn postgres_previews_leave_a_placeholder_inside_a_string_a_comment_or_a_dollar_quote() {
        let p = [Param::Int(7)];
        assert_eq!(preview(Dialect::Postgres, "SELECT '$1', $$ $1 $$, $q$ $1 $q$, /* $1 */ $1 -- $1\n, E'$1'", &p, false), "SELECT '$1', $$ $1 $$, $q$ $1 $q$, /* $1 */ 7 -- $1\n, E'$1'");
    }

    #[test]
    fn preview_fills_values() {
        let s = preview(Dialect::MySql, "UPDATE t SET a = ?, b = ? WHERE id <=> ?", &[Param::Text("it's".into()), Param::Null, Param::Int(4)], true);
        assert_eq!(s, "UPDATE t SET a = 'it''s', b = NULL WHERE id <=> 4");
        let q = [Param::Text("a\\b".into()), Param::Bytes(vec![])];
        assert_eq!(preview(Dialect::MySql, "? ?", &q, true), "'a\\\\b' X''");
        assert_eq!(preview(Dialect::MySql, "? ?", &q, false), "'a\\b' X''");
    }

    // Live tests need QUARRY_TEST_MYSQL and use their own database so they never meet the worker tests.
    struct Live {
        w: Worker,
        rx: Receiver<Msg>,
        db: Db,
        pool: sqlx::MySqlPool,
        table: String,
    }

    fn live(table: &str, ddl: &str) -> Option<Live> {
        let uri = std::env::var("QUARRY_TEST_MYSQL").ok()?;
        let (tx, rx) = std::sync::mpsc::channel();
        let w = Worker::new(tx, eframe::egui::Context::default());
        let pool = w.rt.block_on(async {
            let p = sqlx::MySqlPool::connect(&uri).await.unwrap();
            sqlx::raw_sql("CREATE DATABASE IF NOT EXISTS quarry_write_test").execute(&p).await.unwrap();
            sqlx::raw_sql(&format!("DROP TABLE IF EXISTS quarry_write_test.{table}")).execute(&p).await.unwrap();
            sqlx::raw_sql(&format!("CREATE TABLE quarry_write_test.{table} ({ddl})")).execute(&p).await.unwrap();
            p
        });
        w.sql_connect(1, uri, Duration::from_secs(30), crate::db::Net::default());
        let Msg::SqlConnected { res: Ok((db, _, _)), .. } = rx.recv_timeout(Duration::from_secs(15)).unwrap() else { panic!("connect") };
        Some(Live { w, rx, db, pool, table: table.into() })
    }

    impl Live {
        fn exec(&self, sql: &str) {
            self.w.rt.block_on(async { sqlx::raw_sql(sql).execute(&self.pool).await.unwrap() });
        }
        fn exec_t(&self, sql: &str) {
            self.exec(&sql.replace("{T}", &self.table));
        }
        fn page(&self) -> (Vec<Col>, Vec<String>, Vec<Document>) {
            self.w.sql_page(7, self.db.clone(), "quarry_write_test".into(), self.table.clone(), Default::default(), 100, 0);
            let Msg::SqlCols { cols, pk, .. } = self.next() else { panic!("cols") };
            let Msg::Docs { res: Ok((rows, _, _)), .. } = self.next() else { panic!("docs") };
            (cols, pk, rows.iter().map(|r| r.decode().unwrap()).collect())
        }
        fn next(&self) -> Msg {
            self.rx.recv_timeout(Duration::from_secs(15)).expect("worker answer")
        }
        fn saved(&self, tag: &str) -> (Res<String>, Option<UndoAction>) {
            let Msg::Saved { tag: t, res, undo } = self.next() else { panic!("expected Saved") };
            assert_eq!(t, tag);
            (res, undo)
        }
        fn written(&self) -> (Res<String>, Option<UndoAction>) {
            let Msg::Written { res, undo, .. } = self.next() else { panic!("expected Written") };
            (res, undo)
        }
        fn save(&self, cols: &[Col], edits: Vec<RowEdit>, read_only: bool) -> (Res<String>, Option<UndoAction>) {
            let pk: Vec<String> = edits.first().map(|e| e.pk.keys().cloned().collect()).unwrap_or_default();
            self.save_pk(cols, &pk.iter().map(String::as_str).collect::<Vec<_>>(), edits, read_only)
        }
        fn save_pk(&self, cols: &[Col], pk: &[&str], edits: Vec<RowEdit>, read_only: bool) -> (Res<String>, Option<UndoAction>) {
            self.w.sql_save(vec!["k".into()], 1, self.db.clone(), "quarry_write_test".into(), self.table.clone(), cols.to_vec(), pk.iter().map(|s| s.to_string()).collect(), edits, read_only);
            self.saved("k")
        }
        fn undo_full(&self, a: UndoAction, pk: &[&str]) -> (Res<String>, Option<UndoAction>) {
            self.w.sql_undo(self.db.clone(), a, pk.iter().map(|s| s.to_string()).collect(), false);
            self.written()
        }
        fn undo(&self, a: UndoAction, pk: &[&str]) -> Res<String> {
            self.undo_full(a, pk).0
        }
        fn delete(&self, cols: &[Col], pk: &[&str], rows: Vec<Document>, read_only: bool) -> (Res<String>, Option<UndoAction>) {
            self.w.sql_delete_rows(1, self.db.clone(), "quarry_write_test".into(), self.table.clone(), cols.to_vec(), pk.iter().map(|s| s.to_string()).collect(), rows, read_only);
            self.written()
        }
    }

    fn edit(pk: Document, n: &str, old: Bson, new: Bson) -> RowEdit {
        RowEdit { pk, changes: vec![(n.into(), old, new)] }
    }

    #[test]
    fn live_save_conflict_null_and_readonly() {
        let Some(l) = live("t_save", "id INT PRIMARY KEY, name VARCHAR(20) NULL, qty INT") else { return };
        l.exec_t("INSERT INTO quarry_write_test.{T} VALUES (1,'a',1),(2,'b',2)");
        let (cols, pk, rows) = l.page();
        assert_eq!(pk, ["id"]);
        let id1 = rows[0].get_document("_id").unwrap().clone();
        let e1 = edit(id1.clone(), "name", Bson::String("a".into()), Bson::String("z".into()));
        let (res, undo) = l.save(&cols, vec![e1.clone()], false);
        assert!(res.is_ok(), "{res:?}");
        assert_eq!(l.page().2[0].get_str("name").unwrap(), "z");
        // Undo writes the original back.
        assert!(l.undo(undo.unwrap(), &["id"]).is_ok());
        assert_eq!(l.page().2[0].get_str("name").unwrap(), "a");
        // The same edit again is stale now, and a failed save writes nothing, not even its other rows.
        l.exec_t("UPDATE quarry_write_test.{T} SET name = 'changed' WHERE id = 1");
        let id2 = rows[1].get_document("_id").unwrap().clone();
        let (res, undo) = l.save(&cols, vec![edit(id2, "qty", Bson::Int64(2), Bson::Int64(9)), e1], false);
        assert_eq!(res.unwrap_err(), CONFLICT);
        assert!(undo.is_none());
        assert_eq!(l.page().2[1].get_i64("qty").unwrap(), 2);
        // Setting NULL.
        let (res, _) = l.save(&cols, vec![edit(rows[1].get_document("_id").unwrap().clone(), "name", Bson::String("b".into()), Bson::Null)], false);
        assert!(res.is_ok());
        assert_eq!(l.page().2[1].get("name"), Some(&Bson::Null));
        // Locked connections are refused in the worker.
        let (res, _) = l.save(&cols, vec![edit(id1, "qty", Bson::Int64(1), Bson::Int64(5))], true);
        assert!(res.unwrap_err().contains("locked"));
        l.exec_t("DROP TABLE quarry_write_test.{T}");
    }

    #[test]
    fn live_composite_and_decimal_keys() {
        let Some(l) = live("t_comp", "a INT, b VARCHAR(10), price DECIMAL(30,10), PRIMARY KEY (a, b)") else { return };
        l.exec_t("INSERT INTO quarry_write_test.{T} VALUES (1,'x',1.1),(1,'y',2.2),(2,'x',3.3)");
        let (cols, pk, rows) = l.page();
        assert_eq!(pk, ["a", "b"]);
        let key = rows[1].get_document("_id").unwrap().clone();
        assert_eq!(key, doc! { "a": 1_i64, "b": "y" });
        let (res, _) = l.save(&cols, vec![edit(key, "price", Bson::String("2.2000000000".into()), Bson::String("9.0000000001".into()))], false);
        assert!(res.is_ok(), "{res:?}");
        let after = l.page().2;
        assert_eq!(after[1].get_str("price").unwrap(), "9.0000000001");
        assert_eq!(after[0].get_str("price").unwrap(), "1.1000000000");
        l.exec_t("DROP TABLE quarry_write_test.{T}");
        // A DECIMAL key beyond double precision must match exactly.
        let l = live("t_dec", "d DECIMAL(30,10) PRIMARY KEY, v INT").unwrap();
        l.exec_t("INSERT INTO quarry_write_test.{T} VALUES (12345678901234567890.0000000001, 1), (12345678901234567890.0000000002, 2)");
        let (cols, _, rows) = l.page();
        let key = rows[1].get_document("_id").unwrap().clone();
        let (res, _) = l.save(&cols, vec![edit(key, "v", Bson::Int64(2), Bson::Int64(7))], false);
        assert!(res.is_ok(), "{res:?}");
        let after = l.page().2;
        assert_eq!((after[0].get_i64("v").unwrap(), after[1].get_i64("v").unwrap()), (1, 7));
        l.exec_t("DROP TABLE quarry_write_test.{T}");
    }

    #[test]
    fn live_delete_and_undo() {
        let Some(l) = live("t_del", "id INT PRIMARY KEY AUTO_INCREMENT, name VARCHAR(20)") else { return };
        l.exec_t("INSERT INTO quarry_write_test.{T} (name) VALUES ('a'),('b'),('c')");
        let (cols, _, rows) = l.page();
        let docs = vec![rows[0].clone(), rows[2].clone()];
        let (res, undo) = l.delete(&cols, &["id"], docs, false);
        assert_eq!(res.unwrap(), "Deleted 2 rows");
        assert_eq!(l.page().2.len(), 1);
        assert!(l.undo(undo.unwrap(), &["id"]).is_ok());
        let back = l.page().2;
        assert_eq!(back.len(), 3);
        assert_eq!((back[0].get_i64("id").unwrap(), back[0].get_str("name").unwrap()), (1, "a"));
        assert!(l.delete(&cols, &["id"], vec![rows[1].clone()], true).0.unwrap_err().contains("locked"));
        l.exec_t("DROP TABLE quarry_write_test.{T}");
    }

    #[test]
    fn live_insert_and_undo() {
        let Some(l) = live("t_ins", "id INT PRIMARY KEY AUTO_INCREMENT, name VARCHAR(20) DEFAULT 'dflt', n INT NULL") else { return };
        let (cols, pk, _) = l.page();
        let rows: Vec<NewRow> = vec![vec![("name".into(), Bson::String("x".into()))], vec![], vec![("n".into(), Bson::Null)]];
        l.w.sql_insert(1, l.db.clone(), "quarry_write_test".into(), l.table.clone(), cols, pk, rows, false);
        let (res, undo) = l.written();
        assert_eq!(res.unwrap(), "Inserted 3 rows");
        let got = l.page().2;
        assert_eq!(got.len(), 3);
        assert_eq!(got[1].get_str("name").unwrap(), "dflt");
        assert_eq!(got[2].get("n"), Some(&Bson::Null));
        let SqlUndo::Insert(keys) = (match &undo.as_ref().unwrap().kind { UndoKind::Sql { undo, .. } => undo.clone(), _ => panic!() }) else { panic!("insert undo") };
        assert_eq!(keys, [doc! { "id": 1_i64 }, doc! { "id": 2_i64 }, doc! { "id": 3_i64 }]);
        assert!(l.undo(undo.unwrap(), &["id"]).is_ok());
        assert!(l.page().2.is_empty());
        l.exec_t("DROP TABLE quarry_write_test.{T}");
    }

    #[test]
    fn live_empty_or_partial_key_writes_nothing() {
        let Some(l) = live("t_nokey", "a INT, f FLOAT, g FLOAT") else { return };
        l.exec_t("INSERT INTO quarry_write_test.{T} VALUES (1, 1.5, 2.5), (2, 1.5, 2.5)");
        let (cols, pk, rows) = l.page();
        assert!(pk.is_empty());
        let e = RowEdit { pk: Document::new(), changes: vec![("f".into(), Bson::Double(1.5), Bson::Double(9.0))] };
        let (res, undo) = l.save_pk(&cols, &[], vec![e], false);
        assert!(res.unwrap_err().contains("primary key"));
        assert!(undo.is_none());
        assert!(l.delete(&cols, &[], rows.clone(), false).0.is_err());
        assert_eq!(l.page().2.len(), 2);
        let n: i64 = l.w.rt.block_on(async { sqlx::query_scalar(&format!("SELECT COUNT(*) FROM quarry_write_test.{} WHERE f = 9", l.table)).fetch_one(&l.pool).await.unwrap() });
        assert_eq!(n, 0);
        l.exec_t("DROP TABLE quarry_write_test.{T}");
        // A key that lacks one of the table's key columns is refused too.
        let l = live("t_part", "a INT, b INT, v INT, PRIMARY KEY (a, b)").unwrap();
        l.exec_t("INSERT INTO quarry_write_test.{T} VALUES (1, 1, 0), (1, 2, 0)");
        let (cols, _, _) = l.page();
        let e = edit(doc! { "a": 1_i64 }, "v", Bson::Int64(0), Bson::Int64(5));
        assert!(l.save_pk(&cols, &["a", "b"], vec![e], false).0.is_err());
        assert_eq!(l.page().2.iter().filter(|r| r.get_i64("v") == Ok(0)).count(), 2);
        l.exec_t("DROP TABLE quarry_write_test.{T}");
    }

    #[test]
    fn live_undo_delete_with_generated_and_id_columns() {
        let Some(l) = live("t_gen", "id INT PRIMARY KEY AUTO_INCREMENT, `_id` VARCHAR(10), a INT, b INT GENERATED ALWAYS AS (a * 2) STORED") else { return };
        l.exec_t("INSERT INTO quarry_write_test.{T} (`_id`, a) VALUES ('x', 3), ('y', 4)");
        let (cols, pk, rows) = l.page();
        let real = cols.iter().find(|c| c.name == "_id_2").unwrap();
        assert_eq!(real.src, "_id");
        assert!(cols.iter().find(|c| c.name == "b").unwrap().generated);
        assert!(cols.iter().find(|c| c.name == "id").unwrap().auto_increment);
        // An edit of the renamed column is written to the real one.
        let (res, _) = l.save_pk(&cols, &["id"], vec![edit(rows[0].get_document("_id").unwrap().clone(), "_id_2", Bson::String("x".into()), Bson::String("z".into()))], false);
        assert!(res.is_ok(), "{res:?}");
        let rows = l.page().2;
        assert_eq!(rows[0].get_str("_id_2").unwrap(), "z");
        let pk: Vec<&str> = pk.iter().map(String::as_str).collect();
        let (res, undo) = l.delete(&cols, &pk, vec![rows[0].clone()], false);
        assert!(res.is_ok(), "{res:?}");
        let (res, _) = l.undo_full(undo.unwrap(), &pk);
        assert!(res.is_ok(), "{res:?}");
        let back = l.page().2;
        assert_eq!((back[0].get_str("_id_2").unwrap(), back[0].get_i64("a").unwrap(), back[0].get_i64("b").unwrap()), ("z", 3, 6));
        l.exec_t("DROP TABLE quarry_write_test.{T}");
    }

    #[test]
    fn live_partial_delete_and_failed_undo() {
        let Some(l) = live("t_part_del", "id INT PRIMARY KEY, v INT") else { return };
        l.exec_t("INSERT INTO quarry_write_test.{T} VALUES (1, 1), (2, 2), (3, 3)");
        let (cols, _, rows) = l.page();
        // Someone else deletes row 2 after the page was read.
        l.exec_t("DELETE FROM quarry_write_test.{T} WHERE id = 2");
        let (res, undo) = l.delete(&cols, &["id"], rows[..2].to_vec(), false);
        assert_eq!(res.unwrap(), "Deleted 1 row");
        let undo = undo.unwrap();
        let UndoKind::Sql { undo: SqlUndo::Delete(kept), .. } = &undo.kind else { panic!("delete undo") };
        assert_eq!(kept.len(), 1, "only the row this delete removed is brought back");
        // A row with the same key appears, so the undo fails and the entry must come back.
        l.exec_t("INSERT INTO quarry_write_test.{T} VALUES (1, 99)");
        let (res, back) = l.undo_full(undo, &["id"]);
        assert!(res.is_err());
        let back = back.expect("a failed undo keeps its entry");
        l.exec_t("DELETE FROM quarry_write_test.{T} WHERE id = 1");
        assert!(l.undo(back, &["id"]).is_ok());
        let ids: Vec<i64> = l.page().2.iter().map(|r| r.get_i64("id").unwrap()).collect();
        assert_eq!(ids, [1, 3]);
        l.exec_t("DROP TABLE quarry_write_test.{T}");
    }

    #[test]
    fn live_insert_undo_trusts_only_auto_increment_keys() {
        let Some(l) = live("t_na", "id INT PRIMARY KEY DEFAULT 7, seq INT AUTO_INCREMENT UNIQUE") else { return };
        let (cols, pk, _) = l.page();
        assert!(cols.iter().find(|c| c.name == "seq").unwrap().auto_increment);
        // LAST_INSERT_ID() belongs to seq here, not to the key.
        l.w.sql_insert(1, l.db.clone(), "quarry_write_test".into(), l.table.clone(), cols.clone(), pk.clone(), vec![vec![]], false);
        let (res, undo) = l.written();
        assert!(res.is_ok(), "{res:?}");
        assert!(undo.is_none());
        // A key given in the values is known.
        l.w.sql_insert(1, l.db.clone(), "quarry_write_test".into(), l.table.clone(), cols, pk, vec![vec![("id".into(), Bson::Int64(9))]], false);
        let (_, undo) = l.written();
        let UndoKind::Sql { undo: SqlUndo::Insert(keys), .. } = undo.unwrap().kind else { panic!("insert undo") };
        assert_eq!(keys, [doc! { "id": 9_i64 }]);
        l.exec_t("DROP TABLE quarry_write_test.{T}");
    }
}
