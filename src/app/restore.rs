//! Undo for changes that are not database edits: a deleted group, deleted library scripts and folders, a deleted file.
//! `apply_undo` cannot serve them, since it needs a writable connection and these never touch one.

use super::*;
#[cfg(test)]
pub(crate) use quarry_core::state::restore::*;
use quarry_core::action::{Action, LibA};

impl App {
    /// The Undo of a toast: Core puts back what its id stands for.
    pub(super) fn apply_restore(&mut self, id: u64) {
        self.exec(Action::Library(LibA::Restore(id)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Script;
    use egui_kittest::Harness;

    fn app() -> Harness<'static, App> {
        Harness::builder().build_eframe(|cc| App::for_test(cc))
    }

    #[test]
    fn undoing_a_deleted_group_puts_it_and_its_members_back() {
        let mut h = app();
        let a = h.state_mut();
        a.core.test_mut().conns.extend([1, 2, 3].map(|id| Conn::new(ConnMeta { id, ..Default::default() }, Status::Idle)));
        let (first, second) = (a.core.test_mut().groups.new_group("Prod"), a.core.test_mut().groups.new_group("Dev"));
        let all = a.conn_ids();
        a.core.test_mut().groups.place(1, Place::Into(first, None), &all);
        a.core.test_mut().groups.place(2, Place::Into(first, None), &all);
        a.core.test_mut().groups.place(3, Place::Into(second, None), &all);
        // What deleting the first group does: it goes, and its members are loose again.
        let gone = a.core.test_mut().groups.groups.remove(0);
        a.core.test_mut().groups.loose = vec![1, 2];
        a.toast_restore("Group Prod deleted.".into(), Restore::Group { g: gone, at: 0 });
        let id = a.core.restores[0].0;
        a.apply_restore(id);
        assert_eq!(a.core.groups.groups.iter().map(|g| (g.name.as_str(), g.members.clone())).collect::<Vec<_>>(), [("Prod", vec![1, 2]), ("Dev", vec![3])]);
        assert!(a.core.groups.loose.is_empty() && a.core.restores.is_empty());
        // A second press on the same toast finds nothing and changes nothing.
        a.apply_restore(id);
        assert_eq!(a.core.groups.groups.len(), 2);
    }

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("quarry-restore-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn undo_last(a: &mut App) {
        let id = a.core.restores.last().map(|r| r.0).expect("an Undo");
        a.apply_restore(id);
    }

    #[test]
    fn a_group_put_back_past_the_end_goes_last() {
        let mut h = app();
        let a = h.state_mut();
        a.core.test_mut().conns.extend([1, 2].map(|id| Conn::new(ConnMeta { id, ..Default::default() }, Status::Idle)));
        a.core.test_mut().groups.new_group("Dev");
        let g = Group { id: 9, name: "Prod".into(), members: vec![1, 7], ..Default::default() };
        a.toast_restore("Deleted.".into(), Restore::Group { g, at: 99 });
        undo_last(a);
        // 7 is no saved connection any more, so only 1 comes back.
        assert_eq!(a.core.groups.groups.iter().map(|g| (g.name.as_str(), g.members.clone())).collect::<Vec<_>>(), [("Dev", vec![]), ("Prod", vec![1])]);
    }

    #[test]
    fn undoing_a_rail_drop_skips_a_removed_connection_and_keeps_later_group_edits() {
        let mut h = app();
        let a = h.state_mut();
        a.core.test_mut().conns.extend([1, 2, 3].map(|id| Conn::new(ConnMeta { id, ..Default::default() }, Status::Idle)));
        a.core.test_mut().rail_order = vec![1, 2, 3];
        let all = a.conn_ids();
        // A drop of 2 onto 1 made a group of them.
        let moved = vec![(2, None, a.core.groups.spot(2, &all).1), (1, None, a.core.groups.spot(1, &all).1)];
        let g = a.core.test_mut().groups.new_group("New group");
        a.core.test_mut().groups.place(1, Place::Into(g, None), &all);
        a.core.test_mut().groups.place(2, Place::Into(g, None), &all);
        a.toast_restore("Grouped.".into(), Restore::Placed { moved, created: Some(g), tile: 2, prev: Some(1), at: 1 });
        // Since then: the group was renamed, 3 joined it, and 2 was removed from the app.
        a.core.test_mut().groups.group_mut(g).unwrap().name = "Prod".into();
        a.core.test_mut().groups.place(3, Place::Into(g, None), &all);
        // Removing a connection prunes it from the groups, as Danger::RemoveConnection does.
        a.core.test_mut().conns.retain(|c| c.meta.id != 2);
        a.core.test_mut().rail_order.retain(|c| *c != 2);
        let alive = a.conn_ids();
        a.core.test_mut().groups.prune(&alive);
        undo_last(a);
        let alive = a.conn_ids();
        assert_eq!(a.core.groups.group(g).map(|x| (x.name.as_str(), x.members.clone())), Some(("Prod", vec![3])));
        assert_eq!(a.core.groups.loose_order(&alive), [1]);
        assert!(!a.core.rail_order.contains(&2) && a.core.groups.groups.iter().all(|x| !x.members.contains(&2)));
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
        assert!(a.core.library.scripts.iter().any(|s| s.id == 4));
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
        a.core.test_mut().library.links.push(crate::library::Link { id: 7, path: d.clone(), ..Default::default() });
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
        assert!(a.core.library.scripts.iter().any(|s| s.id == 4));
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
        let id = a.core.restores[0].0;
        a.apply_restore(id);
        assert_eq!(std::fs::read(&path).unwrap(), b"select 1");
        std::fs::write(&path, "mine").unwrap();
        a.toast_restore("Deleted.".into(), Restore::File { path: path.clone(), bytes: b"select 2".to_vec() });
        let id = a.core.restores[0].0;
        a.apply_restore(id);
        assert_eq!(std::fs::read(&path).unwrap(), b"mine");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
