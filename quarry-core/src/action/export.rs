//! The export streams the Export dialog starts. The dialog's form names the connection (`AllRows`, `Script`); Core looks the
//! handle up and, for Shell rows, reads them from the tab, so the actions carry only what the user chose.

use super::{Refusal, Reply, UiFx};
use crate::core::Core;
use crate::db::{self, Msg};
use crate::state::export::{human_rows, AllRows, ExportJob, Script};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ExportA {
    /// Streams a Query result's statement, run again, to a file or the clipboard (`copy`).
    Stream { all: AllRows, plan: db::sql::export::Plan, sink: db::sql::export::Sink, copy: bool },
    /// Writes the Shell rows of tab `tab` to `path`.
    Shell { tab: u64, out: db::RowsOut, path: PathBuf },
    /// Runs the editor's statements and streams their results to files.
    Script { script: Script, plan: db::sql::export::AllPlan, path: PathBuf },
    /// The status bar's Cancel of the running export.
    Cancel,
}

impl Core {
    pub(super) fn export_act(&mut self, a: ExportA) -> Result<Reply, Refusal> {
        match a {
            ExportA::Stream { all, plan, sink, copy } => {
                let Some(db) = self.export_db(all.conn, &all.database) else { return Err(self.refuse_offline(all.conn)) };
                self.export_stream(db, &all, plan, sink, copy);
            }
            ExportA::Shell { tab, out, path } => {
                self.cur_tab(tab)?;
                let rows = self.t().map(|t| t.shell.docs.iter().map(|r| r.raw().to_owned()).collect()).unwrap_or_default();
                self.export_shell(rows, out, path);
            }
            ExportA::Script { script, plan, path } => {
                let Ok(stmts) = script.stmts.clone() else { return Ok(Reply::Done) };
                let Some(db) = self.export_db(script.conn, &script.database) else { return Err(self.refuse_offline(script.conn)) };
                self.export_script(db, &script, stmts, plan, path);
            }
            ExportA::Cancel => self.export_cancel(),
        }
        Ok(Reply::Done)
    }

    /// The handle of connection `conn` pointed at `database`, as the dialog's statement ran in it.
    fn export_db(&self, conn: u64, database: &str) -> Option<db::sql::Db> {
        self.conn(conn)?.sql.as_ref().map(|d| d.at(database))
    }

    /// Streams a Query result's statement to `sink` in a worker task, tracked in the status bar.
    fn export_stream(&mut self, db: db::sql::Db, a: &AllRows, plan: db::sql::export::Plan, sink: db::sql::export::Sink, copy: bool) {
        if self.export_job.is_some() {
            return self.warn("An export is already running.");
        }
        self.req += 1;
        let job = ExportJob { req: self.req, count: Arc::new(AtomicU64::new(0)), cancel: Default::default(), id: Arc::new(AtomicU64::new(0)), db: Some(db.clone()), copy, step: Arc::new(AtomicU64::new(0)), of: 0 };
        self.worker.sql_export(job.req, db, a.schema.clone(), a.stmt.clone(), plan, sink, job.count.clone(), job.id.clone(), job.cancel.clone());
        self.export_job = Some(job);
    }

    /// Writes Shell rows to `path` on a blocking thread, tracked in the status bar like a stream. `rows` is a copy of the
    /// bytes, freed when the export ends, so the result stays on screen meanwhile.
    fn export_shell(&mut self, rows: Vec<mongodb::bson::RawDocumentBuf>, out: db::RowsOut, path: PathBuf) {
        if self.export_job.is_some() {
            return self.warn("An export is already running.");
        }
        self.req += 1;
        let job = ExportJob { req: self.req, count: Arc::new(AtomicU64::new(0)), cancel: Default::default(), id: Arc::new(AtomicU64::new(0)), db: None, copy: false, step: Arc::new(AtomicU64::new(0)), of: 0 };
        let (req, count, cancel) = (job.req, job.count.clone(), job.cancel.clone());
        self.worker.blocking(move || Msg::SqlExport { req, res: db::export_rows(&rows, &out, &path, &count, &cancel) });
        self.export_job = Some(job);
    }

    /// Runs the editor's statements in a worker task and streams their results to files.
    fn export_script(&mut self, db: db::sql::Db, sc: &Script, stmts: Vec<String>, ap: db::sql::export::AllPlan, path: PathBuf) {
        if self.export_job.is_some() {
            return self.warn("An export is already running.");
        }
        self.req += 1;
        let job = ExportJob { req: self.req, count: Arc::new(AtomicU64::new(0)), cancel: Default::default(), id: Arc::new(AtomicU64::new(0)), db: Some(db.clone()), copy: false, step: Arc::new(AtomicU64::new(0)), of: stmts.len() };
        self.worker.sql_export_all(job.req, db, sc.schema.clone(), stmts, ap, path, job.count.clone(), job.step.clone(), job.id.clone(), job.cancel.clone());
        self.export_job = Some(job);
    }

    /// The status bar's Cancel of a running stream: stops the statement on the server, or the writer thread.
    fn export_cancel(&mut self) {
        if let Some(j) = &self.export_job {
            match &j.db {
                Some(db) => self.worker.sql_kill(db.clone(), j.id.load(Ordering::SeqCst), j.cancel.clone()),
                None => j.cancel.store(true, Ordering::Relaxed),
            }
        }
    }

    /// The finished stream: a file is reported, clipboard text is copied. Another job's answer (a dump) is not this one's.
    pub(super) fn export_done(&mut self, req: u64, res: db::Res<db::sql::export::Done>) {
        if self.export_job.take_if(|j| j.req == req).is_none() {
            return;
        }
        match res {
            Ok(db::sql::export::Done { rows, path: Some(p), files, .. }) if files > 1 => self.ok(format!("Exported {} rows to {files} files, from {}", human_rows(rows), p.display())),
            Ok(db::sql::export::Done { rows, path: Some(p), .. }) => self.ok(format!("Exported {} rows to {}", human_rows(rows), p.display())),
            Ok(db::sql::export::Done { rows, text: Some(t), .. }) => {
                self.ui_fx.push(UiFx::CopyText(t));
                self.ok(format!("Copied {} rows", human_rows(rows)));
            }
            Ok(_) => {}
            Err(e) if e == "Export cancelled" => self.ok("Export stopped"),
            Err(e) => self.err(e),
        }
    }
}
