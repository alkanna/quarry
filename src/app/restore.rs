//! Undo for changes that are not database edits: a deleted group, deleted library scripts and folders, a deleted file.
//! `apply_undo` cannot serve them, since it needs a writable connection and these never touch one.
#![allow(dead_code)]

use super::*;
use crate::library::{Folder, Script};

/// What a toast's Undo puts back, held in `App::restores` under the toast's id.
pub(super) enum Restore {
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
}

impl App {
    pub(super) fn apply_restore(&mut self, id: u64) {
        let Some(i) = self.restores.iter().position(|(r, _)| *r == id) else { return };
        match self.restores.remove(i).1 {
            Restore::Group { g, at } => {
                let (name, alive) = (g.name.clone(), self.conn_ids());
                self.groups.restore(g, at, &alive);
                self.save_groups();
                self.ok(format!("Group {name} is back."));
            }
            Restore::Placed { moved, created, tile, prev, at } => {
                let alive = self.conn_ids();
                self.groups.undo_drop(&moved, created, &alive);
                // The tile goes back after the tile it followed, or to its old index when that one has closed since.
                if self.rail_order.contains(&tile) {
                    self.rail_order.retain(|c| *c != tile);
                    let i = match prev {
                        None => 0,
                        Some(p) => self.rail_order.iter().position(|c| *c == p).map_or(at.min(self.rail_order.len()), |i| i + 1),
                    };
                    self.rail_order.insert(i, tile);
                }
                let groups = &self.groups;
                self.rail_open.retain(|g| groups.group(*g).is_some());
                self.save_groups();
                self.ok("Grouping undone.");
            }
            Restore::Saved { scripts, folders, written } => {
                let n = scripts.len();
                self.library.restore(folders, scripts);
                self.save_library();
                // A script moved into a linked folder left a file there. It goes too, unless it was changed since, which
                // would make the undo throw away the person's edits.
                if let Some((link, rel, bytes)) = written
                    && let Some(root) = self.library.links.iter().find(|l| l.id == link).map(|l| l.path.clone())
                {
                    let path = root.join(&rel);
                    if std::fs::read(&path).is_ok_and(|now| now == bytes) {
                        if let Err(e) = std::fs::remove_file(&path) {
                            self.warn(format!("The script is back, but {} could not be removed: {e}", path.display()));
                            return;
                        }
                        self.drop_entry(link, &rel);
                    } else if path.exists() {
                        self.warn(format!("The script is back. {} was changed since, so it was kept.", path.display()));
                        return;
                    }
                }
                self.ok(if n == 1 { "Script restored.".to_string() } else { format!("{n} scripts restored.") });
            }
            Restore::TabText { uid, text } => {
                let Some(t) = self.tabs.iter_mut().find(|t| t.uid == uid) else { return self.warn("That tab was closed.") };
                super::library::linked::set_bound_text(t, &text, None);
                self.ok("Your edits are back.");
            }
            Restore::Script(old) => {
                let name = old.name.clone();
                match self.library.scripts.iter_mut().find(|s| s.id == old.id) {
                    Some(s) => *s = old,
                    None => self.library.restore(Vec::new(), vec![old]),
                }
                self.save_library();
                self.ok(format!("\"{name}\" is back as it was."));
            }
            Restore::File { path, bytes } => {
                // A name the person has taken again since must not be overwritten by an undo.
                if std::fs::symlink_metadata(&path).is_ok() {
                    self.warn(format!("{} exists again, so the old file was not put back.", path.display()));
                    return;
                }
                let (Some(dir), Some(file)) = (path.parent(), path.file_name().and_then(|f| f.to_str())) else { return };
                match write_atomic(dir, file, &bytes) {
                    Ok(()) => {
                        // The folder it came from shows the file again at the next scan.
                        let links: Vec<u64> = self.library.links.iter().filter(|l| path.starts_with(&l.path)).map(|l| l.id).collect();
                        self.lib_ui.rescan.extend(links);
                        self.ok(format!("{file} restored."));
                    }
                    Err(e) => self.err(format!("Could not restore {}: {e}", path.display())),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;

    fn app() -> Harness<'static, App> {
        Harness::builder().build_eframe(|cc| App::for_test(cc))
    }

    #[test]
    fn undoing_a_deleted_group_puts_it_and_its_members_back() {
        let mut h = app();
        let a = h.state_mut();
        a.conns.extend([1, 2, 3].map(|id| Conn::new(ConnMeta { id, ..Default::default() }, Status::Idle)));
        let (first, second) = (a.groups.new_group("Prod"), a.groups.new_group("Dev"));
        let all = a.conn_ids();
        a.groups.place(1, Place::Into(first, None), &all);
        a.groups.place(2, Place::Into(first, None), &all);
        a.groups.place(3, Place::Into(second, None), &all);
        // What deleting the first group does: it goes, and its members are loose again.
        let gone = a.groups.groups.remove(0);
        a.groups.loose = vec![1, 2];
        a.toast_restore("Group Prod deleted.".into(), Restore::Group { g: gone, at: 0 });
        let id = a.restores[0].0;
        a.apply_restore(id);
        assert_eq!(a.groups.groups.iter().map(|g| (g.name.as_str(), g.members.clone())).collect::<Vec<_>>(), [("Prod", vec![1, 2]), ("Dev", vec![3])]);
        assert!(a.groups.loose.is_empty() && a.restores.is_empty());
        // A second press on the same toast finds nothing and changes nothing.
        a.apply_restore(id);
        assert_eq!(a.groups.groups.len(), 2);
    }

    #[test]
    fn a_restored_file_never_overwrites_one_that_is_there() {
        let mut h = app();
        let a = h.state_mut();
        let dir = std::env::temp_dir().join(format!("quarry-restore-{}", std::process::id()));
        let path = dir.join("q.sql");
        a.toast_restore("Deleted.".into(), Restore::File { path: path.clone(), bytes: b"select 1".to_vec() });
        let id = a.restores[0].0;
        a.apply_restore(id);
        assert_eq!(std::fs::read(&path).unwrap(), b"select 1");
        std::fs::write(&path, "mine").unwrap();
        a.toast_restore("Deleted.".into(), Restore::File { path: path.clone(), bytes: b"select 2".to_vec() });
        let id = a.restores[0].0;
        a.apply_restore(id);
        assert_eq!(std::fs::read(&path).unwrap(), b"mine");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
