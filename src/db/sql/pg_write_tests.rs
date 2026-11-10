//! Live tests of writing rows on PostgreSQL, run on every server named by QUARRY_TEST_POSTGRES and
//! QUARRY_TEST_POSTGRES13. Each test makes its own schema in the shared `quarry_test` database.

use super::exec::Cells;
use super::testing::{every_pg, LivePg};
use super::value::{parse_edit_for, Kind};
use super::write::{NewRow, RowEdit, SqlUndo, CONFLICT, SEVERAL};
use super::{Col, Db, Dialect, PageQuery};
use crate::db::{Msg, Res, UndoAction, UndoKind};
use mongodb::bson::{doc, Bson, Document};
use sqlx::{Connection, Executor};

struct T<'a> {
    l: &'a LivePg,
    db: Db,
    pool: sqlx::PgPool,
    schema: String,
}

fn fixture<'a>(l: &'a LivePg, schema: &str, ddl: &str) -> T<'a> {
    let db = l.quarry_test();
    let pool = l.fixture_pool();
    l.qsql(&pool, &format!("DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema}; SET search_path = {schema}; {ddl}"));
    T { l, db, pool, schema: schema.into() }
}

impl T<'_> {
    fn sql(&self, s: &str) {
        self.l.qsql(&self.pool, &format!("SET search_path = {}; {s}", self.schema));
    }

    fn one(&self, s: &str) -> String {
        self.l.qone(&self.pool, &format!("SET search_path = {}; {s}", self.schema))
    }

    fn page(&self, table: &str) -> (Vec<Col>, Vec<String>, Vec<Document>) {
        self.l.w.sql_page(7, self.db.clone(), self.schema.clone(), table.into(), PageQuery::default(), 100, 0);
        let Msg::SqlCols { cols, pk, .. } = self.l.next() else { panic!("expected SqlCols") };
        let Msg::Docs { res: Ok((rows, _, _)), .. } = self.l.next() else { panic!("expected Docs") };
        (cols, pk, rows.iter().map(|r| r.decode().unwrap()).collect())
    }

    /// The row whose `col` holds `val`. A page has no order, and an updated row moves to the end of the heap.
    fn row(&self, table: &str, col: &str, val: i64) -> Document {
        self.page(table).2.into_iter().find(|r| r.get_i64(col) == Ok(val)).unwrap_or_else(|| panic!("no row with {col} = {val}"))
    }

    fn ids(&self, table: &str) -> Vec<i64> {
        let mut v: Vec<i64> = self.page(table).2.iter().map(|r| r.get_i64("id").unwrap()).collect();
        v.sort();
        v
    }

    fn save(&self, table: &str, cols: &[Col], pk: &[&str], edits: Vec<RowEdit>, read_only: bool) -> (Res<String>, Option<UndoAction>) {
        self.l.w.sql_save(vec!["k".into()], 1, self.db.clone(), self.schema.clone(), table.into(), cols.to_vec(), pk.iter().map(|s| s.to_string()).collect(), edits, read_only);
        let Msg::Saved { res, undo, .. } = self.l.next() else { panic!("expected Saved") };
        (res, undo)
    }

    fn written(&self) -> (Res<String>, Option<UndoAction>) {
        let Msg::Written { res, undo, .. } = self.l.next() else { panic!("expected Written") };
        (res, undo)
    }

    fn delete(&self, table: &str, cols: &[Col], pk: &[&str], rows: Vec<Document>, read_only: bool) -> (Res<String>, Option<UndoAction>) {
        self.l.w.sql_delete_rows(1, self.db.clone(), self.schema.clone(), table.into(), cols.to_vec(), pk.iter().map(|s| s.to_string()).collect(), rows, read_only);
        self.written()
    }

    fn insert(&self, table: &str, cols: &[Col], pk: &[&str], rows: Vec<NewRow>, read_only: bool) -> (Res<String>, Option<UndoAction>) {
        self.l.w.sql_insert(1, self.db.clone(), self.schema.clone(), table.into(), cols.to_vec(), pk.iter().map(|s| s.to_string()).collect(), rows, read_only);
        self.written()
    }

    fn undo(&self, a: UndoAction, pk: &[&str]) -> (Res<String>, Option<UndoAction>) {
        self.l.w.sql_undo(self.db.clone(), a, pk.iter().map(|s| s.to_string()).collect(), false);
        self.written()
    }
}

fn pk_doc(row: &Document) -> Document {
    row.get_document("_id").unwrap().clone()
}

fn kind_of(cols: &[Col], name: &str) -> Kind {
    cols.iter().find(|c| c.name == name).unwrap().kind
}

fn bin(b: &[u8]) -> Bson {
    Bson::Binary(mongodb::bson::Binary { subtype: mongodb::bson::spec::BinarySubtype::Generic, bytes: b.to_vec() })
}

fn undo_kind(a: &UndoAction) -> &SqlUndo {
    let UndoKind::Sql { undo, .. } = &a.kind else { panic!("not a SQL undo") };
    undo
}

const KINDS_DDL: &str = "CREATE TYPE mood AS ENUM ('happy', 'sad', 'ok');
    CREATE DOMAIN posint AS int CHECK (VALUE > 0);
    CREATE TABLE t (id int PRIMARY KEY, i int, t text, b bool, n numeric(10,2), by bytea, u uuid, j jsonb, ia int[], iv interval, ip inet, m mood, d posint, ts timestamptz, f float8);
    INSERT INTO t VALUES (1, 5, 'a', true, 1.50, '\\x0102', '123e4567-e89b-12d3-a456-426614174000', '{\"a\": 1}', '{1,2}', '1 day', '10.0.0.1', 'happy', 5, '2024-01-01 00:00:00+00', 1.5),
                         (2, 9, 'z', false, 9.99, '\\x', '00000000-0000-0000-0000-000000000000', '[]', '{}', '0', '::1', 'ok', 9, '2020-02-02 02:02:02+00', 2.5);";

#[test]
fn every_kind_updates_and_sets_null() {
    every_pg(|l, var| {
        let t = fixture(l, "t5_kinds", KINDS_DDL);
        let (cols, pk, rows) = t.page("t");
        assert_eq!(pk, ["id"], "{var}");
        let key = pk_doc(&rows[0]);
        // (column, text as typed in the editor, what the page shows afterwards)
        let steps: [(&str, &str, Bson); 14] = [
            ("i", "6", Bson::Int64(6)),
            ("t", "b it's", Bson::String("b it's".into())),
            ("b", "no", Bson::Boolean(false)),
            ("n", "2.25", Bson::String("2.25".into())),
            ("by", "0xFF00", bin(&[255, 0])),
            ("u", "{123E4567-E89B-12D3-A456-426614174999}", Bson::String("123e4567-e89b-12d3-a456-426614174999".into())),
            ("j", "{\"a\":2}", Bson::String("{\"a\": 2}".into())),
            ("ia", "{3,4,5}", Bson::String("{3,4,5}".into())),
            ("iv", "2 days 03:00:00", Bson::String("2 days 03:00:00".into())),
            ("ip", "192.168.1.5", Bson::String("192.168.1.5".into())),
            ("m", "sad", Bson::String("sad".into())),
            ("d", "7", Bson::Int64(7)),
            ("ts", "2025-06-01 12:30:00+00", Bson::String("2025-06-01 12:30:00+00".into())),
            ("f", "0.1", Bson::Double(0.1)),
        ];
        for (col, text, want) in steps {
            let old = t.row("t", "id", 1).get(col).cloned().unwrap();
            let new = parse_edit_for(Dialect::Postgres, kind_of(&cols, col), text).unwrap_or_else(|x| panic!("{var} {col}: {x}"));
            let (res, undo) = t.save("t", &cols, &["id"], vec![RowEdit { pk: key.clone(), changes: vec![(col.into(), old.clone(), new)] }], false);
            assert!(res.is_ok(), "{var} {col}: {res:?}");
            assert_eq!(t.row("t", "id", 1).get(col), Some(&want), "{var} {col}");
            // Undo writes the original back through the same cast.
            let (res, _) = t.undo(undo.unwrap(), &["id"]);
            assert!(res.is_ok(), "{var} {col} undo: {res:?}");
            assert_eq!(t.row("t", "id", 1).get(col), Some(&old), "{var} {col} undone");
        }
        // The other row was never touched.
        assert_eq!(t.row("t", "id", 2).get_str("t").unwrap(), "z");
        // Every column can be set to NULL and back, and a NULL old value is matched by IS NOT DISTINCT FROM.
        for col in ["i", "t", "b", "n", "by", "u", "j", "ia", "iv", "ip", "m", "d", "ts", "f"] {
            let old = t.row("t", "id", 1).get(col).cloned().unwrap();
            let (res, _) = t.save("t", &cols, &["id"], vec![RowEdit { pk: key.clone(), changes: vec![(col.into(), old.clone(), Bson::Null)] }], false);
            assert!(res.is_ok(), "{var} {col} null: {res:?}");
            assert_eq!(t.row("t", "id", 1).get(col), Some(&Bson::Null), "{var} {col}");
            let (res, _) = t.save("t", &cols, &["id"], vec![RowEdit { pk: key.clone(), changes: vec![(col.into(), Bson::Null, old.clone())] }], false);
            assert!(res.is_ok(), "{var} {col} back: {res:?}");
            assert_eq!(t.row("t", "id", 1).get(col), Some(&old), "{var} {col} restored");
        }
        // The server's own cast error comes back for text it cannot read, and nothing is written.
        let old = Bson::Int64(5);
        let bad = RowEdit { pk: key.clone(), changes: vec![("m".into(), Bson::String("happy".into()), Bson::String("furious".into()))] };
        let (res, undo) = t.save("t", &cols, &["id"], vec![bad], false);
        let err = res.unwrap_err();
        assert!(err.contains("invalid input value for enum"), "{var}: {err}");
        assert_eq!(crate::db::explain(&err, &crate::db::ErrCtx::default()).title, "That value is not valid for the column's type");
        assert!(undo.is_none());
        let bad = RowEdit { pk: key.clone(), changes: vec![("d".into(), Bson::Int64(5), Bson::Int64(-1))] };
        let err = t.save("t", &cols, &["id"], vec![bad], false).0.unwrap_err();
        assert!(err.contains("posint"), "{var}: {err}");
        let bad = RowEdit { pk: key, changes: vec![("i".into(), old, Bson::String("abc".into()))] };
        let err = t.save("t", &cols, &["id"], vec![bad], false).0.unwrap_err();
        assert!(err.contains("invalid input syntax for type integer"), "{var}: {err}");
        assert_eq!(t.row("t", "id", 1).get_i64("i").unwrap(), 5);
    });
}

#[test]
fn a_stale_old_value_refuses_the_whole_save() {
    every_pg(|l, var| {
        let t = fixture(l, "t5_stale", "CREATE TABLE t (id int PRIMARY KEY, name text, qty int); INSERT INTO t VALUES (1, 'a', 1), (2, 'b', 2);");
        let (cols, _, rows) = t.page("t");
        t.sql("UPDATE t SET name = 'changed' WHERE id = 1");
        let stale = RowEdit { pk: pk_doc(&rows[0]), changes: vec![("name".into(), Bson::String("a".into()), Bson::String("z".into()))] };
        let fine = RowEdit { pk: pk_doc(&rows[1]), changes: vec![("qty".into(), Bson::Int64(2), Bson::Int64(9))] };
        let (res, undo) = t.save("t", &cols, &["id"], vec![fine, stale], false);
        assert_eq!(res.unwrap_err(), CONFLICT, "{var}");
        assert!(undo.is_none());
        assert_eq!(t.one("SELECT qty::text FROM t WHERE id = 2"), "2", "{var}: the first row of a failed save was kept");
        assert_eq!(t.one("SELECT name FROM t WHERE id = 1"), "changed");
        // A row that is gone is a conflict too.
        t.sql("DELETE FROM t WHERE id = 2");
        let gone = RowEdit { pk: pk_doc(&rows[1]), changes: vec![("qty".into(), Bson::Int64(2), Bson::Int64(3))] };
        assert_eq!(t.save("t", &cols, &["id"], vec![gone], false).0.unwrap_err(), CONFLICT);
    });
}

#[test]
fn a_key_matching_two_rows_is_refused_and_rolled_back() {
    every_pg(|l, var| {
        let t = fixture(l, "t5_dup", "CREATE TABLE t (k int, v int, w text); INSERT INTO t VALUES (1, 10, 'a'), (1, 20, 'b'), (2, 30, 'c');");
        let (cols, pk, rows) = t.page("t");
        // Without a primary key the table is not editable: the page names no key and every write is refused.
        assert!(pk.is_empty(), "{var}");
        let ed = RowEdit { pk: Document::new(), changes: vec![("w".into(), Bson::String("a".into()), Bson::String("q".into()))] };
        let (res, _) = t.save("t", &cols, &[], vec![ed], false);
        assert!(res.unwrap_err().contains("primary key"));
        assert!(t.delete("t", &cols, &[], rows.clone(), false).0.unwrap_err().contains("primary key"));
        // A key that the app trusts but the table does not enforce: two rows match, so the write is undone.
        let fake = doc! { "k": 1_i64 };
        // v is the same on neither row, so the old-value check alone would not catch it; w equal makes both rows match.
        t.sql("UPDATE t SET w = 'same' WHERE k = 1");
        let ed = RowEdit { pk: fake.clone(), changes: vec![("w".into(), Bson::String("same".into()), Bson::String("q".into()))] };
        let (res, undo) = t.save("t", &cols, &["k"], vec![ed], false);
        assert_eq!(res.unwrap_err(), SEVERAL, "{var}");
        assert!(undo.is_none());
        assert_eq!(t.one("SELECT count(*)::text FROM t WHERE w = 'same'"), "2", "{var}: the update was not rolled back");
        let (res, undo) = t.delete("t", &cols, &["k"], vec![doc! { "_id": fake.clone(), "k": 1_i64 }], false);
        assert_eq!(res.unwrap_err(), SEVERAL);
        assert!(undo.is_none());
        assert_eq!(t.one("SELECT count(*)::text FROM t"), "3", "{var}: the delete was not rolled back");
        // A row the key does match once goes through.
        let (res, _) = t.delete("t", &cols, &["k"], vec![doc! { "_id": doc! { "k": 2_i64 }, "k": 2_i64 }], false);
        assert_eq!(res.unwrap(), "Deleted 1 row");
    });
}

#[test]
fn inserts_use_identity_defaults_and_return_the_keys() {
    every_pg(|l, var| {
        let t = fixture(l, "t5_ins", "CREATE TABLE t (id int GENERATED ALWAYS AS IDENTITY PRIMARY KEY, name text DEFAULT 'dflt', n int, dbl int GENERATED ALWAYS AS (coalesce(n, 0) * 2) STORED);");
        let (cols, _, _) = t.page("t");
        assert!(cols.iter().find(|c| c.name == "dbl").unwrap().generated && cols.iter().find(|c| c.name == "id").unwrap().generated, "{var}");
        let rows: Vec<NewRow> = vec![vec![("name".into(), Bson::String("x".into()))], vec![], vec![("n".into(), Bson::Null)], vec![("n".into(), Bson::Int64(3)), ("dbl".into(), Bson::Int64(99))]];
        let (res, undo) = t.insert("t", &cols, &["id"], rows, false);
        assert_eq!(res.unwrap(), "Inserted 4 rows", "{var}");
        let got = t.page("t").2;
        assert_eq!(got.len(), 4);
        assert_eq!(got[0].get_str("name").unwrap(), "x");
        assert_eq!(got[1].get_str("name").unwrap(), "dflt");
        assert_eq!(got[2].get("n"), Some(&Bson::Null));
        // The computed column was left out, so the server computed it.
        assert_eq!((got[3].get_i64("n").unwrap(), got[3].get_i64("dbl").unwrap()), (3, 6), "{var}");
        let undo = undo.expect("every key is known from RETURNING");
        let SqlUndo::Insert(keys) = undo_kind(&undo).clone() else { panic!("insert undo") };
        assert_eq!(keys, [doc! { "id": 1_i64 }, doc! { "id": 2_i64 }, doc! { "id": 3_i64 }, doc! { "id": 4_i64 }]);
        let (res, _) = t.undo(undo, &["id"]);
        assert_eq!(res.unwrap(), "Removed the inserted rows");
        assert!(t.page("t").2.is_empty(), "{var}");
        // A key given in the values is returned as it was stored, and a uuid key comes back as text.
        let t = fixture(l, "t5_ins2", "CREATE TABLE t (id uuid PRIMARY KEY DEFAULT gen_random_uuid(), tags text[], ok bool NOT NULL DEFAULT false, ts timestamptz DEFAULT '2024-05-05 05:05:05+00');");
        let (cols, _, _) = t.page("t");
        let rows: Vec<NewRow> = vec![vec![("tags".into(), Bson::String("{a,\"b c\"}".into())), ("ok".into(), Bson::Boolean(true))], vec![]];
        let (res, undo) = t.insert("t", &cols, &["id"], rows, false);
        assert!(res.is_ok(), "{var}: {res:?}");
        let got = t.page("t").2;
        let mut by_tags: Vec<_> = got.iter().collect();
        by_tags.sort_by_key(|r| r.get_bool("ok").unwrap());
        assert_eq!(by_tags[1].get_str("tags").unwrap(), "{a,\"b c\"}");
        assert_eq!(by_tags[0].get_str("ts").unwrap(), "2024-05-05 05:05:05+00");
        let SqlUndo::Insert(keys) = undo_kind(undo.as_ref().unwrap()).clone() else { panic!("insert undo") };
        assert!(keys.iter().all(|k| k.get_str("id").is_ok_and(|s| s.len() == 36)), "{keys:?}");
        assert!(t.undo(undo.unwrap(), &["id"]).0.is_ok());
        assert!(t.page("t").2.is_empty());
    });
}

#[test]
fn deleting_and_undoing_brings_the_rows_back_under_their_keys() {
    every_pg(|l, var| {
        let t = fixture(l, "t5_del", "CREATE TABLE t (id int GENERATED ALWAYS AS IDENTITY PRIMARY KEY, name text, n int, dbl int GENERATED ALWAYS AS (coalesce(n, 0) * 2) STORED); INSERT INTO t (name, n) VALUES ('a', 1), ('b', 2), ('c', 3);");
        let (cols, _, rows) = t.page("t");
        let (res, undo) = t.delete("t", &cols, &["id"], vec![rows[0].clone(), rows[2].clone()], false);
        assert_eq!(res.unwrap(), "Deleted 2 rows", "{var}");
        assert_eq!(t.page("t").2.len(), 1);
        // The identity is ALWAYS, so the old values go back with OVERRIDING SYSTEM VALUE and the computed column is recomputed.
        let (res, _) = t.undo(undo.unwrap(), &["id"]);
        assert_eq!(res.unwrap(), "Restored the deleted rows", "{var}");
        let mut seen: Vec<(i64, String, i64)> = t.page("t").2.iter().map(|r| (r.get_i64("id").unwrap(), r.get_str("name").unwrap().to_string(), r.get_i64("dbl").unwrap())).collect();
        seen.sort();
        assert_eq!(seen, [(1, "a".to_string(), 2), (2, "b".to_string(), 4), (3, "c".to_string(), 6)], "{var}");
        // Someone else deleted a row first: only what this delete removed is brought back.
        t.sql("DELETE FROM t WHERE id = 2");
        let (res, undo) = t.delete("t", &cols, &["id"], rows[..2].to_vec(), false);
        assert_eq!(res.unwrap(), "Deleted 1 row", "{var}");
        let undo = undo.unwrap();
        let SqlUndo::Delete(kept) = undo_kind(&undo) else { panic!("delete undo") };
        assert_eq!(kept.len(), 1);
        // A row with that key appears meanwhile: the undo fails, keeps its entry, and a retry works once the key is free.
        t.sql("INSERT INTO t (id, name) OVERRIDING SYSTEM VALUE VALUES (1, 'clash')");
        let (res, back) = t.undo(undo, &["id"]);
        assert!(res.unwrap_err().contains("duplicate key"), "{var}");
        let back = back.expect("a failed undo keeps its entry");
        t.sql("DELETE FROM t WHERE id = 1");
        assert!(t.undo(back, &["id"]).0.is_ok(), "{var}");
        assert_eq!(t.ids("t"), [1, 3], "{var}");
    });
}

#[test]
fn a_failed_undo_keeps_its_entry() {
    every_pg(|l, var| {
        let t = fixture(l, "t5_undo", "CREATE TABLE t (id int PRIMARY KEY, v int); INSERT INTO t VALUES (1, 1), (2, 2), (3, 3);");
        let (cols, _, rows) = t.page("t");
        let (res, undo) = t.delete("t", &cols, &["id"], vec![rows[0].clone()], false);
        assert!(res.is_ok());
        t.sql("INSERT INTO t VALUES (1, 99)");
        let (res, back) = t.undo(undo.unwrap(), &["id"]);
        assert!(res.unwrap_err().contains("duplicate key"), "{var}");
        let back = back.expect("a failed undo keeps its entry");
        t.sql("DELETE FROM t WHERE id = 1");
        assert!(t.undo(back, &["id"]).0.is_ok());
        assert_eq!(t.ids("t"), [1, 2, 3]);
        // An update undo needs the new value to still be there.
        let ed = RowEdit { pk: pk_doc(&rows[1]), changes: vec![("v".into(), Bson::Int64(2), Bson::Int64(20))] };
        let (_, undo) = t.save("t", &cols, &["id"], vec![ed], false);
        t.sql("UPDATE t SET v = 21 WHERE id = 2");
        let (res, back) = t.undo(undo.unwrap(), &["id"]);
        assert_eq!(res.unwrap_err(), CONFLICT);
        assert!(back.is_some());
        // An undo refuses once the primary key is made of other columns.
        let (res, undo) = t.delete("t", &cols, &["id"], vec![rows[2].clone()], false);
        assert!(res.is_ok());
        t.sql("ALTER TABLE t DROP CONSTRAINT t_pkey; ALTER TABLE t ADD PRIMARY KEY (v)");
        let (res, back) = t.undo(undo.unwrap(), &["id"]);
        assert!(res.unwrap_err().contains("Undo refused"), "{var}");
        assert!(back.is_some());
        assert_eq!(t.one("SELECT count(*)::text FROM t WHERE id = 3"), "0");
    });
}

#[test]
fn composite_keys_and_partial_keys() {
    every_pg(|l, var| {
        let t = fixture(l, "t5_comp", "CREATE TABLE t (a int, b text, c uuid, v int, d numeric(30,10), PRIMARY KEY (a, b, c)); INSERT INTO t VALUES (1, 'x', '00000000-0000-0000-0000-000000000001', 0, 1.1), (1, 'y', '00000000-0000-0000-0000-000000000001', 0, 2.2), (1, 'x', '00000000-0000-0000-0000-000000000002', 0, 3.3);");
        let (cols, pk, rows) = t.page("t");
        assert_eq!(pk.len(), 3, "{var}");
        let keys: Vec<&str> = pk.iter().map(String::as_str).collect();
        // A page has no order, so rows are found by value.
        let pick = |b: &str, c: &str| rows.iter().find(|r| r.get_str("b") == Ok(b) && r.get_str("c").is_ok_and(|u| u.ends_with(c))).unwrap().clone();
        let key = pk_doc(&pick("y", "1"));
        assert_eq!(key.get_str("b").unwrap(), "y");
        let (res, undo) = t.save("t", &cols, &keys, vec![RowEdit { pk: key.clone(), changes: vec![("d".into(), Bson::String("2.2000000000".into()), Bson::String("9.0000000001".into()))] }], false);
        assert!(res.is_ok(), "{var}: {res:?}");
        let d_of = |b: &str, c: &str| t.page("t").2.into_iter().find(|r| r.get_str("b") == Ok(b) && r.get_str("c").is_ok_and(|u| u.ends_with(c))).unwrap().get_str("d").unwrap().to_string();
        assert_eq!(d_of("y", "1"), "9.0000000001");
        assert_eq!(d_of("x", "1"), "1.1000000000");
        assert!(t.undo(undo.unwrap(), &keys).0.is_ok());
        assert_eq!(d_of("y", "1"), "2.2000000000");
        // The key of the third row differs only in its uuid.
        let (res, _) = t.delete("t", &cols, &keys, vec![pick("x", "2")], false);
        assert_eq!(res.unwrap(), "Deleted 1 row");
        assert_eq!(t.page("t").2.len(), 2);
        // A key missing one of the table's key columns is refused and nothing changes.
        let partial = RowEdit { pk: doc! { "a": 1_i64, "b": "x" }, changes: vec![("v".into(), Bson::Int64(0), Bson::Int64(5))] };
        assert!(t.save("t", &cols, &keys, vec![partial], false).0.unwrap_err().contains("primary key"));
        assert!(t.delete("t", &cols, &keys, vec![doc! { "_id": doc! { "a": 1_i64 }, "a": 1_i64 }], false).0.is_err());
        assert_eq!(t.one("SELECT count(*)::text FROM t WHERE v = 0"), "2");
    });
}

#[test]
fn a_locked_connection_refuses_every_write() {
    every_pg(|l, var| {
        let t = fixture(l, "t5_lock", "CREATE TABLE t (id int PRIMARY KEY, v int); INSERT INTO t VALUES (1, 1);");
        let (cols, _, rows) = t.page("t");
        let ed = RowEdit { pk: pk_doc(&rows[0]), changes: vec![("v".into(), Bson::Int64(1), Bson::Int64(2))] };
        let locked = "Writes are locked on this connection. Unlock them first.";
        assert_eq!(t.save("t", &cols, &["id"], vec![ed.clone()], true).0.unwrap_err(), locked, "{var}");
        assert_eq!(t.delete("t", &cols, &["id"], rows.clone(), true).0.unwrap_err(), locked);
        assert_eq!(t.insert("t", &cols, &["id"], vec![vec![("id".into(), Bson::Int64(2))]], true).0.unwrap_err(), locked);
        let (_, undo) = t.save("t", &cols, &["id"], vec![ed], false);
        t.l.w.sql_undo(t.db.clone(), undo.unwrap(), vec!["id".into()], true);
        assert_eq!(t.written().0.unwrap_err(), locked);
        assert_eq!(t.one("SELECT v::text FROM t WHERE id = 1"), "2");
    });
}

#[test]
fn the_writer_writes_while_the_pooled_session_stays_read_only() {
    every_pg(|l, var| {
        let t = fixture(l, "t5_ro", "CREATE TABLE t (id int PRIMARY KEY, v int); INSERT INTO t VALUES (1, 1);");
        let (cols, _, rows) = t.page("t");
        let ed = RowEdit { pk: pk_doc(&rows[0]), changes: vec![("v".into(), Bson::Int64(1), Bson::Int64(2))] };
        assert!(t.save("t", &cols, &["id"], vec![ed], false).0.is_ok(), "{var}");
        assert_eq!(t.one("SELECT v::text FROM t"), "2");
        // The pooled read session that follows still refuses a write, and says it is read-only by default.
        l.w.rt.block_on(async {
            let mut r = t.db.acquire_conn().await.unwrap();
            let err = r.execute("UPDATE t5_ro.t SET v = 3").await.unwrap_err();
            assert_eq!(err.as_database_error().and_then(|d| d.code().map(|c| c.to_string())).as_deref(), Some("25006"), "{var}: {err}");
            let rows = r.fetch_all("SHOW default_transaction_read_only").await.unwrap();
            assert_eq!(rows[0].cell(0), Some(&b"on"[..]));
        });
        assert_eq!(t.one("SELECT v::text FROM t"), "2");
        // The write waits at most the lock timeout for a lock another session holds, then reports it.
        let hold = l.w.rt.block_on(async {
            let o = <sqlx::postgres::PgConnectOptions as std::str::FromStr>::from_str(&l.uri).unwrap().database("quarry_test");
            let mut c = sqlx::PgConnection::connect_with(&o).await.unwrap();
            c.execute("BEGIN; LOCK TABLE t5_ro.t IN ACCESS EXCLUSIVE MODE").await.unwrap();
            c
        });
        let ed = RowEdit { pk: doc! { "id": 1_i64 }, changes: vec![("v".into(), Bson::Int64(2), Bson::Int64(4))] };
        let started = std::time::Instant::now();
        let err = t.save("t", &cols, &["id"], vec![ed], false).0.unwrap_err();
        assert!(err.contains("lock timeout"), "{var}: {err}");
        assert!(started.elapsed() > std::time::Duration::from_secs(8) && started.elapsed() < std::time::Duration::from_secs(20), "{var}: {:?}", started.elapsed());
        assert_eq!(crate::db::explain(&err, &crate::db::ErrCtx::default()).title, "Another session holds a lock on this table");
        drop(hold);
    });
}
