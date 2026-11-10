//! The Users and privileges window of a SQL connection and its New user dialog. The statements and
//! their inverses come from `db::sql::users`; this file draws and routes the answers.

use super::*;
use db::sql::ddl::Mode;
use db::sql::users::{self, Account, Edit, Grant, Op, Scope, UserList, DB_PRIVS};

const NOTE_APPLY: &str = "Read-only: unlock writes to change privileges";
const NOTE_USER: &str = "Read-only: unlock writes to create a user";
const SOON: &str = "Coming soon";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum UTab {
    Account,
    Global,
    Databases,
    Limits,
}

/// One database of the Databases tab: what the server holds and what the boxes say now.
pub(super) struct GrantRow {
    /// The database as the server's grant writes it, `\_` for a literal underscore.
    pub(super) pattern: String,
    pub(super) orig: BTreeSet<String>,
    pub(super) now: BTreeSet<String>,
}

pub(super) struct SqlUsersWin {
    pub(super) conn: u64,
    pub(super) list_req: u64,
    pub(super) list: Option<db::Res<UserList>>,
    pub(super) filter: String,
    /// (user, host) of the account on show.
    pub(super) sel: Option<(String, String)>,
    pub(super) tab: UTab,
    pub(super) grants_req: u64,
    pub(super) grants: Option<db::Res<Vec<Grant>>>,
    pub(super) rows: Vec<GrantRow>,
    /// The name being typed for "+ Database".
    pub(super) adding: Option<String>,
    /// An Apply is running.
    pub(super) busy: bool,
}

impl SqlUsersWin {
    fn new(conn: u64) -> SqlUsersWin {
        SqlUsersWin { conn, list_req: 0, list: None, filter: String::new(), sel: None, tab: UTab::Databases, grants_req: 0, grants: None, rows: Vec::new(), adding: None, busy: false }
    }

    fn edits(&self) -> Vec<Edit> {
        self.rows.iter().map(|r| users::diff(&r.pattern, &r.orig, &r.now)).filter(|e| !e.is_empty()).collect()
    }

    fn account(&self) -> Option<Account> {
        let (u, h) = self.sel.as_ref()?;
        let found = self.list.as_ref().and_then(|l| l.as_ref().ok()).and_then(|l| l.accounts.iter().find(|a| &a.user == u && &a.host == h));
        Some(found.cloned().unwrap_or_else(|| Account::named(u, h)))
    }
}

pub(super) struct NewUser {
    pub(super) conn: u64,
    pub(super) user: String,
    pub(super) host: String,
    /// Typed here and handed to the worker by Apply; never shown, logged or kept after.
    pub(super) password: String,
}

enum Act {
    Pick(String, String),
    Apply,
    Close,
    NewUser,
    Drop(Account),
    Unlock,
    Refresh,
}

/// "8.4" from "8.4.3" or "11.4.5-MariaDB", the way the design's subtitle says it.
fn short_version(v: &str) -> String {
    v.split('-').next().unwrap_or_default().split('.').take(2).collect::<Vec<_>>().join(".")
}

fn sql_block(ui: &mut egui::Ui, text: &str) {
    egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.add(egui::Label::new(sql_job(text, TXT_BODY)).selectable(true));
    });
}

/// A tab that exists in the design's order but is built later: visible, not clickable.
fn soon_tab(ui: &mut egui::Ui, label: &str) {
    let g = ui.painter().layout_no_wrap(label.to_string(), FontId::proportional(13.0), tint(FAINT(), 150));
    let (rect, r) = ui.allocate_exact_size(egui::vec2(g.size().x, 30.0), Sense::hover());
    let dy = optical_dy(&g);
    ui.painter().galley(egui::pos2(rect.left(), rect.center().y - g.size().y / 2.0 + dy - 1.0), g, tint(FAINT(), 150));
    r.on_hover_text(SOON).on_hover_cursor(egui::CursorIcon::NotAllowed);
}

impl App {
    fn users_sql(&self, conn: u64) -> Option<db::sql::Db> {
        self.conn(conn).and_then(|c| c.sql.clone())
    }

    /// The sql_mode the connection read, for the previews; Apply quotes for the session's own.
    fn users_mode(&self, conn: u64) -> Mode {
        self.users_sql(conn).map_or(Mode::DEFAULT, |s| Mode::of(&s.sql_mode))
    }

    pub(super) fn open_sql_users(&mut self, conn: u64) {
        self.sql_users = Some(SqlUsersWin::new(conn));
        self.sql_users_reload();
    }

    /// Asks for the account list again; the grants of the chosen account follow when it arrives.
    fn sql_users_reload(&mut self) {
        let Some(conn) = self.sql_users.as_ref().map(|w| w.conn) else { return };
        let Some(sql) = self.users_sql(conn) else { return };
        self.req += 1;
        let req = self.req;
        if let Some(w) = &mut self.sql_users {
            w.list_req = req;
            w.list = None;
        }
        self.worker.sql_users(conn, req, sql);
    }

    fn sql_users_ask_grants(&mut self) {
        let Some((conn, acct)) = self.sql_users.as_ref().and_then(|w| w.account().map(|a| (w.conn, a))) else { return };
        let Some(sql) = self.users_sql(conn) else { return };
        self.req += 1;
        let req = self.req;
        if let Some(w) = &mut self.sql_users {
            w.grants_req = req;
            w.grants = None;
            w.rows.clear();
            w.adding = None;
        }
        self.worker.sql_user_grants(conn, req, sql, acct);
    }

    pub(super) fn sql_users_loaded(&mut self, conn: u64, req: u64, res: db::Res<UserList>) {
        let Some(w) = self.sql_users.as_mut().filter(|w| w.conn == conn && w.list_req == req) else { return };
        if let Ok(l) = &res {
            let has = |s: &Option<(String, String)>| s.as_ref().is_some_and(|(u, h)| l.accounts.iter().any(|a| &a.user == u && &a.host == h));
            if !has(&w.sel) {
                w.sel = l.current.clone().filter(|c| has(&Some(c.clone()))).or_else(|| l.accounts.first().map(|a| (a.user.clone(), a.host.clone())));
            }
        }
        w.list = Some(res);
        self.sql_users_ask_grants();
    }

    pub(super) fn sql_user_grants_loaded(&mut self, conn: u64, req: u64, res: db::Res<Vec<Grant>>) {
        let Some(w) = self.sql_users.as_mut().filter(|w| w.conn == conn && w.grants_req == req) else { return };
        w.rows = match &res {
            Ok(g) => {
                let mut rows: Vec<GrantRow> = users::db_grants(g).into_iter().map(|(pattern, s)| GrantRow { pattern, orig: s.clone(), now: s }).collect();
                rows.sort_by_key(|r| users::shown_pattern(&r.pattern).to_lowercase());
                rows
            }
            Err(_) => Vec::new(),
        };
        w.grants = Some(res);
    }

    /// An edit, create, drop or undo finished. The window reads the server again, so it shows what is there.
    pub(super) fn sql_user_done(&mut self, conn: u64, account: Account, op: Op, res: db::Res<String>, undo: Option<UndoAction>) {
        match res {
            Ok(s) => self.ok(s),
            Err(e) => self.err(e),
        }
        if let Some(u) = undo {
            self.push_undo(u);
        }
        let Some(w) = self.sql_users.as_mut().filter(|w| w.conn == conn) else { return };
        w.busy = false;
        match op {
            Op::Created => w.sel = Some((account.user.clone(), account.host.clone())),
            Op::Dropped if w.sel.as_ref() == Some(&(account.user.clone(), account.host.clone())) => w.sel = None,
            _ => {}
        }
        self.sql_users_reload();
    }

    pub(super) fn sql_new_user(&mut self, conn: u64) {
        if !self.conn(conn).is_some_and(|c| c.writable) {
            return self.err(NOTE_USER);
        }
        self.modal = Some(Modal::SqlNewUser(Box::new(NewUser { conn, user: String::new(), host: "%".into(), password: String::new() })));
    }

    pub(super) fn sql_user_drop_ask(&mut self, conn: u64, acct: Account) {
        let what = if acct.is_role { "role" } else { "user" };
        let title = format!("Drop the {what} {} on {}? Its password and every privilege go with it, and logins that use it stop working. This cannot be undone.", acct.label(), self.conn(conn).map(|c| c.meta.name.clone()).unwrap_or_default());
        self.modal = Some(Modal::Typed { title, expect: acct.label(), typed: String::new(), action: Danger::SqlDropUser { conn, user: acct.user, host: acct.host } });
    }

    pub(super) fn sql_user_drop_run(&mut self, conn: u64, user: String, host: String) {
        if self.demo {
            return self.ok("Offline demo: no database is connected.");
        }
        let Some(c) = self.conn(conn) else { return };
        if !c.writable {
            return self.modal = Some(Modal::Unlock(conn, None));
        }
        let Some(sql) = c.sql.clone() else { return self.err("Not connected.") };
        // Roles are told apart from users by the list the window holds, since DROP ROLE differs on MariaDB.
        let acct = self.sql_users.as_ref().filter(|w| w.conn == conn).and_then(|w| w.list.as_ref()).and_then(|l| l.as_ref().ok()).and_then(|l| l.accounts.iter().find(|a| a.user == user && a.host == host).cloned()).unwrap_or_else(|| Account::named(&user, &host));
        self.worker.sql_user_drop(conn, sql, acct, false);
    }

    fn sql_user_apply(&mut self) {
        let Some((conn, acct, edits)) = self.sql_users.as_ref().and_then(|w| Some((w.conn, w.account()?, w.edits()))) else { return };
        if edits.is_empty() {
            return;
        }
        if self.demo {
            return self.ok("Offline demo: no database is connected.");
        }
        let Some(c) = self.conn(conn) else { return };
        if !c.writable {
            return self.modal = Some(Modal::Unlock(conn, None));
        }
        let Some(sql) = c.sql.clone() else { return self.err("Not connected.") };
        if let Some(w) = &mut self.sql_users {
            w.busy = true;
        }
        self.worker.sql_user_edit(conn, sql, acct, edits, false);
    }

    pub(super) fn sql_users_window(&mut self, ctx: &egui::Context) {
        let Some(mut w) = self.sql_users.take() else { return };
        let conn = w.conn;
        let writable = self.conn(conn).is_some_and(|c| c.writable);
        let conn_name = self.conn(conn).map(|c| c.meta.name.clone()).unwrap_or_default();
        let product = self.conn(conn).map(|c| (c.server.product.clone(), short_version(&c.server.version))).filter(|(p, _)| !p.is_empty()).map_or(String::new(), |(p, v)| format!("{p} {v}"));
        let mode = self.users_mode(conn);
        // Databases the server has, for the "+ Database" picker; the server's own are left out.
        let known: Vec<String> = self.conn(conn).map(|c| c.dbs.iter().map(|d| d.0.clone()).filter(|d| !db::sql::create::SYSTEM_SCHEMAS.contains(&d.as_str())).collect()).unwrap_or_default();
        let mut acts: Vec<Act> = Vec::new();
        let screen = ctx.content_rect();
        let r = dialog(egui::Id::unique(("quarry-layer", "sql-users-window"))).show(ctx, |ui| {
            ui.set_width((screen.width() * 0.7).clamp(720.0, 1000.0));
            dialog_title(ui, "Users and privileges");
            ui.horizontal(|ui| {
                ui.label(RichText::new(if product.is_empty() { conn_name.clone() } else { format!("{conn_name} · {product}") }).color(MUTED()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Refresh").clicked() {
                        acts.push(Act::Refresh);
                    }
                });
            });
            if let Some(Ok(l)) = &w.list {
                if !l.whole {
                    let why = l.why.clone().unwrap_or_default();
                    ui.add(egui::Label::new(RichText::new("This user may not read the list of accounts (mysql.user), so only your own account is shown.").size(TXT_SMALL).color(WARNING())).wrap()).on_hover_text(why);
                }
            }
            ui.add_space(8.0);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(230.0);
                    self.users_list(ui, &mut w, writable, &mut acts);
                });
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width());
                    self.users_detail(ui, &mut w, writable, mode, &known, &mut acts);
                });
            });
            ui.add_space(6.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let can = w.tab == UTab::Databases && writable && !w.busy && !w.edits().is_empty() && users::preview(mode, &w.account().unwrap_or_default(), &w.edits()).is_ok();
                if ui.add_enabled(can, primary_button(ui, "Apply", PRIMARY())).on_disabled_hover_text(if !writable { NOTE_APPLY } else { "Nothing to apply" }).clicked() {
                    acts.push(Act::Apply);
                }
                if ui.button("Cancel").on_hover_text("Esc").clicked() {
                    acts.push(Act::Close);
                }
                if !writable {
                    if quiet(ui, "Unlock…").clicked() {
                        acts.push(Act::Unlock);
                    }
                    ui.label(RichText::new(NOTE_APPLY).size(TXT_SMALL).color(FAINT()));
                }
            });
        });
        let mut open = true;
        let mut pick = None;
        for a in acts {
            match a {
                Act::Pick(u, h) => pick = Some((u, h)),
                Act::Apply => {
                    self.sql_users = Some(w);
                    self.sql_user_apply();
                    w = self.sql_users.take().unwrap();
                }
                Act::Close => open = false,
                Act::NewUser => self.sql_new_user(conn),
                Act::Drop(a) => self.sql_user_drop_ask(conn, a),
                Act::Unlock => self.modal = Some(Modal::Unlock(conn, None)),
                Act::Refresh => {
                    self.sql_users = Some(w);
                    self.sql_users_reload();
                    w = self.sql_users.take().unwrap();
                }
            }
        }
        if !(open && !(r.should_close() && self.modal.is_none())) {
            return;
        }
        let picked = pick.is_some();
        if let Some(p) = pick {
            w.sel = Some(p);
        }
        self.sql_users = Some(w);
        if picked {
            self.sql_users_ask_grants();
        }
    }

    fn users_list(&self, ui: &mut egui::Ui, w: &mut SqlUsersWin, writable: bool, acts: &mut Vec<Act>) {
        ui.add(egui::TextEdit::singleline(&mut w.filter).hint_text("Filter users").desired_width(f32::INFINITY));
        ui.add_space(6.0);
        let find = w.filter.trim().to_lowercase();
        egui::ScrollArea::vertical().id_salt("sql-users-list").max_height(300.0).auto_shrink([false, true]).show(ui, |ui| match &w.list {
            None => {
                spin(ui);
            }
            Some(Err(e)) => {
                ui.add(egui::Label::new(RichText::new(format!("Could not list users: {e}")).color(ALERT())).wrap());
            }
            Some(Ok(l)) => {
                for a in l.accounts.iter().filter(|a| find.is_empty() || a.label().to_lowercase().contains(&find)) {
                    let active = w.sel.as_ref() == Some(&(a.user.clone(), a.host.clone()));
                    let dot = if a.locked { ALERT() } else if a.expired { WARNING() } else { SUCCESS() };
                    let r = side_row(ui, 42.0, 0.0, active, |p, r| {
                        p.circle_filled(egui::pos2(r.left() + 5.0, r.center().y), 4.0, dot);
                        p.text(egui::pos2(r.left() + 20.0, r.top() + 12.0), egui::Align2::LEFT_CENTER, &a.user, FontId::proportional(TXT_TITLE), if active { BRIGHT() } else { TEXT() });
                        let sub = if a.is_role { "role".to_string() } else { format!("@{}", a.host) };
                        p.text(egui::pos2(r.left() + 20.0, r.top() + 29.0), egui::Align2::LEFT_CENTER, sub, FontId::monospace(TXT_CAPTION), FAINT());
                    });
                    let state = [(a.locked, "locked"), (a.expired, "password expired")].iter().filter(|(on, _)| *on).map(|(_, t)| *t).collect::<Vec<_>>().join(", ");
                    let r = if state.is_empty() { r } else { r.on_hover_text(format!("{}: {state}", a.label())) };
                    if r.clicked() && !active {
                        acts.push(Act::Pick(a.user.clone(), a.host.clone()));
                    }
                    r.context_menu(|ui| {
                        menu_look(ui);
                        if ui.add_enabled_ui(writable, |ui| danger_item(ui, "Drop user…")).inner.on_disabled_hover_text("Read-only: unlock writes to drop a user").clicked() {
                            acts.push(Act::Drop(a.clone()));
                        }
                    });
                }
            }
        });
        ui.add_space(6.0);
        if ui.button("+ User").clicked() {
            acts.push(Act::NewUser);
        }
    }

    fn users_detail(&self, ui: &mut egui::Ui, w: &mut SqlUsersWin, writable: bool, mode: Mode, known: &[String], acts: &mut Vec<Act>) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            for (tab, label) in [(UTab::Account, "Account"), (UTab::Global, "Global"), (UTab::Databases, "Databases"), (UTab::Limits, "Limits")] {
                if tab == UTab::Databases {
                    if view_tab(ui, label, true).clicked() {
                        w.tab = tab;
                    }
                } else {
                    soon_tab(ui, label);
                }
            }
            if let Some(a) = w.account().filter(|_| w.list.is_some()) {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add_enabled_ui(writable, |ui| danger(ui, "Drop user…")).inner.on_disabled_hover_text("Read-only: unlock writes to drop a user").clicked() {
                        acts.push(Act::Drop(a));
                    }
                });
            }
        });
        ui.add_space(8.0);
        let Some(acct) = w.account() else {
            ui.label(RichText::new("No user chosen.").color(MUTED()));
            return;
        };
        match &w.grants {
            None => {
                spin(ui);
                return;
            }
            Some(Err(e)) => {
                ui.add(egui::Label::new(RichText::new(format!("Could not read the grants of {}: {e}", acct.label())).color(ALERT())).wrap());
                return;
            }
            Some(Ok(_)) => {}
        }
        self.privilege_table(ui, w);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("+ Database").clicked() && w.adding.is_none() {
                w.adding = Some(String::new());
            }
            if let Some(Ok(g)) = &w.grants {
                let other: Vec<&Grant> = g.iter().filter(|g| !matches!(g.scope, Scope::Db(_)) && !(g.scope == Scope::Global && g.privs.is_empty())).collect();
                if !other.is_empty() {
                    let n = |f: fn(&Scope) -> bool| other.iter().filter(|g| f(&g.scope)).count();
                    let parts: Vec<String> = [(n(|s| *s == Scope::Global), "global"), (n(|s| matches!(s, Scope::Object(_))), "table or routine"), (n(|s| *s == Scope::Role), "role"), (n(|s| *s == Scope::Other), "other")].iter().filter(|(c, _)| *c > 0).map(|(c, t)| format!("{c} {t}")).collect();
                    let text = other.iter().map(|g| g.raw.as_str()).collect::<Vec<_>>().join("\n");
                    ui.label(RichText::new(format!("Not edited here: {} grants", parts.join(", "))).size(TXT_SMALL).color(FAINT())).on_hover_text(text);
                }
            }
        });
        if let Some(text) = &mut w.adding {
            let mut add = false;
            let mut cancel = false;
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(text).hint_text("database name").font(egui::TextStyle::Monospace).desired_width(220.0));
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    add = true;
                }
                if !r.has_focus() && text.is_empty() {
                    r.request_focus();
                }
                let free: Vec<&String> = known.iter().filter(|d| !w.rows.iter().any(|r| r.pattern == users::pattern_of(d))).collect();
                if !free.is_empty() {
                    egui::ComboBox::from_id_salt("sql-users-known").width(160.0).selected_text("Existing database").show_ui(ui, |ui| {
                        menu_look(ui);
                        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                            for d in free {
                                if ui.selectable_label(false, d.as_str()).clicked() {
                                    *text = d.clone();
                                }
                            }
                        });
                    });
                }
                if ui.add_enabled(users::check_db_name(text).is_ok(), primary_button(ui, "Add", PRIMARY())).clicked() {
                    add = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
            if let Err(e) = users::check_db_name(text) {
                if !text.is_empty() {
                    ui.label(RichText::new(e).size(TXT_SMALL).color(ALERT()));
                }
            }
            if add && users::check_db_name(text).is_ok() {
                let pattern = users::pattern_of(text.trim());
                if !w.rows.iter().any(|r| r.pattern == pattern) {
                    w.rows.push(GrantRow { pattern, orig: BTreeSet::new(), now: BTreeSet::new() });
                }
                w.adding = None;
            } else if cancel {
                w.adding = None;
            }
        }
        ui.add_space(10.0);
        caption(ui, "GRANTS TO RUN");
        ui.add_space(4.0);
        let edits = w.edits();
        match users::preview(mode, &acct, &edits) {
            Ok(t) if t.is_empty() => sql_block(ui, "-- No changes yet. Tick a privilege to stage a GRANT or REVOKE."),
            Ok(t) => sql_block(ui, &t),
            Err(e) => {
                ui.label(RichText::new(e).size(TXT_SMALL).color(ALERT()));
            }
        }
        if w.busy {
            ui.horizontal(|ui| {
                spin(ui);
                ui.label(RichText::new("Applying…").color(MUTED()));
            });
        }
    }

    /// The database table of the design: a name column and a checkbox per privilege, sideways scrolling for the
    /// ones beyond the design's nine.
    fn privilege_table(&self, ui: &mut egui::Ui, w: &mut SqlUsersWin) {
        const ROW_H: f32 = 28.0;
        const NAME_W: f32 = 190.0;
        let widths: Vec<f32> = DB_PRIVS.iter().map(|p| ui.painter().layout_no_wrap(users::priv_title(p), FontId::proportional(TXT_SMALL), MUTED()).size().x.max(40.0) + 20.0).collect();
        let total = NAME_W + widths.iter().sum::<f32>();
        egui::Frame::new().fill(PANEL()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).show(ui, |ui| {
            egui::ScrollArea::horizontal().id_salt("sql-users-privs").auto_shrink([false, true]).show(ui, |ui| {
                ui.set_min_width(total);
                let (head, _) = ui.allocate_exact_size(egui::vec2(total.max(ui.available_width()), ROW_H), Sense::hover());
                let mut x = head.left() + 10.0;
                ui.painter().text(egui::pos2(x, head.center().y), egui::Align2::LEFT_CENTER, "Database", FontId::proportional(TXT_SMALL), MUTED());
                x = head.left() + NAME_W;
                for (p, cw) in DB_PRIVS.iter().zip(&widths) {
                    ui.painter().text(egui::pos2(x + cw / 2.0, head.center().y), egui::Align2::CENTER_CENTER, users::priv_title(p), FontId::proportional(TXT_SMALL), MUTED());
                    x += cw;
                }
                if w.rows.is_empty() {
                    ui.painter().hline(head.x_range(), head.bottom(), Stroke::new(1.0, LINE()));
                    let (empty, _) = ui.allocate_exact_size(egui::vec2(total, ROW_H + 8.0), Sense::hover());
                    ui.painter().text(egui::pos2(empty.left() + 10.0, empty.center().y), egui::Align2::LEFT_CENTER, "No database privileges. Use + Database to give some.", FontId::proportional(TXT_SMALL), FAINT());
                }
                for row in &mut w.rows {
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(total.max(ui.available_width()), ROW_H), Sense::hover());
                    ui.painter().hline(rect.x_range(), rect.top(), Stroke::new(1.0, LINE()));
                    if resp.hovered() {
                        ui.painter().rect_filled(rect.shrink2(egui::vec2(1.0, 0.0)), 0.0, HOVER());
                    }
                    let muted = row.now.is_empty();
                    let shown = users::shown_pattern(&row.pattern);
                    let name = ui.painter().text(egui::pos2(rect.left() + 10.0, rect.center().y), egui::Align2::LEFT_CENTER, &shown, FontId::monospace(TXT_BODY), if muted { FAINT() } else { TEXT() });
                    if shown != row.pattern && ui.rect_contains_pointer(name) {
                        resp.clone().on_hover_text(format!("Granted on {}", row.pattern));
                    }
                    let mut x = rect.left() + NAME_W;
                    for (p, cw) in DB_PRIVS.iter().zip(&widths) {
                        let mut on = row.now.contains(*p);
                        let cell = egui::Rect::from_center_size(egui::pos2(x + cw / 2.0, rect.center().y), egui::vec2(18.0, 18.0));
                        if ui.put(cell, egui::Checkbox::without_text(&mut on)).on_hover_text(users::priv_title(p)).changed() {
                            if on {
                                row.now.insert(p.to_string());
                            } else {
                                row.now.remove(*p);
                            }
                        }
                        x += cw;
                    }
                }
            });
        });
    }

    pub(super) fn sql_new_user_dialog(&mut self, ui: &mut egui::Ui, f: &mut NewUser, keep: &mut bool) {
        ui.set_width(520.0);
        let writable = self.conn(f.conn).is_some_and(|c| c.writable);
        dialog_title(ui, "New user");
        ui.label(RichText::new(self.conn(f.conn).map(|c| c.meta.name.clone()).unwrap_or_default()).color(MUTED()));
        ui.add_space(8.0);
        egui::Grid::new("newuser").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W + 14.0).show(ui, |ui| {
            form_label(ui, "Name");
            let r = ui.add(egui::TextEdit::singleline(&mut f.user).font(egui::TextStyle::Monospace).desired_width(340.0));
            if !r.has_focus() && f.user.is_empty() && f.password.is_empty() {
                r.request_focus();
            }
            ui.end_row();
            form_label(ui, "Host");
            ui.add(egui::TextEdit::singleline(&mut f.host).font(egui::TextStyle::Monospace).hint_text("% for any host").desired_width(340.0));
            ui.end_row();
            form_label(ui, "Password");
            ui.add(egui::TextEdit::singleline(&mut f.password).password(true).desired_width(340.0));
            ui.end_row();
        });
        ui.add_space(8.0);
        let acct = Account::named(&f.user, &f.host);
        let check = users::check_new_user(&f.user, &f.host, &f.password);
        // The password never reaches the preview.
        sql_block(ui, &format!("{};", users::create_user_sql(self.users_mode(f.conn), &acct, &f.password, true)));
        if let Err(e) = &check {
            if !f.user.is_empty() || !f.password.is_empty() {
                ui.label(RichText::new(e.as_str()).size(TXT_SMALL).color(ALERT()));
            }
        }
        ui.add_space(4.0);
        ui.add(egui::Label::new(RichText::new("The account starts with no privileges: choose them on the Databases tab afterwards. The password is sent once and not kept by Quarry.").size(TXT_SMALL).color(MUTED())).wrap());
        if !writable {
            ui.label(RichText::new(NOTE_USER).size(TXT_SMALL).color(FAINT()));
        }
        ui.add_space(8.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add_enabled(writable && check.is_ok(), primary_button(ui, "Create", PRIMARY())).clicked() {
                self.sql_user_create_run(f);
                *keep = false;
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
        });
    }

    fn sql_user_create_run(&mut self, f: &mut NewUser) {
        // Taken out of the form either way, so no copy stays in app state once Create was pressed.
        let password = std::mem::take(&mut f.password);
        if self.demo {
            return self.ok("Offline demo: no database is connected.");
        }
        let Some(sql) = self.users_sql(f.conn) else { return };
        self.worker.sql_user_create(f.conn, sql, Account::named(&f.user, &f.host), password, false);
    }

    /// QUARRY_TEST_SQL=users (the window on user "report") or newuser (the + User dialog).
    pub(super) fn sql_users_demo(&mut self, which: &str) {
        if !matches!(which, "users" | "newuser") {
            return;
        }
        if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == 3) {
            c.writable = true;
        }
        if which == "newuser" {
            return self.modal = Some(Modal::SqlNewUser(Box::new(NewUser { conn: 3, user: "reporting".into(), host: "10.0.%".into(), password: "demo".into() })));
        }
        let acct = |u: &str, h: &str| Account { user: u.into(), host: h.into(), plugin: "caching_sha2_password".into(), ..Default::default() };
        let accounts = vec![acct("app", "%"), acct("report", "10.0.%"), acct("backup", "localhost"), Account { expired: true, ..acct("migrate", "%") }, Account { locked: true, ..acct("root", "localhost") }];
        let set = |p: &[&str]| p.iter().map(|s| s.to_string()).collect::<BTreeSet<String>>();
        let mut w = SqlUsersWin::new(3);
        w.list = Some(Ok(UserList { accounts, current: Some(("app".into(), "%".into())), whole: true, why: None }));
        w.sel = Some(("report".into(), "10.0.%".into()));
        w.grants = Some(Ok(Vec::new()));
        w.rows = vec![
            GrantRow { pattern: "shop".into(), orig: BTreeSet::new(), now: set(&["SELECT"]) },
            GrantRow { pattern: "analytics".into(), orig: BTreeSet::new(), now: set(&["SELECT", "EXECUTE"]) },
            GrantRow { pattern: "sys".into(), orig: BTreeSet::new(), now: BTreeSet::new() },
        ];
        self.sql_users = Some(w);
    }
}
