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

fn account_query(d: Dialect, joined: bool) -> Res<&'static str> {
    Ok(match (d, joined) {
        (Dialect::Postgres, _) => return Err(super::pg::NOT_YET.into()),
        (Dialect::MySql, _) => "SELECT User AS u, Host AS h, account_locked AS locked, password_expired AS expired, plugin, 'N' AS is_role FROM mysql.user ORDER BY User, Host",
        // The lock lives only in the JSON of global_priv, which a plain mysql.user view does not show.
        (Dialect::MariaDb, true) => "SELECT u.User AS u, u.Host AS h, IFNULL(JSON_VALUE(g.Priv, '$.account_locked'), 'false') AS locked, u.password_expired AS expired, u.plugin AS plugin, u.is_role AS is_role FROM mysql.user u LEFT JOIN mysql.global_priv g ON g.User = u.User AND g.Host = u.Host ORDER BY u.User, u.Host",
        (Dialect::MariaDb, false) => "SELECT User AS u, Host AS h, 'false' AS locked, password_expired AS expired, plugin, is_role FROM mysql.user ORDER BY User, Host",
    })
}

async fn read_accounts(c: &mut sqlx::MySqlConnection, d: Dialect) -> Res<Vec<Account>> {
    let mut last = None;
    for joined in [true, false] {
        match sqlx::query(account_query(d, joined)?).persistent(false).fetch_all(&mut *c).await {
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
    db.mysql_only()?;
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
    if let Some(Tok::Word(w)) = t.get(i) && ["TABLE", "FUNCTION", "PROCEDURE"].iter().any(|k| w.eq_ignore_ascii_case(k)) {
        routine = !w.eq_ignore_ascii_case("TABLE");
        i += 1;
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
        (Dialect::Postgres, Some(_)) => return Err(super::pg::NOT_YET.into()),
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

fn info_query(d: Dialect, joined: bool) -> Res<&'static str> {
    Ok(match (d, joined) {
        (Dialect::Postgres, _) => return Err(super::pg::NOT_YET.into()),
        // Numbers are cast to text: a prepared statement sends them as binary, which `text` would misread.
        (Dialect::MySql, _) => "SELECT plugin, account_locked AS locked, CAST(password_lifetime AS CHAR) AS lifetime, ssl_type, CONVERT(ssl_cipher USING utf8mb4) AS cipher, CONVERT(x509_issuer USING utf8mb4) AS issuer, CONVERT(x509_subject USING utf8mb4) AS subject, 'N' AS chain, CONCAT_WS('|', password_last_changed, plugin, LEFT(SHA2(authentication_string, 256), 16)) AS ident, CAST(max_questions AS CHAR) AS q, CAST(max_updates AS CHAR) AS u, CAST(max_connections AS CHAR) AS c, CAST(max_user_connections AS CHAR) AS uc FROM mysql.user WHERE User = ? AND Host = ?",
        (Dialect::MariaDb, true) => "SELECT u.plugin AS plugin, IFNULL(JSON_VALUE(g.Priv, '$.account_locked'), 'false') AS locked, JSON_VALUE(g.Priv, '$.password_lifetime') AS lifetime, u.ssl_type AS ssl_type, CONVERT(u.ssl_cipher USING utf8mb4) AS cipher, CONVERT(u.x509_issuer USING utf8mb4) AS issuer, CONVERT(u.x509_subject USING utf8mb4) AS subject, IF(JSON_QUERY(g.Priv, '$.auth_or') IS NULL, 'N', 'Y') AS chain, CONCAT_WS('|', u.plugin, LEFT(SHA2(u.authentication_string, 256), 16)) AS ident, CAST(u.max_questions AS CHAR) AS q, CAST(u.max_updates AS CHAR) AS u, CAST(u.max_connections AS CHAR) AS c, CAST(u.max_user_connections AS CHAR) AS uc FROM mysql.user u LEFT JOIN mysql.global_priv g ON g.User = u.User AND g.Host = u.Host WHERE u.User = ? AND u.Host = ?",
        (Dialect::MariaDb, false) => "SELECT plugin, 'false' AS locked, NULL AS lifetime, ssl_type, CONVERT(ssl_cipher USING utf8mb4) AS cipher, CONVERT(x509_issuer USING utf8mb4) AS issuer, CONVERT(x509_subject USING utf8mb4) AS subject, 'N' AS chain, CONCAT_WS('|', plugin, LEFT(SHA2(authentication_string, 256), 16)) AS ident, CAST(max_questions AS CHAR) AS q, CAST(max_updates AS CHAR) AS u, CAST(max_connections AS CHAR) AS c, CAST(max_user_connections AS CHAR) AS uc FROM mysql.user WHERE User = ? AND Host = ?",
    })
}

async fn read_info(c: &mut sqlx::MySqlConnection, d: Dialect, acct: &Account) -> Res<AccountInfo> {
    let mut last = String::new();
    for joined in [true, false] {
        match sqlx::query(info_query(d, joined)?).bind(&acct.user).bind(&acct.host).persistent(false).fetch_optional(&mut *c).await {
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
    UndoAction { agent: None, at: Instant::now(), conn, db: acct.label(), coll: String::new(), kind: UndoKind::Sql { database: String::new(), cols: Vec::new(), undo: SqlUndo::Account(Box::new(undo)), ids: Default::default() } }
}

// Running.

async fn grants_of(c: &mut sqlx::MySqlConnection, m: Mode, acct: &Account) -> Res<Vec<Grant>> {
    let sql = format!("SHOW GRANTS FOR {}", acct.literal(m.bs));
    let rows = match sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).persistent(false).fetch_all(&mut *c).await {
        Ok(r) => r,
        // Not every server prepares SHOW GRANTS; the text holds only the quoted account.
        Err(x) if e(&x).contains("1295") || e(&x).contains("prepared statement protocol") => c.fetch_all(sqlx::AssertSqlSafe(sql.as_str())).await.map_err(se)?,
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
            let low = ed.db.to_lowercase();
            let alike: Vec<&String> = rows.iter().map(|(k, _)| k).filter(|k| k.to_lowercase() == low).collect();
            match alike.as_slice() {
                [one] => ed.db = (*one).clone(),
                _ => return Err(format!("{} is not among the account's grants after the change", shown_pattern(&ed.db))),
            }
        }
        out.push((ed.db.clone(), held(g, &ed.db)));
    }
    Ok(out)
}

/// Cuts each edit to what it changes against `g`, the grants the account holds now: a privilege already held leaves
/// the grant and one not held leaves the revoke. The undo recorded afterwards is the plain inverse, so without this
/// it would take away what the account had before (a stale window, another admin, or `Shop` typed for a stored
/// `shop`). With `fold` (lower_case_table_names is not 0) a database is matched in any case and renamed to the
/// spelling the server stores. Returns the edits left to run and the parts taken out.
pub fn trim_edits(g: &[Grant], edits: &[Edit], fold: bool) -> Res<(Vec<Edit>, Vec<Edit>)> {
    // Updated as each edit is cut, so a later edit of the same scope sees what an earlier one will have done.
    let mut state = db_grants(g);
    state.push((String::new(), global_privs(g)));
    let (mut kept, mut left) = (Vec::new(), Vec::new());
    for ed in edits {
        let mut db = ed.db.clone();
        if fold && !db.is_empty() && !state.iter().any(|(k, _)| *k == db) {
            let low = db.to_lowercase();
            let alike: Vec<&String> = state.iter().map(|(k, _)| k).filter(|k| !k.is_empty() && k.to_lowercase() == low).collect();
            match alike.as_slice() {
                [] => {}
                [one] => db = (*one).clone(),
                _ => return Err(format!("{} is held under several spellings the server does not tell apart, so Quarry cannot tell which one would change.", shown_pattern(&ed.db))),
            }
        }
        let at = match state.iter().position(|(k, _)| *k == db) {
            Some(i) => i,
            None => {
                state.push((db.clone(), BTreeSet::new()));
                state.len() - 1
            }
        };
        let held = &mut state[at].1;
        let (grant, g_out): (Vec<String>, Vec<String>) = ed.grant.iter().cloned().partition(|p| !held.contains(p));
        held.extend(grant.iter().cloned());
        let (revoke, r_out): (Vec<String>, Vec<String>) = ed.revoke.iter().cloned().partition(|p| held.contains(p));
        for p in &revoke {
            held.remove(p);
        }
        let out = Edit { db: db.clone(), grant: g_out, revoke: r_out };
        if !out.is_empty() {
            left.push(out);
        }
        let ed = Edit { db, grant, revoke };
        if !ed.is_empty() {
            kept.push(ed);
        }
    }
    Ok((kept, left))
}

/// What `trim_edits` took out, for the result message.
fn left_note(left: &[Edit]) -> String {
    let (g, r) = (list_text(left, |e| &e.grant), list_text(left, |e| &e.revoke));
    let mut s = String::new();
    if !g.is_empty() {
        s.push_str(&format!(" Already held, so not granted again: {g}."));
    }
    if !r.is_empty() {
        s.push_str(&format!(" Not held, so not revoked: {r}."));
    }
    s
}

/// Whether two role names are the same role. MySQL writes `name@host`, reads a bare name as host `%` and compares
/// the host without case; MariaDB names are bare on both sides and compared as written.
fn same_role(a: &str, b: &str) -> bool {
    match (a.rsplit_once('@'), b.rsplit_once('@')) {
        (None, None) => a == b,
        (x, y) => {
            let ((ua, ha), (ub, hb)) = (x.unwrap_or((a, "%")), y.unwrap_or((b, "%")));
            ua == ub && ha.eq_ignore_ascii_case(hb)
        }
    }
}

/// Roles cut like `trim_edits`: one the account already has leaves `grant` and one it lacks leaves `revoke`, so the
/// undo never takes away a role it had before. A revoked role takes the spelling SHOW GRANTS gives, which undo reads
/// back. Returns the grants, the revokes and the roles taken out.
pub fn trim_roles(held: &[String], grant: &[String], revoke: &[String]) -> (Vec<String>, Vec<String>, Vec<String>) {
    let find = |r: &String| held.iter().find(|h| same_role(h, r));
    let mut left = Vec::new();
    let mut g = Vec::new();
    for r in grant {
        if find(r).is_some() { left.push(r.clone()) } else { g.push(r.clone()) }
    }
    let mut v = Vec::new();
    for r in revoke {
        match find(r) {
            Some(h) => v.push(h.clone()),
            None => left.push(r.clone()),
        }
    }
    (g, v, left)
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
        // Accounts exist only on the MySQL family, so there is nothing to read.
        Dialect::Postgres => return Vec::new(),
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
            let res = c.execute(sqlx::AssertSqlSafe(sql)).await.map(|_| ());
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
    // What each role about to be taken away carries, read first: undo gives it back with the role. The same read
    // cuts the role changes to what is not so already, and a rename leaves the account's grants as they are.
    let before = if plan.roles_revoke.is_empty() && plan.roles_grant.is_empty() {
        Vec::new()
    } else {
        grants_of(c, m, acct).await.map_err(|x| format!("Could not read the roles of {}: {x}", acct.label()))?
    };
    let (roles_grant, roles_revoke, left) = trim_roles(&roles_of(&before), &plan.roles_grant, &plan.roles_revoke);
    // Only a role taken away needs to know whether it was a default one, for its undo.
    let defaults = if roles_revoke.is_empty() { Vec::new() } else { default_roles(c, d, acct).await };
    let plan = &AccountPlan { roles_grant, roles_revoke, ..plan.clone() };
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
    if (!gr.is_empty() || !rv.is_empty()) && let Ok(g) = grants_of(c, m, cur).await {
        let defaults_now = default_roles(c, d, cur).await;
        let names = |v: Vec<&String>| v.into_iter().map(|r| role_shown(r)).collect::<Vec<_>>().join(", ");
        let label = [(!gr.is_empty()).then(|| format!("gave role {}", names(gr.iter().collect()))), (!rv.is_empty()).then(|| format!("took role {}", names(rv.iter().map(|r| &r.role).collect())))].into_iter().flatten().collect::<Vec<_>>().join(", ");
        let id = ident(c, d, cur).await;
        undo.push(action(conn, cur, AccountUndo { account: cur.clone(), label, what: What::Roles { grant: gr, revoke: rv, expect: roles_state(&g, &defaults_now) }, ident: id }));
    }
    if let Some(x) = stop {
        return fail(x, done);
    }
    let note = if left.is_empty() { String::new() } else { format!(". Left out roles it already had or did not have: {}", left.iter().map(|r| role_shown(r)).collect::<Vec<_>>().join(", ")) };
    Ok(format!("Changed {}{note}", cur.label()))
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
                db.mysql_only()?;
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
                // Unread counts as not folding: the window then only compares names exactly, as before.
                let fold_case = super::create::folds_case(&mut c).await.unwrap_or(false);
                Ok(Loaded { grants, info, info_err, fold_case })
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
                db.mysql_only()?;
                let mut c = writer_conn(&db, "", read_only).await?.into_my()?;
                let out = async {
                    let m = session_mode(&mut c).await?;
                    // Checked on the writer, against what the server holds now rather than what the window last read.
                    let now = grants_of(&mut c, m, &acct).await?;
                    let fold = super::create::folds_case(&mut c).await?;
                    let (edits, left) = trim_edits(&now, &edits, fold)?;
                    let skipped = left_note(&left);
                    if edits.is_empty() {
                        return Ok(format!("Nothing to change for {}.{skipped}", acct.label()));
                    }
                    check_partial(&now, &edits)?;
                    let (mut ran, err) = run_edits(&mut c, m, &acct, &edits).await;
                    let mut note = skipped;
                    if !ran.is_empty() {
                        // Without a snapshot there is nothing to check against, so no undo is offered.
                        match snapshot(&mut c, m, &acct, &mut ran).await {
                            Ok(expect) => {
                                let id = ident(&mut c, db.dialect, &acct).await;
                                undo = Some(action(conn, &acct, AccountUndo { account: acct.clone(), label: label_of(&ran), what: What::Grants { edits: ran.clone(), expect }, ident: id }));
                            }
                            Err(why) => note.push_str(&format!(" There is no undo for it: {why}.")),
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
                db.mysql_only()?;
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
                db.mysql_only()?;
                let mut c = writer_conn(&db, "", read_only).await?.into_my()?;
                let out = async {
                    let m = session_mode(&mut c).await?;
                    if matches!(db.dialect, Dialect::MariaDb) {
                        // SET PASSWORD sets the mysql_native_password part only; any other way in would be left as it was.
                        if let Ok(i) = read_info(&mut c, db.dialect, &acct).await && (i.chain || (!i.plugin.is_empty() && i.plugin != "mysql_native_password")) {
                            return Err(format!("{} logs in with {}, and Quarry only sets a mysql_native_password password on MariaDB. Change it in a SQL shell.", acct.label(), if i.chain { "more than one method".to_string() } else { i.plugin }));
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
                db.mysql_only()?;
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
                db.mysql_only()?;
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
        db.mysql_only()?;
        let mut c = writer_conn(db, "", read_only).await?.into_my()?;
        let out = async {
            let m = session_mode(&mut c).await?;
            let now = grants_of(&mut c, m, &acct).await.map_err(|x| format!("Undo refused: {x}"))?;
            if let (Some(want), Some(got)) = (&u.ident, ident(&mut c, db.dialect, &acct).await) && *want != got {
                return Err(format!("Undo refused: {} is not the account this change was made on (it was dropped and made again, or its password changed).", acct.label()));
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
                    if revoke.iter().any(|r| r.default) && let Some(sql) = default_roles_sql(m, db.dialect, &acct, &want)? {
                        exec_any(&mut c, &sql).await?;
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
    let undo: Vec<UndoAction> = keep.then(|| UndoAction { agent: None, at: a.at, conn, db: a.db, coll: a.coll, kind: a.kind }).into_iter().collect();
    let renamed = res.is_ok() && matches!(u.what, What::Renamed { .. });
    let op = if renamed { Op::UndoneRename } else { Op::Undone };
    let acct = if renamed { match &u.what { What::Renamed { from } => from.clone(), _ => acct } } else { acct };
    Msg::SqlUserDone { conn, account: acct, op, res, undo }
}

#[cfg(test)]
#[path = "users_tests.rs"]
mod tests;
