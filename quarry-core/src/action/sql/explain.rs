//! The requests of the Explain dialog of a SQL statement and the answers to them. The dialog's state is held in the app's
//! Modal, so these take it by reference: the app hands it over when a button is clicked or an answer arrives. Drawn by
//! src/app/sql/explain*.rs.

use crate::action::Reply;
use crate::core::Core;
use crate::db;
use crate::db::sql::pg::hypo;
use crate::db::sql::plan::findings::Fix;
use crate::db::sql::plan::{self, compare, Analyze};
use crate::db::sql::Dialect;
use crate::state::sql::explain::{Pane, SqlExplain};
use crate::state::sql::explain_compare::Prev;
use crate::state::sql::explain_hypo::HypoRun;
use std::sync::Arc;

/// Where this session keeps the last plan of the dialog's statement: two databases of one connection are two places.
pub fn history_key(x: &SqlExplain, analyzed: bool) -> compare::Key {
    compare::Key { conn: x.conn, db: x.database.clone(), schema: x.schema.clone(), stmt: x.key_stmt.clone(), analyzed }
}

/// Why the HypoPG trial of `fix` cannot run, shown beside the disabled button. None when it can.
pub fn hypo_blocked(x: &SqlExplain, fix: &Fix) -> Option<String> {
    if x.demo {
        return Some("The offline demo has no server to try the index on.".into());
    }
    if x.db.pooler() {
        return Some(hypo::POOLER.into());
    }
    if let Some(why) = extension_gate(x.ev.as_ref(), x.ev_req.is_some()) {
        return Some(why.into());
    }
    if hypo::create_sql(fix).is_none() {
        return Some("This candidate is not a CREATE INDEX HypoPG can take.".into());
    }
    // Checked once per lock state as the dialog draws; the worker checks the statement again before the trial runs.
    match &x.hypo_refused {
        Some((locked, why)) if *locked == x.locked => why.clone(),
        _ => hypo::refused(&x.stmt, x.locked),
    }
}

/// Why the catalog's answer does not show HypoPG as installed. A failed read says nothing either way, unless it still saw
/// the extension.
pub fn extension_gate(ev: Option<&plan::Evidence>, busy: bool) -> Option<&'static str> {
    match ev {
        Some(ev) if ev.hypopg => None,
        _ if busy => Some("Checking whether HypoPG is installed."),
        Some(ev) if !ev.failed => Some("Needs the HypoPG extension in this database, usable by this login."),
        _ => Some("The catalog could not be read to check for HypoPG."),
    }
}

/// What the app does once the dialog has taken an answer: raise `err`, ask the catalog for `wants`, and close the dialog when
/// `keep` is false.
pub struct Answered {
    pub keep: bool,
    pub err: Option<String>,
    pub wants: Vec<plan::evidence::Want>,
}

impl SqlExplain {
    /// A request of the dialog went out as `req`, to plan the statement or to run it with `analyze`.
    pub fn asked(&mut self, req: u64, analyze: Option<Analyze>) {
        (self.req, self.busy, self.asked, self.stopped) = (req, true, analyze, false);
    }

    /// The answer of a request of the dialog. Not `keep` when the dialog has nothing left to show and closes.
    pub fn explained(&mut self, prev: &mut compare::History, req: u64, sets: Vec<db::sql::ResultSet>) -> Answered {
        let mut out = Answered { keep: true, err: None, wants: Vec::new() };
        let x = self;
        if x.req != req || !x.busy {
            return out;
        }
        x.busy = false;
        let text = match plan::plan_text(&sets) {
            Ok(t) => t,
            Err(e) => {
                // A failed first read leaves nothing to show; a failed ANALYZE keeps the plan it already had.
                out.keep = x.plan.is_some() || x.unread.is_some();
                // After Stop the error is the cancel itself.
                if !x.stopped {
                    out.err = Some(e);
                }
                return out;
            }
        };
        x.raw = plan::raw_text(&text);
        x.analyzed = x.asked.is_some();
        x.rolled_back = x.asked == Some(Analyze::Rollback) && !crate::db::sql::lex::pageable(&x.stmt, x.dialect);
        match plan::parse(x.dialect, &text) {
            Ok(mut p) => {
                let ready = if x.demo {
                    x.ev = Some(plan::Evidence::demo(&p, x.dialect));
                    true
                } else {
                    let wants = plan::evidence::wants(&p, &x.stmt, x.dialect);
                    // A rerun of the same statement scans the same tables, so the evidence already read still holds.
                    let held = !wants.is_empty() && x.ev.as_ref().is_some_and(|e| e.covers(&wants));
                    if !wants.is_empty() && !held {
                        out.wants = wants;
                    }
                    held
                };
                if ready {
                    p.refind(x.dialect, x.ev.as_ref(), x.db.pooler());
                }
                // The panel opens on what matters most: the node of the top finding, else the one that costs the most.
                (x.hypo, x.hypo_wait, x.hypo_cmp) = (None, None, false);
                x.sel = p.findings.iter().find_map(|f| f.node).or_else(|| (0..p.nodes.len()).max_by(|&a, &b| p.nodes[a].share.unwrap_or(0.0).total_cmp(&p.nodes[b].share.unwrap_or(0.0))));
                x.scroll_to = x.sel;
                // The last plan of the same statement, kept for the session, is what Compare shows when this one differs.
                let now = crate::library::now_secs();
                let p = Arc::new(p);
                x.prev = prev.swap(history_key(x, p.analyzed), p.clone(), now).map(|(plan, at)| Prev { diff: compare::diff(&plan, &p), plan, at }).filter(|v| v.diff.differs);
                x.leave_empty_compare();
                (x.plan, x.unread) = (Some(p), None);
            }
            // The raw text is still worth reading.
            Err(e) => (x.plan, x.unread, x.pane) = (None, Some(e), Pane::Raw),
        }
        x.plan_changed();
        out
    }

    /// The catalog answered: the findings are worked out again with it, and the selection and scroll stay where they are.
    pub fn evidence_in(&mut self, prev: &mut compare::History, req: u64, ev: plan::Evidence) {
        let x = self;
        if x.ev_req != Some(req) {
            return;
        }
        x.ev_req = None;
        let pooler = x.db.pooler();
        let key = history_key(x, x.plan.as_ref().is_some_and(|p| p.analyzed));
        if let Some(p) = &mut x.plan {
            Arc::make_mut(p).refind(x.dialect, Some(&ev), pooler);
            // Each plan is compared with the findings it showed: the plan kept for the next Compare and this Compare's
            // new side follow the strip. `differs` reads no findings, so the Compare tab stays as it was.
            prev.update(&key, p.clone());
            if let Some(prev) = &mut x.prev {
                prev.diff.refind(&prev.plan, p);
            }
            // A trial that ran while the catalog was read again is found with the same evidence as the plan it is set against.
            if let Some(h) = &mut x.hypo {
                h.plan.refind(x.dialect, Some(&ev), pooler);
                h.diff.refind(p, &h.plan);
            }
        }
        x.ev = Some(ev);
        x.plan_changed();
    }

    /// The CREATE INDEX of candidate `fix` when the trial may go out. Only a click gets here.
    pub fn hypo_create(&self, fix: &Fix) -> Option<String> {
        if self.dialect != Dialect::Postgres || hypo_blocked(self, fix).is_some() {
            return None;
        }
        hypo::create_sql(fix).map(str::to_string)
    }

    /// The trial of `fix` went out as request `req`.
    pub fn hypo_asked(&mut self, req: u64, fix: Fix) {
        (self.hypo_wait, self.hypo, self.hypo_cmp) = (Some((req, fix.sql)), None, false);
        self.leave_empty_compare();
    }

    /// The answer of a trial of the dialog; the failure to show, if any.
    pub fn hypo_in(&mut self, req: u64, res: db::Res<hypo::Trial>) -> Option<String> {
        let x = self;
        let fix = x.hypo_wait.take_if(|(r, _)| *r == req).map(|(_, f)| f)?;
        let t = match res {
            Ok(t) => t,
            Err(e) => return Some(e),
        };
        let mut p = match plan::parse(Dialect::Postgres, &t.plan) {
            Ok(p) => p,
            Err(e) => return Some(e),
        };
        // The same evidence as the dialog's plan, so Compare sets like findings against like.
        p.refind(x.dialect, x.ev.as_ref(), x.db.pooler());
        let before = x.plan.as_ref()?;
        let diff = compare::diff(before, &p);
        x.hypo = Some(HypoRun { fix, used: hypo::uses(&p, &t.index), plan: p, diff });
        None
    }
}

impl Core {
    /// Plans the statement of an Explain dialog, or runs it with `analyze`. Pending: the answer is `Msg::SqlExplained` on this request.
    pub(in crate::action) fn sql_explain_send(&mut self, conn: u64, database: String, schema: String, stmt: String, analyze: Option<Analyze>) -> Reply {
        let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) else {
            self.warn_offline();
            return Reply::Done;
        };
        let d = self.dialect_of(conn);
        // A PostgreSQL connection belongs to one database, so the database picks the pool.
        let db = if d == Dialect::Postgres { sql.at(&database) } else { sql };
        self.req += 1;
        // A fresh handle per run, so a Stop for an old run never reaches this one.
        let run = db::sql::RunHandle::default();
        let script = plan::explain_script(d, &stmt, analyze);
        self.worker.sql_explain(self.req, db.clone(), schema, script, !self.conn_writable(conn), run.clone());
        self.explain_run = Some((db, run));
        Reply::Pending(self.req)
    }

    /// Ends the run in flight on the server, which also rolls back an ANALYZE of a write and frees its locks.
    pub(in crate::action) fn sql_explain_stop(&mut self) {
        if let Some((db, run)) = &self.explain_run {
            self.worker.sql_kill(db.clone(), run.conn.load(std::sync::atomic::Ordering::SeqCst), run.cancel.clone());
        }
    }

    /// The catalog read for the tables an Explain dialog's plan scans. Pending.
    pub(in crate::action) fn sql_explain_evidence_ask(&mut self, conn: u64, database: String, schema: String, wants: Vec<plan::evidence::Want>) -> Reply {
        let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) else { return Reply::Done };
        let db = if self.dialect_of(conn) == Dialect::Postgres { sql.at(&database) } else { sql };
        self.req += 1;
        self.worker.sql_explain_evidence(self.req, db, schema, wants);
        Reply::Pending(self.req)
    }

    /// A HypoPG trial of `create` against `stmt`. Pending.
    pub(in crate::action) fn sql_explain_hypo_ask(&mut self, conn: u64, database: String, schema: String, create: String, stmt: String, locked: bool) -> Reply {
        let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) else { return Reply::Done };
        let db = if self.dialect_of(conn) == Dialect::Postgres { sql.at(&database) } else { sql };
        self.req += 1;
        self.worker.sql_explain_hypo(self.req, db, schema, create, stmt, locked);
        Reply::Pending(self.req)
    }
}
