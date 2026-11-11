//! SQL dumps: writing a database or some of its tables as a script (CREATE statements, then the rows as
//! batched INSERTs), and running a script file back. Both stream, so a dump of any size needs only a batch of
//! rows or one statement in memory. A dump is written next to its name as a hidden .part file and renamed at the
//! end, so a stopped or failed one leaves nothing behind.

use super::se;
use super::csv::Source;
use super::export::Done;
use super::import::{JobDone, Progress};
use super::lex::{self, Piece, ScriptSplitter};
use super::value::Kind;
use super::worker::{make_cols, pin_reads, raw, text, text_at, num_at, to_row};
use super::write::writer_conn;
use super::exec::Conn;
use super::{Db, Dialect};
use crate::db::copy::{CopyCol, Fmt, Writer};
use crate::db::{e, Msg, Res, Worker};
use mongodb::bson::Document;
use sqlx::{raw_sql, Connection, Executor, Row as _};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// One table or view of the dialog's list and what to write of it.
#[derive(Clone, Debug, PartialEq)]
pub struct DumpTable {
    pub name: String,
    pub view: bool,
    pub structure: bool,
    pub content: bool,
    /// Write a DROP before the CREATE, so running the file again replaces what is there.
    pub drop: bool,
}

#[derive(Clone, Debug)]
pub struct DumpPlan {
    pub schema: String,
    pub tables: Vec<DumpTable>,
    /// Start the file with CREATE DATABASE and USE, as a dump of a whole database does.
    pub create_db: bool,
    /// Write the triggers of the tables and the stored routines of the database.
    pub extras: bool,
    pub rows_per_insert: usize,
    /// Read every table from one consistent point in time.
    pub snapshot: bool,
    /// A WHERE clause for the rows of the only table, which then has no structure.
    pub filter: Option<String>,
}

/// An INSERT stops growing at this much text, so a table of large cells stays under max_allowed_packet.
pub(super) const INSERT_BYTES: usize = 1 << 20;
/// The mode every dump runs under: backslash escapes on and no ANSI quotes, which is what the values are written for.
const DUMP_MODE: &str = "NO_AUTO_VALUE_ON_ZERO";

fn io(x: std::io::Error) -> String {
    format!("Could not write: {x}")
}

pub(super) fn put(out: &mut dyn Write, s: &str) -> Res<()> {
    out.write_all(s.as_bytes()).map_err(io)
}

/// A string as a SQL literal for the statements a dump writes around its data (never a user value).
fn lit(s: &str) -> String {
    super::value::lit(s, true)
}

/// A name as it is written in a `--` comment line. A line break in a name would end the comment and turn the
/// rest of the name into a statement when the file is run (the class of CVE-2016-5483), so every control
/// character becomes a space.
pub(super) fn note(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// The order to create views in: a view after the views it selects from, otherwise as listed. Each entry is a
/// name and its CREATE statement. A reference is a backtick name after the `AS` of the statement, so a column
/// that shares a view's name may add an order that is not needed; a loop that this makes is broken in list
/// order instead of failing.
fn view_order(d: Dialect, views: &[(String, String)]) -> Vec<usize> {
    let deps: Vec<Vec<usize>> = views
        .iter()
        .enumerate()
        .map(|(i, (_, sql))| {
            let toks = lex::tokens(sql, d);
            let word = |k: usize, w: &str| toks[k].0 == lex::Tok::Word && sql[toks[k].1.clone()].eq_ignore_ascii_case(w);
            let view = (0..toks.len()).find(|&k| word(k, "VIEW")).unwrap_or(0);
            let body = (view..toks.len()).find(|&k| word(k, "AS")).map_or(0, |k| k + 1);
            let named: Vec<&str> = toks[body..].iter().filter(|t| t.0 == lex::Tok::Quoted).map(|t| &sql[t.1.clone()]).collect();
            (0..views.len()).filter(|&j| j != i && named.contains(&d.quote_ident(&views[j].0).as_str())).collect()
        })
        .collect();
    let (mut done, mut out) = (vec![false; views.len()], Vec::new());
    while out.len() < views.len() {
        let next = (0..views.len()).find(|&i| !done[i] && deps[i].iter().all(|&j| done[j])).or_else(|| (0..views.len()).find(|&i| !done[i])).unwrap_or(0);
        done[next] = true;
        out.push(next);
    }
    out
}

fn numeric(k: Kind) -> bool {
    matches!(k, Kind::Int | Kind::UInt | Kind::Float | Kind::Decimal)
}

pub(super) fn stopped(cancel: &AtomicBool) -> Res<()> {
    if cancel.load(Ordering::SeqCst) { Err("Export cancelled".into()) } else { Ok(()) }
}

/// `CREATE DATABASE IF NOT EXISTS` from the server's own statement, which carries the character set.
fn create_db_sql(server: &str) -> String {
    match server.strip_prefix("CREATE DATABASE") {
        Some(rest) if !rest.trim_start().starts_with("IF NOT EXISTS") => format!("CREATE DATABASE IF NOT EXISTS{rest}"),
        _ => server.to_string(),
    }
}

/// A column that the server computes: it cannot be given a value in an INSERT. A default written as an
/// expression (MySQL's DEFAULT_GENERATED) is a normal column.
fn is_generated(extra: &str) -> bool {
    let x = extra.to_ascii_uppercase();
    x.contains("VIRTUAL GENERATED") || x.contains("STORED GENERATED") || x.contains("VIRTUAL") || x.contains("PERSISTENT")
}

struct Cols {
    /// Names to select, without the computed columns.
    names: Vec<String>,
    meta: HashMap<String, (String, bool, String)>,
}

async fn table_cols(c: &mut sqlx::MySqlConnection, d: Dialect, schema: &str, table: &str) -> Res<Cols> {
    let rows = sqlx::query(d.columns()).bind(schema).bind(table).fetch_all(&mut *c).await.map_err(se)?;
    let (mut names, mut meta) = (Vec::new(), HashMap::new());
    for r in &rows {
        let (Some(name), Some(full)) = (text(r, "name"), text(r, "full_type")) else { continue };
        let extra = text(r, "extra").unwrap_or_default();
        let nullable = r.try_get::<i64, _>("nullable").map(|n| n != 0).unwrap_or(true);
        if !is_generated(&extra) {
            names.push(name.clone());
        }
        meta.insert(name, (full, nullable, extra));
    }
    if names.is_empty() {
        return Err(format!("{table} has no columns that can be dumped."));
    }
    Ok(Cols { names, meta })
}

/// One INSERT for the rows in `batch`, which is emptied. A plain function, since a closure held across the awaits
/// of the stream makes the future too general for the executor to be spawned.
fn flush_rows(d: Dialect, batch: &mut Vec<Document>, copy: &[CopyCol], table: &str, out: &mut (dyn Write + Send)) -> Res<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let mut w = Writer::new(d, Fmt::SqlInsert, true, copy.to_vec(), table, &[], true);
    let text = w.begin() + &w.rows(batch) + &w.end() + "\n";
    batch.clear();
    out.write_all(text.as_bytes()).map_err(io)
}

/// The rows of one table as INSERT statements, `per` rows each. Returns the rows written.
#[allow(clippy::too_many_arguments)]
async fn write_rows(c: &mut sqlx::MySqlConnection, d: Dialect, plan: &DumpPlan, table: &str, out: &mut (dyn Write + Send), count: &AtomicU64, base: u64, cancel: &AtomicBool) -> Res<u64> {
    let cols = table_cols(c, d, &plan.schema, table).await?;
    // The server writes a FLOAT as text with six digits, which reads back as another float. As a double it is
    // exact, and the float it is written back into rounds to the same value. `* 1e0` keeps -0, `+ 0e0` would not.
    let list = cols.names.iter().map(|n| if cols.meta.get(n).is_some_and(|m| m.0.to_ascii_lowercase().starts_with("float")) { format!("{0} * 1e0 AS {0}", d.quote_ident(n)) } else { d.quote_ident(n) }).collect::<Vec<_>>().join(", ");
    let mut sql = format!("SELECT {list} FROM {}", d.qualified(&plan.schema, table));
    if let Some(f) = plan.filter.as_ref().filter(|f| !f.trim().is_empty()) {
        // The clause is the user's text, so the whole statement must still be one statement that only reads.
        sql.push_str(&format!("\nWHERE (\n{}\n)", f.trim()));
        if lex::split_statements(&sql, d).len() != 1 {
            return Err("The filter must not contain another statement.".into());
        }
        lex::read_only_check(&sql, d)?;
    }
    let per = plan.rows_per_insert.max(1);
    let started = std::time::Instant::now();
    let (mut kinds, mut copy, mut batch, mut bytes, mut rows) = (Vec::new(), Vec::new(), Vec::<Document>::new(), 0usize, 0u64);
    let res: Res<()> = async {
        let mut s = raw_sql(&sql).fetch(&mut *c);
        while let Some(row) = futures_util::StreamExt::next(&mut s).await {
            let row = row.map_err(se)?;
            if kinds.is_empty() {
                let mc = make_cols(sqlx::Row::columns(&row), &cols.meta, false);
                copy = mc.iter().map(|c| CopyCol { name: c.name.clone(), src: c.src.clone(), numeric: numeric(c.kind) }).collect();
                kinds = mc;
            }
            bytes += (0..row.len()).map(|i| raw(&row, i).map_or(4, <[u8]>::len)).sum::<usize>();
            batch.push(to_row(&kinds, &row, &[])?.owned());
            rows += 1;
            if batch.len() >= per || bytes >= INSERT_BYTES {
                flush_rows(d, &mut batch, &copy, table, out)?;
                bytes = 0;
                count.store(base + rows, Ordering::Relaxed);
                stopped(cancel)?;
            }
        }
        Ok(())
    }
    .await;
    crate::db::note_here(&sql, started, &res);
    res?;
    flush_rows(d, &mut batch, &copy, table, out)?;
    count.store(base + rows, Ordering::Relaxed);
    Ok(rows)
}

/// The text of the statement `sql` (a SHOW CREATE) answers in column `at`, with the session's sql_mode of the
/// object in column 1 when it has one.
async fn show_create(c: &mut sqlx::MySqlConnection, sql: String, at: usize) -> Res<(Option<String>, String)> {
    // Read as a stream like the exports: `fetch_one` of a raw statement on a borrowed connection is refused by the
    // compiler inside a spawned task.
    let t = std::time::Instant::now();
    let mut s = raw_sql(&sql).fetch_many(&mut *c);
    let mut got = None;
    while let Some(item) = futures_util::StreamExt::next(&mut s).await {
        let item = match item {
            Ok(i) => i,
            Err(x) => {
                crate::db::note_here(&sql, t, &Err::<(), _>(&x));
                return Err(e(x));
            }
        };
        if let sqlx::Either::Right(r) = item {
            let mode = text_at(&r, 1).filter(|_| at == 2);
            got = Some((mode, text_at(&r, at).unwrap_or_default()));
        }
    }
    crate::db::note_here(&sql, t, &Ok::<(), String>(()));
    got.ok_or_else(|| "The server returned no definition.".to_string())
}

/// A statement that holds `;` inside its body, written between DELIMITER lines so it can be run back.
fn delimited(sql_mode: Option<&str>, stmt: &str) -> String {
    let set = |m: &str| format!("SET sql_mode = {};\n", lit(m));
    format!("{}DELIMITER ;;\n{stmt};;\nDELIMITER ;\n{}", sql_mode.map(set).unwrap_or_default(), if sql_mode.is_some() { set(DUMP_MODE) } else { String::new() })
}

/// Runs the whole dump on a read-only connection of its own.
pub async fn dump(db: &Db, plan: &DumpPlan, out: &mut (dyn Write + Send), count: &AtomicU64, cancel: &AtomicBool, id: &AtomicU64) -> Res<u64> {
    if db.dialect == Dialect::Postgres {
        return super::pg_dump::dump(db, plan, out, count, cancel, id).await;
    }
    let d = db.dialect;
    let mut conn = Conn::My(db.acquire().await?.detach());
    super::run::prepare(&mut conn, db, &plan.schema, true, id).await?;
    let mut c = conn.into_my()?;
    // The file says SET NAMES utf8mb4, so the values must be read as UTF-8, and every row of them.
    pin_reads(&mut c).await.map_err(se)?;
    // The statement timeout of the pool would cut the read of a big table short.
    let timeout = d.session_timeout(0);
    let _ = c.execute(timeout.as_str()).await;
    // Like mysqldump: SHOW CREATE writes names the way the session's sql_mode quotes them, so ANSI_QUOTES in the
    // server's mode would write "t", which the file cannot run back. A filter is the user's text, read under the
    // mode they typed it in, and a filtered dump writes no CREATE, so it keeps the mode.
    if plan.filter.is_none() {
        c.execute("SET SESSION sql_mode = '', SESSION sql_quote_show_create = 1").await.map_err(se)?;
    }
    // TIMESTAMP values are read in the session's zone; the file says which zone that is, so a restore in any
    // zone stores the same instants.
    c.execute("SET SESSION time_zone = '+00:00'").await.map_err(se)?;
    if plan.snapshot {
        c.execute("START TRANSACTION WITH CONSISTENT SNAPSHOT").await.map_err(se)?;
    }
    let r = dump_on(&mut c, d, db, plan, out, count, cancel).await;
    let _ = c.close().await;
    // A killed read comes back as a server error, so the flag says what really happened.
    match r {
        Err(_) if cancel.load(Ordering::SeqCst) => Err("Export cancelled".into()),
        r => r,
    }
}

async fn dump_on(c: &mut sqlx::MySqlConnection, d: Dialect, db: &Db, plan: &DumpPlan, out: &mut (dyn Write + Send), count: &AtomicU64, cancel: &AtomicBool) -> Res<u64> {
    let q = |n: &str| d.quote_ident(n);
    put(out, &format!("-- Quarry SQL dump\n-- Server: {}\n-- Database: {}\n\n", note(&db.label()), note(&plan.schema)))?;
    put(out, &format!("SET NAMES utf8mb4;\nSET @OLD_SQL_MODE=@@SQL_MODE, SQL_MODE={};\nSET @OLD_FOREIGN_KEY_CHECKS=@@FOREIGN_KEY_CHECKS, FOREIGN_KEY_CHECKS=0;\nSET @OLD_TIME_ZONE=@@TIME_ZONE, TIME_ZONE='+00:00';\n\n", lit(DUMP_MODE)))?;
    if plan.create_db {
        let (_, s) = show_create(c, format!("SHOW CREATE DATABASE {}", q(&plan.schema)), 1).await?;
        put(out, &format!("{};\nUSE {};\n\n", create_db_sql(&s), q(&plan.schema)))?;
    }
    let mut rows = 0u64;
    // The CREATE of every view is read first, so views can be written after the views they select from.
    // Collected first: an iterator with a closure alive across the awaits makes the future too general to spawn.
    let view_rows: Vec<&DumpTable> = plan.tables.iter().filter(|t| t.view).collect();
    let mut views: Vec<(String, String)> = Vec::new();
    for t in &view_rows {
        stopped(cancel)?;
        let s = if t.structure { show_create(c, d.create_stmt(true, &plan.schema, &t.name), 1).await?.1 } else { String::new() };
        views.push((t.name.clone(), s));
    }
    let order = view_order(d, &views);
    // Tables first, then views, which need them; data before triggers, which would otherwise fire on every row.
    let tables: Vec<&DumpTable> = plan.tables.iter().filter(|t| !t.view).chain(order.iter().map(|&i| view_rows[i])).collect();
    for t in &tables {
        stopped(cancel)?;
        if !t.structure && !t.content {
            continue;
        }
        put(out, &format!("--\n-- {} {}\n--\n\n", if t.view { "View" } else { "Table" }, note(&t.name)))?;
        if t.drop {
            put(out, &format!("DROP {} IF EXISTS {};\n", if t.view { "VIEW" } else { "TABLE" }, q(&t.name)))?;
        }
        if t.structure && t.view {
            let s = views.iter().find(|v| v.0 == t.name).map(|v| v.1.as_str()).unwrap_or_default();
            put(out, &format!("{s};\n"))?;
        } else if t.structure {
            let (_, s) = show_create(c, d.create_stmt(false, &plan.schema, &t.name), 1).await?;
            put(out, &format!("{s};\n"))?;
        }
        if t.content && !t.view {
            put(out, "\n")?;
            rows += write_rows(c, d, plan, &t.name, out, count, rows, cancel).await?;
        }
        put(out, "\n")?;
    }
    if plan.extras {
        // Collected first: an iterator with a closure alive across the awaits below makes the future too general to spawn.
        let with_triggers: Vec<&DumpTable> = tables.iter().copied().filter(|t| t.structure && !t.view).collect();
        for t in with_triggers {
            let names: Vec<String> = sqlx::query("SELECT TRIGGER_NAME AS name FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = ? AND EVENT_OBJECT_TABLE = ? ORDER BY TRIGGER_NAME").bind(&plan.schema).bind(&t.name).fetch_all(&mut *c).await.map_err(se)?.iter().filter_map(|r| text(r, "name")).collect();
            for n in names {
                stopped(cancel)?;
                let (mode, stmt) = show_create(c, format!("SHOW CREATE TRIGGER {}", d.qualified(&plan.schema, &n)), 2).await?;
                put(out, &format!("--\n-- Trigger {}\n--\n\nDROP TRIGGER IF EXISTS {};\n{}\n", note(&n), q(&n), delimited(mode.as_deref(), &stmt)))?;
            }
        }
        let routines: Vec<(String, String)> = sqlx::query("SELECT ROUTINE_NAME AS name, ROUTINE_TYPE AS kind FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = ? ORDER BY ROUTINE_TYPE, ROUTINE_NAME").bind(&plan.schema).fetch_all(&mut *c).await.map_err(se)?.iter().filter_map(|r| Some((text(r, "name")?, text(r, "kind")?.to_ascii_uppercase()))).collect();
        for (n, kind) in routines {
            stopped(cancel)?;
            let kind = if kind == "FUNCTION" { "FUNCTION" } else { "PROCEDURE" };
            let (mode, stmt) = show_create(c, format!("SHOW CREATE {kind} {}", d.qualified(&plan.schema, &n)), 2).await?;
            if stmt.is_empty() {
                put(out, &format!("-- {kind} {} was left out: the account may not read its definition.\n\n", note(&n)))?;
                continue;
            }
            put(out, &format!("--\n-- {kind} {}\n--\n\nDROP {kind} IF EXISTS {};\n{}\n", note(&n), q(&n), delimited(mode.as_deref(), &stmt)))?;
        }
    }
    put(out, "SET TIME_ZONE=@OLD_TIME_ZONE;\nSET FOREIGN_KEY_CHECKS=@OLD_FOREIGN_KEY_CHECKS;\nSET SQL_MODE=@OLD_SQL_MODE;\n-- Dump complete\n")?;
    Ok(rows)
}

fn part_of(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!(".{name}.part"))
}

/// Writes the dump beside `path` and renames it at the end.
pub async fn dump_to(db: &Db, plan: &DumpPlan, path: PathBuf, count: &AtomicU64, cancel: &AtomicBool, id: &AtomicU64) -> Res<Done> {
    let tmp = part_of(&path);
    let res = async {
        let file = std::fs::File::create(&tmp).map_err(|x| format!("Could not write {}: {x}", path.display()))?;
        let out = std::io::BufWriter::new(file);
        let fail = |x: std::io::Error| format!("Could not write {}: {x}", path.display());
        // The name is the only switch, so the dialog's checkbox and a typed .gz name agree.
        let rows = if path.extension().is_some_and(|x| x == "gz") {
            let mut gz = flate2::write::GzEncoder::new(out, flate2::Compression::default());
            let rows = dump(db, plan, &mut gz, count, cancel, id).await?;
            gz.finish().and_then(|mut b| b.flush()).map_err(fail)?;
            rows
        } else {
            let mut out = out;
            let rows = dump(db, plan, &mut out, count, cancel, id).await?;
            out.flush().map_err(fail)?;
            rows
        };
        std::fs::rename(&tmp, &path).map_err(|x| format!("Could not write {}: {x}", path.display()))?;
        Ok(rows)
    }
    .await;
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res.map(|rows| Done { rows, #[cfg(test)] peak_batch: 0, path: Some(path), files: 1, text: None })
}

/// The statement as the error message shows it.
pub(super) fn excerpt(s: &[u8]) -> String {
    let s = String::from_utf8_lossy(&s[..s.len().min(2400)]);
    let t: String = s.chars().take(600).collect();
    if s.chars().count() > 600 { format!("{t}...") } else { t }
}

/// How many statements of a script file the query log lists one by one.
pub(super) const FILE_LOGGED: u64 = 20;

#[derive(Debug)]
pub struct Ran {
    pub statements: u64,
}

/// Runs a script file statement by statement on a connection of its own, without a transaction: a dump holds
/// DDL, which commits as it goes. It stops at the first error and says which statement it was. One function
/// with one owned connection: split in two, the compiler cannot prove the future is Send inside a spawned task.
#[allow(clippy::too_many_arguments)]
pub async fn run_file(db: Db, schema: String, src: Source, read_only: bool, prog: Arc<Progress>, cancel: Arc<AtomicBool>, id: Arc<AtomicU64>) -> Res<Ran> {
    if db.dialect == Dialect::Postgres {
        return super::pg_dump::run_file(db, schema, src, read_only, prog, cancel, id).await;
    }
    let mut c = writer_conn(&db, &schema, read_only).await?.into_my()?;
    if let Ok(r) = c.fetch_one("SELECT CONNECTION_ID()").await {
        id.store(num_at(&r, 0).unwrap_or(0), Ordering::SeqCst);
    }
    let timeout = db.dialect.session_timeout(0);
    let _ = c.execute(timeout.as_str()).await;
    let mut file = src.open().map_err(|x| format!("Could not read the file: {x}"))?;
    let mut sp = ScriptSplitter::new(mode_of(&mut c).await);
    if let Some(max) = max_packet(&mut c).await {
        sp.set_max(max);
    }
    let mut buf = vec![0u8; 256 << 10];
    let (mut n, mut eof) = (0u64, false);
    let res: Res<Ran> = 'run: {
        loop {
            let piece = match sp.next_piece() {
                Ok(p) => p,
                Err(x) => break 'run Err(format!("Statement {}: {x}", n + 1)),
            };
            match piece {
                Some(Piece::Delimiter(_)) => {}
                Some(Piece::Stmt(stmt)) => {
                    if cancel.load(Ordering::SeqCst) {
                        break 'run Err("Cancelled.".into());
                    }
                    n += 1;
                    let t = std::time::Instant::now();
                    let outcome = run_stmt(&mut c, &stmt).await;
                    // A dump holds thousands of statements that would push everything else out of the log, so
                    // the first few and any that fail are listed, and the rest come as one line at the end.
                    if n <= FILE_LOGGED || outcome.is_err() {
                        crate::db::note_here(&String::from_utf8_lossy(&stmt[..stmt.len().min(16_000)]), t, &outcome);
                    }
                    if let Err(x) = outcome {
                        break 'run Err(format!("Statement {n} failed: {x}

{}

Statements before it ran and stay applied.", excerpt(&stmt)));
                    }
                    // A SET of sql_mode changes how the rest of the file must be read.
                    if lex::changes_escaping(&String::from_utf8_lossy(&stmt), Dialect::MySql) {
                        let bs = mode_of(&mut c).await;
                        sp.set_backslash(bs);
                    }
                    prog.rows.store(n, Ordering::Relaxed);
                    prog.bytes.store(sp.offset(), Ordering::Relaxed);
                }
                None if eof => break,
                None => {
                    // Checked here too: a statement that never ends (an unclosed quote) is all reading, no running.
                    if cancel.load(Ordering::SeqCst) {
                        break 'run Err("Cancelled.".into());
                    }
                    match file.read(&mut buf) {
                        Ok(0) => {
                            eof = true;
                            sp.finish();
                        }
                        Ok(k) => sp.feed(&buf[..k]),
                        Err(x) => break 'run Err(format!("Could not read the file: {x}")),
                    }
                }
            }
        }
        prog.bytes.store(sp.offset(), Ordering::Relaxed);
        if n > FILE_LOGGED {
            crate::db::note_here(&format!("-- {n} statements run from a file, the first {FILE_LOGGED} are listed"), std::time::Instant::now(), &Ok::<(), String>(()));
        }
        Ok(Ran { statements: n })
    };
    let _ = c.close().await;
    match res {
        Err(x) if cancel.load(Ordering::SeqCst) => Err(format!("Run stopped. Statements that already ran stay applied.
{x}")),
        r => r,
    }
}

/// Runs one statement and reads its results to the end. The text protocol of the driver only takes UTF-8, but a
/// dump may hold binary values written raw (mysqldump without --hex-blob, or a file in latin1). Such a statement
/// goes to the server as a bound value instead, which arrives byte for byte, and runs from there as a prepared
/// statement: a binary string is not converted to the connection's character set, so the server reads exactly
/// the bytes of the file, as the mysql client would send them.
async fn run_stmt(c: &mut sqlx::MySqlConnection, stmt: &[u8]) -> Result<(), String> {
    if let Ok(text) = std::str::from_utf8(stmt) {
        return run_text(c, text).await.map_err(se);
    }
    sqlx::query("SET @quarry_stmt = ?").bind(stmt).execute(&mut *c).await.map_err(se)?;
    let mut r = run_text(c, "PREPARE quarry_stmt FROM @quarry_stmt").await;
    if r.is_ok() {
        r = run_text(c, "EXECUTE quarry_stmt").await;
        let _ = run_text(c, "DEALLOCATE PREPARE quarry_stmt").await;
    }
    let _ = run_text(c, "SET @quarry_stmt = NULL").await;
    r.map_err(|x| format!("{} (The statement holds bytes that are not UTF-8, so it ran as a prepared statement.)", e(x)))
}

/// Runs one statement as text and reads its results to the end.
async fn run_text(c: &mut sqlx::MySqlConnection, stmt: &str) -> Result<(), sqlx::Error> {
    let mut s = raw_sql(stmt).fetch_many(&mut *c);
    while let Some(item) = futures_util::StreamExt::next(&mut s).await {
        item?;
    }
    Ok(())
}

/// The longest statement the server takes, read once per file.
async fn max_packet(c: &mut sqlx::MySqlConnection) -> Option<usize> {
    let mut s = raw_sql("SELECT @@max_allowed_packet").fetch_many(&mut *c);
    let mut max = None;
    while let Some(item) = futures_util::StreamExt::next(&mut s).await {
        if let Ok(sqlx::Either::Right(r)) = item {
            max = num_at(&r, 0).map(|n| n as usize);
        }
    }
    max
}

async fn mode_of(c: &mut sqlx::MySqlConnection) -> bool {
    let mut s = raw_sql("SELECT @@sql_mode").fetch_many(&mut *c);
    let mut mode = String::new();
    while let Some(item) = futures_util::StreamExt::next(&mut s).await {
        if let Ok(sqlx::Either::Right(r)) = item {
            mode = raw(&r, 0).map(|b| String::from_utf8_lossy(b).to_ascii_uppercase()).unwrap_or_default();
        }
    }
    !mode.contains("NO_BACKSLASH_ESCAPES")
}

impl Worker {
    /// Writes a dump to `path`, answering with `SqlExport` like the other exports.
    pub fn sql_dump(&self, req: u64, db: Db, plan: DumpPlan, path: PathBuf, count: Arc<AtomicU64>, id: Arc<AtomicU64>, cancel: Arc<AtomicBool>) {
        self.spawn_sql(db.conn, &plan.schema.clone(), async move {
            let res = dump_to(&db, &plan, path, &count, &cancel, &id).await;
            id.store(0, Ordering::SeqCst);
            Msg::SqlExport { req, res }
        });
    }

    /// Runs a script file, answering with `SqlJob`.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_run_file(&self, req: u64, db: Db, schema: String, src: Source, read_only: bool, prog: Arc<Progress>, id: Arc<AtomicU64>, cancel: Arc<AtomicBool>) {
        let database = db.pg_database();
        self.spawn_sql(db.conn, &schema.clone(), async move {
            let res = run_file(db, schema.clone(), src, read_only, prog, cancel, id.clone()).await;
            id.store(0, Ordering::SeqCst);
            let res = res.map(|r| JobDone { msg: format!("Ran {} statements", r.statements), undo: None, schema: schema.clone(), database, table: None, tables_changed: true });
            Msg::SqlJob { req, res }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_database_gains_if_not_exists_once() {
        assert_eq!(create_db_sql("CREATE DATABASE `shop` /*!40100 DEFAULT CHARACTER SET utf8mb4 */"), "CREATE DATABASE IF NOT EXISTS `shop` /*!40100 DEFAULT CHARACTER SET utf8mb4 */");
        assert_eq!(create_db_sql("CREATE DATABASE IF NOT EXISTS `shop`"), "CREATE DATABASE IF NOT EXISTS `shop`");
    }

    #[test]
    fn routines_and_triggers_are_written_between_delimiter_lines() {
        let s = delimited(Some("ANSI_QUOTES,STRICT_TRANS_TABLES"), "CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END");
        assert_eq!(s, "SET sql_mode = 'ANSI_QUOTES,STRICT_TRANS_TABLES';\nDELIMITER ;;\nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END;;\nDELIMITER ;\nSET sql_mode = 'NO_AUTO_VALUE_ON_ZERO';\n");
        // What was written reads back as exactly its statements.
        let mut sp = ScriptSplitter::new(true);
        sp.feed(s.as_bytes());
        sp.finish();
        let mut got = Vec::new();
        while let Some(p) = sp.next_piece().unwrap() {
            got.push(p);
        }
        assert_eq!(got.len(), 5);
        assert_eq!(got[2], Piece::Stmt("CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END".into()));
        assert_eq!(delimited(None, "CREATE TRIGGER t ..."), "DELIMITER ;;\nCREATE TRIGGER t ...;;\nDELIMITER ;\n");
    }

    #[test]
    fn computed_columns_are_left_out_but_expression_defaults_are_not() {
        for g in ["VIRTUAL GENERATED", "STORED GENERATED", "virtual generated", "VIRTUAL", "PERSISTENT"] {
            assert!(is_generated(g), "{g}");
        }
        for n in ["", "auto_increment", "DEFAULT_GENERATED", "on update CURRENT_TIMESTAMP", "DEFAULT_GENERATED on update CURRENT_TIMESTAMP"] {
            assert!(!is_generated(n), "{n}");
        }
    }

    #[test]
    fn a_name_in_a_comment_line_cannot_start_a_statement() {
        let name = "x\nDROP DATABASE qi_victim;\n-- x\r\u{b}y";
        assert_eq!(note(name), "x DROP DATABASE qi_victim; -- x  y");
        // Read back, the whole header is one comment and nothing else.
        let line = format!("--\n-- Table {}\n--\n", note(name));
        let mut sp = ScriptSplitter::new(true);
        sp.feed(line.as_bytes());
        sp.finish();
        assert_eq!(sp.next_piece().unwrap(), None);
    }

    #[test]
    fn views_come_after_the_views_they_select_from() {
        let v = |n: &str, from: &str| (n.to_string(), format!("CREATE ALGORITHM=UNDEFINED DEFINER=`root`@`%` SQL SECURITY DEFINER VIEW `{n}` AS select `{from}`.`a` AS `a` from `{from}`"));
        // a_top reads z_base, which reads the table t: alphabetical order would create a_top first.
        let views = [v("a_top", "z_base"), v("m_mid", "a_top"), v("z_base", "t")];
        let order: Vec<&str> = view_order(Dialect::MySql, &views).into_iter().map(|i| views[i].0.as_str()).collect();
        assert_eq!(order, ["z_base", "a_top", "m_mid"]);
        // Independent views keep their order, and a loop (from a column that shares a view's name) still writes all.
        assert_eq!(view_order(Dialect::MySql, &[v("a", "t"), v("b", "t")]), [0, 1]);
        assert_eq!(view_order(Dialect::MySql, &[v("a", "b"), v("b", "a")]), [0, 1]);
        // The definer and the view's own name are not references.
        assert_eq!(view_order(Dialect::MySql, &[v("root", "t"), v("b", "t")]), [0, 1]);
        assert!(view_order(Dialect::MySql, &[]).is_empty());
    }
    use crate::db::sql::testing::every_server;

    fn tmp(var: &str, ext: &str) -> PathBuf {
        std::env::temp_dir().join(format!("quarry_ie_{}_{var}.{ext}", std::process::id()))
    }

    /// A CREATE statement without the counter that moves with the data.
    fn shape(l: &crate::db::sql::testing::Live, q: &str) -> String {
        let s = l.cell(q, 1);
        match s.find(" AUTO_INCREMENT=") {
            Some(p) => {
                let end = s[p + 1..].find(' ').map_or(s.len(), |e| p + 1 + e);
                format!("{}{}", &s[..p], &s[end..])
            }
            None => s,
        }
    }

    #[test]
    fn live_dump_a_database_drop_it_and_run_the_dump_back() {
        every_server(|l, var| {
            let db = "quarry_ie_dump";
            l.sql(&format!("DROP DATABASE IF EXISTS {db}"));
            l.sql(&format!("CREATE DATABASE {db} CHARACTER SET utf8mb4"));
            l.sql(&format!("CREATE TABLE {db}.parent (id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(80) NOT NULL, data BLOB, price DECIMAL(10,2), born DATETIME, meta JSON, kind ENUM('a','b') DEFAULT 'a', flags SET('x','y','z'), dbl INT AS (LENGTH(name)) VIRTUAL) DEFAULT CHARSET=utf8mb4"));
            l.sql(&format!("CREATE TABLE {db}.child (id INT AUTO_INCREMENT PRIMARY KEY, parent_id INT NOT NULL, note VARCHAR(100), CONSTRAINT fk_p FOREIGN KEY (parent_id) REFERENCES parent(id)) DEFAULT CHARSET=utf8mb4"));
            l.sql(&format!("CREATE TABLE {db}.small (id INT PRIMARY KEY, v VARCHAR(20)) DEFAULT CHARSET=utf8mb4"));
            l.sql(&format!("CREATE TABLE {db}.empty_t (id INT PRIMARY KEY)"));
            for i in 1..=2500 {
                let name = if i % 7 == 0 { "it''s \\\\ back\\\\slash; DROP TABLE x; -- \u{1F600}".to_string() } else { format!("name {i}") };
                l.sql(&format!("INSERT INTO {db}.parent (name, data, price, born, meta, kind, flags) VALUES ('{name}', X'00FF10', {i}.25, '2026-01-02 03:04:05', '{{\"k\": {i}}}', 'b', 'x,z')"));
            }
            l.sql(&format!("INSERT INTO {db}.parent (name) VALUES ('nulls')"));
            l.sql(&format!("INSERT INTO {db}.child (parent_id, note) SELECT id, CONCAT('t:', id) FROM {db}.parent WHERE id <= 1200"));
            l.sql(&format!("INSERT INTO {db}.small VALUES (1, 'a'), (2, 'b'), (3, NULL)"));
            l.sql(&format!("CREATE VIEW {db}.v_sum AS SELECT parent_id, COUNT(*) AS n FROM {db}.child GROUP BY parent_id"));
            l.sql(&format!("CREATE TRIGGER {db}.trg_bi BEFORE INSERT ON {db}.child FOR EACH ROW SET NEW.note = CONCAT('t:', NEW.parent_id)"));
            l.sql(&format!("CREATE PROCEDURE {db}.p_inc(IN a INT, OUT b INT) BEGIN DECLARE x INT; SET x = a + 1; SET b = x; END"));
            l.sql(&format!("CREATE FUNCTION {db}.f_dbl(a INT) RETURNS INT DETERMINISTIC RETURN a * 2"));
            let tables = ["parent", "child", "small", "empty_t"];
            let before: Vec<(String, String, String)> = tables.iter().map(|t| (shape(l, &format!("SHOW CREATE TABLE {db}.{t}")), l.cell(&format!("CHECKSUM TABLE {db}.{t}"), 1), l.count(&format!("{db}.{t}")).to_string())).collect();
            let plan = |per| DumpPlan { schema: db.into(), tables: tables.iter().map(|t| DumpTable { name: t.to_string(), view: false, structure: true, content: true, drop: true }).chain([DumpTable { name: "v_sum".into(), view: true, structure: true, content: false, drop: true }]).collect(), create_db: true, extras: true, rows_per_insert: per, snapshot: true, filter: None };
            let path = tmp(var, "sql");
            let (count, cancel, id) = (AtomicU64::new(0), AtomicBool::new(false), AtomicU64::new(0));
            let done = l.w.rt.block_on(dump_to(&l.db, &plan(1000), path.clone(), &count, &cancel, &id)).unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!(done.rows, 2501 + 1200 + 3, "{var}");
            assert!(!part_of(&path).exists(), "the .part file is renamed away");
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(text.contains("DELIMITER ;;") && text.contains("CREATE DATABASE IF NOT EXISTS"), "{var}");
            // Triggers come after the data, so loading rows does not fire them.
            assert!(text.find("INSERT INTO `child`").unwrap() < text.find("Trigger trg_bi").unwrap(), "{var}");
            // A .gz name writes the same script compressed.
            let gz = tmp(var, "sql.gz");
            l.w.rt.block_on(dump_to(&l.db, &plan(1000), gz.clone(), &count, &cancel, &id)).unwrap_or_else(|x| panic!("{var}: {x}"));
            let mut unzipped = String::new();
            flate2::read::GzDecoder::new(std::fs::File::open(&gz).unwrap()).read_to_string(&mut unzipped).unwrap();
            assert_eq!(unzipped.lines().filter(|l| l.starts_with("INSERT")).count(), text.lines().filter(|l| l.starts_with("INSERT")).count(), "{var}");
            assert!(unzipped.ends_with("-- Dump complete\n") && std::fs::metadata(&gz).unwrap().len() < text.len() as u64 / 2, "{var}");
            let _ = std::fs::remove_file(&gz);
            let views_before = shape(l, &format!("SHOW CREATE VIEW {db}.v_sum"));
            let trig_before = l.one(&format!("SELECT ACTION_STATEMENT FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = '{db}'"));
            l.sql(&format!("DROP DATABASE {db}"));
            let prog = Arc::new(Progress::default());
            let ran = l.w.rt.block_on(run_file(l.db.clone(), String::new(), Source::File(path.clone()), false, prog.clone(), Arc::new(AtomicBool::new(false)), Arc::new(AtomicU64::new(0)))).unwrap_or_else(|x| panic!("{var}: {x}"));
            assert!(ran.statements > 20, "{var}");
            assert_eq!(prog.bytes.load(Ordering::Relaxed), std::fs::metadata(&path).unwrap().len(), "progress counts bytes");
            for (t, b) in tables.iter().zip(&before) {
                assert_eq!(shape(l, &format!("SHOW CREATE TABLE {db}.{t}")), b.0, "{var} structure of {t}");
                assert_eq!(l.cell(&format!("CHECKSUM TABLE {db}.{t}"), 1), b.1, "{var} data of {t}");
                assert_eq!(l.count(&format!("{db}.{t}")).to_string(), b.2, "{var} rows of {t}");
            }
            assert_eq!(shape(l, &format!("SHOW CREATE VIEW {db}.v_sum")), views_before, "{var}");
            assert_eq!(l.one(&format!("SELECT ACTION_STATEMENT FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = '{db}'")), trig_before, "{var}");
            // The routines and the trigger work again.
            assert_eq!(l.one(&format!("SELECT {db}.f_dbl(21)")), "42");
            assert_eq!(l.one(&format!("CALL {db}.p_inc(1, @r); SELECT @r")), "2");
            l.sql(&format!("INSERT INTO {db}.child (parent_id, note) VALUES (5, 'ignored')"));
            assert_eq!(l.one(&format!("SELECT note FROM {db}.child ORDER BY id DESC LIMIT 1")), "t:5");
            // One INSERT per row, and the same file again replaces what is there.
            let one = tmp(&format!("{var}_one"), "sql");
            let small = DumpPlan { tables: vec![DumpTable { name: "small".into(), view: false, structure: true, content: true, drop: true }], create_db: false, extras: false, ..plan(1) };
            l.w.rt.block_on(dump_to(&l.db, &small, one.clone(), &count, &cancel, &id)).unwrap();
            assert_eq!(std::fs::read_to_string(&one).unwrap().matches("INSERT INTO `small`").count(), 3, "{var}");
            for _ in 0..2 {
                l.w.rt.block_on(run_file(l.db.clone(), db.into(), Source::File(one.clone()), false, Arc::new(Progress::default()), Arc::new(AtomicBool::new(false)), Arc::new(AtomicU64::new(0)))).unwrap();
            }
            assert_eq!(l.count(&format!("{db}.small")), 3);
            // Cancel leaves no file at all.
            let gone = tmp(&format!("{var}_gone"), "sql");
            let r = l.w.rt.block_on(dump_to(&l.db, &plan(1000), gone.clone(), &count, &AtomicBool::new(true), &id));
            assert_eq!(r.err().as_deref(), Some("Export cancelled"), "{var}");
            assert!(!gone.exists() && !part_of(&gone).exists(), "{var}");
            // A filter writes only the matching rows, and cannot carry a second statement.
            let filt = DumpPlan { tables: vec![DumpTable { name: "small".into(), view: false, structure: false, content: true, drop: false }], create_db: false, extras: false, filter: Some("id > 1".into()), ..plan(1000) };
            let fp = tmp(&format!("{var}_filter"), "sql");
            assert_eq!(l.w.rt.block_on(dump_to(&l.db, &filt, fp.clone(), &count, &cancel, &id)).unwrap().rows, 2, "{var}");
            let bad = DumpPlan { filter: Some("1=1); DROP TABLE small; --".into()), ..filt.clone() };
            assert!(l.w.rt.block_on(dump_to(&l.db, &bad, tmp(&format!("{var}_bad"), "sql"), &count, &cancel, &id)).is_err());
            let bad = DumpPlan { filter: Some("id > 0; DELETE FROM small".into()), ..filt };
            assert!(l.w.rt.block_on(dump_to(&l.db, &bad, tmp(&format!("{var}_bad"), "sql"), &count, &cancel, &id)).is_err());
            assert_eq!(l.count(&format!("{db}.small")), 3, "{var}");
            for p in [path, one, fp] {
                let _ = std::fs::remove_file(p);
            }
            l.sql(&format!("DROP DATABASE {db}"));
        });
    }

    #[test]
    fn live_a_script_stops_at_the_first_error_and_names_it() {
        every_server(|l, var| {
            let db = "quarry_ie_script";
            l.sql(&format!("DROP DATABASE IF EXISTS {db}"));
            l.sql(&format!("CREATE DATABASE {db}"));
            let p = tmp(var, "script.sql");
            std::fs::write(&p, "CREATE TABLE a (id INT);\nINSERT INTO a VALUES (1);\nINSERT INTO nowhere VALUES (2);\nINSERT INTO a VALUES (3);\n").unwrap();
            let r = l.w.rt.block_on(run_file(l.db.clone(), db.into(), Source::File(p.clone()), false, Arc::new(Progress::default()), Arc::new(AtomicBool::new(false)), Arc::new(AtomicU64::new(0))));
            let err = r.err().unwrap_or_else(|| panic!("{var}: must fail"));
            assert!(err.contains("Statement 3 failed") && err.contains("INSERT INTO nowhere"), "{var}: {err}");
            assert_eq!(l.count(&format!("{db}.a")), 1, "the statements before it stay, the ones after never ran");
            // A locked connection never runs a file.
            let locked = l.w.rt.block_on(run_file(l.db.clone(), db.into(), Source::File(p.clone()), true, Arc::new(Progress::default()), Arc::new(AtomicBool::new(false)), Arc::new(AtomicU64::new(0))));
            assert!(locked.err().unwrap().contains("locked"), "{var}");
            let _ = std::fs::remove_file(p);
            l.sql(&format!("DROP DATABASE {db}"));
        });
    }

    /// Every table and view of `db`, all of it, as the Whole database choice writes it.
    fn whole(l: &crate::db::sql::testing::Live, db: &str) -> DumpPlan {
        let names: Vec<(String, bool)> = l.w.rt.block_on(async {
            let rows = sqlx::raw_sql(&format!("SELECT TABLE_NAME, TABLE_TYPE = 'VIEW' FROM information_schema.TABLES WHERE TABLE_SCHEMA = '{db}' ORDER BY TABLE_NAME")).fetch_all(&l.pool).await.unwrap();
            rows.iter().map(|r| (String::from_utf8_lossy(raw(r, 0).unwrap()).into_owned(), raw(r, 1) == Some(b"1"))).collect()
        });
        DumpPlan { schema: db.into(), tables: names.into_iter().map(|(name, view)| DumpTable { name, view, structure: true, content: !view, drop: true }).collect(), create_db: true, extras: true, rows_per_insert: 1000, snapshot: true, filter: None }
    }

    fn dump_with(l: &crate::db::sql::testing::Live, db: &Db, plan: &DumpPlan, path: &Path) {
        let (count, cancel, id) = (AtomicU64::new(0), AtomicBool::new(false), AtomicU64::new(0));
        l.w.rt.block_on(dump_to(db, plan, path.to_path_buf(), &count, &cancel, &id)).unwrap_or_else(|x| panic!("dump: {x}"));
    }

    fn run_with(l: &crate::db::sql::testing::Live, db: &Db, schema: &str, path: &Path) -> Res<Ran> {
        l.w.rt.block_on(run_file(db.clone(), schema.into(), Source::File(path.to_path_buf()), false, Arc::new(Progress::default()), Arc::new(AtomicBool::new(false)), Arc::new(AtomicU64::new(0))))
    }

    #[test]
    fn live_a_table_name_with_a_line_break_cannot_drop_another_database() {
        every_server(|l, var| {
            let (src, victim) = ("quarry_if_inj_src", "quarry_if_inj_victim");
            l.sql(&format!("DROP DATABASE IF EXISTS {src}; DROP DATABASE IF EXISTS {victim}; CREATE DATABASE {src}; CREATE DATABASE {victim}; CREATE TABLE {victim}.keep (id INT)"));
            l.sql(&format!("CREATE TABLE {src}.`x\nDROP DATABASE {victim};\n-- x` (id INT); INSERT INTO {src}.`x\nDROP DATABASE {victim};\n-- x` VALUES (1)"));
            let path = tmp(var, "inj.sql");
            dump_with(l, &l.db, &whole(l, src), &path);
            // Read back as the run reads it, no statement drops a database (the name inside backticks may span lines).
            let mut sp = ScriptSplitter::new(true);
            sp.feed(&std::fs::read(&path).unwrap());
            sp.finish();
            while let Some(p) = sp.next_piece().unwrap() {
                if let Piece::Stmt(s) = p {
                    assert!(!String::from_utf8_lossy(&s).to_ascii_uppercase().starts_with("DROP DATABASE"), "{var}: {}", String::from_utf8_lossy(&s));
                }
            }
            l.sql(&format!("DROP DATABASE {src}"));
            run_with(l, &l.db, "", &path).unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!(l.one(&format!("SELECT COUNT(*) FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = '{victim}'")), "1", "{var}: the victim database is still there");
            assert_eq!(l.count(&format!("{src}.`x\nDROP DATABASE {victim};\n-- x`")), 1, "{var}");
            let _ = std::fs::remove_file(path);
            l.sql(&format!("DROP DATABASE {src}; DROP DATABASE {victim}"));
        });
    }

    #[test]
    fn live_binary_values_written_raw_arrive_byte_for_byte() {
        every_server(|l, var| {
            let db = "quarry_if_raw";
            l.sql(&format!("DROP DATABASE IF EXISTS {db}; CREATE DATABASE {db}; CREATE TABLE {db}.t (id INT PRIMARY KEY, b BLOB)"));
            // As mysqldump writes a BLOB without --hex-blob: the bytes raw, only NUL, newline and quote escaped,
            // with and without the _binary introducer of newer versions.
            let raw_value = b"\\0\xFF\x80\xC3\xE2A\\n\\'";
            let mut file = b"/*!40101 SET NAMES utf8mb4 */;\nINSERT INTO t VALUES (1,'".to_vec();
            file.extend_from_slice(raw_value);
            file.extend_from_slice(b"'),(2,_binary '");
            file.extend_from_slice(raw_value);
            file.extend_from_slice(b"');\nINSERT INTO t VALUES (3, 'plain');\n");
            let path = tmp(var, "raw.sql");
            std::fs::write(&path, &file).unwrap();
            let ran = run_with(l, &l.db, db, &path).unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!(ran.statements, 3, "{var}");
            for id in [1, 2] {
                assert_eq!(l.one(&format!("SELECT HEX(b) FROM {db}.t WHERE id = {id}")), "00FF80C3E2410A27", "{var} row {id}");
            }
            assert_eq!(l.one(&format!("SELECT b FROM {db}.t WHERE id = 3")), "plain");
            // The prepared statement is gone and a failing one still names its statement.
            std::fs::write(&path, b"INSERT INTO nowhere VALUES ('\xFF');\n").unwrap();
            let err = run_with(l, &l.db, db, &path).err().unwrap_or_else(|| panic!("{var}: must fail"));
            assert!(err.contains("Statement 1 failed") && err.contains("not UTF-8"), "{var}: {err}");
            let _ = std::fs::remove_file(path);
            l.sql(&format!("DROP DATABASE {db}"));
        });
    }

    #[test]
    fn live_doubles_and_control_characters_survive_a_dump() {
        every_server(|l, var| {
            let db = "quarry_if_dbl";
            l.sql(&format!("DROP DATABASE IF EXISTS {db}; CREATE DATABASE {db} CHARACTER SET utf8mb4; CREATE TABLE {db}.t (id INT PRIMARY KEY, d DOUBLE, f FLOAT, s VARCHAR(20))"));
            l.sql(&format!("INSERT INTO {db}.t VALUES (1, 1e300, 0.1, CONCAT('a', CHAR(0), 'b', CHAR(26), 'c')), (2, 1e-100, -1.5e-30, NULL), (3, 1.7976931348623157e308, 3.4e38, ''), (4, 2.2250738585072014e-308, 1.17549435e-38, 'x'), (5, 0.30000000000000004, 1e-45, 'y'), (6, -0.0, 0, 'z')"));
            let before = l.cell(&format!("CHECKSUM TABLE {db}.t"), 1);
            let path = tmp(var, "dbl.sql");
            dump_with(l, &l.db, &whole(l, db), &path);
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(text.contains("'a\\0b\\Zc'"), "{var}: NUL and Ctrl-Z are escaped");
            l.sql(&format!("DROP DATABASE {db}"));
            run_with(l, &l.db, "", &path).unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!(l.cell(&format!("CHECKSUM TABLE {db}.t"), 1), before, "{var}");
            assert_eq!(l.one(&format!("SELECT d FROM {db}.t WHERE id = 1")), "1e300", "{var}");
            assert_eq!(l.one(&format!("SELECT d FROM {db}.t WHERE id = 2")), "1e-100", "{var}");
            assert_eq!(l.one(&format!("SELECT HEX(s) FROM {db}.t WHERE id = 1")), "6100621A63", "{var}");
            let _ = std::fs::remove_file(path);
            l.sql(&format!("DROP DATABASE {db}"));
        });
    }

    #[test]
    fn live_views_on_views_restore_in_dependency_order() {
        every_server(|l, var| {
            let db = "quarry_if_views";
            l.sql(&format!("DROP DATABASE IF EXISTS {db}; CREATE DATABASE {db}; CREATE TABLE {db}.t (a INT); INSERT INTO {db}.t VALUES (1), (2)"));
            // Alphabetical order would create a_top before the z_base it reads.
            l.sql(&format!("CREATE VIEW {db}.z_base AS SELECT a FROM {db}.t; CREATE VIEW {db}.a_top AS SELECT a FROM {db}.z_base; CREATE VIEW {db}.m_mid AS SELECT a FROM {db}.a_top WHERE a > 0"));
            let path = tmp(var, "views.sql");
            dump_with(l, &l.db, &whole(l, db), &path);
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(text.find("View z_base").unwrap() < text.find("View a_top").unwrap() && text.find("View a_top").unwrap() < text.find("View m_mid").unwrap(), "{var}");
            l.sql(&format!("DROP DATABASE {db}"));
            run_with(l, &l.db, "", &path).unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!(l.one(&format!("SELECT COUNT(*) FROM {db}.m_mid")), "2", "{var}");
            let _ = std::fs::remove_file(path);
            l.sql(&format!("DROP DATABASE {db}"));
        });
    }

    #[test]
    fn live_a_server_in_ansi_quotes_mode_writes_a_dump_that_runs_back() {
        every_server(|l, var| {
            let db = "quarry_if_ansi";
            l.sql(&format!("DROP DATABASE IF EXISTS {db}; CREATE DATABASE {db}; CREATE TABLE {db}.t (id INT PRIMARY KEY, v VARCHAR(10)); INSERT INTO {db}.t VALUES (1, 'x'); CREATE VIEW {db}.v AS SELECT id FROM {db}.t"));
            let ansi = l.with_session("SET SESSION sql_mode = CONCAT(@@sql_mode, ',ANSI_QUOTES')");
            let path = tmp(var, "ansi.sql");
            dump_with(l, &ansi, &whole(l, db), &path);
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(text.contains("CREATE TABLE `t`") && !text.contains("\"id\""), "{var}: {text}");
            l.sql(&format!("DROP DATABASE {db}"));
            run_with(l, &ansi, "", &path).unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!(l.one(&format!("SELECT v FROM {db}.v JOIN {db}.t USING (id)")), "x", "{var}");
            let _ = std::fs::remove_file(path);
            l.sql(&format!("DROP DATABASE {db}"));
        });
    }

    #[test]
    fn live_timestamps_keep_their_instant_in_another_zone() {
        every_server(|l, var| {
            let db = "quarry_if_tz";
            l.sql(&format!("DROP DATABASE IF EXISTS {db}; CREATE DATABASE {db}; CREATE TABLE {db}.t (ts TIMESTAMP NULL, dt DATETIME)"));
            l.sql(&format!("INSERT INTO {db}.t VALUES (FROM_UNIXTIME(1700000000), '2026-01-02 03:04:05')"));
            let path = tmp(var, "tz.sql");
            // Dumped from a session at +03:00, run back in one at +05:00.
            dump_with(l, &l.with_session("SET SESSION time_zone = '+03:00'"), &whole(l, db), &path);
            l.sql(&format!("DROP DATABASE {db}"));
            let east = l.with_session("SET SESSION time_zone = '+05:00'");
            run_with(l, &east, "", &path).unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!(l.one(&format!("SELECT UNIX_TIMESTAMP(ts) FROM {db}.t")), "1700000000", "{var}");
            assert_eq!(l.one(&format!("SELECT dt FROM {db}.t")), "2026-01-02 03:04:05", "{var}: DATETIME has no zone");
            // The file puts the session's zone back at the end.
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(text.contains("TIME_ZONE='+00:00'") && text.contains("SET TIME_ZONE=@OLD_TIME_ZONE"), "{var}");
            let _ = std::fs::remove_file(path);
            l.sql(&format!("DROP DATABASE {db}"));
        });
    }

    #[cfg(unix)]
    #[test]
    fn live_cancel_stops_a_file_whose_quote_never_closes() {
        every_server(|l, var| {
            // A pipe that is written until the run is over never ends, so only Cancel can stop the reading; the
            // statement limit would end it much later, with another message.
            let path = tmp(var, "open.fifo");
            let _ = std::fs::remove_file(&path);
            assert!(std::process::Command::new("mkfifo").arg(&path).status().unwrap().success());
            let (prog, cancel, done) = (Arc::new(Progress::default()), Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
            let writer = {
                let (path, cancel, done) = (path.clone(), cancel.clone(), done.clone());
                std::thread::spawn(move || {
                    let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
                    let _ = f.write_all(b"SELECT 1;\nSELECT 'never closed ");
                    let mut sent = 0usize;
                    while !done.load(Ordering::SeqCst) && f.write_all(&[b'x'; 64 << 10]).is_ok() {
                        sent += 64 << 10;
                        if sent >= 2 << 20 {
                            cancel.store(true, Ordering::SeqCst);
                        }
                    }
                })
            };
            let r = l.w.rt.block_on(run_file(l.db.clone(), String::new(), Source::File(path.clone()), false, prog.clone(), cancel, Arc::new(AtomicU64::new(0))));
            done.store(true, Ordering::SeqCst);
            writer.join().unwrap();
            let err = r.err().unwrap_or_else(|| panic!("{var}: must stop"));
            assert!(err.starts_with("Run stopped.") && err.contains("Cancelled."), "{var}: {err}");
            assert_eq!(prog.rows.load(Ordering::Relaxed), 1, "{var}: the open statement was never sent");
            let _ = std::fs::remove_file(path);
        });
    }
}
