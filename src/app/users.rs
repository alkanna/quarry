//! The database users window.

use super::*;
use quarry_core::action::users::{roles_text, short_err, MongoUsersText};
use quarry_core::action::{Action, MongoUsersA, UsersA};

impl App {
    /// Users of all databases with their roles, custom roles, and user actions. Writes need the
    /// connection unlocked; dropping a user asks for its name.
    pub(super) fn users_window(&mut self, ctx: &egui::Context) {
        let Some(u) = self.core.users.as_ref() else { return };
        let conn = u.conn;
        let writable = self.conn(conn).is_some_and(|c| c.writable);
        let atlas = self.conn(conn).is_some_and(|c| c.masked_uri.contains(".mongodb.net"));
        let mut open = true;
        let mut acts: Vec<MongoUsersA> = Vec::new();
        let screen = ctx.content_rect();
        let conn_name = self.conn_name(conn);
        let r = banded(egui::Id::unique(("quarry-layer", "users-window"))).show(ctx, |ui| {
            ui.set_width(fit_width(ctx, (screen.width() * 0.6).clamp(600.0, 900.0)));
            dialog_head_with(ui, &Head::new("Users and roles").sub(conn_name).glyph(glyphs::user), |ui| {
                if ui.button("Refresh").clicked() {
                    acts.push(MongoUsersA::Refresh);
                }
            });
            let mut b = body_begin(ui);
            let outer = ui;
            let ui = &mut b.content_ui;
            if !writable && banner(ui, Notice::Locked, "Read-only.", "Unlock writes to add, change or drop users.", Some("Unlock…")) {
                acts.push(MongoUsersA::Unlock);
            }
            if u.self_only {
                banner(ui, Notice::Warn, "", "This user may not list other users (it needs viewUser on every database), so only your own user is shown.", None);
            }
            if atlas {
                banner(ui, Notice::Info, "", "Atlas manages database users in its web console; it refuses adding, changing or dropping users from here, and may list none.", None);
            }
            egui::ScrollArea::vertical().max_height(screen.height() - 320.0).show(ui, |ui| {
                settings_section(ui, "USERS", |ui| match &u.users {
                    None => {
                        spin(ui);
                    }
                    Some(Err(e)) => {
                        ui.add(egui::Label::new(RichText::new(format!("Could not list users: {}", short_err(e))).color(ALERT())).wrap()).on_hover_text(e);
                    }
                    Some(Ok(list)) => {
                        if list.is_empty() {
                            ui.label(RichText::new("No users.").color(MUTED()));
                        }
                        for user in list {
                            let (name, db) = (user.get_str("user").unwrap_or_default().to_string(), user.get_str("db").unwrap_or_default().to_string());
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&name).monospace().color(BRIGHT()));
                                ui.label(RichText::new(format!("@{db}")).monospace().color(FAINT()));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.add_enabled_ui(writable, |ui| danger(ui, "Drop…")).inner.clicked() {
                                        acts.push(MongoUsersA::Drop { user: name.clone(), db: db.clone() });
                                    }
                                    if ui.add_enabled(writable, egui::Button::new("Edit")).clicked() {
                                        acts.push(MongoUsersA::Edit { user: name.clone(), db: db.clone() });
                                    }
                                });
                            });
                            // On its own line and wrapped: a user can hold dozens of roles.
                            ui.add(egui::Label::new(RichText::new(roles_text(user)).monospace().size(TXT_SMALL).color(C_STRING())).wrap());
                            if let Some((en, edb, pwd, roles)) = &u.edit && *en == name && *edb == db {
                                let (mut pwd_t, mut roles_t) = (pwd.clone(), roles.clone());
                                egui::Frame::new().fill(pal().well).corner_radius(R_MD).inner_margin(8).show(ui, |ui| {
                                    egui::Grid::new(("edit", &name)).num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
                                        form_label(ui, "New password");
                                        ui.add(egui::TextEdit::singleline(&mut pwd_t).password(true).hint_text("leave empty to keep").desired_width(280.0));
                                        ui.end_row();
                                        form_label(ui, "Roles");
                                        ui.add(egui::TextEdit::singleline(&mut roles_t).hint_text("readWrite@shop, read@analytics").desired_width(280.0).font(egui::TextStyle::Monospace));
                                        ui.end_row();
                                    });
                                    if pwd_t != *pwd {
                                        acts.push(MongoUsersA::Password { edit: true, password: Secret::new(pwd_t) });
                                    }
                                    if roles_t != *roles {
                                        acts.push(MongoUsersA::Text(MongoUsersText::EditRoles, roles_t));
                                    }
                                    ui.horizontal(|ui| {
                                        if primary(ui, "Save", PRIMARY()).clicked() {
                                            acts.push(MongoUsersA::Save);
                                        }
                                        if ui.button("Cancel").clicked() {
                                            acts.push(MongoUsersA::CloseEdit);
                                        }
                                    });
                                });
                            }
                            ui.separator();
                        }
                    }
                });
                settings_section(ui, "ADD USER", |ui| {
                    let (mut name_t, mut pwd_t, mut db_t, mut roles_t) = (u.new_name.clone(), u.new_pwd.clone(), u.new_db.clone(), u.new_roles.clone());
                    egui::Grid::new("adduser").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
                        form_label(ui, "Name");
                        ui.add(egui::TextEdit::singleline(&mut name_t).desired_width(280.0));
                        ui.end_row();
                        form_label(ui, "Password");
                        ui.add(egui::TextEdit::singleline(&mut pwd_t).password(true).desired_width(280.0));
                        ui.end_row();
                        form_label(ui, "Database");
                        ui.add(egui::TextEdit::singleline(&mut db_t).hint_text("admin").desired_width(280.0));
                        ui.end_row();
                        form_label(ui, "Roles");
                        ui.add(egui::TextEdit::singleline(&mut roles_t).hint_text("readWrite@shop, read@analytics").desired_width(280.0).font(egui::TextStyle::Monospace));
                        ui.end_row();
                    });
                    for (f, new, old) in [(MongoUsersText::NewName, name_t, &u.new_name), (MongoUsersText::NewDb, db_t, &u.new_db), (MongoUsersText::NewRoles, roles_t, &u.new_roles)] {
                        if new != *old {
                            acts.push(MongoUsersA::Text(f, new));
                        }
                    }
                    let ok = writable && !u.new_name.trim().is_empty() && !pwd_t.is_empty();
                    if pwd_t != u.new_pwd {
                        acts.push(MongoUsersA::Password { edit: false, password: Secret::new(pwd_t) });
                    }
                    if ui.add_enabled(ok, primary_button(ui, "Add user", PRIMARY())).clicked() {
                        acts.push(MongoUsersA::Add);
                    }
                });
                settings_section(ui, "CUSTOM ROLES", |ui| match &u.roles {
                    None => {
                        spin(ui);
                    }
                    Some(Err(e)) if db::is_unauthorized(e) => {
                        ui.label(RichText::new("This user may not list custom roles.").color(MUTED()));
                    }
                    Some(Err(e)) => {
                        ui.add(egui::Label::new(RichText::new(format!("Could not list roles: {}", short_err(e))).color(ALERT())).wrap()).on_hover_text(e);
                    }
                    Some(Ok(list)) if list.is_empty() => {
                        ui.label(RichText::new("No custom roles; built-in roles like read, readWrite and dbAdmin can be given to users.").color(MUTED()));
                    }
                    Some(Ok(list)) => {
                        for r in list {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(format!("{}@{}", r.get_str("role").unwrap_or_default(), r.get_str("db").unwrap_or_default())).monospace().color(BRIGHT()));
                                ui.label(RichText::new(format!("inherits: {}", roles_text(r))).monospace().size(TXT_SMALL).color(FAINT()));
                            });
                        }
                    }
                });
            });
            b.end(outer);
            open = !dialog_done(outer);
        });
        for a in acts {
            self.exec(Action::Users(UsersA::Mongo(a)));
        }
        if !open || (r.should_close() && self.modal.is_none()) {
            self.exec(Action::Users(UsersA::Mongo(MongoUsersA::Close)));
        }
    }
}
