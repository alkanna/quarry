//! Values opened in full, from the menu, the eye or a double click, and JSON shown as a tree, offline on the demo data.

use super::*;

#[test]
fn a_json_value_opens_in_full_laid_out() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    ui.h.state_mut().sql_view_value("note", &Bson::String(r#"{"a":[1,2],"b":{"c":null}}"#.into()));
    match &ui.app().modal {
        Some(Modal::Text { body, code: true, .. }) => assert!(body.lines().count() > 3, "the JSON is not laid out: {body}"),
        _ => panic!("the value did not open"),
    }
    // Its tree lists the keys once switched on.
    ui.click("Tree");
    ui.show("Expand all");
}

/// Row 2's note set to `note`, as if the table held it.
pub(super) fn set_note(ui: &mut UiApp, note: &str) {
    let app = ui.h.state_mut();
    let t = &mut app.tabs[app.cur];
    let mut d = Rows::Raw(&t.docs).owned(1).expect("row 2");
    d.insert("note", note);
    t.docs[1] = db::Row::from_doc(&d).unwrap();
}

/// A right click on the middle of the widget named `label`, which opens its menu.
fn right_click(ui: &mut UiApp, label: &str) {
    ui.show(label);
    let at = ui.h.get_by_label(label).rect().center();
    ui.press_at(at, egui::PointerButton::Secondary, 1);
    ui.settle();
}

pub(super) fn shown_text(ui: &UiApp) -> Option<String> {
    match &ui.app().modal {
        Some(Modal::Text { body, .. }) => Some(body.clone()),
        _ => None,
    }
}

fn read_only(ui: &mut UiApp) {
    for c in &mut ui.h.state_mut().conns {
        c.writable = false;
    }
}

#[test]
fn the_eye_on_a_hovered_cell_opens_the_value_on_a_read_only_connection() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    read_only(&mut ui);
    // Hidden until the pointer is on the cell.
    assert!(ui.h.query_all_by_label("View note row 2").next().is_none(), "the eye shows on a cell nobody hovers");
    ui.show("note row 2");
    let at = ui.cell("note", 2).rect().center();
    ui.h.hover_at(at);
    ui.settle();
    ui.click("View note row 2");
    assert_eq!(shown_text(&ui).as_deref(), Some("Leave at the door"));
    assert!(ui.app().modal.is_some() && ui.app().conns.iter().all(|c| !c.writable), "viewing asked for write access");
}

#[test]
fn view_value_in_the_cell_menu_opens_the_value() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    read_only(&mut ui);
    right_click(&mut ui, "note row 2");
    ui.click("View value…");
    assert_eq!(shown_text(&ui).as_deref(), Some("Leave at the door"));
}

#[test]
fn the_large_editor_switches_between_text_and_tree() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    let json = "{\n  \"door\": \"back\",\n  \"floors\": [1, 2]\n}";
    set_note(&mut ui, json);
    ui.h.state_mut().sql_open_cell(1, "note".into());
    ui.wait("the large editor", |a| a.sql_cell.as_ref().is_some_and(|e| e.big));
    ui.click("Tree");
    ui.show("Expand all");
    // A leaf's key is its own label; a folding row's label also holds its size.
    ui.show("door");
    ui.click("Text");
    assert!(ui.h.query_all_by_label("Expand all").next().is_none(), "the tree stayed after Text");
    assert_eq!(ui.app().sql_cell.as_ref().map(|e| e.text.as_str()), Some(json), "switching changed the text");
}

#[test]
fn a_query_result_value_opens_from_its_eye_and_a_double_click() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_sql_demo();
        a.sql_query_demo();
    }) else { return };
    read_only(&mut ui);
    ui.show("Result row 1");
    // The first column starts after the check box and the row number.
    let row = ui.h.get_by_label("Result row 1").rect();
    let at = egui::pos2(row.left() + CHECK_W + NUM_W + 30.0, row.center().y);
    ui.h.hover_at(at);
    ui.settle();
    ui.click("View name row 1");
    assert_eq!(shown_text(&ui).as_deref(), Some("Ada Byron"));
    ui.h.state_mut().modal = None;
    ui.settle();
    ui.press_at(at, egui::PointerButton::Primary, 2);
    ui.settle();
    assert_eq!(shown_text(&ui).as_deref(), Some("Ada Byron"), "a double click did not open the value");
}
