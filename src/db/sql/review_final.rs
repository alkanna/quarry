//! Live tests of the final whole-branch review of v2: sessions that get odd settings from the server's globals (a
//! pool whose every connection runs a SET first) must not lose writes, rows or characters. Needs QUARRY_TEST_MYSQL
//! and QUARRY_TEST_MARIADB; a test skips a server whose variable is unset.

use super::csv::Source;
use super::dump::{dump_to, run_file, DumpPlan, DumpTable};
use super::export::{export_to, Plan, Sink};
use super::import::Progress;
use super::testing::every_server;
use super::{Db, Dialect, Relay, RunHandle};
use crate::db::copy::Fmt;
use crate::db::Msg;
use mongodb::bson::Bson;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use sqlx::Connection;
use std::time::Duration;

fn tmp(var: &str, name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("quarry_ff_{}_{var}_{name}", std::process::id()))
}

fn run(l: &super::testing::Live, db: &Db, schema: &str, text: &str) -> Result<super::dump::Ran, String> {
    let path = tmp(schema, "script.sql");
    std::fs::write(&path, text).unwrap();
    let r = l.w.rt.block_on(run_file(db.clone(), schema.into(), Source::File(path.clone()), false, Arc::new(Progress::default()), Arc::new(AtomicBool::new(false)), Arc::new(AtomicU64::new(0))));
    let _ = std::fs::remove_file(path);
    r
}

/// Finding 1: run_file, the Query and the shell relay each write on a detached connection that never commits. With
/// autocommit 0 from the server they reported the INSERT done and the server rolled it back on close.
#[test]
fn live_writers_commit_on_a_session_without_autocommit() {
    every_server(|l, var| {
        let db = "quarry_ff_ac";
        l.sql(&format!("DROP DATABASE IF EXISTS {db}; CREATE DATABASE {db}; CREATE TABLE {db}.t (id INT PRIMARY KEY)"));
        let odd = l.with_session("SET SESSION autocommit = 0");
        run(l, &odd, db, "INSERT INTO t VALUES (1);\n").unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(l.count(&format!("{db}.t")), 1, "{var}: run_file");
        l.w.sql_run(7, odd.clone(), db.into(), "INSERT INTO t VALUES (2)".into(), false, 100, RunHandle { conn: Arc::new(AtomicU64::new(0)), cancel: Arc::new(AtomicBool::new(false)) });
        let Msg::SqlRan { sets, .. } = l.rx.recv_timeout(Duration::from_secs(30)).unwrap() else { panic!("{var}: SqlRan") };
        assert!(sets.iter().all(|s| s.error.is_none()), "{var}: {:?}", sets.iter().map(|s| &s.error).collect::<Vec<_>>());
        assert_eq!(l.count(&format!("{db}.t")), 2, "{var}: Query");
        l.w.rt.block_on(async {
            let mut r = Relay::open(&odd, db, false, &AtomicU64::new(0)).await.unwrap();
            r.query("INSERT INTO t VALUES (3)", &[]).await.unwrap();
        });
        assert_eq!(l.count(&format!("{db}.t")), 3, "{var}: shell relay");
        l.sql(&format!("DROP DATABASE {db}"));
    });
}

/// Finding 2: the dump, the all-rows export and the structure read inherited the session's character set and
/// sql_select_limit: 4-byte characters came back as '?', latin1 results as other bytes, and only the first row
/// or column was read.
#[test]
fn live_dumps_exports_and_structure_ignore_the_session_charset_and_row_limit() {
    every_server(|l, var| {
        let db = "quarry_ff_cs";
        let rows = [(1, "a\u{1F600}b"), (2, "d\u{e9}"), (3, "x")];
        let hex = |s: &str| s.bytes().map(|b| format!("{b:02X}")).collect::<String>();
        for (i, set) in ["SET NAMES utf8mb3", "SET SESSION sql_select_limit = 1", "SET SESSION character_set_results = latin1"].iter().enumerate() {
            l.sql(&format!("DROP DATABASE IF EXISTS {db}; CREATE DATABASE {db} CHARACTER SET utf8mb4; CREATE TABLE {db}.t (id INT PRIMARY KEY, s VARCHAR(20)) DEFAULT CHARSET=utf8mb4"));
            for (id, s) in rows {
                l.sql(&format!("INSERT INTO {db}.t VALUES ({id}, '{s}')"));
            }
            let odd = l.with_session(set);
            let st = l.w.rt.block_on(super::structure::load(&odd, db, "t")).unwrap();
            assert_eq!(st.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "s"], "{var} {set}: structure");
            let out = l.w.rt.block_on(export_to(&odd, db, "SELECT id, s FROM t ORDER BY id", &Plan { fmt: Fmt::Csv, header: false, table: String::new(), bs: true }, Sink::Clipboard, &AtomicU64::new(0), &AtomicBool::new(false), &AtomicU64::new(0))).unwrap();
            assert_eq!(out.rows, 3, "{var} {set}: export");
            assert_eq!(out.text.unwrap_or_default().trim_end(), "1,a\u{1F600}b\n2,d\u{e9}\n3,x", "{var} {set}: export");
            let path = tmp(var, &format!("cs{i}.sql"));
            let plan = DumpPlan { schema: db.into(), tables: vec![DumpTable { name: "t".into(), view: false, structure: true, content: true, drop: true }], create_db: true, extras: false, rows_per_insert: 1000, snapshot: true, filter: None };
            let done = l.w.rt.block_on(dump_to(&odd, &plan, path.clone(), &AtomicU64::new(0), &AtomicBool::new(false), &AtomicU64::new(0))).unwrap_or_else(|x| panic!("{var} {set}: {x}"));
            assert_eq!(done.rows, 3, "{var} {set}: dump");
            l.sql(&format!("DROP DATABASE {db}"));
            let text = std::fs::read_to_string(&path).unwrap();
            run(l, &odd, "", &text).unwrap_or_else(|x| panic!("{var} {set}: {x}"));
            assert_eq!(l.count(&format!("{db}.t")), 3, "{var} {set}");
            for (id, s) in rows {
                assert_eq!(l.one(&format!("SELECT HEX(s) FROM {db}.t WHERE id = {id}")), hex(s), "{var} {set}: row {id}");
            }
            let _ = std::fs::remove_file(path);
        }
        l.sql(&format!("DROP DATABASE {db}"));
    });
}

/// Finding 4: MariaDB's max_statement_time, set from the app timeout, limits every statement, so it cut ALTER
/// TABLE short. Here the ALTER waits 2.5 s for a metadata lock held by an open transaction, under a 1 s limit.
#[test]
fn live_an_alter_runs_past_the_statement_timeout() {
    every_server(|l, var| {
        let db = "quarry_ff_to";
        l.sql(&format!("DROP DATABASE IF EXISTS {db}; CREATE DATABASE {db}; CREATE TABLE {db}.t (id INT PRIMARY KEY); INSERT INTO {db}.t VALUES (1)"));
        let set = if l.db.dialect == Dialect::MariaDb { "SET SESSION max_statement_time = 1" } else { "SET SESSION max_execution_time = 1000" };
        let odd = l.with_session(set);
        let mut hold = l.w.rt.block_on(sqlx::MySqlConnection::connect_with(&l.db.opts)).unwrap();
        l.w.rt.block_on(async {
            sqlx::Executor::execute(&mut hold, "START TRANSACTION").await.unwrap();
            sqlx::Executor::execute(&mut hold, format!("SELECT * FROM {db}.t").as_str()).await.unwrap();
        });
        let m = super::ddl::Mode::of(&l.db.sql_mode);
        l.w.sql_alter(1, odd, db.into(), "t".into(), "ALTER TABLE `t` ADD COLUMN `z` INT".into(), m, None, false, "done".into());
        std::thread::sleep(Duration::from_millis(2500));
        l.w.rt.block_on(async { sqlx::Executor::execute(&mut hold, "COMMIT").await.unwrap() });
        let Msg::SqlAltered { res, .. } = l.rx.recv_timeout(Duration::from_secs(30)).unwrap() else { panic!("{var}: SqlAltered") };
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(l.one(&format!("SELECT COUNT(*) FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = '{db}' AND TABLE_NAME = 't' AND COLUMN_NAME = 'z'")), "1", "{var}");
        drop(hold);
        l.sql(&format!("DROP DATABASE {db}"));
    });
}

/// Finding 9: doubles are written plain between 1e-6 and 1e15 now; the server must read each back as the same
/// double as its exponent form.
#[test]
fn live_plain_doubles_read_back_as_the_same_double() {
    every_server(|l, var| {
        for f in [26.75, 0.1, 1e-6, 0.30000000000000004, 123456789012345.67, 999999999999999.9, 1.0 / 3.0, -2.5e-6, 1e-7, 1e300] {
            let lit = crate::db::copy::sql_literal(Some(&Bson::Double(f)), true, true);
            assert_eq!(l.one(&format!("SELECT ({lit}) + 0e0 = {f:e}")), "1", "{var}: {lit}");
        }
    });
}

/// Finding 10: the shell relay filled values for the sql_mode read at connect. A session in NO_BACKSLASH_ESCAPES
/// stored a doubled backslash.
#[test]
fn live_the_relay_fills_values_for_its_own_session_mode() {
    every_server(|l, var| {
        let db = "quarry_ff_relay";
        l.sql(&format!("DROP DATABASE IF EXISTS {db}; CREATE DATABASE {db}; CREATE TABLE {db}.t (id INT PRIMARY KEY, s VARCHAR(40))"));
        let nb = l.with_session("SET SESSION sql_mode = CONCAT(@@SESSION.sql_mode, ',NO_BACKSLASH_ESCAPES')");
        assert!(l.db.backslash_escapes(), "{var}: the connect-time mode reads backslashes");
        let value = r"C:\temp\'x";
        l.w.rt.block_on(async {
            let mut r = Relay::open(&nb, db, false, &AtomicU64::new(0)).await.unwrap();
            r.query("INSERT INTO t VALUES (1, ?)", &[super::value::Param::Text(value.into())]).await.unwrap();
        });
        assert_eq!(l.one(&format!("SELECT s FROM {db}.t WHERE id = 1")), value, "{var}");
        l.sql(&format!("DROP DATABASE {db}"));
    });
}
