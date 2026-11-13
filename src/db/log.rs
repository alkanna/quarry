//! The command log: every driver command with its timing, kept in memory for the log panel.

use super::*;

/// One command sent to a server, for the query log.
pub struct LogEntry {
    pub conn: u64,
    pub at: std::time::SystemTime,
    pub db: String,
    pub name: String,
    pub cmd: String,
    /// None while the command is still running.
    pub ms: Option<u128>,
    pub err: Option<String>,
    req: i32,
}

impl LogEntry {
    /// A finished command, for the offline demo.
    pub fn finished(conn: u64, secs_ago: u64, db: &str, name: &str, cmd: &str, ms: u128, err: Option<&str>) -> Self {
        let at = std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago);
        LogEntry { conn, at, db: db.into(), name: name.into(), cmd: cmd.into(), ms: Some(ms), err: err.map(Into::into), req: 0 }
    }
}

/// Entries kept. Tests run in parallel in one process and all write here, so a cap this low would drop a test's own
/// entries before it reads them back.
pub const LOG_MAX: usize = if cfg!(test) { 1 << 20 } else { 2000 };

// ponytail: one process-wide log, clients of every connection write here; keyed by conn id.
pub static LOG: std::sync::Mutex<std::collections::VecDeque<LogEntry>> = std::sync::Mutex::new(std::collections::VecDeque::new());

/// The log, even after a thread panicked while holding it: the entries are plain data, and refusing the lock stopped
/// every later write for the rest of the session.
pub fn log() -> std::sync::MutexGuard<'static, std::collections::VecDeque<LogEntry>> {
    LOG.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Feeds the driver's command events into `LOG`. The driver already blanks the bodies of
/// authentication and user password commands, so secrets never reach the log.
pub(crate) fn log_event(conn: u64, ev: mongodb::event::command::CommandEvent) {
    use mongodb::event::command::CommandEvent as E;
    match ev {
        E::Started(s) => {
            // Driver housekeeping, not something the user asked for.
            if matches!(s.command_name.as_str(), "hello" | "isMaster" | "ismaster" | "endSessions" | "killCursors") {
                return;
            }
            let mut cmd = s.command;
            for k in ["lsid", "$db", "$clusterTime", "$readPreference", "txnNumber", "apiVersion"] {
                cmd.remove(k);
            }
            // Bulk writes carry thousands of documents; a few show the shape.
            for k in ["documents", "updates", "deletes"] {
                if let Some(Bson::Array(a)) = cmd.get_mut(k).filter(|a| a.as_array().is_some_and(|a| a.len() > 3)) {
                    let more = a.len() - 3;
                    a.truncate(3);
                    a.push(Bson::String(format!("… {more} more")));
                }
            }
            let mut text = compact(&Bson::Document(cmd));
            if let Some((cut, _)) = text.char_indices().nth(4000) {
                text.truncate(cut);
                text.push_str(" …");
            }
            let entry = LogEntry { conn, at: std::time::SystemTime::now(), db: s.db, name: s.command_name, cmd: text, ms: None, err: None, req: s.request_id };
            let mut log = log();
            if log.len() >= LOG_MAX {
                log.pop_front();
            }
            log.push_back(entry);
        }
        other => {
            let (req, ms, err) = match other {
                E::Succeeded(s) => (s.request_id, s.duration.as_millis(), None),
                E::Failed(f) => (f.request_id, f.duration.as_millis(), Some(e(f.failure))),
                _ => return,
            };
            let mut log = log();
            if let Some(l) = log.iter_mut().rev().find(|l| l.conn == conn && l.req == req) {
                (l.ms, l.err) = (Some(ms), err);
            }
        }
    }
}

tokio::task_local! {
    /// The connection and database a job's statements are logged under, for code that holds only a raw connection.
    static SQL_CTX: (u64, String);
}

/// Runs `f` with its statements logged under this connection and database, through `note_here`.
pub(crate) fn sql_scope<F: std::future::Future>(conn: u64, schema: &str, f: F) -> impl std::future::Future<Output = F::Output> + use<F> {
    SQL_CTX.scope((conn, schema.to_string()), f)
}

/// Logs a statement of the job that `sql_scope` wraps; outside one (a test) it does nothing.
pub(crate) fn note_here<T, X: std::fmt::Display>(sql: &str, start: std::time::Instant, res: &Result<T, X>) {
    let err = res.as_ref().err().map(|x| x.to_string());
    let _ = SQL_CTX.try_with(|(conn, schema)| log_sql(*conn, schema, sql, start.elapsed().as_millis(), err.as_deref()));
}

type Span = std::ops::Range<usize>;

/// Whether the text holds `scheme://user:password@host`, as in a CREATE SUBSCRIPTION or dblink connection string. The
/// whole literal is dropped because the password cannot be told apart from the rest once quoting is involved.
fn has_uri_password(s: &str) -> bool {
    // libpq ends the user and password part at the first `@` or `/` only, so `?`, `#`, spaces and quotes can sit
    // inside a password.
    s.match_indices("://").any(|(i, _)| {
        let rest = &s[i + 3..];
        rest.find(['@', '/']).is_some_and(|j| rest[j..].starts_with('@') && rest[..j].contains(':'))
    })
}

/// Where each secret string of a statement sits. Every reading the server may take of the quotes is tried, one at a
/// time, and the ranges are merged, so a literal that one reading would end early is still covered to its end.
fn secret_ranges(sql: &str) -> Vec<Span> {
    use super::sql::{lex::{nth_reading, readings, Tok}, Dialect};
    let mut hits: Vec<Span> = Vec::new();
    // The log does not know the dialect of what it was handed, so every reading of both is tried: PostgreSQL's dollar
    // quotes and E'' strings can hold a password that no MySQL reading sees as one string.
    let all = [(Dialect::MySql, readings(Dialect::MySql)), (Dialect::Postgres, readings(Dialect::Postgres))];
    for (d, n) in all.into_iter().flat_map(|(d, count)| (0..count).map(move |n| (d, n))) {
        let toks = nth_reading(sql, d, n);
        // Executable comment marks are not tokens of the statement: `/*!80000 'pw' */` reads as the string itself.
        let code: Vec<&(Tok, Span)> = toks.iter().filter(|(t, _)| !matches!(t, Tok::Space | Tok::Comment | Tok::ExecOpen | Tok::ExecClose)).collect();
        let text = |i: usize| code.get(i).map(|(_, r)| &sql[r.clone()]).unwrap_or("");
        let word = |i: usize, w: &str| code.get(i).is_some_and(|(t, _)| *t == Tok::Word) && text(i).eq_ignore_ascii_case(w);
        let punct = |i: usize, p: &str| code.get(i).is_some_and(|(t, _)| *t == Tok::Punct) && text(i) == p;
        // The string at or after `i`, past an `=`, a `(`, PASSWORD and a charset or hex introducer such as `_utf8mb4`, `N`, `X` or `B`.
        let secret = |mut i: usize| -> Option<Span> {
            while let Some((t, r)) = code.get(i) {
                let w = &sql[r.clone()];
                if *t == Tok::Str {
                    return Some(r.clone());
                }
                let skip = (*t == Tok::Punct && (w == "=" || w == "(")) || (*t == Tok::Word && (["password", "n", "x", "b"].iter().any(|k| w.eq_ignore_ascii_case(k)) || w.starts_with('_')));
                if !skip {
                    return None;
                }
                i += 1;
            }
            None
        };
        // Everything between the `(` at `open` and its matching `)`: any expression can build the password.
        let inside = |open: usize| -> Option<Span> {
            let (mut depth, mut end) = (0usize, code.len());
            for j in open..code.len() {
                if punct(j, "(") {
                    depth += 1;
                } else if punct(j, ")") {
                    depth -= 1;
                    if depth == 0 {
                        end = j;
                        break;
                    }
                }
            }
            (end > open + 1).then(|| code[open + 1].1.start..code[end - 1].1.end)
        };
        let (mut identified, mut after_secret) = (false, false);
        for i in 0..code.len() {
            if punct(i, ";") {
                (identified, after_secret) = (false, false);
                continue;
            }
            // A string that holds a statement about passwords: PREPARE s FROM 'CREATE USER ... IDENTIFIED BY ...'.
            if code[i].0 == Tok::Str {
                let lower = text(i).to_ascii_lowercase();
                if lower.contains("identified") || lower.contains("password") || has_uri_password(&lower) {
                    hits.push(code[i].1.clone());
                }
                continue;
            }
            if code[i].0 != Tok::Word {
                continue;
            }
            let found = if word(i, "identified") {
                identified = true;
                None
            } else if identified && (word(i, "by") || word(i, "as") || word(i, "using")) {
                // BY 'x', WITH plugin AS 'hash', VIA plugin USING 'hash' or USING PASSWORD('x')
                if word(i + 1, "password") && punct(i + 2, "(") { inside(i + 2) } else { secret(i + 1) }
            } else if word(i, "password") && i > 0 && word(i - 1, "set") {
                // SET PASSWORD [FOR user] = 'x': the user part holds strings of its own, so look for the `=`.
                (i + 1..code.len()).find(|&j| punct(j, "=") || punct(j, ";")).filter(|&j| punct(j, "=")).and_then(|j| secret(j + 1))
            } else if (word(i, "password") || word(i, "old_password")) && punct(i + 1, "(") {
                inside(i + 1)
            } else if word(i, "password") || word(i, "old_password") || word(i, "master_password") || word(i, "source_password") {
                secret(i + 1)
            } else if word(i, "replace") && after_secret {
                // ALTER USER ... IDENTIFIED BY 'new' REPLACE 'current'
                secret(i + 1)
            } else {
                None
            };
            if let Some(f) = found {
                hits.push(f);
                after_secret = true;
            }
        }
    }
    hits.sort_by_key(|r| (r.start, std::cmp::Reverse(r.end)));
    let mut out: Vec<Span> = Vec::new();
    for h in hits {
        match out.last_mut() {
            Some(l) if h.start < l.end => l.end = l.end.max(h.end),
            _ => out.push(h),
        }
    }
    out
}

/// The statement with every password literal replaced by '***', and the literals that were cut out.
pub(crate) fn redact_sql(sql: &str) -> (String, Vec<String>) {
    let (mut out, mut last, mut cut) = (String::with_capacity(sql.len()), 0, Vec::new());
    for r in secret_ranges(sql) {
        out.push_str(&sql[last..r.start]);
        out.push_str("'***'");
        last = r.end;
        cut.push(sql[r].to_string());
    }
    out.push_str(&sql[last..]);
    (out, cut)
}

/// One statement Quarry sent, for the log. Passwords are redacted here and nowhere else, so every caller can hand over
/// the text it ran. The server's error may quote the statement from the point it failed at, which can be the password,
/// so a failing statement that held one loses that quote as well.
pub fn log_sql(conn: u64, db: &str, sql: &str, ms: u128, err: Option<&str>) {
    // Cut first, so a huge INSERT is never lexed whole. A password literal the cut runs through is unterminated, and the
    // lexer reads an unterminated string to the end of the text, so it stays redacted.
    let long = sql.char_indices().nth(4000).map(|(i, _)| i);
    let (mut text, cut) = redact_sql(&sql[..long.unwrap_or(sql.len())]);
    let guarded = long.is_some() || !cut.is_empty();
    let err = err.map(|m| {
        let mut m = m.to_string();
        if guarded {
            for c in &cut {
                m = m.replace(c.as_str(), "'***'");
            }
            if let Some(p) = m.find("near '") {
                let tail = m.rfind("' at line").map_or(String::new(), |q| m[q + 1..].to_string());
                m = format!("{}near '***'{tail}", &m[..p]);
            }
        }
        m
    });
    if long.is_some() {
        text.push_str(" …");
    }
    // The verb fills the name column like a Mongo command name, so the filter and the columns work the same.
    let name = super::sql::lex::tokens(&text, super::sql::Dialect::MySql).into_iter().find(|(t, _)| *t == super::sql::lex::Tok::Word).map_or(String::new(), |(_, r)| text[r].to_ascii_uppercase());
    let entry = LogEntry { conn, at: std::time::SystemTime::now(), db: db.into(), name, cmd: text, ms: Some(ms), err, req: 0 };
    let mut log = log();
    if log.len() >= LOG_MAX {
        log.pop_front();
    }
    log.push_back(entry);
}

#[cfg(test)]
mod tests {
    use super::redact_sql;

    fn r(s: &str) -> String {
        redact_sql(s).0
    }

    #[test]
    fn postgres_role_passwords_are_redacted_in_every_literal_form() {
        assert_eq!(r("CREATE ROLE a LOGIN PASSWORD 'pw'"), "CREATE ROLE a LOGIN PASSWORD '***'");
        assert_eq!(r("create user a with encrypted password 'pw' valid until 'infinity'"), "create user a with encrypted password '***' valid until 'infinity'");
        assert_eq!(r("ALTER ROLE a WITH PASSWORD E'p\\'w'"), "ALTER ROLE a WITH PASSWORD '***'");
        assert_eq!(r("ALTER USER a PASSWORD $$p'w; x$$ CREATEDB"), "ALTER USER a PASSWORD '***' CREATEDB");
        assert_eq!(r("ALTER ROLE a PASSWORD $tag$p$$w$tag$"), "ALTER ROLE a PASSWORD '***'");
        // With standard_conforming_strings off the literal runs on, so the redaction covers the longer reading.
        assert_eq!(r("ALTER ROLE a PASSWORD 'p\\' -- x\n; SELECT 1"), "ALTER ROLE a PASSWORD '***'");
        assert_eq!(r("CREATE ROLE a PASSWORD NULL"), "CREATE ROLE a PASSWORD NULL");
        assert_eq!(r("SELECT 1"), "SELECT 1");
    }

    #[test]
    fn uri_passwords_are_redacted_in_any_string_literal() {
        assert_eq!(r("CREATE SUBSCRIPTION s CONNECTION 'postgresql://rep:secret@h/db' PUBLICATION p"), "CREATE SUBSCRIPTION s CONNECTION '***' PUBLICATION p");
        assert_eq!(r("SELECT dblink_connect('postgresql://u:secret@h/db')"), "SELECT dblink_connect('***')");
        assert_eq!(r("SELECT dblink_connect($$postgres://u:secret@h:5432/db?sslmode=require$$)"), "SELECT dblink_connect('***')");
        assert_eq!(r("CREATE SERVER x FOREIGN DATA WRAPPER w OPTIONS (url 'mysql://root:secret@h')"), "CREATE SERVER x FOREIGN DATA WRAPPER w OPTIONS (url '***')");
        assert_eq!(r("CHANGE REPLICATION SOURCE TO SOURCE_HOST='h', SOURCE_PASSWORD='secret'"), "CHANGE REPLICATION SOURCE TO SOURCE_HOST='h', SOURCE_PASSWORD='***'");
        assert_eq!(r("CHANGE MASTER TO MASTER_PASSWORD='secret'"), "CHANGE MASTER TO MASTER_PASSWORD='***'");
        // libpq reads the password up to the `@`, so these characters are part of it.
        for s in ["SELECT 'postgresql://u:p?w@h/db'", "SELECT 'postgresql://u:p#w@h/db'", "SELECT 'postgresql://u:p w@h/db'", "SELECT 'postgresql://u:it''s@h/db'"] {
            assert_eq!(r(s), "SELECT '***'", "{s}");
        }
        // A URI without a password, or text that only has an @ later on, stays readable.
        assert_eq!(r("SELECT 'postgresql://u@h/db'"), "SELECT 'postgresql://u@h/db'");
        assert_eq!(r("SELECT 'https://example.com/a:b@c'"), "SELECT 'https://example.com/a:b@c'");
    }

    #[test]
    fn create_user_and_alter() {
        assert_eq!(r("CREATE USER 'a'@'%' IDENTIFIED BY 'pw'"), "CREATE USER 'a'@'%' IDENTIFIED BY '***'");
        assert_eq!(r("create user a identified   by\n\"pw\" password expire"), "create user a identified   by\n'***' password expire");
        assert_eq!(r("CREATE USER a IDENTIFIED WITH mysql_native_password BY 'pw', b IDENTIFIED BY 'two'"), "CREATE USER a IDENTIFIED WITH mysql_native_password BY '***', b IDENTIFIED BY '***'");
        assert_eq!(r("ALTER USER a IDENTIFIED BY 'new' REPLACE 'old'"), "ALTER USER a IDENTIFIED BY '***' REPLACE '***'");
        assert_eq!(r("CREATE USER a IDENTIFIED BY _utf8mb4'pw'"), "CREATE USER a IDENTIFIED BY _utf8mb4'***'");
        assert_eq!(r("CREATE USER a IDENTIFIED BY RANDOM PASSWORD"), "CREATE USER a IDENTIFIED BY RANDOM PASSWORD");
        assert_eq!(r("GRANT SELECT ON *.* TO a IDENTIFIED BY 'pw'"), "GRANT SELECT ON *.* TO a IDENTIFIED BY '***'");
    }

    #[test]
    fn password_functions() {
        assert_eq!(r("SET PASSWORD = PASSWORD('pw')"), "SET PASSWORD = PASSWORD('***')");
        assert_eq!(r("SET PASSWORD FOR 'u'@'h' = 'pw'"), "SET PASSWORD FOR 'u'@'h' = '***'");
        assert_eq!(r("set password for u@h = password ( 'pw' )"), "set password for u@h = password ( '***' )");
        assert_eq!(r("CREATE USER u IDENTIFIED VIA mysql_native_password USING PASSWORD('pw')"), "CREATE USER u IDENTIFIED VIA mysql_native_password USING PASSWORD('***')");
        assert_eq!(r("SELECT PASSWORD('pw')"), "SELECT PASSWORD('***')");
    }

    #[test]
    fn tricky_quoting() {
        // A backslash-escaped quote keeps the literal open under one reading and closes it under the other.
        let (t, cut) = redact_sql(r"CREATE USER a IDENTIFIED BY 'p\'w' , b IDENTIFIED BY 'x'");
        assert!(!t.contains("p\\'w") && !t.contains("'w'"), "{t}");
        assert!(!cut.is_empty());
        assert!(!r(r"ALTER USER a IDENTIFIED BY 'sec\'ret'; SELECT 1").contains("ret"));
        assert_eq!(r("ALTER USER a IDENTIFIED BY 'it''s'"), "ALTER USER a IDENTIFIED BY '***'");
        // Comments between the words and the secret.
        assert_eq!(r("ALTER USER a IDENTIFIED /* x */ BY -- c\n 'pw'"), "ALTER USER a IDENTIFIED /* x */ BY -- c\n '***'");
        // A string or comment that only mentions the words is left alone.
        // A string that talks about a password is dropped whole: it may be a statement for PREPARE.
        assert_eq!(r("SELECT 'IDENTIFIED BY ''x'''"), "SELECT '***'");
        assert_eq!(r("SELECT 1 -- IDENTIFIED BY 'x'"), "SELECT 1 -- IDENTIFIED BY 'x'");
        // Unterminated: everything to the end is the secret.
        assert_eq!(r("ALTER USER a IDENTIFIED BY 'pw"), "ALTER USER a IDENTIFIED BY '***'");
        // Inside an executable comment the server runs it.
        assert!(!r("/*!50000 CREATE USER a IDENTIFIED BY 'pw' */").contains("pw"));
        // Statements after it are untouched and a later one without the keyword is not redacted.
        assert_eq!(r("CREATE USER a IDENTIFIED BY 'pw'; SELECT 'by' AS \"by\""), "CREATE USER a IDENTIFIED BY '***'; SELECT 'by' AS \"by\"");
        assert_eq!(r("SELECT 'it''s'"), "SELECT 'it''s'");
    }

    #[test]
    fn executable_comments_do_not_hide_the_secret() {
        for src in ["ALTER USER a IDENTIFIED BY /*!80000 'pw' */", "ALTER USER a IDENTIFIED /*!BY*/ 'pw'", "ALTER USER a /*!80000 IDENTIFIED BY 'pw' */", "ALTER USER a IDENTIFIED /*M!BY */ 'pw'"] {
            assert!(!r(src).contains("pw"), "{src}: {}", r(src));
        }
        super::log_sql(990_002, "d", "ALTER USER a IDENTIFIED BY /*!80000 'hunter2' */ bogus", 1, Some("syntax error near ''hunter2' */ bogus' at line 1"));
        let log = super::log();
        let l = log.iter().rev().find(|l| l.conn == 990_002).unwrap();
        assert!(!l.cmd.contains("hunter2") && !l.err.as_deref().unwrap().contains("hunter2"), "{} {:?}", l.cmd, l.err);
    }

    #[test]
    fn hashes_expressions_and_prepared_text() {
        for src in [
            "CREATE USER a IDENTIFIED WITH caching_sha2_password AS 'hash0123'",
            "CREATE USER a IDENTIFIED VIA mysql_native_password USING 'hash0123'",
            "CREATE USER a IDENTIFIED VIA mysql_native_password USING PASSWORD('hash0123')",
            "SET PASSWORD = PASSWORD(CONCAT('hash','0123'))",
            "SELECT PASSWORD(x'68756e74657232')",
            "SELECT OLD_PASSWORD(b'0110')",
            "PREPARE s FROM 'CREATE USER a IDENTIFIED BY ''hash0123'''",
            r"SET @q = 'alter user a identified by \'hash0123\''",
            "SET PASSWORD FOR 'u'@'h' = PASSWORD(CONCAT('hash', (SELECT '0123')))",
        ] {
            let out = r(src);
            assert!(!out.contains("hash") && !out.contains("0123") && !out.contains("6875") && !out.contains("0110"), "{src}: {out}");
        }
        assert_eq!(r("SET PASSWORD = PASSWORD(CONCAT('a','b'))"), "SET PASSWORD = PASSWORD('***')");
        assert_eq!(r("SELECT 1, 'x'"), "SELECT 1, 'x'");
    }

    #[test]
    fn a_secret_straddling_the_cut_stays_redacted() {
        let sql = format!("CREATE USER a IDENTIFIED BY '{}tailsecret'", "p".repeat(3970));
        assert!(sql.chars().count() > 4000);
        super::log_sql(990_003, "d", &sql, 1, None);
        let log = super::log();
        let l = log.iter().rev().find(|l| l.conn == 990_003).unwrap();
        assert!(l.cmd.starts_with("CREATE USER a IDENTIFIED BY '***'") && !l.cmd.contains("ppp") && l.cmd.ends_with(" …"), "{}", &l.cmd[..l.cmd.len().min(80)]);
        assert!(l.cmd.chars().count() < 100);
    }

    #[test]
    fn errors_lose_the_quoted_secret() {
        super::log_sql(990_001, "d", "CREATE USER a IDENTIFIED BY 'hunter2' bogus", 1, Some("You have an error in your SQL syntax; check the manual for the right syntax to use near ''hunter2' bogus' at line 1"));
        let log = super::log();
        let l = log.iter().rev().find(|l| l.conn == 990_001).unwrap();
        assert!(!l.cmd.contains("hunter2") && !l.err.as_deref().unwrap().contains("hunter2"), "{} {:?}", l.cmd, l.err);
        assert_eq!(l.name, "CREATE");
    }
}
