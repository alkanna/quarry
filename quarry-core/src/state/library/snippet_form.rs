//! State behind src/app/library/snippet_form.rs, which draws it.

use crate::library::snippet::self;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Fill,
    /// A write on a writable connection, waiting for Run.
    Confirm,
}

pub struct SnippetForm {
    pub id: u64,
    pub name: String,
    /// The script's kind (sql, shell or filter), its text, and for a filter its sort, projection and collection.
    pub kind: String,
    pub body: String,
    pub sort: String,
    pub project: String,
    pub coll: String,
    /// None while the snippet's own connection was removed and the person has not picked another.
    pub conn: Option<u64>,
    pub db: String,
    pub schema: String,
    pub values: snippet::Values,
    pub stage: Stage,
    /// The palette's query, put back by Esc. None when the form was opened from elsewhere, where Esc just closes it.
    pub query: Option<String>,
    /// The filled statement, whether it writes and its verb, kept until a value or the target changes: the read-only
    /// check lexes the text several times, too much for every frame.
    pub memo: Option<(u64, Result<String, String>, bool, String)>,
    /// The target menu's filter, cleared when the menu closes.
    pub target_q: String,
}

impl SnippetForm {
    /// The texts that hold values: a filter's sort and projection too.
    pub fn parts(&self) -> Vec<&str> {
        if self.kind == "filter" { vec![&self.body, &self.sort, &self.project] } else { vec![&self.body] }
    }
}
