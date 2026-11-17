//! Fixes in the MongoDB Documents tabs, driven through the UI against the live server: bulk confirms that stay on the tab
//! they were opened on, projected rows that refuse writes, a count that runs out of time, a failed undo, a write for another
//! tab, a duplicated tab, a pasted query for another collection, an answer that waits for an open dialog and the lock after a
//! reconnect. Each test owns its collections, made before the app connects so the sidebar lists them.

use super::mongo_edit::{DB, Fixture, seed};
use super::*;
use mongodb::bson::doc;

const VAR: &str = "QUARRY_TEST_MONGO";

fn tab_of(a: &App, coll: &str) -> usize {
    a.tabs.iter().position(|t| t.sel.coll == coll).unwrap_or_else(|| panic!("no tab shows {coll}"))
}

/// Opens `first` from the sidebar and `second` in a new tab with Cmd+click, then makes `first` current again. A plain
/// click would reuse the clean first tab.
fn open_two(ui: &mut UiApp, first: &str, second: &str) {
    ui.open_table(DB, "", first);
    ui.show(second);
    ui.h.get_by_label(second).click_modifiers(egui::Modifiers::COMMAND);
    ui.settle();
    ui.wait("the second tab's rows", |a| a.tabs.len() == 2 && a.t().is_some_and(|t| t.sel.coll == second && !t.loading && !t.docs.is_empty()));
    go_tab(ui, first);
}

fn go_tab(ui: &mut UiApp, coll: &str) {
    ui.mouse_hold(&format!("Tab {coll}"), 0);
    ui.wait("the tab to be current", |a| a.t().is_some_and(|t| t.sel.coll == coll && !t.loading));
}

/// Opens two collections in tabs and the typed confirmation of `item` on the first, makes the second current while the dialog
/// is open, as a shortcut or a link answered meanwhile can, then confirms.
fn confirm_on_another_tab(f: &Fixture, item: &str) -> UiApp {
    let (a, b) = (&f.names[0], &f.names[1]);
    let mut ui = ui_app(VAR).unwrap();
    open_two(&mut ui, a, b);
    ui.unlock();
    ui.click("Document actions");
    ui.click(item);
    ui.wait("the typed confirmation", |x| matches!(&x.modal, Some(Modal::Typed { expect, .. }) if expect == a));
    ui.fill("Confirmation", a);
    let ib = tab_of(ui.app(), b);
    ui.h.state_mut().set_cur(ib);
    ui.settle();
    ui.click_last("Confirm");
    ui
}

#[test]
fn delete_all_matching_runs_on_the_tab_it_was_opened_on() {
    let Some(f) = Fixture::many("delpin", vec![seed(), seed()]) else { return };
    let mut ui = confirm_on_another_tab(&f, "Delete all matching documents…");
    f.expect(&mut ui, vec![]);
    assert_eq!(f.all_in(1), seed(), "the tab that became current lost its documents");
    assert_eq!(ui.app().t().unwrap().sel.coll, f.names[1], "running the action moved off the current tab");
}

#[test]
fn drop_collection_drops_the_collection_it_names() {
    let Some(f) = Fixture::many("droppin", vec![seed(), seed()]) else { return };
    let mut ui = confirm_on_another_tab(&f, "Drop collection…");
    ui.wait("the first collection to be dropped", |_| !f.exists(0));
    assert!(f.exists(1) && f.all_in(1) == seed(), "the tab that became current was dropped");
    ui.wait("the dropped collection's tab to close", |x| x.tabs.iter().all(|t| t.sel.coll != f.names[0]));
}

#[test]
fn projected_rows_refuse_edits_duplicates_and_deletes() {
    let Some(f) = Fixture::new("proj", seed()) else { return };
    let mut ui = ui_app(VAR).unwrap();
    ui.open_table(DB, "", &f.names[0]);
    ui.unlock();
    let a = ui.h.state_mut();
    a.tm().unwrap().proj_txt = "{ name: 1 }".into();
    a.run_query();
    ui.wait("the projected rows", |a| a.t().is_some_and(|t| !t.loading && t.query.projection.is_some() && t.docs.len() == 3));
    let refused = "only the projected fields";

    // An inline edit is not staged: its row holds only some fields, so saving it would fail or drop the others.
    ui.double_click_cell("name", 1);
    ui.wait("the inline editor", |a| a.inline.is_some());
    ui.type_over("changed");
    ui.h.key_press(egui::Key::Enter);
    ui.settle();
    assert!(ui.app().drafts.is_empty(), "a projected row was staged");
    assert!(ui.said(refused), "no warning for the edit: {}", ui.app().last_problem());
    ui.h.key_press(egui::Key::Escape);
    ui.settle();

    // Delete opens no dialog. Toasts are cleared between steps, since a second warning with the same title replaces the first.
    ui.h.state_mut().toasts.clear();
    ui.click("Select row 1");
    ui.click("Delete…");
    assert!(!matches!(ui.app().modal, Some(Modal::Delete(..))), "Delete opened its dialog on projected rows");
    assert!(ui.said(refused), "no warning for Delete: {}", ui.app().last_problem());

    // Duplicate opens no insert editor, which would hold only the projected fields. The document open under its row has a
    // Duplicate of its own; the selection bar floats above it, so its button is the last one.
    ui.h.state_mut().toasts.clear();
    ui.click_last("Duplicate");
    assert!(ui.app().insert.is_none(), "Duplicate opened the insert editor on a projected row");
    assert!(ui.said(refused), "no warning for Duplicate: {}", ui.app().last_problem());
    assert_eq!(f.all(), seed());
}

#[test]
fn a_count_past_the_time_limit_still_shows_the_page() {
    // Each document from _id 60 on costs 10 ms of server JavaScript, so counting them all takes about 3.4 s, past the 1.5 s limit,
    // while the first page, read in insertion order, is instant. $where would be simpler, but count_documents runs an aggregation,
    // where it is refused.
    let docs = (0..400).map(|i| doc! { "_id": i, "n": i }).collect();
    let Some(f) = Fixture::new("slowcount", docs) else { return };
    let mut ui = ui_app(VAR).unwrap();
    // Set once connected, since the query timeout also bounds the connect.
    ui.h.state_mut().prefs.timeout_ms = 1500;
    ui.open_table(DB, "", &f.names[0]);
    let a = ui.h.state_mut();
    a.tm().unwrap().filter_txt = r#"{ $expr: { $function: { body: "function(id) { if (id < 60) return true; var end = Date.now() + 10; while (Date.now() < end) {} return true; }", args: ["$_id"], lang: "js" } } }"#.into();
    a.run_query();
    let size = ui.app().page_size();
    ui.wait("the slow filter's page", |a| a.t().is_some_and(|t| !t.loading && t.query.filter.contains_key("$expr")));
    let t = ui.app().t().unwrap();
    assert!(t.count_approx, "the count did not run out of time or failed the page: {}", ui.app().last_problem());
    assert_eq!(t.docs.len() as u64, size, "the page did not show");
    assert_eq!(t.count, size + 1, "the lower bound is the page plus one, since another page follows");
    assert!(t.bulk_count().starts_with("at least"), "{}", t.bulk_count());
}

#[test]
fn a_failed_undo_stays_in_the_list() {
    let Some(f) = Fixture::new("undofail", seed()) else { return };
    let mut ui = ui_app(VAR).unwrap();
    ui.open_table(DB, "", &f.names[0]);
    ui.unlock();
    ui.click("Select row 2");
    ui.click("Delete…");
    ui.click("Delete");
    ui.wait("the delete and its undo entry", |a| a.undo.len() == 1 && a.t().is_some_and(|t| !t.loading && t.docs.len() == 2));
    // The document comes back another way, so putting it back again fails on its _id.
    f.rt.block_on(async { f.coll(0).insert_one(doc! { "_id": 2, "name": "beta", "qty": 2 }).await.unwrap() });
    // The dialog stays open until the answer, whose error toast brings a Close button of its own.
    ui.undo_latest_open();
    ui.wait("the failed undo to come back", |a| a.undo.len() == 1 && a.last_problem().contains("E11000"));
    assert_eq!(f.all(), seed());
    ui.close_recent_changes();
    // Once the blocker is gone, the same entry still undoes the delete.
    f.rt.block_on(async { f.coll(0).delete_one(doc! { "_id": 2 }).await.unwrap() });
    ui.undo_latest_open();
    f.expect(&mut ui, seed());
}

#[test]
fn a_write_for_another_tab_keeps_the_open_editor() {
    let Some(f) = Fixture::many("otherwrite", vec![seed(), seed()]) else { return };
    let (a, b) = (f.names[0].clone(), f.names[1].clone());
    let mut ui = ui_app(VAR).unwrap();
    open_two(&mut ui, &a, &b);
    go_tab(&mut ui, &b);
    ui.unlock();
    ui.double_click_cell("name", 1);
    ui.wait("the inline editor", |x| x.inline.is_some());
    ui.type_over("typing");
    // A write for the other tab's collection finishes while the editor is open.
    let app = ui.h.state_mut();
    let sel = app.tabs[tab_of(app, &a)].sel.clone();
    let target = app.target_for(&sel).unwrap();
    app.worker.insert(CONN, target, doc! { "_id": 10, "name": "delta" }, None);
    let mut want = seed();
    want.push(doc! { "_id": 10, "name": "delta" });
    f.expect(&mut ui, want);
    ui.wait("the other tab to reload", |x| x.tabs[tab_of(x, &a)].docs.len() == 4 && !x.tabs[tab_of(x, &a)].loading);
    ui.settle();
    assert!(ui.app().inline.as_ref().is_some_and(|i| i.text == "typing"), "a write for another tab closed the editor");
}

#[test]
fn duplicate_tab_keeps_a_typed_filter_marked_unrun() {
    let Some(f) = Fixture::new("duptab", seed()) else { return };
    let coll = f.names[0].clone();
    let mut ui = ui_app(VAR).unwrap();
    ui.open_table(DB, "", &coll);
    ui.h.state_mut().tm().unwrap().filter_txt = "{ qty: 2 }".into();
    ui.settle();
    assert!(crate::app::run::query_dirty(ui.app().t().unwrap()));
    ui.h.get_by_label(&format!("Tab {coll}")).click_secondary();
    ui.settle();
    ui.click("Duplicate");
    ui.wait("the copy's rows", |a| a.tabs.len() == 2 && a.cur == 1 && a.t().is_some_and(|t| !t.loading));
    let t = ui.app().t().unwrap();
    assert_eq!(t.filter_txt, "{ qty: 2 }");
    assert_eq!(t.docs.len(), 3, "the copy ran the typed filter");
    assert!(crate::app::run::query_dirty(t), "the copy marks the typed filter as run although its rows are unfiltered");
}

#[test]
fn a_pasted_query_for_another_collection_keeps_that_tabs_work() {
    let Some(f) = Fixture::many("paste", vec![seed(), seed()]) else { return };
    let (a, b) = (f.names[0].clone(), f.names[1].clone());
    let mut ui = ui_app(VAR).unwrap();
    open_two(&mut ui, &a, &b);
    // The second tab holds a query of its own.
    go_tab(&mut ui, &b);
    let app = ui.h.state_mut();
    app.tm().unwrap().filter_txt = "{ qty: 2 }".into();
    app.run_query();
    ui.wait("the second tab's filtered rows", |x| x.t().is_some_and(|t| !t.loading && t.docs.len() == 1));
    go_tab(&mut ui, &a);
    let pasted = format!("db.{b}.find({{ qty: 3 }})");
    let app = ui.h.state_mut();
    app.tm().unwrap().filter_txt = pasted.clone();
    app.run_query();
    ui.wait("the pasted query in a tab", |x| x.t().is_some_and(|t| t.sel.coll == b && t.filter_txt == pasted && !t.loading && t.docs.len() == 1));
    let tabs = &ui.app().tabs;
    assert_eq!(tabs.len(), 3, "the pasted query took the tab that held work");
    let kept: Vec<&Tab> = tabs.iter().filter(|t| t.sel.coll == b && t.filter_txt == "{ qty: 2 }").collect();
    assert_eq!(kept.len(), 1, "the second tab's own query was overwritten");
    assert_eq!(kept[0].docs[0].decode().unwrap().get_i32("qty"), Ok(2));
}

#[test]
fn an_explain_waits_for_the_open_dialog() {
    let Some(f) = Fixture::new("late", seed()) else { return };
    let coll = f.names[0].clone();
    let mut ui = ui_app(VAR).unwrap();
    ui.open_table(DB, "", &coll);
    // In one frame, so the dialog is open before the plan can arrive.
    let a = ui.h.state_mut();
    a.explain();
    a.modal = Some(Modal::Text { title: "Held".into(), body: "typed".into(), code: false });
    // The query log shows when the server has answered; the frames after it let the answer reach the app.
    let answered = || db::log().iter().any(|e| e.name == "explain" && e.ms.is_some() && e.cmd.contains(coll.as_str()));
    ui.wait("the server to answer the explain", |_| answered());
    ui.run_for(Duration::from_millis(300));
    assert!(matches!(&ui.app().modal, Some(Modal::Text { title, .. }) if title == "Held"), "the plan replaced the open dialog");
    ui.key(egui::Modifiers::NONE, egui::Key::Escape);
    ui.wait("the plan to open", |a| matches!(a.modal, Some(Modal::Explain { .. })));
}

#[test]
fn a_reconnect_locks_writes_again() {
    let Some(f) = Fixture::new("relock", seed()) else { return };
    let uri = std::env::var(VAR).unwrap();
    let mut ui = ui_app(VAR).unwrap();
    ui.open_table(DB, "", &f.names[0]);
    ui.unlock();
    ui.connect(CONN, &uri);
    ui.assert_ready(CONN);
    assert!(!ui.app().conn(CONN).unwrap().writable, "the reconnected connection kept writes on");
    ui.wait("the rows", |a| a.t().is_some_and(|t| !t.loading && t.docs.len() == 3));
    ui.double_click_cell("name", 1);
    ui.show("Stay read-only");
    ui.click("Stay read-only");
    assert!(ui.app().inline.is_none() && ui.app().drafts.is_empty());
    // Always allow writes turns them back on after a reconnect.
    ui.h.state_mut().prefs.always_writable = true;
    ui.connect(CONN, &uri);
    ui.assert_ready(CONN);
    ui.settle();
    assert!(ui.app().conn(CONN).unwrap().writable, "Always allow writes did not turn writes back on");
    assert_eq!(f.all(), seed());
}
