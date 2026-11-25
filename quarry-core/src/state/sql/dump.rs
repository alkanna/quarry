//! State behind src/app/sql/dump.rs, which draws it.

use crate::db::sql::csv::Source;
use crate::db::sql::import::Progress;
use std::sync::atomic::{AtomicBool, AtomicU64};
use crate::db;
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq)]
pub enum JobKind {
    Import,
    Script,
}

/// A CSV import or a script file running, with what Cancel needs.
pub struct SqlJob {
    pub req: u64,
    pub conn: u64,
    pub db: db::sql::Db,
    pub schema: String,
    pub kind: JobKind,
    pub prog: Arc<Progress>,
    pub id: Arc<AtomicU64>,
    pub cancel: Arc<AtomicBool>,
    pub total_rows: u64,
    pub total_bytes: u64,
    /// False for an import into a table whose engine has no transactions, where Cancel keeps what was written.
    pub rolls_back: bool,
}

impl SqlJob {
    pub fn new(req: u64, conn: u64, db: db::sql::Db, schema: String, kind: JobKind, total_rows: u64, total_bytes: u64) -> SqlJob {
        SqlJob { req, conn, db, schema, kind, prog: Arc::new(Progress::default()), id: Arc::new(AtomicU64::new(0)), cancel: Arc::new(AtomicBool::new(false)), total_rows, total_bytes, rolls_back: true }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum What {
    /// The rows the Content view shows, with no structure.
    Filtered,
    Selected,
    Whole,
}

pub struct Row {
    pub name: String,
    pub view: bool,
    pub on: bool,
    pub structure: bool,
    pub content: bool,
    pub drop: bool,
    pub rows: Option<u64>,
    /// Data and indexes on disk, once the server said.
    pub bytes: Option<u64>,
}

pub struct DumpForm {
    pub conn: u64,
    pub db: String,
    /// The schema of a PostgreSQL database the table list is of; empty on MySQL, where the database is the namespace.
    pub schema: String,
    pub conn_name: String,
    pub what: What,
    /// The open table with a filter: its name, the WHERE text and the rows it matches.
    pub filtered: Option<(String, String, u64)>,
    pub rows: Vec<Row>,
    pub extended: bool,
    pub snapshot: bool,
    pub extras: bool,
    /// PostgreSQL only. Both off by default: they name roles, and a restore where one is missing stops there.
    pub grants: bool,
    pub owners: bool,
    pub path: String,
    /// The request the table sizes answer.
    pub size_req: u64,
}

pub struct RunForm {
    pub conn: u64,
    pub db: String,
    /// The schema a PostgreSQL file's unqualified names land in (its search_path); empty on MySQL.
    pub schema: String,
    pub conn_name: String,
    pub src: Source,
    pub name: String,
}

#[cfg(any(test, feature = "test-support"))]
impl DumpForm {
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn whole(&self) -> bool {
        self.what == What::Whole
    }
}
