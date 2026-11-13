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

fn rescan(a: &mut App, dir: &Path) {
    let scan = disk::scan(dir, &["sql".to_string()], None);
    a.apply_scan(1, scan);
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
    assert_eq!(std::fs::read_to_string(dir.join("a.sql")).unwrap(), "select 22");
    assert_eq!(h.state().tabs[0].script.as_ref().unwrap().conflict.as_deref(), Some("select 22"));
    // A clean tab saving an unchanged file writes it, and the file keeps nothing but the text.
    let dir2 = temp_dir("write");
    let mut h = bound_app(&dir2, "select 1", false);
    h.state_mut().tabs[0].sql.as_mut().unwrap().query = "select 5".into();
    assert!(h.state_mut().save_linked());
    assert_eq!(std::fs::read_to_string(dir2.join("a.sql")).unwrap(), "select 5");
    assert_eq!(std::fs::read_dir(&dir2).unwrap().count(), 1);
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::remove_dir_all(&dir2).unwrap();
}
