//! Dragging to reorder with the shared engine (`ui::dnd::reorder`), offline on the demo data: rows of the column
//! popover, grid headers in the Mongo and SQL tables, a header click that still opens its menu, and Esc.

use super::*;

/// The columns the grid shows, in order.
fn shown(ui: &UiApp) -> Vec<String> {
    let a = ui.app();
    crate::app::table::visible_cols(a.prefs.cols.get(&a.col_key(false)), &a.t().unwrap().fields, a.default_cols())
}

/// The rect of the header of `col`. Other widgets in the Mongo demo carry field names too, so it is the one right above
/// the column's first cell.
fn header(ui: &UiApp, col: &str) -> egui::Rect {
    let cell = ui.h.get_by_label(Box::leak(format!("{col} row 1").into_boxed_str())).rect();
    ui.h.get_all_by_label(col).map(|n| n.rect()).filter(|r| r.bottom() <= cell.top() + 1.0 && r.x_range().intersects(cell.x_range())).max_by(|a, b| a.top().total_cmp(&b.top())).expect("a header")
}

/// The point `frac` of the way across the header `col`, at its middle height.
fn across(ui: &UiApp, col: &str, frac: f32) -> egui::Pos2 {
    let r = header(ui, col);
    egui::pos2(r.left() + r.width() * frac, r.center().y)
}

fn open_columns(ui: &mut UiApp) {
    let n = shown(ui).len();
    ui.click(&format!("Columns {n}"));
    let first = format!("{} column", shown(ui)[0]);
    ui.show(&first);
}

#[test]
fn a_column_moves_by_dragging_its_whole_row_in_the_popover() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    let before = shown(&ui);
    open_columns(&mut ui);
    // From the middle of the row, nowhere near the grip, to the lower part of the row two below.
    ui.drag(&format!("{} column", before[1]), &format!("{} column", before[3]), 0.8, 1);
    let mut want = before.clone();
    let c = want.remove(1);
    want.insert(3, c);
    assert_eq!(shown(&ui), want, "the row did not land after the one it was dropped on");
    assert!(ui.app().prefs.cols.get(&ui.app().col_key(false)).is_some_and(|p| p.shown.as_ref() == Some(&want)), "the order was not kept in the column prefs");
}

#[test]
fn esc_cancels_a_drag_and_keeps_the_popover_open() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    let before = shown(&ui);
    open_columns(&mut ui);
    let from = ui.h.get_by_label(&format!("{} column", before[0])).rect().center();
    let r = ui.h.get_by_label(&format!("{} column", before[2])).rect();
    let to = egui::pos2(r.center().x, r.top() + r.height() * 0.8);
    ui.press_and_move(from, to);
    ui.h.key_press(egui::Key::Escape);
    ui.h.step();
    ui.release(to);
    assert_eq!(shown(&ui), before, "Esc did not cancel the drag");
    assert!(ui.h.query_all_by_label(&format!("{} column", before[1])).next().is_some(), "Esc closed the popover instead of only cancelling the drag");
}

/// How many of the shown columns are pinned; they come first.
fn pins(ui: &UiApp) -> usize {
    let a = ui.app();
    let p = a.prefs.cols.get(&a.col_key(false)).map(|p| p.pinned.clone()).unwrap_or_default();
    shown(ui).iter().filter(|c| p.contains(c)).count()
}

fn header_drag_reorders(load: fn(&mut App)) {
    let Some(mut ui) = demo_ui(load) else { return };
    let before = shown(&ui);
    // The first unpinned header, past the middle of the one two further, so it lands after that one.
    let k = pins(&ui);
    let (from, to) = (across(&ui, &before[k], 0.5), across(&ui, &before[k + 2], 0.8));
    ui.press_and_move(from, to);
    ui.h.step();
    ui.release(to);
    let mut want = before.clone();
    let c = want.remove(k);
    want.insert(k + 2, c);
    assert_eq!(shown(&ui), want, "the header did not land after the one it was dropped on");
    assert!(ui.h.query_all_by_label("Sort ascending").next().is_none(), "the drag opened the column menu as if it were a click");
}

#[test]
fn a_pinned_header_stays_in_the_pinned_block() {
    // The Mongo demo pins status, alone in its block.
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    let before = shown(&ui);
    assert_eq!(pins(&ui), 1, "the demo no longer pins one column");
    let (from, to) = (across(&ui, &before[0], 0.5), across(&ui, &before[2], 0.8));
    ui.press_and_move(from, to);
    ui.release(to);
    assert_eq!(shown(&ui), before, "a pinned column left the pinned block");
    assert_eq!(pins(&ui), 1, "a drag changed which columns are pinned");
}

#[test]
fn a_mongo_column_moves_by_dragging_its_header() {
    header_drag_reorders(App::load_demo);
}

#[test]
fn a_sql_column_moves_by_dragging_its_header() {
    header_drag_reorders(App::load_sql_demo);
}

#[test]
fn a_click_on_a_header_still_opens_its_menu_and_sorts() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    let before = shown(&ui);
    let c = before[3].clone();
    ui.mouse_hold(&c, 0);
    ui.click("Sort ascending");
    let sort = ui.app().t().and_then(|t| t.query.sort.clone()).unwrap_or_default();
    assert!(sort.len() == 1 && sort.get(&c).is_some_and(|v| v.to_string() == "1"), "the header menu did not sort ascending on {c}: {sort}");
    assert_eq!(shown(&ui), before, "a click moved a column");
}

#[test]
fn esc_cancels_a_header_drag() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    let before = shown(&ui);
    let (from, to) = (across(&ui, &before[0], 0.5), across(&ui, &before[2], 0.8));
    ui.press_and_move(from, to);
    ui.h.key_press(egui::Key::Escape);
    ui.h.step();
    ui.release(to);
    assert_eq!(shown(&ui), before, "Esc did not cancel the header drag");
}

#[test]
fn the_resize_edge_still_resizes_instead_of_dragging_the_header() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    let before = shown(&ui);
    let c = before[pins(&ui)].clone();
    // The header's widget is inset 4 pt from its cell, and the resize edge covers 3 pt either side of the cell's border.
    let from = across(&ui, &c, 1.0) + egui::vec2(3.0, 0.0);
    let to = from + egui::vec2(60.0, 0.0);
    ui.press_and_move(from, to);
    ui.release(to);
    assert_eq!(shown(&ui), before, "dragging the resize edge moved the column");
    let a = ui.app();
    assert!(a.prefs.cols.get(&a.col_key(false)).is_some_and(|p| p.widths.contains_key(&c)), "dragging the resize edge did not resize {c}");
}

#[test]
fn alt_arrow_moves_the_focused_header() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    let before = shown(&ui);
    let k = pins(&ui);
    let r = header(&ui, &before[k]);
    ui.h.get_all_by_label(&before[k]).find(|n| n.rect() == r).expect("the header").focus();
    ui.settle();
    ui.key(egui::Modifiers::ALT, egui::Key::ArrowRight);
    let mut want = before.clone();
    want.swap(k, k + 1);
    assert_eq!(shown(&ui), want, "Alt+Right did not move the focused header");
}

#[test]
fn the_shortcut_list_names_the_keyboard_moves() {
    assert!(SHORTCUTS.iter().any(|(k, what)| k.starts_with("Alt+Up/Down") && what.contains("Move the focused row")), "Alt+arrows are missing from the shortcut list");
}

/// Opens the right-click menu of the widget named `label`.
fn menu_of(ui: &mut UiApp, label: &str) {
    ui.show(label);
    let at = ui.h.get_by_label(label).rect().center();
    ui.press_at(at, egui::PointerButton::Secondary, 1);
    ui.settle();
}

#[test]
fn a_tab_moves_from_its_menu() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    let colls = |a: &App| a.tabs.iter().map(|t| t.sel.coll.clone()).collect::<Vec<_>>();
    assert_eq!(colls(ui.app()), ["customers", "orders", "events"]);
    menu_of(&mut ui, "Tab customers");
    ui.click_contains("Move right");
    assert_eq!(colls(ui.app()), ["orders", "customers", "events"]);
    menu_of(&mut ui, "Tab customers");
    ui.show_contains("Move left");
    let w = ui.h.get_by_label_contains("Move left").rect().width();
    assert!(w < 320.0, "the tab menu is {w} px wide");
    ui.click_contains("Move left");
    assert_eq!(colls(ui.app()), ["customers", "orders", "events"]);
}

#[test]
fn a_group_member_moves_from_its_menu() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_demo();
        a.demo_conns("edit");
    }) else {
        return;
    };
    let form = |a: &App| match &a.modal {
        Some(Modal::GroupEdit(f)) => Some(f.members.clone()),
        _ => None,
    };
    assert_eq!(form(ui.app()), Some(vec![11, 12, 13]));
    menu_of(&mut ui, "Member billing");
    ui.click_contains("Move up");
    assert_eq!(form(ui.app()), Some(vec![11, 13, 12]));
}
