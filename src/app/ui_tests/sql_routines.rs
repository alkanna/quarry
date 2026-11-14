//! The functions and procedures of a SQL connection, offline on the demo data: the ROUTINES sidebar group, and the routine window
//! with its view, edit, drop and run panes. The demo holds the list and the definitions, so no server is needed; a Save, a Drop and
//! a Run reach the same functions as live and stop where a server would answer.

use super::sql_structure::replace;
use super::*;

fn mysql_open(a: &mut App, view: &str) {
    a.load_sql_demo();
    a.demo_routines(3, "shop", "shop", false, view);
}

fn pg_open(a: &mut App, view: &str) {
    a.load_pg_demo();
    a.demo_routines(3, "analytics", "public", true, view);
}

/// Every accessible name on screen, for a failure message that says what was there instead.
fn names(ui: &UiApp) -> String {
    use egui_kittest::kittest::NodeT;
    ui.h.root().children_recursive().filter_map(|n| n.accesskit_node().label()).collect::<Vec<_>>().join(" | ")
}

/// Steps frames until no widget has this name, or fails with what is on screen.
fn gone(ui: &mut UiApp, label: &str) {
    for _ in 0..20 {
        if ui.h.query_all_by_label(label).next().is_none() {
            return;
        }
        ui.h.step();
    }
    panic!("\"{label}\" is still on screen: {}", names(ui));
}

#[test]
fn the_sidebar_lists_routines_and_the_caption_folds_them() {
    let Some(mut ui) = demo_ui(|a| mysql_open(a, "")) else { return };
    // The window is a modal, so a click on the sidebar would only close it.
    ui.click("Close");
    ui.show("order_total");
    ui.show("archive_orders");
    ui.click("Routines");
    gone(&mut ui, "order_total");
    ui.click("Routines");
    ui.show("order_total");
}

#[test]
fn postgresql_overloads_are_listed_by_their_arguments() {
    let Some(mut ui) = demo_ui(|a| pg_open(a, "")) else { return };
    // The arguments are part of a PostgreSQL function's identity, so two functions of one name are two rows.
    ui.show("order_total(order_id integer)");
    ui.show("order_total(order_id integer, with_tax boolean)");
    ui.show("archive_orders(before date)");
}

#[test]
fn the_window_shows_the_definition_and_close_ends_it() {
    let Some(mut ui) = demo_ui(|a| mysql_open(a, "")) else { return };
    ui.show("Routine definition");
    for b in ["Edit", "Run\u{2026}", "Drop\u{2026}"] {
        ui.show(b);
    }
    assert!(!ui.disabled("Edit"), "a routine of a user database cannot be edited");
    ui.click("Close");
    assert!(!ui.app().routines.window_open(), "Close left the window open");
}

#[test]
fn an_edit_previews_what_it_sends_and_refuses_what_cannot_save() {
    let Some(mut ui) = demo_ui(|a| mysql_open(a, "")) else { return };
    ui.click("Edit");
    ui.show("Save routine");
    assert!(ui.disabled("Save routine"), "Save is on before anything changed");
    assert!(!ui.shows("SQL TO RUN"), "a preview is shown for an unchanged definition");

    replace(&mut ui, "Routine definition", "CREATE FUNCTION add_tax(amount DECIMAL(10,2), rate DECIMAL(4,2)) RETURNS decimal(10,2) DETERMINISTIC RETURN amount * rate");
    ui.show("SQL TO RUN");
    assert!(ui.shows("MySQL cannot replace a routine in place"), "the preview does not say the routine is dropped and created again");
    assert!(!ui.disabled("Save routine"), "a changed, valid definition cannot be saved");

    // Another name would create another routine, so the preview refuses it and says where New is.
    replace(&mut ui, "Routine definition", "CREATE FUNCTION other_name(amount DECIMAL(10,2)) RETURNS decimal(10,2) DETERMINISTIC RETURN amount");
    ui.show_contains("The name is other_name, not add_tax");
    assert!(ui.disabled("Save routine"), "a rename saved over the routine");
    replace(&mut ui, "Routine definition", "SELECT 1");
    ui.show_contains("one CREATE FUNCTION or CREATE PROCEDURE statement");
    assert!(ui.disabled("Save routine"));

    replace(&mut ui, "Routine definition", "CREATE FUNCTION add_tax(amount DECIMAL(10,2), rate DECIMAL(4,2)) RETURNS decimal(10,2) DETERMINISTIC RETURN amount * rate");
    ui.h.state_mut().status.clear();
    ui.click("Save routine");
    assert!(ui.said("Offline demo"), "Save did not reach the demo's stop: {}", ui.app().last_problem());
    ui.click("Revert");
    ui.show("Edit");
    assert!(ui.app().routines.window_open());
}

#[test]
fn dropping_needs_the_exact_name_typed() {
    let Some(mut ui) = demo_ui(|a| mysql_open(a, "drop")) else { return };
    ui.show("Confirmation");
    assert!(ui.disabled("Drop"), "Drop is on before the name is typed");
    replace(&mut ui, "Confirmation", "ADD_TAX");
    assert!(ui.disabled("Drop"), "the name is case sensitive");
    replace(&mut ui, "Confirmation", "add_tax");
    assert!(!ui.disabled("Drop"), "Drop stays off with the exact name typed");
    ui.h.state_mut().status.clear();
    ui.click("Drop");
    assert!(ui.said("Offline demo"), "Drop did not reach the demo's stop: {}", ui.app().last_problem());
}

#[test]
fn cancel_leaves_the_drop_pane_for_the_definition() {
    let Some(mut ui) = demo_ui(|a| mysql_open(a, "drop")) else { return };
    ui.click("Cancel");
    ui.show("Routine definition");
    ui.show("Edit");
}

#[test]
fn the_row_menu_opens_the_drop_pane_of_that_routine() {
    let Some(mut ui) = demo_ui(|a| mysql_open(a, "")) else { return };
    // The window is a modal, so a click on the sidebar would only close it.
    ui.click("Close");
    ui.show("order_total");
    let at = ui.h.get_by_label("order_total").rect().center();
    ui.press_at(at, egui::PointerButton::Secondary, 1);
    ui.settle();
    if ui.h.query_all_by_label_contains("Drop function").next().is_none() {
        panic!("the row menu did not open: {}", names(&ui));
    }
    ui.click_contains("Drop function");
    ui.show("Confirmation");
    assert!(ui.disabled("Drop"));
    replace(&mut ui, "Confirmation", "add_tax");
    assert!(ui.disabled("Drop"), "the name of another routine armed the drop");
}

#[test]
fn run_fills_a_call_into_a_new_query_tab() {
    let Some(mut ui) = demo_ui(|a| mysql_open(a, "run")) else { return };
    let before = ui.app().tabs.len();
    ui.show("Argument before_day");
    replace(&mut ui, "Argument before_day", "2024-01-01");
    ui.click_last("Run");
    assert!(!ui.app().routines.window_open(), "the window stayed open after Run");
    assert_eq!(ui.app().tabs.len(), before + 1, "Run did not open a Query tab");
    let q = ui.query_text();
    assert!(q.contains("CALL") && q.contains("archive_orders") && q.contains("2024-01-01"), "the call is not in the new tab: {q}");
}

#[test]
fn back_leaves_the_run_pane_for_the_definition() {
    let Some(mut ui) = demo_ui(|a| mysql_open(a, "")) else { return };
    ui.click("Run\u{2026}");
    ui.show("Argument amount");
    ui.click("Back");
    ui.show("Routine definition");
}

#[test]
fn a_locked_postgresql_connection_runs_a_read_only_function_and_asks_for_a_procedure() {
    let Some(mut ui) = demo_ui(|a| pg_open(a, "")) else { return };
    ui.lock();
    let before = ui.app().tabs.len();
    ui.click("Run\u{2026}");
    ui.click_last("Run");
    assert!(!matches!(ui.app().modal, Some(Modal::Unlock(..))), "an IMMUTABLE function asked to unlock");
    assert_eq!(ui.app().tabs.len(), before + 1, "the function did not run in a new Query tab");
    assert!(ui.query_text().starts_with("SELECT * FROM"), "{}", ui.query_text());

    let Some(mut ui) = demo_ui(|a| pg_open(a, "run")) else { return };
    ui.lock();
    ui.show_contains("Writes are locked.");
    ui.click_last("Run");
    assert!(matches!(ui.app().modal, Some(Modal::Unlock(..))), "a procedure ran on a locked connection");
}
