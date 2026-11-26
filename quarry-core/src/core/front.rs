//! The UI holds the core through `Front`: it reads any state, but changes it only through these doors, so a click
//! cannot skip the action path that tests and agents go through.

use super::Core;
use crate::action::{Action, Refusal, Reply, UiFx};
use crate::db::Msg;
use crate::state::errors::{Act, ErrEntry};
use crate::state::restore::Restore;
use crate::state::{Sel, View};
use std::ops::Deref;

/// The big or hot text a window edits in place, which a `TextEdit` binds a `&mut String` of. A short text is cloned per
/// frame and set back through an action instead; add a variant here only for a buffer that would be too big or too hot to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextRef {
    /// The Insert a document dialog's JSON.
    Insert,
    /// The Import view's source text.
    ImportText,
}

pub struct Front(Core, bool);

impl Deref for Front {
    type Target = Core;
    fn deref(&self) -> &Core {
        &self.0
    }
}

impl Front {
    /// Takes a core once it is built, demo data included; it is not writable from outside after this.
    pub fn new(core: Core) -> Self {
        Front(core, false)
    }

    /// Startup only: the demo builders and test switches fill the core before the first frame. Debug builds panic
    /// after that, so drawing code cannot use it to skip the action path; tests load demos into a running harness.
    pub fn setup(&mut self) -> &mut Core {
        debug_assert!(cfg!(feature = "test-support") || !self.1, "Front::setup after the first tick");
        &mut self.0
    }

    pub fn execute(&mut self, a: Action) -> Result<Reply, Refusal> {
        self.0.execute(a)
    }


    pub fn on_msg(&mut self, m: Msg) {
        self.0.on_msg(m)
    }

    pub fn tick(&mut self) -> Vec<Msg> {
        self.1 = true;
        self.0.tick()
    }

    pub fn take_ui_fx(&mut self) -> Vec<UiFx> {
        self.0.take_ui_fx()
    }

    /// The big value editor is drawn in place, taken out and put back in the same frame: its document and text can be
    /// megabytes, too big to copy on every repaint as other editors are.
    pub fn take_json_edit(&mut self) -> Option<crate::state::JsonEdit> {
        self.0.json_edit.take()
    }

    pub fn put_json_edit(&mut self, je: crate::state::JsonEdit) {
        self.0.json_edit = Some(je);
    }

    /// The SQL value editor, drawn in place as the big value editor is: a cell's text can be megabytes too.
    pub fn take_sql_cell(&mut self) -> Option<crate::state::sql::edit::CellEdit> {
        self.0.sql_cell.take()
    }

    pub fn put_sql_cell(&mut self, e: crate::state::sql::edit::CellEdit) {
        self.0.sql_cell = Some(e);
    }

    /// The text behind `r`, for a `TextEdit`. None when its window is not open.
    pub fn text_mut(&mut self, r: TextRef) -> Option<&mut String> {
        match r {
            TextRef::Insert => self.0.insert.as_mut(),
            TextRef::ImportText => self.0.tabs.get_mut(self.0.cur).map(|t| &mut t.import.text),
        }
    }

    // Answers the app tries on its own dialogs first, and hands to Core when none of them took it. Core applies them as
    // `on_msg` does the rest.
    pub fn docs_loaded(&mut self, req: u64, res: crate::db::Res<(Vec<crate::db::Row>, u64, u128)>, approx: bool, info: &dyn Fn(&[crate::db::Row], Option<&std::collections::HashMap<String, Vec<mongodb::bson::Bson>>>) -> (Vec<String>, crate::state::complete::Fields)) {
        self.0.docs_loaded(req, res, approx, info)
    }

    pub fn structure_loaded(&mut self, req: u64, res: crate::db::Res<crate::db::sql::structure::Structure>) {
        self.0.structure_loaded(req, res)
    }

    /// The duplicate-key toast's wait for the table's indexes, when `req` is the one it waits on.
    pub fn take_dup_wait(&mut self, req: u64) -> Option<(u64, Sel, String, String)> {
        self.0.dup_wait.take_if(|w| w.0 == req)
    }

    // Notes for the user (toasts and the error list) are feedback, not state a click decides, so drawing code may raise
    // them directly. These shadow the Core methods of the same name, which keeps `self.core.warn(..)` working.
    pub fn ok(&mut self, s: impl Into<String>) {
        self.0.ok(s)
    }

    pub fn err(&mut self, s: impl Into<String>) {
        self.0.err(s)
    }

    pub fn warn(&mut self, s: impl Into<String>) {
        self.0.warn(s)
    }


    pub fn warn_ro(&mut self) {
        self.0.warn_ro()
    }


    pub fn warn_offline(&mut self) {
        self.0.warn_offline()
    }



    pub fn toast_ok(&mut self, s: String, undo_at: Option<std::time::Instant>) {
        self.0.toast_ok(s, undo_at)
    }

    pub fn toast_acts(&mut self, s: String, acts: Vec<(String, Act)>) {
        self.0.toast_acts(s, acts)
    }

    pub fn toast_restore(&mut self, s: String, r: Restore) {
        self.0.toast_restore(s, r)
    }


    pub fn log_err(&mut self, raw: impl Into<String>, conn: Option<u64>, sel: Option<Sel>, place: String, what: &str) -> ErrEntry {
        self.0.log_err(raw, conn, sel, place, what)
    }

    /// The read for `view` in tab `i` failed: the view shows the panel until its next read works.
    pub fn fail_view(&mut self, i: usize, view: View, raw: String, what: &str) {
        self.0.fail_view(i, view, raw, what)
    }

    /// What the window shows right now, which an agent call waits on: focus, typing and an open dialog.
    pub fn set_agent_ui(&mut self, ui: crate::state::agents::AgentUi) {
        self.0.agents.ui = ui;
    }

    /// The update timers' clock: what they decide is Core's, the answer is when to look again.
    pub fn update_tick(&mut self) -> Option<std::time::Duration> {
        self.0.update_tick()
    }

    /// Writes the settings changed this frame to ui.json in one go.
    pub fn flush_prefs(&mut self) {
        self.0.flush_prefs()
    }

    /// The toasts' timer: drops the ones whose time is up (a clock, not a decision).
    pub fn toasts_tick(&mut self) -> bool {
        self.0.toasts_tick()
    }

    /// The library's timers (scheduled scans, a script waiting for its connection): `now` is the UI clock in seconds and
    /// `focused` whether the window has focus; the answer is when to look again.
    pub fn library_tick(&mut self, now: f64, focused: bool) -> Option<f64> {
        self.0.library_tick(now, focused)
    }

    /// Tests set up state directly; release builds have no such door.
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_mut(&mut self) -> &mut Core {
        &mut self.0
    }

    /// Tests send fake answers through the real channel, as the worker would.
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_worker(&self) -> &crate::db::Worker {
        &self.0.worker
    }
}
