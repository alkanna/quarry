//! State behind src/app/agents/exec.rs, which draws it.

use crate::agents::audit::Entry;
use crate::agents::config::OpClass;
use crate::agents::link::ToolErr;
use crate::agents::policy::{AskWhy, Grants};
use crate::agents::scrub::Rules;
use crate::agents::tools::{self, Call};
use std::time::{Duration, Instant};
use crate::db;
use std::collections::HashMap;
use serde_json::Value;
use std::sync::Arc;

pub type Reply = tokio::sync::oneshot::Sender<Result<Value, ToolErr>>;

/// A call in the approval queue: what runs once the user allows it.
pub struct Pending {
    pub call: Call,
    pub class: OpClass,
    pub conn: u64,
    /// The PostgreSQL database the call runs in; empty on other engines.
    pub database: String,
    pub session: u64,
    pub why: AskWhy,
    pub reply: Reply,
    /// The agent's stated reason, for the dialog.
    pub reason: Option<String>,
    /// For a write: what it touches, read before it was decided.
    pub snap: Option<db::Snapshot>,
    /// The connection was unlocked when the call was queued, which is what the dialog showed; set by `agent_queue`.
    pub writable: bool,
}

/// A call whose operation runs on the runtime.
pub struct Running {
    pub run: db::AgentRun,
    pub conn: u64,
    /// The PostgreSQL database it runs in; empty on other engines.
    pub database: String,
    pub approved: bool,
    /// It changes the server: a cancel lets it run to its end, so what it changed keeps its undo, or for changes no undo
    /// covers (structure, commands, free SQL that writes, the shell) so it never stops half way unrecorded.
    pub write: bool,
    /// Namespaces it changes, whose open tabs say so once it is done.
    pub targets: Vec<tools::Target>,
    /// It may make or drop a collection or table, so the sidebar lists them again once it is done.
    pub structure: bool,
}

/// The executor's own state, beside what the windows read in `AgentsState`.
pub struct Exec {
    pub pending: HashMap<u64, Pending>,
    /// Writes whose snapshot is being read, before the policy decides on them.
    pub preparing: HashMap<u64, Pending>,
    pub running: HashMap<u64, Running>,
    /// (link session, link call id) to Activity id, so a cancel finds its call.
    pub calls: HashMap<(u64, u64), u64>,
    /// Replies of link calls, held while each runs as an `AgentsA::ToolCall`: a oneshot cannot travel in an action.
    pub replies: HashMap<(u64, u64), Reply>,
    /// What the user allowed "for this session", per (link session, connection).
    pub grants: HashMap<(u64, u64), Grants>,
    /// Compiled rules per connection with the settings they came from, compiled again once those change. An error is
    /// kept too: a rule that does not compile refuses every call there.
    pub rules: HashMap<(u64, String), (u64, Result<Arc<Rules>, String>)>,
    /// Keys a MongoDB describe sampled, per (connection, db, collection).
    pub keys: HashMap<(u64, String, String), (Instant, Vec<String>)>,
    pub audit: Option<std::sync::mpsc::Sender<Entry>>,
    pub next: u64,
    /// How long an approval waits; tests shorten it.
    pub ask_wait: Duration,
    /// The key of the Hash scrub action. Derived from the settings key when the keychain gives one, so hashes match
    /// across launches; random otherwise, and in the demo and the tests.
    pub hash_key: [u8; 32],
}

impl Default for Exec {
    fn default() -> Self {
        Exec { pending: HashMap::new(), preparing: HashMap::new(), running: HashMap::new(), calls: HashMap::new(), replies: HashMap::new(), grants: HashMap::new(), rules: HashMap::new(), keys: HashMap::new(), audit: None, next: 1, ask_wait: ASK_WAIT, hash_key: crate::agents::link::random() }
    }
}

/// The TypeScript MCP SDK gives up on a request after 60 s, so Quarry answers first.
pub const ASK_WAIT: Duration = Duration::from_secs(50);
