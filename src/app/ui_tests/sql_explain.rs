//! The SQL explain dialog, offline on the demo data. The demo has no server to ask for a plan, so the dialog is opened on a plan the way
//! the worker's answer fills it (`App::demo_explain`), and Cmd+E and the Explain buttons are checked up to the point where the server
//! would be asked.

use super::sql_tableops::{disabled, shows};
use super::*;

const STMT: &str = "SELECT id FROM orders";

/// The plan MySQL gives for a full scan of orders, with the cost and rows that make it a warning.
const PLAN: &str = r#"{"query_block":{"select_id":1,"cost_info":{"query_cost":"1250.40"},"table":{"table_name":"orders","access_type":"ALL","rows_examined_per_scan":900000,"filtered":"100.00","cost_info":{"prefix_cost":"1250.40"}}}}"#;

fn explain_open(plan: &'static str) -> Option<UiApp> {
    let mut ui = demo_ui(App::load_sql_demo)?;
    ui.h.state_mut().demo_explain(STMT, plan);
    ui.settle();
    Some(ui)
}

/// Whether some widget holds `part` as its text, which a code editor keeps as its value and not as a label.
fn has_value(ui: &UiApp, part: &str) -> bool {
    ui.h.query_all_by(|n| n.value().is_some_and(|v| v.contains(part))).next().is_some()
}

#[test]
fn the_dialog_shows_the_plan_its_cost_and_what_to_fix() {
    let Some(mut ui) = explain_open(PLAN) else { return };
    assert!(matches!(ui.app().modal, Some(Modal::SqlExplain(_))), "the explain dialog is not open");
    ui.show("NODE");
    assert!(shows(&ui, "1 nodes"), "the head does not count the plan nodes");
    assert!(shows(&ui, "cost 1,250"), "the head does not give the plan's cost");
    assert!(shows(&ui, "Full table scan on orders: about 900,000 rows are read."), "the full scan is not flagged");
    assert!(shows(&ui, "orders"), "the node does not name its table");
    // Nothing has run the statement, so the dialog offers ANALYZE and has not used it.
    assert!(!disabled(&ui, "Run with ANALYZE"), "ANALYZE is not offered on a read");
    ui.click("Close");
    assert!(ui.app().modal.is_none(), "Close left the dialog open");
}

#[test]
fn the_plan_and_raw_toggle_swaps_the_node_grid_for_the_server_text() {
    let Some(mut ui) = explain_open(PLAN) else { return };
    ui.show("NODE");
    assert!(!has_value(&ui, "query_block"), "the raw plan is drawn before it was asked for");
    ui.click("Raw");
    assert!(ui.h.query_by_label("NODE").is_none(), "the node grid stays under Raw");
    assert!(has_value(&ui, "query_block"), "Raw does not show the server's text");
    // The warnings belong to the plan and stay in both views.
    assert!(shows(&ui, "Full table scan on orders"));
    ui.click("Plan");
    ui.show("NODE");
    assert!(!has_value(&ui, "query_block"), "the raw text stays under Plan");
}

#[test]
fn a_plan_the_window_cannot_read_opens_on_its_raw_text_with_a_note() {
    let Some(mut ui) = explain_open("1  SIMPLE  orders  ALL") else { return };
    ui.show("Plan not readable.");
    assert!(has_value(&ui, "SIMPLE"), "the raw text is not shown for a plan that cannot be drawn");
    assert!(ui.h.query_by_label("Plan").is_none(), "the Plan and Raw toggle is offered with no plan to draw");
}

#[test]
fn show_rows_in_query_tab_opens_the_classic_explain_there() {
    let Some(mut ui) = explain_open(PLAN) else { return };
    let before = ui.app().tabs.len();
    ui.click("Show rows in Query tab");
    assert!(ui.app().modal.is_none(), "the dialog stayed open");
    assert_eq!(ui.app().tabs.len(), before + 1, "no Query tab was opened");
    let s = ui.app().t().and_then(|t| t.sql.as_ref()).expect("the new tab is a SQL tab");
    assert_eq!(s.query, format!("EXPLAIN {STMT}"));
}

/// The Query view's demo with `script` in the editor and the caret at its start.
fn query_with(script: &str) -> Option<UiApp> {
    let mut ui = demo_ui(|a| {
        a.load_sql_demo();
        a.sql_query_demo();
    })?;
    let app = ui.h.state_mut();
    let cur = app.cur;
    if let Some(s) = app.tabs[cur].sql.as_mut() {
        s.query = script.into();
        s.caret = 0;
    }
    ui.settle();
    Some(ui)
}

/// Whether the status line now says the demo has no server, which is the answer of the call that would have asked for the plan.
fn reached_server_call(ui: &UiApp) -> bool {
    ui.app().status.contains("Offline demo")
}

#[test]
fn cmd_e_and_the_explain_buttons_reach_the_server_call_from_both_views() {
    // The Content view: the bar's button and the shortcut explain the SELECT of the last page.
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    ui.h.state_mut().status = "idle".into();
    ui.click("Explain");
    assert!(reached_server_call(&ui), "the Explain button did not reach the server call");
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    ui.h.state_mut().status = "idle".into();
    ui.key(egui::Modifiers::COMMAND, egui::Key::E);
    assert!(reached_server_call(&ui), "Cmd+E did not reach the server call on the Content view");
    // The Query view: the button and the shortcut explain the statement under the caret.
    let Some(mut ui) = query_with("SELECT id FROM orders;") else { return };
    ui.h.state_mut().status = "idle".into();
    ui.click("Explain");
    assert!(reached_server_call(&ui), "the Explain button did not reach the server call");
    let Some(mut ui) = query_with("SELECT id FROM orders;") else { return };
    ui.h.state_mut().status = "idle".into();
    ui.key(egui::Modifiers::COMMAND, egui::Key::E);
    assert!(reached_server_call(&ui), "Cmd+E did not reach the server call on the Query view");
}

#[test]
fn a_statement_with_no_plan_is_refused_before_the_server_is_asked() {
    for (script, why) in [("SHOW TABLES;", "SHOW has no plan"), ("EXPLAIN SELECT 1;", "already an explain"), ("   ", "Nothing to run")] {
        let Some(mut ui) = query_with(script) else { return };
        ui.key(egui::Modifiers::COMMAND, egui::Key::E);
        assert!(ui.app().modal.is_none(), "{script:?} opened the dialog");
        let seen = ui.app().last_problem();
        assert!(seen.contains(why), "{script:?} was not refused with {why:?}: {seen}");
    }
}
