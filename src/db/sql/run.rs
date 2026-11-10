//! Running Custom Query scripts: preparing a session, running statements one result at a time, stopping them.

use super::exec::{Conn, Item};
use super::worker::{make_cols, num_at, to_row};
use super::{lex, Col, Db, Dialect, Page, ResultSet, RUN_ROWS_MAX};
use crate::db::{e, Res};
use sqlx::Executor;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Readies the script's own connection: its id for Stop, the database, and write access when unlocked.
pub(super) async fn prepare(conn: &mut Conn, d: Dialect, schema: &str, read_only: bool, id: &AtomicU64) -> Res<()> {
    if let Conn::Pg(c) = conn {
        // No USE: the database is the pool's, and the schema comes in as the search_path.
        super::pg::prepare(c, read_only, id).await.map_err(e)?;
        return super::pg_read::set_search_path(c, schema).await.map_err(e);
    }
    prepare_my(conn.my()?, d, schema, read_only, id).await.map_err(e)
}

/// `prepare` for the MySQL-only modules, which hold sqlx's own connection.
pub(super) async fn prepare_my(conn: &mut sqlx::MySqlConnection, d: Dialect, schema: &str, read_only: bool, id: &AtomicU64) -> Result<(), sqlx::Error> {
    if let Ok(r) = conn.fetch_one("SELECT CONNECTION_ID()").await {
        id.store(num_at(&r, 0).unwrap_or(0), Ordering::SeqCst);
    }
    // With autocommit 0 from the server's globals, every write of the script is rolled back when the connection
    // closes, and reads keep a snapshot and metadata locks. The script can still turn it off itself.
    conn.execute("SET SESSION autocommit = 1").await?;
    if !schema.is_empty() {
        let q = format!("USE {}", d.quote_ident(schema));
        conn.execute(q.as_str()).await?;
    }
    if !read_only {
        conn.execute(d.session_read_only(false)).await?;
    }
    Ok(())
}

/// Pins what a read of every row as stored depends on: UTF-8 both ways, and no sql_select_limit cutting a SELECT
/// short. A session can get either from the server's globals.
pub(super) async fn pin_reads(c: &mut Conn) -> Res<()> {
    // The startup parameters already fix the encoding, and Postgres has no select limit.
    if matches!(c, Conn::Pg(_)) {
        return Ok(());
    }
    pin_reads_my(c.my()?).await.map_err(e)
}

/// `pin_reads` for the MySQL-only modules.
pub(super) async fn pin_reads_my(c: &mut sqlx::MySqlConnection) -> Result<(), sqlx::Error> {
    c.execute("SET NAMES utf8mb4").await?;
    c.execute("SET SESSION sql_select_limit = DEFAULT").await?;
    Ok(())
}

pub(super) fn failed(stmt: &str, error: String) -> ResultSet {
    ResultSet { error: Some(error), ..ResultSet::new(stmt) }
}

/// Runs one statement and keeps at most `cap` rows of each result. A CALL can return several
/// results, each becomes its own set. Reading stops at the cap, which leaves the connection mid-result:
/// the caller must not reuse it (`capped` on the last set says so).
pub(super) async fn run_one(conn: &mut Conn, stmt: &str, cap: usize) -> Vec<ResultSet> {
    run_sent(conn, stmt, stmt, cap, 0).await
}

/// `run_one` for `sent` text that holds `stmt` after `shift` characters, so the position of a server error points
/// into the user's statement and not into the paging wrapper around it.
async fn run_sent(conn: &mut Conn, stmt: &str, sent: &str, cap: usize, shift: usize) -> Vec<ResultSet> {
    let pg = matches!(conn, Conn::Pg(_));
    let t = Instant::now();
    let mut sets: Vec<ResultSet> = Vec::new();
    let mut cur = ResultSet::new(stmt);
    let mut cols: Vec<Col> = Vec::new();
    let mut failure = None;
    {
        let mut s = conn.fetch_text(sent);
        while let Some(item) = futures_util::StreamExt::next(&mut s).await {
            match item {
                Ok(Item::Row(row)) => {
                    if cols.is_empty() {
                        cols = make_cols(&row.cols(), &HashMap::new(), false);
                    }
                    if cur.rows.len() >= cap {
                        cur.capped = true;
                        break;
                    }
                    match to_row(&cols, &row, &[]) {
                        Ok(r) => cur.rows.push(r),
                        Err(err) => {
                            failure = Some(err);
                            break;
                        }
                    }
                }
                // The end of one result set (or the OK of a write).
                Ok(Item::End(r)) => {
                    let mut done = std::mem::replace(&mut cur, ResultSet::new(stmt));
                    done.cols = std::mem::take(&mut cols);
                    if done.cols.is_empty() {
                        done.affected = Some(r.rows_affected());
                    }
                    sets.push(done);
                }
                Err(err) => {
                    failure = Some(if pg { super::pg_read::pg_error(&err, stmt, shift) } else { e(err) });
                    break;
                }
            }
        }
    }
    // A CALL ends with a bare OK after its results, which is not a result of its own.
    if sets.len() > 1 && sets.last().is_some_and(|l| l.cols.is_empty() && l.affected == Some(0)) && sets.iter().any(|x| !x.cols.is_empty()) {
        sets.pop();
    }
    // Stopped in the middle of a result: keep what was read.
    if !cur.rows.is_empty() || cur.capped || failure.is_some() || sets.is_empty() {
        cur.cols = cols;
        cur.error = failure;
        sets.push(cur);
    }
    if sets.len() == 1 && sets[0].error.is_none() && sets[0].cols.is_empty() {
        // No rows came back: a SELECT with an empty result still has headers, a write does not.
        if let Ok(desc) = conn.describe(sent).await {
            if !desc.is_empty() {
                sets[0].cols = make_cols(&desc, &HashMap::new(), false);
                sets[0].affected = None;
            }
        }
    }
    let ms = t.elapsed().as_millis();
    for x in &mut sets {
        x.ms = ms;
    }
    sets
}

/// One statement as a page of at most `RUN_ROWS_MAX` rows, when it is a plain read. A column name that
/// repeats cannot sit in a derived table, so such a statement falls back to a plain capped run.
pub(super) async fn run_stmt(conn: &mut Conn, d: super::Dialect, stmt: &str, page: usize) -> Vec<ResultSet> {
    if !lex::pageable(stmt, d) {
        return run_one(conn, stmt, RUN_ROWS_MAX).await;
    }
    let wrapped = lex::page_sql(stmt, page, RUN_ROWS_MAX);
    let mut rs = run_sent(conn, stmt, &wrapped, RUN_ROWS_MAX + 1, super::pg_read::PAGE_PREFIX).await;
    if rs.len() == 1 && rs[0].error.as_deref().is_some_and(|m| m.contains("Duplicate column name")) {
        return run_one(conn, stmt, RUN_ROWS_MAX).await;
    }
    for r in &mut rs {
        r.stmt = stmt.to_string();
        if r.error.is_none() {
            let next = r.rows.len() > RUN_ROWS_MAX;
            r.rows.truncate(RUN_ROWS_MAX);
            r.page = Some(Page { n: page, next, total: None, ordered: lex::has_order_by(stmt, d) });
        }
    }
    rs
}

/// Runs the statements of `text`; `page` is the page every pageable statement is read at.
pub(super) async fn run(db: &Db, schema: &str, text: &str, read_only: bool, id: &AtomicU64, cancel: &AtomicBool, page: usize) -> Vec<ResultSet> {
    // Detached, so a USE or @variable of this script never leaks into the pool.
    let mut conn = match db.acquire_conn().await {
        Ok(c) => c,
        Err(err) => return vec![failed("", err)],
    };
    let d = db.dialect;
    if let Err(err) = prepare(&mut conn, d, schema, read_only, id).await {
        return vec![failed("", err)];
    }
    // PostgreSQL: the timeout the session got at startup, kept to be applied again where a session value would not hold.
    let mut timeout = String::new();
    if let Conn::Pg(c) = &mut conn {
        timeout = match super::pg_read::statement_timeout(c).await {
            Ok(t) => t,
            Err(err) => return vec![failed("", e(err))],
        };
        // An unlocked session runs as typed, so its limit is set once here and holds for every statement.
        if !read_only {
            if let Err(err) = c.execute(format!("SET statement_timeout = '{timeout}'").as_str()).await {
                return vec![failed("", e(err))];
            }
        }
    }
    let mut sets = Vec::new();
    // Each statement goes alone, so user text is never sent as one multi-statement string.
    let stmts = lex::split_statements(text, d);
    let mut why: Option<&str> = None;
    // A USE in the script changes the database the later statements run in, and so the one the log names.
    let mut here = schema.to_string();
    for (_, stmt) in stmts {
        // KILL QUERY only interrupts the statement in flight, so the flag is what keeps the rest from running.
        if cancel.load(Ordering::SeqCst) {
            why = Some("Stopped before running");
        }
        if let Some(w) = why {
            sets.push(failed(stmt, w.into()));
            continue;
        }
        if read_only {
            if let Err(why) = lex::read_only_check(stmt, d) {
                sets.push(ResultSet { refused: true, ..failed(stmt, why) });
                continue;
            }
        }
        let rs = if read_only && d == Dialect::Postgres { run_locked(&mut conn, d, stmt, page, &timeout, &db.pg_locals()).await } else { run_stmt(&mut conn, d, stmt, page).await };
        let failed = rs.iter().find_map(|r| r.error.as_deref());
        crate::db::log_sql(db.conn, &here, stmt, rs.first().map_or(0, |r| r.ms), failed);
        if let (None, Some(to)) = (failed, lex::use_target(stmt, d)) {
            here = to;
        }
        if rs.iter().any(|r| r.error.is_some()) {
            sets.extend(rs);
            break;
        }
        if rs.last().is_some_and(|r| r.capped) {
            // The rest of the big result is still on the wire; draining it could take as long as the query, and
            // a fresh connection would lose the script's variables and USE, so the script ends here.
            why = Some("Not run: the previous result was cut at the row limit");
        }
        sets.extend(rs);
    }
    // The connection ran text of the user's, so it is closed and never reused: an advisory lock or a dblink
    // connection opened by a function the user called would outlive the rollback on a pooled one.
    if let Conn::Pg(_) = conn {
        let _ = tokio::time::timeout(Duration::from_secs(2), conn.close()).await;
    }
    sets
}

/// One user statement on a locked PostgreSQL connection: alone in `BEGIN READ ONLY` and always rolled back, never
/// committed, so nothing it does to the session outlives it and it never shares a transaction with another statement.
/// The paging wrapper and a read's count go through here too.
pub(super) async fn run_locked(conn: &mut Conn, d: Dialect, stmt: &str, page: usize, timeout: &str, locals: &str) -> Vec<ResultSet> {
    let Conn::Pg(c) = conn else { return run_stmt(conn, d, stmt, page).await };
    if let Err(err) = super::pg_read::begin_read_only(c, timeout, locals).await {
        return vec![failed(stmt, super::pg_read::pg_error(&err, stmt, 0))];
    }
    let rs = run_stmt(conn, d, stmt, page).await;
    // A read cut at the row limit leaves the rest of the result on the wire. The script ends there and the
    // connection is closed, which ends the transaction without waiting for that result to drain.
    if !rs.last().is_some_and(|r| r.capped) {
        let _ = conn.execute("ROLLBACK").await;
    }
    rs
}

/// The row count of a paged statement on its own connection. PostgreSQL counts inside a rolled-back read-only
/// transaction like every other statement that carries user text.
pub(super) async fn count_rows(db: &Db, schema: &str, stmt: &str) -> Res<u64> {
    let mut conn = db.acquire_conn().await?;
    prepare(&mut conn, db.dialect, schema, true, &AtomicU64::new(0)).await?;
    let sql = lex::count_sql(stmt);
    let t = Instant::now();
    let r = match &mut conn {
        Conn::Pg(c) => {
            let timeout = super::pg_read::statement_timeout(c).await.map_err(e)?;
            super::pg_read::begin_read_only(c, &timeout, &db.pg_locals()).await.map_err(|x| super::pg_read::pg_error(&x, &sql, 0))?;
            let r = c.fetch_all(sql.as_str()).await.map(super::exec::SqlRow::wrap_pg);
            let _ = c.execute("ROLLBACK").await;
            r
        }
        c => c.fetch_all(&sql).await,
    };
    db.note(schema, &sql, t, &r);
    let r = r.map_err(|x| if db.dialect == Dialect::Postgres { super::pg_read::pg_error(&x, stmt, super::pg_read::COUNT_PREFIX) } else { e(x) })?;
    let n = r.first().and_then(|r| num_at(r, 0)).ok_or("The count was not a number")?;
    let _ = conn.close().await;
    Ok(n)
}

/// A connection of its own, outside the pool (a busy or exhausted pool never blocks it), or `why` when the server
/// does not answer within 10 s.
pub(super) async fn side_conn(db: &Db, why: &str) -> Res<Conn> {
    tokio::time::timeout(Duration::from_secs(10), db.detached()).await.map_err(|_| why.to_string())?
}

/// KILL QUERY from a connection of its own. A plain function, because inside a spawned block the
/// executor lifetimes of a local connection do not infer.
pub(super) async fn kill_query(db: &Db, id: u64) -> Res<String> {
    if db.dialect == Dialect::Postgres {
        return super::pg::cancel(db, id).await;
    }
    let mut c = side_conn(db, "Could not reach the server to stop the statement").await?;
    let q = db.dialect.cancel(id);
    let r = c.execute(q.as_str()).await.map(|_| "Stopped the running statement".to_string()).map_err(|x| super::server::kill_error(&x));
    let _ = c.close().await;
    r
}

/// What Stop needs of a running Custom Query: the server connection id once it is known, and the flag
/// that keeps the rest of the script from starting. Each run gets a fresh one, so a late Stop never
/// reaches the next run.
#[derive(Clone, Default)]
pub struct RunHandle {
    pub conn: Arc<AtomicU64>,
    pub cancel: Arc<AtomicBool>,
}

/// Live tests of Custom Query on PostgreSQL, on every server named by QUARRY_TEST_POSTGRES and
/// QUARRY_TEST_POSTGRES13. Each test makes its own schema in the shared `quarry_test` database.
#[cfg(test)]
mod pg_tests {
    use super::*;
    use crate::db::sql::testing::{every_pg, LivePg};
    use crate::db::Msg;
    use mongodb::bson::Bson;

    /// A fresh schema of the shared database with `ddl` run in it, and the `Db` pointing at that database.
    fn fixture(l: &LivePg, schema: &str, ddl: &str) -> (Db, sqlx::PgPool) {
        let db = l.quarry_test();
        let pool = l.fixture_pool();
        l.qsql(&pool, &format!("DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema}; SET search_path = {schema}; {ddl}"));
        (db, pool)
    }

    fn go_at(l: &LivePg, db: &Db, schema: &str, text: &str, read_only: bool, page: usize) -> Vec<ResultSet> {
        l.w.sql_run(9, db.clone(), schema.into(), text.into(), read_only, page, RunHandle::default());
        loop {
            if let Msg::SqlRan { sets, .. } = l.next() {
                return sets;
            }
        }
    }

    fn go(l: &LivePg, db: &Db, schema: &str, text: &str, read_only: bool) -> Vec<ResultSet> {
        go_at(l, db, schema, text, read_only, 0)
    }

    /// The cell `name` of row `i`, as text.
    fn val(set: &ResultSet, i: usize, name: &str) -> String {
        match set.rows[i].decode().unwrap().get(name).unwrap_or_else(|| panic!("no column {name}")) {
            Bson::String(s) => s.clone(),
            Bson::Int64(n) => n.to_string(),
            Bson::Int32(n) => n.to_string(),
            b => b.to_string(),
        }
    }

    fn code(x: &sqlx::Error) -> Option<String> {
        x.as_database_error().and_then(|d| d.code().map(|c| c.to_string()))
    }

    #[test]
    fn the_prefixes_match_the_wrappers() {
        assert!(lex::page_sql("X", 0, 1).starts_with("SELECT * FROM (\n") && "SELECT * FROM (\n".chars().count() == super::super::pg_read::PAGE_PREFIX);
        assert!(lex::count_sql("X").starts_with("SELECT COUNT(*) FROM (\n") && "SELECT COUNT(*) FROM (\n".chars().count() == super::super::pg_read::COUNT_PREFIX);
    }

    #[test]
    fn several_statements_give_several_result_sets() {
        every_pg(|l, var| {
            let (db, _p) = fixture(l, "t6_multi", "");
            let sets = go(l, &db, "", "SELECT 1 AS a; SELECT 'x' AS b, 2 AS c;\n-- a comment\nSELECT 3 AS d", true);
            assert_eq!(sets.len(), 3, "{var}: {:?}", sets.iter().map(|s| &s.error).collect::<Vec<_>>());
            assert!(sets.iter().all(|s| s.error.is_none()), "{var}");
            assert_eq!(val(&sets[0], 0, "a"), "1");
            assert_eq!((val(&sets[1], 0, "b"), val(&sets[1], 0, "c")), ("x".into(), "2".into()));
            assert_eq!(val(&sets[2], 0, "d"), "3");
            assert_eq!(sets[1].cols.len(), 2);
        });
    }

    #[test]
    fn the_tab_schema_decides_how_an_unqualified_name_resolves() {
        every_pg(|l, var| {
            let (db, _p) = fixture(l, "t6_path", "CREATE TABLE things (id int); INSERT INTO things VALUES (1), (2);");
            let sets = go(l, &db, "t6_path", "SELECT count(*) AS n FROM things", true);
            assert!(sets[0].error.is_none(), "{var}: {:?}", sets[0].error);
            assert_eq!(val(&sets[0], 0, "n"), "2");
            // A schema with a name that needs quoting still resolves.
            let (db, p) = fixture(l, "\"t6 Odd\"", "CREATE TABLE things (id int); INSERT INTO things VALUES (1);");
            let sets = go(l, &db, "t6 Odd", "SELECT count(*) AS n FROM things", true);
            assert_eq!(val(&sets[0], 0, "n"), "1", "{var}");
            // No schema, no way to find the table.
            let sets = go(l, &db, "", "SELECT count(*) AS n FROM things", true);
            let err = sets[0].error.clone().unwrap();
            assert!(err.contains("42P01") && err.contains("relation \"things\" does not exist"), "{var}: {err}");
            l.qsql(&p, "DROP SCHEMA \"t6 Odd\" CASCADE");
        });
    }

    #[test]
    fn a_refused_statement_carries_the_lexer_message_and_never_runs() {
        every_pg(|l, var| {
            let (db, p) = fixture(l, "t6_refuse", "CREATE TABLE t (id int); INSERT INTO t VALUES (1);");
            let sets = go(l, &db, "t6_refuse", "SELECT 1 AS a; DELETE FROM t; SELECT set_config('search_path', 'pg_catalog', false); SELECT count(*) AS n FROM t", true);
            assert_eq!(sets.len(), 4, "{var}");
            assert!(sets[0].error.is_none());
            for i in [1, 2] {
                assert!(sets[i].refused, "{var}: {i}");
            }
            assert!(sets[1].error.as_deref().unwrap().contains("read-only"), "{:?}", sets[1].error);
            assert_eq!(sets[1].error, lex::read_only_check("DELETE FROM t", Dialect::Postgres).err());
            assert_eq!(sets[2].error, lex::read_only_check("SELECT set_config('search_path', 'pg_catalog', false)", Dialect::Postgres).err());
            // Neither one reached the server: the row is there and the later read still sees the schema.
            assert_eq!(val(&sets[3], 0, "n"), "1", "{var}");
            assert_eq!(l.qone(&p, "SELECT count(*)::text FROM t6_refuse.t"), "1");
        });
    }

    #[test]
    fn set_config_never_survives_to_the_next_statement() {
        every_pg(|l, var| {
            let (db, _p) = fixture(l, "t6_cfg", "CREATE TABLE t (id int);");
            let d = Dialect::Postgres;
            l.w.rt.block_on(async {
                let mut conn = db.acquire_conn().await.unwrap();
                prepare(&mut conn, d, "t6_cfg", true, &AtomicU64::new(0)).await.unwrap();
                let Conn::Pg(c) = &mut conn else { unreachable!() };
                let timeout = crate::db::sql::pg_read::statement_timeout(c).await.unwrap();
                // The lexer would refuse these; they are sent straight to layer 2, which must hold on its own.
                for evil in [
                    "SELECT set_config('default_transaction_read_only', 'off', false)",
                    "SELECT set_config('search_path', 'pg_catalog', false)",
                    "SELECT set_config('client_encoding', 'SJIS', false)",
                    "SELECT set_config('statement_timeout', '1', false)",
                ] {
                    let sets = run_locked(&mut conn, d, evil, 0, &timeout, "").await;
                    assert!(sets[0].error.is_none(), "{var}: {evil}: {:?}", sets[0].error);
                }
                let sets = run_locked(&mut conn, d, "SELECT current_setting('default_transaction_read_only') AS ro, current_setting('search_path') AS sp, current_setting('client_encoding') AS enc, current_setting('statement_timeout') AS st", 0, &timeout, "").await;
                assert_eq!((val(&sets[0], 0, "ro"), val(&sets[0], 0, "sp"), val(&sets[0], 0, "enc")), ("on".into(), "\"t6_cfg\"".into(), "UTF8".into()), "{var}");
                assert_eq!(val(&sets[0], 0, "st"), timeout, "{var}");
                // A write is refused by the server inside the transaction, whatever the lexer did.
                let sets = run_locked(&mut conn, d, "INSERT INTO t VALUES (1)", 0, &timeout, "").await;
                assert!(sets[0].error.as_deref().unwrap().contains("25006"), "{var}: {:?}", sets[0].error);
                let _ = conn.close().await;
            });
        });
    }

    #[test]
    fn stop_cancels_a_sleeping_statement_quickly() {
        every_pg(|l, var| {
            let (db, _p) = fixture(l, "t6_stop", "");
            let h = RunHandle::default();
            let (id, cancel) = (h.conn.clone(), h.cancel.clone());
            l.w.sql_run(3, db.clone(), "t6_stop".into(), "SELECT pg_sleep(30); SELECT 1".into(), true, 0, h);
            let start = Instant::now();
            while id.load(Ordering::SeqCst) == 0 {
                assert!(start.elapsed() < Duration::from_secs(10), "{var}: no backend pid");
                std::thread::sleep(Duration::from_millis(20));
            }
            std::thread::sleep(Duration::from_millis(400));
            l.w.sql_kill(db, id.load(Ordering::SeqCst), cancel);
            let (mut ran, mut info) = (None, None);
            while ran.is_none() || info.is_none() {
                match l.next() {
                    Msg::SqlRan { sets, .. } => ran = Some(sets),
                    Msg::Info(r) => info = Some(r),
                    _ => {}
                }
            }
            assert!(info.unwrap().is_ok(), "{var}");
            assert!(start.elapsed() < Duration::from_secs(8), "{var}: the sleep was not interrupted");
            let sets = ran.unwrap();
            let err = sets[0].error.as_deref().unwrap();
            assert!(err.contains("57014") && err.contains("user request"), "{var}: {err}");
            // The error ends the script; the second statement never starts.
            assert_eq!(sets.len(), 1, "{var}");
            assert_eq!(id.load(Ordering::SeqCst), 0);
        });
    }

    #[test]
    fn the_statement_timeout_fires_on_a_locked_and_an_unlocked_run() {
        for var in crate::db::sql::testing::PG_ENVS {
            let Ok(uri) = std::env::var(var) else { continue };
            let (tx, rx) = std::sync::mpsc::channel();
            let w = crate::db::Worker::new(tx, eframe::egui::Context::default());
            let adv = crate::db::sql::Advanced { query_secs: Some(1), ..Default::default() };
            w.sql_connect_with(1, uri, Duration::from_secs(30), crate::db::Net::default(), adv);
            let Msg::SqlConnected { res: Ok((db, _, _)), .. } = rx.recv_timeout(Duration::from_secs(20)).unwrap() else { panic!("connect") };
            for read_only in [true, false] {
                w.sql_run(5, db.clone(), "".into(), "SELECT current_setting('statement_timeout') AS st; SELECT pg_sleep(10)".into(), read_only, 0, RunHandle::default());
                let start = Instant::now();
                let Msg::SqlRan { sets, .. } = rx.recv_timeout(Duration::from_secs(15)).unwrap() else { panic!("expected SqlRan") };
                assert!(start.elapsed() < Duration::from_secs(6), "{var} read_only={read_only}: {:?}", start.elapsed());
                assert_eq!(val(&sets[0], 0, "st"), "1s", "{var} read_only={read_only}");
                let err = sets[1].error.as_deref().unwrap();
                assert!(err.contains("57014") && err.contains("statement timeout"), "{var}: {err}");
                let ex = crate::db::explain(err, &Default::default());
                assert_eq!(ex.kind, crate::db::ErrKind::Timeout, "{var}");
            }
        }
    }

    #[test]
    fn explain_returns_a_plan_and_stays_read_only() {
        every_pg(|l, var| {
            let (db, _p) = fixture(l, "t6_explain", "CREATE TABLE things (id int); INSERT INTO things VALUES (1);");
            let sets = go(l, &db, "t6_explain", "EXPLAIN SELECT * FROM things WHERE id = 1", true);
            assert!(sets[0].error.is_none(), "{var}: {:?}", sets[0].error);
            assert_eq!(sets[0].cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["QUERY PLAN"]);
            assert!(val(&sets[0], 0, "QUERY PLAN").contains("Scan"), "{var}");
            // ANALYZE would run the statement, in either spelling.
            for text in ["EXPLAIN ANALYZE SELECT 1", "EXPLAIN (ANALYZE, BUFFERS) SELECT 1"] {
                let sets = go(l, &db, "t6_explain", text, true);
                assert!(sets[0].refused, "{var}: {text}");
            }
            // Unlocked, the same plan runs as typed.
            let sets = go(l, &db, "t6_explain", "EXPLAIN SELECT * FROM things", false);
            assert!(sets[0].error.is_none() && !sets[0].rows.is_empty(), "{var}");
        });
    }

    #[test]
    fn paging_reads_pages_of_a_thousand_and_counts_in_the_background() {
        every_pg(|l, var| {
            let (db, p) = fixture(l, "t6_page", "CREATE TABLE big (id int PRIMARY KEY, v text); INSERT INTO big SELECT i, 'row ' || i FROM generate_series(1, 2500) i;");
            let stmt = "SELECT * FROM big ORDER BY id";
            let sets = go(l, &db, "t6_page", stmt, true);
            assert!(sets[0].error.is_none(), "{var}: {:?}", sets[0].error);
            let (rows, page) = (sets[0].rows.len(), sets[0].page.unwrap());
            assert_eq!((rows, page.next, page.n, page.ordered), (1000, true, 0, true), "{var}");
            assert_eq!(val(&sets[0], 999, "id"), "1000");
            l.w.sql_count(11, 0, db.clone(), "t6_page".into(), stmt.into());
            let Msg::SqlCount { res, idx, .. } = l.next() else { panic!("expected SqlCount") };
            assert_eq!((res, idx), (Ok(2500), 0), "{var}");
            let last = go_at(l, &db, "t6_page", stmt, true, 2);
            let page = last[0].page.unwrap();
            assert_eq!((last[0].rows.len(), page.next, page.n), (500, false, 2), "{var}");
            assert_eq!(val(&last[0], 0, "id"), "2001");
            // The count is held to the same rules: text that would write is refused by the read-only transaction.
            l.w.sql_count(12, 0, db.clone(), "t6_page".into(), "SELECT * FROM big WHERE id IN (SELECT id FROM big); DELETE FROM big".into());
            let Msg::SqlCount { res, .. } = l.next() else { panic!("expected SqlCount") };
            assert!(res.is_err(), "{var}");
            assert_eq!(l.qone(&p, "SELECT count(*)::text FROM t6_page.big"), "2500");
        });
    }

    #[test]
    fn an_unlocked_run_sees_its_own_writes_and_honors_a_user_transaction() {
        every_pg(|l, var| {
            let (db, p) = fixture(l, "t6_rw", "CREATE TABLE t (id int PRIMARY KEY);");
            let sets = go(l, &db, "t6_rw", "INSERT INTO t VALUES (1); SELECT count(*) AS n FROM t", false);
            assert!(sets.iter().all(|s| s.error.is_none()), "{var}: {:?}", sets.iter().map(|s| &s.error).collect::<Vec<_>>());
            assert_eq!(sets[0].affected, Some(1));
            assert_eq!(val(&sets[1], 0, "n"), "1", "{var}");
            assert_eq!(l.qone(&p, "SELECT count(*)::text FROM t6_rw.t"), "1", "{var}: autocommit per statement");
            // A user ROLLBACK undoes what came before it in the run.
            let sets = go(l, &db, "t6_rw", "BEGIN; INSERT INTO t VALUES (2); SELECT count(*) AS n FROM t; ROLLBACK; SELECT count(*) AS n FROM t", false);
            assert!(sets.iter().all(|s| s.error.is_none()), "{var}");
            assert_eq!((val(&sets[2], 0, "n"), val(&sets[4], 0, "n")), ("2".into(), "1".into()), "{var}");
            // A user COMMIT keeps it.
            go(l, &db, "t6_rw", "BEGIN; INSERT INTO t VALUES (3); COMMIT", false);
            assert_eq!(l.qone(&p, "SELECT count(*)::text FROM t6_rw.t"), "2", "{var}");
            // A transaction the script leaves open is ended by closing its connection.
            go(l, &db, "t6_rw", "BEGIN; INSERT INTO t VALUES (4)", false);
            assert_eq!(l.qone(&p, "SELECT count(*)::text FROM t6_rw.t"), "2", "{var}");
            // The writes of the unlocked run did not unlock the pool.
            let sets = go(l, &db, "t6_rw", "INSERT INTO t VALUES (5)", true);
            assert!(sets[0].refused, "{var}");
            let err = l.w.rt.block_on(async {
                let mut c = db.acquire_conn().await.unwrap();
                let Conn::Pg(c) = &mut c else { unreachable!() };
                c.execute("INSERT INTO t6_rw.t VALUES (6)").await.unwrap_err()
            });
            assert_eq!(code(&err).as_deref(), Some("25006"), "{var}");
        });
    }

    #[test]
    fn errors_report_their_sqlstate_and_where_they_stopped() {
        every_pg(|l, var| {
            let (db, _p) = fixture(l, "t6_err", "CREATE TABLE t (id int PRIMARY KEY); INSERT INTO t VALUES (1);");
            // A paged read: the position is moved back past the wrapper, into the statement as typed.
            let sets = go(l, &db, "t6_err", "SELECT * FROM nope_table", true);
            let err = sets[0].error.clone().unwrap();
            assert!(err.contains("42P01") && err.contains("Position: 15 (line 1, column 15)"), "{var}: {err}");
            let ex = crate::db::explain(&err, &Default::default());
            assert_eq!((ex.kind, ex.code.as_deref()), (crate::db::ErrKind::Missing, Some("42P01")), "{var}");
            assert!(ex.hint.contains("At position 15"), "{var}: {}", ex.hint);
            let sets = go(l, &db, "t6_err", "SELECT 1;\nSHOW nope_setting", true);
            let err = sets[1].error.clone().unwrap();
            assert!(err.contains("42704"), "{var}: {err}");
            let sets = go(l, &db, "t6_err", "SELEC 1", false);
            let err = sets[0].error.clone().unwrap();
            let ex = crate::db::explain(&err, &Default::default());
            assert_eq!((ex.kind, ex.code.as_deref()), (crate::db::ErrKind::Syntax, Some("42601")), "{var}: {err}");
            assert!(ex.hint.contains("syntax error") && ex.hint.contains("At position 1 (line 1, column 1)."), "{var}: {}", ex.hint);
            let sets = go(l, &db, "t6_err", "INSERT INTO t VALUES (1)", false);
            let err = sets[0].error.clone().unwrap();
            let ex = crate::db::explain(&err, &Default::default());
            assert_eq!((ex.kind, ex.dup), (crate::db::ErrKind::Duplicate, Some(("id".into(), "1".into()))), "{var}: {err}");
            assert!(ex.hint.contains("t_pkey"), "{var}: {}", ex.hint);
        });
    }
}
