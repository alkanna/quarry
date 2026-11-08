//! Unsaved document edits: staging, showing and saving them.

use super::*;

impl App {
    pub(super) fn selected_doc(&self) -> Option<&Document> {
        let t = self.t()?;
        match t.selected? {
            (false, i) => t.docs.get(i),
            (true, i) => t.agg_page().0.get(i),
        }
    }

    pub(super) fn draft_key(&self, d: &Document) -> String {
        let s = self.t().map(|t| format!("{}/{}/{}", t.sel.conn, t.sel.db, t.sel.coll)).unwrap_or_default();
        format!("{s}#{}", d.get("_id").map(db::compact).unwrap_or_default())
    }

    /// The document as it should be shown: with its unsaved edits, if any. Edits only belong to
    /// Documents rows, so a result row (`agg`) with the same _id never gets them.
    pub(super) fn shown_doc(&self, d: &Document, agg: bool) -> Option<Document> {
        if agg || self.drafts.is_empty() {
            return None;
        }
        self.drafts.get(&self.draft_key(d)).map(|x| x.current.clone())
    }

    /// Keeps an edit as unsaved instead of writing it, so several edits go out in one save.
    pub(super) fn stage(&mut self, d: &Document, new: db::Res<Document>) {
        let new = match new {
            Ok(n) => n,
            Err(e) => return self.err(e),
        };
        let key = self.draft_key(d);
        let Some(sel) = self.t().map(|t| t.sel.clone()) else { return };
        let original = self.drafts.get(&key).map(|x| x.original.clone()).unwrap_or_else(|| d.clone());
        self.inline = None;
        self.new_field = None;
        self.json_edit = None;
        if db::diff(&original, &new).is_empty() {
            self.drafts.remove(&key);
            return self.ok("No changes left on this document");
        }
        let n = db::diff(&original, &new).len();
        self.drafts.insert(key.clone(), Draft { conn: sel.conn, db: sel.db, coll: sel.coll, original, current: new });
        if self.prefs.auto_save && self.sel_writable() {
            self.write_draft(&key);
        } else {
            self.ok(format!("{n} unsaved change(s). Press Save on the document when ready."));
        }
    }

    pub(super) fn save_draft(&mut self, key: &str) {
        let Some(dr) = self.drafts.get(key) else { return };
        if !self.conn(dr.conn).is_some_and(|c| c.writable) {
            let key = key.to_string();
            return self.modal = Some(Modal::Unlock(dr.conn, Some(Box::new(move |a: &mut App| a.save_draft(&key)))));
        }
        if self.prefs.confirm_save && !self.prefs.always_writable {
            let changes = db::diff(&dr.original, &dr.current);
            self.modal = Some(Modal::Save { tag: key.to_string(), changes });
        } else {
            self.write_draft(key);
        }
    }

    pub(super) fn write_draft(&mut self, key: &str) {
        let Some(dr) = self.drafts.remove(key) else { return };
        let Some(target) = self.target_for(&Sel { conn: dr.conn, db: dr.db.clone(), coll: dr.coll.clone() }) else {
            self.drafts.insert(key.to_string(), dr);
            return self.err("Not connected.");
        };
        self.worker.save(key.to_string(), dr.conn, target, dr.original.clone(), dr.current.clone());
        self.saving.insert(key.to_string(), dr);
    }

    /// Unsaved documents of the open collection.
    pub(super) fn tab_drafts(&self) -> Vec<String> {
        let Some(t) = self.t() else { return Vec::new() };
        let prefix = format!("{}/{}/{}#", t.sel.conn, t.sel.db, t.sel.coll);
        self.drafts.keys().filter(|k| k.starts_with(&prefix)).cloned().collect()
    }
}
