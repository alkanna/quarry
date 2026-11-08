//! The database users window.

use super::*;

/// "readWrite@shop, read" as role documents; a role without @db belongs to `default_db`.
pub(super) fn parse_roles(text: &str, default_db: &str) -> Vec<Bson> {
    text.split(',')
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(|r| {
            let (role, db) = r.split_once('@').unwrap_or((r, default_db));
            Bson::Document(doc! { "role": role.trim(), "db": db.trim() })
        })
        .collect()
}

/// Whether the server refused for lack of a privilege, in its own or Atlas's wording.
pub(super) fn denied(e: &str) -> bool {
    e.contains("(Unauthorized)") || e.contains("not authorized") || e.contains("is not allowed to do action")
}

/// A server error without the command it echoes back, which can run to several lines.
fn short_err(e: &str) -> &str {
    e.find(" to execute command").map_or(e, |i| &e[..i])
}

/// The logged-in user from a connectionStatus answer, which needs no privilege: the fallback when
/// usersInfo is refused.
pub(super) fn self_users(status: &Document) -> Vec<Document> {
    let auth = status.get_document("authInfo").cloned().unwrap_or_default();
    let roles = auth.get_array("authenticatedUserRoles").cloned().unwrap_or_default();
    auth.get_array("authenticatedUsers").into_iter().flatten().filter_map(Bson::as_document).map(|u| doc! { "user": u.get_str("user").unwrap_or_default(), "db": u.get_str("db").unwrap_or_default(), "roles": roles.clone() }).collect()
}

pub(super) fn roles_text(user: &Document) -> String {
    user.get_array("roles")
        .map(|a| a.iter().filter_map(|r| r.as_document()).map(|r| format!("{}@{}", r.get_str("role").unwrap_or_default(), r.get_str("db").unwrap_or_default())).collect::<Vec<_>>().join(", "))
        .unwrap_or_default()
}

impl App {
    /// Users of all databases with their roles, custom roles, and user actions. Writes need the
    /// connection unlocked; dropping a user asks for its name.
    pub(super) fn users_window(&mut self, ctx: &egui::Context) {
        let Some(mut u) = self.users.take() else { return };
        let conn = u.conn;
        let writable = self.conn(conn).is_some_and(|c| c.writable);
        let client = self.conn(conn).and_then(|c| c.client.clone());
        let atlas = self.conn(conn).is_some_and(|c| c.masked_uri.contains(".mongodb.net"));
        let mut open = true;
        let mut cmds: Vec<(String, Document, String)> = Vec::new();
        let mut drop_user = None;
        let mut unlock = false;
        let screen = ctx.content_rect();
        let r = dialog(egui::Id::unique(("quarry-layer", "users-window"))).show(ctx, |ui| {
            ui.set_width((screen.width() * 0.6).clamp(560.0, 900.0));
            ui.horizontal(|ui| {
                dialog_title(ui, "Users and roles");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⟳ Refresh").clicked() {
                        u.users = None;
                        u.roles = None;
                        u.self_only = false;
                        cmds.push(("*reload".into(), Document::new(), String::new()));
                    }
                });
            });
            if !writable {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Read-only: unlock writes to add, change or drop users.").color(SUCCESS()));
                    unlock = ui.button("Unlock…").clicked();
                });
            }
            if u.self_only {
                ui.label(RichText::new("This user may not list other users (it needs viewUser on every database), so only your own user is shown.").size(TXT_SMALL).color(WARNING()));
            }
            if atlas {
                ui.label(RichText::new("Atlas manages database users in its web console; it refuses adding, changing or dropping users from here, and may list none.").size(TXT_SMALL).color(WARNING()));
            }
            egui::ScrollArea::vertical().max_height(screen.height() - 220.0).show(ui, |ui| {
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
                                    if ui.add_enabled(writable, danger_button("Drop…")).clicked() {
                                        drop_user = Some((db.clone(), name.clone()));
                                    }
                                    if ui.add_enabled(writable, egui::Button::new("Edit")).clicked() {
                                        u.edit = Some((name.clone(), db.clone(), String::new(), roles_text(user)));
                                    }
                                });
                            });
                            // On its own line and wrapped: a user can hold dozens of roles.
                            ui.add(egui::Label::new(RichText::new(roles_text(user)).monospace().size(TXT_SMALL).color(C_STRING())).wrap());
                            if let Some((en, edb, pwd, roles)) = &mut u.edit {
                                if *en == name && *edb == db {
                                    egui::Frame::new().fill(pal().well).corner_radius(R_MD).inner_margin(8).show(ui, |ui| {
                                        egui::Grid::new(("edit", &name)).num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
                                            form_label(ui, "New password");
                                            ui.add(egui::TextEdit::singleline(pwd).password(true).hint_text("leave empty to keep").desired_width(280.0));
                                            ui.end_row();
                                            form_label(ui, "Roles");
                                            ui.add(egui::TextEdit::singleline(roles).hint_text("readWrite@shop, read@analytics").desired_width(280.0).font(egui::TextStyle::Monospace));
                                            ui.end_row();
                                        });
                                        ui.horizontal(|ui| {
                                            if primary(ui, "Save", PRIMARY()).clicked() {
                                                let mut cmd = doc! { "updateUser": en.as_str(), "roles": parse_roles(roles, edb) };
                                                if !pwd.is_empty() {
                                                    cmd.insert("pwd", pwd.as_str());
                                                }
                                                cmds.push((edb.clone(), cmd, format!("Updated user {en}")));
                                            }
                                            if ui.button("Cancel").clicked() {
                                                cmds.push(("*close-edit".into(), Document::new(), String::new()));
                                            }
                                        });
                                    });
                                }
                            }
                            ui.separator();
                        }
                    }
                });
                settings_section(ui, "ADD USER", |ui| {
                    egui::Grid::new("adduser").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
                        form_label(ui, "Name");
                        ui.add(egui::TextEdit::singleline(&mut u.new_name).desired_width(280.0));
                        ui.end_row();
                        form_label(ui, "Password");
                        ui.add(egui::TextEdit::singleline(&mut u.new_pwd).password(true).desired_width(280.0));
                        ui.end_row();
                        form_label(ui, "Database");
                        ui.add(egui::TextEdit::singleline(&mut u.new_db).hint_text("admin").desired_width(280.0));
                        ui.end_row();
                        form_label(ui, "Roles");
                        ui.add(egui::TextEdit::singleline(&mut u.new_roles).hint_text("readWrite@shop, read@analytics").desired_width(280.0).font(egui::TextStyle::Monospace));
                        ui.end_row();
                    });
                    let ok = writable && !u.new_name.trim().is_empty() && !u.new_pwd.is_empty();
                    if ui.add_enabled(ok, primary_button(ui, "Add user", PRIMARY())).clicked() {
                        let db = if u.new_db.trim().is_empty() { "admin".to_string() } else { u.new_db.trim().to_string() };
                        let cmd = doc! { "createUser": u.new_name.trim(), "pwd": u.new_pwd.as_str(), "roles": parse_roles(&u.new_roles, &db) };
                        cmds.push((db, cmd, format!("Added user {}", u.new_name.trim())));
                        u.new_pwd.clear();
                    }
                });
                settings_section(ui, "CUSTOM ROLES", |ui| match &u.roles {
                    None => {
                        spin(ui);
                    }
                    Some(Err(e)) if denied(e) => {
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
            open = !done(ui);
        });
        if unlock {
            self.modal = Some(Modal::Unlock(conn, None));
        }
        for (db, cmd, done) in cmds {
            match db.as_str() {
                "*reload" => self.load_users(conn),
                "*close-edit" => u.edit = None,
                _ => {
                    if let Some(client) = client.clone() {
                        u.edit = None;
                        self.worker.db_change(conn, client, db, cmd, done);
                    }
                }
            }
        }
        if let Some((db, user)) = drop_user {
            self.modal = Some(Modal::Typed { title: format!("Drop the user {user} of {db}? Apps that log in with it will stop working."), expect: user.clone(), typed: String::new(), action: Danger::DropUser { conn, db, user } });
        }
        if open && !(r.should_close() && self.modal.is_none()) {
            self.users = Some(u);
        }
    }

    /// Users of every database and custom roles, read from admin like the original
    /// (usersInfo / rolesInfo with showBuiltinRoles false, renderer ~156395).
    pub(super) fn load_users(&mut self, conn: u64) {
        let Some(client) = self.conn(conn).and_then(|c| c.client.clone()) else { return };
        self.worker.command(client.clone(), "admin".into(), doc! { "usersInfo": { "forAllDBs": true } }, "users");
        self.worker.command(client, "admin".into(), doc! { "rolesInfo": 1, "showBuiltinRoles": false }, "roles");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refused_listing_falls_back_to_the_logged_in_user() {
        let e = "Command failed: Error code 13 (Unauthorized): not authorized on admin to execute command { usersInfo: { forAllDBs: true }, $db: \"admin\" }";
        assert!(denied(e));
        assert_eq!(short_err(e), "Command failed: Error code 13 (Unauthorized): not authorized on admin");
        let status = doc! { "authInfo": { "authenticatedUsers": [{ "user": "khiman", "db": "admin" }], "authenticatedUserRoles": [{ "role": "atlasAdmin", "db": "admin" }] } };
        let me = self_users(&status);
        assert_eq!(me.len(), 1);
        assert_eq!(roles_text(&me[0]), "atlasAdmin@admin");
    }

    #[test]
    fn roles() {
        let r = parse_roles("readWrite@shop, read ,", "admin");
        assert_eq!(r.len(), 2);
        assert_eq!(r[1].as_document().unwrap().get_str("db").unwrap(), "admin");
        let u = doc! { "roles": [ { "role": "read", "db": "x" } ] };
        assert_eq!(roles_text(&u), "read@x");
    }
}
