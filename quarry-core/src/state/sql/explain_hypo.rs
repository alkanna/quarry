//! State behind src/app/sql/explain_hypo.rs, which draws it.

use crate::db::sql::plan::{compare, Plan};

/// The answer of a trial, kept for the candidate it tried.
pub struct HypoRun {
    /// The candidate's SQL, so the line shows under that candidate only.
    pub fix: String,
    pub plan: Plan,
    /// The plan reads through the hypothetical index.
    pub used: bool,
    /// From the dialog's plan to the trial's, for Compare.
    pub diff: compare::Diff,
}
