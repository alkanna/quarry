//! UI tests of Cmd+S, the Save to library dialog and the quick pickers, on the offline demo.

use super::*;
use crate::library::ScriptRef;

const CMD: egui::Modifiers = egui::Modifiers::COMMAND;

/// The MySQL demo with an empty Query tab current.
fn sql_query(a: &mut App) {
    a.load_sql_demo();
    a.sql_new_query(3, "shop".into());
}

/// The same with the demo's scripts in the library.
fn sql_query_with_scripts(a: &mut App) {
    sql_query(a);
    a.demo_scripts();
}

fn set_query(ui: &mut UiApp, text: &str) {
    let a = ui.h.state_mut();
    let c = a.cur;
    a.tabs[c].sql.as_mut().unwrap().query = text.into();
}

fn query(ui: &UiApp) -> String {
    ui.app().t().and_then(|t| t.sql.as_ref()).map(|s| s.query.clone()).unwrap_or_default()
}

fn dialog_name(ui: &UiApp) -> Option<String> {
    match &ui.app().modal {
        Some(Modal::SaveScript(f)) => Some(f.name.clone()),
        _ => None,
    }
}

#[test]
fn cmd_s_opens_the_dialog_and_saves_the_script_bound_to_the_tab() {
    let Some(mut ui) = demo_ui(sql_query) else { return };
    set_query(&mut ui, "SELECT 1");
    ui.key(CMD, egui::Key::S);
    // With no comment to take a name from, the tab's name is offered, focused and selected, so typing replaces it.
    let tab = ui.app().tab_name(ui.app().cur);
    assert_eq!(dialog_name(&ui), Some(tab));
    ui.type_text("Late");
    ui.click("Save");
    assert!(ui.app().modal.is_none());
    let lib = &ui.app().library;
    assert_eq!(lib.scripts.len(), 1);
    let s = &lib.scripts[0];
    assert_eq!((s.name.as_str(), s.kind.as_str(), s.body.as_str(), s.conn, s.db.as_str()), ("Late", "sql", "SELECT 1", 3, "shop"));
    assert_eq!(lib.recent, [ScriptRef::Saved(s.id)]);
    assert!(ui.app().t().and_then(|t| t.script.as_ref()).is_some_and(|b| b.r == ScriptRef::Saved(s.id) && b.base == "SELECT 1"));
}

#[test]
fn a_second_cmd_s_writes_back_without_a_dialog() {
    let Some(mut ui) = demo_ui(sql_query) else { return };
    set_query(&mut ui, "SELECT 1");
    ui.key(CMD, egui::Key::S);
    ui.click("Save");
    set_query(&mut ui, "SELECT 2");
    ui.key(CMD, egui::Key::S);
    assert!(ui.app().modal.is_none(), "the bound script is written without asking");
    let lib = &ui.app().library;
    assert_eq!((lib.scripts.len(), lib.scripts[0].body.as_str()), (1, "SELECT 2"));
    assert!(ui.app().t().and_then(|t| t.script.as_ref()).is_some_and(|b| b.base == "SELECT 2"));
}

#[test]
fn saving_under_a_taken_name_in_the_same_folder_replaces_it() {
    let Some(mut ui) = demo_ui(sql_query) else { return };
    set_query(&mut ui, "SELECT 1");
    ui.key(CMD, egui::Key::S);
    ui.click("Save");
    // Another tab, as an unsaved script with the same name.
    ui.h.state_mut().sql_new_query(3, "shop".into());
    set_query(&mut ui, "SELECT 9");
    ui.key(CMD, egui::Key::S);
    let first = ui.app().library.scripts[0].name.clone();
    if let Some(Modal::SaveScript(f)) = &mut ui.h.state_mut().modal {
        f.name = first.clone();
    }
    // The button says what will happen, and the toast after it can take it back.
    ui.click("Replace");
    let lib = &ui.app().library;
    assert_eq!((lib.scripts.len(), lib.scripts[0].body.as_str()), (1, "SELECT 9"));
    ui.click_last("Undo");
    assert_eq!(ui.app().library.scripts[0].body, "SELECT 1", "Undo did not put the replaced script back");
}

#[test]
fn a_new_file_is_named_in_the_style_of_its_folder() {
    use crate::app::library::save_dialog::styled_name;
    let kebab = ["monthly-revenue.sql".to_string(), "top-customers.sql".into(), "x.sql".into()];
    assert_eq!(styled_name("Late shipments", "sql", &kebab), "late-shipments.sql");
    let snake = ["monthly_revenue.sql".to_string(), "top_customers.sql".into()];
    assert_eq!(styled_name("Late shipments", "sql", &snake), "late_shipments.sql");
    assert_eq!(styled_name("Late shipments", "sql", &[]), "Late shipments.sql");
}

#[test]
fn a_leading_comment_names_the_script() {
    let Some(mut ui) = demo_ui(sql_query) else { return };
    set_query(&mut ui, "-- Paid orders that never shipped\nSELECT 1");
    ui.key(CMD, egui::Key::S);
    assert_eq!(dialog_name(&ui).as_deref(), Some("Paid orders that never shipped"));
    use crate::app::library::save_dialog::comment_name;
    assert_eq!(comment_name("/* Totals */\nSELECT 1").as_deref(), Some("Totals"));
    assert_eq!(comment_name("// count\nasync function run() {}").as_deref(), Some("count"));
    assert_eq!(comment_name("SELECT 1 -- not a name"), None);
}

#[test]
fn cmd_s_with_staged_grid_edits_saves_only_the_edits() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_sql_demo();
        a.sql_demo_stage();
    }) else { return };
    assert!(!ui.app().drafts.is_empty());
    ui.key(CMD, egui::Key::S);
    assert!(!matches!(ui.app().modal, Some(Modal::SaveScript(_))), "the grid's Save took the key");
    assert!(ui.app().library.scripts.is_empty());
}

#[test]
fn cmd_s_in_a_view_without_a_script_does_nothing() {
    // A table's Structure view holds no script, and the library must not answer the key there with a warning about a
    // missing script.
    let Some(mut ui) = demo_ui(|a| {
        a.load_sql_demo();
        if let Some(t) = a.tm() {
            t.view = View::Structure;
        }
    }) else { return };
    assert!(ui.app().tab_text().is_none() && ui.app().drafts.is_empty());
    ui.key(CMD, egui::Key::S);
    assert!(!matches!(ui.app().modal, Some(Modal::SaveScript(_))));
    assert!(!ui.app().last_problem().contains("no script to save"), "{}", ui.app().last_problem());
}

#[test]
fn cmd_s_with_a_document_draft_saves_only_the_draft() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    {
        let a = ui.h.state_mut();
        let t = &a.tabs[a.cur];
        let old = t.docs[2].owned();
        let mut new = old.clone();
        new.insert("status", "refunded");
        let key = format!("{}#{}", t.coll_key(), db::compact(old.get("_id").unwrap_or(&Bson::Null)));
        let sel = t.sel.clone();
        crate::app::drafts::stage_draft(&mut a.drafts, &key, &sel, &old, new);
    }
    ui.settle();
    ui.key(CMD, egui::Key::S);
    assert!(!matches!(ui.app().modal, Some(Modal::SaveScript(_))), "the document's Save took the key");
    assert!(ui.app().library.scripts.is_empty());
}

#[test]
fn a_recent_script_loads_its_text_into_the_empty_tab() {
    let Some(mut ui) = demo_ui(sql_query_with_scripts) else { return };
    let tabs = ui.app().tabs.len();
    ui.click("Library 5");
    ui.click("Script Late shipments");
    assert!(query(&ui).starts_with("-- Paid orders that never shipped"));
    assert_eq!(ui.app().tabs.len(), tabs, "an empty tab is reused, as a click on a table does");
    assert!(ui.app().t().and_then(|t| t.script.as_ref()).is_some());
}

#[test]
fn a_script_opens_beside_a_tab_that_holds_text() {
    let Some(mut ui) = demo_ui(sql_query_with_scripts) else { return };
    set_query(&mut ui, "SELECT 7");
    let tabs = ui.app().tabs.len();
    let id = ui.app().library.scripts.iter().find(|s| s.name == "Revenue by tier" && s.kind == "sql").map(|s| s.id).unwrap();
    ui.h.state_mut().open_script(&ScriptRef::Saved(id), crate::app::library::open::How::Here);
    ui.settle();
    assert_eq!(ui.app().tabs.len(), tabs + 1);
    assert!(query(&ui).starts_with("SELECT tier"));
    // The first tab kept what was typed there.
    let first = ui.app().tabs.iter().filter_map(|t| t.sql.as_ref()).any(|s| s.query == "SELECT 7");
    assert!(first);
}

#[test]
fn saving_from_the_shell_makes_a_shell_script() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    {
        let a = ui.h.state_mut();
        let c = a.cur;
        a.tabs[c].view = View::Shell;
        a.tabs[c].shell.code = "async function run() {\n  return await orders.count();\n}".into();
    }
    ui.settle();
    ui.key(CMD, egui::Key::S);
    assert!(matches!(ui.app().modal, Some(Modal::SaveScript(_))));
    ui.click("Save");
    let s = &ui.app().library.scripts[0];
    assert_eq!((s.kind.as_str(), s.conn, s.db.as_str()), ("shell", 1, "shop"));
    assert!(s.body.contains("orders.count"));
}

#[test]
fn a_shell_script_opens_in_a_tab_with_no_collection_named_for_the_database() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    ui.h.state_mut().demo_scripts();
    let id = ui.app().library.scripts.iter().find(|s| s.kind == "shell" && s.conn == 1).map(|s| s.id).unwrap();
    let before = ui.app().tabs.len();
    ui.h.state_mut().open_script(&ScriptRef::Saved(id), crate::app::library::open::How::Here);
    ui.settle();
    let a = ui.app();
    assert_eq!(a.tabs.len(), before + 1);
    let t = a.t().unwrap();
    assert!(t.view == View::Shell && t.sel.coll.is_empty() && t.sel.db == "shop");
    assert_eq!(a.tab_name(a.cur), "Backfill note field", "a bound tab is named for its script");
    assert!(t.shell.code.contains("Fill the empty notes"));
    // The tab binds again to its script after a restart, and an edited one stays edited.
    let bound = a.rebind(t, ScriptRef::Saved(id)).expect("bound again");
    assert_eq!(bound.base, a.library.scripts.iter().find(|s| s.id == id).unwrap().body);
}

#[test]
fn a_shell_script_of_a_sql_connection_shows_the_shell() {
    let Some(mut ui) = demo_ui(sql_query_with_scripts) else { return };
    let id = ui.app().library.scripts.iter().find(|s| s.kind == "shell" && s.conn == 3).map(|s| s.id).unwrap();
    ui.h.state_mut().open_script(&ScriptRef::Saved(id), crate::app::library::open::How::NewTab);
    ui.settle();
    let a = ui.app();
    assert!(a.t().is_some_and(|t| t.view == View::Shell && t.shell.code.contains("COUNT(*)")));
    assert_eq!(a.tab_name(a.cur), "Count orders by status", "a bound tab is named for its script");
    // The Shell is on screen, not the Query view that shares the tab.
    ui.h.get_by_label("Shell editor");
}

#[test]
fn a_filter_script_opens_its_collection_with_the_query_set() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    ui.h.state_mut().demo_scripts();
    let id = ui.app().library.scripts.iter().find(|s| s.kind == "filter" && s.name == "Pending orders").map(|s| s.id).unwrap();
    ui.h.state_mut().open_script(&ScriptRef::Saved(id), crate::app::library::open::How::NewTab);
    ui.settle();
    let t = ui.app().t().unwrap();
    assert!(t.view == View::Documents);
    assert_eq!((t.sel.coll.as_str(), t.filter_txt.as_str()), ("orders", "{ status: 'pending' }"));
}

#[test]
fn the_old_saved_queries_move_into_the_library_once() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    let a = ui.h.state_mut();
    a.prefs.library.insert("3/#sql".into(), vec![SavedQuery { name: "Old".into(), sql: Some("SELECT 1".into()), ..Default::default() }]);
    a.prefs.library.insert("1/shop/orders".into(), vec![SavedQuery { name: "Paid".into(), filter: "{ status: 'paid' }".into(), ..Default::default() }]);
    a.migrate_library();
    a.migrate_library();
    let lib = &a.library;
    assert_eq!(lib.scripts.len(), 2);
    assert_eq!(lib.scripts.iter().map(|s| s.kind.as_str()).collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>(), ["filter", "sql"]);
    // ui.json keeps its copy for an older build.
    assert_eq!(a.prefs.library.len(), 2);
}

#[test]
fn an_orphaned_folder_and_script_show_at_the_top_of_the_save_dialog_and_the_picker() {
    use crate::library::{Folder, Script};
    let Some(mut ui) = demo_ui(sql_query_with_scripts) else { return };
    let lost = {
        let a = ui.h.state_mut();
        let lost = a.library.next_id();
        a.library.folders.push(Folder { id: lost, name: "Lost folder".into(), parent: Some(9_999) });
        let id = a.library.next_id();
        a.library.scripts.push(Script { id, name: "Lost script".into(), kind: "sql".into(), body: "SELECT 2;".into(), conn: 3, folder: Some(9_998), ..Default::default() });
        lost
    };
    // The panel puts both at the top level; the Save dialog's folder list and the picker must too.
    let rows = crate::app::library::save_dialog::folder_rows(&ui.app().library);
    assert!(rows.contains(&(lost, 0)), "{rows:?}");
    ui.click("Library 6");
    ui.click("Script Lost script");
    assert_eq!(query(&ui), "SELECT 2;");
}

#[test]
fn the_picker_fades_in_once_and_stays_inside_the_window() {
    let Some(mut ui) = demo_ui(sql_query_with_scripts) else { return };
    // When each foreground area started to show: a popover whose fade-in restarts would show see-through for good.
    let starts = |ctx: &egui::Context| {
        let ids = ctx.memory(|m| m.areas().visible_layer_ids());
        ids.into_iter().filter(|l| l.order == egui::Order::Foreground).filter_map(|l| egui::AreaState::load(ctx, l.id).and_then(|s| s.last_became_visible_at)).map(f64::to_bits).collect::<std::collections::BTreeSet<u64>>()
    };
    ui.h.get_by_label("Library 5").click();
    let mut first = None;
    for _ in 0..8 {
        ui.h.step();
        // The frame the lists arrive in must not place the popover from the empty sizing pass, past the window's edge.
        if let Some(n) = ui.h.query_all_by_label("Search library").next() {
            assert!(n.rect().right() <= 1400.0, "the popover runs off the window at {}", n.rect().right());
        }
        let now = starts(&ui.h.ctx);
        if !now.is_empty() {
            assert_eq!(*first.get_or_insert_with(|| now.clone()), now, "the popover's fade-in started again");
        }
    }
    assert!(first.is_some(), "the popover never showed");
    ui.show("Script Late shipments");
}

#[test]
fn the_picker_lists_are_built_only_while_it_is_open() {
    use crate::app::library::picker::PickKind;
    let Some(mut ui) = demo_ui(sql_query_with_scripts) else { return };
    let ctx = ui.h.ctx.clone();
    let closed = ui.app().pick_data(&ctx, PickKind::Sql);
    assert!(!closed.full && closed.rows.is_empty() && closed.count == 5);
    ui.click("Library 5");
    ui.show("Script Late shipments");
    let open = ui.app().pick_data(&ctx, PickKind::Sql);
    assert!(open.full && open.rows.len() == 5);
}

#[test]
fn a_sql_content_filter_saves_to_the_library_lists_in_its_picker_and_opens_again() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    let t = ui.app().t().unwrap();
    assert!(t.view == View::Documents && t.sql.is_some() && t.sel.coll == "orders", "the demo opens on the orders Content view");
    // The bar in Rules mode has written the demo's WHERE back in its own spelling by now; that is the text saved.
    let (filter, sort) = (t.filter_txt.clone(), t.sort_txt.clone());
    assert!(filter.contains("'paid'") && sort == "created_at DESC", "{filter} / {sort}");
    ui.h.state_mut().save_script();
    assert!(matches!(&ui.app().modal, Some(Modal::SaveScript(f)) if f.kind == "where"), "the Content view saves a table filter");
    // The name field takes the focus when the dialog first draws.
    ui.settle();
    ui.type_text("Paid over 50");
    ui.click("Save");
    let s = ui.app().library.scripts.iter().find(|s| s.kind == "where").cloned().expect("a saved filter");
    assert_eq!((s.name.as_str(), s.body.as_str(), s.sort.as_str(), s.db.as_str(), s.coll.as_str()), ("Paid over 50", filter.as_str(), sort.as_str(), "shop", "orders"));
    let ctx = ui.h.ctx.clone();
    assert_eq!(ui.app().pick_data(&ctx, PickKind::Where).count, 1);
    // Another kind's picker does not list it, nor does a Mongo view.
    assert_eq!(ui.app().pick_data(&ctx, PickKind::Sql).count, 0);
    let a = ui.h.state_mut();
    let c = a.cur;
    (a.tabs[c].filter_txt, a.tabs[c].sort_txt) = (String::new(), String::new());
    a.open_script(&ScriptRef::Saved(s.id), crate::app::library::open::How::Here);
    ui.settle();
    let t = ui.app().t().unwrap();
    assert_eq!((t.sel.coll.as_str(), t.filter_txt.as_str(), t.sort_txt.as_str()), ("orders", filter.as_str(), sort.as_str()));
}
