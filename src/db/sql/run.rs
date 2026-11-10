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
    let t = Instant::now();
    let mut sets: Vec<ResultSet> = Vec::new();
    let mut cur = ResultSet::new(stmt);
    let mut cols: Vec<Col> = Vec::new();
    let mut failure = None;
    {
        let mut s = conn.fetch_text(stmt);
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
                    failure = Some(e(err));
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
        if let Ok(desc) = conn.describe(stmt).await {
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
pub(super) async fn run_stmt(conn: &mut Conn, stmt: &str, page: usize) -> Vec<ResultSet> {
    if !lex::pageable(stmt) {
        return run_one(conn, stmt, RUN_ROWS_MAX).await;
    }
    let mut rs = run_one(conn, &lex::page_sql(stmt, page, RUN_ROWS_MAX), RUN_ROWS_MAX + 1).await;
    if rs.len() == 1 && rs[0].error.as_deref().is_some_and(|m| m.contains("Duplicate column name")) {
        return run_one(conn, stmt, RUN_ROWS_MAX).await;
    }
    for r in &mut rs {
        r.stmt = stmt.to_string();
        if r.error.is_none() {
            let next = r.rows.len() > RUN_ROWS_MAX;
            r.rows.truncate(RUN_ROWS_MAX);
            r.page = Some(Page { n: page, next, total: None, ordered: lex::has_order_by(stmt) });
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
    let mut sets = Vec::new();
    // Each statement goes alone, so user text is never sent as one multi-statement string.
    let stmts = lex::split_statements(text);
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
            if let Err(why) = lex::read_only_check(stmt) {
                sets.push(ResultSet { refused: true, ..failed(stmt, why) });
                continue;
            }
        }
        let rs = run_stmt(&mut conn, stmt, page).await;
        let failed = rs.iter().find_map(|r| r.error.as_deref());
        crate::db::log_sql(db.conn, &here, stmt, rs.first().map_or(0, |r| r.ms), failed);
        if let (None, Some(to)) = (failed, lex::use_target(stmt)) {
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
    sets
}

/// A connection of its own, outside the pool (a busy or exhausted pool never blocks it), or `why` when the server
/// does not answer within 10 s.
pub(super) async fn side_conn(db: &Db, why: &str) -> Res<Conn> {
    tokio::time::timeout(Duration::from_secs(10), db.detached()).await.map_err(|_| why.to_string())?
}

/// KILL QUERY from a connection of its own. A plain function, because inside a spawned block the
/// executor lifetimes of a local connection do not infer.
pub(super) async fn kill_query(db: &Db, id: u64) -> Res<String> {
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
