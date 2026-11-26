//! mongodump and mongorestore: finding, installing and running them.

use super::*;

pub(super) use quarry_core::action::tools::{find_bun, find_exe};
use quarry_core::action::{Action, Refusal, RefusalKind, ToolsA};

impl App {
    /// Progress and output of mongodump or mongorestore.
    pub(super) fn tool_window(&mut self, ctx: &egui::Context) {
        if self.modal.is_some() {
            return;
        }
        let Some(t) = &mut self.core.tool else { return };
        let (mut close, mut stop) = (false, false);
        let running = t.done.is_none();
        let r = banded(egui::Id::unique(("quarry-layer", "tool"))).show(ctx, |ui| {
            ui.set_width(660.0);
            let sub = match &t.done {
                None => "Running…".to_string(),
                Some(Ok(_)) => "Finished".to_string(),
                Some(Err(_)) => "Failed".to_string(),
            };
            // While the tool runs it is stopped, not closed, so there is no close button until it ends.
            let head = Head::new(t.title.as_str()).sub(sub).glyph(glyphs::code);
            dialog_head(ui, &if running { head.no_close() } else { head });
            dialog_pane(ui, |ui| {
                match &t.done {
                    Some(Ok(m)) => {
                        banner(ui, Notice::Ok, "", m, None);
                    }
                    Some(Err(e)) => {
                        banner(ui, Notice::Danger, "", e, None);
                    }
                    None => {}
                }
                code_card(ui, "Output", |ui| {
                    egui::ScrollArea::vertical().max_height(320.0).stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                        for l in &t.log {
                            ui.label(RichText::new(l).monospace().size(TXT_SMALL).color(TEXT()));
                        }
                    });
                });
            });
            dialog_foot(ui, |ui| {
                if t.done.is_none() {
                    stop = danger(ui, "Stop").clicked();
                } else {
                    close = primary(ui, "Close", PRIMARY()).clicked();
                }
                foot_left(ui, |ui| {
                    if t.done.is_none() {
                        spin(ui);
                    }
                    if ui.button("Copy log").clicked() {
                        ui.ctx().copy_text(t.log.join("\n"));
                    }
                });
            });
        });
        if stop {
            self.exec(Action::Tools(ToolsA::Stop));
        }
        if close || (!running && r.should_close()) {
            self.exec(Action::Tools(ToolsA::Close));
        }
    }

    /// The Install button, or a spinner while the download runs.
    pub(super) fn install_tools_button(&mut self, ui: &mut egui::Ui) {
        if self.core.tools_installing {
            ui.horizontal(|ui| {
                spin(ui);
                ui.label(RichText::new("Downloading the MongoDB Database Tools…").color(MUTED()));
            });
        } else if ui.button("Install the MongoDB Database Tools").on_hover_text(format!("Downloads MongoDB's official build into {}", tools_dir().display())).clicked() {
            self.exec(Action::Tools(ToolsA::Install));
        }
    }

    /// mongorestore after its typed confirm; its arguments hold the address, so they never go into an action.
    pub(super) fn start_tool(&mut self, restore: bool, title: String, args: Vec<String>) {
        let _ = self.core.start_tool(restore, title, args);
    }
}

impl App {
    pub(super) fn tool_dialog(&mut self, ui: &mut egui::Ui, f: &mut ToolForm, keep: &mut bool) {
        let name = if f.coll.is_empty() { f.db.clone() } else { format!("{}.{}", f.db, f.coll) };
        ui.set_width(560.0);
        let conn_name = self.conn_name(f.conn);
        dialog_head(ui, &Head::new(if f.restore { format!("Restore into {name}") } else { format!("Dump {name}") }).sub(format!("{conn_name}  ·  MongoDB Database Tools")).glyph(if f.restore { glyphs::upload } else { glyphs::download }));
        let mut b = body_begin(ui);
        let outer = ui;
        let ui = &mut b.content_ui;
        let meta = self.conn(f.conn).map(|c| c.meta.clone()).unwrap_or_default();
        let tool = if f.restore { "mongorestore" } else { "mongodump" };
        let exe = find_exe(tool, if f.restore { &self.core.prefs.mongorestore } else { &self.core.prefs.mongodump });
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
        form_grid(ui, "toolform", |ui| {
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
        b.end(outer);
        let ui = outer;
        dialog_foot(ui, |ui| {
            let ok = exe.is_some() && !tunneled && !f.path.trim().is_empty() && (!f.restore || !f.from_db.trim().is_empty());
            if ui.add_enabled(ok, primary_button(ui, if f.restore { "Restore…" } else { "Dump" }, if f.restore { WARNING() } else { PRIMARY() })).clicked() {
                let coll = Some(f.coll.trim()).filter(|c| !c.is_empty());
                let path = f.path.trim();
                if f.restore {
                    match stored_uri(f.conn) {
                        Err(e) => self.err(uri_err(e)),
                        Ok(_) => {
                            let what = if f.drop { "Existing collections with the same names are dropped first. " } else { "" };
                            self.modal = Some(Modal::Typed { title: format!("Restore {path} into {}? {what}Type the database name.", f.db), expect: f.db.clone(), typed: String::new(), action: Danger::Restore { conn: f.conn, archive: path.to_string(), from_db: f.from_db.trim().to_string(), coll: coll.map(str::to_string), to_db: f.db.clone(), drop: f.drop, gzip: f.gzip } });
                            *keep = false;
                        }
                    }
                } else {
                    let r = self.core.execute(Action::Tools(ToolsA::Dump { conn: f.conn, db: f.db.clone(), coll: coll.map(str::to_string), path: path.to_string(), gzip: f.gzip }));
                    self.run_fx();
                    // Only an address that could not be read keeps the dialog, as before.
                    if !matches!(r, Err(Refusal { kind: RefusalKind::Invalid, .. })) {
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
