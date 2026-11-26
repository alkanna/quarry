//! The MongoDB views around the Documents view: Compare, Schema with its validator, Import and the Documents menu's
//! Export, and the conveniences of a document's field menu (group, distinct, go to a reference, index, rename).
//! src/app/compare.rs, schema.rs, import.rs and tree.rs draw them.

mod compare;
mod fields;
mod schema;
mod transfer;

pub use compare::CmpA;
pub use fields::{col_title, FieldA};
pub use schema::SchemaA;
pub use transfer::ImportA;

use super::{Refusal, Reply};
use crate::core::Core;
use crate::db;
use crate::state::{Danger, View};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum MongoA {
    Compare(CmpA),
    Schema(SchemaA),
    Import(ImportA),
    Field(FieldA),
    /// Explains the Documents query or the Aggregate pipeline of the tab, whichever view is open.
    Explain { tab: u64 },
    /// The data relations dialog's list for `key` ("conn/db") as it was edited; an empty list removes the entry.
    Relations { key: String, list: Vec<crate::state::Relation> },
    /// The relations dialog closed: the tabs of that database ask again for the previews of their ids.
    ReloadRelated { conn: u64, db: String },
    /// Samples collection `coll` (every collection when empty) for fields that hold ids of other collections.
    AnalyzeRelations { conn: u64, db: String, coll: String },
}

impl Core {
    pub(super) fn mongo(&mut self, a: MongoA) -> Result<Reply, Refusal> {
        match a {
            MongoA::Compare(a) => self.compare(a),
            MongoA::Schema(a) => self.schema(a),
            MongoA::Import(a) => self.import(a),
            MongoA::Field(a) => self.field(a),
            MongoA::Explain { tab } => {
                self.cur_tab(tab)?;
                self.mongo_explain();
                Ok(Reply::Done)
            }
            MongoA::Relations { key, list } => {
                if list.is_empty() {
                    self.prefs.relations.remove(&key);
                } else {
                    self.prefs.relations.insert(key, list);
                }
                self.save_prefs();
                Ok(Reply::Done)
            }
            MongoA::ReloadRelated { conn, db } => {
                let reqs: Vec<u64> = self.tabs.iter().filter(|t| t.sel.conn == conn && t.sel.db == db).map(|t| t.req).collect();
                for r in reqs {
                    if let Some(t) = self.tab_by_req(r) {
                        t.related.clear();
                    }
                    self.load_related(r);
                }
                Ok(Reply::Done)
            }
            MongoA::AnalyzeRelations { conn, db, coll } => {
                match self.conn(conn).and_then(|x| x.client.clone()) {
                    Some(client) => {
                        let all: Vec<String> = self.conn(conn).and_then(|c| c.dbs.iter().find(|d| d.0 == db)).and_then(|d| d.1.clone()).unwrap_or_default().into_iter().filter(|c| !c.starts_with("system.") && !self.views.has(conn, &db, c)).collect();
                        let colls = if coll.is_empty() { all.clone() } else { vec![coll.clone()] };
                        self.ok(format!("Analyzing data relations of {}…", if coll.is_empty() { db.clone() } else { format!("{db}.{coll}") }));
                        let timeout = self.timeout();
                        self.worker.analyze_relations(conn, db::Target { client, db, coll, timeout }, colls, all);
                    }
                    None => self.warn_offline(),
                }
                Ok(Reply::Done)
            }
        }
    }

    fn mongo_explain(&mut self) {
        let Some(t) = self.t() else { return };
        let what = match t.view {
            View::Documents if !db::is_shell(&t.filter_txt) => Ok(db::ExplainOf::Find(Box::new(t.query.clone()))),
            View::Aggregate if !db::is_shell(&t.agg_txt) => db::parse_pipeline(&t.agg_txt).and_then(|p| if db::has_write_stage(&p) { Err("Explain would run the write stage.".into()) } else { Ok(db::ExplainOf::Aggregate(p)) }),
            View::Documents | View::Aggregate => Err("Run the pasted query first, then explain it.".into()),
            _ => return,
        };
        match (what, self.target()) {
            _ if self.demo => self.ok("Offline demo: nothing to explain."),
            (Err(e), _) => self.err(e),
            (Ok(w), Some(target)) => {
                self.req += 1;
                self.explain_req = self.req;
                self.worker.explain(self.req, target, w, self.page_size());
            }
            _ => self.warn_offline(),
        }
    }

    /// Ties a confirmation to the current tab, as the app's `pinned` does, so a tab switch before Confirm cannot redirect it.
    fn mongo_pinned(&self, d: Danger) -> Danger {
        match self.t() {
            Some(t) => Danger::At(t.sel.clone(), Box::new(d)),
            None => d,
        }
    }

    /// A refusal that the drawing already prevents (a disabled button), so it says nothing.
    fn mongo_quiet() -> Refusal {
        Refusal { kind: super::RefusalKind::Invalid, msg: String::new() }
    }
}
