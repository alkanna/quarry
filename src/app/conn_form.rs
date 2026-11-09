//! The connection form: engine, address, auth, SSH, TLS and proxy settings, with Test and Save.

use super::*;

/// One engine in the left column. Disabled cards show what comes later.
fn engine_card(ui: &mut egui::Ui, code: &str, name: &str, sub: &str, color: Color32, selected: bool, enabled: bool) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 50.0), if enabled { Sense::click() } else { Sense::hover() });
    let state = if enabled { state_of(&r) } else { 0 };
    let p = ui.painter();
    let fill = match (selected, state) {
        (true, 0) => SEL(),
        (true, _) => SEL_HOVER(),
        (false, s) => hover_fill(s),
    };
    p.rect(rect, R_MD, PANEL(), Stroke::new(1.0, if selected { PRIMARY() } else { BORDER() }), egui::StrokeKind::Inside);
    p.rect_filled(rect.shrink(1.0), R_MD, fill);
    let fg = if enabled { color } else { FAINT() };
    let mark = egui::Rect::from_min_size(rect.left_center() + egui::vec2(12.0, -14.0), egui::vec2(28.0, 28.0));
    p.rect_filled(mark, R_SM, tint(fg, 40));
    let g = p.layout_job(egui::text::LayoutJob::single_section(code.to_string(), mono_fmt(TXT_SMALL, fg, W_SEMIBOLD)));
    p.galley(mark.center() - g.size() / 2.0 + egui::vec2(0.0, optical_dy(&g)), g, fg);
    let title = p.layout_job(egui::text::LayoutJob::single_section(name.to_string(), ui_fmt(TXT_BODY, Color32::PLACEHOLDER, W_SEMIBOLD, 0.0)));
    let sub = p.layout_job(egui::text::LayoutJob::single_section(sub.to_string(), ui_fmt(TXT_SMALL, Color32::PLACEHOLDER, 400.0, 0.0)));
    let x = mark.right() + 10.0;
    p.galley(egui::pos2(x, rect.center().y - 16.0), title, if enabled { BRIGHT() } else { FAINT() });
    p.galley(egui::pos2(x, rect.center().y + 1.0), sub, if enabled { MUTED() } else { FAINT() });
    focus_ring(ui, &r, rect, R_MD as f32);
    let label = name.to_string();
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, enabled, selected, label.clone()));
    if enabled { r.on_hover_cursor(egui::CursorIcon::PointingHand) } else { r }
}

fn tls_tab(ui: &mut egui::Ui, f: &mut ConnForm) {
    settings_section(ui, "TLS", |ui| {
        egui::Grid::new("tls").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
            form_label(ui, "SSL mode");
            segmented(ui, &mut f.addr.ssl_mode, &db::sql::SslMode::ALL);
            ui.end_row();
            form_label(ui, "CA file");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut f.addr.ssl_ca).hint_text("optional, PEM file").desired_width(280.0));
                if ui.button("Choose…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().set_directory(dirs::home_dir().unwrap_or_default()).pick_file() {
                        f.addr.ssl_ca = p.display().to_string();
                    }
                }
            });
            ui.end_row();
        });
        ui.add_space(6.0);
        ui.label(RichText::new("Through an SSH tunnel the host name is the tunnel's, so Verify identity behaves like Verify CA.").size(TXT_SMALL).color(FAINT()));
    });
}

fn standard_tab(ui: &mut egui::Ui, f: &mut ConnForm) {
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
            if !f.engine.is_sql() {
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
                return;
            }
            form_label(ui, "Host");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut f.addr.host).hint_text("localhost").desired_width(field_w - 100.0).margin(egui::Margin::symmetric(8, 5)));
                ui.label("Port");
                let mut port = if f.addr.port == 0 { db::sql::DEFAULT_PORT.to_string() } else { f.addr.port.to_string() };
                if ui.add(egui::TextEdit::singleline(&mut port).desired_width(60.0).margin(egui::Margin::symmetric(8, 5))).changed() {
                    f.addr.port = port.trim().parse().unwrap_or(db::sql::DEFAULT_PORT);
                }
            });
            ui.end_row();
            form_label(ui, "User");
            ui.add(egui::TextEdit::singleline(&mut f.addr.user).desired_width(200.0).margin(egui::Margin::symmetric(8, 5)));
            ui.end_row();
            form_label(ui, "Password");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut f.addr.password).password(true).font(egui::TextStyle::Monospace).desired_width(200.0).margin(egui::Margin::symmetric(8, 5)));
                ui.label(RichText::new("Kept in the Keychain").size(TXT_SMALL).color(FAINT()));
            });
            ui.end_row();
            form_label(ui, "Database");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut f.addr.database).hint_text("All databases").desired_width(200.0).margin(egui::Margin::symmetric(8, 5)));
                ui.label(RichText::new("Optional, opens this one first").size(TXT_SMALL).color(FAINT()));
            });
            ui.end_row();
        });
    });
}

fn ssh_tab(ui: &mut egui::Ui, f: &mut ConnForm) {
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
}

fn proxy_tab(ui: &mut egui::Ui, f: &mut ConnForm) {
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
}

impl App {
    pub(super) fn conn_dialog(&mut self, ui: &mut egui::Ui, f: &mut ConnForm, keep: &mut bool) {
        let mut save = false;
        let mut test = false;
        ui.set_width(760.0);
        dialog_title(ui, if f.id.is_some() { "Edit connection" } else { "New connection" });
        ui.label(RichText::new("Addresses, passwords and passphrases are stored in the macOS Keychain. The connection opens read-only.").size(TXT_SMALL).color(MUTED()));
        ui.add_space(8.0);
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(egui::vec2(210.0, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                let sql = f.engine.is_sql();
                let maria = sql && f.addr.scheme == "mariadb";
                let mut pick = None;
                if engine_card(ui, "MO", "MongoDB", "mongodb://, Atlas SRV", C_STRING(), !sql, true).clicked() {
                    pick = Some((db::sql::Engine::Mongo, ""));
                }
                if engine_card(ui, "MY", "MySQL", "5.7, 8.x, 9.x", C_LINK(), sql && !maria, true).clicked() {
                    pick = Some((db::sql::Engine::MySql, "mysql"));
                }
                if engine_card(ui, "MA", "MariaDB", "10.6 and later", C_KEYWORD(), maria, true).clicked() {
                    pick = Some((db::sql::Engine::MySql, "mariadb"));
                }
                engine_card(ui, "PO", "PostgreSQL", "Coming later", FAINT(), false, false);
                engine_card(ui, "SQ", "SQLite", "Coming later", FAINT(), false, false);
                if let Some((engine, scheme)) = pick {
                    f.engine = engine;
                    if engine.is_sql() {
                        f.addr.scheme = scheme.into();
                        if f.addr.port == 0 {
                            f.addr.port = db::sql::DEFAULT_PORT;
                        }
                    }
                    // The other engine has no tab for the one that was open.
                    if matches!((engine.is_sql(), f.tab), (true, ConnTab::Proxy) | (false, ConnTab::Tls)) {
                        f.tab = ConnTab::Standard;
                    }
                }
            });
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 18.0;
                    let last = if f.engine.is_sql() { (ConnTab::Tls, "TLS") } else { (ConnTab::Proxy, "Proxy") };
                    for (t, label) in [(ConnTab::Standard, "Standard"), (ConnTab::Ssh, "SSH"), last] {
                        if view_tab(ui, label, f.tab == t).clicked() {
                            f.tab = t;
                        }
                    }
                });
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(ui.ctx().content_rect().height() - 260.0).show(ui, |ui| match f.tab {
                    ConnTab::Standard => standard_tab(ui, f),
                    ConnTab::Ssh => ssh_tab(ui, f),
                    ConnTab::Proxy => proxy_tab(ui, f),
                    ConnTab::Tls => tls_tab(ui, f),
                });
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
            let address_ok = if f.engine.is_sql() { !f.addr.host.trim().is_empty() && !f.addr.user.trim().is_empty() } else { f.uri.starts_with("mongodb://") || f.uri.starts_with("mongodb+srv://") };
            let valid = !f.name.trim().is_empty() && address_ok && (!f.ssh_on || (!f.ssh.host.trim().is_empty() && !f.ssh.user.trim().is_empty())) && (!f.proxy_on || f.ssh_on || f.engine.is_sql() || !f.proxy.host.trim().is_empty());
            save = ui.add_enabled(valid, primary_button(ui, "Save and connect", PRIMARY())).clicked();
            // Temporary until the SQL worker lands (docs/sql/PLAN.md step 4).
            let sql = f.engine.is_sql();
            test = ui.add_enabled(valid && !sql, egui::Button::new("Test")).on_disabled_hover_text(if sql { "MySQL connections arrive in a later step" } else { "" }).clicked();
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
            // SQL addresses are rebuilt from the fields, so the Keychain holds one URL for either engine.
            let uri = if f.engine.is_sql() { db::sql::Addr { host: f.addr.host.trim().into(), user: f.addr.user.trim().into(), database: f.addr.database.trim().into(), ..f.addr.clone() }.to_uri() } else { f.uri.trim().to_string() };
            let saved = if self.demo { Some(Ok(())) } else { keychain(id).map(|k| k.set_password(&uri)) };
            match saved {
                Some(Ok(())) => {
                    let stored = if self.demo {
                        Vec::new()
                    } else {
                        vec![
                            set_secret(id, "ssh-password", if f.ssh_on { &f.ssh_pwd } else { "" }),
                            set_secret(id, "ssh-passphrase", if f.ssh_on { &f.ssh_pass } else { "" }),
                            set_secret(id, "proxy-password", if f.proxy_on && !f.ssh_on && !f.engine.is_sql() { &f.proxy_pwd } else { "" }),
                        ]
                    };
                    let meta = ConnMeta {
                        id,
                        engine: f.engine,
                        name: f.name.trim().into(),
                        tag: clean_tag(&f.tag).into(),
                        ssh: f.ssh_on.then(|| crate::ssh::SshConf { host: f.ssh.host.trim().into(), user: f.ssh.user.trim().into(), ..f.ssh.clone() }),
                        proxy: (f.proxy_on && !f.ssh_on && !f.engine.is_sql()).then(|| db::Proxy { host: f.proxy.host.trim().into(), password: None, ..f.proxy.clone() }),
                    };
                    match self.conns.iter_mut().find(|c| c.meta.id == id) {
                        Some(c) => {
                            c.meta = meta;
                            c.masked_uri = db::mask_uri(&uri);
                            c.writable = false;
                            c.dbs.clear();
                            c.server = Default::default();
                        }
                        None => self.conns.push(Conn { meta, client: None, status: Status::Idle, writable: false, dbs: Vec::new(), masked_uri: db::mask_uri(&uri), tunnel: None, server: Default::default() }),
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
