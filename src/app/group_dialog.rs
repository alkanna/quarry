//! The Edit group dialog. Filled in by stream A1; F0 holds the frozen form type and the stubs the modal switch and the
//! restore toast call.
#![allow(dead_code)]

use super::*;

/// The fields of the Edit group dialog. A1 adds them.
#[derive(Default)]
pub(super) struct GroupForm {}

impl App {
    /// Opens the Edit group dialog on a group, from the Rename action of a toast or a menu.
    pub(super) fn edit_group(&mut self, _group: u64) {}

    pub(super) fn group_dialog(&mut self, ui: &mut egui::Ui, _f: &mut GroupForm, keep: &mut bool) {
        dialog_head(ui, &Head::new("Edit group"));
        *keep = false;
    }
}
