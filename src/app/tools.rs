//! mongodump and mongorestore: finding, installing and running them.

use super::*;

/// Bun for the Shell: the path from Settings, else the usual install places. Apps opened from
/// Finder do not get the terminal's PATH, so the common folders are checked directly.
pub(crate) fn find_bun(setting: &str) -> Option<PathBuf> {
    find_exe("bun", setting)
}

/// A program from its Settings path, else from PATH and the usual install folders.
pub(crate) fn find_exe(name: &str, setting: &str) -> Option<PathBuf> {
    if !setting.trim().is_empty() {
        return Some(PathBuf::from(setting.trim()));
    }
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    if let Some(h) = dirs::home_dir() {
        dirs.push(h.join(".bun").join("bin"));
    }
    dirs.push(tools_dir());
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    dirs.into_iter().map(|d| d.join(&file)).find(|p| p.is_file())
}
impl App {
    /// Users of all databases with their roles, custom roles, and user actions. Writes need the
    /// connection unlocked; dropping a user asks for its name.
    /// Progress and output of mongodump or mongorestore.
    pub(crate) fn tool_window(&mut self, ctx: &egui::Context) {
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

    pub(crate) fn install_tools(&mut self) {
        if !self.tools_installing {
            self.tools_installing = true;
            self.worker.blocking(|| Msg::ToolsInstalled(crate::update::install_db_tools(&tools_dir()).map(Some)));
        }
    }

    /// The Install button, or a spinner while the download runs.
    pub(crate) fn install_tools_button(&mut self, ui: &mut egui::Ui) {
        if self.tools_installing {
            ui.horizontal(|ui| {
                spin(ui);
                ui.label(RichText::new("Downloading the MongoDB Database Tools…").color(MUTED()));
            });
        } else if ui.button("Install the MongoDB Database Tools").on_hover_text(format!("Downloads MongoDB's official build into {}", tools_dir().display())).clicked() {
            self.install_tools();
        }
    }

    pub(crate) fn start_tool(&mut self, restore: bool, title: String, args: Vec<String>) {
        let (name, setting) = if restore { ("mongorestore", &self.prefs.mongorestore) } else { ("mongodump", &self.prefs.mongodump) };
        let Some(exe) = find_exe(name, setting) else {
            return self.err(format!("{name} was not found. Install the MongoDB Database Tools from Settings or set its path there."));
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tool = Some(ToolRun { title, log: Vec::new(), cancel: Some(tx), done: None });
        self.worker.tool(exe, args, rx);
    }
}
