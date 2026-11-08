//! The settings dialog.

use super::*;

impl App {
    pub(super) fn settings_dialog(&mut self, ui: &mut egui::Ui, keep: &mut bool) {
        ui.set_width(600.0);
        dialog_title(ui, "Settings");
        ui.label(RichText::new("Changes are saved right away.").size(TXT_SMALL).color(FAINT()));
        ui.add_space(8.0);
        // Settings that save when changed. Tuples compare up to 12 items, so the readout toggles are grouped.
        let saved = |p: &UiPrefs| (p.open_inline, p.auto_save, p.always_writable, p.confirm_save, p.default_cols, p.timeout_ms, p.shell_runtime.clone(), p.mongodump.clone(), p.mongorestore.clone(), p.auto_update, (p.show_fps, p.show_mem, p.show_cpu), p.object_preview);
        let before = saved(&self.prefs);
        // The sections scroll inside the dialog, which keeps its title and Done in the window.
        let max_h = (ui.ctx().content_rect().height() - 190.0).max(200.0);
        egui::ScrollArea::vertical().id_salt("settings-scroll").max_height(max_h).min_scrolled_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
            settings_section(ui, "APPEARANCE", |ui| {
                setting_row(ui, "Theme", "System follows the macOS light or dark setting.", |ui| {
                    let was = self.prefs.appearance;
                    segmented(ui, &mut self.prefs.appearance, &[(Appearance::System, "System"), (Appearance::Light, "Light"), (Appearance::Dark, "Dark")]);
                    if self.prefs.appearance != was {
                        self.prefs.appearance.apply(ui.ctx());
                        self.save_prefs();
                    }
                });
            });
            settings_section(ui, "DOCUMENTS", |ui| {
                setting_row(ui, "Open documents", "Where a clicked row shows its document.", |ui| {
                    segmented(ui, &mut self.prefs.open_inline, &[(false, "Side panel"), (true, "Under the row")]);
                });
                ui.separator();
                setting_row(ui, "Columns shown", "How many fields the table shows before you pick your own.", |ui| {
                    stepper(ui, &mut self.prefs.default_cols, 3, 30);
                });
                ui.separator();
                setting_row(ui, "Big objects in columns", "Objects and arrays too long for a cell show their size, or their first characters as in Mingo. Small ones always show in full.", |ui| {
                    segmented(ui, &mut self.prefs.object_preview, &[(false, "Size"), (true, "First characters")]);
                });
            });
            settings_section(ui, "EDITING", |ui| {
                setting_row(ui, "Auto-save edits", "Write each edit right away and offer Undo for 8 seconds.", |ui| {
                    toggle(ui, &mut self.prefs.auto_save);
                });
                ui.separator();
                setting_row(ui, "Always allow writes", "Skip read-only mode on every connection and save edits without showing the changes first.", |ui| {
                    toggle(ui, &mut self.prefs.always_writable);
                });
                ui.separator();
                let auto = self.prefs.auto_save || self.prefs.always_writable;
                setting_row(ui, "Show changes before saving", "List every changed field before writing unsaved edits.", |ui| {
                    ui.add_enabled_ui(!auto, |ui| toggle(ui, &mut self.prefs.confirm_save));
                });
                ui.label(RichText::new(if self.prefs.always_writable { "A save never overwrites someone else's change." } else { "Writes always need the connection unlocked, and a save never overwrites someone else's change." }).size(TXT_SMALL).color(FAINT()));
            });
            settings_section(ui, "QUERIES", |ui| {
                setting_row(ui, "Query timeout", "How long a query may run before it is stopped.", |ui| {
                    egui::ComboBox::from_id_salt("timeout").width(90.0).selected_text(format!("{} s", self.prefs.timeout_ms / 1000)).show_ui(ui, |ui| {
                        menu_look(ui);
                        // Same presets as the original's preferences.
                        for s in [5, 10, 30, 60, 300, 3600] {
                            ui.selectable_value(&mut self.prefs.timeout_ms, s * 1000, format!("{s} s"));
                        }
                    });
                });
            });
            settings_section(ui, "SHELL", |ui| {
                let detected = find_bun("").map(|p| p.display().to_string());
                let desc = match &detected {
                    Some(p) => format!("Found automatically at {p}. Set a path only to use another Bun."),
                    None => "Bun was not found. Install it from bun.sh or enter its path.".to_string(),
                };
                setting_row(ui, "Bun runtime", &desc, |ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.prefs.shell_runtime).hint_text("automatic").desired_width(190.0));
                });
            });
            settings_section(ui, "PERFORMANCE", |ui| {
                setting_row(ui, "Show frame rate", "Frames per second and the time each frame takes, in the status bar. Quarry follows your display's refresh rate (vsync), up to 120 Hz on ProMotion screens.", |ui| {
                    toggle(ui, &mut self.prefs.show_fps);
                });
                setting_row(ui, "Show memory", "How much memory Quarry uses, in the status bar. Same number as Activity Monitor's Memory column on macOS, resident memory elsewhere.", |ui| {
                    toggle(ui, &mut self.prefs.show_mem);
                });
                setting_row(ui, "Show CPU", "Quarry's CPU use over the last second, in percent of one core like Activity Monitor.", |ui| {
                    toggle(ui, &mut self.prefs.show_cpu);
                });
            });
            settings_section(ui, "UPDATES", |ui| {
                let desc = if self.update_note.is_empty() { format!("Version {}. New releases of {} are installed in the background, then Quarry restarts when idle with nothing unsaved.", env!("CARGO_PKG_VERSION"), crate::update::REPO) } else { format!("Version {}. {}", env!("CARGO_PKG_VERSION"), self.update_note) };
                setting_row(ui, "Update automatically", &desc, |ui| {
                    toggle(ui, &mut self.prefs.auto_update);
                });
                if !self.demo && !cfg!(debug_assertions) && self.updated.is_none() && ui.button("Check now").clicked() {
                    self.check_update();
                }
            });
            settings_section(ui, "DATABASE TOOLS", |ui| {
                for (i, (name, path)) in [("mongodump", &mut self.prefs.mongodump), ("mongorestore", &mut self.prefs.mongorestore)].into_iter().enumerate() {
                    if i > 0 {
                        ui.separator();
                    }
                    let desc = match find_exe(name, "") {
                        Some(p) => format!("Found automatically at {}.", p.display()),
                        None => "Not found. Install the MongoDB Database Tools or enter its path.".to_string(),
                    };
                    setting_row(ui, name, &desc, |ui| {
                        ui.add(egui::TextEdit::singleline(path).hint_text("automatic").desired_width(190.0));
                    });
                }
            });
            if find_exe("mongodump", "").is_none() || find_exe("mongorestore", "").is_none() {
                self.install_tools_button(ui);
            }
        });
        if done(ui) {
            *keep = false;
        }
        // The pref unlocks every connection each frame, so turning it off has to lock them back.
        if before.2 && !self.prefs.always_writable {
            self.conns.iter_mut().for_each(|c| c.writable = false);
        }
        if before != saved(&self.prefs) {
            self.save_prefs();
        }
    }
}
