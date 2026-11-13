//! The record Tab opens under the selected SQL row, offline on the demo data.

use super::*;
use super::sql_view::{set_note, shown_text};

#[test]
fn tab_opens_the_record_where_values_edit_and_esc_closes_it() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    ui.mouse_hold("note row 2", 0);
    assert_eq!(ui.app().t().and_then(|t| t.selected), Some((false, 1)), "the click did not select row 2");
    ui.h.key_press(egui::Key::Tab);
    ui.settle();
    assert!(ui.app().t().is_some_and(|t| t.record), "Tab did not open the record");
    ui.show("note record");
    // A double click on a record value opens the grid's editor there, and Enter stages the edit.
    let at = ui.h.get_by_label("note record").rect().center();
    ui.press_at(at, egui::PointerButton::Primary, 2);
    ui.wait("the cell editor", |a| a.sql_cell.as_ref().is_some_and(|e| e.col == "note"));
    ui.type_text("Back door");
    ui.h.key_press(egui::Key::Enter);
    ui.wait("the edit to be staged", |a| a.sql_cell.is_none());
    assert!(!ui.app().drafts.is_empty(), "the record edit was not staged");
    // Esc closes the record first and keeps the row selected.
    ui.h.key_press(egui::Key::Escape);
    ui.settle();
    assert!(ui.app().t().is_some_and(|t| !t.record && t.selected == Some((false, 1))), "Esc did not close only the record");
    assert!(ui.h.query_all_by_label("note record").next().is_none(), "the record is still drawn");
}

#[test]
fn tab_in_a_text_field_leaves_the_record_closed() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    ui.mouse_hold("note row 2", 0);
    ui.h.ctx.memory_mut(|m| m.request_focus(egui::Id::unique("tree-filter")));
    ui.settle();
    assert!(ui.h.ctx.memory(|m| m.focused().is_some()), "the sidebar filter did not take the focus");
    ui.h.key_press(egui::Key::Tab);
    ui.settle();
    assert!(ui.app().t().is_some_and(|t| !t.record), "Tab in a text field opened the record");
}

#[test]
fn a_json_value_in_the_record_folds_out_as_a_tree_and_has_the_eye() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    set_note(&mut ui, r#"{"door":"back","floors":[1,2]}"#);
    ui.mouse_hold("note row 2", 0);
    ui.h.key_press(egui::Key::Tab);
    ui.settle();
    assert!(ui.h.query_all_by_label("Expand all").next().is_none(), "the tree is open before it was asked for");
    ui.click("note tree");
    ui.show("Expand all");
    ui.show("door");
    // The eye on the record line opens the same value in full, laid out.
    let at = ui.h.get_by_label("note record").rect().center();
    ui.h.hover_at(at);
    ui.settle();
    ui.click("View note record");
    assert!(shown_text(&ui).is_some_and(|b| b.lines().count() > 3), "the record's eye did not open the value laid out");
}

/// The Query view's demo with its editor focused, as it is while a script is typed.
fn query_with_editor_focused() -> Option<UiApp> {
    let mut ui = demo_ui(|a| {
        a.load_sql_demo();
        a.sql_query_demo();
    })?;
    let cur = ui.app().cur;
    ui.h.ctx.memory_mut(|m| m.request_focus(App::sql_editor_id(cur)));
    ui.settle();
    Some(ui)
}

fn qsel(ui: &UiApp) -> Option<usize> {
    ui.app().t().and_then(|t| t.sql.as_ref()).and_then(|s| s.qsel)
}

#[test]
fn tab_opens_the_record_of_a_query_result_row() {
    let Some(mut ui) = query_with_editor_focused() else { return };
    ui.h.key_press(egui::Key::Tab);
    ui.settle();
    assert!(ui.app().t().is_some_and(|t| !t.record), "Tab in the editor opened a record");
    // On the row number, away from the eye a hovered cell shows.
    let r = ui.h.get_by_label("Result row 2").rect();
    ui.press_at(egui::pos2(r.left() + CHECK_W + 24.0, r.center().y), egui::PointerButton::Primary, 1);
    ui.settle();
    assert_eq!(qsel(&ui), Some(1), "the click did not select result row 2");
    assert!(ui.h.ctx.memory(|m| m.focused().is_none()), "the editor kept the focus after the row was clicked");
    ui.h.key_press(egui::Key::Tab);
    ui.settle();
    assert!(ui.app().t().is_some_and(|t| t.record), "Tab did not open the record");
    ui.show("email record");
    // Down moves the selection, and the record with it.
    ui.h.key_press(egui::Key::ArrowDown);
    ui.settle();
    assert!(qsel(&ui) == Some(2) && ui.app().t().is_some_and(|t| t.record), "Down did not move the record to row 3");
    ui.h.key_press(egui::Key::ArrowUp);
    ui.settle();
    // Esc closes the record first, then lets the row go.
    ui.h.key_press(egui::Key::Escape);
    ui.settle();
    assert!(ui.app().t().is_some_and(|t| !t.record) && qsel(&ui) == Some(1), "Esc did not close only the record");
    ui.h.key_press(egui::Key::Escape);
    ui.settle();
    assert_eq!(qsel(&ui), None, "the second Esc kept the row selected");
}

#[test]
fn the_run_tip_lists_every_statement_of_the_script() {
    let Some(mut ui) = query_with_editor_focused() else { return };
    ui.click("Choose what to run");
    ui.show("Run statement 1");
    ui.show("Run statement 3");
}

#[test]
fn tab_leaves_the_record_closed_while_the_result_shows_as_text() {
    let Some(mut ui) = query_with_editor_focused() else { return };
    let cur = ui.app().cur;
    if let Some(s) = ui.h.state_mut().tabs[cur].sql.as_mut() {
        (s.qsel, s.text_mode) = (Some(1), true);
    }
    ui.h.ctx.memory_mut(|m| m.surrender_focus(App::sql_editor_id(cur)));
    ui.settle();
    ui.h.key_press(egui::Key::Tab);
    ui.settle();
    assert!(ui.app().t().is_some_and(|t| !t.record), "Tab opened a record the Text result never draws");
}
