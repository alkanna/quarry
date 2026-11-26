//! The Watch view: the collection's inserts, updates and deletes as they happen, from a change stream that runs only
//! while the view is on screen (src/app/watch.rs draws it). Start, Stop and Clear, and the stream's answers.

use super::{Reply, Refusal};
use crate::core::Core;
use crate::db;
use crate::state::View;
use mongodb::bson::{Bson, Document};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum WatchA {
    /// Start watching on the tab with this uid.
    Start { tab: u64 },
    Stop { tab: u64 },
    /// Clear the changes shown.
    Clear { tab: u64 },
}

/// Changes kept on screen; the oldest go first.
const WATCH_CAP: usize = 500;

/// One table row from a change event: when, what, which document, the fields an update touched,
/// and the document as it is now (absent for deletes).
pub fn watch_row(mut ev: Document) -> Document {
    let mut row = Document::new();
    if let Some(at) = ev.remove("wallTime").or_else(|| ev.remove("clusterTime")) {
        row.insert("at", at);
    }
    row.insert("op", ev.get_str("operationType").unwrap_or_default());
    if let Some(id) = ev.get_document_mut("documentKey").ok().and_then(|k| k.remove("_id")) {
        row.insert("_id", id);
    }
    if let Ok(u) = ev.get_document("updateDescription") {
        let mut changed: Vec<Bson> = u.get_document("updatedFields").map(|f| f.keys().map(|k| Bson::String(k.clone())).collect()).unwrap_or_default();
        changed.extend(u.get_array("removedFields").into_iter().flatten().filter_map(|k| k.as_str()).map(|k| Bson::String(format!("-{k}"))));
        row.insert("changed", changed);
    }
    if let Some(d @ Bson::Document(_)) = ev.remove("fullDocument") {
        row.insert("doc", d);
    }
    row
}

/// Open and expanded result rows after the first `n` result rows were dropped. Documents rows are untouched.
fn shift_rows(selected: &mut Option<(bool, usize)>, expanded: &mut BTreeSet<(bool, usize)>, n: usize) {
    let shift = |(agg, i): (bool, usize)| if !agg { Some((agg, i)) } else { i.checked_sub(n).map(|i| (true, i)) };
    *selected = selected.and_then(shift);
    *expanded = expanded.iter().copied().filter_map(shift).collect();
}

/// The message for a stream that could not start or stopped with an error.
fn watch_err(e: &str) -> String {
    if e.contains("only supported on replica sets") || e.contains("40573") {
        "Change streams need a replica set or a sharded cluster; this server is standalone.".into()
    } else if db::is_unauthorized(e) {
        "Watching needs the changeStream and find privileges on this collection (the read role has both).".into()
    } else {
        e.to_string()
    }
}

/// The document editors close, as the app's reset_doc_state does. shortcut: a copy until the documents area has one on Core.
fn reset_doc_state(c: &mut Core) {
    c.json_edit = None;
    c.inline = None;
    c.new_field = None;
}

impl Core {
    pub(super) fn watch(&mut self, a: WatchA) -> Result<Reply, Refusal> {
        let (WatchA::Start { tab } | WatchA::Stop { tab } | WatchA::Clear { tab }) = a;
        let Some(i) = self.tabs.iter().position(|t| t.uid == tab) else { return Ok(Reply::Done) };
        match a {
            WatchA::Start { .. } => return self.start_watch(i),
            WatchA::Stop { .. } => self.tabs[i].watch.stop(),
            WatchA::Clear { .. } => {
                let t = &mut self.tabs[i];
                t.watch.rows.clear();
                // The open rows of the result table go with the changes they showed.
                t.expanded.retain(|k| !k.0);
                if t.selected.is_some_and(|s| s.0) {
                    t.selected = None;
                    reset_doc_state(self);
                }
            }
        }
        Ok(Reply::Done)
    }

    fn start_watch(&mut self, i: usize) -> Result<Reply, Refusal> {
        if self.demo {
            self.ok("Offline demo: nothing to watch.");
            return Ok(Reply::Done);
        }
        let Some(target) = self.target_for(&self.tabs[i].sel) else { return Err(self.refuse_offline(self.tabs[i].sel.conn)) };
        self.req += 1;
        let req = self.req;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let w = &mut self.tabs[i].watch;
        w.req = req;
        w.cancel = Some(tx);
        w.err = None;
        self.worker.watch(req, target, rx);
        Ok(Reply::Pending(req))
    }

    /// Stops every stream whose view is not on screen, so a hidden Watch never keeps reading. The app calls it each frame.
    pub fn stop_hidden_watches(&mut self) {
        let cur = self.cur;
        for (i, t) in self.tabs.iter_mut().enumerate() {
            if t.watch.running() && (i != cur || t.view != View::Watch) {
                t.watch.stop();
            }
        }
    }

    pub(crate) fn watch_event(&mut self, req: u64, ev: Document) {
        let cur = self.cur;
        let Some((i, t)) = self.tabs.iter_mut().enumerate().find(|(_, t)| t.watch.req == req) else { return };
        // Fails only for keys BSON cannot hold, which a server never sends.
        if let Ok(r) = db::Row::from_doc(&watch_row(ev)) {
            t.watch.rows.push(r);
        }
        let n = t.watch.rows.len().saturating_sub(WATCH_CAP);
        if n == 0 {
            return;
        }
        t.watch.rows.drain(..n);
        // The open rows move up with their change; one that fell off the top closes.
        let was_open = t.selected.is_some();
        shift_rows(&mut t.selected, &mut t.expanded, n);
        if was_open && t.selected.is_none() && i == cur {
            reset_doc_state(self);
        }
    }

    pub(crate) fn watch_done(&mut self, req: u64, res: db::Res<()>) {
        let Some(t) = self.tabs.iter_mut().find(|t| t.watch.req == req) else { return };
        t.watch.stop();
        t.watch.err = res.err().map(|e| watch_err(&e));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::doc;

    #[test]
    fn change_events_become_rows() {
        let at = mongodb::bson::DateTime::from_millis(1_700_000_000_000);
        let ev = doc! { "operationType": "update", "wallTime": at, "documentKey": { "_id": 7 }, "updateDescription": { "updatedFields": { "status": "paid", "total": 5 }, "removedFields": ["note"] }, "fullDocument": { "_id": 7, "status": "paid" } };
        assert_eq!(watch_row(ev), doc! { "at": at, "op": "update", "_id": 7, "changed": ["status", "total", "-note"], "doc": { "_id": 7, "status": "paid" } });
        assert_eq!(watch_row(doc! { "operationType": "delete", "documentKey": { "_id": 7 } }), doc! { "op": "delete", "_id": 7 });
    }

    #[test]
    fn open_rows_follow_their_change_when_old_ones_drop() {
        let (mut sel, mut exp) = (Some((true, 4)), BTreeSet::from([(true, 1), (true, 5), (false, 3)]));
        shift_rows(&mut sel, &mut exp, 2);
        assert_eq!(sel, Some((true, 2)));
        assert_eq!(exp, BTreeSet::from([(true, 3), (false, 3)]));
        let mut sel = Some((true, 1));
        shift_rows(&mut sel, &mut BTreeSet::new(), 2);
        assert_eq!(sel, None);
    }

    #[test]
    fn stream_errors_say_what_is_missing() {
        assert!(watch_err("Command failed: Error code 40573 (Location40573): The $changeStream stage is only supported on replica sets").contains("standalone"));
        assert!(watch_err("Command failed: Error code 13 (Unauthorized): not authorized on shop to execute command").contains("changeStream"));
    }
}
