//! Answers that fill no window of their own: a collection's statistics and view definition, the counts under a tab, and
//! what a changed database does to the connection that holds it.

use super::indexes::human_bytes;
use super::{Dialog, UiFx};
use crate::core::Core;
use crate::db;
use crate::state::View;
use crate::state::config::{cache_key, seal};
use crate::state::complete::{FieldInfo, Fields};
use mongodb::bson::{Bson, Document};
use std::collections::HashMap;
use std::sync::Arc;

impl Core {
    /// The `Msg::Command` answers this file takes, each only when it worked. A failed one is reported by the app.
    pub fn takes_command(tag: &str, ok: bool) -> bool {
        ok && (tag.starts_with("tabstats\u{1f}") || tag == "stats" || tag == "viewdef" || tag.starts_with("validator:"))
    }

    pub(super) fn command_reply(&mut self, tag: &str, d: Document) {
        match tag {
            t if t.starts_with("tabstats\u{1f}") => {
                let key: Vec<&str> = t.split('\u{1f}').skip(1).collect();
                let n = |k: &str| match d.get(k) {
                    Some(Bson::Int32(n)) => *n as u64,
                    Some(Bson::Int64(n)) => *n as u64,
                    Some(Bson::Double(n)) => *n as u64,
                    _ => 0,
                };
                for tab in self.tabs.iter_mut().filter(|tab| [tab.sel.conn.to_string().as_str(), &tab.sel.db, &tab.sel.coll] == key[..]) {
                    tab.stats = Some((n("count"), n("size"), n("nindexes")));
                }
            }
            t if t.starts_with("validator:") => self.validator_loaded(t["validator:".len()..].parse().unwrap_or(0), d),
            "stats" => {
                let title = format!("Statistics of {}", d.get_str("collection").unwrap_or_default());
                let mut lines = Vec::new();
                for (k, v) in d.iter().filter(|(k, _)| *k != "collection") {
                    let n = match v {
                        Bson::Int32(n) => Some(*n as f64),
                        Bson::Int64(n) => Some(*n as f64),
                        Bson::Double(n) => Some(*n),
                        _ => None,
                    };
                    let text = match (k.as_str(), n) {
                        ("size" | "avgObjSize" | "storageSize" | "totalIndexSize", Some(n)) => human_bytes(n as usize),
                        (_, Some(n)) => db::group(&(n as i64).to_string()),
                        ("indexSizes", _) => v.as_document().map(|d| d.iter().map(|(i, s)| format!("{i}: {}", human_bytes(match s { Bson::Int32(n) => *n as usize, Bson::Int64(n) => *n as usize, Bson::Double(n) => *n as usize, _ => 0 }))).collect::<Vec<_>>().join(", ")).unwrap_or_default(),
                        _ => db::value_text(v),
                    };
                    lines.push(format!("{k}: {text}"));
                }
                self.ui_fx.push(UiFx::Open(Dialog::Text { title, body: lines.join("\n"), code: false }));
            }
            "viewdef" => {
                let opts = d.get_document("cursor").ok().and_then(|c| c.get_array("firstBatch").ok()).and_then(|a| a.first()).and_then(|x| x.as_document()).and_then(|x| x.get_document("options").ok()).cloned().unwrap_or_default();
                let body = format!("viewOn: {}\npipeline:\n{}", opts.get_str("viewOn").unwrap_or_default(), db::to_edit_value(opts.get("pipeline").unwrap_or(&Bson::Array(vec![]))));
                self.ui_fx.push(UiFx::Open(Dialog::Text { title: "View definition".into(), body, code: true }));
            }
            _ => {}
        }
    }

    pub(super) fn db_changed(&mut self, conn: u64, db: String, copied: Option<String>, dropped: Option<String>, res: db::Res<String>) {
        if let Some(to) = copied {
            // Listed now so the row does not blink out before the reload below answers.
            if res.is_ok() && let Some(l) = self.conns.iter_mut().find(|c| c.meta.id == conn).and_then(|c| c.dbs.iter_mut().find(|d| d.0 == db)).and_then(|d| d.1.as_mut()) {
                l.push(to.clone());
                l.sort();
            }
            self.copying.remove(&(conn, db.clone(), to));
        }
        let open = self.open_after.take_if(|o| o.0 == conn && o.1 == db);
        if res.is_ok() && open.is_some() && let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) && !c.dbs.iter().any(|d| d.0 == db) {
            // A new database is listed now so its collection has a row to open under.
            c.dbs.push((db.clone(), None));
            c.dbs.sort_by(|a, b| a.0.cmp(&b.0));
        }
        if res.is_ok() && let Some((_, _, coll)) = open {
            self.open_collection(crate::state::Sel { conn, db: db.clone(), coll, ..Default::default() }, crate::state::Open::Here);
        }
        match res {
            Ok(m) => {
                // Only a drop the server confirmed forgets its undo entries; a refused one keeps them.
                if let Some(coll) = dropped {
                    self.purge_mongo_undo(conn, &db, &coll);
                }
                self.ok(m)
            }
            Err(e) => {
                self.err(e);
                if dropped.is_some_and(|c| c.is_empty()) && let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
                    c.dbs.push((db.clone(), None));
                    c.dbs.sort_by(|a, b| a.0.cmp(&b.0));
                }
            }
        }
        // The server may normalise the validator, so show what it kept.
        for t in self.tabs.iter_mut().filter(|t| t.sel.conn == conn && t.sel.db == db) {
            t.schema.vloaded = false;
        }
        if let Some(client) = self.conn(conn).and_then(|c| c.client.clone()) {
            if db == "*" {
                self.connect(conn);
            } else {
                self.worker.collections(conn, client, db);
            }
        }
        if self.users.as_ref().is_some_and(|u| u.conn == conn) {
            self.load_users(conn);
        }
        // Hiding an index or switching the profiler changes what the Indexes view shows.
        if self.t().is_some_and(|t| t.view == View::Indexes) {
            self.load_indexes();
        }
    }

    pub fn save_values(&mut self) {
        // The demo must not touch the keychain, and without a key nothing is written in plain text.
        if self.ephemeral {
            return;
        }
        if let Some(b) = cache_key().and_then(|k| seal(k, &mongodb::bson::to_vec(&self.values).ok()?)) {
            self.write_config("values.bson", b);
        }
    }

    /// Asks the server for a field's values once per session: a sample (the copy kept on disk shows meanwhile), or with
    /// `prefix` the values starting with the text being typed. `key` is the collection the request was made in, which a
    /// tab switch may have left.
    pub(super) fn want_values(&mut self, key: String, path: String, prefix: Option<String>) {
        if key != self.col_key(false) || !self.values_asked.insert((key.clone(), path.clone(), prefix.clone())) {
            return;
        }
        let Some(t) = self.target() else { return };
        self.values_pending.insert((key.clone(), path.clone()));
        self.worker.field_values(key, t, path, prefix);
    }

    pub(super) fn values_done(&mut self, key: String, path: String, prefix: Option<String>, res: db::Res<Option<Vec<Bson>>>) {
        self.values_pending.remove(&(key.clone(), path.clone()));
        // Suggestions are best effort: a refused or slow lookup keeps what the page offers. A typed
        // lookup that did not run says why under the list.
        let note = match &res {
            Ok(None) => Some("Field not indexed: only sampled values are suggested"),
            Err(_) if prefix.is_some() => Some("Lookup too slow: only sampled values are suggested"),
            _ => None,
        };
        if let Some(n) = note {
            self.values_note.insert((key.clone(), path.clone()), n);
        }
        let Ok(Some(mut vals)) = res else { return };
        vals.retain(suggestible);
        vals.truncate(db::FIELD_VALUES_MAX);
        let found = HashMap::from([(path, vals)]);
        for t in self.tabs.iter_mut().filter(|t| t.coll_key() == key) {
            merge_values(Arc::make_mut(&mut t.field_info), &found);
        }
        // Values found by what was typed are kept for the session only, so the file holds samples.
        if prefix.is_none() {
            self.values.entry(key).or_default().extend(found);
            self.save_values();
        }
    }
}

/// Whether a value is worth suggesting: short text, numbers and booleans, as `field_info` keeps.
pub fn suggestible(v: &Bson) -> bool {
    match v {
        Bson::String(s) => s.chars().count() <= 80,
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Boolean(_) => true,
        _ => false,
    }
}

/// Adds values fetched from the server after the page's own, which come first because they are
/// counted. A field missing from the page gets an entry typed by its first value.
pub fn merge_values(fields: &mut Fields, server: &HashMap<String, Vec<Bson>>) {
    for (p, vals) in server {
        let Some(first) = vals.first() else { continue };
        let f = fields.entry(p.clone()).or_insert_with(|| FieldInfo { kind: db::type_name(first), values: Vec::new(), freq: Vec::new() });
        for v in vals {
            if !f.values.contains(v) {
                f.values.push(v.clone());
            }
        }
    }
}
