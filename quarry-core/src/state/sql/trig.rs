//! State behind src/app/sql/trig.rs, which draws it.

use crate::db::sql::ddl::{self, TrigDef};
use crate::db::sql::structure::Trigger;

/// What the editor shows for a draft, kept while the draft, the live trigger, the session mode and the columns stay as they are.
pub struct TrigMemo {
    pub key: String,
    pub def: TrigDef,
    pub live: Option<Trigger>,
    pub n_trigs: usize,
    pub mode: ddl::Mode,
    pub cols: Vec<String>,
    pub dirty: bool,
    /// None while the draft is the live trigger.
    pub steps: Option<Result<Vec<String>, String>>,
    pub script: String,
    pub pg_warnings: Vec<String>,
}

impl TrigMemo {
    pub fn holds(&self, key: &str, live: Option<&Trigger>, n_trigs: usize, mode: ddl::Mode, cols: &[&str], def: &TrigDef) -> bool {
        self.key == key && self.live.as_ref() == live && self.n_trigs == n_trigs && self.mode == mode && self.cols.iter().map(String::as_str).eq(cols.iter().copied()) && self.def == *def
    }
}
