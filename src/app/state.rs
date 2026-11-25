//! App methods over the state types, which live in quarry_core::state.

use super::*;
pub(crate) use quarry_core::state::*;

/// What to run once writes are allowed.
pub(super) type Retry = Box<dyn FnOnce(&mut App)>;

/// The core's dialogs, carrying the app closure an unlock reruns.
pub(crate) type Modal = quarry_core::state::Modal<Retry>;

impl App {
    pub(super) fn ok(&mut self, s: impl Into<String>) {
        self.core.ok(s)
    }

    pub(super) fn conn(&self, id: u64) -> Option<&Conn> {
        self.core.conn(id)
    }

    pub(super) fn t(&self) -> Option<&Tab> {
        self.core.tabs.get(self.core.cur)
    }

    pub(super) fn tm(&mut self) -> Option<&mut Tab> {
        self.core.tm()
    }

    pub(super) fn sel_is_view(&self) -> bool {
        self.core.sel_is_view()
    }

    pub(super) fn sel_writable(&self) -> bool {
        self.core.sel_writable()
    }

    pub(super) fn timeout(&self) -> Duration {
        self.core.timeout()
    }

    pub(super) fn target_for(&self, s: &Sel) -> Option<Target> {
        self.core.target_for(s)
    }

    pub(super) fn target(&self) -> Option<Target> {
        self.core.target()
    }

    pub(super) fn writable_target(&mut self) -> Option<(u64, Target)> {
        self.core.writable_target()
    }

    pub(super) fn ask_unlock(&mut self) {
        if let Some(t) = self.t() {
            self.modal = Some(Modal::Unlock(t.sel.conn, None));
        }
    }

    /// Asks to allow writes, then runs `then` if they were allowed.
    pub(super) fn ask_unlock_then(&mut self, then: impl FnOnce(&mut App) + 'static) {
        if let Some(t) = self.t() {
            self.modal = Some(Modal::Unlock(t.sel.conn, Some(Box::new(then))));
        }
    }

    pub(super) fn connect(&mut self, id: u64) {
        self.exec(Action::Conn(ConnA::Connect { conn: id }));
    }

    /// Something a restart would lose: unsaved edits, writes in flight, running scripts or tools.
    /// Work a restart would lose. Settings does not count, since Restart sits in it; every other dialog may hold typed input.
    pub(super) fn busy_work(&self) -> bool {
        self.modal.as_ref().is_some_and(|m| !matches!(m, Modal::Settings))
            || !self.core.drafts.is_empty()
            || !self.core.saving.is_empty()
            || self.core.json_edit.is_some()
            || self.core.insert.is_some()
            || self.core.inline.is_some()
            || self.core.new_field.is_some()
            || self.core.tool.as_ref().is_some_and(|t| t.done.is_none())
            || self.core.tabs.iter().any(|t| t.shell.cancel.is_some() || t.compare.running || t.loading)
    }
}
