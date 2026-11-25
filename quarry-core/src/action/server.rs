//! The Server windows. MongoDB's (src/app/server.rs draws it) is on actions: its tabs, Refresh, the 2 s refresh of
//! Operations, Kill with its second click, and the answers. It asks only while it is open and Quarry is focused.

use super::{Dialog, Reply, Refusal, UiFx};
use crate::core::Core;
use crate::db;
use crate::state::server::*;
use mongodb::bson::{doc, Bson, Document};
use serde::{Deserialize, Serialize};
use std::time::Instant;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ServerA {
    Mongo(MongoServerA),
    Sql(super::SqlServerA),
    Pg(super::PgServerA),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum MongoServerA {
    Open { conn: u64 },
    /// Overview (false) or Operations (true).
    Ops(bool),
    Refresh,
    /// Operations' "Refresh every 2 s".
    AutoOps(bool),
    /// Kill… on the operation with this compact opid, armed at this egui input time.
    ArmKill { op: String, at: f64 },
    /// Keep: the armed Kill is dropped.
    Disarm,
    /// Kill it, on the armed operation.
    Kill { op: String },
    Unlock,
    Close,
}

/// Running operations, longest first, without the currentOp that listed them.
pub fn ops_of(d: &Document) -> Vec<Document> {
    let mut ops: Vec<Document> = d.get_array("inprog").map(|a| a.iter().filter_map(Bson::as_document).filter(|o| !o.get_document("command").is_ok_and(|c| c.contains_key("currentOp"))).cloned().collect()).unwrap_or_default();
    ops.sort_by(|a, b| num(b, "microsecs_running").unwrap_or(0.0).total_cmp(&num(a, "microsecs_running").unwrap_or(0.0)));
    ops
}

pub fn num(d: &Document, path: &str) -> Option<f64> {
    db::get_path(d, path).and_then(db::num)
}

impl Core {
    pub(super) fn server(&mut self, a: ServerA) -> Result<Reply, Refusal> {
        match a {
            ServerA::Mongo(a) => self.mongo_server(a),
            ServerA::Sql(a) => self.sql_server(a),
            ServerA::Pg(a) => self.pg_server(a),
        }
    }

    fn mongo_server(&mut self, a: MongoServerA) -> Result<Reply, Refusal> {
        if let MongoServerA::Open { conn } = a {
            self.req += 1;
            self.server = Some(ServerWin::new(conn, self.req));
            return Ok(Reply::Done);
        }
        let Some(conn) = self.server.as_ref().map(|s| s.conn) else { return Ok(Reply::Done) };
        let s = self.server.as_mut().unwrap();
        match a {
            MongoServerA::Ops(on) => {
                if on != s.ops_tab {
                    s.ops_tab = on;
                    s.next = None;
                }
            }
            MongoServerA::Refresh => {
                s.status = s.status.take().filter(|r| r.is_ok());
                s.rs = s.rs.take().filter(|r| r.is_ok());
                s.hello = None;
                s.refresh = true;
                s.next = None;
            }
            MongoServerA::AutoOps(on) => s.auto_ops = on,
            MongoServerA::ArmKill { op, at } => s.confirm_kill = Some((op, at)),
            MongoServerA::Disarm => s.confirm_kill = None,
            MongoServerA::Kill { op } => {
                if s.confirm_kill.as_ref().is_none_or(|(k, _)| *k != op) {
                    return Ok(Reply::Done);
                }
                s.confirm_kill = None;
                let id = s.ops.as_ref().and_then(|r| r.as_ref().ok()).and_then(|ops| ops.iter().map(|o| o.get("opid").cloned().unwrap_or(Bson::Null)).find(|id| db::compact(id) == op));
                if !self.conn_writable(conn) {
                    return Err(self.refuse_ro(conn, None));
                }
                if let (Some(id), Some(client)) = (id, self.conn(conn).and_then(|c| c.client.clone())) {
                    let tag = format!("server:{}:kill", self.server.as_ref().map_or(0, |s| s.id));
                    self.worker.command(client, "admin".into(), doc! { "killOp": 1, "op": id }, &tag);
                }
            }
            MongoServerA::Unlock => self.ui_fx.push(UiFx::Open(Dialog::Unlock(conn))),
            MongoServerA::Close => self.server = None,
            MongoServerA::Open { .. } => {}
        }
        Ok(Reply::Done)
    }

    /// Asks for the section on screen every POLL once the last answers are in. The window calls it each frame while
    /// Quarry is focused; true while the section refreshes by itself, so the UI wakes up for it.
    pub fn server_poll(&mut self) -> bool {
        let Some(s) = self.server.as_mut() else { return false };
        let auto = !s.ops_tab || s.auto_ops;
        let Some(client) = self.conns.iter().find(|c| c.meta.id == s.conn).and_then(|c| c.client.clone()).filter(|_| !self.demo) else { return false };
        let now = Instant::now();
        if s.waiting > 0 || s.next.is_some_and(|t| now < t) {
            return auto;
        }
        s.next = Some(now + POLL);
        let (id, mut sent) = (s.id, 0);
        let mut ask = |cmd: Document, what: &str| {
            sent += 1;
            self.worker.command(client.clone(), "admin".into(), cmd, &format!("server:{id}:{what}"));
        };
        if s.ops_tab {
            if s.auto_ops || s.ops.is_none() || s.refresh {
                let mut cmd = doc! { "currentOp": 1, "active": true };
                if s.own_only {
                    cmd.insert("$ownOps", true);
                }
                ask(cmd, "ops");
            }
        } else {
            // A section that failed is not asked again until Refresh, so a missing privilege costs one call.
            if !matches!(s.status, Some(Err(_))) {
                ask(doc! { "serverStatus": 1, "repl": 0, "metrics": 0, "locks": 0 }, "status");
            }
            if !matches!(s.rs, Some(Err(_))) {
                ask(doc! { "replSetGetStatus": 1 }, "rs");
            }
            if s.hello.is_none() {
                ask(doc! { "hello": 1 }, "hello");
            }
        }
        s.waiting = sent;
        s.refresh = false;
        auto
    }

    /// Applies an answer to one of the Server window's commands. Errors stay in the window
    /// instead of the status bar, so a role without privileges is not told so every 2 seconds.
    pub fn server_reply(&mut self, tag: &str, r: db::Res<Document>) {
        let mut parts = tag.splitn(3, ':').skip(1);
        let (id, what) = (parts.next().and_then(|i| i.parse::<u64>().ok()), parts.next().unwrap_or_default());
        if what == "kill" {
            match r {
                Ok(_) => {
                    self.ok("Asked the server to kill the operation");
                    if let Some(s) = self.server.as_mut().filter(|s| Some(s.id) == id) {
                        s.refresh = true;
                        s.next = None;
                    }
                }
                Err(e) if db::is_unauthorized(&e) => self.err("Your role cannot kill this operation: killing other users' operations needs the killop privilege."),
                Err(e) => self.err(e),
            }
            return;
        }
        let Some(s) = self.server.as_mut().filter(|s| Some(s.id) == id) else { return };
        s.waiting = s.waiting.saturating_sub(1);
        match what {
            "status" => {
                if let Some(Ok(cur)) = s.status.take() {
                    s.prev = Some(cur);
                }
                s.status = Some(r.map(|d| (Instant::now(), d)));
            }
            "hello" => s.hello = Some(r),
            "rs" => s.rs = Some(r),
            "ops" => match r {
                Err(e) if db::is_unauthorized(&e) && !s.own_only => {
                    s.own_only = true;
                    s.refresh = true;
                    s.next = None;
                }
                r => s.ops = Some(r.map(|d| ops_of(&d))),
            },
            _ => {}
        }
    }
}
