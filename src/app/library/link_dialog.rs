//! The Link a folder dialog. Filled in by stream B2.
#![allow(dead_code)]

use super::*;

/// The fields of the Link a folder dialog. B2 adds them.
#[derive(Default)]
pub(in crate::app) struct LinkForm {}

impl App {
    pub(in crate::app) fn link_dialog(&mut self, ui: &mut egui::Ui, _f: &mut LinkForm, keep: &mut bool) {
        dialog_head(ui, &Head::new("Link a folder"));
        *keep = false;
    }
}
