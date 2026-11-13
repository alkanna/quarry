//! `quarry mcp` end to end: the real binary on pipes, with HOME and the agents dir pointed at a temp dir, and a
//! fake app built from the same link code playing Quarry.

#![allow(dead_code)]

#[path = "../src/agents/link.rs"]
mod link;

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("quarry-mcp-{name}-{}-{}", std::process::id(), link::hex(&link::random()[..4])));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn command(home: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_quarry"));
    c.arg("mcp")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .env("APPDATA", home.join("appdata"))
        .env("QUARRY_AGENTS_DIR", home.join("agents"));
    c
}

struct Bridge {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl Bridge {
    fn start(home: &Path) -> Self {
        let mut child = command(home).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().expect("start quarry mcp");
        let stdin = child.stdin.take().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for l in out.lines() {
                let Ok(l) = l else { break };
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
        Self { child, stdin, lines }
    }

    fn write(&mut self, line: &str) {
        self.stdin.write_all(line.as_bytes()).unwrap();
        self.stdin.write_all(b"\n").unwrap();
        self.stdin.flush().unwrap();
    }

    fn read(&self) -> Value {
        let l = self.lines.recv_timeout(Duration::from_secs(20)).expect("an answer from quarry mcp");
        serde_json::from_str(&l).unwrap_or_else(|_| panic!("stdout must carry JSON-RPC only, got {l}"))
    }

    fn rpc(&mut self, v: Value) -> Value {
        self.write(&v.to_string());
        self.read()
    }

    fn init(&mut self) {
        let r = self.rpc(json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test-client", "version": "1.0"}}}));
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        self.write(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    }

    /// The text of a tools/call result and whether it is an error.
    fn call(&mut self, id: u64) -> (String, bool) {
        let r = self.rpc(json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": "list_connections", "arguments": {}}}));
        assert_eq!(r["id"], id);
        (r["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string(), r["result"]["isError"] == true)
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn answers_mcp_itself_and_says_when_quarry_is_closed() {
    let home = temp("basics");
    let mut b = Bridge::start(&home);
    b.init();
    let r = b.rpc(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}));
    assert_eq!(r["result"]["tools"][0]["name"], "list_connections");
    assert_eq!(r["result"]["tools"][0]["annotations"]["readOnlyHint"], true);
    assert_eq!(b.rpc(json!({"jsonrpc": "2.0", "id": "p", "method": "ping"})), json!({"jsonrpc": "2.0", "id": "p", "result": {}}));
    assert_eq!(b.rpc(json!({"jsonrpc": "2.0", "id": 2, "method": "prompts/list"}))["error"]["code"], -32601);
    assert_eq!(b.rpc(json!([{"jsonrpc": "2.0", "id": 3, "method": "ping"}]))["error"]["code"], -32600);
    let (text, err) = b.call(4);
    assert!(err);
    assert_eq!(text, "Quarry isn't open. Open it, then try again.");
    // A line over the cap is refused, and the next one is read cleanly.
    let big = format!("{{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"ping\",\"params\":{{\"pad\":\"{}\"}}}}", "x".repeat(link::MAX_LINE));
    b.write(&big);
    let r = b.read();
    assert_eq!((r["id"].clone(), r["error"]["code"].clone()), (Value::Null, json!(-32600)));
    assert_eq!(b.rpc(json!({"jsonrpc": "2.0", "id": 6, "method": "ping"}))["id"], 6);
    drop(b);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn help_goes_to_stderr() {
    let home = temp("help");
    let out = command(&home).arg("--help").output().unwrap();
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Usage: quarry mcp"));
    let _ = std::fs::remove_dir_all(&home);
}

/// Binds the link in `home/agents` and runs `app` on the first bridge that connects, while the test drives the
/// bridge from this thread.
fn with_fake_app<F>(name: &str, app: impl FnOnce(link::Stream, link::Token) -> F + Send + 'static, drive: impl FnOnce(&mut Bridge)) -> F::Output
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let home = temp(name);
    let dir = home.join("agents");
    let token = link::ensure_token(&dir).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let mut l = rt.block_on(link::Listener::bind(&dir)).ok().expect("bind the fake app");
    let task = rt.spawn(async move {
        let s = l.accept().await.unwrap();
        app(s, token).await
    });
    let mut b = Bridge::start(&home);
    b.init();
    drive(&mut b);
    let out = rt.block_on(task).unwrap();
    drop(b);
    let _ = std::fs::remove_dir_all(&home);
    out
}

#[test]
fn forwards_a_call_to_the_app() {
    let seen = with_fake_app(
        "call",
        |s, token| async move {
            let (r, mut w) = tokio::io::split(s);
            let mut r = tokio::io::BufReader::new(r);
            assert!(link::accept_hello(&mut r, &mut w, Some(token)).await.unwrap());
            let mut seen = Vec::new();
            for _ in 0..2 {
                let m = link::read_msg(&mut r).await.unwrap().unwrap();
                seen.push(m.clone());
                link::write_msg(&mut w, &json!({"id": m["id"], "ok": {"connections": [{"name": "fake-conn"}]}})).await.unwrap();
            }
            seen
        },
        |b| {
            let (text, err) = b.call(10);
            assert!(!err, "{text}");
            assert!(text.contains("fake-conn"));
            // The second call reuses the same link session.
            assert!(!b.call(11).1);
        },
    );
    assert_eq!(seen[0]["tool"], "list_connections");
    assert_eq!(seen[0]["client"]["name"], "test-client");
    assert_ne!(seen[0]["id"], seen[1]["id"]);
}

#[test]
fn reports_an_app_on_another_link_version() {
    with_fake_app(
        "version",
        |s, _| async move {
            let (r, mut w) = tokio::io::split(s);
            let mut r = tokio::io::BufReader::new(r);
            let hi = link::read_msg(&mut r).await.unwrap().unwrap();
            assert_eq!(hi["link_version"], link::LINK_VERSION);
            link::write_msg(&mut w, &json!({"link_version": link::LINK_VERSION + 1, "error": "version"})).await.unwrap();
        },
        |b| {
            let (text, err) = b.call(20);
            assert!(err);
            assert!(text.starts_with("Quarry was updated. Restart your agent"), "{text}");
        },
    );
}

#[test]
fn reports_agents_turned_off() {
    with_fake_app(
        "off",
        |s, _| async move {
            let (r, mut w) = tokio::io::split(s);
            assert!(!link::accept_hello(&mut tokio::io::BufReader::new(r), &mut w, None).await.unwrap());
        },
        |b| {
            let (text, err) = b.call(30);
            assert!(err);
            assert!(text.starts_with("AI agents are off in Quarry Settings"), "{text}");
        },
    );
}

#[test]
fn a_broken_link_is_not_retried() {
    with_fake_app(
        "broken",
        |s, token| async move {
            let (r, mut w) = tokio::io::split(s);
            let mut r = tokio::io::BufReader::new(r);
            assert!(link::accept_hello(&mut r, &mut w, Some(token)).await.unwrap());
            // Takes the call, then goes away without answering, as a crash would.
            link::read_msg(&mut r).await.unwrap().unwrap();
        },
        |b| {
            let (text, err) = b.call(40);
            assert!(err);
            assert!(text.contains("may or may not have run"), "{text}");
        },
    );
}
