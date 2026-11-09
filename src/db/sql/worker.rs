//! The SQL half of the worker: connecting, listing tables, paging a table and running Custom Query
//! scripts. Rows leave here as BSON like Mongo's, see `Row`.


use super::value::{self, Kind};
use super::schema::{Schema, SchemaCol, SchemaTable};
use super::{lex, Addr, Col, Db, Dialect, Page, PageQuery, Pool, ResultSet, SslMode, RUN_ROWS_MAX};
use crate::db::{e, Msg, Net, Res, Row, Worker};
use mongodb::bson::{Bson, Document};
use sqlx::mysql::{MySqlColumn, MySqlConnectOptions, MySqlPoolOptions, MySqlRow, MySqlSslMode};
use sqlx::{raw_sql, Column, Connection, Either, Executor, Row as _, TypeInfo};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Below this many rows the estimate in information_schema is too loose to show, and counting is cheap.
const ESTIMATE_MIN: u64 = 10_000;

/// A cell of a text-protocol row as the server's own bytes. Metadata queries read strings through this
/// too, because a `_bin` collation makes sqlx report text as VARBINARY.
pub(super) fn raw(row: &MySqlRow, i: usize) -> Option<&[u8]> {
    row.try_get_unchecked::<Option<&[u8]>, _>(i).ok().flatten()
}

pub(super) fn text(row: &MySqlRow, name: &str) -> Option<String> {
    row.try_get_unchecked::<Option<&[u8]>, _>(name).ok().flatten().map(|b| String::from_utf8_lossy(b).into_owned())
}

fn ssl_mode(m: SslMode) -> MySqlSslMode {
    match m {
        SslMode::Disabled => MySqlSslMode::Disabled,
        SslMode::Preferred => MySqlSslMode::Preferred,
        SslMode::Required => MySqlSslMode::Required,
        SslMode::VerifyCa => MySqlSslMode::VerifyCa,
        SslMode::VerifyIdentity => MySqlSslMode::VerifyIdentity,
    }
}

/// What every pool connection gets: read-only and the statement timeout.
async fn session(c: &mut sqlx::MySqlConnection, ms: u64) -> Result<(), sqlx::Error> {
    // The dialect is not known yet, so each connection asks for it to pick the session statements.
    let v: String = sqlx::query_scalar("SELECT VERSION()").fetch_one(&mut *c).await?;
    let d = Dialect::from_version(&v);
    c.execute(d.session_read_only(true)).await?;
    // Old servers and proxies may lack the variable; a missing timeout is better than no connection, but a
    // missing read-only guard is not, so only that one fails the connect.
    let t = d.session_timeout(ms);
    let _ = c.execute(t.as_str()).await;
    Ok(())
}

/// Readies the script's own connection: its id for Stop, the database, and write access when unlocked.
pub(super) async fn prepare(conn: &mut sqlx::MySqlConnection, d: Dialect, schema: &str, read_only: bool, id: &AtomicU64) -> Result<(), sqlx::Error> {
    if let Ok(r) = conn.fetch_one("SELECT CONNECTION_ID()").await {
        id.store(raw(&r, 0).and_then(|b| std::str::from_utf8(b).ok()?.parse().ok()).unwrap_or(0), Ordering::SeqCst);
    }
    if !schema.is_empty() {
        let q = format!("USE {}", d.quote_ident(schema));
        conn.execute(q.as_str()).await?;
    }
    if !read_only {
        conn.execute(d.session_read_only(false)).await?;
    }
    Ok(())
}

async fn open(uri: &str, timeout: Duration, net: &Net) -> Res<(Db, Vec<String>, Option<crate::ssh::Tunnel>)> {
    let a = Addr::parse(uri).ok_or("Not a MySQL address")?;
    let tunnel = match &net.ssh {
        Some((conf, secrets)) => Some(crate::ssh::open(conf, secrets, timeout, Some((a.host.clone(), a.port))).await?),
        None => None,
    };
    // The certificate names the real host, not the tunnel's 127.0.0.1, so the name check cannot pass.
    let (host, port, mode) = match &tunnel {
        Some(t) => ("127.0.0.1".to_string(), t.port, if a.ssl_mode == SslMode::VerifyIdentity { SslMode::VerifyCa } else { a.ssl_mode }),
        None => (a.host.clone(), a.port, a.ssl_mode),
    };
    // The sqlx defaults change how the server reads user SQL (|| as concat, UTC), so they are turned off.
    let mut o = MySqlConnectOptions::new().host(&host).port(port).username(&a.user).ssl_mode(ssl_mode(mode)).pipes_as_concat(false).no_engine_substitution(false).timezone(None);
    if !a.password.is_empty() {
        o = o.password(&a.password);
    }
    if !a.database.is_empty() {
        o = o.database(&a.database);
    }
    if !a.ssl_ca.is_empty() {
        o = o.ssl_ca(&a.ssl_ca);
    }
    let ms = timeout.as_millis() as u64;
    let pool = MySqlPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(timeout)
        .after_connect(move |c, _| Box::pin(session(c, ms)))
        .connect_with(o.clone())
        .await
        .map_err(e)?;
    let version: String = sqlx::query_scalar("SELECT VERSION()").fetch_one(&pool).await.map_err(e)?;
    let sql_mode: String = sqlx::query_scalar("SELECT @@sql_mode").fetch_one(&pool).await.unwrap_or_default();
    let tls = raw_sql("SHOW SESSION STATUS LIKE 'Ssl_cipher'").fetch_all(&pool).await.map_err(e)?.iter().any(|r| raw(r, 1).is_some_and(|v| !v.is_empty()));
    let mut dbs: Vec<String> = raw_sql("SHOW DATABASES").fetch_all(&pool).await.map_err(e)?.iter().filter_map(|r| raw(r, 0).map(|b| String::from_utf8_lossy(b).into_owned())).collect();
    dbs.sort();
    let db = Db { dialect: Dialect::from_version(&version), pool: Pool::My(pool), opts: o, version, tls, sql_mode };
    Ok((db, dbs, tunnel))
}

/// A table kind from information_schema's COLUMN_TYPE ("varchar(20)", "bigint unsigned"), which stays
/// right for `_bin` text columns that sqlx reports as binary.
fn kind_of_full(full: &str) -> Kind {
    Kind::from_column_type(full)
}

/// Names must be unique because a row is a BSON document keyed by them. `reserve_id` keeps a real
/// column from taking the `_id` key that carries the primary key.
fn unique(cols: &mut [Col], reserve_id: bool) {
    let mut seen = HashSet::new();
    if reserve_id {
        seen.insert("_id".to_string());
    }
    for c in cols {
        let base = c.name.clone();
        let mut n = 1;
        while !seen.insert(c.name.clone()) {
            n += 1;
            c.name = format!("{base}_{n}");
        }
    }
}

/// Columns as the result reports them. `meta` (name to COLUMN_TYPE and nullability) wins when the
/// result column is a table column.
pub(super) fn make_cols(cols: &[MySqlColumn], meta: &HashMap<String, (String, bool, String)>, reserve_id: bool) -> Vec<Col> {
    let mut out: Vec<Col> = cols
        .iter()
        .map(|c| {
            let ty = c.type_info().name().to_string();
            match meta.get(c.name()) {
                Some((full, nullable, extra)) => {
                    let extra = extra.to_ascii_uppercase();
                    Col { generated: extra.contains("GENERATED"), auto_increment: extra.contains("AUTO_INCREMENT"), ..Col::plain(c.name(), full, kind_of_full(full), *nullable) }
                }
                None => Col::plain(c.name(), &ty, Kind::from_type_name(&ty), true),
            }
        })
        .collect();
    unique(&mut out, reserve_id);
    out
}

pub(super) fn to_row(cols: &[Col], row: &MySqlRow, pk: &[String]) -> Res<Row> {
    let vals: Vec<Bson> = cols.iter().enumerate().map(|(i, c)| value::to_bson(c.kind, raw(row, i))).collect();
    let mut d = Document::new();
    // The key is looked up by the server's column names, since a renamed column keeps its real name there.
    // A key with a missing column would address too many rows, so then there is no key at all.
    let idx: Option<Vec<usize>> = pk.iter().map(|p| row.columns().iter().position(|c| c.name() == p)).collect();
    if let Some(idx) = idx.filter(|i| !i.is_empty()) {
        let mut k = Document::new();
        for (p, i) in pk.iter().zip(idx) {
            k.insert(p.clone(), vals[i].clone());
        }
        d.insert("_id", k);
    }
    for (c, v) in cols.iter().zip(vals) {
        d.insert(c.name.clone(), v);
    }
    Row::from_doc(&d)
}

/// Text the user typed is spliced into a SELECT, so it must stay one read-only statement.
fn one_read(sql: &str) -> Res<()> {
    if lex::split_statements(sql).len() != 1 {
        return Err("The filter and sort must not contain another statement".into());
    }
    lex::read_only_check(sql)
}

/// The count and whether it is the information_schema estimate rather than an exact count. A
/// grouped or distinct count that the server refuses (duplicate output names) falls back to
/// zero, since the data query reports the same problem.
async fn count(pool: &sqlx::MySqlPool, d: Dialect, schema: &str, table: &str, q: &PageQuery) -> Res<(u64, bool)> {
    if q.plain() {
        let mut c = pool.acquire().await.map_err(e)?;
        // MySQL 8 caches TABLE_ROWS for a day. Fresh numbers cost a lot per schema, so only this one table's
        // lookup asks for them; 5.7 and MariaDB lack the variable, so a refusal is fine.
        if d == Dialect::MySql {
            let _ = c.execute("SET SESSION information_schema_stats_expiry = 0").await;
        }
        let est = sqlx::query(d.table_status()).bind(schema).bind(table).fetch_optional(&mut *c).await.map_err(e)?;
        if let Some(n) = est.and_then(|r| r.try_get::<Option<u64>, _>("row_estimate").ok().flatten()).filter(|n| *n >= ESTIMATE_MIN) {
            return Ok((n, true));
        }
    }
    let sql = q.count_sql(d, schema, table);
    one_read(&sql)?;
    match raw_sql(&sql).fetch_one(pool).await {
        Ok(r) => Ok((raw(&r, 0).and_then(|b| std::str::from_utf8(b).ok()?.parse().ok()).unwrap_or(0), false)),
        Err(_) if q.grouped() || q.distinct => Ok((0, false)),
        Err(err) => Err(e(err)),
    }
}

/// Column types and the primary key, from information_schema.
async fn table_meta(pool: &sqlx::MySqlPool, d: Dialect, schema: &str, table: &str) -> Res<(HashMap<String, (String, bool, String)>, Vec<String>)> {
    let cols = sqlx::query(d.columns()).bind(schema).bind(table).fetch_all(pool).await.map_err(e)?;
    let idx = sqlx::query(d.indexes()).bind(schema).bind(table).fetch_all(pool).await.map_err(e)?;
    let meta = cols
        .iter()
        .filter_map(|r| {
            let nullable = r.try_get::<i64, _>("nullable").map(|n| n != 0).unwrap_or(true);
            Some((text(r, "name")?, (text(r, "full_type")?, nullable, text(r, "extra").unwrap_or_default())))
        })
        .collect();
    let pk = idx.iter().filter(|r| text(r, "name").as_deref() == Some("PRIMARY")).filter_map(|r| text(r, "col")).collect();
    Ok((meta, pk))
}

/// One page: columns, primary key, rows, the count of what the pages come from (groups when
/// grouped), whether that is an estimate, the time, and the rows before grouping when grouped.
type Paged = (Vec<Col>, Vec<String>, Vec<Row>, u64, bool, u128, Option<u64>);

async fn page(db: &Db, schema: &str, table: &str, q: &PageQuery, limit: u64, offset: u64) -> Res<Paged> {
    let start = Instant::now();
    let Pool::My(pool) = &db.pool;
    let d = db.dialect;
    // The key is needed before the data query: without a total order, pages overlap and skip rows.
    let (meta, mut pk) = table_meta(pool, d, schema, table).await?;
    if q.grouped() {
        pk.clear();
    }
    let sql = q.select_sql(d, schema, table, &pk, Some((limit, offset)));
    one_read(&sql)?;
    let data = async { raw_sql(&sql).fetch_all(pool).await.map_err(e) };
    let ((total, estimated), rows) = tokio::try_join!(count(pool, d, schema, table, q), data)?;
    // The LIMIT chip caps the whole result, not a page.
    let count = total.saturating_sub(q.skip).min(q.limit.unwrap_or(u64::MAX));
    let matched = if q.grouped() {
        let sql = q.rows_sql(d, schema, table);
        one_read(&sql)?;
        let r = raw_sql(&sql).fetch_one(pool).await.map_err(e)?;
        Some(raw(&r, 0).and_then(|b| std::str::from_utf8(b).ok()?.parse().ok()).unwrap_or(0))
    } else {
        None
    };
    let rcols: Vec<MySqlColumn> = match rows.first() {
        Some(r) => r.columns().to_vec(),
        // An empty page still needs its headers.
        None => (&*pool).describe(&sql).await.map_err(e)?.columns().to_vec(),
    };
    // An invisible primary key is in the index list but not in SELECT *, so the rows cannot carry it.
    if !pk.iter().all(|p| rcols.iter().any(|c| c.name() == p)) {
        pk.clear();
    }
    // Output columns of a grouped query are computed, so they have no table type to look up.
    let meta = if q.grouped() { HashMap::new() } else { meta };
    let cols = make_cols(&rcols, &meta, !pk.is_empty());
    let rows = rows.iter().map(|r| to_row(&cols, r, &pk)).collect::<Res<Vec<_>>>()?;
    Ok((cols, pk, rows, count, estimated, start.elapsed().as_millis(), matched))
}

fn failed(stmt: &str, error: String) -> ResultSet {
    ResultSet { error: Some(error), ..ResultSet::new(stmt) }
}

/// Runs one statement and keeps at most `cap` rows of each result. A CALL can return several
/// results, each becomes its own set. Reading stops at the cap, which leaves the connection mid-result:
/// the caller must not reuse it (`capped` on the last set says so).
async fn run_one(conn: &mut sqlx::MySqlConnection, stmt: &str, cap: usize) -> Vec<ResultSet> {
    let t = Instant::now();
    let mut sets: Vec<ResultSet> = Vec::new();
    let mut cur = ResultSet::new(stmt);
    let mut cols: Vec<Col> = Vec::new();
    let mut failure = None;
    {
        let mut s = raw_sql(stmt).fetch_many(&mut *conn);
        while let Some(item) = futures_util::StreamExt::next(&mut s).await {
            match item {
                Ok(Either::Right(row)) => {
                    if cols.is_empty() {
                        cols = make_cols(row.columns(), &HashMap::new(), false);
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
                Ok(Either::Left(r)) => {
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
        if let Ok(desc) = (&mut *conn).describe(stmt).await {
            if !desc.columns().is_empty() {
                sets[0].cols = make_cols(desc.columns(), &HashMap::new(), false);
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
async fn run_stmt(conn: &mut sqlx::MySqlConnection, stmt: &str, page: usize) -> Vec<ResultSet> {
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
async fn run(db: &Db, schema: &str, text: &str, read_only: bool, id: &AtomicU64, cancel: &AtomicBool, page: usize) -> Vec<ResultSet> {
    let Pool::My(pool) = &db.pool;
    // Detached, so a USE or @variable of this script never leaks into the pool.
    let mut conn = match pool.acquire().await {
        Ok(c) => c.detach(),
        Err(err) => return vec![failed("", e(err))],
    };
    let d = db.dialect;
    if let Err(err) = prepare(&mut conn, d, schema, read_only, id).await {
        return vec![failed("", e(err))];
    }
    let mut sets = Vec::new();
    // Each statement goes alone, so user text is never sent as one multi-statement string.
    let stmts = lex::split_statements(text);
    let mut why: Option<&str> = None;
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

/// KILL QUERY from a connection of its own. A plain function, because inside a spawned block the
/// executor lifetimes of a local connection do not infer.
async fn kill_query(db: &Db, id: u64) -> Res<String> {
    let mut c = tokio::time::timeout(Duration::from_secs(10), sqlx::MySqlConnection::connect_with(&db.opts)).await.map_err(|_| "Could not reach the server to stop the statement".to_string())?.map_err(e)?;
    let q = db.dialect.cancel(id);
    let r = c.execute(q.as_str()).await.map(|_| "Stopped the running statement".to_string()).map_err(e);
    let _ = c.close().await;
    r
}

/// The most rows one query of a shell script may return; past it the script gets an error instead of
/// the server's whole table through a pipe.
const RELAY_ROWS_MAX: usize = 100_000;
/// And the most bytes of values one answer may hold, so a few huge cells cannot fill memory or the pipe.
const RELAY_BYTES_MAX: usize = 64 << 20;

/// What a shell script's query gave back: rows as relaxed Extended JSON, or the effect of a write.
pub enum Answer {
    Rows(Vec<serde_json::Value>),
    Wrote { affected: u64, insert_id: u64 },
}

/// Where an unsafe JSON number would lose digits.
const JS_SAFE: u64 = (1 << 53) - 1;

/// A literal for one bound value. The script's text goes through the text protocol so rows convert
/// exactly like the Custom Query's, so values are escaped here instead of bound.
fn literal(p: &value::Param, no_backslash: bool) -> Res<String> {
    use value::Param;
    Ok(match p {
        Param::Null => "NULL".into(),
        Param::Int(i) => i.to_string(),
        Param::UInt(u) => u.to_string(),
        Param::Float(f) if f.is_finite() => format!("{f:e}"),
        Param::Float(_) => return Err("A value is not a finite number".into()),
        Param::Text(s) => {
            if s.contains('\0') {
                return Err("A text value contains a NUL character".into());
            }
            // The backslash only escapes when the server reads it as an escape.
            value::lit(s, !no_backslash)
        }
        Param::Bytes(b) => format!("X'{}'", value::hex_digits(b, false)),
    })
}

/// A row cell as the script gets it. An Int64 beyond 2^53 is sent as {"$numberLong"} because a plain
/// JSON number would round in the script.
fn cell(b: Bson) -> Bson {
    match b {
        Bson::Int64(i) if i.unsigned_abs() > JS_SAFE => mongodb::bson::doc! { "$numberLong": i.to_string() }.into(),
        b => b,
    }
}

/// The text with every `?` outside strings and comments replaced by the next value's literal. The text is
/// read with the server's quote rules, and refused when the other backslash reading finds other
/// placeholders or statements, since the server could then see code where this sees a string.
pub fn fill(text: &str, params: &[value::Param], backslash: bool, ansi: bool) -> Res<String> {
    let (at, count) = lex::placeholders(text, backslash, ansi);
    if lex::placeholders(text, !backslash, ansi) != (at, count) {
        return Err("This text reads differently with and without backslash escapes. Use ? for values and keep backslashes out of quotes".into());
    }
    let (mut out, mut last, mut n) = (String::new(), 0, 0);
    for (t, r) in lex::tokens_with(text, backslash, ansi) {
        if t == lex::Tok::Punct && &text[r.clone()] == "?" {
            let p = params.get(n).ok_or("The query has more ? than values")?;
            out.push_str(&text[last..r.start]);
            // Padded, so a literal never fuses with a word before or after it (X?, @?).
            out.push_str(&format!(" {} ", literal(p, !backslash)?));
            (last, n) = (r.end, n + 1);
        }
    }
    if n != params.len() {
        return Err(format!("The query has {n} placeholders but {} values", params.len()));
    }
    out.push_str(&text[last..]);
    Ok(out)
}

/// What Stop needs of a running Custom Query: the server connection id once it is known, and the flag
/// that keeps the rest of the script from starting. Each run gets a fresh one, so a late Stop never
/// reaches the next run.
#[derive(Clone, Default)]
pub struct RunHandle {
    pub conn: Arc<AtomicU64>,
    pub cancel: Arc<AtomicBool>,
}

/// A shell script's own connection, kept for the whole run so its @variables and transactions persist.
/// It is the only way a script reaches the server, so the read-only check below covers every write.
pub struct Relay {
    conn: sqlx::MySqlConnection,
    read_only: bool,
    backslash: bool,
    ansi: bool,
    /// A result was abandoned halfway, which leaves the connection unusable.
    dead: bool,
}

impl Relay {
    pub async fn open(db: &Db, schema: &str, read_only: bool, id: &AtomicU64) -> Res<Relay> {
        let Pool::My(pool) = &db.pool;
        let mut conn = pool.acquire().await.map_err(e)?.detach();
        prepare(&mut conn, db.dialect, schema, read_only, id).await.map_err(e)?;
        Ok(Relay { conn, read_only, backslash: db.backslash_escapes(), ansi: db.sql_mode.to_ascii_uppercase().contains("ANSI_QUOTES"), dead: false })
    }

    /// Interrupts the statement running on connection `id`, from a connection of its own.
    pub async fn kill(db: &Db, id: u64) -> Res<String> {
        kill_query(db, id).await
    }

    /// Runs one statement. On a locked connection the same check as the Custom Query runs on the final
    /// text, and the session itself is READ ONLY.
    pub async fn query(&mut self, text: &str, params: &[value::Param]) -> Res<Answer> {
        if self.dead {
            return Err("The connection of this script was lost after an earlier error".into());
        }
        if lex::changes_escaping(text) {
            return Err("This statement changes how values are escaped; not allowed in the shell".into());
        }
        if lex::uses_prepared(text) {
            return Err("Prepared statements are not available in the shell; use sql`...` with values instead.".into());
        }
        let stmt = fill(text, params, self.backslash, self.ansi)?;
        if lex::count_with(&stmt, self.backslash, self.ansi) != 1 || lex::count_with(&stmt, !self.backslash, self.ansi) != 1 {
            return Err("Send one statement per query".into());
        }
        if self.read_only {
            lex::read_only_check(&stmt)?;
        }
        let (mut cols, mut rows, mut wrote, mut bytes) = (Vec::<Col>::new(), Vec::new(), None, 0usize);
        let mut failure = None;
        {
            let mut s = raw_sql(&stmt).fetch_many(&mut self.conn);
            while let Some(item) = futures_util::StreamExt::next(&mut s).await {
                match item {
                    Ok(Either::Right(row)) => {
                        if cols.is_empty() {
                            cols = make_cols(row.columns(), &HashMap::new(), false);
                        }
                        if rows.len() >= RELAY_ROWS_MAX {
                            failure = Some(format!("A query returned more than {RELAY_ROWS_MAX} rows. Add a LIMIT."));
                            break;
                        }
                        bytes += (0..cols.len()).map(|i| raw(&row, i).map_or(0, |b| b.len()) + 16).sum::<usize>();
                        if bytes > RELAY_BYTES_MAX {
                            failure = Some(format!("A query returned more than {} MB. Select fewer columns or add a LIMIT.", RELAY_BYTES_MAX >> 20));
                            break;
                        }
                        let d: Document = cols.iter().enumerate().map(|(i, c)| (c.name.clone(), cell(value::to_bson(c.kind, raw(&row, i))))).collect();
                        rows.push(Bson::Document(d).into_relaxed_extjson());
                    }
                    // A CALL has several results; the first one with rows is what the script gets.
                    Ok(Either::Left(r)) if cols.is_empty() && wrote.is_none() => wrote = Some((r.rows_affected(), r.last_insert_id())),
                    Ok(Either::Left(_)) => {}
                    Err(err) => {
                        failure = Some(e(err));
                        break;
                    }
                }
            }
        }
        if let Some(f) = failure {
            // Dropping the stream mid-result leaves unread packets on the wire.
            self.dead = f.contains("more than");
            return Err(f);
        }
        if cols.is_empty() {
            // An empty SELECT still has headers, a write or SET does not.
            let empty_read = wrote.is_some_and(|w| w.0 == 0) && (&mut self.conn).describe(&stmt).await.is_ok_and(|d| !d.columns().is_empty());
            if !empty_read {
                let (affected, insert_id) = wrote.unwrap_or_default();
                return Ok(Answer::Wrote { affected, insert_id });
            }
        }
        Ok(Answer::Rows(rows))
    }
}

/// Tables, views and columns of one database in three round trips run together.
async fn load_schema(db: &Db, schema: &str) -> Res<Schema> {
    let Pool::My(pool) = &db.pool;
    let d = db.dialect;
    let (tables, cols, fks) = tokio::try_join!(
        async { sqlx::query(d.tables()).bind(schema).fetch_all(pool).await.map_err(e) },
        async { sqlx::query(d.all_columns()).bind(schema).fetch_all(pool).await.map_err(e) },
        async { sqlx::query(d.all_foreign_keys()).bind(schema).fetch_all(pool).await.map_err(e) },
    )?;
    let fk: HashSet<(String, String)> = fks.iter().filter_map(|r| Some((text(r, "tbl")?, text(r, "name")?))).collect();
    let mut out: Vec<SchemaTable> = tables
        .iter()
        .filter_map(|r| {
            let kind = text(r, "kind").unwrap_or_default();
            Some(SchemaTable { name: text(r, "name")?, view: kind.contains("VIEW"), rows: super::structure::num(r, "row_estimate"), cols: Vec::new() })
        })
        .collect();
    for r in &cols {
        let (Some(t), Some(name), Some(ty)) = (text(r, "tbl"), text(r, "name"), text(r, "full_type")) else { continue };
        let Some(table) = out.iter_mut().find(|x| x.name == t) else { continue };
        let nullable = r.try_get::<i64, _>("nullable").map(|n| n != 0).unwrap_or(true);
        table.cols.push(SchemaCol { fk: fk.contains(&(t, name.clone())), kind: kind_of_full(&ty), key: text(r, "col_key").unwrap_or_default(), nullable, name, ty });
    }
    Ok(Schema { tables: out })
}

/// Below this many rows a table is cheap enough to group in full while someone types.
const VALUES_SMALL: u64 = 100_000;

/// A column's most frequent values, or None where looking would scan a big table: the estimate is
/// large and no index leads with the column. Both checks read metadata only. The lookup runs on a
/// connection of its own with a 300 ms limit, so a slow one never holds the pool.
async fn field_values(db: &Db, schema: &str, table: &str, column: &str, prefix: Option<&str>) -> Res<Option<Vec<(String, u64)>>> {
    let Pool::My(pool) = &db.pool;
    let d = db.dialect;
    let Some(gate) = sqlx::query(d.value_gate()).bind(column).bind(schema).bind(table).fetch_optional(pool).await.map_err(e)? else { return Ok(None) };
    let small = super::structure::num(&gate, "est").is_some_and(|n| n < VALUES_SMALL);
    let leads = super::structure::num(&gate, "leads").unwrap_or(0) > 0;
    if !small && !leads {
        return Ok(None);
    }
    let mut c = tokio::time::timeout(Duration::from_secs(10), sqlx::MySqlConnection::connect_with(&db.opts)).await.map_err(|_| "Could not reach the server".to_string())?.map_err(e)?;
    let res = async {
        session(&mut c, 300).await.map_err(e)?;
        let sql = d.value_counts(schema, table, column, prefix.is_some(), 201);
        let mut q = sqlx::query(&sql);
        if let Some(p) = prefix {
            q = q.bind(format!("{}%", p.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")));
        }
        let rows = q.fetch_all(&mut c).await.map_err(e)?;
        Ok(Some(rows.iter().filter_map(|r| Some((text(r, "v")?, super::structure::num(r, "n")?))).collect()))
    }
    .await;
    let _ = c.close().await;
    res
}

impl Worker {
    /// Every table, view and column of `schema`, for autocomplete.
    pub fn sql_schema(&self, conn: u64, db: Db, schema: String) {
        self.spawn(async move {
            let res = load_schema(&db, &schema).await;
            Msg::SqlSchema { conn, schema, res }
        });
    }

    /// The most frequent values of a column, with `prefix` only those that start with it. Skipped
    /// (None) for big tables unless an index leads with the column.
    pub fn sql_field_values(&self, conn: u64, db: Db, schema: String, table: String, column: String, prefix: Option<String>) {
        self.spawn(async move {
            let res = field_values(&db, &schema, &table, &column, prefix.as_deref()).await;
            Msg::SqlValues { conn, schema, table, column, prefix, res }
        });
    }

    pub fn sql_connect(&self, conn: u64, uri: String, timeout: Duration, net: Net) {
        self.spawn(async move { Msg::SqlConnected { conn, res: open(&uri, timeout, &net).await } });
    }

    /// Connects and drops everything again, for the connection form.
    pub fn sql_test(&self, uri: String, timeout: Duration, net: Net) {
        self.spawn(async move {
            let start = Instant::now();
            let res = open(&uri, timeout, &net).await.map(|(db, _, t)| {
                let via = t.as_ref().map(|t| format!(" through SSH (host key {})", t.fingerprint)).unwrap_or_default();
                // Over a tunnel the certificate cannot match 127.0.0.1, so `open` checks the CA only.
                let note = if t.is_some() && Addr::parse(&uri).is_some_and(|a| a.ssl_mode == SslMode::VerifyIdentity) { " (TLS verified against the CA; the host name is the tunnel's)" } else { "" };
                format!("Connected{via} to {} in {} ms{note}", db.label(), start.elapsed().as_millis())
            });
            Msg::Tested { res }
        });
    }

    /// Tables sorted, and which of them are views.
    pub fn sql_tables(&self, conn: u64, db: Db, schema: String) {
        self.spawn(async move {
            let res = async {
                let Pool::My(pool) = &db.pool;
                let rows = sqlx::query(db.dialect.tables()).bind(&schema).fetch_all(pool).await.map_err(e)?;
                let mut names = Vec::new();
                let mut views = Vec::new();
                for r in &rows {
                    let Some(n) = text(r, "name") else { continue };
                    if text(r, "kind").is_some_and(|k| k.contains("VIEW")) {
                        views.push(n.clone());
                    }
                    names.push(n);
                }
                names.sort();
                Ok((names, views))
            }
            .await;
            Msg::Collections { conn, db: schema, res }
        });
    }

    /// One page of a table. The clauses are the text of the Content bar as the user typed them.
    /// Sends `SqlCols` and then `Docs`.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_page(&self, req: u64, db: Db, schema: String, table: String, q: PageQuery, limit: u64, offset: u64) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let res = match page(&db, &schema, &table, &q, limit, offset).await {
                Ok((cols, pk, rows, count, estimated, ms, matched)) => {
                    let _ = tx.send(Msg::SqlCols { req, cols, pk, estimated, matched });
                    Ok((rows, count, ms))
                }
                Err(err) => Err(err),
            };
            let _ = tx.send(Msg::Docs { req, res });
            ctx.request_repaint();
        });
    }

    /// Runs a Custom Query script on its own connection, whose id is stored in `h` as soon as it is
    /// known so `sql_kill` can stop it.
    pub fn sql_run(&self, req: u64, db: Db, schema: String, text: String, read_only: bool, page: usize, h: RunHandle) {
        self.spawn(async move {
            let sets = run(&db, &schema, &text, read_only, &h.conn, &h.cancel, page).await;
            // The server reuses connection ids, so a late Stop must not hit another session.
            h.conn.store(0, Ordering::SeqCst);
            Msg::SqlRan { req, sets }
        });
    }

    /// The row count of a paged statement, read in the background on a connection of its own under the
    /// statement timeout. `idx` is the result tab it belongs to.
    pub fn sql_count(&self, req: u64, idx: usize, db: Db, schema: String, stmt: String) {
        self.spawn(async move {
            let res = async {
                let Pool::My(pool) = &db.pool;
                let mut conn = pool.acquire().await.map_err(e)?.detach();
                prepare(&mut conn, db.dialect, &schema, true, &AtomicU64::new(0)).await.map_err(e)?;
                let r = conn.fetch_one(lex::count_sql(&stmt).as_str()).await.map_err(e)?;
                let n = raw(&r, 0).and_then(|b| std::str::from_utf8(b).ok()?.parse::<u64>().ok()).ok_or("The count was not a number")?;
                let _ = conn.close().await;
                Ok(n)
            }
            .await;
            Msg::SqlCount { req, idx, res }
        });
    }

    /// Stops a running script: sets `cancel` and interrupts the statement running on connection
    /// `connection_id`. It uses a connection of its own because the pool may be busy with paging.
    pub fn sql_kill(&self, db: Db, connection_id: u64, cancel: Arc<AtomicBool>) {
        self.spawn(async move {
            cancel.store(true, Ordering::SeqCst);
            // Zero means the script has not started a connection yet or already ended; the flag is enough.
            if connection_id == 0 {
                return Msg::Info(Ok("Stopped the script".to_string()));
            }
            let res = kill_query(&db, connection_id).await;
            Msg::Info(res)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Receiver;
    use std::sync::Mutex;

    /// Tests share one database and run in parallel, so the last fixture to go drops it again.
    static OPEN: Mutex<usize> = Mutex::new(0);

    /// The writable fixture pool. Keep it bound for the whole test: dropping it counts the test as finished.
    struct Fixture {
        pool: sqlx::MySqlPool,
        rt: tokio::runtime::Handle,
    }

    impl std::ops::Deref for Fixture {
        type Target = sqlx::MySqlPool;
        fn deref(&self) -> &sqlx::MySqlPool {
            &self.pool
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let mut n = OPEN.lock().unwrap_or_else(|e| e.into_inner());
            *n -= 1;
            if *n == 0 {
                // Dropping the database takes its views with it.
                let _ = self.rt.block_on(raw_sql("DROP DATABASE IF EXISTS quarry_test").execute(&self.pool));
            }
        }
    }

    /// Needs a server: QUARRY_TEST_MYSQL, like mysql://root:quarry@127.0.0.1:3306. Skipped when unset.
    /// Returns a worker, its channel, a connected Db whose queries time out after 30 s, and a plain
    /// writable pool for fixtures (the Db's own session is read-only).
    fn live(table: &str, ddl: &str) -> Option<(Worker, Receiver<Msg>, Db, Fixture)> {
        let uri = std::env::var("QUARRY_TEST_MYSQL").ok()?;
        *OPEN.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        let (tx, rx) = std::sync::mpsc::channel();
        let w = Worker::new(tx, eframe::egui::Context::default());
        let setup = w.rt.block_on(async {
            let p = sqlx::MySqlPool::connect(&uri).await.unwrap();
            raw_sql("CREATE DATABASE IF NOT EXISTS quarry_test").execute(&p).await.unwrap();
            if !table.is_empty() {
                raw_sql(&format!("DROP TABLE IF EXISTS quarry_test.{table}")).execute(&p).await.unwrap();
                raw_sql(&format!("CREATE TABLE quarry_test.{table} ({ddl})")).execute(&p).await.unwrap();
            }
            p
        });
        w.sql_connect(1, uri, Duration::from_secs(30), Net::default());
        let Msg::SqlConnected { res: Ok((db, dbs, _)), .. } = next(&rx) else { panic!("expected SqlConnected") };
        assert!(dbs.iter().any(|d| d == "quarry_test"), "{dbs:?}");
        let rt = w.rt.handle().clone();
        Some((w, rx, db, Fixture { pool: setup, rt }))
    }

    fn next(rx: &Receiver<Msg>) -> Msg {
        rx.recv_timeout(Duration::from_secs(15)).expect("worker answer")
    }

    fn exec(w: &Worker, p: &sqlx::MySqlPool, sql: &str) {
        w.rt.block_on(async { raw_sql(sql).execute(p).await.unwrap() });
    }

    fn page_of(w: &Worker, rx: &Receiver<Msg>, db: &Db, table: &str, filter: &str, order: &str, limit: u64, offset: u64) -> (Vec<Col>, Vec<String>, Vec<Document>, u64) {
        page_q(w, rx, db, table, PageQuery { filter: filter.into(), order: order.into(), ..Default::default() }, limit, offset)
    }

    fn page_q(w: &Worker, rx: &Receiver<Msg>, db: &Db, table: &str, q: PageQuery, limit: u64, offset: u64) -> (Vec<Col>, Vec<String>, Vec<Document>, u64) {
        w.sql_page(7, db.clone(), "quarry_test".into(), table.into(), q, limit, offset);
        let Msg::SqlCols { cols, pk, .. } = next(rx) else { panic!("expected SqlCols") };
        let Msg::Docs { res: Ok((rows, count, _)), .. } = next(rx) else { panic!("expected Docs") };
        (cols, pk, rows.iter().map(|r| r.decode().unwrap()).collect(), count)
    }

    #[test]
    fn live_page_clauses_select_distinct_group_having_and_index() {
        let Some((w, rx, db, p)) = live("t_clause", "id INT PRIMARY KEY, status VARCHAR(10), total INT, at DATETIME, KEY k_status (status)") else { return };
        let vals: Vec<String> = (0..20).map(|i| format!("({i}, '{}', {}, '2026-11-{:02} 10:00:00')", ["paid", "shipped", "open"][i % 3], i * 10, 1 + i % 4)).collect();
        exec(&w, &p, &format!("INSERT INTO quarry_test.t_clause VALUES {}", vals.join(",")));
        let base = PageQuery::default();
        // A select list: only those columns, and the key is gone with its column.
        let (cols, pk, rows, count) = page_q(&w, &rx, &db, "t_clause", PageQuery { select: vec!["status".into(), "total * 2 AS dbl".into()], order: "total".into(), ..base.clone() }, 5, 0);
        assert_eq!(cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["status", "dbl"]);
        assert!(pk.is_empty());
        assert_eq!((rows.len(), count), (5, 20));
        assert_eq!(rows[1].get_str("status"), Ok("shipped"));
        // DISTINCT counts distinct rows.
        let (_, _, rows, count) = page_q(&w, &rx, &db, "t_clause", PageQuery { select: vec!["status".into()], distinct: true, order: "status".into(), ..base.clone() }, 10, 0);
        assert_eq!((rows.len(), count), (3, 3));
        // LIMIT caps the whole result and OFFSET skips: 7 rows in 5 per page leave 2 on page 2.
        let (_, _, rows, count) = page_q(&w, &rx, &db, "t_clause", PageQuery { order: "id".into(), limit: Some(7), skip: 2, ..base.clone() }, 5, 5);
        assert_eq!((rows.len(), count), (2, 7));
        assert_eq!(rows[0].get_i64("id"), Ok(7));
        // GROUP BY with an aggregate and HAVING: counts are groups, rows have no key.
        let g = PageQuery { select: vec!["status".into(), "COUNT(*) AS n".into(), "SUM(total) AS sum_total".into()], group: vec!["status".into()], having: "n > 6".into(), order: "n DESC".into(), ..base.clone() };
        let (cols, pk, rows, count) = page_q(&w, &rx, &db, "t_clause", g.clone(), 10, 0);
        assert_eq!(cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["status", "n", "sum_total"]);
        assert!(pk.is_empty() && rows[0].get("_id").is_none());
        assert_eq!((rows.len(), count), (2, 2));
        assert_eq!(rows[0].get_str("status"), Ok("paid"));
        w.sql_page(8, db.clone(), "quarry_test".into(), "t_clause".into(), g, 10, 0);
        let Msg::SqlCols { matched, .. } = next(&rx) else { panic!("expected SqlCols") };
        assert_eq!(matched, Some(20));
        let _ = next(&rx);
        // A date grain groups by the day.
        let day = crate::db::sql::rules::Grain::Day.expr("at", db.dialect);
        let (_, _, rows, count) = page_q(&w, &rx, &db, "t_clause", PageQuery { select: vec![format!("{day} AS day"), "COUNT(*) AS n".into()], group: vec!["day".into()], order: "day".into(), ..base.clone() }, 10, 0);
        assert_eq!((rows.len(), count), (4, 4));
        // The index hint is quoted, and a bad name is the server's error, not an injection.
        let (_, _, rows, _) = page_q(&w, &rx, &db, "t_clause", PageQuery { filter: "status = 'paid'".into(), index_hint: "k_status".into(), ..base.clone() }, 100, 0);
        assert_eq!(rows.len(), 7);
        w.sql_page(9, db.clone(), "quarry_test".into(), "t_clause".into(), PageQuery { index_hint: "k) ; DROP TABLE t_clause; --".into(), ..base }, 10, 0);
        let Msg::Docs { res, .. } = next(&rx) else { panic!("expected Docs") };
        assert!(res.is_err());
        assert_eq!(page_of(&w, &rx, &db, "t_clause", "", "", 100, 0).3, 20);
    }

    fn run_all(w: &Worker, rx: &Receiver<Msg>, db: &Db, text: &str, read_only: bool) -> Vec<ResultSet> {
        w.sql_run(9, db.clone(), "quarry_test".into(), text.into(), read_only, 0, Default::default());
        let Msg::SqlRan { sets, .. } = next(rx) else { panic!("expected SqlRan") };
        sets
    }

    #[test]
    fn live_connect_reports_version_dialect_and_tls() {
        let Some((w, rx, db, _p)) = live("", "") else { return };
        println!("{} tls={} mode={}", db.label(), db.tls, db.sql_mode);
        assert!(db.version.chars().next().unwrap().is_ascii_digit());
        assert_eq!(db.dialect, Dialect::from_version(&db.version));
        assert!(db.label().starts_with("MySQL ") || db.label().starts_with("MariaDB "));
        let uri = std::env::var("QUARRY_TEST_MYSQL").unwrap();
        w.sql_test(uri, Duration::from_secs(10), Net::default());
        let Msg::Tested { res: Ok(m) } = next(&rx) else { panic!("expected Tested") };
        assert!(m.starts_with("Connected to ") && m.contains(&db.label()) && m.ends_with(" ms"), "{m}");
        w.sql_test("mysql://root:wrong@127.0.0.1:3306".into(), Duration::from_secs(10), Net::default());
        let Msg::Tested { res } = next(&rx) else { panic!("expected Tested") };
        assert!(res.is_err());
    }

    #[test]
    fn live_tables_lists_views_apart() {
        let Some((w, rx, db, p)) = live("t_list", "id INT PRIMARY KEY") else { return };
        exec(&w, &p, "DROP VIEW IF EXISTS quarry_test.v_list");
        exec(&w, &p, "CREATE VIEW quarry_test.v_list AS SELECT id FROM quarry_test.t_list");
        w.sql_tables(1, db, "quarry_test".into());
        let Msg::Collections { res: Ok((names, views)), .. } = next(&rx) else { panic!("expected Collections") };
        assert!(names.contains(&"t_list".to_string()) && names.contains(&"v_list".to_string()));
        assert!(views.contains(&"v_list".to_string()) && !views.contains(&"t_list".to_string()));
    }

    #[test]
    fn live_schema_lists_columns_keys_and_estimates() {
        let Some((w, rx, db, p)) = live("t_sch_parent", "id INT UNSIGNED PRIMARY KEY") else { return };
        exec(&w, &p, "DROP TABLE IF EXISTS quarry_test.t_sch_child");
        exec(&w, &p, "CREATE TABLE quarry_test.t_sch_child (id INT PRIMARY KEY, parent_id INT UNSIGNED NOT NULL, status ENUM('a','b'), note TEXT, KEY k_parent (parent_id), CONSTRAINT fk_parent FOREIGN KEY (parent_id) REFERENCES t_sch_parent (id))");
        exec(&w, &p, "DROP VIEW IF EXISTS quarry_test.v_sch");
        exec(&w, &p, "CREATE VIEW quarry_test.v_sch AS SELECT id FROM quarry_test.t_sch_parent");
        w.sql_schema(1, db, "quarry_test".into());
        let Msg::SqlSchema { schema, res: Ok(s), .. } = next(&rx) else { panic!("expected SqlSchema") };
        assert_eq!(schema, "quarry_test");
        let child = s.table("t_sch_child").unwrap();
        assert!(!child.view && child.rows.is_some());
        let col = |n: &str| child.col(n).unwrap();
        assert_eq!((col("id").key.as_str(), col("id").nullable, col("id").fk), ("PRI", false, false));
        assert_eq!((col("parent_id").ty.as_str(), col("parent_id").kind, col("parent_id").fk), ("int unsigned", Kind::UInt, true));
        assert_eq!((col("status").kind, col("status").nullable), (Kind::Enum, true));
        assert_eq!(crate::db::sql::schema::enum_values(&col("status").ty), ["a", "b"]);
        let view = s.table("v_sch").unwrap();
        assert!(view.view && view.rows.is_none() && view.cols.len() == 1);
        assert_eq!(child.cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "parent_id", "status", "note"]);
    }

    #[test]
    fn live_field_values_count_and_respect_the_gate() {
        let Some((w, rx, db, p)) = live("t_vals", "id INT PRIMARY KEY, status VARCHAR(10), n INT, note VARCHAR(10), KEY k_status (status)") else { return };
        let vals: Vec<String> = (0..10).map(|i| format!("({i}, '{}', {}, NULL)", if i < 6 { "paid" } else if i < 9 { "open" } else { "it''s" }, i % 2)).collect();
        exec(&w, &p, &format!("INSERT INTO quarry_test.t_vals VALUES {}", vals.join(",")));
        let ask = |col: &str, prefix: Option<&str>| {
            w.sql_field_values(1, db.clone(), "quarry_test".into(), "t_vals".into(), col.into(), prefix.map(str::to_string));
            let Msg::SqlValues { res, column, .. } = next(&rx) else { panic!("expected SqlValues") };
            assert_eq!(column, col);
            res
        };
        // Most frequent first, an integer column comes back as text, NULLs are left out.
        assert_eq!(ask("status", None).unwrap().unwrap(), [("paid".to_string(), 6), ("open".to_string(), 3), ("it's".to_string(), 1)]);
        assert_eq!(ask("n", None).unwrap().unwrap().len(), 2);
        assert!(ask("note", None).unwrap().unwrap().is_empty());
        assert_eq!(ask("status", Some("p")).unwrap().unwrap(), [("paid".to_string(), 6)]);
        // A wildcard typed by the user is a plain character.
        assert!(ask("status", Some("%")).unwrap().unwrap().is_empty());
        // A name that is not a column is the server's error, not an injection.
        assert!(ask("x` FROM t_vals; --", None).is_err());
        // Big estimate and no leading index: not looked up. The estimate is faked with a stats-free table of many rows.
        exec(&w, &p, "DROP TABLE IF EXISTS quarry_test.t_big");
        exec(&w, &p, "CREATE TABLE quarry_test.t_big (id INT PRIMARY KEY, a INT, b INT, KEY k_ab (a, b))");
        exec(&w, &p, "SET SESSION cte_max_recursion_depth = 200000; INSERT INTO quarry_test.t_big WITH RECURSIVE s(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM s WHERE i < 150000) SELECT i, i % 7, i % 5 FROM s");
        exec(&w, &p, "ANALYZE TABLE quarry_test.t_big");
        let big = |col: &str| {
            w.sql_field_values(1, db.clone(), "quarry_test".into(), "t_big".into(), col.into(), None);
            let Msg::SqlValues { res, .. } = next(&rx) else { panic!("expected SqlValues") };
            res
        };
        assert_eq!(big("b").unwrap(), None);
        assert_eq!(big("a").unwrap().unwrap().len(), 7);
    }

    #[test]
    fn live_page_filters_orders_and_carries_the_primary_key() {
        let Some((w, rx, db, p)) = live("t_page", "a INT, b VARCHAR(20) COLLATE utf8mb4_bin, n INT, PRIMARY KEY (b, a)") else { return };
        let vals: Vec<String> = (0..30).map(|i| format!("({i}, 'k{i}', {})", i % 3)).collect();
        exec(&w, &p, &format!("INSERT INTO quarry_test.t_page VALUES {}", vals.join(",")));
        let (cols, pk, rows, count) = page_of(&w, &rx, &db, "t_page", "n = 1", "a DESC -- trailing comment", 4, 2);
        assert_eq!(count, 10);
        assert_eq!(pk, ["b", "a"]);
        assert_eq!(cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["a", "b", "n"]);
        // A _bin text column is reported as binary by sqlx, the column's declared type keeps it text.
        assert_eq!(cols[1].kind, Kind::Text);
        assert_eq!(rows.iter().map(|d| d.get_i64("a").unwrap()).collect::<Vec<_>>(), [22, 19, 16, 13]);
        assert_eq!(rows[0].get_str("b"), Ok("k22"));
        assert_eq!(rows[0].get_document("_id").unwrap(), &mongodb::bson::doc! { "b": "k22", "a": 22_i64 });
        // An empty page still has its columns, and injection through the filter is refused.
        let (cols, _, rows, count) = page_of(&w, &rx, &db, "t_page", "n = 99", "", 10, 0);
        assert_eq!((cols.len(), rows.len(), count), (3, 0, 0));
        w.sql_page(8, db.clone(), "quarry_test".into(), "t_page".into(), PageQuery { filter: "1=1; DELETE FROM t_page".into(), ..Default::default() }, 10, 0);
        let Msg::Docs { res, .. } = next(&rx) else { panic!("expected Docs") };
        assert!(res.is_err());
        // The rows are all still there.
        assert_eq!(page_of(&w, &rx, &db, "t_page", "", "", 100, 0).3, 30);
    }

    #[test]
    fn live_every_column_type_converts() {
        let ddl = "id INT PRIMARY KEY, i INT, bu BIGINT UNSIGNED, bi BIGINT, d DECIMAL(10,2), f DOUBLE, v VARCHAR(20), t TEXT, b BLOB, dt DATE, dtt DATETIME, tm TIME, y YEAR, j JSON, e ENUM('a','b'), s SET('a','b','c'), bt BIT(8)";
        let Some((w, rx, db, p)) = live("t_types", ddl) else { return };
        exec(&w, &p, "INSERT INTO quarry_test.t_types VALUES (1, -5, 18446744073709551615, 9007199254740993, 12.50, 1.5, 'héllo', 'long text', 0x0001FF, '2024-02-29', '2024-02-29 13:14:15', '-12:34:56', 2024, '{\"a\": [1, 2]}', 'b', 'a,c', b'101')");
        exec(&w, &p, "INSERT INTO quarry_test.t_types (id) VALUES (2)");
        let (cols, _, rows, _) = page_of(&w, &rx, &db, "t_types", "", "id", 10, 0);
        let bin = |b: &[u8]| Bson::Binary(mongodb::bson::Binary { subtype: mongodb::bson::spec::BinarySubtype::Generic, bytes: b.to_vec() });
        let r = &rows[0];
        assert_eq!(r.get("i"), Some(&Bson::Int64(-5)));
        // Above i64::MAX stays exact as text.
        assert_eq!(r.get("bu"), Some(&Bson::String("18446744073709551615".into())));
        assert_eq!(r.get("bi"), Some(&Bson::Int64(9007199254740993)));
        assert_eq!(r.get("d"), Some(&Bson::String("12.50".into())));
        assert_eq!(r.get("f"), Some(&Bson::Double(1.5)));
        assert_eq!(r.get_str("v"), Ok("héllo"));
        assert_eq!(r.get_str("t"), Ok("long text"));
        assert_eq!(r.get("b"), Some(&bin(&[0, 1, 255])));
        assert_eq!(r.get_str("dt"), Ok("2024-02-29"));
        assert_eq!(r.get_str("dtt"), Ok("2024-02-29 13:14:15"));
        assert_eq!(r.get_str("tm"), Ok("-12:34:56"));
        assert_eq!(r.get("y"), Some(&Bson::Int64(2024)));
        assert!(r.get_str("j").unwrap().contains("\"a\""));
        assert_eq!((r.get_str("e"), r.get_str("s")), (Ok("b"), Ok("a,c")));
        assert_eq!(r.get("bt"), Some(&bin(&[5])));
        let n = &rows[1];
        for c in cols.iter().filter(|c| c.name != "id") {
            assert_eq!(n.get(&c.name), Some(&Bson::Null), "{}", c.name);
            assert!(c.nullable, "{}", c.name);
        }
        let kinds: Vec<Kind> = cols.iter().map(|c| c.kind).collect();
        // MariaDB stores JSON as LONGTEXT, so it is text there.
        let json = if db.dialect == Dialect::MariaDb { Kind::Text } else { Kind::Json };
        assert_eq!(kinds, [Kind::Int, Kind::Int, Kind::UInt, Kind::Int, Kind::Decimal, Kind::Float, Kind::Text, Kind::Text, Kind::Binary, Kind::Date, Kind::DateTime, Kind::Time, Kind::Year, json, Kind::Enum, Kind::Set, Kind::Bit]);
        assert!(!cols[0].nullable);
    }

    #[test]
    fn fill_binds_values_as_escaped_literals() {
        use value::Param;
        let f = |t: &str, p: &[Param], nb: bool| fill(t, p, !nb, false);
        assert_eq!(f("SELECT ? , '?', `?` -- ?\n, ?", &[Param::Int(1), Param::Null], false).unwrap(), "SELECT  1  , '?', `?` -- ?\n,  NULL ");
        assert_eq!(f("WHERE a = ?", &[Param::Text("it's \\ x".into())], false).unwrap(), "WHERE a =  'it''s \\\\ x' ");
        assert_eq!(f("WHERE a = ?", &[Param::Text("a\\".into())], true).unwrap(), "WHERE a =  'a\\' ");
        assert_eq!(f("?, ?", &[Param::Bytes(vec![0, 255]), Param::Float(1.5)], false).unwrap(), " X'00ff' ,  1.5e0 ");
        // A literal never fuses with the word next to it.
        assert_eq!(f("SELECT X?, a?, @?", &[Param::Int(1), Param::Int(2), Param::Int(3)], false).unwrap(), "SELECT X 1 , a 2 , @ 3 ");
        // A value that tries to end the string stays inside it, so the text is still one statement.
        let evil = f("SELECT ?", &[Param::Text("'; DROP TABLE t; --".into())], false).unwrap();
        assert_eq!(lex::split_statements(&evil).len(), 1);
        assert!(f("?", &[], false).is_err() && f("1", &[Param::Null], false).is_err());
        assert!(f("?", &[Param::Float(f64::NAN)], false).is_err() && f("?", &[Param::Text("a\0".into())], false).is_err());
    }

    /// Runs a shell script on a SQL relay, returning the queries it sent and its result.
    fn shell_script(w: &Worker, rx: &Receiver<Msg>, db: &Db, code: &str, tables: &[&str], read_only: bool) -> (Vec<crate::db::ShellQuery>, Res<String>) {
        let input = serde_json::json!({ "db": "quarry_test", "code": code, "tables": tables, "readOnly": read_only, "timeoutMs": 30000, "packages": [] }).to_string();
        let sql = Some(crate::db::SqlRelay { db: db.clone(), schema: "quarry_test".into(), read_only, conn_id: Default::default() });
        let job = crate::db::ShellJob { runtime: "bun".into(), dir: std::env::temp_dir().join("quarry-shell-live-test"), input, packages: Vec::new(), sql };
        let (_cancel, cancel_rx) = tokio::sync::oneshot::channel();
        w.shell(5, job, cancel_rx);
        let mut queries = Vec::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(180)).expect("shell answer") {
                Msg::ShellQuery { q, .. } => queries.push(q),
                Msg::ShellDone { res, .. } => return (queries, res),
                _ => {}
            }
        }
    }

    #[test]
    fn live_shell_runs_sql_tables_and_refuses_writes_when_locked() {
        let Some((w, rx, db, p)) = live("t_shell", "id INT PRIMARY KEY, status ENUM('paid','open') NOT NULL, note VARCHAR(20), total DECIMAL(8,2)") else { return };
        if std::process::Command::new("bun").arg("--version").output().is_err() {
            println!("skipped: bun is not installed");
            return;
        }
        exec(&w, &p, "INSERT INTO quarry_test.t_shell VALUES (1, 'paid', 'it''s', 9.50), (2, 'open', NULL, 3.25), (3, 'paid', NULL, 1.00)");
        let script = "async function run() {
  await sql`SET @x = 41`;
  const [{ y }] = await sql`SELECT @x + 1 AS y`;
  const rows = await sql`SELECT status, COUNT(*) AS n FROM t_shell WHERE id IN (${[1, 2, 3]}) AND (note = ${\"it's\"} OR note IS NULL) GROUP BY status ORDER BY status`;
  const found = await t_shell.find({ status: 'paid' }, { orderBy: '-id' });
  const missing = await t_shell.findOne({ id: 99 });
  const count = await t_shell.count({ total: { gt: 2 } });
  console.log('count', count);
  const refused = [];
  for (const f of [() => t_shell.delete({ id: 1 }), () => t_shell.insert({ id: 9, status: 'open' }), () => sql`UPDATE t_shell SET note = 'x'`, () => sql.unsafe('DROP TABLE t_shell')]) {
    try { await f(); refused.push(null) } catch (e) { refused.push(e.message) }
  }
  return { y, rows, found, missing, count, refused, after: await t_shell.count() };
}";
        let (queries, res) = shell_script(&w, &rx, &db, script, &["t_shell"], true);
        let v: serde_json::Value = serde_json::from_str(&res.expect("script result")).unwrap();
        let v = &v["v"];
        assert_eq!(v["y"], 42, "{v}");
        assert_eq!(v["rows"].as_array().unwrap().len(), 2);
        assert_eq!(v["found"].as_array().unwrap().iter().map(|r| r["id"].as_i64().unwrap()).collect::<Vec<_>>(), [3, 1]);
        // DECIMAL stays exact text.
        assert_eq!(v["found"][1]["total"], "9.50");
        assert!(v["missing"].is_null());
        assert_eq!(v["count"], 2);
        for r in v["refused"].as_array().unwrap() {
            assert!(r.as_str().is_some_and(|m| m.contains("read-only") || m.contains("not allowed") || m.contains("Unlock")), "{v}");
        }
        // Nothing was written, and the refused statements never reached the server.
        assert_eq!(v["after"], 3);
        assert!(queries.iter().any(|q| q.error.is_some()) && queries.iter().any(|q| q.rows == Some(2)));
        // Unlocked, the same table methods write.
        let (_, res) = shell_script(&w, &rx, &db, "async function run() { await t_shell.update({ id: 2 }, { note: 'new' }); await t_shell.insert([{ id: 4, status: 'open' }, { id: 5, status: 'open' }]); return await t_shell.count() }", &["t_shell"], false);
        assert_eq!(serde_json::from_str::<serde_json::Value>(&res.unwrap()).unwrap()["v"], 5);
    }

    #[test]
    fn live_run_several_statements_with_an_empty_result() {
        let Some((w, rx, db, p)) = live("t_run", "id INT PRIMARY KEY, name VARCHAR(10)") else { return };
        exec(&w, &p, "INSERT INTO quarry_test.t_run VALUES (1, 'a'), (2, 'b')");
        let sets = run_all(&w, &rx, &db, "SELECT * FROM t_run ORDER BY id; SELECT id, name FROM t_run WHERE id > 99; SELECT 1 + 1 AS two; SET @x = 5; SELECT @x AS x", true);
        assert_eq!(sets.len(), 5);
        assert_eq!((sets[0].rows.len(), sets[0].cols.len()), (2, 2));
        assert!(sets[1].rows.is_empty() && sets[1].affected.is_none() && sets[1].error.is_none());
        assert_eq!(sets[1].cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "name"]);
        assert_eq!(sets[2].rows[0].decode().unwrap().get_i64("two"), Ok(2));
        assert!(sets[3].cols.is_empty() && sets[3].affected == Some(0));
        // The user variable lives on the script's own connection.
        assert_eq!(sets[4].rows[0].decode().unwrap().get_i64("x"), Ok(5));
        // A failing statement stops the script and says why.
        let sets = run_all(&w, &rx, &db, "SELECT 1; SELECT * FROM missing_table; SELECT 3", true);
        assert_eq!(sets.len(), 2);
        assert!(sets[1].error.as_deref().is_some_and(|m| m.contains("missing_table")));
        // A long SELECT is read a page at a time, with a row more asked for to know another page follows.
        let sets = run_all(&w, &rx, &db, "SELECT 1 AS n FROM information_schema.columns a, information_schema.columns b LIMIT 1500", true);
        if sets[0].error.is_none() {
            assert_eq!((sets[0].rows.len(), sets[0].capped), (RUN_ROWS_MAX, false));
            assert_eq!(sets[0].page.map(|p| (p.n, p.next)), Some((0, true)));
        }
    }

    #[test]
    fn live_read_only_refuses_writes_but_runs_selects() {
        let Some((w, rx, db, p)) = live("t_ro", "id INT PRIMARY KEY") else { return };
        exec(&w, &p, "INSERT INTO quarry_test.t_ro VALUES (1)");
        let text = "SELECT COUNT(*) AS n FROM t_ro; DELETE FROM t_ro; SET SESSION transaction_read_only=0; /*!DELETE FROM t_ro*/; SELECT id FROM t_ro";
        let sets = run_all(&w, &rx, &db, text, true);
        assert_eq!(sets.len(), 5);
        assert!(sets[0].error.is_none() && sets[4].error.is_none());
        for i in [1, 2, 3] {
            assert!(sets[i].error.is_some(), "{}", sets[i].stmt);
        }
        assert_eq!(sets[4].rows.len(), 1);
        // Even a statement that slipped past the check would hit the read-only session.
        let sets = run_all(&w, &rx, &db, "SELECT 1", false);
        assert!(sets[0].error.is_none());
        let Pool::My(pool) = &db.pool;
        let n: i64 = w.rt.block_on(async { sqlx::query_scalar("SELECT COUNT(*) FROM quarry_test.t_ro").fetch_one(pool).await.unwrap() });
        assert_eq!(n, 1);
        // The unlocked run must not leave READ WRITE on a connection that went back to the pool. 1792 is
        // ER_CANT_EXECUTE_IN_READ_ONLY_TRANSACTION on MySQL and MariaDB alike.
        let err = w.rt.block_on(async { raw_sql("DELETE FROM quarry_test.t_ro").execute(pool).await.unwrap_err() });
        let code = err.as_database_error().and_then(|d| d.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>()).map(|d| d.number());
        assert_eq!(code, Some(1792), "{err}");
        // Unlocked, the same write runs.
        let sets = run_all(&w, &rx, &db, "DELETE FROM t_ro", false);
        assert_eq!(sets[0].affected, Some(1), "{:?}", sets[0].error);
    }

    #[test]
    fn live_kill_stops_a_running_statement() {
        let Some((w, rx, db, _p)) = live("", "") else { return };
        let h = RunHandle::default();
        let (id, cancel) = (h.conn.clone(), h.cancel.clone());
        w.sql_run(3, db.clone(), "quarry_test".into(), "SELECT SLEEP(10)".into(), true, 0, h);
        let start = Instant::now();
        while id.load(Ordering::SeqCst) == 0 {
            assert!(start.elapsed() < Duration::from_secs(5), "no connection id");
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(300));
        w.sql_kill(db, id.load(Ordering::SeqCst), cancel);
        let (mut ran, mut info) = (None, None);
        while ran.is_none() || info.is_none() {
            match next(&rx) {
                Msg::SqlRan { sets, .. } => ran = Some(sets),
                Msg::Info(r) => info = Some(r),
                _ => {}
            }
        }
        assert!(info.unwrap().is_ok());
        assert!(start.elapsed() < Duration::from_secs(6), "the sleep was not interrupted");
        assert!(ran.is_some());
        // A finished run resets the id so a late Stop cannot hit another session.
        assert_eq!(id.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn live_stop_keeps_later_statements_from_running() {
        let Some((w, rx, db, p)) = live("t_stop", "id INT PRIMARY KEY") else { return };
        exec(&w, &p, "INSERT INTO quarry_test.t_stop VALUES (1)");
        let h = RunHandle::default();
        let (id, cancel) = (h.conn.clone(), h.cancel.clone());
        // Unlocked, so the DELETE would really run if Stop only interrupted the sleep.
        w.sql_run(3, db.clone(), "quarry_test".into(), "SELECT SLEEP(5); DELETE FROM t_stop".into(), false, 0, h);
        let start = Instant::now();
        while id.load(Ordering::SeqCst) == 0 {
            assert!(start.elapsed() < Duration::from_secs(5), "no connection id");
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(300));
        w.sql_kill(db, id.load(Ordering::SeqCst), cancel);
        let sets = loop {
            if let Msg::SqlRan { sets, .. } = next(&rx) {
                break sets;
            }
        };
        // MySQL answers an interrupted SLEEP with 1, so the flag marks the DELETE; MariaDB fails the SLEEP itself.
        if sets.len() == 2 {
            assert!(sets[1].error.as_deref().is_some_and(|m| m.contains("Stopped before running")), "{:?}", sets[1].error);
        } else {
            assert!(sets[0].error.is_some());
        }
        let n: i64 = w.rt.block_on(async { sqlx::query_scalar("SELECT COUNT(*) FROM quarry_test.t_stop").fetch_one(&*p).await.unwrap() });
        assert_eq!(n, 1);
    }

    #[test]
    fn live_invisible_primary_key_is_not_a_usable_key() {
        let Some((w, rx, db, p)) = live("", "") else { return };
        let made = w.rt.block_on(async {
            let mut c = p.acquire().await.unwrap();
            raw_sql("DROP TABLE IF EXISTS quarry_test.t_inv").execute(&mut *c).await.unwrap();
            // Only MySQL 8.0.30 and later know this variable.
            if raw_sql("SET SESSION sql_generate_invisible_primary_key = ON").execute(&mut *c).await.is_err() {
                return false;
            }
            raw_sql("CREATE TABLE quarry_test.t_inv (a INT)").execute(&mut *c).await.unwrap();
            raw_sql("INSERT INTO quarry_test.t_inv VALUES (1), (2)").execute(&mut *c).await.unwrap();
            true
        });
        if !made {
            return;
        }
        let (cols, pk, rows, _) = page_of(&w, &rx, &db, "t_inv", "", "", 10, 0);
        assert!(pk.is_empty(), "{pk:?}");
        assert_eq!(cols.len(), 1);
        assert!(rows.iter().all(|d| !d.contains_key("_id")));
    }

    #[test]
    fn live_pages_are_stable_and_a_real_id_column_survives() {
        let Some((w, rx, db, p)) = live("t_stable", "_id INT, k INT, g INT, PRIMARY KEY (k)") else { return };
        let vals: Vec<String> = (0..20).map(|i| format!("({}, {i}, {})", 100 + i, i % 2)).collect();
        exec(&w, &p, &format!("INSERT INTO quarry_test.t_stable VALUES {}", vals.join(",")));
        // No order: the key orders it. A user order that ties gets the key as tie-breaker, so pages never overlap.
        let (_, _, first, _) = page_of(&w, &rx, &db, "t_stable", "", "g", 10, 0);
        let (cols, pk, second, _) = page_of(&w, &rx, &db, "t_stable", "", "g", 10, 10);
        let mut ks: Vec<i64> = first.iter().chain(&second).map(|d| d.get_i64("k").unwrap()).collect();
        ks.sort();
        assert_eq!(ks, (0..20).collect::<Vec<_>>());
        let (_, _, plain, _) = page_of(&w, &rx, &db, "t_stable", "", "", 3, 0);
        assert_eq!(plain.iter().map(|d| d.get_i64("k").unwrap()).collect::<Vec<_>>(), [0, 1, 2]);
        // The real _id column is renamed, the key keeps the _id slot.
        assert_eq!(pk, ["k"]);
        assert_eq!(cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["_id_2", "k", "g"]);
        assert_eq!(second[0].get_document("_id").unwrap(), &mongodb::bson::doc! { "k": second[0].get_i64("k").unwrap() });
        assert!(second[0].get_i64("_id_2").is_ok());
    }

    #[test]
    fn live_call_returns_each_result_set_apart() {
        let Some((w, rx, db, p)) = live("", "") else { return };
        exec(&w, &p, "DROP PROCEDURE IF EXISTS quarry_test.p_two");
        exec(&w, &p, "CREATE PROCEDURE quarry_test.p_two() BEGIN SELECT 1 AS a; SELECT 'x' AS b, 2 AS c; END");
        let sets = run_all(&w, &rx, &db, "CALL p_two(); SELECT 3 AS z", false);
        assert_eq!(sets.len(), 3, "{:?}", sets.iter().map(|s| (s.cols.len(), s.rows.len(), s.affected, s.error.clone())).collect::<Vec<_>>());
        assert_eq!(sets[0].cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["a"]);
        assert_eq!(sets[1].cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["b", "c"]);
        assert_eq!(sets[2].rows[0].decode().unwrap().get_i64("z"), Ok(3));
    }

    #[test]
    fn live_pages_follow_each_other_and_a_count_comes_apart() {
        let Some((w, rx, db, p)) = live("t_pages", "id INT PRIMARY KEY, v VARCHAR(5)") else { return };
        let vals: Vec<String> = (1..=2500).map(|i| format!("({i}, 'x')")).collect();
        exec(&w, &p, &format!("INSERT INTO quarry_test.t_pages VALUES {}", vals.join(",")));
        let stmt = "SELECT id -- the key\nFROM t_pages ORDER BY id DESC";
        let page = |n: usize| {
            w.sql_run(9, db.clone(), "quarry_test".into(), stmt.into(), true, n, Default::default());
            let Msg::SqlRan { mut sets, .. } = next(&rx) else { panic!("expected SqlRan") };
            sets.remove(0)
        };
        let (a, b, c) = (page(0), page(1), page(2));
        assert_eq!((a.rows.len(), b.rows.len(), c.rows.len()), (1000, 1000, 500));
        assert_eq!(a.stmt, stmt);
        assert_eq!((a.page.unwrap().next, b.page.unwrap().next, c.page.unwrap().next), (true, true, false));
        assert_eq!(a.rows[0].decode().unwrap().get_i64("id"), Ok(2500));
        assert_eq!(c.rows[0].decode().unwrap().get_i64("id"), Ok(500));
        assert!(a.page.unwrap().ordered);
        w.sql_count(4, 0, db.clone(), "quarry_test".into(), stmt.into());
        let Msg::SqlCount { req, idx, res } = next(&rx) else { panic!("expected SqlCount") };
        assert_eq!((req, idx, res), (4, 0, Ok(2500)));
        // A repeated column name cannot be wrapped, so it falls back to a plain run without pages.
        let dup = page_of_text(&w, &rx, &db, "SELECT 1 AS a, 2 AS a");
        assert!(dup.error.is_none() && dup.page.is_none() && dup.rows.len() == 1);
    }

    fn page_of_text(w: &Worker, rx: &Receiver<Msg>, db: &Db, text: &str) -> ResultSet {
        run_all(w, rx, db, text, true).remove(0)
    }

    #[test]
    fn live_export_streams_200k_rows_to_csv_and_json_one_batch_at_a_time() {
        use super::super::export::{export_to, Plan, Sink, BATCH};
        use crate::db::copy::Fmt;
        let Some((w, _rx, db, p)) = live("t_big", "id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(20)") else { return };
        exec(&w, &p, "INSERT INTO quarry_test.t_big (name) SELECT 'x' FROM information_schema.columns a, information_schema.columns b LIMIT 200000");
        let dir = std::env::temp_dir().join(format!("quarry-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stmt = "SELECT id, name FROM t_big ORDER BY id";
        let run = |fmt: Fmt, file: &str| {
            let plan = Plan { fmt, header: true, table: "t_big".into(), bs: true };
            let path = dir.join(file);
            let count = AtomicU64::new(0);
            let done = w.rt.block_on(export_to(&db, "quarry_test", stmt, &plan, Sink::File(path.clone()), &count, &AtomicBool::new(false), &AtomicU64::new(0))).unwrap();
            // The worker never held more than one batch, and the count the status bar reads is the whole result.
            assert!(done.peak_batch <= BATCH, "{}", done.peak_batch);
            assert_eq!((done.rows, count.load(Ordering::Relaxed)), (200_000, 200_000));
            // Written beside and renamed: no half file stays.
            assert!(!dir.join(format!(".{file}.part")).exists());
            path
        };
        let csv = std::fs::read_to_string(run(Fmt::Csv, "big.csv")).unwrap();
        assert_eq!((csv.lines().count(), csv.lines().next(), csv.lines().last()), (200_001, Some("id,name"), Some("200000,x")));
        let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(run(Fmt::Json, "big.json")).unwrap()).unwrap();
        assert_eq!((json.as_array().map(|a| a.len()), json[199_999]["id"].as_i64()), (Some(200_000), Some(200_000)));
        // A statement that writes is refused before it reaches the server, and leaves no file.
        let plan = Plan { fmt: Fmt::Csv, header: true, table: String::new(), bs: true };
        let refused = dir.join("never.csv");
        let err = w.rt.block_on(export_to(&db, "quarry_test", "DELETE FROM t_big", &plan, Sink::File(refused.clone()), &AtomicU64::new(0), &AtomicBool::new(false), &AtomicU64::new(0))).err().unwrap();
        assert!(err.contains("read"), "{err}");
        assert!(!refused.exists());
        let n: i64 = w.rt.block_on(async { sqlx::query_scalar("SELECT COUNT(*) FROM quarry_test.t_big").fetch_one(&p.pool).await.unwrap() });
        assert_eq!(n, 200_000);
        // An empty result still writes its header and footer.
        let empty = w.rt.block_on(export_to(&db, "quarry_test", "SELECT id FROM t_big WHERE id < 0", &Plan { fmt: Fmt::Json, header: true, table: String::new(), bs: true }, Sink::Clipboard, &AtomicU64::new(0), &AtomicBool::new(false), &AtomicU64::new(0))).unwrap();
        assert_eq!((empty.rows, empty.text.as_deref()), (0, Some("[]\n")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn live_a_capped_result_ends_the_script() {
        let Some((w, rx, db, _p)) = live("", "") else { return };
        exec(&w, &_p, "DROP PROCEDURE IF EXISTS quarry_test.p_big");
        exec(&w, &_p, "CREATE PROCEDURE quarry_test.p_big() BEGIN SELECT 1 AS n FROM information_schema.columns a, information_schema.columns b LIMIT 1500; END");
        let sets = run_all(&w, &rx, &db, "CALL p_big(); SELECT 2", false);
        assert_eq!(sets.len(), 2);
        assert_eq!((sets[0].rows.len(), sets[0].capped), (RUN_ROWS_MAX, true));
        assert!(sets[1].error.as_deref().is_some_and(|m| m.contains("row limit")), "{:?}", sets[1].error);
    }

    #[test]
    fn fill_refuses_text_that_reads_two_ways() {
        use value::Param;
        let evil = [Param::Text("x; DELETE FROM t; #".into())];
        let text = "SELECT '\\'' AS a, ? AS b -- '";
        // With backslash escapes the ? is inside a comment-free tail; without them the quote closes early.
        assert!(fill(text, &evil, true, false).is_err());
        assert!(fill(text, &evil, false, false).is_err());
        // A text both readings agree on still fills, under either mode.
        assert!(fill("SELECT ? AS b", &evil, true, false).is_ok() && fill("SELECT ? AS b", &evil, false, true).is_ok());
        // ANSI_QUOTES makes "?" an identifier, not a placeholder, which both backslash readings share.
        assert!(fill("SELECT \"a\" AS \"?\", ?", &evil, true, true).is_ok());
    }

    #[test]
    fn int_params_keep_every_digit() {
        use value::Param;
        let p = |j: serde_json::Value| crate::db::shell::sql_params_for_test(&j);
        assert_eq!(p(serde_json::json!([{ "$int": "9007199254740993" }, { "$int": "-9223372036854775808" }, { "$int": "18446744073709551615" }])).unwrap(), [Param::Int(9007199254740993), Param::Int(i64::MIN), Param::UInt(u64::MAX)]);
        assert!(p(serde_json::json!([{ "$int": "18446744073709551616" }])).is_err());
    }

    /// Needs Bun and a server. Shared by the live tests that run scripts the person could not trust.
    fn bun_ok() -> bool {
        std::process::Command::new("bun").arg("--version").output().is_ok()
    }

    #[test]
    fn live_shell_where_never_means_every_row_by_accident() {
        let Some((w, rx, db, p)) = live("t_where", "id BIGINT PRIMARY KEY, note VARCHAR(20)") else { return };
        if !bun_ok() {
            return;
        }
        exec(&w, &p, "INSERT INTO quarry_test.t_where VALUES (1, 'a'), (2, NULL), (9007199254740993, 'big')");
        let script = "async function run() {
  const bad = [5, null, [], false, 'x', { id: {} }, { id: undefined }, { id: { gt: undefined } }, { id: new Map() }];
  const out = [];
  for (const w of bad) {
    for (const f of [() => t_where.delete(w), () => t_where.update(w, { note: 'z' }), () => t_where.find(w), () => t_where.count(w)]) {
      try { await f(); out.push('ran') } catch (e) { out.push('refused') }
    }
  }
  const big = await t_where.findOne({ id: 9007199254740993n });
  let unsafe;
  try { await sql`SELECT ${2 ** 60}`; unsafe = 'ran' } catch (e) { unsafe = e.message }
  return { out, count: await t_where.count({}), big, unsafe, sum: (await sql`SELECT ${9223372036854775807n} AS v, ${18446744073709551615n} AS u`)[0] };
}";
        let (_, res) = shell_script(&w, &rx, &db, script, &["t_where"], false);
        let v: serde_json::Value = serde_json::from_str(&res.expect("script result")).unwrap();
        let v = &v["v"];
        assert!(v["out"].as_array().unwrap().iter().all(|x| x == "refused"), "{v}");
        assert_eq!(v["count"], 3, "{v}");
        assert_eq!(v["big"]["note"], "big");
        // The row id came back as an exact value, not a rounded number.
        assert_eq!(v["big"]["id"].to_string().trim_matches('"'), "9007199254740993", "{v}");
        assert!(v["unsafe"].as_str().unwrap().contains("BigInt"), "{v}");
        assert_eq!(v["sum"]["v"].to_string().trim_matches('"'), "9223372036854775807", "{v}");
        assert_eq!(v["sum"]["u"].to_string().trim_matches('"'), "18446744073709551615", "{v}");
    }

    #[test]
    fn live_shell_refuses_sets_that_move_the_escaping_even_when_unlocked() {
        let Some((w, rx, db, _p)) = live("", "") else { return };
        if !bun_ok() {
            return;
        }
        let script = "async function run() {
  const out = [];
  for (const t of ['SET NAMES utf8mb4', 'SET CHARACTER SET utf8mb4', 'SET CHARSET utf8mb4', 'SET SESSION character_set_client = 1', 'set @@character_set_connection = 1', \"SET sql_mode = ''\", \"SET @@session.sql_mode = ''\", \"SET @a = 1, @@sql_mode = ''\"]) {
    try { await sql.unsafe(t); out.push('ran') } catch (e) { out.push(e.message) }
  }
  await sql`SET @a = 1`;
  return out;
}";
        let (_, res) = shell_script(&w, &rx, &db, script, &[], false);
        let v: serde_json::Value = serde_json::from_str(&res.expect("script result")).unwrap();
        assert!(v["v"].as_array().unwrap().iter().all(|m| m.as_str().unwrap().contains("not allowed in the shell")), "{v}");
    }

    #[test]
    fn live_no_backslash_escapes_cannot_smuggle_a_statement() {
        let Some((w, rx, _db, p)) = live("t_nb", "id INT PRIMARY KEY") else { return };
        if !bun_ok() {
            return;
        }
        exec(&w, &p, "INSERT INTO quarry_test.t_nb VALUES (1)");
        let old: String = w.rt.block_on(async { sqlx::query_scalar("SELECT @@GLOBAL.sql_mode").fetch_one(&*p).await.unwrap() });
        exec(&w, &p, &format!("SET GLOBAL sql_mode = '{old},NO_BACKSLASH_ESCAPES'"));
        let uri = std::env::var("QUARRY_TEST_MYSQL").unwrap();
        w.sql_connect(2, uri, Duration::from_secs(30), Net::default());
        let got = next(&rx);
        // Restored before any assertion can fail, so a red test never leaves the server in this mode.
        exec(&w, &p, &format!("SET GLOBAL sql_mode = '{old}'"));
        let Msg::SqlConnected { res: Ok((db, _, _)), .. } = got else { panic!("expected SqlConnected") };
        assert!(db.sql_mode.contains("NO_BACKSLASH_ESCAPES"), "{}", db.sql_mode);
        exec(&w, &p, &format!("SET GLOBAL sql_mode = '{old},NO_BACKSLASH_ESCAPES'"));
        let script = "async function run() {
  let r;
  try { r = await sql.unsafe(\"SELECT '\\\\'' AS a, ? AS b -- '\", ['x; DELETE FROM t_nb; #']); r = 'ran' } catch (e) { r = e.message }
  return { r, left: await t_nb.count() };
}";
        let (_, res) = shell_script(&w, &rx, &db, script, &["t_nb"], false);
        exec(&w, &p, &format!("SET GLOBAL sql_mode = '{old}'"));
        let v: serde_json::Value = serde_json::from_str(&res.expect("script result")).unwrap();
        assert_eq!(v["v"]["left"], 1, "{v}");
        assert!(v["v"]["r"].as_str().unwrap().contains("differently"), "{v}");
    }
}
