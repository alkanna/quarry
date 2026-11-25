//! State behind src/ui/sql_complete.rs, which draws it.

/// A lookup the caret asks the app to make: the schema of the database, or a column's values.
#[derive(Default, Clone, PartialEq, Debug)]
pub struct Wants {
    pub schema: bool,
    pub values: Option<Need>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Need {
    pub table: String,
    pub column: String,
    /// The text typed so far, when the list of frequent values did not hold it.
    pub prefix: Option<String>,
}
