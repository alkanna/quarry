//! Shell editor intelligence from tsrs, a TypeScript language server, over LSP on stdio:
//! completions with their docs, hover docs and parameter hints. It runs from the shell folder so
//! the installed packages' own type definitions drive it.

use super::*;
use serde_json::{Value, json};
use std::collections::HashMap;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

/// What a request asks for at the caret.
pub enum JsAsk {
    Complete,
    /// Parameter hints for the call around the caret.
    Signature,
    /// Docs of the word at the position.
    Hover,
    /// Signature and docs of one completion item, as the server sent it.
    Resolve(Value),
}

/// One request: the whole script and an LSP position (UTF-16 column). `globals` declares the names
/// the runner puts in scope; it is only sent when they changed, and the server keeps the last one.
pub struct JsLsReq {
    pub req: u64,
    pub ask: JsAsk,
    pub code: String,
    pub line: usize,
    pub col: usize,
    pub globals: Option<String>,
}

/// A completion item as the server sent it.
#[derive(Clone)]
pub struct JsItem {
    pub label: String,
    /// LSP CompletionItemKind.
    pub kind: u32,
    pub sort: String,
    /// What to insert: the item's own text edit or insert text, else the label.
    pub insert: String,
    /// Where the replaced text starts, as an LSP position, when the item says. A name that is not an
    /// identifier comes back as `["my-field"]` replacing the dot before it.
    pub from: Option<(usize, usize)>,
    pub raw: Value,
}

/// A signature with its docs (markdown). `active` is the byte range of the current parameter in `sig`.
#[derive(Clone, Debug, PartialEq)]
pub struct JsDoc {
    pub sig: String,
    pub doc: String,
    pub active: Option<std::ops::Range<usize>>,
}

pub enum JsOut {
    Items(Vec<JsItem>),
    Doc(Option<JsDoc>),
}

/// The server stops after this long without a request, so it only runs while someone edits a script.
const IDLE: Duration = Duration::from_secs(300);
/// A request the server does not answer in this time is treated as a hang: the server is stopped.
const ANSWER: Duration = Duration::from_secs(5);
const SCRIPT: &str = "__quarry_script.js";
const GLOBALS: &str = "__quarry_globals.d.ts";

/// The platform binary from the tsrs npm package, which installs it under node_modules/@tsrs/<platform>.
fn tsrs_exe(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let exe = format!("tsrs{}", std::env::consts::EXE_SUFFIX);
    std::fs::read_dir(dir.join("node_modules").join("@tsrs")).ok()?.flatten().map(|d| d.path().join(&exe)).find(|p| p.is_file())
}

/// The LSP position of character `at` of `code`.
pub fn lsp_pos(code: &str, at: usize) -> (usize, usize) {
    let before: String = code.chars().take(at).collect();
    let line = before.matches('\n').count();
    (line, before.rsplit('\n').next().unwrap_or_default().encode_utf16().count())
}

/// The character index of an LSP position in `code`.
pub fn char_at(code: &str, line: usize, col: usize) -> usize {
    let start: usize = code.split('\n').take(line).map(|l| l.chars().count() + 1).sum();
    let mut units = 0;
    let n = code.split('\n').nth(line).unwrap_or_default().chars().take_while(|c| {
        units += c.len_utf16();
        units <= col
    });
    start + n.count()
}

/// Declarations for what the runner puts in scope, in the runner's order so the same name wins:
/// its helpers, then collections, then the built-in _, dayjs and UUIDv1 (set over a collection of
/// that name), then added packages where the name is still free. Must match shell_runner.mjs.
/// `schemas` types the documents of the collections whose fields are known, so the server
/// completes their fields and values.
pub fn js_globals(collections: &[String], schemas: &HashMap<String, String>, packages: &[String], ident: impl Fn(&str) -> String, reserved: &[&str]) -> String {
    let mut decls: Vec<(String, String)> = [
        ("db", "declare const db: import(\"mongodb\").Db;"),
        ("ObjectId", "declare function ObjectId(id?: string | number): import(\"mongodb\").ObjectId;"),
        ("ISODate", "declare function ISODate(s?: string): Date;"),
        ("randomId", "declare function randomId(n?: number): string;"),
        ("UUIDv4", "declare function UUIDv4(): string;"),
        ("require", "declare function require(id: string): any;"),
        ("console", "declare const console: { log(...m: any[]): void; info(...m: any[]): void; warn(...m: any[]): void; error(...m: any[]): void };"),
        ("log", "declare const log: typeof console.log;"),
    ]
    .map(|(n, d)| (n.to_string(), d.to_string()))
    .to_vec();
    let plain = |c: &str| c.starts_with(|ch: char| ch.is_ascii_alphabetic() || ch == '_' || ch == '$') && c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$');
    for c in collections {
        if plain(c) && !reserved.contains(&c.as_str()) && !decls.iter().any(|d| d.0 == *c) {
            let doc = schemas.get(c).map(|t| format!("<{t}>")).unwrap_or_default();
            decls.push((c.clone(), format!("declare const {c}: import(\"mongodb\").Collection{doc};")));
        }
    }
    for (n, d) in [("_", "declare const _: typeof import(\"lodash\");"), ("dayjs", "declare const dayjs: typeof import(\"dayjs\");"), ("UUIDv1", "declare const UUIDv1: typeof import(\"uuid\").v1;")] {
        decls.retain(|x| x.0 != n);
        decls.push((n.into(), d.into()));
    }
    for p in packages {
        let id = ident(p);
        if !decls.iter().any(|d| d.0 == id) {
            decls.push((id.clone(), format!("declare const {id}: QuarryDefault<typeof import({})>;", Value::String(p.clone()))));
        }
    }
    let mut out = String::from("type QuarryDefault<T> = T extends { default: infer D } ? D : T;\n");
    for (_, d) in decls {
        out += &d;
        out.push('\n');
    }
    out
}

/// The values of an `enum('a','b')` column type, in order; quotes inside a value are doubled.
fn enum_values(full: &str) -> Vec<String> {
    let Some(body) = full.strip_prefix("enum(").and_then(|r| r.strip_suffix(')')) else { return Vec::new() };
    let (mut out, mut cur, mut open) = (Vec::new(), String::new(), false);
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, open) {
            ('\'', false) => open = true,
            ('\'', true) if chars.peek() == Some(&'\'') => {
                cur.push('\'');
                chars.next();
            }
            ('\'', true) => {
                open = false;
                out.push(std::mem::take(&mut cur));
            }
            (c, true) => cur.push(c),
            _ => {}
        }
    }
    out
}

/// The TypeScript type of a column as the runner returns it: integers as numbers, ENUM as its values,
/// and DECIMAL, dates and JSON as the server's own text so nothing is rounded.
fn sql_ts_type(c: &sql::TableCol) -> String {
    use sql::value::Kind;
    let t = match Kind::from_column_type(&c.full_type) {
        Kind::Int | Kind::UInt | Kind::Float | Kind::Year => "number".to_string(),
        Kind::Binary | Kind::Bit | Kind::Geometry => "Uint8Array".to_string(),
        Kind::Enum => {
            let v = enum_values(&c.full_type);
            if v.is_empty() { "string".to_string() } else { v.iter().map(|x| format!("'{}'", x.replace('\\', "\\\\").replace('\'', "\\'").replace('\n', "\\n"))).collect::<Vec<_>>().join(" | ") }
        }
        _ => "string".to_string(),
    };
    if c.nullable { format!("{t} | null") } else { t }
}

/// `order_items` as `OrderItems`; a name with no letters or digits becomes `Table`.
fn interface_name(table: &str) -> String {
    let s: String = table.split(|c: char| !c.is_ascii_alphanumeric()).filter(|p| !p.is_empty()).map(|p| p[..1].to_ascii_uppercase() + &p[1..]).collect();
    if s.is_empty() || s.starts_with(|c: char| c.is_ascii_digit()) { format!("Table{s}") } else { s }
}

/// The type aliases a SQL tab and an attached SQL connection are both typed with.
fn sql_types() -> String {
    let mut out = String::from("type QuarryOp<V> = { gt?: V; gte?: V; lt?: V; lte?: V; ne?: V | null; like?: string; in?: V[] };\n");
    out += "type QuarryWhere<R> = { [K in keyof R]?: R[K] | R[K][] | null | QuarryOp<R[K]> };\n";
    out += "type QuarryKey<R> = (keyof R & string) | `-${keyof R & string}`;\n";
    out += "interface QuarryFind<R> { select?: (keyof R & string)[]; orderBy?: QuarryKey<R> | QuarryKey<R>[] | { [K in keyof R]?: 'asc' | 'desc' }; limit?: number; offset?: number }\n";
    out += "interface QuarryWrite { affected: number; insertId: number }\n";
    out += "interface QuarryTable<R> {\n  find(where?: QuarryWhere<R>, opts?: QuarryFind<R>): Promise<R[]>;\n  findOne(where?: QuarryWhere<R>, opts?: QuarryFind<R>): Promise<R | null>;\n  count(where?: QuarryWhere<R>): Promise<number>;\n  insert(row: Partial<R> | Partial<R>[]): Promise<QuarryWrite>;\n  update(where: QuarryWhere<R>, set: Partial<R>): Promise<QuarryWrite>;\n  delete(where: QuarryWhere<R>): Promise<QuarryWrite>;\n}\n";
    out += "interface QuarryConn {\n  <R = any>(s: TemplateStringsArray, ...v: unknown[]): Promise<R[]>;\n  sql<R = any>(s: TemplateStringsArray, ...v: unknown[]): Promise<R[]>;\n  unsafe<R = any>(text: string, params?: unknown[]): Promise<R[]>;\n  table<R = any>(name: string): QuarryTable<R>;\n}\n";
    out
}

/// What an attached connection holds, as far as the editor knows it.
pub enum AttachedKind<'a> {
    /// Collection names, and the TypeScript type of the documents of those whose fields are known.
    Mongo { collections: &'a [String], schemas: &'a HashMap<String, String> },
    Sql { tables: &'a [sql::TableCols] },
}

fn plain_name(n: &str, reserved: &[&str]) -> bool {
    n.starts_with(|ch: char| ch.is_ascii_alphabetic() || ch == '_' || ch == '$') && n.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$') && !reserved.contains(&n)
}

/// One `declare const` per attached connection, matching what shell_runner.mjs builds: a Mongo one is a Db with
/// a collection per plain name (a name the Db already has keeps its method), a SQL one is a tag function that
/// also has `.sql`, `.unsafe`, `.table()` and a property per table. `with_types` adds the shared SQL type
/// aliases for a tab that has no SQL of its own; a SQL tab already declares them.
pub fn attached_globals(items: &[(&str, AttachedKind)], with_types: bool, reserved: &[&str]) -> String {
    let mut out = String::new();
    if with_types && items.iter().any(|i| matches!(i.1, AttachedKind::Sql { .. })) {
        out += &sql_types();
    }
    for (name, kind) in items {
        if !plain_name(name, reserved) {
            continue;
        }
        match kind {
            AttachedKind::Mongo { collections, schemas } => {
                let props: Vec<String> = collections.iter().filter(|c| plain_name(c, reserved)).map(|c| format!("{c}: import(\"mongodb\").Collection{}", schemas.get(c).map(|t| format!("<{t}>")).unwrap_or_default())).collect();
                out += &format!("declare const {name}: import(\"mongodb\").Db & Omit<{{ {} }}, keyof import(\"mongodb\").Db>;\n", props.join("; "));
            }
            AttachedKind::Sql { tables } => {
                let mut names: Vec<String> = Vec::new();
                let mut props: Vec<String> = Vec::new();
                for t in tables.iter().filter(|t| plain_name(&t.name, reserved)) {
                    let mut iface = format!("Quarry_{name}_{}", interface_name(&t.name));
                    while names.contains(&iface) {
                        iface.push('_');
                    }
                    out += &format!("interface {iface} {{\n");
                    for c in &t.cols {
                        out += &format!("  {}: {};\n", if c.name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$') && !c.name.starts_with(|ch: char| ch.is_ascii_digit()) { c.name.clone() } else { Value::String(c.name.clone()).to_string() }, sql_ts_type(c));
                    }
                    out += "}\n";
                    props.push(format!("{}: QuarryTable<{iface}>", t.name));
                    names.push(iface);
                }
                out += &format!("declare const {name}: QuarryConn & Omit<{{ {} }}, keyof QuarryConn | keyof Function>;\n", props.join("; "));
            }
        }
    }
    out
}

/// Declarations for a SQL tab's scripts, in the order shell_runner.mjs puts names in scope: the helpers,
/// one interface and one global per table whose name is a plain word, then `_`, dayjs and added packages
/// where the name is still free. Must match shell_runner.mjs.
pub fn sql_globals(tables: &[sql::TableCols], packages: &[String], ident: impl Fn(&str) -> String, reserved: &[&str]) -> String {
    let mut out = String::from("type QuarryDefault<T> = T extends { default: infer D } ? D : T;\n");
    out += &sql_types();
    out += "declare function sql<T = any>(s: TemplateStringsArray, ...v: unknown[]): Promise<T[]>;\n";
    out += "declare namespace sql { function unsafe<T = any>(text: string, params?: unknown[]): Promise<T[]>; }\n";
    out += "declare function table<T = any>(name: string): QuarryTable<T>;\n";
    out += "declare function require(id: string): any;\n";
    out += "declare const console: { log(...m: any[]): void; info(...m: any[]): void; warn(...m: any[]): void; error(...m: any[]): void };\n";
    out += "declare const log: typeof console.log;\n";
    let mut taken: Vec<String> = ["sql", "table", "require", "console", "log"].iter().map(|s| s.to_string()).collect();
    let mut names: Vec<String> = Vec::new();
    for t in tables {
        let plain = t.name.starts_with(|ch: char| ch.is_ascii_alphabetic() || ch == '_' || ch == '$') && t.name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$');
        if !plain || reserved.contains(&t.name.as_str()) || taken.contains(&t.name) {
            continue;
        }
        let mut name = interface_name(&t.name);
        while names.contains(&name) {
            name.push('_');
        }
        out += &format!("interface {name} {{\n");
        for c in &t.cols {
            out += &format!("  {}: {};\n", if c.name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$') && !c.name.starts_with(|ch: char| ch.is_ascii_digit()) { c.name.clone() } else { Value::String(c.name.clone()).to_string() }, sql_ts_type(c));
        }
        out += &format!("}}\ndeclare const {}: QuarryTable<{name}>;\n", t.name);
        taken.push(t.name.clone());
        names.push(name);
    }
    for (n, d) in [("_", "declare const _: typeof import(\"lodash\");"), ("dayjs", "declare const dayjs: typeof import(\"dayjs\");")] {
        if let Some(i) = out.find(&format!("declare const {n}: QuarryTable")) {
            // A table of that name is replaced, as the runner sets the helper over it.
            let end = out[i..].find('\n').map_or(out.len(), |e| i + e + 1);
            out.replace_range(i..end, "");
        }
        out += d;
        out.push('\n');
        taken.push(n.to_string());
    }
    for p in packages {
        let id = ident(p);
        if !taken.contains(&id) {
            out += &format!("declare const {id}: QuarryDefault<typeof import({})>;\n", Value::String(p.clone()));
            taken.push(id);
        }
    }
    out
}

/// A file:// URI for a folder, ending in a slash. The shell folder holds spaces on macOS ("Application Support").
fn file_uri(dir: &std::path::Path) -> String {
    let path = dir.to_string_lossy().replace('\\', "/");
    let path = if path.starts_with('/') { path } else { format!("/{path}") };
    let mut out = String::from("file://");
    for b in path.trim_end_matches('/').bytes().chain(*b"/") {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            out.push(b as char);
        } else {
            out += &format!("%{b:02X}");
        }
    }
    out
}

/// Text of a string or MarkupContent.
fn markup(v: &Value) -> String {
    v.as_str().or_else(|| v["value"].as_str()).unwrap_or_default().to_string()
}

fn item(i: &Value) -> Option<JsItem> {
    let label = i["label"].as_str()?.to_string();
    let edit = &i["textEdit"];
    let insert = edit["newText"].as_str().or(i["insertText"].as_str()).unwrap_or(&label).to_string();
    let start = edit.get("range").or(edit.get("replace")).map(|r| &r["start"]);
    let from = start.and_then(|s| Some((s["line"].as_u64()? as usize, s["character"].as_u64()? as usize)));
    Some(JsItem { kind: i["kind"].as_u64().unwrap_or(0) as u32, sort: i["sortText"].as_str().unwrap_or_default().to_string(), insert, from, raw: i.clone(), label })
}

/// Hover markdown starts with the signature in a code fence; the rest is its docs.
fn hover_doc(v: &Value) -> Option<JsDoc> {
    let text = markup(&v["contents"]);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Some(rest) = text.strip_prefix("```")
        && let Some((fence, doc)) = rest.split_once("```")
    {
        let sig = fence.split_once('\n').map_or(fence, |(_, s)| s).trim().to_string();
        return Some(JsDoc { sig, doc: doc.trim().to_string(), active: None });
    }
    Some(JsDoc { sig: String::new(), doc: text.to_string(), active: None })
}

fn signature_doc(v: &Value) -> Option<JsDoc> {
    let sigs = v["signatures"].as_array()?;
    let s = sigs.get(v["activeSignature"].as_u64().unwrap_or(0) as usize).or(sigs.first())?;
    let sig = s["label"].as_str()?.to_string();
    let n = s["activeParameter"].as_u64().or(v["activeParameter"].as_u64()).unwrap_or(0) as usize;
    let p = &s["parameters"][n];
    // A parameter label is its text, or with labelOffsetSupport its UTF-16 offsets in the signature.
    let active = match &p["label"] {
        Value::String(l) => sig.find(l.as_str()).map(|i| i..i + l.len()),
        Value::Array(o) => {
            // The byte where UTF-16 offset `u` falls; the end offset is exclusive, so it may be the length.
            let byte = |u: u64| {
                let mut n = 0;
                sig.char_indices().find(|(_, c)| {
                    let hit = n >= u;
                    n += c.len_utf16() as u64;
                    hit
                })
                .map_or(sig.len(), |(b, _)| b)
            };
            Some(byte(o.first()?.as_u64()?)..byte(o.get(1)?.as_u64()?))
        }
        _ => None,
    };
    let mut doc = markup(&s["documentation"]);
    let pdoc = markup(&p["documentation"]);
    if !pdoc.is_empty() {
        doc = format!("{pdoc}\n\n{doc}");
    }
    Some(JsDoc { sig, doc: doc.trim().to_string(), active })
}

struct Lsp {
    stdin: tokio::process::ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
    next: u64,
}

impl Lsp {
    async fn send(&mut self, v: Value) -> Res<()> {
        let body = v.to_string();
        self.stdin.write_all(format!("Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.map_err(e)?;
        self.stdin.flush().await.map_err(e)
    }

    async fn notify(&mut self, method: &str, params: Value) -> Res<()> {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params })).await
    }

    async fn read(&mut self) -> Res<Value> {
        let mut len = 0;
        loop {
            let mut line = String::new();
            if self.stdout.read_line(&mut line).await.map_err(e)? == 0 {
                return Err("The language server stopped.".into());
            }
            let line = line.trim();
            if line.is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':')
                && k.eq_ignore_ascii_case("content-length")
            {
                len = v.trim().parse().map_err(e)?;
            }
        }
        let mut body = vec![0; len];
        self.stdout.read_exact(&mut body).await.map_err(e)?;
        serde_json::from_slice(&body).map_err(e)
    }

    /// Sends a request and waits for its answer, replying to the server's own requests meanwhile.
    async fn request(&mut self, method: &str, params: Value) -> Res<Value> {
        self.next += 1;
        let id = self.next;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).await?;
        let wait = async {
            loop {
                let m = self.read().await?;
                match (m.get("id"), m.get("method")) {
                    // The server asks for settings and to register watchers; empty answers are enough.
                    (Some(rid), Some(method)) => {
                        let n = m["params"]["items"].as_array().map_or(0, |a| a.len());
                        let result = if method == "workspace/configuration" { json!(vec![Value::Null; n]) } else { Value::Null };
                        self.send(json!({ "jsonrpc": "2.0", "id": rid, "result": result })).await?;
                    }
                    (Some(rid), None) if rid.as_u64() == Some(id) => return Ok(m.get("result").cloned().unwrap_or(Value::Null)),
                    _ => {}
                }
            }
        };
        tokio::time::timeout(ANSWER, wait).await.map_err(|_| format!("The language server did not answer {method}."))?
    }
}

/// Runs the language server until `rx` closes or stays quiet for `IDLE`; an error means it failed.
/// Only the newest waiting request of each kind is answered, so fast typing never queues stale work.
pub(crate) async fn run_js_ls(runtime: std::path::PathBuf, dir: std::path::PathBuf, packages: Vec<String>, mut rx: tokio::sync::mpsc::UnboundedReceiver<JsLsReq>, send: impl Fn(Msg)) -> Res<()> {
    prepare_shell_dir(&runtime, &dir, &packages, &|_| {}).await?;
    let exe = tsrs_exe(&dir).ok_or("The tsrs language server is not available for this platform.")?;
    for f in [SCRIPT, GLOBALS] {
        if !dir.join(f).exists() {
            std::fs::write(dir.join(f), "").map_err(e)?;
        }
    }
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.args(["--lsp", "-stdio"]).current_dir(&dir).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(crate::update::CREATE_NO_WINDOW);
    let mut child = cmd.spawn().map_err(|err| format!("Could not start tsrs ({}): {err}", exe.display()))?;
    let mut ls = Lsp { stdin: child.stdin.take().ok_or("no stdin")?, stdout: BufReader::new(child.stdout.take().ok_or("no stdout")?), next: 0 };
    let root = file_uri(&dir);
    let uri = |f: &str| format!("{root}{f}");
    let markdown = json!(["markdown", "plaintext"]);
    let caps = json!({ "textDocument": {
        "completion": { "completionItem": { "documentationFormat": markdown, "resolveSupport": { "properties": ["documentation", "detail"] } } },
        "hover": { "contentFormat": markdown },
        "signatureHelp": { "signatureInformation": { "documentationFormat": markdown, "parameterInformation": { "labelOffsetSupport": true } } },
    } });
    ls.request("initialize", json!({ "processId": std::process::id(), "rootUri": root, "capabilities": caps, "workspaceFolders": [{ "uri": root, "name": "shell" }] })).await?;
    ls.notify("initialized", json!({})).await?;
    for (f, lang) in [(GLOBALS, "typescript"), (SCRIPT, "javascript")] {
        ls.notify("textDocument/didOpen", json!({ "textDocument": { "uri": uri(f), "languageId": lang, "version": 1, "text": "" } })).await?;
    }
    let (mut version, mut code) = (1, String::new());
    while let Ok(Some(first)) = tokio::time::timeout(IDLE, rx.recv()).await {
        // The newest of each kind, answered in this order; globals from any of them still apply.
        let mut newest: [Option<JsLsReq>; 4] = Default::default();
        let mut globals = None;
        let mut next = Some(first);
        while let Some(mut r) = next {
            globals = r.globals.take().or(globals);
            let slot = match r.ask {
                JsAsk::Complete => 0,
                JsAsk::Signature => 1,
                JsAsk::Resolve(_) => 2,
                JsAsk::Hover => 3,
            };
            newest[slot] = Some(r);
            next = rx.try_recv().ok();
        }
        if let Some(g) = globals {
            version += 1;
            ls.notify("textDocument/didChange", json!({ "textDocument": { "uri": uri(GLOBALS), "version": version }, "contentChanges": [{ "text": g }] })).await?;
        }
        for r in newest.into_iter().flatten() {
            if r.code != code {
                version += 1;
                ls.notify("textDocument/didChange", json!({ "textDocument": { "uri": uri(SCRIPT), "version": version }, "contentChanges": [{ "text": r.code }] })).await?;
                code = r.code;
            }
            let at = json!({ "textDocument": { "uri": uri(SCRIPT) }, "position": { "line": r.line, "character": r.col } });
            let out = match r.ask {
                JsAsk::Complete => {
                    let res = ls.request("textDocument/completion", at).await?;
                    // Either a CompletionList or a bare array of items.
                    JsOut::Items(res.get("items").unwrap_or(&res).as_array().map(|a| a.iter().filter_map(item).collect()).unwrap_or_default())
                }
                JsAsk::Signature => JsOut::Doc(signature_doc(&ls.request("textDocument/signatureHelp", at).await?)),
                JsAsk::Hover => JsOut::Doc(hover_doc(&ls.request("textDocument/hover", at).await?)),
                JsAsk::Resolve(raw) => {
                    let res = ls.request("completionItem/resolve", raw).await?;
                    let sig = res["detail"].as_str().unwrap_or_default().to_string();
                    let doc = markup(&res["documentation"]);
                    JsOut::Doc((!sig.is_empty() || !doc.is_empty()).then_some(JsDoc { sig, doc, active: None }))
                }
            };
            send(Msg::JsLs { req: r.req, out });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_folder_uri_and_globals() {
        assert_eq!(file_uri(std::path::Path::new("/Users/a/Library/Application Support/quarry/shell")), "file:///Users/a/Library/Application%20Support/quarry/shell/");
        let colls = ["orders", "my-coll", "class", "db", "faker", "dayjs"].map(String::from);
        let ident = |p: &str| match p {
            "date-fns" => "dateFns".to_string(),
            "@faker-js/faker" => "faker".to_string(),
            _ => p.to_string(),
        };
        let schemas = HashMap::from([("faker".to_string(), "{ status?: \"paid\" }".to_string())]);
        let g = js_globals(&colls, &schemas, &["date-fns".into(), "@faker-js/faker".into()], ident, &["class"]);
        assert!(g.contains("declare const dateFns: QuarryDefault<typeof import(\"date-fns\")>;"));
        assert!(g.contains("declare const orders: import(\"mongodb\").Collection;"));
        assert!(!g.contains("my-coll") && !g.contains("const class") && g.matches("const db:").count() == 1);
        // Like the runner: a collection keeps its name over a package, the built-in dayjs over a collection.
        assert!(g.contains("declare const faker: import(\"mongodb\").Collection<{ status?: \"paid\" }>;") && !g.contains("\"@faker-js/faker\""));
        assert!(g.contains("declare const dayjs: typeof import(\"dayjs\");") && g.matches("const dayjs").count() == 1);
    }

    #[test]
    fn positions_and_docs() {
        let code = "a\né🙂x.y";
        assert_eq!(lsp_pos(code, 5), (1, 4));
        assert_eq!(char_at(code, 1, 4), 5);
        let sig = json!({ "signatures": [{ "label": "add(value: number, unit?: string): Dayjs", "parameters": [{ "label": "value: number" }, { "label": "unit?: string" }] }], "activeParameter": 1 });
        let d = signature_doc(&sig).unwrap();
        assert_eq!(&d.sig[d.active.clone().unwrap()], "unit?: string");
        let offsets = json!({ "signatures": [{ "label": "f(a: 1, b: 2)", "parameters": [{ "label": [2, 6] }, { "label": [8, 12] }] }], "activeParameter": 1 });
        let d = signature_doc(&offsets).unwrap();
        assert_eq!(&d.sig[d.active.unwrap()], "b: 2");
        let hover = json!({ "contents": { "kind": "markdown", "value": "```typescript\n(method) f(): void\n```\nDoes f." } });
        assert_eq!(hover_doc(&hover), Some(JsDoc { sig: "(method) f(): void".into(), doc: "Does f.".into(), active: None }));
        let edit = item(&json!({ "label": "my-field", "kind": 5, "textEdit": { "range": { "start": { "line": 2, "character": 10 } }, "newText": "[\"my-field\"]" } })).unwrap();
        assert_eq!((edit.insert.as_str(), edit.from), ("[\"my-field\"]", Some((2, 10))));
    }

    /// Installs the shell packages with Bun and asks tsrs for real answers.
    /// Run with: QUARRY_LS_DIR=<empty folder> cargo test js_ls_end_to_end -- --ignored
    #[test]
    #[ignore = "needs Bun and network"]
    fn js_ls_end_to_end() {
        let dir = std::path::PathBuf::from(std::env::var("QUARRY_LS_DIR").expect("QUARRY_LS_DIR"));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (out, got) = std::sync::mpsc::channel();
        let schemas = HashMap::from([("orders".to_string(), "{ status?: \"paid\" | \"shipped\" | (string & {}) }".to_string())]);
        let globals = js_globals(&["orders".into()], &schemas, &[], |p| p.into(), &[]);
        let ask = |req, ask, code: &str, globals: Option<String>| {
            let (line, col) = lsp_pos(code, code.chars().count());
            JsLsReq { req, ask, code: code.into(), line, col, globals }
        };
        tx.send(ask(1, JsAsk::Complete, "async function run() {\n  return dayjs().", Some(globals))).unwrap();
        tx.send(ask(2, JsAsk::Complete, "async function run() {\n  return orders.", None)).unwrap();
        tx.send(ask(3, JsAsk::Signature, "async function run() {\n  return dayjs().add(1, ", None)).unwrap();
        drop(tx);
        rt.block_on(run_js_ls(std::path::PathBuf::from("bun"), dir, Vec::new(), rx, move |m| {
            let _ = out.send(m);
        }))
        .unwrap();
        // Requests 1 and 2 were both waiting, so only the newest completion is answered.
        let answers: Vec<Msg> = got.try_iter().collect();
        assert_eq!(answers.len(), 2);
        let Msg::JsLs { req: 2, out: JsOut::Items(items) } = &answers[0] else { panic!() };
        assert!(items.iter().any(|i| i.label == "findOne" && i.kind == 2));
        let Msg::JsLs { req: 3, out: JsOut::Doc(Some(d)) } = &answers[1] else { panic!() };
        assert!(d.sig.starts_with("add(") && d.active.is_some(), "{d:?}");
    }

    /// A collection typed from its loaded fields: a quote after a text field's colon offers its values.
    /// Run with: QUARRY_LS_DIR=<empty folder> cargo test js_ls_query_values -- --ignored
    #[test]
    #[ignore = "needs Bun and network"]
    fn js_ls_query_values() {
        let dir = std::path::PathBuf::from(std::env::var("QUARRY_LS_DIR").expect("QUARRY_LS_DIR"));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (out, got) = std::sync::mpsc::channel();
        let schemas = HashMap::from([("travels".to_string(), "{ status?: \"booked\" | \"cancelled\" | (string & {}); price?: number; [key: string]: any }".to_string())]);
        let code = "async function run() {\n  return await travels.find({status: \"";
        let (line, col) = lsp_pos(code, code.chars().count());
        let globals = js_globals(&["travels".into()], &schemas, &[], |p| p.into(), &[]);
        tx.send(JsLsReq { req: 1, ask: JsAsk::Complete, code: format!("{code}\"}}).limit(20).toArray();\n}}"), line, col, globals: Some(globals) }).unwrap();
        drop(tx);
        rt.block_on(run_js_ls(std::path::PathBuf::from("bun"), dir, Vec::new(), rx, move |m| {
            let _ = out.send(m);
        }))
        .unwrap();
        let Some(Msg::JsLs { req: 1, out: JsOut::Items(items) }) = got.try_iter().next() else { panic!() };
        // Plain text items (kind 1) are dropped from the Shell's list, so the values must not be one.
        let labels: Vec<&str> = items.iter().filter(|i| i.kind != 1).map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"booked") && labels.contains(&"cancelled"), "{labels:?}");
    }

    #[test]
    fn sql_declarations_from_columns() {
        let col = |name: &str, full_type: &str, nullable: bool| sql::TableCol { name: name.into(), full_type: full_type.into(), nullable };
        let tables = vec![
            sql::TableCols { name: "orders".into(), cols: vec![col("id", "int unsigned", false), col("status", "enum('pending','paid','it''s')", false), col("total", "decimal(10,2)", false), col("note", "varchar(20)", true), col("created_at", "datetime", false), col("meta", "json", true), col("blob", "blob", true)] },
            sql::TableCols { name: "order_items".into(), cols: vec![col("qty", "smallint", false)] },
            sql::TableCols { name: "my-table".into(), cols: vec![col("x", "int", false)] },
            sql::TableCols { name: "delete".into(), cols: Vec::new() },
        ];
        let g = sql_globals(&tables, &["date-fns".into()], |p| p.replace('-', "_"), &["delete"]);
        assert!(g.contains("declare function sql<T = any>(s: TemplateStringsArray, ...v: unknown[]): Promise<T[]>;"), "{g}");
        assert!(g.contains("function unsafe<T = any>(text: string, params?: unknown[]): Promise<T[]>"));
        assert!(g.contains("interface Orders {\n  id: number;\n  status: 'pending' | 'paid' | 'it\\'s';\n  total: string;\n  note: string | null;\n  created_at: string;\n  meta: string | null;\n  blob: Uint8Array | null;\n}"), "{g}");
        assert!(g.contains("declare const orders: QuarryTable<Orders>;"));
        assert!(g.contains("declare const order_items: QuarryTable<OrderItems>;"));
        // Names that are not plain words or are reserved are not globals; table('name') reaches them.
        assert!(!g.contains("my-table") && !g.contains("declare const delete"));
        assert!(g.contains("declare const date_fns: QuarryDefault<typeof import(\"date-fns\")>;"));
        assert!(g.contains("declare function table<T = any>(name: string): QuarryTable<T>;"));
        for m in ["find(", "findOne(", "count(", "insert(", "update(", "delete("] {
            assert!(g.contains(m), "{m}");
        }
    }

    #[test]
    fn attached_declarations() {
        let col = |name: &str, full_type: &str, nullable: bool| sql::TableCol { name: name.into(), full_type: full_type.into(), nullable };
        let tables = vec![sql::TableCols { name: "orders".into(), cols: vec![col("id", "int", false), col("note", "text", true)] }, sql::TableCols { name: "my-table".into(), cols: Vec::new() }];
        let colls = ["users", "trips", "my-coll", "class"].map(String::from);
        let schemas = HashMap::from([("users".to_string(), "{ name?: string }".to_string())]);
        let items = [("travels", AttachedKind::Mongo { collections: &colls, schemas: &schemas }), ("shop", AttachedKind::Sql { tables: &tables }), ("class", AttachedKind::Sql { tables: &[] })];
        let g = attached_globals(&items, true, &["class"]);
        assert!(g.contains("declare const travels: import(\"mongodb\").Db & Omit<{ users: import(\"mongodb\").Collection<{ name?: string }>; trips: import(\"mongodb\").Collection }, keyof import(\"mongodb\").Db>;"), "{g}");
        assert!(g.contains("interface Quarry_shop_Orders {\n  id: number;\n  note: string | null;\n}"), "{g}");
        assert!(g.contains("declare const shop: QuarryConn & Omit<{ orders: QuarryTable<Quarry_shop_Orders> }, keyof QuarryConn | keyof Function>;"), "{g}");
        assert!(g.contains("interface QuarryConn {") && g.contains("  unsafe<R = any>(text: string, params?: unknown[]): Promise<R[]>;"));
        // A name that cannot be a const is skipped, and a SQL tab already has the aliases.
        assert!(!g.contains("const class") && !g.contains("my-table") && !g.contains("my-coll"));
        assert!(!attached_globals(&items, false, &["class"]).contains("interface QuarryConn"));
        assert!(attached_globals(&items[..1], true, &[]).find("QuarryTable").is_none());
    }
}
