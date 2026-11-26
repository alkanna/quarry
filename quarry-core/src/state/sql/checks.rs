//! State behind src/app/sql/checks.rs, which draws it.

use crate::db::sql::ddl::Change;
use crate::db::sql::structure::CheckConstraint;

/// The Add check and Edit check dialog.
pub struct CheckForm {
    /// The constraint being edited; None adds one.
    pub from: Option<CheckConstraint>,
    pub def: CheckConstraint,
    /// The staged changes this one goes after.
    pub rest: Vec<Change>,
    pub view: Option<CheckView>,
}

/// The plan and the warnings of the constraint as it stands, kept until the definition changes.
pub struct CheckView {
    pub def: CheckConstraint,
    pub changes_empty: bool,
    /// The staged changes with this one added, which Stage change keeps.
    pub all: Vec<Change>,
    pub text: Result<String, String>,
    pub warnings: Vec<String>,
}
