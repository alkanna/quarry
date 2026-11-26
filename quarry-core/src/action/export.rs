//! The export streams the app's Export dialog starts. The dialog's form holds the connection handles and the rows, so
//! they are not actions: `Front::export()` hands the app these four, and Core keeps the job and the worker call.

use crate::core::Core;
use crate::db::{self, Msg};
use crate::state::export::{AllRows, ExportJob};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

impl Core {
    /// Streams a Query result's statement to `sink` in a worker task, tracked in the status bar.
    pub fn export_stream(&mut self, a: &AllRows, plan: db::sql::export::Plan, sink: db::sql::export::Sink, copy: bool) {
        if self.export_job.is_some() {
            return self.warn("An export is already running.");
        }
        self.req += 1;
        let job = ExportJob { req: self.req, count: Arc::new(AtomicU64::new(0)), cancel: Default::default(), id: Arc::new(AtomicU64::new(0)), db: Some(a.db.clone()), copy, step: Arc::new(AtomicU64::new(0)), of: 0 };
        self.worker.sql_export(job.req, a.db.clone(), a.schema.clone(), a.stmt.clone(), plan, sink, job.count.clone(), job.id.clone(), job.cancel.clone());
        self.export_job = Some(job);
    }

    /// Writes Shell rows to `path` on a blocking thread, tracked in the status bar like a stream. `rows` is a copy of the
    /// bytes, freed when the export ends, so the result stays on screen meanwhile.
    pub fn export_shell(&mut self, rows: Vec<mongodb::bson::RawDocumentBuf>, out: db::RowsOut, path: PathBuf) {
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
    pub fn export_script(&mut self, sc: &crate::state::export::Script, stmts: Vec<String>, ap: db::sql::export::AllPlan, path: PathBuf) {
        if self.export_job.is_some() {
            return self.warn("An export is already running.");
        }
        self.req += 1;
        let job = ExportJob { req: self.req, count: Arc::new(AtomicU64::new(0)), cancel: Default::default(), id: Arc::new(AtomicU64::new(0)), db: Some(sc.db.clone()), copy: false, step: Arc::new(AtomicU64::new(0)), of: stmts.len() };
        self.worker.sql_export_all(job.req, sc.db.clone(), sc.schema.clone(), stmts, ap, path, job.count.clone(), job.step.clone(), job.id.clone(), job.cancel.clone());
        self.export_job = Some(job);
    }

    /// The status bar's Cancel of a running stream: stops the statement on the server, or the writer thread.
    pub fn export_cancel(&mut self) {
        if let Some(j) = &self.export_job {
            match &j.db {
                Some(db) => self.worker.sql_kill(db.clone(), j.id.load(Ordering::SeqCst), j.cancel.clone()),
                None => j.cancel.store(true, Ordering::Relaxed),
            }
        }
    }
}
