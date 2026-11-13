//! UI tests of opening and running library scripts: the connection and database a script lands on, what Run does on a
//! read-only or a writable connection, a script that waits for its connection, and the keys of the Library tree. They need
//! no server: the SQL ones use the MySQL demo, the linked ones a folder of their own under the temp directory.

use super::*;
use crate::app::library::open::How;
use crate::library::{Link, Script, ScriptRef, disk};
use std::path::{Path, PathBuf};

const CMD: egui::Modifiers = egui::Modifiers::COMMAND;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("quarry-open-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn harness() -> Harness<'static, App> {
    Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_accessibility_check(false).build_eframe(|cc| App::for_test(cc))
}

/// The MySQL demo with an empty Query tab on shop current.
fn sql_query(a: &mut App) {
    a.load_sql_demo();
    a.sql_new_query(3, "shop".into());
}

fn add(a: &mut App, kind: &str, conn: u64, name: &str, body: &str, db: &str, coll: &str) -> u64 {
    let id = a.library.next_id();
    a.library.scripts.push(Script { id, name: name.into(), kind: kind.into(), body: body.into(), conn, db: db.into(), coll: coll.into(), ..Default::default() });
    id
}

fn open(ui: &mut UiApp, id: u64, how: How) {
    ui.h.state_mut().open_script(&ScriptRef::Saved(id), how);
    ui.settle();
}

fn sql_of(a: &App) -> &SqlTab {
    a.t().and_then(|t| t.sql.as_ref()).expect("a SQL tab")
}

/// A scan as the worker makes it: the files open in tabs are read in full when they changed.
fn rescan(a: &mut App, link: u64, dir: &Path) {
    let open: Vec<(PathBuf, disk::Meta)> = a.tabs.iter().filter_map(|t| t.script.as_ref()).filter_map(|b| match &b.r {
        ScriptRef::File { rel, .. } => Some((rel.clone(), b.disk)),
        _ => None,
    }).collect();
    a.apply_scan(link, disk::scan_reading(dir, &["sql".to_string()], None, &open));
}

#[test]
fn a_restored_tab_on_a_linked_file_keeps_its_edits_when_the_file_is_read() {
    // After a restart the tab holds what was typed, and nothing says what the file held when it was opened. The first scan
    // must not take the edits for a clean tab and reload the file over them, even on a link that follows changes.
    let dir = temp_dir("restored");
    std::fs::write(dir.join("a.sql"), "select 1").unwrap();
    let mut h = harness();
    let a = h.state_mut();
    a.conns.push(Conn::new(ConnMeta { id: 1, name: "shop".into(), engine: db::sql::Engine::MySql, ..Default::default() }, Status::Idle));
    a.library.links.push(Link { id: 1, path: dir.clone(), name: "queries".into(), on: "all".into(), exts: vec!["sql".into()], follow: true, ..Default::default() });
    let mut t = Tab::new(Sel { conn: 1, db: "shop".into(), schema: String::new(), coll: String::new() }).for_engine(db::sql::Engine::MySql);
    t.view = View::Sql;
    t.sql.as_mut().unwrap().query = "select 1 -- mine".into();
    t.script = Some(a.rebind(&t, ScriptRef::File { link: 1, rel: "a.sql".into() }).expect("bound again"));
    a.tabs.push(t);
    a.cur = 0;
    rescan(a, 1, &dir);
    let t = &a.tabs[0];
    assert_eq!(t.sql.as_ref().unwrap().query, "select 1 -- mine", "the edits were replaced by the file");
    assert_eq!(t.script.as_ref().unwrap().conflict.as_deref(), Some("select 1"), "the difference shows as a conflict");
    // A restored tab that holds what the file holds is simply clean.
    let a = h.state_mut();
    a.tabs[0].sql.as_mut().unwrap().query = "select 1".into();
    let b = a.rebind(&a.tabs[0], ScriptRef::File { link: 1, rel: "a.sql".into() }).unwrap();
    a.tabs[0].script = Some(b);
    rescan(a, 1, &dir);
    assert!(a.tabs[0].script.as_ref().unwrap().conflict.is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_reused_query_tab_runs_the_script_in_its_own_database() {
    let Some(mut ui) = demo_ui(sql_query) else { return };
    let tabs = ui.app().tabs.len();
    {
        let a = ui.h.state_mut();
        let c = a.cur;
        a.tabs[c].sql.as_mut().unwrap().run_db = "analytics".into();
    }
    let id = add(ui.h.state_mut(), "sql", 3, "Late", "SELECT 1", "shop", "");
    open(&mut ui, id, How::Here);
    assert_eq!(ui.app().tabs.len(), tabs, "the empty tab is reused");
    let s = sql_of(ui.app());
    assert!(s.run_db.is_empty() || s.run_db == "shop", "the script runs in {:?}, not shop", s.run_db);
}

#[test]
fn cmd_s_writes_back_the_database_the_tab_runs_in() {
    let Some(mut ui) = demo_ui(sql_query) else { return };
    let id = add(ui.h.state_mut(), "sql", 3, "Late", "SELECT 1", "shop", "");
    open(&mut ui, id, How::Here);
    {
        let a = ui.h.state_mut();
        let c = a.cur;
        let s = a.tabs[c].sql.as_mut().unwrap();
        (s.query, s.run_db) = ("SELECT 2".into(), "analytics".into());
    }
    ui.key(CMD, egui::Key::S);
    let s = ui.app().library.scripts.iter().find(|s| s.id == id).unwrap();
    assert_eq!((s.body.as_str(), s.db.as_str()), ("SELECT 2", "analytics"));
}

#[test]
fn a_script_of_another_engine_is_refused_without_a_half_built_tab() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    let tabs = ui.app().tabs.len();
    let id = add(ui.h.state_mut(), "sql", 1, "Wrong engine", "SELECT 1", "", "");
    open(&mut ui, id, How::Here);
    assert_eq!(ui.app().tabs.len(), tabs, "a SQL script opened a tab on a MongoDB connection");
    assert!(ui.app().last_problem().contains("Not opened"), "{}", ui.app().last_problem());

    let Some(mut ui) = demo_ui(sql_query) else { return };
    let tabs = ui.app().tabs.len();
    let id = add(ui.h.state_mut(), "filter", 3, "Mongo filter", "{ a: 1 }", "shop", "orders");
    open(&mut ui, id, How::Here);
    assert_eq!(ui.app().tabs.len(), tabs, "a filter opened a tab on a MySQL connection");
    assert!(ui.app().last_problem().contains("Not opened"), "{}", ui.app().last_problem());
}

#[test]
fn a_migrated_filter_with_no_collection_says_so_and_opens_nothing() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    {
        let a = ui.h.state_mut();
        a.prefs.library.insert("1/shop".into(), vec![SavedQuery { name: "Loose".into(), filter: "{ a: 1 }".into(), ..Default::default() }]);
        a.migrate_library();
    }
    let s = ui.app().library.scripts.iter().find(|s| s.name == "Loose").cloned().expect("migrated");
    assert_eq!((s.kind.as_str(), s.db.as_str(), s.coll.as_str()), ("filter", "shop", ""));
    let tabs = ui.app().tabs.len();
    let sel = ui.app().t().map(|t| t.sel.clone());
    open(&mut ui, s.id, How::Here);
    assert_eq!(ui.app().tabs.len(), tabs);
    assert!(ui.app().t().map(|t| t.sel.clone()) == sel, "the current tab moved");
    assert!(ui.app().last_problem().contains("names no collection"), "{}", ui.app().last_problem());
}

#[test]
fn cmd_enter_in_the_editor_runs_the_query_after_a_click_in_the_library() {
    let Some(mut ui) = demo_ui(|a| {
        sql_query(a);
        a.demo_library("panel");
        a.lib_ui.panel.sel = None;
    }) else { return };
    {
        let a = ui.h.state_mut();
        let c = a.cur;
        a.tabs[c].sql.as_mut().unwrap().query = "SELECT 1".into();
    }
    ui.click("Top customers");
    assert!(ui.app().lib_ui.panel.focus);
    // The keyboard moves to the editor without a click, as Tab or a shortcut moves it.
    let id = App::sql_editor_id(ui.app().cur);
    ui.h.ctx.memory_mut(|m| m.request_focus(id));
    ui.settle();
    let before = ui.app().tabs.len();
    ui.key(CMD, egui::Key::Enter);
    assert_eq!(ui.app().tabs.len(), before, "the library script ran instead of the editor's query");
    assert!(ui.app().last_problem().contains("Offline demo"), "the editor's query did not run: {}", ui.app().last_problem());
}

#[test]
fn return_alt_return_and_cmd_enter_act_on_the_selected_script() {
    let Some(mut ui) = demo_ui(|a| {
        sql_query(a);
        a.demo_library("panel");
        a.lib_ui.panel.sel = None;
    }) else { return };
    let tabs = ui.app().tabs.len();
    ui.click("Top customers");
    ui.key(egui::Modifiers::NONE, egui::Key::Enter);
    assert_eq!(ui.app().tabs.len(), tabs, "Return opens in the empty tab");
    assert!(sql_of(ui.app()).query.starts_with("SELECT customer_id"));
    ui.click("Top customers");
    ui.key(egui::Modifiers::ALT, egui::Key::Enter);
    assert_eq!(ui.app().tabs.len(), tabs + 1, "Alt+Return opens a new tab");
    ui.click("Monthly revenue");
    ui.key(CMD, egui::Key::Enter);
    assert!(sql_of(ui.app()).query.contains("revenue per month"));
    assert!(ui.app().last_problem().contains("Offline demo"), "Cmd+Enter did not run it: {}", ui.app().last_problem());
}
