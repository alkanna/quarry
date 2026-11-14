//! UI tests of the ways to a library script from anywhere: the quick picker of each view, the palette, the chip of a bound
//! tab, Cmd+O and Cmd+Shift+L, and which handler takes Cmd+S. All on the offline demo, with no server.

use super::*;
use crate::app::library::open::How;
use crate::app::library::picker::PickKind;
use crate::library::tree::Node;
use crate::library::{Folder, Link, Script, ScriptRef, disk};

const CMD: egui::Modifiers = egui::Modifiers::COMMAND;

fn sql_with_scripts(a: &mut App) {
    a.load_sql_demo();
    a.sql_new_query(3, "shop".into());
    a.demo_scripts();
}

fn names(ui: &UiApp, kind: PickKind, of: impl Fn(&crate::app::library::picker::PickData) -> Vec<usize>) -> Vec<String> {
    let d = ui.app().pick_data(&ui.h.ctx, kind);
    of(&d).into_iter().map(|i| d.rows[i].name.clone()).collect()
}

fn script_id(a: &App, name: &str) -> u64 {
    a.library.scripts.iter().find(|s| s.name == name).map(|s| s.id).unwrap_or_else(|| panic!("no script {name}"))
}

#[test]
fn the_picker_lists_the_scripts_of_this_connection_and_kind_with_the_recent_ones_first() {
    let Some(mut ui) = demo_ui(sql_with_scripts) else { return };
    ui.click("Library 5");
    ui.show("Script Late shipments");
    // A filter of the Mongo connection and a Shell script of this one are not Query scripts.
    for other in ["Script Paid over 100", "Script Count orders by status", "Script Backfill note field"] {
        assert!(ui.h.query_all_by_label(other).next().is_none(), "{other} is listed");
    }
    assert_eq!(names(&ui, PickKind::Sql, |d| d.recent.clone()), ["Late shipments", "Revenue by tier", "Orphaned payments"]);
    // A script in no folder is listed under its own heading, not under FOLDERS.
    assert_eq!(names(&ui, PickKind::Sql, |d| d.loose.clone()), ["Scratch"]);
    ui.show("NOT IN A FOLDER");
    // The search looks at the names and the start of the text.
    ui.fill("Search library", "JOIN orders");
    ui.show("Script Orphaned payments");
    assert!(ui.h.query_all_by_label("Script Late shipments").next().is_none());
}

#[test]
fn the_documents_picker_lists_the_filters_of_its_collection_only() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_demo();
        a.demo_scripts();
    }) else { return };
    let ctx = ui.h.ctx.clone();
    let on = ui.app().t().map(|t| (t.sel.db.clone(), t.sel.coll.clone())).unwrap();
    assert_eq!(on, ("shop".to_string(), "orders".to_string()));
    assert_eq!(ui.app().pick_data(&ctx, PickKind::Query).count, 2);
    assert_eq!(ui.app().pick_data(&ctx, PickKind::Pipeline).count, 1);
    let c = ui.app().cur;
    ui.h.state_mut().tabs[c].sel.coll = "users".into();
    assert_eq!(ui.app().pick_data(&ctx, PickKind::Query).count, 0);
}

#[test]
fn a_loop_of_folders_does_not_hang_the_picker() {
    let Some(mut ui) = demo_ui(sql_with_scripts) else { return };
    {
        let a = ui.h.state_mut();
        let (x, y) = (a.library.next_id(), a.library.next_id());
        a.library.folders.extend([Folder { id: x, name: "X".into(), parent: Some(y) }, Folder { id: y, name: "Y".into(), parent: Some(x) }]);
        let id = a.library.next_id();
        a.library.scripts.push(Script { id, name: "In the loop".into(), kind: "sql".into(), body: "SELECT 3".into(), conn: 3, folder: Some(x), ..Default::default() });
    }
    ui.click("Library 6");
    ui.show("Search library");
    ui.fill("Search library", "loop");
    ui.show("Script In the loop");
}

#[test]
fn return_in_the_picker_opens_and_open_in_sidebar_shows_the_panel() {
    let Some(mut ui) = demo_ui(sql_with_scripts) else { return };
    ui.click("Library 5");
    ui.show("Script Late shipments");
    ui.key(egui::Modifiers::NONE, egui::Key::Enter);
    assert!(ui.query_text().starts_with("-- Paid orders that never shipped"));
    ui.click_contains("Library 5");
    ui.click("Open in sidebar");
    assert!(ui.app().prefs.side_mode() == SideMode::Library && !ui.app().prefs.sidebar_hidden);
}

#[test]
fn cmd_o_opens_the_picker_of_the_view() {
    let Some(mut ui) = demo_ui(sql_with_scripts) else { return };
    ui.key(CMD, egui::Key::O);
    ui.show("Search library");
    ui.show("Script Late shipments");
    assert!(!ui.app().lib_ui.open_picker);
}

#[test]
fn the_picker_lists_linked_files_and_keeps_them_in_recent() {
    let Some(mut ui) = demo_ui(sql_with_scripts) else { return };
    {
        let a = ui.h.state_mut();
        a.lib_ui.frozen = true;
        a.library.links.push(Link { id: 50, path: "/nowhere".into(), name: "team".into(), on: "all".into(), exts: vec!["sql".into()], ..Default::default() });
        let e = |rel: &str, head: &str| disk::Entry { rel: rel.into(), mtime: std::time::SystemTime::UNIX_EPOCH, len: 0, kind: "sql".into(), head: head.into(), target: Default::default() };
        a.lib_ui.scans.insert(50, disk::Scan { entries: vec![e("reports/q.sql", "select 9"), e("w.sql", "select 8")], ..Default::default() });
        a.library.recent.insert(0, ScriptRef::File { link: 50, rel: "w.sql".into() });
    }
    ui.click("Library 7");
    ui.show("LINKED");
    ui.show("Script q.sql");
    assert_eq!(names(&ui, PickKind::Sql, |d| d.recent.clone()).first().map(String::as_str), Some("w.sql"));
    assert_eq!(names(&ui, PickKind::Sql, |d| d.linked.clone()), ["q.sql", "w.sql"]);
}

#[test]
fn a_bound_tab_is_named_for_its_script_and_its_chip_acts_on_the_binding() {
    let Some(mut ui) = demo_ui(sql_with_scripts) else { return };
    let id = script_id(ui.app(), "Late shipments");
    ui.h.state_mut().open_script(&ScriptRef::Saved(id), How::Here);
    ui.settle();
    let cur = ui.app().cur;
    assert_eq!(ui.app().tab_name(cur), "Late shipments");
    assert!(!ui.app().tab_edited(cur));
    ui.h.state_mut().tabs[cur].sql.as_mut().unwrap().query.push_str("\nLIMIT 5");
    ui.settle();
    assert!(ui.app().tab_edited(cur));
    // Cmd+Shift+S saves a copy, leaving the bound script as it was.
    ui.key(CMD | egui::Modifiers::SHIFT, egui::Key::S);
    assert!(matches!(&ui.app().modal, Some(Modal::SaveScript(f)) if f.name == "Late shipments copy"));
    ui.click("Cancel");
    ui.click_contains("Bound to Late shipments");
    ui.click("Show in library");
    let a = ui.app();
    assert!(a.prefs.side_mode() == SideMode::Library && a.lib_ui.panel.sel == Some(Node::Script(id)));
    let folder = a.library.scripts.iter().find(|s| s.id == id).and_then(|s| s.folder).unwrap();
    assert!(a.lib_ui.panel.open.contains(&Node::Folder(folder)));
    ui.click_contains("Bound to Late shipments");
    ui.click("Unbind");
    assert!(ui.app().t().unwrap().script.is_none());
    assert_ne!(ui.app().tab_name(cur), "Late shipments");
}

#[test]
fn the_palette_opens_and_runs_scripts_and_has_the_library_commands() {
    let Some(mut ui) = demo_ui(sql_with_scripts) else { return };
    ui.h.state_mut().status.clear();
    ui.h.state_mut().open_palette_with("Orphaned".into());
    ui.settle();
    ui.key(egui::Modifiers::NONE, egui::Key::Enter);
    assert!(ui.query_text().starts_with("SELECT p.*"), "{}", ui.query_text());
    assert!(!ui.said("Offline demo"));
    ui.h.state_mut().open_palette_with("Orphaned".into());
    ui.settle();
    ui.key(CMD, egui::Key::Enter);
    assert!(ui.said("Offline demo"), "Cmd+Enter did not run it: {}", ui.app().last_problem());
    // The commands: a new group opens its dialog.
    ui.h.state_mut().open_palette_with(">New group".into());
    ui.settle();
    ui.key(egui::Modifiers::NONE, egui::Key::Enter);
    assert!(matches!(ui.app().modal, Some(Modal::GroupEdit(_))));
    assert_eq!(ui.app().groups.groups.len(), 1);
}

#[test]
fn cmd_shift_l_moves_the_keys_to_the_library_tree() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_sql_demo();
        a.sql_new_query(3, "shop".into());
        a.demo_library("panel");
        a.lib_ui.panel.sel = None;
        a.prefs.set_side_mode(SideMode::Explorer);
    }) else { return };
    ui.key(CMD | egui::Modifiers::SHIFT, egui::Key::L);
    ui.settle();
    let a = ui.app();
    assert!(a.prefs.side_mode() == SideMode::Library && a.lib_ui.panel.focus && a.lib_ui.panel.sel.is_some());
    // The arrows now walk the tree.
    let before = a.lib_ui.panel.sel.clone();
    ui.key(egui::Modifiers::NONE, egui::Key::ArrowDown);
    assert_ne!(ui.app().lib_ui.panel.sel, before);
}

#[test]
fn the_search_counts_its_matches_and_the_empty_state_offers_to_save_the_query() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_sql_demo();
        a.sql_new_query(3, "shop".into());
        a.demo_library("panel");
        a.lib_ui.panel.sel = None;
    }) else { return };
    // Found only in the text of Monthly revenue.
    ui.fill("Search scripts", "date_format");
    ui.show("1 match");
    ui.show("Monthly revenue");

    let Some(mut ui) = demo_ui(|a| {
        a.load_sql_demo();
        a.sql_new_query(3, "shop".into());
        a.demo_library("empty");
        let c = a.cur;
        a.tabs[c].sql.as_mut().unwrap().query = "SELECT 1".into();
    }) else { return };
    ui.click("Save current query");
    assert!(matches!(ui.app().modal, Some(Modal::SaveScript(_))));
}

#[test]
fn cmd_s_leaves_a_dialog_alone_and_a_file_bound_in_another_view_takes_the_dialog() {
    let Some(mut ui) = demo_ui(sql_with_scripts) else { return };
    let c = ui.app().cur;
    ui.h.state_mut().tabs[c].sql.as_mut().unwrap().query = "SELECT 1".into();
    ui.h.state_mut().modal = Some(Modal::Settings);
    ui.settle();
    ui.key(CMD, egui::Key::S);
    assert!(matches!(ui.app().modal, Some(Modal::Settings)), "Cmd+S reached the page under a dialog");
    ui.h.state_mut().modal = None;
    // Bound to a file in the Shell view, but the Query view is on screen: Cmd+S saves the Query text, not the file.
    {
        let a = ui.h.state_mut();
        a.library.links.push(Link { id: 60, path: "/nowhere".into(), name: "team".into(), on: "all".into(), exts: vec!["sql".into(), "js".into()], ..Default::default() });
        a.tabs[c].script = Some(Bound { r: ScriptRef::File { link: 60, rel: "s.js".into() }, view: View::Shell, base: String::new(), disk: None, conflict: None });
    }
    ui.settle();
    ui.key(CMD, egui::Key::S);
    assert!(matches!(&ui.app().modal, Some(Modal::SaveScript(f)) if f.body == "SELECT 1"));
    assert!(ui.app().lib_ui.jobs.is_empty(), "the file was written");
}
