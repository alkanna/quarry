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

/// Runs `stmt` and writes every row to `out`. Only a statement that passes the read-only check runs, on
/// a read-only session, whatever the connection allows. `count` follows the rows written for a progress readout.
pub async fn stream(db: &Db, schema: &str, stmt: &str, plan: &Plan, out: &mut (dyn Write + Send), count: &AtomicU64, cancel: &AtomicBool, id: &AtomicU64) -> Res<(u64, usize)> {
    lex::read_only_check(stmt).map_err(|why| format!("Export all only runs statements that read: {why}"))?;
    let Pool::My(pool) = &db.pool;
    let mut conn = pool.acquire().await.map_err(e)?.detach();
    prepare(&mut conn, db.dialect, schema, true, id).await.map_err(e)?;
    let io = |err: std::io::Error| if err.to_string() == "too large" { "The text is over 64 MB. Export it to a file instead.".to_string() } else { format!("Could not write: {err}") };
    let mut done = false;
    let (mut cols, mut writer, mut batch, mut rows, mut peak) = (Vec::new(), None::<Writer>, Vec::<Document>::new(), 0u64, 0usize);
    let copy_cols = |c: &[super::Col]| c.iter().map(|c| CopyCol { name: c.name.clone(), src: c.src.clone(), numeric: numeric(c.kind) }).collect::<Vec<_>>();
    {
        let mut s = raw_sql(stmt).fetch_many(&mut conn);
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
                        out.write_all(w.begin().as_bytes()).map_err(io)?;
                        writer = Some(w);
                    }
                    batch.push(to_row(&cols, &row, &[])?.owned());
                    peak = peak.max(batch.len());
                    if batch.len() >= BATCH {
                        let text = writer.as_mut().map(|w| w.rows(&batch)).unwrap_or_default();
                        out.write_all(text.as_bytes()).map_err(io)?;
                        rows += batch.len() as u64;
                        count.store(rows, Ordering::Relaxed);
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
        let d = (&mut conn).describe(stmt).await.map_err(e)?;
        cols = make_cols(d.columns(), &Default::default(), false);
        let mut w = Writer::new(plan.fmt, plan.header, copy_cols(&cols), &plan.table, &[], plan.bs);
        out.write_all(w.begin().as_bytes()).map_err(io)?;
        writer = Some(w);
    }
    if let Some(mut w) = writer {
        out.write_all(w.rows(&batch).as_bytes()).map_err(io)?;
        rows += batch.len() as u64;
        count.store(rows, Ordering::Relaxed);
        // Line formats end with a newline like any text file.
        out.write_all(format!("{}\n", w.end()).as_bytes()).map_err(io)?;
    }
    let _ = conn.close().await;
    Ok((rows, peak))
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
            Ok(Done { rows, #[cfg(test)] peak_batch: _peak, path: None, text: Some(String::from_utf8_lossy(&out.buf).into_owned()) })
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
            res.map(|(rows, _peak)| Done { rows, #[cfg(test)] peak_batch: _peak, path: Some(path), text: None })
        }
    }
}
