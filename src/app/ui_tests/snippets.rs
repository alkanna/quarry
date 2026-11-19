//! UI tests of snippets: Cmd+K asks for a script's `{{...}}` values and runs it, a write asks first or goes through
//! Unlock, and a Query tab fills its values on Run. All on the offline demo, with no server.

use super::*;
use crate::app::library::snippet_form::Stage;
use crate::library::{Script, ScriptRef};

const CMD: egui::Modifiers = egui::Modifiers::COMMAND;
const READ: &str = "SELECT id FROM orders WHERE DATE(created_at) = {{day:date}} AND status = {{status:paid|pending=paid}} LIMIT {{n:int=10}};";
const WRITE: &str = "UPDATE orders SET status = 'shipped' WHERE id IN ({{ids:int list}});";

fn with_snippets(a: &mut App) {
    a.load_sql_demo();
    a.sql_new_query(3, "shop".into());
    for (name, body) in [("Orders on a day", READ), ("Mark orders shipped", WRITE)] {
        let id = a.library.next_id();
        a.library.scripts.push(Script { id, name: name.into(), kind: "sql".into(), body: body.into(), conn: 3, db: "shop".into(), ..Default::default() });
    }
}

fn id_of(a: &App, name: &str) -> u64 {
    a.library.scripts.iter().find(|s| s.name == name).map(|s| s.id).unwrap()
}

fn form(a: &mut App) -> &mut crate::app::library::snippet_form::SnippetForm {
    match a.modal.as_mut() {
        Some(Modal::Snippet(f)) => f,
        _ => panic!("the snippet form is not open"),
    }
}

fn query(a: &App) -> String {
    a.t().and_then(|t| t.sql.as_ref()).map(|s| s.query.clone()).unwrap_or_default()
}

#[test]
fn enter_on_a_snippet_asks_for_its_values_and_enter_runs_the_filled_read() {
    let Some(mut ui) = demo_ui(with_snippets) else { return };
    ui.key(CMD, egui::Key::K);
    ui.type_text("orders on");
    ui.show("Orders on a day");
    ui.key(egui::Modifiers::NONE, egui::Key::Enter);
    let f = form(ui.h.state_mut());
    assert_eq!(f.name, "Orders on a day");
    // Defaults come in; a value with none waits for the person.
    assert_eq!([&f.values["day"], &f.values["status"], &f.values["n"]], ["", "paid", "10"]);
    f.values.insert("day".into(), "2026-11-03".into());
    ui.h.run_steps(2);
    ui.key(egui::Modifiers::NONE, egui::Key::Enter);
    assert!(ui.app().modal.is_none());
    assert_eq!(query(ui.app()), "SELECT id FROM orders WHERE DATE(created_at) = '2026-11-03' AND status = 'paid' LIMIT 10;");
    // The next time, the values of the last run come back.
    let id = id_of(ui.app(), "Orders on a day");
    ui.h.state_mut().open_snippet_form(id, None);
    assert_eq!(form(ui.h.state_mut()).values["day"], "2026-11-03");
}

#[test]
fn run_all_on_a_snippet_from_anywhere_asks_for_its_values() {
    let Some(mut ui) = demo_ui(with_snippets) else { return };
    let id = id_of(ui.app(), "Orders on a day");
    ui.h.state_mut().open_script(&ScriptRef::Saved(id), crate::app::library::open::How::Run);
    assert!(matches!(ui.app().modal, Some(Modal::Snippet(_))), "Run all ran a snippet without its values");
    assert!(!query(ui.app()).contains("orders"));
}

#[test]
fn a_tab_reused_for_another_snippet_drops_the_old_values() {
    let Some(mut ui) = demo_ui(with_snippets) else { return };
    let id = id_of(ui.app(), "Orders on a day");
    ui.h.state_mut().open_script(&ScriptRef::Saved(id), crate::app::library::open::How::Here);
    ui.h.run_steps(2);
    let s = ui.h.state_mut().tm().and_then(|t| t.sql.as_mut()).unwrap();
    s.snip_vals.insert("day".into(), "2025-01-01".into());
    s.snip_filled = true;
    let cur = ui.app().cur;
    // Back to the saved text, so the tab is clean and the next script takes it.
    ui.h.state_mut().tabs[cur].sql.as_mut().unwrap().query = READ.into();
    let other = id_of(ui.app(), "Mark orders shipped");
    ui.h.state_mut().open_script(&ScriptRef::Saved(other), crate::app::library::open::How::Here);
    let s = ui.app().t().and_then(|t| t.sql.as_ref()).unwrap();
    assert_eq!(s.query, WRITE);
    assert!(s.snip_vals.is_empty() && !s.snip_filled);
}

#[test]
fn a_bad_value_keeps_the_form_open() {
    let Some(mut ui) = demo_ui(with_snippets) else { return };
    let id = id_of(ui.app(), "Orders on a day");
    ui.h.state_mut().open_snippet_form(id, None);
    form(ui.h.state_mut()).values.insert("day".into(), "tomorrow".into());
    ui.h.run_steps(2);
    ui.show_contains("day needs a date");
    ui.key(egui::Modifiers::NONE, egui::Key::Enter);
    assert!(matches!(ui.app().modal, Some(Modal::Snippet(_))));
    assert!(!query(ui.app()).contains("tomorrow"));
}

#[test]
fn a_write_asks_first_and_esc_steps_back() {
    let Some(mut ui) = demo_ui(with_snippets) else { return };
    let id = id_of(ui.app(), "Mark orders shipped");
    ui.h.state_mut().open_snippet_form(id, None);
    form(ui.h.state_mut()).values.insert("ids".into(), "1, 2".into());
    ui.h.run_steps(2);
    ui.key(egui::Modifiers::NONE, egui::Key::Enter);
    assert!(form(ui.h.state_mut()).stage == Stage::Confirm);
    assert!(!query(ui.app()).contains("UPDATE"), "a write ran before it was confirmed");
    ui.key(egui::Modifiers::NONE, egui::Key::Escape);
    assert!(form(ui.h.state_mut()).stage == Stage::Fill);
    ui.key(egui::Modifiers::NONE, egui::Key::Enter);
    ui.key(CMD, egui::Key::Enter);
    assert!(ui.app().modal.is_none());
    assert_eq!(query(ui.app()), "UPDATE orders SET status = 'shipped' WHERE id IN (1, 2);");
}

#[test]
fn a_write_on_a_locked_connection_unlocks_then_confirms() {
    let Some(mut ui) = demo_ui(with_snippets) else { return };
    for c in &mut ui.h.state_mut().conns {
        c.writable = false;
    }
    let id = id_of(ui.app(), "Mark orders shipped");
    ui.h.state_mut().open_snippet_form(id, None);
    form(ui.h.state_mut()).values.insert("ids".into(), "4405".into());
    ui.h.run_steps(2);
    ui.key(egui::Modifiers::NONE, egui::Key::Enter);
    assert!(matches!(ui.app().modal, Some(Modal::Unlock(3, _))));
    ui.click("Allow writes");
    let f = form(ui.h.state_mut());
    assert!(f.stage == Stage::Confirm);
    assert_eq!(f.values["ids"], "4405");
}

#[test]
fn cmd_enter_on_a_snippet_opens_it_to_edit_and_make_default_moves_it() {
    let Some(mut ui) = demo_ui(with_snippets) else { return };
    ui.key(CMD, egui::Key::K);
    ui.type_text("orders on");
    ui.show("Orders on a day");
    ui.key(CMD, egui::Key::Enter);
    assert!(ui.app().modal.is_none());
    assert_eq!(query(ui.app()), READ);
    let id = id_of(ui.app(), "Orders on a day");
    assert!(ui.app().t().and_then(|t| t.script.as_ref()).is_some_and(|b| b.r == ScriptRef::Saved(id)));
    ui.show("Run on");
    // Run fills the tab's values, and refuses one that does not read.
    let s = ui.h.state_mut().tm().and_then(|t| t.sql.as_mut()).unwrap();
    s.snip_vals.insert("day".into(), "2026-11-03".into());
    assert_eq!(ui.app().snippet_fill_run(READ.into()).unwrap(), "SELECT id FROM orders WHERE DATE(created_at) = '2026-11-03' AND status = 'paid' LIMIT 10;");
    let s = ui.h.state_mut().tm().and_then(|t| t.sql.as_mut()).unwrap();
    s.snip_vals.insert("n".into(), "ten".into());
    assert!(ui.app().snippet_fill_run(READ.into()).unwrap_err().contains("n needs a whole number"));
    // The tab runs in another database: Make default puts that on the script.
    let s = ui.h.state_mut().tm().and_then(|t| t.sql.as_mut()).unwrap();
    s.run_db = "shop_eu".into();
    ui.click("Make default");
    let sc = ui.app().library.scripts.iter().find(|s| s.id == id).unwrap();
    assert_eq!((sc.conn, sc.db.as_str()), (3, "shop_eu"));
}

#[test]
fn make_it_a_value_turns_a_literal_into_a_placeholder() {
    let Some(mut ui) = demo_ui(with_snippets) else { return };
    let text = "SELECT id FROM orders WHERE DATE(created_at) = '2026-11-03';";
    ui.h.state_mut().tm().and_then(|t| t.sql.as_mut()).unwrap().query = text.into();
    let at = text.find('\'').unwrap();
    ui.h.state_mut().ask_make_value(at..at + 12);
    ui.click("Make value");
    assert_eq!(query(ui.app()), "SELECT id FROM orders WHERE DATE(created_at) = {{day:date=2026-11-03}};");
    ui.show("Run on");
}

#[test]
fn esc_closes_a_form_opened_outside_the_palette() {
    let Some(mut ui) = demo_ui(with_snippets) else { return };
    let id = id_of(ui.app(), "Orders on a day");
    ui.h.state_mut().open_snippet_form(id, None);
    ui.h.run_steps(2);
    ui.key(egui::Modifiers::NONE, egui::Key::Escape);
    assert!(ui.app().modal.is_none(), "Esc opened something instead of closing");
}

#[test]
fn a_snippet_whose_connection_was_removed_waits_for_a_pick() {
    let Some(mut ui) = demo_ui(with_snippets) else { return };
    let id = id_of(ui.app(), "Orders on a day");
    ui.h.state_mut().library.scripts.iter_mut().find(|s| s.id == id).unwrap().conn = 9999;
    ui.h.state_mut().open_snippet_form(id, None);
    form(ui.h.state_mut()).values.insert("day".into(), "2026-11-03".into());
    ui.h.run_steps(2);
    ui.show_contains("Pick a connection");
    ui.key(egui::Modifiers::NONE, egui::Key::Enter);
    assert!(matches!(ui.app().modal, Some(Modal::Snippet(_))), "it ran on a connection nobody picked");
}

#[test]
fn running_from_cmd_k_leaves_the_snippet_tab_alone() {
    let Some(mut ui) = demo_ui(with_snippets) else { return };
    let id = id_of(ui.app(), "Orders on a day");
    ui.h.state_mut().open_script(&ScriptRef::Saved(id), crate::app::library::open::How::Here);
    let tabs = ui.app().tabs.len();
    ui.h.state_mut().open_snippet_form(id, None);
    form(ui.h.state_mut()).values.insert("day".into(), "2026-11-03".into());
    ui.h.run_steps(2);
    ui.key(egui::Modifiers::NONE, egui::Key::Enter);
    assert_eq!(ui.app().tabs.len(), tabs + 1, "the filled statement did not get a tab of its own");
    assert!(ui.app().tabs.iter().any(|t| t.script.as_ref().is_some_and(|b| b.r == ScriptRef::Saved(id)) && t.sql.as_ref().is_some_and(|s| s.query == READ)));
}
