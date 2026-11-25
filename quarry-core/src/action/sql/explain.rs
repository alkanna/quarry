//! The requests of the Explain dialog of a SQL statement and the answers to them. The dialog's state is held in the app's
//! Modal, so these take it by reference: the app hands it over when a button is clicked or an answer arrives. Drawn by
//! src/app/sql/explain*.rs.

use crate::core::Core;
use crate::db;
use crate::db::sql::pg::hypo;
use crate::db::sql::plan::findings::Fix;
use crate::db::sql::plan::{self, compare, Analyze};
use crate::db::sql::Dialect;
use crate::state::sql::explain::{Pane, SqlExplain};
use crate::state::sql::explain_compare::Prev;
use crate::state::sql::explain_hypo::HypoRun;
use crate::state::sql::query::{run_db, run_target};
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

impl Core {
    /// The dialog for the plan of `text`, with its first request sent. `text` is the statement under the caret of a Query
    /// view or the last page's SELECT of a Content grid. None, with the reason shown, when there is nothing to explain.
    pub fn sql_explain_new(&mut self, text: String) -> Option<SqlExplain> {
        let t = self.t()?;
        let conn = t.sel.conn;
        let d = self.dialect_of(conn);
        if text.trim().is_empty() {
            self.warn("Nothing to explain.");
            return None;
        }
        if let Err(e) = plan::explainable(&text, d) {
            self.warn(e);
            return None;
        }
        let db = self.sql_db_for(conn, Some("Connect first."))?;
        let t = &self.tabs[self.cur];
        let (tab_db, database) = (t.sel.db.clone(), run_db(t));
        let (db, schema) = run_target(t, db);
        let writable = self.conn_writable(conn);
        let analyze = plan::analyze_needs(d, &db.version).map(str::to_string).map_or_else(|| plan::analyze_mode(&text, d, db.pooler()), Err).and_then(|a| match (d, writable) {
            // The read-only guard refuses ANALYZE on these engines, so it needs the unlock the user would otherwise be asked for.
            (Dialect::Postgres, false) => Err("ANALYZE on PostgreSQL runs the statement inside a transaction it rolls back, which needs writes unlocked on this connection.".into()),
            (Dialect::MariaDb, false) => Err("ANALYZE on MariaDB is a statement the read-only guard refuses. Unlock writes on this connection to use it.".into()),
            _ => Ok(a),
        });
        let mut x = SqlExplain::new(text, conn, tab_db, database, d, db, schema, analyze);
        self.sql_explain_send(&mut x, None);
        Some(x)
    }

    /// Plans the dialog's statement, or runs it with `analyze`.
    pub fn sql_explain_send(&mut self, x: &mut SqlExplain, analyze: Option<Analyze>) {
        self.req += 1;
        // A fresh handle per run, so a Stop for an old run never reaches this one.
        (x.req, x.busy, x.asked, x.run, x.stopped) = (self.req, true, analyze, Default::default(), false);
        let script = plan::explain_script(x.dialect, &x.stmt, analyze);
        self.worker.sql_explain(self.req, x.db.clone(), x.schema.clone(), script, !self.conn_writable(x.conn), x.run.clone());
    }

    /// Ends the run in flight on the server, which also rolls back an ANALYZE of a write and frees its locks.
    pub fn sql_explain_stop(&mut self, x: &mut SqlExplain) {
        if x.busy {
            x.stopped = true;
            self.worker.sql_kill(x.db.clone(), x.run.conn.load(std::sync::atomic::Ordering::SeqCst), x.run.cancel.clone());
        }
    }

    /// The answer of a request of the dialog `x`. False when the dialog has nothing left to show and closes.
    pub fn sql_explained(&mut self, x: &mut SqlExplain, req: u64, sets: Vec<db::sql::ResultSet>) -> bool {
        if x.req != req || !x.busy {
            return true;
        }
        x.busy = false;
        let text = match plan::plan_text(&sets) {
            Ok(t) => t,
            Err(e) => {
                // A failed first read leaves nothing to show; a failed ANALYZE keeps the plan it already had.
                let keep = x.plan.is_some() || x.unread.is_some();
                // After Stop the error is the cancel itself.
                if !x.stopped {
                    self.err(e);
                }
                return keep;
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
                        self.req += 1;
                        x.ev_req = Some(self.req);
                        self.worker.sql_explain_evidence(self.req, x.db.clone(), x.schema.clone(), wants);
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
                x.prev = self.explain_prev.swap(history_key(x, p.analyzed), p.clone(), now).map(|(plan, at)| Prev { diff: compare::diff(&plan, &p), plan, at }).filter(|v| v.diff.differs);
                x.leave_empty_compare();
                (x.plan, x.unread) = (Some(p), None);
            }
            // The raw text is still worth reading.
            Err(e) => (x.plan, x.unread, x.pane) = (None, Some(e), Pane::Raw),
        }
        x.plan_changed();
        true
    }

    /// The catalog answered: the findings are worked out again with it, and the selection and scroll stay where they are.
    pub fn sql_explain_evidence(&mut self, x: &mut SqlExplain, req: u64, ev: plan::Evidence) {
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
            self.explain_prev.update(&key, p.clone());
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

    /// Asks the server for the plan with the candidate `fix` as a hypothetical index. Only a click gets here.
    pub fn sql_explain_hypo_try(&mut self, x: &mut SqlExplain, fix: Fix) {
        if x.dialect != Dialect::Postgres || hypo_blocked(x, &fix).is_some() {
            return;
        }
        let Some(create) = hypo::create_sql(&fix).map(str::to_string) else { return };
        self.req += 1;
        (x.hypo_wait, x.hypo, x.hypo_cmp) = (Some((self.req, fix.sql)), None, false);
        x.leave_empty_compare();
        self.worker.sql_explain_hypo(self.req, x.db.clone(), x.schema.clone(), create, x.stmt.clone(), x.locked);
    }

    /// The answer of a trial of the dialog `x`.
    pub fn sql_explain_hypo(&mut self, x: &mut SqlExplain, req: u64, res: db::Res<hypo::Trial>) {
        let Some(fix) = x.hypo_wait.take_if(|(r, _)| *r == req).map(|(_, f)| f) else { return };
        let t = match res {
            Ok(t) => t,
            Err(e) => return self.err(e),
        };
        let mut p = match plan::parse(Dialect::Postgres, &t.plan) {
            Ok(p) => p,
            Err(e) => return self.err(e),
        };
        // The same evidence as the dialog's plan, so Compare sets like findings against like.
        p.refind(x.dialect, x.ev.as_ref(), x.db.pooler());
        let Some(before) = &x.plan else { return };
        let diff = compare::diff(before, &p);
        x.hypo = Some(HypoRun { fix, used: hypo::uses(&p, &t.index), plan: p, diff });
    }
}
