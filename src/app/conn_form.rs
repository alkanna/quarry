//! The connection form: address, auth, SSH and proxy settings, with Test and Save.

use super::*;

impl App {
    pub(super) fn conn_dialog(&mut self, ui: &mut egui::Ui, f: &mut ConnForm, keep: &mut bool) {
        let mut save = false;
        let mut test = false;
        ui.set_width(600.0);
        dialog_title(ui, if f.id.is_some() { "Edit connection" } else { "New connection" });
        ui.label(RichText::new("Addresses, passwords and passphrases are stored in the macOS Keychain. The connection opens read-only.").size(TXT_SMALL).color(MUTED()));
        ui.add_space(8.0);
        egui::ScrollArea::vertical().max_height(ui.ctx().content_rect().height() - 220.0).show(ui, |ui| {
            settings_section(ui, "SERVER", |ui| {
                // Grid cells do not know their width on the first frame, so size fields from the section.
                let field_w = ui.available_width() - FORM_LABEL_W - 14.0;
                egui::Grid::new("cf").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
                    form_label(ui, "Name");
                    ui.add(egui::TextEdit::singleline(&mut f.name).hint_text("Production cluster").desired_width(field_w).margin(egui::Margin::symmetric(8, 5)));
                    ui.end_row();
                    form_label(ui, "Tag");
                    ui.vertical(|ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            for t in std::iter::once("").chain(TAG_PRESETS) {
                                let c = if t.is_empty() { MUTED() } else { tag_color(t) };
                                if chip(ui, if t.is_empty() { "None" } else { t }, c, clean_tag(&f.tag).eq_ignore_ascii_case(t)).clicked() {
                                    f.tag = t.to_string();
                                }
                            }
                        });
                        ui.add_space(4.0);
                        ui.add(egui::TextEdit::singleline(&mut f.tag).hint_text("or type a custom tag").char_limit(24).desired_width(220.0).margin(egui::Margin::symmetric(8, 5)));
                    });
                    ui.end_row();
                    form_label(ui, "URI");
                    ui.vertical(|ui| {
                        let edit = if f.show { egui::TextEdit::multiline(&mut f.uri).desired_rows(3).return_key(None) } else { egui::TextEdit::singleline(&mut f.uri).password(true) };
                        ui.add(edit.font(egui::TextStyle::Monospace).hint_text("mongodb+srv://user:password@host/").desired_width(field_w).margin(egui::Margin::symmetric(8, 6)));
                        // Wrapping is only visual; a pasted newline would break the URI.
                        if f.show {
                            f.uri.retain(|c| c != '\n' && c != '\r');
                        }
                        ui.add_space(2.0);
                        ui.checkbox(&mut f.show, "Show address");
                    });
                    ui.end_row();
                });
            });
            settings_section(ui, "SSH TUNNEL", |ui| {
                ui.checkbox(&mut f.ssh_on, "Connect through an SSH server (jump host)");
                if f.ssh_on {
                    egui::Grid::new("ssh").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
                        form_label(ui, "Host");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut f.ssh.host).hint_text("bastion.example.com").desired_width(280.0));
                            ui.label("Port");
                            let mut port = if f.ssh.port == 0 { "22".to_string() } else { f.ssh.port.to_string() };
                            if ui.add(egui::TextEdit::singleline(&mut port).desired_width(60.0)).changed() {
                                f.ssh.port = port.trim().parse().unwrap_or(22);
                            }
                        });
                        ui.end_row();
                        form_label(ui, "User");
                        ui.add(egui::TextEdit::singleline(&mut f.ssh.user).desired_width(280.0));
                        ui.end_row();
                        form_label(ui, "Log in with");
                        segmented(ui, &mut f.ssh.auth, &[(crate::ssh::SshAuth::Auto, "Auto"), (crate::ssh::SshAuth::Password, "Password"), (crate::ssh::SshAuth::KeyFile, "Key file"), (crate::ssh::SshAuth::Agent, "SSH agent")]);
                        ui.end_row();
                        use crate::ssh::SshAuth::*;
                        if matches!(f.ssh.auth, Auto | Password) {
                            form_label(ui, "Password");
                            ui.add(egui::TextEdit::singleline(&mut f.ssh_pwd).password(true).hint_text(if f.ssh.auth == Auto { "optional, tried last" } else { "" }).desired_width(280.0));
                            ui.end_row();
                        }
                        if matches!(f.ssh.auth, Auto | KeyFile) {
                            form_label(ui, "Key file");
                            ui.horizontal(|ui| {
                                ui.add(egui::TextEdit::singleline(&mut f.ssh.key_path).hint_text(if f.ssh.auth == Auto { "optional; ~/.ssh/id_* are tried" } else { "~/.ssh/id_ed25519" }).desired_width(280.0));
                                if ui.button("Choose…").clicked() {
                                    if let Some(p) = rfd::FileDialog::new().set_directory(dirs::home_dir().unwrap_or_default().join(".ssh")).pick_file() {
                                        f.ssh.key_path = p.display().to_string();
                                    }
                                }
                            });
                            ui.end_row();
                            form_label(ui, "Passphrase");
                            ui.add(egui::TextEdit::singleline(&mut f.ssh_pass).password(true).hint_text("if the key has one").desired_width(280.0));
                            ui.end_row();
                        }
                        ui.label("");
                        ui.checkbox(&mut f.ssh.legacy_rsa, "Allow legacy ssh-rsa host keys (old servers)");
                        ui.end_row();
                        form_label(ui, "Host key");
                        ui.horizontal(|ui| match f.ssh.known_host.clone() {
                            Some(fp) => {
                                ui.label(RichText::new(fp).monospace().size(TXT_SMALL).color(MUTED()));
                                if ui.button("Forget").on_hover_text("Trust the next key the server shows, e.g. after a reinstall").clicked() {
                                    f.ssh.known_host = None;
                                }
                            }
                            None => {
                                ui.label(RichText::new("remembered on first connect; a different key later is refused").size(TXT_SMALL).color(FAINT()));
                            }
                        });
                        ui.end_row();
                    });
                }
            });
            settings_section(ui, "SOCKS5 PROXY", |ui| {
                ui.add_enabled(!f.ssh_on, egui::Checkbox::new(&mut f.proxy_on, "Connect through a SOCKS5 proxy"));
                if f.ssh_on {
                    ui.label(RichText::new("Not needed with an SSH tunnel: the tunnel already carries the connection.").size(TXT_SMALL).color(FAINT()));
                } else if f.proxy_on {
                    egui::Grid::new("px").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
                        form_label(ui, "Host");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut f.proxy.host).desired_width(280.0));
                            ui.label("Port");
                            let mut port = if f.proxy.port == 0 { "1080".to_string() } else { f.proxy.port.to_string() };
                            if ui.add(egui::TextEdit::singleline(&mut port).desired_width(60.0)).changed() {
                                f.proxy.port = port.trim().parse().unwrap_or(1080);
                            }
                        });
                        ui.end_row();
                        form_label(ui, "User");
                        ui.add(egui::TextEdit::singleline(&mut f.proxy.user).hint_text("optional").desired_width(280.0));
                        ui.end_row();
                        form_label(ui, "Password");
                        ui.add(egui::TextEdit::singleline(&mut f.proxy_pwd).password(true).desired_width(280.0));
                        ui.end_row();
                    });
                }
            });
        });
        match &self.conn_test {
            None if f.testing => {
                ui.horizontal(|ui| {
                    spin(ui);
                    ui.label(RichText::new("Testing…").color(MUTED()));
                });
            }
            Some(Ok(m)) => {
                ui.label(RichText::new(m).color(SUCCESS()));
            }
            Some(Err(e)) => {
                ui.add(egui::Label::new(RichText::new(e).color(ALERT())).wrap());
            }
            None => {}
        }
        dialog_actions(ui, |ui| {
            let valid = !f.name.trim().is_empty() && (f.uri.starts_with("mongodb://") || f.uri.starts_with("mongodb+srv://")) && (!f.ssh_on || (!f.ssh.host.trim().is_empty() && !f.ssh.user.trim().is_empty())) && (!f.proxy_on || f.ssh_on || !f.proxy.host.trim().is_empty());
            save = ui.add_enabled(valid, primary_button(ui, "Save and connect", PRIMARY())).clicked();
            test = ui.add_enabled(valid, egui::Button::new("Test")).clicked();
            if ui.button("Cancel").clicked() {
                *keep = false;
            }
        });
        if test {
            f.testing = true;
            self.conn_test = None;
            let net = f.net();
            self.worker.test(f.uri.trim().to_string(), self.timeout(), net);
        }
        if save {
            let id = f.id.unwrap_or_else(|| self.conns.iter().map(|c| c.meta.id).max().unwrap_or(0) + 1);
            // Demo ids overlap real ones, so the demo keeps its edits in memory only.
            let saved = if self.demo { Some(Ok(())) } else { keychain(id).map(|k| k.set_password(f.uri.trim())) };
            match saved {
                Some(Ok(())) => {
                    let stored = if self.demo {
                        Vec::new()
                    } else {
                        vec![
                            set_secret(id, "ssh-password", if f.ssh_on { &f.ssh_pwd } else { "" }),
                            set_secret(id, "ssh-passphrase", if f.ssh_on { &f.ssh_pass } else { "" }),
                            set_secret(id, "proxy-password", if f.proxy_on && !f.ssh_on { &f.proxy_pwd } else { "" }),
                        ]
                    };
                    let meta = ConnMeta {
                        id,
                        name: f.name.trim().into(),
                        tag: clean_tag(&f.tag).into(),
                        ssh: f.ssh_on.then(|| crate::ssh::SshConf { host: f.ssh.host.trim().into(), user: f.ssh.user.trim().into(), ..f.ssh.clone() }),
                        proxy: (f.proxy_on && !f.ssh_on).then(|| db::Proxy { host: f.proxy.host.trim().into(), password: None, ..f.proxy.clone() }),
                    };
                    match self.conns.iter_mut().find(|c| c.meta.id == id) {
                        Some(c) => {
                            c.meta = meta;
                            c.masked_uri = db::mask_uri(f.uri.trim());
                            c.writable = false;
                            c.dbs.clear();
                            c.server = Default::default();
                        }
                        None => self.conns.push(Conn { meta, client: None, status: Status::Idle, writable: false, dbs: Vec::new(), masked_uri: db::mask_uri(f.uri.trim()), tunnel: None, server: Default::default() }),
                    }
                    self.active = Some(id);
                    self.save_conns();
                    self.connect(id);
                    *keep = false;
                    if let Some(Err(e)) = stored.into_iter().find(|r| r.is_err()) {
                        self.err(e);
                    }
                }
                Some(Err(e)) => self.err(format!("Could not save the address to the Keychain: {e}")),
                None => self.err("The Keychain is not available."),
            }
        }
    }
}
