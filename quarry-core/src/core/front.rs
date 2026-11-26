//! The UI holds the core through `Front`: it reads any state, but changes it only through these doors, so a click
//! cannot skip the action path that tests and agents go through.

use super::Core;
use crate::action::agents::{AgentCall, Origin};
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

    /// Startup only: the demo builders and test switches fill the core before the first frame. Panics after that,
    /// so drawing code cannot use it to skip the action path.
    pub fn setup(&mut self) -> &mut Core {
        assert!(!self.1, "Front::setup after the first tick");
        &mut self.0
    }

    pub fn execute(&mut self, a: Action) -> Result<Reply, Refusal> {
        self.0.execute(a)
    }

    pub fn execute_as(&mut self, origin: Origin, a: AgentCall) {
        self.0.execute_as(origin, a)
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

    /// The text behind `r`, for a `TextEdit`. None when its window is not open.
    pub fn text_mut(&mut self, r: TextRef) -> Option<&mut String> {
        match r {
            TextRef::Insert => self.0.insert.as_mut(),
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

    /// Ends the run in flight of an Explain dialog the app holds, when it closes.
    pub fn sql_explain_stop(&mut self, x: &mut crate::state::sql::explain::SqlExplain) {
        self.0.sql_explain_stop(x)
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

    pub fn warn_act(&mut self, s: impl Into<String>, act: Option<(&str, Act)>) {
        self.0.warn_act(s, act)
    }

    pub fn warn_ro(&mut self) {
        self.0.warn_ro()
    }

    pub fn warn_ro_on(&mut self, conn: Option<u64>) {
        self.0.warn_ro_on(conn)
    }

    pub fn warn_offline(&mut self) {
        self.0.warn_offline()
    }

    pub fn warn_offline_on(&mut self, conn: Option<u64>) {
        self.0.warn_offline_on(conn)
    }

    pub fn warn_settings(&mut self, s: impl Into<String>) {
        self.0.warn_settings(s)
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

    pub fn toast_restore_edit(&mut self, s: String, r: Restore, group: Option<u64>) {
        self.0.toast_restore_edit(s, r, group)
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

    /// The gate a write passes: the target, or the read-only or offline toast and None.
    pub fn writable_target(&mut self) -> Option<(u64, crate::db::Target)> {
        self.0.writable_target()
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

    /// Tests set up state directly; release builds have no such door.
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_mut(&mut self) -> &mut Core {
        &mut self.0
    }
}
