//! Fixes in the MongoDB Documents tabs, driven through the UI against the live server: bulk confirms that stay on the tab
//! they were opened on, projected rows that refuse writes, a count that runs out of time, a failed undo, a write for another
//! tab, a duplicated tab, a pasted query for another collection, an answer that waits for an open dialog and the lock after a
//! reconnect. Each test owns its collections, made before the app connects so the sidebar lists them.

use super::*;
use futures_util::TryStreamExt;
use mongodb::bson::{Document, doc};

const VAR: &str = "QUARRY_TEST_MONGO";
const DB: &str = "quarry_ui_fixes";

/// Collections of one test with their own runtime and client, dropped on drop.
struct Fx {
    rt: tokio::runtime::Runtime,
    db: mongodb::Database,
    names: Vec<String>,
}

impl Fx {
    fn new(test: &str, colls: Vec<Vec<Document>>) -> Option<Fx> {
        let uri = std::env::var(VAR).ok()?;
        // Unique per run so a crashed earlier run and parallel tests never share a collection.
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let names: Vec<String> = (0..colls.len()).map(|i| format!("{test}_{}_{stamp}", (b'a' + i as u8) as char)).collect();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let db = rt.block_on(async {
            let client = mongodb::Client::with_uri_str(&uri).await.unwrap();
            let db = client.database(DB);
            for (n, docs) in names.iter().zip(colls) {
                db.collection::<Document>(n).insert_many(docs).await.unwrap();
            }
            db
        });
        Some(Fx { rt, db, names })
    }

    fn coll(&self, i: usize) -> mongodb::Collection<Document> {
        self.db.collection(&self.names[i])
    }

    fn all(&self, i: usize) -> Vec<Document> {
        self.rt.block_on(async { self.coll(i).find(doc! {}).sort(doc! { "_id": 1 }).await.unwrap().try_collect().await.unwrap() })
    }

    fn exists(&self, i: usize) -> bool {
        self.rt.block_on(async { self.db.list_collection_names().await.unwrap() }).contains(&self.names[i])
    }

    /// Steps the UI until the server holds `want` in collection `i`, since a write lands some frames after its click.
    fn expect(&self, ui: &mut UiApp, i: usize, want: Vec<Document>) {
        let t = std::time::Instant::now();
        loop {
            ui.h.step();
            let got = self.all(i);
            if got == want {
                return;
            }
            assert!(t.elapsed() < LIMIT, "{} holds {got:?}, wanted {want:?}; last error: {}", self.names[i], ui.app().last_problem());
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        for i in 0..self.names.len() {
            self.rt.block_on(async { self.coll(i).drop().await.ok() });
        }
    }
}

fn seed() -> Vec<Document> {
    vec![doc! { "_id": 1, "name": "alpha", "qty": 1 }, doc! { "_id": 2, "name": "beta", "qty": 2 }, doc! { "_id": 3, "name": "gamma", "qty": 3 }]
}

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

/// Replaces the text of the focused editor. The inline editor does not select its text when it opens.
fn type_over(ui: &mut UiApp, text: &str) {
    ui.h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    ui.h.event(egui::Event::Text(text.into()));
    ui.settle();
}

/// Takes back the most recent action through the toolbar and Recent changes, then closes that dialog. A toast may show a Close
/// button of its own, so the one nearest the dialog's title is clicked.
fn undo_latest(ui: &mut UiApp) {
    let n = ui.app().undo.len();
    ui.click(&format!("Undo ({n})"));
    ui.click("Undo change 1");
    let y = ui.h.get_by_label("Recent changes").rect().center().y;
    let near = ui.h.get_all_by_label("Close").min_by(|a, b| (a.rect().center().y - y).abs().total_cmp(&(b.rect().center().y - y).abs())).unwrap();
    near.click();
    ui.settle();
}

/// Opens the Documents bar's menu and the typed confirmation of `item`, then types `coll` into it.
fn typed_confirm_of(ui: &mut UiApp, item: &str, coll: &str) {
    ui.click("Document actions");
    ui.click(item);
    ui.wait("the typed confirmation", |a| matches!(&a.modal, Some(Modal::Typed { expect, .. }) if expect == coll));
    ui.fill("Confirmation", coll);
}

#[test]
fn delete_all_matching_runs_on_the_tab_it_was_opened_on() {
    let Some(f) = Fx::new("delpin", vec![seed(), seed()]) else { return };
    let (a, b) = (f.names[0].clone(), f.names[1].clone());
    let mut ui = ui_app(VAR).unwrap();
    open_two(&mut ui, &a, &b);
    ui.unlock();
    typed_confirm_of(&mut ui, "Delete all matching documents…", &a);
    // Another tab becomes current while the dialog is open, as a shortcut or a link answered meanwhile can make it.
    let ib = tab_of(ui.app(), &b);
    ui.h.state_mut().set_cur(ib);
    ui.settle();
    ui.click_last("Confirm");
    f.expect(&mut ui, 0, vec![]);
    assert_eq!(f.all(1), seed(), "the tab that became current lost its documents");
    assert_eq!(ui.app().t().unwrap().sel.coll, b, "running the action moved off the current tab");
}

#[test]
fn drop_collection_drops_the_collection_it_names() {
    let Some(f) = Fx::new("droppin", vec![seed(), seed()]) else { return };
    let (a, b) = (f.names[0].clone(), f.names[1].clone());
    let mut ui = ui_app(VAR).unwrap();
    open_two(&mut ui, &a, &b);
    ui.unlock();
    typed_confirm_of(&mut ui, "Drop collection…", &a);
    let ib = tab_of(ui.app(), &b);
    ui.h.state_mut().set_cur(ib);
    ui.settle();
    ui.click_last("Confirm");
    let t = std::time::Instant::now();
    while f.exists(0) {
        assert!(t.elapsed() < LIMIT, "{a} was not dropped; last error: {}", ui.app().last_problem());
        ui.h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(f.exists(1) && f.all(1) == seed(), "the tab that became current was dropped");
    ui.wait("the dropped collection's tab to close", |x| x.tabs.iter().all(|t| t.sel.coll != a));
}

#[test]
fn projected_rows_refuse_edits_duplicates_and_deletes() {
    let Some(f) = Fx::new("proj", vec![seed()]) else { return };
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
    type_over(&mut ui, "changed");
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

    // Duplicate opens no insert editor, which would hold only the projected fields.
    ui.h.state_mut().toasts.clear();
    ui.click("Duplicate");
    assert!(ui.app().insert.is_none(), "Duplicate opened the insert editor on a projected row");
    assert!(ui.said(refused), "no warning for Duplicate: {}", ui.app().last_problem());
    assert_eq!(f.all(0), seed());
}

#[test]
fn a_count_past_the_time_limit_still_shows_the_page() {
    // Each document from _id 60 on costs 5 ms of server JavaScript, so counting them all takes about 15 s while the first page,
    // read in insertion order, is instant. $where would be simpler, but count_documents runs an aggregation, where it is refused.
    let docs = (0..3000).map(|i| doc! { "_id": i, "n": i }).collect();
    let Some(f) = Fx::new("slowcount", vec![docs]) else { return };
    let mut ui = ui_app(VAR).unwrap();
    // Set once connected, since the query timeout also bounds the connect.
    ui.h.state_mut().prefs.timeout_ms = 1500;
    ui.open_table(DB, "", &f.names[0]);
    let a = ui.h.state_mut();
    a.tm().unwrap().filter_txt = r#"{ $expr: { $function: { body: "function(id) { if (id < 60) return true; var end = Date.now() + 5; while (Date.now() < end) {} return true; }", args: ["$_id"], lang: "js" } } }"#.into();
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
    let Some(f) = Fx::new("undofail", vec![seed()]) else { return };
    let mut ui = ui_app(VAR).unwrap();
    ui.open_table(DB, "", &f.names[0]);
    ui.unlock();
    ui.click("Select row 2");
    ui.click("Delete…");
    ui.click("Delete");
    ui.wait("the delete and its undo entry", |a| a.undo.len() == 1 && a.t().is_some_and(|t| !t.loading && t.docs.len() == 2));
    // The document comes back another way, so putting it back again fails on its _id.
    f.rt.block_on(async { f.coll(0).insert_one(doc! { "_id": 2, "name": "beta", "qty": 2 }).await.unwrap() });
    undo_latest(&mut ui);
    ui.wait("the failed undo to come back", |a| a.undo.len() == 1 && a.last_problem().contains("E11000"));
    assert_eq!(f.all(0), seed());
    // Once the blocker is gone, the same entry still undoes the delete.
    f.rt.block_on(async { f.coll(0).delete_one(doc! { "_id": 2 }).await.unwrap() });
    undo_latest(&mut ui);
    f.expect(&mut ui, 0, seed());
}

#[test]
fn a_write_for_another_tab_keeps_the_open_editor() {
    let Some(f) = Fx::new("otherwrite", vec![seed(), seed()]) else { return };
    let (a, b) = (f.names[0].clone(), f.names[1].clone());
    let mut ui = ui_app(VAR).unwrap();
    open_two(&mut ui, &a, &b);
    go_tab(&mut ui, &b);
    ui.unlock();
    ui.double_click_cell("name", 1);
    ui.wait("the inline editor", |x| x.inline.is_some());
    type_over(&mut ui, "typing");
    // A write for the other tab's collection finishes while the editor is open.
    let app = ui.h.state_mut();
    let sel = app.tabs[tab_of(app, &a)].sel.clone();
    let target = app.target_for(&sel).unwrap();
    app.worker.insert(CONN, target, doc! { "_id": 10, "name": "delta" }, None);
    let mut want = seed();
    want.push(doc! { "_id": 10, "name": "delta" });
    f.expect(&mut ui, 0, want);
    ui.wait("the other tab to reload", |x| x.tabs[tab_of(x, &a)].docs.len() == 4 && !x.tabs[tab_of(x, &a)].loading);
    ui.settle();
    assert!(ui.app().inline.as_ref().is_some_and(|i| i.text == "typing"), "a write for another tab closed the editor");
}

#[test]
fn duplicate_tab_keeps_a_typed_filter_marked_unrun() {
    let Some(f) = Fx::new("duptab", vec![seed()]) else { return };
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
    let Some(f) = Fx::new("paste", vec![seed(), seed()]) else { return };
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
    let Some(f) = Fx::new("late", vec![seed()]) else { return };
    let mut ui = ui_app(VAR).unwrap();
    ui.open_table(DB, "", &f.names[0]);
    // In one frame, so the dialog is open before the plan can arrive.
    let a = ui.h.state_mut();
    a.explain();
    a.modal = Some(Modal::Text { title: "Held".into(), body: "typed".into(), code: false });
    ui.wait("the plan to arrive", |a| a.late_modal.is_some());
    assert!(matches!(&ui.app().modal, Some(Modal::Text { title, .. }) if title == "Held"), "the plan replaced the open dialog");
    ui.key(egui::Modifiers::NONE, egui::Key::Escape);
    ui.wait("the plan to open", |a| matches!(a.modal, Some(Modal::Explain { .. })));
}

/// Connects the test connection again and waits until it is ready.
fn reconnect(ui: &mut UiApp, uri: &str) {
    let before = ui.app().conn(CONN).unwrap().attempt;
    ui.h.state_mut().connect_uri(CONN, uri.into());
    ui.wait("the reconnect", |a| a.conn(CONN).is_some_and(|c| c.attempt != before && c.status == Status::Ready));
}

#[test]
fn a_reconnect_locks_writes_again() {
    let Some(f) = Fx::new("relock", vec![seed()]) else { return };
    let uri = std::env::var(VAR).unwrap();
    let mut ui = ui_app(VAR).unwrap();
    ui.open_table(DB, "", &f.names[0]);
    ui.unlock();
    reconnect(&mut ui, &uri);
    assert!(!ui.app().conn(CONN).unwrap().writable, "the reconnected connection kept writes on");
    ui.wait("the rows", |a| a.t().is_some_and(|t| !t.loading && t.docs.len() == 3));
    ui.double_click_cell("name", 1);
    ui.show("Stay read-only");
    ui.click("Stay read-only");
    assert!(ui.app().inline.is_none() && ui.app().drafts.is_empty());
    // Always allow writes turns them back on after a reconnect.
    ui.h.state_mut().prefs.always_writable = true;
    reconnect(&mut ui, &uri);
    ui.settle();
    assert!(ui.app().conn(CONN).unwrap().writable, "Always allow writes did not turn writes back on");
    assert_eq!(f.all(0), seed());
}
