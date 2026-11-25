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

use crate::state::Sel;

/// What an answer does to the tabs of its table: (clear the staged changes, reload the structure). A done ALTER
/// leaves the staged changes written against a structure that is gone. A trigger answer leaves them as they are,
/// and reloads even after a failure, which on MySQL may have come after the old trigger was dropped and put back.
/// Whether the answer of a structure change, as (connection, PostgreSQL database or empty, namespace, table), is about the
/// table `sel` points at. `target` is what a read of the tab resolves to: the database of its pool and its namespace, which
/// is not `sel.schema` when a PostgreSQL tab leaves its schema to the one the sidebar shows.
pub fn answers_tab(sel: &Sel, target: Option<(String, String)>, answer: (u64, &str, &str, &str)) -> bool {
    let (conn, database, schema, table) = answer;
    sel.conn == conn && sel.coll == table && target.is_some_and(|(d, ns)| ns == schema && d == database)
}

/// Whether `sel` could be the tab that sent an answer no tab resolves to: its connection and table, and a database and schema
/// that do not contradict the answer. An empty `sel.db` is the PostgreSQL connection's own database, an empty `sel.schema`
/// the sidebar's pick. Only used to turn Apply back on when no tab matched; a near miss can only enable Apply early on a tab of
/// a same-named table with its own change running, which is rare.
pub fn may_answer_tab(sel: &Sel, answer: (u64, &str, &str, &str)) -> bool {
    let (conn, database, schema, table) = answer;
    // MySQL answers name the database as the namespace and leave the PostgreSQL database empty.
    let db = if database.is_empty() { schema } else { database };
    sel.conn == conn && sel.coll == table && (sel.db.is_empty() || sel.db == db) && (database.is_empty() || sel.schema.is_empty() || sel.schema == schema)
}

/// What an answer does to the open tabs of its table: (clear the staged changes, reload the structure). An answer that does
/// not know the outcome (a COMMIT that failed) clears and reloads like a success, since the staged changes may already be
/// applied and what is on screen may be stale.
pub fn answer_effects(ok: bool, alter: bool, unsure: bool) -> (bool, bool) {
    ((ok || unsure) && alter, ok || !alter || unsure)
}

pub fn plural_changes(n: usize) -> String {
    format!("{n} structure change{}", if n == 1 { "" } else { "s" })
}

/// Shown under a disabled Apply now button.
pub const BUSY_NOTE: &str = "A structure change is still running.";
