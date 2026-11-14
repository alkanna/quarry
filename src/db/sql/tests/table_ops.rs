//! Live tests of renaming and duplicating tables, renaming a PostgreSQL schema, and emptying a PostgreSQL table. They run on every
//! server named by the QUARRY_TEST_* variables and skip the ones that are unset.

use crate::db::sql::testing::{every_pg, every_server, unique_name, PgFixture};
use crate::db::{Msg, Res, UndoAction};
use std::time::Duration;

fn ddl_answer(m: Msg) -> (Res<String>, Option<UndoAction>) {
    let Msg::SqlDdl { res, undo, .. } = m else { panic!("expected SqlDdl") };
    (res, undo)
}

fn renamed_answer(m: Msg) -> (Res<String>, Option<UndoAction>) {
    let Msg::SqlRenamed { res, undo, .. } = m else { panic!("expected SqlRenamed") };
    (res, undo)
}

#[test]
fn live_mysql_a_table_is_renamed_and_copied_with_its_generated_columns() {
    every_server(|l, var| {
        let next = || l.rx.recv_timeout(Duration::from_secs(20)).expect("worker answer");
        let s = unique_name("qto");
        l.sql(&format!("CREATE DATABASE `{s}`"));
        l.sql(&format!("CREATE TABLE `{s}`.src (id INT AUTO_INCREMENT PRIMARY KEY, n INT, d INT GENERATED ALWAYS AS (n * 2) STORED, v INT GENERATED ALWAYS AS (n + 1) VIRTUAL)"));
        l.sql(&format!("INSERT INTO `{s}`.src (n) VALUES (1), (2), (3)"));
        // With rows: the generated columns are left out of the INSERT, and there is no undo because the copy is never empty.
        l.w.sql_duplicate(1, l.db.clone(), s.clone(), "src".into(), "cp".into(), true, false);
        let (res, undo) = ddl_answer(next());
        assert!(res.is_ok() && undo.is_none(), "{var}: {res:?}");
        assert_eq!(l.count(&format!("`{s}`.cp")), 3, "{var}");
        assert_eq!(l.one(&format!("SELECT SUM(d) FROM `{s}`.cp")), "12", "{var}");
        // Structure only: no rows, and an undo that drops it again.
        l.w.sql_duplicate(1, l.db.clone(), s.clone(), "src".into(), "cp2".into(), false, false);
        let (res, undo) = ddl_answer(next());
        assert!(res.is_ok() && undo.is_some(), "{var}: {res:?}");
        assert_eq!(l.count(&format!("`{s}`.cp2")), 0, "{var}");
        // A taken name fails and leaves the other table alone.
        l.w.sql_duplicate(1, l.db.clone(), s.clone(), "src".into(), "cp".into(), true, false);
        assert!(ddl_answer(next()).0.is_err(), "{var}");
        // Rename, then undo it.
        l.w.sql_rename(1, l.db.clone(), s.clone(), "cp2".into(), "cp3".into(), false, false);
        let (res, undo) = renamed_answer(next());
        assert!(res.is_ok(), "{var}: {res:?}");
        assert_eq!(l.one(&format!("SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = '{s}' AND TABLE_NAME = 'cp3'")), "1", "{var}");
        l.w.sql_undo(l.db.clone(), undo.expect("a rename has an undo"), Vec::new(), false);
        let (res, again) = renamed_answer(next());
        assert!(res.is_ok() && again.is_none(), "{var}: {res:?}");
        assert_eq!(l.one(&format!("SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = '{s}' AND TABLE_NAME = 'cp2'")), "1", "{var}");
        // A locked connection refuses.
        l.w.sql_rename(1, l.db.clone(), s.clone(), "cp".into(), "cp9".into(), false, true);
        assert!(renamed_answer(next()).0.is_err(), "{var}");
        l.sql(&format!("DROP DATABASE `{s}`"));
    });
}

const PG_DDL: &str = "CREATE TABLE parent (id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY, n integer NOT NULL, d integer GENERATED ALWAYS AS (n * 2) STORED); \
                      CREATE TABLE child (id serial PRIMARY KEY, p integer REFERENCES parent (id)); \
                      INSERT INTO parent (n) VALUES (1), (2), (3); INSERT INTO child (p) VALUES (1), (2); \
                      CREATE VIEW v AS SELECT id, n FROM parent";

#[test]
fn live_pg_a_table_is_copied_renamed_and_emptied() {
    every_pg(|l, var| {
        let t = PgFixture::new(l, &unique_name(&format!("qto_{}", if var.ends_with("13") { "13" } else { "18" })), PG_DDL);
        let (db, s) = (t.db.clone(), t.schema.clone());
        // With rows: generated columns are left out, an ALWAYS identity takes the copied values, and the new sequence moves on.
        l.w.sql_duplicate(1, db.clone(), s.clone(), "parent".into(), "parent2".into(), true, false);
        let (res, undo) = ddl_answer(l.next());
        assert!(res.is_ok() && undo.is_none(), "{var}: {res:?}");
        assert_eq!(t.one("SELECT sum(d) FROM parent2"), "12", "{var}");
        t.sql("INSERT INTO parent2 (n) VALUES (9)");
        assert_eq!(t.one("SELECT count(DISTINCT id) FROM parent2"), "4", "{var}: the copy's identity restarted at 1");
        // Structure only: no foreign key comes along, and the undo drops it.
        l.w.sql_duplicate(1, db.clone(), s.clone(), "child".into(), "child2".into(), false, false);
        let (res, undo) = ddl_answer(l.next());
        assert!(res.is_ok() && undo.is_some(), "{var}: {res:?}");
        assert_eq!(t.one("SELECT count(*) FROM pg_constraint WHERE conrelid = 'child2'::regclass AND contype = 'f'"), "0", "{var}");
        assert_eq!(t.one("SELECT count(*) FROM pg_constraint WHERE conrelid = 'child2'::regclass AND contype = 'p'"), "1", "{var}");
        l.w.sql_undo(db.clone(), undo.unwrap(), Vec::new(), false);
        assert!(ddl_answer(l.next()).0.is_ok(), "{var}");
        assert_eq!(t.one("SELECT to_regclass('child2') IS NULL"), "true", "{var}");
        // A view is not copied.
        l.w.sql_duplicate(1, db.clone(), s.clone(), "v".into(), "v_copy".into(), false, false);
        assert!(ddl_answer(l.next()).0.is_err(), "{var}");
        // Rename a table and a view, and undo one.
        l.w.sql_rename(1, db.clone(), s.clone(), "parent2".into(), "parent3".into(), false, false);
        let (res, undo) = renamed_answer(l.next());
        assert!(res.is_ok(), "{var}: {res:?}");
        assert_eq!(t.one("SELECT to_regclass('parent3') IS NOT NULL"), "true", "{var}");
        l.w.sql_undo(db.clone(), undo.expect("a rename has an undo"), Vec::new(), false);
        assert!(renamed_answer(l.next()).0.is_ok(), "{var}");
        assert_eq!(t.one("SELECT to_regclass('parent2') IS NOT NULL"), "true", "{var}");
        l.w.sql_rename(1, db.clone(), s.clone(), "v".into(), "v2".into(), false, false);
        assert!(renamed_answer(l.next()).0.is_ok(), "{var}");
        assert_eq!(t.one("SELECT relkind::text FROM pg_class WHERE oid = to_regclass('v2')"), "v", "{var}");
        // The schema itself, and back.
        let moved = format!("{s}_r");
        l.w.sql_rename(1, db.clone(), s.clone(), s.clone(), moved.clone(), true, false);
        let (res, undo) = renamed_answer(l.next());
        assert!(res.is_ok(), "{var}: {res:?}");
        assert_eq!(l.one(&format!("SELECT count(*) FROM pg_namespace WHERE nspname = '{moved}'")), "1", "{var}");
        l.w.sql_undo(db.clone(), undo.expect("a schema rename has an undo"), Vec::new(), false);
        assert!(renamed_answer(l.next()).0.is_ok(), "{var}");
        // A table another one points at refuses to be emptied alone, and the dialog is told which one points at it.
        l.w.pg_truncate_info(1, db.clone(), db.pg_database(), s.clone(), "parent".into());
        let Msg::SqlDropInfo { res, .. } = l.next() else { panic!("expected SqlDropInfo") };
        let info = res.unwrap();
        assert!(info.exists && info.rows == Some(3) && info.referenced_by.len() == 1 && info.referenced_by[0].contains("child"), "{var}: {info:?}");
        l.w.pg_truncate(1, db.clone(), s.clone(), "parent".into(), false, false, false);
        let (res, _) = ddl_answer(l.next());
        assert!(res.as_ref().is_err_and(|e| e.contains("Tick CASCADE") && e.contains("Nothing was emptied")), "{var}: {res:?}");
        assert_eq!(t.one("SELECT count(*) FROM parent"), "3", "{var}");
        l.w.pg_truncate(1, db.clone(), s.clone(), "parent".into(), true, true, true);
        assert!(ddl_answer(l.next()).0.is_err(), "{var}: a locked connection must refuse");
        l.w.pg_truncate(1, db.clone(), s.clone(), "parent".into(), true, true, false);
        assert!(ddl_answer(l.next()).0.is_ok(), "{var}");
        assert_eq!(t.one("SELECT (SELECT count(*) FROM parent) + (SELECT count(*) FROM child)"), "0", "{var}");
        t.sql("INSERT INTO parent (n) VALUES (1)");
        assert_eq!(t.one("SELECT id FROM parent"), "1", "{var}: RESTART IDENTITY");
    });
}
