//! State behind src/app/drafts.rs, which draws it.

use crate::state::{Draft, Sel};

impl Draft {
    pub fn is_in(&self, sel: &Sel) -> bool {
        self.conn == sel.conn && self.db == sel.db && self.schema == sel.schema && self.coll == sel.coll
    }
}
