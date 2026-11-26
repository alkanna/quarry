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

mod grants;
mod settings;
mod statements;
pub use grants::*;
pub use settings::*;
pub use statements::*;

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
