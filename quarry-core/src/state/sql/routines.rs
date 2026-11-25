//! State behind src/app/sql/routines.rs, which draws it.

use crate::db::sql::routines::{Arg, Detail, Kind, Plan, Routine};
use std::collections::{HashMap, HashSet};
use crate::db;

/// What the sidebar knows of one schema's routines.
pub enum Loaded {
    Wait,
    Ok(Vec<Routine>),
    Err(String),
}

#[derive(Default)]
pub struct Routines {
    pub lists: HashMap<(u64, String), Loaded>,
    pub folded: HashSet<(u64, String)>,
    pub win: Option<RoutineWin>,
}

impl Routines {
    pub fn window_open(&self) -> bool {
        self.win.is_some()
    }
}

#[derive(Default, PartialEq, Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub enum Pane {
    #[default]
    View,
    Run,
    Drop,
}

/// The preview of what a save sends, kept while the text and the drop-and-create choice stay as they are.
pub type Memo = (String, bool, Result<Plan, String>);

pub struct RoutineWin {
    pub conn: u64,
    /// The sidebar's database, and its namespace: the schema on PostgreSQL, the database again on MySQL.
    pub db: String,
    pub schema: String,
    pub dialect: db::sql::Dialect,
    /// A schema that belongs to the server: shown, never changed.
    pub system: bool,
    pub kind: Kind,
    /// None while a new routine is being written.
    pub routine: Option<Routine>,
    pub detail: Option<Detail>,
    /// The read in flight.
    pub req: u64,
    /// A save or a drop in flight.
    pub busy: bool,
    pub error: Option<String>,
    pub text: String,
    pub base: String,
    pub editing: bool,
    pub recreate: bool,
    /// PostgreSQL refused to replace the routine in place, so the window offers a drop and create.
    pub offer_recreate: bool,
    /// The text before the last save and whether that save dropped and created, for Undo.
    pub undo: Option<(String, bool)>,
    pub pane: Pane,
    pub typed: String,
    pub args: Vec<Arg>,
    pub memo: Option<Memo>,
}

impl RoutineWin {
    pub fn is(&self, conn: u64, db: &str, schema: &str, r: &Routine) -> bool {
        self.conn == conn && self.db == db && self.schema == schema && self.routine.as_ref().is_some_and(|x| x.key() == r.key())
    }

    pub fn dirty(&self) -> bool {
        self.text.trim() != self.base.trim()
    }

    pub fn title(&self) -> String {
        match &self.routine {
            Some(r) => r.label(self.dialect),
            None => format!("New {}", self.kind.noun()),
        }
    }
}
