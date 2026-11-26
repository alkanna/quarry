//! A document field's menu in the tree: pin or highlight its column, count its values, index it, follow the id it
//! holds to its document, rename it; and the answers of those reads and of the page's reference previews.

use super::super::{Dialog, Refusal, Reply, UiFx};
use crate::core::Core;
use crate::db::{self, ColOp, ColOut, Target, COL_VALUES_CAP};
use crate::state::*;
use mongodb::bson::{doc, Bson, Document};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum FieldA {
    /// Pin as column: shown, pinned first, and the Table layout.
    PinColumn { tab: u64, path: String },
    Highlight { tab: u64, path: String },
    /// Count the values of the field, in the documents matching the query when `filtered`; the answer opens a window.
    Group { tab: u64, path: String, value: Bson, filtered: bool },
    /// The number of distinct values, under the query.
    Distinct { tab: u64, path: String },
    /// Index this field: the Indexes view with the New index dialog on it.
    Index { tab: u64, path: String },
    /// Opens the document whose `_id` the field holds, through a saved relation or by looking for it.
    GoRef { tab: u64, path: String, value: Bson },
    /// Rename the field of document `doc`, in a dialog.
    Rename { tab: u64, doc: Document, path: Vec<String> },
}

/// Names the collection the values came from, not whichever tab is current when a result arrives.
pub fn col_title(agg: bool, op: ColOp, coll: &str, path: &str) -> String {
    if agg { format!("{}: {path} in the results", op.label()) } else { format!("{}: {coll}.{path}", op.label()) }
}

impl Core {
    pub(super) fn field(&mut self, a: FieldA) -> Result<Reply, Refusal> {
        let tab = match &a {
            FieldA::PinColumn { tab, .. } | FieldA::Highlight { tab, .. } | FieldA::Group { tab, .. } | FieldA::Distinct { tab, .. } | FieldA::Index { tab, .. } | FieldA::GoRef { tab, .. } | FieldA::Rename { tab, .. } => *tab,
        };
        self.cur_tab(tab)?;
        match a {
            FieldA::PinColumn { path, .. } => {
                let key = self.col_key(false);
                let fields = self.tabs[self.cur].fields.clone();
                self.apply_col_acts(&key, &fields, vec![ColAct::Show(path.clone()), ColAct::Pin(path)]);
                self.prefs.table = true;
            }
            FieldA::Highlight { path, .. } => {
                let fields = self.tabs[self.cur].fields.clone();
                let key = self.col_key(false);
                self.apply_col_acts(&key, &fields, vec![ColAct::Highlight(path)]);
            }
            FieldA::Group { path, value, filtered, .. } => {
                let filter = if filtered { self.tabs[self.cur].query.filter.clone() } else { Document::new() };
                // Counted in the background and shown in a window, so the tab's own pipeline is kept;
                // "Open in Aggregate" opens the pipeline in a new tab, as the original does.
                match (db::group_count_pipeline(&path, &value, filter), self.target()) {
                    (Err(e), _) => self.err(e),
                    (_, None) => self.warn_offline(),
                    (Ok(p), Some(target)) => {
                        let t = &self.tabs[self.cur];
                        let (conn, co) = (t.sel.conn, filtered.then(|| t.query.collation.clone()).flatten());
                        self.ok(format!("Counting the values of {path}…"));
                        self.worker.group_count(conn, target, path, p, co);
                    }
                }
            }
            FieldA::Distinct { path, .. } => match self.target() {
                Some(target) => {
                    let scope = self.tabs[self.cur].query.scope();
                    self.worker.distinct_count(target, path, scope);
                }
                None => self.warn_offline(),
            },
            FieldA::Index { path, .. } => {
                let t = &mut self.tabs[self.cur];
                t.view = View::Indexes;
                t.selected = None;
                self.reset_doc_state();
                self.load_indexes();
                let keys = db::shell_text(&Bson::Document(doc! { db::strip_indices(&path): 1 }));
                self.ui_fx.push(UiFx::Open(Dialog::CreateIndex(Box::new(crate::state::indexes::IndexForm { keys, strength: 3, ..Default::default() }))));
            }
            FieldA::GoRef { path, value, .. } => {
                let s = self.tabs[self.cur].sel.clone();
                let path = db::strip_indices(&path);
                let rel = self.prefs.relations.get(&format!("{}/{}", s.conn, s.db)).and_then(|l| l.iter().find(|r| r.coll == s.coll && r.path == path)).cloned();
                match (rel, self.conn(s.conn).and_then(|c| c.client.clone())) {
                    (_, None) => self.warn_offline(),
                    (Some(r), _) => self.go_ref(s.conn, s.db, r.target, value),
                    (None, Some(client)) => {
                        let colls = self.conn(s.conn).and_then(|c| c.dbs.iter().find(|d| d.0 == s.db)).and_then(|d| d.1.clone()).unwrap_or_default();
                        self.ok("Looking for the collection that holds this id…");
                        let timeout = self.timeout();
                        self.worker.find_ref(s.conn, Target { client, db: s.db, coll: s.coll, timeout }, colls, path, value);
                    }
                }
            }
            FieldA::Rename { doc, path, .. } => {
                if !self.sel_writable() {
                    let conn = self.tabs[self.cur].sel.conn;
                    return Err(self.refuse_ro_quiet(conn));
                }
                let name = path.last().cloned().unwrap_or_default();
                self.ui_fx.push(UiFx::Open(Dialog::Rename { doc, path, name }));
            }
        }
        Ok(Reply::Done)
    }

    /// Opens the referenced document in its collection's tab.
    pub fn go_ref(&mut self, conn: u64, db: String, coll: String, id: Bson) {
        self.open_tab(Sel { conn, db, coll, ..Default::default() }, Open::Link);
        let t = self.tm().unwrap();
        t.view = View::Documents;
        t.filter_txt = format!("{{ _id: {} }}", db::shell_text(&id));
        t.clear_options();
        self.run_query();
    }

    /// The value counts of a field, shown in a window once the one open closes.
    pub(in crate::action) fn grouped(&mut self, conn: u64, db: String, coll: String, path: String, pipeline: Vec<Document>, res: db::Res<Vec<Document>>) {
        match res {
            Ok(rows) => {
                self.ok(format!("{} distinct values of {path}", rows.len()));
                self.ui_fx.push(UiFx::Late(Dialog::Grouped { sel: Sel { conn, db, coll, ..Default::default() }, path, pipeline, rows }));
            }
            Err(e) => self.err(e),
        }
    }

    /// Previews of the ids the page `req` holds.
    pub(in crate::action) fn related_done(&mut self, req: u64, res: db::Res<Vec<(String, String)>>) {
        match (res, self.tab_by_req(req)) {
            (Ok(v), Some(t)) => t.related.extend(v),
            (Err(e), _) => self.err(format!("Could not load related documents: {e}")),
            _ => {}
        }
    }

    /// The collection holding an id a field pointed at: saved as a relation of `db`, then opened.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::action) fn ref_found(&mut self, conn: u64, db: String, coll: String, path: String, value: Bson, res: db::Res<Option<(String, String)>>) {
        match res {
            Ok(Some((target, preview))) => {
                self.prefs.relations.entry(format!("{conn}/{db}")).or_default().push(Relation { coll: coll.clone(), path: path.clone(), target: target.clone(), preview: preview.clone() });
                self.save_prefs();
                let reqs: Vec<u64> = self.tabs.iter().filter(|t| t.sel.conn == conn && t.sel.db == db && t.sel.coll == coll).map(|t| t.req).collect();
                for r in reqs {
                    self.load_related(r);
                }
                self.go_ref(conn, db, target.clone(), value);
                self.ok(format!("Linked {path} to {target}, showing {preview} next to its ids. Edit it under Data relations in the database menu."));
            }
            Ok(None) => self.err(format!("No collection of {db} has a document with this _id.")),
            Err(e) => self.err(e),
        }
    }

    /// A column data action read on the server over the whole query, of a MongoDB collection or a SQL table.
    pub(in crate::action) fn column_values_done(&mut self, conn: u64, op: ColOp, coll: String, path: String, res: db::Res<(ColOut, bool)>) {
        let (noun, one) = if self.is_sql(conn) { ("rows", "row") } else { ("documents", "document") };
        match res {
            Ok((out, capped)) => {
                let scope = if capped { format!("From the first {COL_VALUES_CAP} {noun} of the query, in its sort order.") } else { format!("From every {one} matching the query.") };
                let title = col_title(false, op, &coll, &path);
                match out {
                    ColOut::Copy(text, n) => {
                        self.ui_fx.push(UiFx::CopyText(text));
                        self.ok(format!("Copied {n} values of {path}"));
                    }
                    ColOut::Notice(body) => self.ui_fx.push(UiFx::Open(Dialog::Text { title, body: format!("{body}\n\n{scope}"), code: false })),
                    // Groups open their own window from the page; the server read never answers with them.
                    ColOut::Groups(_) => {}
                }
            }
            Err(e) => self.err(e),
        }
    }
}
