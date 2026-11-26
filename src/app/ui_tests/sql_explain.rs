//! The SQL explain dialog, offline on the demo data. The demo has no server to ask for a plan, so the dialog is opened on a plan the way
//! the worker's answer fills it (`App::demo_explain`), and Cmd+E and the Explain buttons are checked up to the point where the server
//! would be asked.

use super::*;
use crate::db::sql::Dialect;

const STMT: &str = "SELECT id FROM orders";

/// The plan MySQL gives for a filtered full scan of orders, with the cost and rows that make it a finding.
const PLAN: &str = r#"{"query_block":{"select_id":1,"cost_info":{"query_cost":"1250.40"},"table":{"table_name":"orders","access_type":"ALL","rows_examined_per_scan":900000,"filtered":"10.00","cost_info":{"read_cost":"1160.40","eval_cost":"90.00","prefix_cost":"1250.40"},"attached_condition":"(`shop`.`orders`.`status` = 'paid')"}}}"#;

fn explain_open(plan: &'static str) -> Option<UiApp> {
    explain_in(Dialect::MySql, plan)
}

fn explain_in(d: Dialect, plan: &'static str) -> Option<UiApp> {
    let mut ui = demo_ui(App::load_sql_demo)?;
    ui.h.state_mut().demo_explain(d, STMT, plan);
    ui.settle();
    Some(ui)
}

fn explain_state(ui: &UiApp) -> &crate::app::sql::explain::SqlExplain {
    match &ui.app().modal {
        Some(Modal::SqlExplain(x)) => x,
        _ => panic!("the explain dialog is not open"),
    }
}

const PG_JOIN: &str = include_str!("../../../quarry-core/src/db/sql/plan/fixtures/pg18-join-analyze.json");
const PG_SPILL: &str = include_str!("../../../quarry-core/src/db/sql/plan/fixtures/pg18-spill-analyze.json");
const PG_STALE: &str = include_str!("../../../quarry-core/src/db/sql/plan/fixtures/pg18-stale-analyze.json");

/// Whether some widget holds `part` as its text, which a code editor keeps as its value and not as a label.
fn has_value(ui: &UiApp, part: &str) -> bool {
    ui.h.query_all_by(|n| n.value().is_some_and(|v| v.contains(part))).next().is_some()
}

#[test]
fn the_tree_and_the_node_panel_sit_side_by_side_inside_the_window() {
    for size in [egui::vec2(1400.0, 900.0), egui::vec2(860.0, 620.0)] {
        let Some(mut ui) = explain_open(PLAN) else { return };
        ui.h.set_size(size);
        ui.settle();
        // SELF COST is the column kept at every width.
        let cost = ui.h.get_by_label("SELF COST").rect();
        let details = ui.h.get_by_label("DETAILS").rect();
        assert!(cost.right() < details.left(), "the tree runs under the node panel at {size:?}");
        let close = ui.h.get_by_label("Close").rect();
        assert!(close.right() <= size.x, "the dialog runs off the window at {size:?}");
    }
}

#[test]
fn the_dialog_shows_the_plan_its_cost_and_what_to_fix() {
    let Some(mut ui) = explain_open(PLAN) else { return };
    assert!(matches!(ui.app().modal, Some(Modal::SqlExplain(_))), "the explain dialog is not open");
    ui.show("NODE");
    assert!(ui.shows("1 node"), "the head does not count the plan nodes");
    assert!(ui.shows("cost 1,250"), "the head does not give the plan's cost");
    ui.show("Full scan of orders");
    assert!(ui.shows("Reads every row, then keeps those matching status."), "the finding does not say what the scan filters");
    assert!(ui.shows("FILTER"), "the selected node does not show its filter");
    // Nothing has run the statement, so the dialog offers ANALYZE and has not used it.
    assert!(!ui.disabled("Run with ANALYZE"), "ANALYZE is not offered on a read");
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
    ui.click("Tree");
    ui.show("NODE");
    assert!(!has_value(&ui, "query_block"), "the raw text stays under Plan");
}

#[test]
fn a_plan_the_window_cannot_read_opens_on_its_raw_text_with_a_note() {
    let Some(mut ui) = explain_open("1  SIMPLE  orders  ALL") else { return };
    ui.show("Plan not readable.");
    assert!(has_value(&ui, "SIMPLE"), "the raw text is not shown for a plan that cannot be drawn");
    assert!(ui.h.query_by_label("Tree").is_none(), "the Tree and Raw tabs are offered with no plan to draw");
}

#[test]
fn show_rows_in_query_tab_opens_the_classic_explain_there() {
    let Some(mut ui) = explain_open(PLAN) else { return };
    let before = ui.app().core.tabs.len();
    ui.click("Show rows in Query tab");
    assert!(ui.app().modal.is_none(), "the dialog stayed open");
    assert_eq!(ui.app().core.tabs.len(), before + 1, "no Query tab was opened");
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
    let cur = app.core.cur;
    if let Some(s) = app.core.test_mut().tabs[cur].sql.as_mut() {
        s.query = script.into();
        s.caret = 0;
    }
    ui.settle();
    Some(ui)
}

/// Whether the status line now says the demo has no server, which is the answer of the call that would have asked for the plan.
fn reached_server_call(ui: &UiApp) -> bool {
    ui.app().core.status.contains("Offline demo")
}

#[test]
fn cmd_e_and_the_explain_buttons_reach_the_server_call_from_both_views() {
    // The Content view: the bar's button and the shortcut explain the SELECT of the last page.
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    ui.h.state_mut().core.test_mut().status = "idle".into();
    ui.click("Run options");
    ui.click("Explain");
    assert!(reached_server_call(&ui), "the Explain button did not reach the server call");
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    ui.h.state_mut().core.test_mut().status = "idle".into();
    ui.key(egui::Modifiers::COMMAND, egui::Key::E);
    assert!(reached_server_call(&ui), "Cmd+E did not reach the server call on the Content view");
    // The Query view: the button and the shortcut explain the statement under the caret.
    let Some(mut ui) = query_with("SELECT id FROM orders;") else { return };
    ui.h.state_mut().core.test_mut().status = "idle".into();
    ui.click("Explain");
    assert!(reached_server_call(&ui), "the Explain button did not reach the server call");
    let Some(mut ui) = query_with("SELECT id FROM orders;") else { return };
    ui.h.state_mut().core.test_mut().status = "idle".into();
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

#[test]
fn a_row_or_a_finding_shows_its_node_in_the_panel() {
    let Some(mut ui) = explain_in(Dialect::Postgres, PG_STALE) else { return };
    // The panel opens on the node of the top finding.
    assert_eq!(explain_state(&ui).sel, Some(0));
    ui.show("Estimate off 199,000x on events");
    let Some(mut ui) = explain_in(Dialect::Postgres, PG_JOIN) else { return };
    let scan = explain_state(&ui).plan.as_ref().unwrap().nodes.iter().position(|n| n.alias == "si").unwrap();
    ui.click("Seq Scan service_inductors si");
    assert_eq!(explain_state(&ui).sel, Some(scan), "clicking a row did not select its node");
    assert!(ui.shows("builds the join's hash table"), "the panel does not say what the node is for");
}

#[test]
fn analyze_columns_compare_the_estimate_with_what_ran() {
    let Some(mut ui) = explain_in(Dialect::Postgres, PG_STALE) else { return };
    ui.show("EST. > ACTUAL");
    ui.show("TIME");
    assert!(ui.h.query_by_label("SELF COST").is_none(), "the estimate columns stay after ANALYZE");
}

#[test]
fn a_fix_opens_as_a_draft_and_nothing_runs() {
    let Some(mut ui) = explain_in(Dialect::Postgres, PG_SPILL) else { return };
    ui.h.state_mut().core.test_mut().status = "idle".into();
    // The title is on the card and in the panel; the card is the first.
    ui.click_nth("Sort spilled to disk", 0);
    let before = ui.app().core.tabs.len();
    ui.click("Open in Query tab");
    assert!(ui.app().modal.is_none(), "the dialog stayed open");
    assert_eq!(ui.app().core.tabs.len(), before + 1, "no Query tab was opened");
    let s = ui.app().t().and_then(|t| t.sql.as_ref()).expect("the new tab is a SQL tab");
    assert!(s.query.starts_with("SET work_mem = '16MB';\n"), "{}", s.query);
    assert!(s.query.ends_with(&format!("{STMT};")), "{}", s.query);
    assert!(!reached_server_call(&ui), "opening the draft ran it");
}

/// The tab's run picker set to `database`, as before an explain that ran there.
fn pick_db(ui: &mut UiApp, database: &str) {
    let a = ui.h.state_mut();
    let c = a.core.cur;
    a.core.test_mut().tabs[c].sql.as_mut().unwrap().run_db = database.into();
}

#[test]
fn a_draft_runs_in_the_database_the_plan_was_made_in() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    pick_db(&mut ui, "billing");
    ui.h.state_mut().demo_explain(Dialect::Postgres, STMT, PG_SPILL);
    ui.settle();
    ui.h.state_mut().core.test_mut().status = "idle".into();
    let schema = explain_state(&ui).schema.clone();
    ui.click_nth("Sort spilled to disk", 0);
    ui.click("Open in Query tab");
    let s = ui.app().t().and_then(|t| t.sql.as_ref()).expect("the new tab is a SQL tab");
    assert_eq!((s.run_db.as_str(), s.run_schema.as_str()), ("billing", schema.as_str()), "the draft lost the database the plan was made in");
}

#[test]
fn copy_puts_the_fix_on_the_clipboard_with_a_note() {
    let Some(mut ui) = explain_in(Dialect::Postgres, PG_SPILL) else { return };
    ui.click_nth("Sort spilled to disk", 0);
    ui.click_last("Copy");
    assert!(ui.shows("Copied."), "copying said nothing");
}

const FIX_BEFORE: &str = include_str!("../../../quarry-core/src/db/sql/plan/fixtures/pg18-fix-before.json");
const FIX_AFTER: &str = include_str!("../../../quarry-core/src/db/sql/plan/fixtures/pg18-fix-after.json");
const FIX_BEFORE_RUN: &str = include_str!("../../../quarry-core/src/db/sql/plan/fixtures/pg18-fix-before-analyze.json");

/// The statement explained once on `before`, then again on `after`, as after creating an index.
fn explain_twice(before: &'static str, after: &'static str) -> Option<UiApp> {
    let mut ui = explain_in(Dialect::Postgres, before)?;
    ui.h.state_mut().demo_explain(Dialect::Postgres, STMT, after);
    ui.settle();
    Some(ui)
}

#[test]
fn explaining_again_after_an_index_shows_what_changed_in_compare() {
    let Some(mut ui) = explain_twice(FIX_BEFORE, FIX_AFTER) else { return };
    assert!(ui.shows("cost 3,073, was 5,769"), "the head does not give the old cost");
    ui.click("Compare");
    for col in ["NODE", "BEFORE", "AFTER", "CHANGE"] {
        ui.show(col);
    }
    assert!(ui.shows("Cost down 47%."), "the summary does not give the cost change");
    assert!(ui.shows("bookings b now reads through bookings_provider_status_idx."), "the summary does not name the index now used");
    assert!(ui.shows("Compared with the plan from "), "the footer does not say when the old plan was made");
    // The scan that became a bitmap scan is one row that says what it was.
    ui.show("Bitmap Heap Scan bookings b");
    let x = explain_state(&ui);
    let d = &x.prev.as_ref().expect("a previous plan").diff;
    let heap = x.plan.as_ref().unwrap().nodes.iter().position(|n| n.label == "Bitmap Heap Scan").unwrap();
    let row = d.rows.iter().find(|r| r.after == Some(heap)).unwrap();
    assert_eq!(row.was.as_deref(), Some("Seq Scan"));
    assert!(row.cost.text().starts_with('-'), "{}", row.cost.text());
    // The full scan is gone from the new plan.
    ui.show("Full scan of bookings b");
    ui.show("resolved");
    // A row opens its node in the tree.
    ui.click("Bitmap Heap Scan bookings b");
    let x = explain_state(&ui);
    assert_eq!((x.pane, x.sel), (crate::app::sql::explain::Pane::Tree, Some(heap)));
}

#[test]
fn a_first_explain_or_the_same_plan_again_has_no_compare_tab() {
    let Some(mut ui) = explain_in(Dialect::Postgres, FIX_BEFORE) else { return };
    ui.show("Flame");
    assert!(ui.h.query_by_label("Compare").is_none(), "Compare is offered with nothing to compare with");
    let Some(mut ui) = explain_twice(FIX_BEFORE, FIX_BEFORE) else { return };
    ui.show("Flame");
    assert!(ui.h.query_by_label("Compare").is_none(), "Compare is offered for the same plan");
    // An estimate is not compared with a run.
    let Some(mut ui) = explain_twice(FIX_BEFORE, FIX_BEFORE_RUN) else { return };
    ui.show("Flame");
    assert!(ui.h.query_by_label("Compare").is_none(), "an analyzed plan is compared with an estimate");
    // Plans of a connection that closed are forgotten.
    let Some(mut ui) = explain_in(Dialect::Postgres, FIX_BEFORE) else { return };
    let conn = ui.app().t().map(|t| t.sel.conn).unwrap();
    ui.h.state_mut().core.test_mut().drop_session(conn);
    // The close's ForgetPlans effect runs on the next frame, as it does after a real disconnect.
    ui.settle();
    ui.h.state_mut().demo_explain(Dialect::Postgres, STMT, FIX_AFTER);
    ui.settle();
    ui.show("Flame");
    assert!(ui.h.query_by_label("Compare").is_none(), "a plan from before the connection closed is compared");
}

#[test]
fn the_same_statement_in_another_database_is_not_compared() {
    let Some(mut ui) = explain_in(Dialect::Postgres, FIX_BEFORE) else { return };
    pick_db(&mut ui, "billing");
    ui.h.state_mut().demo_explain(Dialect::Postgres, STMT, FIX_AFTER);
    ui.settle();
    ui.show("Flame");
    assert!(ui.h.query_by_label("Compare").is_none(), "a plan from another database is compared");
}

const PG_DATEWRAP: &str = include_str!("../../../quarry-core/src/db/sql/plan/fixtures/pg18-datewrap.json");

/// Whether the plan has the finding of an index its filter defeats, which rests on the catalog.
fn defeated(p: &crate::db::sql::plan::Plan) -> bool {
    p.findings.iter().any(|f| f.title.contains("has an index the planner did not use"))
}

#[test]
fn compare_follows_the_findings_once_the_catalog_answers() {
    let Some(mut ui) = explain_in(Dialect::Postgres, PG_SCAN) else { return };
    ui.h.state_mut().demo_explain(Dialect::Postgres, STMT, PG_DATEWRAP);
    ui.settle();
    assert!(defeated(explain_state(&ui).plan.as_ref().unwrap()), "the demo's catalog shows no index the filter defeats");
    assert!(explain_state(&ui).prev.is_some(), "the two plans are not compared");
    // The catalog answers with nothing about the table: the finding that rested on it goes, from the strip and Compare.
    ui.h.state_mut().demo_explain_evidence(crate::db::sql::plan::Evidence::default());
    ui.settle();
    let x = explain_state(&ui);
    assert!(!defeated(x.plan.as_ref().unwrap()), "the finding stayed without its evidence");
    let titles: Vec<&str> = x.plan.as_ref().unwrap().findings.iter().map(|f| f.title.as_str()).collect();
    for c in &x.prev.as_ref().expect("a previous plan").diff.findings {
        let shown = titles.contains(&c.title.as_str());
        assert_eq!(c.state != crate::db::sql::plan::compare::State::Resolved, shown, "Compare says {:?} for {}", c.state, c.title);
    }
    // The plan kept for the next Compare is the one found with the catalog's answer.
    ui.h.state_mut().demo_explain(Dialect::Postgres, STMT, PG_SCAN);
    ui.settle();
    assert!(!defeated(&explain_state(&ui).prev.as_ref().expect("a previous plan").plan), "the kept plan has the findings from before the catalog answered");
}

#[test]
fn a_flame_bar_selects_its_node_and_time_waits_for_analyze() {
    let Some(mut ui) = explain_in(Dialect::Postgres, FIX_BEFORE) else { return };
    ui.click("Flame");
    ui.show("WIDTH BY");
    assert!(ui.disabled("Time"), "Time is offered before ANALYZE");
    assert!(ui.shows("Time needs ANALYZE"), "the reason Time is off is not given");
    let join = explain_state(&ui).plan.as_ref().unwrap().nodes.iter().position(|n| n.label == "Hash Join (Inner)").unwrap();
    ui.click("Hash Join (Inner)");
    assert_eq!(explain_state(&ui).sel, Some(join), "clicking a bar did not select its node");
    assert!(ui.shows("the join's outer side") || ui.shows("under Sort") || ui.shows("feeds the sort"), "the panel does not follow the bar");
    // After ANALYZE the bars can follow the time.
    let Some(mut ui) = explain_in(Dialect::Postgres, FIX_BEFORE_RUN) else { return };
    ui.click("Flame");
    assert!(!ui.disabled("Time"), "Time is off after ANALYZE");
    ui.click("Time");
    assert!(explain_state(&ui).flame_time, "Time did not switch the widths");
}

const PG_SCAN: &str = include_str!("../../../quarry-core/src/db/sql/plan/fixtures/pg18-scan.json");

#[test]
fn a_filtered_full_scan_shows_its_candidate_index_and_the_evidence() {
    let Some(mut ui) = explain_in(Dialect::Postgres, PG_SCAN) else { return };
    ui.show("CANDIDATE INDEX");
    assert!(ui.shows("From the filter: equal columns first, then the range. Check it against the table's other queries before creating it."), "the candidate has no note");
    assert!(ui.shows("Then Explain again to compare."), "the candidate does not say what to do after");
    assert!(ui.shows("public.bookings: about 400,000 rows"), "the evidence is not shown");
    assert!(ui.shows("Fix: Candidate index"), "the finding card does not name its fix");
}

#[test]
fn the_candidate_index_opens_as_a_draft_and_nothing_runs() {
    let Some(mut ui) = explain_in(Dialect::Postgres, PG_SCAN) else { return };
    ui.h.state_mut().core.test_mut().status = "idle".into();
    ui.show("CANDIDATE INDEX");
    let before = ui.app().core.tabs.len();
    ui.click("Open in Query tab");
    assert!(ui.app().modal.is_none(), "the dialog stayed open");
    assert_eq!(ui.app().core.tabs.len(), before + 1, "no Query tab was opened");
    let s = ui.app().t().and_then(|t| t.sql.as_ref()).expect("the new tab is a SQL tab");
    assert_eq!(s.query, r#"CREATE INDEX CONCURRENTLY "bookings_provider_status_idx" ON "public"."bookings" ("provider", "status");"#);
    assert!(!reached_server_call(&ui), "opening the draft ran it");
}

#[test]
fn copying_the_candidate_index_says_so() {
    let Some(mut ui) = explain_in(Dialect::Postgres, PG_SCAN) else { return };
    ui.show("CANDIDATE INDEX");
    ui.click_last("Copy");
    assert!(ui.shows("Copied."), "copying said nothing");
}

#[test]
fn a_mysql_estimate_alone_suggests_no_index_and_says_why() {
    let Some(mut ui) = explain_open(PLAN) else { return };
    ui.show("Full scan of orders");
    assert!(ui.shows("No index suggested: the 10% of rows the server says the filter keeps is its guess. Run with ANALYZE to measure it."), "the panel does not say why there is no index");
    assert!(ui.h.query_by_label("CANDIDATE INDEX").is_none(), "a guess gave a candidate");
}

const HYPO_TRIAL: &str = include_str!("../../../quarry-core/src/db/sql/plan/fixtures/pg18-hypo-trial.json");
const HYPO_BEFORE: &str = include_str!("../../../quarry-core/src/db/sql/plan/fixtures/pg18-hypo-before.json");

fn trial(plan: &str) -> crate::db::sql::pg::hypo::Trial {
    crate::db::sql::pg::hypo::Trial { plan: plan.into(), index: "<13640>btree_bookings_provider_status".into() }
}

#[test]
fn the_hypopg_trial_is_off_in_the_demo_and_says_why() {
    let Some(mut ui) = explain_in(Dialect::Postgres, FIX_BEFORE) else { return };
    ui.h.state_mut().core.test_mut().status = "idle".into();
    ui.show("CANDIDATE INDEX");
    assert!(ui.disabled("Try it (nothing is created)"), "the trial is offered in the offline demo");
    assert!(ui.shows("The offline demo has no server to try the index on."), "the reason the trial is off is not given");
    assert!(!reached_server_call(&ui));
    // MySQL has no HypoPG, so a MySQL dialog has no button at all.
    let Some(mut ui) = explain_open(PLAN) else { return };
    ui.show("Full scan of orders");
    assert!(ui.h.query_by_label("Try it (nothing is created)").is_none(), "the trial is offered on MySQL");
}

#[test]
fn a_trial_that_uses_the_index_gives_its_cost_and_opens_in_compare() {
    let Some(mut ui) = explain_in(Dialect::Postgres, FIX_BEFORE) else { return };
    ui.h.state_mut().demo_explain_hypo(trial(HYPO_TRIAL));
    ui.settle();
    ui.show("CANDIDATE INDEX");
    assert!(ui.shows("With this index: cost 2,704, was 5,769 (-53%)."), "the trial's cost is not given");
    assert!(explain_state(&ui).hypo.as_ref().is_some_and(|h| h.used));
    ui.click("Show in Compare");
    let x = explain_state(&ui);
    assert_eq!((x.pane, x.hypo_cmp), (crate::app::sql::explain::Pane::Compare, true));
    assert!(ui.shows("After is hypothetical"), "Compare does not say the plan is hypothetical");
    assert!(ui.shows("Cost down 53%."), "the summary does not give the trial's cost change");
    assert!(ui.shows("Compared with the plan the server would choose with the hypothetical index."), "the footer does not say what is compared");
    // Leaving Compare drops the trial from it; with no earlier plan the tab goes away.
    ui.click("Tree");
    assert!(!explain_state(&ui).hypo_cmp);
    ui.show("Flame");
    assert!(ui.h.query_by_label("Compare").is_none(), "Compare stays without a plan to compare with");
}

#[test]
fn a_trial_the_planner_ignores_says_so() {
    let Some(mut ui) = explain_in(Dialect::Postgres, FIX_BEFORE) else { return };
    ui.h.state_mut().demo_explain_hypo(trial(HYPO_BEFORE));
    ui.settle();
    ui.show("CANDIDATE INDEX");
    assert!(ui.shows("The planner would not use this index."), "an unused index is not reported");
}

const PG_SUBJOIN: &str = include_str!("../../../quarry-core/src/db/sql/plan/fixtures/pg18-subjoin.json");

#[test]
fn a_join_that_is_most_of_the_plan_is_explained_with_its_join_filter() {
    let Some(mut ui) = explain_in(Dialect::Postgres, PG_SUBJOIN) else { return };
    ui.show("Hash Join does most of the work");
    assert!(!ui.shows("Nothing in this plan stands out."), "a plan that is nearly all one join says nothing stands out");
    // The panel opens on the join, the top finding's node, and shows the filter each match is checked against.
    let join = explain_state(&ui).plan.as_ref().unwrap().nodes.iter().position(|n| n.label == "Hash Join (Inner)").unwrap();
    assert_eq!(explain_state(&ui).sel, Some(join));
    ui.show("JOIN FILTER");
    assert!(ui.shows("JOIN CONDITION"), "the hash condition is not shown beside the join filter");
}

#[test]
fn the_tree_captions_end_on_their_columns_right_edge() {
    let Some(mut ui) = explain_in(Dialect::Postgres, PG_JOIN) else { return };
    ui.h.set_size(egui::vec2(1400.0, 900.0));
    ui.settle();
    // The values end 8 px inside the row, as side_row pads it; OFF is the last column and 60 px wide.
    let right = ui.h.get_by_label("Seq Scan service_inductors si").rect().right() - 8.0;
    let off = ui.h.get_by_label("OFF").rect().right();
    let est = ui.h.get_by_label("EST. > ACTUAL").rect().right();
    assert!((off - right).abs() <= 2.0, "OFF ends at {off}, its column at {right}");
    assert!((est - (right - 60.0)).abs() <= 2.0, "EST. > ACTUAL ends at {est}, its column at {}", right - 60.0);
}

#[test]
fn the_compare_captions_end_on_their_columns_right_edge() {
    let Some(mut ui) = explain_twice(FIX_BEFORE, FIX_AFTER) else { return };
    ui.h.set_size(egui::vec2(1400.0, 900.0));
    ui.settle();
    ui.click("Compare");
    // The figures end 8 px inside the row; CHANGE is the last column, 90 px wide, and AFTER before it 120 px wide.
    let right = ui.h.get_by_label("Bitmap Heap Scan bookings b").rect().right() - 8.0;
    for (col, edge) in [("CHANGE", right), ("AFTER", right - 90.0), ("BEFORE", right - 210.0)] {
        let at = ui.h.get_by_label(col).rect().right();
        assert!((at - edge).abs() <= 2.0, "{col} ends at {at}, its column at {edge}");
    }
}

/// The statement of the design's MySQL frame, whose plan wraps an indexed column in DATE().
const DATE_STMT: &str = "SELECT o.id, c.name FROM orders o JOIN customers c ON c.id = o.customer_id WHERE o.status = 'paid' AND DATE(o.created_at) = '2026-11-01'";
const MY_JOIN: &str = include_str!("../../../quarry-core/src/db/sql/plan/fixtures/mysql-join.json");

#[test]
fn a_filter_that_defeats_an_index_shows_it_rewritten_and_drafts_the_statement() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    ui.h.state_mut().demo_explain(Dialect::MySql, DATE_STMT, MY_JOIN);
    ui.settle();
    ui.show("REWRITTEN FILTER");
    assert!(ui.shows("Same rows, and o_created_at_idx can serve it."), "the rewrite does not say why it helps");
    assert!(ui.shows("Fix: Rewritten filter"), "the finding card does not name its fix");
    ui.h.state_mut().core.test_mut().status = "idle".into();
    let before = ui.app().core.tabs.len();
    ui.click("Open in Query tab");
    assert!(ui.app().modal.is_none(), "the dialog stayed open");
    assert_eq!(ui.app().core.tabs.len(), before + 1, "no Query tab was opened");
    let s = ui.app().t().and_then(|t| t.sql.as_ref()).expect("the new tab is a SQL tab");
    assert_eq!(s.query, "SELECT o.id, c.name FROM orders o JOIN customers c ON c.id = o.customer_id WHERE o.status = 'paid' AND o.created_at >= '2026-11-01' AND o.created_at < '2026-11-02'");
    assert!(!reached_server_call(&ui), "opening the draft ran it");
}
