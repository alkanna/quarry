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
