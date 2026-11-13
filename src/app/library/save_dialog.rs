//! The Save to library dialog. Filled in by stream B1.
#![allow(dead_code)]

use super::*;

/// The fields of the Save to library dialog. B1 adds them.
#[derive(Default)]
pub(in crate::app) struct SaveForm {}

impl App {
    pub(in crate::app) fn save_dialog(&mut self, ui: &mut egui::Ui, _f: &mut SaveForm, keep: &mut bool) {
        dialog_head(ui, &Head::new("Save to library"));
        *keep = false;
    }
}
