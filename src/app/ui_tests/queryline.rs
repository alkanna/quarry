//! The query line of the Documents and Content views, offline on the demo: the "+" menu, the Run menu, and the edited state that
//! shows while the line holds a query the rows below did not come from.

use super::*;
use crate::app::query::query_id;
use crate::app::run::query_dirty;

fn dirty(ui: &UiApp) -> bool {
    ui.app().t().is_some_and(query_dirty)
}

fn focus(ui: &mut UiApp, id: &str) {
    ui.h.ctx.memory_mut(|m| m.request_focus(query_id(id)));
    ui.settle();
}

#[test]
fn a_mongo_filter_typed_without_enter_is_edited_and_enter_runs_it() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    ui.h.run_steps(3);
    assert!(!dirty(&ui), "an untouched bar is out of date");
    focus(&mut ui, "QUERY");
    ui.key(egui::Modifiers::COMMAND, egui::Key::A);
    ui.type_text("{ status: 'paid' }");
    assert!(dirty(&ui), "typing did not mark the line edited");
    assert!(ui.shows("Showing the results of the previous query."), "the note is missing");
    ui.h.key_press(egui::Key::Enter);
    ui.settle();
    assert!(!dirty(&ui), "Enter left the line edited");
}

#[test]
fn sql_text_typed_without_enter_is_edited() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    let app = ui.h.state_mut();
    let cur = app.cur;
    app.tabs[cur].sql.as_mut().expect("a SQL tab").bar.c.sql = true;
    ui.h.run_steps(3);
    assert!(!dirty(&ui), "an untouched SQL bar is out of date");
    focus(&mut ui, "SQL-WHERE");
    ui.type_text(" AND 1 = 1");
    assert!(dirty(&ui), "typing did not mark the line edited");
}

#[test]
fn the_plus_menu_adds_a_limit_chip() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    ui.click("Add a condition or clause");
    ui.click("LIMIT");
    ui.settle();
    assert!(ui.h.ctx.memory(|m| m.has_focus(query_id("SQL-LIMIT"))), "the LIMIT editor did not take the focus");
    ui.type_text("25");
    ui.h.key_press(egui::Key::Enter);
    ui.settle();
    assert_eq!(ui.app().t().and_then(|t| t.sql.as_ref()).map(|s| s.bar.c.limit.clone()).as_deref(), Some("25"));
    assert!(ui.app().t().is_some_and(|t| crate::app::sql::bar::page_query(t).limit == Some(25)));
}

#[test]
fn the_plus_menu_of_a_mongo_line_starts_an_option() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    ui.click("Add a condition or clause");
    ui.click("collation");
    ui.settle();
    assert!(ui.h.ctx.memory(|m| m.has_focus(query_id("COLLATION"))), "the collation editor did not take the focus");
}

#[test]
fn copy_as_sql_is_in_the_run_menu() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    ui.click("Run options");
    ui.h.get_by_label("Copy as SQL").click();
    let mut copied = false;
    for _ in 0..4 {
        ui.h.step();
        copied |= ui.h.ctx.output(|o| o.commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(t) if t.starts_with("SELECT"))));
    }
    assert!(copied, "Copy as SQL copied no SELECT");
}

#[test]
fn the_mongo_run_menu_has_explain_and_copy_as_code() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    ui.click("Run options");
    ui.show("Explain");
    ui.show("Copy as code");
}
