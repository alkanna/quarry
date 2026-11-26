//! Toasts, the session's error list and the undo list: what an action tells the person and what it can take back.
//! The egui app draws them; wording comes from `db::explain`.

use super::Core;
use crate::db::{self, Fix, Target, UndoAction};
use crate::state::errors::{Act, ErrEntry, Failed, Toast, Tone};
use crate::state::*;
use std::time::{Duration, Instant};

/// Errors kept for the status bar list; older ones drop off.
pub const ERRORS_MAX: usize = 200;

/// Undo entries kept for toasts that may still be up; older ones drop off.
pub const RESTORES_MAX: usize = 10;

pub fn fix_label(f: Fix, timeout_ms: u64) -> String {
    match f {
        Fix::Retry => "Retry".into(),
        Fix::EditConn => "Edit connection".into(),
        Fix::Explain => "Explain".into(),
        Fix::RaiseLimit => format!("Raise limit to {} s", raised(timeout_ms) / 1000),
        Fix::Refresh => "Refresh sidebar".into(),
        Fix::Validator => "Open Validator".into(),
        Fix::Structure => "Open Structure".into(),
    }
}

/// The next time limit offered after a timeout.
pub fn raised(ms: u64) -> u64 {
    (ms * 4).max(120_000)
}

/// "Documents · orders", where an error happened.
pub fn place_of(v: View, sel: &Sel) -> String {
    format!("{} · {}", view_name(v), if sel.coll.is_empty() { sel.db.clone() } else { sel.shown() })
}

fn view_name(v: View) -> &'static str {
    match v {
        View::Documents => "Documents",
        View::Aggregate => "Aggregate",
        View::Indexes => "Indexes",
        View::Shell => "Shell",
        View::Schema => "Schema",
        View::Import => "Import",
        View::Compare => "Compare",
        View::Watch => "Watch",
        View::Structure => "Structure",
        View::Relations => "Relations",
        View::Triggers => "Triggers",
        View::Info => "Info",
        View::Sql => "Query",
    }
}

/// Drops actions older than a day and keeps the newest `UNDO_MAX`, like the original.
pub fn trim_undo(undo: &mut Vec<UndoAction>, now: Instant) {
    undo.retain(|a| now.saturating_duration_since(a.at).as_secs() <= db::UNDO_MAX_AGE_S);
    undo.truncate(db::UNDO_MAX);
}

impl Core {
    pub fn conn(&self, id: u64) -> Option<&Conn> {
        self.conns.iter().find(|c| c.meta.id == id)
    }

    pub fn t(&self) -> Option<&Tab> {
        self.tabs.get(self.cur)
    }

    pub fn conn_name(&self, conn: u64) -> String {
        self.conn(conn).map(|c| c.meta.name.clone()).unwrap_or_default()
    }

    pub fn conn_writable(&self, conn: u64) -> bool {
        self.conn(conn).is_some_and(|c| c.writable)
    }

    pub fn ok(&mut self, s: impl Into<String>) {
        self.status = s.into();
    }

    pub fn push_undo(&mut self, a: UndoAction) {
        self.undo.insert(0, a);
        trim_undo(&mut self.undo, Instant::now());
    }

    /// Records an error for the session list and explains it with what is known of where it happened.
    pub fn log_err(&mut self, raw: impl Into<String>, conn: Option<u64>, sel: Option<Sel>, place: String, what: &str) -> ErrEntry {
        self.err_seq += 1;
        let e = self.entry(raw.into(), conn, sel, place, what, self.err_seq);
        self.errors.push(e.clone());
        if self.errors.len() > ERRORS_MAX {
            self.errors.remove(0);
        }
        self.errors_unseen += 1;
        e
    }

    /// Explains an error with what is known of where it happened, without logging it.
    pub fn entry(&self, raw: String, conn: Option<u64>, sel: Option<Sel>, place: String, what: &str, id: u64) -> ErrEntry {
        let c = conn.and_then(|id| self.conn(id));
        let host = c.map(|c| if c.server.host.is_empty() { host_of(&c.masked_uri) } else { c.server.host.clone() }).unwrap_or_default();
        let (conn_name, user) = c.map(|c| (c.meta.name.clone(), user_of(&c.masked_uri))).unwrap_or_default();
        let ns = sel.as_ref().map(Sel::ns).unwrap_or_default();
        let ex = db::explain(&raw, &db::ErrCtx { what: what.into(), ns, host });
        ErrEntry { id, at: std::time::SystemTime::now(), conn, conn_name, user, sel, place, raw, ex, connect: what == "connect" }
    }

    /// Where the current tab is, for errors that come without a place of their own.
    fn here(&self) -> (Option<u64>, Option<Sel>, String) {
        match self.t() {
            Some(t) => (Some(t.sel.conn), Some(t.sel.clone()), place_of(t.view, &t.sel)),
            None => (self.active, None, String::new()),
        }
    }

    /// An action failed: an error toast that stays until dismissed, and an entry in the list.
    pub fn err(&mut self, s: impl Into<String>) {
        let (conn, sel, place) = self.here();
        let e = self.log_err(s, conn, sel, place, "");
        self.err_toast(e);
    }

    /// Whether the failure card of `conn` is on screen: alone with no tab open, or in place of the
    /// empty results of one of its Documents, Aggregate or Indexes views.
    pub fn conn_card_shown(&self, conn: u64) -> bool {
        if !self.conn(conn).is_some_and(|c| matches!(c.status, Status::Failed(_))) {
            return false;
        }
        match self.t() {
            None => self.active == Some(conn),
            Some(t) => t.sel.conn == conn && matches!(t.view, View::Documents | View::Aggregate | View::Indexes),
        }
    }

    pub fn err_toast(&mut self, e: ErrEntry) {
        let mut acts: Vec<(String, Act)> = Vec::new();
        // A SQL error is placed in the tab that was open when it came back, which need not be the table it is about, so
        // the click reads that table's indexes and looks for the key's index there.
        if let (Some(_), Some(sel), Some(c)) = (&e.ex.dup, &e.sel, e.conn.and_then(|c| self.conn(c))) {
            let sql = c.meta.engine.is_sql();
            if !sql || (!sel.coll.is_empty() && db::dup_filterable(&e.raw, &sel.coll).is_some()) {
                acts.push((if sql { "Show the other row" } else { "Show the other document" }.into(), Act::ShowDup));
            }
        }
        for f in &e.ex.fixes {
            // Retry needs the thing that failed, which a toast does not know; the action is still there to redo.
            if *f != Fix::Retry || e.connect {
                acts.push((fix_label(*f, self.prefs.timeout_ms), Act::Fix(*f)));
            }
        }
        acts.truncate(2);
        self.toasts.retain(|t| !(t.tone == Tone::Err && t.title == e.ex.title));
        acts.push(("Details".into(), Act::Details));
        acts.push(("Copy".into(), Act::Copy));
        self.err_seq += 1;
        self.toasts.push(Toast { id: self.err_seq, tone: Tone::Err, title: e.ex.title.clone(), body: e.ex.hint.clone(), acts, at: Instant::now(), err: Some(e) });
    }

    /// Something that could not start: a warning toast that fades, never a logged error.
    pub fn warn(&mut self, s: impl Into<String>) {
        self.warn_act(s, None);
    }

    pub fn warn_act(&mut self, s: impl Into<String>, act: Option<(&str, Act)>) {
        let s = s.into();
        let (title, body) = match s.split_once(". ") {
            Some((t, b)) if t.len() < 70 => (t.to_string(), b.trim_end_matches('.').to_string() + "."),
            _ => (s.trim_end_matches('.').to_string(), String::new()),
        };
        self.toasts.retain(|t| !(t.tone == Tone::Warn && t.title == title));
        let acts = act.map(|(l, a)| vec![(l.to_string(), a)]).unwrap_or_default();
        self.err_seq += 1;
        self.toasts.push(Toast { id: self.err_seq, tone: Tone::Warn, title, body, acts, at: Instant::now(), err: None });
    }

    /// The current connection is read-only: says so with the unlock at hand.
    pub fn warn_ro(&mut self) {
        self.warn_ro_on(self.t().map(|t| t.sel.conn).or(self.active));
    }

    /// `warn_ro` for a window that works on `conn` rather than the current tab's connection.
    pub fn warn_ro_on(&mut self, conn: Option<u64>) {
        let name = conn.and_then(|c| self.conn(c)).map(|c| c.meta.name.clone()).unwrap_or_else(|| "This connection".into());
        self.warn_act(format!("{name} is read-only. Unlock writes to make this change."), conn.map(|c| ("Unlock writes", Act::Unlock(c))));
    }

    /// Nothing to work on without a connection, with Connect at hand when there is one to open.
    pub fn warn_offline(&mut self) {
        self.warn_offline_on(self.t().map(|t| t.sel.conn).or(self.active));
    }

    /// `warn_offline` for a window that works on `conn`.
    pub fn warn_offline_on(&mut self, conn: Option<u64>) {
        let conn = conn.filter(|c| self.conn(*c).is_some_and(|c| c.status != Status::Ready));
        self.warn_act("Not connected. Connect to this server first.", conn.map(|c| ("Connect", Act::Connect(c))));
    }

    pub fn warn_settings(&mut self, s: impl Into<String>) {
        self.warn_act(s, Some(("Open Settings", Act::Settings)));
    }

    pub fn tm(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.cur)
    }

    pub fn sel_is_view(&self) -> bool {
        self.t().is_some_and(|t| self.views.has(t.sel.conn, &t.sel.nskey(), &t.sel.coll))
    }

    pub fn sel_writable(&self) -> bool {
        if self.sel_is_view() {
            return false;
        }
        self.t().and_then(|t| self.conn(t.sel.conn)).is_some_and(|c| c.writable)
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.prefs.timeout_ms)
    }

    pub fn target_for(&self, s: &Sel) -> Option<Target> {
        let client = self.conn(s.conn)?.client.clone()?;
        Some(Target { client, db: s.db.clone(), coll: s.coll.clone(), timeout: self.timeout() })
    }

    pub fn target(&self) -> Option<Target> {
        self.target_for(&self.t()?.sel)
    }

    /// The single gate every write goes through.
    pub fn writable_target(&mut self) -> Option<(u64, Target)> {
        let conn = self.t()?.sel.conn;
        if !self.sel_writable() {
            self.warn_ro();
            return None;
        }
        match self.target() {
            Some(t) => Some((conn, t)),
            None => {
                self.warn_offline();
                None
            }
        }
    }

    /// Documents per page, from the setting; a hand-edited value is kept in range.
    pub fn page_size(&self) -> u64 {
        self.prefs.page_size.clamp(1, db::PAGE_SIZE_MAX)
    }

    pub fn write_config(&mut self, file: &str, bytes: Vec<u8>) {
        if self.ephemeral {
            return;
        }
        if let Err(err) = crate::state::config::write_atomic(&crate::paths::config_dir(), file, &bytes) {
            self.err(format!("Could not save {file}: {err}"));
        }
    }

    /// Marks ui.json to be written; `flush_prefs` writes it once per frame, however many settings changed in it.
    pub fn save_prefs(&mut self) {
        self.prefs_dirty = true;
    }

    /// Writes ui.json when something marked it since the last flush.
    pub fn flush_prefs(&mut self) {
        if std::mem::take(&mut self.prefs_dirty) {
            let b = serde_json::to_vec_pretty(&self.prefs).unwrap_or_default();
            self.write_config("ui.json", b);
        }
    }

    /// A failure of work the user did not start, like listing a database's collections on
    /// connect: it goes to the list with its own connection and place, without a toast.
    pub fn err_quiet(&mut self, raw: impl Into<String>, conn: Option<u64>, sel: Option<Sel>, place: String) {
        self.log_err(raw, conn, sel, place, "");
    }

    /// The read for `view` in tab `i` failed: the view shows the panel until its next read works.
    pub fn fail_view(&mut self, i: usize, view: View, raw: String, what: &str) {
        let Some(t) = self.tabs.get(i) else { return };
        let (conn, sel) = (t.sel.conn, t.sel.clone());
        let place = place_of(view, &sel);
        let e = self.log_err(raw, Some(conn), Some(sel), place, what);
        // Each view keeps its own: a background index load must not hide why the documents failed.
        let t = &mut self.tabs[i];
        t.clear_failed(view);
        t.failed.push(Failed { view, e });
    }

    /// A success note with Undo when the change can be undone.
    pub fn toast_ok(&mut self, s: String, undo_at: Option<Instant>) {
        let acts = undo_at.map(|at| vec![("Undo".to_string(), Act::Undo(at))]).unwrap_or_default();
        self.toasts.retain(|t| t.tone != Tone::Ok);
        self.err_seq += 1;
        self.toasts.push(Toast { id: self.err_seq, tone: Tone::Ok, title: s, body: String::new(), acts, at: Instant::now(), err: None });
    }

    /// A success note whose Undo puts back something that is not a database edit, which `apply_undo` could not do on a
    /// read-only connection: a group, saved scripts or a file.
    pub fn toast_restore(&mut self, s: String, r: crate::state::restore::Restore) {
        self.toast_restore_edit(s, r, None);
    }

    /// `toast_restore` with a Rename beside Undo that opens the Edit group dialog on `group`.
    pub fn toast_restore_edit(&mut self, s: String, r: crate::state::restore::Restore, group: Option<u64>) {
        self.err_seq += 1;
        let id = self.err_seq;
        self.restores.push((id, r));
        if self.restores.len() > RESTORES_MAX {
            self.restores.remove(0);
        }
        let mut acts = vec![("Undo".to_string(), Act::Restore(id))];
        acts.extend(group.map(|g| ("Rename".to_string(), Act::EditGroup(g))));
        self.toasts.retain(|t| t.tone != Tone::Ok);
        self.toasts.push(Toast { id, tone: Tone::Ok, title: s, body: String::new(), acts, at: Instant::now(), err: None });
    }
}
