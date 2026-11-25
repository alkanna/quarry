//! The users windows: MySQL and MariaDB's Users and privileges (src/app/sql/users.rs draws it) and MongoDB's Users and
//! roles (src/app/users.rs). What their clicks do, their loads and the worker's answers.

use super::{Dialog, Reply, Refusal, Secret, UiFx};
use crate::core::Core;
use crate::db::sql::users::{self, Account, Caps, Loaded, Op, Tls, UserList};
use crate::db::sql::Dialect;
use crate::db::{self, UndoAction};
use crate::state::sql::users::*;
use crate::state::{Danger, UsersWin};
use mongodb::bson::{doc, Bson, Document};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use zeroize::Zeroize;

pub const NOTE_USER: &str = "Read-only: unlock writes to create a user";

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum UsersA {
    Sql(SqlUsersA),
    Mongo(MongoUsersA),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum SqlUsersA {
    /// Opens the window, or the Roles and privileges window on PostgreSQL.
    Open { conn: u64 },
    Pick { user: String, host: String },
    Tab(UTab),
    Apply,
    Close,
    Refresh,
    /// The banner's Unlock.
    Unlock,
    NewUser,
    /// Asks to drop the account, by typing its name.
    Drop { user: String, host: String },
    /// The typed confirmation's Drop.
    DropRun { conn: u64, user: String, host: String, is_role: bool },
    Password { user: String, host: String },
    /// The New user dialog's Create.
    Create { conn: u64, user: String, host: String, password: Secret },
    /// The Change password dialog's button; no plugin keeps the server default.
    SetPassword { conn: u64, user: String, host: String, plugin: Option<String>, password: Secret },
    Lock(bool),
    Expire(ExpKind),
    Tls(Tls),
    /// A role chip: kept (granted) or taken away.
    Role { name: String, on: bool },
    /// The + Role field: Some opens it with this text, None closes it.
    RoleAdd(Option<String>),
    /// Adds the role typed in the + Role field.
    RoleAddDone,
    /// A global privilege, static or dynamic.
    Global { privilege: String, on: bool },
    PrivAdd(Option<String>),
    PrivAddDone,
    /// A privilege box of one database row, by the row's pattern.
    DbPriv { pattern: String, privilege: String, on: bool },
    /// The + Database field.
    DbAdd(Option<String>),
    DbAddDone,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum MongoUsersA {
    Open { conn: u64 },
    Refresh,
    /// The banner's Unlock.
    Unlock,
    /// Opens the editor of one user with its roles as text.
    Edit { user: String, db: String },
    CloseEdit,
    /// Saves the open editor's password and roles, read from the window.
    Save,
    /// Adds the user of the Add user fields.
    Add,
    /// Asks to drop the user, by typing its name.
    Drop { user: String, db: String },
    /// The typed confirmation's Drop.
    DropRun { conn: u64, user: String, db: String },
    Close,
}

/// Whether `rows` has one for `pattern` already. Where the server folds case, `Shop` is the row `shop`, and a second
/// row would grant on it again under another spelling.
pub fn has_row(rows: &[GrantRow], fold: bool, pattern: &str) -> bool {
    if fold {
        let low = pattern.to_lowercase();
        rows.iter().any(|r| r.pattern.to_lowercase() == low)
    } else {
        rows.iter().any(|r| r.pattern == pattern)
    }
}

/// "readWrite@shop, read" as role documents; a role without @db belongs to `default_db`.
pub fn parse_roles(text: &str, default_db: &str) -> Vec<Bson> {
    text.split(',')
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(|r| {
            let (role, db) = r.split_once('@').unwrap_or((r, default_db));
            Bson::Document(doc! { "role": role.trim(), "db": db.trim() })
        })
        .collect()
}

/// A server error without the command it echoes back, which can run to several lines.
pub fn short_err(e: &str) -> &str {
    e.find(" to execute command").map_or(e, |i| &e[..i])
}

/// The logged-in user from a connectionStatus answer, which needs no privilege: the fallback when
/// usersInfo is refused.
pub fn self_users(status: &Document) -> Vec<Document> {
    let auth = status.get_document("authInfo").cloned().unwrap_or_default();
    let roles = auth.get_array("authenticatedUserRoles").cloned().unwrap_or_default();
    auth.get_array("authenticatedUsers").into_iter().flatten().filter_map(Bson::as_document).map(|u| doc! { "user": u.get_str("user").unwrap_or_default(), "db": u.get_str("db").unwrap_or_default(), "roles": roles.clone() }).collect()
}

pub fn roles_text(user: &Document) -> String {
    user.get_array("roles")
        .map(|a| a.iter().filter_map(|r| r.as_document()).map(|r| format!("{}@{}", r.get_str("role").unwrap_or_default(), r.get_str("db").unwrap_or_default())).collect::<Vec<_>>().join(", "))
        .unwrap_or_default()
}

impl Core {
    pub(super) fn users(&mut self, a: UsersA) -> Result<Reply, Refusal> {
        match a {
            UsersA::Sql(a) => self.sql_users(a),
            UsersA::Mongo(a) => self.mongo_users(a),
        }
    }

    pub fn users_sql(&self, conn: u64) -> Option<db::sql::Db> {
        self.conn(conn).and_then(|c| c.sql.clone())
    }

    pub fn users_dialect(&self, conn: u64) -> Dialect {
        self.users_sql(conn).map_or(Dialect::MySql, |s| s.dialect)
    }

    /// The demo has no server, so it assumes a current MySQL.
    pub fn users_caps(&self, conn: u64) -> Caps {
        self.users_sql(conn).map_or_else(|| Caps::of(Dialect::MySql, "8.4.3"), |s| Caps::of(s.dialect, &s.version))
    }

    fn sql_users(&mut self, a: SqlUsersA) -> Result<Reply, Refusal> {
        match a {
            SqlUsersA::Open { conn } => return Ok(self.open_sql_users(conn)),
            SqlUsersA::DropRun { conn, user, host, is_role } => return self.sql_user_drop_run(conn, Account { is_role, ..Account::named(&user, &host) }),
            SqlUsersA::Create { conn, user, host, mut password } => {
                // Taken out of the action either way, so no copy stays once Create was pressed.
                let Some(mut pw) = password.take() else { return Err(self.refuse("The password did not come through. Type it again.")) };
                if !self.conn_writable(conn) {
                    pw.zeroize();
                    return Err(self.refuse_ro(conn, Some(NOTE_USER)));
                }
                let Some(sql) = self.sql_db_for(conn, None) else {
                    pw.zeroize();
                    return Ok(Reply::Done);
                };
                self.worker.sql_user_create(conn, sql, Account::named(&user, &host), pw, false);
                return Ok(Reply::Done);
            }
            SqlUsersA::SetPassword { conn, user, host, plugin, mut password } => {
                let Some(mut pw) = password.take() else { return Err(self.refuse("The password did not come through. Type it again.")) };
                if !self.conn_writable(conn) {
                    pw.zeroize();
                    return Err(self.refuse_ro(conn, None));
                }
                let Some(sql) = self.sql_db_for(conn, None) else {
                    pw.zeroize();
                    return Ok(Reply::Done);
                };
                let acct = self.sql_users_account(&user, &host);
                self.worker.sql_user_password(conn, sql, acct, plugin, pw, false);
                return Ok(Reply::Done);
            }
            _ => {}
        }
        let Some(conn) = self.sql_users.as_ref().map(|w| w.conn) else { return Ok(Reply::Done) };
        let dialect = self.users_dialect(conn);
        let w = self.sql_users.as_mut().unwrap();
        match a {
            SqlUsersA::Pick { user, host } => {
                w.sel = Some((user, host));
                return Ok(self.sql_users_ask_grants());
            }
            SqlUsersA::Tab(t) => w.tab = t,
            SqlUsersA::Apply => return self.sql_user_apply(),
            SqlUsersA::Close => self.sql_users = None,
            SqlUsersA::Refresh => return Ok(self.sql_users_reload()),
            SqlUsersA::Unlock => self.ui_fx.push(UiFx::Open(Dialog::Unlock(conn))),
            SqlUsersA::NewUser => return self.sql_new_user(conn),
            SqlUsersA::Drop { user, host } => {
                let acct = self.sql_users_account(&user, &host);
                let what = if acct.is_role { "role" } else { "user" };
                let title = format!("Drop the {what} {} on {}? Its password and every privilege go with it, and logins that use it stop working. This cannot be undone.", acct.label(), self.conn_name(conn));
                self.ui_fx.push(UiFx::Open(Dialog::Typed { title, expect: acct.label(), action: Danger::SqlDropUser { conn, acct } }));
            }
            SqlUsersA::Password { user, host } => {
                if !self.conn_writable(conn) {
                    return Err(self.refuse_ro(conn, None));
                }
                let a = self.sql_users_account(&user, &host);
                let w = self.sql_users.as_ref().unwrap();
                let current = w.info.as_ref().map(|i| i.plugin.clone()).unwrap_or_default();
                // MariaDB's SET PASSWORD sets the mysql_native_password part only, so an account that logs in another way is left alone.
                let blocked = w.info.as_ref().filter(|_| matches!(dialect, Dialect::MariaDb)).and_then(|i| (i.chain || (!i.plugin.is_empty() && i.plugin != "mysql_native_password")).then(|| format!("{} logs in with {}; Quarry only sets a mysql_native_password password on MariaDB. Change it in a SQL shell.", a.label(), if i.chain { "more than one method".to_string() } else { i.plugin.clone() })));
                self.req += 1;
                self.ui_fx.push(UiFx::Open(Dialog::SqlPassword(Box::new(PwForm { conn, acct: a, plugin: String::new(), current, blocked, password: String::new(), again: String::new(), serial: self.req }))));
            }
            SqlUsersA::Lock(on) => w.locked = on,
            SqlUsersA::Expire(e) => w.exp = e,
            SqlUsersA::Tls(t) => w.tls = t,
            SqlUsersA::Role { name, on } => {
                if !on {
                    w.roles_now.retain(|x| *x != name);
                } else if !w.roles_now.contains(&name) {
                    w.roles_now.push(name);
                }
            }
            SqlUsersA::RoleAdd(t) => w.role_add = t,
            SqlUsersA::RoleAddDone => {
                let Some(text) = w.role_add.as_ref().filter(|t| !t.trim().is_empty()) else { return Ok(Reply::Done) };
                // MySQL names a role with its host, % when none is given.
                let name = text.trim().to_string();
                let role = if matches!(dialect, Dialect::MySql) && !name.contains('@') { format!("{name}@%") } else { name };
                if !w.roles_now.contains(&role) {
                    w.roles_now.push(role);
                }
                w.role_add = None;
            }
            SqlUsersA::Global { privilege, on } => {
                if on {
                    w.g_now.insert(privilege);
                } else {
                    w.g_now.remove(&privilege);
                }
            }
            SqlUsersA::PrivAdd(t) => w.priv_add = t,
            SqlUsersA::PrivAddDone => {
                let Some(name) = w.priv_add.as_ref().map(|t| t.trim().to_ascii_uppercase()).filter(|n| users::is_dynamic(n)) else { return Ok(Reply::Done) };
                w.g_now.insert(name);
                w.priv_add = None;
            }
            SqlUsersA::DbPriv { pattern, privilege, on } => {
                if let Some(row) = w.rows.iter_mut().find(|r| r.pattern == pattern) {
                    if on {
                        row.now.insert(privilege);
                    } else {
                        row.now.remove(&privilege);
                    }
                }
            }
            SqlUsersA::DbAdd(t) => w.adding = t,
            SqlUsersA::DbAddDone => {
                let Some(text) = w.adding.as_ref().filter(|t| users::check_db_name(t).is_ok()) else { return Ok(Reply::Done) };
                let pattern = users::pattern_of(text.trim());
                if !has_row(&w.rows, w.fold_case, &pattern) {
                    w.rows.push(GrantRow { pattern, orig: BTreeSet::new(), now: BTreeSet::new() });
                }
                w.adding = None;
            }
            SqlUsersA::Open { .. } | SqlUsersA::DropRun { .. } | SqlUsersA::Create { .. } | SqlUsersA::SetPassword { .. } => {}
        }
        Ok(Reply::Done)
    }

    /// The account (user, host) as the window's list has it, else one with that name.
    fn sql_users_account(&self, user: &str, host: &str) -> Account {
        let found = self.sql_users.as_ref().and_then(|w| w.list.as_ref()).and_then(|l| l.as_ref().ok()).and_then(|l| l.accounts.iter().find(|a| a.user == user && a.host == host));
        found.cloned().unwrap_or_else(|| Account::named(user, host))
    }

    /// The SQL session of a connection for a statement about to run. The offline demo has no server, so it says so
    /// and gives none. `missing` is what to say when the connection has no session, or nothing when None.
    pub fn sql_db_for(&mut self, conn: u64, missing: Option<&str>) -> Option<db::sql::Db> {
        if self.demo {
            self.ok("Offline demo: no database is connected.");
            return None;
        }
        let db = self.conn(conn).and_then(|c| c.sql.clone());
        if let (None, Some(m)) = (&db, missing) {
            self.warn(m);
        }
        db
    }

    pub fn open_sql_users(&mut self, conn: u64) -> Reply {
        if self.users_dialect(conn) == Dialect::Postgres || self.conn(conn).is_some_and(|c| c.meta.engine == db::sql::Engine::Postgres) {
            return self.open_pg_roles(conn);
        }
        self.sql_users = Some(SqlUsersWin::new(conn));
        self.sql_users_reload()
    }

    /// Asks for the account list again; the grants of the chosen account follow when it arrives.
    fn sql_users_reload(&mut self) -> Reply {
        let Some(conn) = self.sql_users.as_ref().map(|w| w.conn) else { return Reply::Done };
        let Some(sql) = self.users_sql(conn) else { return Reply::Done };
        self.req += 1;
        let req = self.req;
        if let Some(w) = &mut self.sql_users {
            w.list_req = req;
            w.list = None;
        }
        self.worker.sql_users(conn, req, sql);
        Reply::Pending(req)
    }

    fn sql_users_ask_grants(&mut self) -> Reply {
        let Some((conn, acct)) = self.sql_users.as_ref().and_then(|w| w.account().map(|a| (w.conn, a))) else { return Reply::Done };
        let Some(sql) = self.users_sql(conn) else { return Reply::Done };
        self.req += 1;
        let req = self.req;
        if let Some(w) = &mut self.sql_users {
            w.grants_req = req;
            w.grants = None;
            w.rows.clear();
            w.adding = None;
        }
        self.worker.sql_user_grants(conn, req, sql, acct);
        Reply::Pending(req)
    }

    pub fn sql_users_loaded(&mut self, conn: u64, req: u64, res: db::Res<UserList>) {
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

    pub fn sql_user_grants_loaded(&mut self, conn: u64, req: u64, res: db::Res<Loaded>) {
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
    pub fn sql_user_done(&mut self, conn: u64, account: Account, op: Op, res: db::Res<String>, undo: Vec<UndoAction>) {
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

    fn sql_new_user(&mut self, conn: u64) -> Result<Reply, Refusal> {
        if !self.conn_writable(conn) {
            return Err(self.refuse_ro(conn, Some(NOTE_USER)));
        }
        self.req += 1;
        self.ui_fx.push(UiFx::Open(Dialog::SqlNewUser(Box::new(NewUser { conn, user: String::new(), host: "%".into(), password: String::new(), serial: self.req }))));
        Ok(Reply::Done)
    }

    fn sql_user_drop_run(&mut self, conn: u64, acct: Account) -> Result<Reply, Refusal> {
        if self.demo {
            self.ok("Offline demo: no database is connected.");
            return Ok(Reply::Done);
        }
        let Some(c) = self.conn(conn) else { return Ok(Reply::Done) };
        if !c.writable {
            return Err(self.refuse_ro(conn, None));
        }
        let Some(sql) = c.sql.clone() else { return Err(self.refuse_offline(conn)) };
        self.worker.sql_user_drop(conn, sql, acct, false);
        Ok(Reply::Done)
    }

    /// Runs what the open tab has staged.
    fn sql_user_apply(&mut self) -> Result<Reply, Refusal> {
        let Some((conn, acct, tab)) = self.sql_users.as_ref().and_then(|w| Some((w.conn, w.account()?, w.tab))) else { return Ok(Reply::Done) };
        if self.demo {
            self.ok("Offline demo: no database is connected.");
            return Ok(Reply::Done);
        }
        let Some(c) = self.conn(conn) else { return Ok(Reply::Done) };
        if !c.writable {
            return Err(self.refuse_ro(conn, None));
        }
        let Some(sql) = c.sql.clone() else { return Err(self.refuse_offline(conn)) };
        let Some(w) = self.sql_users.as_mut() else { return Ok(Reply::Done) };
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
            return Ok(Reply::Done);
        }
        w.busy = true;
        match plan {
            Some(p) => self.worker.sql_user_account(conn, sql, acct, p, false),
            None => self.worker.sql_user_edit(conn, sql, acct, edits, false),
        }
        Ok(Reply::Done)
    }

    fn mongo_users(&mut self, a: MongoUsersA) -> Result<Reply, Refusal> {
        if let MongoUsersA::DropRun { conn, user, db } = a {
            if !self.conn_writable(conn) {
                return Err(self.refuse_ro(conn, None));
            }
            let Some(client) = self.conn(conn).and_then(|c| c.client.clone()) else { return Err(self.refuse_offline(conn)) };
            self.worker.db_change(conn, client, db, doc! { "dropUser": &user }, format!("Dropped user {user}"));
            return Ok(Reply::Done);
        }
        if let MongoUsersA::Open { conn } = a {
            self.users = Some(UsersWin { conn, new_db: "admin".into(), ..Default::default() });
            self.load_users(conn);
            return Ok(Reply::Done);
        }
        let Some(conn) = self.users.as_ref().map(|u| u.conn) else { return Ok(Reply::Done) };
        let client = self.conn(conn).and_then(|c| c.client.clone());
        if matches!(a, MongoUsersA::Save | MongoUsersA::Add) && !self.conn_writable(conn) {
            return Err(self.refuse_ro(conn, None));
        }
        let u = self.users.as_mut().unwrap();
        let (db, cmd, done) = match a {
            MongoUsersA::Refresh => {
                u.users = None;
                u.roles = None;
                u.self_only = false;
                self.load_users(conn);
                return Ok(Reply::Done);
            }
            MongoUsersA::Unlock => {
                self.ui_fx.push(UiFx::Open(Dialog::Unlock(conn)));
                return Ok(Reply::Done);
            }
            MongoUsersA::Edit { user, db } => {
                let roles = u.users.as_ref().and_then(|r| r.as_ref().ok()).and_then(|l| l.iter().find(|d| d.get_str("user") == Ok(user.as_str()) && d.get_str("db") == Ok(db.as_str()))).map(roles_text).unwrap_or_default();
                u.edit = Some((user, db, String::new(), roles));
                return Ok(Reply::Done);
            }
            MongoUsersA::CloseEdit => {
                u.edit = None;
                return Ok(Reply::Done);
            }
            MongoUsersA::Drop { user, db } => {
                self.ui_fx.push(UiFx::Open(Dialog::Typed { title: format!("Drop the user {user} of {db}? Apps that log in with it will stop working."), expect: user.clone(), action: Danger::DropUser { conn, db, user } }));
                return Ok(Reply::Done);
            }
            MongoUsersA::Save => {
                let Some((en, edb, pwd, roles)) = &u.edit else { return Ok(Reply::Done) };
                let mut cmd = doc! { "updateUser": en.as_str(), "roles": parse_roles(roles, edb) };
                if !pwd.is_empty() {
                    cmd.insert("pwd", pwd.as_str());
                }
                (edb.clone(), cmd, format!("Updated user {en}"))
            }
            MongoUsersA::Add => {
                if u.new_name.trim().is_empty() || u.new_pwd.is_empty() {
                    return Ok(Reply::Done);
                }
                let db = if u.new_db.trim().is_empty() { "admin".to_string() } else { u.new_db.trim().to_string() };
                let cmd = doc! { "createUser": u.new_name.trim(), "pwd": u.new_pwd.as_str(), "roles": parse_roles(&u.new_roles, &db) };
                let done = format!("Added user {}", u.new_name.trim());
                u.new_pwd.zeroize();
                (db, cmd, done)
            }
            MongoUsersA::Close => {
                self.users = None;
                return Ok(Reply::Done);
            }
            MongoUsersA::Open { .. } | MongoUsersA::DropRun { .. } => return Ok(Reply::Done),
        };
        if let Some(client) = client {
            if let Some(u) = self.users.as_mut() {
                u.edit = None;
            }
            self.worker.db_change(conn, client, db, cmd, done);
        }
        Ok(Reply::Done)
    }

    /// Users of every database and custom roles, read from admin like the original
    /// (usersInfo / rolesInfo with showBuiltinRoles false, renderer ~156395).
    pub fn load_users(&mut self, conn: u64) {
        let Some(client) = self.conn(conn).and_then(|c| c.client.clone()) else { return };
        self.worker.command(client.clone(), "admin".into(), doc! { "usersInfo": { "forAllDBs": true } }, "users");
        self.worker.command(client, "admin".into(), doc! { "rolesInfo": 1, "showBuiltinRoles": false }, "roles");
    }

    /// The answers to `load_users`.
    pub fn mongo_users_reply(&mut self, tag: &str, r: db::Res<Document>) {
        match (tag, r) {
            // Atlas and least-privilege users often may not list everyone; show who is logged in instead.
            ("users", Err(e)) if db::is_unauthorized(&e) && self.users.as_ref().is_some_and(|u| !u.self_only) => {
                let conn = self.users.as_ref().map(|u| u.conn).unwrap_or_default();
                if let Some(client) = self.conn(conn).and_then(|c| c.client.clone()) {
                    self.worker.command(client, "admin".into(), doc! { "connectionStatus": 1 }, "users-self");
                }
                if let Some(u) = &mut self.users {
                    u.self_only = true;
                }
            }
            ("users", r) => {
                if let Some(u) = &mut self.users {
                    u.users = Some(r.map(|d| d.get_array("users").map(|a| a.iter().filter_map(|x| x.as_document().cloned()).collect()).unwrap_or_default()));
                }
            }
            ("users-self", r) => {
                if let Some(u) = &mut self.users {
                    u.users = Some(r.map(|d| self_users(&d)));
                }
            }
            ("roles", r) => {
                if let Some(u) = &mut self.users {
                    u.roles = Some(r.map(|d| d.get_array("roles").map(|a| a.iter().filter_map(|x| x.as_document().cloned()).collect()).unwrap_or_default()));
                }
            }
            _ => {}
        }
    }
}
