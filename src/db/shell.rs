//! The JS shell: package specs, the bun runner, and parsing mongosh-style commands.

use super::*;

pub struct ShellJob {
    pub runtime: std::path::PathBuf,
    /// Folder holding the runner script and its installed MongoDB driver.
    pub dir: std::path::PathBuf,
    /// JSON for the runner: uri, db, code, collections, readOnly, timeoutMs.
    pub input: String,
    /// Extra npm packages the person added ("name" or "name@version").
    pub packages: Vec<String>,
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

pub(crate) async fn run_shell(req: u64, job: &ShellJob, send: &(impl Fn(Msg) + Send + Sync), mut cancel: tokio::sync::oneshot::Receiver<()>) -> Res<String> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    use tokio::process::Command;
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
    drop(stdin);
    let mut lines = tokio::io::BufReader::new(child.stdout.take().ok_or("no stdout")?).lines();
    let mut stderr = child.stderr.take().ok_or("no stderr")?;
    // Read alongside stdout: a full stderr pipe would block the child before stdout closes.
    let errs = tokio::spawn(async move {
        let mut err = String::new();
        let _ = stderr.read_to_string(&mut err).await;
        err
    });
    let read = async {
        let mut result: Option<Res<String>> = None;
        while let Some(line) = lines.next_line().await.map_err(e)? {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                send(Msg::ShellLog { req, level: "log".into(), text: line });
                continue;
            };
            let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
            match v.get("type").and_then(|t| t.as_str()) {
                Some("log") => send(Msg::ShellLog { req, level: s("level"), text: s("text") }),
                Some("result") if v.get("ok").and_then(|o| o.as_bool()) == Some(true) => result = Some(Ok(s("ejson"))),
                Some("result") => result = Some(Err(s("error"))),
                _ => {}
            }
        }
        Ok::<_, String>(result)
    };
    tokio::select! {
        r = read => {
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
            let _ = child.kill().await;
            Err("Stopped.".into())
        }
    }
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

/// Understands `db.coll.find(filter, projection).sort().skip().limit()`, `findOne`,
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
