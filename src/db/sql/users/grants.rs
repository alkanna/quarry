//! Parsing SHOW GRANTS into scopes, privileges and roles.

use super::*;

// Parsing SHOW GRANTS.

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Tok {
    Word(String),
    Ident(String),
    Str(String),
    Sym(char),
}

/// The tokens of one SHOW GRANTS line. Backticks double to escape themselves; in a string both a doubled
/// quote and a backslash escape are read, because the server writes the first and old versions the second.
pub(super) fn tokens(s: &str) -> Vec<Tok> {
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

pub(super) fn is_word(t: Option<&Tok>, w: &str) -> bool {
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
