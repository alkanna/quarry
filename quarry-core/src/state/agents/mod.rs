//! State behind src/app/agents/mod.rs, which draws it.

pub mod activity;
pub mod exec;
pub mod rules;
pub mod settings;

use crate::agents::config::AgentsConfig;
use crate::agents::store;
use crate::db;
use crate::state::agents::activity::Filter;
use crate::state::agents::rules::RulesWin;
use crate::state::agents::settings::ClientKind;

#[derive(Default, PartialEq)]
pub enum LinkState {
    /// No listener: the demo and the tests.
    #[default]
    Idle,
    Starting,
    Listening,
    Busy,
    Failed(String),
}

pub struct AgentSession {
    pub id: u64,
    /// What the agent says it is; shown, never trusted.
    pub client: String,
    pub calls: u32,
}

/// How a call ended, for the Activity window, the pill and the audit log.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Running,
    /// In the approval queue.
    Waiting,
    /// Allowed by the settings.
    Allowed,
    /// Allowed by the user in the approval dialog.
    Approved,
    Denied,
    /// No answer before the agent's own timeout.
    TimedOut,
    /// The agent cancelled it or went away.
    Cancelled,
    /// Refused by a setting, a rule or the guard; the text says which.
    Refused(String),
    /// Allowed, then the database or Quarry failed; scrubbed error text.
    Failed(String),
}

/// One agent call as the Activity window shows it. Everything here is already scrubbed.
#[derive(Clone, Debug)]
pub struct Activity {
    pub id: u64,
    /// Local wall-clock time, "22:41:07".
    pub time: String,
    pub session: u64,
    /// As the agent reported it; shown, never trusted.
    pub client: String,
    pub tool: String,
    pub conn: Option<u64>,
    /// "db.coll", "schema.table" or empty, with the database in front for another database of a PostgreSQL connection.
    pub ns: String,
    /// The PostgreSQL database the call ran in; empty on other engines and for calls from earlier sessions.
    pub database: String,
    pub outcome: Outcome,
    /// One line: "412 changed", "200 of 3,120 rows".
    pub summary: String,
    /// Arguments as sent, scrubbed and cut, for the detail view.
    pub args: String,
    /// The start of the answer, scrubbed and cut, for the detail view.
    pub answer: String,
    /// Set while Undo call can still revert this change.
    pub undo: Option<u64>,
}

/// A call waiting in the approval queue. The parsed call and its reply stay with the executor; this is what the
/// dialog draws.
#[derive(Clone, Debug)]
pub struct Ask {
    /// The Activity id of the call.
    pub id: u64,
    pub client: String,
    pub tool: String,
    pub conn: u64,
    pub ns: String,
    /// "UPDATE MANY", "SCRIPT", "DROP COLLECTION".
    pub op: String,
    /// The operation as Quarry will run it, pretty-printed and scrubbed.
    pub code: String,
    /// Quarry's own facts, first: "Matches 412 documents, counted just now."
    pub facts: Vec<String>,
    /// The agent's stated reason, untrusted, at most 300 characters.
    pub reason: Option<String>,
    pub why: crate::agents::policy::AskWhy,
    /// Set when Allow without undo is the only allow, with why it can't be undone; empty draws the undo cap line.
    pub no_undo: Option<String>,
    /// When the call was queued; it is denied 50 s later.
    pub at: std::time::Instant,
    /// When the dialog first showed, for the one-second delay before Allow works.
    pub shown: Option<std::time::Instant>,
    /// The agent cancelled or timed out while it was open: only Close remains.
    pub gone: bool,
}

/// What the user chose in the approval dialog.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Answer {
    Deny,
    Once,
    /// Covers the same class on the same connection for this link session.
    Session,
    /// Unlocks the connection's writes for the session, then allows once.
    UnlockOnce,
    /// Over the undo cap: runs without keeping what it needs to undo.
    WithoutUndo,
    /// Dismisses a call whose agent stopped waiting.
    Close,
}

#[derive(Default)]
pub struct AgentsState {
    /// Newest last, capped; the audit log on disk keeps the full history.
    pub activity: std::collections::VecDeque<Activity>,
    /// Oldest first; the dialog shows the front.
    pub asks: std::collections::VecDeque<Ask>,
    /// The Activity window is open.
    pub activity_open: bool,
    pub cfg: AgentsConfig,
    /// Bumped whenever `cfg` changes, so compiled rules and the tester's lines know to recompute.
    pub rev: u64,
    /// Why agents.json could not be trusted at start, until the user reviews and saves: it came back with agents
    /// off and everything but the custom rules at its default.
    pub distrust: Option<store::Trust>,
    pub handle: Option<db::AgentsHandle>,
    pub link: LinkState,
    pub sessions: Vec<AgentSession>,
    /// The client whose setup snippet Settings shows.
    pub client: ClientKind,
    /// Calls waiting or running, grants, compiled rules and the audit writer; see `exec.rs`.
    pub exec: exec::Exec,
    /// The connection whose own page Settings > AI agents shows instead of the list.
    pub page: Option<u64>,
    /// The scope glob field of that page, as typed.
    pub globs: String,
    /// The Data protection window.
    pub rules: Option<RulesWin>,
    /// What undoes agent changes, by Activity id. In memory only and capped in total by the limits.
    pub undo: crate::agents::undo::UndoStore<db::AgentUndo>,
    pub filter: Filter,
    /// The Activity row whose arguments and answer show below the list.
    pub picked: Option<u64>,
    /// What the approval dialog answered, for the UI tests.
    #[cfg(any(test, feature = "test-support"))]
    pub answered: Vec<(u64, Answer)>,
    /// The window as the app saw it just before handing over the latest agent event.
    pub ui: AgentUi,
}

/// What only the window knows and an agent call depends on, set by the app before each agent event.
#[derive(Clone, Copy, Debug)]
pub struct AgentUi {
    /// The window has focus; an approval asks for attention otherwise.
    pub focused: bool,
    /// A text field has the keyboard, so keys would land in a tab open_shell shows.
    pub typing: bool,
    /// One of the app's dialogs is open.
    pub modal: bool,
}

impl Default for AgentUi {
    fn default() -> Self {
        AgentUi { focused: true, typing: false, modal: false }
    }
}
