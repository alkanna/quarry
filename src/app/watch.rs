//! The Watch view: the collection's inserts, updates and deletes as they happen, from a change
//! stream that runs only while the view is on screen.

use super::*;

/// Columns of the Watch table.
pub(super) static WATCH_FIELDS: std::sync::LazyLock<Vec<String>> = std::sync::LazyLock::new(|| ["at", "op", "_id", "changed", "doc"].map(String::from).to_vec());

/// Changes kept on screen; the oldest go first.
const WATCH_CAP: usize = 500;

impl WatchTab {
    pub(super) fn running(&self) -> bool {
        self.cancel.is_some()
    }

    /// Ends the stream; its last events, already sent, are dropped since no tab has its id.
    pub(super) fn stop(&mut self) {
        self.cancel = None;
        self.req = 0;
    }
}

#[derive(Default)]
pub(super) struct WatchTab {
    /// The running stream's id, 0 when stopped.
    pub(super) req: u64,
    /// Dropping it stops the stream.
    pub(super) cancel: Option<tokio::sync::oneshot::Sender<()>>,
    pub(super) rows: Vec<Document>,
    pub(super) err: Option<String>,
}

/// One table row from a change event: when, what, which document, the fields an update touched,
/// and the document as it is now (absent for deletes).
pub(super) fn watch_row(mut ev: Document) -> Document {
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

impl App {
    pub(super) fn watch_view(&mut self, ui: &mut egui::Ui) {
        let t = self.t().unwrap();
        let (running, n, err) = (t.watch.running(), t.watch.rows.len(), t.watch.err.clone());
        let (mut start, mut stop, mut clear) = (false, false, false);
        toolbar(ui, |ui| {
            ui.horizontal(|ui| {
                if running {
                    stop = ui.button("■ Stop").clicked();
                    spin(ui);
                    ui.label(RichText::new(format!("Watching: {n} change{}", if n == 1 { "" } else { "s" })).color(MUTED()));
                } else {
                    start = primary(ui, "▶ Start watching", PRIMARY()).clicked();
                    ui.label(RichText::new("Shows changes from now on. It stops when you leave this view.").color(MUTED()));
                }
                if n > 0 {
                    clear = ui.button("Clear").clicked();
                }
            });
        });
        if let Some(e) = err {
            ui.label(RichText::new(e).color(ALERT()));
        }
        if start {
            self.start_watch();
        }
        let t = self.tm().unwrap();
        if stop {
            t.watch.stop();
        }
        if clear {
            t.watch.rows.clear();
            self.drop_agg_row();
        }
        ui.add_space(6.0);
        if self.t().is_some_and(|t| t.watch.rows.is_empty()) {
            ui.add_space(14.0);
            ui.label(RichText::new(if running { "Waiting for the first change…" } else { "Nothing is read until you press Start." }).color(MUTED()));
        } else {
            self.results(ui, true);
        }
    }

    fn start_watch(&mut self) {
        if self.demo {
            return self.ok("Offline demo: nothing to watch.");
        }
        let Some(target) = self.target() else { return self.err("Not connected.") };
        self.req += 1;
        let req = self.req;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let w = &mut self.tm().unwrap().watch;
        w.req = req;
        w.cancel = Some(tx);
        w.err = None;
        self.worker.watch(req, target, rx);
    }

    /// Stops every stream whose view is not on screen, so a hidden Watch never keeps reading.
    pub(super) fn stop_hidden_watches(&mut self) {
        let cur = self.cur;
        for (i, t) in self.tabs.iter_mut().enumerate() {
            if t.watch.running() && (i != cur || t.view != View::Watch) {
                t.watch.stop();
            }
        }
    }

    pub(super) fn watch_event(&mut self, req: u64, ev: Document) {
        let cur = self.cur;
        let Some((i, t)) = self.tabs.iter_mut().enumerate().find(|(_, t)| t.watch.req == req) else { return };
        t.watch.rows.push(watch_row(ev));
        let n = t.watch.rows.len().saturating_sub(WATCH_CAP);
        if n == 0 {
            return;
        }
        t.watch.rows.drain(..n);
        // The open rows move up with their change; one that fell off the top closes.
        let was_open = t.selected.is_some();
        shift_rows(&mut t.selected, &mut t.expanded, n);
        if was_open && t.selected.is_none() && i == cur {
            self.reset_doc_state();
        }
    }

    pub(super) fn watch_done(&mut self, req: u64, res: db::Res<()>) {
        let Some(t) = self.tabs.iter_mut().find(|t| t.watch.req == req) else { return };
        t.watch.stop();
        t.watch.err = res.err().map(|e| watch_err(&e));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
