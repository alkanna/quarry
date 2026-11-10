//! Plain-language versions of server and driver errors: a title, a hint and the fixes to offer.
//! The raw text is never replaced, only explained; callers keep it for Server message and Copy details.

/// What went wrong, as far as the text tells.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrKind {
    Privilege,
    SignIn,
    Unreachable,
    Tunnel,
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

/// What went wrong, without the wording: cheap enough for a label drawn every frame.
pub fn classify(raw: &str) -> ErrKind {
    let low = raw.to_lowercase();
    let n = mongo_code(raw).map(|m| m.0).or(mysql_code(raw));
    let pg = pg_code(raw);
    let pg = pg.as_deref();
    if is_unauthorized(raw) {
        ErrKind::Privilege
    } else if matches!(n, Some(18 | 1045 | 1698)) || matches!(pg, Some("28P01" | "28000")) || low.contains("authentication failed") || low.contains("scram failure") {
        ErrKind::SignIn
    } else if low.contains("ssh") && (low.contains("tunnel") || low.contains("handshake") || low.contains("ssh:")) {
        ErrKind::Tunnel
    } else if low.contains("certificate") || low.contains("tls") || low.contains("ssl") || n == Some(2026) {
        ErrKind::Tls
    } else if matches!(n, Some(50 | 262 | 3024 | 1205 | 1969)) || low.contains("exceeded time limit") || low.contains("maximum statement execution time") || pg == Some("57014") && low.contains("statement timeout") {
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
            ex.hint = "The server stopped it. A filter on an unindexed field scans every document; Explain shows whether an index is used.".into();
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
            match first.split_once(": ").filter(|(t, _)| ours && t.len() < 80 && !t.contains('{')) {
                Some((t, r)) => (ex.title, ex.hint) = (t.trim_end_matches('.').to_string(), sentence(r)),
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

fn sentence(s: &str) -> String {
    let mut c = s.trim().chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(explain("error returned from database: 57014: canceling statement due to user request", &cx()).kind, ErrKind::Other);
        assert_eq!(explain("error returned from database: 28P01: password authentication failed for user \"x\"", &cx()).kind, ErrKind::SignIn);
        assert_eq!(explain("error returned from database: 08006: connection failure", &cx()).kind, ErrKind::Lost);
        // A MySQL number is not read as a SQLSTATE.
        assert_eq!(pg_code("error returned from database: 1062 (23000): Duplicate entry"), None);
        assert_eq!(server_text("error returned from database: 42P01: relation \"x\" does not exist\nPosition: 3"), "relation \"x\" does not exist");
    }

    #[test]
    fn copied_text_hides_passwords() {
        assert_eq!(scrub_uri("failed for mongodb://ana:s3cret@h1:27017,h2/db?x=1 now"), "failed for mongodb://ana:****@h1:27017,h2/db?x=1 now");
        assert_eq!(scrub_uri("mysql://root@h:3306"), "mysql://root@h:3306");
    }
}
