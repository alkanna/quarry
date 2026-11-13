//! Opening a library script into a tab, and writing a tab's text back to the script it came from. The signatures of
//! `open_script` and `save_script` are fixed so the panel (B3) and the linked folders (B2) can call them.
#![allow(dead_code)]

use super::*;
use crate::library::ScriptRef;

/// How a script leaves the library: Return opens it, Alt+Return opens it in a new tab, Cmd+Enter runs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum How {
    Here,
    NewTab,
    Run,
}

/// What the view on screen holds, as a script of the library would hold it.
pub(in crate::app) struct TabText {
    pub kind: &'static str,
    pub body: String,
    pub sort: String,
    pub project: String,
    pub view: View,
    pub db: String,
    pub schema: String,
    pub coll: String,
}

/// The view whose text is a script of `kind`.
pub(in crate::app) fn view_of_kind(kind: &str) -> Option<View> {
    Some(match kind {
        "sql" => View::Sql,
        "shell" => View::Shell,
        "filter" => View::Documents,
        "pipeline" => View::Aggregate,
        _ => return None,
    })
}

pub(in crate::app) fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl App {
    /// The text of the current tab's view as a script, or None for a view that holds no script (Indexes, Schema, a table's
    /// Content grid and the like).
    pub(in crate::app) fn tab_text(&self) -> Option<TabText> {
        Self::text_of(self.t()?)
    }

    fn text_of(t: &Tab) -> Option<TabText> {
        let (db, coll) = (t.sel.db.clone(), t.sel.coll.clone());
        Some(match (t.view, t.sql.as_ref()) {
            (View::Sql, Some(s)) => {
                let run_db = if s.run_db.is_empty() { db } else { s.run_db.clone() };
                TabText { kind: "sql", body: s.query.clone(), sort: String::new(), project: String::new(), view: View::Sql, db: run_db, schema: s.run_schema.trim().to_string(), coll: String::new() }
            }
            (View::Shell, _) => TabText { kind: "shell", body: t.shell.code.clone(), sort: String::new(), project: String::new(), view: View::Shell, db, schema: String::new(), coll: String::new() },
            (View::Documents, None) => TabText { kind: "filter", body: t.filter_txt.clone(), sort: t.sort_txt.clone(), project: t.proj_txt.clone(), view: View::Documents, db, schema: t.sel.schema.clone(), coll },
            (View::Aggregate, None) => TabText { kind: "pipeline", body: t.agg_txt.clone(), sort: String::new(), project: String::new(), view: View::Aggregate, db, schema: t.sel.schema.clone(), coll },
            _ => return None,
        })
    }

    /// The binding a restored tab gets back: the script is still in the library and the tab shows the view it is written in.
    /// Its base is the saved text, so edits made before the restart still show as edits. A linked file is bound with the
    /// text the tab holds; the linked folders' code adjusts its disk state when it rescans.
    pub(in crate::app) fn rebind(&self, t: &Tab, r: ScriptRef) -> Option<Bound> {
        let (view, base) = match &r {
            ScriptRef::Saved(id) => {
                let s = self.library.scripts.iter().find(|s| s.id == *id)?;
                (view_of_kind(&s.kind)?, s.body.clone())
            }
            ScriptRef::File { .. } => {
                let tt = Self::text_of(t)?;
                (tt.view, tt.body)
            }
        };
        (t.view == view).then_some(Bound { r, view, base, disk: None, conflict: None })
    }

    /// The database a SQL or Shell script opens in: its own when it names one, else the current tab's on the same
    /// connection, else the first the connection lists.
    fn script_db(&self, conn: u64, wanted: &str) -> String {
        if !wanted.is_empty() {
            return wanted.to_string();
        }
        if let Some(t) = self.t().filter(|t| t.sel.conn == conn && !t.sel.db.is_empty()) {
            return t.sel.db.clone();
        }
        self.conn(conn).and_then(|c| c.dbs.first().map(|d| d.0.clone())).unwrap_or_default()
    }

    /// Whether tab `i` is one that opening a script can replace: it holds no text of the person's beyond the script it
    /// was opened from, and that is unchanged.
    fn tab_clean(&self, i: usize, kind: &str) -> bool {
        let Some(t) = self.tabs.get(i) else { return false };
        let text = match (kind, t.sql.as_ref()) {
            ("sql", Some(s)) if t.view == View::Sql && t.sel.coll.is_empty() => s.query.as_str(),
            _ => return false,
        };
        let staged = t.sql.as_ref().is_some_and(|s| !s.st.staged.is_empty());
        let unchanged = match &t.script {
            Some(b) => b.view == View::Sql && b.base == text,
            None => text.trim().is_empty(),
        };
        unchanged && !staged
    }

    /// Scripts that left the library (deleted, or moved into a linked folder): tabs bound to them keep their text but are
    /// unbound, so their next Cmd+S opens the Save dialog, and they leave the recent list and a pending open.
    pub(in crate::app) fn forget_scripts(&mut self, ids: &[u64]) {
        let gone = |r: &ScriptRef| matches!(r, ScriptRef::Saved(id) if ids.contains(id));
        for t in &mut self.tabs {
            if t.script.as_ref().is_some_and(|b| gone(&b.r)) {
                t.script = None;
            }
        }
        self.library.recent.retain(|r| !gone(r));
        if self.lib_ui.pending.as_ref().is_some_and(|(r, _)| gone(r)) {
            self.lib_ui.pending = None;
        }
    }

    /// Ties the current tab to a script, so Cmd+S writes it back, and puts it first in the recent list.
    pub(in crate::app) fn bind_script(&mut self, r: ScriptRef, view: View, base: String) {
        if let Some(t) = self.tm() {
            t.script = Some(Bound { r: r.clone(), view, base, disk: None, conflict: None });
        }
        self.library.push_recent(r);
        self.save_library();
    }

    /// Opens the script `r` in a tab, the way `how` says. A plain open takes the current tab only when it is a clean one
    /// of the same kind and connection, like a click on a table, and otherwise opens a tab beside it. A script whose
    /// connection is not ready waits in `lib_ui.pending` and opens when it is. Run goes through the gated run paths.
    pub(in crate::app) fn open_script(&mut self, r: &ScriptRef, how: How) {
        match r {
            ScriptRef::Saved(id) => match self.library.scripts.iter().find(|s| s.id == *id).cloned() {
                Some(s) => self.open_resolved(r, how, s, None),
                None => self.warn("That script is no longer in the library."),
            },
            // A linked file is read on the worker, as a script of the connection it is opened on, and opens when it is back.
            ScriptRef::File { link, rel } => self.read_file_job(*link, rel, super::linked::Job::Open { r: r.clone(), how }),
        }
    }

    /// `open_script` once the script is known. `file_meta` is the time and length of a linked file as it was read, which go
    /// on the tab's binding, so a change on disk is told from the text just read.
    pub(in crate::app) fn open_resolved(&mut self, r: &ScriptRef, how: How, s: crate::library::Script, file_meta: Option<Option<(std::time::SystemTime, u64)>>) {
        let file = match (r, file_meta) {
            (ScriptRef::File { link, rel }, Some(meta)) => Some((*link, rel.clone(), meta)),
            _ => None,
        };
        let Some(status_ready) = self.conn(s.conn).map(|c| c.status == Status::Ready) else {
            return self.warn("The connection of this script was removed. The script stays in the library, but it needs a connection to open.");
        };
        if !status_ready && !self.demo {
            self.lib_ui.pending = Some((r.clone(), how));
            // activate connects only an idle connection; a failed one is tried again.
            if self.conn(s.conn).is_some_and(|c| matches!(c.status, Status::Failed(_))) {
                self.connect(s.conn);
            }
            self.activate(s.conn);
            return;
        }
        self.lib_ui.pending = None;
        let new_tab = how == How::NewTab;
        match s.kind.as_str() {
            "sql" => {
                let db = self.script_db(s.conn, &s.db);
                let reuse = !new_tab && self.tab_clean(self.cur, "sql") && self.t().is_some_and(|t| t.sel.conn == s.conn && t.sel.db == db);
                if !reuse {
                    self.sql_new_query(s.conn, db);
                }
                if let Some(q) = self.tm().and_then(|t| t.sql.as_mut()) {
                    q.query = s.body.clone();
                    q.caret = 0;
                    q.run_schema = s.schema.clone();
                }
                self.bind_script(r.clone(), View::Sql, s.body);
                if how == How::Run {
                    self.sql_run(true, false);
                }
            }
            "shell" => {
                let db = self.script_db(s.conn, &s.db);
                let sql = self.conn(s.conn).is_some_and(|c| c.meta.engine.is_sql());
                if sql {
                    self.sql_new_query(s.conn, db);
                } else {
                    // The Shell needs no collection, so its tab has none and is named for the database.
                    let sel = Sel { conn: s.conn, db, schema: String::new(), coll: String::new() };
                    self.open_tab(sel, Open::NewTab);
                }
                self.open_in_shell(s.body.clone());
                self.bind_script(r.clone(), View::Shell, s.body);
                if how == How::Run {
                    self.run_shell_script();
                }
            }
            "filter" => {
                let sel = Sel { conn: s.conn, db: s.db.clone(), schema: s.schema.clone(), coll: s.coll.clone() };
                self.open_tab(sel, if new_tab { Open::NewTab } else { Open::Link });
                if let Some(t) = self.tm() {
                    t.clear_options();
                    (t.filter_txt, t.sort_txt, t.proj_txt) = (s.body.clone(), s.sort.clone(), s.project.clone());
                    t.view = View::Documents;
                }
                self.bind_script(r.clone(), View::Documents, s.body);
                self.run_query();
            }
            "pipeline" => {
                let sel = Sel { conn: s.conn, db: s.db.clone(), schema: s.schema.clone(), coll: s.coll.clone() };
                self.open_tab(sel, if new_tab { Open::NewTab } else { Open::Link });
                if let Some(t) = self.tm() {
                    t.agg_txt = s.body.clone();
                    t.view = View::Aggregate;
                }
                self.bind_script(r.clone(), View::Aggregate, s.body.clone());
                self.run_script_pipeline(&s.body);
            }
            other => self.warn(format!("This build does not know scripts of kind \"{other}\".")),
        }
        if let Some((link, rel, meta)) = file {
            if let Some(b) = self.tm().and_then(|t| t.script.as_mut()) {
                b.disk = meta;
            }
            self.mark_seen(link, &rel, meta);
        }
    }

    /// Runs a pipeline opened from the library through the Aggregate view's own gate, which asks before a write stage.
    fn run_script_pipeline(&mut self, text: &str) {
        if db::is_shell(text) {
            return self.run_shell(text);
        }
        match db::parse_pipeline(text) {
            Ok(p) => self.run_pipeline(p),
            Err(e) => self.err(e),
        }
    }

    /// Cmd+S and the Save button: writes the view's text back to the script the tab is bound to, or opens the Save to
    /// library dialog when there is none. Page handlers consume Cmd+S first (staged grid edits, document drafts), so this
    /// only runs when the person means the script.
    pub(in crate::app) fn save_script(&mut self) {
        let Some(tt) = self.tab_text() else { return self.warn("There is no script to save in this view. Open a Query, Documents, Aggregate or Shell view.") };
        if tt.body.trim().is_empty() && tt.sort.trim().is_empty() && tt.project.trim().is_empty() {
            return self.warn("Nothing to save: the script is empty.");
        }
        // A tab bound to a linked file writes that file, or raises the conflict when it changed on disk.
        if self.save_linked() {
            return;
        }
        let bound = self.t().and_then(|t| t.script.as_ref()).filter(|b| b.view == tt.view).map(|b| b.r.clone());
        // A bound linked file is written by the linked folders' code; until then it takes the dialog.
        if let Some(ScriptRef::Saved(id)) = bound
            && let Some(i) = self.library.scripts.iter().position(|s| s.id == id)
        {
            let sc = &mut self.library.scripts[i];
            (sc.body, sc.sort, sc.project, sc.updated) = (tt.body.clone(), tt.sort.clone(), tt.project.clone(), now_secs());
            let name = sc.name.clone();
            self.bind_script(ScriptRef::Saved(id), tt.view, tt.body.clone());
            self.ok(format!("Saved \"{name}\""));
            self.warn_plain_password(&tt);
            return;
        }
        self.open_save_dialog(tt);
    }

    /// A SQL script with a password in it is saved as typed, since one with the password cut out would not run, and the
    /// person is told where it now sits.
    pub(in crate::app) fn warn_plain_password(&mut self, tt: &TabText) {
        if tt.kind == "sql" && !db::redact_sql(&tt.body).1.is_empty() {
            self.warn("Saved, but the script holds a password in plain text in the library file.");
        }
    }
}
