//! State behind src/app/sql/import.rs, which draws it.

use crate::db::sql::csv::{self, DateFmt, Enc, Mapping, Opts, Report, Source, TableInfo};
use crate::db::sql::json;
use crate::db::sql::import::Dup;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use crate::db;
use std::sync::Arc;

/// The file being imported and everything read from it.
pub struct FileState {
    /// What the checks and the import read: the file itself for CSV, the CSV a JSON file was flattened to.
    pub src: Source,
    /// What the person gave, which Back and an unlock open again.
    pub origin: Source,
    pub name: String,
    /// Bytes of `src`, which the progress is counted in, and of `origin`, which the dialog shows.
    pub size: u64,
    pub shown_size: u64,
    /// Set for JSON and JSON Lines.
    pub json: Option<JsonFile>,
    /// A JSON file is read and flattened in a thread of its own, which sends the finished state here. Until then this
    /// one holds only the name, and the dialog shows that it is reading.
    pub load: Option<mpsc::Receiver<Result<FileState, String>>>,
    /// Why reading the JSON failed.
    pub failed: Option<String>,
    pub opts: Opts,
    pub header: bool,
    /// What the encoding looked like when the file was opened, shown beside the choice.
    pub detected: Enc,
    pub names: Vec<String>,
    /// The first records after the header, for the values column and the date reading.
    pub sample: Vec<Vec<String>>,
    pub map: Mapping,
    /// Hash of what the sample was read with, and of what the check was run for.
    pub read_key: u64,
    pub check_key: u64,
    pub report: Option<Report>,
    pub rx: Option<(u64, mpsc::Receiver<(u64, Report)>)>,
    pub cancel: Arc<AtomicBool>,
    pub epoch: u64,
}

pub struct ImportForm {
    pub conn: u64,
    /// The database on MySQL, the PostgreSQL database of the connection otherwise.
    pub db: String,
    /// The PostgreSQL schema of the table, empty on MySQL where the database is the whole namespace.
    pub schema: String,
    pub table: String,
    pub req: u64,
    pub info: Option<Result<TableInfo, String>>,
    pub file: FileState,
    pub dup: Dup,
    /// The mapping chosen before an unlock reopened the form, put back once the table's columns are read again.
    pub restore: Option<csv::Mapping>,
}

/// What a JSON file said about itself when it was flattened.
pub struct JsonFile {
    /// One object per line rather than one array.
    pub lines: bool,
    /// By key: whether any row holds an object or array there, which is written as JSON text.
    pub nested: Vec<bool>,
}

impl FileState {
    pub fn blank(origin: &Source, src: Source, name: String, opts: Opts, header: bool, json: Option<JsonFile>) -> FileState {
        FileState { size: src.len(), shown_size: origin.len(), origin: origin.clone(), src, json, load: None, failed: None, name, opts, header, detected: opts.enc, names: Vec::new(), sample: Vec::new(), map: Mapping { map: Vec::new(), fmts: Vec::new(), empty_null: true }, read_key: u64::MAX, check_key: u64::MAX, report: None, rx: None, cancel: Arc::new(AtomicBool::new(false)), epoch: 0 }
    }

    /// `name` is the file's name; None for pasted text, which is named by what it turned out to be. CSV is sniffed on
    /// the first 64 KB, which is quick. JSON has to be read whole, so it comes back still loading, with its thread running.
    pub fn open(origin: Source, name: Option<String>) -> Result<FileState, String> {
        let head = origin.head(64 << 10).map_err(|e| format!("Could not read the file: {e}"))?;
        let is_json = json::looks_like_json(name.as_deref().unwrap_or(""), &head);
        let name = name.unwrap_or_else(|| if is_json { "Pasted JSON" } else { "Pasted CSV" }.into());
        if is_json {
            let (tx, rx) = mpsc::channel();
            let mut pending = FileState::blank(&origin, origin.clone(), name.clone(), json_opts(), true, None);
            pending.load = Some(rx);
            std::thread::spawn(move || {
                let _ = tx.send(FileState::load_json(origin, name));
            });
            return Ok(pending);
        }
        let (opts, header) = csv::sniff(&origin)?;
        let mut f = FileState::blank(&origin, origin.clone(), name, opts, header, None);
        f.resample()?;
        Ok(f)
    }

    /// Reads and flattens a JSON file, the slow part of opening one.
    pub fn load_json(origin: Source, name: String) -> Result<FileState, String> {
        let flat = json::flatten(&json::read_text(&origin)?)?;
        // The flattened text is plain UTF-8 CSV, so the reading options are fixed and the dialog does not offer them.
        let mut f = FileState::blank(&origin, Source::Mem(Arc::new(flat.csv)), name, json_opts(), true, Some(JsonFile { lines: flat.lines, nested: flat.nested }));
        f.resample()?;
        Ok(f)
    }

    /// The finished state of a JSON file once its thread is done, or None while it still reads.
    pub fn poll_load(&mut self) -> Option<Result<FileState, String>> {
        let got = match self.load.as_ref()?.try_recv() {
            Ok(r) => r,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Err("Reading the file stopped before it finished.".to_string()),
        };
        self.load = None;
        Some(got)
    }

    /// Reads the first records again with the options as they are now.
    pub fn resample(&mut self) -> Result<(), String> {
        let rows = csv::sample(&self.src, self.opts, 200)?;
        let width = rows.first().map_or(0, Vec::len);
        (self.names, self.sample) = if self.header && !rows.is_empty() { (rows[0].clone(), rows[1..].to_vec()) } else { ((1..=width).map(|i| format!("Column {i}")).collect(), rows) };
        self.map.map = vec![None; width];
        self.map.fmts = vec![DateFmt::DayFirst; width];
        self.read_key = hash_of(&(self.opts, self.header));
        Ok(())
    }

    pub fn values(&self, ci: usize) -> Vec<&str> {
        self.sample.iter().filter_map(|r| r.get(ci).map(String::as_str)).collect()
    }

    /// Matches columns by name and reads each date column the way its values say.
    pub fn remap(&mut self, info: &TableInfo) {
        self.map.map = csv::auto_map(&self.names, &info.cols);
        for ci in 0..self.map.map.len() {
            self.refmt(ci, info);
        }
    }

    pub fn refmt(&mut self, ci: usize, info: &TableInfo) {
        if let Some(t) = self.map.map[ci] && matches!(info.cols[t].kind, db::sql::value::Kind::Date | db::sql::value::Kind::DateTime) {
            self.map.fmts[ci] = csv::guess_date_fmt(&self.values(ci));
        }
    }

    /// Starts the check again when anything it depends on changed. `wake` redraws once the check is done.
    pub fn recheck(&mut self, info: &TableInfo, wake: impl Fn() + Send + 'static) {
        let key = hash_of(&(self.opts, self.header, &self.map.map, &self.map.fmts, self.map.empty_null));
        if key == self.check_key {
            return;
        }
        self.check_key = key;
        self.cancel.store(true, Ordering::Relaxed);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.report = None;
        self.epoch += 1;
        let (tx, rx) = mpsc::channel();
        self.rx = Some((self.epoch, rx));
        let (src, opts, header, cols, map, width, cancel, epoch) = (self.src.clone(), self.opts, self.header, info.cols.clone(), self.map.clone(), self.names.len(), self.cancel.clone(), self.epoch);
        std::thread::spawn(move || {
            let rep = csv::analyze(&src, opts, header, &cols, &map, width, &cancel);
            if !rep.cancelled {
                let _ = tx.send((epoch, rep));
                wake();
            }
        });
    }

    pub fn poll(&mut self) {
        if let Some((epoch, rx)) = &self.rx && let Ok((g, rep)) = rx.try_recv() && g == *epoch {
            self.report = Some(rep);
            self.rx = None;
        }
    }
}

impl Drop for FileState {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub fn hash_of(v: &impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

pub fn json_opts() -> Opts {
    Opts { delim: ',', quote: Some('"'), enc: Enc::Utf8 }
}
