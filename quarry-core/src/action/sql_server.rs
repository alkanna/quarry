//! The Server windows of MySQL and MariaDB (src/app/sql/server.rs draws it) and of PostgreSQL (src/app/sql/pg_server.rs):
//! their tabs, switches, selection, Refresh, the polling of the tab on screen, Kill, Cancel and Terminate with their second
//! click, and the answers. They ask only while open and Quarry is focused, and keep errors in the window.

use super::{Dialog, Reply, Refusal, UiFx};
use crate::core::Core;
use crate::db;
use crate::db::sql::server::{Answer, KillTarget, StatusGroup};
use crate::state::sql::pg_server::*;
use crate::state::sql::server::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum SqlServerA {
    Open { conn: u64 },
    Tab(SqlServerTab),
    Refresh,
    /// Processes' and Locks' "Refresh every 2 s".
    Auto(bool),
    HideSleeping(bool),
    Select(u64),
    /// Kill on thread `id` of the list on screen, the whole connection or only its statement, armed at this egui input time.
    ArmKill { id: u64, whole: bool, at: f64 },
    /// Keep, or the armed thread left the list: the armed Kill is dropped.
    Disarm,
    /// The second click on the armed Kill of thread `id`.
    Kill { id: u64 },
    /// Variables: the global (true) or the session values.
    Global(bool),
    StGroup(StatusGroup),
    /// Status' "Refresh every 2 s".
    StAuto(bool),
    Unlock,
    Close,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum PgServerA {
    Open { conn: u64 },
    Tab(PgTab),
    Refresh,
    /// Activity's and Locks' "Refresh every 2 s".
    Auto(bool),
    HideIdle(bool),
    ChangedOnly(bool),
    Select(u64),
    /// The setting whose detail shows, by name.
    Setting(Option<String>),
    /// Cancel query (`terminate` false) or Terminate on session `pid`, armed at this egui input time.
    ArmSignal { pid: u64, terminate: bool, at: f64 },
    Disarm,
    /// The second click on the armed signal of session `pid`.
    Signal { pid: u64 },
    /// Copy query on a row whose text the list cut: asks for the whole of it.
    FetchQuery { pid: u64 },
    Unlock,
    Close,
}

/// Database sizes walk the data directory, so the Overview asks for them this often and not on every poll.
const SIZES_EVERY: Duration = Duration::from_secs(30);
/// Each pg_blocking_pids call takes every lock manager partition lock, so blockers are asked this often; the polls between
/// keep the last ones for sessions that still wait.
const BLOCKERS_EVERY: Duration = Duration::from_secs(5);

/// What to tell the person about a Kill answer, and only for a request that was really sent: the answer can arrive
/// after the Server window was closed, so it is matched by request id on Core and not on the window.
pub fn kill_toast(kills: &mut HashSet<u64>, req: u64, ans: &Answer) -> Option<Result<String, String>> {
    match ans {
        Answer::Kill { whole, res } if kills.remove(&req) => Some(res.clone().map(|_| if *whole { "Ended the connection".to_string() } else { "Stopped the statement".to_string() })),
        _ => None,
    }
}

impl Core {
    pub(super) fn sql_server(&mut self, a: SqlServerA) -> Result<Reply, Refusal> {
        if let SqlServerA::Open { conn } = a {
            let Some(c) = self.conn(conn) else { return Ok(Reply::Done) };
            let quarry = c.sql.as_ref().map(|d| d.dialect).map(|d| db::sql::server::set_by_quarry(&c.meta.advanced, d, db::sql::advanced::local_offset_secs())).unwrap_or_default();
            self.sql_server = Some(SqlServerWin::new(conn, quarry));
            return Ok(Reply::Done);
        }
        let Some(s) = self.sql_server.as_mut() else { return Ok(Reply::Done) };
        let conn = s.conn;
        match a {
            SqlServerA::Tab(t) => {
                if t != s.tab {
                    s.tab = t;
                    // Ask for the new tab's data on the next frame instead of when the old tab's timer ends.
                    s.next = None;
                }
            }
            SqlServerA::Refresh => s.refresh = true,
            SqlServerA::Auto(on) => s.auto = on,
            SqlServerA::HideSleeping(on) => s.hide_sleeping = on,
            SqlServerA::Select(id) => s.sel = Some(id),
            SqlServerA::ArmKill { id, whole, at } => {
                let target = if s.tab == SqlServerTab::Locks {
                    match &s.locks {
                        Some(Ok(l)) => l.procs.get(&id).map(|p| (p.clone(), l.identity.clone())),
                        _ => None,
                    }
                } else {
                    match &s.procs {
                        Some(Ok(p)) => p.rows.iter().find(|r| r.id == id).map(|r| (r.clone(), p.identity.clone())),
                        _ => None,
                    }
                };
                if let Some((p, identity)) = target {
                    s.confirm = Some(Confirm { p, identity, whole, at });
                }
            }
            SqlServerA::Disarm => s.confirm = None,
            SqlServerA::Kill { id } => {
                let Some(c) = s.confirm.take_if(|c| c.p.id == id) else { return Ok(Reply::Done) };
                let read_only = !s.writable;
                if !self.conn_writable(conn) {
                    return Err(self.refuse_ro(conn, None));
                }
                if let Some(db) = self.conn(conn).and_then(|c| c.sql.clone()) {
                    self.req += 1;
                    self.sql_kills.insert(self.req);
                    self.worker.sql_server_kill(db, self.req, KillTarget::of(&c.p, &c.identity), c.whole, read_only);
                    return Ok(Reply::Pending(self.req));
                }
            }
            SqlServerA::Global(on) => {
                if on != s.global {
                    // The other scope has other values, so its rows are asked for again.
                    (s.global, s.vars, s.vars_req, s.vars_wait) = (on, None, 0, false);
                }
            }
            SqlServerA::StGroup(g) => s.st_group = g,
            SqlServerA::StAuto(on) => s.st_auto = on,
            SqlServerA::Unlock => self.ui_fx.push(UiFx::Open(Dialog::Unlock(conn))),
            SqlServerA::Close => self.sql_server = None,
            SqlServerA::Open { .. } => {}
        }
        Ok(Reply::Done)
    }

    pub(super) fn pg_server(&mut self, a: PgServerA) -> Result<Reply, Refusal> {
        if let PgServerA::Open { conn } = a {
            let Some(c) = self.conn(conn) else { return Ok(Reply::Done) };
            self.pg_server = Some(PgServerWin::new(conn, db::sql::pg::server::set_by_quarry(&c.meta.advanced)));
            return Ok(Reply::Done);
        }
        let Some(s) = self.pg_server.as_mut() else { return Ok(Reply::Done) };
        let conn = s.conn;
        match a {
            PgServerA::Tab(tab) => {
                if tab != s.tab {
                    s.tab = tab;
                    match tab {
                        PgTab::Overview => s.ov_next = None,
                        PgTab::Activity | PgTab::Locks => s.act_next = None,
                        PgTab::Settings => {}
                        // The counters only grow, so each visit reads them again.
                        PgTab::Counters => s.refresh = true,
                    }
                    // Locks is about the blockers, so it asks for them at once.
                    if tab == PgTab::Locks {
                        s.blk_at = None;
                    }
                    s.confirm = None;
                }
            }
            PgServerA::Refresh => s.refresh = true,
            PgServerA::Auto(on) => s.auto = on,
            PgServerA::HideIdle(on) => s.hide_idle = on,
            PgServerA::ChangedOnly(on) => s.changed_only = on,
            PgServerA::Select(pid) => s.sel = Some(pid),
            PgServerA::Setting(name) => s.set_sel = name,
            PgServerA::ArmSignal { pid, terminate, at } => {
                if let Some(r) = s.rows().iter().find(|r| r.pid == pid).cloned() {
                    s.confirm = Some(PgConfirm { s: r, terminate, at });
                }
            }
            PgServerA::Disarm => s.confirm = None,
            PgServerA::Signal { pid } => {
                let Some(c) = s.confirm.take_if(|c| c.s.pid == pid) else { return Ok(Reply::Done) };
                let (read_only, target) = (!s.writable, db::sql::pg::server::Target::of(&c.s, s.server()));
                if !self.conn_writable(conn) {
                    return Err(self.refuse_ro(conn, None));
                }
                if let Some(db) = self.conn(conn).and_then(|c| c.sql.clone()) {
                    self.req += 1;
                    self.sql_kills.insert(self.req);
                    self.worker.pg_server_signal(db, self.req, target, c.terminate, read_only);
                    return Ok(Reply::Pending(self.req));
                }
            }
            PgServerA::FetchQuery { pid } => {
                let Some(r) = s.rows().iter().find(|r| r.pid == pid).cloned() else { return Ok(Reply::Done) };
                if let Some(db) = self.conns.iter().find(|c| c.meta.id == conn).and_then(|c| c.sql.clone()) {
                    self.req += 1;
                    s.copy_req = self.req;
                    self.worker.pg_server_query(db, self.req, &r);
                    return Ok(Reply::Pending(self.req));
                }
            }
            PgServerA::Unlock => self.ui_fx.push(UiFx::Open(Dialog::Unlock(conn))),
            PgServerA::Close => self.pg_server = None,
            PgServerA::Open { .. } => {}
        }
        Ok(Reply::Done)
    }

    /// Quarry's own sessions on connection `conn` by server thread id, with what each one is: the ids kept for Stop
    /// (a Query run, a running export, an import or script file, the shell's relays). They get no Kill, because
    /// Stop also sets the cancel flag so the work ends instead of carrying on after the statement is cut.
    pub fn own_sessions(&self, conn: u64) -> HashMap<u64, String> {
        let mut own = HashMap::new();
        let mut add = |id: u64, what: &str| {
            if id != 0 {
                own.insert(id, what.to_string());
            }
        };
        for t in &self.tabs {
            if let Some(s) = t.sql.as_ref().filter(|s| s.run_req != 0 && t.sel.conn == conn) {
                add(s.run.conn.load(Ordering::SeqCst), "Query tab");
            }
            if t.shell.cancel.is_some() {
                for (c, id) in &t.shell.relays {
                    if *c == conn {
                        add(id.load(Ordering::SeqCst), "Shell");
                    }
                }
            }
        }
        if let Some(j) = self.export_job.as_ref().filter(|j| j.db.as_ref().is_some_and(|d| d.conn == conn)) {
            add(j.id.load(Ordering::SeqCst), "Export");
        }
        if let Some(j) = self.sql_job.as_ref().filter(|j| j.conn == conn) {
            add(j.id.load(Ordering::SeqCst), if j.kind == crate::state::sql::dump::JobKind::Import { "Import" } else { "Script file" });
        }
        own
    }

    /// What the Server windows read from the rest of Quarry each frame: whether writes are unlocked, and Quarry's own
    /// sessions. A locked connection offers no Kill, and a confirmation armed before the lock is dropped.
    pub fn server_windows_frame(&mut self) {
        if let Some(conn) = self.sql_server.as_ref().map(|s| s.conn) {
            let (writable, own) = (self.conn(conn).map_or(self.demo, |c| c.writable), self.own_sessions(conn));
            let s = self.sql_server.as_mut().unwrap();
            (s.writable, s.own) = (writable, own);
            if !writable {
                s.confirm = None;
            }
        }
        if let Some(conn) = self.pg_server.as_ref().map(|s| s.conn) {
            let (writable, own) = (self.conn(conn).map_or(self.demo, |c| c.writable), self.own_sessions(conn));
            let s = self.pg_server.as_mut().unwrap();
            (s.writable, s.own) = (writable, own);
            if !writable {
                s.confirm = None;
            }
        }
    }

    /// Asks for the tab on screen. The processes repeat every POLL while Quarry is focused and the last answer is
    /// in; the variables are read once per tab, scope or Refresh, since they hardly change. The window calls it each
    /// frame while Quarry is focused; true while the tab refreshes by itself, so the UI wakes up for it.
    pub fn sql_server_poll(&mut self) -> bool {
        let Some(s) = self.sql_server.as_mut() else { return false };
        let Some(db) = self.conns.iter().find(|c| c.meta.id == s.conn).and_then(|c| c.sql.clone()).filter(|_| !self.demo) else { return false };
        let now = Instant::now();
        let mut repaint = false;
        match s.tab {
            SqlServerTab::Processes => {
                repaint = s.auto;
                let due = s.refresh || s.procs.is_none() || (s.auto && s.next.is_none_or(|t| now >= t));
                if s.procs_wait || !due {
                    return repaint;
                }
                self.req += 1;
                (s.procs_req, s.procs_wait, s.refresh) = (self.req, true, false);
                self.worker.sql_server_processes(db, self.req);
            }
            SqlServerTab::Locks => {
                repaint = s.auto;
                let due = s.refresh || s.locks.is_none() || (s.auto && s.next.is_none_or(|t| now >= t));
                if s.locks_wait || !due {
                    return repaint;
                }
                self.req += 1;
                (s.locks_req, s.locks_wait, s.refresh) = (self.req, true, false);
                self.worker.sql_server_locks(db, self.req);
            }
            SqlServerTab::Overview | SqlServerTab::Status => {
                let overview = s.tab == SqlServerTab::Overview;
                // MariaDB opens every table to size it, so sizes are read once per window and on Refresh, never on a timer.
                if overview && !s.sz_wait && (s.refresh || s.sizes_at.is_none()) {
                    self.req += 1;
                    (s.sz_req, s.sz_wait, s.sizes_at) = (self.req, true, Some(now));
                    self.worker.sql_server_sizes(db.clone(), self.req);
                }
                let (auto, empty, wait) = if overview { (true, s.ov.is_none(), s.ov_wait) } else { (s.st_auto, s.st.is_none(), s.st_wait) };
                repaint = auto;
                let due = s.refresh || empty || (auto && s.next.is_none_or(|t| now >= t));
                if wait || !due {
                    return repaint;
                }
                self.req += 1;
                s.refresh = false;
                if overview {
                    (s.ov_req, s.ov_wait) = (self.req, true);
                    self.worker.sql_server_overview(db, self.req);
                } else {
                    (s.st_req, s.st_wait) = (self.req, true);
                    self.worker.sql_server_status(db, self.req);
                }
            }
            SqlServerTab::Variables => {
                if s.vars_wait || !(s.refresh || s.vars.is_none()) {
                    return repaint;
                }
                self.req += 1;
                (s.vars_req, s.vars_wait, s.refresh) = (self.req, true, false);
                self.worker.sql_server_variables(db, self.req, s.global);
            }
        }
        repaint
    }

    pub fn sql_server_done(&mut self, req: u64, ans: Answer) {
        // A Kill answer is told even when the window is gone, so a failure is never silent.
        if let Some(told) = kill_toast(&mut self.sql_kills, req, &ans) {
            if let (Ok(_), Some(s)) = (&told, self.sql_server.as_mut()) {
                s.refresh = true;
                s.next = None;
            }
            match told {
                Ok(m) => self.ok(m),
                Err(e) => self.err(e),
            }
            return;
        }
        let Some(s) = self.sql_server.as_mut() else { return };
        match ans {
            Answer::Procs(r) if req == s.procs_req => {
                s.procs_wait = false;
                s.next = Some(Instant::now() + retry_after(r.is_ok()));
                match &r {
                    Ok(p) => {
                        s.uptime = p.uptime.or(s.uptime);
                        // The selection follows the thread by id. A pending Kill follows it only while it is the same
                        // statement on the same server: ids are reused, and a restart starts them over.
                        s.sel = s.sel.filter(|id| p.rows.iter().any(|x| x.id == *id));
                        s.confirm = s.confirm.take().filter(|c| crate::state::sql::server::confirm_ok(c, p));
                    }
                    Err(_) => s.confirm = None,
                }
                s.procs = Some(r);
            }
            Answer::Locks(r) if req == s.locks_req => {
                s.locks_wait = false;
                s.next = Some(Instant::now() + retry_after(r.is_ok()));
                // A pending Kill follows its thread only while it is the same statement on the same server.
                s.confirm = s.confirm.take().filter(|c| r.as_ref().is_ok_and(|l| confirm_still_ok(c, l)));
                s.chains = std::sync::Arc::new(r.as_ref().map(|l| db::sql::server::locks::chains(&l.waits)).unwrap_or_default());
                s.locks = Some(r.map(std::sync::Arc::new));
            }
            Answer::Vars(r) if req == s.vars_req => {
                s.vars_wait = false;
                if let Ok(v) = &r {
                    s.uptime = v.uptime.or(s.uptime);
                }
                s.vars = Some(r);
            }
            Answer::Sizes(r) if req == s.sz_req => {
                s.sz_wait = false;
                match r {
                    Ok(v) => (s.sizes, s.sizes_err) = (Some(v), None),
                    Err(e) => s.sizes_err = Some(e),
                }
            }
            Answer::Overview(r) if req == s.ov_req => {
                s.ov_wait = false;
                let now = Instant::now();
                s.next = Some(now + retry_after(r.is_ok()));
                s.ov_prev = prev_of(s.ov.take().map(|o| o.map(|(at, o)| (at, o.snap))), now);
                if let Ok(o) = &r {
                    s.uptime = o.snap.num("Uptime").map(|u| u as u64).or(s.uptime);
                }
                s.ov = Some(r.map(|o| (now, o)));
            }
            Answer::Status(r) if req == s.st_req => {
                s.st_wait = false;
                let now = Instant::now();
                s.next = Some(now + retry_after(r.is_ok()));
                s.st_prev = prev_of(s.st.take(), now);
                if let Ok(v) = &r {
                    s.uptime = v.num("Uptime").map(|u| u as u64).or(s.uptime);
                }
                s.st = Some(r.map(|v| (now, v)));
                s.set_lower();
            }
            _ => {}
        }
    }

    /// Asks for the tab on screen. Overview, Activity and Locks repeat every POLL while Quarry is focused; Settings is read
    /// once. Called each frame while Quarry is focused; true while the tab refreshes by itself, so the UI wakes up for it.
    pub fn pg_server_poll(&mut self) -> bool {
        let Some(s) = self.pg_server.as_mut() else { return false };
        let Some(db) = self.conns.iter().find(|c| c.meta.id == s.conn).and_then(|c| c.sql.clone()).filter(|_| !self.demo) else { return false };
        let now = Instant::now();
        let mut repaint = false;
        match s.tab {
            PgTab::Overview => {
                repaint = true;
                let sizes = !s.sz_wait && (s.refresh || s.sizes_at.is_none_or(|t| now.duration_since(t) >= SIZES_EVERY));
                if sizes {
                    self.req += 1;
                    (s.sz_req, s.sz_wait, s.sizes_at) = (self.req, true, Some(now));
                    self.worker.pg_server_sizes(db.clone(), self.req);
                }
                let due = s.refresh || s.ov.is_none() || s.ov_next.is_none_or(|t| now >= t);
                if s.ov_wait || !due {
                    return repaint;
                }
                self.req += 1;
                (s.ov_req, s.ov_wait, s.refresh) = (self.req, true, false);
                self.worker.pg_server_overview(db, self.req);
            }
            PgTab::Settings => {
                if s.set_wait || !(s.refresh || s.settings.is_none()) {
                    return repaint;
                }
                self.req += 1;
                (s.set_req, s.set_wait, s.refresh) = (self.req, true, false);
                self.worker.pg_server_settings(db, self.req);
            }
            PgTab::Counters => {
                if s.cnt_wait || !(s.refresh || s.counters.is_none()) {
                    return repaint;
                }
                self.req += 1;
                (s.cnt_req, s.cnt_wait, s.refresh) = (self.req, true, false);
                self.worker.pg_server_counters(db, self.req);
            }
            PgTab::Activity | PgTab::Locks => {
                repaint = s.auto;
                let due = s.refresh || s.act.is_none() || (s.auto && s.act_next.is_none_or(|t| now >= t));
                if s.act_wait || !due {
                    return repaint;
                }
                let blockers = s.refresh || s.blk_at.is_none_or(|t| now.duration_since(t) >= BLOCKERS_EVERY);
                if blockers {
                    s.blk_at = Some(now);
                }
                self.req += 1;
                (s.act_req, s.act_wait, s.refresh) = (self.req, true, false);
                self.worker.pg_server_activity(db, self.req, blockers);
            }
        }
        repaint
    }

    pub fn pg_server_done(&mut self, req: u64, ans: db::sql::pg::server::Answer) {
        // A Cancel or Terminate is told even when the window is gone, and only once.
        if let db::sql::pg::server::Answer::Signal(res) = &ans {
            if !self.sql_kills.remove(&req) {
                return;
            }
            if let (Ok(_), Some(s)) = (res, self.pg_server.as_mut()) {
                (s.refresh, s.act_next) = (true, None);
            }
            match res.clone() {
                Ok(m) => self.ok(m),
                Err(e) => self.err(e),
            }
            return;
        }
        let Some(s) = self.pg_server.as_mut() else { return };
        if let Some(e) = s.take(req, ans, Instant::now()) {
            self.err(e);
        }
    }
}

/// Whether the armed Kill still points at the same statement of the same connection on the same server.
pub fn confirm_still_ok(c: &Confirm, l: &db::sql::server::locks::Locks) -> bool {
    c.identity == l.identity && l.procs.get(&c.p.id).is_some_and(|p| c.p.same_thread(p))
}
