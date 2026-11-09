//! Streams the rows of a read statement into a file or into clipboard text in a Copy as format. The
//! statement runs again and rows go through a `copy::Writer` a batch at a time, so a result of any size
//! never sits in memory.

use super::value::Kind;
use super::worker::{make_cols, prepare, to_row};
use super::{lex, Db, Pool};
use crate::db::copy::{CopyCol, Fmt, Writer};
use crate::db::{e, Msg, Res, Worker};
use mongodb::bson::Document;
use sqlx::{raw_sql, Connection, Either, Executor};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Rows held at once, the most the stream ever keeps.
pub const BATCH: usize = 1000;
/// The most text a copy to the clipboard may build; past it Export to a file is the way.
pub const CLIP_MAX: usize = 64 << 20;

#[derive(Clone)]
pub struct Plan {
    pub fmt: Fmt,
    pub header: bool,
    /// The table name SQL INSERT and UPDATE write.
    pub table: String,
    pub bs: bool,
}

/// Where the text goes.
pub enum Sink {
    File(PathBuf),
    Clipboard,
}

/// How a stream ended. `peak_batch` is the most rows held at once, which tests check against `BATCH`.
pub struct Done {
    pub rows: u64,
    #[cfg(test)]
    pub peak_batch: usize,
    pub path: Option<PathBuf>,
    /// Files written: more than one for "one file per result", where `path` is the first.
    pub files: usize,
    pub text: Option<String>,
}

/// A text buffer that refuses to grow past a limit, so a copy of a huge result fails instead of filling memory.
struct Capped {
    buf: Vec<u8>,
    max: usize,
}

impl Write for Capped {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if self.buf.len() + b.len() > self.max {
            return Err(std::io::Error::other("too large"));
        }
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn numeric(k: Kind) -> bool {
    matches!(k, Kind::Int | Kind::UInt | Kind::Float | Kind::Decimal)
}

/// How one statement's result sits inside a bigger file. The default is a file of its own.
#[derive(Default)]
pub struct Wrap<'a> {
    /// Text written before the result, once it is known to have a result set.
    pub head: &'a str,
    /// A JSON string literal put in front of every JSON lines row as its `statement` field.
    pub tag: Option<&'a str>,
    /// Rows already written by earlier statements, so the progress count keeps growing.
    pub base: u64,
    /// A statement without a result set (USE, SET) is skipped instead of written as an empty table.
    pub lenient: bool,
}

/// Puts `"statement":<tag>` first in every object line of JSON lines text.
fn tag_lines(text: &str, tag: &str) -> String {
    text.split('\n').map(|l| match l.strip_prefix('{') { Some(rest) => format!("{{\"statement\":{tag},{rest}"), None => l.to_string() }).collect::<Vec<_>>().join("\n")
}

fn write_err(err: std::io::Error) -> String {
    if err.to_string() == "too large" { "The text is over 64 MB. Export it to a file instead.".to_string() } else { format!("Could not write: {err}") }
}

/// Runs `stmt` and writes every row to `out`. Only a statement that passes the read-only check runs, on
/// a read-only session, whatever the connection allows. `count` follows the rows written for a progress readout.
pub async fn stream(db: &Db, schema: &str, stmt: &str, plan: &Plan, out: &mut (dyn Write + Send), count: &AtomicU64, cancel: &AtomicBool, id: &AtomicU64) -> Res<(u64, usize)> {
    lex::read_only_check(stmt).map_err(|why| format!("Export all only runs statements that read: {why}"))?;
    let Pool::My(pool) = &db.pool;
    let mut conn = pool.acquire().await.map_err(e)?.detach();
    prepare(&mut conn, db.dialect, schema, true, id).await.map_err(e)?;
    let r = stream_conn(&mut conn, stmt, plan, out, count, cancel, &Wrap::default()).await?;
    let _ = conn.close().await;
    let (rows, peak) = r.unwrap_or_default();
    // Line formats end with a newline like any text file.
    out.write_all(b"\n").map_err(write_err)?;
    Ok((rows, peak))
}

/// Streams one statement on an open session, without the closing newline. None when `wrap.lenient` and
/// the statement has no result set.
async fn stream_conn(conn: &mut sqlx::MySqlConnection, stmt: &str, plan: &Plan, out: &mut (dyn Write + Send), count: &AtomicU64, cancel: &AtomicBool, wrap: &Wrap<'_>) -> Res<Option<(u64, usize)>> {
    let io = write_err;
    let fix = |t: String| match wrap.tag { Some(tag) => tag_lines(&t, tag), None => t };
    let mut done = false;
    let (mut cols, mut writer, mut batch, mut rows, mut peak) = (Vec::new(), None::<Writer>, Vec::<Document>::new(), 0u64, 0usize);
    let copy_cols = |c: &[super::Col]| c.iter().map(|c| CopyCol { name: c.name.clone(), src: c.src.clone(), numeric: numeric(c.kind) }).collect::<Vec<_>>();
    {
        let mut s = raw_sql(stmt).fetch_many(&mut *conn);
        while let Some(item) = futures_util::StreamExt::next(&mut s).await {
            if cancel.load(Ordering::SeqCst) {
                return Err("Export cancelled".into());
            }
            match item.map_err(e)? {
                // Later results of a CALL are read off the wire and dropped.
                Either::Right(_) if done => {}
                Either::Right(row) => {
                    if writer.is_none() {
                        cols = make_cols(sqlx::Row::columns(&row), &Default::default(), false);
                        let mut w = Writer::new(plan.fmt, plan.header, copy_cols(&cols), &plan.table, &[], plan.bs);
                        out.write_all(wrap.head.as_bytes()).map_err(io)?;
                        out.write_all(w.begin().as_bytes()).map_err(io)?;
                        writer = Some(w);
                    }
                    batch.push(to_row(&cols, &row, &[])?.owned());
                    peak = peak.max(batch.len());
                    if batch.len() >= BATCH {
                        let text = writer.as_mut().map(|w| fix(w.rows(&batch))).unwrap_or_default();
                        out.write_all(text.as_bytes()).map_err(io)?;
                        rows += batch.len() as u64;
                        count.store(wrap.base + rows, Ordering::Relaxed);
                        batch.clear();
                    }
                }
                // The end of the first result. Reading goes on to the end so the connection is clean for `describe`.
                Either::Left(_) => done = true,
            }
        }
    }
    if writer.is_none() {
        // No rows: the header and footer of an empty table still need the column names.
        let d = match (&mut *conn).describe(stmt).await {
            Ok(d) if !(wrap.lenient && d.columns().is_empty()) => d,
            // USE and similar statements cannot be prepared and have no columns.
            Ok(_) => return Ok(None),
            Err(_) if wrap.lenient => return Ok(None),
            Err(x) => return Err(e(x)),
        };
        cols = make_cols(d.columns(), &Default::default(), false);
        let mut w = Writer::new(plan.fmt, plan.header, copy_cols(&cols), &plan.table, &[], plan.bs);
        out.write_all(wrap.head.as_bytes()).map_err(io)?;
        out.write_all(w.begin().as_bytes()).map_err(io)?;
        writer = Some(w);
    }
    if let Some(mut w) = writer {
        out.write_all(fix(w.rows(&batch)).as_bytes()).map_err(io)?;
        rows += batch.len() as u64;
        count.store(wrap.base + rows, Ordering::Relaxed);
        out.write_all(w.end().as_bytes()).map_err(io)?;
    }
    Ok(Some((rows, peak)))
}

impl Worker {
    /// Runs `stmt` again and streams it to `sink`, answering with `SqlExport`. A file is written beside its
    /// name and renamed over it at the end, so a stopped or failed export leaves no half file. `id` and
    /// `cancel` work like a script's: `sql_kill` with them stops it.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_export(&self, req: u64, db: Db, schema: String, stmt: String, plan: Plan, sink: Sink, count: Arc<AtomicU64>, id: Arc<AtomicU64>, cancel: Arc<AtomicBool>) {
        self.spawn(async move {
            let res = export_to(&db, &schema, &stmt, &plan, sink, &count, &cancel, &id).await;
            id.store(0, Ordering::SeqCst);
            Msg::SqlExport { req, res }
        });
    }
}

pub async fn export_to(db: &Db, schema: &str, stmt: &str, plan: &Plan, sink: Sink, count: &AtomicU64, cancel: &AtomicBool, id: &AtomicU64) -> Res<Done> {
    match sink {
        Sink::Clipboard => {
            let mut out = Capped { buf: Vec::new(), max: CLIP_MAX };
            let (rows, _peak) = stream(db, schema, stmt, plan, &mut out, count, cancel, id).await?;
            Ok(Done { rows, #[cfg(test)] peak_batch: _peak, path: None, files: 0, text: Some(String::from_utf8_lossy(&out.buf).into_owned()) })
        }
        Sink::File(path) => {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let tmp = path.with_file_name(format!(".{name}.part"));
            let res = async {
                let file = std::fs::File::create(&tmp).map_err(|x| format!("Could not write {}: {x}", path.display()))?;
                let mut out = std::io::BufWriter::new(file);
                let r = stream(db, schema, stmt, plan, &mut out, count, cancel, id).await?;
                out.flush().map_err(|x| format!("Could not write {}: {x}", path.display()))?;
                std::fs::rename(&tmp, &path).map_err(|x| format!("Could not write {}: {x}", path.display()))?;
                Ok(r)
            }
            .await;
            if res.is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
            res.map(|(rows, _peak)| Done { rows, #[cfg(test)] peak_batch: _peak, path: Some(path), files: 1, text: None })
        }
    }
}

/// What "All statements in the editor" shares across statements.
pub struct AllPlan {
    pub plan: Plan,
    /// One file for the whole script instead of one per result. Only formats that can hold several results.
    pub single: bool,
}

/// Formats that can hold several results in one file.
pub fn can_single(f: Fmt) -> bool {
    matches!(f, Fmt::SqlInsert | Fmt::JsonLines | Fmt::Json | Fmt::Markdown | Fmt::Html)
}

/// The statements of a script, or why the export must not start. Every statement is checked before any
/// runs, so a script with a write in it never half runs, whatever the connection's lock says.
pub fn check_script(text: &str) -> Result<Vec<String>, String> {
    let stmts: Vec<String> = lex::split_statements(text).into_iter().map(|(_, s)| s.to_string()).collect();
    if stmts.is_empty() {
        return Err("The editor has no statements.".into());
    }
    for (i, s) in stmts.iter().enumerate() {
        lex::read_only_check(s).map_err(|why| format!("Statement {} does not only read, so nothing was exported: {why}", i + 1))?;
    }
    Ok(stmts)
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

/// Text before result `n` (the statement's number) of a single file. `first` is true for the first result in the file.
fn single_head(f: Fmt, n: usize, stmt: &str, first: bool) -> String {
    let gap = if first { "" } else { "\n" };
    match f {
        Fmt::SqlInsert => format!("{gap}-- Statement {n}\n"),
        Fmt::Json => format!("{}{{\"statement\": {}, \"rows\": ", if first { "[\n" } else { ",\n" }, json_str(stmt)),
        Fmt::Markdown => format!("{gap}## Statement {n}\n\n```sql\n{stmt}\n```\n\n"),
        Fmt::Html => format!("{gap}<h2>Statement {n}</h2>\n<pre>{}</pre>\n", crate::db::copy::html_cell(stmt)),
        _ => String::new(),
    }
}

/// Text after a result's last row.
fn result_tail(f: Fmt, single: bool, rows: u64) -> &'static str {
    match (f, single) {
        (Fmt::Json, true) => "}",
        // A JSON lines result with no rows wrote no line, so it has no newline to end.
        (Fmt::JsonLines, _) if rows == 0 => "",
        _ => "\n",
    }
}

/// Text after the last result of a single file.
fn single_close(f: Fmt) -> &'static str {
    if f == Fmt::Json { "\n]\n" } else { "" }
}

/// A file that appears on its first write, so a statement without a result leaves nothing behind.
struct Lazy {
    tmp: PathBuf,
    file: Option<std::io::BufWriter<std::fs::File>>,
}

impl Lazy {
    fn new(tmp: PathBuf) -> Lazy {
        Lazy { tmp, file: None }
    }

    /// Creates the file when nothing was written yet, for a result with no text at all.
    fn ensure(&mut self) -> std::io::Result<&mut std::io::BufWriter<std::fs::File>> {
        if self.file.is_none() {
            self.file = Some(std::io::BufWriter::new(std::fs::File::create(&self.tmp)?));
        }
        Ok(self.file.as_mut().expect("just made"))
    }
}

impl Write for Lazy {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.ensure()?.write(b)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match &mut self.file {
            Some(f) => f.flush(),
            None => Ok(()),
        }
    }
}

fn part_of(path: &std::path::Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!(".{name}.part"))
}

/// `name-2.ext` beside `path` for the second result of a script.
pub fn numbered(path: &std::path::Path, n: usize) -> PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    match path.extension() {
        Some(x) => path.with_file_name(format!("{stem}-{n}.{}", x.to_string_lossy())),
        None => path.with_file_name(format!("{stem}-{n}")),
    }
}

/// Runs every statement on one read-only session, so USE and @variables carry over, and streams each result
/// set to `path` (one file) or to `path` numbered (a file per result). Nothing is renamed to its final name
/// until every statement has finished, and a failure or Cancel removes every temporary file.
#[allow(clippy::too_many_arguments)]
pub async fn export_all(db: &Db, schema: &str, stmts: &[String], ap: &AllPlan, path: &std::path::Path, count: &AtomicU64, step: &AtomicU64, cancel: &AtomicBool, id: &AtomicU64) -> Res<Done> {
    for s in stmts {
        lex::read_only_check(s).map_err(|why| format!("Export all only runs statements that read: {why}"))?;
    }
    let mut parts: Vec<(PathBuf, PathBuf)> = Vec::new();
    // The file being written when a statement fails, which `parts` does not hold yet.
    let mut open: Option<PathBuf> = None;
    let res = run_all(db, schema, stmts, ap, path, count, step, cancel, id, &mut parts, &mut open).await;
    // A killed statement comes back as a server error, so the flag says what really happened.
    let res = match res {
        Err(_) if cancel.load(Ordering::SeqCst) => Err("Export cancelled".to_string()),
        r => r,
    };
    let rows = match res {
        Ok(rows) => rows,
        Err(x) => {
            parts.iter().map(|p| &p.0).chain(open.iter()).for_each(|t| drop(std::fs::remove_file(t)));
            return Err(x);
        }
    };
    for (i, (tmp, fin)) in parts.iter().enumerate() {
        if let Err(x) = std::fs::rename(tmp, fin) {
            parts[i..].iter().for_each(|p| drop(std::fs::remove_file(&p.0)));
            return Err(format!("Could not write {}: {x}", fin.display()));
        }
    }
    let first = parts.first().map(|p| p.1.clone());
    Ok(Done { rows, #[cfg(test)] peak_batch: 0, path: first, files: parts.len(), text: None })
}

#[allow(clippy::too_many_arguments)]
async fn run_all(db: &Db, schema: &str, stmts: &[String], ap: &AllPlan, path: &std::path::Path, count: &AtomicU64, step: &AtomicU64, cancel: &AtomicBool, id: &AtomicU64, parts: &mut Vec<(PathBuf, PathBuf)>, open: &mut Option<PathBuf>) -> Res<u64> {
    let Pool::My(pool) = &db.pool;
    let mut conn = pool.acquire().await.map_err(e)?.detach();
    prepare(&mut conn, db.dialect, schema, true, id).await.map_err(e)?;
    let fmt = ap.plan.fmt;
    let (mut total, mut results) = (0u64, 0usize);
    let mut shared = Lazy::new(part_of(path));
    let io = write_err;
    for (i, stmt) in stmts.iter().enumerate() {
        step.store(i as u64 + 1, Ordering::Relaxed);
        if cancel.load(Ordering::SeqCst) {
            return Err("Export cancelled".into());
        }
        let first = results == 0;
        let head = if ap.single { single_head(fmt, i + 1, stmt, first) } else { String::new() };
        let tag = (ap.single && fmt == Fmt::JsonLines).then(|| json_str(stmt));
        let wrap = Wrap { head: &head, tag: tag.as_deref(), base: total, lenient: true };
        let n = results + 1;
        let mut own = Lazy::new(part_of(&numbered(path, n)));
        let out: &mut Lazy = if ap.single { &mut shared } else { &mut own };
        *open = Some(out.tmp.clone());
        let Some((rows, _)) = stream_conn(&mut conn, stmt, &ap.plan, out, count, cancel, &wrap).await? else {
            *open = None;
            continue;
        };
        out.write_all(result_tail(fmt, ap.single, rows).as_bytes()).map_err(io)?;
        out.ensure().map_err(io)?;
        if !ap.single {
            out.flush().map_err(io)?;
            parts.push((out.tmp.clone(), numbered(path, n)));
        } else if first {
            parts.push((shared.tmp.clone(), path.to_path_buf()));
        }
        *open = None;
        results += 1;
        total += rows;
        count.store(total, Ordering::Relaxed);
    }
    if results == 0 {
        return Err("No statement in the editor returns rows.".into());
    }
    if ap.single {
        shared.write_all(single_close(fmt).as_bytes()).map_err(io)?;
        shared.flush().map_err(io)?;
    }
    let _ = conn.close().await;
    Ok(total)
}

impl Worker {
    /// Runs a whole script's statements and streams every result to files, answering with `SqlExport`.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_export_all(&self, req: u64, db: Db, schema: String, stmts: Vec<String>, ap: AllPlan, path: PathBuf, count: Arc<AtomicU64>, step: Arc<AtomicU64>, id: Arc<AtomicU64>, cancel: Arc<AtomicBool>) {
        self.spawn(async move {
            let res = export_all(&db, &schema, &stmts, &ap, &path, &count, &step, &cancel, &id).await;
            id.store(0, Ordering::SeqCst);
            Msg::SqlExport { req, res }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::doc;

    fn cols() -> Vec<CopyCol> {
        vec![CopyCol { name: "id".into(), src: "id".into(), numeric: true }, CopyCol { name: "name".into(), src: "name".into(), numeric: false }]
    }

    /// What `run_all` writes for the results of `sets` in a single file, built from the same pieces.
    fn single(fmt: Fmt, sets: &[(&str, Vec<Document>)]) -> String {
        let mut out = String::new();
        for (i, (stmt, rows)) in sets.iter().enumerate() {
            let tag = (fmt == Fmt::JsonLines).then(|| json_str(stmt));
            out += &single_head(fmt, i + 1, stmt, i == 0);
            let mut w = Writer::new(fmt, true, cols(), "t", &[], true);
            out += &w.begin();
            let text = w.rows(rows);
            out += &match &tag { Some(t) => tag_lines(&text, t), None => text };
            out += &w.end();
            out += result_tail(fmt, true, rows.len() as u64);
        }
        out + single_close(fmt)
    }

    fn sets() -> Vec<(&'static str, Vec<Document>)> {
        vec![("SELECT id, name FROM a", vec![doc! { "id": 1_i64, "name": "x" }, doc! { "id": 2_i64, "name": "y\"z" }]), ("SELECT id, name FROM b WHERE 0", vec![]), ("SELECT 1 <b>", vec![doc! { "id": 3_i64, "name": "w" }])]
    }

    #[test]
    fn a_script_with_a_write_is_refused_before_anything_runs() {
        for w in ["SELECT 1; DELETE FROM t", "USE a; SELECT 1; UPDATE t SET a = 1", "SELECT 1; SET SESSION transaction_read_only = 0; SELECT 2", "SELECT 1; SELECT * FROM t INTO OUTFILE '/tmp/x'"] {
            let why = check_script(w).unwrap_err();
            assert!(why.contains("nothing was exported"), "{w}: {why}");
        }
        assert!(check_script("SELECT 1; DELETE FROM t").unwrap_err().starts_with("Statement 2 "));
        assert!(check_script("  ;; ").is_err());
        let ok = check_script("USE shop; SET @n = 3; SELECT @n; SELECT 'a;b'").unwrap();
        assert_eq!(ok, ["USE shop", "SET @n = 3", "SELECT @n", "SELECT 'a;b'"]);
    }

    #[test]
    fn results_are_numbered_beside_the_chosen_name() {
        let p = std::path::Path::new("/tmp/out/report.csv");
        assert_eq!(numbered(p, 2), std::path::Path::new("/tmp/out/report-2.csv"));
        assert_eq!(numbered(std::path::Path::new("/tmp/raw"), 1), std::path::Path::new("/tmp/raw-1"));
        assert_eq!(part_of(p), std::path::Path::new("/tmp/out/.report.csv.part"));
    }

    #[test]
    fn only_formats_with_room_for_several_results_go_in_one_file() {
        for f in [Fmt::SqlInsert, Fmt::JsonLines, Fmt::Json, Fmt::Markdown, Fmt::Html] {
            assert!(can_single(f));
        }
        for f in [Fmt::Csv, Fmt::Tsv, Fmt::Values, Fmt::SqlUpdate] {
            assert!(!can_single(f));
        }
    }

    #[test]
    fn single_json_is_an_array_of_statement_and_rows() {
        let v: serde_json::Value = serde_json::from_str(&single(Fmt::Json, &sets())).unwrap();
        let a = v.as_array().unwrap();
        assert_eq!(a.len(), 3);
        assert_eq!(a[0]["statement"], "SELECT id, name FROM a");
        assert_eq!(a[0]["rows"].as_array().unwrap().len(), 2);
        assert_eq!(a[0]["rows"][1]["name"], "y\"z");
        assert_eq!(a[1]["rows"].as_array().unwrap().len(), 0);
        assert_eq!(a[2]["rows"][0]["id"], 3);
    }

    #[test]
    fn single_json_lines_rows_carry_their_statement() {
        let text = single(Fmt::JsonLines, &sets());
        let lines: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        // The empty result writes no line.
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["statement"], "SELECT id, name FROM a");
        assert_eq!(lines[1]["statement"], "SELECT id, name FROM a");
        assert_eq!(lines[1]["name"], "y\"z");
        assert_eq!(lines[2]["statement"], "SELECT 1 <b>");
        assert_eq!(lines[2]["id"], 3);
    }

    #[test]
    fn single_sql_markdown_and_html_get_a_section_per_statement() {
        let sql = single(Fmt::SqlInsert, &sets());
        assert_eq!(sql.matches("-- Statement ").count(), 3);
        assert_eq!(sql.matches("INSERT INTO `t`").count(), 3);
        assert!(sql.contains("-- Statement 3\nINSERT"));
        let md = single(Fmt::Markdown, &sets());
        assert_eq!(md.matches("## Statement ").count(), 3);
        assert!(md.contains("```sql\nSELECT id, name FROM a\n```"));
        let html = single(Fmt::Html, &sets());
        assert_eq!(html.matches("<h2>Statement ").count(), 3);
        assert_eq!(html.matches("</table>").count(), 3);
        // The statement text is escaped.
        assert!(html.contains("<pre>SELECT 1 &lt;b&gt;</pre>"));
    }
}
