//! State behind src/app/restore.rs, which draws it.

use crate::library::{Folder, Script};
use crate::state::Bound;
use crate::state::groups::Group;
use std::path::PathBuf;

/// What a toast's Undo puts back, held in `App::restores` under the toast's id.
pub enum Restore {
    /// A deleted group, with where it stood in the list. Its members left the group when it went.
    Group { g: Group, at: usize },
    /// A drop on the rail: the connections it moved with the spot each had (`Groups::spot`), the group it made, and the
    /// rail tile it moved with the tile before it and its index then. Undo puts back only these, so a change made to the
    /// groups while the toast is up stays.
    Placed { moved: Vec<(u64, Option<u64>, usize)>, created: Option<u64>, tile: u64, prev: Option<u64>, at: usize },
    /// Deleted library scripts and the folders that held them.
    /// `written` is the file a script moved into a linked folder left there: its link, its path in the link and what it held.
    Saved { scripts: Vec<Script>, folders: Vec<Folder>, written: Option<(u64, PathBuf, Vec<u8>)> },
    /// A file moved to the trash, with the bytes it held.
    File { path: PathBuf, bytes: Vec<u8> },
    /// The text a tab's script view held before Reload replaced it with the file, by tab uid.
    TabText { uid: u64, text: String },
    /// A saved script as it was before a save replaced it.
    Script(Script),
    /// A linked file the Save dialog wrote over: what it held before and what was written. Undo puts the old bytes back
    /// only while the file still holds the written ones, so an edit made since is never lost.
    Replaced { link: u64, path: PathBuf, old: Vec<u8>, new: Vec<u8> },
    /// An unlinked folder, its place in the list, the tabs it unbound (by uid) and its recent files with their places.
    Link { link: crate::library::Link, at: usize, tabs: Vec<(u64, Bound)>, recent: Vec<(usize, crate::library::ScriptRef)> },
}
