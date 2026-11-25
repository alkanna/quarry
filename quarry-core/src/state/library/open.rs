//! State behind src/app/library/open.rs, which draws it.

use crate::library::ScriptRef;
use crate::state::View;

/// How a script leaves the library: Return opens it, Alt+Return opens it in a new tab, Cmd+Enter runs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    Here,
    NewTab,
    Run,
}

/// What the view on screen holds, as a script of the library would hold it.
pub struct TabText {
    pub kind: &'static str,
    pub body: String,
    pub sort: String,
    pub project: String,
    pub view: View,
    pub db: String,
    pub schema: String,
    pub coll: String,
}

/// Where a script runs, as `App::script_target` works it out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    On(u64),
    /// No connection is the obvious one: the person picks from these.
    Ask(Vec<u64>),
    /// Nothing can run it, and why.
    None(String),
}

/// A script waiting for its connection to be ready, which `library_tick` opens once it is.
#[derive(Clone, Debug)]
pub struct Pending {
    pub r: ScriptRef,
    pub how: How,
    pub conn: u64,
    /// The tab that was current when it was asked for: a Run becomes a plain open if another tab is current by then.
    pub uid: Option<u64>,
    /// The connection's connect attempt when it was asked for, so the failure it had then is not taken for a new one.
    pub attempt: u64,
    /// Egui time the wait started while the connection was not connecting, for the time limit.
    pub since: Option<f64>,
    /// The connection was seen connecting since, so a failure now is news even on the same attempt.
    pub seen_connecting: bool,
    pub name: String,
}

/// The Open on dialog: the connections a script can run on when none is the obvious one.
pub struct OpenOnForm {
    pub r: ScriptRef,
    pub how: How,
    pub conns: Vec<u64>,
    pub sel: Option<u64>,
    pub name: String,
}
