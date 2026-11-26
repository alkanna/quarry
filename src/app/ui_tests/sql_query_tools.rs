//! The Query view's toolbar, footer and gutter markers, offline on the demo data.

use super::*;

fn query_demo() -> Option<UiApp> {
    demo_ui(|a| {
        a.load_sql_demo();
        a.sql_query_demo();
    })
}

#[test]
fn a_gutter_marker_runs_its_statement() {
    let Some(mut ui) = query_demo() else { return };
    ui.h.state_mut().core.test_mut().status = "idle".into();
    ui.click("Run statement 3 from the gutter");
    assert!(ui.said("Offline demo"), "the gutter marker did not reach the run path: {}", ui.app().last_problem());
}

#[test]
fn stop_replaces_run_statement_only_while_a_run_is_in_flight() {
    let Some(mut ui) = query_demo() else { return };
    ui.show("Choose what to run");
    assert!(ui.h.query_all_by_label("Stop").next().is_none(), "Stop shows with nothing running");
    let cur = ui.app().core.cur;
    if let Some(s) = ui.h.state_mut().core.test_mut().tabs[cur].sql.as_mut() {
        s.run_req = u64::MAX;
    }
    ui.settle();
    ui.show("Stop");
    assert!(ui.h.query_all_by_label("Choose what to run").next().is_none(), "Run statement stays beside Stop");
}

#[test]
fn history_and_library_sit_in_the_toolbar_with_labels() {
    let Some(ui) = query_demo() else { return };
    let run = ui.h.get_by_label("Format").rect();
    let (history, library) = (ui.h.get_by_label("History").rect(), ui.h.get_by_label_contains("Library").rect());
    assert!((history.center().y - run.center().y).abs() < 6.0, "History is not on the toolbar row");
    assert!((library.center().y - run.center().y).abs() < 6.0, "Library is not on the toolbar row");
    assert!(history.left() > run.right() && library.left() > history.left(), "History and Library are not at the right");
}

#[test]
fn the_footer_reads_the_statement_position_and_the_caret() {
    let Some(mut ui) = query_demo() else { return };
    ui.show_contains("statement 1 of 3");
    ui.show_contains("Ln 6, Col 49");
}

#[test]
fn the_run_menu_offers_explain_and_the_statements() {
    let Some(mut ui) = query_demo() else { return };
    ui.click("Choose what to run");
    ui.show("Explain");
    ui.show("Run statement 2");
}

#[test]
fn lint_of_the_statement_under_the_caret_shows_in_the_footer() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_sql_demo();
        a.sql_query_demo();
        a.demo_lint(false);
    }) else { return };
    assert!(ui.shows("Syntax error near 'SELCT ( FRO orders'"), "the syntax error of the caret's statement is not in the footer");
    assert!(!ui.shows("doesn't exist"), "another statement's mark showed in the footer");
    let cur = ui.app().core.cur;
    let at = ui.query_text().find("SELECT").unwrap() + 3;
    if let Some(s) = ui.h.state_mut().core.test_mut().tabs[cur].sql.as_mut() {
        s.caret = at;
    }
    ui.settle();
    assert!(ui.shows("Table 'shop.shipment' doesn't exist, unless an earlier statement creates it"), "an unknown table is not worded as a warning");
}
