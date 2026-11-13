//! The script library in the app: loading and saving library.json, opening scripts into tabs, the Save dialog, the quick
//! pickers and the sidebar panel. The data types and the rules on them are in `crate::library`, free of egui.
//! F0 holds the frozen state types and the stubs; the streams fill the submodules.
#![allow(dead_code)]

use super::*;
use crate::library::Library;

pub(super) mod link_dialog;
pub(super) mod linked;
pub(super) mod open;
pub(super) mod panel;
pub(super) mod picker;
pub(super) mod save_dialog;

/// What the Library panel and its dialogs remember while the app runs. Each stream adds its own block below, apart from
/// the others, so merging them stays free of conflicts.
#[derive(Default)]
pub(super) struct LibUi {
    // B1: the script waiting for its connection to be ready, the folder the Save dialog offers first.
    /// A script asked for while its connection was not ready: opened by `library_tick` once it is, dropped if it fails.
    pub(in crate::app) pending: Option<open::Pending>,
    /// The folder the last save went to, which the Save dialog offers first.
    pub(super) last_folder: Option<u64>,

    // B3: the panel's open folders, selection, search and drags.
    pub(in crate::app) panel: panel::PanelUi,

    // B2: link scans in flight, conflicts and the Link a folder dialog.
    /// The last scan of each link, by link id. Entries keep their modified time and length, so a rescan reuses what did not change.
    pub(super) scans: HashMap<u64, crate::library::disk::Scan>,
    /// Links with a scan running, so each has at most one.
    pub(super) scanning: HashSet<u64>,
    /// Egui time each link was last scanned.
    pub(super) scan_at: HashMap<u64, f64>,
    /// Links to scan at the next frame, whatever the timer says.
    pub(super) rescan: HashSet<u64>,
    pub(super) was_focused: bool,
    /// The scans are set by hand, as the demo does, and nothing reads the disk.
    pub(super) frozen: bool,
    /// What Quarry last read or wrote of each linked file, by link and path; a scan that finds another time or length marks the
    /// file in `changed`. Neither is saved.
    pub(super) baseline: HashMap<(u64, std::path::PathBuf), (std::time::SystemTime, u64)>,
    pub(super) changed: HashSet<(u64, std::path::PathBuf)>,
    /// Linked-file reads and writes running on the worker, by job id, and the last id given.
    pub(in crate::app) jobs: HashMap<u64, linked::Job>,
    pub(super) job_seq: u64,

    // Hardening: the write notice, and the keys that reach the picker and the tree from anywhere.
    /// Tabs, by uid, that Run opened instead of running because their script can write: they show "This script writes".
    pub(in crate::app) writes: HashSet<u64>,
    /// Cmd+O asked for the current view's picker; the picker button opens it when it draws and `pick_acts` clears it.
    pub(in crate::app) open_picker: bool,
    /// Cmd+Shift+L asked for the tree: the panel selects its first row if none is selected and takes the keys.
    pub(in crate::app) focus_tree: bool,
}

/// A shortcut spelled as the palette spells it: the key glyphs on macOS, words elsewhere, where Cmd is Ctrl.
pub(in crate::app) fn keys_text(cmd: bool, shift: bool, alt: bool, key: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("{}{}{}{key}", if alt { "\u{2325}" } else { "" }, if shift { "\u{21e7}" } else { "" }, if cmd { "\u{2318}" } else { "" })
    } else {
        format!("{}{}{}{key}", if cmd { "Ctrl+" } else { "" }, if shift { "Shift+" } else { "" }, if alt { "Alt+" } else { "" })
    }
}

/// Reads library.json. Its lists are parsed entry by entry: a bad script or folder is skipped and the file copied to
/// `.bak`, never the whole library reset.
pub(super) fn load_library_from(path: &std::path::Path) -> (Library, Option<String>) {
    let Ok(b) = std::fs::read(path) else { return (Library::default(), None) };
    let Ok(serde_json::Value::Object(mut o)) = serde_json::from_slice::<serde_json::Value>(&b) else { return parse_config(path) };
    let mut list = |k: &str| match o.remove(k) {
        Some(serde_json::Value::Array(a)) => a,
        _ => Vec::new(),
    };
    let (folders, scripts, links, recent) = (list("folders"), list("scripts"), list("links"), list("recent"));
    let total = folders.len() + scripts.len() + links.len() + recent.len();
    let (folders, mut errs) = parse_entries(folders);
    let (scripts, e) = parse_entries(scripts);
    errs.extend(e);
    let (links, e) = parse_entries(links);
    errs.extend(e);
    let (recent, e) = parse_entries(recent);
    errs.extend(e);
    let v = o.get("v").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let next = o.get("next").and_then(|v| v.as_u64()).unwrap_or(0);
    let mut lib = Library { v, folders, scripts, links, recent, next };
    lib.reserve_ids();
    (lib, skipped_note(path, total, &errs))
}

pub(super) fn load_library() -> (Library, Option<String>) {
    load_library_from(&config_dir().join("library.json"))
}

impl App {
    pub(super) fn save_library(&mut self) {
        // The demo's made-up scripts must never reach the real library file.
        if self.demo {
            return;
        }
        self.library.v = 1;
        let b = serde_json::to_vec_pretty(&self.library).unwrap_or_default();
        self.write_config("library.json", b);
    }

    /// Called every frame from `logic()`: scheduled library work, such as opening a script once its connection is ready
    /// and rescanning linked folders. Timers use egui time and anything that changes asks for a repaint.
    pub(super) fn library_tick(&mut self, ctx: &egui::Context) {
        self.library_pending(ctx);
        self.linked_tick(ctx);
    }

    /// Opens the script that waited for its connection, once the connection is ready. A connection that fails on a new
    /// attempt drops it with one warning; one that never starts connecting (its address could not be read, say) drops it after
    /// `WAIT_SECS`, so it never opens later out of the blue. A Run becomes a plain open if another tab is current by then.
    fn library_pending(&mut self, ctx: &egui::Context) {
        const WAIT_SECS: f64 = 15.0;
        let Some(p) = self.lib_ui.pending.clone() else { return };
        let Some(c) = self.conn(p.conn) else {
            self.lib_ui.pending = None;
            return self.warn(format!("{} was not opened: its connection was removed.", p.name));
        };
        let (ready, connecting, failed, attempt, cname) = (c.status == Status::Ready, c.status == Status::Connecting, matches!(c.status, Status::Failed(_)), c.attempt, c.meta.name.clone());
        let now = ctx.input(|i| i.time);
        if ready {
            self.lib_ui.pending = None;
            let moved = self.t().map(|t| t.uid) != p.uid;
            let how = if p.how == open::How::Run && moved { open::How::Here } else { p.how };
            return self.open_script_on(&p.r, how, p.conn);
        }
        if connecting {
            if let Some(q) = self.lib_ui.pending.as_mut() {
                q.since = None;
            }
            return;
        }
        // The failure the connection had when the script was asked for is not news: only a new attempt that fails is.
        if failed && attempt != p.attempt {
            self.lib_ui.pending = None;
            return self.warn(format!("{} was not opened: {cname} failed to connect.", p.name));
        }
        let since = p.since.unwrap_or(now);
        if let Some(q) = self.lib_ui.pending.as_mut() {
            q.since = Some(since);
        }
        if now - since >= WAIT_SECS {
            self.lib_ui.pending = None;
            self.warn(format!("{} was not opened: {cname} did not connect.", p.name));
        } else {
            ctx.request_repaint_after(Duration::from_secs_f64(WAIT_SECS - (now - since)));
        }
    }

    /// Fills an empty library from the saved queries of ui.json, once: library.json is absent and the old map is not.
    /// The old map stays in ui.json, frozen, for an older build.
    pub(super) fn migrate_library(&mut self) {
        if self.prefs.library.is_empty() {
            return;
        }
        let old: Vec<(String, Vec<crate::library::migrate::OldEntry>)> = self
            .prefs
            .library
            .iter()
            .map(|(k, v)| (k.clone(), v.iter().map(|q| crate::library::migrate::OldEntry { name: q.name.clone(), filter: q.filter.clone(), sort: q.sort.clone(), project: q.project.clone(), pipeline: q.pipeline.clone(), sql: q.sql.clone() }).collect()))
            .collect();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        // Written even when nothing came over (every key was bad), so library.json exists and this never runs again.
        crate::library::migrate::migrate_into(&mut self.library, &old, now);
        self.save_library();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::ScriptRef;

    #[test]
    fn one_bad_script_does_not_reset_the_library() {
        let dir = std::env::temp_dir().join(format!("quarry-library-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("library.json");
        std::fs::write(&p, r#"{"v":1,"folders":[{"id":1,"name":"Reports"}],"scripts":[{"id":2,"name":"ok","kind":"sql","body":"select 1","folder":1},{"id":"x"}],"links":[{"id":3,"path":"/tmp/q","exts":["sql"]}],"recent":[{"Saved":2},"junk"]}"#).unwrap();
        let (l, note) = load_library_from(&p);
        assert_eq!((l.folders.len(), l.scripts.len(), l.links.len(), l.recent.clone()), (1, 1, 1, vec![ScriptRef::Saved(2)]));
        assert!(note.is_some_and(|n| n.contains("Skipped 2 of 6")));
        assert!(p.exists() && dir.join("library.json.bak").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_id_counter_is_read_back_from_the_file() {
        let dir = std::env::temp_dir().join(format!("quarry-library-next-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("library.json");
        // Script 12 was deleted: the highest id left is 3, but 12 and below were handed out.
        std::fs::write(&p, r#"{"v":1,"scripts":[{"id":3,"name":"a","kind":"sql"}],"next":13}"#).unwrap();
        let (mut l, _) = load_library_from(&p);
        assert_eq!(l.next_id(), 13);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
