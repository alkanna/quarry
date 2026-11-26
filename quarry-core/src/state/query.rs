//! State behind src/app/query.rs, which draws it.

/// Where the query box's caret is, for the server lookups of field values.
#[derive(Default)]
pub struct Typed {
    /// The field whose value the caret is in.
    pub field: Option<String>,
    /// Set when the box was edited this frame: the text typed so far in a string value, if any.
    pub edit: Option<Option<String>>,
}
