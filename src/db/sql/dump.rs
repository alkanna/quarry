//! SQL dumps: writing a database or some of its tables as a script (CREATE statements, then the rows as
//! batched INSERTs), and running a script file back. Both stream, so a dump of any size needs only a batch of
//! rows or one statement in memory. A dump is written next to its name as a hidden .part file and renamed at the
//! end, so a stopped or failed one leaves nothing behind.

use super::csv::Source;
use super::export::Done;
use super::import::{JobDone, Progress};
use super::lex::{self, Piece, ScriptSplitter};
use super::value::Kind;
use super::worker::{make_cols, prepare, raw, text, to_row};
use super::write::writer;
use super::{Db, Dialect, Pool};
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
const INSERT_BYTES: usize = 1 << 20;
/// The mode every dump runs under: backslash escapes on and no ANSI quotes, which is what the values are written for.
const DUMP_MODE: &str = "NO_AUTO_VALUE_ON_ZERO";

fn io(x: std::io::Error) -> String {
    format!("Could not write: {x}")
}

fn put(out: &mut dyn Write, s: &str) -> Res<()> {
    out.write_all(s.as_bytes()).map_err(io)
}

/// A string as a SQL literal for the statements a dump writes around its data (never a user value).
fn lit(s: &str) -> String {
    super::value::lit(s, true)
}

fn numeric(k: Kind) -> bool {
    matches!(k, Kind::Int | Kind::UInt | Kind::Float | Kind::Decimal)
}

fn stopped(cancel: &AtomicBool) -> Res<()> {
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
    let rows = sqlx::query(d.columns()).bind(schema).bind(table).fetch_all(&mut *c).await.map_err(e)?;
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
fn flush_rows(batch: &mut Vec<Document>, copy: &[CopyCol], table: &str, out: &mut (dyn Write + Send)) -> Res<()> {
    if batch.is_empty() {
        return Ok(());
    }
    let mut w = Writer::new(Fmt::SqlInsert, true, copy.to_vec(), table, &[], true);
    let text = w.begin() + &w.rows(batch) + &w.end() + "\n";
    batch.clear();
    out.write_all(text.as_bytes()).map_err(io)
}

/// The rows of one table as INSERT statements, `per` rows each. Returns the rows written.
#[allow(clippy::too_many_arguments)]
async fn write_rows(c: &mut sqlx::MySqlConnection, d: Dialect, plan: &DumpPlan, table: &str, out: &mut (dyn Write + Send), count: &AtomicU64, base: u64, cancel: &AtomicBool) -> Res<u64> {
    let cols = table_cols(c, d, &plan.schema, table).await?;
    let list = cols.names.iter().map(|n| d.quote_ident(n)).collect::<Vec<_>>().join(", ");
    let mut sql = format!("SELECT {list} FROM {}", d.qualified(&plan.schema, table));
    if let Some(f) = plan.filter.as_ref().filter(|f| !f.trim().is_empty()) {
        // The clause is the user's text, so the whole statement must still be one statement that only reads.
        sql.push_str(&format!("\nWHERE (\n{}\n)", f.trim()));
        if lex::split_statements(&sql).len() != 1 {
            return Err("The filter must not contain another statement.".into());
        }
        lex::read_only_check(&sql)?;
    }
    let per = plan.rows_per_insert.max(1);
    let mut s = raw_sql(&sql).fetch(&mut *c);
    let (mut kinds, mut copy, mut batch, mut bytes, mut rows) = (Vec::new(), Vec::new(), Vec::<Document>::new(), 0usize, 0u64);
    while let Some(row) = futures_util::StreamExt::next(&mut s).await {
        let row = row.map_err(e)?;
        if kinds.is_empty() {
            let mc = make_cols(sqlx::Row::columns(&row), &cols.meta, false);
            copy = mc.iter().map(|c| CopyCol { name: c.name.clone(), src: c.src.clone(), numeric: numeric(c.kind) }).collect();
            kinds = mc;
        }
        bytes += (0..row.len()).map(|i| raw(&row, i).map_or(4, <[u8]>::len)).sum::<usize>();
        batch.push(to_row(&kinds, &row, &[])?.owned());
        rows += 1;
        if batch.len() >= per || bytes >= INSERT_BYTES {
            flush_rows(&mut batch, &copy, table, out)?;
            bytes = 0;
            count.store(base + rows, Ordering::Relaxed);
            stopped(cancel)?;
        }
    }
    flush_rows(&mut batch, &copy, table, out)?;
    count.store(base + rows, Ordering::Relaxed);
    Ok(rows)
}

/// The text of the statement `sql` (a SHOW CREATE) answers in column `at`, with the session's sql_mode of the
/// object in column 1 when it has one.
async fn show_create(c: &mut sqlx::MySqlConnection, sql: String, at: usize) -> Res<(Option<String>, String)> {
    // Read as a stream like the exports: `fetch_one` of a raw statement on a borrowed connection is refused by the
    // compiler inside a spawned task.
    let mut s = raw_sql(&sql).fetch_many(&mut *c);
    let mut got = None;
    while let Some(item) = futures_util::StreamExt::next(&mut s).await {
        if let sqlx::Either::Right(r) = item.map_err(e)? {
            let mode = raw(&r, 1).map(|b| String::from_utf8_lossy(b).into_owned()).filter(|_| at == 2);
            got = Some((mode, raw(&r, at).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default()));
        }
    }
    got.ok_or_else(|| "The server returned no definition.".to_string())
}

/// A statement that holds `;` inside its body, written between DELIMITER lines so it can be run back.
fn delimited(sql_mode: Option<&str>, stmt: &str) -> String {
    let set = |m: &str| format!("SET sql_mode = {};\n", lit(m));
    format!("{}DELIMITER ;;\n{stmt};;\nDELIMITER ;\n{}", sql_mode.map(set).unwrap_or_default(), if sql_mode.is_some() { set(DUMP_MODE) } else { String::new() })
}

/// Runs the whole dump on a read-only connection of its own.
pub async fn dump(db: &Db, plan: &DumpPlan, out: &mut (dyn Write + Send), count: &AtomicU64, cancel: &AtomicBool, id: &AtomicU64) -> Res<u64> {
    let Pool::My(pool) = &db.pool;
    let d = db.dialect;
    let mut c = pool.acquire().await.map_err(e)?.detach();
    prepare(&mut c, d, &plan.schema, true, id).await.map_err(e)?;
    // The statement timeout of the pool would cut the read of a big table short.
    let timeout = d.session_timeout(0);
    let _ = c.execute(timeout.as_str()).await;
    if plan.snapshot {
        c.execute("START TRANSACTION WITH CONSISTENT SNAPSHOT").await.map_err(e)?;
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
    put(out, &format!("-- Quarry SQL dump\n-- Server: {}\n-- Database: {}\n\n", db.label(), plan.schema))?;
    put(out, &format!("SET NAMES utf8mb4;\nSET @OLD_SQL_MODE=@@SQL_MODE, SQL_MODE={};\nSET @OLD_FOREIGN_KEY_CHECKS=@@FOREIGN_KEY_CHECKS, FOREIGN_KEY_CHECKS=0;\n\n", lit(DUMP_MODE)))?;
    if plan.create_db {
        let (_, s) = show_create(c, format!("SHOW CREATE DATABASE {}", q(&plan.schema)), 1).await?;
        put(out, &format!("{};\nUSE {};\n\n", create_db_sql(&s), q(&plan.schema)))?;
    }
    let mut rows = 0u64;
    // Tables first, then views, which need them; data before triggers, which would otherwise fire on every row.
    let tables: Vec<&DumpTable> = plan.tables.iter().filter(|t| !t.view).chain(plan.tables.iter().filter(|t| t.view)).collect();
    for t in &tables {
        stopped(cancel)?;
        if !t.structure && !t.content {
            continue;
        }
        put(out, &format!("--\n-- {} {}\n--\n\n", if t.view { "View" } else { "Table" }, t.name))?;
        if t.drop {
            put(out, &format!("DROP {} IF EXISTS {};\n", if t.view { "VIEW" } else { "TABLE" }, q(&t.name)))?;
        }
        if t.structure {
            let (_, s) = show_create(c, d.create_stmt(t.view, &plan.schema, &t.name), 1).await?;
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
            let names: Vec<String> = sqlx::query("SELECT TRIGGER_NAME AS name FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = ? AND EVENT_OBJECT_TABLE = ? ORDER BY TRIGGER_NAME").bind(&plan.schema).bind(&t.name).fetch_all(&mut *c).await.map_err(e)?.iter().filter_map(|r| text(r, "name")).collect();
            for n in names {
                stopped(cancel)?;
                let (mode, stmt) = show_create(c, format!("SHOW CREATE TRIGGER {}", d.qualified(&plan.schema, &n)), 2).await?;
                put(out, &format!("--\n-- Trigger {n}\n--\n\nDROP TRIGGER IF EXISTS {};\n{}\n", q(&n), delimited(mode.as_deref(), &stmt)))?;
            }
        }
        let routines: Vec<(String, String)> = sqlx::query("SELECT ROUTINE_NAME AS name, ROUTINE_TYPE AS kind FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = ? ORDER BY ROUTINE_TYPE, ROUTINE_NAME").bind(&plan.schema).fetch_all(&mut *c).await.map_err(e)?.iter().filter_map(|r| Some((text(r, "name")?, text(r, "kind")?.to_ascii_uppercase()))).collect();
        for (n, kind) in routines {
            stopped(cancel)?;
            let kind = if kind == "FUNCTION" { "FUNCTION" } else { "PROCEDURE" };
            let (mode, stmt) = show_create(c, format!("SHOW CREATE {kind} {}", d.qualified(&plan.schema, &n)), 2).await?;
            if stmt.is_empty() {
                put(out, &format!("-- {kind} {n} was left out: the account may not read its definition.\n\n"))?;
                continue;
            }
            put(out, &format!("--\n-- {kind} {n}\n--\n\nDROP {kind} IF EXISTS {};\n{}\n", q(&n), delimited(mode.as_deref(), &stmt)))?;
        }
    }
    put(out, "SET FOREIGN_KEY_CHECKS=@OLD_FOREIGN_KEY_CHECKS;\nSET SQL_MODE=@OLD_SQL_MODE;\n-- Dump complete\n")?;
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
        let mut out = std::io::BufWriter::new(file);
        let rows = dump(db, plan, &mut out, count, cancel, id).await?;
        out.flush().map_err(|x| format!("Could not write {}: {x}", path.display()))?;
        std::fs::rename(&tmp, &path).map_err(|x| format!("Could not write {}: {x}", path.display()))?;
        Ok(rows)
    }
    .await;
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res.map(|rows| Done { rows, #[cfg(test)] peak_batch: 0, path: Some(path), files: 1, text: None })
}

/// Reads text from `r` in pieces, holding back a character that a read cut in two.
struct Utf8Chunks<R: Read> {
    r: R,
    carry: Vec<u8>,
    first: bool,
    buf: Vec<u8>,
}

impl<R: Read> Utf8Chunks<R> {
    fn new(r: R) -> Utf8Chunks<R> {
        Utf8Chunks { r, carry: Vec::new(), first: true, buf: vec![0; 256 << 10] }
    }
}

impl<R: Read> Utf8Chunks<R> {
    fn next(&mut self) -> Res<Option<String>> {
        let n = self.r.read(&mut self.buf).map_err(|x| format!("Could not read the file: {x}"))?;
        let mut bytes = std::mem::take(&mut self.carry);
        bytes.extend_from_slice(&self.buf[..n]);
        if n == 0 {
            return Ok((!bytes.is_empty()).then(|| String::from_utf8_lossy(&bytes).into_owned()));
        }
        let keep = match std::str::from_utf8(&bytes) {
            Ok(_) => bytes.len(),
            Err(x) if x.error_len().is_none() => x.valid_up_to(),
            Err(_) => bytes.len(),
        };
        self.carry = bytes.split_off(keep);
        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        if self.first {
            self.first = false;
            if let Some(t) = text.strip_prefix('\u{feff}') {
                text = t.to_string();
            }
        }
        Ok(Some(text))
    }
}

/// The statement as the error message shows it.
fn excerpt(s: &str) -> String {
    let t: String = s.chars().take(600).collect();
    if s.chars().count() > 600 { format!("{t}...") } else { t }
}

pub struct Ran {
    pub statements: u64,
}

/// Runs a script file statement by statement on a connection of its own, without a transaction: a dump holds
/// DDL, which commits as it goes. It stops at the first error and says which statement it was. One function
/// with one owned connection: split in two, the compiler cannot prove the future is Send inside a spawned task.
#[allow(clippy::too_many_arguments)]
pub async fn run_file(db: Db, schema: String, src: Source, read_only: bool, prog: Arc<Progress>, cancel: Arc<AtomicBool>, id: Arc<AtomicU64>) -> Res<Ran> {
    let mut c = writer(&db, &schema, read_only).await?;
    if let Ok(r) = c.fetch_one("SELECT CONNECTION_ID()").await {
        id.store(raw(&r, 0).and_then(|b| std::str::from_utf8(b).ok()?.parse().ok()).unwrap_or(0), Ordering::SeqCst);
    }
    let timeout = db.dialect.session_timeout(0);
    let _ = c.execute(timeout.as_str()).await;
    let open = src.open().map_err(|x| format!("Could not read the file: {x}"))?;
    let mut chunks = Utf8Chunks::new(open);
    let mut sp = ScriptSplitter::new(mode_of(&mut c).await);
    let (mut n, mut eof) = (0u64, false);
    let res: Res<Ran> = 'run: {
        loop {
            let piece = match sp.next_piece() {
                Ok(p) => p,
                Err(x) => break 'run Err(x),
            };
            match piece {
                Some(Piece::Delimiter(_)) => {}
                Some(Piece::Stmt(stmt)) => {
                    if cancel.load(Ordering::SeqCst) {
                        break 'run Err("Cancelled.".into());
                    }
                    n += 1;
                    let outcome = run_text(&mut c, &stmt).await;
                    if let Err(x) = outcome {
                        break 'run Err(format!("Statement {n} failed: {}

{}

Statements before it ran and stay applied.", e(x), excerpt(&stmt)));
                    }
                    // A SET of sql_mode changes how the rest of the file must be read.
                    if lex::changes_escaping(&stmt) {
                        let bs = mode_of(&mut c).await;
                        sp.set_backslash(bs);
                    }
                    prog.rows.store(n, Ordering::Relaxed);
                    prog.bytes.store(sp.offset(), Ordering::Relaxed);
                }
                None if eof => break,
                None => match chunks.next() {
                    Ok(Some(t)) => sp.feed(&t),
                    Ok(None) => {
                        eof = true;
                        sp.finish();
                    }
                    Err(x) => break 'run Err(x),
                },
            }
        }
        prog.bytes.store(sp.offset(), Ordering::Relaxed);
        Ok(Ran { statements: n })
    };
    let _ = c.close().await;
    match res {
        Err(x) if cancel.load(Ordering::SeqCst) => Err(format!("Run stopped. Statements that already ran stay applied.
{x}")),
        r => r,
    }
}

/// Runs one statement as text and reads its results to the end.
async fn run_text(c: &mut sqlx::MySqlConnection, stmt: &str) -> Result<(), sqlx::Error> {
    let mut s = raw_sql(stmt).fetch_many(&mut *c);
    while let Some(item) = futures_util::StreamExt::next(&mut s).await {
        item?;
    }
    Ok(())
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
        self.spawn(async move {
            let res = dump_to(&db, &plan, path, &count, &cancel, &id).await;
            id.store(0, Ordering::SeqCst);
            Msg::SqlExport { req, res }
        });
    }

    /// Runs a script file, answering with `SqlJob`.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_run_file(&self, req: u64, db: Db, schema: String, src: Source, read_only: bool, prog: Arc<Progress>, id: Arc<AtomicU64>, cancel: Arc<AtomicBool>) {
        self.spawn(async move {
            let res = run_file(db, schema.clone(), src, read_only, prog, cancel, id.clone()).await;
            id.store(0, Ordering::SeqCst);
            let res = res.map(|r| JobDone { msg: format!("Ran {} statements", r.statements), undo: None, schema: schema.clone(), table: None, tables_changed: true });
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
        sp.feed(&s);
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
    fn a_file_is_read_in_chunks_without_cutting_a_character() {
        let text = "SELECT 'caf\u{e9} \u{1F600}';\n".repeat(300);
        // A reader that hands out a few bytes at a time lands inside characters.
        struct Slow<'a>(&'a [u8], usize);
        impl Read for Slow<'_> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = self.1.min(self.0.len()).min(buf.len());
                buf[..n].copy_from_slice(&self.0[..n]);
                self.0 = &self.0[n..];
                Ok(n)
            }
        }
        for cut in [1usize, 2, 3, 5, 4096] {
            let mut ch = Utf8Chunks::new(Slow(text.as_bytes(), cut));
            let mut all = String::new();
            while let Some(t) = ch.next().unwrap() {
                all.push_str(&t);
            }
            assert_eq!(all, text, "cut {cut}");
        }
        let mut ch = Utf8Chunks::new(std::io::Cursor::new(b"\xEF\xBB\xBFSELECT 1".to_vec()));
        assert_eq!(ch.next().unwrap().unwrap(), "SELECT 1");
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
}
