//! State behind src/app/library/mod.rs, which draws it.

pub mod link_dialog;
pub mod linked;
pub mod open;
pub mod panel;
pub mod save_dialog;
pub mod snippet_form;
pub mod snippet_strip;

use std::collections::{HashMap, HashSet};

/// What the Library panel and its dialogs remember while the app runs.
#[derive(Default)]
pub struct LibUi {
    /// A script asked for while its connection was not ready: opened by `library_tick` once it is, dropped if it fails.
    pub pending: Option<open::Pending>,
    /// The folder the last save went to, which the Save dialog offers first.
    pub last_folder: Option<u64>,

    /// The panel's open folders, selection, search and drags.
    pub panel: panel::PanelUi,

    /// The last scan of each link, by link id. Entries keep their modified time and length, so a rescan reuses what did not change.
    pub scans: HashMap<u64, crate::library::disk::Scan>,
    /// Links with a scan running, so each has at most one.
    pub scanning: HashSet<u64>,
    /// Egui time each link was last scanned.
    pub scan_at: HashMap<u64, f64>,
    /// Links to scan at the next frame, whatever the timer says.
    pub rescan: HashSet<u64>,
    pub was_focused: bool,
    /// The scans are set by hand, as the demo does, and nothing reads the disk.
    pub frozen: bool,
    /// What Quarry last read or wrote of each linked file, by link and path; a scan that finds another time or length marks the
    /// file in `changed`. Neither is saved.
    /// By link, then path, so a scan looks up each file without building a key.
    pub baseline: HashMap<u64, HashMap<std::path::PathBuf, (std::time::SystemTime, u64)>>,
    pub changed: HashSet<(u64, std::path::PathBuf)>,
    /// Linked-file reads and writes running on the worker, by job id, and the last id given.
    pub jobs: HashMap<u64, linked::Job>,
    pub job_seq: u64,

    /// Tabs, by uid, that Run opened instead of running because their script can write: they show "This script writes".
    pub writes: HashSet<u64>,
    /// Cmd+O asked for the current view's picker; the picker button opens it when it draws and `pick_acts` clears it.
    pub open_picker: bool,
    /// Cmd+Shift+L asked for the tree: the panel selects its first row if none is selected and takes the keys.
    pub focus_tree: bool,
    /// The values each snippet last ran with, by script id, offered first the next time. Not saved.
    pub snippet_values: HashMap<u64, HashMap<String, String>>,
}
