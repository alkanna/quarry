//! State behind src/app/sql/explain_tree.rs, which draws it.

use crate::db::sql::plan::Severity;

/// What a row shows beside its numbers: the name screen readers and tests find it by, its second line and the mark of
/// its most severe finding.
pub struct TreeRow {
    pub name: String,
    pub sub: Option<String>,
    pub mark: Option<Severity>,
}
