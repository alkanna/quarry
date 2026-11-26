//! What runs once a confirmation is accepted, and the Mongo calls of a few dialogs: the writes of Danger, a pipeline that
//! writes, the preview of a many-document update, and the grouped values' Open in Aggregate. Each names its tab by uid.

use super::{Dialog, Refusal, Reply, UiFx};
use crate::core::Core;
use crate::db::{self, Target};
use crate::state::*;
use mongodb::bson::{Bson, Document};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ConfirmA {
    DeleteMatching { tab: u64, scope: db::Scope },
    DropIndex { tab: u64, name: String },
    Validator { tab: u64, cmd: Document },
    RenameMany { tab: u64, scope: db::Scope, from: String, to: String },
    UpdateMany { tab: u64, scope: db::Scope, stages: Vec<Document> },
    /// The Update dialog's Preview: what the stages would do to the matching documents, nothing written.
    UpdatePreview { tab: u64, scope: db::Scope, stages: Vec<Document> },
    /// The import's Upsert by `key`, once confirmed.
    Upsert { tab: u64, key: String },
    /// A pipeline that ends in $out or $merge, once confirmed.
    AggWrite { tab: u64, pipeline: Vec<Document> },
    /// Compare's Sync of the two collections, once confirmed.
    Sync { conn: u64 },
    /// The grouped values' Open in Aggregate: the pipeline in a new tab next to the current one, run at once.
    OpenGrouped { sel: Sel, pipeline: Vec<Document> },
}

impl Core {
    pub(super) fn confirm(&mut self, a: ConfirmA) -> Result<Reply, Refusal> {
        match a {
            ConfirmA::UpdatePreview { tab, scope, stages } => {
                self.cur_tab(tab)?;
                if let Some(t) = self.target() {
                    self.worker.update_preview(t, scope, stages);
                }
            }
            ConfirmA::OpenGrouped { sel, pipeline } => {
                let mut t = Tab::new(sel);
                t.view = View::Aggregate;
                t.agg_txt = format!("[\n{}\n]", pipeline.iter().map(|s| format!("  {}", db::shell_text(&Bson::Document(s.clone())))).collect::<Vec<_>>().join(",\n"));
                // Next to the current tab, like the original's nextToActive.
                let at = (self.cur + 1).min(self.tabs.len());
                self.tabs.insert(at, t);
                self.set_cur(at);
                self.reset_doc_state();
                self.run_pipeline(pipeline);
            }
            ConfirmA::Sync { conn } => {
                let Some(client) = self.conn(conn).filter(|c| c.writable).and_then(|c| c.client.clone()) else {
                    self.ui_fx.push(UiFx::Open(Dialog::Unlock(conn)));
                    return Ok(Reply::Done);
                };
                let timeout = self.timeout();
                self.drop_agg_row();
                let c = &mut self.tabs[self.cur].compare;
                let r = c.result.take();
                c.set_result(None);
                let Some(r) = r else { return Ok(Reply::Done) };
                let insert = if c.insert { r.only_src } else { Vec::new() };
                let replace = if c.replace { r.changed } else { Vec::new() };
                let delete = if c.delete { r.only_dst.iter().filter_map(|d| d.get("_id").cloned()).collect() } else { Vec::new() };
                self.worker.sync(Target { client, db: c.db.clone(), coll: c.coll.clone(), timeout }, insert, replace, delete);
                self.ok("Syncing… Run Compare again afterwards to check.");
            }
            ConfirmA::AggWrite { tab, pipeline } => {
                self.cur_tab(tab)?;
                if let Some((_, target)) = self.writable_target() {
                    self.req += 1;
                    let req = self.req;
                    self.tm().unwrap().agg_req = req;
                    self.worker.aggregate(req, target, pipeline);
                }
            }
            ConfirmA::DeleteMatching { tab, .. } | ConfirmA::DropIndex { tab, .. } | ConfirmA::Validator { tab, .. } | ConfirmA::RenameMany { tab, .. } | ConfirmA::UpdateMany { tab, .. } | ConfirmA::Upsert { tab, .. } => {
                self.cur_tab(tab)?;
                // The single gate every write goes through; it has said why when there is none.
                let Some((conn, target)) = self.writable_target() else { return Ok(Reply::Done) };
                match a {
                    ConfirmA::DeleteMatching { scope, .. } => self.worker.delete_many(target, scope),
                    ConfirmA::DropIndex { name, .. } => self.worker.drop_index(target, name),
                    ConfirmA::Validator { cmd, .. } => self.worker.db_change(conn, target.client, target.db, cmd, "Validator updated".into()),
                    ConfirmA::RenameMany { scope, from, to, .. } => self.worker.rename_many(target, scope, from, to),
                    ConfirmA::UpdateMany { scope, stages, .. } => self.worker.update_many(target, scope, stages),
                    ConfirmA::Upsert { key, .. } => {
                        if let Some(Ok(docs)) = self.tabs[self.cur].import.parsed.clone() {
                            self.worker.import(conn, target, docs, Some(key));
                        }
                    }
                    _ => unreachable!(),
                }
            }
        }
        Ok(Reply::Done)
    }
}
