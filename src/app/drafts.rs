//! Unsaved document edits: staging, showing and saving them.

use super::*;

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

    /// Keeps an edit as unsaved instead of writing it, so several edits go out in one save.
    pub(super) fn stage(&mut self, d: &Document, new: db::Res<Document>) {
        // Refused before the editors close, so the typed edit is still there once Project is cleared.
        if self.projected() {
            return;
        }
        let new = match new {
            Ok(n) => n,
            Err(e) => return self.err(e),
        };
        let key = self.draft_key(d);
        let Some(sel) = self.t().map(|t| t.sel.clone()) else { return };
        self.core.inline = None;
        self.core.new_field = None;
        self.core.json_edit = None;
        let n = stage_draft(&mut self.core.drafts, &key, &sel, d, new);
        if n == 0 {
            return self.ok("No changes left on this document");
        }
        if self.core.prefs.auto_save && self.sel_writable() {
            self.write_draft(&key);
        } else {
            self.ok(format!("{n} unsaved change(s). Press Save on the document when ready."));
        }
    }

    /// Rows read with a projection hold only some fields: saving one fails its unchanged check, a copy holds only those
    /// fields, and rows the projection left without _id would share one draft.
    pub(super) fn projected(&mut self) -> bool {
        let p = self.t().is_some_and(|t| t.sql.is_none() && t.query.projection.as_ref().is_some_and(|p| !p.is_empty()));
        if p {
            self.warn("These rows hold only the projected fields. Clear Project and run the query again to change documents.");
        }
        p
    }

    pub(super) fn save_draft(&mut self, key: &str) {
        let Some(dr) = self.core.drafts.get(key) else { return };
        if !self.conn(dr.conn).is_some_and(|c| c.writable) {
            let key = key.to_string();
            return self.modal = Some(Modal::Unlock(dr.conn, Some(Box::new(move |a: &mut App| a.save_draft(&key)))));
        }
        if self.core.prefs.confirm_save && !self.core.prefs.always_writable {
            let changes = db::diff(&dr.original, &dr.current);
            self.modal = Some(Modal::Save { tag: key.to_string(), changes });
        } else {
            self.write_draft(key);
        }
    }

    pub(super) fn write_draft(&mut self, key: &str) {
        if self.core.drafts.get(key).is_some_and(|d| self.conn(d.conn).is_some_and(|c| c.sql.is_some())) {
            return self.write_sql_drafts(vec![key.to_string()]);
        }
        let Some(dr) = self.core.drafts.remove(key) else { return };
        let Some(target) = self.target_for(&Sel { conn: dr.conn, db: dr.db.clone(), schema: dr.schema.clone(), coll: dr.coll.clone() }) else {
            self.core.drafts.insert(key.to_string(), dr);
            return self.warn_offline();
        };
        self.core.worker.save(key.to_string(), dr.conn, target, dr.original.clone(), dr.current.clone());
        self.core.saving.insert(key.to_string(), dr);
    }

    pub(super) fn tab_drafts(&self) -> Vec<String> {
        self.core.tab_drafts()
    }
}

pub(super) use quarry_core::core::drafts::{stage_draft, stored_version};
