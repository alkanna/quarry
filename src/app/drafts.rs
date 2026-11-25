//! Unsaved document edits. Staging and saving them run in Core (`quarry_core::action::docs`); these forward for the
//! views that show them.

use super::*;
pub(super) use quarry_core::core::drafts::{stage_draft, stored_version};

impl App {
    pub(super) fn selected_doc(&self) -> Option<&Document> {
        self.core.selected_doc()
    }

    pub(super) fn draft_key(&self, d: &Document) -> String {
        self.core.draft_key(d)
    }

    pub(super) fn shown_doc(&self, d: &Document, agg: bool) -> Option<Document> {
        self.core.shown_doc(d, agg)
    }

    pub(super) fn shown_row(&self, rows: Rows, i: usize, agg: bool) -> Option<Document> {
        self.core.shown_row(rows, i, agg)
    }

    pub(super) fn tab_drafts(&self) -> Vec<String> {
        self.core.tab_drafts()
    }
}
