//! Plain-language versions of server and driver errors: a title, a hint and the fixes to offer.
//! The raw text is never replaced, only explained; callers keep it for Server message and Copy details.

/// What went wrong, as far as the text tells.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrKind {
    Privilege,
    SignIn,
    Unreachable,
    Tunnel,
    /// The Cloud SQL proxy or gcloud failed, or Cloud SQL refused an IAM sign-in.
    CloudSql,
    Tls,
    Lost,
    Timeout,
    Duplicate,
    Validation,
    Missing,
    Syntax,
    Other,
}

/// A button the error offers. The app decides what each one does in its context.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fix {
    Retry,
    EditConn,
    Explain,
    RaiseLimit,
    Refresh,
    Validator,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Explained {
    pub kind: ErrKind,
    pub title: String,
    pub hint: String,
    /// "13 Unauthorized", "1062 (23000)", for the chip next to Server message.
    pub code: Option<String>,
    /// The privilege a refused command needed and a built-in role that grants it.
    pub need: Option<(String, Option<&'static str>)>,
    /// A duplicate key's field and value, for Show the other document.
    pub dup: Option<(String, String)>,
    pub fixes: Vec<Fix>,
}

/// Where the error happened, for the wording. Every field may be empty.
#[derive(Clone, Default, Debug)]
pub struct ErrCtx {
    /// What was being done, as a verb phrase: "load the documents", "save".
    pub what: String,
    /// "shop.orders"
    pub ns: String,
    pub host: String,
}

/// Whether a server error means the user lacks a privilege, in the server's, Atlas's or MySQL's wording.
pub fn is_unauthorized(e: &str) -> bool {
    pg_code(e).as_deref() == Some("42501") || e.contains("(Unauthorized)") || e.contains("not authorized") || e.contains("is not allowed to do action") || matches!(mysql_code(e), Some(1142 | 1143 | 1044 | 1227 | 1370))
}

/// The SQLSTATE in a PostgreSQL error as `db::sql` words it: "error returned from database: 42P01: relation ...".
/// A MySQL number is four digits followed by a bracket, so the two never read as each other.
fn pg_code(e: &str) -> Option<String> {
    let s = e.strip_prefix("error returned from database: ")?;
    let code = s.get(..5)?;
    (code.chars().all(|c| c.is_ascii_digit() || c.is_ascii_uppercase()) && s[5..].starts_with(": ")).then(|| code.to_string())
}

/// Where a PostgreSQL error points in the statement, from the "Position: 15 (line 1, column 15)" line.
fn pg_position(e: &str) -> Option<String> {
    let i = e.find("\nPosition: ")?;
    Some(e[i + 11..].lines().next()?.trim().to_string())
}

/// The MongoDB server error code and name in "Error code 13 (Unauthorized)", or a write error's code.
fn mongo_code(e: &str) -> Option<(u32, String)> {
    if let Some(i) = e.find("Error code ") {
        let rest = &e[i + 11..];
        let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
        let name = rest[n.len()..].strip_prefix(" (").and_then(|r| r.split(')').next()).unwrap_or_default().to_string();
        return n.parse().ok().map(|n| (n, name));
    }
    // Write errors print their struct: "WriteError { code: 11000, code_name: None, ..."
    let i = e.find("code: ")?;
    let n: String = e[i + 6..].chars().take_while(char::is_ascii_digit).collect();
    n.parse().ok().map(|n| (n, String::new()))
}

/// The MySQL error number in sqlx's "1062 (23000): Duplicate entry ...".
fn mysql_code(e: &str) -> Option<u32> {
    let s = e.strip_prefix("error returned from database: ").unwrap_or(e);
    let n: String = s.chars().take_while(char::is_ascii_digit).collect();
    (n.len() == 4 && s[4..].starts_with(" (")).then(|| n.parse().ok()).flatten()
}

/// The server's own sentence, without the driver's prefix, the echoed command and our advice line.
pub fn server_text(e: &str) -> String {
    let first = e.lines().next().unwrap_or(e);
    if pg_code(first).is_some() {
        return first["error returned from database: ".len() + 7..].trim().to_string();
    }
    let s = match first.find("Error code ").and_then(|i| first[i..].find("): ").map(|j| i + j + 3)) {
        Some(i) => &first[i..],
        None => first.strip_prefix("error returned from database: ").map(|s| s.split_once("): ").map_or(s, |x| x.1)).unwrap_or(first),
    };
    let s = s.find(" to execute command").map_or(s, |i| &s[..i]);
    if let Some(i) = s.find("message: \"") {
        return s[i + 10..].split('"').next().unwrap_or(s).to_string();
    }
    s.trim().to_string()
}

/// Masks the password in any connection string inside the text, for Copy details.
pub fn scrub_uri(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("://") {
        out.push_str(&rest[..i + 3]);
        rest = &rest[i + 3..];
        let end = rest.find(|c: char| c.is_whitespace() || c == '/' || c == '"').unwrap_or(rest.len());
        let auth = &rest[..end];
        match (auth.rfind('@'), auth.find(':')) {
            (Some(at), Some(c)) if c < at => {
                out.push_str(&auth[..c + 1]);
                out.push_str("****");
                out.push_str(&auth[at..]);
            }
            _ => out.push_str(auth),
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// The command and namespace a privilege error names, and the built-in role that grants it.
fn refused(e: &str, ns: &str) -> (Option<String>, String, Option<&'static str>) {
    let between = |s: &str, a: &str, b: &str| s.find(a).map(|i| &s[i + a.len()..]).and_then(|r| r.find(b).map(|j| r[..j].to_string()));
    // Atlas: "user is not allowed to do action [find] on [shop.orders]"
    let (cmd, mut where_) = if let Some(c) = between(e, "do action [", "]") {
        (Some(c), between(e, "] on [", "]").unwrap_or_default().trim_end_matches('.').to_string())
    } else if let Some(c) = between(e, "execute command { ", ":") {
        let db = between(e, "not authorized on ", " ").unwrap_or_default();
        let coll = between(e, &format!("{c}: \""), "\"").filter(|_| !db.is_empty());
        (Some(c), coll.map_or(db.clone(), |c| format!("{db}.{c}")))
    } else if pg_code(e).is_some() {
        // PostgreSQL: "permission denied for table orders"; it names no command.
        let what = server_text(e);
        (None, what.strip_prefix("permission denied for ").and_then(|r| r.split_once(' ')).map(|x| x.1.to_string()).unwrap_or_default())
    } else if let Some(c) = mysql_code(e).and_then(|_| server_text(e).split(' ').next().map(str::to_string)).filter(|w| w.chars().all(|c| c.is_ascii_uppercase())) {
        // MySQL: "SELECT command denied to user 'a'@'%' for table 'orders'"
        (Some(c.to_lowercase()), between(e, "for table '", "'").unwrap_or_default())
    } else {
        (None, String::new())
    };
    if where_.is_empty() || !where_.contains('.') && ns.starts_with(&format!("{where_}.")) {
        where_ = ns.to_string();
    }
    let role = match cmd.as_deref() {
        Some("find" | "aggregate" | "count" | "distinct" | "listIndexes" | "listCollections" | "collStats" | "dbStats" | "changeStream" | "getMore" | "select") => Some("read"),
        Some("insert" | "update" | "delete" | "findAndModify" | "createIndexes" | "dropIndexes" | "drop" | "create" | "renameCollection" | "remove" | "insert_into") => Some("readWrite"),
        Some("collMod" | "compact" | "validate") => Some("dbAdmin"),
        Some("serverStatus" | "currentOp" | "inprog" | "replSetGetStatus" | "top" | "listDatabases") => Some("clusterMonitor"),
        Some("killOp" | "killop") => Some("clusterManager"),
        Some("usersInfo" | "rolesInfo" | "viewUser" | "viewRole") => Some("userAdmin"),
        _ => None,
    };
    (cmd, where_, role)
}

fn verb(cmd: Option<&str>) -> &'static str {
    match cmd {
        Some("find" | "aggregate" | "count" | "distinct" | "getMore" | "select" | "listIndexes" | "collStats" | "changeStream") => "read",
        Some("insert" | "update" | "delete" | "findAndModify" | "remove") => "change",
        _ => "",
    }
}

/// The duplicate key's field and value: Mongo's "dup key: { number: 4410 }", MySQL's "Duplicate entry '4410' for key 'orders.number'".
fn dup_key(e: &str) -> Option<(String, String)> {
    if let Some(i) = e.find("dup key: { ") {
        let r = &e[i + 11..];
        let (f, v) = r.split_once(": ")?;
        // The driver prints the message through Debug, so a string's quotes arrive escaped.
        let v = v.split_once(" }").map_or(v, |x| x.0).replace("\\\"", "\"");
        return Some((f.trim().to_string(), v.trim().to_string()));
    }
    // PostgreSQL's detail line: "Key (email)=(a@b.c) already exists."
    if let Some(i) = e.find("Key (") {
        let r = &e[i + 5..];
        if let Some((f, v)) = r.split_once(")=(") {
            return Some((f.to_string(), v.split_once(") already exists").map_or(v, |x| x.0).to_string()));
        }
    }
    let i = e.find("Duplicate entry '")?;
    let r = &e[i + 17..];
    let (v, r) = r.split_once("' for key '")?;
    let k = r.split('\'').next()?;
    Some((k.rsplit('.').next().unwrap_or(k).to_string(), v.to_string()))
}

/// Closing line the worker adds to a 57014 that came after Stop was pressed, so a stop is told from a statement timeout
/// by what Quarry did and not by the server's sentence, which follows its lc_messages.
pub const STOP_MARK: &str = "\nStopped by you.";

/// `raw` told as a stop when it is a cancelled statement (57014) and the person pressed Stop. Every path that sees a job's cancel flag
/// set calls this on the error it hands on, so a stop is never taken for a statement timeout, whichever path surfaced it.
pub fn stopped(mut raw: String) -> String {
    if pg_code(&raw).as_deref() == Some("57014") && !raw.contains(STOP_MARK) {
        raw.push_str(STOP_MARK);
    }
    raw
}

/// What went wrong, without the wording: cheap enough for a label drawn every frame.
pub fn classify(raw: &str) -> ErrKind {
    let low = raw.to_lowercase();
    let n = mongo_code(raw).map(|m| m.0).or(mysql_code(raw));
    let pg = pg_code(raw);
    let pg = pg.as_deref();
    // Before the others: an IAM refusal also says "authentication failed", and the cause is the Google identity, not a password.
    if low.contains("cloud sql proxy") || low.contains("cloud sql:") || low.contains("cloud sql iam") {
        ErrKind::CloudSql
    } else if is_unauthorized(raw) {
        ErrKind::Privilege
    } else if matches!(n, Some(18 | 1045 | 1698)) || matches!(pg, Some("28P01" | "28000")) || low.contains("authentication failed") || low.contains("scram failure") {
        ErrKind::SignIn
    } else if low.contains("ssh") && (low.contains("tunnel") || low.contains("handshake") || low.contains("ssh:")) {
        ErrKind::Tunnel
    } else if low.contains("certificate") || low.contains("tls") || low.contains("ssl") || n == Some(2026) {
        ErrKind::Tls
    } else if matches!(n, Some(50 | 262 | 3024 | 1205 | 1969)) || low.contains("exceeded time limit") || low.contains("maximum statement execution time") || pg == Some("57014") && !raw.contains(STOP_MARK) {
        ErrKind::Timeout
    } else if matches!(n, Some(11000 | 11001 | 1062 | 1586)) || pg == Some("23505") || raw.contains("E11000") {
        ErrKind::Duplicate
    } else if matches!(n, Some(121 | 3819 | 4025)) || low.contains("document failed validation") {
        ErrKind::Validation
    } else if matches!(n, Some(26 | 1146 | 1049)) || matches!(pg, Some("42P01" | "3F000" | "3D000")) || low.contains("ns does not exist") || low.contains("ns not found") {
        ErrKind::Missing
    } else if matches!(n, Some(2 | 9 | 14 | 15 | 40324 | 1064 | 1054)) || pg.is_some_and(|c| c.starts_with("42")) {
        ErrKind::Syntax
    } else if matches!(n, Some(6 | 89 | 91 | 189 | 2006 | 2013)) || pg.is_some_and(|c| c.starts_with("08") || c == "57P01") || low.contains("connection reset") || low.contains("broken pipe") || low.contains("connection closed") || low.contains("unexpected end of file") {
        ErrKind::Lost
    } else if low.contains("server selection timeout") || low.contains("connection refused") || low.contains("failed to lookup address") || low.contains("nodename nor servname") || low.contains("name or service not known") || low.contains("no route to host") || low.contains("pool timed out") || matches!(n, Some(2002 | 2003 | 2005)) || low.contains("timed out") {
        ErrKind::Unreachable
    } else {
        ErrKind::Other
    }
}

/// Explains a raw error. Unknown errors keep the server's sentence as the hint.
pub fn explain(raw: &str, cx: &ErrCtx) -> Explained {
    let low = raw.to_lowercase();
    let mongo = mongo_code(raw);
    let my = mysql_code(raw);
    let code = match (&mongo, my) {
        (Some((n, name)), _) if !name.is_empty() => Some(format!("{n} {name}")),
        (Some((n, _)), _) => Some(n.to_string()),
        _ if pg_code(raw).is_some() => pg_code(raw),
        (_, Some(n)) => raw.split(" (").nth(1).and_then(|s| s.split(')').next()).map(|state| format!("{n} ({state})")).or(Some(n.to_string())),
        _ => None,
    };
    let host = if cx.host.is_empty() { "the server".to_string() } else { cx.host.clone() };
    let ns = if cx.ns.is_empty() { "this collection".to_string() } else { cx.ns.clone() };
    let said = server_text(raw);
    let mut ex = Explained { kind: ErrKind::Other, title: String::new(), hint: String::new(), code, need: None, dup: None, fixes: Vec::new() };
    let kind = classify(raw);
    ex.kind = kind;
    match kind {
        ErrKind::Privilege => {
            let (cmd, where_, role) = refused(raw, &cx.ns);
            let place = if where_.is_empty() { ns.clone() } else { where_ };
            ex.title = match (verb(cmd.as_deref()), &cmd) {
                ("", Some(c)) => format!("Your user can't run {c} on {place}"),
                ("", None) => format!("Your user can't do this on {place}"),
                (v, _) => format!("Your user can't {v} {place}"),
            };
            ex.hint = "The server refused the request: the signed-in user lacks a privilege. Nothing is wrong with the query. Ask an admin for the role below, or connect as another user.".into();
            ex.need = cmd.map(|c| (format!("{c} on {place}"), role));
            ex.fixes = vec![Fix::Retry, Fix::EditConn];
        }
        ErrKind::SignIn => {
            ex.title = format!("Could not sign in to {host}");
            ex.hint = "The server was reached, but it did not accept the user name and password.".into();
            ex.fixes = vec![Fix::EditConn, Fix::Retry];
        }
        ErrKind::Unreachable => {
            ex.title = format!("Could not reach {host}");
            ex.hint = if low.contains("lookup") || low.contains("servname") || low.contains("service not known") { "The host name could not be found. Check its spelling, or whether it only resolves on a VPN.".into() } else { "No answer in time. The host may be down, or only reachable through a VPN or an SSH tunnel.".into() };
            ex.fixes = vec![Fix::Retry, Fix::EditConn];
        }
        ErrKind::Tunnel => {
            ex.title = "The SSH tunnel could not be opened".into();
            ex.hint = said.clone();
            ex.fixes = vec![Fix::EditConn, Fix::Retry];
        }
        ErrKind::CloudSql => {
            if low.contains("iam sign-in refused") {
                ex.title = "Cloud SQL refused the IAM sign-in".into();
                ex.hint = "The proxy signed in with Application Default Credentials, and the database has no IAM user for that account, or the instance does not allow IAM sign-in. Those credentials are often another account than the gcloud one that filled the user. The database user must be the same account (a service account name ends in .iam, not .gserviceaccount.com), and the instance flag cloudsql.iam_authentication must be on. Run `gcloud auth application-default login` as that account, then Retry.".into();
            } else if raw.contains(crate::cloudsql::ADC_MISSING) {
                ex.title = "IAM sign-in needs Application Default Credentials".into();
                ex.hint = "The Cloud SQL proxy signs in to IAM with Application Default Credentials, not the gcloud account, and this machine has none. Run `gcloud auth application-default login` as the account the database user names, then Retry.".into();
            } else if low.contains("gcloud auth login") {
                ex.title = "Sign in to Google Cloud first".into();
                ex.hint = "gcloud is signed out or its sign-in expired. Run `gcloud auth login` in a terminal, then try again.".into();
            } else if raw.contains(crate::cloudsql::PROXY_MISSING) {
                ex.title = "Cloud SQL Auth Proxy not found".into();
                ex.hint = "Quarry starts cloud-sql-proxy for each connection. It was not on PATH or in the gcloud SDK.".into();
            } else if low.contains("did not accept connections") {
                ex.title = "Cloud SQL proxy failed".into();
                ex.hint = "The proxy started but did not accept connections in time.".into();
            } else if low.starts_with("cloud sql proxy failed") {
                ex.title = "Cloud SQL proxy failed".into();
                ex.hint = "The proxy stopped before it accepted connections.".into();
            } else if low.contains("gcloud did not answer") {
                ex.title = "gcloud did not answer".into();
                ex.hint = "gcloud took too long and was stopped. Run the same gcloud command in a terminal to see what it waits for, then Retry.".into();
            } else {
                ex.title = "Cloud SQL proxy failed".into();
                ex.hint = raw.trim().into();
            }
            ex.fixes = vec![Fix::EditConn, Fix::Retry];
        }
        ErrKind::Tls => {
            ex.title = if low.contains("certificate") { "The server's certificate is not trusted".into() } else { format!("Could not open a TLS connection to {host}") };
            ex.hint = if low.contains("certificate") { "The server sent a certificate this machine does not trust. Add its CA file in the connection's TLS settings.".into() } else { said.clone() };
            ex.fixes = vec![Fix::EditConn, Fix::Retry];
        }
        ErrKind::Lost => {
            ex.title = format!("Lost the connection to {host}");
            ex.hint = "The server stopped answering mid-request. Tabs and drafts are kept; Retry asks again on a fresh connection.".into();
            ex.fixes = vec![Fix::Retry, Fix::EditConn];
        }
        ErrKind::Timeout => {
            ex.title = "The query ran past its time limit".into();
            ex.hint = if pg_code(raw).is_some() { "The server stopped it at the statement timeout. A filter or sort on a column without an index reads the whole table; Explain shows the plan.".into() } else { "The server stopped it. A filter on an unindexed field scans every document; Explain shows whether an index is used.".into() };
            ex.fixes = vec![Fix::Retry, Fix::Explain, Fix::RaiseLimit];
        }
        ErrKind::Duplicate => {
            let dup = dup_key(raw);
            let index = raw.find("index: ").map(|i| raw[i + 7..].split(' ').next().unwrap_or_default().to_string()).or_else(|| raw.find("unique constraint \"").map(|i| raw[i + 19..].split('"').next().unwrap_or_default().to_string()));
            ex.title = match &dup {
                Some((f, v)) => format!("Another document already has {f} {v}"),
                None => "Another document already has this unique value".into(),
            };
            ex.hint = match index {
                Some(i) => format!("The {i} index on {ns} allows each value once."),
                None => format!("A unique index on {ns} allows each value once."),
            };
            ex.dup = dup;
        }
        ErrKind::Validation => {
            ex.title = format!("The document breaks the validator of {ns}");
            ex.hint = "The collection's validation rules refused it. Open Validator to see the rules.".into();
            ex.fixes = vec![Fix::Validator];
        }
        ErrKind::Missing => {
            ex.title = format!("{ns} no longer exists");
            ex.hint = "It was dropped or renamed since the sidebar was loaded. Refresh the sidebar to see what is there now.".into();
            ex.fixes = vec![Fix::Refresh];
        }
        ErrKind::Syntax => {
            ex.title = if cx.what.is_empty() { "The server rejected the request".into() } else { format!("The server rejected {}", cx.what.trim_start_matches("run ")) };
            ex.hint = said.clone();
        }
        ErrKind::Other => {
            // Our own messages read "Could not write x: reason"; the part before the colon is the title.
            let first = raw.lines().next().unwrap_or(raw).trim();
            let ours = mongo.is_none() && my.is_none() && pg_code(raw).is_none();
            match (mongo.is_none() && my.is_none()).then(|| pg_wording(pg_code(raw).as_deref(), &said)).flatten().or_else(|| first.split_once(": ").filter(|(t, _)| ours && t.len() < 80 && !t.contains('{')).map(|(t, r)| (t.trim_end_matches('.').to_string(), sentence(r)))) {
                Some((t, r)) => (ex.title, ex.hint) = (t, r),
                None if ours && first.len() <= 120 => ex.title = first.trim_end_matches('.').to_string(),
                None => {
                    ex.title = if cx.what.is_empty() { "The request failed".into() } else { format!("Could not {}", cx.what) };
                    ex.hint = said.clone();
                }
            }
            // Advice we add on a second line, like the timeout note, stays visible.
            if let Some(more) = raw.lines().nth(1).filter(|l| !l.trim().is_empty() && ours) {
                ex.hint = if ex.hint.is_empty() { more.trim().to_string() } else { format!("{} {}", ex.hint, more.trim()) };
            }
        }
    }
    // A PostgreSQL error says where in the statement it stopped.
    if let Some(at) = pg_position(raw) {
        let note = format!("At position {at}.");
        ex.hint = if ex.hint.is_empty() { note } else { format!("{} {note}", ex.hint) };
    }
    if ex.hint == ex.title {
        ex.hint.clear();
    }
    ex
}

/// PostgreSQL's refusals of a written value, told apart by SQLSTATE, because the server's sentence follows its lc_messages
/// and reads in another language on many servers. The title says what is wrong in the user's terms and the hint keeps
/// the server's sentence, which names the value and the type.
fn pg_wording(code: Option<&str>, said: &str) -> Option<(String, String)> {
    let server = sentence(said.lines().next().unwrap_or(said));
    let (title, hint) = match code? {
        "22P02" => ("That value is not valid for the column's type", server),
        "22003" | "22001" => ("That value is too large for the column", server),
        "23502" => ("This column does not allow NULL", server),
        "23503" => ("Other rows depend on this value", server),
        "23514" => ("The value breaks a check constraint", server),
        "55P03" => ("Another session holds a lock on this table", format!("The write waited {} seconds for the lock and gave up. Nothing was changed. Try again when the other session is done.", super::sql::WRITE_LOCK_SECS)),
        "25006" => ("This connection is read-only", server),
        _ => return None,
    };
    Some((title.to_string(), hint))
}

/// PostgreSQL 55P03 (lock_not_available) for a table a structure change, trigger edit or create waited for. `secs` is the wait that
/// applied, which differs from a plain write's (`WRITE_LOCK_SECS`), and the wording stays here with the other server errors.
pub(crate) fn lock_held(name: &str, secs: u32) -> String {
    format!("The table {name} is locked by another session that did not let go within {secs} seconds. Nothing was changed.")
}

/// 55P03 for the drop that reads what a CASCADE takes.
pub(crate) fn lock_held_cascade(secs: u32) -> String {
    format!("Another session holds a lock on it or on something that depends on it, and did not let go within {secs} seconds, so what the drop takes cannot be read right now. Try again in a moment.")
}

/// What 55006 (object_in_use) is about, for the wording of `in_use`.
pub(crate) enum InUse<'a> {
    Template,
    Drop { name: &'a str, pooler: bool },
    Undo { name: &'a str },
}

/// PostgreSQL 55006 on a database or its template: `server` is the server's own text, and the rest says what Quarry did about it.
pub(crate) fn in_use(server: &str, what: InUse) -> String {
    match what {
        InUse::Template => format!("{server}\nAnother session is using the template database, and a CREATE DATABASE running at the same moment does. Try again in a moment."),
        InUse::Drop { name, pooler } => {
            let who = if pooler { "A connection pooler keeps a server connection open on it after Quarry's own are closed, or another session is using it" } else { "Another session or application is using it" };
            format!("{server}\nQuarry closed its own connections to {name} first. {who}. Nothing was dropped. WITH (FORCE) in the confirmation disconnects them.")
        }
        InUse::Undo { name } => format!("Not undone: another session is connected to {name}.\n{server}"),
    }
}

fn sentence(s: &str) -> String {
    let mut c = s.trim().chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_lock_timeout_says_the_wait_that_applied() {
        assert!(lock_held("\"s\".\"t\"", 3).contains("within 3 seconds") && lock_held("t", 3).contains("Nothing was changed"));
        assert!(lock_held_cascade(3).contains("3 seconds"));
        let write = explain("error returned from database: 55P03: canceling statement due to lock timeout", &cx());
        assert!(write.hint.contains(&format!("{} seconds", crate::db::sql::WRITE_LOCK_SECS)), "{}", write.hint);
    }

    #[test]
    fn an_object_in_use_keeps_the_server_text_and_says_what_was_done() {
        assert!(in_use("srv", InUse::Template).starts_with("srv\nAnother session is using the template"));
        assert!(in_use("srv", InUse::Drop { name: "d", pooler: true }).contains("connection pooler"));
        assert!(in_use("srv", InUse::Drop { name: "d", pooler: false }).contains("Another session or application"));
        assert!(in_use("srv", InUse::Undo { name: "d" }).starts_with("Not undone: another session is connected to d."));
    }

    use super::*;

    fn cx() -> ErrCtx {
        ErrCtx { what: "load the documents".into(), ns: "shop.orders".into(), host: "cluster0.mongodb.net".into() }
    }

    #[test]
    fn privilege_errors_name_the_command_place_and_role() {
        let e = explain(r#"Command failed: Error code 13 (Unauthorized): not authorized on shop to execute command { find: "orders", filter: {}, $db: "shop" }"#, &cx());
        assert_eq!(e.kind, ErrKind::Privilege);
        assert_eq!(e.title, "Your user can't read shop.orders");
        assert_eq!(e.need, Some(("find on shop.orders".into(), Some("read"))));
        assert_eq!(e.code.as_deref(), Some("13 Unauthorized"));
        let atlas = explain("Command failed: Error code 8000 (AtlasError): user is not allowed to do action [serverStatus] on [admin.]", &ErrCtx::default());
        assert_eq!(atlas.title, "Your user can't run serverStatus on admin");
        assert_eq!(atlas.need.unwrap().1, Some("clusterMonitor"));
        let my = explain("error returned from database: 1142 (42000): SELECT command denied to user 'ro'@'%' for table 'orders'", &cx());
        assert_eq!((my.kind, my.title.as_str()), (ErrKind::Privilege, "Your user can't read orders"));
        assert!(!is_unauthorized("Command failed: Error code 76 (NoReplicationEnabled): not running with --replSet"));
    }

    #[test]
    fn connection_failures_are_told_apart() {
        assert_eq!(explain("Command failed: Error code 18 (AuthenticationFailed): Authentication failed.", &cx()).kind, ErrKind::SignIn);
        assert_eq!(explain("error returned from database: 1045 (28000): Access denied for user 'root'@'172.17.0.1' (using password: YES)", &cx()).kind, ErrKind::SignIn);
        assert_eq!(explain("Server selection timeout: No available servers. Topology: { Type: Unknown, Servers: [ { Address: localhost:27017, Type: Unknown, Error: Kind: I/O error: Connection refused (os error 61) } ] }", &cx()).kind, ErrKind::Unreachable);
        assert_eq!(explain("I/O error: invalid peer certificate: UnknownIssuer", &cx()).kind, ErrKind::Tls);
        assert_eq!(explain("I/O error: Connection reset by peer (os error 54)", &cx()).kind, ErrKind::Lost);
        let t = explain("Command failed: Error code 50 (MaxTimeMSExpired): operation exceeded time limit\nThis usually means a query on a non-indexed field.", &cx());
        assert_eq!((t.kind, t.fixes.contains(&Fix::RaiseLimit)), (ErrKind::Timeout, true));
    }

    #[test]
    fn duplicates_show_the_field_and_value() {
        let e = explain(r#"An error occurred when trying to execute a write operation: WriteError(WriteError { code: 11000, code_name: None, message: "E11000 duplicate key error collection: shop.orders index: number_1 dup key: { number: 4410 }", details: None })"#, &cx());
        assert_eq!(e.title, "Another document already has number 4410");
        assert_eq!(e.hint, "The number_1 index on shop.orders allows each value once.");
        assert_eq!(e.dup, Some(("number".into(), "4410".into())));
        let s = explain(r#"WriteError { code: 11000, message: "E11000 duplicate key error collection: shop.users index: email_1 dup key: { email: \"a@b.c\" }" }"#, &cx());
        assert_eq!(s.dup, Some(("email".into(), "\"a@b.c\"".into())));
        let my = explain("error returned from database: 1062 (23000): Duplicate entry '4410' for key 'orders.number'", &cx());
        assert_eq!(my.dup, Some(("number".into(), "4410".into())));
    }

    #[test]
    fn own_messages_split_into_title_and_hint() {
        let e = explain("Could not write /tmp/a.csv: No space left on device (os error 28)", &ErrCtx::default());
        assert_eq!((e.title.as_str(), e.hint.as_str()), ("Could not write /tmp/a.csv", "No space left on device (os error 28)"));
        let e = explain("That change is no longer in Recent changes.", &ErrCtx::default());
        assert_eq!((e.title.as_str(), e.hint.as_str()), ("That change is no longer in Recent changes", ""));
        let e = explain("Command failed: Error code 9 (FailedToParse): $lookup requires 'from' to be specified", &ErrCtx { what: "run the pipeline".into(), ..cx() });
        assert_eq!((e.kind, e.title.as_str(), e.hint.as_str()), (ErrKind::Syntax, "The server rejected the pipeline", "$lookup requires 'from' to be specified"));
    }

    #[test]
    fn cloud_sql_failures_get_the_card_titles() {
        let t = |raw: &str| {
            let e = explain(raw, &cx());
            assert_eq!(e.kind, ErrKind::CloudSql, "{raw}");
            e.title
        };
        assert_eq!(t("Cloud SQL proxy not found. Download it from the connection's error card or the Cloud SQL browser, or set its path in Settings."), "Cloud SQL Auth Proxy not found");
        assert_eq!(t("Cloud SQL proxy failed: failed to get instance metadata: googleapi: Error 403: not authorized"), "Cloud SQL proxy failed");
        assert_eq!(t("Cloud SQL proxy did not accept connections within 20 s"), "Cloud SQL proxy failed");
        assert_eq!(t("Cloud SQL: gcloud is signed out. Run `gcloud auth login` and try again."), "Sign in to Google Cloud first");
        assert_eq!(t("Cloud SQL: gcloud did not answer within 30 s and was stopped."), "gcloud did not answer");
        // IAM never uses the gcloud account, so both IAM cards point at Application Default Credentials.
        let adc = explain(&format!("{}. Run `gcloud auth application-default login`, then Retry.", crate::cloudsql::ADC_MISSING), &cx());
        assert_eq!((adc.kind, adc.title.as_str()), (ErrKind::CloudSql, "IAM sign-in needs Application Default Credentials"));
        assert!(adc.hint.contains("gcloud auth application-default login") && adc.hint.contains("then Retry"));
        let refused = explain("Cloud SQL IAM sign-in refused (the proxy used Application Default Credentials): error returned from database: 28P01: password authentication failed", &cx());
        assert_eq!(refused.title, "Cloud SQL refused the IAM sign-in");
        assert!(refused.hint.contains("Application Default Credentials") && refused.hint.contains("gcloud auth application-default login"));
    }

    #[test]
    fn postgres_errors_are_told_apart_by_sqlstate() {
        let e = explain("error returned from database: 23505: duplicate key value violates unique constraint \"users_email_key\"\nDetail: Key (email)=(a@b.c) already exists.", &cx());
        assert_eq!((e.kind, e.code.as_deref()), (ErrKind::Duplicate, Some("23505")));
        assert_eq!(e.dup, Some(("email".into(), "a@b.c".into())));
        assert!(e.hint.contains("users_email_key"));
        let e = explain("error returned from database: 42601: syntax error at or near \"FORM\"\nPosition: 10 (line 1, column 10)", &cx());
        assert_eq!((e.kind, e.hint.as_str()), (ErrKind::Syntax, "syntax error at or near \"FORM\" At position 10 (line 1, column 10)."));
        let e = explain("error returned from database: 42501: permission denied for table orders", &cx());
        assert_eq!((e.kind, e.title.as_str()), (ErrKind::Privilege, "Your user can't do this on orders"));
        assert_eq!(explain("error returned from database: 42P01: relation \"x\" does not exist", &cx()).kind, ErrKind::Missing);
        assert_eq!(explain("error returned from database: 57014: canceling statement due to statement timeout", &cx()).kind, ErrKind::Timeout);
        assert_eq!(explain(&format!("error returned from database: 57014: storniere Anfrage auf Benutzerwunsch{STOP_MARK}"), &cx()).kind, ErrKind::Other);
        // Whatever path surfaced the cancelled statement, `stopped` makes it a stop, once, and leaves other errors alone.
        let cancelled = "error returned from database: 57014: canceling statement due to user request".to_string();
        assert_eq!(classify(&stopped(cancelled.clone())), ErrKind::Other);
        assert_eq!(stopped(stopped(cancelled)).matches("Stopped by you").count(), 1);
        assert_eq!(stopped("error returned from database: 23505: duplicate key".into()), "error returned from database: 23505: duplicate key");
        assert_eq!(explain("error returned from database: 28P01: password authentication failed for user \"x\"", &cx()).kind, ErrKind::SignIn);
        assert_eq!(explain("error returned from database: 08006: connection failure", &cx()).kind, ErrKind::Lost);
        // A MySQL number is not read as a SQLSTATE.
        assert_eq!(pg_code("error returned from database: 1062 (23000): Duplicate entry"), None);
        assert_eq!(server_text("error returned from database: 42P01: relation \"x\" does not exist\nPosition: 3"), "relation \"x\" does not exist");
    }

    #[test]
    fn postgres_errors_of_content_reads_and_grid_writes_keep_their_kind() {
        // The texts `db::sql::se` makes for a Content read and a grid write, which carry the SQLSTATE and no position.
        let e = explain("error returned from database: 23505: duplicate key value violates unique constraint \"t_pkey\"\nDetail: Key (id)=(1) already exists.", &cx());
        assert_eq!((e.kind, e.code.as_deref()), (ErrKind::Duplicate, Some("23505")));
        let e = explain("error returned from database: 57014: canceling statement due to statement timeout", &ErrCtx { what: "read the rows".into(), ..cx() });
        assert_eq!(e.kind, ErrKind::Timeout);
        assert!(e.title.contains("time limit") && !e.hint.contains("non-indexed") && !e.hint.contains("unindexed field"), "{e:?}");
    }

    #[test]
    fn mysql_errors_do_not_get_postgres_wording() {
        let e = explain("error returned from database: 1690 (22003): BIGINT UNSIGNED value is out of range in '(`s`.`t`.`a` - 1)'", &cx());
        assert_eq!(e.title, "Could not load the documents");
        assert!(e.hint.starts_with("BIGINT UNSIGNED value is out of range"), "{e:?}");
    }

    #[test]
    fn postgres_write_refusals_are_explained() {
        let ex = |raw: &str| explain(raw, &cx());
        let e = ex("error returned from database: 22P02: invalid input syntax for type integer: \"abc\"");
        assert_eq!((e.title.as_str(), e.hint.as_str()), ("That value is not valid for the column's type", "Invalid input syntax for type integer: \"abc\""));
        assert_eq!(ex("error returned from database: 22P02: invalid input value for enum mood: \"furious\"").title, "That value is not valid for the column's type");
        assert_eq!(ex("error returned from database: 22003: value \"99999999999\" is out of range for type integer").title, "That value is too large for the column");
        assert_eq!(ex("error returned from database: 22001: value too long for type character varying(3)").title, "That value is too large for the column");
        assert_eq!(ex("error returned from database: 23502: null value in column \"n\" of relation \"t\" violates not-null constraint").title, "This column does not allow NULL");
        assert_eq!(ex("error returned from database: 23503: update or delete on table \"p\" violates foreign key constraint \"fk\" on table \"c\"").title, "Other rows depend on this value");
        assert_eq!(ex("error returned from database: 23514: new row for relation \"t\" violates check constraint \"c\"").title, "The value breaks a check constraint");
        let lock = ex("error returned from database: 55P03: canceling statement due to lock timeout");
        assert_eq!(lock.title, "Another session holds a lock on this table");
        assert!(lock.hint.contains("Nothing was changed") && !lock.hint.contains("index"));
        assert_eq!(ex("error returned from database: 25006: cannot execute UPDATE in a read-only transaction").title, "This connection is read-only");
        // A sentence without a SQLSTATE is not guessed at from its words.
        assert_ne!(ex("error returned from database: cannot execute UPDATE in a read-only transaction").title, "This connection is read-only");
    }

    #[test]
    fn postgres_errors_are_told_apart_by_sqlstate_in_any_language() {
        let ex = |raw: &str| explain(raw, &cx());
        // The sentences of a server whose lc_messages is German or French: nothing of the English wording is left to match.
        assert_eq!(ex("error returned from database: 22P02: ung\u{fc}ltige Eingabesyntax f\u{fc}r Typ integer: \"abc\"").title, "That value is not valid for the column's type");
        assert_eq!(ex("error returned from database: 22003: Wert \"99999999999\" ist au\u{df}erhalb des g\u{fc}ltigen Bereichs f\u{fc}r Typ integer").title, "That value is too large for the column");
        assert_eq!(ex("error returned from database: 23502: NULL-Wert in Spalte \"n\" verletzt Not-Null-Constraint").title, "This column does not allow NULL");
        assert_eq!(ex("error returned from database: 23503: la mise \u{e0} jour viole la contrainte de cl\u{e9} \u{e9}trang\u{e8}re \"fk\"").title, "Other rows depend on this value");
        assert_eq!(ex("error returned from database: 23514: la nouvelle ligne viole la contrainte de v\u{e9}rification \"c\"").title, "The value breaks a check constraint");
        assert_eq!(ex("error returned from database: 55P03: storniere Anfrage wegen Sperr-Timeout").title, "Another session holds a lock on this table");
        assert_eq!(ex("error returned from database: 25006: kann UPDATE in einer Read-Only-Transaktion nicht ausf\u{fc}hren").title, "This connection is read-only");
        let t = ex("error returned from database: 57014: storniere Anfrage wegen Statement-Timeout");
        assert_eq!(t.kind, ErrKind::Timeout);
        assert_eq!(classify("error returned from database: 57014: annulation de la requ\u{ea}te \u{e0} cause du d\u{e9}lai d'expiration"), ErrKind::Timeout);
    }

    #[test]
    fn copied_text_hides_passwords() {
        assert_eq!(scrub_uri("failed for mongodb://ana:s3cret@h1:27017,h2/db?x=1 now"), "failed for mongodb://ana:****@h1:27017,h2/db?x=1 now");
        assert_eq!(scrub_uri("mysql://root@h:3306"), "mysql://root@h:3306");
    }
}
