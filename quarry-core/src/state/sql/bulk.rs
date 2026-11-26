//! State behind src/app/sql/bulk.rs, which draws it.

use crate::db::sql::bulk::BulkPrep;
use crate::state::Sel;

/// One `column = value` of an update. `col` is the column's `Col::name`.
#[derive(Clone, Default)]
pub struct Pair {
    pub col: String,
    pub text: String,
    pub null: bool,
}

pub struct BulkForm {
    pub update: bool,
    pub sel: Sel,
    /// The namespace the page was read in, which the count and the write use too.
    pub ns: String,
    /// The WHERE of the last page asked for, kept from when the dialog opened: the grid may reload while it is open.
    pub filter: String,
    /// The last page had a LIMIT or OFFSET chip, which the write does not follow.
    pub capped: bool,
    pub req: u64,
    pub prep: Option<BulkPrep>,
    pub pairs: Vec<Pair>,
    pub typed: String,
}
