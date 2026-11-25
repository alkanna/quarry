//! State behind src/app/sql/alter.rs, which draws it.

use crate::db::sql::ddl::{Change, ColDef, IndexDef, Mode, Plan, Pos};

#[derive(Clone, Copy, PartialEq)]
pub enum DMode {
    None,
    Null,
    Value,
    Expr,
    /// The server computes the column from an expression, which `dtext` holds.
    Generated,
}

/// The Add column and Edit column dialog.
pub struct ColForm {
    /// The column's name now; None adds a new one.
    pub from: Option<String>,
    pub def: ColDef,
    /// What the form opened with, so "nothing changed" is known.
    pub orig: Option<(ColDef, Pos)>,
    pub dmode: DMode,
    pub dtext: String,
    /// A generated column is STORED rather than VIRTUAL.
    pub stored: bool,
    pub pos: Pos,
    /// The staged changes this one goes after.
    pub rest: Vec<Change>,
    /// The other columns in the order the staged changes leave them, for the position list.
    pub order: Vec<String>,
    pub memo: Option<PlanMemo>,
}

pub struct IdxForm {
    /// The index being edited; None adds one.
    pub from: Option<IndexDef>,
    pub def: IndexDef,
    /// The columns of the table with their types, in order.
    pub avail: Vec<(String, String)>,
    pub rest: Vec<Change>,
    pub view: Option<IdxView>,
    /// The INCLUDE columns as typed, comma separated, which `def.include` follows.
    pub include_text: String,
}

/// The checks, the plan and the warnings of the index as it stands, kept until the definition changes.
pub struct IdxView {
    pub def: IndexDef,
    pub changes_empty: bool,
    /// The staged changes with this one added, which Stage change keeps.
    pub all: Vec<Change>,
    pub text: Result<String, String>,
    pub warnings: Vec<String>,
}

/// The plan and the warnings of a list of changes, kept while the changes, the table definition and the session
/// mode stay as they are. Both walk the whole structure and a bar or dialog draws them every frame.
pub struct PlanMemo {
    pub changes: Vec<Change>,
    pub create: String,
    pub mode: Mode,
    pub warnings: Vec<String>,
    pub plan: Result<Plan, String>,
}
