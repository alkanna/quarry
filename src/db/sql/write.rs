//! Writing SQL rows: saving staged cell edits, deleting rows and inserting them, each in one
//! transaction on a connection of its own that is switched to READ WRITE. The pool stays read-only.

use super::value::{Kind, Param};
use super::{Col, Db, Dialect, Pool};
use crate::db::{e, Msg, Res, UndoAction, UndoKind, Worker};
use mongodb::bson::{Bson, Document};
use sqlx::{Connection, Executor};
use std::time::Instant;

pub const CONFLICT: &str = "Not saved: a row changed on the server since you opened it. Nothing was written. Reload and try again.";

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
}

impl SqlUndo {
    /// Key column names, read off the first row: undo of a delete re-inserts the rows and needs them.
    pub fn pk_names(&self) -> Vec<String> {
        let first = match self {
            SqlUndo::Update(r) => r.first().map(|r| r.pk.clone()),
            SqlUndo::Delete(r) => r.first().map(pk_of),
            SqlUndo::Insert(r) => r.first().cloned(),
        };
        first.map(|d| d.keys().cloned().collect()).unwrap_or_default()
    }

    /// For the Recent changes list.
    pub fn describe(&self) -> String {
        match self {
            SqlUndo::Update(r) => format!("edited {}", plural(r.len(), "row")),
            SqlUndo::Delete(r) => format!("deleted {}", plural(r.len(), "row")),
            SqlUndo::Insert(r) => format!("inserted {}", plural(r.len(), "row")),
        }
    }
}

/// A row to insert: only the columns given a value; the rest take the column default.
pub type NewRow = Vec<(String, Bson)>;

fn kind_of(cols: &[Col], name: &str) -> Kind {
    cols.iter().find(|c| c.name == name).map_or(Kind::Text, |c| c.kind)
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

fn key_where(d: Dialect, cols: &[Col], pk: &Document, sql: &mut String, params: &mut Vec<Param>) {
    for (i, (name, v)) in pk.iter().enumerate() {
        let kind = kind_of(cols, name);
        sql.push_str(if i == 0 { " WHERE " } else { " AND " });
        sql.push_str(&equals(d, kind, name));
        params.push(Param::for_kind(kind, v));
    }
}

/// The UPDATE of one row: SET the changed columns, WHERE the key and every checkable changed column
/// still hold the value that was read.
pub fn update_sql(d: Dialect, schema: &str, table: &str, cols: &[Col], edit: &RowEdit) -> (String, Vec<Param>) {
    let mut params = Vec::new();
    let sets: Vec<String> = edit
        .changes
        .iter()
        .map(|(n, _, new)| {
            params.push(Param::for_kind(kind_of(cols, n), new));
            format!("{} = ?", d.quote_ident(n))
        })
        .collect();
    let mut sql = format!("UPDATE {} SET {}", d.qualified(schema, table), sets.join(", "));
    key_where(d, cols, &edit.pk, &mut sql, &mut params);
    for (n, old, _) in &edit.changes {
        let kind = kind_of(cols, n);
        if checkable(kind) {
            sql.push_str(" AND ");
            sql.push_str(&equals(d, kind, n));
            params.push(Param::for_kind(kind, old));
        }
    }
    (sql, params)
}

pub fn delete_sql(d: Dialect, schema: &str, table: &str, cols: &[Col], pk: &Document) -> (String, Vec<Param>) {
    let mut params = Vec::new();
    let mut sql = format!("DELETE FROM {}", d.qualified(schema, table));
    key_where(d, cols, pk, &mut sql, &mut params);
    sql.push_str(" LIMIT 1");
    (sql, params)
}

pub fn insert_sql(d: Dialect, schema: &str, table: &str, cols: &[Col], row: &NewRow) -> (String, Vec<Param>) {
    let t = d.qualified(schema, table);
    if row.is_empty() {
        return (format!("INSERT INTO {t} () VALUES ()"), Vec::new());
    }
    let names: Vec<String> = row.iter().map(|(n, _)| d.quote_ident(n)).collect();
    let marks = vec!["?"; row.len()].join(", ");
    let params = row.iter().map(|(n, v)| Param::for_kind(kind_of(cols, n), v)).collect();
    (format!("INSERT INTO {t} ({}) VALUES ({marks})", names.join(", ")), params)
}

/// The same statement as it reads with its values filled in, for the Preview SQL window. Never sent.
pub fn preview(sql: &str, params: &[Param]) -> String {
    let mut out = String::new();
    let mut it = params.iter();
    for ch in sql.chars() {
        if ch == '?' {
            match it.next() {
                Some(Param::Null) | None => out.push_str("NULL"),
                Some(Param::Int(i)) => out.push_str(&i.to_string()),
                Some(Param::UInt(u)) => out.push_str(&u.to_string()),
                Some(Param::Float(f)) => out.push_str(&f.to_string()),
                Some(Param::Text(s)) => out.push_str(&format!("'{}'", s.replace('\\', "\\\\").replace('\'', "''"))),
                Some(Param::Bytes(b)) => out.push_str(&format!("0x{}", b.iter().map(|x| format!("{x:02X}")).collect::<String>())),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

type Query<'q> = sqlx::query::Query<'q, sqlx::MySql, sqlx::mysql::MySqlArguments>;

fn bound<'q>(sql: &'q str, params: &'q [Param]) -> Query<'q> {
    let mut q = sqlx::query(sql);
    for p in params {
        q = match p {
            Param::Null => q.bind(None::<String>),
            Param::Int(i) => q.bind(*i),
            Param::UInt(u) => q.bind(*u),
            Param::Float(f) => q.bind(*f),
            Param::Text(s) => q.bind(s.as_str()),
            Param::Bytes(b) => q.bind(b.as_slice()),
        };
    }
    q
}

/// A connection of its own with write access, so the pool's read-only session is never changed.
async fn writer(db: &Db, schema: &str, read_only: bool) -> Res<sqlx::MySqlConnection> {
    if read_only {
        return Err("Writes are locked on this connection. Unlock them first.".into());
    }
    let Pool::My(pool) = &db.pool;
    let mut c = pool.acquire().await.map_err(e)?.detach();
    if !schema.is_empty() {
        let q = format!("USE {}", db.dialect.quote_ident(schema));
        c.execute(q.as_str()).await.map_err(e)?;
    }
    c.execute(db.dialect.session_read_only(false)).await.map_err(e)?;
    Ok(c)
}

fn pk_of(row: &Document) -> Document {
    row.get_document("_id").cloned().unwrap_or_default()
}

async fn apply_updates(db: &Db, schema: &str, table: &str, cols: &[Col], edits: &[RowEdit], read_only: bool) -> Res<()> {
    let mut c = writer(db, schema, read_only).await?;
    let mut tx = c.begin().await.map_err(e)?;
    for ed in edits {
        let (sql, params) = update_sql(db.dialect, schema, table, cols, ed);
        let r = bound(&sql, &params).execute(&mut *tx).await.map_err(e)?;
        // The session asks for matched rows, so zero means the key or an original value no longer matches.
        if r.rows_affected() == 0 {
            return Err(CONFLICT.into());
        }
    }
    tx.commit().await.map_err(e)?;
    Ok(())
}

async fn apply_deletes(db: &Db, schema: &str, table: &str, cols: &[Col], pks: &[Document], read_only: bool) -> Res<u64> {
    let mut c = writer(db, schema, read_only).await?;
    let mut tx = c.begin().await.map_err(e)?;
    let mut n = 0;
    for pk in pks {
        let (sql, params) = delete_sql(db.dialect, schema, table, cols, pk);
        n += bound(&sql, &params).execute(&mut *tx).await.map_err(e)?.rows_affected();
    }
    tx.commit().await.map_err(e)?;
    Ok(n)
}

/// Inserts the rows and returns each one's key document when it is known: the values given for the key
/// columns, or LAST_INSERT_ID() for an auto-increment key left out.
async fn apply_inserts(db: &Db, schema: &str, table: &str, cols: &[Col], pk: &[String], rows: &[NewRow], read_only: bool) -> Res<Vec<Option<Document>>> {
    let mut c = writer(db, schema, read_only).await?;
    let mut tx = c.begin().await.map_err(e)?;
    let mut keys = Vec::new();
    for row in rows {
        let (sql, params) = insert_sql(db.dialect, schema, table, cols, row);
        let r = bound(&sql, &params).execute(&mut *tx).await.map_err(e)?;
        let mut key = Document::new();
        for p in pk {
            match row.iter().find(|(n, _)| n == p) {
                Some((_, v)) if *v != Bson::Null => {
                    key.insert(p.clone(), v.clone());
                }
                // Only a single auto-increment column leaves a key behind; id 0 means the table has none.
                None if pk.len() == 1 && r.last_insert_id() > 0 => {
                    let id = r.last_insert_id();
                    key.insert(p.clone(), i64::try_from(id).map_or_else(|_| Bson::String(id.to_string()), Bson::Int64));
                }
                _ => {}
            }
        }
        keys.push((key.len() == pk.len() && !pk.is_empty()).then_some(key));
    }
    tx.commit().await.map_err(e)?;
    Ok(keys)
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

fn action(conn: u64, schema: &str, table: &str, cols: &[Col], undo: SqlUndo) -> UndoAction {
    UndoAction { at: Instant::now(), conn, db: schema.into(), coll: table.into(), kind: UndoKind::Sql { cols: cols.to_vec(), undo } }
}

impl Worker {
    /// Saves staged edits in one transaction. Each of `tags` (one per staged row, the Mongo draft keys)
    /// gets its own `Saved` so the app can settle every draft; only the first carries the undo.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_save(&self, tags: Vec<String>, conn: u64, db: Db, schema: String, table: String, cols: Vec<Col>, edits: Vec<RowEdit>, read_only: bool) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let res = apply_updates(&db, &schema, &table, &cols, &edits, read_only).await;
            let n = edits.len();
            let mut undo = res.is_ok().then(|| action(conn, &schema, &table, &cols, SqlUndo::Update(edits)));
            for tag in tags {
                let res = res.clone().map(|_| format!("Saved {}", plural(n, "row")));
                let _ = tx.send(Msg::Saved { tag, res, undo: undo.take() });
            }
            ctx.request_repaint();
        });
    }

    /// Deletes rows by key. `rows` are the full rows as read, kept so undo can put them back.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_delete_rows(&self, conn: u64, db: Db, schema: String, table: String, cols: Vec<Col>, rows: Vec<Document>, read_only: bool) {
        self.spawn(async move {
            let at = (schema.clone(), table.clone());
            let pks: Vec<Document> = rows.iter().map(pk_of).collect();
            let (res, undo) = match apply_deletes(&db, &schema, &table, &cols, &pks, read_only).await {
                Ok(n) => (Ok(format!("Deleted {}", plural(n as usize, "row"))), (n > 0).then(|| action(conn, &schema, &table, &cols, SqlUndo::Delete(rows)))),
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
            let (res, undo) = match apply_inserts(&db, &schema, &table, &cols, &pk, &rows, read_only).await {
                Ok(keys) => {
                    let known: Option<Vec<Document>> = keys.into_iter().collect();
                    (Ok(format!("Inserted {}", plural(rows.len(), "row"))), known.map(|k| action(conn, &schema, &table, &cols, SqlUndo::Insert(k))))
                }
                Err(err) => (Err(err), None),
            };
            Msg::Written { at, res, undo, reload: true, retry: None }
        });
    }

    /// Reverts a SQL write. Updates need the new values to still be there, like the original save.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_undo(&self, db: Db, schema: String, table: String, cols: Vec<Col>, pk: Vec<String>, undo: SqlUndo, read_only: bool) {
        self.spawn(async move {
            let at = (schema.clone(), table.clone());
            let res = match undo {
                SqlUndo::Update(edits) => {
                    let back: Vec<RowEdit> = edits.into_iter().map(|r| RowEdit { pk: r.pk, changes: r.changes.into_iter().map(|(n, old, new)| (n, new, old)).collect() }).collect();
                    apply_updates(&db, &schema, &table, &cols, &back, read_only).await.map(|_| "Undid the edit".to_string())
                }
                SqlUndo::Delete(rows) => {
                    // The whole row goes back, the key included, so auto-increment values come back as they were.
                    let rows: Vec<NewRow> = rows.iter().map(|r| r.iter().filter(|(k, _)| *k != "_id").map(|(k, v)| (k.clone(), v.clone())).collect()).collect();
                    apply_inserts(&db, &schema, &table, &cols, &pk, &rows, read_only).await.map(|_| "Restored the deleted rows".to_string())
                }
                SqlUndo::Insert(pks) => apply_deletes(&db, &schema, &table, &cols, &pks, read_only).await.map(|_| "Removed the inserted rows".to_string()),
            };
            Msg::Written { at, res, undo: None, reload: true, retry: None }
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
        Col { name: name.into(), ty: String::new(), kind, nullable: true }
    }

    #[test]
    fn update_checks_key_and_changed_columns() {
        let cols = [col("a", Kind::Int), col("b", Kind::UInt), col("price", Kind::Decimal), col("f", Kind::Float), col("j", Kind::Json)];
        let ed = RowEdit {
            pk: doc! { "a": 1, "b": "18446744073709551615" },
            changes: vec![("price".into(), Bson::String("1.10".into()), Bson::String("2.20".into())), ("f".into(), Bson::Double(1.0), Bson::Double(2.0)), ("j".into(), Bson::String("{}".into()), Bson::Null)],
        };
        let (sql, p) = update_sql(Dialect::MySql, "s", "t", &cols, &ed);
        assert_eq!(sql, "UPDATE `s`.`t` SET `price` = ?, `f` = ?, `j` = ? WHERE `a` <=> ? AND `b` <=> CAST(? AS UNSIGNED) AND `price` <=> CAST(? AS DECIMAL(65,30))");
        assert_eq!(p.len(), 6);
        assert_eq!(p[4], Param::UInt(u64::MAX));
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

    #[test]
    fn preview_fills_values() {
        let s = preview("UPDATE t SET a = ?, b = ? WHERE id <=> ?", &[Param::Text("it's".into()), Param::Null, Param::Int(4)]);
        assert_eq!(s, "UPDATE t SET a = 'it''s', b = NULL WHERE id <=> 4");
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
            self.w.sql_save(vec!["k".into()], 1, self.db.clone(), "quarry_write_test".into(), self.table.clone(), cols.to_vec(), edits, read_only);
            self.saved("k")
        }
        fn undo(&self, a: UndoAction, pk: &[&str]) -> Res<String> {
            let UndoKind::Sql { cols, undo } = a.kind else { panic!("sql undo") };
            self.w.sql_undo(self.db.clone(), a.db, a.coll, cols, pk.iter().map(|s| s.to_string()).collect(), undo, false);
            self.written().0
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
        l.w.sql_delete_rows(1, l.db.clone(), "quarry_write_test".into(), l.table.clone(), cols.clone(), docs, false);
        let (res, undo) = l.written();
        assert_eq!(res.unwrap(), "Deleted 2 rows");
        assert_eq!(l.page().2.len(), 1);
        assert!(l.undo(undo.unwrap(), &["id"]).is_ok());
        let back = l.page().2;
        assert_eq!(back.len(), 3);
        assert_eq!((back[0].get_i64("id").unwrap(), back[0].get_str("name").unwrap()), (1, "a"));
        l.w.sql_delete_rows(1, l.db.clone(), "quarry_write_test".into(), l.table.clone(), cols, vec![rows[1].clone()], true);
        assert!(l.written().0.unwrap_err().contains("locked"));
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
}
