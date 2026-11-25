//! UI tests of Mongo snippets: Shell scripts and Documents filters with `{{...}}` values, filled from Cmd+K or in their
//! tab. All on the offline demo, with no server.

use super::*;
use crate::app::library::snippet_form::Stage;
use crate::app::library::snippet_strip::Src;
use crate::library::{Script, ScriptRef};

const SHELL: &str = "async function run() {\n  return await orders.find({ status: {{status:paid|shipped=paid}}, createdAt: { $gte: {{from:date}} } }).toArray();\n}";
const WRITE: &str = "async function run() {\n  return await orders.updateMany({ number: {{n:int}} }, { $set: { gift: true } });\n}";
const FILTER: &str = "{ status: {{status:paid|shipped=paid}}, total: { $gte: {{min:number}} } }";

fn with_scripts(a: &mut App) {
    a.load_demo();
    for (name, kind, body, coll) in [("Paid since", "shell", SHELL, ""), ("Gift wrap", "shell", WRITE, ""), ("Big orders", "filter", FILTER, "orders")] {
        let id = a.core.library.next_id();
        a.core.library.scripts.push(Script { id, name: name.into(), kind: kind.into(), body: body.into(), conn: 1, db: "shop".into(), coll: coll.into(), ..Default::default() });
    }
}

fn id_of(a: &App, name: &str) -> u64 {
    a.core.library.scripts.iter().find(|s| s.name == name).map(|s| s.id).unwrap()
}

fn form(a: &mut App) -> &mut crate::app::library::snippet_form::SnippetForm {
    match a.modal.as_mut() {
        Some(Modal::Snippet(f)) => f,
        _ => panic!("the snippet form is not open"),
    }
}

#[test]
fn a_shell_snippet_from_cmd_k_opens_filled_in_the_shell() {
    let Some(mut ui) = demo_ui(with_scripts) else { return };
    ui.key(egui::Modifiers::COMMAND, egui::Key::K);
    ui.type_text("paid since");
    ui.show("Paid since");
    ui.key(egui::Modifiers::NONE, egui::Key::Enter);
    let f = form(ui.h.state_mut());
    assert_eq!([&f.values["status"], &f.values["from"]], ["paid", ""]);
    f.values.insert("from".into(), "2026-11-03".into());
    ui.h.run_steps(2);
    ui.key(egui::Modifiers::NONE, egui::Key::Enter);
    assert!(ui.app().modal.is_none(), "a read asked first");
    let t = ui.app().t().unwrap();
    assert!(t.view == View::Shell);
    assert_eq!(t.shell.code, SHELL.replace("{{status:paid|shipped=paid}}", "\"paid\"").replace("{{from:date}}", "ISODate(\"2026-11-03T00:00:00Z\")"));
    // Run again sends the same text, not one filled a second time.
    assert!(t.snip.off);
    assert_eq!(ui.app().shell_code().unwrap(), t.shell.code);
}

#[test]
fn a_shell_snippet_that_writes_asks_first() {
    let Some(mut ui) = demo_ui(with_scripts) else { return };
    let id = id_of(ui.app(), "Gift wrap");
    ui.h.state_mut().open_snippet_form(id, None);
    form(ui.h.state_mut()).values.insert("n".into(), "10400".into());
    ui.h.run_steps(2);
    ui.key(egui::Modifiers::NONE, egui::Key::Enter);
    assert!(form(ui.h.state_mut()).stage == Stage::Confirm, "a write ran without asking");
}

#[test]
fn a_filter_snippet_from_cmd_k_shows_its_collection_filtered() {
    let Some(mut ui) = demo_ui(with_scripts) else { return };
    let id = id_of(ui.app(), "Big orders");
    // Run on a snippet asks for its values first, from anywhere.
    ui.h.state_mut().open_script(&ScriptRef::Saved(id), crate::app::library::open::How::Run);
    form(ui.h.state_mut()).values.insert("min".into(), "100".into());
    ui.h.run_steps(2);
    ui.key(egui::Modifiers::NONE, egui::Key::Enter);
    assert!(ui.app().modal.is_none());
    let t = ui.app().t().unwrap();
    assert!(t.view == View::Documents && t.sel.coll == "orders");
    assert_eq!(t.filter_txt, "{ status: \"paid\", total: { $gte: 100 } }");
    assert_eq!(t.bar_err, None);
}

#[test]
fn a_shell_tab_fills_its_values_and_refuses_one_in_quotes() {
    let Some(mut ui) = demo_ui(with_scripts) else { return };
    let id = id_of(ui.app(), "Paid since");
    ui.h.state_mut().open_script(&ScriptRef::Saved(id), crate::app::library::open::How::Here);
    ui.h.run_steps(2);
    assert!(ui.app().t().unwrap().view == View::Shell);
    ui.show("Run on");
    assert!(ui.app().shell_code().unwrap_err().contains("from needs a value"));
    ui.h.state_mut().tm().unwrap().snip.vals.insert("from".into(), "2026-11-03".into());
    assert!(ui.app().shell_code().unwrap().contains("$gte: ISODate(\"2026-11-03T00:00:00Z\")"));
    // In the Shell a value goes in code: inside quotes it could be SQL text of an attached connection.
    ui.h.state_mut().tm().unwrap().shell.code = "print('{{from}}')".into();
    assert!(ui.app().shell_code().unwrap_err().contains("outside quotes"));
}

#[test]
fn make_it_a_value_works_in_the_shell() {
    let Some(mut ui) = demo_ui(with_scripts) else { return };
    let code = "f({ s: 'paid' })";
    ui.h.state_mut().open_in_shell(code.into());
    let at = code.find('\'').unwrap();
    ui.h.state_mut().ask_make_value(at..at + 6, Src::Shell);
    ui.click("Make value");
    assert_eq!(ui.app().t().unwrap().shell.code, "f({ s: {{value:text=paid}} })");
}

#[test]
fn the_documents_bar_fills_its_values_and_names_a_missing_one() {
    let Some(mut ui) = demo_ui(|a| a.load_demo()) else { return };
    let t = ui.h.state_mut().tm().unwrap();
    t.view = View::Documents;
    (t.filter_txt, t.sort_txt) = (FILTER.into(), "{ {{by:identifier=total}}: -1 }".into());
    ui.h.run_steps(2);
    ui.show("Run on");
    let err = crate::app::run::find_query(ui.app().t().unwrap()).err().unwrap();
    assert!(err.contains("Filter: min needs a value"), "{err}");
    ui.h.state_mut().tm().unwrap().snip.vals.insert("min".into(), "50".into());
    let q = crate::app::run::find_query(ui.app().t().unwrap()).unwrap();
    assert_eq!(q.filter.get_str("status").unwrap(), "paid");
    assert!(q.sort.unwrap().contains_key("total"));
}
