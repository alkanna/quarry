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
    let cur = app.core.cur;
    app.core.test_mut().tabs[cur].sql.as_mut().expect("a SQL tab").bar.c.sql = true;
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
        copied |= ui.h.output().platform_output.commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(t) if t.starts_with("SELECT")));
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

#[test]
fn a_mongo_header_sort_leaves_the_line_clean() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    ui.h.run_steps(3);
    assert!(!dirty(&ui), "an untouched bar is out of date");
    let a = ui.app();
    let col = crate::app::table::visible_cols(a.core.prefs.cols.get(&a.col_key(false)), &a.t().unwrap().fields, a.default_cols())[3].clone();
    ui.mouse_hold(&col, 0);
    ui.click("Sort ascending");
    assert!(ui.app().t().is_some_and(|t| t.sort_txt.contains(&col)), "the header menu did not sort on {col}");
    assert!(!dirty(&ui), "a header sort marked the line edited");
}

fn rect_of(ui: &UiApp, label: &str) -> egui::Rect {
    ui.h.get_by_label(label).rect()
}

/// Run and its menu tip are one control: the tip on the right of Run, on the same line.
fn assert_run_split(ui: &UiApp) {
    let (run, tip) = (rect_of(ui, "Run"), rect_of(ui, "Run options"));
    assert!(tip.left() >= run.right() - 1.5 && (tip.center().y - run.center().y).abs() < 1.0, "the Run tip at {tip:?} is not on the right of Run at {run:?}");
}

#[test]
fn the_grouped_sql_line_wraps_its_chips_beside_run() {
    let Some(mut ui) = demo_ui(|a| a.sql_demo("grouped")) else { return };
    ui.settle();
    assert_run_split(&ui);
    let run = rect_of(&ui, "Run");
    let plus = rect_of(&ui, "Add a condition or clause");
    // Run, its tip, and the history and library icons left of them.
    let tools = run.union(rect_of(&ui, "Run options")).with_min_x(run.left() - 120.0);
    for label in ["ORDER BY clause", "GROUP BY clause", "HAVING clause", "Add a condition or clause"] {
        let r = rect_of(&ui, label);
        assert!(!r.intersects(tools), "{label} at {r:?} runs under the Run group at {tools:?}");
    }
    let having = rect_of(&ui, "HAVING clause");
    assert!((plus.center().y - having.center().y).abs() < 2.0 && plus.left() > having.right(), "the + at {plus:?} is not right after the last chip at {having:?}");
    assert!(plus.center().y - run.center().y <= crate::ui::theme::BUTTON_H + 8.0, "the chips took more than two rows: the + is at {plus:?}, Run at {run:?}");
}

#[test]
fn the_mongo_line_keeps_its_plus_on_the_first_row() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    ui.settle();
    assert_run_split(&ui);
    let (run, plus) = (rect_of(&ui, "Run"), rect_of(&ui, "Add a condition or clause"));
    assert!((plus.center().y - run.center().y).abs() < 2.0, "the + at {plus:?} left the row of Run at {run:?}");
    assert!(plus.right() < run.left(), "the + at {plus:?} runs under Run at {run:?}");
}
