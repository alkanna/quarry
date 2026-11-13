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
    // B1: the script waiting for its connection to be ready, the quick picker.

    // B3: the panel's open folders, selection, search and drags.

    // B2: link scans in flight, conflicts and the Link a folder dialog.
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
    (Library { v, folders, scripts, links, recent }, skipped_note(path, total, &errs))
}

pub(super) fn load_library() -> (Library, Option<String>) {
    load_library_from(&config_dir().join("library.json"))
}

impl App {
    pub(super) fn save_library(&mut self) {
        self.library.v = 1;
        let b = serde_json::to_vec_pretty(&self.library).unwrap_or_default();
        self.write_config("library.json", b);
    }

    /// Called every frame from `logic()`: scheduled library work, such as opening a script once its connection is ready
    /// and rescanning linked folders. Timers use egui time and anything that changes asks for a repaint.
    pub(super) fn library_tick(&mut self, _ctx: &egui::Context) {}
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
}
