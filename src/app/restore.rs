//! Undo for changes that are not database edits: a deleted group, deleted library scripts and folders, a deleted file.
//! `apply_undo` cannot serve them, since it needs a writable connection and these never touch one.
#![allow(dead_code)]

use super::*;
use crate::library::{Folder, Script};

/// What a toast's Undo puts back, held in `App::restores` under the toast's id.
pub(super) enum Restore {
    /// A deleted group, with where it stood in the list. Its members left the group when it went.
    Group { g: Group, at: usize },
    /// The groups as they were before a change that created or moved into one, such as a drop on the rail.
    /// `rail` is the rail order then, so the tile goes back to its place as well.
    Groups { groups: Box<Groups>, rail: Vec<u64> },
    /// Deleted library scripts and the folders that held them.
    Saved { scripts: Vec<Script>, folders: Vec<Folder> },
    /// A file moved to the trash, with the bytes it held.
    File { path: PathBuf, bytes: Vec<u8> },
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
            Restore::Groups { groups: before, rail } => {
                // Connections removed since are pruned so the snapshot never brings a dead id back.
                self.groups = *before;
                let alive = self.conn_ids();
                self.groups.prune(&alive);
                // The rail drops what has closed since and appends what opened since on its next frame.
                self.rail_order = rail;
                let groups = &self.groups;
                self.rail_open.retain(|g| groups.group(*g).is_some());
                self.save_groups();
                self.ok("Grouping undone.");
            }
            Restore::Saved { scripts, folders } => {
                let n = scripts.len();
                self.library.restore(folders, scripts);
                self.save_library();
                self.ok(if n == 1 { "Script restored.".to_string() } else { format!("{n} scripts restored.") });
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
