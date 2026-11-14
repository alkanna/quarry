//! Live tests of functions and procedures on MySQL and MariaDB (QUARRY_TEST_MYSQL, QUARRY_TEST_MARIADB) and PostgreSQL 18 and 13
//! (QUARRY_TEST_POSTGRES, QUARRY_TEST_POSTGRES13): the list, a routine's definition, create, edit, a refused edit and its
//! restore, drop, and the scripts that run one. Each test works in a database or schema of its own.

use crate::db::sql::routines::*;
use crate::db::sql::testing::{every_pg, every_server, unique_name, Live, LivePg};
use crate::db::sql::{Db, Dialect};
use crate::db::Msg;
use std::sync::mpsc::Receiver;

fn answer(rx: &Receiver<Msg>) -> Answer {
    loop {
        if let Msg::SqlRoutine { ans, .. } = rx.recv_timeout(std::time::Duration::from_secs(40)).expect("worker answer") {
            return ans;
        }
    }
}

fn list_of(w: &crate::db::Worker, rx: &Receiver<Msg>, db: &Db, database: &str, schema: &str) -> Vec<Routine> {
    w.sql_routines(1, db.clone(), database.into(), schema.into());
    let Answer::List { res, .. } = answer(rx) else { panic!("not a list") };
    res.unwrap()
}

fn read(w: &crate::db::Worker, rx: &Receiver<Msg>, db: &Db, schema: &str, r: &Routine) -> Detail {
    w.sql_routine_detail(1, 1, db.clone(), schema.into(), r.clone());
    let Answer::Detail { res } = answer(rx) else { panic!("not a detail") };
    *res.unwrap()
}

fn save(w: &crate::db::Worker, rx: &Receiver<Msg>, db: &Db, schema: &str, old: Option<&Detail>, text: &str, recreate: bool, read_only: bool) -> Result<Option<Routine>, Failure> {
    w.sql_routine_save(1, 1, db.clone(), schema.into(), old.cloned(), text.into(), recreate, read_only);
    let Answer::Changed { res } = answer(rx) else { panic!("not a change") };
    res
}

fn drop_it(w: &crate::db::Worker, rx: &Receiver<Msg>, db: &Db, schema: &str, r: &Routine, read_only: bool) -> Result<Option<Routine>, Failure> {
    w.sql_routine_drop(1, 1, db.clone(), schema.into(), r.clone(), read_only);
    let Answer::Changed { res } = answer(rx) else { panic!("not a change") };
    res
}

fn find<'a>(l: &'a [Routine], kind: Kind, name: &str) -> Option<&'a Routine> {
    l.iter().find(|r| r.kind == kind && r.name == name)
}

#[test]
fn live_mysql_routines_list_view_create_edit_restore_and_drop() {
    every_server(|l: &Live, var| {
        let db = "quarry_rt_my";
        let d = l.db.dialect;
        l.sql(&format!("DROP DATABASE IF EXISTS {db}"));
        l.sql(&format!("CREATE DATABASE {db} CHARACTER SET utf8mb4"));
        l.sql(&format!("CREATE FUNCTION {db}.f_add(a INT) RETURNS INT DETERMINISTIC RETURN a + 1"));
        l.sql(&format!("CREATE PROCEDURE {db}.p_inc(IN a INT, OUT b INT) BEGIN SET b = a + 1; END"));
        // A function and a procedure may share a name, so the list keys them by kind as well.
        l.sql(&format!("CREATE FUNCTION {db}.same(a INT) RETURNS INT DETERMINISTIC RETURN a"));
        l.sql(&format!("CREATE PROCEDURE {db}.same(IN a INT) SELECT a"));

        let all = list_of(&l.w, &l.rx, &l.db, db, db);
        let f = find(&all, Kind::Function, "f_add").unwrap_or_else(|| panic!("{var}: f_add missing in {all:?}")).clone();
        assert!(find(&all, Kind::Procedure, "p_inc").is_some(), "{var}");
        let (sf, sp) = (find(&all, Kind::Function, "same").unwrap(), find(&all, Kind::Procedure, "same").unwrap());
        assert_ne!(sf.key(), sp.key(), "{var}");

        let det = read(&l.w, &l.rx, &l.db, db, &f);
        assert!(det.header_missing.is_none(), "{var}: {:?}", det.header_missing);
        assert!(det.source.to_ascii_uppercase().starts_with("CREATE"), "{var}: {}", det.source);
        assert_eq!(det.behavior, "DETERMINISTIC", "{var}");
        assert_eq!(det.params.len(), 1, "{var}");
        let p_inc = find(&all, Kind::Procedure, "p_inc").unwrap();
        let pd = read(&l.w, &l.rx, &l.db, db, p_inc);
        assert_eq!(pd.params.iter().map(|p| p.mode).collect::<Vec<_>>(), [ArgMode::In, ArgMode::Out], "{var}");

        // The scripts the Run dialog writes are valid on this server.
        let script = run_script(d, db, &det, &[Arg { value: "1".into(), null: false }], l.db.backslash_escapes()).unwrap();
        assert_eq!(l.one(&script), "2", "{var}: {script}");
        let call = run_script(d, db, &pd, &[Arg { value: "4".into(), null: false }], true).unwrap();
        l.sql(&call);

        // An edit saves, and the new body runs.
        let text = det.source.replace("a + 1", "a + 2");
        let saved = save(&l.w, &l.rx, &l.db, db, Some(&det), &text, false, false).unwrap_or_else(|e| panic!("{var}: {}", e.why));
        assert_eq!(saved.map(|r| r.name), Some("f_add".to_string()), "{var}");
        assert_eq!(l.one(&format!("SELECT {db}.f_add(1)")), "3", "{var}");

        // A refused CREATE leaves the old definition in place: after the DROP on MySQL, atomically on MariaDB.
        let det = read(&l.w, &l.rx, &l.db, db, &f);
        let broken = det.source.replace("a + 2", "a +");
        let err = save(&l.w, &l.rx, &l.db, db, Some(&det), &broken, false, false).err().unwrap_or_else(|| panic!("{var}: a broken body must be refused"));
        assert!(!err.why.is_empty(), "{var}");
        assert_eq!(l.one(&format!("SELECT {db}.f_add(1)")), "3", "{var}: the old definition is back: {}", err.why);
        assert!(find(&list_of(&l.w, &l.rx, &l.db, db, db), Kind::Function, "f_add").is_some(), "{var}");

        // A changed name is another routine, which is not an edit.
        let renamed = det.source.replace("f_add", "f_other");
        assert!(save(&l.w, &l.rx, &l.db, db, Some(&det), &renamed, false, false).is_err(), "{var}");

        // A new routine, and no second one under the same name.
        let new = template(d, Kind::Function, db);
        let made = save(&l.w, &l.rx, &l.db, db, None, &new, false, false).unwrap_or_else(|e| panic!("{var}: {}", e.why));
        assert_eq!(made.map(|r| r.name), Some("new_function".to_string()), "{var}");
        let again = save(&l.w, &l.rx, &l.db, db, None, &new, false, false).err().unwrap_or_else(|| panic!("{var}: a second create must be refused"));
        assert!(again.why.contains("already exists"), "{var}: {}", again.why);

        // A locked connection writes nothing.
        let locked = save(&l.w, &l.rx, &l.db, db, Some(&det), &text, false, true).err().unwrap_or_else(|| panic!("{var}: locked"));
        assert!(locked.why.contains("locked"), "{var}: {}", locked.why);
        assert!(drop_it(&l.w, &l.rx, &l.db, db, &f, true).is_err(), "{var}");

        // Drop.
        drop_it(&l.w, &l.rx, &l.db, db, &f, false).unwrap_or_else(|e| panic!("{var}: {}", e.why));
        assert!(find(&list_of(&l.w, &l.rx, &l.db, db, db), Kind::Function, "f_add").is_none(), "{var}");
        l.sql(&format!("DROP DATABASE {db}"));
    });
}

fn pg_text(l: &LivePg, pool: &sqlx::PgPool, sql: &str) -> String {
    l.w.rt.block_on(async {
        let rows = sqlx::raw_sql(sql).fetch_all(pool).await.unwrap_or_else(|x| panic!("{sql}: {x}"));
        crate::db::sql::worker::text_at(&rows[0], 0).unwrap_or_default()
    })
}

#[test]
fn live_postgres_routines_list_view_create_edit_recreate_and_drop() {
    every_pg(|l: &LivePg, var| {
        let db = l.quarry_test();
        let pool = l.fixture_pool();
        let s = unique_name("qr_rt");
        l.qsql(&pool, &format!(
            "CREATE SCHEMA {s}; \
             CREATE FUNCTION {s}.add(a integer, b integer) RETURNS integer LANGUAGE sql IMMUTABLE AS $$ SELECT a + b $$; \
             CREATE FUNCTION {s}.add(a text, b text) RETURNS text LANGUAGE sql AS $$ SELECT a || b $$; \
             CREATE PROCEDURE {s}.hello(IN a integer) LANGUAGE plpgsql AS $$ BEGIN NULL; END $$; \
             CREATE AGGREGATE {s}.my_sum(integer) (sfunc = int4pl, stype = integer, initcond = 0);"
        ));
        let all = list_of(&l.w, &l.rx, &db, "quarry_test", &s);
        let ints = all.iter().find(|r| r.name == "add" && r.args.contains("integer")).unwrap_or_else(|| panic!("{var}: {all:?}")).clone();
        let texts = all.iter().find(|r| r.name == "add" && r.args.contains("text")).unwrap().clone();
        assert_ne!(ints.key(), texts.key(), "{var}: overloads are keyed by signature");
        assert_eq!(all.len(), 3, "{var}: the aggregate is left out: {all:?}");
        assert!(find(&all, Kind::Procedure, "hello").is_some(), "{var}");

        let det = read(&l.w, &l.rx, &db, &s, &ints);
        assert!(det.read_only && det.behavior == "IMMUTABLE", "{var}");
        assert_eq!(det.params.len(), 2, "{var}");
        assert!(det.source.starts_with("CREATE OR REPLACE FUNCTION"), "{var}: {}", det.source);
        let hello = read(&l.w, &l.rx, &db, &s, find(&all, Kind::Procedure, "hello").unwrap());
        assert!(!hello.read_only, "{var}: a procedure counts as a write");

        // The scripts the Run dialog writes are valid.
        let script = run_script(Dialect::Postgres, &s, &det, &[Arg { value: "1".into(), null: false }, Arg { value: "2".into(), null: false }], true).unwrap();
        assert_eq!(pg_text(l, &pool, &script), "3", "{var}: {script}");
        let call = run_script(Dialect::Postgres, &s, &hello, &[Arg { value: "1".into(), null: false }], true).unwrap();
        l.qsql(&pool, &call);

        // A replace keeps the oid.
        let text = det.source.replace("SELECT a + b", "SELECT a + b + 1");
        let saved = save(&l.w, &l.rx, &db, &s, Some(&det), &text, false, false).unwrap_or_else(|e| panic!("{var}: {}", e.why)).unwrap();
        assert_eq!(saved.key(), ints.key(), "{var}");
        assert_eq!(pg_text(l, &pool, &format!("SELECT {s}.add(1, 2)::text")), "4", "{var}");
        assert_eq!(pg_text(l, &pool, &format!("SELECT oid::text FROM pg_proc WHERE oid = '{s}.add(integer, integer)'::regprocedure")), det.oid, "{var}");

        // A changed return type is refused by the server, and the answer offers the drop and create.
        let det = read(&l.w, &l.rx, &db, &s, &ints);
        let wider = det.source.replace("RETURNS integer", "RETURNS bigint");
        let refused = save(&l.w, &l.rx, &db, &s, Some(&det), &wider, false, false).err().unwrap_or_else(|| panic!("{var}: must be refused"));
        assert!(refused.recreate, "{var}: 42P13 must offer a drop and create: {}", refused.why);
        assert_eq!(pg_text(l, &pool, &format!("SELECT pg_get_function_result('{s}.add(integer, integer)'::regprocedure)")), "integer", "{var}: nothing changed");
        let again = save(&l.w, &l.rx, &db, &s, Some(&det), &wider, true, false).unwrap_or_else(|e| panic!("{var}: {}", e.why));
        assert!(again.is_some(), "{var}");
        assert_eq!(pg_text(l, &pool, &format!("SELECT pg_get_function_result('{s}.add(integer, integer)'::regprocedure)")), "bigint", "{var}");

        // A changed argument list is not a replace: it would add a second overload, so it is refused and rolled back.
        let det = read(&l.w, &l.rx, &db, &s, &texts);
        let more = det.source.replace("a text, b text", "a text, b text, c text");
        let refused = save(&l.w, &l.rx, &db, &s, Some(&det), &more, false, false).err().unwrap_or_else(|| panic!("{var}: must be refused"));
        assert!(refused.recreate, "{var}: {}", refused.why);
        assert_eq!(list_of(&l.w, &l.rx, &db, "quarry_test", &s).len(), 3, "{var}: rolled back");

        // Another session changed it since it was read.
        l.qsql(&pool, &format!("COMMENT ON FUNCTION {s}.add(text, text) IS 'changed elsewhere'"));
        let stale = save(&l.w, &l.rx, &db, &s, Some(&det), &det.source, false, false).err().unwrap_or_else(|| panic!("{var}: stale"));
        assert!(stale.why.contains("changed by someone else"), "{var}: {}", stale.why);

        // A new function, and no silent replace of an existing one.
        let new = template(Dialect::Postgres, Kind::Function, &s);
        let made = save(&l.w, &l.rx, &db, &s, None, &new, false, false).unwrap_or_else(|e| panic!("{var}: {}", e.why));
        assert_eq!(made.map(|r| r.name), Some("new_function".to_string()), "{var}");
        let replace_typed = new.replace("CREATE FUNCTION", "CREATE OR REPLACE FUNCTION");
        let noop = save(&l.w, &l.rx, &db, &s, None, &replace_typed, false, false).err().unwrap_or_else(|| panic!("{var}: an existing routine is not replaced by a create"));
        assert!(noop.why.contains("Nothing new was created"), "{var}: {}", noop.why);

        // A locked connection writes nothing.
        let locked = save(&l.w, &l.rx, &db, &s, None, &new, false, true).err().unwrap_or_else(|| panic!("{var}: locked"));
        assert!(locked.why.contains("locked"), "{var}: {}", locked.why);

        // A routine something depends on is not dropped, and not taken along.
        l.qsql(&pool, &format!("CREATE VIEW {s}.v AS SELECT {s}.add(1::bigint, 2) AS n"));
        let all = list_of(&l.w, &l.rx, &db, "quarry_test", &s);
        let big = all.iter().find(|r| r.name == "add" && r.args.contains("bigint")).unwrap().clone();
        assert!(drop_it(&l.w, &l.rx, &db, &s, &big, false).is_err(), "{var}: the view needs it");
        assert!(list_of(&l.w, &l.rx, &db, "quarry_test", &s).iter().any(|r| r.key() == big.key()), "{var}");
        l.qsql(&pool, &format!("DROP VIEW {s}.v"));
        drop_it(&l.w, &l.rx, &db, &s, &big, false).unwrap_or_else(|e| panic!("{var}: {}", e.why));
        assert!(!list_of(&l.w, &l.rx, &db, "quarry_test", &s).iter().any(|r| r.key() == big.key()), "{var}");
        l.qsql(&pool, &format!("DROP SCHEMA {s} CASCADE"));
    });
}
