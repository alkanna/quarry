//! The Schema view: the analysis of the fields of the documents matching the query, its field menu, the validator
//! editor, and the data relations analysis that the database menu starts.

use super::super::{Dialog, Refusal, Reply, UiFx};
use crate::core::Core;
use crate::db;
use crate::state::*;
use mongodb::bson::{doc, Bson, Document};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum SchemaA {
    Analyze { tab: u64 },
    /// The sample: the first or last `limit` documents, 0 for all of them.
    Limit { tab: u64, limit: i64, last: bool },
    /// The validator's level (0 off, 1 moderate, 2 strict) and whether a failure only warns.
    Rules { tab: u64, level: u8, warn: bool },
    /// Generate from analysis.
    Generate { tab: u64 },
    /// Reads the validator again from the server.
    Reload { tab: u64 },
    /// Apply: asks for the collection's name first.
    Apply { tab: u64 },
    /// A field's chevron or name folds or unfolds its children.
    Fold { tab: u64, path: String },
    /// Expand all, or Collapse all.
    FoldAll { tab: u64, closed: bool },
    /// The field menu's Add as column.
    AddColumn { tab: u64, path: String },
    /// The field menu's Filter: has this field, or missing it.
    FilterHas { tab: u64, path: String, exists: bool },
    /// Treat an object's keys as map entries, or show them as fields again; saved for the collection.
    Map { tab: u64, path: String },
}

impl Core {
    pub(super) fn schema(&mut self, a: SchemaA) -> Result<Reply, Refusal> {
        let tab = match &a {
            SchemaA::Analyze { tab } | SchemaA::Limit { tab, .. } | SchemaA::Rules { tab, .. } | SchemaA::Generate { tab } | SchemaA::Reload { tab } | SchemaA::Apply { tab } | SchemaA::Fold { tab, .. } | SchemaA::FoldAll { tab, .. } | SchemaA::AddColumn { tab, .. } | SchemaA::FilterHas { tab, .. } | SchemaA::Map { tab, .. } => *tab,
        };
        self.cur_tab(tab)?;
        let t = &mut self.tabs[self.cur];
        match a {
            SchemaA::Analyze { .. } => self.analyze_schema(),
            SchemaA::Limit { limit, last, .. } => {
                // Views refuse a $natural sort, so they only offer the first documents.
                let last = last && !self.views.has(t.sel.conn, &t.sel.db, &t.sel.coll);
                (t.schema.limit, t.schema.last) = (limit, last);
            }
            SchemaA::Rules { level, warn, .. } => (t.schema.vlevel, t.schema.vwarn) = (level.min(2), warn),
            SchemaA::Generate { .. } => {
                let s = &mut t.schema;
                if !s.stats.is_empty() {
                    s.vtext = db::to_edit_json(&db::schema_from_stats(&s.stats, s.docs));
                }
            }
            SchemaA::Reload { .. } => {
                t.schema.vloaded = false;
                self.load_validator();
            }
            SchemaA::Apply { .. } => match db::parse_doc(&t.schema.vtext) {
                Ok(v) => {
                    let level = ["off", "moderate", "strict"][t.schema.vlevel as usize];
                    let cmd = doc! { "collMod": &t.sel.coll, "validator": v, "validationLevel": level, "validationAction": if t.schema.vwarn { "warn" } else { t.schema.vaction.as_deref().unwrap_or("error") } };
                    let (title, expect) = (format!("Change the validator of {}? Writes that fail it may be rejected.", t.sel.coll), t.sel.coll.clone());
                    let action = self.mongo_pinned(Danger::Validator(cmd));
                    self.ui_fx.push(UiFx::Open(Dialog::Typed { title, expect, action }));
                }
                Err(e) => self.err(e),
            },
            SchemaA::Fold { path, .. } => {
                let closed = &mut t.schema.closed;
                if !closed.remove(&path) {
                    closed.insert(path);
                }
            }
            SchemaA::FoldAll { closed, .. } => {
                let s = &mut t.schema;
                if closed {
                    // Fields followed by a deeper row are the ones that fold.
                    let parents: Vec<String> = s.stats.windows(2).filter(|w| w[1].depth > w[0].depth).map(|w| w[0].path.clone()).collect();
                    s.closed.extend(parents);
                } else {
                    s.closed.clear();
                }
            }
            SchemaA::AddColumn { path, .. } => {
                let fields = t.fields.clone();
                let key = self.col_key(false);
                self.apply_col_acts(&key, &fields, vec![ColAct::Show(path)]);
                self.ok("Column added; open Documents to see it");
            }
            SchemaA::FilterHas { path, exists, .. } => {
                t.filter_txt = db::shell_text(&Bson::Document(doc! { path: { "$exists": exists } }));
                t.view = View::Documents;
                self.run_query();
            }
            SchemaA::Map { path, .. } => {
                let key = t.coll_key();
                let list = self.prefs.schema_maps.entry(key.clone()).or_default();
                match list.iter().position(|p| *p == path) {
                    Some(i) => _ = list.remove(i),
                    None => list.push(path),
                }
                if list.is_empty() {
                    self.prefs.schema_maps.remove(&key);
                }
                self.save_prefs();
                self.analyze_schema();
            }
        }
        Ok(Reply::Done)
    }

    /// What the Schema view asks for by itself, each frame it shows: an analysis once it opens, the validator once.
    pub fn schema_tick(&mut self) {
        let Some(t) = self.tabs.get_mut(self.cur) else { return };
        let sel = &t.sel;
        // Views refuse a $natural sort, so they only offer the first documents.
        if self.views.has(sel.conn, &sel.db, &sel.coll) {
            t.schema.last = false;
        }
        // Analyze once when the view opens; the limit keeps it light on big collections.
        let s = &t.schema;
        if s.stats.is_empty() && !s.running && s.ms.is_none() && s.err.is_none() {
            self.analyze_schema();
        }
        self.load_validator();
    }

    /// Reads the validator of the current tab's collection, unless it was read already.
    fn load_validator(&mut self) {
        if !self.tabs[self.cur].schema.vloaded && let Some(target) = self.target() {
            self.req += 1;
            let (req, coll) = (self.req, target.coll.clone());
            let s = &mut self.tabs[self.cur].schema;
            (s.vloaded, s.vreq) = (true, req);
            self.worker.command(target.client, target.db, doc! { "listCollections": 1, "filter": { "name": coll } }, &format!("validator:{req}"));
        }
    }

    /// Analyzes the current tab's collection with its saved map fields; the offline demo analyzes the page it shows.
    fn analyze_schema(&mut self) {
        let key = self.tabs[self.cur].coll_key();
        let maps = self.prefs.schema_maps.get(&key).cloned().unwrap_or_default();
        if let Some(target) = self.target() {
            self.req += 1;
            let req = self.req;
            let t = &mut self.tabs[self.cur];
            t.schema.req = req;
            t.schema.running = true;
            t.schema.err = None;
            self.worker.schema(req, target, t.query.scope(), t.schema.limit, t.schema.last, maps);
        } else if self.demo {
            let t = &mut self.tabs[self.cur];
            t.schema.stats = db::analyze_schema(&t.decoded_docs(), &maps);
            (t.schema.docs, t.schema.ms) = (t.docs.len(), Some(3));
        }
    }

    /// The analysis answer `req`; one no tab waits for any more is dropped.
    pub(in crate::action) fn schema_done(&mut self, req: u64, res: db::Res<(Vec<db::FieldStat>, usize, u128)>) {
        let Some(i) = self.tabs.iter().position(|t| t.schema.req == req) else { return };
        let t = &mut self.tabs[i];
        t.schema.running = false;
        match res {
            Ok((stats, n, ms)) => {
                t.clear_failed(View::Schema);
                (t.schema.stats, t.schema.docs, t.schema.ms, t.schema.err) = (stats, n, Some(ms), None);
            }
            Err(e) => {
                (t.schema.stats, t.schema.ms, t.schema.err) = (Vec::new(), None, Some(e.clone()));
                self.fail_view(i, View::Schema, e, "analyze the schema");
            }
        }
    }

    /// The validator read for request `req` (`listCollections` on the collection), into the tab that asked.
    pub fn validator_loaded(&mut self, req: u64, d: Document) {
        let opts = d.get_document("cursor").ok().and_then(|c| c.get_array("firstBatch").ok()).and_then(|a| a.first()).and_then(|x| x.as_document()).and_then(|x| x.get_document("options").ok()).cloned().unwrap_or_default();
        if let Some(t) = self.tabs.iter_mut().find(|t| t.schema.vreq == req) {
            let s = &mut t.schema;
            s.vtext = opts.get_document("validator").map(db::to_edit_json).unwrap_or_default();
            s.vlevel = match opts.get_str("validationLevel") {
                Ok("off") => 0,
                Ok("moderate") => 1,
                _ => 2,
            };
            s.vwarn = opts.get_str("validationAction") == Ok("warn");
            s.vaction = opts.get_str("validationAction").ok().filter(|a| !matches!(*a, "warn" | "error")).map(str::to_string);
        }
    }

    /// Relations found by the analysis of `db`: the new ones are saved, the open tabs of `db` read their previews,
    /// and the Relations window opens unless a dialog is open.
    pub(in crate::action) fn analyzed(&mut self, conn: u64, db: String, res: db::Res<Vec<(String, String, String, String)>>) {
        match res {
            Ok(found) => {
                let list = self.prefs.relations.entry(format!("{conn}/{db}")).or_default();
                let mut added = 0;
                for (coll, path, target, preview) in found {
                    if !list.iter().any(|r| r.coll == coll && r.path == path) {
                        list.push(Relation { coll, path, target, preview });
                        added += 1;
                    }
                }
                self.save_prefs();
                let reqs: Vec<u64> = self.tabs.iter().filter(|t| t.sel.conn == conn && t.sel.db == db).map(|t| t.req).collect();
                for r in reqs {
                    self.load_related(r);
                }
                self.ok(if added == 0 { "Analysis done: no new relations found.".to_string() } else { format!("Analysis done: {added} new relation{} found.", if added == 1 { "" } else { "s" }) });
                self.ui_fx.push(UiFx::IfFree(Dialog::Relations { conn, db }));
            }
            Err(e) => self.err(format!("Could not analyze relations: {e}")),
        }
    }
}
