//! PostgreSQL roles and privileges: the reads behind the Roles and privileges window (roles, memberships, object ACLs through
//! `aclexplode`, default privileges), the GRANT, REVOKE, ALTER ROLE, membership and ALTER DEFAULT PRIVILEGES statements with
//! their exact inverses, CREATE and DROP ROLE, and password changes. Every name goes in as a quoted identifier and every value
//! as a literal escaped by `pg_lit`; privilege names come from the lists here, never from typed text.
//!
//! A batch of changes runs in one write transaction on the database it touches. Each change first reads its state again and
//! compares it with what the window showed, then its statements run, then the state is read once more and compared with what
//! the change meant to leave. A grant made meanwhile by someone else, or a REVOKE the server skipped because another role made
//! that grant, rolls the whole batch back with the reason. Undo is the same run with every change inverted.
//!
//! A password never goes out as typed: its hash is computed here, the way psql's \password does it, and only that is sent,
//! inside the one statement on the writer. Like \password it follows the server's password_encryption: a SCRAM-SHA-256
//! verifier with the server's scram_iterations and PostgreSQL's own SASLprep, or the MD5 hash on a server set to md5 (the
//! default up to PostgreSQL 13). It is shown as '***' in previews and in the Query log.

use crate::db::sql::pg::create::read;
use crate::db::sql::pg::ddl::{commit_unknown, exec, name_ok, q, set_lock_wait};
use crate::db::sql::exec::{SqlRow, Tx};
use crate::db::sql::value::{pg_lit, Param};
use crate::db::sql::worker::text;
use crate::db::sql::write::{writer_conn, SqlUndo, LOCKED};
use crate::db::sql::{se, Db};
use crate::db::{Msg, Res, UndoAction, UndoKind, Worker};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

mod run;
mod statements;
pub use run::*;
pub use statements::*;

// Kinds and privileges.

/// What the Privileges matrix lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default, serde::Serialize, serde::Deserialize)]
pub enum Kind {
    #[default]
    Tables,
    Sequences,
    Functions,
    Schemas,
    Databases,
}

pub const KINDS: [Kind; 5] = [Kind::Tables, Kind::Sequences, Kind::Functions, Kind::Schemas, Kind::Databases];

/// The privileges of a table in the order the cell editor lists them. MAINTAIN is PostgreSQL 17 and later.
const TABLE_PRIVS: [&str; 8] = ["SELECT", "INSERT", "UPDATE", "DELETE", "TRUNCATE", "REFERENCES", "TRIGGER", "MAINTAIN"];

fn table_privs(major: u32) -> Vec<&'static str> {
    TABLE_PRIVS.iter().copied().filter(|p| *p != "MAINTAIN" || major >= 17).collect()
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Tables => "Tables",
            Kind::Sequences => "Sequences",
            Kind::Functions => "Functions",
            Kind::Schemas => "Schema",
            Kind::Databases => "Database",
        }
    }

    pub fn noun(self) -> &'static str {
        match self {
            Kind::Tables => "table",
            Kind::Sequences => "sequence",
            Kind::Functions => "function",
            Kind::Schemas => "schema",
            Kind::Databases => "database",
        }
    }

    /// The privileges GRANT takes on this kind on a server of `major`.
    pub fn privs(self, major: u32) -> Vec<&'static str> {
        match self {
            Kind::Tables => table_privs(major),
            Kind::Sequences => vec!["USAGE", "SELECT", "UPDATE"],
            Kind::Functions => vec!["EXECUTE"],
            Kind::Schemas => vec!["USAGE", "CREATE"],
            Kind::Databases => vec!["CONNECT", "CREATE", "TEMPORARY"],
        }
    }

    /// ROUTINE covers functions, procedures and aggregates alike.
    fn word(self) -> &'static str {
        match self {
            Kind::Tables => "TABLE",
            Kind::Sequences => "SEQUENCE",
            Kind::Functions => "ROUTINE",
            Kind::Schemas => "SCHEMA",
            Kind::Databases => "DATABASE",
        }
    }

    /// The catalog, its ACL column, its owner column and the `acldefault` letter that stands for a NULL ACL.
    fn catalog(self) -> (&'static str, &'static str, &'static str, char) {
        match self {
            Kind::Tables => ("pg_class", "relacl", "relowner", 'r'),
            Kind::Sequences => ("pg_class", "relacl", "relowner", 's'),
            Kind::Functions => ("pg_proc", "proacl", "proowner", 'f'),
            Kind::Schemas => ("pg_namespace", "nspacl", "nspowner", 'n'),
            Kind::Databases => ("pg_database", "datacl", "datdba", 'd'),
        }
    }

    /// Whether the objects sit in a schema, so the schema picker applies.
    pub fn in_schema(self) -> bool {
        matches!(self, Kind::Tables | Kind::Sequences | Kind::Functions)
    }
}

/// The kinds ALTER DEFAULT PRIVILEGES knows, with `pg_default_acl.defaclobjtype`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default, serde::Serialize, serde::Deserialize)]
pub enum DefKind {
    #[default]
    Tables,
    Sequences,
    Functions,
    Types,
    Schemas,
}

pub const DEF_KINDS: [DefKind; 5] = [DefKind::Tables, DefKind::Sequences, DefKind::Functions, DefKind::Types, DefKind::Schemas];

impl DefKind {
    fn code(self) -> &'static str {
        match self {
            DefKind::Tables => "r",
            DefKind::Sequences => "S",
            DefKind::Functions => "f",
            DefKind::Types => "T",
            DefKind::Schemas => "n",
        }
    }

    fn of(code: &str) -> Option<DefKind> {
        DEF_KINDS.into_iter().find(|k| k.code() == code)
    }

    fn word(self) -> &'static str {
        match self {
            DefKind::Tables => "TABLES",
            DefKind::Sequences => "SEQUENCES",
            DefKind::Functions => "FUNCTIONS",
            DefKind::Types => "TYPES",
            DefKind::Schemas => "SCHEMAS",
        }
    }

    /// The `acldefault` letter: what PostgreSQL gives a new object when no default privileges apply.
    fn acl_char(self) -> char {
        match self {
            DefKind::Tables => 'r',
            DefKind::Sequences => 's',
            DefKind::Functions => 'f',
            DefKind::Types => 'T',
            DefKind::Schemas => 'n',
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DefKind::Tables => "Tables",
            DefKind::Sequences => "Sequences",
            DefKind::Functions => "Functions",
            DefKind::Types => "Types",
            DefKind::Schemas => "Schemas",
        }
    }

    pub fn privs(self, major: u32) -> Vec<&'static str> {
        match self {
            DefKind::Tables => table_privs(major),
            DefKind::Sequences => vec!["USAGE", "SELECT", "UPDATE"],
            DefKind::Functions => vec!["EXECUTE"],
            DefKind::Types => vec!["USAGE"],
            DefKind::Schemas => vec!["USAGE", "CREATE"],
        }
    }
}

/// Privileges held, each with whether it carries the grant option.
pub type Privs = BTreeMap<String, bool>;

/// "SELECT, INSERT (grant option)" for messages and labels.
pub fn privs_text(p: &Privs) -> String {
    if p.is_empty() {
        return "none".into();
    }
    p.iter().map(|(k, g)| if *g { format!("{k} with grant option") } else { k.clone() }).collect::<Vec<_>>().join(", ")
}

// Roles.

/// What ALTER ROLE changes. `valid_until` is empty for never, "-infinity" for a role that never could sign in, else the moment
/// in UTC as `YYYY-MM-DD HH:MM:SS+00`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Attrs {
    pub login: bool,
    pub superuser: bool,
    pub createdb: bool,
    pub createrole: bool,
    pub inherit: bool,
    pub replication: bool,
    pub bypassrls: bool,
    pub conn_limit: i64,
    pub valid_until: String,
}

impl Default for Attrs {
    fn default() -> Attrs {
        Attrs { login: false, superuser: false, createdb: false, createrole: false, inherit: true, replication: false, bypassrls: false, conn_limit: -1, valid_until: String::new() }
    }
}

/// The flags in the order the design lists them, with their keyword and what they mean.
pub const FLAGS: [(&str, &str); 7] = [
    ("LOGIN", "can sign in"),
    ("SUPERUSER", "bypasses every check"),
    ("CREATEDB", "creates databases"),
    ("CREATEROLE", "creates and changes roles"),
    ("INHERIT", "uses its roles' privileges"),
    ("REPLICATION", "streams WAL, runs backups"),
    ("BYPASSRLS", "ignores row security"),
];

impl Attrs {
    pub fn flag(&self, name: &str) -> bool {
        match name {
            "LOGIN" => self.login,
            "SUPERUSER" => self.superuser,
            "CREATEDB" => self.createdb,
            "CREATEROLE" => self.createrole,
            "INHERIT" => self.inherit,
            "REPLICATION" => self.replication,
            "BYPASSRLS" => self.bypassrls,
            _ => false,
        }
    }

    pub fn flag_mut(&mut self, name: &str) -> Option<&mut bool> {
        Some(match name {
            "LOGIN" => &mut self.login,
            "SUPERUSER" => &mut self.superuser,
            "CREATEDB" => &mut self.createdb,
            "CREATEROLE" => &mut self.createrole,
            "INHERIT" => &mut self.inherit,
            "REPLICATION" => &mut self.replication,
            "BYPASSRLS" => &mut self.bypassrls,
            _ => return None,
        })
    }

    /// Whether the fields `from` and `to` differ in hold the same values in `self` and `want`. Only those are compared, so an
    /// attribute nobody touched may change meanwhile without stopping the change.
    fn same_touched(&self, want: &Attrs, from: &Attrs, to: &Attrs) -> bool {
        FLAGS.iter().all(|(f, _)| from.flag(f) == to.flag(f) || self.flag(f) == want.flag(f)) && (from.conn_limit == to.conn_limit || self.conn_limit == want.conn_limit) && (from.valid_until == to.valid_until || self.valid_until == want.valid_until)
    }

    fn text(&self) -> String {
        let flags: Vec<&str> = FLAGS.iter().filter(|(f, _)| self.flag(f)).map(|(f, _)| *f).collect();
        format!("{}, connection limit {}, valid until {}", if flags.is_empty() { "no attributes".into() } else { flags.join(" ") }, self.conn_limit, until_shown(&self.valid_until))
    }
}

/// VALID UNTIL for people: "never" for none, "expired" for -infinity, else the moment.
pub fn until_shown(v: &str) -> &str {
    match v {
        "" => "never",
        "-infinity" => "expired",
        v => v,
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Role {
    pub oid: String,
    pub name: String,
    pub attrs: Attrs,
    /// Whether the signed-in role holds ADMIN on this one (always true for a superuser).
    pub admin: bool,
}

impl Role {
    /// The built-in pg_ roles, hidden unless asked for.
    pub fn system(&self) -> bool {
        self.name.starts_with("pg_")
    }
}

/// The options of one membership. INHERIT and SET are per membership from PostgreSQL 16 and None before.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Opts {
    pub admin: bool,
    pub inherit: Option<bool>,
    pub set: Option<bool>,
}

impl Opts {
    /// A new membership as PostgreSQL 16 makes it by default, or the ADMIN-only one before.
    pub fn new(major: u32, inherit: bool) -> Opts {
        if major >= 16 { Opts { admin: false, inherit: Some(inherit), set: Some(true) } } else { Opts::default() }
    }

    fn text(o: &Option<Opts>) -> String {
        match o {
            None => "not a member".into(),
            Some(o) => {
                let mut v = vec![format!("admin {}", o.admin)];
                v.extend(o.inherit.map(|i| format!("inherit {i}")));
                v.extend(o.set.map(|s| format!("set {s}")));
                format!("a member ({})", v.join(", "))
            }
        }
    }
}

/// `member` is in `group`. PostgreSQL 16 keeps one row per grantor; they are merged here, an option held if any row holds it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Membership {
    pub group: String,
    pub member: String,
    pub opts: Opts,
    pub grantors: Vec<String>,
}

/// The server-wide part the window reads first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cluster {
    pub major: u32,
    pub me: String,
    pub roles: Vec<Role>,
    pub members: Vec<Membership>,
    /// Databases that can be connected to, for the database picker.
    pub databases: Vec<String>,
    /// How the server keeps a password; on md5 the dialogs say so and send the MD5 hash.
    pub password_encryption: String,
}

impl Cluster {
    pub fn role(&self, name: &str) -> Option<&Role> {
        self.roles.iter().find(|r| r.name == name)
    }

    pub fn me(&self) -> Option<&Role> {
        self.role(&self.me)
    }

    /// Whether memberships have INHERIT and SET options of their own (PostgreSQL 16 and later).
    pub fn options(&self) -> bool {
        self.major >= 16
    }

    fn me_is(&self, f: impl Fn(&Attrs) -> bool) -> bool {
        self.me().is_some_and(|r| f(&r.attrs))
    }

    /// Why privileges and roles cannot be changed from this login, whatever the lock says. Its own password and the members of
    /// roles it holds ADMIN on stay open to it, as on the server.
    pub fn read_only_reason(&self) -> Option<String> {
        if self.me_is(|a| a.superuser || a.createrole) {
            return None;
        }
        Some(format!("{} has neither CREATEROLE nor SUPERUSER, so roles can be read here but not changed, apart from its own password and the members of roles it holds ADMIN on. Unlocking the connection does not change that.", self.me))
    }

    /// Whether the signed-in role may ALTER `r` at all.
    pub fn may_alter(&self, r: &Role) -> Result<(), String> {
        if self.me_is(|a| a.superuser) {
            return Ok(());
        }
        if let Some(why) = self.read_only_reason() {
            return Err(why);
        }
        if r.attrs.superuser {
            return Err(format!("{} is a superuser, and only a superuser changes one.", r.name));
        }
        // Before 16 the server checks REPLICATION before CREATEROLE, so such a role is a superuser's alone.
        if !self.options() && r.attrs.replication {
            return Err(format!("{} has REPLICATION, and before PostgreSQL 16 only a superuser changes such a role.", r.name));
        }
        if self.options() && !r.admin {
            return Err(format!("{} has CREATEROLE but not the ADMIN option on {}, which PostgreSQL 16 and later require to change it.", self.me, r.name));
        }
        Ok(())
    }

    /// Whether the signed-in role may set the password of `r`. Every role may set its own, but before 16 not a REPLICATION one.
    pub fn may_password(&self, r: &Role) -> Result<(), String> {
        if self.me_is(|a| a.superuser) {
            return Ok(());
        }
        if !self.options() && r.attrs.replication {
            return Err(format!("{} has REPLICATION, and before PostgreSQL 16 only a superuser changes such a role, its password included.", r.name));
        }
        if r.name == self.me {
            return Ok(());
        }
        self.may_alter(r)
    }

    /// Why a change to the signed-in role itself is refused: taking away its own LOGIN, SUPERUSER or CREATEROLE, or a VALID UNTIL
    /// of -infinity, could leave it unable to sign in or to change roles, and so unable to undo it. A date in the past is the
    /// same; the run checks that one, since only the server reads the date.
    pub fn self_lockout(me: &str, c: &Change) -> Option<String> {
        let Change::Attrs { role, from, to } = c else { return None };
        if role != me {
            return None;
        }
        let gone: Vec<&str> = ["LOGIN", "SUPERUSER", "CREATEROLE"].into_iter().filter(|f| from.flag(f) && !to.flag(f)).collect();
        if !gone.is_empty() {
            return Some(format!("{me} is the role you are signed in as. Taking away its own {} could leave it unable to sign in or to change roles, and then nothing could undo it.", gone.join(" and ")));
        }
        if from.valid_until != to.valid_until && to.valid_until.trim().eq_ignore_ascii_case("-infinity") {
            return Some(format!("{me} is the role you are signed in as. A VALID UNTIL of -infinity would stop it signing in, and then nothing could undo it."));
        }
        None
    }

    /// Why a change to the signed-in role's own membership may take away the rights this login works with: the membership
    /// carries ADMIN, or leads to a role with SUPERUSER or CREATEROLE, or to one holding ADMIN on another role. Such a change runs
    /// only after a typed confirmation, and records no undo, since the undo may need the very rights it took.
    pub fn self_membership_risk(&self, c: &Change) -> Option<String> {
        let Change::Member { group, member, from: Some(f), to } = c else { return None };
        if *member != self.me {
            return None;
        }
        let less = match to {
            None => true,
            Some(t) => (f.admin && !t.admin) || (f.inherit == Some(true) && t.inherit == Some(false)) || (f.set == Some(true) && t.set == Some(false)),
        };
        if !less {
            return None;
        }
        // SET ROLE reaches every role up the chain whatever INHERIT says, so the whole chain counts.
        let mut reach = vec![group.clone()];
        let mut i = 0;
        while i < reach.len() {
            let cur = reach[i].clone();
            for m in self.members.iter().filter(|m| m.member == cur) {
                if !reach.contains(&m.group) {
                    reach.push(m.group.clone());
                }
            }
            i += 1;
        }
        let strong = reach.iter().filter_map(|r| self.role(r)).any(|r| r.attrs.superuser || r.attrs.createrole);
        let admin = f.admin || self.members.iter().any(|m| reach.contains(&m.member) && m.opts.admin);
        (strong || admin).then(|| format!("{} is the role you are signed in as, and its membership in {group} may give it the rights it uses to change roles. Once that is gone Quarry may not be able to undo it, so no undo is recorded.", self.me))
    }

    /// Whether the signed-in role may turn an attribute on or off on others.
    pub fn may_set_flag(&self, flag: &str) -> Result<(), String> {
        if self.me_is(|a| a.superuser) {
            return Ok(());
        }
        match flag {
            "SUPERUSER" => Err("Only a superuser gives or takes SUPERUSER.".into()),
            "REPLICATION" | "BYPASSRLS" if !self.options() => Err(format!("Only a superuser gives or takes {flag} before PostgreSQL 16.")),
            "REPLICATION" | "BYPASSRLS" | "CREATEDB" | "CREATEROLE" if self.options() && !self.me_is(|a| a.flag(flag)) => Err(format!("{} does not have {flag} itself, so PostgreSQL 16 and later do not let it give or take it.", self.me)),
            _ => Ok(()),
        }
    }

    /// Whether the signed-in role may add members to `group` or take them away. ADMIN on the group is enough on every version,
    /// CREATEROLE or not; before 16 CREATEROLE alone is too.
    pub fn may_grant_role(&self, group: &Role) -> Result<(), String> {
        if self.me_is(|a| a.superuser) {
            return Ok(());
        }
        if group.attrs.superuser {
            return Err(format!("{} is a superuser, and only a superuser grants it.", group.name));
        }
        if group.admin || (!self.options() && self.me_is(|a| a.createrole)) {
            return Ok(());
        }
        if self.options() {
            return Err(format!("{} needs the ADMIN option on {} to change its members on PostgreSQL 16 and later.", self.me, group.name));
        }
        Err(format!("{} needs CREATEROLE or the ADMIN option on {} to change its members.", self.me, group.name))
    }

    /// The memberships of `role` in other roles.
    pub fn member_of(&self, role: &str) -> Vec<&Membership> {
        self.members.iter().filter(|m| m.member == role).collect()
    }

    /// The members of `group`.
    pub fn members_of(&self, group: &str) -> Vec<&Membership> {
        self.members.iter().filter(|m| m.group == group).collect()
    }

    pub fn membership(&self, group: &str, member: &str) -> Option<Opts> {
        self.members.iter().find(|m| m.group == group && m.member == member).map(|m| m.opts)
    }

    /// The roles whose privileges `role` uses without SET ROLE, itself first, then PUBLIC as "". A membership passes them on when
    /// its INHERIT option is set (16 and later) or when the member has the INHERIT attribute (before).
    pub fn chain(&self, role: &str) -> Vec<String> {
        let mut out = vec![role.to_string()];
        if role.is_empty() {
            return out;
        }
        let mut i = 0;
        while i < out.len() {
            let cur = out[i].clone();
            let inherits = |m: &Membership| if self.options() { m.opts.inherit == Some(true) } else { self.role(&cur).is_some_and(|r| r.attrs.inherit) };
            for m in self.members.iter().filter(|m| m.member == cur && inherits(m)) {
                if !out.contains(&m.group) {
                    out.push(m.group.clone());
                }
            }
            i += 1;
        }
        out.push(String::new());
        out
    }
}

// Objects.

/// One grantor's grant to one grantee; `grantee` is empty for PUBLIC.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AclEntry {
    pub grantee: String,
    pub grantor: String,
    pub privs: Privs,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Obj {
    pub oid: String,
    pub schema: String,
    pub name: String,
    /// A function's arguments as `pg_get_function_identity_arguments` writes them, with an ordered-set aggregate's
    /// ORDER BY written as a comma: GRANT names a routine by its plain argument list and refuses ORDER BY there.
    pub sig: String,
    /// "view", "procedure" and so on, empty for a plain table or function.
    pub detail: String,
    pub owner: String,
    pub acl: Vec<AclEntry>,
}

/// Merges every grantor's grants to `grantee`; a privilege carries the grant option if any of them gave it.
fn merged(acl: &[AclEntry], grantee: &str) -> Privs {
    let mut out = Privs::new();
    for e in acl.iter().filter(|e| e.grantee == grantee) {
        for (p, g) in &e.privs {
            let v = out.entry(p.clone()).or_insert(false);
            *v |= *g;
        }
    }
    out
}

impl Obj {
    pub fn direct(&self, grantee: &str) -> Privs {
        merged(&self.acl, grantee)
    }

    pub fn target(&self, kind: Kind) -> Target {
        Target { kind, schema: self.schema.clone(), name: self.name.clone(), sig: self.sig.clone(), oid: self.oid.clone() }
    }

    /// The other grantors that gave `grantee` something, which a REVOKE from here does not reach.
    pub fn other_grantors(&self, grantee: &str, me: &str) -> Vec<String> {
        let mut v: Vec<String> = self.acl.iter().filter(|e| e.grantee == grantee && e.grantor != me && e.grantor != self.owner).map(|e| e.grantor.clone()).collect();
        v.dedup();
        v
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Objects {
    pub database: String,
    pub schema: String,
    pub kind: Kind,
    pub objs: Vec<Obj>,
    /// The schemas of the database, for the schema picker.
    pub schemas: Vec<String>,
    /// The database's oid, which a batch checks so it never runs on one made again under the name.
    pub db_oid: String,
    /// More objects than the matrix lists.
    pub cut: bool,
}

/// How one role stands on one object, for a matrix cell.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cell {
    pub owner: bool,
    pub direct: Privs,
    /// A privilege held only through another role, with that role ("" for PUBLIC).
    pub via: BTreeMap<String, String>,
}

/// `chain` is `Cluster::chain` of the role (or just [""] for the PUBLIC column).
pub fn cell(obj: &Obj, chain: &[String]) -> Cell {
    let role = chain.first().map(String::as_str).unwrap_or_default();
    let direct = obj.direct(role);
    let mut via = BTreeMap::new();
    for r in chain.iter().skip(1) {
        for p in obj.direct(r).into_keys() {
            if !direct.contains_key(&p) {
                via.entry(p).or_insert_with(|| r.clone());
            }
        }
    }
    Cell { owner: !role.is_empty() && obj.owner == role, direct, via }
}

/// One row of `pg_default_acl`: what `owner` gives on what it creates (in `schema`, or everywhere when empty).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DefaultAcl {
    pub owner: String,
    pub schema: String,
    pub kind: DefKind,
    pub acl: Vec<AclEntry>,
}

impl DefaultAcl {
    pub fn granted(&self, grantee: &str) -> Privs {
        merged(&self.acl, grantee)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Defaults {
    pub database: String,
    pub db_oid: String,
    pub rows: Vec<DefaultAcl>,
    pub schemas: Vec<String>,
}

// The window's answers.

#[derive(Clone, Debug, PartialEq)]
pub enum Did {
    Applied,
    Undone,
    Created(String),
    Dropped(String),
    Password,
}

pub enum Answer {
    Cluster(Res<Cluster>),
    Objects(Res<Objects>),
    Defaults(Res<Defaults>),
    DropInfo(Res<DropInfo>),
    /// A write finished. `undo` is the entry to record, or the one a refused undo hands back.
    Done { did: Did, res: Res<String>, undo: Option<UndoAction> },
}

/// What Recent changes keeps for an undo.
#[derive(Clone, Debug, PartialEq)]
pub enum RolesUndo {
    Batch { batch: Batch, label: String },
    Created(Made),
}

impl RolesUndo {
    pub fn label(&self) -> String {
        match self {
            RolesUndo::Batch { label, .. } => label.clone(),
            RolesUndo::Created(m) => format!("created role {}", m.name),
        }
    }
}

fn entry(conn: u64, database: &str, u: RolesUndo) -> UndoAction {
    UndoAction { agent: None, at: Instant::now(), conn, db: database.into(), coll: String::new(), kind: UndoKind::Sql { database: database.into(), cols: Vec::new(), undo: SqlUndo::PgRoles(Box::new(u)), ids: Default::default() } }
}

/// `req` is the Apply's request id, so a late answer cannot clear changes staged after it; 0 for the others.
fn done(conn: u64, req: u64, did: Did, res: Res<String>, undo: Option<UndoAction>) -> Msg {
    Msg::PgRoles { conn, req, ans: Answer::Done { did, res, undo } }
}

impl Worker {
    /// Roles, memberships and databases, read on the connection's own database.
    pub fn pg_roles_cluster(&self, conn: u64, req: u64, db: Db) {
        self.spawn_sql(db.conn, "", async move { Msg::PgRoles { conn, req, ans: Answer::Cluster(load_cluster(&db).await) } });
    }

    /// The objects of one kind with their ACLs, read in `database` (and `schema` for the kinds that sit in one).
    pub fn pg_roles_objects(&self, conn: u64, req: u64, db: Db, database: String, schema: String, kind: Kind) {
        self.spawn_sql(db.conn, &schema.clone(), async move { Msg::PgRoles { conn, req, ans: Answer::Objects(load_objects(&db.at(&database), &schema, kind).await) } });
    }

    pub fn pg_roles_defaults(&self, conn: u64, req: u64, db: Db, database: String) {
        self.spawn_sql(db.conn, "", async move { Msg::PgRoles { conn, req, ans: Answer::Defaults(load_defaults(&db.at(&database)).await) } });
    }

    /// Runs a batch on its database; the undo entry holds it inverted, without the changes confirmed as having none.
    pub fn pg_roles_apply(&self, conn: u64, req: u64, db: Db, batch: Batch, read_only: bool) {
        self.spawn_sql(db.conn, "", async move {
            let at = db.at(&batch.database);
            match run_batch(&at, batch, read_only, false).await {
                Ok(ran) => {
                    let label = ran.label();
                    let back = ran.inverse();
                    let note = if ran.no_undo.is_empty() { "" } else { " (no undo recorded for the change to your own membership)" };
                    let undo = (!back.changes.is_empty()).then(|| entry(conn, &ran.database, RolesUndo::Batch { batch: back, label: label.clone() }));
                    done(conn, req, Did::Applied, Ok(format!("Applied: {label}{note}")), undo)
                }
                Err(e) => done(conn, req, Did::Applied, Err(e), None),
            }
        });
    }

    /// CREATE ROLE. The password lives in this task only, wiped once it is done, and only its verifier is sent.
    pub fn pg_role_create(&self, conn: u64, db: Db, n: NewRole, password: String, read_only: bool) {
        let password = zeroize::Zeroizing::new(password);
        self.spawn_sql(db.conn, "", async move {
            let name = n.name.clone();
            match create_role(&db, &n, &password, read_only).await {
                Ok(m) => done(conn, 0, Did::Created(name.clone()), Ok(format!("Created role {name}")), Some(entry(conn, &db.pg_database(), RolesUndo::Created(m)))),
                Err(e) => done(conn, 0, Did::Created(name), Err(e), None),
            }
        });
    }

    /// ALTER ROLE ... PASSWORD with the password's hash. Nothing is recorded to undo it.
    pub fn pg_role_password(&self, conn: u64, db: Db, role: String, oid: String, password: String, read_only: bool) {
        let password = zeroize::Zeroizing::new(password);
        self.spawn_sql(db.conn, "", async move {
            let res = set_password(&db, &role, &oid, &password, read_only).await.map(|_| format!("Changed the password of {role}"));
            done(conn, 0, Did::Password, res, None)
        });
    }

    pub fn pg_role_drop_info(&self, conn: u64, req: u64, db: Db, role: String) {
        self.spawn_sql(db.conn, "", async move { Msg::PgRoles { conn, req, ans: Answer::DropInfo(drop_info(&db, &role).await) } });
    }

    pub fn pg_role_drop(&self, conn: u64, db: Db, info: DropInfo, mode: DropMode, read_only: bool) {
        self.spawn_sql(db.conn, "", async move {
            let role = info.role.clone();
            done(conn, 0, Did::Dropped(role), drop_role(&db, &info, &mode, read_only).await, None)
        });
    }
}

/// Runs an undo from Recent changes on the database it was recorded for (`db` is already that one). A refused undo changed
/// nothing, since every undo is one transaction, so the entry comes back to stay in the list.
pub(in crate::db::sql) async fn undo(db: &Db, a: UndoAction, u: RolesUndo, read_only: bool) -> Msg {
    let conn = a.conn;
    let res = match &u {
        RolesUndo::Batch { batch, label } => run_batch(db, batch.clone(), read_only, true).await.map(|_| format!("Undid: {label}")),
        RolesUndo::Created(m) => undo_created(db, m, read_only).await.map(|_| format!("Dropped role {}", m.name)),
    };
    let keep = res.is_err().then_some(a);
    done(conn, 0, Did::Undone, res, keep)
}

#[cfg(test)]
#[path = "roles_tests.rs"]
mod tests;
