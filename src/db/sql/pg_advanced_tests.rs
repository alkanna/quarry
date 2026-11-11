//! Live tests of the PostgreSQL Advanced tab: every setting applied on a direct connection (QUARRY_TEST_POSTGRES and
//! QUARRY_TEST_POSTGRES13) and behind a real PgBouncer in transaction mode (QUARRY_TEST_PGBOUNCER), read back with SHOW in
//! Quarry's own session and in a Query run, the search_path order, refusals on Test and on connect, the read-only guard
//! with every setting applied, and that nothing reaches another client of the pooler.

use super::advanced::{Advanced, Zone};
use super::exec::Cells;
use super::run::RunHandle;
use super::testing::{LivePg, PGBOUNCER_ENV, PG_ENVS};
use super::{Db, ResultSet};
use crate::db::{Msg, Net};
use mongodb::bson::Bson;
use sqlx::{Connection, Executor};
use std::time::Duration;

/// (name, value as typed, what SHOW prints), covering every group of the allow-list.
const ALL: &[(&str, &str, &str)] = &[
    ("statement_timeout", "45s", "45s"),
    ("lock_timeout", "7s", "7s"),
    ("application_name", "quarry adv", "quarry adv"),
    ("random_page_cost", "1.5", "1.5"),
    ("seq_page_cost", "1.2", "1.2"),
    ("cpu_tuple_cost", "0.02", "0.02"),
    ("effective_cache_size", "2GB", "2GB"),
    ("enable_nestloop", "off", "off"),
    ("jit", "off", "off"),
    ("jit_above_cost", "200000", "200000"),
    ("max_parallel_workers_per_gather", "0", "0"),
    ("work_mem", "7MB", "7MB"),
    ("maintenance_work_mem", "80MB", "80MB"),
    ("hash_mem_multiplier", "3", "3"),
    ("lc_messages", "C", "C"),
    ("lc_numeric", "C", "C"),
    ("lc_time", "C", "C"),
    ("default_text_search_config", "pg_catalog.simple", "pg_catalog.simple"),
    ("idle_in_transaction_session_timeout", "3min", "3min"),
    ("log_min_duration_statement", "250ms", "250ms"),
    ("temp_file_limit", "1GB", "1GB"),
];

fn all_settings() -> Advanced {
    Advanced { zone: Zone::Custom("Europe/Paris".into()), pg_settings: ALL.iter().map(|(n, v, _)| (n.to_string(), v.to_string())).collect(), ..Default::default() }
}

/// Every server with its URI: a direct connection on each PostgreSQL, and the PgBouncer.
fn each(f: impl Fn(&str, &str, bool)) {
    for var in PG_ENVS {
        if let Ok(uri) = std::env::var(var) {
            f(var, &uri, false);
        }
    }
    if let Ok(uri) = std::env::var(PGBOUNCER_ENV) {
        f(PGBOUNCER_ENV, &uri, true);
    }
}

/// A connection made the way the app makes one from a saved connection with these settings.
fn open(uri: &str, adv: &Advanced, pooled: bool) -> LivePg {
    let (tx, rx) = std::sync::mpsc::channel();
    let w = crate::db::Worker::new(tx, eframe::egui::Context::default());
    let pool = w.rt.block_on(async { sqlx::PgPool::connect(uri).await.unwrap() });
    w.sql_connect_with(1, uri.to_string(), Duration::from_secs(30), Net::default(), adv.clone());
    let Msg::SqlConnected { res, .. } = rx.recv_timeout(Duration::from_secs(30)).unwrap() else { panic!("connect") };
    let (db, dbs, _) = res.unwrap_or_else(|x| panic!("connect: {x}"));
    assert_eq!(db.pooler(), pooled, "{uri}");
    LivePg { w, rx, db, dbs, pool, uri: uri.to_string() }
}

/// What the connect or the Test of these settings answered.
fn connect_error(uri: &str, adv: &Advanced) -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    let w = crate::db::Worker::new(tx, eframe::egui::Context::default());
    w.sql_connect_with(1, uri.to_string(), Duration::from_secs(30), Net::default(), adv.clone());
    match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
        Msg::SqlConnected { res: Err(x), .. } => x,
        Msg::SqlConnected { .. } => panic!("the connection should have been refused"),
        _ => panic!("connect"),
    }
}

fn test_answer(uri: &str, adv: &Advanced) -> (Result<String, String>, super::TestInfo) {
    let (tx, rx) = std::sync::mpsc::channel();
    let w = crate::db::Worker::new(tx, eframe::egui::Context::default());
    w.sql_test_with(5, uri.to_string(), Duration::from_secs(30), Net::default(), adv.clone());
    let Msg::Tested { res, info, .. } = rx.recv_timeout(Duration::from_secs(40)).unwrap() else { panic!("tested") };
    (res, info.expect("the Test always reports what it learned"))
}

/// A setting as Quarry's own read path sees it: a read in the session or, behind a pooler, in a transaction with the locals.
fn show(l: &LivePg, db: &Db, name: &str) -> String {
    let rows = l.w.rt.block_on(db.read_text("", &format!("SELECT current_setting('{name}')"), false)).unwrap_or_else(|x| panic!("{name}: {x}"));
    super::worker::text_at(&rows[0], 0).unwrap_or_default()
}

fn go(l: &LivePg, db: &Db, schema: &str, text: &str, read_only: bool) -> Vec<ResultSet> {
    l.w.sql_run(9, db.clone(), schema.into(), text.into(), read_only, 0, RunHandle::default());
    loop {
        if let Msg::SqlRan { sets, .. } = l.next() {
            return sets;
        }
    }
}

fn val(set: &ResultSet, name: &str) -> String {
    match set.rows[0].decode().unwrap().get(name).unwrap_or_else(|| panic!("no column {name}")) {
        Bson::String(s) => s.clone(),
        b => b.to_string(),
    }
}

/// The settings of a plain connection to the same server, which is what any other client of it would find.
fn fresh(l: &LivePg, names: &[&str]) -> Vec<String> {
    l.w.rt.block_on(async {
        use std::str::FromStr;
        let mut c = sqlx::PgConnection::connect_with(&sqlx::postgres::PgConnectOptions::from_str(&l.uri).unwrap()).await.unwrap();
        let mut out = Vec::new();
        for n in names {
            let rows = c.fetch_all(format!("SHOW {n}").as_str()).await.unwrap();
            out.push(String::from_utf8_lossy(rows[0].cell(0).unwrap()).into_owned());
        }
        let _ = c.close().await;
        out
    })
}

#[test]
fn every_setting_is_applied_on_a_direct_connection_and_behind_pgbouncer() {
    each(|var, uri, pooled| {
        let l = open(uri, &all_settings(), pooled);
        let db = l.quarry_test();
        for (name, _, shown) in ALL {
            assert_eq!(&show(&l, &db, name), shown, "{var}: {name} in Quarry's own session");
        }
        assert_eq!(show(&l, &db, "TimeZone"), "Europe/Paris", "{var}");
        // A Query run sees them too: it runs on a connection of its own.
        let sets = go(&l, &db, "public", "SELECT current_setting('work_mem') AS w, current_setting('TimeZone') AS z, current_setting('statement_timeout') AS s, current_setting('application_name') AS a, current_setting('random_page_cost') AS r", true);
        assert!(sets[0].error.is_none(), "{var}: {:?}", sets[0].error);
        let got: Vec<String> = ["w", "z", "s", "a", "r"].iter().map(|c| val(&sets[0], c)).collect();
        assert_eq!(got, ["7MB", "Europe/Paris", "45s", "quarry adv", "1.5"], "{var}");
        // The same statement as a read of the grid does it: the timestamptz of a Content page is shown in that zone.
        let rows = go(&l, &db, "public", "SELECT '2024-06-01 12:00:00+00'::timestamptz::text AS t", true);
        assert_eq!(val(&rows[0], "t"), "2024-06-01 14:00:00+02", "{var}");
    });
}

#[test]
fn a_default_connection_sends_nothing() {
    each(|var, uri, pooled| {
        let l = open(uri, &Advanced::default(), pooled);
        let db = l.quarry_test();
        let names = ["work_mem", "search_path", "application_name", "lock_timeout", "extra_float_digits"];
        let plain = fresh(&l, &names);
        for (n, want) in names.iter().zip(&plain) {
            // sqlx sends extra_float_digits 2 and Quarry names itself, so only the settings the tab owns are compared.
            if ["work_mem", "search_path", "lock_timeout"].contains(n) {
                assert_eq!(&show(&l, &db, n), want, "{var}: {n}");
            }
        }
        assert_eq!(show(&l, &db, "application_name"), "Quarry", "{var}");
    });
}

#[test]
fn the_sidebar_schema_comes_first_and_the_saved_list_follows() {
    each(|var, uri, pooled| {
        let adv = Advanced { search_path: vec!["adv_p1".into(), "adv_p2".into()], ..Default::default() };
        let l = open(uri, &adv, pooled);
        let db = l.quarry_test();
        let pool = l.fixture_pool();
        l.reset_schemas(&pool, &["adv_s1", "adv_p1", "adv_p2"], "CREATE TABLE adv_s1.t_who (v int); INSERT INTO adv_s1.t_who VALUES (1); CREATE TABLE adv_p1.t_who (v int); INSERT INTO adv_p1.t_who VALUES (2); CREATE TABLE adv_p2.t_only (v int); INSERT INTO adv_p2.t_only VALUES (3)");
        assert_eq!(db.pg_path("adv_s1"), "\"adv_s1\", \"adv_p1\", \"adv_p2\"", "{var}");
        // The session default is the saved list alone.
        assert_eq!(show(&l, &db, "search_path").replace('"', ""), "adv_p1, adv_p2", "{var}");
        let sets = go(&l, &db, "adv_s1", "SELECT (SELECT v FROM t_who) AS who, (SELECT v FROM t_only) AS only, current_setting('search_path') AS sp", true);
        assert!(sets[0].error.is_none(), "{var}: {:?}", sets[0].error);
        assert_eq!((val(&sets[0], "who"), val(&sets[0], "only")), ("1".into(), "3".into()), "{var}: the sidebar schema wins, the list is searched after it");
        assert_eq!(val(&sets[0], "sp").replace('"', ""), "adv_s1, adv_p1, adv_p2", "{var}");
        // Another tab schema: the saved list still follows, and a schema that is also in the list is not repeated.
        let sets = go(&l, &db, "adv_p2", "SELECT (SELECT v FROM t_who) AS who, current_setting('search_path') AS sp", true);
        assert_eq!(val(&sets[0], "who"), "2", "{var}");
        assert_eq!(val(&sets[0], "sp").replace('"', ""), "adv_p2, adv_p1", "{var}");
        // No schema picked: the saved list alone.
        let sets = go(&l, &db, "", "SELECT (SELECT v FROM t_who) AS who", true);
        assert_eq!(val(&sets[0], "who"), "2", "{var}");
        // Structure reads and the editing path use the same order.
        assert_eq!(db.pg_set_path("adv_s1"), "SET LOCAL search_path = \"adv_s1\", \"adv_p1\", \"adv_p2\"", "{var}");
        l.qsql(&pool, "DROP SCHEMA adv_s1 CASCADE; DROP SCHEMA adv_p1 CASCADE; DROP SCHEMA adv_p2 CASCADE");
    });
}

#[test]
fn a_schema_with_a_quote_in_its_name_is_quoted_in_the_path() {
    each(|var, uri, pooled| {
        let adv = Advanced { search_path: vec!["adv \"q\" x".into(), "public".into()], ..Default::default() };
        let l = open(uri, &adv, pooled);
        let db = l.quarry_test();
        let pool = l.fixture_pool();
        l.reset_schemas(&pool, &["adv \"q\" x"], "CREATE TABLE \"adv \"\"q\"\" x\".odd (v int); INSERT INTO \"adv \"\"q\"\" x\".odd VALUES (5)");
        let sets = go(&l, &db, "public", "SELECT (SELECT v FROM odd) AS v", true);
        assert!(sets[0].error.is_none(), "{var}: {:?}", sets[0].error);
        assert_eq!(val(&sets[0], "v"), "5", "{var}");
        l.qsql(&pool, "DROP SCHEMA \"adv \"\"q\"\" x\" CASCADE");
    });
}

#[test]
fn a_refused_value_fails_the_test_on_its_row_and_the_connect_by_name() {
    each(|var, uri, _| {
        // Shaped like a size, so only the server can say that it is below the minimum.
        let adv = Advanced { pg_settings: vec![("work_mem".into(), "1kB".into()), ("random_page_cost".into(), "1.5".into()), ("lock_timeout".into(), "7s".into())], ..Default::default() };
        let (res, info) = test_answer(uri, &adv);
        let res = res.expect_err(var);
        assert!(res.contains("refused 1 setting"), "{var}: {res}");
        assert_eq!(info.setting_errors.len(), 1, "{var}: {:?}", info.setting_errors);
        let (name, msg) = &info.setting_errors[0];
        assert_eq!(name, "work_mem", "{var}");
        assert!(msg.contains("work_mem") && msg.contains("outside the valid range"), "{var}: {msg}");
        assert!(info.schemas.iter().any(|s| s == "public"), "{var}: Test lists the schemas even when a row is refused: {:?}", info.schemas);
        let err = connect_error(uri, &adv);
        assert!(err.contains("work_mem = 1kB") && err.contains("outside the valid range") && err.contains("Advanced"), "{var}: {err}");
        // A good set passes Test with no refusals and the schemas for the path list.
        let (res, info) = test_answer(uri, &Advanced { pg_settings: vec![("work_mem".into(), "8MB".into())], ..Default::default() });
        assert!(res.is_ok() && info.setting_errors.is_empty(), "{var}: {res:?}");
    });
}

#[test]
fn every_refused_row_is_reported_not_only_the_first() {
    each(|var, uri, _| {
        let adv = Advanced {
            zone: Zone::Custom("Not/AZone".into()),
            pg_settings: vec![("work_mem".into(), "1kB".into()), ("random_page_cost".into(), "1.5".into()), ("jit_above_cost".into(), "-5".into())],
            ..Default::default()
        };
        let (res, info) = test_answer(uri, &adv);
        assert!(res.is_err(), "{var}");
        let names: Vec<&str> = info.setting_errors.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"TimeZone") && names.contains(&"work_mem"), "{var}: {names:?}");
        assert!(!names.contains(&"random_page_cost"), "{var}: a good row is not reported");
        let zone = info.setting_errors.iter().find(|(n, _)| n == "TimeZone").unwrap();
        assert!(zone.1.contains("TimeZone"), "{var}: {}", zone.1);
    });
}

#[test]
fn the_read_only_guard_holds_with_every_setting_applied() {
    each(|var, uri, pooled| {
        let l = open(uri, &all_settings(), pooled);
        let db = l.quarry_test();
        let pool = l.fixture_pool();
        l.qsql(&pool, "DROP TABLE IF EXISTS public.adv_guard; CREATE TABLE public.adv_guard (id int)");
        // The session is read-only wherever the session carries anything, and the readback passed to get here.
        if !pooled {
            assert_eq!(show(&l, &db, "default_transaction_read_only"), "on", "{var}");
        }
        let sets = go(&l, &db, "public", "INSERT INTO adv_guard VALUES (1); DELETE FROM adv_guard; CREATE TABLE adv_guard2 (a int); SELECT set_config('default_transaction_read_only', 'off', false); SELECT 1 AS ok", true);
        for i in 0..4 {
            assert!(sets[i].error.is_some(), "{var}: statement {i} must be refused on a locked connection: {:?}", sets[i].stmt);
        }
        assert_eq!(val(&sets[4], "ok"), "1", "{var}");
        assert_eq!(l.qone(&pool, "SELECT count(*)::text FROM public.adv_guard"), "0", "{var}");
        // Unlocked, the same connection writes, and the settings still apply to that run.
        let sets = go(&l, &db, "public", "INSERT INTO adv_guard VALUES (1); SELECT current_setting('work_mem') AS w, current_setting('transaction_read_only') AS ro", false);
        assert!(sets[0].error.is_none() && sets[1].error.is_none(), "{var}: {:?} {:?}", sets[0].error, sets[1].error);
        assert_eq!((val(&sets[1], "w"), val(&sets[1], "ro")), ("7MB".into(), "off".into()), "{var}");
        assert_eq!(l.qone(&pool, "SELECT count(*)::text FROM public.adv_guard"), "1", "{var}");
        // Locked again afterwards: the unlocked run left nothing behind.
        let sets = go(&l, &db, "public", "DELETE FROM adv_guard", true);
        assert!(sets[0].error.is_some(), "{var}");
        assert_eq!(l.qone(&pool, "SELECT count(*)::text FROM public.adv_guard"), "1", "{var}");
        l.qsql(&pool, "DROP TABLE public.adv_guard");
    });
}

#[test]
fn the_readback_still_refuses_a_session_that_is_not_read_only() {
    each(|var, uri, pooled| {
        if pooled {
            return;
        }
        let l = open(uri, &Advanced::default(), false);
        let sess = super::pg_advanced::PgSession::new(&all_settings(), Some("Europe/Paris".into())).unwrap();
        let (writable, scs_off) = l.w.rt.block_on(async {
            use std::str::FromStr;
            let o = sqlx::postgres::PgConnectOptions::from_str(uri).unwrap();
            let mut a = sqlx::PgConnection::connect_with(&o).await.unwrap();
            a.execute("SET default_transaction_read_only = off").await.unwrap();
            let failure = std::sync::Mutex::new(None);
            let writable = super::pg::session_with(&mut a, None, &sess, &failure).await.is_err();
            let mut b = sqlx::PgConnection::connect_with(&o).await.unwrap();
            b.execute("SET default_transaction_read_only = on; SET standard_conforming_strings = off").await.unwrap();
            let scs_off = super::pg::session_with(&mut b, None, &sess, &failure).await.is_err();
            (writable, scs_off)
        });
        assert!(writable, "{var}: a session that could write is still refused after the settings");
        assert!(scs_off, "{var}: strings that read a backslash as an escape are still refused");
    });
}

#[test]
fn a_writer_keeps_its_own_timeouts_and_still_gets_the_other_settings() {
    each(|var, uri, pooled| {
        let l = open(uri, &all_settings(), pooled);
        let db = l.quarry_test();
        let (st, lt, wm, tz) = l.w.rt.block_on(async {
            let mut c = super::write::writer_conn(&db, "public", false).await.unwrap();
            let mut tx = c.begin_write(&db).await.unwrap();
            let rows = tx.fetch_bound("SELECT current_setting('statement_timeout') AS st, current_setting('lock_timeout') AS lt, current_setting('work_mem') AS wm, current_setting('TimeZone') AS tz", &[]).await.unwrap();
            let t = |n| super::worker::text(&rows[0], n).unwrap();
            let r = (t("st"), t("lt"), t("wm"), t("tz"));
            let _ = tx.rollback().await;
            r
        });
        assert_eq!((st.as_str(), lt.as_str(), wm.as_str(), tz.as_str()), ("0", "10s", "7MB", "Europe/Paris"), "{var}: DDL and writes run with no statement timeout and their own lock timeout");
    });
}

#[test]
fn nothing_reaches_another_client_of_the_pooler() {
    let Ok(uri) = std::env::var(PGBOUNCER_ENV) else { return };
    let names = ["work_mem", "TimeZone", "search_path", "statement_timeout", "lock_timeout", "random_page_cost", "jit", "default_transaction_read_only", "extra_float_digits", "lc_time"];
    let mut adv = all_settings();
    adv.search_path = vec!["public".into()];
    let l = open(&uri, &adv, true);
    let before = fresh(&l, &names);
    let db = l.quarry_test();
    let pool = l.fixture_pool();
    l.qsql(&pool, "DROP TABLE IF EXISTS public.adv_leak; CREATE TABLE public.adv_leak (id int primary key, v int); INSERT INTO public.adv_leak VALUES (1, 1)");
    // Reads, a locked run, an unlocked run and a write: every kind of transaction Quarry opens.
    for round in 0..6 {
        let sets = go(&l, &db, "public", "SELECT count(*) AS n FROM adv_leak", round % 2 == 0);
        assert!(sets[0].error.is_none(), "round {round}: {:?}", sets[0].error);
        assert_eq!(show(&l, &db, "work_mem"), "7MB");
        l.w.sql_structure(3, db.clone(), "public".into(), "adv_leak".into());
        let Msg::SqlStructure { res, .. } = l.next() else { panic!("expected SqlStructure") };
        res.unwrap();
        let sets = go(&l, &db, "public", "UPDATE adv_leak SET v = v + 1", false);
        assert!(sets[0].error.is_none(), "round {round}: {:?}", sets[0].error);
        // Another client of the same pooler, in between: whichever backend it gets, it finds the server's own settings.
        assert_eq!(fresh(&l, &names), before, "round {round}: a setting reached another client");
    }
    l.qsql(&pool, "DROP TABLE public.adv_leak");
}

#[test]
fn the_application_name_is_shown_from_the_first_statement() {
    each(|var, uri, pooled| {
        let adv = Advanced { pg_settings: vec![("application_name".into(), "quarry adv name".into())], ..Default::default() };
        let l = open(uri, &adv, pooled);
        let db = l.quarry_test();
        let rows = l.w.rt.block_on(db.read_text("", "SELECT application_name FROM pg_stat_activity WHERE pid = pg_backend_pid()", false)).unwrap();
        assert_eq!(super::worker::text_at(&rows[0], 0).as_deref(), Some("quarry adv name"), "{var}");
    });
}

/// `open` that hands the refusal back instead of panicking, for settings a server or Quarry may refuse.
fn try_open(uri: &str, adv: &Advanced) -> Result<LivePg, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let w = crate::db::Worker::new(tx, eframe::egui::Context::default());
    let pool = w.rt.block_on(async { sqlx::PgPool::connect(uri).await.unwrap() });
    w.sql_connect_with(1, uri.to_string(), Duration::from_secs(30), Net::default(), adv.clone());
    let Msg::SqlConnected { res, .. } = rx.recv_timeout(Duration::from_secs(30)).unwrap() else { panic!("connect") };
    let (db, dbs, _) = res?;
    Ok(LivePg { w, rx, db, dbs, pool, uri: uri.to_string() })
}

#[test]
fn a_function_in_a_saved_schema_never_shadows_a_catalog_function() {
    each(|var, uri, pooled| {
        // pg_catalog in the list would sit behind the schemas before it, so it is refused, on Test and on connect.
        for list in [vec!["adv_evil", "pg_catalog"], vec!["pg_catalog"], vec!["adv_evil", "PG_Catalog"]] {
            let adv = Advanced { search_path: list.iter().map(|s| s.to_string()).collect(), ..Default::default() };
            assert!(connect_error(uri, &adv).contains("system schema"), "{var}: {list:?}");
        }
        let adv = Advanced { search_path: vec!["adv_evil".into(), "public".into()], ..Default::default() };
        let l = open(uri, &adv, pooled);
        let db = l.quarry_test();
        let pool = l.fixture_pool();
        l.qsql(&pool, "DROP SCHEMA IF EXISTS adv_evil CASCADE; CREATE SCHEMA adv_evil; CREATE TABLE adv_evil.t (id int PRIMARY KEY, v int); INSERT INTO adv_evil.t VALUES (1, 1); \
            CREATE FUNCTION adv_evil.format_type(oid, integer) RETURNS text LANGUAGE sql AS 'SELECT ''EVIL'''; \
            CREATE FUNCTION adv_evil.to_regtype(text) RETURNS regtype LANGUAGE sql AS 'SELECT ''money''::regtype'; \
            CREATE FUNCTION adv_evil.current_setting(text) RETURNS text LANGUAGE sql AS 'SELECT ''EVIL'''");
        let probe = "SELECT format_type(23, NULL) AS t, to_regtype('int4')::text AS r, current_setting('work_mem') <> 'EVIL' AS s";
        // The saved path as Quarry sets it never names pg_catalog, so the server keeps searching it first.
        for tab in ["adv_evil", "public", ""] {
            assert!(!db.pg_path(tab).contains("pg_"), "{var}: {}", db.pg_path(tab));
            let sets = go(&l, &db, tab, probe, true);
            assert!(sets[0].error.is_none(), "{var}: {:?}", sets[0].error);
            assert_eq!((val(&sets[0], "t"), val(&sets[0], "r"), val(&sets[0], "s")), ("integer".into(), "integer".into(), "true".into()), "{var}: a run in {tab:?}");
        }
        // A read of Quarry's own and the metadata behind a structure.
        let rows = l.w.rt.block_on(db.read_text("adv_evil", "SELECT format_type(23, NULL)", false)).unwrap();
        assert_eq!(super::worker::text_at(&rows[0], 0).unwrap_or_default(), "integer", "{var}");
        l.w.sql_structure(3, db.clone(), "adv_evil".into(), "t".into());
        let Msg::SqlStructure { res, .. } = l.next() else { panic!("expected SqlStructure") };
        let st = res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert!(st.columns.iter().all(|c| !c.full_type.contains("EVIL") && !c.full_type.contains("money")), "{var}: {:?}", st.columns.iter().map(|c| &c.full_type).collect::<Vec<_>>());
        // The writer's READ WRITE transaction, where the table's schema is set first.
        let (t, r) = l.w.rt.block_on(async {
            let mut c = super::write::writer_conn(&db, "adv_evil", false).await.unwrap();
            let mut tx = c.begin_write(&db).await.unwrap();
            let rows = tx.fetch_bound("SELECT format_type(23, NULL) AS t, to_regtype('int4')::text AS r", &[]).await.unwrap();
            let out = (super::worker::text(&rows[0], "t").unwrap(), super::worker::text(&rows[0], "r").unwrap());
            let _ = tx.rollback().await;
            out
        });
        assert_eq!((t.as_str(), r.as_str()), ("integer", "integer"), "{var}: the writer");
        l.qsql(&pool, "DROP SCHEMA adv_evil CASCADE");
    });
}

#[test]
fn floats_and_money_keep_their_spelling_whatever_the_form_asked_for() {
    each(|var, uri, pooled| {
        // The form refuses the settings that change how a value reads. A server that took them anyway would lose digits.
        let mut adv = all_settings();
        adv.pg_settings.extend([("extra_float_digits".to_string(), "-15".to_string()), ("lc_monetary".to_string(), "C".to_string())]);
        let l = match try_open(uri, &adv) {
            Err(x) => {
                assert!(x.contains("kept by Quarry"), "{var}: {x}");
                open(uri, &all_settings(), pooled)
            }
            Ok(l) => l,
        };
        let db = l.quarry_test();
        let pool = l.fixture_pool();
        l.reset_schemas(&pool, &["adv_flt"], "CREATE TABLE adv_flt.t (id int PRIMARY KEY, f float8); INSERT INTO adv_flt.t VALUES (1, 0.1::float8 + 0.2::float8), (2, 1234567.891), (3, 1e-7)");
        l.w.sql_page(7, db.clone(), "adv_flt".into(), "t".into(), Default::default(), 100, 0);
        let Msg::SqlCols { cols, .. } = l.next() else { panic!("expected SqlCols") };
        let Msg::Docs { res: Ok((rows, _, _)), .. } = l.next() else { panic!("expected Docs") };
        let docs: Vec<_> = rows.iter().map(|r| r.decode().unwrap()).collect();
        let at = |id: i64| docs.iter().find(|d| d.get_i64("id") == Ok(id)).unwrap_or_else(|| panic!("{var}: no row {id}")).clone();
        assert_eq!(at(1).get_f64("f").unwrap(), 0.1 + 0.2, "{var}: read");
        assert_eq!(at(2).get_f64("f").unwrap(), 1234567.891, "{var}: read");
        assert_eq!(at(3).get_f64("f").unwrap(), 1e-7, "{var}: read");
        // An edit of the float itself puts the value as read in its WHERE, which only matches the exact value.
        let ed = super::write::RowEdit { pk: at(1).get_document("_id").unwrap().clone(), changes: vec![("f".into(), Bson::Double(0.1 + 0.2), Bson::Double(5.5))] };
        l.w.sql_save(vec!["k".into()], 1, db.clone(), "adv_flt".into(), "t".into(), cols, vec!["id".into()], vec![ed], false);
        let Msg::Saved { res, .. } = l.next() else { panic!("expected Saved") };
        assert!(res.is_ok(), "{var}: the edit failed as if another session had changed the row: {res:?}");
        assert_eq!(l.qone(&pool, "SELECT f::text FROM adv_flt.t WHERE id = 1"), "5.5", "{var}");
        // The export is not lossy.
        let path = std::env::temp_dir().join(format!("adv_flt_{}_{}.csv", std::process::id(), var));
        let plan = super::export::Plan { fmt: crate::db::copy::Fmt::Csv, header: false, table: String::new(), bs: false };
        use std::sync::atomic::{AtomicBool, AtomicU64};
        l.w.rt.block_on(super::export::export_to(&db, "adv_flt", "SELECT id, f FROM t WHERE id > 1 ORDER BY id", &plan, super::export::Sink::File(path.clone()), &AtomicU64::new(0), &AtomicBool::new(false), &AtomicU64::new(0))).unwrap_or_else(|x| panic!("{var}: {x}"));
        let text = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(text.replace("\r\n", "\n"), "2,1234567.891\n3,0.0000001\n", "{var}");
        l.qsql(&pool, "DROP SCHEMA adv_flt CASCADE");
    });
}

#[test]
fn a_custom_offset_means_east_of_greenwich_as_the_form_says() {
    each(|var, uri, pooled| {
        let adv = Advanced { zone: Zone::Custom("+02:00".into()), ..Default::default() };
        let l = open(uri, &adv, pooled);
        let db = l.quarry_test();
        let sets = go(&l, &db, "public", "SELECT '2024-06-01 12:00:00+00'::timestamptz::text AS shown, '2024-06-01 12:00:00'::timestamptz::text AS typed, ('2024-06-01 12:00:00+02'::timestamptz = '2024-06-01 12:00:00'::timestamptz)::text AS same", true);
        assert!(sets[0].error.is_none(), "{var}: {:?}", sets[0].error);
        assert_eq!((val(&sets[0], "shown"), val(&sets[0], "typed"), val(&sets[0], "same")), ("2024-06-01 14:00:00+02".into(), "2024-06-01 12:00:00+02".into(), "true".into()), "{var}");
        assert_eq!(show(&l, &db, "TimeZone"), "<+0200>-02:00", "{var}");
        // A zone name still goes through as typed.
        let l = open(uri, &Advanced { zone: Zone::Custom("Europe/Paris".into()), ..Default::default() }, pooled);
        assert_eq!(show(&l, &l.quarry_test(), "TimeZone"), "Europe/Paris", "{var}");
    });
}

#[test]
fn a_write_refusal_is_worded_by_its_sqlstate_with_lc_messages_set() {
    each(|var, uri, pooled| {
        // C is the one locale every image has; the wording of another language is covered by the unit tests on the SQLSTATE.
        let adv = Advanced { pg_settings: vec![("lc_messages".into(), "C".into())], ..Default::default() };
        let l = open(uri, &adv, pooled);
        let db = l.quarry_test();
        let pool = l.fixture_pool();
        l.reset_schemas(&pool, &["adv_msg"], "CREATE TABLE adv_msg.t (id int PRIMARY KEY, n int NOT NULL); INSERT INTO adv_msg.t VALUES (1, 1)");
        let sets = go(&l, &db, "adv_msg", "UPDATE t SET n = NULL", false);
        let err = sets[0].error.clone().expect("the NOT NULL refuses");
        assert!(err.starts_with("error returned from database: 23502: "), "{var}: {err}");
        let e = crate::db::explain(&err, &crate::db::ErrCtx { what: "run the statement".into(), ..Default::default() });
        assert_eq!(e.title, "This column does not allow NULL", "{var}");
        l.qsql(&pool, "DROP SCHEMA adv_msg CASCADE");
    });
}

#[test]
fn a_table_is_made_with_a_type_from_the_saved_search_path() {
    each(|var, uri, pooled| {
        let adv = Advanced { search_path: vec!["adv_ext".into()], ..Default::default() };
        let l = open(uri, &adv, pooled);
        let db = l.quarry_test();
        let pool = l.fixture_pool();
        l.reset_schemas(&pool, &["adv_mk", "adv_ext"], "CREATE TYPE adv_ext.adv_mood AS ENUM ('a', 'b')");
        let spec = super::create_pg::TableSpec { schema: "adv_mk".into(), name: "made".into(), cols: vec![super::ddl::ColDef::new("m", "adv_mood")], ..Default::default() };
        l.w.pg_create_table(1, db.clone(), spec, false);
        let Msg::SqlDdl { res, undo, .. } = l.next() else { panic!("expected SqlDdl") };
        res.unwrap_or_else(|x| panic!("{var}: a type of the saved list resolves in a create, as it does everywhere else: {x}"));
        // The undo reads the table back under the same path, so it still sees the table as the create left it.
        l.w.sql_undo(db.clone(), undo.expect("an undo"), Vec::new(), false);
        let Msg::SqlDdl { res, .. } = l.next() else { panic!("expected SqlDdl") };
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        l.qsql(&pool, "DROP SCHEMA adv_mk CASCADE; DROP SCHEMA adv_ext CASCADE");
    });
}

/// The review's probe: a pooled or direct connection with a saved search_path and a TimeZone runs a subset dump (an enum and a domain made
/// behind the dump's guard DO blocks) and then saves a grid edit of the enum and the timestamp.
#[test]
fn a_file_with_guarded_types_and_a_grid_edit_work_under_a_saved_path_and_zone() {
    use super::dump::run_file;
    use super::import::Progress;
    use super::write::RowEdit;
    use mongodb::bson::doc;
    each(|var, uri, pooled| {
        let adv = Advanced { search_path: vec!["adv_wr".into()], zone: Zone::Custom("Europe/Paris".into()), ..Default::default() };
        let l = open(uri, &adv, pooled);
        let db = l.quarry_test();
        let pool = l.fixture_pool();
        l.qsql(&pool, "DROP SCHEMA IF EXISTS adv_wr CASCADE");
        let text = "CREATE SCHEMA adv_wr;\n\
                    DO $q$BEGIN CREATE TYPE adv_wr.mood AS ENUM ('a', 'b'); EXCEPTION WHEN duplicate_object THEN NULL; END$q$;\n\
                    DO $q$BEGIN CREATE DOMAIN adv_wr.posint AS integer CHECK (VALUE > 0); EXCEPTION WHEN duplicate_object THEN NULL; END$q$;\n\
                    CREATE TABLE adv_wr.t (id integer PRIMARY KEY, m adv_wr.mood, p adv_wr.posint, ts timestamptz);\n\
                    INSERT INTO adv_wr.t VALUES (1, 'a', 5, '2024-01-01 00:00:00+00');\n";
        let src = super::csv::Source::Mem(std::sync::Arc::new(text.as_bytes().to_vec()));
        let ran = l.w.rt.block_on(run_file(db.clone(), "public".into(), src, false, std::sync::Arc::new(Progress::default()), std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)), std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0))));
        ran.unwrap_or_else(|x| panic!("{var}: {x}"));
        l.w.sql_page(7, db.clone(), "adv_wr".into(), "t".into(), super::PageQuery::default(), 100, 0);
        let Msg::SqlCols { cols, .. } = l.next() else { panic!("expected SqlCols") };
        let Msg::Docs { res: Ok(_), .. } = l.next() else { panic!("expected Docs") };
        let edit = RowEdit { pk: doc! { "id": 1_i64 }, changes: vec![("m".into(), Bson::String("a".into()), Bson::String("b".into())), ("p".into(), Bson::Int64(5), Bson::Int64(7))] };
        l.w.sql_save(vec!["k".into()], 1, db.clone(), "adv_wr".into(), "t".into(), cols, vec!["id".into()], vec![edit], false);
        let Msg::Saved { res, .. } = l.next() else { panic!("expected Saved") };
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(l.qone(&pool, "SELECT m::text || p::text FROM adv_wr.t"), "b7", "{var}");
        l.qsql(&pool, "DROP SCHEMA adv_wr CASCADE");
    });
}
