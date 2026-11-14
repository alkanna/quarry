//! Fixes to the chrome around SQL tabs, offline on the demo: File > Insert Document, Cmd+F, the palette's digit hints, the Cloud SQL
//! entries on MongoDB connections, and the maintenance buttons of the Info page.

use super::*;
use crate::app::palette::keys;

fn hint(a: &App, name: &str) -> Option<String> {
    a.command_hints().into_iter().find(|(n, _)| n == name).map(|(_, k)| k)
}

// File > Insert Document is a macOS menu item and cannot be driven here, so this calls the function the menu calls.
#[test]
fn insert_document_on_a_sql_tab_opens_the_row_form_on_the_content_grid() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    let app = ui.h.state_mut();
    app.tabs[app.cur].view = View::Sql;
    app.sql_insert_row_menu();
    ui.settle();
    assert!(ui.app().t().map(|t| t.view) == Some(View::Documents), "the tab did not go to Content first");
    assert!(matches!(ui.app().modal, Some(Modal::SqlRow(_))), "no insert row form opened");
}

#[test]
fn insert_document_with_no_table_open_says_so_and_opens_nothing() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    let app = ui.h.state_mut();
    let cur = app.cur;
    app.tabs[cur].sel.coll.clear();
    app.sql_insert_row_menu();
    assert!(ui.app().modal.is_none(), "a form opened with no table");
    assert!(ui.app().last_problem().contains("Open a table first"), "{}", ui.app().last_problem());
}

#[test]
fn cmd_f_switches_the_sql_bar_to_sql_mode_and_focuses_its_where_field() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    let app = ui.h.state_mut();
    let cur = app.cur;
    app.tabs[cur].sql.as_mut().expect("a SQL tab").bar.c.sql = false;
    ui.settle();
    ui.key(egui::Modifiers::COMMAND, egui::Key::F);
    ui.wait("the SQL mode", |a| a.t().and_then(|t| t.sql.as_ref()).is_some_and(|s| s.bar.c.sql));
    ui.settle();
    let id = crate::app::query::query_id("SQL-WHERE");
    assert!(ui.h.ctx.memory(|m| m.has_focus(id)), "Cmd+F left the SQL WHERE field without focus");
}

#[test]
fn cmd_f_on_mongo_still_focuses_the_query_bar() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    ui.key(egui::Modifiers::COMMAND, egui::Key::F);
    ui.settle();
    assert!(ui.h.ctx.memory(|m| m.has_focus(crate::app::query::query_id("QUERY"))), "Cmd+F did not focus the Mongo query bar");
}

#[test]
fn the_palette_hints_the_view_digits_of_each_engine() {
    let Some(ui) = demo_ui(App::load_sql_demo) else { return };
    // The digit is the view's place in its engine's list, which is why the SQL list differs from MongoDB's.
    assert_eq!(hint(ui.app(), "Show Structure"), Some(keys(false, true, "2")));
    assert_eq!(hint(ui.app(), "Show Info"), Some(keys(false, true, "5")));
    assert_eq!(hint(ui.app(), "Show Query"), Some(keys(false, true, "6")));
    assert_eq!(hint(ui.app(), "Show Shell"), Some(keys(false, true, "7")));
    assert_eq!(hint(ui.app(), "Show Content"), None, "the open view is not offered");

    let Some(mongo) = demo_ui(App::load_demo) else { return };
    assert_eq!(hint(mongo.app(), "Show Aggregate"), Some(keys(false, true, "2")));
    assert_eq!(hint(mongo.app(), "Show Watch"), Some(keys(false, true, "8")));
}

#[test]
fn add_from_cloud_sql_is_offered_on_sql_connections_and_hidden_on_mongodb() {
    let Some(mut sql) = demo_ui(App::load_sql_demo) else { return };
    assert!(sql.app().command_hints().iter().any(|(n, _)| n.starts_with("Add from Cloud SQL")), "the palette lost the Cloud SQL command on a SQL connection");
    sql.click("Connections");
    sql.show("Add from Cloud SQL\u{2026}");

    let Some(mut mongo) = demo_ui(App::load_demo) else { return };
    assert!(!mongo.app().command_hints().iter().any(|(n, _)| n.starts_with("Add from Cloud SQL")), "the palette offers Cloud SQL on a MongoDB connection");
    mongo.click("Connections");
    mongo.show("+  New connection\u{2026}");
    assert!(mongo.h.query_all_by_label("Add from Cloud SQL\u{2026}").next().is_none(), "the connection menu offers Cloud SQL on a MongoDB connection");
}

fn mysql_info(a: &mut App) {
    a.load_sql_demo();
    a.sql_structure_demo("info");
}

fn pg_info(a: &mut App) {
    a.load_pg_demo();
    a.pg_structure_demo("info");
}

#[test]
fn mysql_info_buttons_run_light_statements_in_the_query_view() {
    let Some(mut ui) = demo_ui(mysql_info) else { return };
    for b in ["Analyze", "Optimize", "Check", "Truncate..."] {
        ui.show(b);
    }
    ui.click("Check");
    assert!(ui.app().t().map(|t| t.view) == Some(View::Sql), "Check did not go to the Query view");
    let q = ui.query_text();
    assert!(q.starts_with("CHECK TABLE") && q.contains("`orders`"), "{q}");
}

#[test]
fn a_heavy_maintenance_statement_asks_first_and_cancel_runs_nothing() {
    let Some(mut ui) = demo_ui(mysql_info) else { return };
    let before = ui.query_text();
    ui.click("Optimize");
    assert!(matches!(&ui.app().modal, Some(Modal::SqlMaintain { stmt, .. }) if stmt.starts_with("OPTIMIZE TABLE")), "Optimize did not ask first");
    ui.click_last("Cancel");
    assert!(ui.app().modal.is_none());
    assert!(ui.app().t().map(|t| t.view) == Some(View::Info), "a cancelled Optimize left the Info page");
    assert_eq!(ui.query_text(), before, "a cancelled Optimize filled the Query view");

    ui.click("Optimize");
    ui.click_last("Run");
    assert!(ui.app().t().map(|t| t.view) == Some(View::Sql));
    assert!(ui.query_text().starts_with("OPTIMIZE TABLE"), "{}", ui.query_text());
}

#[test]
fn truncate_asks_for_the_table_name_and_a_locked_connection_turns_every_button_off() {
    let Some(mut ui) = demo_ui(mysql_info) else { return };
    ui.click("Truncate...");
    assert!(matches!(ui.app().modal, Some(Modal::SqlDrop(_))), "Truncate did not open its confirmation");
    ui.h.key_press(egui::Key::Escape);
    ui.settle();

    ui.lock();
    ui.settle();
    for b in ["Analyze", "Optimize", "Check", "Truncate..."] {
        assert!(ui.disabled(b), "{b} is on for a read-only connection");
    }
}

#[test]
fn postgresql_info_offers_vacuum_analyze_and_reindex_with_a_confirm_for_the_slow_two() {
    let Some(mut ui) = demo_ui(pg_info) else { return };
    for b in ["Vacuum", "Analyze", "Reindex"] {
        ui.show(b);
    }
    assert!(ui.h.query_all_by_label("Optimize").next().is_none() && ui.h.query_all_by_label("Truncate...").next().is_none(), "MySQL's buttons show on PostgreSQL");
    ui.click("Analyze");
    let q = ui.query_text();
    assert!(q.starts_with("ANALYZE ") && q.contains("\"orders\""), "{q}");

    let Some(mut ui) = demo_ui(pg_info) else { return };
    for (button, stmt) in [("Vacuum", "VACUUM"), ("Reindex", "REINDEX TABLE")] {
        ui.click(button);
        assert!(matches!(&ui.app().modal, Some(Modal::SqlMaintain { stmt: s, .. }) if s.starts_with(stmt)), "{button} did not ask first");
        ui.click_last("Cancel");
        assert!(ui.app().modal.is_none());
    }
}
