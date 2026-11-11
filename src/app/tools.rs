//! mongodump and mongorestore: finding, installing and running them.

use super::*;

/// Bun for the Shell: the path from Settings, else the usual install places. Apps opened from
/// Finder do not get the terminal's PATH, so the common folders are checked directly.
pub(super) fn find_bun(setting: &str) -> Option<PathBuf> {
    find_exe("bun", setting)
}

/// A program from its Settings path, else from PATH and the usual install folders.
pub(super) fn find_exe(name: &str, setting: &str) -> Option<PathBuf> {
    let mut extra = ["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from).to_vec();
    if let Some(h) = dirs::home_dir() {
        extra.push(h.join(".bun").join("bin"));
    }
    crate::cloudsql::find_program(&format!("{name}{}", std::env::consts::EXE_SUFFIX), setting, &extra)
}

impl App {
    /// Progress and output of mongodump or mongorestore.
    pub(super) fn tool_window(&mut self, ctx: &egui::Context) {
        if self.modal.is_some() {
            return;
        }
        let Some(t) = &mut self.tool else { return };
        let mut close = false;
        dialog(egui::Id::unique(("quarry-layer", "tool"))).show(ctx, |ui| {
            ui.set_width(640.0);
            ui.horizontal(|ui| {
                dialog_title(ui, &t.title);
                match &t.done {
                    None => {
                        spin(ui);
                    }
                    Some(Ok(m)) => {
                        ui.label(RichText::new(m).color(SUCCESS()));
                    }
                    Some(Err(e)) => {
                        ui.label(RichText::new(e).color(ALERT()));
                    }
                }
            });
            egui::Frame::new().fill(pal().well).corner_radius(R_MD).inner_margin(8).show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(320.0).stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                    for l in &t.log {
                        ui.label(RichText::new(l).monospace().size(TXT_SMALL).color(TEXT()));
                    }
                });
            });
            dialog_actions(ui, |ui| {
                if t.done.is_none() {
                    if danger(ui, "Stop").clicked() {
                        if let Some(c) = t.cancel.take() {
                            let _ = c.send(());
                        }
                    }
                } else {
                    close = ui.button("Close").clicked();
                }
                if ui.button("Copy log").clicked() {
                    ui.ctx().copy_text(t.log.join("\n"));
                }
            });
        });
        if close {
            self.tool = None;
        }
    }

    pub(super) fn install_tools(&mut self) {
        if !self.tools_installing {
            self.tools_installing = true;
            self.worker.blocking(|| Msg::ToolsInstalled(crate::update::install_db_tools(&tools_dir()).map(Some)));
        }
    }

    /// The Install button, or a spinner while the download runs.
    pub(super) fn install_tools_button(&mut self, ui: &mut egui::Ui) {
        if self.tools_installing {
            ui.horizontal(|ui| {
                spin(ui);
                ui.label(RichText::new("Downloading the MongoDB Database Tools…").color(MUTED()));
            });
        } else if ui.button("Install the MongoDB Database Tools").on_hover_text(format!("Downloads MongoDB's official build into {}", tools_dir().display())).clicked() {
            self.install_tools();
        }
    }

    pub(super) fn start_tool(&mut self, restore: bool, title: String, args: Vec<String>) {
        let (name, setting) = if restore { ("mongorestore", &self.prefs.mongorestore) } else { ("mongodump", &self.prefs.mongodump) };
        let Some(exe) = find_exe(name, setting) else {
            return self.warn_settings(format!("{name} was not found. Install the MongoDB Database Tools from Settings or set its path there."));
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tool = Some(ToolRun { title, log: Vec::new(), cancel: Some(tx), done: None });
        self.worker.tool(exe, args, rx);
    }
}

impl App {
    pub(super) fn tool_dialog(&mut self, ui: &mut egui::Ui, f: &mut ToolForm, keep: &mut bool) {
        let name = if f.coll.is_empty() { f.db.clone() } else { format!("{}.{}", f.db, f.coll) };
        ui.set_width(560.0);
        dialog_title(ui, if f.restore { format!("Restore into {name}") } else { format!("Dump {name}") });
        let meta = self.conn(f.conn).map(|c| c.meta.clone()).unwrap_or_default();
        let tool = if f.restore { "mongorestore" } else { "mongodump" };
        let exe = find_exe(tool, if f.restore { &self.prefs.mongorestore } else { &self.prefs.mongodump });
        ui.label(RichText::new(match &exe {
            Some(p) => format!("Runs {} with this connection's address.", p.display()),
            None => format!("{tool} was not found. Install the MongoDB Database Tools here, or set its path in Settings."),
        }).size(TXT_SMALL).color(if exe.is_some() { MUTED() } else { WARNING() }));
        if exe.is_none() {
            self.install_tools_button(ui);
        }
        let tunneled = meta.ssh.is_some() || meta.proxy.is_some();
        if tunneled {
            ui.label(RichText::new("This connection goes through SSH or a proxy, which the MongoDB tools cannot use. Open an SSH port forward and use a direct connection instead.").color(WARNING()));
        }
        ui.add_space(6.0);
        egui::Grid::new("toolform").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
            form_label(ui, "Archive file");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut f.path).desired_width(360.0));
                if ui.button("Choose…").clicked() {
                    let d = rfd::FileDialog::new();
                    let picked = if f.restore { d.pick_file() } else { d.set_file_name(format!("{name}.archive.gz")).save_file() };
                    if let Some(p) = picked {
                        f.path = p.display().to_string();
                        if f.restore {
                            f.gzip = f.path.ends_with(".gz");
                        }
                    }
                }
            });
            ui.end_row();
            form_label(ui, "");
            ui.checkbox(&mut f.gzip, "Compressed (gzip)");
            ui.end_row();
            if f.restore {
                form_label(ui, "Database in the archive");
                ui.add(egui::TextEdit::singleline(&mut f.from_db).desired_width(200.0)).on_hover_text("Restored under the name of this database when it differs");
                ui.end_row();
                form_label(ui, "Only collection");
                ui.add(egui::TextEdit::singleline(&mut f.coll).hint_text("all").desired_width(200.0));
                ui.end_row();
                form_label(ui, "");
                let col = if f.drop { ALERT() } else { TEXT() };
                ui.checkbox(&mut f.drop, RichText::new("Drop each collection before restoring it").color(col));
                ui.end_row();
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let ok = exe.is_some() && !tunneled && !f.path.trim().is_empty() && (!f.restore || !f.from_db.trim().is_empty());
            if ui.add_enabled(ok, primary_button(ui, if f.restore { "Restore…" } else { "Dump" }, if f.restore { WARNING() } else { PRIMARY() })).clicked() {
                match stored_uri(f.conn) {
                    Err(e) => self.err(uri_err(e)),
                    Ok(uri) => {
                        let coll = Some(f.coll.trim()).filter(|c| !c.is_empty());
                        let path = f.path.trim();
                        if f.restore {
                            let args = db::restore_args(&uri, path, f.from_db.trim(), coll, &f.db, f.drop, f.gzip);
                            let what = if f.drop { "Existing collections with the same names are dropped first. " } else { "" };
                            self.modal = Some(Modal::Typed { title: format!("Restore {path} into {}? {what}Type the database name.", f.db), expect: f.db.clone(), typed: String::new(), action: Danger::Restore { conn: f.conn, args } });
                        } else {
                            self.start_tool(false, format!("Dump {name}"), db::dump_args(&uri, &f.db, coll, path, f.gzip));
                        }
                        *keep = false;
                    }
                }
            }
            if ui.button("Cancel").clicked() {
                *keep = false;
            }
        });
    }
}
