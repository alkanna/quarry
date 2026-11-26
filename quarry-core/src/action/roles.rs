//! The Roles and privileges window of a PostgreSQL connection: what its clicks do, its loads and the worker's answers.
//! Drawn by src/app/sql/pg_roles.rs; the statements, their checks and their inverses come from `db::sql::pg::roles`.

use super::{Dialog, Reply, Refusal, Secret, UiFx};
use crate::core::Core;
use crate::db::sql::pg::roles::{self as rp, Answer, Attrs, Batch, Change, Cluster, DefKind, Defaults, Did, DropInfo, DropMode, Kind, NewRole, Obj, Objects, Opts, Privs, Role};
use crate::db::{self, UndoAction};
use crate::state::sql::pg_roles::*;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

pub const NOTE_LOCKED: &str = "Read-only: unlock writes to change roles and privileges.";

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum RolesA {
    /// Opens the window on the connection's own database.
    Open { conn: u64 },
    Tab(PTab),
    RTab(RTab),
    Database(String),
    Schema(String),
    Kind(Kind),
    /// The role of the side panel and the Roles tab.
    Role(String),
    /// The side panel's Open role: the Roles tab on it.
    OpenRole(String),
    Stage(Change),
    Apply,
    /// Drops every staged change.
    Discard,
    Close,
    /// The banner's Unlock.
    Unlock,
    Refresh,
    NewRole,
    Drop(String),
    Password(String),
    /// Show inherited.
    Inherited(bool),
    /// Whether the pg_ roles are listed.
    System(bool),
    /// The matrix cell whose editor is open: the object's oid and the grantee ("" for PUBLIC).
    Cell(Option<(String, String)>),
    /// The Role tab's Connection limit field changed to this text.
    LimitEdited(String),
    /// The Role tab's Valid until field changed to this text.
    UntilEdited(String),
    /// The matrix's and the role list's Filter roles field.
    Filter(String),
    /// The staged bar's field for typing the own role's name to confirm.
    Confirm(String),
    /// The + Default privilege form, as edited, or None to close it.
    DefForm(Option<DefForm>),
    /// The New role dialog's Create.
    Create { conn: u64, spec: NewRole, password: Secret },
    /// The Change password dialog's button.
    SetPassword { conn: u64, role: String, oid: String, password: Secret },
    /// The Drop role dialog's button, with what the dialog read and showed.
    DropRun { conn: u64, info: DropInfo, mode: DropMode },
}

/// What the tabs need to know about the connection and the login.
pub struct RolesEnv {
    pub major: u32,
    /// The lock's note while writes are locked.
    pub locked: Option<String>,
    /// Why privileges and attributes cannot be staged: the lock, or a login without CREATEROLE. Its own password and the members
    /// of roles it holds ADMIN on have checks of their own.
    pub blocked: Option<String>,
}

impl RolesEnv {
    /// Whether `c` may be staged: memberships go by the ADMIN option the server asks for, not by CREATEROLE.
    pub fn may_stage(&self, c: &Change) -> bool {
        self.locked.is_none() && (self.blocked.is_none() || matches!(c, Change::Member { .. }))
    }
}

impl Core {
    pub(super) fn roles(&mut self, a: RolesA) -> Result<Reply, Refusal> {
        if let RolesA::Open { conn } = a {
            return Ok(self.open_pg_roles(conn));
        }
        match a {
            RolesA::Create { conn, spec, mut password } => {
                let Some(mut pw) = password.take() else { return Err(self.refuse("The password did not come through. Type it again.")) };
                if !self.conn_writable(conn) {
                    pw.zeroize();
                    return Err(self.refuse_ro(conn, None));
                }
                // A group signs in with nothing, so a password typed before switching to Group only goes nowhere.
                if !spec.attrs.login {
                    pw.zeroize();
                }
                if self.demo {
                    pw.zeroize();
                    self.ok("Offline demo: no database is connected.");
                } else if let Some(sql) = self.pg_roles_db(conn) {
                    self.worker.pg_role_create(conn, sql, spec, pw, false);
                } else {
                    pw.zeroize();
                }
                return Ok(Reply::Done);
            }
            RolesA::SetPassword { conn, role, oid, mut password } => {
                let Some(mut pw) = password.take() else { return Err(self.refuse("The password did not come through. Type it again.")) };
                if !self.conn_writable(conn) {
                    pw.zeroize();
                    return Err(self.refuse_ro(conn, None));
                }
                if self.demo {
                    pw.zeroize();
                    self.ok("Offline demo: no database is connected.");
                } else if let Some(sql) = self.pg_roles_db(conn) {
                    self.worker.pg_role_password(conn, sql, role, oid, pw, false);
                } else {
                    pw.zeroize();
                }
                return Ok(Reply::Done);
            }
            RolesA::DropRun { conn, info, mode } => {
                if !self.conn_writable(conn) {
                    return Err(self.refuse_ro(conn, None));
                }
                if let Some(sql) = self.pg_roles_db(conn) {
                    self.worker.pg_role_drop(conn, sql, info, mode, false);
                }
                return Ok(Reply::Done);
            }
            _ => {}
        }
        let Some(conn) = self.pg_roles.as_ref().map(|w| w.conn) else { return Ok(Reply::Done) };
        match a {
            RolesA::Tab(t) => {
                let w = self.pg_roles.as_mut().unwrap();
                w.tab = t;
                w.cell = None;
            }
            RolesA::RTab(t) => self.pg_roles.as_mut().unwrap().rtab = t,
            RolesA::Database(d) => {
                self.pg_roles.as_mut().unwrap().database = d;
                self.pg_roles_objects();
                self.pg_roles_defaults();
            }
            RolesA::Schema(s) => {
                self.pg_roles.as_mut().unwrap().schema = s;
                self.pg_roles_objects();
            }
            RolesA::Kind(k) => {
                self.pg_roles.as_mut().unwrap().kind = k;
                self.pg_roles_objects();
            }
            RolesA::Role(r) => self.pg_roles.as_mut().unwrap().role = Some(r),
            RolesA::OpenRole(r) => {
                let w = self.pg_roles.as_mut().unwrap();
                w.role = Some(r);
                w.tab = PTab::Roles;
                w.rtab = RTab::Role;
                w.cell = None;
            }
            RolesA::Stage(c) => self.pg_roles_stage(c),
            RolesA::Apply => return self.pg_roles_apply(),
            RolesA::Discard => {
                let w = self.pg_roles.as_mut().unwrap();
                w.staged.clear();
                w.fields_for.clear();
                w.confirm.clear();
            }
            RolesA::Close => self.pg_roles = None,
            RolesA::Unlock => self.ui_fx.push(UiFx::Open(Dialog::Unlock(conn))),
            RolesA::Refresh => return Ok(self.pg_roles_reload()),
            RolesA::NewRole => return self.pg_new_role(conn),
            RolesA::Drop(role) => return self.pg_drop_role_ask(conn, role),
            RolesA::Password(role) => {
                let oid = self.pg_roles.as_ref().and_then(|w| w.cl()).and_then(|c| c.role(&role)).map(|r| r.oid.clone()).unwrap_or_default();
                self.req += 1;
                self.ui_fx.push(UiFx::Open(Dialog::PgPassword(Box::new(PgPwForm { conn, role, oid, password: String::new(), again: String::new(), serial: self.req }))));
            }
            RolesA::Inherited(on) => self.pg_roles.as_mut().unwrap().inherited = on,
            RolesA::System(on) => self.pg_roles.as_mut().unwrap().system = on,
            RolesA::Cell(c) => self.pg_roles.as_mut().unwrap().cell = c,
            RolesA::Filter(t) => self.pg_roles.as_mut().unwrap().filter = t,
            RolesA::Confirm(t) => self.pg_roles.as_mut().unwrap().confirm = t,
            RolesA::LimitEdited(t) => {
                self.pg_roles.as_mut().unwrap().lim = t;
                let Some((role, now)) = self.pg_roles_field_role() else { return Ok(Reply::Done) };
                let w = self.pg_roles.as_mut().unwrap();
                match w.lim.trim().parse::<i64>() {
                    Ok(n) if (-1..=i32::MAX as i64).contains(&n) => {
                        w.field_err = None;
                        self.pg_roles_stage(Change::Attrs { role: role.name, from: role.attrs, to: Attrs { conn_limit: n, ..now } });
                    }
                    _ => w.field_err = Some("The connection limit is a whole number, -1 for none.".into()),
                }
            }
            RolesA::UntilEdited(t) => {
                self.pg_roles.as_mut().unwrap().until = t;
                let Some((role, now)) = self.pg_roles_field_role() else { return Ok(Reply::Done) };
                let w = self.pg_roles.as_ref().unwrap();
                let v = w.until.trim();
                let v = if v.eq_ignore_ascii_case("never") { "" } else { v }.to_string();
                self.pg_roles_stage(Change::Attrs { role: role.name, from: role.attrs, to: Attrs { valid_until: v, ..now } });
            }
            RolesA::DefForm(f) => self.pg_roles.as_mut().unwrap().def_form = f,
            RolesA::Open { .. } | RolesA::Create { .. } | RolesA::SetPassword { .. } | RolesA::DropRun { .. } => {}
        }
        Ok(Reply::Done)
    }

    fn pg_roles_db(&self, conn: u64) -> Option<db::sql::Db> {
        self.conn(conn).and_then(|c| c.sql.clone())
    }

    /// The role the Role tab's fields belong to, with its attributes as staged now.
    fn pg_roles_field_role(&self) -> Option<(Role, Attrs)> {
        let w = self.pg_roles.as_ref()?;
        let role = w.role.as_ref().and_then(|r| w.cl()?.role(r).cloned())?;
        let now = w.attrs_now(&role.name).unwrap_or_default();
        Some((role, now))
    }

    /// Fills the Role tab's text fields from the role on show when it changed, before they are drawn.
    pub fn pg_roles_fields(&mut self) {
        let Some(w) = self.pg_roles.as_mut().filter(|w| w.tab == PTab::Roles && w.rtab == RTab::Role) else { return };
        let Some(name) = w.role.clone().filter(|r| w.cl().is_some_and(|c| c.role(r).is_some())) else { return };
        if w.fields_for != name {
            let now = w.attrs_now(&name).unwrap_or_default();
            w.fields_for = name;
            w.lim = now.conn_limit.to_string();
            w.until = now.valid_until.clone();
            w.field_err = None;
        }
    }

    /// Opens the window on the connection's own database.
    pub fn open_pg_roles(&mut self, conn: u64) -> Reply {
        let database = self.pg_roles_db(conn).map(|d| d.pg_database()).filter(|d| !d.is_empty()).unwrap_or_else(|| "postgres".into());
        self.pg_roles = Some(PgRolesWin::new(conn, database));
        self.pg_roles_reload()
    }

    /// Reads the roles again, then the objects and default privileges the window shows.
    fn pg_roles_reload(&mut self) -> Reply {
        let Some((conn, database, schema, kind)) = self.pg_roles.as_ref().map(|w| (w.conn, w.database.clone(), w.schema.clone(), w.kind)) else { return Reply::Done };
        let Some(sql) = self.pg_roles_db(conn) else { return Reply::Done };
        self.req += 3;
        let r = self.req;
        if let Some(w) = &mut self.pg_roles {
            (w.req_cluster, w.req_objs, w.req_defs) = (r - 2, r - 1, r);
            w.cluster = None;
            w.objs = None;
            w.defs = None;
        }
        self.worker.pg_roles_cluster(conn, r - 2, sql.clone());
        self.worker.pg_roles_objects(conn, r - 1, sql.clone(), database.clone(), schema, kind);
        self.worker.pg_roles_defaults(conn, r, sql, database);
        Reply::Pending(r - 2)
    }

    fn pg_roles_objects(&mut self) {
        let Some((conn, database, schema, kind)) = self.pg_roles.as_ref().map(|w| (w.conn, w.database.clone(), w.schema.clone(), w.kind)) else { return };
        let Some(sql) = self.pg_roles_db(conn) else { return };
        self.req += 1;
        let r = self.req;
        if let Some(w) = &mut self.pg_roles {
            w.req_objs = r;
            w.objs = None;
            w.cell = None;
        }
        self.worker.pg_roles_objects(conn, r, sql, database, schema, kind);
    }

    fn pg_roles_defaults(&mut self) {
        let Some((conn, database)) = self.pg_roles.as_ref().map(|w| (w.conn, w.database.clone())) else { return };
        let Some(sql) = self.pg_roles_db(conn) else { return };
        self.req += 1;
        let r = self.req;
        if let Some(w) = &mut self.pg_roles {
            w.req_defs = r;
            w.defs = None;
        }
        self.worker.pg_roles_defaults(conn, r, sql, database);
    }

    /// What the tabs and the staging rules need to know about the connection and the login of `w`.
    pub fn roles_env(&self, w: &PgRolesWin) -> RolesEnv {
        let major = w.cl().map(|c| c.major).or_else(|| self.pg_roles_db(w.conn).map(|d| d.version.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0))).unwrap_or(17);
        let locked = (!self.conn_writable(w.conn)).then(|| NOTE_LOCKED.to_string());
        let blocked = locked.clone().or_else(|| w.cl().and_then(Cluster::read_only_reason));
        RolesEnv { major, locked, blocked }
    }

    fn pg_roles_stage(&mut self, c: Change) {
        let Some(w) = self.pg_roles.as_ref() else { return };
        let env = self.roles_env(w);
        let me = w.cl().map(|c| c.me.clone()).unwrap_or_default();
        if let Some(why) = Cluster::self_lockout(&me, &c) {
            self.warn(why);
        } else if env.may_stage(&c) && !w.busy {
            self.pg_roles.as_mut().unwrap().stage(c);
        }
    }

    fn pg_roles_apply(&mut self) -> Result<Reply, Refusal> {
        let Some(w) = self.pg_roles.as_ref() else { return Ok(Reply::Done) };
        if w.staged.is_empty() || w.busy {
            return Ok(Reply::Done);
        }
        let conn = w.conn;
        if self.demo {
            self.ok("Offline demo: no database is connected.");
            return Ok(Reply::Done);
        }
        if !self.conn_writable(conn) {
            return Err(self.refuse_ro(conn, None));
        }
        let Some(sql) = self.pg_roles_db(conn) else { return Err(self.refuse_offline(conn)) };
        self.req += 1;
        let req = self.req;
        let Some(w) = self.pg_roles.as_mut() else { return Ok(Reply::Done) };
        let risky = w.risky();
        let me = w.cl().map(|c| c.me.clone()).unwrap_or_default();
        if !risky.is_empty() && w.confirm != me {
            return Err(self.refuse(format!("Type {me} to confirm the change to your own membership.")));
        }
        // Objects and default privileges run in the database they were read from; role changes run anywhere, so there too.
        let db_oid = if w.tied() { w.objects().map(|o| o.db_oid.clone()).or_else(|| w.defaults().map(|d| d.db_oid.clone())).unwrap_or_default() } else { String::new() };
        let roles = w.cl().map(|c| c.roles.iter().map(|r| (r.name.clone(), r.oid.clone())).collect()).unwrap_or_default();
        let batch = Batch { database: w.database.clone(), db_oid, changes: w.staged.clone(), roles, no_undo: risky.into_iter().map(|(i, _)| i).collect(), ..Default::default() };
        w.busy = true;
        w.req_apply = req;
        self.worker.pg_roles_apply(conn, req, sql, batch, false);
        Ok(Reply::Pending(req))
    }

    fn pg_new_role(&mut self, conn: u64) -> Result<Reply, Refusal> {
        if !self.conn_writable(conn) {
            return Err(self.refuse_ro(conn, None));
        }
        self.req += 1;
        self.ui_fx.push(UiFx::Open(Dialog::PgNewRole(Box::new(NewRoleForm { conn, name: String::new(), attrs: Attrs { login: true, ..Default::default() }, groups: Vec::new(), password: String::new(), serial: self.req }))));
        Ok(Reply::Done)
    }

    fn pg_drop_role_ask(&mut self, conn: u64, role: String) -> Result<Reply, Refusal> {
        if !self.conn_writable(conn) {
            return Err(self.refuse_ro(conn, None));
        }
        self.req += 1;
        let req = self.req;
        let heir = self.pg_roles.as_ref().and_then(|w| w.cl().map(|c| c.me.clone())).unwrap_or_default();
        self.ui_fx.push(UiFx::Open(Dialog::PgDropRole(Box::new(DropForm { conn, role: role.clone(), req, info: None, reassign: true, heir, typed: String::new() }))));
        match self.pg_roles_db(conn) {
            Some(sql) => {
                self.worker.pg_role_drop_info(conn, req, sql, role);
                Ok(Reply::Pending(req))
            }
            None => Ok(Reply::Done),
        }
    }

    pub fn pg_roles_answer(&mut self, conn: u64, req: u64, ans: Answer) {
        match ans {
            Answer::Cluster(res) => {
                if let Some(w) = self.pg_roles.as_mut().filter(|w| w.conn == conn && w.req_cluster == req) {
                    if let Ok(c) = &res && w.role.as_ref().is_none_or(|r| c.role(r).is_none()) {
                        w.role = c.roles.iter().find(|r| !r.system() && r.name != c.me).or_else(|| c.me()).map(|r| r.name.clone());
                    }
                    w.set_cluster(res);
                }
            }
            Answer::Objects(res) => {
                if let Some(w) = self.pg_roles.as_mut().filter(|w| w.conn == conn && w.req_objs == req) {
                    // A schema that is not in this database falls back to public or the first one listed.
                    if let Ok(o) = &res && w.kind.in_schema() && !o.schemas.contains(&w.schema) && let Some(s) = o.schemas.iter().find(|s| *s == "public").or(o.schemas.first()).cloned() {
                        w.schema = s;
                        w.objs = None;
                        self.pg_roles_objects();
                        return;
                    }
                    w.objs = Some(res);
                }
            }
            Answer::Defaults(res) => {
                if let Some(w) = self.pg_roles.as_mut().filter(|w| w.conn == conn && w.req_defs == req) {
                    w.defs = Some(res);
                }
            }
            // The app fills its Drop role dialog with this one; see `Core::handles`.
            Answer::DropInfo(_) => {}
            Answer::Done { did, res, undo } => self.pg_roles_done(conn, req, did, res, undo),
        }
    }

    fn pg_roles_done(&mut self, conn: u64, req: u64, did: Did, res: db::Res<String>, undo: Option<UndoAction>) {
        let ok = res.is_ok();
        match res {
            Ok(s) => self.ok(s),
            Err(e) => self.err(e),
        }
        if let Some(u) = undo {
            self.push_undo(u);
        }
        let Some(w) = self.pg_roles.as_mut().filter(|w| w.conn == conn) else { return };
        match &did {
            // Only the Apply this window waits for: one from before Esc closed it must not clear what was staged since.
            Did::Applied if req == w.req_apply => {
                w.busy = false;
                // A failed batch ran nothing and stays staged, so its reason can be read against what the server holds now.
                if ok {
                    w.staged.clear();
                    w.confirm.clear();
                }
            }
            Did::Created(name) if ok => w.role = Some(name.clone()),
            Did::Dropped(name) if ok && w.role.as_deref() == Some(name.as_str()) => w.role = None,
            _ => {}
        }
        w.fields_for.clear();
        if did != Did::Password {
            self.pg_roles_reload();
        }
    }

    /// QUARRY_TEST_PG=roles (the Privileges tab with a cell editor open and one change staged) or roles-role (the Roles tab on
    /// app_writer with its connection limit staged).
    pub fn pg_roles_demo(&mut self, which: &str, conn: u64) {
        if !matches!(which, "roles" | "roles-role") {
            return;
        }
        if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
            c.writable = true;
        }
        let mut w = PgRolesWin::new(conn, "analytics".into());
        let (mut cl, objs, defs) = demo_data();
        w.role = Some("app_writer".into());
        if which == "roles" {
            let o = objs.objs.iter().find(|o| o.name == "orders").cloned().unwrap_or_default();
            let mut to = o.direct("app_writer");
            to.insert("DELETE".into(), false);
            w.staged.push(Change::Acl { target: o.target(Kind::Tables), grantee: "app_writer".into(), from: o.direct("app_writer"), to });
            w.cell = Some((o.oid.clone(), "app_writer".into()));
        } else {
            w.tab = PTab::Roles;
            // The server has no limit yet, so the staged change reads as in the design.
            if let Some(r) = cl.roles.iter_mut().find(|r| r.name == "app_writer") {
                r.attrs.conn_limit = -1;
            }
            let r = cl.role("app_writer").cloned().unwrap_or_default();
            w.staged.push(Change::Attrs { role: r.name.clone(), from: r.attrs.clone(), to: Attrs { conn_limit: 20, ..r.attrs.clone() } });
        }
        w.set_cluster(Ok(cl));
        w.objs = Some(Ok(objs));
        w.defs = Some(Ok(defs));
        self.pg_roles = Some(w);
    }
}

/// The sample server of QUARRY_TEST_PG=roles: PostgreSQL 17, the roles of the design, the tables of `public` and a few default
/// privileges.
pub fn demo_data() -> (Cluster, Objects, Defaults) {
    let role = |oid: &str, name: &str, a: Attrs| Role { oid: oid.into(), name: name.into(), attrs: a, admin: true };
    let login = Attrs { login: true, ..Default::default() };
    let roles = vec![
        role("16401", "app_reader", login.clone()),
        role("16402", "app_writer", Attrs { conn_limit: 20, ..login.clone() }),
        role("16405", "jane", Attrs { valid_until: "2026-12-31 00:00:00+00".into(), ..login.clone() }),
        role("10", "postgres", Attrs { superuser: true, createdb: true, createrole: true, replication: true, bypassrls: true, ..login.clone() }),
        role("16403", "readonly", Attrs::default()),
        role("16404", "reporting", Attrs::default()),
        role("3373", "pg_monitor", Attrs::default()),
    ];
    let m = |g: &str, mem: &str| rp::Membership { group: g.into(), member: mem.into(), opts: Opts { admin: false, inherit: Some(true), set: Some(true) }, grantors: vec!["postgres".into()] };
    let members = vec![m("reporting", "app_writer"), m("reporting", "jane"), m("readonly", "app_reader"), m("readonly", "reporting")];
    let cl = Cluster { major: 17, me: "postgres".into(), roles, members, databases: vec!["analytics".into(), "billing".into(), "postgres".into()], password_encryption: "scram-sha-256".into() };
    let p = |v: &[(&str, bool)]| -> Privs { v.iter().map(|(k, g)| (k.to_string(), *g)).collect() };
    let e = |g: &str, v: &[(&str, bool)]| rp::AclEntry { grantee: g.into(), grantor: "postgres".into(), privs: p(v) };
    let all: Vec<(&str, bool)> = ["SELECT", "INSERT", "UPDATE", "DELETE", "TRUNCATE", "REFERENCES", "TRIGGER", "MAINTAIN"].iter().map(|k| (*k, false)).collect();
    let write = [("SELECT", false), ("INSERT", false), ("UPDATE", false)];
    let table = |oid: &str, name: &str, detail: &str, owner: &str, acl: Vec<rp::AclEntry>| Obj { oid: oid.into(), schema: "public".into(), name: name.into(), detail: detail.into(), owner: owner.into(), acl, ..Default::default() };
    let objs = vec![
        table("16501", "orders", "", "postgres", vec![e("postgres", &all), e("app_reader", &[("SELECT", false)]), e("app_writer", &[("SELECT", false), ("INSERT", false), ("UPDATE", false), ("TRUNCATE", false)]), e("reporting", &[("SELECT", false)])]),
        table("16502", "customers", "", "postgres", vec![e("postgres", &all), e("app_reader", &[("SELECT", false)]), e("app_writer", &write), e("reporting", &[("SELECT", false)])]),
        table("16503", "order_items", "", "postgres", vec![e("postgres", &all), e("app_reader", &[("SELECT", false)]), e("app_writer", &[("SELECT", false), ("INSERT", false), ("UPDATE", false), ("DELETE", false)]), e("reporting", &[("SELECT", false)])]),
        table("16504", "payments", "", "postgres", vec![e("postgres", &all), e("app_writer", &[("SELECT", false), ("INSERT", false)]), e("reporting", &[("SELECT", false)])]),
        table("16505", "daily_sales", "view", "jane", vec![e("jane", &all), e("reporting", &[("SELECT", true)])]),
        table("16506", "events", "", "postgres", vec![e("postgres", &all), e("app_reader", &[("SELECT", false)]), e("app_writer", &[("INSERT", false)])]),
    ];
    let objects = Objects { database: "analytics".into(), schema: "public".into(), kind: Kind::Tables, objs, schemas: vec!["public".into(), "reporting".into(), "staging".into()], db_oid: "16384".into(), cut: false };
    let d = |owner: &str, schema: &str, kind: DefKind, acl: Vec<rp::AclEntry>| rp::DefaultAcl { owner: owner.into(), schema: schema.into(), kind, acl };
    let defs = Defaults {
        database: "analytics".into(),
        db_oid: "16384".into(),
        rows: vec![
            d("postgres", "public", DefKind::Tables, vec![e("reporting", &[("SELECT", false)]), e("app_writer", &write)]),
            d("postgres", "public", DefKind::Sequences, vec![e("app_writer", &[("USAGE", false), ("SELECT", false)])]),
            d("postgres", "reporting", DefKind::Tables, vec![e("reporting", &[("SELECT", false)])]),
        ],
        schemas: vec!["public".into(), "reporting".into(), "staging".into()],
    };
    (cl, objects, defs)
}
