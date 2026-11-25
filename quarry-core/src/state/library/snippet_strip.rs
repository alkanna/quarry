//! State behind src/app/library/snippet_strip.rs, which draws it.

use std::ops::Range;

/// The text of a tab that holds values: the SQL Query editor, the Shell editor, or the Documents bar's filter, sort and
/// projection, read as one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Src {
    Query,
    Shell,
    Bar,
}

pub struct MakeValue {
    pub uid: u64,
    pub src: Src,
    pub range: Range<usize>,
    pub literal: String,
    pub name: String,
    pub ty: String,
    pub default: Option<String>,
    pub keep: bool,
}
