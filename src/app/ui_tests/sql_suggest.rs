//! SQL suggestions as typed into the Query editor, offline on the demo schema.

use super::*;

/// A Query tab on the demo's shop database, its editor empty and focused.
fn query_editor() -> Option<UiApp> {
    let mut ui = demo_ui(|a| {
        a.load_sql_demo();
        a.sql_ac_demo();
        a.sql_query_demo();
    })?;
    let app = ui.h.state_mut();
    // Already in memory, so the editor never asks the worker, which has no server here.
    app.sql_schema_asked.insert((3, "shop".into()));
    let cur = app.cur;
    if let Some(s) = app.tabs[cur].sql.as_mut() {
        (s.query, s.caret) = (String::new(), 0);
    }
    ui.h.ctx.memory_mut(|m| m.request_focus(App::sql_editor_id(cur)));
    ui.settle();
    Some(ui)
}

fn listed(ui: &UiApp) -> Vec<String> {
    ui.app().t().and_then(|t| t.sql.as_ref()).map(|s| s.bar.ac.items.iter().map(|x| x.text.clone()).collect()).unwrap_or_default()
}

#[test]
fn tables_list_right_after_from_and_values_after_a_comparison() {
    let Some(mut ui) = query_editor() else { return };
    ui.type_text("SELECT * FROM ");
    assert!(listed(&ui).iter().any(|s| s == "orders"), "no tables after FROM: {:?}", listed(&ui));
    // Typed on without Esc, which would also take the focus from the editor.
    ui.type_text("orders WHERE status = ");
    assert!(listed(&ui).iter().any(|s| s == "'paid'"), "no values after =: {:?}", listed(&ui));
}

#[test]
fn ctrl_space_lists_where_typing_alone_shows_nothing() {
    let Some(mut ui) = query_editor() else { return };
    ui.type_text("SELECT ");
    assert!(listed(&ui).is_empty(), "a list opened after a plain space: {:?}", listed(&ui));
    ui.h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Space);
    ui.settle();
    assert!(!listed(&ui).is_empty(), "Ctrl+Space listed nothing");
    let q = ui.app().t().and_then(|t| t.sql.as_ref()).map(|s| s.query.clone()).unwrap_or_default();
    assert_eq!(q, "SELECT ", "Ctrl+Space typed into the editor");
}
