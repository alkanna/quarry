//! The New view and Edit view dialog of SQL connections, and the Save as view buttons of the Content bar and the Query view,
//! offline on the demo data. The definition an edit reads from the server is handed to the dialog the way the worker's answer is.

use super::sql_structure::replace;
use super::sql_tableops::{disabled, right_click_row, shows};
use super::*;
use crate::app::sql::viewdef::ViewForm;
use db::sql::view::ViewDef;

fn view_form(ui: &UiApp) -> &ViewForm {
    match &ui.app().modal {
        Some(Modal::SqlView(f)) => f,
        _ => panic!("the view dialog is not open"),
    }
}

/// An edit of `name` the way `sql_edit_view` opens it, waiting for its definition.
fn open_edit(app: &mut App, database: &str, schema: &str, name: &str) {
    let conn = app.active.expect("a connection");
    app.modal = Some(Modal::SqlView(Box::new(ViewForm { conn, database: database.into(), schema: schema.into(), name: name.into(), body: String::new(), edit: true, loading: true, error: None, def: ViewDef::default(), algorithm: "UNDEFINED".into(), security: "DEFINER".into(), check: "NONE".into(), keep_definer: true, focused: false, busy: false, ask_drop: false })));
}

#[test]
fn save_as_view_on_the_content_bar_opens_a_new_view_named_after_the_table() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    ui.click("Save as view…");
    let f = view_form(&ui);
    assert!(!f.edit && f.name == "orders_view", "the dialog is not a new view named after the table: {}", f.name);
    assert!(f.body.starts_with("SELECT") && f.body.contains("orders"), "the SELECT is not the grid's: {}", f.body);
    assert!(shows(&ui, "VIEW `shop`.`orders_view` AS"), "the preview does not hold the statement");
    assert!(!disabled(&ui, "Create view"), "a named view with a SELECT did not arm Create view");
    // The name is a field of its own, and an empty one leaves nothing to create.
    ui.h.get_by_label("View name").focus();
    ui.h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    ui.h.key_press(egui::Key::Backspace);
    ui.settle();
    assert!(disabled(&ui, "Create view"), "an empty name armed Create view");
    replace(&mut ui, "View name", "recent_orders");
    assert!(shows(&ui, "VIEW `shop`.`recent_orders` AS"), "the preview did not follow the name");
    // Two statements in one SELECT are refused before anything runs.
    replace(&mut ui, "View SELECT", "SELECT 1; DROP TABLE orders");
    assert!(disabled(&ui, "Create view"), "a second statement armed Create view");
    ui.click("Cancel");
    assert!(ui.app().modal.is_none(), "Cancel left the dialog open");
}

#[test]
fn save_as_view_on_a_locked_connection_asks_to_unlock() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    for c in &mut ui.h.state_mut().conns {
        c.writable = false;
    }
    ui.click("Save as view…");
    assert!(matches!(ui.app().modal, Some(Modal::Unlock(..))), "a locked connection did not ask to unlock");
}

/// The Query view's demo with `script` in the editor and the caret `at` characters in.
fn query_with(script: &str, at: usize) -> Option<UiApp> {
    let mut ui = demo_ui(|a| {
        a.load_sql_demo();
        a.sql_query_demo();
    })?;
    let app = ui.h.state_mut();
    let cur = app.cur;
    if let Some(s) = app.tabs[cur].sql.as_mut() {
        s.query = script.into();
        s.caret = at;
    }
    ui.settle();
    Some(ui)
}

#[test]
fn save_as_view_on_the_query_view_takes_the_statement_under_the_caret() {
    let Some(mut ui) = query_with("SELECT 1;\nSELECT id, total FROM orders WHERE total > 50;\nSELECT 2;", 20) else { return };
    ui.click("Save as view…");
    let f = view_form(&ui);
    assert_eq!(f.body, "SELECT id, total FROM orders WHERE total > 50", "the dialog did not take the statement under the caret without its semicolon");
    assert!(f.name.is_empty(), "a view made from a query has no name yet");
    assert!(disabled(&ui, "Create view"), "a view without a name armed Create view");
    replace(&mut ui, "View name", "big_orders");
    assert!(shows(&ui, "VIEW `shop`.`big_orders` AS"));
    assert!(!disabled(&ui, "Create view"));
}

#[test]
fn save_as_view_without_a_statement_says_so() {
    let Some(mut ui) = query_with("", 0) else { return };
    ui.click("Save as view…");
    assert!(ui.app().modal.is_none(), "an empty editor opened the dialog");
    assert!(ui.app().last_problem().contains("Put the caret in the SELECT"), "no notice says what is missing: {}", ui.app().last_problem());
}

#[test]
fn new_view_from_the_database_menu_starts_empty_on_mysql_and_postgres() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    right_click_row(&mut ui, "shop");
    ui.click("New view…");
    let f = view_form(&ui);
    assert!(!f.edit && f.name.is_empty() && f.body.is_empty(), "the new view dialog does not start empty");
    assert!(disabled(&ui, "Create view"), "an empty form armed Create view");
    // An empty form is not a mistake yet, so no refusal is shown.
    assert!(!shows(&ui, "needs a name"), "the empty form already complains");
    replace(&mut ui, "View name", "paid_orders");
    replace(&mut ui, "View SELECT", "SELECT id FROM orders WHERE status = 'paid'");
    assert!(shows(&ui, "CREATE ALGORITHM = UNDEFINED SQL SECURITY DEFINER VIEW `shop`.`paid_orders` AS"), "the MySQL preview is not the statement");
    assert!(!disabled(&ui, "Create view"));
    let Some(mut ui) = demo_ui(App::load_pg_demo) else { return };
    right_click_row(&mut ui, "analytics");
    ui.click("New view…");
    assert_eq!(view_form(&ui).schema, "public", "the view belongs to the schema the sidebar shows");
    replace(&mut ui, "View name", "paid_orders");
    replace(&mut ui, "View SELECT", "SELECT id FROM orders");
    assert!(shows(&ui, r#"CREATE VIEW "public"."paid_orders" AS"#), "the PostgreSQL preview is not the statement");
    // PostgreSQL has none of MySQL's algorithm, security and check options.
    assert!(ui.h.query_by_label("Algorithm").is_none(), "PostgreSQL shows MySQL's algorithm");
}

#[test]
fn an_edit_waits_for_the_definition_and_then_keeps_what_the_server_holds() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    open_edit(ui.h.state_mut(), "", "shop", "order_totals");
    ui.settle();
    assert!(shows(&ui, "Reading the definition"), "the dialog does not say the definition is on its way");
    assert!(disabled(&ui, "Save view"), "Save view was armed before the definition arrived");
    let def = ViewDef { schema: "shop".into(), name: "order_totals".into(), body: "SELECT order_id, SUM(total) AS total FROM order_items GROUP BY order_id".into(), algorithm: "MERGE".into(), security: "INVOKER".into(), definer: Some(("app".into(), "%".into())), check: String::new(), ..Default::default() };
    let app = ui.h.state_mut();
    let conn = app.active.expect("a connection");
    app.sql_view_def_done(conn, String::new(), "shop".into(), "order_totals".into(), Ok(def));
    ui.settle();
    ui.show("Save view");
    assert!(!disabled(&ui, "Save view"), "the read definition did not arm Save view");
    // The name of an existing view is a label, not a field.
    assert!(ui.h.query_by_label("View name").is_none(), "an existing view can be renamed here");
    assert!(shows(&ui, "CREATE OR REPLACE ALGORITHM = MERGE DEFINER = `app`@`%` SQL SECURITY INVOKER VIEW `shop`.`order_totals` AS"), "the preview drops what the dialog has no field for");
    replace(&mut ui, "View SELECT", "SELECT order_id FROM order_items");
    assert!(shows(&ui, "AS\nSELECT order_id FROM order_items"), "the preview did not follow the edited SELECT");
}

#[test]
fn a_definition_that_cannot_be_read_says_why_and_saves_nothing() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    open_edit(ui.h.state_mut(), "", "shop", "order_totals");
    let app = ui.h.state_mut();
    let conn = app.active.expect("a connection");
    app.sql_view_def_done(conn, String::new(), "shop".into(), "order_totals".into(), Err("The view is gone".into()));
    ui.settle();
    assert!(shows(&ui, "The view is gone"), "the reason is not shown");
    assert!(shows(&ui, "Could not read the view"));
    assert!(disabled(&ui, "Save view"), "Save view was armed without a definition");
}

#[test]
fn a_materialized_postgres_view_is_shown_and_cannot_be_saved() {
    let Some(mut ui) = demo_ui(App::load_pg_demo) else { return };
    open_edit(ui.h.state_mut(), "analytics", "public", "mv_cohorts");
    let def = ViewDef { schema: "public".into(), name: "mv_cohorts".into(), body: "SELECT 1".into(), materialized: true, ..Default::default() };
    let app = ui.h.state_mut();
    let conn = app.active.expect("a connection");
    app.sql_view_def_done(conn, "analytics".into(), "public".into(), "mv_cohorts".into(), Ok(def));
    ui.settle();
    assert!(shows(&ui, "PostgreSQL has no CREATE OR REPLACE for a materialized view"), "the dialog does not explain why it cannot edit");
    assert!(disabled(&ui, "Save view"), "a materialized view can be saved");
    ui.click("Close");
    assert!(ui.app().modal.is_none());
}
