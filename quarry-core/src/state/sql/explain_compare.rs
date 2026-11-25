//! State behind src/app/sql/explain_compare.rs, which draws it.

use crate::db::sql::plan::compare::Diff;
use crate::db::sql::plan::Plan;

/// The plan this one is compared with and when it was made.
pub struct Prev {
    pub plan: std::sync::Arc<Plan>,
    /// Unix seconds.
    pub at: u64,
    pub diff: Diff,
}
