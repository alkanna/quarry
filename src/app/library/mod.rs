//! The script library in the app: loading and saving library.json, opening scripts into tabs, the Save dialog, the quick
//! pickers and the sidebar panel. The data types and the rules on them are in `crate::library`, free of egui.

use super::*;
use crate::library::Library;

pub(super) mod link_dialog;
pub(super) mod linked;
pub(super) mod open;
pub(super) mod panel;
pub(super) mod picker;
pub(super) mod save_dialog;
pub(super) mod snippet_form;
pub(in crate::app) mod snippet_strip;

/// Whether the saved queries of ui.json come over at launch: the first run with a library, or a library.json so broken that
/// nothing of it could be read, which leaves an empty library while the old queries are still in ui.json.
pub(super) fn should_migrate(existed: bool, broken: bool, lib: &Library) -> bool {
    !existed || (broken && lib.scripts.is_empty() && lib.folders.is_empty())
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
    load_lists(path, &["folders", "scripts", "links", "recent"], |l| {
        let (folders, scripts, mut links, recent): (_, _, Vec<crate::library::Link>, Vec<crate::library::ScriptRef>) = (l.take(0), l.take(1), l.take(2), l.take(3));
        // A recent file whose path leaves its folder is skipped like an entry that does not parse.
        let (recent, bad): (Vec<_>, Vec<_>) = recent.into_iter().partition(|r| r.is_safe());
        l.errs.extend(bad.iter().map(|r| format!("{r:?} leaves its linked folder")));
        // A link with no file types, which no dialog makes, would show nothing at all: it shows every type instead.
        for k in &mut links {
            if k.exts.is_empty() {
                k.exts = ["sql", "js", "json"].map(String::from).to_vec();
            }
        }
        let mut lib = Library { v: l.num("v") as u32, folders, scripts, links, recent, next: l.num("next") };
        lib.reserve_ids();
        lib
    })
}

pub(super) fn load_library() -> (Library, Option<String>) {
    load_library_from(&config_dir().join("library.json"))
}

impl App {
    pub(super) fn save_library(&mut self) {
        self.core.save_library()
    }

    /// Called every frame from `logic()`: the library's timers in Core, the repaint they ask for, and the script they may
    /// open.
    pub(super) fn library_tick(&mut self, ctx: &egui::Context) {
        let (now, focused) = ctx.input(|i| (i.time, i.viewport().focused.unwrap_or(true)));
        if let Some(wait) = self.core.library_tick(now, focused) {
            ctx.request_repaint_after(Duration::from_secs_f64(wait));
        }
        self.run_fx();
    }

    pub(super) fn migrate_library(&mut self) {
        self.core.migrate_library()
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
    fn a_library_file_that_could_not_be_read_lets_the_old_queries_come_over_again() {
        let empty = Library::default();
        assert!(should_migrate(false, false, &empty));
        assert!(!should_migrate(true, false, &empty), "an empty library someone kept stays empty");
        assert!(should_migrate(true, true, &empty));
        let kept = Library { scripts: vec![crate::library::Script { id: 1, ..Default::default() }], ..Default::default() };
        assert!(!should_migrate(true, true, &kept), "what was read of a damaged file is not doubled");
    }

    #[test]
    fn a_recent_file_that_leaves_its_folder_is_skipped() {
        let dir = std::env::temp_dir().join(format!("quarry-library-rel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("library.json");
        std::fs::write(&p, r#"{"v":1,"links":[{"id":3,"path":"/tmp/q"}],"recent":[{"File":{"link":3,"rel":"a.sql"}},{"File":{"link":3,"rel":"../../etc/x.sql"}},{"File":{"link":3,"rel":"/etc/x.sql"}}]}"#).unwrap();
        let (l, note) = load_library_from(&p);
        assert_eq!(l.recent, [ScriptRef::File { link: 3, rel: "a.sql".into() }]);
        assert!(note.is_some_and(|n| n.contains("Skipped 2 of 4")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_list_key_of_another_type_is_kept_aside_and_a_newer_file_loads() {
        let dir = std::env::temp_dir().join(format!("quarry-library-shape-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("library.json");
        std::fs::write(&p, r#"{"v":7,"scripts":{"id":1},"folders":[{"id":2,"name":"F"}],"later":true}"#).unwrap();
        let (l, note) = load_library_from(&p);
        assert_eq!((l.v, l.folders.len(), l.scripts.len()), (7, 1, 0));
        assert!(note.is_some_and(|n| n.contains("scripts is not a list")));
        assert!(dir.join("library.json.bak").exists());
        // A script whose folder entry did not parse stays, at the top level.
        std::fs::write(&p, r#"{"v":1,"folders":[{"id":"bad"}],"scripts":[{"id":5,"name":"s","kind":"sql","folder":9}]}"#).unwrap();
        let (l, _) = load_library_from(&p);
        assert_eq!((l.scripts.len(), l.scripts[0].folder), (1, Some(9)));
        assert_eq!(crate::library::tree::live_parent(&l, l.scripts[0].folder), None);
        let rows = crate::library::tree::flatten(&l, &Default::default(), None, None, "", &Default::default());
        assert!(rows.iter().any(|r| r.node == crate::library::tree::Node::Script(5) && r.depth == 0));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_link_without_file_types_shows_every_type() {
        let dir = std::env::temp_dir().join(format!("quarry-library-exts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("library.json");
        std::fs::write(&p, r#"{"v":1,"links":[{"id":3,"path":"/tmp/q","on":"all"},{"id":4,"path":"/tmp/r","exts":["sql"]}]}"#).unwrap();
        let (l, _) = load_library_from(&p);
        assert_eq!(l.links[0].exts, ["sql", "js", "json"]);
        assert_eq!(l.links[1].exts, ["sql"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_backup_that_cannot_be_made_says_the_skipped_entries_will_be_lost() {
        let dir = std::env::temp_dir().join(format!("quarry-library-nobak-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // A folder where the copy would go makes the copy fail.
        std::fs::create_dir_all(dir.join("library.json.bak")).unwrap();
        let p = dir.join("library.json");
        std::fs::write(&p, r#"{"v":1,"scripts":[{"id":"x"},{"id":2,"name":"ok","kind":"sql"}]}"#).unwrap();
        let (l, note) = load_library_from(&p);
        assert_eq!(l.scripts.len(), 1);
        assert!(note.is_some_and(|n| n.contains("will be lost")));
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
