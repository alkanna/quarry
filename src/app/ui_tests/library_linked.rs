//! UI tests of linked folders: the Link a folder dialog, and how a tab bound to a file follows it on disk. They read and
//! write a folder of their own under the temp directory and need no server, so they run without any QUARRY_TEST_* variable.
use super::*;
use crate::app::library::linked::read_script;
use crate::library::{Link, ScriptRef, disk};
use std::path::{Path, PathBuf};

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("quarry-linked-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn put(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn harness() -> Harness<'static, App> {
    Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_accessibility_check(false).build_eframe(|cc| App::for_test(cc))
}

/// An app with a MySQL tab on the file `a.sql` of link 1, opened with `text`.
fn bound_app(dir: &Path, text: &str, follow: bool) -> Harness<'static, App> {
    put(dir, "a.sql", text);
    let mut h = harness();
    let a = h.state_mut();
    a.conns.push(Conn::new(ConnMeta { id: 1, name: "shop".into(), engine: db::sql::Engine::MySql, ..Default::default() }, Status::Idle));
    a.library.links.push(Link { id: 1, path: dir.to_path_buf(), name: "queries".into(), on: "all".into(), exts: vec!["sql".into()], follow, ..Default::default() });
    let mut t = Tab::new(Sel { conn: 1, db: "shop".into(), schema: String::new(), coll: String::new() }).for_engine(db::sql::Engine::MySql);
    t.view = View::Sql;
    t.sql.as_mut().unwrap().query = text.to_string();
    let meta = read_script(&dir.join("a.sql")).unwrap().meta;
    t.script = Some(Bound { r: ScriptRef::File { link: 1, rel: "a.sql".into() }, view: View::Sql, base: text.to_string(), disk: meta, conflict: None });
    a.tabs.push(t);
    a.cur = 0;
    h
}

/// A scan as the worker makes it: the files open in tabs are read in full when they changed.
fn rescan(a: &mut App, dir: &Path) {
    let open: Vec<(PathBuf, disk::Meta)> = a.tabs.iter().filter_map(|t| t.script.as_ref()).filter_map(|b| match &b.r {
        ScriptRef::File { rel, .. } => Some((rel.clone(), b.disk)),
        _ => None,
    }).collect();
    let scan = disk::scan_reading(dir, &["sql".to_string()], None, &open);
    a.apply_scan(1, scan);
}

/// Steps frames until the linked-file reads and writes on the worker are back.
fn io(h: &mut Harness<'static, App>) {
    let t = std::time::Instant::now();
    while !h.state().lib_ui.jobs.is_empty() {
        assert!(t.elapsed() < Duration::from_secs(20), "a linked-file job never came back");
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    h.step();
}

fn query(a: &App) -> String {
    a.tabs[0].sql.as_ref().unwrap().query.clone()
}

#[test]
fn linking_a_folder_through_the_dialog_saves_the_link_and_scans_it() {
    let dir = temp_dir("dialog");
    put(&dir, "a.sql", "select 1");
    put(&dir, "sub/b.sql", "-- quarry: db=shop\nselect 2");
    put(&dir, "notes.txt", "x");
    let mut ui = UiApp { h: harness() };
    let a = ui.h.state_mut();
    a.conns.push(Conn::new(ConnMeta { id: 1, name: "shop".into(), engine: db::sql::Engine::MySql, ..Default::default() }, Status::Idle));
    a.active = Some(1);
    a.open_link_dialog(None);
    ui.show("Link a folder");
    ui.fill("Folder path", &dir.to_string_lossy());
    // The scan waits for the path to rest, then runs off the UI thread.
    ui.show("2 scripts in 2 folders");
    ui.click("Link folder");
    let link = ui.app().library.links.first().cloned().expect("a link");
    assert_eq!((link.path.clone(), link.on.as_str(), link.conns.clone(), link.follow), (dir.clone(), "conns", vec![1], true));
    assert_eq!(link.name, dir.file_name().unwrap().to_string_lossy());
    assert!(link.exts.iter().any(|e| e == "sql") && ui.app().modal.is_none());
    ui.wait("the link to be scanned", |a| a.lib_ui.scans.get(&link.id).is_some_and(|s| s.entries.len() == 2));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_dialog_refuses_a_missing_folder_and_a_second_link_to_the_same_one() {
    let dir = temp_dir("refuse");
    let mut ui = UiApp { h: harness() };
    let a = ui.h.state_mut();
    a.conns.push(Conn::new(ConnMeta { id: 1, name: "shop".into(), ..Default::default() }, Status::Idle));
    a.active = Some(1);
    a.library.links.push(Link { id: 5, path: dir.clone(), name: "queries".into(), on: "all".into(), exts: vec!["sql".into()], ..Default::default() });
    a.open_link_dialog(None);
    ui.fill("Folder path", &dir.join("nope").to_string_lossy());
    ui.show("That folder does not exist");
    ui.click("Cancel");
    ui.h.state_mut().open_link_dialog(None);
    ui.fill("Folder path", &dir.to_string_lossy());
    ui.show("Already linked as queries");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_saved_file_the_tab_already_has_only_updates_what_is_remembered() {
    let dir = temp_dir("same");
    let mut h = bound_app(&dir, "select 1", false);
    // Touched with the same text: the length is the same, so the time is what tells.
    std::thread::sleep(Duration::from_millis(20));
    put(&dir, "a.sql", "select 1");
    rescan(h.state_mut(), &dir);
    let b = h.state().tabs[0].script.as_ref().unwrap();
    assert!(b.conflict.is_none() && b.disk == read_script(&dir.join("a.sql")).unwrap().meta);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_clean_tab_reloads_when_the_link_follows_and_shows_the_banner_when_it_does_not() {
    let dir = temp_dir("follow");
    let mut h = bound_app(&dir, "select 1", true);
    put(&dir, "a.sql", "select 22");
    rescan(h.state_mut(), &dir);
    assert_eq!(query(h.state()), "select 22");
    let b = h.state().tabs[0].script.as_ref().unwrap();
    assert!((b.base.as_str(), b.conflict.is_none()) == ("select 22", true));

    let dir2 = temp_dir("nofollow");
    let mut h = bound_app(&dir2, "select 1", false);
    put(&dir2, "a.sql", "select 22");
    rescan(h.state_mut(), &dir2);
    assert_eq!(query(h.state()), "select 1");
    assert_eq!(h.state().tabs[0].script.as_ref().unwrap().conflict.as_deref(), Some("select 22"));
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&dir2).unwrap();
}

#[test]
fn an_edited_tab_gets_a_conflict_and_keep_mine_lets_cmd_s_replace_the_file() {
    let dir = temp_dir("conflict");
    let mut h = bound_app(&dir, "select 1", true);
    h.state_mut().tabs[0].sql.as_mut().unwrap().query = "select 3".into();
    put(&dir, "a.sql", "select 22");
    rescan(h.state_mut(), &dir);
    let b = h.state().tabs[0].script.as_ref().unwrap();
    assert_eq!((query(h.state()).as_str(), b.conflict.as_deref(), b.base.as_str()), ("select 3", Some("select 22"), "select 1"));
    // The banner buttons call these; the tab's view is not drawn here because its connection is offline.
    h.state_mut().conflict_compare();
    assert!(matches!(&h.state().modal, Some(Modal::Text { body, .. }) if body.contains("- select 3") && body.contains("+ select 22")));
    h.state_mut().modal = None;
    // Saving with the conflict open writes nothing.
    assert!(h.state_mut().save_linked());
    assert_eq!(std::fs::read_to_string(dir.join("a.sql")).unwrap(), "select 22");
    h.state_mut().conflict_keep();
    assert!(h.state().tabs[0].script.as_ref().unwrap().conflict.is_none());
    assert!(h.state_mut().save_linked());
    io(&mut h);
    assert_eq!(std::fs::read_to_string(dir.join("a.sql")).unwrap(), "select 3");
    let b = h.state().tabs[0].script.as_ref().unwrap();
    assert_eq!(b.base, "select 3");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn reload_replaces_the_edits_with_the_file() {
    let dir = temp_dir("reload");
    let mut h = bound_app(&dir, "select 1", false);
    h.state_mut().tabs[0].sql.as_mut().unwrap().query = "select 3".into();
    put(&dir, "a.sql", "select 22");
    rescan(h.state_mut(), &dir);
    h.state_mut().conflict_reload();
    io(&mut h);
    assert_eq!(query(h.state()), "select 22");
    assert!(h.state().tabs[0].script.as_ref().unwrap().conflict.is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn cmd_s_on_a_file_that_changed_since_it_loaded_raises_the_conflict_instead_of_writing() {
    let dir = temp_dir("race");
    let mut h = bound_app(&dir, "select 1", false);
    h.state_mut().tabs[0].sql.as_mut().unwrap().query = "select 3".into();
    // Changed behind the app's back, before any scan noticed.
    put(&dir, "a.sql", "select 22");
    assert!(h.state_mut().save_linked());
    io(&mut h);
    assert_eq!(std::fs::read_to_string(dir.join("a.sql")).unwrap(), "select 22");
    assert_eq!(h.state().tabs[0].script.as_ref().unwrap().conflict.as_deref(), Some("select 22"));
    // A clean tab saving an unchanged file writes it, and the file keeps nothing but the text.
    let dir2 = temp_dir("write");
    let mut h = bound_app(&dir2, "select 1", false);
    h.state_mut().tabs[0].sql.as_mut().unwrap().query = "select 5".into();
    assert!(h.state_mut().save_linked());
    io(&mut h);
    assert_eq!(std::fs::read_to_string(dir2.join("a.sql")).unwrap(), "select 5");
    assert_eq!(std::fs::read_dir(&dir2).unwrap().count(), 1);
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&dir2).unwrap();
}

// The Library panel over linked folders, the open and save paths and the file operations. They run on a folder of their
// own under the temp directory, on a connection that is only declared, so no server is involved.

use crate::app::library::open::How;

fn panel_app(dir: &Path, files: &[(&str, &str)]) -> Harness<'static, App> {
    for (rel, text) in files {
        put(dir, rel, text);
    }
    let mut h = harness();
    let a = h.state_mut();
    a.conns.push(Conn::new(ConnMeta { id: 1, name: "shop".into(), engine: db::sql::Engine::MySql, ..Default::default() }, Status::Ready));
    a.active = Some(1);
    a.library.links.push(Link { id: 1, path: dir.to_path_buf(), name: "queries".into(), on: "all".into(), exts: vec!["sql".into(), "js".into(), "json".into()], follow: true, ..Default::default() });
    a.prefs.set_side_mode(SideMode::Library);
    a.prefs.sidebar_hidden = false;
    // The scan is made here, so no timer decides when the rows appear.
    a.lib_ui.frozen = true;
    a.lib_ui.scans.insert(1, disk::scan(dir, &["sql".into(), "js".into(), "json".into()], None));
    h.run_steps(3);
    h
}

impl UiApp {
    fn lk_shown(&self, label: &str) -> bool {
        self.h.query_all_by_label(label).next().is_some()
    }

    /// Rests the pointer on a row, then opens its dots menu and picks an item.
    fn lk_menu(&mut self, row: &str, item: &str) {
        self.show(row);
        let at = self.h.get_by_label(row).rect().center();
        self.h.hover_at(at);
        self.h.run_steps(2);
        self.click(&format!("More for {row}"));
        self.click(item);
    }
}

#[test]
fn the_panel_shows_the_files_of_a_linked_folder_and_its_subfolders() {
    let dir = temp_dir("tree");
    let h = panel_app(&dir, &[("a.sql", "select 1"), ("sub/b.sql", "select 2"), ("notes.txt", "x")]);
    let mut ui = UiApp { h };
    assert!(ui.lk_shown("queries") && !ui.lk_shown("a.sql"));
    ui.click("queries");
    assert!(ui.lk_shown("a.sql") && ui.lk_shown("sub") && !ui.lk_shown("b.sql") && !ui.lk_shown("notes.txt"));
    ui.click("sub");
    assert!(ui.lk_shown("b.sql"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_missing_folder_shows_not_found_and_no_files() {
    let dir = temp_dir("gone");
    let mut h = panel_app(&dir, &[("a.sql", "select 1")]);
    h.state_mut().lib_ui.scans.insert(1, disk::Scan { missing: true, ..Default::default() });
    h.run_steps(3);
    let ui = UiApp { h };
    assert!(ui.lk_shown("queries (not found)"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn opening_a_file_binds_the_tab_and_cmd_s_writes_it_back_without_adding_a_header() {
    let dir = temp_dir("open");
    let h = panel_app(&dir, &[("a.sql", "-- quarry: db=shop\nselect 1")]);
    let mut ui = UiApp { h };
    ui.h.state_mut().open_script(&ScriptRef::File { link: 1, rel: "a.sql".into() }, How::Here);
    io(&mut ui.h);
    let b = ui.app().tabs[ui.app().cur].script.as_ref().expect("a bound tab");
    assert!(matches!(&b.r, ScriptRef::File { link: 1, rel } if rel == Path::new("a.sql")) && b.disk.is_some() && b.view == View::Sql);
    assert_eq!(ui.app().tabs[ui.app().cur].sel.db, "shop");
    let cur = ui.app().cur;
    ui.h.state_mut().tabs[cur].sql.as_mut().unwrap().query = "-- quarry: db=shop\nselect 2".into();
    ui.h.state_mut().save_script();
    io(&mut ui.h);
    assert_eq!(std::fs::read_to_string(dir.join("a.sql")).unwrap(), "-- quarry: db=shop\nselect 2");
    assert!(ui.app().modal.is_none(), "a bound file saves without the dialog");
    // A file with no header gets none.
    put(&dir, "plain.sql", "select 1");
    ui.h.state_mut().lib_ui.scans.insert(1, disk::scan(&dir, &["sql".into()], None));
    ui.h.state_mut().open_script(&ScriptRef::File { link: 1, rel: "plain.sql".into() }, How::NewTab);
    io(&mut ui.h);
    let cur = ui.app().cur;
    ui.h.state_mut().tabs[cur].sql.as_mut().unwrap().query = "select 9".into();
    ui.h.state_mut().save_script();
    io(&mut ui.h);
    assert_eq!(std::fs::read_to_string(dir.join("plain.sql")).unwrap(), "select 9");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn deleting_a_file_moves_it_to_the_trash_and_undo_puts_its_bytes_back() {
    let dir = temp_dir("trash");
    let h = panel_app(&dir, &[("a.sql", "select 1")]);
    let mut ui = UiApp { h };
    ui.click("queries");
    ui.lk_menu("a.sql", "Move to Trash\u{2026}");
    io(&mut ui.h);
    assert!(!dir.join("a.sql").exists());
    assert!(ui.app().lib_ui.scans[&1].entries.is_empty());
    let id = ui.app().restores.first().map(|r| r.0).expect("an Undo toast");
    ui.h.state_mut().apply_restore(id);
    assert_eq!(std::fs::read_to_string(dir.join("a.sql")).unwrap(), "select 1");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn copying_a_file_out_of_its_folder_leaves_the_file_and_adds_a_library_copy() {
    let dir = temp_dir("out");
    let h = panel_app(&dir, &[("a.sql", "-- quarry: db=shop\nselect 1")]);
    let mut ui = UiApp { h };
    ui.h.state_mut().file_to_quarry(1, Path::new("a.sql"), crate::library::ops::DropAt::Root);
    io(&mut ui.h);
    assert!(dir.join("a.sql").exists());
    let s = ui.app().library.scripts.iter().find(|s| s.name == "a").expect("a copy");
    assert_eq!(ui.app().lib_ui.panel.sel, Some(crate::library::tree::Node::Script(s.id)));
    assert_eq!((s.name.as_str(), s.kind.as_str(), s.db.as_str(), s.conn), ("a", "sql", "shop", 1));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_library_script_dropped_on_a_linked_folder_becomes_a_file_and_leaves_the_library() {
    let dir = temp_dir("into");
    let h = panel_app(&dir, &[("a.sql", "select 1")]);
    let mut ui = UiApp { h };
    let a = ui.h.state_mut();
    a.library.scripts.push(crate::library::Script { id: 7, name: "Revenue".into(), kind: "sql".into(), body: "select sum(1)".into(), conn: 1, ..Default::default() });
    let rel = a.script_to_link(7, 1, Path::new("")).expect("a file");
    assert_eq!(rel, Path::new("Revenue.sql"));
    assert_eq!(std::fs::read_to_string(dir.join("Revenue.sql")).unwrap(), "select sum(1)");
    assert!(a.library.scripts.iter().all(|s| s.id != 7));
    // Undo brings the script back and takes the file it wrote away again.
    let id = a.restores.first().map(|r| r.0).unwrap();
    a.apply_restore(id);
    assert!(a.library.scripts.iter().any(|s| s.name == "Revenue"), "Undo did not bring the script back");
    assert!(!dir.join("Revenue.sql").exists(), "Undo left the file the move wrote");
    // A file edited since the move is kept by the undo.
    let s = a.library.scripts.iter().find(|s| s.name == "Revenue").map(|s| s.id).unwrap();
    a.script_to_link(s, 1, Path::new("")).expect("a file again");
    std::fs::write(dir.join("Revenue.sql"), "select 2").unwrap();
    let id = a.restores.last().map(|r| r.0).unwrap();
    a.apply_restore(id);
    assert!(dir.join("Revenue.sql").exists(), "Undo removed a file that was edited after the move");
    assert_eq!(a.library.scripts.iter().filter(|s| s.name == "Revenue").count(), 1);
    // A folder that does not show .js files refuses a shell script.
    a.library.links[0].exts = vec!["sql".into()];
    a.library.scripts.push(crate::library::Script { id: 8, name: "Job".into(), kind: "shell".into(), body: "1".into(), conn: 1, ..Default::default() });
    assert!(a.script_to_link(8, 1, Path::new("")).is_none() && a.library.scripts.iter().any(|s| s.id == 8));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_file_moves_between_the_folders_of_its_link_and_renames_on_disk() {
    let dir = temp_dir("move");
    let h = panel_app(&dir, &[("a.sql", "select 1"), ("sub/b.sql", "select 2")]);
    let mut ui = UiApp { h };
    let a = ui.h.state_mut();
    assert_eq!(a.move_linked_file(1, Path::new("a.sql"), Path::new("sub")), Some(PathBuf::from("sub/a.sql")));
    assert!(!dir.join("a.sql").exists() && dir.join("sub/a.sql").exists());
    // The name in the folder is taken, so the moved one is numbered, never overwritten.
    put(&dir, "a.sql", "again");
    assert_eq!(a.move_linked_file(1, Path::new("a.sql"), Path::new("sub")), Some(PathBuf::from("sub/a 2.sql")));
    assert_eq!(std::fs::read_to_string(dir.join("sub/a.sql")).unwrap(), "select 1");
    assert_eq!(a.rename_linked(1, Path::new("sub/b.sql"), false, "renamed"), Some(PathBuf::from("sub/renamed.sql")));
    assert!(dir.join("sub/renamed.sql").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_save_dialog_writes_a_new_file_into_a_linked_folder_and_binds_the_tab() {
    let dir = temp_dir("save");
    let h = panel_app(&dir, &[("a.sql", "select 1")]);
    let mut ui = UiApp { h };
    ui.h.state_mut().open_script(&ScriptRef::File { link: 1, rel: "a.sql".into() }, How::NewTab);
    io(&mut ui.h);
    // The tab is unbound again and holds its own text.
    let a = ui.h.state_mut();
    let cur = a.cur;
    a.tabs[cur].script = None;
    a.tabs[cur].sql.as_mut().unwrap().query = "select 42".into();
    a.save_script();
    assert!(matches!(a.modal, Some(Modal::SaveScript(_))));
    if let Some(Modal::SaveScript(f)) = &mut a.modal {
        (f.link, f.name) = (Some(1), "Answer".into());
    }
    ui.h.run_steps(3);
    ui.show("File name");
    ui.click("Save");
    io(&mut ui.h);
    assert_eq!(std::fs::read_to_string(dir.join("Answer.sql")).unwrap(), "select 42");
    let a = ui.app();
    assert!(matches!(a.tabs[a.cur].script.as_ref().map(|b| &b.r), Some(ScriptRef::File { link: 1, rel }) if rel == Path::new("Answer.sql")));
    assert!(a.lib_ui.scans[&1].entries.iter().any(|e| e.rel == Path::new("Answer.sql")));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_new_script_and_a_new_folder_are_made_on_disk_and_listed_at_once() {
    let dir = temp_dir("new");
    let h = panel_app(&dir, &[("a.sql", "select 1")]);
    let mut ui = UiApp { h };
    let a = ui.h.state_mut();
    let rel = a.new_linked_script(1, Path::new("")).unwrap();
    assert_eq!(rel, Path::new("New script.sql"));
    assert!(dir.join("New script.sql").exists() && a.lib_ui.scans[&1].entries.iter().any(|e| e.rel == rel));
    let d = a.new_linked_dir(1, Path::new("")).unwrap();
    assert!(dir.join(&d).is_dir() && a.lib_ui.scans[&1].empty_dirs.contains(&d));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_file_that_changed_on_disk_gets_a_dot_until_it_is_opened() {
    let dir = temp_dir("dot");
    let mut h = panel_app(&dir, &[("a.sql", "-- quarry: db=shop\nselect 1")]);
    // The first scan is the baseline: a link just made shows nothing changed.
    let scan = disk::scan(&dir, &["sql".into()], None);
    h.state_mut().apply_scan(1, scan);
    assert!(h.state().lib_ui.changed.is_empty());
    std::thread::sleep(Duration::from_millis(20));
    put(&dir, "a.sql", "-- quarry: db=shop\nselect 11");
    let scan = disk::scan(&dir, &["sql".into()], None);
    h.state_mut().apply_scan(1, scan);
    assert!(h.state().lib_ui.changed.contains(&(1, PathBuf::from("a.sql"))));
    h.state_mut().open_script(&ScriptRef::File { link: 1, rel: "a.sql".into() }, How::Here);
    io(&mut h);
    assert!(h.state().lib_ui.changed.is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_changed_file_is_read_by_the_scan_and_not_on_the_ui_thread() {
    let dir = temp_dir("offthread");
    let mut h = bound_app(&dir, "select 1", true);
    put(&dir, "a.sql", "select 22");
    // A scan that found the change but read nothing (as one without the tab's file would) changes nothing: the UI
    // thread never reads the file itself.
    let scan = disk::scan(&dir, &["sql".to_string()], None);
    h.state_mut().apply_scan(1, scan);
    assert_eq!(query(h.state()), "select 1");
    // The worker's scan reads the open file, and the tab follows it.
    rescan(h.state_mut(), &dir);
    assert_eq!(query(h.state()), "select 22");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_save_dialog_looks_for_the_file_once_per_path() {
    let dir = temp_dir("exists");
    let h = panel_app(&dir, &[("a.sql", "select 1")]);
    let mut ui = UiApp { h };
    ui.h.state_mut().open_script(&ScriptRef::File { link: 1, rel: "a.sql".into() }, How::NewTab);
    io(&mut ui.h);
    let a = ui.h.state_mut();
    let cur = a.cur;
    a.tabs[cur].script = None;
    a.tabs[cur].sql.as_mut().unwrap().query = "select 42".into();
    a.save_script();
    if let Some(Modal::SaveScript(f)) = &mut a.modal {
        (f.link, f.name) = (Some(1), "a".into());
    }
    ui.h.run_steps(3);
    ui.show("Replaces a.sql in this folder.");
    // Gone from disk while the dialog is up: the answer is kept until the name or folder changes.
    std::fs::remove_file(dir.join("a.sql")).unwrap();
    ui.h.run_steps(3);
    let cached = |ui: &UiApp| match &ui.app().modal {
        Some(Modal::SaveScript(f)) => f.exists.clone(),
        _ => None,
    };
    assert_eq!(cached(&ui), Some((dir.join("a.sql"), true)));
    if let Some(Modal::SaveScript(f)) = &mut ui.h.state_mut().modal {
        f.name = "b".into();
    }
    ui.h.run_steps(3);
    assert_eq!(cached(&ui), Some((dir.join("b.sql"), false)));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_second_cmd_s_before_the_first_is_back_raises_no_conflict() {
    let dir = temp_dir("twice");
    let mut h = bound_app(&dir, "select 1", false);
    h.state_mut().tabs[0].sql.as_mut().unwrap().query = "select 5".into();
    assert!(h.state_mut().save_linked());
    assert!(h.state_mut().save_linked());
    io(&mut h);
    assert_eq!(std::fs::read_to_string(dir.join("a.sql")).unwrap(), "select 5");
    let b = h.state().tabs[0].script.as_ref().unwrap();
    assert!(b.conflict.is_none() && b.base == "select 5", "{:?}", b.conflict);
    // A save after it is back goes through as well, from the time and text it left.
    h.state_mut().tabs[0].sql.as_mut().unwrap().query = "select 6".into();
    assert!(h.state_mut().save_linked());
    io(&mut h);
    assert_eq!(std::fs::read_to_string(dir.join("a.sql")).unwrap(), "select 6");
    assert!(h.state().tabs[0].script.as_ref().unwrap().conflict.is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn cmd_s_on_a_file_deleted_on_disk_writes_nothing_until_asked_again() {
    let dir = temp_dir("deleted");
    let mut h = bound_app(&dir, "select 1", false);
    h.state_mut().tabs[0].sql.as_mut().unwrap().query = "select 3".into();
    std::fs::remove_file(dir.join("a.sql")).unwrap();
    rescan(h.state_mut(), &dir);
    assert!(h.state_mut().save_linked());
    io(&mut h);
    // Before, the save recreated the file without a word.
    assert!(!dir.join("a.sql").exists(), "a file deleted on disk came back on Cmd+S");
    assert!(h.state().last_problem().contains("deleted on disk"), "{}", h.state().last_problem());
    assert!(h.state().tabs[0].script.as_ref().is_some_and(|b| b.disk.is_none()));
    // Told once, the second Cmd+S writes it back.
    assert!(h.state_mut().save_linked());
    io(&mut h);
    assert_eq!(std::fs::read_to_string(dir.join("a.sql")).unwrap(), "select 3");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_file_changed_to_what_the_tab_holds_raises_no_conflict() {
    let dir = temp_dir("sametext");
    let mut h = bound_app(&dir, "select 1", false);
    h.state_mut().tabs[0].sql.as_mut().unwrap().query = "select 3".into();
    put(&dir, "a.sql", "select 3");
    rescan(h.state_mut(), &dir);
    let b = h.state().tabs[0].script.as_ref().unwrap();
    assert!(b.conflict.is_none(), "a conflict between two equal texts: {:?}", b.conflict);
    assert_eq!(b.base, "select 3");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn saving_over_a_linked_file_flags_the_tabs_on_it_and_undo_brings_the_old_bytes_back() {
    let dir = temp_dir("replace");
    let h = panel_app(&dir, &[("a.sql", "select 1")]);
    let mut ui = UiApp { h };
    // One tab holds edits to a.sql; another, unbound, is saved over it.
    ui.h.state_mut().open_script(&ScriptRef::File { link: 1, rel: "a.sql".into() }, How::NewTab);
    io(&mut ui.h);
    let edited = ui.app().cur;
    ui.h.state_mut().tabs[edited].sql.as_mut().unwrap().query = "select 7".into();
    ui.h.state_mut().open_script(&ScriptRef::File { link: 1, rel: "a.sql".into() }, How::NewTab);
    io(&mut ui.h);
    let a = ui.h.state_mut();
    let cur = a.cur;
    assert_ne!(cur, edited);
    a.tabs[cur].script = None;
    a.tabs[cur].sql.as_mut().unwrap().query = "select 42".into();
    a.save_script();
    if let Some(Modal::SaveScript(f)) = &mut a.modal {
        (f.link, f.name) = (Some(1), "a".into());
    }
    ui.h.run_steps(3);
    ui.show("Replaces a.sql in this folder.");
    ui.click_last("Save");
    io(&mut ui.h);
    assert_eq!(std::fs::read_to_string(dir.join("a.sql")).unwrap(), "select 42");
    let other = ui.app().tabs[edited].script.as_ref().unwrap();
    assert_eq!(other.conflict.as_deref(), Some("select 42"), "the tab with edits to the replaced file was not told");
    let id = ui.app().restores.last().map(|r| r.0).expect("an Undo for the replaced file");
    ui.h.state_mut().apply_restore(id);
    assert_eq!(std::fs::read_to_string(dir.join("a.sql")).unwrap(), "select 1");
    // An Undo after the file was edited again keeps the edit.
    std::fs::write(dir.join("a.sql"), "select 9").unwrap();
    ui.h.state_mut().toast_restore("Replaced.".into(), crate::app::restore::Restore::Replaced { link: 1, path: dir.join("a.sql"), old: b"select 1".to_vec(), new: b"select 42".to_vec() });
    let id = ui.app().restores.last().map(|r| r.0).unwrap();
    ui.h.state_mut().apply_restore(id);
    assert_eq!(std::fs::read_to_string(dir.join("a.sql")).unwrap(), "select 9");
    std::fs::remove_dir_all(&dir).unwrap();
}
