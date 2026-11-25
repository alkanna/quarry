//! The Compare view: this tab's collection against a target picked by connection, database and collection, the
//! comparison by `_id` and the sync that copies the differences over.

use super::super::{Dialog, Refusal, Reply, UiFx};
use crate::core::Core;
use crate::db::{self, Target};
use crate::state::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum CmpA {
    /// The target's connection; its database and collection start over.
    Conn { tab: u64, conn: u64 },
    /// The target's database; its collections are listed if they were not yet.
    Db { tab: u64, db: String },
    Coll { tab: u64, coll: String },
    /// A count card: the list of differences shown below it.
    List { tab: u64, list: CmpList },
    Page { tab: u64, page: usize },
    /// The sync check boxes: copy the documents only in source, overwrite the different ones, delete those only in target.
    Opts { tab: u64, insert: bool, replace: bool, delete: bool },
    Run { tab: u64 },
    /// Sync to target: asks for the target's name first.
    Sync { tab: u64 },
}

impl Core {
    pub(super) fn compare(&mut self, a: CmpA) -> Result<Reply, Refusal> {
        let tab = match &a {
            CmpA::Conn { tab, .. } | CmpA::Db { tab, .. } | CmpA::Coll { tab, .. } | CmpA::List { tab, .. } | CmpA::Page { tab, .. } | CmpA::Opts { tab, .. } | CmpA::Run { tab } | CmpA::Sync { tab } => *tab,
        };
        self.cur_tab(tab)?;
        let i = self.cur;
        let c = &mut self.tabs[i].compare;
        match a {
            CmpA::Conn { conn, .. } => {
                if c.conn != conn {
                    (c.conn, c.db, c.coll) = (conn, String::new(), String::new());
                }
            }
            CmpA::Db { db, .. } => {
                c.db = db.clone();
                c.coll.clear();
                let tc = c.conn;
                let unlisted = self.conn(tc).and_then(|c| c.dbs.iter().find(|d| d.0 == db)).is_some_and(|d| d.1.is_none());
                if unlisted && let Some(client) = self.conn(tc).and_then(|c| c.client.clone()) {
                    self.worker.collections(tc, client, db);
                }
            }
            CmpA::Coll { coll, .. } => c.coll = coll,
            CmpA::List { list, .. } => {
                if c.result.is_some() && list != c.list {
                    (c.list, c.page) = (list, 0);
                    self.drop_agg_row();
                }
            }
            CmpA::Page { page, .. } => {
                c.page = page;
                self.drop_agg_row();
            }
            CmpA::Opts { insert, replace, delete, .. } => (c.insert, c.replace, c.delete) = (insert, replace, delete),
            CmpA::Run { .. } => return self.compare_run(i),
            CmpA::Sync { .. } => return self.compare_sync(i),
        }
        self.compare_follow();
        Ok(Reply::Done)
    }

    /// Syncing writes the old diff to whatever target is picked, so a diff for another one is dropped. Open rows are
    /// kept by position, so whenever the rows under them change they are closed. Run each frame and after each pick.
    pub fn compare_follow(&mut self) {
        let Some(c) = self.tabs.get_mut(self.cur).map(|t| &mut t.compare) else { return };
        if c.result.is_some() && c.ran != (c.conn, c.db.clone(), c.coll.clone()) {
            c.set_result(None);
            self.drop_agg_row();
        }
    }

    fn compare_run(&mut self, i: usize) -> Result<Reply, Refusal> {
        let src = self.tabs[i].sel.clone();
        let c = &self.tabs[i].compare;
        let (tc, tdb, tcoll) = (c.conn, c.db.clone(), c.coll.clone());
        let same = tc == src.conn && tdb == src.db && tcoll == src.coll;
        if tcoll.is_empty() || same || c.running {
            return Err(Self::mongo_quiet());
        }
        let (Some(a), Some(b)) = (self.conn(src.conn).and_then(|c| c.client.clone()), self.conn(tc).and_then(|c| c.client.clone())) else {
            return Err(self.refuse(if self.demo { "Offline demo: nothing to compare." } else { "Connect both sides first." }));
        };
        self.req += 1;
        let req = self.req;
        let to = self.timeout();
        self.drop_agg_row();
        let c = &mut self.tabs[i].compare;
        c.set_result(None);
        (c.req, c.running, c.ran) = (req, true, (tc, tdb.clone(), tcoll.clone()));
        self.worker.compare(req, Target { client: a, db: src.db, coll: src.coll, timeout: to }, Target { client: b, db: tdb, coll: tcoll, timeout: to });
        Ok(Reply::Pending(req))
    }

    fn compare_sync(&mut self, i: usize) -> Result<Reply, Refusal> {
        self.compare_follow();
        let c = &self.tabs[i].compare;
        let Some(r) = c.result.as_ref() else { return Err(Self::mongo_quiet()) };
        let (tc, tdb, tcoll) = (c.conn, c.db.clone(), c.coll.clone());
        // The target may be another connection than the tab's, and Unlock comes with nothing to run after it.
        if !self.conn(tc).is_some_and(|c| c.writable) {
            self.ui_fx.push(UiFx::Open(Dialog::Unlock(tc)));
            return Ok(Reply::Done);
        }
        let n = |on: bool, k: usize| if on { k } else { 0 };
        let title = format!(
            "Write to {tdb}.{tcoll}: copy {}, overwrite {} and delete {} documents? Overwrites skip documents changed since the comparison. This has no Undo.",
            n(c.insert, r.only_src.len()),
            n(c.replace, r.changed.len()),
            n(c.delete, r.only_dst.len())
        );
        let action = self.mongo_pinned(Danger::Sync { conn: tc });
        self.ui_fx.push(UiFx::Open(Dialog::Typed { title, expect: tcoll, action }));
        Ok(Reply::Done)
    }

    /// The comparison answer `req`; one no tab waits for any more is dropped.
    pub(in crate::action) fn compared(&mut self, req: u64, res: db::Res<db::Compared>) {
        let Some(i) = self.tabs.iter().position(|t| t.compare.req == req) else { return };
        let t = &mut self.tabs[i];
        t.compare.running = false;
        match res {
            Ok(r) => {
                t.clear_failed(View::Compare);
                t.compare.set_result(Some(r));
            }
            Err(e) => self.fail_view(i, View::Compare, e, "compare"),
        }
    }
}
