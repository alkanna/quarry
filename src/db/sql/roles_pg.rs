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

use super::create_pg::read;
use super::ddl_pg::{commit_unknown, exec, name_ok, q, set_lock_wait};
use super::exec::{SqlRow, Tx};
use super::value::{pg_lit, Param};
use super::worker::text;
use super::write::{writer_conn, SqlUndo, LOCKED};
use super::{se, Db};
use crate::db::{Msg, Res, UndoAction, UndoKind, Worker};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

// Kinds and privileges.

/// What the Privileges matrix lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
    /// A function's arguments as `pg_get_function_identity_arguments` writes them.
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

// Statements.

/// What a GRANT names: the object and, for a function, its arguments. `oid` tells it from one made again under the name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Target {
    pub kind: Kind,
    pub schema: String,
    pub name: String,
    pub sig: String,
    pub oid: String,
}

impl Target {
    /// The arguments come from the server's catalog, and the run checks they still read the same for that oid before using them.
    pub fn sql(&self) -> String {
        match self.kind {
            Kind::Tables | Kind::Sequences => format!("{} {}.{}", self.kind.word(), q(&self.schema), q(&self.name)),
            Kind::Functions => format!("ROUTINE {}.{}({})", q(&self.schema), q(&self.name), self.sig),
            Kind::Schemas | Kind::Databases => format!("{} {}", self.kind.word(), q(&self.name)),
        }
    }

    pub fn shown(&self) -> String {
        match self.kind {
            Kind::Tables | Kind::Sequences => format!("{}.{}", self.schema, self.name),
            Kind::Functions => format!("{}.{}({})", self.schema, self.name, self.sig),
            Kind::Schemas | Kind::Databases => format!("{} {}", self.kind.noun(), self.name),
        }
    }
}

fn grantee_sql(g: &str) -> String {
    if g.is_empty() { "PUBLIC".into() } else { q(g) }
}

fn grantee_shown(g: &str) -> &str {
    if g.is_empty() { "PUBLIC" } else { g }
}

/// The GRANT and REVOKE statements that turn `from` into `to`. `head` comes before the verb (ALTER DEFAULT PRIVILEGES ...), `on`
/// is the ON clause. Revokes go first, so a privilege moved from plain to grant option never sits both ways at once.
fn acl_stmts(head: &str, on: &str, grantee: &str, from: &Privs, to: &Privs, allowed: &[&str]) -> Res<Vec<String>> {
    for p in from.keys().chain(to.keys()) {
        if !allowed.contains(&p.as_str()) {
            return Err(format!("{p} is not a privilege of {on} here."));
        }
    }
    if grantee.is_empty() && to.values().any(|g| *g) {
        return Err("PUBLIC cannot hold a grant option.".into());
    }
    // In the order of `allowed`, so the statements read the way the editor lists the boxes.
    let pick = |f: &dyn Fn(&str) -> bool| allowed.iter().copied().filter(|p| f(p)).collect::<Vec<_>>();
    let (fg, tg) = (|p: &str| from.get(p).copied(), |p: &str| to.get(p).copied());
    let revoke = pick(&|p| fg(p).is_some() && tg(p).is_none());
    let revoke_go = pick(&|p| fg(p) == Some(true) && tg(p) == Some(false));
    let grant = pick(&|p| fg(p).is_none() && tg(p) == Some(false));
    let grant_go = pick(&|p| fg(p) != Some(true) && tg(p) == Some(true));
    let g = grantee_sql(grantee);
    let mut out = Vec::new();
    if !revoke.is_empty() {
        out.push(format!("{head}REVOKE {} {on} FROM {g}", revoke.join(", ")));
    }
    if !revoke_go.is_empty() {
        out.push(format!("{head}REVOKE GRANT OPTION FOR {} {on} FROM {g}", revoke_go.join(", ")));
    }
    if !grant.is_empty() {
        out.push(format!("{head}GRANT {} {on} TO {g}", grant.join(", ")));
    }
    if !grant_go.is_empty() {
        out.push(format!("{head}GRANT {} {on} TO {g} WITH GRANT OPTION", grant_go.join(", ")));
    }
    Ok(out)
}

fn check_limit(n: i64) -> Res<()> {
    if n < -1 || n > i32::MAX as i64 {
        return Err("The connection limit is a whole number, -1 for none.".into());
    }
    Ok(())
}

/// ALTER ROLE with the fields that differ, or None.
pub fn attrs_sql(role: &str, from: &Attrs, to: &Attrs) -> Res<Option<String>> {
    let mut parts: Vec<String> = FLAGS.iter().filter(|(f, _)| from.flag(f) != to.flag(f)).map(|(f, _)| if to.flag(f) { f.to_string() } else { format!("NO{f}") }).collect();
    if from.conn_limit != to.conn_limit {
        check_limit(to.conn_limit)?;
        parts.push(format!("CONNECTION LIMIT {}", to.conn_limit));
    }
    if from.valid_until != to.valid_until {
        // There is no VALID UNTIL NULL; infinity reads back as never.
        parts.push(format!("VALID UNTIL {}", pg_lit(if to.valid_until.trim().is_empty() { "infinity" } else { to.valid_until.trim() })));
    }
    Ok((!parts.is_empty()).then(|| format!("ALTER ROLE {} WITH {}", q(role), parts.join(" "))))
}

fn tf(b: bool) -> &'static str {
    if b { "TRUE" } else { "FALSE" }
}

/// The statements that take a membership from `from` to `to`. From 16 a GRANT on an existing membership sets the options it names.
pub fn member_sql(major: u32, group: &str, member: &str, from: &Option<Opts>, to: &Option<Opts>) -> Vec<String> {
    let (g, m) = (q(group), q(member));
    match (from, to) {
        (None, Some(o)) if major >= 16 => vec![format!("GRANT {g} TO {m} WITH ADMIN {}, INHERIT {}, SET {}", tf(o.admin), tf(o.inherit.unwrap_or(true)), tf(o.set.unwrap_or(true)))],
        (None, Some(o)) => vec![if o.admin { format!("GRANT {g} TO {m} WITH ADMIN OPTION") } else { format!("GRANT {g} TO {m}") }],
        (Some(_), None) => vec![format!("REVOKE {g} FROM {m}")],
        (Some(a), Some(b)) if a != b && major >= 16 => {
            let mut opts = Vec::new();
            if a.admin != b.admin {
                opts.push(format!("ADMIN {}", tf(b.admin)));
            }
            if a.inherit != b.inherit {
                opts.push(format!("INHERIT {}", tf(b.inherit.unwrap_or(true))));
            }
            if a.set != b.set {
                opts.push(format!("SET {}", tf(b.set.unwrap_or(true))));
            }
            vec![format!("GRANT {g} TO {m} WITH {}", opts.join(", "))]
        }
        (Some(a), Some(b)) if a.admin != b.admin => vec![if b.admin { format!("GRANT {g} TO {m} WITH ADMIN OPTION") } else { format!("REVOKE ADMIN OPTION FOR {g} FROM {m}") }],
        _ => Vec::new(),
    }
}

/// What each grantor gave, by the grantor's name. Memberships before PostgreSQL 16 keep one row whatever the grantor, keyed "".
pub type Grantors<T> = BTreeMap<String, T>;

/// What a change found before its statements and left after, grantor by grantor, recorded by the run so its undo finds and
/// restores exactly that. Merged, a second grantor's row would read the same as none, so an undo could leave it behind.
#[derive(Clone, Debug, PartialEq)]
pub enum Exact {
    Acl(Grantors<Privs>, Grantors<Privs>),
    Member(Grantors<Opts>, Grantors<Opts>),
}

impl Exact {
    fn swap(self) -> Exact {
        match self {
            Exact::Acl(a, b) => Exact::Acl(b, a),
            Exact::Member(a, b) => Exact::Member(b, a),
        }
    }
}

fn merge_privs(g: &Grantors<Privs>) -> Privs {
    let mut out = Privs::new();
    for (p, go) in g.values().flatten() {
        *out.entry(p.clone()).or_insert(false) |= *go;
    }
    out
}

fn merge_opts(g: &Grantors<Opts>) -> Option<Opts> {
    let or = |a: Option<bool>, b: Option<bool>| a.zip(b).map(|(a, b)| a || b).or(a).or(b);
    g.values().copied().reduce(|a, b| Opts { admin: a.admin || b.admin, inherit: or(a.inherit, b.inherit), set: or(a.set, b.set) })
}

/// The one grantor whose grants differ between `now` and `then`, as the privileges to go from and to. A GRANT or REVOKE from
/// this login reaches only the grant it makes itself, so an undo that would have to change two grantors' grants is refused.
fn one_grantor(now: &Grantors<Privs>, then: &Grantors<Privs>) -> Res<(Privs, Privs)> {
    let keys: BTreeSet<&String> = now.keys().chain(then.keys()).collect();
    let diff: Vec<&String> = keys.into_iter().filter(|g| now.get(*g) != then.get(*g)).collect();
    match diff.as_slice() {
        [] => Ok((Privs::new(), Privs::new())),
        [g] => Ok((now.get(*g).cloned().unwrap_or_default(), then.get(*g).cloned().unwrap_or_default())),
        many => Err(format!("The grants of {} would all have to change, and one login only reaches its own.", many.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" and "))),
    }
}

/// The statements that take a membership from `now` to `then` row by row. From 16 each names its grantor, so the row this app
/// made is the one revoked even when another grantor's row stays.
fn member_rows_sql(major: u32, group: &str, member: &str, now: &Grantors<Opts>, then: &Grantors<Opts>) -> Vec<String> {
    let by = |g: &str| if major >= 16 && !g.is_empty() { format!(" GRANTED BY {}", q(g)) } else { String::new() };
    let keys: BTreeSet<&String> = now.keys().chain(then.keys()).collect();
    let mut out = Vec::new();
    for g in keys {
        match (now.get(g), then.get(g)) {
            (Some(_), None) => out.push(format!("REVOKE {} FROM {}{}", q(group), q(member), by(g))),
            (a, Some(b)) if a != Some(b) => out.extend(member_sql(major, group, member, &a.copied(), &Some(*b)).into_iter().map(|s| format!("{s}{}", by(g)))),
            _ => {}
        }
    }
    out
}

/// The privileges an ACL change's statements go from and to: what the window showed, or for an undo the one grantor's grant
/// it restores.
fn acl_pair(c: &Change, exact: Option<&Exact>) -> Res<Option<(Privs, Privs)>> {
    Ok(match (c, exact) {
        (Change::Acl { .. }, Some(Exact::Acl(now, then))) => Some(one_grantor(now, then)?),
        (Change::Acl { from, to, .. }, _) => Some((from.clone(), to.clone())),
        _ => None,
    })
}

/// The statements of a change in a run, from its exact state when it has one.
fn stmts(c: &Change, exact: Option<&Exact>, major: u32) -> Res<Vec<String>> {
    match (c, exact) {
        (Change::Acl { target, grantee, .. }, Some(Exact::Acl(..))) => {
            let (from, to) = acl_pair(c, exact)?.unwrap_or_default();
            acl_stmts("", &format!("ON {}", target.sql()), grantee, &from, &to, &target.kind.privs(major))
        }
        (Change::Member { group, member, .. }, Some(Exact::Member(now, then))) => Ok(member_rows_sql(major, group, member, now, then)),
        _ => c.sql(major),
    }
}

/// One staged change. `from` is what the window showed, `to` what the change leaves.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Acl { target: Target, grantee: String, from: Privs, to: Privs },
    Attrs { role: String, from: Attrs, to: Attrs },
    Member { group: String, member: String, from: Option<Opts>, to: Option<Opts> },
    Default { owner: String, schema: String, kind: DefKind, grantee: String, from: Privs, to: Privs },
}

impl Change {
    pub fn inverse(&self) -> Change {
        match self.clone() {
            Change::Acl { target, grantee, from, to } => Change::Acl { target, grantee, from: to, to: from },
            Change::Attrs { role, from, to } => Change::Attrs { role, from: to, to: from },
            Change::Member { group, member, from, to } => Change::Member { group, member, from: to, to: from },
            Change::Default { owner, schema, kind, grantee, from, to } => Change::Default { owner, schema, kind, grantee, from: to, to: from },
        }
    }

    pub fn is_noop(&self) -> bool {
        match self {
            Change::Acl { from, to, .. } | Change::Default { from, to, .. } => from == to,
            Change::Attrs { from, to, .. } => from == to,
            Change::Member { from, to, .. } => from == to,
        }
    }

    /// Whether the change acts on objects of one database, so the batch must run there.
    pub fn in_database(&self) -> bool {
        matches!(self, Change::Default { .. }) || matches!(self, Change::Acl { target, .. } if target.kind != Kind::Databases)
    }

    pub fn sql(&self, major: u32) -> Res<Vec<String>> {
        match self {
            Change::Acl { target, grantee, from, to } => acl_stmts("", &format!("ON {}", target.sql()), grantee, from, to, &target.kind.privs(major)),
            Change::Attrs { role, from, to } => Ok(attrs_sql(role, from, to)?.into_iter().collect()),
            Change::Member { group, member, from, to } => Ok(member_sql(major, group, member, from, to)),
            Change::Default { owner, schema, kind, grantee, from, to } => {
                if *kind == DefKind::Schemas && !schema.is_empty() {
                    return Err("Default privileges on schemas apply to the whole database, not to one schema.".into());
                }
                let head = format!("ALTER DEFAULT PRIVILEGES FOR ROLE {} {}", q(owner), if schema.is_empty() { String::new() } else { format!("IN SCHEMA {} ", q(schema)) });
                acl_stmts(&head, &format!("ON {}", kind.word()), grantee, from, to, &kind.privs(major))
            }
        }
    }

    /// For the staged list and Recent changes.
    pub fn describe(&self) -> String {
        match self {
            Change::Acl { target, grantee, from, to } => privs_label(from, to, &target.shown(), grantee_shown(grantee)),
            Change::Attrs { role, from, to } => {
                let mut v: Vec<String> = FLAGS.iter().filter(|(f, _)| from.flag(f) != to.flag(f)).map(|(f, _)| if to.flag(f) { f.to_string() } else { format!("NO{f}") }).collect();
                if from.conn_limit != to.conn_limit {
                    v.push(format!("connection limit {}", to.conn_limit));
                }
                if from.valid_until != to.valid_until {
                    v.push(format!("valid until {}", until_shown(&to.valid_until)));
                }
                format!("set {} on role {role}", v.join(", "))
            }
            Change::Member { group, member, from, to } => match (from, to) {
                (None, Some(_)) => format!("added {member} to {group}"),
                (Some(_), None) => format!("removed {member} from {group}"),
                _ => format!("changed the options of {member} in {group}"),
            },
            Change::Default { owner, schema, kind, grantee, from, to } => {
                let place = if schema.is_empty() { String::new() } else { format!(" in {schema}") };
                privs_label(from, to, &format!("new {} of {owner}{place}", kind.label().to_lowercase()), grantee_shown(grantee))
            }
        }
    }

    fn roles(&self) -> Vec<&str> {
        let v = match self {
            Change::Acl { grantee, .. } => vec![grantee.as_str()],
            Change::Attrs { role, .. } => vec![role.as_str()],
            Change::Member { group, member, .. } => vec![group.as_str(), member.as_str()],
            Change::Default { owner, grantee, .. } => vec![owner.as_str(), grantee.as_str()],
        };
        v.into_iter().filter(|r| !r.is_empty()).collect()
    }
}

fn privs_label(from: &Privs, to: &Privs, on: &str, to_whom: &str) -> String {
    let added: Vec<String> = to.iter().filter(|(p, g)| from.get(*p) != Some(g) && (!from.contains_key(*p) || **g)).map(|(p, g)| if *g { format!("{p} with grant option") } else { p.clone() }).collect();
    let gone: Vec<String> = from.iter().filter(|(p, g)| !to.contains_key(*p) || (**g && to.get(*p) == Some(&false))).map(|(p, g)| if to.contains_key(p) && *g { format!("grant option for {p}") } else { p.clone() }).collect();
    match (added.is_empty(), gone.is_empty()) {
        (false, true) => format!("granted {} on {on} to {to_whom}", added.join(", ")),
        (true, false) => format!("revoked {} on {on} from {to_whom}", gone.join(", ")),
        _ => format!("granted {}, revoked {} on {on} for {to_whom}", added.join(", "), gone.join(", ")),
    }
}

/// The statements of a batch, for the staged bar.
pub fn preview(major: u32, changes: &[Change]) -> Res<String> {
    let mut out = Vec::new();
    for c in changes {
        for s in c.sql(major)? {
            out.push(format!("{s};"));
        }
    }
    Ok(out.join("\n"))
}

/// Changes staged together and run in one transaction on `database`. `roles` are the oids the window read for each role named, so
/// a role dropped and made again under its name stops the run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Batch {
    pub database: String,
    pub db_oid: String,
    pub changes: Vec<Change>,
    pub roles: Vec<(String, String)>,
    /// Per change, the exact state the run found and left, which the undo restores. Empty until the batch ran.
    pub exact: Vec<Option<Exact>>,
    /// The changes confirmed knowing their undo may be impossible, which the undo leaves out.
    pub no_undo: BTreeSet<usize>,
}

impl Batch {
    /// What undoes this batch: every change inverted, the last first, each with the exact state it restores.
    pub fn inverse(&self) -> Batch {
        let keep: Vec<usize> = (0..self.changes.len()).rev().filter(|i| !self.no_undo.contains(i)).collect();
        Batch {
            changes: keep.iter().map(|&i| self.changes[i].inverse()).collect(),
            exact: keep.iter().map(|&i| self.exact.get(i).cloned().flatten().map(Exact::swap)).collect(),
            no_undo: BTreeSet::new(),
            ..self.clone()
        }
    }

    pub fn label(&self) -> String {
        match self.changes.as_slice() {
            [one] => one.describe(),
            many => format!("{} role and privilege changes", many.len()),
        }
    }
}

// New roles and passwords.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NewRole {
    pub name: String,
    pub attrs: Attrs,
    /// Roles the new one joins (IN ROLE).
    pub groups: Vec<String>,
}

pub fn check_name(name: &str) -> Res<()> {
    name_ok(name, "The role")?;
    if name.starts_with("pg_") {
        return Err("Role names starting with pg_ are reserved for the server's own roles.".into());
    }
    if name.eq_ignore_ascii_case("public") || name.eq_ignore_ascii_case("none") || name.eq_ignore_ascii_case("current_user") || name.eq_ignore_ascii_case("session_user") || name.eq_ignore_ascii_case("current_role") {
        return Err(format!("{name} is a word PostgreSQL keeps for itself, not a role name."));
    }
    Ok(())
}

/// The password stands in as `secret`: the SCRAM verifier when it runs, '***' in a preview, nothing when there is none.
pub fn create_sql(n: &NewRole, secret: Option<&str>) -> Res<String> {
    check_name(&n.name)?;
    check_limit(n.attrs.conn_limit)?;
    let a = &n.attrs;
    let mut parts = vec![if a.login { "LOGIN" } else { "NOLOGIN" }.to_string(), if a.inherit { "INHERIT" } else { "NOINHERIT" }.to_string()];
    for f in ["SUPERUSER", "CREATEDB", "CREATEROLE", "REPLICATION", "BYPASSRLS"] {
        if a.flag(f) {
            parts.push(f.into());
        }
    }
    if a.conn_limit != -1 {
        parts.push(format!("CONNECTION LIMIT {}", a.conn_limit));
    }
    if !a.valid_until.trim().is_empty() {
        parts.push(format!("VALID UNTIL {}", pg_lit(a.valid_until.trim())));
    }
    if let Some(s) = secret {
        parts.push(if s == REDACTED { format!("PASSWORD {REDACTED}") } else { format!("PASSWORD {}", pg_lit(s)) });
    }
    if !n.groups.is_empty() {
        parts.push(format!("IN ROLE {}", n.groups.iter().map(|g| q(g)).collect::<Vec<_>>().join(", ")));
    }
    Ok(format!("CREATE ROLE {} WITH {}", q(&n.name), parts.join(" ")))
}

pub const REDACTED: &str = "'***'";

pub fn password_sql(role: &str, secret: &str) -> String {
    if secret == REDACTED { format!("ALTER ROLE {} PASSWORD {REDACTED}", q(role)) } else { format!("ALTER ROLE {} PASSWORD {}", q(role), pg_lit(secret)) }
}

pub fn check_password(p: &str) -> Res<()> {
    if p.is_empty() {
        return Err("Type a password.".into());
    }
    if p.contains('\0') {
        return Err("A password cannot hold a NUL character.".into());
    }
    Ok(())
}

/// Whether password_encryption tells the server to keep MD5 hashes, so a password goes out as one; PostgreSQL 13 still reads
/// "on" as md5. A SCRAM verifier there would be stored all the same, against what the server was set to keep.
pub fn wants_md5(setting: &str) -> bool {
    ["md5", "on", "true", "yes", "1"].iter().any(|v| setting.trim().eq_ignore_ascii_case(v))
}

/// The MD5 hash PostgreSQL stores for `password` of `role`: "md5" and the hex MD5 of the password then the role name, with no
/// SASLprep.
pub fn md5_verifier(password: &str, role: &str) -> String {
    let mut data = zeroize::Zeroizing::new(Vec::with_capacity(password.len() + role.len()));
    data.extend_from_slice(password.as_bytes());
    data.extend_from_slice(role.as_bytes());
    format!("md5{:x}", md5::compute(&*data))
}

/// The SCRAM iterations to compute, from the server's scram_iterations (absent before PostgreSQL 16, which uses 4096). Fewer than
/// 4096 is weaker than any server default, and more than ten million would hold a worker thread for minutes.
pub fn check_iterations(setting: &str) -> Res<u32> {
    match setting.trim().parse::<u32>() {
        Ok(n) if (4096..=10_000_000).contains(&n) => Ok(n),
        _ => Err(format!("The server asks for {} SCRAM iterations (scram_iterations), and Quarry computes from 4096 to 10000000. Nothing was changed.", setting.trim())),
    }
}

fn hmac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    use zeroize::Zeroize;
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let (mut ipad, mut opad) = ([0x36u8; 64], [0x5cu8; 64]);
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    for p in parts {
        inner.update(p);
    }
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    let mut out = [0u8; 32];
    out.copy_from_slice(&outer.finalize());
    // The padded key is the password itself for the first rounds.
    k.zeroize();
    ipad.zeroize();
    opad.zeroize();
    out
}

fn b64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for ch in data.chunks(3) {
        let n = (ch[0] as u32) << 16 | (*ch.get(1).unwrap_or(&0) as u32) << 8 | *ch.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= ch.len() { A[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// SASLprep the way PostgreSQL's pg_saslprep runs it, both in the server on a password given in clear and in libpq when it signs
/// in. None means the password is used as it is: an ASCII one, one left empty by the mapping, or one with a prohibited or
/// unassigned (in Unicode 3.2) character or a bad mix of directions. As in PostgreSQL, those checks read the mapped text before
/// normalization, where RFC 4013 reads it after.
fn pg_saslprep(p: &str) -> Option<zeroize::Zeroizing<String>> {
    use stringprep::tables as tb;
    use unicode_normalization::UnicodeNormalization;
    if p.is_ascii() {
        return None;
    }
    let mapped = zeroize::Zeroizing::new(p.chars().filter_map(|c| if tb::non_ascii_space_character(c) { Some(' ') } else if tb::commonly_mapped_to_nothing(c) { None } else { Some(c) }).collect::<Vec<char>>());
    let (first, last) = (*mapped.first()?, *mapped.last()?);
    let prohibited = |c: char| {
        tb::non_ascii_space_character(c)
            || tb::ascii_control_character(c)
            || tb::non_ascii_control_character(c)
            || tb::private_use(c)
            || tb::non_character_code_point(c)
            || tb::surrogate_code(c)
            || tb::inappropriate_for_plain_text(c)
            || tb::inappropriate_for_canonical_representation(c)
            || tb::change_display_properties_or_deprecated(c)
            || tb::tagging_character(c)
            || tb::unassigned_code_point(c)
    };
    if mapped.iter().any(|&c| prohibited(c)) {
        return None;
    }
    if mapped.iter().any(|&c| tb::bidi_r_or_al(c)) && (mapped.iter().any(|&c| tb::bidi_l(c)) || !tb::bidi_r_or_al(first) || !tb::bidi_r_or_al(last)) {
        return None;
    }
    Some(zeroize::Zeroizing::new(mapped.iter().copied().nfkc().collect()))
}

/// The SCRAM-SHA-256 verifier PostgreSQL stores for `password` (RFC 5802 and 7677), as the server computes it from a password
/// given in clear: SASLprep first, as `pg_saslprep` does it, so libpq signs in with the same key.
pub fn scram_verifier(password: &str, salt: &[u8], iterations: u32) -> String {
    use zeroize::Zeroize;
    let prepped = pg_saslprep(password);
    let pw = prepped.as_deref().map_or(password.as_bytes(), |p| p.as_bytes());
    let mut u = hmac(pw, &[salt, &1u32.to_be_bytes()]);
    let mut salted = u;
    for _ in 1..iterations.max(1) {
        u = hmac(pw, &[&u]);
        for (s, x) in salted.iter_mut().zip(u.iter()) {
            *s ^= x;
        }
    }
    use sha2::Digest;
    let mut client = hmac(&salted, &[b"Client Key"]);
    let stored = sha2::Sha256::digest(client);
    let server = hmac(&salted, &[b"Server Key"]);
    // The salted password and the client key are each enough to sign in.
    salted.zeroize();
    u.zeroize();
    client.zeroize();
    format!("SCRAM-SHA-256${}:{}${}:{}", iterations.max(1), b64(salt), b64(&stored), b64(&server))
}

fn new_salt() -> [u8; 16] {
    use chacha20poly1305::{aead::Generate, Nonce};
    let (a, b) = (Nonce::generate(), Nonce::generate());
    let mut s = [0u8; 16];
    s[..8].copy_from_slice(&a[..8]);
    s[8..].copy_from_slice(&b[..8]);
    s
}

/// An error with the password and its verifier cut out, in case the server quotes the statement back.
fn scrub(err: String, secrets: &[&str]) -> String {
    let mut e = err;
    for s in secrets.iter().filter(|s| !s.is_empty()) {
        e = e.replace(s, "***");
        e = e.replace(&s.replace('\'', "''"), "***");
    }
    e
}

// Reading.

/// The moment a role expires, read back the same way whatever the session's time zone. to_char gives NULL for -infinity, which
/// would read as never, so both infinities are named.
const UNTIL: &str = "CASE WHEN r.rolvaliduntil IS NULL OR r.rolvaliduntil = 'infinity' THEN '' WHEN r.rolvaliduntil = '-infinity' THEN '-infinity' ELSE to_char(r.rolvaliduntil AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS') || '+00' END";

fn roles_sql(filter: &str) -> String {
    format!(
        "SELECT r.oid::text AS oid, r.rolname::text AS name, r.rolcanlogin::text AS login, r.rolsuper::text AS su, r.rolcreatedb::text AS cdb, r.rolcreaterole::text AS cr, r.rolinherit::text AS inh, \
         r.rolreplication::text AS repl, r.rolbypassrls::text AS bypass, r.rolconnlimit::text AS lim, {UNTIL} AS until, pg_has_role(current_user, r.oid, 'MEMBER WITH ADMIN OPTION')::text AS adm \
         FROM pg_roles r {filter} ORDER BY r.rolname"
    )
}

fn members_sql(major: u32) -> String {
    let (inh, set) = if major >= 16 { ("a.inherit_option::text", "a.set_option::text") } else { ("NULL::text", "NULL::text") };
    format!(
        "SELECT g.rolname::text AS grp, m.rolname::text AS member, COALESCE(gr.rolname::text, '') AS grantor, a.admin_option::text AS adm, {inh} AS inh, {set} AS setopt \
         FROM pg_auth_members a JOIN pg_roles g ON g.oid = a.roleid JOIN pg_roles m ON m.oid = a.member LEFT JOIN pg_roles gr ON gr.oid = a.grantor ORDER BY 1, 2, 3"
    )
}

fn t(r: &SqlRow, c: &str) -> String {
    text(r, c).unwrap_or_default()
}

fn b(r: &SqlRow, c: &str) -> bool {
    matches!(text(r, c).as_deref(), Some("true" | "t"))
}

fn ob(r: &SqlRow, c: &str) -> Option<bool> {
    text(r, c).map(|v| v == "true" || v == "t")
}

fn attrs_of(r: &SqlRow) -> Attrs {
    Attrs {
        login: b(r, "login"),
        superuser: b(r, "su"),
        createdb: b(r, "cdb"),
        createrole: b(r, "cr"),
        inherit: b(r, "inh"),
        replication: b(r, "repl"),
        bypassrls: b(r, "bypass"),
        conn_limit: t(r, "lim").parse().unwrap_or(-1),
        valid_until: t(r, "until"),
    }
}

fn memberships(rows: &[SqlRow]) -> Vec<Membership> {
    let mut out: Vec<Membership> = Vec::new();
    for r in rows {
        let (g, m) = (t(r, "grp"), t(r, "member"));
        let o = Opts { admin: b(r, "adm"), inherit: ob(r, "inh"), set: ob(r, "setopt") };
        match out.iter_mut().find(|x| x.group == g && x.member == m) {
            Some(x) => {
                x.opts.admin |= o.admin;
                x.opts.inherit = x.opts.inherit.zip(o.inherit).map(|(a, b)| a || b).or(o.inherit);
                x.opts.set = x.opts.set.zip(o.set).map(|(a, b)| a || b).or(o.set);
                x.grantors.push(t(r, "grantor"));
            }
            None => out.push(Membership { group: g, member: m, opts: o, grantors: vec![t(r, "grantor")] }),
        }
    }
    out
}

/// 17 from "17.2" or "18beta1", the way `Db::version` holds it.
fn major_of_version(v: &str) -> u32 {
    v.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0)
}

fn major_of(version_num: &str) -> u32 {
    version_num.trim().parse::<u32>().map(|n| n / 10000).unwrap_or(0)
}

async fn load_cluster(db: &Db) -> Res<Cluster> {
    let head = read(db, "", &[("SELECT current_user::text AS me, current_setting('server_version_num') AS v, current_setting('password_encryption') AS pe", Vec::new())]).await?.remove(0);
    let r = head.first().ok_or("The server gave no answer")?;
    let (me, major, password_encryption) = (t(r, "me"), major_of(&t(r, "v")), t(r, "pe"));
    let (roles, members) = (roles_sql(""), members_sql(major));
    let mut all = read(db, "", &[(roles.as_str(), Vec::new()), (members.as_str(), Vec::new()), (super::pg::DB_LIST, Vec::new())]).await?;
    let dbs = all.pop().unwrap_or_default();
    let members = memberships(&all.pop().unwrap_or_default());
    let roles = all.pop().unwrap_or_default().iter().map(|r| Role { oid: t(r, "oid"), name: t(r, "name"), attrs: attrs_of(r), admin: b(r, "adm") }).collect();
    let databases = dbs.iter().filter_map(|r| super::worker::text_at(r, 0)).collect();
    Ok(Cluster { major, me, roles, members, databases, password_encryption })
}

/// The ACL of a row as JSON triples, NULL read as the defaults the owner gets.
fn acl_json(col: &str, ch: char, owner: &str) -> String {
    format!(
        "COALESCE((SELECT json_agg(json_build_array(CASE WHEN a.grantee = 0 THEN '' ELSE pg_get_userbyid(a.grantee)::text END, pg_get_userbyid(a.grantor)::text, a.privilege_type, a.is_grantable))::text \
         FROM aclexplode(COALESCE({col}, acldefault('{ch}', {owner}))) a), '[]')"
    )
}

fn parse_acl(json: &str) -> Vec<AclEntry> {
    let rows: Vec<(String, String, String, bool)> = serde_json::from_str(json).unwrap_or_default();
    let mut out: Vec<AclEntry> = Vec::new();
    for (grantee, grantor, p, g) in rows {
        match out.iter_mut().find(|e| e.grantee == grantee && e.grantor == grantor) {
            Some(e) => {
                e.privs.insert(p, g);
            }
            None => out.push(AclEntry { grantee, grantor, privs: BTreeMap::from([(p, g)]) }),
        }
    }
    out
}

/// How many objects the matrix lists at most; a schema with more says so.
pub const OBJ_LIMIT: usize = 2000;

const SCHEMAS: &str = "SELECT n.nspname::text AS name FROM pg_namespace n WHERE n.nspname !~ '^pg_' AND n.nspname <> 'information_schema' ORDER BY 1";
const DB_OID: &str = "SELECT oid::text AS o FROM pg_database WHERE datname = current_database()";

fn objects_sql(kind: Kind) -> String {
    let (_, col, owner, ch) = kind.catalog();
    let lim = OBJ_LIMIT + 1;
    match kind {
        Kind::Tables | Kind::Sequences => {
            let (kinds, detail) = if kind == Kind::Tables {
                ("'r','v','m','f','p'", "CASE c.relkind WHEN 'v' THEN 'view' WHEN 'm' THEN 'materialized view' WHEN 'f' THEN 'foreign table' WHEN 'p' THEN 'partitioned' ELSE '' END")
            } else {
                ("'S'", "''")
            };
            format!(
                "SELECT c.oid::text AS oid, c.relname::text AS name, '' AS sig, {detail} AS detail, pg_get_userbyid(c.{owner})::text AS owner, {} AS acl \
                 FROM pg_class c WHERE c.relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1) AND c.relkind IN ({kinds}) ORDER BY 2 LIMIT {lim}",
                acl_json(&format!("c.{col}"), ch, &format!("c.{owner}"))
            )
        }
        Kind::Functions => format!(
            "SELECT p.oid::text AS oid, p.proname::text AS name, pg_get_function_identity_arguments(p.oid) AS sig, CASE p.prokind WHEN 'p' THEN 'procedure' WHEN 'a' THEN 'aggregate' WHEN 'w' THEN 'window' ELSE '' END AS detail, \
             pg_get_userbyid(p.proowner)::text AS owner, {} AS acl FROM pg_proc p WHERE p.pronamespace = (SELECT oid FROM pg_namespace WHERE nspname = $1) ORDER BY 2, 3 LIMIT {lim}",
            acl_json("p.proacl", ch, "p.proowner")
        ),
        Kind::Schemas => format!(
            "SELECT n.oid::text AS oid, n.nspname::text AS name, '' AS sig, '' AS detail, pg_get_userbyid(n.nspowner)::text AS owner, {} AS acl \
             FROM pg_namespace n WHERE n.nspname !~ '^pg_' AND n.nspname <> 'information_schema' AND $1::text IS NOT NULL ORDER BY 2 LIMIT {lim}",
            acl_json("n.nspacl", ch, "n.nspowner")
        ),
        Kind::Databases => format!(
            "SELECT d.oid::text AS oid, d.datname::text AS name, '' AS sig, CASE WHEN d.datistemplate THEN 'template' ELSE '' END AS detail, pg_get_userbyid(d.datdba)::text AS owner, {} AS acl \
             FROM pg_database d WHERE d.datallowconn AND $1::text IS NOT NULL ORDER BY 2 LIMIT {lim}",
            acl_json("d.datacl", ch, "d.datdba")
        ),
    }
}

async fn load_objects(db: &Db, schema: &str, kind: Kind) -> Res<Objects> {
    let sql = objects_sql(kind);
    let mut all = read(db, schema, &[(DB_OID, Vec::new()), (SCHEMAS, Vec::new()), (sql.as_str(), vec![Param::Text(schema.into())])]).await?;
    let rows = all.pop().unwrap_or_default();
    let schemas = all.pop().unwrap_or_default().iter().map(|r| t(r, "name")).collect();
    let db_oid = all.pop().unwrap_or_default().first().map(|r| t(r, "o")).unwrap_or_default();
    let cut = rows.len() > OBJ_LIMIT;
    let objs = rows
        .iter()
        .take(OBJ_LIMIT)
        .map(|r| Obj { oid: t(r, "oid"), schema: if kind.in_schema() { schema.to_string() } else { String::new() }, name: t(r, "name"), sig: t(r, "sig"), detail: t(r, "detail"), owner: t(r, "owner"), acl: parse_acl(&t(r, "acl")) })
        .collect();
    Ok(Objects { database: db.pg_database(), schema: schema.into(), kind, objs, schemas, db_oid, cut })
}

const DEFAULTS: &str = "SELECT pg_get_userbyid(d.defaclrole)::text AS owner, COALESCE(n.nspname::text, '') AS schema, d.defaclobjtype::text AS kind, \
    COALESCE((SELECT json_agg(json_build_array(CASE WHEN a.grantee = 0 THEN '' ELSE pg_get_userbyid(a.grantee)::text END, pg_get_userbyid(a.grantor)::text, a.privilege_type, a.is_grantable))::text FROM aclexplode(d.defaclacl) a), '[]') AS acl \
    FROM pg_default_acl d LEFT JOIN pg_namespace n ON n.oid = d.defaclnamespace ORDER BY 1, 2, 3";

async fn load_defaults(db: &Db) -> Res<Defaults> {
    let mut all = read(db, "", &[(DB_OID, Vec::new()), (SCHEMAS, Vec::new()), (DEFAULTS, Vec::new())]).await?;
    let rows = all.pop().unwrap_or_default().iter().filter_map(|r| Some(DefaultAcl { owner: t(r, "owner"), schema: t(r, "schema"), kind: DefKind::of(&t(r, "kind"))?, acl: parse_acl(&t(r, "acl")) })).collect();
    let schemas = all.pop().unwrap_or_default().iter().map(|r| t(r, "name")).collect();
    let db_oid = all.pop().unwrap_or_default().first().map(|r| t(r, "o")).unwrap_or_default();
    Ok(Defaults { database: db.pg_database(), db_oid, rows, schemas })
}

// Running a batch.

const ROLE_OID: &str = "SELECT oid::text AS o FROM pg_roles WHERE rolname = $1";
/// The grantee as an oid, 0 for PUBLIC.
const GRANTEE: &str = "CASE WHEN $2 = '' THEN 0::oid ELSE (SELECT oid FROM pg_roles WHERE rolname = $2) END";

async fn rows(tx: &mut Tx<'_>, db: &Db, sql: &str, args: Vec<Param>) -> Res<Vec<SqlRow>> {
    let t0 = Instant::now();
    let r = tx.fetch_bound(sql, &args).await;
    db.note("", sql, t0, &r);
    r.map_err(se)
}

/// Privileges by grantor, from rows of `gr`, `p` and `g`.
fn grants_of(rows: &[SqlRow]) -> Grantors<Privs> {
    let mut out = Grantors::<Privs>::new();
    for r in rows {
        *out.entry(t(r, "gr")).or_default().entry(t(r, "p")).or_insert(false) |= b(r, "g");
    }
    out
}

/// Whether the object is still the one the window read: same oid, name and schema, and for a function the same arguments.
async fn target_ok(tx: &mut Tx<'_>, db: &Db, tg: &Target) -> Res<bool> {
    let (sql, args): (&str, Vec<Param>) = match tg.kind {
        Kind::Tables | Kind::Sequences => ("SELECT count(*)::text AS n FROM pg_class WHERE oid = $1::oid AND relname = $2 AND relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = $3)", vec![Param::Text(tg.oid.clone()), Param::Text(tg.name.clone()), Param::Text(tg.schema.clone())]),
        Kind::Functions => (
            "SELECT count(*)::text AS n FROM pg_proc WHERE oid = $1::oid AND proname = $2 AND pronamespace = (SELECT oid FROM pg_namespace WHERE nspname = $3) AND pg_get_function_identity_arguments(oid) = $4",
            vec![Param::Text(tg.oid.clone()), Param::Text(tg.name.clone()), Param::Text(tg.schema.clone()), Param::Text(tg.sig.clone())],
        ),
        Kind::Schemas => ("SELECT count(*)::text AS n FROM pg_namespace WHERE oid = $1::oid AND nspname = $2", vec![Param::Text(tg.oid.clone()), Param::Text(tg.name.clone())]),
        Kind::Databases => ("SELECT count(*)::text AS n FROM pg_database WHERE oid = $1::oid AND datname = $2", vec![Param::Text(tg.oid.clone()), Param::Text(tg.name.clone())]),
    };
    Ok(rows(tx, db, sql, args).await?.first().is_some_and(|r| t(r, "n") == "1"))
}

/// What a change looks at, read inside the transaction, grantor by grantor.
#[derive(Clone, Debug, PartialEq)]
enum State {
    Privs(Grantors<Privs>),
    Attrs(Attrs),
    Member(Grantors<Opts>),
}

async fn state(tx: &mut Tx<'_>, db: &Db, major: u32, c: &Change) -> Res<State> {
    match c {
        Change::Acl { target, grantee, .. } => {
            let (cat, col, owner, ch) = target.kind.catalog();
            let sql = format!("SELECT pg_get_userbyid(a.grantor)::text AS gr, a.privilege_type AS p, a.is_grantable::text AS g FROM {cat} c, aclexplode(COALESCE(c.{col}, acldefault('{ch}', c.{owner}))) a WHERE c.oid = $1::oid AND a.grantee = {GRANTEE}");
            Ok(State::Privs(grants_of(&rows(tx, db, &sql, vec![Param::Text(target.oid.clone()), Param::Text(grantee.clone())]).await?)))
        }
        Change::Attrs { role, .. } => {
            let sql = roles_sql("WHERE r.rolname = $1");
            let rs = rows(tx, db, &sql, vec![Param::Text(role.clone())]).await?;
            let r = rs.first().ok_or_else(|| format!("Role {role} is gone."))?;
            Ok(State::Attrs(attrs_of(r)))
        }
        Change::Member { group, member, .. } => {
            let (inh, set) = if major >= 16 { ("a.inherit_option::text", "a.set_option::text") } else { ("NULL::text", "NULL::text") };
            let sql = format!(
                "SELECT COALESCE(gr.rolname::text, '') AS gr, a.admin_option::text AS adm, {inh} AS inh, {set} AS setopt FROM pg_auth_members a LEFT JOIN pg_roles gr ON gr.oid = a.grantor \
                 WHERE a.roleid = (SELECT oid FROM pg_roles WHERE rolname = $1) AND a.member = (SELECT oid FROM pg_roles WHERE rolname = $2)"
            );
            let rs = rows(tx, db, &sql, vec![Param::Text(group.clone()), Param::Text(member.clone())]).await?;
            // Before 16 there is one row, and a GRANT WITH ADMIN OPTION may rewrite its grantor, which means nothing there.
            Ok(State::Member(rs.iter().map(|r| (if major >= 16 { t(r, "gr") } else { String::new() }, Opts { admin: b(r, "adm"), inherit: ob(r, "inh"), set: ob(r, "setopt") })).collect()))
        }
        Change::Default { owner, schema, kind, grantee, .. } => {
            // Without a row for the whole database, new objects get the built-in defaults; a schema's row only adds to them.
            let sql = format!(
                "WITH d AS (SELECT defaclacl FROM pg_default_acl WHERE defaclrole = (SELECT oid FROM pg_roles WHERE rolname = $1) AND defaclobjtype = $3 \
                 AND defaclnamespace = CASE WHEN $4 = '' THEN 0::oid ELSE (SELECT oid FROM pg_namespace WHERE nspname = $4) END) \
                 SELECT pg_get_userbyid(a.grantor)::text AS gr, a.privilege_type AS p, a.is_grantable::text AS g FROM aclexplode(COALESCE((SELECT defaclacl FROM d), CASE WHEN $4 = '' THEN acldefault('{}', (SELECT oid FROM pg_roles WHERE rolname = $1)) END)) a \
                 WHERE a.grantee = {GRANTEE}",
                kind.acl_char()
            );
            let args = vec![Param::Text(owner.clone()), Param::Text(grantee.clone()), Param::Text(kind.code().into()), Param::Text(schema.clone())];
            Ok(State::Privs(grants_of(&rows(tx, db, &sql, args).await?)))
        }
    }
}

impl State {
    fn text(&self) -> String {
        match self {
            State::Privs(p) => privs_text(&merge_privs(p)),
            State::Attrs(a) => a.text(),
            State::Member(o) => Opts::text(&merge_opts(o)),
        }
    }
}

/// Whether `now` is what `c` expects before (`before`) or after it runs: grantor by grantor when the run recorded its exact
/// state, else merged, as the window shows it.
fn holds(c: &Change, exact: Option<&Exact>, now: &State, before: bool) -> bool {
    match (c, exact, now) {
        (_, Some(Exact::Acl(a, b)), State::Privs(p)) => p == if before { a } else { b },
        (_, Some(Exact::Member(a, b)), State::Member(m)) => m == if before { a } else { b },
        (Change::Acl { from, to, .. } | Change::Default { from, to, .. }, None, State::Privs(p)) => merge_privs(p) == *if before { from } else { to },
        (Change::Attrs { from, to, .. }, None, State::Attrs(a)) => a.same_touched(if before { from } else { to }, from, to),
        (Change::Member { from, to, .. }, None, State::Member(m)) => merge_opts(m) == *if before { from } else { to },
        _ => false,
    }
}

fn exact_of(before: &State, after: &State) -> Option<Exact> {
    match (before, after) {
        (State::Privs(a), State::Privs(b)) => Some(Exact::Acl(a.clone(), b.clone())),
        (State::Member(a), State::Member(b)) => Some(Exact::Member(a.clone(), b.clone())),
        _ => None,
    }
}

/// What a change is about, for messages.
fn subject(c: &Change) -> String {
    match c {
        Change::Acl { target, grantee, .. } => format!("what {} holds on {}", grantee_shown(grantee), target.shown()),
        Change::Attrs { role, .. } => format!("role {role}"),
        Change::Member { group, member, .. } => format!("the membership of {member} in {group}"),
        Change::Default { owner, schema, kind, grantee, .. } => format!("the default privileges {owner} gives {} on new {}{}", grantee_shown(grantee), kind.label().to_lowercase(), if schema.is_empty() { String::new() } else { format!(" in {schema}") }),
    }
}

/// VALID UNTIL as typed, read by the server in this session and written back as `UNTIL` reads it, so the check after the run and
/// the undo compare like with like, with whether that moment has passed. A value the written form would not give back (a
/// fraction of a second, a year BC) is refused rather than changed.
async fn canonical_until(tx: &mut Tx<'_>, db: &Db, typed: &str) -> Res<(String, bool)> {
    let v = typed.trim();
    if v.is_empty() || v.eq_ignore_ascii_case("never") || v.eq_ignore_ascii_case("infinity") {
        return Ok((String::new(), false));
    }
    let sql = "WITH c AS (SELECT ts, CASE WHEN ts = 'infinity' THEN '' WHEN ts = '-infinity' THEN '-infinity' ELSE to_char(ts AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS') || '+00' END AS v FROM (SELECT $1::timestamptz AS ts) t) \
               SELECT v, (CASE WHEN v = '' THEN ts = 'infinity' ELSE v::timestamptz = ts END)::text AS same, (ts <= now())::text AS past FROM c";
    let rs = rows(tx, db, sql, vec![Param::Text(v.into())]).await.map_err(|e| format!("Valid until: {e}"))?;
    let r = rs.first().ok_or("The server gave no answer")?;
    if !b(r, "same") {
        return Err(format!("Valid until {v} reads as {}, which is not the same moment: Quarry keeps whole seconds, in UTC, of years AD. Nothing was changed.", until_shown(&t(r, "v"))));
    }
    Ok((t(r, "v"), b(r, "past")))
}

/// Why a table REVOKE in `c` is refused: PostgreSQL revokes a privilege, or its grant option, on every column of the table along
/// with the table's, and nothing here would give those column grants back.
async fn column_grants(tx: &mut Tx<'_>, db: &Db, c: &Change, exact: Option<&Exact>) -> Res<Option<String>> {
    let (Change::Acl { target, grantee, .. }, Some((from, to))) = (c, acl_pair(c, exact)?) else { return Ok(None) };
    if target.kind != Kind::Tables {
        return Ok(None);
    }
    // A privilege revoked, or only its grant option (true when `to` still holds it).
    let lost: Vec<(&String, bool)> = from.iter().filter(|(p, go)| to.get(*p).is_none_or(|g| **go && !*g)).map(|(p, _)| (p, to.contains_key(p))).collect();
    if lost.is_empty() {
        return Ok(None);
    }
    let sql = format!(
        "SELECT a.privilege_type AS p, bool_or(a.is_grantable)::text AS g, string_agg(DISTINCT att.attname::text, ', ') AS cols FROM pg_attribute att, aclexplode(att.attacl) a \
         WHERE att.attrelid = $1::oid AND att.attnum > 0 AND NOT att.attisdropped AND att.attacl IS NOT NULL AND a.grantee = {GRANTEE} GROUP BY 1 ORDER BY 1"
    );
    for r in rows(tx, db, &sql, vec![Param::Text(target.oid.clone()), Param::Text(grantee.clone())]).await? {
        let p = t(&r, "p");
        if lost.iter().any(|(l, only_go)| **l == p && (!only_go || b(&r, "g"))) {
            return Ok(Some(format!(
                "{} also holds {p} on columns of {} ({}). A REVOKE on the table takes those column grants too, and Quarry could not give them back. Revoke the column grants first, or keep {p}.",
                grantee_shown(grantee),
                target.shown(),
                t(&r, "cols")
            )));
        }
    }
    Ok(None)
}

async fn check_roles(tx: &mut Tx<'_>, db: &Db, roles: &[(String, String)], c: &[Change]) -> Res<()> {
    let named: BTreeSet<&str> = c.iter().flat_map(Change::roles).collect();
    for (name, oid) in roles.iter().filter(|(n, _)| named.contains(n.as_str())) {
        let now = rows(tx, db, ROLE_OID, vec![Param::Text(name.clone())]).await?.first().map(|r| t(r, "o"));
        match now {
            None => return Err(format!("Role {name} is gone.")),
            Some(o) if !oid.is_empty() && o != *oid => return Err(format!("Role {name} was dropped and made again since the window read it.")),
            _ => {}
        }
    }
    Ok(())
}

/// Runs a batch in one transaction: every check, then every statement, then every check of the result. `undoing` words the
/// refusals for an undo. Returns the batch as it ran, with VALID UNTIL written the way the server reads it back and the exact
/// state of each change, which its undo restores.
async fn run_batch(db: &Db, mut b: Batch, read_only: bool, undoing: bool) -> Res<Batch> {
    let major = major_of_version(&db.version);
    let mut conn = writer_conn(db, "", read_only).await?;
    let out = async {
        let mut tx = conn.begin_write(db).await.map_err(se)?;
        let res = async {
            set_lock_wait(&mut tx, db, "").await.map_err(se)?;
            let refuse = |why: String| if undoing { format!("Undo refused: {why} Nothing was changed.") } else { format!("{why} Nothing was changed: refresh and stage it again.") };
            if !b.db_oid.is_empty() {
                let now = rows(&mut tx, db, DB_OID, Vec::new()).await?.first().map(|r| t(r, "o")).unwrap_or_default();
                if now != b.db_oid {
                    return Err(refuse(format!("database {} was dropped and made again since.", b.database)));
                }
            }
            check_roles(&mut tx, db, &b.roles, &b.changes).await.map_err(&refuse)?;
            let me = rows(&mut tx, db, "SELECT current_user::text AS me", Vec::new()).await?.first().map(|r| t(r, "me")).unwrap_or_default();
            for c in b.changes.iter_mut() {
                if let Some(why) = Cluster::self_lockout(&me, c) {
                    return Err(refuse(why));
                }
                if let Change::Attrs { role, from, to } = c {
                    if from.valid_until != to.valid_until {
                        let (until, past) = canonical_until(&mut tx, db, &to.valid_until).await?;
                        if past && *role == me {
                            return Err(refuse(format!("{me} is the role you are signed in as. A VALID UNTIL that has passed would stop it signing in, and then nothing could undo it.")));
                        }
                        to.valid_until = until;
                    }
                }
            }
            let given = std::mem::take(&mut b.exact);
            let ex = |i: usize| given.get(i).and_then(Option::as_ref);
            let mut before = Vec::new();
            for (i, c) in b.changes.iter().enumerate() {
                if let Change::Acl { target, .. } = c {
                    if !target_ok(&mut tx, db, target).await? {
                        return Err(refuse(format!("{} was dropped, renamed or made again since.", target.shown())));
                    }
                }
                let now = state(&mut tx, db, major, c).await?;
                if !holds(c, ex(i), &now, true) {
                    let when = if undoing { "after this change ran" } else { "after the window read it" };
                    return Err(refuse(format!("{} changed {when} (it is {} now).", subject(c), now.text())));
                }
                if let Some(why) = column_grants(&mut tx, db, c, ex(i)).await? {
                    return Err(refuse(why));
                }
                before.push(now);
            }
            for (i, c) in b.changes.iter().enumerate() {
                for s in stmts(c, ex(i), major).map_err(&refuse)? {
                    exec(&mut tx, db, "", &s).await.map_err(se)?;
                }
            }
            let mut exact = Vec::new();
            for (i, c) in b.changes.iter().enumerate() {
                let now = state(&mut tx, db, major, c).await?;
                if !holds(c, ex(i), &now, false) {
                    return Err(format!(
                        "After the statements ran, {} was {} rather than what they were meant to leave. A grant made by another role is revoked only by that role, so it may still be there. Nothing was changed.",
                        subject(c),
                        now.text()
                    ));
                }
                exact.push(exact_of(&before[i], &now));
            }
            b.exact = exact;
            Ok(())
        }
        .await;
        match res {
            Ok(()) => tx.commit().await.map_err(|e| commit_unknown(e, "Refresh the window and check before running it again.")),
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }
    .await;
    let _ = conn.close().await;
    out.map(|_| b)
}

// Creating, dropping and passwords.

/// What goes out for `role`'s password, as the server's password_encryption asks: the SCRAM verifier, with the settings read
/// first and the hashing on a blocking thread, all before the write transaction opens, so neither holds a worker or the
/// transaction for the length of PBKDF2; or the MD5 hash.
async fn verifier(db: &Db, role: &str, password: &str) -> Res<zeroize::Zeroizing<String>> {
    const SETTINGS: &str = "SELECT current_setting('password_encryption') AS pe, COALESCE(current_setting('scram_iterations', true), '4096') AS n";
    let rs = read(db, "", &[(SETTINGS, Vec::new())]).await?.remove(0);
    let r = rs.first().ok_or("The server gave no answer")?;
    if wants_md5(&t(r, "pe")) {
        return Ok(zeroize::Zeroizing::new(md5_verifier(password, role)));
    }
    let n = check_iterations(&t(r, "n"))?;
    let (pw, salt) = (zeroize::Zeroizing::new(password.to_string()), new_salt());
    tokio::task::spawn_blocking(move || zeroize::Zeroizing::new(scram_verifier(&pw, &salt, n))).await.map_err(|e| format!("Computing the password's verifier failed: {e}"))
}

/// The server's error on a statement with a password, with the password and verifier cut out. A policy extension can refuse a
/// verifier where it wants the password in clear, so that is said.
fn password_err(e: String, secrets: &[&str]) -> String {
    let e = scrub(e, secrets);
    if e.to_lowercase().contains("password") || e.to_lowercase().contains("encrypt") {
        return format!("{e}. Quarry sends a password only as its hash, and a password policy extension such as credcheck may refuse one sent that way.");
    }
    e
}

/// How a role stood right after this app made it, which its undo must still find.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Made {
    pub name: String,
    pub oid: String,
    /// The roles it is in, and the roles in it (a CREATEROLE creator gets ADMIN on it from 16).
    pub groups: Vec<String>,
    pub members: Vec<String>,
}

const MADE_GROUPS: &str = "SELECT g.rolname::text AS n FROM pg_auth_members a JOIN pg_roles g ON g.oid = a.roleid WHERE a.member = $1::oid ORDER BY 1";
const MADE_MEMBERS: &str = "SELECT m.rolname::text AS n FROM pg_auth_members a JOIN pg_roles m ON m.oid = a.member WHERE a.roleid = $1::oid ORDER BY 1";
/// Whatever the role owns or holds in any database or on the server, and its per-role settings.
const HOLDS: &str = "SELECT ((SELECT count(*) FROM pg_shdepend WHERE refclassid = 'pg_authid'::regclass AND refobjid = $1::oid) + (SELECT count(*) FROM pg_db_role_setting WHERE setrole = $1::oid))::text AS n";

async fn names(tx: &mut Tx<'_>, db: &Db, sql: &str, oid: &str) -> Res<Vec<String>> {
    Ok(rows(tx, db, sql, vec![Param::Text(oid.into())]).await?.iter().map(|r| t(r, "n")).collect())
}

async fn in_write<T>(db: &Db, read_only: bool, body: impl AsyncFnOnce(&mut Tx<'_>) -> Res<T>) -> Res<T> {
    let mut conn = writer_conn(db, "", read_only).await?;
    let out = async {
        let mut tx = conn.begin_write(db).await.map_err(se)?;
        let res = async {
            set_lock_wait(&mut tx, db, "").await.map_err(se)?;
            body(&mut tx).await
        }
        .await;
        match res {
            Ok(v) => tx.commit().await.map(|_| v).map_err(|e| commit_unknown(e, "Refresh the window and check before running it again.")),
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }
    .await;
    let _ = conn.close().await;
    out
}

async fn create_role(db: &Db, n: &NewRole, password: &str, read_only: bool) -> Res<Made> {
    create_sql(n, None)?;
    if read_only {
        return Err(LOCKED.into());
    }
    let secret = if password.is_empty() { None } else { Some(verifier(db, &n.name, password).await?) };
    in_write(db, read_only, async |tx| {
        let sql = zeroize::Zeroizing::new(create_sql(n, secret.as_deref().map(String::as_str))?);
        exec(tx, db, "", &sql).await.map_err(|e| password_err(se(e), &[password, secret.as_deref().map(String::as_str).unwrap_or_default()]))?;
        let oid = rows(tx, db, ROLE_OID, vec![Param::Text(n.name.clone())]).await?.first().map(|r| t(r, "o")).ok_or("The new role cannot be read back")?;
        Ok(Made { name: n.name.clone(), groups: names(tx, db, MADE_GROUPS, &oid).await?, members: names(tx, db, MADE_MEMBERS, &oid).await?, oid })
    })
    .await
}

async fn undo_created(db: &Db, m: &Made, read_only: bool) -> Res<()> {
    in_write(db, read_only, async |tx| {
        let now = rows(tx, db, ROLE_OID, vec![Param::Text(m.name.clone())]).await?.first().map(|r| t(r, "o"));
        match now {
            None => return Err(format!("Undo refused: role {} is gone already.", m.name)),
            Some(o) if o != m.oid => return Err(format!("Undo refused: role {} was dropped and made again since, so it is not the role this created.", m.name)),
            _ => {}
        }
        let holds = rows(tx, db, HOLDS, vec![Param::Text(m.oid.clone())]).await?.first().map(|r| t(r, "n")).unwrap_or_default();
        if holds != "0" {
            return Err(format!("Not undone: {} owns objects, holds privileges or has settings now, so it is not dropped.", m.name));
        }
        if names(tx, db, MADE_GROUPS, &m.oid).await? != m.groups || names(tx, db, MADE_MEMBERS, &m.oid).await? != m.members {
            return Err(format!("Not undone: the memberships of {} changed since it was created, so it is not dropped.", m.name));
        }
        exec(tx, db, "", &format!("DROP ROLE {}", q(&m.name))).await.map_err(se)
    })
    .await
}

async fn set_password(db: &Db, role: &str, oid: &str, password: &str, read_only: bool) -> Res<()> {
    check_password(password)?;
    if read_only {
        return Err(LOCKED.into());
    }
    let secret = verifier(db, role, password).await?;
    in_write(db, read_only, async |tx| {
        let now = rows(tx, db, ROLE_OID, vec![Param::Text(role.into())]).await?.first().map(|r| t(r, "o"));
        if now.as_deref() != Some(oid) {
            return Err(format!("Role {role} is gone or was made again since the window read it. Nothing was changed."));
        }
        let sql = zeroize::Zeroizing::new(password_sql(role, &secret));
        exec(tx, db, "", &sql).await.map_err(|e| password_err(se(e), &[password, &secret]))
    })
    .await
}

/// What a role owns and holds in one database.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DbHold {
    pub database: String,
    /// What `pg_describe_object` calls each owned object.
    pub owned: Vec<String>,
    /// Privileges and default privileges held there.
    pub held: usize,
    /// Every pg_shdepend row of the role in this database as `classid:objid:objsubid:deptype`, sorted. The drop finds the same set
    /// again before it runs, so an object swapped for another between the dialog and the run stops it.
    pub deps: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DropInfo {
    pub role: String,
    pub oid: String,
    pub dbs: Vec<DbHold>,
    /// Databases (and tablespaces) the role owns: shared objects that DROP OWNED does not remove.
    pub shared: Vec<String>,
    /// Shared objects the role holds privileges on (a database's CONNECT, a tablespace, a parameter). DROP OWNED in any one
    /// database revokes them.
    pub shared_held: Vec<String>,
    /// The pg_shdepend rows of the shared objects, as in `DbHold::deps`.
    pub shared_deps: Vec<String>,
    /// Databases with something of the role that could not be read, with the reason.
    pub unread: Vec<(String, String)>,
}

impl DropInfo {
    pub fn owned_count(&self) -> usize {
        self.dbs.iter().map(|d| d.owned.len()).sum::<usize>() + self.shared.len()
    }
}

/// What to do with what the role owns before DROP ROLE.
#[derive(Clone, Debug, PartialEq)]
pub enum DropMode {
    Reassign(String),
    DropOwned,
}

/// The statements of a drop, per database ("" for the connection's own), DROP ROLE last.
pub fn drop_plan(info: &DropInfo, mode: &DropMode, home: &str) -> Res<Vec<(String, Vec<String>)>> {
    if let Some((d, why)) = info.unread.first() {
        return Err(format!("{} has objects or privileges in {d}, which could not be read ({why}), so Quarry cannot tell what dropping it would do.", info.role));
    }
    let r = q(&info.role);
    let steps = |to: &DropMode| match to {
        DropMode::Reassign(t) => vec![format!("REASSIGN OWNED BY {r} TO {}", q(t)), format!("DROP OWNED BY {r}")],
        DropMode::DropOwned => vec![format!("DROP OWNED BY {r}")],
    };
    if let DropMode::Reassign(t) = mode {
        if t.is_empty() || *t == info.role {
            return Err("Pick the role that gets the objects.".into());
        }
    }
    if *mode == DropMode::DropOwned && !info.shared.is_empty() {
        return Err(format!("{} owns {}, which DROP OWNED does not drop. Give the objects to another role, or drop that first.", info.role, info.shared.join(", ")));
    }
    let mut plan: Vec<(String, Vec<String>)> = info.dbs.iter().map(|d| (d.database.clone(), steps(mode))).collect();
    // A shared object is reached from any database: REASSIGN OWNED moves a database owned, DROP OWNED revokes a CONNECT held.
    if plan.is_empty() && (!info.shared.is_empty() || !info.shared_held.is_empty()) {
        plan.push((home.to_string(), steps(mode)));
    }
    plan.push((String::new(), vec![format!("DROP ROLE {r}")]));
    Ok(plan)
}

pub fn drop_preview(plan: &[(String, Vec<String>)]) -> String {
    let mut out = Vec::new();
    for (d, stmts) in plan {
        out.push(if d.is_empty() { "-- then, once".to_string() } else { format!("-- in {d}") });
        out.extend(stmts.iter().map(|s| format!("{s};")));
    }
    out.join("\n")
}

/// The role's pg_shdepend rows in this database (`{}` is `current_database()`) or on the server (`0`), with what each is.
fn hold_sql(dbid: &str) -> String {
    format!(
        "SELECT pg_describe_object(classid, objid, objsubid) AS what, deptype::text AS dep, classid::text || ':' || objid::text || ':' || objsubid::text || ':' || deptype::text AS k \
         FROM pg_shdepend WHERE dbid = {dbid} AND refclassid = 'pg_authid'::regclass AND refobjid = $1::oid"
    )
}

const IN_DB: &str = "(SELECT oid FROM pg_database WHERE datname = current_database())";
const HOLD_DBS: &str = "SELECT d.datname::text AS name FROM pg_shdepend s JOIN pg_database d ON d.oid = s.dbid WHERE s.refclassid = 'pg_authid'::regclass AND s.refobjid = $1::oid GROUP BY 1 ORDER BY 1";

fn deps_of(rs: &[SqlRow]) -> Vec<String> {
    let mut v: Vec<String> = rs.iter().map(|r| t(r, "k")).collect();
    v.sort();
    v
}

/// (owned, held) descriptions, each sorted.
fn split_hold(rs: &[SqlRow]) -> (Vec<String>, Vec<String>) {
    let (mut owned, mut held) = (Vec::new(), Vec::new());
    for r in rs {
        if t(r, "dep") == "o" { owned.push(t(r, "what")) } else { held.push(t(r, "what")) }
    }
    owned.sort();
    held.sort();
    (owned, held)
}

fn hold_of(database: &str, rs: &[SqlRow]) -> DbHold {
    let (owned, held) = split_hold(rs);
    DbHold { database: database.into(), owned, held: held.len(), deps: deps_of(rs) }
}

async fn drop_info(db: &Db, role: &str) -> Res<DropInfo> {
    let oid = read(db, "", &[(ROLE_OID, vec![Param::Text(role.into())])]).await?.remove(0).first().map(|r| t(r, "o")).ok_or_else(|| format!("Role {role} is gone."))?;
    let shared_sql = hold_sql("0");
    let mut all = read(db, "", &[(shared_sql.as_str(), vec![Param::Text(oid.clone())]), (HOLD_DBS, vec![Param::Text(oid.clone())])]).await?;
    let dbs = all.pop().unwrap_or_default();
    let shared_rows = all.pop().unwrap_or_default();
    let (shared, shared_held) = split_hold(&shared_rows);
    let mut info = DropInfo { role: role.into(), oid: oid.clone(), shared, shared_held, shared_deps: deps_of(&shared_rows), ..Default::default() };
    // Only the databases where the role has something are opened, each through its own pool.
    let in_db = hold_sql(IN_DB);
    for name in dbs.iter().map(|r| t(r, "name")) {
        match read(&db.at(&name), "", &[(in_db.as_str(), vec![Param::Text(oid.clone())])]).await {
            Ok(mut v) => info.dbs.push(hold_of(&name, &v.remove(0))),
            Err(e) => info.unread.push((name, e)),
        }
    }
    Ok(info)
}

/// Runs a drop: each database's statements in a transaction of their own on that database, after checking it holds what the
/// window showed, then DROP ROLE. The first step also checks the shared objects, which its DROP OWNED or REASSIGN OWNED reaches.
/// No undo: what DROP OWNED removed and the password are gone.
async fn drop_role(db: &Db, info: &DropInfo, mode: &DropMode, read_only: bool) -> Res<String> {
    let home = db.pg_database();
    let plan = drop_plan(info, mode, &home)?;
    let mut done: Vec<String> = Vec::new();
    let (in_db, shared_sql) = (hold_sql(IN_DB), hold_sql("0"));
    for (i, (database, stmts)) in plan.iter().enumerate() {
        let at = if database.is_empty() { db.clone() } else { db.at(database) };
        let want = info.dbs.iter().find(|d| d.database == *database).map(|d| &d.deps);
        let r = in_write(&at, read_only, async |tx| {
            let now = rows(tx, &at, ROLE_OID, vec![Param::Text(info.role.clone())]).await?.first().map(|r| t(r, "o"));
            if now.as_deref() != Some(info.oid.as_str()) {
                return Err(format!("Role {} is gone or was made again since the window read it.", info.role));
            }
            if let Some(want) = want {
                if deps_of(&rows(tx, &at, &in_db, vec![Param::Text(info.oid.clone())]).await?) != *want {
                    return Err(format!("What {} owns or holds in {database} changed since the window read it. Look again before dropping it.", info.role));
                }
            }
            if i == 0 && deps_of(&rows(tx, &at, &shared_sql, vec![Param::Text(info.oid.clone())]).await?) != info.shared_deps {
                return Err(format!("The databases, tablespaces or parameters {} owns or holds privileges on changed since the window read it. Look again before dropping it.", info.role));
            }
            for s in stmts {
                exec(tx, &at, "", s).await.map_err(se)?;
            }
            Ok(())
        })
        .await;
        if let Err(e) = r {
            let place = if database.is_empty() { String::new() } else { format!(" in {database}") };
            let before = if done.is_empty() { "Nothing was changed.".to_string() } else { format!("Already done in {}, which stays done. The role was not dropped.", done.join(", ")) };
            return Err(format!("{e} (stopped{place}). {before}"));
        }
        if !database.is_empty() {
            done.push(database.clone());
        }
    }
    Ok(format!("Dropped role {}", info.role))
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
    UndoAction { at: Instant::now(), conn, db: database.into(), coll: String::new(), kind: UndoKind::Sql { database: database.into(), cols: Vec::new(), undo: SqlUndo::PgRoles(Box::new(u)), ids: Default::default() } }
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
pub(super) async fn undo(db: &Db, a: UndoAction, u: RolesUndo, read_only: bool) -> Msg {
    let conn = a.conn;
    let res = match &u {
        RolesUndo::Batch { batch, label } => run_batch(db, batch.clone(), read_only, true).await.map(|_| format!("Undid: {label}")),
        RolesUndo::Created(m) => undo_created(db, m, read_only).await.map(|_| format!("Dropped role {}", m.name)),
    };
    let keep = res.is_err().then_some(a);
    done(conn, 0, Did::Undone, res, keep)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn privs(p: &[(&str, bool)]) -> Privs {
        p.iter().map(|(k, g)| (k.to_string(), *g)).collect()
    }

    fn table() -> Target {
        Target { kind: Kind::Tables, schema: "public".into(), name: "orders".into(), sig: String::new(), oid: "1".into() }
    }

    #[test]
    fn acl_statements_and_their_inverse() {
        let from = privs(&[("SELECT", false), ("UPDATE", true), ("DELETE", false)]);
        let to = privs(&[("SELECT", true), ("UPDATE", false), ("INSERT", false)]);
        let c = Change::Acl { target: table(), grantee: "app writer".into(), from, to };
        assert_eq!(
            c.sql(18).unwrap(),
            [
                "REVOKE DELETE ON TABLE \"public\".\"orders\" FROM \"app writer\"",
                "REVOKE GRANT OPTION FOR UPDATE ON TABLE \"public\".\"orders\" FROM \"app writer\"",
                "GRANT INSERT ON TABLE \"public\".\"orders\" TO \"app writer\"",
                "GRANT SELECT ON TABLE \"public\".\"orders\" TO \"app writer\" WITH GRANT OPTION",
            ]
        );
        assert_eq!(
            c.inverse().sql(18).unwrap(),
            [
                "REVOKE INSERT ON TABLE \"public\".\"orders\" FROM \"app writer\"",
                "REVOKE GRANT OPTION FOR SELECT ON TABLE \"public\".\"orders\" FROM \"app writer\"",
                "GRANT DELETE ON TABLE \"public\".\"orders\" TO \"app writer\"",
                "GRANT UPDATE ON TABLE \"public\".\"orders\" TO \"app writer\" WITH GRANT OPTION",
            ]
        );
        assert_eq!(c.inverse().inverse(), c);
    }

    #[test]
    fn privilege_names_come_from_the_lists_and_maintain_from_17() {
        let to = privs(&[("MAINTAIN", false)]);
        let c = Change::Acl { target: table(), grantee: "a".into(), from: Privs::new(), to };
        assert!(c.sql(16).is_err());
        assert_eq!(c.sql(17).unwrap(), ["GRANT MAINTAIN ON TABLE \"public\".\"orders\" TO \"a\""]);
        let bad = Change::Acl { target: table(), grantee: "a".into(), from: Privs::new(), to: privs(&[("SELECT; DROP TABLE x", false)]) };
        assert!(bad.sql(18).is_err());
        let public_go = Change::Acl { target: table(), grantee: String::new(), from: Privs::new(), to: privs(&[("SELECT", true)]) };
        assert!(public_go.sql(18).is_err());
        let public = Change::Acl { target: table(), grantee: String::new(), from: Privs::new(), to: privs(&[("SELECT", false)]) };
        assert_eq!(public.sql(18).unwrap(), ["GRANT SELECT ON TABLE \"public\".\"orders\" TO PUBLIC"]);
    }

    #[test]
    fn every_kind_names_its_object() {
        let f = Target { kind: Kind::Functions, schema: "s\"x".into(), name: "f".into(), sig: "a integer, text".into(), oid: "1".into() };
        assert_eq!(f.sql(), "ROUTINE \"s\"\"x\".\"f\"(a integer, text)");
        assert_eq!(Target { kind: Kind::Schemas, name: "rep".into(), ..Default::default() }.sql(), "SCHEMA \"rep\"");
        assert_eq!(Target { kind: Kind::Databases, name: "shop".into(), ..Default::default() }.sql(), "DATABASE \"shop\"");
        assert_eq!(Target { kind: Kind::Sequences, schema: "public".into(), name: "s".into(), ..Default::default() }.sql(), "SEQUENCE \"public\".\"s\"");
    }

    #[test]
    fn alter_role_writes_only_what_changed() {
        let from = Attrs { login: true, conn_limit: -1, ..Default::default() };
        let to = Attrs { login: true, createdb: true, inherit: false, conn_limit: 20, valid_until: "2026-12-31".into(), ..Default::default() };
        assert_eq!(attrs_sql("app", &from, &to).unwrap().unwrap(), "ALTER ROLE \"app\" WITH CREATEDB NOINHERIT CONNECTION LIMIT 20 VALID UNTIL '2026-12-31'");
        assert_eq!(attrs_sql("app", &to, &from).unwrap().unwrap(), "ALTER ROLE \"app\" WITH NOCREATEDB INHERIT CONNECTION LIMIT -1 VALID UNTIL 'infinity'");
        assert_eq!(attrs_sql("app", &from, &from).unwrap(), None);
        assert!(attrs_sql("app", &from, &Attrs { conn_limit: -2, ..from.clone() }).is_err());
        let quote = Attrs { valid_until: "2026-01-01' ; DROP ROLE x; --".into(), ..from.clone() };
        assert_eq!(attrs_sql("app", &from, &quote).unwrap().unwrap(), "ALTER ROLE \"app\" WITH VALID UNTIL '2026-01-01'' ; DROP ROLE x; --'");
    }

    #[test]
    fn memberships_follow_the_version() {
        let o16 = Some(Opts { admin: false, inherit: Some(true), set: Some(false) });
        assert_eq!(member_sql(16, "rep", "app", &None, &o16), ["GRANT \"rep\" TO \"app\" WITH ADMIN FALSE, INHERIT TRUE, SET FALSE"]);
        assert_eq!(member_sql(16, "rep", "app", &o16, &None), ["REVOKE \"rep\" FROM \"app\""]);
        let admin16 = Some(Opts { admin: true, ..o16.unwrap() });
        assert_eq!(member_sql(16, "rep", "app", &o16, &admin16), ["GRANT \"rep\" TO \"app\" WITH ADMIN TRUE"]);
        assert_eq!(member_sql(16, "rep", "app", &admin16, &o16), ["GRANT \"rep\" TO \"app\" WITH ADMIN FALSE"]);
        let (plain, admin) = (Some(Opts::default()), Some(Opts { admin: true, ..Default::default() }));
        assert_eq!(member_sql(13, "rep", "app", &None, &plain), ["GRANT \"rep\" TO \"app\""]);
        assert_eq!(member_sql(13, "rep", "app", &None, &admin), ["GRANT \"rep\" TO \"app\" WITH ADMIN OPTION"]);
        assert_eq!(member_sql(13, "rep", "app", &admin, &plain), ["REVOKE ADMIN OPTION FOR \"rep\" FROM \"app\""]);
    }

    #[test]
    fn default_privileges_name_the_creator_and_schema() {
        let c = Change::Default { owner: "etl".into(), schema: "staging".into(), kind: DefKind::Tables, grantee: "reporting".into(), from: Privs::new(), to: privs(&[("SELECT", false)]) };
        assert_eq!(c.sql(17).unwrap(), ["ALTER DEFAULT PRIVILEGES FOR ROLE \"etl\" IN SCHEMA \"staging\" GRANT SELECT ON TABLES TO \"reporting\""]);
        assert_eq!(c.inverse().sql(17).unwrap(), ["ALTER DEFAULT PRIVILEGES FOR ROLE \"etl\" IN SCHEMA \"staging\" REVOKE SELECT ON TABLES FROM \"reporting\""]);
        let all = Change::Default { owner: "etl".into(), schema: String::new(), kind: DefKind::Functions, grantee: String::new(), from: privs(&[("EXECUTE", false)]), to: Privs::new() };
        assert_eq!(all.sql(17).unwrap(), ["ALTER DEFAULT PRIVILEGES FOR ROLE \"etl\" REVOKE EXECUTE ON FUNCTIONS FROM PUBLIC"]);
        let schemas = Change::Default { owner: "etl".into(), schema: "s".into(), kind: DefKind::Schemas, grantee: "r".into(), from: Privs::new(), to: privs(&[("USAGE", false)]) };
        assert!(schemas.sql(17).is_err());
    }

    #[test]
    fn create_role_hides_the_password_in_the_preview() {
        let n = NewRole { name: "etl_loader".into(), attrs: Attrs { login: true, ..Default::default() }, groups: vec!["reporting".into()] };
        assert_eq!(create_sql(&n, Some(REDACTED)).unwrap(), "CREATE ROLE \"etl_loader\" WITH LOGIN INHERIT PASSWORD '***' IN ROLE \"reporting\"");
        let v = scram_verifier("s3cret'", &[1u8; 16], 4096);
        let sql = create_sql(&n, Some(&v)).unwrap();
        assert!(!sql.contains("s3cret"));
        // The Query log redacts the verifier like any password literal.
        let (logged, cut) = crate::db::log::redact_sql(&sql);
        assert!(!logged.contains("SCRAM"), "{logged}");
        assert_eq!(cut.len(), 1);
        assert!(crate::db::log::redact_sql(&password_sql("a", &v)).0.ends_with("PASSWORD '***'"));
        assert!(create_sql(&NewRole { name: "pg_x".into(), ..Default::default() }, None).is_err());
        assert!(create_sql(&NewRole { name: "public".into(), ..Default::default() }, None).is_err());
    }

    #[test]
    fn scram_verifier_has_the_server_shape() {
        let v = scram_verifier("pencil", b"0123456789abcdef", 4096);
        let (head, keys) = v.split_once('$').unwrap().1.split_once('$').unwrap();
        assert_eq!(v.split('$').next().unwrap(), "SCRAM-SHA-256");
        assert_eq!(head, "4096:MDEyMzQ1Njc4OWFiY2RlZg==");
        let (stored, server) = keys.split_once(':').unwrap();
        assert_eq!((stored.len(), server.len()), (44, 44));
        assert_eq!(v, scram_verifier("pencil", b"0123456789abcdef", 4096));
        assert_ne!(v, scram_verifier("pencil", b"0123456789abcdeg", 4096));
        assert_eq!(b64(b"ab"), "YWI=");
        assert_eq!(b64(b"abc"), "YWJj");
        assert_eq!(b64(b"a"), "YQ==");
        // HMAC-SHA-256 test case 2 of RFC 4231.
        let mac = hmac(b"Jefe", &[b"what do ya want ", b"for nothing?"]);
        assert_eq!(mac.iter().map(|b| format!("{b:02x}")).collect::<String>(), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    }

    #[test]
    fn scram_verifier_matches_the_server() {
        // What PostgreSQL 18 stored for CREATE ROLE ... PASSWORD 'pencil' with this salt.
        let salt = [213, 162, 177, 185, 246, 83, 247, 67, 102, 63, 234, 9, 224, 106, 165, 225];
        assert_eq!(scram_verifier("pencil", &salt, 4096), "SCRAM-SHA-256$4096:1aKxufZT90NmP+oJ4Gql4Q==$YfDn2h9kok2vJsroTHF8qZB6imCrb6dZzOeUtflU/3g=:LWrs39JieGOjDtBFgWciRWgDO9e0ztNpQV1IXghvEPo=");
    }

    #[test]
    fn non_ascii_passwords_are_normalized_like_libpq_does() {
        // SASLprep is NFKC, so the ligature reads as the two letters; psql and the server compute the same.
        assert_eq!(scram_verifier("Zq9 Marker \u{fb01}", &[7; 16], 16), scram_verifier("Zq9 Marker fi", &[7; 16], 16));
        // An ASCII password is used as it is, control characters included.
        assert_ne!(scram_verifier("a\u{7}", &[7; 16], 16), scram_verifier("a", &[7; 16], 16));
    }

    #[test]
    fn errors_lose_the_password() {
        assert_eq!(scrub("near 'p''w' at x, also p'w".into(), &["p'w"]), "near '***' at x, also ***");
    }

    fn cluster(major: u32) -> Cluster {
        let role = |n: &str, inherit: bool| Role { name: n.into(), attrs: Attrs { inherit, ..Default::default() }, ..Default::default() };
        let m = |g: &str, mem: &str, inh: Option<bool>| Membership { group: g.into(), member: mem.into(), opts: Opts { admin: false, inherit: inh, set: inh }, grantors: vec![] };
        Cluster {
            major,
            me: "admin".into(),
            roles: vec![role("jane", true), role("noinh", false), role("reporting", true), role("readonly", true)],
            members: vec![m("reporting", "jane", Some(true)), m("readonly", "reporting", Some(true)), m("reporting", "noinh", Some(true)), m("readonly", "jane", Some(false))],
            databases: vec![],
            password_encryption: "scram-sha-256".into(),
        }
    }

    #[test]
    fn inherited_privileges_follow_inherit() {
        let c16 = cluster(16);
        assert_eq!(c16.chain("jane"), ["jane", "reporting", "readonly", ""]);
        // On 16 the membership's option counts, not the member's attribute.
        assert_eq!(c16.chain("noinh"), ["noinh", "reporting", "readonly", ""]);
        let mut c13 = cluster(13);
        for m in c13.members.iter_mut() {
            m.opts.inherit = None;
            m.opts.set = None;
        }
        assert_eq!(c13.chain("noinh"), ["noinh", ""]);
        assert_eq!(c13.chain("jane"), ["jane", "reporting", "readonly", ""]);
        let obj = Obj {
            owner: "etl".into(),
            acl: vec![
                AclEntry { grantee: "readonly".into(), grantor: "etl".into(), privs: privs(&[("SELECT", false)]) },
                AclEntry { grantee: "jane".into(), grantor: "etl".into(), privs: privs(&[("INSERT", true)]) },
                AclEntry { grantee: String::new(), grantor: "etl".into(), privs: privs(&[("TRIGGER", false)]) },
            ],
            ..Default::default()
        };
        let cl = cell(&obj, &c16.chain("jane"));
        assert_eq!(cl.direct, privs(&[("INSERT", true)]));
        assert_eq!(cl.via, BTreeMap::from([("SELECT".to_string(), "readonly".to_string()), ("TRIGGER".to_string(), String::new())]));
        assert!(!cl.owner);
        assert!(cell(&obj, &["etl".to_string()]).owner);
    }

    #[test]
    fn createrole_rules_follow_the_version() {
        let mut c = cluster(16);
        c.me = "mgr".into();
        c.roles.push(Role { name: "mgr".into(), attrs: Attrs { createrole: true, ..Default::default() }, ..Default::default() });
        let jane = c.role("jane").unwrap().clone();
        assert!(c.may_alter(&jane).is_err(), "16 needs ADMIN on the role");
        assert!(c.may_alter(&Role { admin: true, ..jane.clone() }).is_ok());
        assert!(c.may_set_flag("CREATEDB").is_err());
        assert!(c.may_set_flag("LOGIN").is_ok());
        c.major = 15;
        assert!(c.may_alter(&jane).is_ok(), "before 16 CREATEROLE manages any non-superuser role");
        assert!(c.may_set_flag("CREATEDB").is_ok());
        assert!(c.may_set_flag("REPLICATION").is_err());
        c.me = "jane".into();
        assert!(c.read_only_reason().unwrap().contains("neither CREATEROLE nor SUPERUSER"));
    }

    #[test]
    fn acl_json_merges_per_grantor() {
        let acl = parse_acl(r#"[["","postgres","CONNECT",false],["","postgres","TEMPORARY",false],["app","postgres","CONNECT",true],["app","jane","CREATE",false]]"#);
        assert_eq!(acl.len(), 3);
        assert_eq!(merged(&acl, "app"), privs(&[("CONNECT", true), ("CREATE", false)]));
        assert_eq!(merged(&acl, ""), privs(&[("CONNECT", false), ("TEMPORARY", false)]));
    }

    #[test]
    fn drop_plans_reassign_or_drop_and_refuse_what_cannot_work() {
        let info = DropInfo { role: "jane".into(), oid: "9".into(), dbs: vec![DbHold { database: "analytics".into(), owned: vec!["table t".into()], held: 2, deps: vec![] }, DbHold { database: "shop".into(), owned: vec![], held: 1, deps: vec![] }], ..Default::default() };
        let p = drop_plan(&info, &DropMode::Reassign("app_owner".into()), "postgres").unwrap();
        assert_eq!(drop_preview(&p), "-- in analytics\nREASSIGN OWNED BY \"jane\" TO \"app_owner\";\nDROP OWNED BY \"jane\";\n-- in shop\nREASSIGN OWNED BY \"jane\" TO \"app_owner\";\nDROP OWNED BY \"jane\";\n-- then, once\nDROP ROLE \"jane\";");
        assert_eq!(drop_plan(&info, &DropMode::DropOwned, "postgres").unwrap().len(), 3);
        assert!(drop_plan(&info, &DropMode::Reassign("jane".into()), "postgres").is_err());
        let owns_db = DropInfo { shared: vec!["database x".into()], ..info.clone() };
        assert!(drop_plan(&owns_db, &DropMode::DropOwned, "postgres").is_err());
        let only_db = DropInfo { dbs: vec![], ..owns_db.clone() };
        assert_eq!(drop_plan(&only_db, &DropMode::Reassign("a".into()), "postgres").unwrap()[0].0, "postgres");
        let unread = DropInfo { unread: vec![("shop".into(), "no access".into())], ..info };
        assert!(drop_plan(&unread, &DropMode::DropOwned, "postgres").is_err());
    }

    #[test]
    fn the_signed_in_role_cannot_lock_itself_out() {
        let me = Attrs { login: true, createrole: true, superuser: true, ..Default::default() };
        let ch = |to: Attrs| Change::Attrs { role: "admin".into(), from: me.clone(), to };
        for f in ["LOGIN", "SUPERUSER", "CREATEROLE"] {
            let mut to = me.clone();
            *to.flag_mut(f).unwrap() = false;
            assert!(Cluster::self_lockout("admin", &ch(to.clone())).is_some_and(|w| w.contains(f)), "{f}");
            // Another role may lose it.
            assert!(Cluster::self_lockout("someone", &ch(to)).is_none(), "{f}");
        }
        assert!(Cluster::self_lockout("admin", &ch(Attrs { createdb: true, ..me.clone() })).is_none());
        assert!(Cluster::self_lockout("admin", &ch(Attrs { valid_until: "-infinity".into(), ..me.clone() })).is_some());
        // Its own membership in a group that carries CREATEROLE, or with ADMIN, asks for a typed confirmation.
        let mut c = cluster(16);
        c.me = "jane".into();
        c.roles.push(Role { name: "mgrs".into(), attrs: Attrs { createrole: true, ..Default::default() }, ..Default::default() });
        c.members.push(Membership { group: "mgrs".into(), member: "reporting".into(), opts: Opts { admin: false, inherit: Some(true), set: Some(true) }, grantors: vec![] });
        let o = Opts { admin: false, inherit: Some(true), set: Some(true) };
        let leave = |g: &str, from: Opts, to: Option<Opts>| Change::Member { group: g.into(), member: "jane".into(), from: Some(from), to };
        assert!(c.self_membership_risk(&leave("reporting", o, None)).is_some(), "reporting leads to mgrs");
        assert!(c.self_membership_risk(&leave("readonly", o, None)).is_none(), "readonly leads nowhere");
        assert!(c.self_membership_risk(&leave("readonly", Opts { admin: true, ..o }, Some(o))).is_some(), "its own ADMIN");
        assert!(c.self_membership_risk(&leave("reporting", o, Some(Opts { admin: true, ..o }))).is_none(), "more rights, no risk");
        assert!(c.self_membership_risk(&Change::Member { group: "reporting".into(), member: "noinh".into(), from: Some(o), to: None }).is_none(), "not the signed-in role");
        // The confirmed change gets no undo; the others keep theirs.
        let b = Batch { changes: vec![leave("reporting", o, None), Change::Member { group: "readonly".into(), member: "noinh".into(), from: None, to: Some(o) }], no_undo: BTreeSet::from([0]), ..Default::default() };
        assert_eq!(b.inverse().changes, [b.changes[1].inverse()]);
    }

    #[test]
    fn minus_infinity_reads_as_expired_and_writes_back_as_itself() {
        assert_eq!(until_shown("-infinity"), "expired");
        assert_eq!(until_shown(""), "never");
        let from = Attrs { valid_until: "-infinity".into(), ..Default::default() };
        assert_eq!(attrs_sql("a", &Attrs::default(), &from).unwrap().unwrap(), "ALTER ROLE \"a\" WITH VALID UNTIL '-infinity'");
        assert!(from.text().contains("valid until expired"));
        assert!(UNTIL.contains("'-infinity' THEN '-infinity'"));
    }

    #[test]
    fn an_undo_restores_each_grantors_grant_exactly() {
        let g = |v: &[(&str, &[(&str, bool)])]| -> Grantors<Privs> { v.iter().map(|(k, p)| (k.to_string(), privs(p))).collect() };
        // mgr gave SELECT with grant option where the owner had given it plain: the undo takes mgr's grant away whole.
        let before = g(&[("postgres", &[("SELECT", false)])]);
        let after = g(&[("postgres", &[("SELECT", false)]), ("mgr", &[("SELECT", true)])]);
        let c = Change::Acl { target: table(), grantee: "m".into(), from: privs(&[("SELECT", false)]), to: privs(&[("SELECT", true)]) };
        let ran = Batch { changes: vec![c.clone()], exact: vec![Some(Exact::Acl(before.clone(), after.clone()))], ..Default::default() };
        let back = ran.inverse();
        assert_eq!(back.exact, [Some(Exact::Acl(after.clone(), before.clone()))]);
        assert_eq!(stmts(&back.changes[0], back.exact[0].as_ref(), 18).unwrap(), ["REVOKE SELECT ON TABLE \"public\".\"orders\" FROM \"m\""]);
        // Merged, the two states differ from what the undo must leave, so the check compares grantor by grantor.
        assert!(!holds(&back.changes[0], back.exact[0].as_ref(), &State::Privs(g(&[("postgres", &[("SELECT", false)]), ("mgr", &[("SELECT", false)])])), false));
        assert!(holds(&back.changes[0], back.exact[0].as_ref(), &State::Privs(before.clone()), false));
        assert!(one_grantor(&after, &g(&[])).is_err(), "two grantors at once");
        // A membership row of another grantor is revoked by name from 16; before, there is one row and no name.
        let o = Opts { admin: false, inherit: Some(true), set: Some(true) };
        let rows = |v: &[(&str, Opts)]| -> Grantors<Opts> { v.iter().map(|(k, o)| (k.to_string(), *o)).collect() };
        let now = rows(&[("postgres", o), ("mgr", Opts { admin: true, ..o })]);
        assert_eq!(member_rows_sql(16, "g", "m", &now, &rows(&[("postgres", o)])), ["REVOKE \"g\" FROM \"m\" GRANTED BY \"mgr\""]);
        assert_eq!(member_rows_sql(16, "g", "m", &now, &rows(&[("postgres", o), ("mgr", o)])), ["GRANT \"g\" TO \"m\" WITH ADMIN FALSE GRANTED BY \"mgr\""]);
        let one = |admin| rows(&[("", Opts { admin, inherit: None, set: None })]);
        assert_eq!(member_rows_sql(13, "g", "m", &one(true), &one(false)), ["REVOKE ADMIN OPTION FOR \"g\" FROM \"m\""]);
        assert_eq!(merge_opts(&now), Some(Opts { admin: true, ..o }));
    }

    #[test]
    fn a_role_with_only_shared_privileges_is_dropped_through_drop_owned() {
        let info = DropInfo { role: "jane".into(), oid: "9".into(), shared_held: vec!["database shop".into()], ..Default::default() };
        let p = drop_plan(&info, &DropMode::DropOwned, "postgres").unwrap();
        assert_eq!(drop_preview(&p), "-- in postgres\nDROP OWNED BY \"jane\";\n-- then, once\nDROP ROLE \"jane\";");
        assert_eq!(drop_plan(&DropInfo { shared_held: vec![], ..info }, &DropMode::DropOwned, "postgres").unwrap().len(), 1);
    }

    #[test]
    fn password_settings_are_followed_and_bounded() {
        assert!(wants_md5("md5") && wants_md5(" on ") && !wants_md5("scram-sha-256"));
        assert_eq!(md5_verifier("pencil", "jane"), "md5ab570ec1cf09f85f83d43e87e1a7d3c7");
        assert_eq!(check_iterations("4096"), Ok(4096));
        assert_eq!(check_iterations("10000000"), Ok(10_000_000));
        for bad in ["1", "4095", "10000001", "", "x"] {
            assert!(check_iterations(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn saslprep_follows_postgresql() {
        let v = |p: &str| scram_verifier(p, &[7; 16], 16);
        // Only characters mapped to nothing: PostgreSQL uses the password as typed rather than an empty one.
        assert!(pg_saslprep("\u{ad}").is_none());
        assert_ne!(v("\u{ad}"), v("\u{200b}"));
        // An emoji is unassigned in Unicode 3.2, so the whole password is used as typed, its no-break space included.
        assert!(pg_saslprep("Zq9\u{a0}x\u{1f600}").is_none());
        assert_ne!(v("Zq9\u{a0}x\u{1f600}"), v("Zq9 x\u{1f600}"));
        // Without one, the no-break space maps to a space and NFKC applies.
        assert_eq!(pg_saslprep("Zq9\u{a0}\u{fb01}").as_deref().map(String::as_str), Some("Zq9 fi"));
        // Right to left text must start and end right to left.
        assert!(pg_saslprep("\u{627}1").is_none());
        assert_eq!(pg_saslprep("\u{627}\u{628}").as_deref().map(String::as_str), Some("\u{627}\u{628}"));
    }

    #[test]
    fn may_follows_the_server() {
        let mut c = cluster(15);
        c.me = "mgr".into();
        c.roles.push(Role { name: "mgr".into(), attrs: Attrs { createrole: true, ..Default::default() }, ..Default::default() });
        let repl = Role { name: "streamer".into(), attrs: Attrs { replication: true, login: true, ..Default::default() }, ..Default::default() };
        assert!(c.may_alter(&repl).is_err(), "before 16 a REPLICATION role is a superuser's");
        assert!(c.may_password(&repl).is_err());
        c.major = 16;
        assert!(c.may_alter(&Role { admin: true, ..repl.clone() }).is_ok());
        // Without CREATEROLE: its own password, and the members of a group it holds ADMIN on.
        for major in [13, 16] {
            let mut c = cluster(major);
            c.me = "jane".into();
            let jane = c.role("jane").unwrap().clone();
            let other = c.role("noinh").unwrap().clone();
            assert!(c.may_password(&jane).is_ok(), "{major}");
            assert!(c.may_password(&other).is_err(), "{major}");
            let group = c.role("reporting").unwrap().clone();
            assert!(c.may_grant_role(&group).is_err(), "{major}");
            assert!(c.may_grant_role(&Role { admin: true, ..group }).is_ok(), "{major}");
        }
    }

    #[test]
    fn labels_say_what_changed() {
        let c = Change::Acl { target: table(), grantee: "app".into(), from: Privs::new(), to: privs(&[("DELETE", false)]) };
        assert_eq!(c.describe(), "granted DELETE on public.orders to app");
        assert_eq!(c.inverse().describe(), "revoked DELETE on public.orders from app");
        let go = Change::Acl { target: table(), grantee: "app".into(), from: privs(&[("SELECT", true)]), to: privs(&[("SELECT", false)]) };
        assert_eq!(go.describe(), "revoked grant option for SELECT on public.orders from app");
        let m = Change::Member { group: "rep".into(), member: "app".into(), from: None, to: Some(Opts::default()) };
        assert_eq!(m.describe(), "added app to rep");
    }
}
