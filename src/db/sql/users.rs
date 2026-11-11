//! SQL users and their database privileges: listing accounts, reading what SHOW GRANTS says, and the
//! GRANT, REVOKE, CREATE USER and DROP USER statements with their exact inverses. Account names and
//! passwords cannot be bound as parameters in these statements, so they are written as quoted strings
//! by `lit` for the sql_mode of the very session that runs them, and every privilege name comes from
//! `DB_PRIVS`, never from user text. Each statement is sent alone, prepared, on a writer connection.

use super::se;
use super::ddl::{self, session_mode, Mode};
use super::value::lit;
use super::worker::text;
use super::write::{writer_conn, SqlUndo};
use super::{Db, Dialect};
use crate::db::{e, Msg, Res, UndoAction, UndoKind, Worker};
use sqlx::{Connection, Executor};
use std::collections::BTreeSet;
use std::time::Instant;

/// The privileges that can be given on a whole database, in the order the checkboxes show them. The first
/// nine are the design's columns; the rest are reached by scrolling sideways. GRANT OPTION is a privilege
/// in these statements, which keeps WITH GRANT OPTION out of the statement builder.
pub const DB_PRIVS: [&str; 19] = [
    "SELECT",
    "INSERT",
    "UPDATE",
    "DELETE",
    "CREATE",
    "DROP",
    "INDEX",
    "ALTER",
    "EXECUTE",
    "REFERENCES",
    "CREATE TEMPORARY TABLES",
    "LOCK TABLES",
    "CREATE VIEW",
    "SHOW VIEW",
    "CREATE ROUTINE",
    "ALTER ROUTINE",
    "EVENT",
    "TRIGGER",
    "GRANT OPTION",
];

/// The global privileges of the design, by group. Others the server has (FILE, LOCK TABLES and so on) are left alone.
pub const GLOBAL_DATA: [&str; 6] = ["SELECT", "INSERT", "UPDATE", "DELETE", "EXECUTE", "SHOW VIEW"];
pub const GLOBAL_STRUCTURE: [&str; 9] = ["CREATE", "ALTER", "DROP", "INDEX", "CREATE VIEW", "TRIGGER", "EVENT", "CREATE ROUTINE", "ALTER ROUTINE"];
pub const GLOBAL_ADMIN: [&str; 9] = ["PROCESS", "RELOAD", "SHUTDOWN", "SUPER", "REPLICATION CLIENT", "REPLICATION SLAVE", "CREATE USER", "SHOW DATABASES", "GRANT OPTION"];

fn global_static() -> impl Iterator<Item = &'static str> {
    GLOBAL_DATA.iter().chain(GLOBAL_STRUCTURE.iter()).chain(GLOBAL_ADMIN.iter()).copied()
}

/// A MySQL 8 dynamic privilege such as CONNECTION_ADMIN: capitals, digits and underscores, at least one underscore.
/// The check is the whole defence for names typed into "+ Privilege", since they are written into a statement.
pub fn is_dynamic(p: &str) -> bool {
    !p.is_empty() && p.len() <= 64 && p.starts_with(|c: char| c.is_ascii_uppercase()) && p.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_') && p.contains('_')
}

/// Whether `p` may be written into a GRANT on a database (`global` false) or on everything.
fn priv_ok(global: bool, p: &str) -> bool {
    if global { global_static().any(|k| k == p) || is_dynamic(p) } else { DB_PRIVS.contains(&p) }
}

/// Database list first, then the global ones, then the rest by name, so statements read the same every time.
fn order_key(p: &str) -> (usize, String) {
    let at = DB_PRIVS.iter().position(|k| *k == p).or_else(|| global_static().position(|k| k == p).map(|i| 100 + i)).unwrap_or(usize::MAX);
    (at, p.to_string())
}

/// A column title for a privilege: "Select", "Create temporary tables".
pub fn priv_title(p: &str) -> String {
    let mut s = p.to_ascii_lowercase();
    s[..1].make_ascii_uppercase();
    s
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Account {
    pub user: String,
    /// Empty for a MariaDB role.
    pub host: String,
    pub locked: bool,
    pub expired: bool,
    pub is_role: bool,
    pub plugin: String,
}

impl Account {
    pub fn named(user: &str, host: &str) -> Account {
        Account { user: user.into(), host: host.into(), ..Default::default() }
    }

    /// `user@host`, or the bare name of a MariaDB role.
    pub fn label(&self) -> String {
        if self.is_role { self.user.clone() } else { format!("{}@{}", self.user, self.host) }
    }

    /// `'user'@'host'` for the session whose backslash rule is `bs`. Only a role is written without a host: a user
    /// whose host is empty is `'u'@''`, never the role `'u'` or the user `'u'@'%'`.
    pub fn literal(&self, bs: bool) -> String {
        if self.is_role { lit(&self.user, bs) } else { format!("{}@{}", lit(&self.user, bs), lit(&self.host, bs)) }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UserList {
    pub accounts: Vec<Account>,
    /// CURRENT_USER(): the account the server matched this connection to.
    pub current: Option<(String, String)>,
    /// False when mysql.user could not be read, so `accounts` holds only the logged in user.
    pub whole: bool,
    /// Why the full list was not read.
    pub why: Option<String>,
}

// Reading the accounts.

fn split_account(s: &str) -> (String, String) {
    // A user name may hold an @, a host cannot, so the last one divides them.
    match s.rfind('@') {
        Some(i) => (s[..i].to_string(), s[i + 1..].to_string()),
        None => (s.to_string(), String::new()),
    }
}

fn account_query(d: Dialect, joined: bool) -> &'static str {
    match (d, joined) {
        (Dialect::Postgres, _) => unreachable!("accounts are MySQL family only"),
        (Dialect::MySql, _) => "SELECT User AS u, Host AS h, account_locked AS locked, password_expired AS expired, plugin, 'N' AS is_role FROM mysql.user ORDER BY User, Host",
        // The lock lives only in the JSON of global_priv, which a plain mysql.user view does not show.
        (Dialect::MariaDb, true) => "SELECT u.User AS u, u.Host AS h, IFNULL(JSON_VALUE(g.Priv, '$.account_locked'), 'false') AS locked, u.password_expired AS expired, u.plugin AS plugin, u.is_role AS is_role FROM mysql.user u LEFT JOIN mysql.global_priv g ON g.User = u.User AND g.Host = u.Host ORDER BY u.User, u.Host",
        (Dialect::MariaDb, false) => "SELECT User AS u, Host AS h, 'false' AS locked, password_expired AS expired, plugin, is_role FROM mysql.user ORDER BY User, Host",
    }
}

async fn read_accounts(c: &mut sqlx::MySqlConnection, d: Dialect) -> Res<Vec<Account>> {
    let mut last = None;
    for joined in [true, false] {
        match sqlx::query(account_query(d, joined)).persistent(false).fetch_all(&mut *c).await {
            Ok(rows) => {
                let yes = |v: Option<String>| v.is_some_and(|v| matches!(v.to_ascii_uppercase().as_str(), "Y" | "TRUE" | "1"));
                return Ok(rows
                    .iter()
                    .map(|r| Account {
                        user: text(r, "u").unwrap_or_default(),
                        host: text(r, "h").unwrap_or_default(),
                        locked: yes(text(r, "locked")),
                        expired: yes(text(r, "expired")),
                        is_role: yes(text(r, "is_role")),
                        plugin: text(r, "plugin").unwrap_or_default(),
                    })
                    .collect());
            }
            Err(x) => last = Some(e(x)),
        }
        // MySQL has one form only.
        if matches!(d, Dialect::MySql | Dialect::Postgres) {
            break;
        }
    }
    Err(last.unwrap_or_default())
}

async fn load_list(db: &Db) -> Res<UserList> {
    let mut c = db.acquire().await?;
    let current = sqlx::query("SELECT CURRENT_USER() AS who").persistent(false).fetch_one(&mut *c).await.ok().and_then(|r| text(&r, "who")).map(|w| split_account(&w));
    match read_accounts(&mut c, db.dialect).await {
        Ok(accounts) => Ok(UserList { accounts, current, whole: true, why: None }),
        Err(why) => {
            let Some((user, host)) = current.clone() else { return Err(why) };
            Ok(UserList { accounts: vec![Account::named(&user, &host)], current, whole: false, why: Some(why) })
        }
    }
}

// Parsing SHOW GRANTS.

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Word(String),
    Ident(String),
    Str(String),
    Sym(char),
}

/// The tokens of one SHOW GRANTS line. Backticks double to escape themselves; in a string both a doubled
/// quote and a backslash escape are read, because the server writes the first and old versions the second.
fn tokens(s: &str) -> Vec<Tok> {
    let c: Vec<char> = s.chars().collect();
    let (mut i, mut out) = (0, Vec::new());
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
        } else if ch == '`' || ch == '"' {
            // Under ANSI_QUOTES the server writes names in double quotes. A string in a grant line always has
            // single quotes, so a double-quoted token is only ever a name. No backslash escape applies to either.
            let mut v = String::new();
            i += 1;
            while i < c.len() {
                if c[i] == ch {
                    if c.get(i + 1) == Some(&ch) {
                        v.push(ch);
                        i += 2;
                        continue;
                    }
                    break;
                }
                v.push(c[i]);
                i += 1;
            }
            i += 1;
            out.push(Tok::Ident(v));
        } else if ch == '\'' {
            let mut v = String::new();
            i += 1;
            while i < c.len() {
                if c[i] == ch {
                    if c.get(i + 1) == Some(&ch) {
                        v.push(ch);
                        i += 2;
                        continue;
                    }
                    break;
                }
                if c[i] == '\\' && i + 1 < c.len() {
                    i += 1;
                    v.push(match c[i] {
                        '0' => '\0',
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        o => o,
                    });
                    i += 1;
                    continue;
                }
                v.push(c[i]);
                i += 1;
            }
            i += 1;
            out.push(Tok::Str(v));
        } else if ch.is_alphanumeric() || ch == '_' {
            let start = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            out.push(Tok::Word(c[start..i].iter().collect()));
        } else {
            out.push(Tok::Sym(ch));
            i += 1;
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq)]
pub enum Scope {
    Global,
    /// A database pattern as the server prints it: `\_` and `\%` are literal characters, bare `_` and `%` wildcards.
    Db(String),
    /// A table, column or routine grant, written "db.object".
    Object(String),
    /// A role given to the account (names).
    Role,
    /// A MySQL partial revoke (`REVOKE SELECT ON db.* FROM account`): what the account lacks inside a global grant.
    Revoked(String),
    /// PROXY, partial revokes and anything else this parser does not read.
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Grant {
    pub scope: Scope,
    /// Upper case privilege names, ALL PRIVILEGES expanded on a database, USAGE dropped and GRANT OPTION added
    /// when WITH GRANT OPTION ends the line. For a role grant the role names.
    pub privs: Vec<String>,
    /// WITH GRANT OPTION ended the line, which covers every privilege on it.
    pub with_grant: bool,
    /// WITH ADMIN OPTION ended a role grant.
    pub with_admin: bool,
    pub raw: String,
}

fn is_word(t: Option<&Tok>, w: &str) -> bool {
    matches!(t, Some(Tok::Word(x)) if x.eq_ignore_ascii_case(w))
}

/// One SHOW GRANTS line. A line that is not understood is kept as `Scope::Other` with its text, so it is shown
/// and left alone, never guessed at.
pub fn parse_grant(line: &str) -> Grant {
    let other = || Grant { scope: Scope::Other, privs: Vec::new(), with_grant: false, with_admin: false, raw: line.to_string() };
    let t = tokens(line);
    let revoke = is_word(t.first(), "REVOKE");
    if !is_word(t.first(), "GRANT") && !revoke {
        return other();
    }
    let (mut i, mut items, mut cur, mut depth, mut roles, mut columns) = (1, Vec::<String>::new(), String::new(), 0, Vec::<String>::new(), false);
    while i < t.len() {
        match &t[i] {
            Tok::Word(w) if depth == 0 && (w.eq_ignore_ascii_case("ON") || w.eq_ignore_ascii_case("TO") || w.eq_ignore_ascii_case("FROM")) => break,
            Tok::Word(w) if depth == 0 => {
                if !cur.is_empty() {
                    cur.push(' ');
                }
                cur.push_str(&w.to_ascii_uppercase());
            }
            Tok::Sym('(') => {
                depth += 1;
                columns = true;
            }
            Tok::Sym(')') => depth -= 1,
            Tok::Sym(',') if depth == 0 => {
                if !cur.is_empty() {
                    items.push(std::mem::take(&mut cur));
                }
            }
            Tok::Ident(n) | Tok::Str(n) if depth == 0 => {
                // MySQL writes a role as `name`@`host`; MariaDB writes the bare name.
                if matches!(t.get(i + 1), Some(Tok::Sym('@'))) {
                    let host = match t.get(i + 2) {
                        Some(Tok::Ident(h) | Tok::Str(h)) => h.clone(),
                        _ => String::new(),
                    };
                    roles.push(format!("{n}@{host}"));
                    i += 2;
                } else {
                    roles.push(n.clone());
                }
            }
            _ => {}
        }
        i += 1;
    }
    if !cur.is_empty() {
        items.push(cur);
    }
    // A role grant has names where privileges would be, and no ON.
    if !roles.is_empty() && is_word(t.get(i), "TO") && !revoke {
        let admin = t[i..].windows(2).any(|w| is_word(Some(&w[0]), "ADMIN") && is_word(Some(&w[1]), "OPTION"));
        return Grant { scope: Scope::Role, privs: roles, with_grant: false, with_admin: admin, raw: line.to_string() };
    }
    if !is_word(t.get(i), "ON") {
        return other();
    }
    i += 1;
    let mut routine = false;
    if let Some(Tok::Word(w)) = t.get(i) {
        if ["TABLE", "FUNCTION", "PROCEDURE"].iter().any(|k| w.eq_ignore_ascii_case(k)) {
            routine = !w.eq_ignore_ascii_case("TABLE");
            i += 1;
        }
    }
    let part = |t: Option<&Tok>| match t {
        Some(Tok::Ident(n)) => Some(Some(n.clone())),
        Some(Tok::Sym('*')) => Some(None),
        _ => None,
    };
    let (Some(a), Some(Tok::Sym('.')), Some(b)) = (part(t.get(i)), t.get(i + 1), part(t.get(i + 2))) else { return other() };
    i += 3;
    if !is_word(t.get(i), if revoke { "FROM" } else { "TO" }) {
        return other();
    }
    if revoke {
        return match (a, b) {
            (Some(d), None) if !columns && !routine => Grant { scope: Scope::Revoked(d), privs: items.into_iter().filter(|p| p != "USAGE").collect(), with_grant: false, with_admin: false, raw: line.to_string() },
            _ => other(),
        };
    }
    let scope = match (a, b) {
        (None, None) => Scope::Global,
        (Some(d), None) if !columns && !routine => Scope::Db(d),
        (Some(d), Some(o)) => Scope::Object(format!("{d}.{o}")),
        (Some(d), None) => Scope::Object(format!("{d}.*")),
        (None, Some(_)) => return other(),
    };
    let mut privs: Vec<String> = Vec::new();
    // On a line of dynamic privileges only, WITH GRANT OPTION belongs to those, not to the global GRANT OPTION.
    let has_static = items.iter().any(|it| !is_dynamic(it));
    for it in items {
        match it.as_str() {
            "USAGE" => {}
            "ALL PRIVILEGES" | "ALL" if matches!(scope, Scope::Db(_)) => privs.extend(DB_PRIVS.iter().filter(|p| **p != "GRANT OPTION").map(|p| p.to_string())),
            "ALL PRIVILEGES" | "ALL" if scope == Scope::Global => privs.extend(global_static().filter(|p| *p != "GRANT OPTION").map(|p| p.to_string())),
            _ => privs.push(it),
        }
    }
    let rest = &t[i..];
    let with_grant = rest.windows(2).any(|w| is_word(Some(&w[0]), "GRANT") && is_word(Some(&w[1]), "OPTION"));
    if with_grant && (has_static || scope != Scope::Global) {
        privs.push("GRANT OPTION".into());
    }
    // MariaDB 10.5 and later print REPLICATION CLIENT as BINLOG MONITOR, which the same statements still accept either way.
    if scope == Scope::Global {
        for p in privs.iter_mut().filter(|p| *p == "BINLOG MONITOR") {
            *p = "REPLICATION CLIENT".into();
        }
    }
    if scope != Scope::Role {
        privs.sort_by_key(|p| DB_PRIVS.iter().position(|k| k == p).unwrap_or(usize::MAX));
        privs.dedup();
    }
    Grant { scope, privs, with_grant, with_admin: false, raw: line.to_string() }
}

/// What an account holds on whole databases: (pattern, privileges), merged when a database is on several lines.
pub fn db_grants(grants: &[Grant]) -> Vec<(String, BTreeSet<String>)> {
    let mut out: Vec<(String, BTreeSet<String>)> = Vec::new();
    for g in grants {
        let Scope::Db(p) = &g.scope else { continue };
        match out.iter_mut().find(|(k, _)| k == p) {
            Some((_, s)) => s.extend(g.privs.iter().cloned()),
            None => out.push((p.clone(), g.privs.iter().cloned().collect())),
        }
    }
    out
}

/// What an account holds on everything (`ON *.*`).
pub fn global_privs(grants: &[Grant]) -> BTreeSet<String> {
    grants.iter().filter(|g| g.scope == Scope::Global).flat_map(|g| g.privs.iter().cloned()).collect()
}

/// The roles an account was given, as written: `name@host` on MySQL, the bare name on MariaDB.
pub fn roles_of(grants: &[Grant]) -> Vec<String> {
    let mut v: Vec<String> = grants.iter().filter(|g| g.scope == Scope::Role).flat_map(|g| g.privs.iter().cloned()).collect();
    v.sort();
    v.dedup();
    v
}

/// Dynamic privileges held WITH GRANT OPTION: revoking one here could not be undone exactly, so the window leaves them.
pub fn dynamic_with_grant(grants: &[Grant]) -> BTreeSet<String> {
    grants.iter().filter(|g| g.scope == Scope::Global && g.with_grant).flat_map(|g| g.privs.iter().filter(|p| is_dynamic(p)).cloned()).collect()
}

/// The roles held WITH ADMIN OPTION.
pub fn roles_with_admin(grants: &[Grant]) -> BTreeSet<String> {
    grants.iter().filter(|g| g.scope == Scope::Role && g.with_admin).flat_map(|g| g.privs.iter().cloned()).collect()
}

/// A role as a person reads it: `reader@%` is just `reader`.
pub fn role_shown(r: &str) -> String {
    r.strip_suffix("@%").unwrap_or(r).to_string()
}

/// True when the lines give nothing beyond USAGE: no privilege, no role, no object.
pub fn only_usage(grants: &[Grant]) -> bool {
    grants.iter().all(|g| g.scope == Scope::Global && g.privs.is_empty())
}

/// The pattern a plain database name is granted on: `_` and `%` are wildcards in a grant, so a name that holds
/// them would also match its neighbours unless they are escaped.
pub fn pattern_of(name: &str) -> String {
    name.replace('_', "\\_").replace('%', "\\%")
}

/// A pattern as a person reads it: escaped characters shown plain, a real wildcard left as written.
pub fn shown_pattern(p: &str) -> String {
    let c: Vec<char> = p.chars().collect();
    let (mut out, mut i, mut wild) = (String::new(), 0, false);
    while i < c.len() {
        if c[i] == '\\' && matches!(c.get(i + 1), Some('_' | '%' | '\\')) {
            out.push(c[i + 1]);
            i += 2;
        } else {
            wild |= matches!(c[i], '_' | '%');
            out.push(c[i]);
            i += 1;
        }
    }
    if wild { p.to_string() } else { out }
}

/// Why a database name cannot be added, when it cannot.
pub fn check_db_name(name: &str) -> Res<()> {
    if name.trim().is_empty() {
        return Err("Name a database.".into());
    }
    if name.chars().count() > 64 {
        return Err("A database name is at most 64 characters.".into());
    }
    if name.contains('\\') || name.chars().any(char::is_control) {
        return Err("A database name cannot contain a backslash or control characters.".into());
    }
    Ok(())
}

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

fn check_privs(global: bool, privs: &[String]) -> Res<()> {
    match privs.iter().find(|p| !priv_ok(global, p)) {
        Some(p) => Err(format!("{p} is not a privilege this window gives {}.", if global { "globally" } else { "on a database" })),
        None => Ok(()),
    }
}

fn check_pattern(p: &str) -> Res<()> {
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
fn scrub(err: String, password: &str) -> String {
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
            Dialect::MySql => Caps { alter_user: at >= (5, 7, 6), lock: at >= (5, 7, 6), lifetime: at >= (5, 7, 4), roles: at >= (8, 0, 0), dynamic: at >= (8, 0, 0), plugin_choice: true, caching: at >= (8, 0, 0), native_off: at >= (8, 4, 0), sha256: at >= (5, 6, 0) && at < (9, 0, 0) },
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
    for i in 0..4 {
        if from.limits[i] != to.limits[i] {
            if to.limits[i] > LIMIT_MAX {
                return Err(format!("{} is at most {LIMIT_MAX}.", LIMIT_NAMES[i]));
            }
            with.push(format!("{} {}", LIMIT_NAMES[i], to.limits[i]));
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
fn role_literal(d: Dialect, role: &str, bs: bool) -> Res<String> {
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
        (Dialect::Postgres, Some(_)) => unreachable!("accounts are MySQL family only"),
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

fn settings_label(a: &Settings, b: &Settings) -> String {
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

// Reading the settings.

fn lifetime_of(v: Option<&str>) -> Expire {
    match v.and_then(|x| x.trim().parse::<i64>().ok()) {
        Some(0) => Expire::Never,
        Some(n) if n > 0 => Expire::Days(n as u32),
        _ => Expire::Default,
    }
}

fn info_query(d: Dialect, joined: bool) -> &'static str {
    match (d, joined) {
        (Dialect::Postgres, _) => unreachable!("accounts are MySQL family only"),
        // Numbers are cast to text: a prepared statement sends them as binary, which `text` would misread.
        (Dialect::MySql, _) => "SELECT plugin, account_locked AS locked, CAST(password_lifetime AS CHAR) AS lifetime, ssl_type, CONVERT(ssl_cipher USING utf8mb4) AS cipher, CONVERT(x509_issuer USING utf8mb4) AS issuer, CONVERT(x509_subject USING utf8mb4) AS subject, 'N' AS chain, CONCAT_WS('|', password_last_changed, plugin, LEFT(SHA2(authentication_string, 256), 16)) AS ident, CAST(max_questions AS CHAR) AS q, CAST(max_updates AS CHAR) AS u, CAST(max_connections AS CHAR) AS c, CAST(max_user_connections AS CHAR) AS uc FROM mysql.user WHERE User = ? AND Host = ?",
        (Dialect::MariaDb, true) => "SELECT u.plugin AS plugin, IFNULL(JSON_VALUE(g.Priv, '$.account_locked'), 'false') AS locked, JSON_VALUE(g.Priv, '$.password_lifetime') AS lifetime, u.ssl_type AS ssl_type, CONVERT(u.ssl_cipher USING utf8mb4) AS cipher, CONVERT(u.x509_issuer USING utf8mb4) AS issuer, CONVERT(u.x509_subject USING utf8mb4) AS subject, IF(JSON_QUERY(g.Priv, '$.auth_or') IS NULL, 'N', 'Y') AS chain, CONCAT_WS('|', u.plugin, LEFT(SHA2(u.authentication_string, 256), 16)) AS ident, CAST(u.max_questions AS CHAR) AS q, CAST(u.max_updates AS CHAR) AS u, CAST(u.max_connections AS CHAR) AS c, CAST(u.max_user_connections AS CHAR) AS uc FROM mysql.user u LEFT JOIN mysql.global_priv g ON g.User = u.User AND g.Host = u.Host WHERE u.User = ? AND u.Host = ?",
        (Dialect::MariaDb, false) => "SELECT plugin, 'false' AS locked, NULL AS lifetime, ssl_type, CONVERT(ssl_cipher USING utf8mb4) AS cipher, CONVERT(x509_issuer USING utf8mb4) AS issuer, CONVERT(x509_subject USING utf8mb4) AS subject, 'N' AS chain, CONCAT_WS('|', plugin, LEFT(SHA2(authentication_string, 256), 16)) AS ident, CAST(max_questions AS CHAR) AS q, CAST(max_updates AS CHAR) AS u, CAST(max_connections AS CHAR) AS c, CAST(max_user_connections AS CHAR) AS uc FROM mysql.user WHERE User = ? AND Host = ?",
    }
}

async fn read_info(c: &mut sqlx::MySqlConnection, d: Dialect, acct: &Account) -> Res<AccountInfo> {
    let mut last = String::new();
    for joined in [true, false] {
        match sqlx::query(info_query(d, joined)).bind(&acct.user).bind(&acct.host).persistent(false).fetch_optional(&mut *c).await {
            Ok(Some(r)) => {
                let n = |k: &str| text(&r, k).and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(0);
                let tls = match text(&r, "ssl_type").unwrap_or_default().to_ascii_uppercase().as_str() {
                    "" => Tls::None,
                    "ANY" => Tls::Ssl,
                    "X509" => Tls::X509,
                    _ => Tls::Specified,
                };
                let locked = matches!(text(&r, "locked").unwrap_or_default().to_ascii_uppercase().as_str(), "Y" | "TRUE" | "1");
                // The requirement's values mean something only under a SPECIFIED type.
                let spec = if tls == Tls::Specified { TlsSpec { cipher: text(&r, "cipher").unwrap_or_default(), issuer: text(&r, "issuer").unwrap_or_default(), subject: text(&r, "subject").unwrap_or_default() } } else { TlsSpec::default() };
                let settings = Settings { locked, expire: lifetime_of(text(&r, "lifetime").as_deref()), tls, spec, limits: [n("q"), n("u"), n("c"), n("uc")] };
                let chain = text(&r, "chain").is_some_and(|v| v.eq_ignore_ascii_case("Y"));
                return Ok(AccountInfo { plugin: text(&r, "plugin").unwrap_or_default(), settings, chain, ident: text(&r, "ident") });
            }
            Ok(None) => return Err(format!("{} has no row in mysql.user.", acct.label())),
            Err(x) => last = e(x),
        }
        if matches!(d, Dialect::MySql | Dialect::Postgres) {
            break;
        }
    }
    Err(last)
}

// Undo.

#[derive(Clone, Debug, PartialEq)]
pub enum What {
    /// Edits that ran, and the privileges each database held right after, which undo must still find.
    Grants { edits: Vec<Edit>, expect: Vec<(String, Vec<String>)> },
    /// An account this app created: dropped again while it holds nothing but USAGE.
    Created,
    /// RENAME USER from the account named in `AccountUndo::account`'s sibling: `from` is the old name.
    Renamed { from: Account },
    /// ALTER USER settings; undo requires the changed fields to still read as `to`.
    Settings { from: Settings, to: Settings },
    /// Roles given and taken away (with what each taken role carried), and how the account's roles read right after.
    Roles { grant: Vec<String>, revoke: Vec<RoleMeta>, expect: Vec<String> },
}

/// A role taken away, with what undo must give back besides the role.
#[derive(Clone, Debug, PartialEq)]
pub struct RoleMeta {
    pub role: String,
    pub admin: bool,
    pub default: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AccountUndo {
    pub account: Account,
    pub what: What,
    pub label: String,
    /// What told this account from a namesake when the change ran, when the server let it be read.
    pub ident: Option<String>,
}

fn list_text(edits: &[Edit], f: impl Fn(&Edit) -> &Vec<String>) -> String {
    edits.iter().filter(|e| !f(e).is_empty()).map(|e| format!("{} on {}", f(e).iter().map(|p| p.to_ascii_lowercase()).collect::<Vec<_>>().join(", "), if e.db.is_empty() { "*.*".to_string() } else { shown_pattern(&e.db) })).collect::<Vec<_>>().join("; ")
}

/// For the Recent changes list.
pub fn label_of(edits: &[Edit]) -> String {
    let (g, r) = (list_text(edits, |e| &e.grant), list_text(edits, |e| &e.revoke));
    match (g.is_empty(), r.is_empty()) {
        (false, true) => format!("granted {g}"),
        (true, false) => format!("revoked {r}"),
        _ => format!("granted {g}, revoked {r}"),
    }
}

fn action(conn: u64, acct: &Account, undo: AccountUndo) -> UndoAction {
    UndoAction { at: Instant::now(), conn, db: acct.label(), coll: String::new(), kind: UndoKind::Sql { database: String::new(), cols: Vec::new(), undo: SqlUndo::Account(Box::new(undo)) } }
}

// Running.

async fn grants_of(c: &mut sqlx::MySqlConnection, m: Mode, acct: &Account) -> Res<Vec<Grant>> {
    let sql = format!("SHOW GRANTS FOR {}", acct.literal(m.bs));
    let rows = match sqlx::query(&sql).persistent(false).fetch_all(&mut *c).await {
        Ok(r) => r,
        // Not every server prepares SHOW GRANTS; the text holds only the quoted account.
        Err(x) if e(&x).contains("1295") || e(&x).contains("prepared statement protocol") => c.fetch_all(sql.as_str()).await.map_err(se)?,
        Err(x) => return Err(e(x)),
    };
    Ok(rows.iter().filter_map(|r| super::worker::raw(r, 0).map(|b| parse_grant(&String::from_utf8_lossy(b)))).collect())
}

/// What an account holds in one scope. For everything (`db` empty) the partial revokes are part of it, so an undo
/// notices when they changed.
fn held(grants: &[Grant], db: &str) -> Vec<String> {
    if db.is_empty() {
        let mut v: Vec<String> = global_privs(grants).into_iter().filter(|p| priv_ok(true, p)).collect();
        v.sort_by_key(|p| order_key(p));
        let mut revoked: Vec<String> = grants.iter().filter_map(|g| if let Scope::Revoked(d) = &g.scope { Some(g.privs.iter().map(move |p| format!("-{p} on {d}"))) } else { None }).flatten().collect();
        revoked.sort();
        v.extend(revoked);
        return v;
    }
    db_grants(grants).into_iter().find(|(k, _)| k == db).map(|(_, s)| DB_PRIVS.iter().filter(|p| s.contains(**p)).map(|p| p.to_string()).collect()).unwrap_or_default()
}

/// Runs the edits in order on `c`. Returns the part that ran, and the error that stopped it.
async fn run_edits(c: &mut sqlx::MySqlConnection, m: Mode, acct: &Account, edits: &[Edit]) -> (Vec<Edit>, Option<String>) {
    let mut ran = Vec::new();
    for ed in edits {
        let mut done = Edit { db: ed.db.clone(), ..Default::default() };
        let mut err = None;
        for (privs, revoke) in [(&ed.grant, false), (&ed.revoke, true)] {
            if privs.is_empty() {
                continue;
            }
            let step = match grant_sql(m, acct, &ed.db, privs, revoke) {
                Ok(sql) => ddl::exec(c, &sql).await,
                Err(x) => Err(x),
            };
            match step {
                Ok(()) => *(if revoke { &mut done.revoke } else { &mut done.grant }) = privs.clone(),
                Err(x) => {
                    err = Some(x);
                    break;
                }
            }
        }
        if !done.is_empty() {
            ran.push(done);
        }
        if err.is_some() {
            return (ran, err);
        }
    }
    (ran, None)
}

/// Finds each edited database among the grants the server holds now. A server that folds case stores a typed `Shop`
/// as `shop`, so a missing one is looked for in another case and the edit is renamed to what is stored; when none is
/// found after a grant, nothing could be compared later and there is no undo.
fn fold_edits(g: &[Grant], edits: &mut [Edit]) -> Res<Vec<(String, Vec<String>)>> {
    let rows = db_grants(g);
    let mut out = Vec::new();
    for ed in edits.iter_mut() {
        if !ed.db.is_empty() && !ed.grant.is_empty() && !rows.iter().any(|(k, _)| *k == ed.db) {
            let alike: Vec<&String> = rows.iter().map(|(k, _)| k).filter(|k| k.eq_ignore_ascii_case(&ed.db)).collect();
            match alike.as_slice() {
                [one] => ed.db = (*one).clone(),
                _ => return Err(format!("{} is not among the account's grants after the change", shown_pattern(&ed.db))),
            }
        }
        out.push((ed.db.clone(), held(g, &ed.db)));
    }
    Ok(out)
}

/// What each touched database holds now, to compare against at undo.
async fn snapshot(c: &mut sqlx::MySqlConnection, m: Mode, acct: &Account, edits: &mut [Edit]) -> Res<Vec<(String, Vec<String>)>> {
    let g = grants_of(c, m, acct).await?;
    fold_edits(&g, edits)
}

/// The roles an account was given, each with its admin and default marks, sorted: what a role undo compares.
fn roles_state(grants: &[Grant], defaults: &[String]) -> Vec<String> {
    let admin = roles_with_admin(grants);
    let mut v: Vec<String> = roles_of(grants).into_iter().map(|r| format!("{r}{}{}", if admin.contains(&r) { " +admin" } else { "" }, if defaults.contains(&r) { " +default" } else { "" })).collect();
    v.sort();
    v
}

/// The roles an account has as default roles. Unreadable counts as none.
async fn default_roles(c: &mut sqlx::MySqlConnection, d: Dialect, acct: &Account) -> Vec<String> {
    let sql = match d {
        Dialect::MySql => "SELECT CONCAT(DEFAULT_ROLE_USER, '@', DEFAULT_ROLE_HOST) AS r FROM mysql.default_roles WHERE USER = ? AND HOST = ?",
        Dialect::MariaDb => "SELECT default_role AS r FROM mysql.user WHERE User = ? AND Host = ?",
        Dialect::Postgres => unreachable!("accounts are MySQL family only"),
    };
    match sqlx::query(sql).bind(&acct.user).bind(&acct.host).persistent(false).fetch_all(&mut *c).await {
        Ok(rows) => rows.iter().filter_map(|r| text(r, "r")).filter(|r| !r.is_empty()).collect(),
        Err(_) => Vec::new(),
    }
}

/// One prepared statement; a server that cannot prepare it (error 1295) gets the text as is, which holds only quoted
/// names and the one secret the caller scrubs from errors. The log redacts the secret, so both readings are noted.
async fn exec_any(c: &mut sqlx::MySqlConnection, sql: &str) -> Res<()> {
    match ddl::exec(c, sql).await {
        Err(x) if x.contains("1295") || x.contains("prepared statement protocol") => {
            let t = std::time::Instant::now();
            let res = c.execute(sql).await.map(|_| ());
            crate::db::note_here(sql, t, &res);
            res.map_err(se)
        }
        r => r,
    }
}

async fn ident(c: &mut sqlx::MySqlConnection, d: Dialect, acct: &Account) -> Option<String> {
    read_info(c, d, acct).await.ok().and_then(|i| i.ident)
}

/// What a finished account change says to the window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    Granted,
    Created,
    Dropped,
    /// An undo from Recent changes; `account` is the account it acted on.
    Undone,
    /// A rename: `account` is the name it has now.
    Renamed,
    /// The undo of a rename: `account` is the name it has now.
    UndoneRename,
    Password,
}

#[allow(clippy::too_many_arguments)]
async fn run_plan(c: &mut sqlx::MySqlConnection, db: &Db, conn: u64, acct: &Account, plan: &AccountPlan, cur: &mut Account, renamed: &mut bool, undo: &mut Vec<UndoAction>) -> Res<String> {
    let m = session_mode(c).await?;
    let d = db.dialect;
    let total = plan_sql(m, d, acct, plan)?.len();
    let mut done = 0;
    let fail = |x: String, done: usize| Err(format!("{x}. {done} of {total} statements ran before it stopped."));
    if let Some(to) = &plan.rename {
        let sql = rename_sql(m, acct, to)?;
        if let Err(x) = ddl::exec(c, &sql).await {
            return fail(x, done);
        }
        done += 1;
        *renamed = true;
        *cur = Account { user: to.user.clone(), host: to.host.clone(), ..acct.clone() };
        let id = ident(c, d, cur).await;
        undo.push(action(conn, cur, AccountUndo { account: cur.clone(), label: format!("renamed {} to {}", acct.label(), cur.label()), what: What::Renamed { from: acct.clone() }, ident: id }));
    }
    if let Some((a, b)) = plan.settings.as_ref().filter(|(a, b)| a != b) {
        let Some(sql) = alter_sql(m, cur, a, b)? else { return Ok(String::new()) };
        if let Err(x) = ddl::exec(c, &sql).await {
            return fail(x, done);
        }
        done += 1;
        let id = ident(c, d, cur).await;
        undo.push(action(conn, cur, AccountUndo { account: cur.clone(), label: settings_label(a, b), what: What::Settings { from: a.clone(), to: b.clone() }, ident: id }));
    }
    // What each role about to be taken away carries, read first: undo gives it back with the role.
    let (before, defaults) = if plan.roles_revoke.is_empty() { (Vec::new(), Vec::new()) } else { (grants_of(c, m, cur).await.unwrap_or_default(), default_roles(c, d, cur).await) };
    let admin = roles_with_admin(&before);
    let (mut gr, mut rv): (Vec<String>, Vec<RoleMeta>) = (Vec::new(), Vec::new());
    let mut stop = None;
    for r in &plan.roles_revoke {
        match ddl::exec(c, &role_sql(m, d, cur, r, true)?).await {
            Ok(()) => {
                done += 1;
                rv.push(RoleMeta { role: r.clone(), admin: admin.contains(r), default: defaults.contains(r) });
            }
            Err(x) => {
                stop = Some(x);
                break;
            }
        }
    }
    if stop.is_none() {
        for r in &plan.roles_grant {
            match ddl::exec(c, &role_sql(m, d, cur, r, false)?).await {
                Ok(()) => {
                    done += 1;
                    gr.push(r.clone());
                }
                Err(x) => {
                    stop = Some(x);
                    break;
                }
            }
        }
    }
    if !gr.is_empty() || !rv.is_empty() {
        if let Ok(g) = grants_of(c, m, cur).await {
            let defaults_now = default_roles(c, d, cur).await;
            let names = |v: Vec<&String>| v.into_iter().map(|r| role_shown(r)).collect::<Vec<_>>().join(", ");
            let label = [(!gr.is_empty()).then(|| format!("gave role {}", names(gr.iter().collect()))), (!rv.is_empty()).then(|| format!("took role {}", names(rv.iter().map(|r| &r.role).collect())))].into_iter().flatten().collect::<Vec<_>>().join(", ");
            let id = ident(c, d, cur).await;
            undo.push(action(conn, cur, AccountUndo { account: cur.clone(), label, what: What::Roles { grant: gr, revoke: rv, expect: roles_state(&g, &defaults_now) }, ident: id }));
        }
    }
    if let Some(x) = stop {
        return fail(x, done);
    }
    Ok(format!("Changed {}", cur.label()))
}

impl Worker {
    /// The accounts of the server, or only the logged in one when mysql.user is closed to this user.
    pub fn sql_users(&self, conn: u64, req: u64, db: Db) {
        self.spawn(async move { Msg::SqlUsers { conn, req, res: load_list(&db).await } });
    }

    /// What SHOW GRANTS says for one account.
    pub fn sql_user_grants(&self, conn: u64, req: u64, db: Db, acct: Account) {
        self.spawn(async move {
            let res = async {
                let mut c = db.acquire().await?;
                let m = session_mode(&mut c).await?;
                let grants = grants_of(&mut c, m, &acct).await?;
                let (info, info_err) = if acct.is_role {
                    (None, Some("A role has no login settings.".to_string()))
                } else {
                    match read_info(&mut c, db.dialect, &acct).await {
                        Ok(i) => (Some(i), None),
                        Err(x) => (None, Some(x)),
                    }
                };
                Ok(Loaded { grants, info, info_err })
            }
            .await;
            Msg::SqlUserGrants { conn, req, res }
        });
    }

    /// Runs privilege edits for one account. Undo is offered for the part that ran, also when a later
    /// statement failed.
    pub fn sql_user_edit(&self, conn: u64, db: Db, acct: Account, edits: Vec<Edit>, read_only: bool) {
        self.spawn_sql(db.conn, "", async move {
            let mut undo = None;
            let res: Res<String> = async {
                let mut c = writer_conn(&db, "", read_only).await?.into_my()?;
                let out = async {
                    let m = session_mode(&mut c).await?;
                    // Checked on the writer, against what the server holds now rather than what the window last read.
                    check_partial(&grants_of(&mut c, m, &acct).await?, &edits)?;
                    let (mut ran, err) = run_edits(&mut c, m, &acct, &edits).await;
                    let mut note = String::new();
                    if !ran.is_empty() {
                        // Without a snapshot there is nothing to check against, so no undo is offered.
                        match snapshot(&mut c, m, &acct, &mut ran).await {
                            Ok(expect) => {
                                let id = ident(&mut c, db.dialect, &acct).await;
                                undo = Some(action(conn, &acct, AccountUndo { account: acct.clone(), label: label_of(&ran), what: What::Grants { edits: ran.clone(), expect }, ident: id }));
                            }
                            Err(why) => note = format!(" There is no undo for it: {why}."),
                        }
                    }
                    match err {
                        None => Ok(format!("Changed privileges of {}.{note}", acct.label())),
                        Some(x) => Err(format!("{x}. {} of {} changes were made before it stopped.{note}", ran.len(), edits.len())),
                    }
                }
                .await;
                let _ = c.close().await;
                out
            }
            .await;
            Msg::SqlUserDone { conn, account: acct, op: Op::Granted, res, undo: undo.into_iter().collect() }
        });
    }

    /// CREATE USER. The password is used for the one statement and dropped with this task.
    pub fn sql_user_create(&self, conn: u64, db: Db, acct: Account, password: String, read_only: bool) {
        self.spawn_sql(db.conn, "", async move {
            let mut undo = None;
            let res: Res<String> = async {
                check_new_user(&acct.user, &acct.host, &password)?;
                let mut c = writer_conn(&db, "", read_only).await?.into_my()?;
                let out = async {
                    let m = session_mode(&mut c).await?;
                    ddl::exec(&mut c, &create_user_sql(m, &acct, &password, false)).await.map_err(|x| scrub(x, &password))?;
                    let id = ident(&mut c, db.dialect, &acct).await;
                    undo = Some(action(conn, &acct, AccountUndo { account: acct.clone(), label: "created user".into(), what: What::Created, ident: id }));
                    Ok(format!("Created user {}", acct.label()))
                }
                .await;
                let _ = c.close().await;
                out
            }
            .await;
            Msg::SqlUserDone { conn, account: acct, op: Op::Created, res, undo: undo.into_iter().collect() }
        });
    }

    /// ALTER USER ... IDENTIFIED BY. Nothing is recorded to undo it, and the password is scrubbed from any error.
    pub fn sql_user_password(&self, conn: u64, db: Db, acct: Account, plugin: Option<String>, password: String, read_only: bool) {
        self.spawn_sql(db.conn, "", async move {
            let res: Res<String> = async {
                check_password(&password)?;
                let mut c = writer_conn(&db, "", read_only).await?.into_my()?;
                let out = async {
                    let m = session_mode(&mut c).await?;
                    if matches!(db.dialect, Dialect::MariaDb) {
                        // SET PASSWORD sets the mysql_native_password part only; any other way in would be left as it was.
                        if let Ok(i) = read_info(&mut c, db.dialect, &acct).await {
                            if i.chain || (!i.plugin.is_empty() && i.plugin != "mysql_native_password") {
                                return Err(format!("{} logs in with {}, and Quarry only sets a mysql_native_password password on MariaDB. Change it in a SQL shell.", acct.label(), if i.chain { "more than one method".to_string() } else { i.plugin }));
                            }
                        }
                    }
                    let sql = password_sql(m, db.dialect, &acct, plugin.as_deref(), &password, false)?;
                    exec_any(&mut c, &sql).await.map_err(|x| scrub(x, &password))?;
                    Ok(format!("Changed the password of {}", acct.label()))
                }
                .await;
                let _ = c.close().await;
                out
            }
            .await;
            Msg::SqlUserDone { conn, account: acct, op: Op::Password, res, undo: Vec::new() }
        });
    }

    /// Runs a plan: the rename, the ALTER USER and the role changes, each recorded for undo as it succeeds.
    pub fn sql_user_account(&self, conn: u64, db: Db, acct: Account, plan: AccountPlan, read_only: bool) {
        self.spawn_sql(db.conn, "", async move {
            let mut undo = Vec::new();
            let mut cur = acct.clone();
            let mut renamed = false;
            let res: Res<String> = async {
                let mut c = writer_conn(&db, "", read_only).await?.into_my()?;
                let out = run_plan(&mut c, &db, conn, &acct, &plan, &mut cur, &mut renamed, &mut undo).await;
                let _ = c.close().await;
                out
            }
            .await;
            Msg::SqlUserDone { conn, account: cur, op: if renamed { Op::Renamed } else { Op::Granted }, res, undo }
        });
    }

    /// DROP USER. No undo: the password and every grant are gone with it.
    pub fn sql_user_drop(&self, conn: u64, db: Db, acct: Account, read_only: bool) {
        self.spawn_sql(db.conn, "", async move {
            let res: Res<String> = async {
                let mut c = writer_conn(&db, "", read_only).await?.into_my()?;
                let out = async {
                    let m = session_mode(&mut c).await?;
                    ddl::exec(&mut c, &drop_user_sql(m, &acct, db.dialect)).await?;
                    Ok(format!("Dropped user {}", acct.label()))
                }
                .await;
                let _ = c.close().await;
                out
            }
            .await;
            Msg::SqlUserDone { conn, account: acct, op: Op::Dropped, res, undo: Vec::new() }
        });
    }
}

/// Runs a recorded undo after checking the account is still as the change left it. The action comes back in
/// the answer when nothing ran, so it stays in the list.
pub(super) async fn undo(db: &Db, a: UndoAction, u: AccountUndo, read_only: bool) -> Msg {
    let (conn, acct) = (a.conn, u.account.clone());
    // Whether the undo ran something before it stopped; an entry that changed nothing stays in the list.
    let mut ran_any = false;
    let res: Res<String> = async {
        let mut c = writer_conn(db, "", read_only).await?.into_my()?;
        let out = async {
            let m = session_mode(&mut c).await?;
            let now = grants_of(&mut c, m, &acct).await.map_err(|x| format!("Undo refused: {x}"))?;
            if let (Some(want), Some(got)) = (&u.ident, ident(&mut c, db.dialect, &acct).await) {
                if *want != got {
                    return Err(format!("Undo refused: {} is not the account this change was made on (it was dropped and made again, or its password changed).", acct.label()));
                }
            }
            match &u.what {
                What::Grants { edits, expect } => {
                    for (d, want) in expect {
                        if held(&now, d) != *want {
                            return Err(format!("Undo refused: the privileges of {} on {} changed after this change ran, so undoing it now would not put them back.", acct.label(), shown_pattern(d)));
                        }
                    }
                    let back: Vec<Edit> = edits.iter().rev().map(Edit::inverse).collect();
                    let (ran, err) = run_edits(&mut c, m, &acct, &back).await;
                    ran_any = !ran.is_empty();
                    match err {
                        None => Ok(format!("Undid: {}", u.label)),
                        Some(x) => Err(format!("{x}. {} of {} steps were undone before it stopped.", ran.len(), back.len())),
                    }
                }
                What::Renamed { from } => {
                    ddl::exec(&mut c, &rename_sql(m, &acct, from)?).await?;
                    Ok(format!("Undid: {}", u.label))
                }
                What::Settings { from, to } => {
                    let cur = read_info(&mut c, db.dialect, &acct).await.map_err(|x| format!("Undo refused: {x}"))?.settings;
                    let same = |a: &Settings, b: &Settings| (from.locked == to.locked || a.locked == b.locked) && (from.expire == to.expire || a.expire == b.expire) && ((from.tls == to.tls && from.spec == to.spec) || (a.tls == b.tls && a.spec == b.spec)) && (0..4).all(|i| from.limits[i] == to.limits[i] || a.limits[i] == b.limits[i]);
                    if !same(&cur, to) {
                        return Err(format!("Undo refused: the settings of {} changed after this change ran, so undoing it now would not put them back.", acct.label()));
                    }
                    // Only the fields this change touched go back; the others keep what they hold now.
                    let mut back = cur.clone();
                    if from.locked != to.locked {
                        back.locked = from.locked;
                    }
                    if from.expire != to.expire {
                        back.expire = from.expire;
                    }
                    if from.tls != to.tls || from.spec != to.spec {
                        back.tls = from.tls;
                        back.spec = from.spec.clone();
                    }
                    for i in 0..4 {
                        if from.limits[i] != to.limits[i] {
                            back.limits[i] = from.limits[i];
                        }
                    }
                    if let Some(sql) = alter_sql(m, &acct, &cur, &back)? {
                        ddl::exec(&mut c, &sql).await?;
                    }
                    Ok(format!("Undid: {}", u.label))
                }
                What::Roles { grant, revoke, expect } => {
                    let defaults = default_roles(&mut c, db.dialect, &acct).await;
                    if roles_state(&now, &defaults) != *expect {
                        return Err(format!("Undo refused: the roles of {} changed after this change ran, so undoing it now would not put them back.", acct.label()));
                    }
                    for r in grant {
                        ddl::exec(&mut c, &role_sql(m, db.dialect, &acct, r, true)?).await?;
                        ran_any = true;
                    }
                    for r in revoke {
                        ddl::exec(&mut c, &role_grant_sql(m, db.dialect, &acct, &r.role, false, r.admin)?).await?;
                        ran_any = true;
                    }
                    // Default roles go back as they were: MySQL takes the whole list, so the ones it holds now stay in it.
                    let mut want: Vec<String> = defaults;
                    want.extend(revoke.iter().filter(|r| r.default).map(|r| r.role.clone()));
                    want.sort();
                    want.dedup();
                    if revoke.iter().any(|r| r.default) {
                        if let Some(sql) = default_roles_sql(m, db.dialect, &acct, &want)? {
                            exec_any(&mut c, &sql).await?;
                        }
                    }
                    Ok(format!("Undid: {}", u.label))
                }
                What::Created => {
                    if !only_usage(&now) {
                        return Err(format!("Not undone: {} has been given privileges since, so it is not dropped.", acct.label()));
                    }
                    ddl::exec(&mut c, &drop_user_sql(m, &acct, db.dialect)).await?;
                    Ok(format!("Dropped user {}", acct.label()))
                }
            }
        }
        .await;
        let _ = c.close().await;
        out
    }
    .await;
    // Any failure that ran nothing keeps the entry, whatever its cause; a partial run does not, since what it
    // expected no longer holds.
    let keep = res.is_err() && !ran_any;
    let undo: Vec<UndoAction> = keep.then(|| UndoAction { at: a.at, conn, db: a.db, coll: a.coll, kind: a.kind }).into_iter().collect();
    let renamed = res.is_ok() && matches!(u.what, What::Renamed { .. });
    let op = if renamed { Op::UndoneRename } else { Op::Undone };
    let acct = if renamed { match &u.what { What::Renamed { from } => from.clone(), _ => acct } } else { acct };
    Msg::SqlUserDone { conn, account: acct, op, res, undo }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sql::testing::every_server;

    fn set(p: &[&str]) -> BTreeSet<String> {
        p.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn mysql_8_4_grant_lines() {
        let g = parse_grant("GRANT USAGE ON *.* TO `report`@`10.0.%`");
        assert_eq!(g.scope, Scope::Global);
        assert!(g.privs.is_empty());
        let g = parse_grant("GRANT SELECT, INSERT, UPDATE ON `shop`.* TO `report`@`10.0.%`");
        assert_eq!(g.scope, Scope::Db("shop".into()));
        assert_eq!(g.privs, ["SELECT", "INSERT", "UPDATE"]);
        let g = parse_grant("GRANT SELECT ON `my\\_db`.* TO `u`@`%` WITH GRANT OPTION");
        assert_eq!(g.scope, Scope::Db("my\\_db".into()));
        assert_eq!(g.privs, ["SELECT", "GRANT OPTION"]);
        let g = parse_grant("GRANT ALL PRIVILEGES ON `shop`.* TO `u`@`%`");
        assert_eq!(g.privs.len(), DB_PRIVS.len() - 1);
        assert!(!g.privs.contains(&"GRANT OPTION".to_string()));
        let g = parse_grant("GRANT CREATE TEMPORARY TABLES, LOCK TABLES ON `a``b`.* TO `u`@`%`");
        assert_eq!(g.scope, Scope::Db("a`b".into()));
        assert_eq!(g.privs, ["CREATE TEMPORARY TABLES", "LOCK TABLES"]);
    }

    #[test]
    fn global_and_object_grants_are_not_database_rows() {
        let g = parse_grant("GRANT SELECT, PROCESS, REPLICATION CLIENT ON *.* TO `u`@`%`");
        assert_eq!(g.scope, Scope::Global);
        assert_eq!(g.privs, ["SELECT", "PROCESS", "REPLICATION CLIENT"]);
        assert!(!only_usage(&[g]));
        let g = parse_grant("GRANT SELECT, INSERT ON `shop`.`orders` TO `u`@`%`");
        assert_eq!(g.scope, Scope::Object("shop.orders".into()));
        let g = parse_grant("GRANT SELECT (`a`, `b`), UPDATE (`a`) ON `shop`.`orders` TO `u`@`%`");
        assert_eq!(g.scope, Scope::Object("shop.orders".into()));
        let g = parse_grant("GRANT EXECUTE ON PROCEDURE `shop`.`p` TO `u`@`%`");
        assert_eq!(g.scope, Scope::Object("shop.p".into()));
        let g = parse_grant("GRANT EXECUTE ON `shop`.* TO `u`@`%`");
        assert_eq!(g.scope, Scope::Db("shop".into()));
        assert_eq!(db_grants(&[g]).len(), 1);
    }

    #[test]
    fn role_and_unknown_lines_are_kept_not_guessed() {
        let g = parse_grant("GRANT `reader`@`%`, `w`@`h` TO `u`@`%`");
        assert_eq!((g.scope, g.privs), (Scope::Role, vec!["reader@%".to_string(), "w@h".to_string()]));
        let g = parse_grant("GRANT `reader`, `writer` TO `u`@`%`");
        assert_eq!((g.scope, g.privs), (Scope::Role, vec!["reader".to_string(), "writer".to_string()]));
        for l in ["GRANT PROXY ON ''@'' TO `u`@`%`", "REVOKE SELECT ON `a`.`t` FROM `u`@`%`", "something else", "GRANT SELECT ON TO"] {
            let g = parse_grant(l);
            assert_eq!(g.scope, Scope::Other, "{l}");
            assert_eq!(g.raw, l);
        }
    }

    #[test]
    fn mariadb_11_4_grant_lines() {
        let g = parse_grant("GRANT USAGE ON *.* TO `u`@`%` IDENTIFIED VIA mysql_native_password USING '*2470C0C06DEE42FD1618BB99005ADCA2EC9D1E19'");
        assert_eq!(g.scope, Scope::Global);
        assert!(g.privs.is_empty());
        let g = parse_grant("GRANT SELECT, DELETE HISTORY ON `shop`.* TO `u`@`%` IDENTIFIED BY PASSWORD '*ABC' WITH GRANT OPTION");
        assert_eq!(g.privs, ["SELECT", "GRANT OPTION", "DELETE HISTORY"]);
        let g = parse_grant("GRANT ALL PRIVILEGES ON `shop`.* TO `u`@`localhost`");
        assert_eq!(g.privs.len(), DB_PRIVS.len() - 1);
        assert_eq!(parse_grant("GRANT PROCESS, BINLOG MONITOR ON *.* TO `u`@`%`").privs, ["PROCESS", "REPLICATION CLIENT"]);
        let g = parse_grant("GRANT `r1` TO `u`@`%`");
        assert_eq!(g.scope, Scope::Role);
        // A string that says WITH GRANT OPTION is a hash, not the clause.
        let g = parse_grant("GRANT SELECT ON `shop`.* TO `u`@`%` IDENTIFIED BY PASSWORD 'GRANT OPTION'");
        assert_eq!(g.privs, ["SELECT"]);
    }

    #[test]
    fn odd_names_parse_back_exactly() {
        let g = parse_grant("GRANT SELECT ON `we``ird\\_%db`.* TO `o'brien`@`h'ost`");
        assert_eq!(g.scope, Scope::Db("we`ird\\_%db".into()));
        let g = parse_grant("GRANT SELECT ON `100\\%_sure`.* TO `a`@`b`");
        assert_eq!(g.scope, Scope::Db("100\\%_sure".into()));
        assert_eq!(shown_pattern("my\\_db"), "my_db");
        assert_eq!(shown_pattern("50\\%"), "50%");
        assert_eq!(shown_pattern("shop_%"), "shop_%");
        assert_eq!(pattern_of("my_db%"), "my\\_db\\%");
        assert_eq!(shown_pattern(&pattern_of("my_db%")), "my_db%");
        // The statement writes the pattern back as it was read.
        let sql = grant_sql(Mode::DEFAULT, &Account::named("u", "%"), "we`ird\\_%db", &["SELECT".into()], false).unwrap();
        assert_eq!(sql, "GRANT SELECT ON `we``ird\\_%db`.* TO 'u'@'%'");
    }

    #[test]
    fn db_rows_merge_and_keep_order() {
        let g = [parse_grant("GRANT SELECT ON `a`.* TO `u`@`%`"), parse_grant("GRANT INSERT ON `b`.* TO `u`@`%`"), parse_grant("GRANT DELETE ON `a`.* TO `u`@`%`"), parse_grant("GRANT USAGE ON *.* TO `u`@`%`")];
        assert_eq!(db_grants(&g), vec![("a".to_string(), set(&["SELECT", "DELETE"])), ("b".to_string(), set(&["INSERT"]))]);
    }

    #[test]
    fn statements_and_inverses() {
        let u = Account::named("report", "10.0.%");
        let ed = diff("shop", &set(&["SELECT", "INSERT"]), &set(&["SELECT", "EXECUTE", "DELETE"]));
        assert_eq!(ed.grant, ["DELETE", "EXECUTE"]);
        assert_eq!(ed.revoke, ["INSERT"]);
        assert_eq!(edit_sql(Mode::DEFAULT, &u, &ed).unwrap(), ["GRANT DELETE, EXECUTE ON `shop`.* TO 'report'@'10.0.%'", "REVOKE INSERT ON `shop`.* FROM 'report'@'10.0.%'"]);
        let back = ed.inverse();
        assert_eq!(back.grant, ["INSERT"]);
        assert_eq!(back.revoke, ["DELETE", "EXECUTE"]);
        assert_eq!(back.inverse(), ed);
        assert_eq!(preview(Mode::DEFAULT, &u, &[Edit { db: "analytics".into(), grant: vec!["SELECT".into(), "EXECUTE".into()], revoke: vec![] }]).unwrap(), "GRANT SELECT, EXECUTE ON `analytics`.* TO 'report'@'10.0.%';");
        assert!(diff("x", &set(&["SELECT"]), &set(&["SELECT"])).is_empty());
        // A privilege this window does not know is left alone.
        assert!(diff("x", &set(&["SELECT", "DELETE HISTORY"]), &set(&["SELECT"])).is_empty());
    }

    #[test]
    fn privilege_names_never_come_from_text() {
        let u = Account::named("u", "%");
        assert!(grant_sql(Mode::DEFAULT, &u, "db", &["SELECT ON *.* TO 'x'; --".into()], false).is_err());
        assert!(grant_sql(Mode::DEFAULT, &u, "db", &[], false).is_err());
        assert!(grant_sql(Mode::DEFAULT, &u, "d\0b", &["SELECT".into()], false).is_err());
    }

    #[test]
    fn account_strings_follow_the_sql_mode() {
        let u = Account::named("o'b\\x", "h'");
        assert_eq!(u.literal(true), "'o''b\\\\x'@'h'''");
        assert_eq!(u.literal(false), "'o''b\\x'@'h'''");
        let sql = grant_sql(Mode { bs: false, ansi: true }, &u, "d", &["SELECT".into()], true).unwrap();
        assert_eq!(sql, "REVOKE SELECT ON `d`.* FROM 'o''b\\x'@'h'''");
        assert_eq!(Account { user: "r".into(), is_role: true, ..Default::default() }.literal(true), "'r'");
        assert_eq!(drop_user_sql(Mode::DEFAULT, &Account { user: "r".into(), is_role: true, ..Default::default() }, Dialect::MariaDb), "DROP ROLE 'r'");
        assert_eq!(drop_user_sql(Mode::DEFAULT, &Account::named("u", "h"), Dialect::MariaDb), "DROP USER 'u'@'h'");
    }

    #[test]
    fn the_password_is_hidden_in_every_preview() {
        let u = Account::named("app", "%");
        let shown = create_user_sql(Mode::DEFAULT, &u, "s3cr'et", true);
        assert_eq!(shown, "CREATE USER 'app'@'%' IDENTIFIED BY '***'");
        assert!(!shown.contains("s3cr"));
        assert_eq!(create_user_sql(Mode::DEFAULT, &u, "s3cr'et", false), "CREATE USER 'app'@'%' IDENTIFIED BY 's3cr''et'");
        assert!(!scrub("syntax near 's3cr''et'".into(), "s3cr'et").contains("s3cr"));
        assert!(check_new_user("app", "%", "").is_err());
        assert!(check_new_user("", "%", "x").is_err());
        assert!(check_new_user("app", "", "x").is_err());
        assert!(check_new_user("a\nb", "%", "x").is_err());
        assert!(check_new_user("app", "%", "x").is_ok());
    }

    #[test]
    fn labels_for_recent_changes() {
        let ed = [Edit { db: "my\\_db".into(), grant: vec!["SELECT".into()], revoke: vec![] }];
        assert_eq!(label_of(&ed), "granted select on my_db");
        assert_eq!(label_of(&[ed[0].inverse()]), "revoked select on my_db");
        assert_eq!(split_account("o@brien@%"), ("o@brien".to_string(), "%".to_string()));
    }

    #[test]
    fn global_privileges_use_everything_as_the_scope() {
        let u = Account::named("report", "10.0.%");
        let ed = diff("", &set(&["SELECT", "SHOW VIEW", "REPLICATION CLIENT", "CONNECTION_ADMIN"]), &set(&["SELECT", "SHOW VIEW", "REPLICATION CLIENT", "CONNECTION_ADMIN", "PROCESS", "SYSTEM_VARIABLES_ADMIN"]));
        assert_eq!(ed.grant, ["PROCESS", "SYSTEM_VARIABLES_ADMIN"]);
        assert!(ed.revoke.is_empty());
        assert_eq!(edit_sql(Mode::DEFAULT, &u, &ed).unwrap(), ["GRANT PROCESS, SYSTEM_VARIABLES_ADMIN ON *.* TO 'report'@'10.0.%'"]);
        assert_eq!(edit_sql(Mode::DEFAULT, &u, &ed.inverse()).unwrap(), ["REVOKE PROCESS, SYSTEM_VARIABLES_ADMIN ON *.* FROM 'report'@'10.0.%'"]);
        // A privilege that is not on the design's list or a dynamic name is never written.
        assert!(grant_sql(Mode::DEFAULT, &u, "", &["FILE".into()], false).is_err());
        assert!(grant_sql(Mode::DEFAULT, &u, "", &["X_ADMIN ON *.* TO 'a'; --".into()], false).is_err());
        assert!(grant_sql(Mode::DEFAULT, &u, "", &["lower_case".into()], false).is_err());
        assert!(grant_sql(Mode::DEFAULT, &u, "", &["ADMIN".into()], false).is_err());
        assert!(is_dynamic("CONNECTION_ADMIN") && is_dynamic("BINLOG_ADMIN") && !is_dynamic("SELECT") && !is_dynamic("A_B;"));
        // A database privilege name is not valid as a global one by being dynamic-looking.
        assert!(grant_sql(Mode::DEFAULT, &u, "db", &["CONNECTION_ADMIN".into()], false).is_err());
        let g = parse_grant("GRANT SELECT, PROCESS ON *.* TO `u`@`%`");
        let h = parse_grant("GRANT CONNECTION_ADMIN, SYSTEM_VARIABLES_ADMIN ON *.* TO `u`@`%` WITH GRANT OPTION");
        // A grant option on a line of dynamic privileges belongs to those, not to everything.
        assert_eq!(global_privs(&[g.clone(), h.clone()]), set(&["SELECT", "PROCESS", "CONNECTION_ADMIN", "SYSTEM_VARIABLES_ADMIN"]));
        assert_eq!(held(&[g, h.clone()], ""), ["SELECT", "PROCESS", "CONNECTION_ADMIN", "SYSTEM_VARIABLES_ADMIN"]);
        assert_eq!(dynamic_with_grant(&[h]), set(&["CONNECTION_ADMIN", "SYSTEM_VARIABLES_ADMIN"]));
        let all = parse_grant("GRANT ALL PRIVILEGES ON *.* TO `u`@`%` WITH GRANT OPTION");
        assert!(all.privs.contains(&"SUPER".to_string()) && all.privs.contains(&"GRANT OPTION".to_string()));
    }

    #[test]
    fn alter_user_statements_and_inverses() {
        let u = Account::named("report", "10.0.%");
        let base = Settings::default();
        let locked = Settings { locked: true, ..base.clone() };
        assert_eq!(alter_sql(Mode::DEFAULT, &u, &base, &locked).unwrap().unwrap(), "ALTER USER 'report'@'10.0.%' ACCOUNT LOCK");
        assert_eq!(alter_sql(Mode::DEFAULT, &u, &locked, &base).unwrap().unwrap(), "ALTER USER 'report'@'10.0.%' ACCOUNT UNLOCK");
        assert_eq!(alter_sql(Mode::DEFAULT, &u, &base, &base).unwrap(), None);
        let lim = Settings { limits: [5000, 0, 0, 3], ..base.clone() };
        assert_eq!(alter_sql(Mode::DEFAULT, &u, &base, &lim).unwrap().unwrap(), "ALTER USER 'report'@'10.0.%' WITH MAX_QUERIES_PER_HOUR 5000 MAX_USER_CONNECTIONS 3");
        assert_eq!(alter_sql(Mode::DEFAULT, &u, &lim, &base).unwrap().unwrap(), "ALTER USER 'report'@'10.0.%' WITH MAX_QUERIES_PER_HOUR 0 MAX_USER_CONNECTIONS 0");
        let all = Settings { locked: true, expire: Expire::Days(90), tls: Tls::X509, limits: [1, 2, 3, 4], ..Default::default() };
        assert_eq!(
            alter_sql(Mode::DEFAULT, &u, &base, &all).unwrap().unwrap(),
            "ALTER USER 'report'@'10.0.%' REQUIRE X509 WITH MAX_QUERIES_PER_HOUR 1 MAX_UPDATES_PER_HOUR 2 MAX_CONNECTIONS_PER_HOUR 3 MAX_USER_CONNECTIONS 4 PASSWORD EXPIRE INTERVAL 90 DAY ACCOUNT LOCK"
        );
        let never = Settings { expire: Expire::Never, ..base.clone() };
        assert_eq!(alter_sql(Mode::DEFAULT, &u, &base, &never).unwrap().unwrap(), "ALTER USER 'report'@'10.0.%' PASSWORD EXPIRE NEVER");
        assert_eq!(alter_sql(Mode::DEFAULT, &u, &never, &base).unwrap().unwrap(), "ALTER USER 'report'@'10.0.%' PASSWORD EXPIRE DEFAULT");
        assert!(alter_sql(Mode::DEFAULT, &u, &base, &Settings { expire: Expire::Days(0), ..base.clone() }).is_err());
        assert!(alter_sql(Mode::DEFAULT, &u, &base, &Settings { expire: Expire::Days(70000), ..base.clone() }).is_err());
        assert!(alter_sql(Mode::DEFAULT, &u, &base, &Settings { limits: [LIMIT_MAX + 1, 0, 0, 0], ..base.clone() }).is_err());
        assert!(alter_sql(Mode::DEFAULT, &u, &base, &Settings { tls: Tls::Specified, ..base.clone() }).is_err());
        // A requirement this window cannot write is never touched when nothing else about TLS changed.
        let spec = Settings { tls: Tls::Specified, ..base.clone() };
        assert_eq!(alter_sql(Mode::DEFAULT, &u, &spec, &Settings { locked: true, ..spec.clone() }).unwrap().unwrap(), "ALTER USER 'report'@'10.0.%' ACCOUNT LOCK");
    }

    #[test]
    fn password_statements_hide_the_password_and_take_plugins_from_a_list() {
        let u = Account::named("a", "%");
        assert_eq!(password_sql(Mode::DEFAULT, Dialect::MySql, &u, None, "p'w", true).unwrap(), "ALTER USER 'a'@'%' IDENTIFIED BY '***'");
        assert_eq!(password_sql(Mode::DEFAULT, Dialect::MySql, &u, Some("mysql_native_password"), "p'w", false).unwrap(), "ALTER USER 'a'@'%' IDENTIFIED WITH mysql_native_password BY 'p''w'");
        assert!(password_sql(Mode::DEFAULT, Dialect::MySql, &u, Some("x' BY 'y"), "p", true).is_err());
        // MariaDB has no plugin clause here.
        // MariaDB's ALTER USER ... IDENTIFIED BY would replace every way of logging in, SET PASSWORD keeps the others.
        assert_eq!(password_sql(Mode::DEFAULT, Dialect::MariaDb, &u, Some("mysql_native_password"), "pw", true).unwrap(), "SET PASSWORD FOR 'a'@'%' = PASSWORD('***')");
        assert_eq!(password_sql(Mode::DEFAULT, Dialect::MariaDb, &u, None, "p'w", false).unwrap(), "SET PASSWORD FOR 'a'@'%' = PASSWORD('p''w')");
        assert!(check_password("").is_err() && check_password("a\0b").is_err() && check_password("x").is_ok());
        let shown = password_sql(Mode::DEFAULT, Dialect::MySql, &u, Some("caching_sha2_password"), "hunter2", true).unwrap();
        assert!(!shown.contains("hunter2"));
    }

    #[test]
    fn a_plan_runs_in_order_on_the_new_name() {
        let u = Account::named("report", "10.0.%");
        let plan = AccountPlan {
            rename: Some(Account::named("reporting", "10.0.%")),
            settings: Some((Settings::default(), Settings { locked: true, ..Default::default() })),
            roles_grant: vec!["app_read@%".into()],
            roles_revoke: vec!["old".into()],
        };
        assert_eq!(
            plan_sql(Mode::DEFAULT, Dialect::MySql, &u, &plan).unwrap(),
            ["RENAME USER 'report'@'10.0.%' TO 'reporting'@'10.0.%'", "ALTER USER 'reporting'@'10.0.%' ACCOUNT LOCK", "REVOKE 'old' FROM 'reporting'@'10.0.%'", "GRANT 'app_read'@'%' TO 'reporting'@'10.0.%'"]
        );
        assert_eq!(plan_sql(Mode::DEFAULT, Dialect::MariaDb, &Account::named("u", ""), &AccountPlan { roles_grant: vec!["r".into()], ..Default::default() }).unwrap(), ["GRANT 'r' TO 'u'@''"]);
        let role = Account { user: "u".into(), is_role: true, ..Default::default() };
        assert_eq!(plan_sql(Mode::DEFAULT, Dialect::MariaDb, &role, &AccountPlan { roles_grant: vec!["r".into()], ..Default::default() }).unwrap(), ["GRANT 'r' TO 'u'"]);
        assert!(AccountPlan::default().is_empty());
        assert!(plan_sql(Mode::DEFAULT, Dialect::MySql, &u, &AccountPlan { rename: Some(Account::named("", "%")), ..Default::default() }).is_err());
        assert!(role_sql(Mode::DEFAULT, Dialect::MySql, &u, "", false).is_err());
        let hostile = role_sql(Mode::DEFAULT, Dialect::MySql, &u, "r'; DROP USER x; --@%", false).unwrap();
        assert_eq!(hostile, "GRANT 'r''; DROP USER x; --'@'%' TO 'report'@'10.0.%'");
        assert_eq!(role_shown("reader@%"), "reader");
        assert_eq!(role_shown("w@h"), "w@h");
    }

    #[test]
    fn what_each_version_supports() {
        let c = Caps::of(Dialect::MySql, "8.4.3");
        assert!(c.alter_user && c.lock && c.lifetime && c.roles && c.dynamic && c.plugin_choice);
        let c = Caps::of(Dialect::MySql, "5.7.5");
        assert!(!c.alter_user && !c.lock && c.lifetime && !c.roles && !c.dynamic);
        let c = Caps::of(Dialect::MySql, "5.7.3");
        assert!(!c.lifetime);
        let c = Caps::of(Dialect::MySql, "5.7.8-log");
        assert!(c.alter_user && c.lock && c.lifetime && !c.roles);
        let c = Caps::of(Dialect::MariaDb, "11.4.5-MariaDB");
        assert!(c.alter_user && c.lock && c.lifetime && c.roles && !c.dynamic && !c.plugin_choice);
        let c = Caps::of(Dialect::MariaDb, "5.5.5-10.3.39-MariaDB");
        assert!(c.alter_user && !c.lock && !c.lifetime && c.roles);
        let c = Caps::of(Dialect::MariaDb, "10.4.2-MariaDB");
        assert!(c.lock && !c.lifetime);
    }

    #[test]
    fn password_lifetimes_read_in_both_servers_forms() {
        assert_eq!(lifetime_of(None), Expire::Default);
        assert_eq!(lifetime_of(Some("0")), Expire::Never);
        assert_eq!(lifetime_of(Some("90")), Expire::Days(90));
        assert_eq!(lifetime_of(Some("-1")), Expire::Default);
        assert_eq!(lifetime_of(Some("junk")), Expire::Default);
    }

    #[test]
    fn partial_revokes_are_read_and_a_global_revoke_over_them_is_refused() {
        let g = [parse_grant("GRANT SELECT, INSERT ON *.* TO `u`@`%`"), parse_grant("REVOKE SELECT ON `mysql`.* FROM `u`@`%`"), parse_grant("REVOKE INSERT, UPDATE ON `sys`.* FROM `u`@`%`")];
        assert_eq!(g[1].scope, Scope::Revoked("mysql".into()));
        assert_eq!(g[1].privs, ["SELECT"]);
        assert_eq!(held(&g, ""), ["SELECT", "INSERT", "-INSERT on sys", "-SELECT on mysql", "-UPDATE on sys"]);
        let ed = |p: &str| Edit { db: String::new(), grant: vec![], revoke: vec![p.into()] };
        let err = check_partial(&g, &[ed("SELECT")]).unwrap_err();
        assert!(err.contains("mysql") && err.contains("partial revokes"), "{err}");
        assert!(check_partial(&g, &[ed("INSERT")]).unwrap_err().contains("sys"));
        assert!(check_partial(&g, &[ed("DELETE")]).is_ok());
        // A database-level edit is not a global revoke.
        assert!(check_partial(&g, &[Edit { db: "mysql".into(), grant: vec![], revoke: vec!["SELECT".into()] }]).is_ok());
        assert!(preview_checked(Mode::DEFAULT, &Account::named("u", "%"), &[ed("SELECT")], &g).is_err());
        // Roles and unreadable lines are not partial revokes.
        assert_eq!(parse_grant("REVOKE SELECT ON `a`.`t` FROM `u`@`%`").scope, Scope::Other);
    }

    #[test]
    fn ansi_quoted_grant_lines_read_like_backtick_ones() {
        let g = parse_grant("GRANT SELECT, INSERT ON \"shop\".* TO \"report\"@\"10.0.%\"");
        assert_eq!(g.scope, Scope::Db("shop".into()));
        assert_eq!(g.privs, ["SELECT", "INSERT"]);
        let g = parse_grant("GRANT SELECT ON \"my\\_db\".* TO \"u\"@\"%\" WITH GRANT OPTION");
        assert_eq!(g.scope, Scope::Db("my\\_db".into()));
        assert_eq!(g.privs, ["SELECT", "GRANT OPTION"]);
        assert_eq!(parse_grant("GRANT SELECT ON \"a\"\"b\".* TO \"u\"@\"%\"").scope, Scope::Db("a\"b".into()));
        assert_eq!(parse_grant("GRANT USAGE ON *.* TO \"u\"@\"%\" IDENTIFIED BY PASSWORD '*AB'").scope, Scope::Global);
        assert_eq!(parse_grant("GRANT \"r\"@\"%\" TO \"u\"@\"%\"").privs, ["r@%"]);
    }

    #[test]
    fn a_server_that_folds_case_is_followed_or_no_undo_is_recorded() {
        let g = [parse_grant("GRANT SELECT ON `shop`.* TO `u`@`%`")];
        let mut eds = [Edit { db: "Shop".into(), grant: vec!["SELECT".into()], revoke: vec![] }];
        let snap = fold_edits(&g, &mut eds).unwrap();
        assert_eq!(eds[0].db, "shop");
        assert_eq!(snap, vec![("shop".to_string(), vec!["SELECT".to_string()])]);
        let mut nope = [Edit { db: "Other".into(), grant: vec!["SELECT".into()], revoke: vec![] }];
        assert!(fold_edits(&g, &mut nope).is_err());
        // A revoke of everything leaves no row, which is not a miss.
        let mut gone = [Edit { db: "Other".into(), grant: vec![], revoke: vec!["SELECT".into()] }];
        assert_eq!(fold_edits(&[], &mut gone).unwrap(), vec![("Other".to_string(), Vec::<String>::new())]);
        // Two stored names that differ only by case cannot be told apart.
        let two = [parse_grant("GRANT SELECT ON `ab`.* TO `u`@`%`"), parse_grant("GRANT SELECT ON `aB`.* TO `u`@`%`")];
        let mut amb = [Edit { db: "AB".into(), grant: vec!["INSERT".into()], revoke: vec![] }];
        assert!(fold_edits(&two, &mut amb).is_err());
    }

    #[test]
    fn grant_option_stays_with_its_line() {
        let st = parse_grant("GRANT SELECT ON *.* TO `u`@`%` WITH GRANT OPTION");
        assert_eq!(st.privs, ["SELECT", "GRANT OPTION"]);
        let dy = parse_grant("GRANT CONNECTION_ADMIN ON *.* TO `u`@`%` WITH GRANT OPTION");
        assert_eq!(dy.privs, ["CONNECTION_ADMIN"]);
        assert!(dy.with_grant);
        let only = parse_grant("GRANT USAGE ON *.* TO `u`@`%` WITH GRANT OPTION");
        assert_eq!(only.privs, ["GRANT OPTION"]);
        assert_eq!(dynamic_with_grant(&[st.clone(), dy]), set(&["CONNECTION_ADMIN"]));
        assert!(dynamic_with_grant(&[st]).is_empty());
    }

    #[test]
    fn role_grants_carry_the_admin_option() {
        let g = [parse_grant("GRANT `a`@`%` TO `u`@`%` WITH ADMIN OPTION"), parse_grant("GRANT `b`@`%` TO `u`@`%`")];
        assert!(g[0].with_admin && !g[1].with_admin);
        assert_eq!(roles_with_admin(&g), set(&["a@%"]));
        assert_eq!(roles_state(&g, &["b@%".to_string()]), ["a@% +admin", "b@% +default"]);
        let u = Account::named("u", "%");
        assert_eq!(role_grant_sql(Mode::DEFAULT, Dialect::MySql, &u, "a@%", false, true).unwrap(), "GRANT 'a'@'%' TO 'u'@'%' WITH ADMIN OPTION");
        assert_eq!(role_grant_sql(Mode::DEFAULT, Dialect::MariaDb, &u, "a", false, true).unwrap(), "GRANT 'a' TO 'u'@'%' WITH ADMIN OPTION");
        assert_eq!(default_roles_sql(Mode::DEFAULT, Dialect::MySql, &u, &["a@%".into(), "b@%".into()]).unwrap().unwrap(), "SET DEFAULT ROLE 'a'@'%', 'b'@'%' TO 'u'@'%'");
        assert_eq!(default_roles_sql(Mode::DEFAULT, Dialect::MariaDb, &u, &["a".into()]).unwrap().unwrap(), "SET DEFAULT ROLE 'a' FOR 'u'@'%'");
        assert_eq!(default_roles_sql(Mode::DEFAULT, Dialect::MySql, &u, &[]).unwrap(), None);
    }

    #[test]
    fn tls_requirements_are_written_back_exactly() {
        let u = Account::named("u", "%");
        let spec = TlsSpec { cipher: "AES256-SHA".into(), issuer: "/CN=ca'x".into(), subject: "/CN=me".into() };
        let held = Settings { tls: Tls::Specified, spec: spec.clone(), ..Default::default() };
        let none = Settings::default();
        assert_eq!(alter_sql(Mode::DEFAULT, &u, &held, &none).unwrap().unwrap(), "ALTER USER 'u'@'%' REQUIRE NONE");
        assert_eq!(alter_sql(Mode::DEFAULT, &u, &none, &held).unwrap().unwrap(), "ALTER USER 'u'@'%' REQUIRE SUBJECT '/CN=me' AND ISSUER '/CN=ca''x' AND CIPHER 'AES256-SHA'");
        // Only the cipher: the others are not required.
        let c = Settings { tls: Tls::Specified, spec: TlsSpec { cipher: "X".into(), ..Default::default() }, ..Default::default() };
        assert_eq!(alter_sql(Mode::DEFAULT, &u, &none, &c).unwrap().unwrap(), "ALTER USER 'u'@'%' REQUIRE CIPHER 'X'");
        // A changed subject under the same kind of requirement is a change.
        let d = Settings { spec: TlsSpec { subject: "/CN=you".into(), ..spec.clone() }, ..held.clone() };
        assert!(alter_sql(Mode::DEFAULT, &u, &held, &d).unwrap().unwrap().contains("SUBJECT '/CN=you'"));
        assert_eq!(alter_sql(Mode::DEFAULT, &u, &held, &held).unwrap(), None);
    }

    #[test]
    fn errors_lose_the_password_in_every_quoted_form() {
        for pw in ["o'k", "a\\b", "plain-secret", "x'y\\z"] {
            for echoed in [pw.to_string(), lit(pw, true), lit(pw, false), pw.replace('\'', "''"), pw.replace('\\', "\\\\")] {
                let e = scrub(format!("Access denied for {echoed} in a statement"), pw);
                assert!(!e.contains(pw) && !e.contains(&echoed), "{pw}: {e}");
            }
        }
        let e = scrub("You have an error in your SQL syntax; check the manual near ''secret'' at line 1".into(), "secret");
        assert!(!e.contains("secret") && e.contains("hidden"), "{e}");
    }

    #[test]
    fn a_user_without_a_host_is_not_a_role() {
        let u = Account::named("u", "");
        assert_eq!(u.literal(true), "'u'@''");
        assert_eq!(u.label(), "u@");
        let r = Account { user: "r".into(), is_role: true, ..Default::default() };
        assert_eq!((r.literal(true).as_str(), r.label().as_str()), ("'r'", "r"));
        assert_eq!(drop_user_sql(Mode::DEFAULT, &u, Dialect::MariaDb), "DROP USER 'u'@''");
        assert_eq!(drop_user_sql(Mode::DEFAULT, &r, Dialect::MariaDb), "DROP ROLE 'r'");
    }

    #[test]
    fn plugin_choices_follow_the_version() {
        let c = Caps::of(Dialect::MySql, "8.4.3");
        assert_eq!(c.plugins("caching_sha2_password"), ["caching_sha2_password", "sha256_password"]);
        assert_eq!(c.plugins("mysql_native_password"), ["caching_sha2_password", "mysql_native_password", "sha256_password"]);
        assert_eq!(Caps::of(Dialect::MySql, "8.0.36").plugins(""), ["caching_sha2_password", "mysql_native_password", "sha256_password"]);
        assert_eq!(Caps::of(Dialect::MySql, "5.7.44").plugins(""), ["mysql_native_password", "sha256_password"]);
        assert_eq!(Caps::of(Dialect::MySql, "9.1.0").plugins("caching_sha2_password"), ["caching_sha2_password"]);
    }

    // Live tests. Each makes a user of its own with a unique name and drops it at the end.

    fn unique(tag: &str) -> String {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        format!("qu_{tag}_{:x}", n & 0xffff_ffff_ffff)
    }

    fn done(l: &crate::db::sql::testing::Live) -> (Account, Res<String>, Option<UndoAction>) {
        match l.rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap() {
            Msg::SqlUserDone { account, res, undo, .. } => (account, res, undo.into_iter().next()),
            _ => panic!("expected SqlUserDone"),
        }
    }

    fn grants(l: &crate::db::sql::testing::Live, a: &Account) -> Vec<String> {
        l.w.rt.block_on(async {
            let sql = format!("SHOW GRANTS FOR {}", a.literal(true));
            sqlx::raw_sql(&sql).fetch_all(&l.pool).await.unwrap().iter().map(|r| super::super::worker::text_at(r, 0).unwrap_or_default()).collect()
        })
    }

    fn create(l: &crate::db::sql::testing::Live, a: &Account) -> Option<UndoAction> {
        l.w.sql_user_create(1, l.db.clone(), a.clone(), "pw-for-tests".into(), false);
        let (_, res, undo) = done(l);
        res.unwrap();
        undo
    }

    fn drop_it(l: &crate::db::sql::testing::Live, a: &Account) {
        l.w.rt.block_on(async { sqlx::raw_sql(&format!("DROP USER IF EXISTS {}", a.literal(true))).execute(&l.pool).await.unwrap() });
    }

    #[test]
    fn live_grant_then_revoke_leaves_show_grants_as_before() {
        every_server(|l, var| {
            let a = Account::named(&unique("rt"), "%");
            create(l, &a);
            let before = grants(l, &a);
            let ed = Edit { db: pattern_of("shop_x"), grant: vec!["SELECT".into(), "INSERT".into(), "EXECUTE".into(), "GRANT OPTION".into()], revoke: vec![] };
            l.w.sql_user_edit(1, l.db.clone(), a.clone(), vec![ed.clone()], false);
            let (_, res, undo) = done(l);
            res.unwrap_or_else(|x| panic!("{var}: {x}"));
            let mid = grants(l, &a);
            let parsed: Vec<Grant> = mid.iter().map(|g| parse_grant(g)).collect();
            let rows = db_grants(&parsed);
            assert_eq!(rows.len(), 1, "{var}: {mid:?}");
            assert_eq!(shown_pattern(&rows[0].0), "shop_x", "{var}: {mid:?}");
            assert_eq!(rows[0].1, set(&["SELECT", "INSERT", "EXECUTE", "GRANT OPTION"]), "{var}: {mid:?}");
            // Undo is the exact inverse and passes its check.
            let undo = undo.expect("an undo entry");
            l.w.sql_undo(l.db.clone(), undo, Vec::new(), false);
            let (_, res, _) = done(l);
            res.unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!(grants(l, &a), before, "{var}");
            // The inverse written by hand does the same.
            l.w.sql_user_edit(1, l.db.clone(), a.clone(), vec![ed.clone()], false);
            let (_, res, _) = done(l);
            res.unwrap();
            l.w.sql_user_edit(1, l.db.clone(), a.clone(), vec![ed.inverse()], false);
            let (_, res, _) = done(l);
            res.unwrap();
            assert_eq!(grants(l, &a), before, "{var}");
            drop_it(l, &a);
        });
    }

    #[test]
    fn live_undo_refuses_when_the_grants_changed_since() {
        every_server(|l, var| {
            let a = Account::named(&unique("ch"), "%");
            create(l, &a);
            let ed = Edit { db: pattern_of("shop_y"), grant: vec!["SELECT".into()], revoke: vec![] };
            l.w.sql_user_edit(1, l.db.clone(), a.clone(), vec![ed], false);
            let (_, res, undo) = done(l);
            res.unwrap();
            l.sql(&format!("GRANT DELETE ON `shop\\_y`.* TO {}", a.literal(true)));
            l.w.sql_undo(l.db.clone(), undo.unwrap(), Vec::new(), false);
            let (_, res, back) = done(l);
            let err = res.unwrap_err();
            assert!(err.starts_with("Undo refused"), "{var}: {err}");
            assert!(back.is_some(), "{var}: the entry stays");
            assert!(grants(l, &a).iter().any(|g| g.contains("DELETE")), "{var}");
            drop_it(l, &a);
        });
    }

    #[test]
    fn live_create_user_then_undo_drops_it_only_while_unused() {
        every_server(|l, var| {
            let a = Account::named(&unique("cr"), "%");
            let undo = create(l, &a).expect("undo");
            assert_eq!(l.one(&format!("SELECT COUNT(*) FROM mysql.user WHERE User = '{}'", a.user)), "1", "{var}");
            l.w.sql_undo(l.db.clone(), undo, Vec::new(), false);
            let (_, res, _) = done(l);
            res.unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!(l.one(&format!("SELECT COUNT(*) FROM mysql.user WHERE User = '{}'", a.user)), "0", "{var}");
            // With a grant given since, the undo leaves the user in place.
            let b = Account::named(&unique("cr2"), "%");
            let undo = create(l, &b).unwrap();
            l.sql(&format!("GRANT SELECT ON `shop\\_z`.* TO {}", b.literal(true)));
            l.w.sql_undo(l.db.clone(), undo, Vec::new(), false);
            let (_, res, back) = done(l);
            assert!(res.unwrap_err().starts_with("Not undone"), "{var}");
            assert!(back.is_some());
            assert_eq!(l.one(&format!("SELECT COUNT(*) FROM mysql.user WHERE User = '{}'", b.user)), "1", "{var}");
            drop_it(l, &b);
        });
    }

    #[test]
    fn live_every_write_refuses_on_a_read_only_connection() {
        every_server(|l, var| {
            let a = Account::named(&unique("ro"), "%");
            l.w.sql_user_create(1, l.db.clone(), a.clone(), "pw".into(), true);
            assert!(done(l).1.unwrap_err().contains("locked"), "{var}");
            l.w.sql_user_edit(1, l.db.clone(), a.clone(), vec![Edit { db: "d".into(), grant: vec!["SELECT".into()], revoke: vec![] }], true);
            assert!(done(l).1.unwrap_err().contains("locked"), "{var}");
            l.w.sql_user_drop(1, l.db.clone(), a.clone(), true);
            assert!(done(l).1.unwrap_err().contains("locked"), "{var}");
            assert_eq!(l.one(&format!("SELECT COUNT(*) FROM mysql.user WHERE User = '{}'", a.user)), "0", "{var}");
        });
    }

    #[test]
    fn live_an_apostrophe_in_the_name_works() {
        every_server(|l, var| {
            let a = Account::named(&format!("{}'b;--", unique("ap")), "h'ost");
            create(l, &a);
            let ed = Edit { db: "we`ird_db".into(), grant: vec!["SELECT".into()], revoke: vec![] };
            l.w.sql_user_edit(1, l.db.clone(), a.clone(), vec![Edit { db: pattern_of("we`ird_db"), ..ed }], false);
            done(l).1.unwrap_or_else(|x| panic!("{var}: {x}"));
            l.w.sql_user_grants(1, 9, l.db.clone(), a.clone());
            let Msg::SqlUserGrants { res, .. } = l.rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap() else { panic!() };
            let rows = db_grants(&res.unwrap().grants);
            assert_eq!(rows.len(), 1, "{var}");
            assert_eq!(shown_pattern(&rows[0].0), "we`ird_db", "{var}");
            l.w.sql_users(1, 1, l.db.clone());
            let Msg::SqlUsers { res, .. } = l.rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap() else { panic!() };
            let list = res.unwrap();
            assert!(list.whole, "{var}: {:?}", list.why);
            assert!(list.accounts.iter().any(|x| x.user == a.user && x.host == a.host), "{var}");
            l.w.sql_user_drop(1, l.db.clone(), a.clone(), false);
            done(l).1.unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!(l.one(&format!("SELECT COUNT(*) FROM mysql.user WHERE User = {}", lit(&a.user, true))), "0", "{var}");
        });
    }

    #[test]
    fn live_a_weak_user_sees_only_itself() {
        every_server(|l, var| {
            let a = Account::named(&unique("me"), "%");
            create(l, &a);
            l.sql(&format!("GRANT SELECT ON `shop\\_w`.* TO {}", a.literal(true)));
            let uri = std::env::var(crate::db::sql::testing::ENVS[if var.ends_with("MARIADB") { 1 } else { 0 }]).unwrap();
            let host = uri.split('@').nth(1).unwrap().to_string();
            let weak = format!("mysql://{}:pw-for-tests@{host}", a.user);
            let (tx, rx) = std::sync::mpsc::channel();
            let w = Worker::new(tx, eframe::egui::Context::default());
            w.sql_connect(2, weak, std::time::Duration::from_secs(30), crate::db::Net::default());
            let Msg::SqlConnected { res: Ok((db, _, _)), .. } = rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap() else { panic!("connect") };
            w.sql_users(2, 1, db);
            let Msg::SqlUsers { res, .. } = rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap() else { panic!() };
            let list = res.unwrap();
            assert!(!list.whole, "{var}");
            assert!(list.why.is_some());
            assert_eq!(list.accounts.len(), 1, "{var}");
            assert_eq!(list.accounts[0].user, a.user, "{var}");
            drop_it(l, &a);
        });
    }

    fn info(l: &crate::db::sql::testing::Live, a: &Account) -> AccountInfo {
        l.w.sql_user_grants(1, 5, l.db.clone(), a.clone());
        let Msg::SqlUserGrants { res, .. } = l.rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap() else { panic!() };
        res.unwrap().info.expect("account info")
    }

    fn plan_run(l: &crate::db::sql::testing::Live, a: &Account, plan: AccountPlan) -> (Account, Res<String>, Vec<UndoAction>) {
        l.w.sql_user_account(1, l.db.clone(), a.clone(), plan, false);
        match l.rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap() {
            Msg::SqlUserDone { account, res, undo, .. } => (account, res, undo),
            _ => panic!("expected SqlUserDone"),
        }
    }

    #[test]
    fn live_partial_revokes_are_never_widened() {
        // Needs a MySQL 8 server started with --partial-revokes=ON.
        let Some(l) = crate::db::sql::testing::live("QUARRY_TEST_MYSQL_PR") else { return };
        let a = Account::named(&unique("pr"), "%");
        create(&l, &a);
        l.sql(&format!("GRANT SELECT, INSERT ON *.* TO {}", a.literal(true)));
        l.sql(&format!("REVOKE SELECT ON `mysql`.* FROM {}", a.literal(true)));
        let before = grants(&l, &a);
        assert!(before.iter().any(|g| g.starts_with("REVOKE SELECT ON `mysql`")), "{before:?}");
        // Revoking SELECT everywhere would drop the partial revoke: refused, and nothing changes.
        l.w.sql_user_edit(1, l.db.clone(), a.clone(), vec![Edit { db: String::new(), grant: vec![], revoke: vec!["SELECT".into()] }], false);
        let (_, res, undo) = done(&l);
        let err = res.unwrap_err();
        assert!(err.contains("partial revokes") && undo.is_none(), "{err}");
        assert_eq!(grants(&l, &a), before);
        // INSERT has none: it goes and comes back, with the partial revoke intact.
        l.w.sql_user_edit(1, l.db.clone(), a.clone(), vec![Edit { db: String::new(), grant: vec![], revoke: vec!["INSERT".into()] }], false);
        let (_, res, undo) = done(&l);
        res.unwrap();
        l.w.sql_undo(l.db.clone(), undo.unwrap(), Vec::new(), false);
        let (_, res, _) = done(&l);
        res.unwrap();
        assert_eq!(grants(&l, &a), before);
        // An escaped underscore still names the database my_db under partial revokes.
        let ed = Edit { db: pattern_of("my_db"), grant: vec!["SELECT".into()], revoke: vec![] };
        l.w.sql_user_edit(1, l.db.clone(), a.clone(), vec![ed], false);
        let (_, res, undo) = done(&l);
        res.unwrap_or_else(|x| panic!("{x}"));
        let rows = db_grants(&grants(&l, &a).iter().map(|g| parse_grant(g)).collect::<Vec<_>>());
        assert_eq!(rows.len(), 1);
        assert_eq!(shown_pattern(&rows[0].0), "my_db");
        l.w.sql_undo(l.db.clone(), undo.unwrap(), Vec::new(), false);
        done(&l).1.unwrap();
        assert_eq!(grants(&l, &a), before);
        drop_it(&l, &a);
    }

    #[test]
    fn live_tls_requirements_come_back_with_undo() {
        every_server(|l, var| {
            let a = Account::named(&unique("tl"), "%");
            create(l, &a);
            l.sql(&format!("ALTER USER {} REQUIRE SUBJECT '/CN=me' AND ISSUER '/CN=ca' AND CIPHER 'AES256-SHA'", a.literal(true)));
            let before = info(l, &a).settings;
            assert_eq!(before.tls, Tls::Specified, "{var}");
            assert_eq!((before.spec.subject.as_str(), before.spec.issuer.as_str(), before.spec.cipher.as_str()), ("/CN=me", "/CN=ca", "AES256-SHA"), "{var}");
            // Replace the requirement and lock the account in one change.
            let caps = Caps::of(l.db.dialect, &l.db.version);
            let to = Settings { locked: caps.lock, tls: Tls::Ssl, spec: TlsSpec::default(), limits: [3, 0, 0, 0], ..before.clone() };
            let (_, res, undo) = plan_run(l, &a, AccountPlan { settings: Some((before.clone(), to.clone())), ..Default::default() });
            res.unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!(info(l, &a).settings.tls, Tls::Ssl, "{var}");
            l.w.sql_undo(l.db.clone(), undo.into_iter().next().unwrap(), Vec::new(), false);
            let (_, res, back) = done(l);
            res.unwrap_or_else(|x| panic!("{var}: {x}"));
            assert!(back.is_none(), "{var}");
            assert_eq!(info(l, &a).settings, before, "{var}: lock, limits and the requirement all come back");
            drop_it(l, &a);
        });
    }

    #[test]
    fn live_a_failed_undo_keeps_its_entry() {
        every_server(|l, var| {
            let a = Account::named(&unique("ke"), "%");
            create(l, &a);
            let s0 = Settings::default();
            let s1 = Settings { limits: [4, 0, 0, 0], ..s0.clone() };
            let (_, res, undo) = plan_run(l, &a, AccountPlan { settings: Some((s0, s1)), ..Default::default() });
            res.unwrap();
            // The account is gone, so the undo fails before it runs anything and stays in the list.
            drop_it(l, &a);
            l.w.sql_undo(l.db.clone(), undo.into_iter().next().unwrap(), Vec::new(), false);
            let (_, res, back) = done(l);
            assert!(res.is_err(), "{var}");
            assert!(back.is_some(), "{var}");
        });
    }

    #[test]
    fn live_database_grants_are_read_and_checked_under_ansi_quotes() {
        every_server(|l, var| {
            let a = Account::named(&unique("an"), "%");
            create(l, &a);
            let ansi = l.with_session("SET SESSION sql_mode = CONCAT(@@sql_mode, ',ANSI_QUOTES')");
            let ed = Edit { db: pattern_of("ansi_db"), grant: vec!["SELECT".into()], revoke: vec![] };
            l.w.sql_user_edit(1, ansi.clone(), a.clone(), vec![ed], false);
            let (_, res, undo) = done(l);
            res.unwrap_or_else(|x| panic!("{var}: {x}"));
            l.w.sql_user_grants(1, 7, ansi.clone(), a.clone());
            let Msg::SqlUserGrants { res, .. } = l.rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap() else { panic!() };
            let rows = db_grants(&res.unwrap().grants);
            assert_eq!(rows.len(), 1, "{var}: the database row is found");
            assert_eq!(shown_pattern(&rows[0].0), "ansi_db", "{var}");
            // The undo check is real: a change made behind its back refuses it.
            l.sql(&format!("GRANT INSERT ON `ansi\\_db`.* TO {}", a.literal(true)));
            l.w.sql_undo(ansi.clone(), undo.unwrap(), Vec::new(), false);
            let (_, res, back) = done(l);
            assert!(res.unwrap_err().starts_with("Undo refused"), "{var}");
            assert!(back.is_some(), "{var}");
            drop_it(l, &a);
        });
    }

    #[test]
    fn live_undo_refuses_a_namesake_made_after_a_drop() {
        every_server(|l, var| {
            let a = Account::named(&unique("ns"), "%");
            create(l, &a);
            let s0 = Settings::default();
            let s1 = Settings { limits: [9, 0, 0, 0], ..s0.clone() };
            let (_, res, undo) = plan_run(l, &a, AccountPlan { settings: Some((s0, s1)), ..Default::default() });
            res.unwrap();
            drop_it(l, &a);
            l.sql(&format!("CREATE USER {} IDENTIFIED BY 'a-different-password'", a.literal(true)));
            l.sql(&format!("ALTER USER {} WITH MAX_QUERIES_PER_HOUR 9", a.literal(true)));
            l.w.sql_undo(l.db.clone(), undo.into_iter().next().unwrap(), Vec::new(), false);
            let (_, res, back) = done(l);
            let err = res.unwrap_err();
            assert!(err.starts_with("Undo refused") && err.contains("not the account"), "{var}: {err}");
            assert!(back.is_some(), "{var}");
            assert_eq!(info(l, &a).settings.limits[0], 9, "{var}: the namesake was left alone");
            drop_it(l, &a);
        });
    }

    #[test]
    fn live_roles_give_back_their_admin_option_and_default_status() {
        every_server(|l, var| {
            let caps = Caps::of(l.db.dialect, &l.db.version);
            if !caps.roles {
                return;
            }
            let a = Account::named(&unique("rd"), "%");
            let role = unique("rr");
            create(l, &a);
            l.sql(&format!("CREATE ROLE '{role}'"));
            l.sql(&format!("GRANT '{role}' TO {} WITH ADMIN OPTION", a.literal(true)));
            let maria = l.db.dialect == Dialect::MariaDb;
            l.sql(&if maria { format!("SET DEFAULT ROLE '{role}' FOR {}", a.literal(true)) } else { format!("SET DEFAULT ROLE '{role}' TO {}", a.literal(true)) });
            let given = if maria { role.clone() } else { format!("{role}@%") };
            let (_, res, undo) = plan_run(l, &a, AccountPlan { roles_revoke: vec![given.clone()], ..Default::default() });
            res.unwrap_or_else(|x| panic!("{var}: {x}"));
            // MariaDB keeps the SET DEFAULT ROLE line after a revoke; only the role grant must be gone.
            assert!(!grants(l, &a).iter().any(|g| g.starts_with("GRANT") && g.contains(&role)), "{var}");
            l.w.sql_undo(l.db.clone(), undo.into_iter().next().unwrap(), Vec::new(), false);
            let (_, res, _) = done(l);
            res.unwrap_or_else(|x| panic!("{var}: {x}"));
            let g = grants(l, &a);
            assert!(g.iter().any(|x| x.contains(&role) && x.contains("WITH ADMIN OPTION")), "{var}: {g:?}");
            let defaults = if maria { l.one(&format!("SELECT default_role FROM mysql.user WHERE User = {}", lit(&a.user, true))) } else { l.one(&format!("SELECT DEFAULT_ROLE_USER FROM mysql.default_roles WHERE USER = {}", lit(&a.user, true))) };
            assert_eq!(defaults, role, "{var}");
            drop_it(l, &a);
            l.sql(&format!("DROP ROLE '{role}'"));
        });
    }

    #[test]
    fn live_a_grant_option_on_a_dynamic_privilege_is_not_everyones() {
        every_server(|l, var| {
            if !Caps::of(l.db.dialect, &l.db.version).dynamic {
                return;
            }
            let a = Account::named(&unique("dg"), "%");
            create(l, &a);
            l.sql(&format!("GRANT CONNECTION_ADMIN ON *.* TO {} WITH GRANT OPTION", a.literal(true)));
            l.sql(&format!("GRANT SELECT ON *.* TO {}", a.literal(true)));
            let g: Vec<Grant> = grants(l, &a).iter().map(|g| parse_grant(g)).collect();
            assert_eq!(global_privs(&g), set(&["SELECT", "CONNECTION_ADMIN"]), "{var}: {:?}", grants(l, &a));
            assert_eq!(dynamic_with_grant(&g), set(&["CONNECTION_ADMIN"]), "{var}");
            drop_it(l, &a);
        });
    }

    #[test]
    fn live_mariadb_set_password_keeps_the_login_methods_and_refuses_a_chain() {
        every_server(|l, var| {
            if l.db.dialect != Dialect::MariaDb {
                return;
            }
            // A chain needs a second plugin; use unix_socket when the server has it.
            let a = Account::named(&unique("ch"), "localhost");
            let made = l.w.rt.block_on(async { sqlx::raw_sql(&format!("CREATE USER {} IDENTIFIED VIA unix_socket OR mysql_native_password USING PASSWORD('x1')", a.literal(true))).execute(&l.pool).await.is_ok() });
            if made {
                l.w.sql_user_password(1, l.db.clone(), a.clone(), None, "new-secret".into(), false);
                let err = done(l).1.unwrap_err();
                assert!(err.contains("more than one method") && !err.contains("new-secret"), "{var}: {err}");
                // The chain is untouched.
                assert_eq!(l.one(&format!("SELECT COUNT(*) FROM mysql.global_priv WHERE User = {} AND JSON_QUERY(Priv, '$.auth_or') IS NOT NULL", lit(&a.user, true))), "1", "{var}");
                drop_it(l, &a);
            }
            // A plain account takes the new password and no other plugin appears.
            let b = Account::named(&unique("pl"), "%");
            create(l, &b);
            l.w.sql_user_password(1, l.db.clone(), b.clone(), None, "second-secret".into(), false);
            done(l).1.unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!(info(l, &b).plugin, "mysql_native_password", "{var}");
            drop_it(l, &b);
        });
    }

    #[test]
    fn live_a_mariadb_role_is_dropped_as_a_role() {
        every_server(|l, var| {
            if l.db.dialect != Dialect::MariaDb {
                return;
            }
            let role = unique("dr");
            l.sql(&format!("CREATE ROLE '{role}'"));
            let acct = Account { user: role.clone(), host: String::new(), is_role: true, ..Default::default() };
            l.w.sql_user_drop(1, l.db.clone(), acct, false);
            done(l).1.unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!(l.one(&format!("SELECT COUNT(*) FROM mysql.user WHERE User = '{role}'")), "0", "{var}");
        });
    }

    #[test]
    fn live_settings_read_change_and_undo_exactly() {
        every_server(|l, var| {
            let a = Account::named(&unique("st"), "%");
            create(l, &a);
            let caps = Caps::of(l.db.dialect, &l.db.version);
            let before = info(l, &a);
            assert_eq!(before.settings, Settings::default(), "{var}");
            let to = Settings { locked: caps.lock, expire: if caps.lifetime { Expire::Days(90) } else { Expire::Default }, tls: Tls::Ssl, limits: [5000, 7, 9, 3], ..Default::default() };
            let (_, res, undo) = plan_run(l, &a, AccountPlan { settings: Some((before.settings.clone(), to.clone())), ..Default::default() });
            res.unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!(info(l, &a).settings, to, "{var}");
            assert_eq!(undo.len(), 1, "{var}");
            // Never and the default are different things on both servers.
            let never = Settings { expire: Expire::Never, ..to.clone() };
            if caps.lifetime {
                let (_, res, undo2) = plan_run(l, &a, AccountPlan { settings: Some((to.clone(), never.clone())), ..Default::default() });
                res.unwrap_or_else(|x| panic!("{var}: {x}"));
                assert_eq!(info(l, &a).settings, never, "{var}");
                l.w.sql_undo(l.db.clone(), undo2.into_iter().next().unwrap(), Vec::new(), false);
                let (_, res, _) = done(l);
                res.unwrap_or_else(|x| panic!("{var}: {x}"));
                assert_eq!(info(l, &a).settings, to, "{var}");
            }
            l.w.sql_undo(l.db.clone(), undo.into_iter().next().unwrap(), Vec::new(), false);
            let (_, res, _) = done(l);
            res.unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!(info(l, &a).settings, before.settings, "{var}");
            drop_it(l, &a);
        });
    }

    #[test]
    fn live_a_settings_undo_refuses_when_the_field_changed_since() {
        every_server(|l, var| {
            let a = Account::named(&unique("sr"), "%");
            create(l, &a);
            let s0 = Settings::default();
            let s1 = Settings { limits: [10, 0, 0, 0], ..s0.clone() };
            let (_, res, undo) = plan_run(l, &a, AccountPlan { settings: Some((s0, s1)), ..Default::default() });
            res.unwrap();
            l.sql(&format!("ALTER USER {} WITH MAX_QUERIES_PER_HOUR 11", a.literal(true)));
            l.w.sql_undo(l.db.clone(), undo.into_iter().next().unwrap(), Vec::new(), false);
            let (_, res, back) = done(l);
            assert!(res.unwrap_err().starts_with("Undo refused"), "{var}");
            assert!(back.is_some());
            assert_eq!(info(l, &a).settings.limits[0], 11, "{var}");
            drop_it(l, &a);
        });
    }

    #[test]
    fn live_rename_and_undo() {
        every_server(|l, var| {
            let a = Account::named(&unique("rn"), "%");
            let b = Account::named(&format!("{}'x", a.user), "h.example");
            create(l, &a);
            l.sql(&format!("GRANT SELECT ON `shop\\_rn`.* TO {}", a.literal(true)));
            let (cur, res, undo) = plan_run(l, &a, AccountPlan { rename: Some(b.clone()), ..Default::default() });
            res.unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!((cur.user.as_str(), cur.host.as_str()), (b.user.as_str(), b.host.as_str()), "{var}");
            assert!(grants(l, &b).iter().any(|g| g.contains("SELECT")), "{var}: the grants follow the name");
            l.w.sql_undo(l.db.clone(), undo.into_iter().next().unwrap(), Vec::new(), false);
            let Msg::SqlUserDone { account, res, op, .. } = l.rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap() else { panic!() };
            res.unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!(op, Op::UndoneRename, "{var}");
            assert_eq!(account.user, a.user, "{var}");
            assert!(grants(l, &a).iter().any(|g| g.contains("SELECT")), "{var}");
            drop_it(l, &a);
        });
    }

    #[test]
    fn live_roles_are_given_and_taken_back_with_undo() {
        every_server(|l, var| {
            let caps = Caps::of(l.db.dialect, &l.db.version);
            if !caps.roles {
                return;
            }
            let a = Account::named(&unique("ro"), "%");
            let role = unique("rl");
            l.sql(&format!("CREATE ROLE '{role}'"));
            create(l, &a);
            let given = if l.db.dialect == Dialect::MySql { format!("{role}@%") } else { role.clone() };
            let (_, res, undo) = plan_run(l, &a, AccountPlan { roles_grant: vec![given.clone()], ..Default::default() });
            res.unwrap_or_else(|x| panic!("{var}: {x}"));
            l.w.sql_user_grants(1, 6, l.db.clone(), a.clone());
            let Msg::SqlUserGrants { res, .. } = l.rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap() else { panic!() };
            assert_eq!(roles_of(&res.unwrap().grants), vec![given.clone()], "{var}");
            l.w.sql_undo(l.db.clone(), undo.into_iter().next().unwrap(), Vec::new(), false);
            let (_, res, _) = done(l);
            res.unwrap_or_else(|x| panic!("{var}: {x}"));
            assert!(!grants(l, &a).iter().any(|g| g.contains(&role)), "{var}");
            drop_it(l, &a);
            l.sql(&format!("DROP ROLE '{role}'"));
        });
    }

    #[test]
    fn live_global_privileges_round_trip_and_dynamic_ones_on_mysql() {
        every_server(|l, var| {
            let a = Account::named(&unique("gl"), "%");
            create(l, &a);
            let before = grants(l, &a);
            let mut grant = vec!["PROCESS".to_string(), "SHOW VIEW".to_string(), "REPLICATION CLIENT".to_string()];
            if Caps::of(l.db.dialect, &l.db.version).dynamic {
                grant.push("CONNECTION_ADMIN".into());
            }
            let ed = Edit { db: String::new(), grant, revoke: vec![] };
            l.w.sql_user_edit(1, l.db.clone(), a.clone(), vec![ed.clone()], false);
            let (_, res, undo) = done(l);
            res.unwrap_or_else(|x| panic!("{var}: {x}"));
            let parsed: Vec<Grant> = grants(l, &a).iter().map(|g| parse_grant(g)).collect();
            assert_eq!(global_privs(&parsed), ed.grant.iter().cloned().collect::<BTreeSet<_>>(), "{var}");
            l.w.sql_undo(l.db.clone(), undo.unwrap(), Vec::new(), false);
            let (_, res, _) = done(l);
            res.unwrap_or_else(|x| panic!("{var}: {x}"));
            assert_eq!(grants(l, &a), before, "{var}");
            drop_it(l, &a);
        });
    }

    #[test]
    fn live_a_changed_password_logs_in_and_leaves_no_trace() {
        every_server(|l, var| {
            let a = Account::named(&unique("pw"), "%");
            create(l, &a);
            let secret = "n3w'pa\\ss-for-tests";
            l.w.sql_user_password(1, l.db.clone(), a.clone(), None, secret.into(), false);
            let (_, res, undo) = done(l);
            let msg = res.unwrap_or_else(|x| panic!("{var}: {x}"));
            assert!(!msg.contains(secret) && undo.is_none(), "{var}");
            // The new password logs in; the old one does not.
            let uri = std::env::var(crate::db::sql::testing::ENVS[if var.ends_with("MARIADB") { 1 } else { 0 }]).unwrap();
            let host = uri.split('@').nth(1).unwrap().to_string();
            let try_login = |pw: &str| {
                let url = format!("mysql://{}:{}@{host}", a.user, pw.replace('\\', "%5C").replace('\'', "%27"));
                l.w.rt.block_on(async { sqlx::MySqlPool::connect(&url).await.is_ok() })
            };
            assert!(try_login(secret), "{var}");
            assert!(!try_login("pw-for-tests"), "{var}");
            // A refused change reports no password either.
            l.w.sql_user_password(1, l.db.clone(), a.clone(), Some("no_such_plugin_x".into()), secret.into(), false);
            let (_, res, _) = done(l);
            if l.db.dialect == Dialect::MySql {
                assert!(!res.unwrap_err().contains(secret), "{var}");
            }
            drop_it(l, &a);
        });
    }

    #[test]
    fn the_log_never_holds_the_password_of_create_or_change() {
        // Quotes and backslashes in the password end a literal early under one reading or the other; none may leak.
        let secrets = ["Zq9Marker", "Zq9'Marker", "Zq9\\'Marker", "Zq9\\Marker\\", "Zq9\"Marker`x", "Zq9''Marker\\\\"];
        let a = Account::named("log_pw_user", "%");
        let mut n = 0u64;
        for bs in [true, false] {
            let m = Mode { bs, ansi: false };
            for d in [Dialect::MySql, Dialect::MariaDb] {
                for secret in secrets {
                    let mut stmts = vec![create_user_sql(m, &a, secret, false)];
                    for plugin in [None, Some("caching_sha2_password")] {
                        if let Ok(sql) = password_sql(m, d, &a, plugin, secret, false) {
                            stmts.push(sql);
                        }
                    }
                    for sql in stmts {
                        n += 1;
                        let conn = 991_000 + n;
                        crate::db::log_sql(conn, "", &sql, 1, None);
                        // And once more as a failure whose message quotes the statement from where it broke.
                        crate::db::log_sql(conn, "", &sql, 1, Some(&format!("You have an error in your SQL syntax; check the manual for the right syntax to use near '{}' at line 1", &sql[sql.find("BY ").or_else(|| sql.find("PASSWORD(")).unwrap_or(0)..])));
                        let log = crate::db::LOG.lock().unwrap();
                        for l in log.iter().filter(|l| l.conn == conn) {
                            let all = format!("{} {}", l.cmd, l.err.clone().unwrap_or_default());
                            assert!(!all.contains("Marker") && !all.contains("Zq9"), "bs={bs} {d:?} {secret:?}: {all}");
                            assert!(l.cmd.contains("'***'"), "{}", l.cmd);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn live_users_writes_reach_the_query_log_with_passwords_redacted() {
        every_server(|l, var| {
            let a = Account::named(&unique("lg"), "%");
            let secret = "Zq9'Marker\\x";
            l.w.sql_user_create(1, l.db.clone(), a.clone(), secret.into(), false);
            done(l).1.unwrap_or_else(|x| panic!("{var}: {x}"));
            let ed = Edit { db: pattern_of("shop_lg"), grant: vec!["SELECT".into()], revoke: vec![] };
            l.w.sql_user_edit(1, l.db.clone(), a.clone(), vec![ed], false);
            let (_, res, undo) = done(l);
            res.unwrap_or_else(|x| panic!("{var}: {x}"));
            l.w.sql_undo(l.db.clone(), undo.expect("undo"), Vec::new(), false);
            done(l).1.unwrap_or_else(|x| panic!("{var}: {x}"));
            l.w.sql_user_password(1, l.db.clone(), a.clone(), None, secret.into(), false);
            done(l).1.unwrap_or_else(|x| panic!("{var}: {x}"));
            l.w.sql_user_drop(1, l.db.clone(), a.clone(), false);
            done(l).1.unwrap_or_else(|x| panic!("{var}: {x}"));
            let log = crate::db::LOG.lock().unwrap();
            let mine: Vec<&str> = log.iter().filter(|e| e.conn == l.db.conn && e.cmd.contains(&a.user)).map(|e| e.cmd.as_str()).collect();
            let has = |w: &str| mine.iter().any(|c| c.starts_with(w));
            assert!(has("CREATE USER") && has("GRANT SELECT") && has("REVOKE SELECT") && has("DROP USER"), "{var}: {mine:?}");
            assert!(mine.iter().any(|c| c.contains("PASSWORD(") || c.starts_with("ALTER USER") || c.starts_with("SET PASSWORD")), "{var}: {mine:?}");
            assert!(mine.iter().all(|c| !c.contains("Marker") && !c.contains("Zq9")), "{var}: {mine:?}");
        });
    }
}
