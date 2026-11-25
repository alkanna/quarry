//! GRANT, REVOKE, ALTER ROLE, membership, default privilege and CREATE ROLE statements, and the password hashes sent in place of a password.

use super::*;

// Statements.

/// What a GRANT names: the object and, for a function, its arguments. `oid` tells it from one made again under the name.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
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

pub(super) fn grantee_shown(g: &str) -> &str {
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

pub(super) fn merge_privs(g: &Grantors<Privs>) -> Privs {
    let mut out = Privs::new();
    for (p, go) in g.values().flatten() {
        *out.entry(p.clone()).or_insert(false) |= *go;
    }
    out
}

pub(super) fn merge_opts(g: &Grantors<Opts>) -> Option<Opts> {
    let or = |a: Option<bool>, b: Option<bool>| a.zip(b).map(|(a, b)| a || b).or(a).or(b);
    g.values().copied().reduce(|a, b| Opts { admin: a.admin || b.admin, inherit: or(a.inherit, b.inherit), set: or(a.set, b.set) })
}

/// The one grantor whose grants differ between `now` and `then`, as the privileges to go from and to. A GRANT or REVOKE from
/// this login reaches only the grant it makes itself, so an undo that would have to change two grantors' grants is refused.
pub(super) fn one_grantor(now: &Grantors<Privs>, then: &Grantors<Privs>) -> Res<(Privs, Privs)> {
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
pub(super) fn member_rows_sql(major: u32, group: &str, member: &str, now: &Grantors<Opts>, then: &Grantors<Opts>) -> Vec<String> {
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
pub(super) fn acl_pair(c: &Change, exact: Option<&Exact>) -> Res<Option<(Privs, Privs)>> {
    Ok(match (c, exact) {
        (Change::Acl { .. }, Some(Exact::Acl(now, then))) => Some(one_grantor(now, then)?),
        (Change::Acl { from, to, .. }, _) => Some((from.clone(), to.clone())),
        _ => None,
    })
}

/// The statements of a change in a run, from its exact state when it has one.
pub(super) fn stmts(c: &Change, exact: Option<&Exact>, major: u32) -> Res<Vec<String>> {
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
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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

    pub(super) fn roles(&self) -> Vec<&str> {
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

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
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

pub(super) fn hmac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
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

pub(super) fn b64(data: &[u8]) -> String {
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
pub(super) fn pg_saslprep(p: &str) -> Option<zeroize::Zeroizing<String>> {
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

pub(super) fn new_salt() -> [u8; 16] {
    use chacha20poly1305::{aead::Generate, Nonce};
    let (a, b) = (Nonce::generate(), Nonce::generate());
    let mut s = [0u8; 16];
    s[..8].copy_from_slice(&a[..8]);
    s[8..].copy_from_slice(&b[..8]);
    s
}

/// An error with the password and its verifier cut out, in case the server quotes the statement back.
pub(super) fn scrub(err: String, secrets: &[&str]) -> String {
    let mut e = err;
    for s in secrets.iter().filter(|s| !s.is_empty()) {
        e = e.replace(s, "***");
        e = e.replace(&s.replace('\'', "''"), "***");
    }
    e
}
