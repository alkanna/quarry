//! `quarry mcp`: the stdio MCP server an agent starts. It answers initialize, tools/list and ping itself and
//! forwards each tools/call to the running app over the link. It decides nothing: every check is in the app.

use super::link::{self, ClientInfo, ConnectErr, Line, Refused};
use super::protocol::{self, Server, Step};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::BufReader;
use tokio::sync::{mpsc, oneshot};

const USAGE: &str = "Usage: quarry mcp

Runs Quarry's MCP server on stdin and stdout, for AI agents such as Claude Code, Claude Desktop, Cursor or Codex.
The agent starts this command itself; see Settings > AI agents in Quarry for the line to add to each client.
Quarry has to be open, with AI agents turned on, for tools to work.";

const NOT_OPEN: &str = "Quarry isn't open. Open it, then try again.";
const OFF: &str = "AI agents are off in Quarry Settings. Turn them on in Settings > AI agents, then try again.";
const BROKEN: &str = "The link to Quarry broke during this call, so it may or may not have run. Check in Quarry before trying again: the call was not retried.";
const AUTH: &str = "Could not confirm that the program answering is Quarry, so nothing was sent. Restart Quarry, then try again.";

pub fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(2).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        attach_console();
        eprintln!("{USAGE}");
        return 0;
    }
    if let Some(a) = args.first() {
        attach_console();
        eprintln!("quarry mcp: unknown argument {a}\n\n{USAGE}");
        return 2;
    }
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("quarry mcp: could not start: {e}");
            return 1;
        }
    };
    rt.block_on(serve(link::agents_dir()));
    0
}

/// Release builds on Windows are GUI programs with no console, so `--help` typed in one would print nowhere.
/// Agents pass pipes instead, which work without this.
fn attach_console() {
    #[cfg(windows)]
    // SAFETY: plain call with a constant; fails harmlessly when there is no parent console.
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS);
    }
}

/// Only JSON-RPC ever goes to stdout; one message per line, written whole.
fn send(v: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = serde_json::to_writer(&mut out, v);
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

enum Reply {
    Ok(Value),
    Err(link::ToolErr),
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>>;

/// One authenticated link session. Kept across calls, since what the user allows "for this session" belongs to
/// the link connection; made again on the next call once it breaks.
#[derive(Clone)]
struct Conn {
    out: mpsc::UnboundedSender<Value>,
    pending: Pending,
    alive: Arc<AtomicBool>,
}

async fn open(dir: &std::path::Path) -> Result<Conn, String> {
    let s = match link::connect(dir).await {
        Ok(s) => s,
        Err(ConnectErr::NotOpen) => return Err(NOT_OPEN.into()),
        Err(ConnectErr::Io(e)) => return Err(format!("Could not reach Quarry: {e}")),
    };
    let (r, mut w) = tokio::io::split(s);
    let mut r = BufReader::new(r);
    let token_dir = dir.to_path_buf();
    match link::hello(&mut r, &mut w, move || link::read_token(&token_dir).ok()).await {
        Ok(()) => {}
        Err(Refused::Off) => return Err(OFF.into()),
        Err(Refused::Auth) => return Err(AUTH.into()),
        Err(Refused::Version { app }) if app > link::LINK_VERSION as u64 => return Err("Quarry was updated. Restart your agent so it starts the new Quarry bridge, then try again.".into()),
        Err(Refused::Version { .. }) => return Err("This Quarry window runs an older version than the bridge your agent started. Restart Quarry, then try again.".into()),
        Err(Refused::Io(e)) => return Err(format!("Could not reach Quarry: {e}")),
    }
    let (out, mut rx) = mpsc::unbounded_channel::<Value>();
    let pending: Pending = Default::default();
    let alive = Arc::new(AtomicBool::new(true));
    let conn = Conn { out, pending: pending.clone(), alive: alive.clone() };
    tokio::spawn(async move {
        while let Some(v) = rx.recv().await {
            if link::write_msg(&mut w, &v).await.is_err() {
                break;
            }
        }
    });
    tokio::spawn(async move {
        while let Ok(Some(v)) = link::read_msg(&mut r).await {
            let Some(id) = v["id"].as_u64() else { continue };
            let Some(tx) = pending.lock().unwrap().remove(&id) else { continue };
            let reply = match serde_json::from_value::<link::ToolErr>(v["err"].clone()) {
                Ok(e) => Reply::Err(e),
                Err(_) => Reply::Ok(v.get("ok").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(reply);
        }
        // Dropping the senders tells every call still waiting that the link broke.
        alive.store(false, Ordering::SeqCst);
        pending.lock().unwrap().clear();
    });
    Ok(conn)
}

/// The key of a JSON-RPC id in maps: ids are strings or integers, and 1 and "1" are different requests.
fn id_key(id: &Value) -> String {
    id.to_string()
}

struct Bridge {
    dir: PathBuf,
    conn: tokio::sync::Mutex<Option<Conn>>,
    /// JSON-RPC id of each call in flight to its link id, so a cancel finds it. A call missing here when its
    /// answer comes was cancelled and gets no answer.
    inflight: Mutex<HashMap<String, (u64, Conn)>>,
    next: AtomicU64,
}

impl Bridge {
    async fn call(self: Arc<Self>, id: Value, name: String, args: Value, progress: Option<Value>, client: ClientInfo) {
        let conn = {
            let mut g = self.conn.lock().await;
            if !g.as_ref().is_some_and(|c| c.alive.load(Ordering::SeqCst)) {
                *g = None;
                match open(&self.dir).await {
                    Ok(c) => *g = Some(c),
                    Err(msg) => return send(&protocol::tool_result(id, msg, true)),
                }
            }
            g.clone().expect("set above")
        };
        let lid = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        conn.pending.lock().unwrap().insert(lid, tx);
        self.inflight.lock().unwrap().insert(id_key(&id), (lid, conn.clone()));
        let mut msg = json!({"id": lid, "tool": name, "args": args, "client": client});
        if let Some(p) = progress {
            msg["progress"] = p;
        }
        if conn.out.send(msg).is_err() {
            conn.pending.lock().unwrap().remove(&lid);
        }
        let reply = rx.await;
        if self.inflight.lock().unwrap().remove(&id_key(&id)).is_none() {
            return;
        }
        match reply {
            Ok(Reply::Ok(v)) => send(&protocol::tool_result(id, serde_json::to_string_pretty(&v).unwrap_or_default(), false)),
            Ok(Reply::Err(e)) => send(&protocol::tool_result(id, e.message, true)),
            Err(_) => send(&protocol::tool_result(id, BROKEN.into(), true)),
        }
    }

    fn cancel(&self, id: &Value) {
        if let Some((lid, conn)) = self.inflight.lock().unwrap().remove(&id_key(id)) {
            let _ = conn.out.send(json!({"cancel": lid}));
        }
    }
}

async fn serve(dir: PathBuf) {
    // stdin is read on a plain thread: tokio's stdin needs a feature the app does not otherwise use.
    let (lines, mut rx) = mpsc::unbounded_channel::<Line>();
    std::thread::spawn(move || {
        let mut input = std::io::stdin().lock();
        loop {
            match link::read_line_sync(&mut input, link::MAX_LINE) {
                Ok(Line::Eof) | Err(_) => break,
                Ok(l) => {
                    if lines.send(l).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let bridge = Arc::new(Bridge { dir, conn: Default::default(), inflight: Default::default(), next: AtomicU64::new(1) });
    let mut server = Server::default();
    while let Some(line) = rx.recv().await {
        let text = match line {
            Line::Text(t) if t.trim().is_empty() => continue,
            Line::Text(t) => t,
            Line::TooLong => {
                send(&protocol::err(Value::Null, protocol::INVALID_REQUEST, "Message too large"));
                continue;
            }
            Line::Eof => break,
        };
        match server.handle(&text) {
            Step::Reply(v) => send(&v),
            Step::Call { id, name, args, progress } => {
                tokio::spawn(bridge.clone().call(id, name, args, progress, server.client.clone()));
            }
            Step::Cancelled(id) => bridge.cancel(&id),
            Step::Nothing => {}
        }
    }
    // stdin closed: the agent is done. Calls still waiting end with the process.
}
