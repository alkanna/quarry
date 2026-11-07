//! Shell editor intelligence from tsrs, a TypeScript language server, over LSP on stdio:
//! completions with their docs, hover docs and parameter hints. It runs from the shell folder so
//! the installed packages' own type definitions drive it.

use super::*;
use serde_json::{Value, json};
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
pub fn js_globals(collections: &[String], packages: &[String], ident: impl Fn(&str) -> String, reserved: &[&str]) -> String {
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
            decls.push((c.clone(), format!("declare const {c}: import(\"mongodb\").Collection;")));
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
        let g = js_globals(&colls, &["date-fns".into(), "@faker-js/faker".into()], ident, &["class"]);
        assert!(g.contains("declare const dateFns: QuarryDefault<typeof import(\"date-fns\")>;"));
        assert!(g.contains("declare const orders: import(\"mongodb\").Collection;"));
        assert!(!g.contains("my-coll") && !g.contains("const class") && g.matches("const db:").count() == 1);
        // Like the runner: a collection keeps its name over a package, the built-in dayjs over a collection.
        assert!(g.contains("declare const faker: import(\"mongodb\").Collection;") && !g.contains("\"@faker-js/faker\""));
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
        let globals = js_globals(&["orders".into()], &[], |p| p.into(), &[]);
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
}
