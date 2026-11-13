//! Opening a library script into a tab. Filled in by stream B1; the signatures are fixed here so the panel (B3) and the
//! linked folders (B2) can call them before B1 lands.
#![allow(dead_code)]

use super::*;
use crate::library::ScriptRef;

/// How a script leaves the library: Return opens it, Alt+Return opens it in a new tab, Cmd+Enter runs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum How {
    Here,
    NewTab,
    Run,
}

impl App {
    /// Opens the script `r` in a tab, the way `how` says. B1 fills this in.
    pub(in crate::app) fn open_script(&mut self, _r: &ScriptRef, _how: How) {}

    /// Opens the Save to library dialog for the current tab, or writes back the script the tab came from. B1 fills this in.
    pub(in crate::app) fn save_script(&mut self) {}
}
