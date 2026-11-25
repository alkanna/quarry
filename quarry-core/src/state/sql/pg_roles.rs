//! State behind src/app/sql/pg_roles.rs, which draws it.

use crate::db::sql::pg::roles::{Attrs, Change, Cluster, DefKind, Defaults, DropInfo, Kind, Obj, Objects, Opts, Privs, Role};
use std::sync::Arc;
use zeroize::Zeroize;
use crate::db;
use std::collections::{BTreeSet, HashMap};

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum PTab {
    Privileges,
    Roles,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum RTab {
    Role,
    Membership,
    Defaults,
}

/// The + Default privilege form of the Roles tab.
#[derive(Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub struct DefForm {
    pub owner: String,
    pub schema: String,
    pub kind: DefKind,
    pub privs: BTreeSet<String>,
}

pub struct PgRolesWin {
    pub conn: u64,
    pub tab: PTab,
    pub rtab: RTab,
    pub req_cluster: u64,
    /// Shared so a frame borrows it alongside the window instead of copying every role and membership.
    pub cluster: Option<db::Res<Arc<Cluster>>>,
    /// Each role's `Cluster::chain`, worked out once per answer rather than for every cell of every frame.
    pub chains: HashMap<String, Vec<String>>,
    pub database: String,
    pub schema: String,
    pub kind: Kind,
    pub inherited: bool,
    /// Whether the pg_ roles are listed.
    pub system: bool,
    pub req_objs: u64,
    pub objs: Option<db::Res<Objects>>,
    pub req_defs: u64,
    pub defs: Option<db::Res<Defaults>>,
    /// The role of the side panel and the Roles tab.
    pub role: Option<String>,
    /// The cell whose editor is open: the object's oid and the grantee ("" for PUBLIC).
    pub cell: Option<(String, String)>,
    pub filter: String,
    pub staged: Vec<Change>,
    pub busy: bool,
    /// The request id of the Apply in flight; an answer with another one is from an Apply this window no longer waits for.
    pub req_apply: u64,
    /// The signed-in role's name typed to confirm a change to its own membership.
    pub confirm: String,
    /// The Role tab's text fields as typed, for the role in `fields_for`.
    pub lim: String,
    pub until: String,
    pub fields_for: String,
    pub field_err: Option<String>,
    pub def_form: Option<DefForm>,
}

impl PgRolesWin {
    pub fn new(conn: u64, database: String) -> PgRolesWin {
        PgRolesWin {
            conn,
            tab: PTab::Privileges,
            rtab: RTab::Role,
            req_cluster: 0,
            cluster: None,
            chains: HashMap::new(),
            database,
            schema: "public".into(),
            kind: Kind::Tables,
            inherited: true,
            system: false,
            req_objs: 0,
            objs: None,
            req_defs: 0,
            defs: None,
            role: None,
            cell: None,
            filter: String::new(),
            staged: Vec::new(),
            busy: false,
            req_apply: 0,
            confirm: String::new(),
            lim: String::new(),
            until: String::new(),
            fields_for: String::new(),
            field_err: None,
            def_form: None,
        }
    }

    pub fn cl(&self) -> Option<&Cluster> {
        self.cluster.as_ref().and_then(|c| c.as_ref().ok()).map(|c| &**c)
    }

    pub fn cl_arc(&self) -> Option<Arc<Cluster>> {
        self.cluster.as_ref().and_then(|c| c.as_ref().ok()).cloned()
    }

    pub fn set_cluster(&mut self, res: db::Res<Cluster>) {
        self.chains = match &res {
            Ok(c) => c.roles.iter().map(|r| (r.name.clone(), c.chain(&r.name))).chain([(String::new(), vec![String::new()])]).collect(),
            Err(_) => HashMap::new(),
        };
        self.cluster = Some(res.map(Arc::new));
    }

    /// The staged changes to the signed-in role's own membership that need a typed confirmation, by index, with why.
    pub fn risky(&self) -> Vec<(usize, String)> {
        let Some(c) = self.cl() else { return Vec::new() };
        self.staged.iter().enumerate().filter_map(|(i, s)| c.self_membership_risk(s).map(|why| (i, why))).collect()
    }

    pub fn objects(&self) -> Option<&Objects> {
        self.objs.as_ref().and_then(|o| o.as_ref().ok())
    }

    pub fn defaults(&self) -> Option<&Defaults> {
        self.defs.as_ref().and_then(|o| o.as_ref().ok())
    }

    /// The roles shown as matrix columns and in the list, pg_ roles only when asked for.
    pub fn roles(&self) -> Vec<&Role> {
        self.cl().map(|c| c.roles.iter().filter(|r| self.system || !r.system()).collect()).unwrap_or_default()
    }

    /// Staged changes of objects or default privileges, which tie the batch to the picked database.
    pub fn tied(&self) -> bool {
        self.staged.iter().any(Change::in_database)
    }

    /// Adds a change, folded into one staged earlier on the same thing so the batch holds one change per thing, from what the
    /// server had to what the boxes say now. A change back to where it started is dropped.
    pub fn stage(&mut self, c: Change) {
        let k = key(&c);
        if let Some(i) = self.staged.iter().position(|s| key(s) == k) {
            let first = self.staged.remove(i);
            let merged = match (first, c) {
                (Change::Acl { target, grantee, from, .. }, Change::Acl { to, .. }) => Change::Acl { target, grantee, from, to },
                (Change::Attrs { role, from, .. }, Change::Attrs { to, .. }) => Change::Attrs { role, from, to },
                (Change::Member { group, member, from, .. }, Change::Member { to, .. }) => Change::Member { group, member, from, to },
                (Change::Default { owner, schema, kind, grantee, from, .. }, Change::Default { to, .. }) => Change::Default { owner, schema, kind, grantee, from, to },
                (_, c) => c,
            };
            if !merged.is_noop() {
                self.staged.insert(i, merged);
            }
        } else if !c.is_noop() {
            self.staged.push(c);
        }
    }

    pub fn privs_now(&self, o: &Obj, grantee: &str) -> Privs {
        let k = format!("acl|{:?}|{}|{grantee}", self.kind, o.oid);
        match self.staged.iter().find(|s| key(s) == k) {
            Some(Change::Acl { to, .. }) => to.clone(),
            _ => o.direct(grantee),
        }
    }

    pub fn attrs_now(&self, role: &str) -> Option<Attrs> {
        let k = format!("attrs|{role}");
        match self.staged.iter().find(|s| key(s) == k) {
            Some(Change::Attrs { to, .. }) => Some(to.clone()),
            _ => self.cl()?.role(role).map(|r| r.attrs.clone()),
        }
    }

    pub fn member_now(&self, group: &str, member: &str) -> Option<Opts> {
        let k = format!("mem|{group}|{member}");
        match self.staged.iter().find(|s| key(s) == k) {
            Some(Change::Member { to, .. }) => *to,
            _ => self.cl()?.membership(group, member),
        }
    }

    pub fn staged_key(&self, k: &str) -> bool {
        self.staged.iter().any(|s| key(s) == k)
    }
}

pub struct NewRoleForm {
    pub conn: u64,
    pub name: String,
    pub attrs: Attrs,
    pub groups: Vec<String>,
    /// Typed here and handed to the worker by Create; never shown, logged or kept after.
    pub password: String,
    pub serial: u64,
}

pub struct PgPwForm {
    pub conn: u64,
    pub role: String,
    pub oid: String,
    pub password: String,
    pub again: String,
    pub serial: u64,
}

/// The typed passwords are wiped whenever a dialog goes away, by a button, Esc or another dialog taking its place.
impl Drop for NewRoleForm {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}

impl Drop for PgPwForm {
    fn drop(&mut self) {
        self.password.zeroize();
        self.again.zeroize();
    }
}

#[cfg(any(test, feature = "test-support"))]
impl PgPwForm {
    pub fn for_test(conn: u64, role: &str, serial: u64) -> PgPwForm {
        PgPwForm { conn, role: role.into(), oid: String::new(), password: String::new(), again: String::new(), serial }
    }

    pub fn typed(&self) -> &str {
        &self.password
    }
}

pub struct DropForm {
    pub conn: u64,
    pub role: String,
    pub req: u64,
    pub info: Option<db::Res<DropInfo>>,
    pub reassign: bool,
    pub heir: String,
    pub typed: String,
}

pub fn key(c: &Change) -> String {
    match c {
        Change::Acl { target, grantee, .. } => format!("acl|{:?}|{}|{grantee}", target.kind, target.oid),
        Change::Attrs { role, .. } => format!("attrs|{role}"),
        Change::Member { group, member, .. } => format!("mem|{group}|{member}"),
        Change::Default { owner, schema, kind, grantee, .. } => format!("def|{owner}|{schema}|{:?}|{grantee}", kind),
    }
}
