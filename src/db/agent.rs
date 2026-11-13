//! The app's end of the AI agents link: accepts bridges on the Worker's runtime and hands each authenticated call
//! to the App as a `Msg::Agent`, with a oneshot for the answer. Decisions are the App's; this only carries them.
//!
//! Below the link: the read operations an allowed call runs, capped by the agent limits and scrubbed by the
//! connection's rules before anything is turned into text, and the audit log's writer thread.

use super::*;
use crate::agents::audit::{Audit, Entry};
use crate::agents::config::{Limits, Scope};
use crate::agents::link::{self, ClientInfo, Listener, Token, ToolErr};
use crate::agents::policy::visible;
use crate::agents::scrub::{NsRules, Rules, scrub_error};
use crate::agents::tools::Call;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::BufReader;
use tokio::sync::{mpsc, oneshot, watch};

pub type Reply = oneshot::Sender<Result<Value, ToolErr>>;

pub enum AgentEvent {
    /// The endpoint is taken and serves bridges.
    Listening,
    /// Another Quarry serves the endpoint; this one tries again every few seconds.
    Busy,
    Failed(String),
    Opened { session: u64 },
    Closed { session: u64 },
    Call { session: u64, id: u64, tool: String, args: Value, client: ClientInfo, reply: Reply },
    Cancel { session: u64, id: u64 },
    /// An operation the App started finished; its answer already went to the agent.
    Done(Finished),
    /// An approval waited as long as the agent's client would; the App denies it if it is still open.
    Expired { activity: u64 },
    /// What a write would touch, read before the App decides on it.
    Prepared { activity: u64, res: Res<Snapshot> },
    /// The user's Undo call of an agent change finished. `back` is what is left to undo when it stopped early.
    Undone { activity: u64, res: Res<String>, back: Option<AgentUndo> },
    /// The last entries of the audit log, oldest first, read once when it opens.
    Tail(Vec<Entry>),
    /// List the collections of `db` again: a structure change Quarry stopped waiting for may have finished since.
    Refresh { conn: u64, db: String },
    /// The audit log could not be opened or written.
    AuditFailed(String),
}

/// How an allowed call ended, for the Activity list and the audit log. Holds counts and scrubbed text, never rows.
pub struct Finished {
    pub activity: u64,
    /// Scrubbed error text when it failed.
    pub error: Option<String>,
    pub summary: String,
    /// The start of the answer, scrubbed, for the detail view.
    pub answer: String,
    pub count: Option<u64>,
    pub bytes: Option<u64>,
    pub ms: u64,
    /// Field paths a MongoDB describe saw in its sample, as (db, coll, paths), for the free-text check of the policy.
    pub keys: Option<(String, String, Vec<String>)>,
    /// What undoes the change, kept by the App in memory only.
    pub undo: Option<AgentUndo>,
    /// Quarry stopped waiting at its time limit, but the change may still be running on the server.
    pub running: bool,
}

/// What an allowed call reads from, taken from its connection on the UI thread.
pub enum AgentTarget {
    Mongo(Client),
    Sql(sql::Db),
}

/// One allowed call, with everything it needs so it never looks back at App state.
pub struct AgentJob {
    pub activity: u64,
    /// The connection id, which a SQL undo action names.
    pub conn: u64,
    pub target: AgentTarget,
    pub call: Call,
    pub rules: Arc<Rules>,
    pub limits: Limits,
    pub scope: Scope,
    /// For writes: what the write was decided on, read just before.
    pub snap: Option<Snapshot>,
    /// Write exactly the snapshot and keep what undoes it. False runs the write on its filter, as the user allowed.
    pub keep_undo: bool,
    /// The connection's writes are unlocked.
    pub writable: bool,
    /// run_shell: the Bun job, built by the App, which alone may read the connection's address, and the secrets in it to
    /// take out of the output.
    pub shell: Option<(ShellJob, Redact)>,
}

/// What a write touches, read before it is decided. With `docs` the write runs on exactly these documents or rows, one at
/// a time while each is unchanged, so its undo knows every before and after.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// The documents or rows as read; None when undo cannot be kept, which `no_undo` says why.
    pub docs: Option<Vec<Document>>,
    /// How many documents or rows the write touches, for the large-change threshold.
    pub matched: u64,
    pub no_undo: Option<String>,
    /// SQL: the table's columns and primary key.
    pub table: Option<(Vec<sql::Col>, Vec<String>)>,
}

/// What undoes one agent change.
pub enum AgentUndo {
    /// Each document as it was and as the agent left it; the undo puts the first back only while the second is there.
    Updated { db: String, coll: String, docs: Vec<(Document, Document)> },
    /// Documents the agent deleted, inserted back by the undo.
    Deleted { db: String, coll: String, docs: Vec<Document> },
    /// Documents the agent inserted, removed by the undo only while unchanged.
    Inserted { db: String, coll: String, docs: Vec<Document> },
    /// A SQL row change, undone by the same code as the user's (`sql::write::undo_rows`).
    Sql(UndoAction),
}

/// An `io::Write` that only counts, to size an encoding without building it.
#[derive(Default)]
struct Count(u64);

impl std::io::Write for Count {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0 += b.len() as u64;
        Ok(b.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(in crate::db) fn doc_bytes(d: &Document) -> u64 {
    let mut n = Count::default();
    d.to_writer(&mut n).map_or(0, |()| n.0)
}

impl AgentUndo {
    pub fn bytes(&self) -> u64 {
        match self {
            AgentUndo::Updated { docs, .. } => docs.iter().map(|(a, b)| doc_bytes(a) + doc_bytes(b)).sum(),
            AgentUndo::Deleted { docs, .. } | AgentUndo::Inserted { docs, .. } => docs.iter().map(doc_bytes).sum(),
            AgentUndo::Sql(a) => match &a.kind {
                UndoKind::Sql { undo: sql::write::SqlUndo::Update(r), .. } => r.iter().map(|e| doc_bytes(&e.pk) + e.changes.iter().map(|(n, o, v)| n.len() as u64 + doc_bytes(&doc! { "o": o.clone(), "v": v.clone() })).sum::<u64>()).sum(),
                UndoKind::Sql { undo: sql::write::SqlUndo::Delete(r) | sql::write::SqlUndo::Insert(r) | sql::write::SqlUndo::Inserted(r), .. } => r.iter().map(doc_bytes).sum(),
                _ => 0,
            },
        }
    }

    /// "db.coll" or "schema.table".
    pub fn ns(&self) -> (String, String) {
        match self {
            AgentUndo::Updated { db, coll, .. } | AgentUndo::Deleted { db, coll, .. } | AgentUndo::Inserted { db, coll, .. } => (db.clone(), coll.clone()),
            AgentUndo::Sql(a) => (a.db.clone(), a.coll.clone()),
        }
    }
}

/// Why a change this size cannot keep its undo, or None when it can.
pub(in crate::db) fn too_big(matched: u64, bytes: u64, l: &Limits, what: &str) -> Option<String> {
    if matched > l.undo_docs as u64 {
        return Some(format!("Touches {matched} {what}, more than the {} Quarry keeps to undo an agent change.", l.undo_docs));
    }
    if bytes > l.undo_call_bytes {
        let mb = |b: u64| b.div_ceil(1 << 20);
        return Some(format!("Its {what} take about {} MB, more than the {} MB Quarry keeps to undo one agent change.", mb(bytes), mb(l.undo_call_bytes)));
    }
    None
}

/// The snapshot of a write that needs no read first: inserts know their documents, and $out or $merge never keep an undo.
pub fn snapshot_now(call: &Call, l: &Limits) -> Option<Snapshot> {
    match call {
        Call::Insert { docs, .. } => {
            let bytes = docs.iter().map(doc_bytes).sum();
            let matched = docs.len() as u64;
            let no_undo = too_big(matched, bytes, l, "documents");
            Some(Snapshot { docs: no_undo.is_none().then(|| docs.clone()), matched, no_undo, table: None })
        }
        Call::AggregateInto { .. } => Some(Snapshot { no_undo: Some("A pipeline that writes with $out or $merge can't be undone.".into()), ..Default::default() }),
        _ => None,
    }
}

/// A running call, kept so a cancel can stop it.
pub struct AgentRun {
    abort: tokio::task::AbortHandle,
    sql: Option<(sql::Db, sql::RunHandle)>,
}

impl AgentRun {
    /// Drops the operation, and for free SQL also stops the statement on the server, which dropping alone leaves running.
    pub fn cancel(self, w: &Worker) {
        self.abort.abort();
        if let Some((db, h)) = self.sql {
            w.rt.spawn(async move { sql::agent::stop(&db, &h).await });
        }
    }
}

/// Keeps the listener running; dropping it stops it, closes every session and removes the socket.
pub struct AgentsHandle {
    token: watch::Sender<Option<Token>>,
}

impl AgentsHandle {
    /// The token while agents are on, None while off: bridges are then told agents are off and open sessions close.
    pub fn set_token(&self, token: Option<Token>) {
        self.token.send_replace(token);
    }
}

const BUSY_RETRY: Duration = Duration::from_secs(5);

impl Worker {
    pub fn agents_listen(&self, dir: PathBuf, token: Option<Token>) -> AgentsHandle {
        let (tx, rx) = watch::channel(token);
        let out = Out { tx: self.tx.clone(), ctx: self.ctx.clone() };
        self.rt.spawn(listen(dir, rx, out));
        AgentsHandle { token: tx }
    }
}

#[derive(Clone)]
struct Out {
    tx: Sender<Msg>,
    ctx: eframe::egui::Context,
}

impl Out {
    fn send(&self, ev: AgentEvent) {
        let _ = self.tx.send(Msg::Agent(ev));
        // Wakes logic() even while the window is hidden, so calls are answered then too.
        self.ctx.request_repaint();
    }
}

async fn listen(dir: PathBuf, mut token: watch::Receiver<Option<Token>>, out: Out) {
    // Told once per change, not on every retry.
    let mut last = String::new();
    let mut l = loop {
        let (now, ev) = match Listener::bind(&dir).await {
            Ok(l) => break l,
            Err(link::BindErr::Busy) => ("busy".to_string(), AgentEvent::Busy),
            Err(link::BindErr::Io(e)) => (e.to_string(), AgentEvent::Failed(e.to_string())),
        };
        if now != last {
            out.send(ev);
            last = now;
        }
        tokio::select! {
            _ = tokio::time::sleep(BUSY_RETRY) => {}
            r = token.changed() => if r.is_err() { return },
        }
    };
    out.send(AgentEvent::Listening);
    static SESSIONS: AtomicU64 = AtomicU64::new(1);
    loop {
        tokio::select! {
            s = l.accept() => match s {
                Ok(s) => {
                    tokio::spawn(session(s, SESSIONS.fetch_add(1, Ordering::Relaxed), token.clone(), out.clone()));
                }
                // A client that went away mid-accept; pausing keeps a persistent failure from spinning.
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            },
            r = token.changed() => if r.is_err() { return },
        }
    }
}

async fn session(s: link::Stream, id: u64, mut token: watch::Receiver<Option<Token>>, out: Out) {
    let (r, mut w) = tokio::io::split(s);
    let mut r = BufReader::new(r);
    let current = *token.borrow_and_update();
    if !matches!(link::accept_hello(&mut r, &mut w, current).await, Ok(true)) {
        return;
    }
    out.send(AgentEvent::Opened { session: id });
    let (answers, mut rx) = mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        while let Some(v) = rx.recv().await {
            if link::write_msg(&mut w, &v).await.is_err() {
                break;
            }
        }
    });
    loop {
        tokio::select! {
            m = link::read_msg(&mut r) => {
                let Ok(Some(m)) = m else { break };
                if let Some(c) = m["cancel"].as_u64() {
                    out.send(AgentEvent::Cancel { session: id, id: c });
                    continue;
                }
                let (Some(call), Some(tool)) = (m["id"].as_u64(), m["tool"].as_str()) else { break };
                // Turned off between two calls: the session is closing anyway, but this call must not run.
                if token.borrow().is_none() {
                    let _ = answers.send(json!({"id": call, "err": ToolErr::new("off", "AI agents are off in Quarry Settings.")}));
                    continue;
                }
                let (reply, ans) = oneshot::channel();
                let client = serde_json::from_value(m["client"].clone()).unwrap_or_default();
                out.send(AgentEvent::Call { session: id, id: call, tool: tool.into(), args: m["args"].clone(), client, reply });
                let answers = answers.clone();
                tokio::spawn(async move {
                    let v = match ans.await {
                        Ok(Ok(v)) => json!({"id": call, "ok": v}),
                        Ok(Err(e)) => json!({"id": call, "err": e}),
                        Err(_) => json!({"id": call, "err": ToolErr::new("internal", "Quarry dropped this call without answering. Try again.")}),
                    };
                    let _ = answers.send(v);
                });
            }
            r = token.changed() => if r.is_err() || token.borrow().is_none() { break },
        }
    }
    writer.abort();
    out.send(AgentEvent::Closed { session: id });
}

impl Worker {
    /// Runs an allowed call on the runtime. The answer goes to the agent through `reply`, then `AgentEvent::Done` tells
    /// the App how it ended. A cancelled call sends neither: the App already marked it.
    pub fn agent_run(&self, job: AgentJob, reply: Reply) -> AgentRun {
        let out = Out { tx: self.tx.clone(), ctx: self.ctx.clone() };
        let h = sql::RunHandle::default();
        let sql = match &job.target {
            AgentTarget::Sql(db) => Some((db.clone(), h.clone())),
            AgentTarget::Mongo(_) => None,
        };
        let (activity, secs) = (job.activity, job.limits.secs.max(1) as u64);
        // A MongoDB write stops itself at the limit between two documents and keeps the undo of what it did; cutting it
        // off here would lose that, so its own margin is wider. A SQL write is one transaction, which a cut rolls back.
        let margin = if job.snap.is_some() { 60 } else { 5 };
        // A MongoDB structure change goes on on the server when Quarry stops waiting for it (an index build above all), so
        // past the limit it is reported as still running, never as failed.
        let lingers = matches!(job.target, AgentTarget::Mongo(_)) && matches!(job.call, Call::CreateCollection { .. } | Call::DropCollection { .. } | Call::CreateIndex { .. } | Call::DropIndex { .. });
        let start = Instant::now();
        let work = self.rt.spawn(async move {
            // The server's own limits (maxTimeMS, the statement timeout) come first; this one catches what they miss.
            match tokio::time::timeout(Duration::from_secs(secs + margin), exec(&job, &h)).await {
                Ok(r) => r,
                Err(_) if lingers => Ok(still_running(secs)),
                Err(_) => Err(format!("Stopped after {secs} s, the time limit for agent calls in Settings > AI agents.")),
            }
        });
        let abort = work.abort_handle();
        self.rt.spawn(async move {
            let res = match work.await {
                Ok(r) => r,
                Err(j) if j.is_cancelled() => return,
                Err(_) => Err("Quarry hit an internal error running this call. Try again.".to_string()),
            };
            let ms = start.elapsed().as_millis() as u64;
            let fin = match res {
                Ok(a) => {
                    let answer = cut(&a.value.to_string(), ANSWER_SHOWN);
                    // A write that stopped part way still answers with what it did, so the agent knows what changed.
                    let _ = reply.send(Ok(a.value));
                    Finished { activity, error: a.error, summary: a.summary, answer, count: a.count, bytes: Some(a.bytes), ms, keys: a.keys, undo: a.undo, running: a.running }
                }
                Err(err) => {
                    let _ = reply.send(Err(ToolErr::new("failed", err.clone())));
                    Finished { activity, error: Some(err), summary: "Failed".into(), answer: String::new(), count: None, bytes: None, ms, keys: None, undo: None, running: false }
                }
            };
            out.send(AgentEvent::Done(fin));
        });
        AgentRun { abort, sql }
    }

    /// Sends `ev` to the App after `after`, from the runtime, so it arrives while no frame is drawn.
    pub fn agent_after(&self, after: Duration, ev: AgentEvent) {
        let out = Out { tx: self.tx.clone(), ctx: self.ctx.clone() };
        self.rt.spawn(async move {
            tokio::time::sleep(after).await;
            out.send(ev);
        });
    }

    /// Opens the audit log on a thread of its own and returns where to send entries; file work never runs on the UI
    /// thread. With `tail` above zero the last entries come back first as `AgentEvent::Tail`.
    pub fn agents_audit(&self, dir: PathBuf, max_bytes: u64, tail: usize) -> std::sync::mpsc::Sender<Entry> {
        let (tx, rx) = std::sync::mpsc::channel::<Entry>();
        let out = Out { tx: self.tx.clone(), ctx: self.ctx.clone() };
        let spawned = std::thread::Builder::new().name("quarry-agents-audit".into()).spawn(move || {
            let mut a = match Audit::open(&dir, max_bytes) {
                Ok(a) => a,
                Err(e) => return out.send(AgentEvent::AuditFailed(format!("Could not open the AI agents audit log: {e}"))),
            };
            if tail > 0 {
                out.send(AgentEvent::Tail(a.read_tail(tail)));
            }
            // Told once: a full disk would otherwise raise one error per call.
            let mut told = false;
            while let Ok(e) = rx.recv() {
                if let Err(err) = a.append(&e)
                    && !std::mem::replace(&mut told, true)
                {
                    out.send(AgentEvent::AuditFailed(format!("Could not write the AI agents audit log: {err}")));
                }
            }
        });
        if let Err(e) = spawned {
            self.send(Msg::Agent(AgentEvent::AuditFailed(format!("Could not start the AI agents audit log: {e}"))));
        }
        tx
    }
}

/// Characters of an answer kept for the Activity detail view.
const ANSWER_SHOWN: usize = 2000;

pub(in crate::db) fn cut(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &s[..end])
}

/// What an operation answers: the value the agent gets, and what the Activity list and audit log keep about it.
#[derive(Debug)]
pub(in crate::db) struct Answer {
    pub value: Value,
    pub summary: String,
    pub count: Option<u64>,
    pub bytes: u64,
    pub keys: Option<(String, String, Vec<String>)>,
    pub undo: Option<AgentUndo>,
    /// A write that stopped part way: what stopped it. The value still says what was done.
    pub error: Option<String>,
    /// Quarry stopped waiting, but the server may still be running it.
    pub running: bool,
}

impl std::fmt::Debug for AgentUndo {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let (db, coll) = self.ns();
        write!(f, "AgentUndo({db}.{coll}, {} bytes)", self.bytes())
    }
}

impl Answer {
    pub fn new(value: Value, summary: String) -> Answer {
        let bytes = value.to_string().len() as u64;
        Answer { value, summary, count: None, bytes, keys: None, undo: None, error: None, running: false }
    }
}

/// A server error as an agent may see it: the values it quotes removed (duplicate keys, failing rows, DETAIL lines)
/// and the detectors run, then `db::explain`'s title in front when it says more than the message.
pub(in crate::db) fn failure(raw: &str, rules: &NsRules, what: &str) -> String {
    let s = scrub_error(raw, rules);
    let x = explain(&s, &ErrCtx { what: what.into(), ..Default::default() });
    let mut out = if x.title.is_empty() || s.starts_with(&x.title) { s } else { format!("{}: {s}", x.title) };
    if !x.hint.is_empty() {
        out.push_str(&format!("\n{}", x.hint));
    }
    out
}

/// Scrubs each document, turns it into relaxed Extended JSON and stops before the byte limit, on a row boundary.
/// `docs` holds up to `want` rows plus one, which only says more match. The agent is told what was left out.
pub(in crate::db) fn page_out(docs: Vec<Document>, want: usize, rules: &NsRules, limits: &Limits, key: &str) -> Answer {
    let more = docs.len() > want;
    let taken = docs.len().min(want);
    let max = limits.bytes as usize;
    let mut rows = Vec::with_capacity(taken);
    let mut bytes = 64;
    for mut d in docs.into_iter().take(want) {
        rules.scrub_doc(&mut d);
        let v = Bson::Document(d).into_relaxed_extjson();
        let mut c = Count::default();
        let n = serde_json::to_writer(&mut c, &v).map_or(0, |()| c.0 as usize) + 1;
        if bytes + n > max {
            break;
        }
        bytes += n;
        rows.push(v);
    }
    let returned = rows.len();
    let left = taken - returned;
    let mut value = json!({ key: rows, "returned": returned, "left_out": left, "more": more });
    if left > 0 {
        value["note"] = json!(format!("{left} more {key} were left out to stay under the {max} byte limit. Ask for fewer fields, or page with a smaller limit."));
    } else if more {
        value["note"] = json!(format!("More {key} match than the {returned} returned. Page with skip or offset, or narrow the filter."));
    }
    let summary = format!("{returned} {key}{}", if more || left > 0 { ", more left out" } else { "" });
    Answer { value, summary, count: Some(returned as u64), bytes: bytes as u64, keys: None, undo: None, error: None, running: false }
}

/// The error mapper of one Mongo call: the server's message scrubbed by the namespace's rules, with what was attempted.
fn mongo_err<'a>(nsr: &'a NsRules, what: &'a str) -> impl Fn(mongodb::error::Error) -> String + Copy + 'a {
    move |x| failure(&e(x), nsr, what)
}

/// Every dotted key path in the documents, arrays of documents included, for the policy's free-text check.
fn key_paths(docs: &[Document]) -> Vec<String> {
    fn walk(d: &Document, prefix: &str, depth: usize, out: &mut BTreeSet<String>) {
        for (k, v) in d {
            if out.len() >= KEY_PATHS_MAX {
                return;
            }
            let p = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
            match v {
                Bson::Document(sub) if depth < 16 => walk(sub, &p, depth + 1, out),
                Bson::Array(a) if depth < 16 => {
                    for e in a {
                        if let Bson::Document(sub) = e {
                            walk(sub, &p, depth + 1, out);
                        }
                    }
                }
                _ => {}
            }
            out.insert(p);
        }
    }
    let mut out = BTreeSet::new();
    for d in docs {
        walk(d, "", 0, &mut out);
    }
    out.into_iter().collect()
}

const KEY_PATHS_MAX: usize = 5000;
/// Documents a MongoDB describe samples.
const DESCRIBE_SAMPLE: i64 = 100;

/// Removes what explain output says about the servers rather than the query: host names, ports, versions, parameters.
fn strip_servers(d: &mut Document) {
    for k in ["serverInfo", "serverParameters", "$clusterTime", "operationTime", "host", "hostname", "connectionString"] {
        d.remove(k);
    }
    for (_, v) in d.iter_mut() {
        match v {
            Bson::Document(sub) => strip_servers(sub),
            Bson::Array(a) => a.iter_mut().filter_map(|e| if let Bson::Document(s) = e { Some(s) } else { None }).for_each(strip_servers),
            _ => {}
        }
    }
}

fn want(limit: Option<u32>, l: &Limits) -> usize {
    limit.unwrap_or(l.rows).min(l.rows).max(1) as usize
}

async fn exec(job: &AgentJob, h: &sql::RunHandle) -> Res<Answer> {
    use Call as C;
    let l = &job.limits;
    let t = Duration::from_secs(l.secs.max(1) as u64);
    let rules = &job.rules;
    let listing = |e: String| failure(&e, &rules.all(), "list them");
    match (&job.target, &job.call) {
        (AgentTarget::Mongo(c), C::ListDatabases { .. }) => {
            let names = c.list_database_names().await.map_err(|x| listing(e(x)))?;
            let names: Vec<String> = names.into_iter().filter(|n| !matches!(n.as_str(), "local" | "config") && visible(&job.scope, n, None).unwrap_or(false)).collect();
            let n = names.len();
            Ok(Answer::new(json!({ "databases": names }), format!("{n} databases")))
        }
        (AgentTarget::Mongo(c), C::ListCollections { db, .. }) => {
            let mut names = c.database(db).list_collection_names().await.map_err(|x| listing(e(x)))?;
            names.retain(|n| (!n.starts_with("system.") || n == "system.views") && visible(&job.scope, db, Some(n)).unwrap_or(false));
            names.sort();
            let n = names.len();
            Ok(Answer::new(json!({ "collections": names }), format!("{n} collections")))
        }
        (AgentTarget::Mongo(c), C::Describe { ns }) => describe(c, &ns.db, &ns.coll, rules, l, t).await,
        (AgentTarget::Mongo(c), C::Find { ns, filter, projection, sort, skip, limit }) => {
            let nsr = rules.for_ns(&ns.db, &ns.coll);
            let fail = mongo_err(&nsr, "find the documents");
            let n = want(*limit, l);
            let col = c.database(&ns.db).collection::<Document>(&ns.coll);
            let mut f = col.find(filter.clone()).skip(*skip).limit(n as i64 + 1).max_time(t);
            if let Some(s) = sort {
                f = f.sort(s.clone());
            }
            if let Some(p) = projection {
                f = f.projection(p.clone());
            }
            let docs: Vec<Document> = f.await.map_err(fail)?.try_collect().await.map_err(fail)?;
            Ok(page_out(docs, n, &nsr, l, "documents"))
        }
        (AgentTarget::Mongo(c), C::Aggregate { ns, pipeline, limit }) => {
            let nsr = rules.for_ns(&ns.db, &ns.coll);
            let fail = mongo_err(&nsr, "run the pipeline");
            let n = want(*limit, l);
            let mut p = pipeline.clone();
            p.push(doc! { "$limit": n as i64 + 1 });
            let docs: Vec<Document> = c.database(&ns.db).collection::<Document>(&ns.coll).aggregate(p).max_time(t).await.map_err(fail)?.try_collect().await.map_err(fail)?;
            Ok(page_out(docs, n, &nsr, l, "documents"))
        }
        (AgentTarget::Mongo(c), C::Count { ns, filter }) => {
            let nsr = rules.for_ns(&ns.db, &ns.coll);
            // Like the user's actions on matching documents, a count follows the filter alone, never a limit or skip.
            let n = c.database(&ns.db).collection::<Document>(&ns.coll).count_documents(filter.clone()).max_time(t).await.map_err(mongo_err(&nsr, "count the documents"))?;
            let mut a = Answer::new(json!({ "count": n }), format!("{n} documents"));
            a.count = Some(n);
            Ok(a)
        }
        (AgentTarget::Mongo(c), C::Explain { ns, filter, sort, pipeline }) => {
            let nsr = rules.for_ns(&ns.db, &ns.coll);
            let inner = match pipeline {
                Some(p) => doc! { "aggregate": &ns.coll, "pipeline": p.clone(), "cursor": {} },
                None => {
                    let mut d = doc! { "find": &ns.coll, "filter": filter.clone(), "limit": l.rows as i64 };
                    if let Some(s) = sort {
                        d.insert("sort", s.clone());
                    }
                    d
                }
            };
            // The planner's view only: executionStats would run the query in full.
            let cmd = doc! { "explain": inner, "verbosity": "queryPlanner", "maxTimeMS": t.as_millis() as i64 };
            let mut d = c.database(&ns.db).run_command(cmd).await.map_err(mongo_err(&nsr, "explain the query"))?;
            strip_servers(&mut d);
            let a = page_out(vec![d], 1, &nsr, l, "explain");
            if a.count == Some(0) {
                return Err(format!("The plan is longer than the {} byte limit for agent answers.", l.bytes));
            }
            let plan = a.value["explain"][0].clone();
            Ok(Answer::new(json!({ "explain": plan }), "Plan".into()))
        }
        (AgentTarget::Sql(db), C::ListDatabases { .. }) => sql::agent::databases(db, &job.scope).await.map_err(listing),
        (AgentTarget::Sql(db), C::ListCollections { db: schema, .. }) => sql::agent::tables(db, schema, &job.scope).await.map_err(listing),
        (AgentTarget::Sql(db), C::Describe { ns }) => sql::agent::describe(db, &ns.db, &ns.coll, rules).await,
        (AgentTarget::Sql(db), C::SelectRows { ns, columns, wher, order, limit, offset }) => sql::agent::select_rows(db, &ns.db, &ns.coll, columns, wher, order, *limit, *offset, rules, l).await,
        (AgentTarget::Sql(db), C::QuerySql { db: schema, sql, limit, .. }) => sql::agent::query_sql(db, schema.as_deref().unwrap_or(""), sql, *limit, rules, l, h).await,
        (AgentTarget::Mongo(c), C::Insert { .. } | C::Update { .. } | C::Delete { .. } | C::AggregateInto { .. }) => mongo_write(c, job, t).await,
        (AgentTarget::Sql(db), C::SqlInsertRows { .. } | C::SqlUpdateRows { .. } | C::SqlDeleteRows { .. }) => sql::agent::write(db, job).await,
        (AgentTarget::Mongo(c), C::CreateCollection { .. } | C::DropCollection { .. } | C::CreateIndex { .. } | C::DropIndex { .. }) => mongo_structure(c, &job.call, rules, t).await,
        (AgentTarget::Sql(db), C::CreateIndex { .. } | C::DropIndex { .. } | C::CreateTable { .. } | C::DropTable { .. }) => sql::agent::structure(db, &job.call, job.writable, rules, l, h).await,
        (AgentTarget::Mongo(c), C::RunCommand { db, command, .. }) => run_command(c, db, command, rules, l, t).await,
        (AgentTarget::Sql(db), C::RunSql { db: schema, sql, .. }) => sql::agent::run_sql(db, schema.as_deref().unwrap_or(""), sql, !job.writable, rules, l, h).await,
        (AgentTarget::Mongo(_), C::RunShell { .. }) => match &job.shell {
            Some((s, redact)) => agent_shell(s, redact, rules, l).await,
            None => Err("Quarry could not prepare the shell for this script.".into()),
        },
        _ => Err("This tool does not run on this kind of connection.".into()),
    }
}

impl Worker {
    /// Reads what a write would touch, so the App can count it against the large-change threshold and the undo caps
    /// before deciding. Answers with `AgentEvent::Prepared`.
    pub fn agent_prepare(&self, activity: u64, target: AgentTarget, call: Call, limits: Limits) {
        let out = Out { tx: self.tx.clone(), ctx: self.ctx.clone() };
        self.rt.spawn(async move {
            let t = Duration::from_secs(limits.secs.max(1) as u64);
            let work = async {
                match &target {
                    AgentTarget::Mongo(c) => prepare_mongo(c, &call, &limits, t).await,
                    AgentTarget::Sql(db) => sql::agent::prepare(db, &call, &limits).await,
                }
            };
            let res = match tokio::time::timeout(t + Duration::from_secs(5), work).await {
                Ok(r) => r,
                Err(_) => Err(format!("Reading what this would change took longer than {} s, the time limit for agent calls.", t.as_secs())),
            };
            out.send(AgentEvent::Prepared { activity, res });
        });
    }

    /// Reverts an agent change from the Activity window. Each document or row goes back only while it is as the agent
    /// left it; one changed since is left alone and counted. Answers with `AgentEvent::Undone`.
    pub fn agent_undo(&self, activity: u64, target: AgentTarget, u: AgentUndo) {
        let out = Out { tx: self.tx.clone(), ctx: self.ctx.clone() };
        self.rt.spawn(async move {
            let (res, back) = match (target, u) {
                (AgentTarget::Sql(db), AgentUndo::Sql(a)) => {
                    let pk = match &a.kind {
                        UndoKind::Sql { undo, .. } => undo.pk_names(),
                        _ => Vec::new(),
                    };
                    match sql::write::undo_rows(db, a, pk, false).await {
                        Msg::Written { res, undo, .. } => (res, undo.map(AgentUndo::Sql)),
                        _ => (Err("Quarry could not undo this change.".into()), None),
                    }
                }
                (AgentTarget::Mongo(c), u) => undo_mongo(&c, u).await,
                (_, u) => (Err("The connection is no longer of the kind this change was made on.".into()), Some(u)),
            };
            out.send(AgentEvent::Undone { activity, res, back });
        });
    }
}

/// The documents an update or delete matches, up to the undo cap and one more. Over the cap the write can't keep its
/// undo, and the matches are counted on their own for the large-change threshold.
async fn prepare_mongo(c: &Client, call: &Call, l: &Limits, t: Duration) -> Res<Snapshot> {
    let (ns, filter, many, update) = match call {
        Call::Update { ns, filter, many, .. } => (ns, filter, *many, true),
        Call::Delete { ns, filter, many } => (ns, filter, *many, false),
        _ => return snapshot_now(call, l).ok_or_else(|| "This call changes nothing that needs reading first.".to_string()),
    };
    let col = c.database(&ns.db).collection::<Document>(&ns.coll);
    let fail = |x: mongodb::error::Error| e(x);
    let cap = l.undo_docs as i64;
    let docs: Vec<Document> = col.find(filter.clone()).limit(if many { cap + 1 } else { 1 }).max_time(t).await.map_err(fail)?.try_collect().await.map_err(fail)?;
    let over = many && docs.len() as i64 > cap;
    let matched = if over { col.count_documents(filter.clone()).max_time(t).await.map_err(fail)? } else { docs.len() as u64 };
    // An update keeps two images of each document.
    let bytes = docs.iter().map(doc_bytes).sum::<u64>() * if update { 2 } else { 1 };
    let no_undo = too_big(matched, bytes, l, "documents");
    Ok(Snapshot { docs: no_undo.is_none().then_some(docs), matched, no_undo, table: None })
}

/// Agent writes on MongoDB. With the snapshot each document is written by its `_id` only while it is still as read, so a
/// document someone changed meanwhile is skipped and reported, never overwritten, and the undo holds exact images.
/// Without it (the user allowed the write without undo) the write runs on its filter like any other.
async fn mongo_write(c: &Client, job: &AgentJob, t: Duration) -> Res<Answer> {
    use mongodb::options::{ReturnDocument, UpdateModifications};
    let until = Some(Instant::now() + t);
    let late = || until.is_some_and(|u| Instant::now() >= u);
    let rules = &job.rules;
    let snap = job.snap.as_ref().filter(|_| job.keep_undo).and_then(|s| s.docs.as_ref());
    let kept = |n: usize| if n > 0 { "Undo call in Quarry's Agent activity can revert this." } else { "" };
    match &job.call {
        Call::Update { ns, filter, update, many } => {
            let nsr = rules.for_ns(&ns.db, &ns.coll);
            let fail = mongo_err(&nsr, "update the documents");
            let col = c.database(&ns.db).collection::<Document>(&ns.coll);
            let upd = || -> UpdateModifications {
                match update {
                    crate::agents::tools::Update::Doc(d) => UpdateModifications::Document(d.clone()),
                    crate::agents::tools::Update::Pipeline(p) => UpdateModifications::Pipeline(p.clone()),
                }
            };
            let Some(before) = snap else {
                let r = if *many { col.update_many(filter.clone(), upd()).await } else { col.update_one(filter.clone(), upd()).await }.map_err(fail)?;
                let mut a = Answer::new(json!({ "matched": r.matched_count, "modified": r.modified_count, "undo": false }), format!("{} changed, no undo", r.modified_count));
                a.count = Some(r.modified_count);
                return Ok(a);
            };
            let (mut pairs, mut same, mut skipped, mut stop) = (Vec::new(), 0u64, 0u64, None);
            for b in before {
                if late() {
                    stop = Some(format!("Stopped at the {} s time limit.", t.as_secs()));
                    break;
                }
                match col.find_one_and_update(unchanged(b), upd()).return_document(ReturnDocument::After).await {
                    Ok(Some(after)) if after == *b => same += 1,
                    Ok(Some(after)) => pairs.push((b.clone(), after)),
                    Ok(None) => skipped += 1,
                    Err(x) => {
                        stop = Some(fail(x));
                        break;
                    }
                }
            }
            let n = pairs.len();
            let mut v = json!({ "matched": before.len(), "modified": n, "already_as_asked": same, "skipped_changed": skipped, "undo": n > 0 });
            let mut note = Vec::new();
            if skipped > 0 {
                note.push(format!("{skipped} documents changed after Quarry read them and were left as they are."));
            }
            if let Some(s) = &stop {
                note.push(format!("Stopped after changing {n}: {s}"));
            }
            note.push(kept(n).to_string());
            v["note"] = json!(note.join(" ").trim());
            let mut a = Answer::new(v, format!("{n} changed{}", if skipped > 0 { format!(", {skipped} skipped") } else { String::new() }));
            a.count = Some(n as u64);
            a.error = stop;
            a.undo = (n > 0).then(|| AgentUndo::Updated { db: ns.db.clone(), coll: ns.coll.clone(), docs: pairs });
            Ok(a)
        }
        Call::Delete { ns, filter, many } => {
            let nsr = rules.for_ns(&ns.db, &ns.coll);
            let col = c.database(&ns.db).collection::<Document>(&ns.coll);
            let Some(before) = snap else {
                let r = if *many { col.delete_many(filter.clone()).await } else { col.delete_one(filter.clone()).await }.map_err(mongo_err(&nsr, "delete the documents"))?;
                let mut a = Answer::new(json!({ "deleted": r.deleted_count, "undo": false }), format!("{} deleted, no undo", r.deleted_count));
                a.count = Some(r.deleted_count);
                return Ok(a);
            };
            let (gone, stop) = super::delete_unchanged(&col, before.clone(), until).await;
            let stop = stop.map(|s| scrub_error(&s, &nsr));
            let n = gone.len();
            let skipped = if stop.is_none() { before.len() - n } else { 0 };
            let mut note = Vec::new();
            if skipped > 0 {
                note.push(format!("{skipped} documents changed or went after Quarry read them and were not deleted."));
            }
            if let Some(s) = &stop {
                note.push(format!("Stopped after deleting {n}: {s}"));
            }
            note.push(kept(n).to_string());
            let mut a = Answer::new(json!({ "matched": before.len(), "deleted": n, "skipped_changed": skipped, "undo": n > 0, "note": note.join(" ").trim() }), format!("{n} deleted{}", if skipped > 0 { format!(", {skipped} skipped") } else { String::new() }));
            a.count = Some(n as u64);
            a.error = stop;
            a.undo = (n > 0).then(|| AgentUndo::Deleted { db: ns.db.clone(), coll: ns.coll.clone(), docs: gone });
            Ok(a)
        }
        Call::Insert { ns, docs } => {
            let nsr = rules.for_ns(&ns.db, &ns.coll);
            let col = c.database(&ns.db).collection::<Document>(&ns.coll);
            let mut docs = docs.clone();
            // Ids made here rather than by the server, so the undo knows each inserted document whole.
            assign_ids(&mut docs, None);
            let total = docs.len();
            let (done, stop) = match col.insert_many(docs.clone()).ordered(true).await {
                Ok(_) => (total, None),
                Err(x) => (inserted_before(&x, total), Some(failure(&e(x), &nsr, "insert the documents"))),
            };
            docs.truncate(done);
            // The ids go back through the rules: an _id can be data, like an email.
            let ids: Vec<Value> = docs
                .iter()
                .map(|d| {
                    let mut id = doc! { "_id": d.get("_id").cloned().unwrap_or(Bson::Null) };
                    nsr.scrub_doc(&mut id);
                    Bson::Document(id).into_relaxed_extjson()["_id"].clone()
                })
                .collect();
            let keep = job.keep_undo && done > 0;
            let mut v = json!({ "inserted": done, "ids": ids, "undo": keep });
            if let Some(s) = &stop {
                v["note"] = json!(format!("Stopped after inserting {done} of {total}: {s}"));
            }
            let mut a = Answer::new(v, format!("{done} inserted"));
            a.count = Some(done as u64);
            a.error = stop;
            a.undo = keep.then(|| AgentUndo::Inserted { db: ns.db.clone(), coll: ns.coll.clone(), docs });
            Ok(a)
        }
        Call::AggregateInto { ns, pipeline } => {
            let nsr = rules.for_ns(&ns.db, &ns.coll);
            let fail = mongo_err(&nsr, "run the pipeline");
            let _: Vec<Document> = c.database(&ns.db).collection::<Document>(&ns.coll).aggregate(pipeline.clone()).max_time(t).await.map_err(fail)?.try_collect().await.map_err(fail)?;
            Ok(Answer::new(json!({ "ok": true, "undo": false }), "Pipeline written".into()))
        }
        _ => Err("Not a write.".into()),
    }
}

const STILL_RUNNING: &str = "Still running on the server; check again later.";

/// The answer to a MongoDB structure change Quarry stopped waiting for: not a failure, since the server goes on.
fn still_running(secs: u64) -> Answer {
    let note = format!("{STILL_RUNNING} Quarry stopped waiting after {secs} s, the time limit for agent calls, but the server may finish it. Call describe or list_collections to see.");
    let mut a = Answer::new(json!({ "still_running": true, "undo": false, "note": note }), STILL_RUNNING.into());
    a.running = true;
    a
}

/// Structure changes from agents on MongoDB, with the commands the user's own create and drop use, under the agent time
/// limit where the server takes one. None can be undone, which the approval said.
async fn mongo_structure(c: &Client, call: &Call, rules: &Rules, t: Duration) -> Res<Answer> {
    let Some(ns) = call.ns() else { return Err("Not a structure change.".into()) };
    let nsr = rules.for_ns(&ns.db, &ns.coll);
    let d = c.database(&ns.db);
    let col = d.collection::<Document>(&ns.coll);
    let done = match call {
        Call::CreateCollection { .. } => {
            // Since MongoDB 7 create succeeds on a collection that already exists, so the name is checked first.
            let taken = d.list_collection_names().filter(doc! { "name": &ns.coll }).await.map_err(mongo_err(&nsr, "create the collection"))?;
            if !taken.is_empty() {
                return Err(format!("{}.{} already exists.", ns.db, ns.coll));
            }
            let r = match d.run_command(doc! { "create": &ns.coll, "maxTimeMS": t.as_millis() as i64 }).await {
                // Servers older than 4.4 may refuse maxTimeMS on create, which they run at once anyway.
                Err(x) if e(&x).contains("maxTimeMS") => d.run_command(doc! { "create": &ns.coll }).await,
                r => r,
            };
            r.map_err(mongo_err(&nsr, "create the collection"))?;
            format!("Created collection {}.{}", ns.db, ns.coll)
        }
        Call::DropCollection { .. } => {
            col.drop().await.map_err(mongo_err(&nsr, "drop the collection"))?;
            format!("Dropped collection {}.{}", ns.db, ns.coll)
        }
        Call::CreateIndex { keys, name, unique, .. } => {
            let opts = mongodb::options::IndexOptions::builder().name(name.clone()).unique(unique.then_some(true)).build();
            let r = col.create_index(mongodb::IndexModel::builder().keys(keys.clone()).options(opts).build()).max_time(t).await.map_err(mongo_err(&nsr, "create the index"))?;
            format!("Created index {}", r.index_name)
        }
        Call::DropIndex { name, .. } => {
            col.drop_index(name).max_time(t).await.map_err(mongo_err(&nsr, "drop the index"))?;
            format!("Dropped index {name}")
        }
        _ => return Err("Not a structure change.".into()),
    };
    Ok(Answer::new(json!({ "ok": true, "done": done, "undo": false }), done))
}

/// Commands that answer at once and take no maxTimeMS of their own.
const QUICK_COMMANDS: [&str; 5] = ["ping", "hello", "isMaster", "buildInfo", "connectionStatus"];

/// run_command, once the guard passed it: the plain command path with the time limit, and the reply scrubbed by the rules
/// of the collection it names, documents of a cursor one by one.
async fn run_command(c: &Client, db: &str, command: &Document, rules: &Rules, l: &Limits, t: Duration) -> Res<Answer> {
    let name = command.keys().next().cloned().unwrap_or_default();
    let coll = command.get_str(&name).unwrap_or("").to_string();
    let nsr = if coll.is_empty() { rules.all() } else { rules.for_ns(db, &coll) };
    let mut cmd = command.clone();
    if !QUICK_COMMANDS.iter().any(|q| q.eq_ignore_ascii_case(&name)) && !cmd.contains_key("maxTimeMS") {
        cmd.insert("maxTimeMS", t.as_millis() as i64);
    }
    let mut r = c.database(db).run_command(cmd).await.map_err(|x| failure(&e(x), &nsr, &format!("run {name}")))?;
    strip_servers(&mut r);
    if let Ok(batch) = r.get_document_mut("cursor").and_then(|c| c.get_array_mut("firstBatch")) {
        for d in batch.iter_mut() {
            if let Bson::Document(d) = d {
                nsr.scrub_doc(d);
            }
        }
    }
    let a = page_out(vec![r], 1, &nsr, l, "reply");
    if a.count == Some(0) {
        return Err(format!("The reply is longer than the {} byte limit for agent answers. Narrow the command.", l.bytes));
    }
    let reply = a.value["reply"][0].clone();
    Ok(Answer::new(json!({ "reply": reply }), format!("{name} ran")))
}

/// Literal secrets a run_shell script can read from the address it runs with: the password, user:password and the whole
/// URI, as written and percent-decoded, and the proxy password. They are replaced in the shell's output before the agent
/// sees it. A cheap net, not a boundary: a script can still encode them. Debug shows only how many there are.
#[derive(Clone, Default)]
pub struct Redact(Vec<String>);

impl std::fmt::Debug for Redact {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "Redact({})", self.0.len())
    }
}

/// A bare password shorter than this would blank out ordinary text all over the output; it is still hidden inside
/// user:password and the URI.
const REDACT_MIN: usize = 3;
const REDACTED: &str = "[hidden]";

/// The user and password of a `scheme://user:password@host/...` address, as written. A password may hold `@` or `/`
/// when it was not encoded, so the last `@` before the options ends it.
fn userinfo(uri: &str) -> Option<(&str, &str)> {
    let rest = uri.split_once("://")?.1;
    let rest = rest.split_once('?').map_or(rest, |(a, _)| a);
    let (info, _) = rest.rsplit_once('@')?;
    info.split_once(':')
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let hex = |c: u8| (c as char).to_digit(16);
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl Redact {
    /// `extra` holds other secrets the runner gets, such as a proxy password.
    pub fn new(uri: &str, extra: &[String]) -> Redact {
        let mut v = vec![uri.to_string(), pct_decode(uri)];
        if let Some((user, pass)) = userinfo(uri)
            && !pass.is_empty()
        {
            for (u, p) in [(user.to_string(), pass.to_string()), (pct_decode(user), pct_decode(pass))] {
                v.push(format!("{u}:{p}"));
                if p.chars().count() >= REDACT_MIN {
                    v.push(p);
                }
            }
        }
        v.extend(extra.iter().filter(|s| s.chars().count() >= REDACT_MIN).cloned());
        v.retain(|s| !s.is_empty());
        // Longest first, so the URI goes whole before the password inside it.
        v.sort_by_key(|s| std::cmp::Reverse(s.len()));
        v.dedup();
        Redact(v)
    }

    pub fn apply(&self, s: &str) -> String {
        let mut out = s.to_string();
        for x in &self.0 {
            if out.contains(x.as_str()) {
                out = out.replace(x.as_str(), REDACTED);
            }
        }
        out
    }

    /// The longest secret, so text cut for the byte limit is cleaned this far past the cut and never shows half a one.
    fn longest(&self) -> usize {
        self.0.first().map_or(0, String::len)
    }
}

/// The start of `s`, at most `n` bytes, on a char boundary.
fn head(s: &str, n: usize) -> &str {
    if s.len() <= n {
        return s;
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Cleans the start of `s`, about `n` bytes of it. A longer `s` is cleaned `margin` bytes further, then that margin is
/// dropped again: a secret cut by the end is replaced whole or not kept at all, never half shown. The second value says
/// whether `s` was cut; the text may still run past `n`.
fn clean_head(s: &str, n: usize, margin: usize, clean: &dyn Fn(&str) -> String) -> (String, bool) {
    let src = head(s, n.saturating_add(margin));
    let mut t = clean(src);
    let was_cut = src.len() < s.len();
    if was_cut {
        let keep = head(&t, t.len().saturating_sub(margin)).len();
        t.truncate(keep);
    }
    (t, was_cut)
}

/// The log of an agent's script, kept under the byte limit as it arrives, so a script that prints without end never
/// holds more than the limit here. Lines are cleaned (secrets, then the value rules) with a margin past any cut.
#[derive(Default)]
struct ShellLog {
    lines: Vec<String>,
    bytes: usize,
    /// Lines left out or cut, and about how many bytes of them.
    cut_lines: usize,
    cut_bytes: usize,
    /// The last line was cut, so nothing after it is kept and dropping it later does not count it twice.
    partial: bool,
}

impl ShellLog {
    fn push(&mut self, text: &str, max: usize, margin: usize, clean: &dyn Fn(&str) -> String) {
        let room = if self.partial { 0 } else { max.saturating_sub(self.bytes) };
        if room == 0 {
            self.cut_lines += 1;
            self.cut_bytes += text.len();
            return;
        }
        let (t, was_cut) = clean_head(text, room, margin, clean);
        if !was_cut && t.len() <= room {
            self.bytes += t.len();
            self.lines.push(t);
            return;
        }
        let kept = head(&t, room).to_string();
        self.cut_lines += 1;
        self.cut_bytes += text.len().saturating_sub(kept.len());
        self.bytes += kept.len();
        self.lines.push(kept);
        self.partial = true;
    }

    /// Drops the newest lines until `keep` more bytes fit under `max`: the log gives way before the result.
    fn make_room(&mut self, keep: usize, max: usize) {
        while self.bytes + keep > max {
            let Some(l) = self.lines.pop() else { break };
            self.bytes -= l.len();
            self.cut_bytes += l.len();
            if !std::mem::take(&mut self.partial) {
                self.cut_lines += 1;
            }
        }
    }
}

/// run_shell: the user's own Bun runner on a job the App built, which the policy runs only on an unlocked connection.
/// Its output is free text no field rule can follow, so the connection's secrets are replaced and the value detectors
/// run over it, which is best effort. Log and result together stay under the byte limit. Dropping this future (a cancel
/// or the time limit) kills the Bun process.
async fn agent_shell(job: &ShellJob, redact: &Redact, rules: &Rules, l: &Limits) -> Res<Answer> {
    let max = l.bytes as usize;
    let nsr = rules.all();
    let clean = |s: &str| nsr.scrub_text(&redact.apply(s));
    let margin = redact.longest().max(256);
    let log = std::sync::Mutex::new(ShellLog::default());
    let send = |m: Msg| {
        if let Msg::ShellLog { text, .. } = m
            && let Ok(mut g) = log.lock()
        {
            g.push(&text, max, margin, &clean);
        }
    };
    // Held until the script ends: a dropped sender reads as Stop in the runner.
    let (_stop, cancel) = tokio::sync::oneshot::channel::<()>();
    let res = run_shell(0, job, &send, cancel).await;
    let log = log.into_inner().unwrap_or_default();
    shell_answer(res, log, &clean, max, margin)
}

/// The agent's answer from a script's result and log, cleaned and held to `max` bytes in all.
fn shell_answer(res: Res<String>, mut log: ShellLog, clean: &dyn Fn(&str) -> String, max: usize, margin: usize) -> Res<Answer> {
    let note = "Quarry replaced this connection's secrets and checked the output with its value detectors only, not field rules, so treat it as best effort.";
    match res {
        Ok(out) => {
            let (mut result, was_cut) = clean_head(&out, max, margin, clean);
            let result_cut = was_cut || result.len() > max;
            if result.len() > max {
                result = cut(&result, max.saturating_sub(3));
            }
            log.make_room(result.len(), max);
            let lines = log.lines.len();
            let mut notes = vec![note.to_string()];
            if result_cut {
                notes.push(format!("The result was cut to the {max} byte limit. Return less, or page through it."));
            }
            let mut v = json!({ "result": result, "log": log.lines });
            if log.cut_lines > 0 {
                notes.push(format!("{} log lines, about {} bytes, were cut or left out to stay under the {max} byte limit.", log.cut_lines, log.cut_bytes));
                v["log_left_out"] = json!({ "lines": log.cut_lines, "bytes": log.cut_bytes });
            }
            v["note"] = json!(notes.join(" "));
            Ok(Answer::new(v, format!("Script ran, {lines} log lines")))
        }
        Err(err) => {
            let err = clean_head(&err, max, margin, clean).0;
            let tail = log.lines.iter().rev().take(5).rev().cloned().collect::<Vec<_>>().join("\n");
            Err(if tail.is_empty() { err } else { format!("{err}\nLast output:\n{tail}") })
        }
    }
}

/// Reverts a MongoDB agent change, each document only while it is still as the agent left it. Stops at the first
/// server error and hands back what it did not reach.
async fn undo_mongo(c: &Client, u: AgentUndo) -> (Res<String>, Option<AgentUndo>) {
    let (db, coll) = u.ns();
    let col = c.database(&db).collection::<Document>(&coll);
    let (mut done, mut skipped) = (0usize, 0usize);
    let said = |done: usize, skipped: usize, verb: &str| -> Res<String> {
        let changed = if skipped > 0 { format!("; {skipped} changed since the agent's change and were left as they are") } else { String::new() };
        if done == 0 && skipped > 0 { Err(format!("Nothing was undone: every document changed since the agent's change{changed}.")) } else { Ok(format!("Undo {verb} {done} documents in {db}.{coll}{changed}")) }
    };
    match u {
        AgentUndo::Updated { docs, .. } => {
            for (i, (before, after)) in docs.iter().enumerate() {
                match col.replace_one(unchanged(after), before.clone()).await {
                    Ok(r) if r.matched_count == 1 => done += 1,
                    Ok(_) => skipped += 1,
                    Err(x) => return (Err(format!("Undo stopped after {done} documents: {}", e(x))), Some(AgentUndo::Updated { db, coll, docs: docs[i..].to_vec() })),
                }
            }
            (said(done, skipped, "restored"), None)
        }
        AgentUndo::Deleted { docs, .. } => {
            for (i, d) in docs.iter().enumerate() {
                match col.insert_one(d.clone()).await {
                    Ok(_) => done += 1,
                    // A document under the same _id was made since; it is not the agent's to replace.
                    Err(x) if e(&x).contains("E11000") => skipped += 1,
                    Err(x) => return (Err(format!("Undo stopped after {done} documents: {}", e(x))), Some(AgentUndo::Deleted { db, coll, docs: docs[i..].to_vec() })),
                }
            }
            (said(done, skipped, "put back"), None)
        }
        AgentUndo::Inserted { docs, .. } => {
            for (i, d) in docs.iter().enumerate() {
                match col.delete_one(unchanged(d)).await {
                    Ok(r) if r.deleted_count == 1 => done += 1,
                    Ok(_) => skipped += 1,
                    Err(x) => return (Err(format!("Undo stopped after {done} documents: {}", e(x))), Some(AgentUndo::Inserted { db, coll, docs: docs[i..].to_vec() })),
                }
            }
            (said(done, skipped, "removed"), None)
        }
        u @ AgentUndo::Sql(_) => (Err("This change was made on a SQL connection.".into()), Some(u)),
    }
}

/// A MongoDB collection's fields from a sample of scrubbed documents, its indexes with their filters scrubbed, and its
/// estimated count. The unscrubbed key paths go back to the App for the free-text check, never to the agent.
async fn describe(c: &Client, db: &str, coll: &str, rules: &Rules, l: &Limits, t: Duration) -> Res<Answer> {
    let nsr = rules.for_ns(db, coll);
    let fail = mongo_err(&nsr, "sample the collection");
    let col = c.database(db).collection::<Document>(coll);
    let mut sample: Vec<Document> = col.find(doc! {}).limit(DESCRIBE_SAMPLE).max_time(t).await.map_err(fail)?.try_collect().await.map_err(fail)?;
    let keys = key_paths(&sample);
    for d in &mut sample {
        nsr.scrub_doc(d);
    }
    let stats = analyze_schema(&sample, &[]);
    let fields: Vec<Value> = stats
        .iter()
        .map(|f| {
            let mut v = json!({ "path": f.path, "in_documents": f.docs, "types": f.types.iter().map(|(t, n)| json!({ "type": t, "count": n })).collect::<Vec<_>>() });
            if nsr.hides(&f.path) {
                v["hidden"] = json!(true);
            } else if nsr.masks(&f.path) {
                v["masked"] = json!(true);
            }
            if !f.examples.is_empty() {
                v["examples"] = json!(f.examples.iter().take(3).map(|x| cut(x, 80)).collect::<Vec<_>>());
            }
            v
        })
        .collect();
    // A view has no indexes and no count of its own; it still describes.
    let indexes: Vec<Value> = match c.database(db).run_command(doc! { "listIndexes": coll }).await {
        Ok(mut r) => r
            .get_document_mut("cursor")
            .and_then(|c| c.get_array_mut("firstBatch"))
            .map(std::mem::take)
            .unwrap_or_default()
            .iter()
            .filter_map(Bson::as_document)
            .map(|spec| {
                let mut d = index_doc(spec);
                // A partial index's filter holds literals from the data.
                if let Ok(p) = d.get_document_mut("partialFilterExpression") {
                    nsr.scrub_doc(p);
                }
                Bson::Document(d).into_relaxed_extjson()
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    let count = col.estimated_document_count().max_time(t).await.ok();
    let mut value = json!({ "collection": format!("{db}.{coll}"), "estimated_count": count, "sampled": sample.len(), "fields": fields, "indexes": indexes });
    // Wide collections can outgrow the byte limit; fields go first since indexes are fewer and say more.
    let max = l.bytes.max(1024) as usize;
    while value.to_string().len() > max && value["fields"].as_array().is_some_and(|a| !a.is_empty()) {
        if let Some(a) = value["fields"].as_array_mut() {
            let keep = a.len() * 3 / 4;
            a.truncate(keep);
        }
        value["note"] = json!(format!("Some fields were left out to stay under the {max} byte limit."));
    }
    let mut a = Answer::new(value, format!("{} fields", stats.len()));
    a.keys = Some((db.to_string(), coll.to_string(), keys));
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::config::{Action, Matcher, PresetMode, PresetOverride, Presets, RuleSpec};
    use crate::agents::tools::{ConnRef, Ns};

    fn rules() -> Arc<Rules> {
        let pw = RuleSpec { on: true, matcher: Matcher::FieldName("^password$".into()), scope: Vec::new(), action: Action::Replace("hidden".into()), mode: PresetMode::Hide };
        Arc::new(Rules::compile(&Presets::default(), &PresetOverride::default(), &[pw], &[], &[5; 32]).unwrap())
    }

    #[test]
    fn page_out_caps_rows_and_bytes_on_row_boundaries() {
        let r = rules();
        let nsr = r.for_ns("d", "c");
        let docs: Vec<Document> = (0..11).map(|i| doc! { "_id": i, "password": "p", "pad": "x".repeat(300) }).collect();
        let a = page_out(docs.clone(), 10, &nsr, &Limits { bytes: 1024, ..Limits::default() }, "documents");
        let rows = a.value["documents"].as_array().unwrap();
        assert!(rows.len() < 10 && !rows.is_empty());
        assert_eq!(a.value["left_out"], json!(10 - rows.len()));
        assert_eq!(a.value["more"], json!(true));
        assert!(rows.iter().all(|r| r["password"] == json!("hidden")));
        let a = page_out(docs[..3].to_vec(), 10, &nsr, &Limits::default(), "documents");
        assert_eq!((a.value["returned"].clone(), a.value["more"].clone(), a.value.get("note").is_none()), (json!(3), json!(false), true));
    }

    #[test]
    fn explain_loses_server_names() {
        let mut d = doc! { "queryPlanner": { "winningPlan": { "shards": [{ "shardName": "a", "connectionString": "rs/db1:27017", "serverInfo": { "host": "db1" } }] } }, "serverInfo": { "host": "db1", "port": 27017 }, "ok": 1 };
        strip_servers(&mut d);
        let s = d.to_string();
        assert!(!s.contains("db1") && s.contains("shardName"), "{s}");
    }

    #[test]
    fn key_paths_go_through_arrays() {
        let k = key_paths(&[doc! { "a": 1, "b": { "c": 1 }, "items": [{ "secret": 1 }, 2] }]);
        assert_eq!(k, ["a", "b", "b.c", "items", "items.secret"]);
    }

    /// A worker and a client of QUARRY_TEST_MONGO, with 30 documents in a collection of this test's own, or None to skip.
    fn live(coll: &str) -> Option<(Worker, std::sync::mpsc::Receiver<Msg>, Client)> {
        let uri = std::env::var("QUARRY_TEST_MONGO").ok()?;
        let (tx, rx) = std::sync::mpsc::channel();
        let w = Worker::new(tx, eframe::egui::Context::default());
        let client = w.rt.block_on(async {
            let c = Client::with_uri_str(&uri).await.unwrap();
            let col = c.database("quarry_test").collection::<Document>(coll);
            col.drop().await.unwrap();
            let docs: Vec<Document> = (0..30).map(|i| doc! { "_id": i, "email": format!("user{i}@example.org"), "password": format!("pw{i}"), "n": i, "pad": "y".repeat(200) }).collect();
            col.insert_many(docs).await.unwrap();
            let partial = mongodb::options::IndexOptions::builder().partial_filter_expression(doc! { "email": { "$eq": "user1@example.org" } }).build();
            col.create_index(mongodb::IndexModel::builder().keys(doc! { "n": 1 }).options(partial).build()).await.unwrap();
            c
        });
        Some((w, rx, client))
    }

    fn job(c: &Client, call: Call, limits: Limits) -> AgentJob {
        AgentJob { activity: 7, conn: 1, target: AgentTarget::Mongo(c.clone()), call, rules: rules(), limits, scope: Scope::default(), snap: None, keep_undo: false, writable: false, shell: None }
    }

    fn run(w: &Worker, rx: &std::sync::mpsc::Receiver<Msg>, c: &Client, call: Call, limits: Limits) -> (Result<Value, ToolErr>, Finished) {
        let (reply, ans) = oneshot::channel();
        w.agent_run(job(c, call, limits), reply);
        let res = w.rt.block_on(ans).unwrap();
        let fin = loop {
            if let Msg::Agent(AgentEvent::Done(f)) = rx.recv_timeout(Duration::from_secs(20)).unwrap() {
                break f;
            }
        };
        (res, fin)
    }

    fn ns(coll: &str) -> Ns {
        Ns { conn: ConnRef::Id(1), db: "quarry_test".into(), coll: coll.into() }
    }

    fn find(coll: &str, filter: Document, limit: Option<u32>) -> Call {
        Call::Find { ns: ns(coll), filter, projection: None, sort: Some(doc! { "n": 1 }), skip: 0, limit }
    }

    #[test]
    fn live_mongo_reads_are_scrubbed_and_capped() {
        let Some((w, rx, c)) = live("agent_reads") else { return };
        let (res, fin) = run(&w, &rx, &c, find("agent_reads", doc! { "n": { "$gte": 5 } }, Some(4)), Limits::default());
        let v = res.unwrap();
        let docs = v["documents"].as_array().unwrap();
        assert_eq!(docs.len(), 4);
        assert_eq!(docs[0]["n"], json!(5));
        assert_eq!(docs[0]["password"], json!("hidden"));
        assert!(!docs[0]["email"].as_str().unwrap().starts_with("user5"), "email is masked: {}", docs[0]);
        assert_eq!(v["more"], json!(true));
        assert_eq!((fin.activity, fin.count, fin.error.is_none()), (7, Some(4), true));
        assert!(!fin.answer.contains("pw5"));
        // The row cap of the settings wins over a larger limit.
        let (res, _) = run(&w, &rx, &c, find("agent_reads", doc! {}, Some(100)), Limits { rows: 3, ..Limits::default() });
        assert_eq!(res.unwrap()["documents"].as_array().unwrap().len(), 3);
        // The byte cap cuts on a document and says how many were left out.
        let (res, _) = run(&w, &rx, &c, find("agent_reads", doc! {}, None), Limits { bytes: 2048, ..Limits::default() });
        let v = res.unwrap();
        let n = v["documents"].as_array().unwrap().len();
        assert!(n > 0 && n < 30, "{n}");
        assert!(v["left_out"].as_u64().unwrap() > 0);
        let agg = Call::Aggregate { ns: ns("agent_reads"), pipeline: vec![doc! { "$match": { "n": { "$lt": 3 } } }, doc! { "$sort": { "n": -1 } }], limit: None };
        let v = run(&w, &rx, &c, agg, Limits::default()).0.unwrap();
        let docs = v["documents"].as_array().unwrap();
        assert_eq!(docs.iter().map(|d| d["n"].clone()).collect::<Vec<_>>(), [json!(2), json!(1), json!(0)]);
        assert_eq!(docs[0]["password"], json!("hidden"));
        let count = Call::Count { ns: ns("agent_reads"), filter: doc! { "n": { "$gte": 10 } } };
        assert_eq!(run(&w, &rx, &c, count, Limits::default()).0.unwrap()["count"], json!(20));
        let explain = Call::Explain { ns: ns("agent_reads"), filter: doc! { "email": "user1@example.org" }, sort: None, pipeline: None };
        let v = run(&w, &rx, &c, explain, Limits::default()).0.unwrap();
        let s = v.to_string();
        assert!(s.contains("queryPlanner") && !s.contains("user1@example.org") && !s.contains("serverInfo"), "{s}");
        let (res, fin) = run(&w, &rx, &c, Call::Describe { ns: ns("agent_reads") }, Limits::default());
        let v = res.unwrap();
        let pw = v["fields"].as_array().unwrap().iter().find(|f| f["path"] == "password").unwrap().clone();
        assert_eq!(pw["hidden"], json!(true));
        assert!(!v.to_string().contains("pw1\"") && !v.to_string().contains("user1@example.org"), "{v}");
        assert_eq!(v["estimated_count"], json!(30));
        assert!(fin.keys.unwrap().2.contains(&"password".to_string()));
        let dbs = run(&w, &rx, &c, Call::ListDatabases { conn: ConnRef::Id(1) }, Limits::default()).0.unwrap();
        assert!(dbs["databases"].as_array().unwrap().contains(&json!("quarry_test")) && !dbs.to_string().contains("\"local\""));
        let colls = run(&w, &rx, &c, Call::ListCollections { conn: ConnRef::Id(1), db: "quarry_test".into() }, Limits::default()).0.unwrap();
        assert!(colls["collections"].as_array().unwrap().contains(&json!("agent_reads")));
    }

    #[test]
    fn live_mongo_errors_lose_their_values() {
        let Some((w, rx, c)) = live("agent_errors") else { return };
        // A real duplicate key error, made outside the agent path, through the agent's error wording.
        let raw = w.rt.block_on(async { c.database("quarry_test").collection::<Document>("agent_errors").insert_one(doc! { "_id": 1, "email": "dup@example.org" }).await.unwrap_err() });
        let msg = failure(&e(raw), &rules().all(), "insert");
        assert!(msg.contains("E11000") && !msg.contains("dup@example.org"), "{msg}");
        let (res, fin) = run(&w, &rx, &c, find("agent_errors", doc! { "n": { "$bogus": "s3cret@example.org" } }, None), Limits::default());
        let err = res.unwrap_err().message;
        assert!(!err.contains("s3cret@example.org"), "{err}");
        assert!(fin.error.is_some());
    }

    #[test]
    fn live_a_cancelled_run_answers_nothing_and_reports_nothing() {
        let Some((w, rx, c)) = live("agent_cancel") else { return };
        let slow = find("agent_cancel", doc! { "$where": "sleep(3000) || true" }, Some(1));
        let (reply, ans) = oneshot::channel();
        let run = w.agent_run(job(&c, slow, Limits::default()), reply);
        run.cancel(&w);
        assert!(w.rt.block_on(ans).is_err(), "the reply is dropped, never sent");
        assert!(!std::iter::from_fn(|| rx.recv_timeout(Duration::from_millis(500)).ok()).any(|m| matches!(m, Msg::Agent(AgentEvent::Done(_)))));
    }

    #[test]
    fn redact_takes_the_password_pair_and_uri_out() {
        let uri = "mongodb://app:p%40ss%2Fw0rd@h1:27017,h2/shop?authSource=admin";
        let r = Redact::new(uri, &["proxy-secret".to_string(), "ab".to_string()]);
        assert!(!format!("{r:?}").contains("w0rd"), "Debug never shows them");
        let text = format!("{uri} | app:p%40ss%2Fw0rd | app:p@ss/w0rd | p@ss/w0rd | p%40ss%2Fw0rd | proxy-secret | ab");
        let out = r.apply(&text);
        for s in ["p@ss/w0rd", "p%40ss%2Fw0rd", "proxy-secret", "h1:27017"] {
            assert!(!out.contains(s), "{s} in {out}");
        }
        assert!(out.ends_with("| ab"), "a two letter secret is too short to take out everywhere: {out}");
        assert_eq!(Redact::new("mongodb://h1/shop", &[]).apply("h1 is up"), "h1 is up");
        assert_eq!(userinfo("mongodb://u:a@b@h/db"), Some(("u", "a@b")));
        assert_eq!(userinfo("mongodb://h/db?x=a@b"), None);
    }

    /// Past the outer time limit a MongoDB structure change answers that it is still running, not that it failed.
    #[test]
    fn a_structure_change_past_the_limit_answers_still_running() {
        let (tx, rx) = std::sync::mpsc::channel();
        let w = Worker::new(tx, eframe::egui::Context::default());
        let c = w.rt.block_on(Client::with_uri_str("mongodb://127.0.0.1:9/?serverSelectionTimeoutMS=60000")).unwrap();
        let call = Call::CreateIndex { ns: ns("x"), keys: doc! { "a": 1 }, name: None, unique: false };
        let (reply, ans) = oneshot::channel();
        w.agent_run(AgentJob { writable: true, ..job(&c, call, Limits { secs: 1, ..Limits::default() }) }, reply);
        let v = w.rt.block_on(ans).unwrap().unwrap();
        assert_eq!(v["still_running"], json!(true), "{v}");
        assert!(v["note"].as_str().unwrap().starts_with(STILL_RUNNING));
        let fin = event(&rx, |e| if let AgentEvent::Done(f) = e { Some(f) } else { None });
        assert!(fin.running && fin.error.is_none(), "{:?}", fin.error);
        // Anything else past the limit is still a failure.
        let (reply, ans) = oneshot::channel();
        w.agent_run(job(&c, find("x", doc! {}, None), Limits { secs: 1, ..Limits::default() }), reply);
        assert!(w.rt.block_on(ans).unwrap().is_err());
    }

    /// The log is cut as it arrives and gives way to the result, so log and result together stay under the limit, and a
    /// secret split by a cut is still taken out.
    #[test]
    fn shell_output_stays_under_the_byte_limit() {
        let r = Redact::new("mongodb://app:Sup3rSecret@h/db", &[]);
        let clean = |s: &str| r.apply(s);
        let max = 1000;
        let margin = r.longest().max(256);
        let mut log = ShellLog::default();
        for i in 0..10 {
            log.push(&format!("line {i} {}", "x".repeat(40)), max, margin, &clean);
        }
        assert_eq!(log.bytes, 470);
        // One huge line, its secret right across where the cut falls, then lines that no longer fit.
        let huge = format!("{}Sup3rSecret{}", "y".repeat(525), "z".repeat(5_000_000));
        log.push(&huge, max, margin, &clean);
        for _ in 0..5 {
            log.push("after", max, margin, &clean);
        }
        assert!(log.bytes <= max, "{}", log.bytes);
        assert!(log.lines.iter().all(|l| !l.contains("Sup3r")), "half a secret never shows");
        assert_eq!(log.lines.len(), 11);
        let a = shell_answer(Ok(format!("\"{}\"", "r".repeat(600))), log, &clean, max, margin).unwrap();
        let total = a.value["result"].as_str().unwrap().len() + a.value["log"].as_array().unwrap().iter().map(|l| l.as_str().unwrap().len()).sum::<usize>();
        assert!(total <= max, "{total}");
        assert_eq!(a.value["log_left_out"]["lines"], json!(8), "the cut line, five dropped, two given way to the result");
        assert!(a.value["note"].as_str().unwrap().contains("left out"));
        let big = shell_answer(Ok("Sup3rSecret".repeat(500)), ShellLog::default(), &clean, max, margin).unwrap();
        let res = big.value["result"].as_str().unwrap();
        assert!(res.len() <= max && !res.contains("Su") && big.value["note"].as_str().unwrap().contains("result was cut"), "{res}");
        let e = shell_answer(Err("auth failed for mongodb://app:Sup3rSecret@h/db".into()), ShellLog::default(), &clean, max, margin).unwrap_err();
        assert!(!e.contains("Sup3rSecret"), "{e}");
    }

    #[test]
    fn too_big_says_why() {
        let l = Limits { undo_docs: 10, undo_call_bytes: 1000, ..Limits::default() };
        assert!(too_big(10, 1000, &l, "documents").is_none());
        assert!(too_big(11, 10, &l, "documents").unwrap().contains("more than the 10"));
        assert!(too_big(1, 1001, &l, "rows").unwrap().contains("MB"));
        let ins = Call::Insert { ns: ns("x"), docs: (0..11).map(|i| doc! { "_id": i }).collect() };
        let s = snapshot_now(&ins, &l).unwrap();
        assert_eq!((s.matched, s.docs.is_none(), s.no_undo.is_some()), (11, true, true));
        let s = snapshot_now(&ins, &Limits::default()).unwrap();
        assert_eq!((s.docs.map(|d| d.len()), s.no_undo), (Some(11), None));
    }

    /// Waits for the first agent event `f` picks.
    fn event<T>(rx: &std::sync::mpsc::Receiver<Msg>, mut f: impl FnMut(AgentEvent) -> Option<T>) -> T {
        loop {
            if let Msg::Agent(ev) = rx.recv_timeout(Duration::from_secs(20)).unwrap()
                && let Some(t) = f(ev)
            {
                return t;
            }
        }
    }

    fn prepare(w: &Worker, rx: &std::sync::mpsc::Receiver<Msg>, c: &Client, call: &Call, l: &Limits) -> Snapshot {
        w.agent_prepare(7, AgentTarget::Mongo(c.clone()), call.clone(), l.clone());
        event(rx, |e| if let AgentEvent::Prepared { res, .. } = e { Some(res.unwrap()) } else { None })
    }

    fn write(w: &Worker, rx: &std::sync::mpsc::Receiver<Msg>, c: &Client, call: Call, snap: Snapshot, keep: bool) -> (Value, Finished) {
        let (reply, ans) = oneshot::channel();
        let job = AgentJob { snap: Some(snap), keep_undo: keep, writable: true, ..job(c, call, Limits::default()) };
        w.agent_run(job, reply);
        let v = w.rt.block_on(ans).unwrap().unwrap();
        (v, event(rx, |e| if let AgentEvent::Done(f) = e { Some(f) } else { None }))
    }

    fn undo(w: &Worker, rx: &std::sync::mpsc::Receiver<Msg>, c: &Client, u: AgentUndo) -> (Res<String>, Option<AgentUndo>) {
        w.agent_undo(7, AgentTarget::Mongo(c.clone()), u);
        event(rx, |e| if let AgentEvent::Undone { res, back, .. } = e { Some((res, back)) } else { None })
    }

    fn get(c: &Client, w: &Worker, coll: &str, id: i32) -> Option<Document> {
        w.rt.block_on(async { c.database("quarry_test").collection::<Document>(coll).find_one(doc! { "_id": id }).await }).unwrap()
    }

    fn touch(c: &Client, w: &Worker, coll: &str, id: i32) {
        w.rt.block_on(async { c.database("quarry_test").collection::<Document>(coll).update_one(doc! { "_id": id }, doc! { "$set": { "touched": true } }).await }).unwrap();
    }

    #[test]
    fn live_mongo_update_undo_and_the_conflict_guard() {
        let Some((w, rx, c)) = live("agent_upd") else { return };
        let call = Call::Update { ns: ns("agent_upd"), filter: doc! { "n": { "$lt": 5 } }, update: crate::agents::tools::Update::Doc(doc! { "$inc": { "n": 100 } }), many: true };
        let snap = prepare(&w, &rx, &c, &call, &Limits::default());
        assert_eq!((snap.matched, snap.docs.as_ref().map(Vec::len), snap.no_undo.is_none()), (5, Some(5), true));
        let (v, fin) = write(&w, &rx, &c, call, snap, true);
        assert_eq!((v["modified"].clone(), v["undo"].clone()), (json!(5), json!(true)));
        assert_eq!(get(&c, &w, "agent_upd", 2).unwrap().get_i32("n").unwrap(), 102);
        let u = fin.undo.expect("an undo");
        assert!(matches!(&u, AgentUndo::Updated { docs, .. } if docs.len() == 5));
        // Someone changes one of them after the agent: its undo leaves that one alone and says so.
        touch(&c, &w, "agent_upd", 0);
        let (res, back) = undo(&w, &rx, &c, u);
        let msg = res.unwrap();
        assert!(msg.contains("restored 4") && msg.contains("1 changed since"), "{msg}");
        assert!(back.is_none());
        assert_eq!(get(&c, &w, "agent_upd", 2).unwrap().get_i32("n").unwrap(), 2);
        let zero = get(&c, &w, "agent_upd", 0).unwrap();
        assert_eq!((zero.get_i32("n").unwrap(), zero.get_bool("touched").unwrap()), (100, true));
    }

    #[test]
    fn live_mongo_a_document_changed_after_the_snapshot_is_skipped() {
        let Some((w, rx, c)) = live("agent_race") else { return };
        let call = Call::Update { ns: ns("agent_race"), filter: doc! { "n": { "$lt": 3 } }, update: crate::agents::tools::Update::Doc(doc! { "$set": { "flag": true } }), many: true };
        let snap = prepare(&w, &rx, &c, &call, &Limits::default());
        touch(&c, &w, "agent_race", 1);
        let (v, fin) = write(&w, &rx, &c, call, snap, true);
        assert_eq!((v["modified"].clone(), v["skipped_changed"].clone()), (json!(2), json!(1)), "{v}");
        assert!(v["note"].as_str().unwrap().contains("changed after Quarry read them"));
        assert!(!get(&c, &w, "agent_race", 1).unwrap().contains_key("flag"), "never forced over the newer change");
        assert!(matches!(fin.undo, Some(AgentUndo::Updated { ref docs, .. }) if docs.len() == 2));
    }

    #[test]
    fn live_mongo_delete_over_the_cap_has_no_undo_and_within_it_comes_back() {
        let Some((w, rx, c)) = live("agent_del") else { return };
        let small = Limits { undo_docs: 3, ..Limits::default() };
        let call = Call::Delete { ns: ns("agent_del"), filter: doc! { "n": { "$gte": 20 } }, many: true };
        let snap = prepare(&w, &rx, &c, &call, &small);
        assert_eq!((snap.matched, snap.docs.is_none()), (10, true));
        assert!(snap.no_undo.as_deref().unwrap().contains("more than the 3"), "{:?}", snap.no_undo);
        let (v, fin) = write(&w, &rx, &c, call, snap, false);
        assert_eq!((v["deleted"].clone(), v["undo"].clone()), (json!(10), json!(false)));
        assert!(fin.undo.is_none());
        let call = Call::Delete { ns: ns("agent_del"), filter: doc! { "n": { "$lt": 3 } }, many: true };
        let snap = prepare(&w, &rx, &c, &call, &Limits::default());
        let (v, fin) = write(&w, &rx, &c, call, snap, true);
        assert_eq!(v["deleted"], json!(3));
        assert!(get(&c, &w, "agent_del", 1).is_none());
        let (res, _) = undo(&w, &rx, &c, fin.undo.unwrap());
        assert!(res.unwrap().contains("put back 3"));
        assert_eq!(get(&c, &w, "agent_del", 1).unwrap().get_str("password").unwrap(), "pw1");
    }

    #[test]
    fn live_mongo_insert_undo_removes_only_unchanged_documents() {
        let Some((w, rx, c)) = live("agent_ins") else { return };
        let call = Call::Insert { ns: ns("agent_ins"), docs: vec![doc! { "_id": 100, "a": 1 }, doc! { "_id": 101, "a": 2 }, doc! { "a": 3 }] };
        let snap = snapshot_now(&call, &Limits::default()).unwrap();
        let (v, fin) = write(&w, &rx, &c, call, snap, true);
        assert_eq!((v["inserted"].clone(), v["ids"].as_array().unwrap().len()), (json!(3), 3));
        touch(&c, &w, "agent_ins", 101);
        let (res, _) = undo(&w, &rx, &c, fin.undo.unwrap());
        let msg = res.unwrap();
        assert!(msg.contains("removed 2") && msg.contains("1 changed since"), "{msg}");
        assert!(get(&c, &w, "agent_ins", 100).is_none());
        assert!(get(&c, &w, "agent_ins", 101).is_some(), "a document changed since the insert stays");
        let n = w.rt.block_on(async { c.database("quarry_test").collection::<Document>("agent_ins").count_documents(doc! { "a": 3 }).await }).unwrap();
        assert_eq!(n, 0, "the one without an _id had one made by Quarry and went too");
    }

    #[test]
    fn live_mongo_run_command_answers_scrubbed() {
        let Some((w, rx, c)) = live("agent_cmd") else { return };
        let cmd = |command: Document| Call::RunCommand { conn: ConnRef::Id(1), db: "quarry_test".into(), command };
        let (res, _) = run(&w, &rx, &c, cmd(doc! { "find": "agent_cmd", "filter": { "n": 1 } }), Limits::default());
        let v = res.unwrap();
        let doc = &v["reply"]["cursor"]["firstBatch"][0];
        assert_eq!((doc["n"].clone(), doc["password"].clone()), (json!(1), json!("hidden")), "{v}");
        assert!(!v.to_string().contains("$clusterTime"));
        let (res, _) = run(&w, &rx, &c, cmd(doc! { "listIndexes": "agent_cmd" }), Limits::default());
        assert!(!res.unwrap().to_string().contains("user1@example.org"), "a partial index filter is data");
        let (res, _) = run(&w, &rx, &c, cmd(doc! { "ping": 1 }), Limits::default());
        assert_eq!(res.unwrap()["reply"]["ok"], json!(1.0));
    }

    /// A run_shell job on QUARRY_TEST_MONGO, built as the App builds it, or None when Bun is missing.
    fn shell_job(code: &str, read_only: bool) -> Option<ShellJob> {
        if std::process::Command::new("bun").arg("--version").output().is_err() {
            println!("skipped: bun is not installed");
            return None;
        }
        let uri = std::env::var("QUARRY_TEST_MONGO").ok()?;
        let input = json!({ "mongo": [{ "name": "test", "uri": uri, "db": "quarry_test", "proxy": null, "collections": [], "readOnly": read_only }], "sql": [], "code": code, "packages": [], "timeoutMs": 30000, "main": "test" });
        Some(ShellJob { runtime: "bun".into(), dir: std::env::temp_dir().join("quarry-shell-live-test"), input: input.to_string(), packages: Vec::new(), sql: Vec::new() })
    }

    fn shell(c: &Client, code: &str, read_only: bool, limits: Limits) -> Option<AgentJob> {
        let call = Call::RunShell { conn: ConnRef::Id(1), db: Some("quarry_test".into()), script: code.into() };
        Some(AgentJob { shell: Some((shell_job(code, read_only)?, Redact::new("mongodb://app:S3cr%40tPass@db.example.org/shop", &[]))), writable: !read_only, ..job(c, call, limits) })
    }

    #[test]
    fn live_mongo_run_shell_runs_read_only_when_locked_and_dies_when_cancelled() {
        let Some((w, rx, c)) = live("agent_shell") else { return };
        let read = "async function run() { console.log('mail ana@example.org'); return { n: await db.collection('agent_shell').countDocuments(), who: 'bob@example.org' }; }";
        let Some(j) = shell(&c, read, false, Limits::default()) else { return };
        let (reply, ans) = oneshot::channel();
        w.agent_run(j, reply);
        let v = w.rt.block_on(ans).unwrap().unwrap();
        let out = v.to_string();
        assert!(v["result"].as_str().unwrap().contains("30"), "{v}");
        assert!(!out.contains("bob@example.org") && !out.contains("ana@example.org"), "the detectors mask emails in output: {out}");
        assert!(v["note"].as_str().unwrap().contains("best effort"));
        event(&rx, |e| if let AgentEvent::Done(f) = e { Some(f) } else { None });
        // A script that prints and returns the connection's password, as written and decoded, gets neither back.
        let leak = "async function run() { const p = 'S3cr' + '@tPass'; console.log('pw ' + p); return { p, e: 'S3cr%40' + 'tPass', pair: 'app:' + p }; }";
        let (reply, ans) = oneshot::channel();
        w.agent_run(shell(&c, leak, false, Limits::default()).unwrap(), reply);
        let out = w.rt.block_on(ans).unwrap().unwrap().to_string();
        assert!(!out.contains("S3cr@tPass") && !out.contains("S3cr%40tPass") && out.contains("[hidden]"), "{out}");
        event(&rx, |e| if let AgentEvent::Done(f) = e { Some(f) } else { None });
        // Locked: the runner's read-only guard refuses the write.
        let write = "async function run() { return await db.collection('agent_shell').insertOne({ _id: 'w' }); }";
        let (reply, ans) = oneshot::channel();
        w.agent_run(shell(&c, write, true, Limits::default()).unwrap(), reply);
        let e = w.rt.block_on(ans).unwrap().unwrap_err().message;
        assert!(e.to_lowercase().contains("read"), "{e}");
        event(&rx, |e| if let AgentEvent::Done(f) = e { Some(f) } else { None });
        // A cancel kills Bun: the insert it would make after two seconds never comes.
        let slow = "async function run() { await new Promise((r) => setTimeout(r, 2000)); await db.collection('agent_shell').insertOne({ _id: 'late' }); return 1; }";
        let (reply, ans) = oneshot::channel();
        let r = w.agent_run(shell(&c, slow, false, Limits::default()).unwrap(), reply);
        std::thread::sleep(Duration::from_millis(800));
        r.cancel(&w);
        assert!(w.rt.block_on(ans).is_err(), "nothing answered after a cancel");
        std::thread::sleep(Duration::from_secs(4));
        assert!(get(&c, &w, "agent_shell", 0).is_some());
        let late = w.rt.block_on(async { c.database("quarry_test").collection::<Document>("agent_shell").find_one(doc! { "_id": "late" }).await }).unwrap();
        assert!(late.is_none(), "the script was killed before its insert");
        // The time limit of the settings stops a script that runs too long.
        let (reply, ans) = oneshot::channel();
        w.agent_run(shell(&c, "async function run() { await new Promise((r) => setTimeout(r, 30000)); return 1; }", false, Limits { secs: 1, ..Limits::default() }).unwrap(), reply);
        let t = Instant::now();
        assert!(w.rt.block_on(ans).unwrap().is_err());
        assert!(t.elapsed() < Duration::from_secs(20), "{:?}", t.elapsed());
    }
}
