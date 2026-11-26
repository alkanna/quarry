//! The Indexes view's reads: the index list of a tab, the usage and profiler suggestions, and the rows built from them.
//! src/app/indexes.rs draws the view and the New index dialog.

use super::{Refusal, Reply};
use crate::core::Core;
use crate::db;
use crate::state::*;
use mongodb::bson::{doc, Bson, Document};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum IdxA {
    /// The New index dialog's Create, with the index spec it built.
    Create { tab: u64, spec: Document },
    /// A row menu's Hide or Unhide.
    Hide { tab: u64, name: String, hidden: bool },
    /// Enable profiling, for the suggestions.
    Profile { tab: u64 },
}

/// A week, in ms: a usage counter younger than this cannot prove an index unused.
const WEEK_MS: i64 = 7 * 24 * 3600 * 1000;

/// One table row per index: its keys and options with the usage stats merged in.
pub fn index_rows(indexes: &[Document], info: Option<&db::IndexInfo>, now: i64) -> Vec<Document> {
    indexes
        .iter()
        .map(|ix| {
            let name = ix.get_str("name").unwrap_or_default().to_string();
            let mut opts = ix.clone();
            opts.remove("name");
            let keys = opts.remove("keys").unwrap_or_else(|| Bson::Document(doc! {}));
            let stat = info.and_then(|i| i.stats.iter().find(|s| s.name == name));
            let mut status = Vec::new();
            if stat.is_some_and(|s| s.hidden) || opts.get_bool("hidden").unwrap_or(false) {
                status.push("hidden");
            }
            let mut row = doc! { "name": &name, "keys": keys };
            if !opts.is_empty() {
                row.insert("options", opts);
            }
            if let Some(s) = stat {
                if name != "_id_" && db::is_unused(s.ops, s.since, now) {
                    status.push("unused");
                } else if s.ops == 0 && s.since.is_some_and(|t| now - t < WEEK_MS) {
                    status.push("no use since restart");
                }
                row.insert("ops", s.ops);
                if let Some(t) = s.since {
                    row.insert("since", mongodb::bson::DateTime::from_millis(t));
                }
                if let Some(b) = s.size {
                    row.insert("size", human_bytes(b));
                }
            }
            if !status.is_empty() {
                row.insert("status", status.join(", "));
            }
            row
        })
        .collect()
}

/// The app's byte sizes, as the status bar writes them.
pub(super) fn human_bytes(n: usize) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.0} KB", n as f64 / 1024.0),
        n => format!("{n} bytes"),
    }
}

/// Why the usage columns are empty, without the raw command the server echoes back.
pub fn stats_note(e: &str) -> &'static str {
    if db::is_unauthorized(e) {
        "Usage counts are not available: this user may not read index statistics here (it needs the indexStats privilege, part of clusterMonitor)."
    } else {
        "Usage counts are not available for this collection."
    }
}

impl Core {
    /// Each one writes through `write_target`, which says why it could not: the New index dialog stays open then.
    pub(super) fn indexes(&mut self, a: IdxA) -> Result<Reply, Refusal> {
        let tab = match &a {
            IdxA::Create { tab, .. } | IdxA::Hide { tab, .. } | IdxA::Profile { tab } => *tab,
        };
        self.cur_tab(tab)?;
        let (conn, target) = self.write_target()?;
        match a {
            IdxA::Create { spec, .. } => {
                let done = format!("Created index {}", spec.get_str("name").unwrap_or_default());
                let command = doc! { "createIndexes": &target.coll, "indexes": [spec] };
                self.worker.db_change(conn, target.client, target.db, command, done);
            }
            IdxA::Hide { name, hidden, .. } => {
                let done = format!("{} index {name}", if hidden { "Hid" } else { "Unhid" });
                self.worker.db_change(conn, target.client, target.db, doc! { "collMod": target.coll, "index": { "name": name, "hidden": hidden } }, done);
            }
            IdxA::Profile { .. } => self.worker.db_change(conn, target.client, target.db, doc! { "profile": 1, "slowms": 100 }, "Profiling slow operations".into()),
        }
        Ok(Reply::Done)
    }

    /// Rebuilds the index rows of the tabs showing collection `key` ("db.coll"). Open rows follow
    /// their index by name, since a create or drop moves the others up or down.
    pub fn refresh_idx_rows(&mut self, key: &str) {
        let now = mongodb::bson::DateTime::now().timestamp_millis();
        let info = self.idx_info.as_ref().filter(|(k, _)| k == key).map(|(_, i)| i);
        for t in self.tabs.iter_mut().filter(|t| t.sel.key() == key) {
            let rows = index_rows(&t.indexes, info, now);
            if t.view == View::Indexes {
                let name = |i: usize| t.idx_rows.get(i).and_then(|r| r.get_str("name").ok());
                let pos = |i: usize| name(i).and_then(|n| rows.iter().position(|r| r.get_str("name") == Ok(n)));
                t.selected = match t.selected {
                    Some((true, i)) => pos(i).map(|p| (true, p)),
                    s => s,
                };
                t.expanded = t.expanded.iter().filter_map(|&(agg, i)| if agg { pos(i).map(|p| (true, p)) } else { Some((agg, i)) }).collect();
            }
            t.idx_rows = rows;
            t.rows_replaced();
        }
    }

    pub fn load_indexes(&mut self) {
        self.load_indexes_of(self.cur);
    }

    pub fn load_indexes_of(&mut self, i: usize) {
        if let Some(target) = self.tabs.get(i).and_then(|t| self.target_for(&t.sel)) {
            self.req += 1;
            let req = self.req;
            self.tabs[i].idx_req = req;
            // Only the Indexes view shows usage, and it costs a profiler query.
            if self.tabs[i].view == View::Indexes {
                self.worker.index_info(target.clone());
            }
            self.worker.indexes(req, target);
        }
    }

    /// The index usage and profiler answer of collection `key`.
    pub(super) fn index_info_loaded(&mut self, key: String, res: db::Res<db::IndexInfo>) {
        match res {
            Ok(i) => {
                self.idx_info = Some((key.clone(), i));
                self.refresh_idx_rows(&key);
            }
            Err(e) => self.err(e),
        }
    }

    /// The index list answer `req`; one no tab waits for any more is dropped.
    pub(super) fn indexes_loaded(&mut self, req: u64, res: db::Res<Vec<Document>>) {
        match (res, self.tabs.iter().position(|t| t.idx_req == req)) {
            (Ok(v), Some(i)) => {
                let t = &mut self.tabs[i];
                t.clear_failed(View::Indexes);
                t.set_indexes(v);
                let key = t.sel.key();
                self.refresh_idx_rows(&key);
            }
            (Err(e), Some(i)) => self.fail_view(i, View::Indexes, e, "list the indexes"),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_rows_merge_usage() {
        let ixs = [doc! { "name": "_id_", "keys": { "_id": 1 } }, doc! { "name": "email_1", "keys": { "email": 1 }, "unique": true }];
        let day = 24 * 3600 * 1000;
        let now = 100 * day;
        let stat = |name: &str, ops, since| db::IndexStat { name: name.into(), ops, since: Some(since), hidden: false, size: Some(2048) };
        let info = db::IndexInfo { stats: vec![stat("_id_", 0, 0), stat("email_1", 0, 0)], stats_err: None, level: 0, slowms: 100, shapes: Vec::new() };
        let rows = index_rows(&ixs, Some(&info), now);
        assert_eq!(rows[0].get_str("status"), Err(mongodb::bson::document::ValueAccessError::NotPresent), "_id_ is never unused");
        assert_eq!(rows[1].get_str("status").unwrap(), "unused");
        assert_eq!(rows[1].get_document("options").unwrap(), &doc! { "unique": true });
        assert_eq!(rows[1].get_str("size").unwrap(), "2 KB");
        let info = db::IndexInfo { stats: vec![stat("email_1", 0, now - day)], ..info };
        assert_eq!(index_rows(&ixs, Some(&info), now)[1].get_str("status").unwrap(), "no use since restart");
        assert!(index_rows(&ixs, None, now)[1].get("ops").is_none());
    }

    #[test]
    fn refused_index_stats_get_a_short_note() {
        let atlas = "Command failed: Error code 13 (Unauthorized): not authorized on admin to execute command { aggregate: \"system.roles\", pipeline: [ { $indexStats: {} } ] }";
        assert!(stats_note(atlas).contains("indexStats privilege"));
        assert_eq!(stats_note("Command failed: something else"), "Usage counts are not available for this collection.");
    }
}
