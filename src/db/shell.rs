//! The JS shell: package specs, the bun runner, and parsing mongosh-style commands.

use super::*;

pub struct ShellJob {
    pub runtime: std::path::PathBuf,
    /// Folder holding the runner script and its installed MongoDB driver.
    pub dir: std::path::PathBuf,
    /// First line for the runner: code, packages, timeoutMs, main, mongo entries and sql entries (see shell_runner.mjs).
    pub input: String,
    /// Extra npm packages the person added ("name" or "name@version").
    pub packages: Vec<String>,
    /// The SQL connections of the script by their name in the input. Their queries are answered here.
    pub sql: Vec<(String, SqlRelay)>,
}

/// Where a SQL script's queries run. The runner never gets a driver or a password.
pub struct SqlRelay {
    pub db: sql::Db,
    pub schema: String,
    pub read_only: bool,
    /// Server connection id of the script, set once it opens so Stop can interrupt the statement.
    pub conn_id: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

/// One query a script ran, for the Queries panel.
#[derive(Clone)]
pub struct ShellQuery {
    /// Name of the connection the query ran on.
    pub conn: String,
    pub text: String,
    /// The values, shortened, as the script passed them.
    pub params: String,
    pub rows: Option<usize>,
    pub affected: Option<u64>,
    pub error: Option<String>,
    pub ms: u128,
}

/// Splits "name@version" (scoped names keep their leading @).
pub fn package_spec(p: &str) -> (String, String) {
    match p.rfind('@') {
        Some(i) if i > 0 => (p[..i].to_string(), p[i + 1..].to_string()),
        _ => (p.to_string(), "latest".to_string()),
    }
}

/// The Shell runner itself needs these, so their versions are not the person's to change.
pub const PINNED_PACKAGES: [&str; 3] = ["mongodb", "socks", "tsrs"];

/// Only for editor autocomplete: tsrs is the TypeScript language server, and lodash ships no types of its own.
pub const EDITOR_PACKAGES: [(&str, &str); 2] = [("tsrs", "0.23.0-ts7.1.0-beta"), ("@types/lodash", "^4.17.20")];

/// npm package names: lowercase letters, digits, - . _ ~ and one optional @scope/.
pub fn valid_package(p: &str) -> bool {
    let (name, version) = package_spec(p);
    // npm forbids a leading . or _, which also keeps "." and ".." from naming a folder outside node_modules.
    let ok = |s: &str| !s.is_empty() && !s.starts_with(['.', '_']) && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-._~".contains(c));
    let name_ok = match name.strip_prefix('@').and_then(|r| r.split_once('/')) {
        Some((scope, n)) => ok(scope) && ok(n),
        None => !name.starts_with('@') && ok(&name),
    };
    // A version starting with . is read by the installer as a local folder, not a registry version.
    let version_ok = !version.is_empty() && !version.starts_with('.') && version.chars().all(|c| c.is_ascii_alphanumeric() || "-._^~<>=*|".contains(c));
    name_ok && version_ok && !PINNED_PACKAGES.contains(&name.as_str())
}

/// Always available in the Shell, like the original NodeShell's context (renderer ~131560:
/// db, ObjectId, dayjs, _, console, log, randomId, UUIDv1, UUIDv4). Versions follow the
/// original's package.json where it lists them (lodash ^4.17.21, dayjs ^1.11.13).
pub const SHELL_BUILTINS: [(&str, &str); 4] = [("lodash", "^4.17.21"), ("dayjs", "^1.11.13"), ("uuid", "^11.0.0"), ("socks", "^2.8.7")];

pub(crate) fn runner_package(packages: &[String]) -> String {
    let mut deps = serde_json::Map::new();
    deps.insert("mongodb".into(), "^6.10.0".into());
    for (name, version) in SHELL_BUILTINS.iter().chain(&EDITOR_PACKAGES) {
        deps.insert(name.to_string(), version.to_string().into());
    }
    for p in packages.iter().filter(|p| valid_package(p)) {
        let (name, version) = package_spec(p);
        deps.insert(name, version.into());
    }
    serde_json::json!({ "name": "quarry-shell", "private": true, "type": "module", "dependencies": deps }).to_string()
}

pub(crate) const RUNNER: &str = include_str!("shell_runner.mjs");

/// Lets the editor's language server resolve the shell's packages and their types.
const TSCONFIG: &str = r#"{"compilerOptions":{"allowJs":true,"checkJs":false,"noEmit":true,"target":"es2022","module":"esnext","moduleResolution":"bundler","lib":["es2022"],"types":[]},"files":["__quarry_globals.d.ts","__quarry_script.js"]}"#;

/// Writes the runner and its package.json, then installs packages when they changed or are missing.
/// The Shell and the editor's language server both call this, so one install runs at a time.
pub(crate) async fn prepare_shell_dir(runtime: &std::path::Path, dir: &std::path::Path, packages: &[String], note: &(impl Fn(&str) + Sync)) -> Res<()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = match LOCK.try_lock() {
        Ok(g) => g,
        Err(_) => {
            // The editor's language server may be installing packages; say why the run waits.
            note("Waiting for the package install to finish…");
            LOCK.lock().await
        }
    };
    std::fs::create_dir_all(dir).map_err(e)?;
    let package = runner_package(packages);
    let mut deps_changed = false;
    for (name, body) in [("runner.mjs", RUNNER), ("package.json", package.as_str()), ("tsconfig.json", TSCONFIG)] {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(body) {
            std::fs::write(&path, body).map_err(e)?;
            deps_changed |= name == "package.json";
        }
    }
    let modules = dir.join("node_modules");
    let missing = !modules.join("mongodb").exists()
        || SHELL_BUILTINS.iter().chain(&EDITOR_PACKAGES).any(|(n, _)| !modules.join(n).exists())
        || packages.iter().any(|p| !modules.join(package_spec(p).0).exists());
    if deps_changed || missing {
        note("Installing packages for the shell with Bun…");
        let mut cmd = tokio::process::Command::new(runtime);
        cmd.arg("install").current_dir(dir).kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(crate::update::CREATE_NO_WINDOW);
        let out = cmd.output().await.map_err(|err| format!("Could not start Bun ({}): {err}", runtime.display()))?;
        if !out.status.success() {
            return Err(format!("bun install failed: {}", String::from_utf8_lossy(&out.stderr)));
        }
        note("Packages ready.");
    }
    Ok(())
}

pub(crate) async fn run_shell(req: u64, job: &ShellJob, send: &(impl Fn(Msg) + Send + Sync), cancel: tokio::sync::oneshot::Receiver<()>) -> Res<String> {
    run_shell_gated(req, job, None, send, cancel).await
}

/// `run_shell` for an agent's script: `gate` checks each SQL statement and cleans each row and error before the script
/// gets them.
pub(crate) async fn run_shell_gated(req: u64, job: &ShellJob, gate: Option<&dyn sql::RelayGate>, send: &(impl Fn(Msg) + Send + Sync), mut cancel: tokio::sync::oneshot::Receiver<()>) -> Res<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::process::Command;
    if let Some(n) = repeated_name(job.sql.iter().map(|(n, _)| n.as_str())) {
        return Err(format!("The connection name \"{n}\" is used more than once in this script. Give each connection its own name."));
    }
    let note = |t: &str| send(Msg::ShellLog { req, level: "info".into(), text: t.into() });
    tokio::select! {
        r = prepare_shell_dir(&job.runtime, &job.dir, &job.packages, &note) => r?,
        _ = &mut cancel => return Err("Stopped.".into()),
    }
    let mut cmd = Command::new(&job.runtime);
    cmd.arg("runner.mjs")
        .current_dir(&job.dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(crate::update::CREATE_NO_WINDOW);
    let mut child = cmd
        .spawn()
        .map_err(|err| format!("Could not start Bun ({}): {err}", job.runtime.display()))?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    stdin.write_all(job.input.as_bytes()).await.map_err(e)?;
    // The runner reads one line and then the answers to SQL queries, so stdin stays open until the result arrives.
    stdin.write_all(b"\n").await.map_err(e)?;
    let mut stdin = Some(stdin);
    let mut lines = tokio::io::BufReader::new(child.stdout.take().ok_or("no stdout")?);
    let mut stderr = child.stderr.take().ok_or("no stderr")?;
    // Read alongside stdout: a full stderr pipe would block the child before stdout closes.
    let errs = tokio::spawn(async move {
        let mut err = String::new();
        let _ = stderr.read_to_string(&mut err).await;
        err
    });
    let read = async {
        let mut result: Option<Res<String>> = None;
        let mut relays: std::collections::HashMap<String, sql::Relay> = std::collections::HashMap::new();
        // Names whose relay was dropped after a cut result. A later query errors like it does on MySQL, where the relay
        // stays but is dead, instead of quietly opening a new session that has lost the script's transaction.
        let mut closed: std::collections::HashSet<String> = std::collections::HashSet::new();
        while let Some(line) = next_line(&mut lines).await? {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                send(Msg::ShellLog { req, level: "log".into(), text: line });
                continue;
            };
            let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
            match v.get("type").and_then(|t| t.as_str()) {
                Some("log") => send(Msg::ShellLog { req, level: s("level"), text: s("text") }),
                Some("result") => {
                    result = Some(if v.get("ok").and_then(|o| o.as_bool()) == Some(true) { Ok(s("ejson")) } else { Err(s("error")) });
                    // Closing the pipe lets the runner exit.
                    stdin = None;
                }
                Some("query") => {
                    // A command the runner's own MongoDB client ran.
                    let q = ShellQuery {
                        conn: s("conn"),
                        text: s("text"),
                        params: String::new(),
                        rows: v.get("rows").and_then(|x| x.as_u64()).map(|n| n as usize),
                        affected: v.get("affected").and_then(|x| x.as_u64()),
                        error: v.get("error").and_then(|x| x.as_str()).map(String::from),
                        ms: v.get("ms").and_then(|x| x.as_f64()).unwrap_or(0.0) as u128,
                    };
                    send(Msg::ShellQuery { req, q });
                }
                Some("sql") => {
                    let conn = s("conn");
                    let params = sql_args(v.get("params"), job.sql.iter().find(|(n, _)| *n == conn).map_or(sql::Dialect::MySql, |(_, c)| c.db.dialect));
                    let text = s("text");
                    let t = Instant::now();
                    let res = async {
                        let Some((_, cfg)) = job.sql.iter().find(|(n, _)| *n == conn) else { return Err(format!("There is no SQL connection named \"{conn}\" in this script.")) };
                        if closed.contains(&conn) {
                            return Err("The connection was closed after a result was cut. Start the script again.".to_string());
                        }
                        if !relays.contains_key(&conn) {
                            relays.insert(conn.clone(), sql::Relay::open(&cfg.db, &cfg.schema, cfg.read_only, &cfg.conn_id).await?);
                        }
                        let p = params.as_ref().map_err(|x| x.clone())?;
                        let Some(r) = relays.get_mut(&conn) else { return Err("No connection".to_string()) };
                        let out = r.query_args(&text, p, gate).await;
                        // Dropping the relay closes its connection, which ends the transaction and frees its locks.
                        if r.abandoned() {
                            relays.remove(&conn);
                            closed.insert(conn.clone());
                        }
                        out
                    }
                    .await;
                    let res = res.map_err(|x| match gate {
                        Some(g) => g.error(x),
                        None => x,
                    });
                    let mut q = ShellQuery { conn, text, params: params_summary(v.get("params")), rows: None, affected: None, error: None, ms: t.elapsed().as_millis() };
                    let mut answer = serde_json::json!({ "type": "sqlres", "id": v.get("id") });
                    match res {
                        Ok(sql::Answer::Rows(rows)) => {
                            q.rows = Some(rows.len());
                            answer["ok"] = true.into();
                            answer["rows"] = rows.into();
                        }
                        Ok(sql::Answer::Wrote { affected, insert_id }) => {
                            q.affected = Some(affected);
                            answer["ok"] = true.into();
                            answer["affected"] = affected.into();
                            // PostgreSQL has no generated key to report here, the runner leaves insertId undefined there.
                            if job.sql.iter().find(|(n, _)| *n == q.conn).is_some_and(|(_, c)| c.db.dialect != sql::Dialect::Postgres) {
                                answer["insertId"] = insert_id.into();
                            }
                        }
                        Err(err) => {
                            q.error = Some(err.clone());
                            answer["ok"] = false.into();
                            answer["error"] = err.into();
                        }
                    }
                    send(Msg::ShellQuery { req, q });
                    if let Some(w) = stdin.as_mut() {
                        let _ = w.write_all(format!("{answer}\n").as_bytes()).await;
                        let _ = w.flush().await;
                    }
                }
                _ => {}
            }
        }
        Ok::<_, String>(result)
    };
    tokio::select! {
        r = read => {
            // A read that failed leaves the runner blocked on a pipe nobody reads, so waiting would never end.
            if r.is_err() {
                let _ = child.kill().await;
            }
            let _ = child.wait().await;
            match r? {
                Some(res) => res,
                None => {
                    let err = errs.await.unwrap_or_default();
                    Err(if err.trim().is_empty() { "The shell ended without a result.".into() } else { err })
                }
            }
        }
        _ = cancel => {
            // The statement in flight keeps running on the server unless it is interrupted too.
            for (_, c) in &job.sql {
                let id = c.conn_id.load(std::sync::atomic::Ordering::SeqCst);
                // Behind a pooler the pid may now belong to another client's statement, so the connection is just closed.
                if id != 0 && !sql::Relay::pooled(&c.db) {
                    let _ = sql::Relay::kill(&c.db, id).await;
                }
            }
            let _ = child.kill().await;
            Err("Stopped.".into())
        }
    }
}

/// The first name that appears twice. Queries find their relay by name, so a repeat would answer on the wrong connection.
fn repeated_name<'a>(names: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let mut seen = std::collections::HashSet::new();
    names.into_iter().find(|n| !seen.insert(*n))
}

/// The longest line the runner may print. A script that prints more would otherwise grow this buffer until memory runs out.
const LINE_MAX: usize = 64 << 20;

/// The next line of the runner's stdout, ended by an error when it passes `LINE_MAX`.
async fn next_line(r: &mut tokio::io::BufReader<tokio::process::ChildStdout>) -> Res<Option<String>> {
    use tokio::io::AsyncBufReadExt;
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let chunk = r.fill_buf().await.map_err(e)?;
        if chunk.is_empty() {
            return Ok(if buf.is_empty() { None } else { Some(String::from_utf8_lossy(&buf).into_owned()) });
        }
        let (take, done) = match chunk.iter().position(|b| *b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        buf.extend_from_slice(&chunk[..take]);
        r.consume(take);
        if buf.len() > LINE_MAX {
            return Err(format!("The shell printed a line longer than {} MB and was stopped.", LINE_MAX >> 20));
        }
        if done {
            buf.pop();
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
            return Ok(Some(String::from_utf8_lossy(&buf).into_owned()));
        }
    }
}

/// The values a script sent with a query, as bind values. A `{ "$hex": ... }` object is bytes, any other
/// object or array is its JSON text, which is what a JSON column takes.
#[cfg(test)]
fn sql_params(v: Option<&serde_json::Value>) -> Res<Vec<sql::value::Param>> {
    let Some(serde_json::Value::Array(a)) = v else { return Ok(Vec::new()) };
    a.iter().map(sql_param).collect()
}

/// `sql_params` for the relay: on PostgreSQL a `{ "$bool": true }` object is a boolean, which only that runner sends. On
/// MySQL the same JSON is an ordinary value and keeps the reading it always had.
fn sql_args(v: Option<&serde_json::Value>, d: sql::Dialect) -> Res<Vec<sql::Arg>> {
    let Some(serde_json::Value::Array(a)) = v else { return Ok(Vec::new()) };
    a.iter()
        .map(|x| match x.as_object().filter(|o| d == sql::Dialect::Postgres && o.len() == 1).and_then(|o| o.get("$bool")) {
            Some(serde_json::Value::Bool(b)) => Ok(sql::Arg::Bool(*b)),
            _ => sql_param(x).map(sql::Arg::Val),
        })
        .collect()
}

fn sql_param(x: &serde_json::Value) -> Res<sql::value::Param> {
    use serde_json::Value;
    use sql::value::Param;
    Ok(match x {
        Value::Null => Param::Null,
        Value::Bool(b) => Param::Int(*b as i64),
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => Param::Int(i),
            (_, Some(u)) => Param::UInt(u),
            _ => Param::Float(n.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(s) => Param::Text(s.clone()),
        // A BigInt beyond 2^53, which a JSON number would round.
        Value::Object(o) if o.len() == 1 && o.contains_key("$int") => {
            let t = o["$int"].as_str().unwrap_or_default();
            match (t.parse::<i64>(), t.parse::<u64>()) {
                (Ok(i), _) => Param::Int(i),
                (_, Ok(u)) => Param::UInt(u),
                _ => return Err("A BigInt value does not fit in 64 bits".to_string()),
            }
        }
        Value::Object(o) if o.len() == 1 && o.contains_key("$hex") => {
            let h = o["$hex"].as_str().unwrap_or_default();
            Param::Bytes((0..h.len() / 2).map(|i| u8::from_str_radix(h.get(i * 2..i * 2 + 2).unwrap_or("zz"), 16)).collect::<Result<_, _>>().map_err(|_| "Bad binary value".to_string())?)
        }
        other => Param::Text(other.to_string()),
    })
}

#[cfg(test)]
pub(crate) fn sql_params_for_test(v: &serde_json::Value) -> Res<Vec<sql::value::Param>> {
    sql_params(Some(v))
}

/// The values of a query for the Queries panel, cut to one short line.
fn params_summary(v: Option<&serde_json::Value>) -> String {
    let s = v.filter(|v| v.as_array().is_some_and(|a| !a.is_empty())).map(|v| v.to_string()).unwrap_or_default();
    if s.chars().count() > 80 { format!("{}…", s.chars().take(80).collect::<String>()) } else { s }
}

/// Readable shell syntax for a stage body: short values stay on one line, longer objects and
/// arrays get one entry per line.
pub fn pretty_shell(b: &Bson) -> String {
    fn at(b: &Bson, level: usize) -> String {
        let flat = shell_text(b);
        if flat.len() <= 60 {
            return flat;
        }
        let (pad, end) = ("  ".repeat(level + 1), "  ".repeat(level));
        match b {
            Bson::Document(d) => format!("{{\n{}\n{end}}}", d.iter().map(|(k, v)| format!("{pad}{}: {}", shell_key(k), at(v, level + 1))).collect::<Vec<_>>().join(",\n")),
            Bson::Array(a) => format!("[\n{}\n{end}]", a.iter().map(|v| format!("{pad}{}", at(v, level + 1))).collect::<Vec<_>>().join(",\n")),
            _ => flat,
        }
    }
    at(b, 0)
}

// ponytail: made once per Shell run, so the copy is cheap; boxing the large variant would change every match on it.
#[allow(clippy::large_enum_variant)]
pub enum ShellKind {
    Find(Query),
    Aggregate(Vec<Document>),
}

pub struct Shell {
    pub coll: String,
    pub kind: ShellKind,
}

pub fn is_shell(text: &str) -> bool {
    text.trim_start().starts_with("db.")
}

/// Text inside the parentheses that `s` starts after, and what follows the closing one.
pub(crate) fn take_args(s: &str) -> Res<(&str, &str)> {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut esc = false;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' if depth == 0 => return Ok((&s[..i], &s[i + 1..])),
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    Err("A parenthesis is not closed".into())
}

pub(crate) fn as_u64(b: &Bson) -> Res<u64> {
    match b {
        Bson::Int32(n) if *n >= 0 => Ok(*n as u64),
        Bson::Int64(n) if *n >= 0 => Ok(*n as u64),
        Bson::Double(n) if *n >= 0.0 => Ok(*n as u64),
        _ => Err("skip and limit need a positive number".into()),
    }
}

/// Understands `db.coll.find(filter, projection).sort().skip().limit().hint().collation()`, `findOne`,
/// `countDocuments`, `aggregate([...])` and `db.getCollection("name")...`.
pub fn parse_shell(src: &str) -> Res<Shell> {
    let s = src.trim().trim_end_matches(';').trim();
    let rest = s.strip_prefix("db.").ok_or("Shell queries start with db.")?;
    let (coll, mut rest) = if let Some(r) = rest.strip_prefix("getCollection(") {
        let (arg, r) = take_args(r)?;
        (arg.trim().trim_matches(|c| c == '"' || c == '\'').to_string(), r)
    } else {
        let end = rest.find('.').ok_or("Expected db.<collection>.<method>(...)")?;
        (rest[..end].trim().to_string(), &rest[end..])
    };
    let mut calls = Vec::new();
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let r = rest.strip_prefix('.').ok_or_else(|| format!("Unexpected text: {}", rest.chars().take(20).collect::<String>()))?;
        let open = r.find('(').ok_or("Expected a method call like .find(...)")?;
        let (args, after) = take_args(&r[open + 1..])?;
        calls.push((r[..open].trim().to_string(), args.to_string()));
        rest = after;
    }
    let args = |a: &str| -> Res<Vec<Bson>> {
        match parse_value(&format!("[{a}]"))? {
            Bson::Array(v) => Ok(v),
            _ => unreachable!(),
        }
    };
    let as_doc = |b: Option<&Bson>| -> Res<Option<Document>> {
        match b {
            None => Ok(None),
            Some(Bson::Document(d)) => Ok(Some(d.clone())),
            Some(_) => Err("Expected an object".into()),
        }
    };
    let mut it = calls.into_iter();
    let (method, a) = it.next().ok_or("Missing a method like .find() or .aggregate()")?;
    let kind = match method.as_str() {
        "aggregate" => {
            let v = args(&a)?;
            let stages = match v.as_slice() {
                [Bson::Array(st)] => st.clone(),
                _ => v,
            };
            let p = stages
                .into_iter()
                .map(|s| match s {
                    Bson::Document(d) => Ok(d),
                    _ => Err("Every pipeline stage must be an object".to_string()),
                })
                .collect::<Res<Vec<_>>>()?;
            ShellKind::Aggregate(p)
        }
        "find" | "findOne" | "count" | "countDocuments" => {
            let v = args(&a)?;
            let mut q = Query {
                filter: as_doc(v.first())?.unwrap_or_default(),
                projection: as_doc(v.get(1))?.filter(|p| !p.is_empty()),
                limit: (method == "findOne").then_some(1),
                ..Default::default()
            };
            for (m, a) in it.by_ref() {
                let v = args(&a)?;
                match m.as_str() {
                    "sort" => q.sort = as_doc(v.first())?,
                    "projection" | "project" => q.projection = as_doc(v.first())?,
                    "skip" => q.skip = as_u64(v.first().ok_or("skip needs a number")?)?,
                    "limit" => q.limit = Some(as_u64(v.first().ok_or("limit needs a number")?)?).filter(|l| *l > 0),
                    "hint" => match v.first() {
                        Some(h @ (Bson::Document(_) | Bson::String(_))) => q.hint = Some(h.clone()),
                        _ => return Err("hint needs index keys like { a: 1 } or an index name".into()),
                    },
                    "collation" => q.collation = as_doc(v.first())?,
                    "toArray" | "pretty" => {}
                    other => return Err(format!(".{other}() is not supported here")),
                }
            }
            ShellKind::Find(q)
        }
        other => return Err(format!("db.{coll}.{other}() is not supported. Use find, findOne, countDocuments or aggregate.")),
    };
    if let Some((m, _)) = it.find(|(m, _)| m != "toArray" && m != "pretty") {
        return Err(format!(".{m}() is not supported after this method"));
    }
    Ok(Shell { coll, kind })
}

/// The original's randomId (short-unique-id, "alphanum" dictionary): digits and letters of both cases.
pub fn random_id(len: usize) -> String {
    const ALPHANUM: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEED: AtomicU64 = AtomicU64::new(0);
    let mut x = SEED.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed)
        ^ std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1)
        | 1;
    (0..len)
        .map(|_| {
            // xorshift64: ids only need to be unlikely to repeat, not secret.
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            ALPHANUM[(x % ALPHANUM.len() as u64) as usize] as char
        })
        .collect()
}

/// A key as written in shell syntax: bare when it is a plain identifier, quoted otherwise.
pub(crate) fn shell_key(k: &str) -> String {
    if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$') && !k.starts_with(|c: char| c.is_ascii_digit()) {
        k.to_string()
    } else {
        serde_json::Value::String(k.into()).to_string()
    }
}

/// Readable shell syntax that `parse_value` reads back, e.g. `{ _id: ObjectId("...") }`.
pub fn shell_text(b: &Bson) -> String {
    let key = shell_key;
    match b {
        Bson::ObjectId(o) => format!("ObjectId(\"{}\")", o.to_hex()),
        Bson::DateTime(t) => format!("ISODate(\"{}\")", t.try_to_rfc3339_string().unwrap_or_default()),
        Bson::Int64(n) => format!("NumberLong({n})"),
        Bson::Decimal128(d) => format!("NumberDecimal(\"{d}\")"),
        Bson::Document(d) if d.is_empty() => "{}".into(),
        Bson::Document(d) => format!("{{ {} }}", d.iter().map(|(k, v)| format!("{}: {}", key(k), shell_text(v))).collect::<Vec<_>>().join(", ")),
        Bson::Array(a) => format!("[{}]", a.iter().map(shell_text).collect::<Vec<_>>().join(", ")),
        other => compact(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bool_marker_is_read_on_postgres_only() {
        let v = serde_json::json!([{ "$bool": true }, 3]);
        assert_eq!(sql_args(Some(&v), sql::Dialect::Postgres).unwrap()[0], sql::Arg::Bool(true));
        // On MySQL the same object is a plain value, as it was before PostgreSQL support.
        assert_eq!(sql_args(Some(&v), sql::Dialect::MySql).unwrap()[0], sql::Arg::Val(sql_param(&v[0]).unwrap()));
    }

    #[test]
    fn a_repeated_connection_name_is_found() {
        assert_eq!(repeated_name(["shop", "other", "shop"].into_iter()), Some("shop"));
        assert_eq!(repeated_name(["shop", "other"].into_iter()), None);
    }

    #[test]
    fn package_validation() {
        assert!(valid_package("lodash@4.17.21"));
        assert!(valid_package("@faker-js/faker"));
        assert!(valid_package("dayjs@^1.11"));
        assert!(!valid_package(".."));
        assert!(!valid_package("@../x"));
        assert!(!valid_package("lodash@.."));
        assert!(!valid_package("mongodb@5.0.0"));
        assert!(!valid_package("socks"));
        let deps: serde_json::Value = serde_json::from_str(&runner_package(&["mongodb@1.0.0".into(), "..".into()])).unwrap();
        assert_eq!(deps["dependencies"]["mongodb"], "^6.10.0");
        assert!(deps["dependencies"].get("..").is_none());
    }

    #[test]
    fn shell_queries() {
        let s = parse_shell(
            r#"db.approvals.aggregate([
  { $match: { travelID: { $gt: "" } } },
  { $group: { _id: "$travelID", n: { $sum: 1 }, ids: { $push: "$_id" }, statuses: { $push: "$status" } } },
  { $match: { n: { $gt: 1 } } }
])"#,
        )
        .unwrap();
        assert_eq!(s.coll, "approvals");
        assert!(matches!(s.kind, ShellKind::Aggregate(ref p) if p.len() == 3));
        let s = parse_shell("db.getCollection('a.b').find({x: 'y)'}, {x: 1}).sort({_id: -1}).skip(5).limit(10);").unwrap();
        assert_eq!(s.coll, "a.b");
        let ShellKind::Find(q) = s.kind else { panic!() };
        assert_eq!((q.skip, q.limit, q.filter.get_str("x").unwrap()), (5, Some(10), "y)"));
        assert!(q.sort.is_some() && q.projection.is_some());
        let ShellKind::Find(q) = parse_shell("db.a.find({}).hint('a_1').collation({ locale: 'fr' })").unwrap().kind else { panic!() };
        assert_eq!((q.hint, q.collation), (Some(Bson::String("a_1".into())), Some(doc! { "locale": "fr" })));
        assert!(parse_shell("db.x.drop()").is_err());
    }

    #[test]
    fn random_ids() {
        let a = random_id(17);
        assert_eq!(a.len(), 17);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(a, random_id(17));
    }
}
