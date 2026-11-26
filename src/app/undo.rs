//! The undo list of recent writes. Undoing runs in Core (`quarry_core::action::undo`); these forward for the other
//! areas' call sites.

use super::*;
use quarry_core::action::{Action, UndoA};

impl App {
    /// Undoes entry `i` of Recent changes; on a read-only connection Unlock opens and runs it again after.
    pub(super) fn apply_undo(&mut self, i: usize) {
        self.apply_undo_as(i, false);
    }

    pub(super) fn apply_undo_as(&mut self, i: usize, confirmed: bool) {
        let Some(at) = self.core.undo.get(i).map(|a| self.core.undo_id(a.at)) else { return };
        self.exec(Action::Undo(UndoA::Apply { at, confirmed }));
    }
}
