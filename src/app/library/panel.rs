//! The Library panel of the sidebar. Filled in by stream B3.
#![allow(dead_code)]

use super::*;

/// Whether the rail shows the Library button. B3 turns it on with the panel.
pub(in crate::app) const READY: bool = false;

impl App {
    /// The body of the sidebar in Library mode.
    pub(in crate::app) fn library_panel(&mut self, _ui: &mut egui::Ui) {}

    /// QUARRY_TEST_LIBRARY=<state>: save, picker, picker-docs, picker-shell (B1); panel, all, search, empty, menu,
    /// folder-menu, drag (B3); linked, missing, link, save-linked, conflict (B2).
    pub(in crate::app) fn demo_library(&mut self, _state: &str) {}
}
