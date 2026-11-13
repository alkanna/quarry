//! Undo for changes that are not database edits: a deleted group, deleted library scripts and folders, a deleted file.
//! `apply_undo` cannot serve them, since it needs a writable connection and these never touch one.

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
    /// A linked file the Save dialog wrote over: what it held before and what was written. Undo puts the old bytes back
    /// only while the file still holds the written ones, so an edit made since is never lost.
    Replaced { link: u64, path: PathBuf, old: Vec<u8>, new: Vec<u8> },
    /// An unlinked folder, its place in the list, the tabs it unbound (by uid) and its recent files with their places.
    Link { link: crate::library::Link, at: usize, tabs: Vec<(u64, Bound)>, recent: Vec<(usize, crate::library::ScriptRef)> },
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
                self.prune_rail_open();
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
                    && let Some(path) = self.library.link_join(link, &rel)
                {
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
                let (Some(dir), Some(file)) = (path.parent(), path.file_name().and_then(|f| f.to_str())) else {
                    // An Undo that does nothing must say so, or the person believes the file is back.
                    return self.err(format!("Could not restore \"{}\": it names no file.", path.display()));
                };
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
            Restore::Link { link, at, tabs, recent } => self.relink(link, at, tabs, recent),
            Restore::Replaced { link, path, old, new } => {
                let name = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                if !std::fs::read(&path).is_ok_and(|now| now == new) {
                    self.warn(format!("{name} was changed since, so it was kept."));
                    return;
                }
                match crate::library::disk::write_file(&path, &old) {
                    Ok(()) => {
                        self.rescan_link(link);
                        self.ok(format!("{name} is back as it was."));
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

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("quarry-restore-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn undo_last(a: &mut App) {
        let id = a.restores.last().map(|r| r.0).expect("an Undo");
        a.apply_restore(id);
    }

    #[test]
    fn a_group_put_back_past_the_end_goes_last() {
        let mut h = app();
        let a = h.state_mut();
        a.conns.extend([1, 2].map(|id| Conn::new(ConnMeta { id, ..Default::default() }, Status::Idle)));
        a.groups.new_group("Dev");
        let g = Group { id: 9, name: "Prod".into(), members: vec![1, 7], ..Default::default() };
        a.toast_restore("Deleted.".into(), Restore::Group { g, at: 99 });
        undo_last(a);
        // 7 is no saved connection any more, so only 1 comes back.
        assert_eq!(a.groups.groups.iter().map(|g| (g.name.as_str(), g.members.clone())).collect::<Vec<_>>(), [("Dev", vec![]), ("Prod", vec![1])]);
    }

    #[test]
    fn undoing_a_rail_drop_skips_a_removed_connection_and_keeps_later_group_edits() {
        let mut h = app();
        let a = h.state_mut();
        a.conns.extend([1, 2, 3].map(|id| Conn::new(ConnMeta { id, ..Default::default() }, Status::Idle)));
        a.rail_order = vec![1, 2, 3];
        let all = a.conn_ids();
        // A drop of 2 onto 1 made a group of them.
        let moved = vec![(2, None, a.groups.spot(2, &all).1), (1, None, a.groups.spot(1, &all).1)];
        let g = a.groups.new_group("New group");
        a.groups.place(1, Place::Into(g, None), &all);
        a.groups.place(2, Place::Into(g, None), &all);
        a.toast_restore("Grouped.".into(), Restore::Placed { moved, created: Some(g), tile: 2, prev: Some(1), at: 1 });
        // Since then: the group was renamed, 3 joined it, and 2 was removed from the app.
        a.groups.group_mut(g).unwrap().name = "Prod".into();
        a.groups.place(3, Place::Into(g, None), &all);
        // Removing a connection prunes it from the groups, as Danger::RemoveConnection does.
        a.conns.retain(|c| c.meta.id != 2);
        a.rail_order.retain(|c| *c != 2);
        let alive = a.conn_ids();
        a.groups.prune(&alive);
        undo_last(a);
        let alive = a.conn_ids();
        assert_eq!(a.groups.group(g).map(|x| (x.name.as_str(), x.members.clone())), Some(("Prod", vec![3])));
        assert_eq!(a.groups.loose_order(&alive), [1]);
        assert!(!a.rail_order.contains(&2) && a.groups.groups.iter().all(|x| !x.members.contains(&2)));
    }

    #[test]
    fn undoing_a_move_into_a_linked_folder_that_was_unlinked_since_brings_the_script_back_only() {
        let mut h = app();
        let a = h.state_mut();
        let d = temp("unlinked");
        std::fs::write(d.join("R.sql"), "select 1").unwrap();
        let s = Script { id: 4, name: "R".into(), kind: "sql".into(), body: "select 1".into(), ..Default::default() };
        a.toast_restore("Moved.".into(), Restore::Saved { scripts: vec![s], folders: vec![], written: Some((7, "R.sql".into(), b"select 1".to_vec())) });
        undo_last(a);
        assert!(a.library.scripts.iter().any(|s| s.id == 4));
        assert!(d.join("R.sql").exists(), "a file under a link that is gone must be left alone");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_file_that_cannot_be_removed_on_undo_is_said() {
        let mut h = app();
        let a = h.state_mut();
        let d = temp("stuck");
        let f = d.join("R.sql");
        std::fs::write(&f, "select 1").unwrap();
        a.library.links.push(crate::library::Link { id: 7, path: d.clone(), ..Default::default() });
        // A read-only file cannot be removed on Windows, and a read-only folder keeps its files on unix.
        let lock = |on: bool| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(if on { 0o555 } else { 0o755 }));
            }
            let mut p = std::fs::metadata(&f).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            p.set_readonly(on);
            let _ = std::fs::set_permissions(&f, p);
        };
        lock(true);
        let s = Script { id: 4, name: "R".into(), kind: "sql".into(), body: "select 1".into(), ..Default::default() };
        a.toast_restore("Moved.".into(), Restore::Saved { scripts: vec![s], folders: vec![], written: Some((7, "R.sql".into(), b"select 1".to_vec())) });
        undo_last(a);
        let still = f.exists();
        lock(false);
        assert!(a.library.scripts.iter().any(|s| s.id == 4));
        // Running as an admin removes it anyway; otherwise the person is told the file stayed.
        if still {
            assert!(a.last_problem().contains("could not be removed"), "{}", a.last_problem());
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_file_undo_that_cannot_write_says_so() {
        let mut h = app();
        let a = h.state_mut();
        a.toast_restore("Deleted.".into(), Restore::File { path: PathBuf::new(), bytes: b"x".to_vec() });
        undo_last(a);
        assert!(a.last_problem().contains("names no file"), "{}", a.last_problem());
        // The folder it lived in is now a file, so it cannot be made again.
        let d = temp("blocked");
        std::fs::write(d.join("sub"), "a file").unwrap();
        a.toast_restore("Deleted.".into(), Restore::File { path: d.join("sub").join("q.sql"), bytes: b"x".to_vec() });
        undo_last(a);
        assert!(a.last_problem().contains("Could not restore"), "{}", a.last_problem());
        std::fs::remove_dir_all(&d).unwrap();
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
