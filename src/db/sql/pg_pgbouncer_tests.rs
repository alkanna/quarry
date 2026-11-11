//! Live tests through a real PgBouncer in transaction mode with `max_prepared_statements=0` (QUARRY_TEST_PGBOUNCER), the
//! worst case: every transaction may run on another backend and the pooler knows nothing of named statements. The fake
//! pooler of the other tests keeps one backend per connection, so it cannot show what these do.

use super::testing::{live_pgbouncer, LivePg};
use crate::db::{Msg, UndoAction};
use super::write::RowEdit;
use super::{Col, PageQuery};
use mongodb::bson::{Bson, Document};
use std::collections::HashMap;
use sqlx::Connection;

const ROWS: i64 = 6;
/// DEFAULT_POOL_SIZE of the test PgBouncer.
const POOL_SIZE: usize = 8;

fn fixture(l: &LivePg, schema: &str) -> sqlx::PgPool {
    l.quarry_test();
    let pool = l.fixture_pool();
    l.qsql(&pool, &format!("DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema}; CREATE TABLE {schema}.t (id int PRIMARY KEY, v int, s text); INSERT INTO {schema}.t SELECT g, g, 'a' FROM generate_series(1, {ROWS}) g"));
    pool
}

/// Waits for `n` answers and files them by request id (pages and structures) or tag (saves).
fn collect(l: &LivePg, n: usize) -> Vec<Msg> {
    (0..n).map(|_| l.next()).collect()
}

fn page_rows(msgs: Vec<Msg>) -> (Vec<Col>, Vec<Document>) {
    let mut cols = Vec::new();
    let mut docs = Vec::new();
    for m in msgs {
        match m {
            Msg::SqlCols { cols: c, .. } => cols = c,
            Msg::Docs { res, .. } => docs = res.unwrap_or_else(|x| panic!("page: {x}")).0.iter().map(|r| r.decode().unwrap()).collect(),
            _ => {}
        }
    }
    (cols, docs)
}

#[test]
fn bound_statements_survive_a_pooler_that_moves_every_transaction() {
    let Some(l) = live_pgbouncer() else { return };
    let schema = "pgb_prepared";
    let pool = fixture(&l, schema);
    let db = l.quarry_test();
    let mut req = 100u64;
    // One after another, so a pooled connection is reused across transactions many times over.
    for round in 0..8 {
        l.w.sql_page(req, db.clone(), schema.into(), "t".into(), PageQuery::default(), 100, 0);
        let (cols, docs) = page_rows(collect(&l, 2));
        assert_eq!(docs.len() as i64, ROWS, "round {round}");
        l.w.sql_structure(req + 1, db.clone(), schema.into(), "t".into());
        let Msg::SqlStructure { res, .. } = l.next() else { panic!("expected SqlStructure") };
        res.unwrap_or_else(|x| panic!("round {round} structure: {x}"));
        l.w.sql_count(req + 2, 0, db.clone(), schema.into(), "SELECT * FROM t".into());
        let Msg::SqlCount { res, .. } = l.next() else { panic!("expected SqlCount") };
        res.unwrap_or_else(|x| panic!("round {round} count: {x}"));
        let row = docs.iter().find(|d| d.get_i64("id") == Ok(1)).unwrap();
        let ed = RowEdit { pk: row.get_document("_id").unwrap().clone(), changes: vec![("s".into(), Bson::String("a".into()), Bson::String(format!("b{round}")))] };
        l.w.sql_save(vec!["k".into()], 1, db.clone(), schema.into(), "t".into(), cols.clone(), vec!["id".into()], vec![ed.clone()], false);
        let Msg::Saved { res, undo, .. } = l.next() else { panic!("expected Saved") };
        res.unwrap_or_else(|x| panic!("round {round} save: {x}"));
        l.w.sql_undo(db.clone(), undo.unwrap(), vec!["id".into()], false);
        let Msg::Written { res, .. } = l.next() else { panic!("expected Written") };
        res.unwrap_or_else(|x| panic!("round {round} undo: {x}"));
        assert_eq!(l.qone(&pool, &format!("SELECT s FROM {schema}.t WHERE id = 1")), "a", "round {round}");
        req += 10;
    }
    // Several at once, so the pool's connections interleave on the pooler's backends.
    for round in 0..6 {
        for i in 0..ROWS as u64 {
            l.w.sql_page(req + i, db.clone(), schema.into(), "t".into(), PageQuery::default(), 100, 0);
            l.w.sql_structure(req + 50 + i, db.clone(), schema.into(), "t".into());
            l.w.sql_count(req + 100 + i, 0, db.clone(), schema.into(), "SELECT * FROM t".into());
        }
        let msgs = collect(&l, ROWS as usize * 4);
        let mut seen = HashMap::new();
        for m in msgs {
            let (kind, ok) = match m {
                Msg::SqlCols { .. } => ("cols", true),
                Msg::Docs { res, .. } => ("docs", res.map_err(|x| panic!("round {round} page: {x}")).is_ok()),
                Msg::SqlStructure { res, .. } => ("structure", res.map_err(|x| panic!("round {round} structure: {x}")).is_ok()),
                Msg::SqlCount { res, .. } => ("count", res.map_err(|x| panic!("round {round} count: {x}")).is_ok()),
                _ => ("other", false),
            };
            assert!(ok, "round {round}: unexpected {kind}");
            *seen.entry(kind).or_insert(0) += 1;
        }
        assert_eq!(seen.get("docs"), Some(&(ROWS as usize)), "round {round}");
        req += 200;
        // Edits on other rows at the same time, each undone.
        let (cols, docs) = {
            l.w.sql_page(req, db.clone(), schema.into(), "t".into(), PageQuery::default(), 100, 0);
            page_rows(collect(&l, 2))
        };
        for (i, d) in docs.iter().enumerate() {
            let id = d.get_i64("id").unwrap();
            let ed = RowEdit { pk: d.get_document("_id").unwrap().clone(), changes: vec![("v".into(), Bson::Int64(id), Bson::Int64(id + 100))] };
            l.w.sql_save(vec![format!("k{i}")], 1, db.clone(), schema.into(), "t".into(), cols.clone(), vec!["id".into()], vec![ed], false);
        }
        let undos: Vec<UndoAction> = collect(&l, docs.len())
            .into_iter()
            .map(|m| {
                let Msg::Saved { res, undo, .. } = m else { panic!("expected Saved") };
                res.unwrap_or_else(|x| panic!("round {round} save: {x}"));
                undo.unwrap()
            })
            .collect();
        for u in undos {
            l.w.sql_undo(db.clone(), u, vec!["id".into()], false);
        }
        for m in collect(&l, docs.len()) {
            let Msg::Written { res, .. } = m else { panic!("expected Written") };
            res.unwrap_or_else(|x| panic!("round {round} undo: {x}"));
        }
        assert_eq!(l.qone(&pool, &format!("SELECT sum(v)::text FROM {schema}.t")), "21", "round {round}");
        req += 200;
    }
    l.qsql(&pool, &format!("DROP SCHEMA {schema} CASCADE"));
}

fn script(l: &LivePg, db: &super::Db, schema: &str, text: &str) -> Vec<super::ResultSet> {
    let (id, stop) = (std::sync::atomic::AtomicU64::new(0), std::sync::atomic::AtomicBool::new(false));
    l.w.rt.block_on(super::run::run(db, schema, text, false, &id, &stop, 0))
}

fn relay_query(l: &LivePg, db: &super::Db, schema: &str, text: &str) -> Result<(), String> {
    let id = std::sync::atomic::AtomicU64::new(0);
    let mut relay = l.w.rt.block_on(super::relay::Relay::open(db, schema, false, &id))?;
    l.w.rt.block_on(relay.query(text, &[])).map(|_| ())
}

#[test]
fn value_suggestions_leave_no_timeout_on_a_shared_backend() {
    let Some(l) = live_pgbouncer() else { return };
    let schema = "pgb_values";
    let pool = fixture(&l, schema);
    let db = l.quarry_test();
    // The configured value, which a SET on some backend cannot change, unlike what SHOW says on whichever backend answers.
    let default = l.qone(&pool, "SELECT reset_val FROM pg_settings WHERE name = 'statement_timeout'");
    assert_eq!(default, "0", "the test server is expected to have no statement timeout of its own");
    l.w.sql_field_values(1, db, schema.into(), "t".into(), "s".into(), None);
    let Msg::SqlValues { res, .. } = l.next() else { panic!("expected SqlValues") };
    assert_eq!(res.unwrap().map(|v| v.len()), Some(1), "the suggestion itself still works");
    // The pooler keeps 8 backends per database and login. Eight clients of the same database, each pinning one in an open
    // transaction, cover every one of them, so whichever backend the lookup ran on is read.
    let at = <sqlx::postgres::PgConnectOptions as std::str::FromStr>::from_str(&l.uri).unwrap().database("quarry_test");
    let mut holders = Vec::new();
    for i in 0..POOL_SIZE {
        let mut c = l.w.rt.block_on(sqlx::PgConnection::connect_with(&at)).unwrap();
        l.w.rt.block_on(sqlx::Executor::execute(&mut c, "BEGIN")).unwrap();
        let got = l.w.rt.block_on(sqlx::query_scalar::<_, String>("SELECT current_setting('statement_timeout')").persistent(false).fetch_one(&mut c)).unwrap();

        assert_eq!(got, default, "client {i} found the 300 ms limit left on its backend");
        holders.push(c);
    }
    for mut c in holders {
        l.w.rt.block_on(sqlx::Executor::execute(&mut c, "ROLLBACK")).unwrap();
    }
    l.qsql(&pool, &format!("DROP SCHEMA {schema} CASCADE"));
}

#[test]
fn session_advisory_locks_and_dblink_are_refused_before_they_reach_a_shared_backend() {
    let Some(l) = live_pgbouncer() else { return };
    let schema = "pgb_locks";
    let pool = fixture(&l, schema);
    let db = l.quarry_test();
    let held = |key: i64| l.qone(&pool, &format!("SELECT count(*)::text FROM pg_locks WHERE locktype = 'advisory' AND objid = {key} AND granted"));
    let release = |key: i64| l.qsql(&pool, &format!("SELECT pg_terminate_backend(pid) FROM pg_locks WHERE locktype = 'advisory' AND objid = {key}"));
    let mut leaked = Vec::new();
    for (i, stmt) in ["SELECT pg_advisory_lock(7101)", "SELECT pg_try_advisory_lock(7102)", "SELECT pg_catalog.pg_advisory_lock_shared(7103)", "SELECT pg_try_advisory_lock_shared(7104)"].into_iter().enumerate() {
        let key = 7101 + i as i64;
        let sets = script(&l, &db, schema, stmt);
        let refused = sets[0].error.as_deref() == Some(super::run::POOLER_SESSION);
        if held(key) != "0" {
            leaked.push(key);
        }
        release(key);
        assert!(refused, "{stmt}: {:?}", sets[0].error);
    }
    assert!(leaked.is_empty(), "locks left on a shared backend: {leaked:?}");
    // The shell relay takes the same path.
    let err = relay_query(&l, &db, schema, "SELECT pg_advisory_lock(7110)").err().expect("refused");
    assert_eq!(err, super::run::POOLER_SESSION);
    assert_eq!(held(7110), "0");
    release(7110);
    // The transaction-scoped lock ends with its transaction and stays allowed.
    let sets = script(&l, &db, schema, "SELECT pg_advisory_xact_lock(7111)");
    assert!(sets[0].error.is_none(), "{:?}", sets[0].error);
    assert_eq!(held(7111), "0");
    l.qsql(&pool, &format!("DROP SCHEMA {schema} CASCADE"));
}

/// A DO block and anything made in pg_temp run on a backend other clients get next, where a lock stays held and a table stays
/// there. The checks read the whole statement, so neither is let through behind some other first word.
#[test]
fn do_blocks_and_pg_temp_are_refused_behind_a_pooler_in_a_query_a_shell_and_a_file() {
    let Some(l) = live_pgbouncer() else { return };
    let schema = "pgb_do_temp";
    let pool = fixture(&l, schema);
    let db = l.quarry_test();
    let held = |key: i64| l.qone(&pool, &format!("SELECT count(*)::text FROM pg_locks WHERE locktype = 'advisory' AND objid = {key} AND granted"));
    let release = |key: i64| l.qsql(&pool, &format!("SELECT pg_terminate_backend(pid) FROM pg_locks WHERE locktype = 'advisory' AND objid = {key}"));
    let lock = "DO $$BEGIN PERFORM pg_advisory_lock(424243); END$$";
    let sets = script(&l, &db, schema, lock);
    let (refused, leaked) = (sets[0].error.as_deref() == Some(super::run::POOLER_SESSION), held(424243) != "0");
    release(424243);
    assert!(refused && !leaked, "Custom Query: a DO block took a session lock on a shared backend (refused: {refused}, held: {leaked}): {:?}", sets[0].error);
    for stmt in [
        "DO $$BEGIN CREATE TEMP TABLE pgb_tmp_do (a int); END$$", "CREATE TABLE pg_temp.pgb_tmp_a (a int)", "SELECT 1 AS a INTO pg_temp.pgb_tmp_b", "CREATE TABLE pgb_tmp_c AS SELECT 1 AS a FROM pg_temp.pgb_tmp_a",
        "CREATE FUNCTION pg_temp.pgb_tmp_f() RETURNS int LANGUAGE sql AS 'select 1'",
    ] {
        let sets = script(&l, &db, schema, stmt);
        assert_eq!(sets[0].error.as_deref(), Some(super::run::POOLER_SESSION), "Custom Query: {stmt}");
        // The shell refuses a DO block earlier still, as something that changes how values are escaped.
        let shell = relay_query(&l, &db, schema, stmt).err();
        assert!(if stmt.starts_with("DO") { shell.is_some() } else { shell.as_deref() == Some(super::run::POOLER_SESSION) }, "shell: {stmt}: {shell:?}");
    }
    assert!(relay_query(&l, &db, schema, lock).is_err());
    assert_eq!(held(424243), "0");
    release(424243);
    // A plain statement and one transaction-scoped lock still run.
    let sets = script(&l, &db, schema, "CREATE TABLE pgb_do_ok AS SELECT 1 AS a; SELECT pg_advisory_xact_lock(7131)");
    assert!(sets.iter().all(|s| s.error.is_none()), "{:?}", sets.iter().map(|s| &s.error).collect::<Vec<_>>());
    l.qsql(&pool, &format!("DROP SCHEMA {schema} CASCADE"));
}

#[test]
fn statements_that_cannot_run_in_a_transaction_block_say_so_behind_a_pooler() {
    let Some(l) = live_pgbouncer() else { return };
    let schema = "pgb_notx";
    let pool = fixture(&l, schema);
    let db = l.quarry_test();
    for stmt in ["VACUUM t", "CREATE INDEX CONCURRENTLY i ON t (v)", "DROP INDEX CONCURRENTLY IF EXISTS i", "REINDEX TABLE CONCURRENTLY t", "REINDEX SCHEMA pgb_notx", "CREATE DATABASE pgb_notx_db", "DROP DATABASE IF EXISTS pgb_notx_db", "ALTER SYSTEM SET work_mem = '5MB'", "CLUSTER"] {
        let sets = script(&l, &db, schema, stmt);
        assert_eq!(sets[0].error.as_deref(), Some(super::run::POOLER_NO_TX), "{stmt}");
        assert_eq!(relay_query(&l, &db, schema, stmt).err().as_deref(), Some(super::run::POOLER_NO_TX), "relay: {stmt}");
    }
    // What a transaction block allows still runs.
    for stmt in ["ALTER TABLE t ADD COLUMN w int", "REFRESH MATERIALIZED VIEW m", "CREATE INDEX i ON t (v)", "REINDEX TABLE t"] {
        l.qsql(&pool, &format!("DROP MATERIALIZED VIEW IF EXISTS {schema}.m; CREATE MATERIALIZED VIEW {schema}.m AS SELECT 1 AS x; DROP INDEX IF EXISTS {schema}.i; ALTER TABLE {schema}.t DROP COLUMN IF EXISTS w"));
        let sets = script(&l, &db, schema, stmt);
        assert!(sets[0].error.is_none(), "{stmt}: {:?}", sets[0].error);
    }
    l.qsql(&pool, &format!("DROP SCHEMA {schema} CASCADE"));
}
