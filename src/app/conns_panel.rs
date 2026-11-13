//! The Connections panel of the sidebar: saved connections in colored groups. Filled in by stream A1; F0 holds the
//! frozen state type and the stubs the rail, the sidebar and the demo call.
#![allow(dead_code)]

use super::*;

/// Whether the rail shows the Connections button. A1 turns it on with the panel.
pub(super) const READY: bool = false;

/// What the Connections panel remembers while the app runs. A1 adds its fields.
#[derive(Default)]
pub(super) struct ConnsUi {}

impl App {
    /// The body of the sidebar in Connections mode.
    pub(super) fn conns_panel(&mut self, _ui: &mut egui::Ui) {}

    /// QUARRY_TEST_CONNS=<state>: panel, filter, menu, new, edit, drag or picker.
    pub(super) fn demo_conns(&mut self, _state: &str) {}
}
