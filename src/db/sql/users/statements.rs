//! The GRANT, REVOKE, CREATE USER, DROP USER, ALTER USER, password and role statements, and the
//! checks that run before any of them is built, with the account settings they read (Settings, Caps, Loaded, AccountPlan).

use super::*;

// Statements.

/// One scope's change: what to give and what to take away. The inverse swaps them. An empty `db` means everything
/// (`ON *.*`), which no database pattern can be.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Edit {
    pub db: String,
    pub grant: Vec<String>,
    pub revoke: Vec<String>,
}

impl Edit {
    pub fn inverse(&self) -> Edit {
        Edit { db: self.db.clone(), grant: self.revoke.clone(), revoke: self.grant.clone() }
    }

    pub fn is_empty(&self) -> bool {
        self.grant.is_empty() && self.revoke.is_empty()
    }
}

/// What turning `from` into `to` takes, for one database. Only the privileges of `DB_PRIVS` are compared, so
/// ones this window does not show stay as they are.
pub fn diff(db: &str, from: &BTreeSet<String>, to: &BTreeSet<String>) -> Edit {
    let global = db.is_empty();
    let pick = |a: &BTreeSet<String>, b: &BTreeSet<String>| {
        let mut v: Vec<String> = a.iter().filter(|p| !b.contains(*p) && priv_ok(global, p)).cloned().collect();
        v.sort_by_key(|p| order_key(p));
        v
    };
    Edit { db: db.into(), grant: pick(to, from), revoke: pick(from, to) }
}

/// A global revoke of a privilege that has partial revokes would drop them too (REVOKE ... ON *.* clears the
/// restrictions), and the undo, a GRANT ON *.*, would then cover the schemas they excluded.
pub fn check_partial(grants: &[Grant], edits: &[Edit]) -> Res<()> {
    for ed in edits.iter().filter(|e| e.db.is_empty()) {
        for p in &ed.revoke {
            let dbs: Vec<&str> = grants.iter().filter_map(|g| match &g.scope {
                Scope::Revoked(d) if g.privs.contains(p) => Some(d.as_str()),
                _ => None,
            }).collect();
            if !dbs.is_empty() {
                return Err(format!("{p} has partial revokes on {}. Revoking it everywhere would drop them, and an undo would then grant it on those schemas too. Remove the partial revokes first.", dbs.join(", ")));
            }
        }
    }
    Ok(())
}

pub(super) fn check_privs(global: bool, privs: &[String]) -> Res<()> {
    match privs.iter().find(|p| !priv_ok(global, p)) {
        Some(p) => Err(format!("{p} is not a privilege this window gives {}.", if global { "globally" } else { "on a database" })),
        None => Ok(()),
    }
}

pub(super) fn check_pattern(p: &str) -> Res<()> {
    if p.is_empty() || p.contains('\0') {
        return Err("The database name is empty or holds a NUL.".into());
    }
    Ok(())
}

/// `GRANT a, b ON `db`.* TO 'u'@'h'` or the REVOKE form. No trailing semicolon.
pub fn grant_sql(m: Mode, acct: &Account, db: &str, privs: &[String], revoke: bool) -> Res<String> {
    let global = db.is_empty();
    check_privs(global, privs)?;
    if !global {
        check_pattern(db)?;
    }
    if privs.is_empty() {
        return Err("No privileges to change.".into());
    }
    let mut sorted: Vec<&String> = privs.iter().collect();
    sorted.sort_by_key(|p| order_key(p));
    sorted.dedup();
    let list = sorted.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(", ");
    // MySQL and MariaDB quote alike, and GRANT here exists for no other dialect
    let on = if global { "*.*".to_string() } else { format!("{}.*", Dialect::MySql.quote_ident(db)) };
    let who = acct.literal(m.bs);
    Ok(if revoke { format!("REVOKE {list} ON {on} FROM {who}") } else { format!("GRANT {list} ON {on} TO {who}") })
}

/// The statements of one edit: the grant, then the revoke.
pub fn edit_sql(m: Mode, acct: &Account, ed: &Edit) -> Res<Vec<String>> {
    let mut v = Vec::new();
    if !ed.grant.is_empty() {
        v.push(grant_sql(m, acct, &ed.db, &ed.grant, false)?);
    }
    if !ed.revoke.is_empty() {
        v.push(grant_sql(m, acct, &ed.db, &ed.revoke, true)?);
    }
    Ok(v)
}

/// The preview text of all edits: one statement per line, each ended with a semicolon.
pub fn preview(m: Mode, acct: &Account, edits: &[Edit]) -> Res<String> {
    let mut lines = Vec::new();
    for ed in edits {
        lines.extend(edit_sql(m, acct, ed)?);
    }
    Ok(lines.iter().map(|l| format!("{l};")).collect::<Vec<_>>().join("\n"))
}

/// `preview` after checking the edits against the grants the account holds now.
pub fn preview_checked(m: Mode, acct: &Account, edits: &[Edit], grants: &[Grant]) -> Res<String> {
    check_partial(grants, edits)?;
    preview(m, acct, edits)
}

pub const REDACTED: &str = "'***'";

/// Why an account cannot be created, when it cannot.
pub fn check_new_user(user: &str, host: &str, password: &str) -> Res<()> {
    if user.is_empty() {
        return Err("Name the user.".into());
    }
    if host.is_empty() {
        return Err("Give a host, or % for any.".into());
    }
    if user.chars().count() > 128 {
        return Err("A user name is at most 128 characters.".into());
    }
    if host.chars().count() > 255 {
        return Err("A host is at most 255 characters.".into());
    }
    if user.chars().chain(host.chars()).any(char::is_control) {
        return Err("A name cannot contain control characters.".into());
    }
    if password.is_empty() {
        return Err("Give a password: an account without one lets anyone in.".into());
    }
    if password.contains('\0') {
        return Err("A password cannot contain a NUL character.".into());
    }
    Ok(())
}

/// `CREATE USER 'u'@'h' IDENTIFIED BY '...'`; with `redact` the password is `'***'`, which is what every preview
/// and log line uses.
pub fn create_user_sql(m: Mode, acct: &Account, password: &str, redact: bool) -> String {
    format!("CREATE USER {} IDENTIFIED BY {}", acct.literal(m.bs), if redact { REDACTED.to_string() } else { lit(password, m.bs) })
}

pub fn drop_user_sql(m: Mode, acct: &Account, d: Dialect) -> String {
    format!("{} {}", if acct.is_role && matches!(d, Dialect::MariaDb) { "DROP ROLE" } else { "DROP USER" }, acct.literal(m.bs))
}

/// An error with the password taken out, in case a server echoes the statement. The statement holds the password
/// as a quoted string, so the quoted forms are replaced too, and a syntax error, which echoes the statement from the
/// fault onward, is cut at "near '".
pub(super) fn scrub(err: String, password: &str) -> String {
    if password.is_empty() {
        return err;
    }
    let mut out = err;
    // Every way the statement or an error may have escaped it: quotes doubled or not, backslashes doubled or not.
    let mut forms = Vec::new();
    for dq in [false, true] {
        for db in [false, true] {
            let mut f = password.to_string();
            if db {
                f = f.replace('\\', "\\\\");
            }
            if dq {
                f = f.replace('\'', "''");
            }
            forms.push(f);
        }
    }
    forms.sort_by_key(|f| std::cmp::Reverse(f.len()));
    for f in forms.iter().filter(|f| !f.is_empty()) {
        out = out.replace(f.as_str(), "***");
    }
    if let Some(i) = out.find("near '") {
        out.truncate(i);
        out.push_str("near [the statement is hidden]");
    }
    out
}

// Account settings.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expire {
    Never,
    /// The server's default_password_lifetime.
    Default,
    Days(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tls {
    None,
    Ssl,
    X509,
    /// A cipher, issuer or subject requirement, held in `Settings::spec`.
    Specified,
}

/// The values of REQUIRE CIPHER, ISSUER and SUBJECT; empty ones are not required.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TlsSpec {
    pub cipher: String,
    pub issuer: String,
    pub subject: String,
}

pub const LIMIT_NAMES: [&str; 4] = ["MAX_QUERIES_PER_HOUR", "MAX_UPDATES_PER_HOUR", "MAX_CONNECTIONS_PER_HOUR", "MAX_USER_CONNECTIONS"];
pub const LIMIT_MAX: u64 = 4_294_967_295;

/// What ALTER USER can change besides the password: the lock, the password lifetime, the TLS requirement and the
/// four resource limits (0 is no limit).
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub locked: bool,
    pub expire: Expire,
    pub tls: Tls,
    /// What `Tls::Specified` requires.
    pub spec: TlsSpec,
    pub limits: [u64; 4],
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { locked: false, expire: Expire::Default, tls: Tls::None, spec: TlsSpec::default(), limits: [0; 4] }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AccountInfo {
    pub plugin: String,
    pub settings: Settings,
    /// MariaDB: the account has more than one way to log in (`unix_socket OR mysql_native_password`).
    pub chain: bool,
    /// Something that changes when the account is dropped and made again, to tell it from a namesake.
    pub ident: Option<String>,
}

/// What the grants of an account and its row in mysql.user say, for the window's tabs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Loaded {
    pub grants: Vec<Grant>,
    pub info: Option<AccountInfo>,
    /// Why `info` is missing: mysql.user is closed to this user, or the account has no row.
    pub info_err: Option<String>,
    /// lower_case_table_names is not 0, so `Shop` and `shop` are one database to the server.
    pub fold_case: bool,
}

/// What this server version can do, so only that is offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    pub alter_user: bool,
    pub lock: bool,
    pub lifetime: bool,
    pub roles: bool,
    pub dynamic: bool,
    /// IDENTIFIED WITH <plugin> BY: MySQL only; MariaDB changes the plugin another way.
    pub plugin_choice: bool,
    pub caching: bool,
    /// mysql_native_password is off by default from 8.4 and gone in 9.0.
    pub native_off: bool,
    pub sha256: bool,
}

impl Caps {
    pub fn of(d: Dialect, version: &str) -> Caps {
        // Older MariaDB servers report "5.5.5-10.11.6-MariaDB" for compatibility.
        let v = version.strip_prefix("5.5.5-").filter(|_| matches!(d, Dialect::MariaDb)).unwrap_or(version);
        let mut n = v.split(|c: char| !c.is_ascii_digit()).filter(|x| !x.is_empty()).map(|x| x.parse::<u32>().unwrap_or(0));
        let at = (n.next().unwrap_or(0), n.next().unwrap_or(0), n.next().unwrap_or(0));
        match d {
            Dialect::MySql => Caps { alter_user: at >= (5, 7, 6), lock: at >= (5, 7, 6), lifetime: at >= (5, 7, 4), roles: at >= (8, 0, 0), dynamic: at >= (8, 0, 0), plugin_choice: true, caching: at >= (8, 0, 0), native_off: at >= (8, 4, 0), sha256: ((5, 6, 0)..(9, 0, 0)).contains(&at) },
            Dialect::MariaDb => Caps { alter_user: at >= (10, 2, 0), lock: at >= (10, 4, 2), lifetime: at >= (10, 4, 3), roles: at >= (10, 0, 5), dynamic: false, plugin_choice: false, caching: false, native_off: false, sha256: false },
            // Nothing here applies to PostgreSQL roles
            Dialect::Postgres => Caps { alter_user: false, lock: false, lifetime: false, roles: false, dynamic: false, plugin_choice: false, caching: false, native_off: false, sha256: false },
        }
    }

    /// The plugins "Change password" offers: mysql_native_password only where it is on by default, or when the
    /// account already uses it.
    pub fn plugins(&self, current: &str) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.caching {
            v.push("caching_sha2_password");
        }
        if !self.native_off || current == "mysql_native_password" {
            v.push("mysql_native_password");
        }
        if self.sha256 {
            v.push("sha256_password");
        }
        v
    }
}

pub const MYSQL_PLUGINS: [&str; 3] = ["caching_sha2_password", "mysql_native_password", "sha256_password"];

/// ALTER USER for what differs between `from` and `to`; None when nothing does. One statement, clauses in the
/// order both servers read them: REQUIRE, WITH limits, PASSWORD EXPIRE, ACCOUNT LOCK.
pub fn alter_sql(m: Mode, acct: &Account, from: &Settings, to: &Settings) -> Res<Option<String>> {
    let mut parts: Vec<String> = Vec::new();
    if from.tls != to.tls || (to.tls == Tls::Specified && from.spec != to.spec) {
        parts.push(match to.tls {
            Tls::None => "REQUIRE NONE".into(),
            Tls::Ssl => "REQUIRE SSL".into(),
            Tls::X509 => "REQUIRE X509".into(),
            Tls::Specified => {
                let mut v = Vec::new();
                for (word, val) in [("SUBJECT", &to.spec.subject), ("ISSUER", &to.spec.issuer), ("CIPHER", &to.spec.cipher)] {
                    if !val.is_empty() {
                        if val.contains('\0') {
                            return Err("A TLS requirement cannot contain a NUL character.".into());
                        }
                        v.push(format!("{word} {}", lit(val, m.bs)));
                    }
                }
                if v.is_empty() {
                    return Err("Name the subject, issuer or cipher the account must present.".into());
                }
                format!("REQUIRE {}", v.join(" AND "))
            }
        });
    }
    let mut with = Vec::new();
    for (i, name) in LIMIT_NAMES.iter().enumerate() {
        if from.limits[i] != to.limits[i] {
            if to.limits[i] > LIMIT_MAX {
                return Err(format!("{name} is at most {LIMIT_MAX}."));
            }
            with.push(format!("{name} {}", to.limits[i]));
        }
    }
    if !with.is_empty() {
        parts.push(format!("WITH {}", with.join(" ")));
    }
    if from.expire != to.expire {
        parts.push(match to.expire {
            Expire::Never => "PASSWORD EXPIRE NEVER".into(),
            Expire::Default => "PASSWORD EXPIRE DEFAULT".into(),
            Expire::Days(n) if (1..=65535).contains(&n) => format!("PASSWORD EXPIRE INTERVAL {n} DAY"),
            Expire::Days(_) => return Err("A password lifetime is 1 to 65535 days.".into()),
        });
    }
    if from.locked != to.locked {
        parts.push(if to.locked { "ACCOUNT LOCK" } else { "ACCOUNT UNLOCK" }.into());
    }
    Ok((!parts.is_empty()).then(|| format!("ALTER USER {} {}", acct.literal(m.bs), parts.join(" "))))
}

/// MySQL: `ALTER USER ... IDENTIFIED [WITH plugin] BY '...'`. MariaDB: `SET PASSWORD FOR ... = PASSWORD('...')`, because
/// its ALTER USER ... IDENTIFIED BY replaces the whole authentication chain. The password is `'***'` when `redact`. The
/// plugin comes from a fixed list and only MySQL takes it here.
pub fn password_sql(m: Mode, d: Dialect, acct: &Account, plugin: Option<&str>, password: &str, redact: bool) -> Res<String> {
    if matches!(d, Dialect::Postgres) {
        return Err("Passwords of PostgreSQL roles are not set through this window.".into());
    }
    if matches!(d, Dialect::MariaDb) {
        return Ok(format!("SET PASSWORD FOR {} = PASSWORD({})", acct.literal(m.bs), if redact { REDACTED.to_string() } else { lit(password, m.bs) }));
    }
    let with = match plugin.filter(|_| matches!(d, Dialect::MySql)) {
        Some(p) if MYSQL_PLUGINS.contains(&p) => format!("WITH {p} "),
        Some(p) => return Err(format!("{p} is not an authentication plugin this window sets.")),
        None => String::new(),
    };
    Ok(format!("ALTER USER {} IDENTIFIED {with}BY {}", acct.literal(m.bs), if redact { REDACTED.to_string() } else { lit(password, m.bs) }))
}

/// Why a password cannot be set, when it cannot.
pub fn check_password(password: &str) -> Res<()> {
    if password.is_empty() {
        return Err("Give a password: an account without one lets anyone in.".into());
    }
    if password.contains('\0') {
        return Err("A password cannot contain a NUL character.".into());
    }
    Ok(())
}

pub fn check_names(user: &str, host: &str) -> Res<()> {
    check_new_user(user, host, "x")
}

pub fn rename_sql(m: Mode, from: &Account, to: &Account) -> Res<String> {
    check_names(&to.user, &to.host)?;
    Ok(format!("RENAME USER {} TO {}", from.literal(m.bs), to.literal(m.bs)))
}

/// A role as a statement names it: MySQL `'name'@'host'`, MariaDB `'name'`.
pub(super) fn role_literal(d: Dialect, role: &str, bs: bool) -> Res<String> {
    if role.is_empty() || role.chars().any(char::is_control) {
        return Err("A role name is empty or holds control characters.".into());
    }
    Ok(match (d, role.rsplit_once('@')) {
        (Dialect::MySql, Some((n, h))) => format!("{}@{}", lit(n, bs), lit(h, bs)),
        (Dialect::MySql, None) => lit(role, bs),
        (Dialect::MariaDb | Dialect::Postgres, _) => lit(role, bs),
    })
}

pub fn role_sql(m: Mode, d: Dialect, acct: &Account, role: &str, revoke: bool) -> Res<String> {
    role_grant_sql(m, d, acct, role, revoke, false)
}

/// GRANT or REVOKE of a role; `admin` adds WITH ADMIN OPTION to a grant.
pub fn role_grant_sql(m: Mode, d: Dialect, acct: &Account, role: &str, revoke: bool, admin: bool) -> Res<String> {
    let r = role_literal(d, role, m.bs)?;
    let who = acct.literal(m.bs);
    Ok(if revoke { format!("REVOKE {r} FROM {who}") } else { format!("GRANT {r} TO {who}{}", if admin { " WITH ADMIN OPTION" } else { "" }) })
}

/// SET DEFAULT ROLE: MySQL takes the whole list, MariaDB one role.
pub fn default_roles_sql(m: Mode, d: Dialect, acct: &Account, roles: &[String]) -> Res<Option<String>> {
    let lits = roles.iter().map(|r| role_literal(d, r, m.bs)).collect::<Res<Vec<_>>>()?;
    Ok(match (d, lits.first()) {
        (_, None) => None,
        (Dialect::MySql, _) => Some(format!("SET DEFAULT ROLE {} TO {}", lits.join(", "), acct.literal(m.bs))),
        (Dialect::MariaDb, Some(r)) => Some(format!("SET DEFAULT ROLE {r} FOR {}", acct.literal(m.bs))),
        (Dialect::Postgres, Some(_)) => return Err(crate::db::sql::pg::NOT_YET.into()),
    })
}

/// What the Account and Limits tabs stage together: a rename, setting changes and role changes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccountPlan {
    pub rename: Option<Account>,
    pub settings: Option<(Settings, Settings)>,
    pub roles_grant: Vec<String>,
    pub roles_revoke: Vec<String>,
}

impl AccountPlan {
    pub fn is_empty(&self) -> bool {
        self.rename.is_none() && self.settings.as_ref().is_none_or(|(a, b)| a == b) && self.roles_grant.is_empty() && self.roles_revoke.is_empty()
    }
}

/// The statements of a plan in the order they run: rename, ALTER USER, then roles, the last two on the new name.
pub fn plan_sql(m: Mode, d: Dialect, acct: &Account, plan: &AccountPlan) -> Res<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = acct.clone();
    if let Some(to) = &plan.rename {
        out.push(rename_sql(m, acct, to)?);
        cur = Account { user: to.user.clone(), host: to.host.clone(), ..acct.clone() };
    }
    if let Some((a, b)) = &plan.settings {
        out.extend(alter_sql(m, &cur, a, b)?);
    }
    for r in &plan.roles_revoke {
        out.push(role_sql(m, d, &cur, r, true)?);
    }
    for r in &plan.roles_grant {
        out.push(role_sql(m, d, &cur, r, false)?);
    }
    Ok(out)
}

pub fn plan_preview(m: Mode, d: Dialect, acct: &Account, plan: &AccountPlan) -> Res<String> {
    Ok(plan_sql(m, d, acct, plan)?.iter().map(|l| format!("{l};")).collect::<Vec<_>>().join("\n"))
}

pub(super) fn settings_label(a: &Settings, b: &Settings) -> String {
    let mut v = Vec::new();
    if a.locked != b.locked {
        v.push(if b.locked { "locked the account" } else { "unlocked the account" });
    }
    if a.expire != b.expire {
        v.push("changed the password lifetime");
    }
    if a.tls != b.tls {
        v.push("changed the TLS requirement");
    }
    if a.limits != b.limits {
        v.push("changed the resource limits");
    }
    v.join(", ")
}
