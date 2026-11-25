//! Unsaved edits of documents and SQL rows: their keys, how a row shows with them, and where the large value editor
//! draws. Staging and saving them is each area's.

use super::Core;
use crate::db;
use crate::state::table::Rows;
use crate::state::*;
use mongodb::bson::{Bson, Document};
use std::collections::HashMap;

impl Core {
    pub fn selected_doc(&self) -> Option<&Document> {
        let t = self.t()?;
        match t.selected? {
            (false, i) => t.docs.get(i).map(db::Row::doc),
            (true, i) => t.agg_page().0.doc(i),
        }
    }

    pub fn draft_key(&self, d: &Document) -> String {
        self.draft_key_of(d.get("_id"))
    }

    pub fn draft_key_of(&self, id: Option<&Bson>) -> String {
        let s = self.t().map(Tab::coll_key).unwrap_or_default();
        format!("{s}#{}", id.map(db::compact).unwrap_or_default())
    }

    /// The document as it should be shown: with its unsaved edits, if any. Edits only belong to
    /// Documents rows, so a result row (`agg`) with the same _id never gets them.
    pub fn shown_doc(&self, d: &Document, agg: bool) -> Option<Document> {
        self.shown_row(Rows::Docs(std::slice::from_ref(d)), 0, agg)
    }

    /// `shown_doc` for row `i` of a table, found by its _id without decoding the row.
    pub fn shown_row(&self, rows: Rows, i: usize, agg: bool) -> Option<Document> {
        if agg || self.drafts.is_empty() {
            return None;
        }
        self.drafts.get(&self.draft_key_of(rows.id(i).as_ref())).map(|x| x.current.clone())
    }

    /// Unsaved documents of the open collection.
    pub fn tab_drafts(&self) -> Vec<String> {
        let Some(t) = self.t() else { return Vec::new() };
        drafts_of(&self.drafts, &t.sel)
    }

    /// The large editor shows in the docked panel rather than a dialog. A whole document always gets the dialog.
    pub fn value_in_panel(&self) -> bool {
        self.prefs.value_panel && (self.json_edit.as_ref().is_some_and(|je| !je.path.is_empty()) || self.sql_cell.as_ref().is_some_and(|e| e.big))
    }

    /// The large editor shows as a dialog, which takes the keys from the page.
    pub fn value_dialog_open(&self) -> bool {
        (self.json_edit.is_some() || self.sql_cell.as_ref().is_some_and(|e| e.big)) && !self.value_in_panel()
    }

    /// The side panel holds text not applied yet, which opening another value would throw away.
    pub fn panel_dirty(&self) -> bool {
        self.value_in_panel() && (self.json_edit.as_ref().is_some_and(|j| j.dirty) || self.sql_cell.as_ref().is_some_and(|e| e.big && e.dirty))
    }
}

/// A value the one-line editors cannot show faithfully, so double-click opens the large editor straight away.
pub fn is_long(s: &str) -> bool {
    s.contains('\n') || s.len() > 500
}

/// Records `new` as the unsaved version of `d`, keeping the original from the first edit.
/// Returns how many changes are left; at 0 the draft is dropped.
pub fn stage_draft(drafts: &mut HashMap<String, Draft>, key: &str, sel: &Sel, d: &Document, new: Document) -> usize {
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
pub fn drafts_of(drafts: &HashMap<String, Draft>, sel: &Sel) -> Vec<String> {
    drafts.iter().filter(|(_, d)| d.is_in(sel)).map(|(k, _)| k.clone()).collect()
}

/// The document as stored on the server: deletes must match that, not its unsaved edits.
pub fn stored_version(drafts: &HashMap<String, Draft>, key: &str, d: &Document) -> Document {
    drafts.get(key).map(|dr| dr.original.clone()).unwrap_or_else(|| d.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::doc;

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
