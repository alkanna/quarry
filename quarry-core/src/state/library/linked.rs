//! State behind src/app/library/linked.rs, which draws it.

use crate::library::ops::DropAt;
use crate::library::ScriptRef;
use crate::state::View;
use std::path::PathBuf;

/// A linked-file job running on the worker, by id in `LibUi::jobs`: what to do with its answer. Tabs are named by their
/// `uid`, since the tab list can change before the answer comes back.
pub enum Job {
    /// Open a file into a tab on the connection `conn`, picked when it was asked for.
    Open { r: ScriptRef, how: super::open::How, conn: u64 },
    /// Copy a file into the library, at `at`.
    Copy { link: u64, rel: PathBuf, at: DropAt },
    /// Reload: the tab takes the file's text.
    Reload { uid: u64, link: u64, rel: PathBuf },
    /// Cmd+S on a tab bound to a file, which wrote `text`.
    Save { uid: u64, link: u64, rel: PathBuf, text: String },
    /// The Save dialog wrote a new file; the tab is bound to it in `view` with `body` as its base.
    SaveNew { uid: u64, link: u64, rel: PathBuf, view: Option<View>, body: String, place: String, password: bool },
    /// Move to Trash.
    Trash { link: u64, rel: PathBuf, path: PathBuf },
}
