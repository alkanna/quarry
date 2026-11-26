//! The Watch view: the collection's inserts, updates and deletes as they happen, from a change
//! stream that runs only while the view is on screen.

use super::*;

pub(super) use quarry_core::action::watch::watch_row;
use quarry_core::action::{Action, WatchA};

impl App {
    pub(super) fn watch_view(&mut self, ui: &mut egui::Ui) {
        let t = self.t().unwrap();
        let (running, n, err) = (t.watch.running(), t.watch.rows.len(), t.watch.err.clone());
        let (mut start, mut stop, mut clear) = (false, false, false);
        toolbar(ui, |ui| {
            ui.horizontal(|ui| {
                if running {
                    stop = ui.button("■ Stop").clicked();
                    spin(ui);
                    ui.label(RichText::new(format!("Watching: {n} change{}", if n == 1 { "" } else { "s" })).color(MUTED()));
                } else {
                    start = primary(ui, "▶ Start watching", PRIMARY()).clicked();
                    ui.label(RichText::new("Shows changes from now on. It stops when you leave this view.").color(MUTED()));
                }
                if n > 0 {
                    clear = ui.button("Clear").clicked();
                }
            });
        });
        if let Some(e) = err {
            ui.label(RichText::new(e).color(ALERT()));
        }
        let tab = self.cur_uid().unwrap_or_default();
        if start {
            self.exec(Action::Watch(WatchA::Start { tab }));
        }
        if stop {
            self.exec(Action::Watch(WatchA::Stop { tab }));
        }
        if clear {
            self.exec(Action::Watch(WatchA::Clear { tab }));
        }
        ui.add_space(6.0);
        if self.t().is_some_and(|t| t.watch.rows.is_empty()) {
            ui.add_space(14.0);
            ui.label(RichText::new(if running { "Waiting for the first change…" } else { "Nothing is read until you press Start." }).color(MUTED()));
        } else {
            self.results(ui, true);
        }
    }
}
