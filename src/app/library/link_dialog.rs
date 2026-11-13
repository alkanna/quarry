//! The Link a folder dialog. Filled in by stream B2.
#![allow(dead_code)]

use super::*;

/// The fields of the Link a folder dialog. B2 adds them.
#[derive(Default)]
pub(in crate::app) struct LinkForm {}

impl App {
    /// Opens the Link a folder dialog, editing the link `edit` when given. B2 fills this in.
    pub(in crate::app) fn open_link_dialog(&mut self, _edit: Option<u64>) {}

    pub(in crate::app) fn link_dialog(&mut self, ui: &mut egui::Ui, _f: &mut LinkForm, keep: &mut bool) {
        dialog_head(ui, &Head::new("Link a folder"));
        *keep = false;
    }
}
