//! UI tests of the Library panel on the demo library, with no server: the scope and the search, the preview card, the keys,
//! rename, duplicate, move, delete with Undo, both drags and the empty state.
//!
//! Opening and running a script is B1's `open_script`, so these tests stop at the call: the buttons exist and select what
//! they should, and B1's own tests check the tab that comes out.

use super::*;
use crate::library::tree::Node;

/// The "panel" demo state plus the Mongo connection that owns the filter, the pipeline and the shell script, and nothing
/// selected so no preview repeats a row's name.
fn load(a: &mut App) {
    a.conns.push(Conn::new(ConnMeta { id: 1, name: "Local sample".into(), ..Default::default() }, Status::Ready));
    a.demo_library("panel");
    a.lib_ui.panel.sel = None;
}

fn load_empty(a: &mut App) {
    a.conns.push(Conn::new(ConnMeta { id: 1, name: "Local sample".into(), ..Default::default() }, Status::Ready));
    a.demo_library("empty");
}

impl UiApp {
    fn shown(&self, label: &str) -> bool {
        self.h.query_all_by_label(label).next().is_some()
    }

    /// Rests the pointer on a row, which is what makes its Run and dots buttons appear. They are clicked by name right after,
    /// while the pointer is still there.
    fn hover_row(&mut self, name: &str) {
        self.show(name);
        let at = self.h.get_by_label(name).rect().center();
        self.h.hover_at(at);
        self.h.run_steps(2);
    }

    fn row_menu(&mut self, name: &str, item: &str) {
        self.hover_row(name);
        self.click(&format!("More for {name}"));
        self.click(item);
    }

    fn script(&self, name: &str) -> crate::library::Script {
        self.app().library.scripts.iter().find(|s| s.name == name).cloned().unwrap_or_else(|| panic!("no script named {name}"))
    }
}

#[test]
fn the_scope_shows_one_connection_or_all_of_them() {
    let Some(mut ui) = demo_ui(load) else { return };
    assert!(ui.shown("Monthly revenue") && ui.shown("Slow orders lookup"));
    assert!(!ui.shown("Paid orders") && !ui.shown("Legacy export"));
    ui.click("All connections");
    assert!(ui.app().prefs.lib_scope_all);
    assert!(ui.shown("Paid orders") && ui.shown("Legacy export"));
    ui.click("shop-prod");
    assert!(!ui.app().prefs.lib_scope_all && !ui.shown("Paid orders"));
}

#[test]
fn the_search_opens_the_way_to_a_match_and_hides_the_rest() {
    let Some(mut ui) = demo_ui(load) else { return };
    // Folders start closed apart from the demo's open ones, so Maintenance hides its script until a search needs it.
    assert!(!ui.shown("Stale carts"));
    ui.fill("Search scripts", "stale");
    assert!(ui.shown("Stale carts") && ui.shown("Maintenance"));
    assert!(!ui.shown("Monthly revenue") && !ui.shown("Reports"));
    // The folder was never written open.
    assert!(!ui.app().lib_ui.panel.open.contains(&Node::Folder(3)));
    ui.fill("Search scripts", "zzzz");
    assert!(ui.shown("Search scripts"));
}

#[test]
fn a_clicked_script_gets_a_preview_with_open_run_and_new_tab() {
    let Some(mut ui) = demo_ui(load) else { return };
    ui.click("Top customers");
    assert_eq!(ui.app().lib_ui.panel.sel, Some(Node::Script(11)));
    // The name is in the row and in the card.
    assert_eq!(ui.h.query_all_by_label("Top customers").count(), 2);
    for b in ["Open", "Run", "New tab"] {
        assert!(ui.shown(b), "{b}");
    }
    // A filter has nothing to run.
    ui.click("All connections");
    ui.click("Paid orders");
    assert!(ui.shown("Open") && ui.shown("New tab") && !ui.shown("Run"));
    ui.click("New tab");
    ui.click("Open");
}

#[test]
fn the_keys_walk_the_tree_and_fold_folders() {
    let Some(mut ui) = demo_ui(load) else { return };
    let none = egui::Modifiers::NONE;
    ui.click("Reports");
    // The click on a folder also folded it.
    assert!(!ui.app().lib_ui.panel.open.contains(&Node::Folder(1)));
    ui.key(none, egui::Key::Enter);
    assert!(ui.app().lib_ui.panel.open.contains(&Node::Folder(1)));
    ui.key(none, egui::Key::ArrowDown);
    assert_eq!(ui.app().lib_ui.panel.sel, Some(Node::Folder(2)));
    ui.key(none, egui::Key::ArrowLeft);
    assert!(!ui.app().lib_ui.panel.open.contains(&Node::Folder(2)));
    ui.key(none, egui::Key::ArrowLeft);
    assert_eq!(ui.app().lib_ui.panel.sel, Some(Node::Folder(1)));
}

#[test]
fn f2_renames_in_place_and_enter_keeps_the_name() {
    let Some(mut ui) = demo_ui(load) else { return };
    ui.click("Top customers");
    ui.key(egui::Modifiers::NONE, egui::Key::F2);
    assert!(ui.app().lib_ui.panel.rename.is_some());
    ui.type_text("Best customers");
    ui.h.key_press(egui::Key::Enter);
    ui.settle();
    assert!(ui.app().lib_ui.panel.rename.is_none());
    assert_eq!(ui.script("Best customers").id, 11);
    assert!(ui.app().library.scripts.iter().all(|s| s.name != "Top customers"));
}

#[test]
fn escape_leaves_the_name_alone() {
    let Some(mut ui) = demo_ui(load) else { return };
    ui.click("Top customers");
    ui.key(egui::Modifiers::NONE, egui::Key::F2);
    ui.type_text("Nope");
    ui.h.key_press(egui::Key::Escape);
    ui.settle();
    assert_eq!(ui.script("Top customers").id, 11);
}

#[test]
fn a_script_is_duplicated_below_itself() {
    let Some(mut ui) = demo_ui(load) else { return };
    let before = ui.app().library.scripts.len();
    ui.row_menu("Top customers", "Duplicate");
    assert_eq!(ui.app().library.scripts.len(), before + 1);
    let copy = ui.script("Top customers copy");
    assert_eq!(copy.folder, Some(1));
    assert_eq!(copy.body, ui.script("Top customers").body);
}

#[test]
fn a_script_moves_to_a_folder_from_its_menu() {
    let Some(mut ui) = demo_ui(load) else { return };
    assert_eq!(ui.script("Slow orders lookup").folder, None);
    ui.hover_row("Slow orders lookup");
    ui.click("More for Slow orders lookup");
    ui.click("Move to folder");
    ui.click_last("Maintenance");
    assert_eq!(ui.script("Slow orders lookup").folder, Some(3));
    // The folder it went into is open, so the move shows.
    assert!(ui.app().lib_ui.panel.open.contains(&Node::Folder(3)));
    assert!(ui.shown("Stale carts"));
}

#[test]
fn deleting_a_script_can_be_undone() {
    let Some(mut ui) = demo_ui(load) else { return };
    ui.row_menu("Slow orders lookup", "Delete…");
    assert!(!ui.app().library.scripts.iter().any(|s| s.name == "Slow orders lookup"));
    assert!(!ui.shown("Slow orders lookup"));
    ui.click_last("Undo");
    assert_eq!(ui.script("Slow orders lookup").id, 14);
    assert!(ui.shown("Slow orders lookup"));
}

#[test]
fn dragging_a_script_onto_a_folder_moves_it_inside_and_opens_a_closed_folder() {
    let Some(mut ui) = demo_ui(load) else { return };
    // Held long enough over the closed folder for it to open: 0.6 s is three frames of 250 ms.
    ui.drag("Slow orders lookup", "Maintenance", 0.5, 4);
    assert_eq!(ui.script("Slow orders lookup").folder, Some(3));
    assert!(ui.app().lib_ui.panel.open.contains(&Node::Folder(3)));
}

#[test]
fn dragging_between_rows_puts_the_script_at_the_drop_line() {
    let Some(mut ui) = demo_ui(load) else { return };
    // The top edge of "Monthly revenue" is the line above it, in its folder.
    ui.drag("Slow orders lookup", "Monthly revenue", 0.15, 1);
    let order: Vec<String> = ui.app().library.scripts.iter().map(|s| s.name.clone()).collect();
    let (a, b) = (order.iter().position(|n| n == "Slow orders lookup").unwrap(), order.iter().position(|n| n == "Monthly revenue").unwrap());
    assert_eq!(a + 1, b);
    assert_eq!(ui.script("Slow orders lookup").folder, Some(1));
}

#[test]
fn a_folder_cannot_be_dropped_into_its_own_subfolder() {
    let Some(mut ui) = demo_ui(load) else { return };
    let before = ui.app().library.clone();
    ui.drag("Reports", "Weekly", 0.5, 1);
    assert_eq!(ui.app().library, before);
    assert!(ui.app().last_problem().contains("cannot go into itself"), "{}", ui.app().last_problem());
}

#[test]
fn the_empty_state_offers_a_link_a_folder_and_the_other_connections() {
    let Some(mut ui) = demo_ui(load_empty) else { return };
    assert!(ui.shown("No saved scripts on shop-prod") || ui.h.query_all_by_label_contains("No saved scripts").next().is_some());
    assert!(ui.shown("Link a folder…"));
    assert!(ui.h.query_all_by_label_contains("3 scripts on other connections").next().is_some());
    ui.click("Link a folder…");
    assert!(matches!(ui.app().modal, Some(Modal::LinkFolder(_))));
    ui.click("Cancel");
    ui.click("Show all connections");
    assert!(ui.app().prefs.lib_scope_all);
    assert!(ui.shown("Paid orders"));
}

#[test]
fn new_folder_adds_a_folder_and_starts_its_rename() {
    let Some(mut ui) = demo_ui(load) else { return };
    let before = ui.app().library.folders.len();
    ui.click("New folder");
    assert_eq!(ui.app().library.folders.len(), before + 1);
    assert!(ui.app().lib_ui.panel.rename.is_some());
    ui.type_text("Archive");
    ui.h.key_press(egui::Key::Enter);
    ui.settle();
    assert!(ui.app().library.folders.iter().any(|f| f.name == "Archive"));
}
