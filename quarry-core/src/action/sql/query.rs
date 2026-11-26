//! The Query view of a SQL tab: Run, Stop, the result tabs and their pages, and the live check of the editor. Drawn by
//! src/app/sql/query.rs.

use crate::action::{Dialog, UiFx};
use crate::core::Core;
use crate::db;
use crate::db::sql::ResultSet;
use crate::state::library::snippet_strip::{fill_tab, Src};
use crate::state::config::push_history;
use crate::state::sql::query::{ran_target, run_db, run_target};
use std::sync::atomic::Ordering;
use std::sync::Arc;

impl Core {
    /// `text`, run from the open Query tab, with its snippet values filled in.
    pub fn snippet_fill_run(&self, text: String) -> Result<String, String> {
        let Some(t) = self.t() else { return Ok(text) };
        fill_tab(t, &text, Src::Query, self.sql_dialect().into(), self.sql_bs_of(t.sel.conn))
    }

    /// Runs the statement under the caret, or the whole script. `explain` puts EXPLAIN in front of the statement.
    pub fn sql_run(&mut self, all: bool, explain: bool) {
        let dialect = self.sql_dialect();
        let Some(s) = self.t().and_then(|t| t.sql.as_ref()) else { return };
        let text = if all { s.query.clone() } else { s.caret_info(dialect).stmt.map(|(r, _, _)| s.query[r].to_string()).unwrap_or_default() };
        self.sql_run_text(text, explain);
    }

    /// Runs `text` in the current Query tab, as picked from the Run button's tip or found by `sql_run`.
    pub fn sql_run_text(&mut self, text: String, explain: bool) {
        let Some(t) = self.t() else { return };
        let (conn, schema) = (t.sel.conn, run_db(t));
        if t.sql.as_ref().is_none_or(|s| s.run_req != 0) {
            return;
        }
        if text.trim().is_empty() {
            return self.warn("Nothing to run.");
        }
        // A snippet's values go in here, so Run, Run all, a picked statement and Explain all send the filled text.
        let text = match self.snippet_fill_run(text) {
            Ok(t) => t,
            Err(e) => return self.warn(e),
        };
        // The plan has a dialog of its own, which the app holds; the classic rows are one click away there.
        if explain {
            return self.ui_fx.push(UiFx::Open(Dialog::SqlExplain(text)));
        }
        let Some(db) = self.sql_db_for(conn, Some("Connect first.")) else { return };
        let (db, ns) = run_target(&self.tabs[self.cur], db);
        let read_only = !self.conn_writable(conn);
        let key = format!("{conn}/{schema}/#query");
        // History is written to ui.json and listed in the menu, so a password never goes in.
        push_history(&mut self.prefs.history.entry(key).or_default().selector, &db::redact_sql(&text).0);
        self.save_prefs();
        self.req += 1;
        let req = self.req;
        let Some(s) = self.tabs[self.cur].sql.as_mut() else { return };
        // Fresh handles per run, so a Stop pressed for an old run never reaches this one.
        (s.run_req, s.run, s.ran_in) = (req, Default::default(), Some((s.run_db.clone(), s.run_schema.clone())));
        self.worker.sql_run(req, db, ns, text, read_only, 0, s.run.clone());
    }

    /// Stops the running script: the statement in flight is interrupted and the rest never starts.
    pub fn sql_stop(&mut self) {
        let Some(t) = self.t() else { return };
        let Some(s) = t.sql.as_ref().filter(|s| s.run_req != 0) else { return };
        if let Some(db) = self.conn(t.sel.conn).and_then(|c| c.sql.clone()) {
            self.worker.sql_kill(db, s.run.conn.load(Ordering::SeqCst), s.run.cancel.clone());
        }
    }

    /// Applies a finished run to its tab; a run the tab no longer waits for is dropped. A page read replaces
    /// only its result tab and keeps the row count; a new run starts the counts of its paged results.
    pub fn sql_ran(&mut self, req: u64, mut sets: Vec<ResultSet>) {
        let Some(ti) = self.tabs.iter().position(|t| t.sql.as_ref().is_some_and(|s| s.run_req == req)) else { return };
        let conn = self.tabs[ti].sel.conn;
        let Some(s) = self.tabs[ti].sql.as_mut() else { return };
        // Checks belong to the page they were made on.
        (s.run_req, s.text_cache, s.qanchor, s.qsel) = (0, None, None, None);
        s.qchecked.clear();
        if let Some(idx) = s.paging.take() {
            let failed = match (s.sets.get_mut(idx), sets.pop()) {
                (Some(old), Some(mut new)) if new.error.is_none() => {
                    if let (Some(o), Some(n)) = (old.page, new.page.as_mut()) {
                        n.total = o.total;
                    }
                    *old = Arc::new(new);
                    None
                }
                (_, Some(new)) => new.error,
                _ => None,
            };
            if let Some(e) = failed {
                self.err(e);
            }
            return;
        }
        s.sets_req = req;
        (s.set_sel, s.sets) = (0, sets.into_iter().map(Arc::new).collect());
        // A last page already knows the total; the others ask the server.
        let wants: Vec<(usize, String)> = s.sets.iter().enumerate().filter(|(_, x)| x.page.is_some_and(|p| p.next)).map(|(i, x)| (i, x.stmt.clone())).collect();
        if let Some((db, ns)) = self.conn(conn).and_then(|c| c.sql.clone()).map(|db| ran_target(&self.tabs[ti], db)) {
            for (i, stmt) in wants {
                self.worker.sql_count(req, i, db.clone(), ns.clone(), stmt);
            }
        }
        self.lint_schema_changed(conn);
    }

    /// A run, or a schema read again after a change elsewhere, may have made what an unknown name pointed at, so the
    /// warnings of every tab of the connection are asked again; syntax stays as it is.
    pub fn lint_schema_changed(&mut self, conn: u64) {
        for s in self.tabs.iter_mut().filter(|t| t.sel.conn == conn).filter_map(|t| t.sql.as_mut()) {
            s.lint.drop_warnings();
        }
    }

    /// The row count of a paged result arrived.
    pub fn sql_counted(&mut self, req: u64, idx: usize, res: db::Res<u64>) {
        let p = self.tabs.iter_mut().find_map(|t| t.sql.as_mut().filter(|s| s.sets_req == req)).and_then(|s| s.sets.get_mut(idx)).and_then(|x| Arc::make_mut(x).page.as_mut());
        if let (Some(p), Ok(n)) = (p, res) {
            p.total = Some(n);
        }
    }

    /// Sorts the shown result by `keys` (column and whether descending) on the client, which only orders the rows
    /// already fetched. No keys restores the server's order. Checks and the selected row follow their rows.
    pub fn sql_sort_page(&mut self, keys: Vec<(String, bool)>) {
        let Some(s) = self.tabs.get_mut(self.cur).and_then(|t| t.sql.as_mut()) else { return };
        let i = s.set_sel.min(s.sets.len().saturating_sub(1));
        let Some(set) = s.sets.get_mut(i) else { return };
        let moved = db::sql::sort::sort_rows(Arc::make_mut(set), &keys);
        s.qchecked = s.qchecked.iter().filter_map(|&r| moved.get(r).copied()).collect();
        s.qanchor = s.qanchor.and_then(|r| moved.get(r).copied());
        s.qsel = s.qsel.and_then(|r| moved.get(r).copied());
        s.text_cache = None;
    }

    /// Reads result tab `idx` again at page `n` on a fresh connection, a run of its own that Stop reaches.
    pub fn sql_goto_page(&mut self, idx: usize, n: usize) {
        let Some(t) = self.t() else { return };
        let conn = t.sel.conn;
        let Some(stmt) = t.sql.as_ref().filter(|s| s.run_req == 0).and_then(|s| s.sets.get(idx)).map(|x| x.stmt.clone()) else { return };
        let Some(db) = self.sql_db_for(conn, Some("Connect first.")) else { return };
        let (db, schema) = ran_target(&self.tabs[self.cur], db);
        let read_only = !self.conn_writable(conn);
        self.req += 1;
        let req = self.req;
        let Some(s) = self.tabs[self.cur].sql.as_mut() else { return };
        (s.run_req, s.paging, s.run) = (req, Some(idx), Default::default());
        self.worker.sql_run(req, db, schema, stmt, read_only, n, s.run.clone());
    }

    /// The check of request `req` came back. Answers for statements no longer in the script are dropped with it, so the
    /// map stays as small as the script.
    pub fn sql_linted(&mut self, req: u64, lint: Result<Option<db::sql::Lint>, ()>) {
        let Some(s) = self.tabs.iter_mut().filter_map(|t| t.sql.as_mut()).find(|s| s.lint.req == req) else { return };
        s.lint.req = 0;
        let stmt = std::mem::take(&mut s.lint.asked);
        s.lint.put(stmt, lint);
        s.lint.seen.retain(|k, _| s.query.contains(k.as_str()));
    }
}
