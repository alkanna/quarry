//! The Users and privileges window of a SQL connection, its New user and Change password dialogs. The
//! statements and their inverses come from `db::sql::users`; this file draws and routes the answers.

use super::*;
use db::sql::ddl::Mode;
use db::sql::users::{self, Account, AccountInfo, AccountPlan, Caps, Edit, Expire, Grant, Loaded, Op, Scope, Settings, Tls, UserList, DB_PRIVS};
use db::sql::Dialect;

const NOTE_APPLY: &str = "Read-only: unlock writes to change this account";
const NOTE_USER: &str = "Read-only: unlock writes to create a user";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum UTab {
    Account,
    Global,
    Databases,
    Limits,
}

/// How the password lifetime is chosen; "After N days" has its number in a field beside it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ExpKind {
    Never,
    Default,
    Days,
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
    /// The label of each account of `list` in lowercase, for the filter box.
    search: Vec<String>,
    pub(super) filter: String,
    /// (user, host) of the account on show.
    pub(super) sel: Option<(String, String)>,
    pub(super) tab: UTab,
    pub(super) grants_req: u64,
    pub(super) grants: Option<db::Res<Vec<Grant>>>,
    pub(super) info: Option<AccountInfo>,
    /// Why the login settings are not shown.
    pub(super) info_err: Option<String>,
    pub(super) rows: Vec<GrantRow>,
    /// The name being typed for "+ Database".
    pub(super) adding: Option<String>,
    /// An Apply is running.
    pub(super) busy: bool,
    // Account tab.
    pub(super) name: String,
    pub(super) host: String,
    pub(super) locked: bool,
    pub(super) exp: ExpKind,
    pub(super) days: String,
    pub(super) tls: Tls,
    pub(super) roles_orig: Vec<String>,
    pub(super) roles_now: Vec<String>,
    pub(super) role_add: Option<String>,
    // Global tab.
    pub(super) g_orig: BTreeSet<String>,
    pub(super) g_now: BTreeSet<String>,
    pub(super) priv_add: Option<String>,
    /// Dynamic privileges held WITH GRANT OPTION, which this window does not revoke.
    pub(super) dyn_grant: BTreeSet<String>,
    // Limits tab, as typed.
    pub(super) limits: [String; 4],
}

impl SqlUsersWin {
    fn new(conn: u64) -> SqlUsersWin {
        SqlUsersWin {
            conn,
            list_req: 0,
            list: None,
            search: Vec::new(),
            filter: String::new(),
            sel: None,
            tab: UTab::Databases,
            grants_req: 0,
            grants: None,
            info: None,
            info_err: None,
            rows: Vec::new(),
            adding: None,
            busy: false,
            name: String::new(),
            host: String::new(),
            locked: false,
            exp: ExpKind::Default,
            days: "90".into(),
            tls: Tls::None,
            roles_orig: Vec::new(),
            roles_now: Vec::new(),
            role_add: None,
            g_orig: BTreeSet::new(),
            g_now: BTreeSet::new(),
            priv_add: None,
            dyn_grant: BTreeSet::new(),
            limits: Default::default(),
        }
    }

    fn set_list(&mut self, res: db::Res<UserList>) {
        self.search = res.as_ref().map(|l| l.accounts.iter().map(|a| a.label().to_lowercase()).collect()).unwrap_or_default();
        self.list = Some(res);
    }

    fn edits(&self) -> Vec<Edit> {
        self.rows.iter().map(|r| users::diff(&r.pattern, &r.orig, &r.now)).filter(|e| !e.is_empty()).collect()
    }

    fn global_edits(&self) -> Vec<Edit> {
        Some(users::diff("", &self.g_orig, &self.g_now)).filter(|e| !e.is_empty()).into_iter().collect()
    }

    fn account(&self) -> Option<Account> {
        let (u, h) = self.sel.as_ref()?;
        let found = self.list.as_ref().and_then(|l| l.as_ref().ok()).and_then(|l| l.accounts.iter().find(|a| &a.user == u && &a.host == h));
        Some(found.cloned().unwrap_or_else(|| Account::named(u, h)))
    }

    /// The fields of the Account tab from the server's values, and the other tabs' too.
    fn fill(&mut self, acct: &Account, l: &Loaded) {
        self.rows = users::db_grants(&l.grants).into_iter().map(|(pattern, s)| GrantRow { pattern, orig: s.clone(), now: s }).collect();
        self.rows.sort_by_key(|r| users::shown_pattern(&r.pattern).to_lowercase());
        self.adding = None;
        self.role_add = None;
        self.priv_add = None;
        self.g_orig = users::global_privs(&l.grants);
        self.g_now = self.g_orig.clone();
        self.dyn_grant = users::dynamic_with_grant(&l.grants);
        self.roles_orig = users::roles_of(&l.grants);
        self.roles_now = self.roles_orig.clone();
        self.name = acct.user.clone();
        self.host = acct.host.clone();
        self.info = l.info.clone();
        self.info_err = l.info_err.clone();
        let s = l.info.as_ref().map(|i| i.settings.clone()).unwrap_or_default();
        self.locked = s.locked;
        (self.exp, self.days) = match s.expire {
            Expire::Never => (ExpKind::Never, "90".into()),
            Expire::Default => (ExpKind::Default, "90".into()),
            Expire::Days(n) => (ExpKind::Days, n.to_string()),
        };
        self.tls = s.tls;
        self.limits = s.limits.map(|n| n.to_string());
    }

    fn orig_settings(&self) -> Settings {
        self.info.as_ref().map(|i| i.settings.clone()).unwrap_or_default()
    }

    /// The Account tab's changes: a rename, the lock, lifetime and TLS, and roles.
    fn account_plan(&self, acct: &Account) -> Result<AccountPlan, String> {
        let orig = self.orig_settings();
        let expire = match self.exp {
            ExpKind::Never => Expire::Never,
            ExpKind::Default => Expire::Default,
            ExpKind::Days => Expire::Days(self.days.trim().parse().map_err(|_| "Days is a whole number.".to_string())?),
        };
        // The requirement's values stay only while the requirement is the one that was read.
        let spec = if self.tls == Tls::Specified { orig.spec.clone() } else { Default::default() };
        let now = Settings { locked: self.locked, expire, tls: self.tls, spec, limits: orig.limits };
        let rename = ((self.name.as_str(), self.host.as_str()) != (acct.user.as_str(), acct.host.as_str())).then(|| Account::named(&self.name, &self.host));
        let diff = |a: &[String], b: &[String]| a.iter().filter(|r| !b.contains(r)).cloned().collect::<Vec<_>>();
        Ok(AccountPlan {
            rename,
            settings: self.info.is_some().then_some((orig, now)),
            roles_grant: diff(&self.roles_now, &self.roles_orig),
            roles_revoke: diff(&self.roles_orig, &self.roles_now),
        })
    }

    /// The Limits tab's changes.
    fn limits_plan(&self) -> Result<AccountPlan, String> {
        let orig = self.orig_settings();
        let mut now = orig.clone();
        for (i, t) in self.limits.iter().enumerate() {
            now.limits[i] = t.trim().parse().map_err(|_| "A limit is a whole number, 0 for none.".to_string())?;
        }
        Ok(AccountPlan { settings: self.info.is_some().then_some((orig, now)), ..Default::default() })
    }

    /// Whether a tab holds staged changes, for the mark on its title.
    fn staged(&self, tab: UTab, acct: &Account) -> bool {
        match tab {
            UTab::Databases => !self.edits().is_empty(),
            UTab::Global => !self.global_edits().is_empty(),
            UTab::Account => self.account_plan(acct).map_or(true, |p| !p.is_empty()),
            UTab::Limits => self.limits_plan().map_or(true, |p| !p.is_empty()),
        }
    }
}

pub(super) struct NewUser {
    pub(super) conn: u64,
    pub(super) user: String,
    pub(super) host: String,
    /// Typed here and handed to the worker by Create; never shown, logged or kept after.
    pub(super) password: String,
    /// Makes the password field's id new for each dialog, so the text editor's undo history of an earlier one is out of reach.
    pub(super) serial: u64,
}

pub(super) struct PwForm {
    pub(super) conn: u64,
    pub(super) acct: Account,
    pub(super) plugin: String,
    /// The plugin the account has now, for the plugins on offer.
    pub(super) current: String,
    /// Why the password cannot be changed from here (MariaDB accounts with another way in).
    pub(super) blocked: Option<String>,
    pub(super) password: String,
    pub(super) again: String,
    pub(super) serial: u64,
}

enum Act {
    Pick(String, String),
    Apply,
    Close,
    NewUser,
    Drop(Account),
    Password(Account),
    Unlock,
    Refresh,
}

/// What the tabs need to know about the server and connection.
struct Env {
    writable: bool,
    mode: Mode,
    dialect: Dialect,
    caps: Caps,
    /// Databases for "+ Database".
    known: Vec<String>,
    /// Existing roles for "+ Role" (MariaDB lists them; MySQL roles are plain accounts).
    roles: Vec<String>,
}

/// Statements of a plan that has something to run; changes to settings need ALTER USER, a rename and roles do not.
fn plan_ready(p: &AccountPlan, env: &Env, acct: &Account) -> Result<(), String> {
    if p.is_empty() {
        return Err(String::new());
    }
    if !env.caps.alter_user && p.settings.as_ref().is_some_and(|(a, b)| a != b) {
        return Err("This server version cannot change these settings with ALTER USER.".into());
    }
    users::plan_sql(env.mode, env.dialect, acct, p).map(|_| ())
}

/// The preview of a plan, or why it cannot run; an empty plan previews as nothing.
fn plan_ready_or_empty(p: &AccountPlan, env: &Env, acct: &Account) -> Result<String, String> {
    if !env.caps.alter_user && p.settings.as_ref().is_some_and(|(a, b)| a != b) {
        return Err("This server version cannot change these settings with ALTER USER.".into());
    }
    users::plan_preview(env.mode, env.dialect, acct, p)
}

/// The grants the window last read, or none.
fn held_grants(w: &SqlUsersWin) -> &[Grant] {
    match &w.grants {
        Some(Ok(g)) => g,
        _ => &[],
    }
}

/// The id of a password field. egui keeps the cursor and an undo history of each text field under its id, in memory
/// that outlives the dialog, so a password field gets an id of its own for each dialog.
pub(super) fn secret_id(what: &str, serial: u64, n: u8) -> egui::Id {
    egui::Id::unique(("quarry-secret", what, serial, n))
}

/// Drops what egui remembers of the password fields of one dialog.
pub(super) fn forget_secret(ctx: &egui::Context, what: &str, serial: u64, fields: u8) {
    ctx.data_mut(|d| {
        for n in 0..fields {
            d.remove::<egui::text_edit::TextEditState>(secret_id(what, serial, n));
        }
    });
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

/// A preview block that says why there is none: nothing staged, or a field that is not valid yet.
fn preview_block(ui: &mut egui::Ui, text: Result<String, String>) {
    match text {
        Ok(t) if t.is_empty() => sql_block(ui, "-- No changes yet."),
        Ok(t) => sql_block(ui, &t),
        Err(e) => {
            sql_block(ui, "-- Not valid yet.");
            ui.label(RichText::new(e).size(TXT_SMALL).color(ALERT()));
        }
    }
}

impl App {
    fn users_sql(&self, conn: u64) -> Option<db::sql::Db> {
        self.conn(conn).and_then(|c| c.sql.clone())
    }

    fn users_dialect(&self, conn: u64) -> Dialect {
        self.users_sql(conn).map_or(Dialect::MySql, |s| s.dialect)
    }

    /// The demo has no server, so it assumes a current MySQL.
    fn users_caps(&self, conn: u64) -> Caps {
        self.users_sql(conn).map_or_else(|| Caps::of(Dialect::MySql, "8.4.3"), |s| Caps::of(s.dialect, &s.version))
    }

    pub(super) fn open_sql_users(&mut self, conn: u64) {
        if self.users_dialect(conn) == Dialect::Postgres || self.conn(conn).is_some_and(|c| c.meta.engine == db::sql::Engine::Postgres) {
            return self.open_pg_roles(conn);
        }
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
        w.set_list(res);
        self.sql_users_ask_grants();
    }

    pub(super) fn sql_user_grants_loaded(&mut self, conn: u64, req: u64, res: db::Res<Loaded>) {
        let Some(w) = self.sql_users.as_mut().filter(|w| w.conn == conn && w.grants_req == req) else { return };
        match res {
            Ok(l) => {
                if let Some(acct) = w.account() {
                    w.fill(&acct, &l);
                }
                w.grants = Some(Ok(l.grants));
            }
            Err(e) => {
                w.rows.clear();
                w.grants = Some(Err(e));
            }
        }
    }

    /// An edit, create, drop, password change or undo finished. The window reads the server again, so it shows what is there.
    pub(super) fn sql_user_done(&mut self, conn: u64, account: Account, op: Op, res: db::Res<String>, undo: Vec<UndoAction>) {
        match res {
            Ok(s) => self.ok(s),
            Err(e) => self.err(e),
        }
        // Oldest first, so the newest ends on top of Recent changes.
        for u in undo {
            self.push_undo(u);
        }
        let Some(w) = self.sql_users.as_mut().filter(|w| w.conn == conn) else { return };
        // An undo from Recent changes is not the window's Apply, so it does not end the Apply that may be running.
        if !matches!(op, Op::Undone | Op::UndoneRename) {
            w.busy = false;
        }
        match op {
            Op::Created | Op::Renamed | Op::UndoneRename => w.sel = Some((account.user.clone(), account.host.clone())),
            Op::Dropped if w.sel.as_ref() == Some(&(account.user.clone(), account.host.clone())) => w.sel = None,
            _ => {}
        }
        // What the boxes show was applied or undone: until the server is read again nothing may be applied a second time.
        w.grants = None;
        w.rows.clear();
        w.adding = None;
        self.sql_users_reload();
    }

    pub(super) fn sql_new_user(&mut self, conn: u64) {
        if !self.conn_writable(conn) {
            return self.warn(NOTE_USER);
        }
        self.req += 1;
        self.modal = Some(Modal::SqlNewUser(Box::new(NewUser { conn, user: String::new(), host: "%".into(), password: String::new(), serial: self.req })));
    }

    pub(super) fn sql_user_drop_ask(&mut self, conn: u64, acct: Account) {
        let what = if acct.is_role { "role" } else { "user" };
        let title = format!("Drop the {what} {} on {}? Its password and every privilege go with it, and logins that use it stop working. This cannot be undone.", acct.label(), self.conn(conn).map(|c| c.meta.name.clone()).unwrap_or_default());
        self.modal = Some(Modal::Typed { title, expect: acct.label(), typed: String::new(), action: Danger::SqlDropUser { conn, acct } });
    }

    pub(super) fn sql_user_drop_run(&mut self, conn: u64, acct: Account) {
        if self.demo {
            return self.ok("Offline demo: no database is connected.");
        }
        let Some(c) = self.conn(conn) else { return };
        if !c.writable {
            return self.modal = Some(Modal::Unlock(conn, None));
        }
        let Some(sql) = c.sql.clone() else { return self.warn_offline() };
        self.worker.sql_user_drop(conn, sql, acct, false);
    }

    /// Runs what the open tab has staged.
    fn sql_user_apply(&mut self) {
        let Some((conn, acct, tab)) = self.sql_users.as_ref().and_then(|w| Some((w.conn, w.account()?, w.tab))) else { return };
        if self.demo {
            return self.ok("Offline demo: no database is connected.");
        }
        let Some(c) = self.conn(conn) else { return };
        if !c.writable {
            return self.modal = Some(Modal::Unlock(conn, None));
        }
        let Some(sql) = c.sql.clone() else { return self.warn_offline() };
        let Some(w) = self.sql_users.as_mut() else { return };
        let plan = match tab {
            UTab::Databases | UTab::Global => None,
            UTab::Account => w.account_plan(&acct).ok(),
            UTab::Limits => w.limits_plan().ok(),
        };
        let edits = match tab {
            UTab::Databases => w.edits(),
            UTab::Global => w.global_edits(),
            _ => Vec::new(),
        };
        if edits.is_empty() && plan.as_ref().is_none_or(|p| p.is_empty()) {
            return;
        }
        w.busy = true;
        match plan {
            Some(p) => self.worker.sql_user_account(conn, sql, acct, p, false),
            None => self.worker.sql_user_edit(conn, sql, acct, edits, false),
        }
    }

    pub(super) fn sql_users_window(&mut self, ctx: &egui::Context) {
        let Some(mut w) = self.sql_users.take() else { return };
        let conn = w.conn;
        let writable = self.conn_writable(conn);
        let conn_name = self.conn(conn).map(|c| c.meta.name.clone()).unwrap_or_default();
        let product = self.conn(conn).map(|c| (c.server.product.clone(), short_version(&c.server.version))).filter(|(p, _)| !p.is_empty()).map_or(String::new(), |(p, v)| format!("{p} {v}"));
        let dialect = self.users_dialect(conn);
        let env = Env {
            writable,
            mode: self.sql_mode(conn),
            dialect,
            caps: self.users_caps(conn),
            // The server's own databases are left out.
            known: self.conn(conn).map(|c| c.dbs.iter().map(|d| d.0.clone()).filter(|d| !db::sql::create::SYSTEM_SCHEMAS.contains(&d.as_str())).collect()).unwrap_or_default(),
            roles: w.list.as_ref().and_then(|l| l.as_ref().ok()).map(|l| l.accounts.iter().filter(|a| a.is_role).map(|a| a.user.clone()).collect()).unwrap_or_default(),
        };
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
                    self.users_detail(ui, &mut w, &env, &mut acts);
                });
            });
            ui.add_space(6.0);
            let acct = w.account();
            let can = self.users_can_apply(&w, &env);
            ui.horizontal(|ui| {
                // Dropping sits at the left of the Account tab's footer, as in the design.
                if w.tab == UTab::Account {
                    if let Some(a) = acct.clone().filter(|_| w.list.is_some()) {
                        if ui.add_enabled_ui(writable, |ui| danger(ui, "Drop user…")).inner.on_disabled_hover_text("Read-only: unlock writes to drop a user").clicked() {
                            acts.push(Act::Drop(a));
                        }
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                Act::Password(a) => {
                    if writable {
                        let current = w.info.as_ref().map(|i| i.plugin.clone()).unwrap_or_default();
                        // MariaDB's SET PASSWORD sets the mysql_native_password part only, so an account that logs in another way is left alone.
                        let blocked = w.info.as_ref().filter(|_| matches!(dialect, Dialect::MariaDb)).and_then(|i| (i.chain || (!i.plugin.is_empty() && i.plugin != "mysql_native_password")).then(|| format!("{} logs in with {}; Quarry only sets a mysql_native_password password on MariaDB. Change it in a SQL shell.", a.label(), if i.chain { "more than one method".to_string() } else { i.plugin.clone() })));
                        self.req += 1;
                        self.modal = Some(Modal::SqlPassword(Box::new(PwForm { conn, acct: a, plugin: String::new(), current, blocked, password: String::new(), again: String::new(), serial: self.req })));
                    } else {
                        self.modal = Some(Modal::Unlock(conn, None));
                    }
                }
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

    /// Whether the open tab has something valid to run on a connection that allows writes.
    fn users_can_apply(&self, w: &SqlUsersWin, env: &Env) -> bool {
        let Some(acct) = w.account() else { return false };
        if !env.writable || w.busy || !matches!(w.grants, Some(Ok(_))) {
            return false;
        }
        match w.tab {
            UTab::Databases => !w.edits().is_empty() && users::preview(env.mode, &acct, &w.edits()).is_ok(),
            UTab::Global => !w.global_edits().is_empty() && users::preview_checked(env.mode, &acct, &w.global_edits(), held_grants(w)).is_ok(),
            UTab::Account => w.account_plan(&acct).and_then(|p| plan_ready(&p, env, &acct)).is_ok(),
            UTab::Limits => w.limits_plan().and_then(|p| plan_ready(&p, env, &acct)).is_ok(),
        }
    }

    fn users_list(&self, ui: &mut egui::Ui, w: &mut SqlUsersWin, writable: bool, acts: &mut Vec<Act>) {
        ui.add(egui::TextEdit::singleline(&mut w.filter).hint_text("Filter users").desired_width(f32::INFINITY));
        ui.add_space(6.0);
        let find = w.filter.trim().to_lowercase();
        let shown: Vec<&Account> = match &w.list {
            Some(Ok(l)) => l.accounts.iter().enumerate().filter(|(i, _)| find.is_empty() || w.search.get(*i).is_some_and(|s| s.contains(&find))).map(|(_, a)| a).collect(),
            _ => Vec::new(),
        };
        let area = egui::ScrollArea::vertical().id_salt("sql-users-list").max_height(300.0).auto_shrink([false, true]);
        match &w.list {
            Some(Ok(_)) => {
                // Every row has the same height, so only the visible ones are laid out.
                area.show_rows(ui, 42.0, shown.len(), |ui, range| {
                    for a in &shown[range] {
                        let active = w.sel.as_ref() == Some(&(a.user.clone(), a.host.clone()));
                        let dot = if a.locked { ALERT() } else if a.expired { WARNING() } else { SUCCESS() };
                        let r = side_row(ui, 42.0, 0.0, active, &a.user, |p, r| {
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
                                acts.push(Act::Drop((*a).clone()));
                            }
                        });
                    }
                });
            }
            other => {
                area.show(ui, |ui| match other {
                    Some(Err(e)) => {
                        ui.add(egui::Label::new(RichText::new(format!("Could not list users: {e}")).color(ALERT())).wrap());
                    }
                    _ => {
                        spin(ui);
                    }
                });
            }
        }
        ui.add_space(6.0);
        if ui.button("+ User").clicked() {
            acts.push(Act::NewUser);
        }
    }

    fn users_detail(&self, ui: &mut egui::Ui, w: &mut SqlUsersWin, env: &Env, acts: &mut Vec<Act>) {
        let acct = w.account();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            for (tab, label) in [(UTab::Account, "Account"), (UTab::Global, "Global"), (UTab::Databases, "Databases"), (UTab::Limits, "Limits")] {
                // A staged change is marked on its tab, since Apply only runs the open one.
                let staged = acct.as_ref().is_some_and(|a| w.grants.is_some() && w.staged(tab, a));
                let r = view_tab(ui, &if staged { format!("{label} •") } else { label.to_string() }, w.tab == tab);
                if r.clicked() {
                    w.tab = tab;
                }
                if staged {
                    r.on_hover_text("Has changes that are not applied yet");
                }
            }
        });
        ui.add_space(8.0);
        let Some(acct) = acct else {
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
        match w.tab {
            UTab::Account => self.tab_account(ui, w, env, &acct, acts),
            UTab::Global => self.tab_global(ui, w, env, &acct),
            UTab::Databases => self.tab_databases(ui, w, env, &acct),
            UTab::Limits => self.tab_limits(ui, w, env, &acct),
        }
        if w.busy {
            ui.horizontal(|ui| {
                spin(ui);
                ui.label(RichText::new("Applying…").color(MUTED()));
            });
        }
    }

    fn tab_account(&self, ui: &mut egui::Ui, w: &mut SqlUsersWin, env: &Env, acct: &Account, acts: &mut Vec<Act>) {
        let editable = w.info.is_some() && env.caps.alter_user;
        if let Some(why) = w.info_err.as_ref().filter(|_| w.info.is_none()) {
            ui.add(egui::Label::new(RichText::new(format!("The login settings cannot be read: {why}")).size(TXT_SMALL).color(WARNING())).wrap());
        } else if !env.caps.alter_user {
            ui.label(RichText::new("This server version cannot change these settings with ALTER USER.").size(TXT_SMALL).color(WARNING()));
        }
        caption(ui, "LOGIN");
        ui.add_space(4.0);
        egui::Grid::new("acc-login").num_columns(3).spacing([14.0, 10.0]).min_col_width(170.0).show(ui, |ui| {
            form_label(ui, "User name");
            ui.add_enabled(!acct.is_role, egui::TextEdit::singleline(&mut w.name).font(egui::TextStyle::Monospace).desired_width(240.0));
            ui.end_row();
            form_label(ui, "Host");
            ui.add_enabled(!acct.is_role, egui::TextEdit::singleline(&mut w.host).font(egui::TextStyle::Monospace).desired_width(240.0));
            ui.label(RichText::new("% is any host").size(TXT_SMALL).color(FAINT()));
            ui.end_row();
            form_label(ui, "Authentication plugin");
            // Changing the plugin needs a new password, so it is chosen in the Change password dialog.
            let plugin = w.info.as_ref().map(|i| i.plugin.clone()).unwrap_or_default();
            ui.add_enabled(false, egui::TextEdit::singleline(&mut plugin.clone()).font(egui::TextStyle::Monospace).desired_width(240.0)).on_disabled_hover_text(if env.caps.plugin_choice { "Pick another plugin in Change password" } else { "The plugin of this account" });
            ui.end_row();
            form_label(ui, "Password");
            ui.horizontal(|ui| {
                if ui.add_enabled(!acct.is_role && env.caps.alter_user, egui::Button::new("Change password…")).on_disabled_hover_text(if env.writable { "Not available for this account" } else { NOTE_APPLY }).clicked() {
                    acts.push(Act::Password(acct.clone()));
                }
                ui.label(RichText::new("A new password cannot be undone").size(TXT_SMALL).color(FAINT()));
            });
            ui.end_row();
        });
        ui.add_space(10.0);
        caption(ui, "ACCESS");
        ui.add_space(4.0);
        ui.add_enabled_ui(editable, |ui| {
            egui::Grid::new("acc-access").num_columns(2).spacing([14.0, 10.0]).min_col_width(170.0).show(ui, |ui| {
                form_label(ui, "Account locked");
                ui.add_enabled(env.caps.lock, egui::Checkbox::new(&mut w.locked, "Refuse new logins for this account")).on_disabled_hover_text("This server version has no ACCOUNT LOCK");
                ui.end_row();
                form_label(ui, "Password expires");
                ui.add_enabled_ui(env.caps.lifetime, |ui| {
                    ui.horizontal(|ui| {
                        segmented(ui, &mut w.exp, &[(ExpKind::Never, "Never"), (ExpKind::Default, "Server default"), (ExpKind::Days, "After N days")]);
                        if w.exp == ExpKind::Days {
                            ui.add(egui::TextEdit::singleline(&mut w.days).font(egui::TextStyle::Monospace).desired_width(60.0));
                            ui.label(RichText::new("days").size(TXT_SMALL).color(FAINT()));
                        }
                    });
                });
                ui.end_row();
                form_label(ui, "Require TLS");
                ui.horizontal(|ui| {
                    segmented(ui, &mut w.tls, &[(Tls::None, "None"), (Tls::Ssl, "TLS"), (Tls::X509, "X509")]);
                    if w.tls == Tls::Specified {
                        ui.label(RichText::new("A cipher or certificate requirement is set; pick one to replace it").size(TXT_SMALL).color(FAINT()));
                    }
                });
                ui.end_row();
            });
        });
        // Roles are GRANT and REVOKE, which older servers have without ALTER USER.
        if env.caps.roles && !acct.is_role {
            egui::Grid::new("acc-roles").num_columns(2).spacing([14.0, 10.0]).min_col_width(170.0).show(ui, |ui| {
                form_label(ui, "Roles");
                self.role_chips(ui, w, env);
                ui.end_row();
            });
        }
        ui.add_space(10.0);
        caption(ui, "STATEMENTS TO RUN");
        ui.add_space(4.0);
        preview_block(ui, w.account_plan(acct).and_then(|p| plan_ready_or_empty(&p, env, acct)));
    }

    fn role_chips(&self, ui: &mut egui::Ui, w: &mut SqlUsersWin, env: &Env) {
        ui.horizontal_wrapped(|ui| {
            let mut all: Vec<String> = w.roles_orig.clone();
            for r in &w.roles_now {
                if !all.contains(r) {
                    all.push(r.clone());
                }
            }
            for r in all {
                let on = w.roles_now.contains(&r);
                let hint = if !on { "Will be taken away; click to keep" } else if w.roles_orig.contains(&r) { "Click to take this role away" } else { "Will be given; click to cancel" };
                if chip(ui, &users::role_shown(&r), PRIMARY(), on).on_hover_text(hint).clicked() {
                    if on {
                        w.roles_now.retain(|x| *x != r);
                    } else {
                        w.roles_now.push(r);
                    }
                }
            }
            if w.role_add.is_none() {
                if ui.button("+ Role").clicked() {
                    w.role_add = Some(String::new());
                }
                return;
            }
            let mut add = false;
            let mut cancel = false;
            let text = w.role_add.as_mut().unwrap();
            let r = ui.add(egui::TextEdit::singleline(text).hint_text("role name").font(egui::TextStyle::Monospace).desired_width(140.0));
            if !r.has_focus() && text.is_empty() {
                r.request_focus();
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                add = true;
            }
            if !env.roles.is_empty() {
                egui::ComboBox::from_id_salt("sql-users-roles").width(120.0).selected_text("Existing").show_ui(ui, |ui| {
                    menu_look(ui);
                    for n in &env.roles {
                        if ui.selectable_label(false, n.as_str()).clicked() {
                            *text = n.clone();
                        }
                    }
                });
            }
            if ui.add_enabled(!text.trim().is_empty(), primary_button(ui, "Add", PRIMARY())).clicked() {
                add = true;
            }
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
            if add && !text.trim().is_empty() {
                // MySQL names a role with its host, % when none is given.
                let name = text.trim().to_string();
                let role = if matches!(env.dialect, Dialect::MySql) && !name.contains('@') { format!("{name}@%") } else { name };
                if !w.roles_now.contains(&role) {
                    w.roles_now.push(role);
                }
                w.role_add = None;
            } else if cancel {
                w.role_add = None;
            }
        });
    }

    fn tab_global(&self, ui: &mut egui::Ui, w: &mut SqlUsersWin, env: &Env, acct: &Account) {
        ui.columns(3, |cols| {
            for (ui, (title, list)) in cols.iter_mut().zip([("DATA", &users::GLOBAL_DATA[..]), ("STRUCTURE", &users::GLOBAL_STRUCTURE[..]), ("ADMINISTRATION", &users::GLOBAL_ADMIN[..])]) {
                caption(ui, title);
                ui.add_space(4.0);
                for p in list {
                    let mut on = w.g_now.contains(*p);
                    if ui.checkbox(&mut on, users::priv_title(p)).changed() {
                        if on {
                            w.g_now.insert(p.to_string());
                        } else {
                            w.g_now.remove(*p);
                        }
                    }
                }
            }
        });
        if env.caps.dynamic {
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Dynamic privileges (MySQL 8)").color(MUTED()));
                let mut all: Vec<String> = w.g_orig.iter().filter(|p| users::is_dynamic(p)).cloned().collect();
                for p in w.g_now.iter().filter(|p| users::is_dynamic(p)) {
                    if !all.contains(p) {
                        all.push(p.clone());
                    }
                }
                for p in all {
                    let on = w.g_now.contains(&p);
                    // One held WITH GRANT OPTION would lose the option when granted back, so it stays as it is.
                    let kept = w.dyn_grant.contains(&p);
                    let hint = if kept { "Held WITH GRANT OPTION: change it in a SQL shell" } else if !on { "Will be revoked; click to keep" } else if w.g_orig.contains(&p) { "Click to revoke" } else { "Will be granted; click to cancel" };
                    if chip(ui, &p, PRIMARY(), on).on_hover_text(hint).clicked() && !kept {
                        if on {
                            w.g_now.remove(&p);
                        } else {
                            w.g_now.insert(p);
                        }
                    }
                }
                match &mut w.priv_add {
                    None => {
                        if ui.button("+ Privilege").clicked() {
                            w.priv_add = Some(String::new());
                        }
                    }
                    Some(text) => {
                        let mut done = None;
                        let r = ui.add(egui::TextEdit::singleline(text).hint_text("CONNECTION_ADMIN").font(egui::TextStyle::Monospace).desired_width(200.0));
                        if !r.has_focus() && text.is_empty() {
                            r.request_focus();
                        }
                        let name = text.trim().to_ascii_uppercase();
                        let ok = users::is_dynamic(&name);
                        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && ok {
                            done = Some(true);
                        }
                        if ui.add_enabled(ok, primary_button(ui, "Add", PRIMARY())).on_disabled_hover_text("Capitals and underscores, like CONNECTION_ADMIN").clicked() {
                            done = Some(true);
                        }
                        if ui.button("Cancel").clicked() {
                            done = Some(false);
                        }
                        if let Some(add) = done {
                            if add {
                                w.g_now.insert(name);
                            }
                            w.priv_add = None;
                        }
                    }
                }
            });
        }
        ui.add_space(10.0);
        caption(ui, "GRANTS TO RUN");
        ui.add_space(4.0);
        preview_block(ui, users::preview_checked(env.mode, acct, &w.global_edits(), held_grants(w)));
    }

    fn tab_limits(&self, ui: &mut egui::Ui, w: &mut SqlUsersWin, env: &Env, acct: &Account) {
        let editable = w.info.is_some() && env.caps.alter_user;
        if let Some(why) = w.info_err.as_ref().filter(|_| w.info.is_none()) {
            ui.add(egui::Label::new(RichText::new(format!("The limits cannot be read: {why}")).size(TXT_SMALL).color(WARNING())).wrap());
        }
        caption(ui, "RESOURCE LIMITS");
        ui.add_space(4.0);
        ui.add_enabled_ui(editable, |ui| {
            egui::Grid::new("limits").num_columns(2).spacing([14.0, 10.0]).min_col_width(190.0).show(ui, |ui| {
                for (i, label) in ["Queries per hour", "Updates per hour", "Connections per hour", "Simultaneous connections"].into_iter().enumerate() {
                    form_label(ui, label);
                    ui.add(egui::TextEdit::singleline(&mut w.limits[i]).font(egui::TextStyle::Monospace).hint_text("0").desired_width(96.0));
                    ui.end_row();
                }
            });
        });
        ui.add_space(4.0);
        ui.label(RichText::new("0 means no limit. Counts reset every hour, or at FLUSH USER_RESOURCES.").size(TXT_SMALL).color(FAINT()));
        ui.add_space(10.0);
        caption(ui, "STATEMENTS TO RUN");
        ui.add_space(4.0);
        preview_block(ui, w.limits_plan().and_then(|p| plan_ready_or_empty(&p, env, acct)));
    }

    fn tab_databases(&self, ui: &mut egui::Ui, w: &mut SqlUsersWin, env: &Env, acct: &Account) {
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
                let free: Vec<&String> = env.known.iter().filter(|d| !w.rows.iter().any(|r| r.pattern == users::pattern_of(d))).collect();
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
        preview_block(ui, users::preview(env.mode, acct, &w.edits()));
    }

    /// The database table of the design: a name column and a checkbox per privilege, sideways scrolling for the
    /// ones beyond the design's nine.
    fn privilege_table(&self, ui: &mut egui::Ui, w: &mut SqlUsersWin) {
        const ROW_H: f32 = 28.0;
        const NAME_W: f32 = 160.0;
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
                // egui's scroll bar floats over the content, so leave it a strip of its own below the last row.
                if total > ui.clip_rect().width() {
                    ui.add_space(ui.spacing().scroll.bar_width + ui.spacing().scroll.bar_inner_margin);
                }
            });
        });
    }

    pub(super) fn sql_new_user_dialog(&mut self, ui: &mut egui::Ui, f: &mut NewUser, keep: &mut bool) {
        ui.set_width(520.0);
        let writable = self.conn_writable(f.conn);
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
            ui.add(egui::TextEdit::singleline(&mut f.password).id(secret_id("new-user", f.serial, 0)).password(true).desired_width(340.0));
            ui.end_row();
        });
        ui.add_space(8.0);
        let acct = Account::named(&f.user, &f.host);
        let check = users::check_new_user(&f.user, &f.host, &f.password);
        // The password never reaches the preview.
        sql_block(ui, &format!("{};", users::create_user_sql(self.sql_mode(f.conn), &acct, &f.password, true)));
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
        if !*keep {
            forget_secret(ui.ctx(), "new-user", f.serial, 1);
        }
    }

    fn sql_user_create_run(&mut self, f: &mut NewUser) {
        // Taken out of the form either way, so no copy stays in app state once Create was pressed.
        let password = std::mem::take(&mut f.password);
        let Some(sql) = self.sql_db_for(f.conn, None) else { return };
        self.worker.sql_user_create(f.conn, sql, Account::named(&f.user, &f.host), password, false);
    }

    /// Change password: two masked fields, the plugin on MySQL, and a preview that shows '***'.
    pub(super) fn sql_password_dialog(&mut self, ui: &mut egui::Ui, f: &mut PwForm, keep: &mut bool) {
        ui.set_width(520.0);
        let writable = self.conn_writable(f.conn);
        let caps = self.users_caps(f.conn);
        let dialect = self.users_dialect(f.conn);
        dialog_title(ui, "Change password");
        ui.label(RichText::new(f.acct.label()).monospace().color(MUTED()));
        ui.add_space(8.0);
        egui::Grid::new("newpw").num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W + 14.0).show(ui, |ui| {
            form_label(ui, "New password");
            let r = ui.add(egui::TextEdit::singleline(&mut f.password).id(secret_id("change-password", f.serial, 0)).password(true).desired_width(340.0));
            if !r.has_focus() && f.password.is_empty() && f.again.is_empty() {
                r.request_focus();
            }
            ui.end_row();
            form_label(ui, "Repeat");
            ui.add(egui::TextEdit::singleline(&mut f.again).id(secret_id("change-password", f.serial, 1)).password(true).desired_width(340.0));
            ui.end_row();
            if caps.plugin_choice {
                form_label(ui, "Plugin");
                egui::ComboBox::from_id_salt("pw-plugin").width(340.0).selected_text(if f.plugin.is_empty() { "Server default".to_string() } else { f.plugin.clone() }).show_ui(ui, |ui| {
                    menu_look(ui);
                    if ui.selectable_label(f.plugin.is_empty(), "Server default").clicked() {
                        f.plugin.clear();
                    }
                    for p in caps.plugins(&f.current) {
                        if ui.selectable_label(f.plugin == p, p).clicked() {
                            f.plugin = p.to_string();
                        }
                    }
                });
                ui.end_row();
            }
        });
        ui.add_space(8.0);
        let plugin = Some(f.plugin.as_str()).filter(|p| !p.is_empty());
        let shown = users::password_sql(self.sql_mode(f.conn), dialect, &f.acct, plugin, "", true);
        sql_block(ui, &shown.as_ref().map_or_else(|e| format!("-- {e}"), |s| format!("{s};")));
        let check = users::check_password(&f.password).and_then(|_| if f.password == f.again { Ok(()) } else { Err("The two passwords differ.".to_string()) }).and_then(|_| f.blocked.clone().map_or(Ok(()), Err));
        if let Err(e) = &check {
            if !f.password.is_empty() || !f.again.is_empty() {
                ui.label(RichText::new(e.as_str()).size(TXT_SMALL).color(ALERT()));
            }
        }
        ui.add_space(4.0);
        ui.add(egui::Label::new(RichText::new("A password change cannot be undone. It is sent once and is not shown, logged or kept by Quarry.").size(TXT_SMALL).color(MUTED())).wrap());
        if !writable {
            ui.label(RichText::new(NOTE_APPLY).size(TXT_SMALL).color(FAINT()));
        }
        ui.add_space(8.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add_enabled(writable && check.is_ok() && shown.is_ok(), primary_button(ui, "Change password", PRIMARY())).clicked() {
                self.sql_password_run(f);
                *keep = false;
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
        });
        // The typed text is cleared when Change password or Cancel ends the dialog, and the editor's own undo history
        // of these fields is dropped too. Closing with Esc leaves both to the next dialog's new ids (`serial`), which
        // cannot reach what was typed here.
        if !*keep {
            f.password.clear();
            f.again.clear();
            forget_secret(ui.ctx(), "change-password", f.serial, 2);
        }
    }

    fn sql_password_run(&mut self, f: &mut PwForm) {
        let password = std::mem::take(&mut f.password);
        f.again.clear();
        let Some(sql) = self.sql_db_for(f.conn, None) else { return };
        let plugin = Some(f.plugin.clone()).filter(|p| !p.is_empty());
        self.worker.sql_user_password(f.conn, sql, f.acct.clone(), plugin, password, false);
    }

    /// QUARRY_TEST_SQL=users, useraccount, userglobal or userlimits (the window on user "report"), or newuser (the + User dialog).
    pub(super) fn sql_users_demo(&mut self, which: &str) {
        if !matches!(which, "users" | "newuser" | "useraccount" | "userglobal" | "userlimits") {
            return;
        }
        if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == 3) {
            c.writable = true;
        }
        if which == "newuser" {
            return self.modal = Some(Modal::SqlNewUser(Box::new(NewUser { conn: 3, user: "reporting".into(), host: "10.0.%".into(), password: "demo".into(), serial: 0 })));
        }
        let acct = |u: &str, h: &str| Account { user: u.into(), host: h.into(), plugin: "caching_sha2_password".into(), ..Default::default() };
        let accounts = vec![acct("app", "%"), acct("report", "10.0.%"), acct("backup", "localhost"), Account { expired: true, ..acct("migrate", "%") }, Account { locked: true, ..acct("root", "localhost") }];
        let set = |p: &[&str]| p.iter().map(|s| s.to_string()).collect::<BTreeSet<String>>();
        let mut w = SqlUsersWin::new(3);
        w.set_list(Ok(UserList { accounts, current: Some(("app".into(), "%".into())), whole: true, why: None }));
        w.sel = Some(("report".into(), "10.0.%".into()));
        w.grants = Some(Ok(Vec::new()));
        w.name = "report".into();
        w.host = "10.0.%".into();
        w.info = Some(AccountInfo { plugin: "caching_sha2_password".into(), settings: Settings::default(), chain: false, ident: None });
        match which {
            "useraccount" => {
                w.tab = UTab::Account;
                w.locked = true;
                w.roles_orig = vec!["app_read@%".into(), "dashboards@%".into()];
                w.roles_now = w.roles_orig.clone();
            }
            "userglobal" => {
                w.tab = UTab::Global;
                w.g_orig = set(&["SELECT", "SHOW VIEW", "REPLICATION CLIENT", "CONNECTION_ADMIN", "SYSTEM_VARIABLES_ADMIN"]);
                w.g_now = w.g_orig.clone();
                w.g_now.insert("PROCESS".into());
            }
            "userlimits" => {
                w.tab = UTab::Limits;
                w.limits = ["5000".into(), "0".into(), "0".into(), "0".into()];
            }
            _ => {
                w.rows = vec![
                    GrantRow { pattern: "shop".into(), orig: BTreeSet::new(), now: set(&["SELECT"]) },
                    GrantRow { pattern: "analytics".into(), orig: BTreeSet::new(), now: set(&["SELECT", "EXECUTE"]) },
                    GrantRow { pattern: "sys".into(), orig: BTreeSet::new(), now: BTreeSet::new() },
                ];
            }
        }
        if which != "users" {
            w.limits = if which == "userlimits" { w.limits } else { ["0".into(), "0".into(), "0".into(), "0".into()] };
        }
        self.sql_users = Some(w);
    }
}
