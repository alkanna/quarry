//! Unsaved document edits: staging, showing and saving them.

use super::*;

impl App {
    pub(super) fn selected_doc(&self) -> Option<&Document> {
        let t = self.t()?;
        match t.selected? {
            (false, i) => t.docs.get(i).map(db::Row::doc),
            (true, i) => t.agg_page().0.doc(i),
        }
    }

    pub(super) fn draft_key(&self, d: &Document) -> String {
        self.draft_key_of(d.get("_id"))
    }

    fn draft_key_of(&self, id: Option<&Bson>) -> String {
        let s = self.t().map(Tab::coll_key).unwrap_or_default();
        format!("{s}#{}", id.map(db::compact).unwrap_or_default())
    }

    /// The document as it should be shown: with its unsaved edits, if any. Edits only belong to
    /// Documents rows, so a result row (`agg`) with the same _id never gets them.
    pub(super) fn shown_doc(&self, d: &Document, agg: bool) -> Option<Document> {
        self.shown_row(Rows::Docs(std::slice::from_ref(d)), 0, agg)
    }

    /// `shown_doc` for row `i` of a table, found by its _id without decoding the row.
    pub(super) fn shown_row(&self, rows: Rows, i: usize, agg: bool) -> Option<Document> {
        if agg || self.drafts.is_empty() {
            return None;
        }
        self.drafts.get(&self.draft_key_of(rows.id(i).as_ref())).map(|x| x.current.clone())
    }

    /// Keeps an edit as unsaved instead of writing it, so several edits go out in one save.
    pub(super) fn stage(&mut self, d: &Document, new: db::Res<Document>) {
        let new = match new {
            Ok(n) => n,
            Err(e) => return self.err(e),
        };
        let key = self.draft_key(d);
        let Some(sel) = self.t().map(|t| t.sel.clone()) else { return };
        self.inline = None;
        self.new_field = None;
        self.json_edit = None;
        let n = stage_draft(&mut self.drafts, &key, &sel, d, new);
        if n == 0 {
            return self.ok("No changes left on this document");
        }
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
        if self.drafts.get(key).is_some_and(|d| self.conn(d.conn).is_some_and(|c| c.sql.is_some())) {
            return self.write_sql_drafts(vec![key.to_string()]);
        }
        let Some(dr) = self.drafts.remove(key) else { return };
        let Some(target) = self.target_for(&Sel { conn: dr.conn, db: dr.db.clone(), schema: dr.schema.clone(), coll: dr.coll.clone() }) else {
            self.drafts.insert(key.to_string(), dr);
            return self.warn_offline();
        };
        self.worker.save(key.to_string(), dr.conn, target, dr.original.clone(), dr.current.clone());
        self.saving.insert(key.to_string(), dr);
    }

    /// Unsaved documents of the open collection.
    pub(super) fn tab_drafts(&self) -> Vec<String> {
        let Some(t) = self.t() else { return Vec::new() };
        drafts_of(&self.drafts, &t.sel)
    }
}

/// Records `new` as the unsaved version of `d`, keeping the original from the first edit.
/// Returns how many changes are left; at 0 the draft is dropped.
pub(super) fn stage_draft(drafts: &mut HashMap<String, Draft>, key: &str, sel: &Sel, d: &Document, new: Document) -> usize {
    let original = drafts.get(key).map(|x| x.original.clone()).unwrap_or_else(|| d.clone());
    let n = db::diff(&original, &new).len();
    if n == 0 {
        drafts.remove(key);
    } else {
        drafts.insert(key.to_string(), Draft { conn: sel.conn, db: sel.db.clone(), schema: sel.schema.clone(), coll: sel.coll.clone(), original, current: new });
    }
    n
}

/// Keys of the drafts in one collection. Matched on the draft's own fields, not a key prefix:
/// collection names may contain `#`, so `a#` would also match drafts of `a#b`.
pub(super) fn drafts_of(drafts: &HashMap<String, Draft>, sel: &Sel) -> Vec<String> {
    drafts.iter().filter(|(_, d)| d.is_in(sel)).map(|(k, _)| k.clone()).collect()
}

impl Draft {
    pub(super) fn is_in(&self, sel: &Sel) -> bool {
        self.conn == sel.conn && self.db == sel.db && self.schema == sel.schema && self.coll == sel.coll
    }
}

/// The document as stored on the server: deletes must match that, not its unsaved edits.
pub(super) fn stored_version(drafts: &HashMap<String, Draft>, key: &str, d: &Document) -> Document {
    drafts.get(key).map(|dr| dr.original.clone()).unwrap_or_else(|| d.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(coll: &str) -> Sel {
        Sel { conn: 1, db: "shop".into(), coll: coll.into(), ..Default::default() }
    }

    #[test]
    fn staging_keeps_first_original_and_drops_reverted_drafts() {
        let mut drafts = HashMap::new();
        let d = doc! { "_id": 1, "a": 1 };
        assert_eq!(stage_draft(&mut drafts, "k", &sel("c"), &d, doc! { "_id": 1, "a": 2 }), 1);
        // The second edit starts from the edited version but must still diff against the stored one.
        assert_eq!(stage_draft(&mut drafts, "k", &sel("c"), &doc! { "_id": 1, "a": 2 }, doc! { "_id": 1, "a": 3, "b": 1 }), 2);
        assert_eq!(drafts["k"].original, d);
        assert_eq!(stage_draft(&mut drafts, "k", &sel("c"), &doc! { "_id": 1, "a": 3, "b": 1 }, d.clone()), 0);
        assert!(drafts.is_empty());
    }

    #[test]
    fn drafts_stay_in_their_collection() {
        let mut drafts = HashMap::new();
        stage_draft(&mut drafts, "1/shop/a#b#1", &sel("a#b"), &doc! { "_id": 1 }, doc! { "_id": 1, "x": 1 });
        stage_draft(&mut drafts, "1/shop/a#1", &sel("a"), &doc! { "_id": 1 }, doc! { "_id": 1, "x": 1 });
        assert_eq!(drafts_of(&drafts, &sel("a")), ["1/shop/a#1"]);
        assert_eq!(drafts_of(&drafts, &sel("a#b")), ["1/shop/a#b#1"]);
    }

    #[test]
    fn deletes_match_the_stored_document() {
        let mut drafts = HashMap::new();
        let stored = doc! { "_id": 1, "a": 1 };
        let edited = doc! { "_id": 1, "a": 2 };
        stage_draft(&mut drafts, "k", &sel("c"), &stored, edited.clone());
        assert_eq!(stored_version(&drafts, "k", &edited), stored);
        assert_eq!(stored_version(&drafts, "other", &edited), edited);
    }
}
