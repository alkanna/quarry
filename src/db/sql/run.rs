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
    prepare_as(conn, d, schema, read_only, id, None).await.map(|_| ())
}

/// `prepare` with the pooler mode stated (`Db::pooler`) instead of read off the session. Returns whether the
/// connection is behind a pooler, where nothing is set on the session: the schema is a `SET LOCAL search_path` of
/// every transaction (`Db::pg_run_locals`) and write access a `BEGIN READ WRITE` of every statement.
pub(super) async fn prepare_as(conn: &mut Conn, d: Dialect, schema: &str, read_only: bool, id: &AtomicU64, pooled: Option<bool>) -> Res<bool> {
    if let Conn::Pg(c) = conn {
        // No USE: the database is the pool's, and the schema comes in as the search_path.
        let pooled = super::pg::prepare(c, read_only, id, pooled).await.map_err(e)?;
        if !pooled {
            super::pg_read::set_search_path(c, schema).await.map_err(e)?;
        }
        return Ok(pooled);
    }
    prepare_my(conn.my()?, d, schema, read_only, id).await.map_err(e)?;
    Ok(false)
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
    run_sent(conn, stmt, stmt, cap, 0, false).await.0
}

/// `run_one` for `sent` text that holds `stmt` after `shift` characters, so the position of a server error points
/// into the user's statement and not into the paging wrapper around it. With `drain` the rows past `cap` are read
/// and dropped instead of leaving the result on the wire, so a statement that writes runs to its end and its
/// transaction can finish. Also gives the code of the failure: the MySQL error number or the PostgreSQL SQLSTATE.
async fn run_sent(conn: &mut Conn, stmt: &str, sent: &str, cap: usize, shift: usize, drain: bool) -> (Vec<ResultSet>, Option<String>) {
    let mut code = None;
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
                        if drain {
                            continue;
                        }
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
                    // sqlx's code() is the SQLSTATE on both servers, and MySQL's own number is what tells its errors apart.
                    code = err.as_database_error().and_then(|d| match d.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>() {
                        Some(m) => Some(m.number().to_string()),
                        None => d.code().map(|c| c.to_string()),
                    });
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
    (sets, code)
}

/// One statement as a page of at most `RUN_ROWS_MAX` rows, when it is a plain read. A column name that
/// repeats cannot sit in a derived table, so such a statement falls back to a plain capped run.
pub(super) async fn run_stmt(conn: &mut Conn, d: super::Dialect, stmt: &str, page: usize) -> Vec<ResultSet> {
    if !lex::pageable(stmt, d) {
        return run_one(conn, stmt, RUN_ROWS_MAX).await;
    }
    let wrapped = lex::page_sql(stmt, page, RUN_ROWS_MAX);
    let (mut rs, code) = run_sent(conn, stmt, &wrapped, RUN_ROWS_MAX + 1, super::pg_read::PAGE_PREFIX, false).await;
    // ER_DUP_FIELDNAME, which a derived table raises and a plain run does not.
    if rs.len() == 1 && rs[0].error.is_some() && d != Dialect::Postgres && code.as_deref() == Some("1060") {
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

/// Why a script ends after a statement whose connection cannot be reused.
const CUT: &str = "Not run: the previous result was cut at the row limit";
const DEAD: &str = "Not run: the connection could not end the transaction of the previous statement";
pub(super) const POOLER_SESSION: &str = "Behind a connection pooler this statement would leave state on a server connection other clients share (a session SET, set_config, a temporary table, a named prepared statement, a held cursor or LISTEN). Use SET LOCAL, or connect without the pooler.";
const POOLER_TX: &str = "Transactions you open yourself are not available behind a connection pooler; each statement runs in its own transaction.";
/// Behind a pooler Stop cannot cancel by pid, because the backend may be serving another client by then.
const STOPPED_POOLED: &str = "Stopped. Behind a connection pooler the server is not told to cancel, so the statement may keep running until the statement timeout.";

/// Resolves once Stop has been pressed. Polled, because the flag is shared with the UI as a plain atomic.
async fn until_stopped(cancel: &AtomicBool) {
    while !cancel.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Runs the statements of `text`; `page` is the page every pageable statement is read at.
pub(super) async fn run(db: &Db, schema: &str, text: &str, read_only: bool, id: &AtomicU64, cancel: &AtomicBool, page: usize) -> Vec<ResultSet> {
    // Detached, so a USE or @variable of this script never leaks into the pool.
    let mut conn = match db.acquire_conn().await {
        Ok(c) => c,
        Err(err) => return vec![failed("", err)],
    };
    let d = db.dialect;
    // Behind a pooler nothing is set on the session, so the schema travels in the locals of every transaction.
    let (pooler, locals) = (db.pooler(), db.pg_run_locals(schema));
    if let Err(err) = prepare_as(&mut conn, d, schema, read_only, id, Some(pooler)).await {
        return vec![failed("", err)];
    }
    // PostgreSQL: the timeout the session got at startup, kept to be applied again where a session value would not hold.
    let mut timeout = String::new();
    if let Conn::Pg(c) = &mut conn {
        timeout = match super::pg_read::statement_timeout(c).await {
            Ok(t) => t,
            Err(err) => return vec![failed("", e(err))],
        };
        // An unlocked session runs as typed, so its limit is set once here and holds for every statement. Behind a
        // pooler a session value would leak, so each transaction sets it itself.
        if !read_only && !pooler {
            if let Err(err) = c.execute(format!("SET statement_timeout = '{timeout}'").as_str()).await {
                return vec![failed("", e(err))];
            }
        }
    }
    let mut sets = Vec::new();
    // Each statement goes alone, so user text is never sent as one multi-statement string.
    let stmts = lex::split_statements(text, d);
    let mut why: Option<&str> = None;
    let mut stopped = false;
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
        } else if pooler && lex::transaction_control(stmt, d) {
            // Each statement is wrapped in a transaction of Quarry's own, which the user's would end or nest in.
            sets.push(failed(stmt, POOLER_TX.into()));
            break;
        } else if pooler && lex::session_state(stmt, d) {
            sets.push(failed(stmt, POOLER_SESSION.into()));
            break;
        }
        let one = run_on(&mut conn, d, stmt, page, read_only, pooler, &timeout, &locals);
        let ran = if pooler {
            // No cancel by pid there: dropping the statement closes the connection, which ends its transaction, and
            // the server's statement_timeout bounds what it still does.
            tokio::select! {
                r = one => Some(r),
                _ = until_stopped(cancel) => None,
            }
        } else {
            Some(one.await)
        };
        let Some((rs, stop)) = ran else {
            stopped = true;
            why = Some("Stopped before running");
            sets.push(failed(stmt, STOPPED_POOLED.into()));
            continue;
        };
        let failed = rs.iter().find_map(|r| r.error.as_deref());
        crate::db::log_sql(db.conn, &here, stmt, rs.first().map_or(0, |r| r.ms), failed);
        if let (None, Some(to)) = (failed, lex::use_target(stmt, d)) {
            here = to;
        }
        if rs.iter().any(|r| r.error.is_some()) {
            sets.extend(rs);
            break;
        }
        // The rest of a big result is still on the wire, and draining it could take as long as the query, and
        // a fresh connection would lose the script's variables and USE, so the script ends here.
        why = stop;
        sets.extend(rs);
    }
    // The connection ran text of the user's, so it is closed and never reused: an advisory lock or a dblink
    // connection opened by a function the user called would outlive the rollback on a pooled one. After a Stop it is
    // only dropped, because the statement it was running is still on the wire.
    if let (Conn::Pg(_), false) = (&conn, stopped) {
        let _ = tokio::time::timeout(Duration::from_secs(2), conn.close()).await;
    }
    sets
}

/// One user statement on the script's connection, and why the script must end after it when the connection cannot be
/// used again (a result cut at the row limit, or a transaction that would not end).
#[allow(clippy::too_many_arguments)]
async fn run_on(conn: &mut Conn, d: Dialect, stmt: &str, page: usize, read_only: bool, pooler: bool, timeout: &str, locals: &str) -> (Vec<ResultSet>, Option<&'static str>) {
    if d != Dialect::Postgres {
        let rs = run_stmt(conn, d, stmt, page).await;
        let stop = rs.last().is_some_and(|r| r.capped).then_some(CUT);
        return (rs, stop);
    }
    if read_only {
        return run_locked(conn, d, stmt, page, timeout, locals).await;
    }
    if pooler {
        return run_write_pooled(conn, stmt, timeout, locals).await;
    }
    // Unlocked on a session of its own: as typed, with no paging or count wrapper that would change what the statement
    // does (a data-modifying CTE is refused inside one, and a side effect would run once per wrapper). A statement the
    // read-only check would refuse may write, so it is read to its end and its rows past the cap are dropped.
    let drain = lex::read_only_check(stmt, d).is_err();
    let rs = run_sent(conn, stmt, stmt, RUN_ROWS_MAX, 0, drain).await.0;
    let stop = (!drain && rs.last().is_some_and(|r| r.capped)).then_some(CUT);
    (rs, stop)
}

/// Ends the transaction of a statement. False when the connection could not, which leaves it inside one: the next
/// BEGIN would only warn, and two user statements would share a transaction.
async fn rollback(conn: &mut Conn, _stmt: &str) -> bool {
    #[cfg(test)]
    if tests_hook::fails(_stmt) {
        return false;
    }
    conn.execute("ROLLBACK").await.is_ok()
}

/// Test hook that makes the ROLLBACK after a marked statement fail, the way a connection that died would.
#[cfg(test)]
mod tests_hook {
    pub static MARK: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    pub fn fails(stmt: &str) -> bool {
        MARK.lock().unwrap().iter().any(|m| stmt.contains(m.as_str()))
    }
}

/// One user statement on a locked PostgreSQL connection: alone in `BEGIN READ ONLY` and always rolled back, never
/// committed, so nothing it does to the session outlives it and it never shares a transaction with another statement.
/// The paging wrapper and a read's count go through here too. The second value is why the script must end after the
/// statement: its result was cut at the row limit, or the ROLLBACK failed, and the connection is then closed.
pub(super) async fn run_locked(conn: &mut Conn, d: Dialect, stmt: &str, page: usize, timeout: &str, locals: &str) -> (Vec<ResultSet>, Option<&'static str>) {
    let Conn::Pg(c) = conn else { return (run_stmt(conn, d, stmt, page).await, None) };
    if let Err(err) = super::pg_read::begin_read_only(c, timeout, locals).await {
        return (vec![failed(stmt, super::pg_read::pg_error(&err, stmt, 0))], None);
    }
    let rs = run_stmt(conn, d, stmt, page).await;
    // A read cut at the row limit leaves the rest of the result on the wire. The script ends there and the
    // connection is closed, which ends the transaction without waiting for that result to drain.
    if rs.last().is_some_and(|r| r.capped) {
        return (rs, Some(CUT));
    }
    let stop = (!rollback(conn, stmt).await).then_some(DEAD);
    (rs, stop)
}

/// One unlocked statement behind a pooler, in a transaction of its own: `BEGIN READ WRITE`, the locals, the statement
/// and COMMIT, or ROLLBACK when it failed. Nothing is set on the session, so no other client of the pooler sees it. The
/// result is read to its end so the COMMIT can follow.
async fn run_write_pooled(conn: &mut Conn, stmt: &str, timeout: &str, locals: &str) -> (Vec<ResultSet>, Option<&'static str>) {
    let Conn::Pg(c) = conn else { return (run_one(conn, stmt, RUN_ROWS_MAX).await, None) };
    if let Err(err) = super::pg_read::begin_read_write(c, timeout, locals).await {
        return (vec![failed(stmt, super::pg_read::pg_error(&err, stmt, 0))], None);
    }
    let mut rs = run_sent(conn, stmt, stmt, RUN_ROWS_MAX, 0, true).await.0;
    if rs.iter().any(|r| r.error.is_some()) {
        let stop = (!rollback(conn, stmt).await).then_some(DEAD);
        return (rs, stop);
    }
    if let Err(x) = conn.execute("COMMIT").await {
        // A deferred constraint can fail here. The server has ended the transaction either way.
        let msg = super::pg_read::pg_error(&x, stmt, 0);
        match rs.last_mut() {
            Some(l) => l.error = Some(msg),
            None => rs.push(failed(stmt, msg)),
        }
    }
    (rs, None)
}

/// The row count of a paged statement on its own connection. PostgreSQL counts inside a rolled-back read-only
/// transaction like every other statement that carries user text.
pub(super) async fn count_rows(db: &Db, schema: &str, stmt: &str) -> Res<u64> {
    let mut conn = db.acquire_conn().await?;
    prepare_as(&mut conn, db.dialect, schema, true, &AtomicU64::new(0), Some(db.pooler())).await?;
    let sql = lex::count_sql(stmt);
    let t = Instant::now();
    let r = match &mut conn {
        Conn::Pg(c) => {
            let timeout = super::pg_read::statement_timeout(c).await.map_err(e)?;
            super::pg_read::begin_read_only(c, &timeout, &db.pg_run_locals(schema)).await.map_err(|x| super::pg_read::pg_error(&x, &sql, 0))?;
            let r = c.fetch_all(sql.as_str()).await.map(super::exec::SqlRow::wrap_pg);
            // The connection is closed below whatever happens, so a failed ROLLBACK cannot reach another statement.
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
    use crate::db::sql::exec::Cells;
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
                    let sets = run_locked(&mut conn, d, evil, 0, &timeout, "").await.0;
                    assert!(sets[0].error.is_none(), "{var}: {evil}: {:?}", sets[0].error);
                }
                let sets = run_locked(&mut conn, d, "SELECT current_setting('default_transaction_read_only') AS ro, current_setting('search_path') AS sp, current_setting('client_encoding') AS enc, current_setting('statement_timeout') AS st", 0, &timeout, "").await.0;
                assert_eq!((val(&sets[0], 0, "ro"), val(&sets[0], 0, "sp"), val(&sets[0], 0, "enc")), ("on".into(), "\"t6_cfg\"".into(), "UTF8".into()), "{var}");
                assert_eq!(val(&sets[0], 0, "st"), timeout, "{var}");
                // A write is refused by the server inside the transaction, whatever the lexer did.
                let sets = run_locked(&mut conn, d, "INSERT INTO t VALUES (1)", 0, &timeout, "").await.0;
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

    /// Rows of a count query that carry text unique to a test, for the query log of the server.
    fn activity(l: &LivePg, p: &sqlx::PgPool, like: &str) -> String {
        l.qone(p, &format!("SELECT count(*)::text FROM pg_stat_activity WHERE state = 'active' AND query LIKE '{like}' AND pid <> pg_backend_pid()"))
    }

    #[test]
    fn an_unlocked_data_modifying_cte_runs_and_returns_its_rows() {
        every_pg(|l, var| {
            let (db, p) = fixture(l, "t6f_cte", "CREATE TABLE t (a int, b text); INSERT INTO t VALUES (1, 'x'), (2, 'y');");
            let sets = go(l, &db, "t6f_cte", "WITH d AS (DELETE FROM t WHERE a = 1 RETURNING *) SELECT * FROM d", false);
            assert!(sets[0].error.is_none(), "{var}: {:?}", sets[0].error);
            assert_eq!((sets[0].rows.len(), val(&sets[0], 0, "b")), (1, "x".into()), "{var}");
            assert!(sets[0].page.is_none(), "{var}: no page, so no count is asked for");
            assert_eq!(l.qone(&p, "SELECT count(*)::text FROM t6f_cte.t"), "1", "{var}");
            // Locked, the lexer refuses it before it reaches the server.
            let sets = go(l, &db, "t6f_cte", "WITH d AS (DELETE FROM t WHERE a = 2 RETURNING *) SELECT * FROM d", true);
            assert!(sets[0].refused, "{var}");
            assert_eq!(l.qone(&p, "SELECT count(*)::text FROM t6f_cte.t"), "1", "{var}");
        });
    }

    #[test]
    fn an_unlocked_side_effect_runs_exactly_as_often_as_typed() {
        every_pg(|l, var| {
            let (db, p) = fixture(l, "t6f_seq", "CREATE SEQUENCE s; CREATE TABLE hits (n int); CREATE FUNCTION bump() RETURNS int LANGUAGE sql AS 'INSERT INTO hits VALUES (1) RETURNING 1';");
            // The rows past the cap are read and dropped, so the sequence advances by all 2500 and never by the 1001 a
            // LIMIT 1001 wrapper left behind.
            let sets = go(l, &db, "t6f_seq", "SELECT nextval('s') AS n FROM generate_series(1, 2500)", false);
            assert!(sets[0].error.is_none(), "{var}: {:?}", sets[0].error);
            assert_eq!((sets[0].rows.len(), sets[0].capped), (1000, true), "{var}");
            assert_eq!(l.qone(&p, "SELECT last_value::text FROM t6f_seq.s"), "2500", "{var}");
            // No page means no automatic count, which would run the statement again on a fresh connection.
            assert!(sets[0].page.is_none(), "{var}");
            let sets = go(l, &db, "t6f_seq", "SELECT bump(), set_config('t6f.x', 'y', false) FROM generate_series(1, 1500)", false);
            assert!(sets[0].error.is_none() && sets[0].page.is_none(), "{var}: {:?}", sets[0].error);
            assert_eq!(l.qone(&p, "SELECT count(*)::text FROM t6f_seq.hits"), "1500", "{var}: once per row, not once more for a count");
            // The set_config makes the lexer see a statement that may write, so its rows past the cap are read and dropped
            // and the script goes on, because that leaves the connection at the end of the result.
            let sets = go(l, &db, "t6f_seq", "SELECT bump(), set_config('t6f.x', 'y', false) FROM generate_series(1, 1200); SELECT count(*) AS n FROM hits", false);
            assert_eq!(val(&sets[1], 0, "n"), "2700", "{var}");
            // A plain read in an unlocked run still stops at the cap instead of reading everything.
            let sets = go(l, &db, "t6f_seq", "SELECT i FROM generate_series(1, 5000) i; SELECT 1 AS x", false);
            assert_eq!((sets[0].rows.len(), sets[0].capped, sets.len()), (1000, true, 2), "{var}");
            assert!(sets[1].error.as_deref().unwrap().contains("cut at the row limit"), "{var}");
        });
    }

    #[test]
    fn a_locked_run_still_pages() {
        every_pg(|l, var| {
            let (db, _p) = fixture(l, "t6f_lock", "");
            let sets = go(l, &db, "t6f_lock", "SELECT i FROM generate_series(1, 2500) i ORDER BY i", true);
            let page = sets[0].page.unwrap();
            assert_eq!((sets[0].rows.len(), page.next), (1000, true), "{var}");
        });
    }

    /// Runs `stmts` the way the pooler fallback does, on one connection that stays open, and gives it back so a test can
    /// look at the session the way the next client of that backend would.
    fn pooled_conn(l: &LivePg, db: &Db, schema: &str, read_only: bool) -> (Conn, String, String) {
        l.w.rt.block_on(async {
            let mut conn = db.acquire_conn().await.unwrap();
            prepare_as(&mut conn, Dialect::Postgres, schema, read_only, &AtomicU64::new(0), Some(true)).await.unwrap();
            let Conn::Pg(c) = &mut conn else { unreachable!() };
            let timeout = crate::db::sql::pg_read::statement_timeout(c).await.unwrap();
            (conn, timeout, db.pg_run_locals(schema))
        })
    }

    #[test]
    fn behind_a_pooler_a_run_leaves_the_session_as_the_server_made_it() {
        every_pg(|l, var| {
            let schema = "t6f \"q\" x";
            let (base, p) = fixture(l, "\"t6f \"\"q\"\" x\"", "CREATE TABLE things (id int PRIMARY KEY); INSERT INTO things VALUES (1);");
            let db = base.forced_pooler(&l.uri, 30_000);
            assert!(db.pooler() && !base.pooler(), "{var}");
            assert_eq!(db.pg_run_locals(""), db.pg_locals(), "{var}");
            assert!(db.pg_run_locals(schema).ends_with("SET LOCAL search_path = \"t6f \"\"q\"\" x\""), "{var}: {}", db.pg_run_locals(schema));
            assert_eq!(base.pg_run_locals(schema), "", "{var}: normal mode sets the search_path once per run");
            for read_only in [true, false] {
                let (mut conn, timeout, locals) = pooled_conn(l, &db, schema, read_only);
                let defaults: Vec<String> = ["search_path", "default_transaction_read_only", "statement_timeout"].iter().map(|n| l.one(&format!("SHOW {n}"))).collect();
                l.w.rt.block_on(async {
                    for stmt in ["SELECT count(*) AS n FROM things", "SELECT current_setting('search_path') AS sp, current_setting('transaction_read_only') AS ro, current_setting('statement_timeout') AS st"] {
                        let (sets, stop) = run_on(&mut conn, Dialect::Postgres, stmt, 0, read_only, true, &timeout, &locals).await;
                        assert!(sets[0].error.is_none() && stop.is_none(), "{var} read_only={read_only}: {:?}", sets[0].error);
                        if stmt.contains("search_path") {
                            // Inside the transaction the schema resolves; the read-only state follows the lock; the timeout is the one of the connection.
                            assert_eq!(val(&sets[0], 0, "sp"), "\"t6f \"\"q\"\" x\"", "{var}");
                            assert_eq!(val(&sets[0], 0, "ro"), if read_only { "on" } else { "off" }, "{var}");
                            assert_eq!(val(&sets[0], 0, "st"), "30s", "{var}");
                        } else {
                            assert_eq!(val(&sets[0], 0, "n"), "1", "{var}");
                        }
                    }
                    // The same backend, outside any transaction, as the next client of the pooler would find it.
                    let Conn::Pg(c) = &mut conn else { unreachable!() };
                    for (name, want) in ["search_path", "default_transaction_read_only", "statement_timeout"].iter().zip(&defaults) {
                        let rows = c.fetch_all(format!("SHOW {name}").as_str()).await.unwrap();
                        assert_eq!(&String::from_utf8_lossy(rows[0].cell(0).unwrap()), want, "{var} {name} read_only={read_only}");
                    }
                    let _ = conn.close().await;
                });
            }
            // Through the worker as well: a fresh connection afterwards shows the server's own settings.
            for read_only in [true, false] {
                let sets = go(l, &db, schema, "SELECT count(*) AS n FROM things", read_only);
                assert!(sets[0].error.is_none(), "{var}: {:?}", sets[0].error);
                assert_eq!(val(&sets[0], 0, "n"), "1", "{var}");
            }
            for name in ["search_path", "default_transaction_read_only"] {
                let fresh = l.w.rt.block_on(async {
                    use std::str::FromStr;
                    let mut c = <sqlx::PgConnection as sqlx::Connection>::connect_with(&sqlx::postgres::PgConnectOptions::from_str(&l.uri).unwrap().database("quarry_test")).await.unwrap();
                    let rows = c.fetch_all(format!("SHOW {name}").as_str()).await.unwrap();
                    String::from_utf8_lossy(rows[0].cell(0).unwrap()).into_owned()
                });
                assert_eq!(fresh, l.one(&format!("SHOW {name}")), "{var} {name}");
            }
            l.qsql(&p, "DROP SCHEMA \"t6f \"\"q\"\" x\" CASCADE");
        });
    }

    #[test]
    fn behind_a_pooler_an_unlocked_statement_commits_and_a_user_transaction_is_refused() {
        every_pg(|l, var| {
            let (base, p) = fixture(l, "t6f_pw", "CREATE TABLE t (id int PRIMARY KEY);");
            let db = base.forced_pooler(&l.uri, 30_000);
            let sets = go(l, &db, "t6f_pw", "INSERT INTO t VALUES (1); SELECT count(*) AS n FROM t", false);
            assert!(sets.iter().all(|s| s.error.is_none()), "{var}: {:?}", sets.iter().map(|s| &s.error).collect::<Vec<_>>());
            assert_eq!((sets[0].affected, val(&sets[1], 0, "n")), (Some(1), "1".into()), "{var}");
            assert_eq!(l.qone(&p, "SELECT count(*)::text FROM t6f_pw.t"), "1", "{var}: committed");
            // A failing statement rolls back and ends the script.
            let sets = go(l, &db, "t6f_pw", "INSERT INTO t VALUES (2), (2); INSERT INTO t VALUES (3)", false);
            assert!(sets[0].error.as_deref().unwrap().contains("23505") && sets.len() == 1, "{var}: {:?}", sets[0].error);
            assert_eq!(l.qone(&p, "SELECT count(*)::text FROM t6f_pw.t"), "1", "{var}");
            // A data-modifying CTE with more rows than the cap runs to its end and commits.
            let sets = go(l, &db, "t6f_pw", "WITH i AS (INSERT INTO t SELECT g FROM generate_series(10, 1509) g RETURNING id) SELECT id FROM i", false);
            assert!(sets[0].error.is_none() && sets[0].rows.len() == 1000, "{var}: {:?}", sets[0].error);
            assert_eq!(l.qone(&p, "SELECT count(*)::text FROM t6f_pw.t"), "1501", "{var}");
            let msg = "Transactions you open yourself are not available behind a connection pooler; each statement runs in its own transaction.";
            for tx in ["BEGIN", "START TRANSACTION", "COMMIT", "END", "ROLLBACK", "SAVEPOINT a", "RELEASE a", "PREPARE TRANSACTION 'x'"] {
                let sets = go(l, &db, "t6f_pw", &format!("{tx}; INSERT INTO t VALUES (99999)"), false);
                assert_eq!((sets.len(), sets[0].error.as_deref()), (1, Some(msg)), "{var}: {tx}");
            }
            assert_eq!(l.qone(&p, "SELECT count(*)::text FROM t6f_pw.t"), "1501", "{var}: nothing after the refusal ran");
            // Where startup options carry the session the user's own transactions still work.
            let sets = go(l, &base, "t6f_pw", "BEGIN; INSERT INTO t VALUES (77777); COMMIT", false);
            assert!(sets.iter().all(|s| s.error.is_none()), "{var}");
        });
    }

    #[test]
    fn behind_a_pooler_stop_closes_the_connection_and_does_not_cancel_by_pid() {
        every_pg(|l, var| {
            let (base, p) = fixture(l, "t6f_stop", "");
            let db = base.forced_pooler(&l.uri, 0);
            let h = RunHandle::default();
            let (id, cancel) = (h.conn.clone(), h.cancel.clone());
            l.w.sql_run(3, db.clone(), "t6f_stop".into(), "SELECT pg_sleep(6.25); SELECT 1".into(), false, 0, h);
            std::thread::sleep(Duration::from_millis(1500));
            // The pid of a pooled backend means nothing, so none is kept for Stop to cancel.
            assert_eq!(id.load(Ordering::SeqCst), 0, "{var}");
            assert_eq!(activity(l, &p, "SELECT pg_sleep(6.25)%"), "1", "{var}: the statement is running");
            let t = Instant::now();
            l.w.sql_kill(db.clone(), id.load(Ordering::SeqCst), cancel);
            let (mut ran, mut info) = (None, None);
            while ran.is_none() || info.is_none() {
                match l.next() {
                    Msg::SqlRan { sets, .. } => ran = Some(sets),
                    Msg::Info(r) => info = Some(r),
                    _ => {}
                }
            }
            assert!(info.unwrap().is_ok(), "{var}");
            assert!(t.elapsed() < Duration::from_secs(3), "{var}: {:?}", t.elapsed());
            let sets = ran.unwrap();
            assert!(sets[0].error.as_deref().unwrap().starts_with("Stopped."), "{var}: {:?}", sets[0].error);
            assert_eq!(sets[1].error.as_deref(), Some("Stopped before running"), "{var}");
            // pg_cancel_backend would have ended the statement at once; the backend is still sleeping, which is what
            // the statement timeout is left to bound.
            assert_eq!(activity(l, &p, "SELECT pg_sleep(6.25)%"), "1", "{var}: not cancelled");
            // Nor does a direct call to the cancel path reach out to the server.
            let msg = l.w.rt.block_on(kill_query(&db, 1)).unwrap();
            assert_eq!(msg, "Stopped the script", "{var}");
            assert_eq!(activity(l, &p, "SELECT pg_sleep(6.25)%"), "1", "{var}");
        });
    }

    #[test]
    fn a_failed_rollback_ends_the_script_and_the_connection() {
        every_pg(|l, var| {
            let (db, _p) = fixture(l, "t6f_rb", "");
            tests_hook::MARK.lock().unwrap().push("t6f_rollback_fails".into());
            let sets = go(l, &db, "t6f_rb", "SELECT 1 AS a; SELECT 2 AS t6f_rollback_fails; SELECT 3 AS c; SELECT 4 AS d", true);
            tests_hook::MARK.lock().unwrap().retain(|m| m != "t6f_rollback_fails");
            assert_eq!(sets.len(), 4, "{var}");
            assert!(sets[0].error.is_none() && sets[1].error.is_none(), "{var}");
            for s in &sets[2..] {
                let why = s.error.as_deref().unwrap();
                assert!(why.contains("could not end the transaction of the previous statement"), "{var}: {why}");
            }
            // The same through run_locked, which says why it stopped.
            l.w.rt.block_on(async {
                let mut conn = db.acquire_conn().await.unwrap();
                prepare(&mut conn, Dialect::Postgres, "t6f_rb", true, &AtomicU64::new(0)).await.unwrap();
                let Conn::Pg(c) = &mut conn else { unreachable!() };
                let timeout = crate::db::sql::pg_read::statement_timeout(c).await.unwrap();
                tests_hook::MARK.lock().unwrap().push("t6f_direct".into());
                let (_, stop) = run_locked(&mut conn, Dialect::Postgres, "SELECT 1 AS t6f_direct", 0, &timeout, "").await;
                tests_hook::MARK.lock().unwrap().retain(|m| m != "t6f_direct");
                assert_eq!(stop, Some(DEAD), "{var}");
                let (_, stop) = run_locked(&mut conn, Dialect::Postgres, "SELECT 1 AS fine", 0, &timeout, "").await;
                assert_eq!(stop, None, "{var}");
                let _ = conn.close().await;
            });
        });
    }
}
