//! Tabs open as in a browser: a click reuses the current tab unless work in it would be lost, and a new tab is asked for.

use super::*;

fn colls(a: &App) -> Vec<&str> {
    a.tabs.iter().map(|t| t.sel.coll.as_str()).collect()
}

#[test]
fn a_click_reuses_a_clean_tab_and_new_tabs_are_asked_for() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    assert_eq!(colls(ui.app()), ["customers", "orders", "events"]);
    // orders has a filter, so it stays and carts opens next to it.
    ui.click("carts");
    assert_eq!(colls(ui.app()), ["customers", "orders", "carts", "events"]);
    assert_eq!(ui.app().cur, 2);
    // carts holds nothing of the person's, so products takes its place and Reopen closed tab can bring it back.
    ui.click("products");
    assert_eq!(colls(ui.app()), ["customers", "orders", "products", "events"]);
    assert_eq!(ui.app().closed.first().map(|t| t.sel.coll.as_str()), Some("carts"));
    ui.h.get_by_label("reviews").click_secondary();
    ui.settle();
    ui.click("Open in new tab");
    assert_eq!(colls(ui.app()), ["customers", "orders", "products", "reviews", "events"]);
    assert_eq!(ui.app().cur, 3);
    // Cmd+click keeps reviews too.
    ui.h.get_by_label("shipments").click_modifiers(egui::Modifiers::COMMAND);
    ui.settle();
    assert_eq!(colls(ui.app()), ["customers", "orders", "products", "reviews", "shipments", "events"]);
    // Cmd+T asks the palette for a new tab.
    ui.h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::T);
    ui.settle();
    assert!(matches!(ui.app().modal, Some(Modal::Palette { new_tab: true, .. })));
    // Its first row is an open tab, which is switched to rather than opened twice.
    ui.h.key_press(egui::Key::Enter);
    ui.settle();
    assert!(ui.app().modal.is_none());
    assert_eq!(colls(ui.app()).len(), 6, "an open tab was opened again");
}

#[test]
fn the_memory_tooltip_moves_a_closed_tabs_rows_to_the_reopen_line() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    let part = |a: &App, what: &str| a.mem_parts().iter().find(|p| p.0 == what).map_or(0, |p| p.1);
    let a = ui.h.state_mut();
    let mut t = Tab::new(a.tabs[0].sel.clone());
    let pad = "x".repeat(1000);
    t.docs = (0..100).filter_map(|i| db::Row::from_doc(&mongodb::bson::doc! { "_id": i, "pad": &pad }).ok()).collect();
    let bytes = t.mem();
    assert!(bytes > 100_000);
    a.tabs.push(t);
    let (open, closed) = (part(a, "Other open tabs"), part(a, "Closed tabs kept to reopen"));
    let last = a.tabs.len() - 1;
    a.close_tab(last);
    assert_eq!(part(a, "Other open tabs"), open - bytes);
    assert_eq!(part(a, "Closed tabs kept to reopen"), closed + bytes);
}

/// The point `frac` of the way across the tab named `coll`, at its middle height.
fn across_tab(ui: &UiApp, coll: &str, frac: f32) -> egui::Pos2 {
    let r = ui.h.get_by_label(&format!("Tab {coll}")).rect();
    egui::pos2(r.left() + r.width() * frac, r.center().y)
}

#[test]
fn a_tab_moves_by_dragging_it_and_stays_current() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    assert_eq!(colls(ui.app()), ["customers", "orders", "events"]);
    ui.mouse_hold("Tab customers", 0);
    assert_eq!(ui.app().cur, 0, "a click did not pick the tab");
    let (from, to) = (across_tab(&ui, "customers", 0.5), across_tab(&ui, "events", 0.8));
    ui.press_and_move(from, to);
    ui.release(to);
    assert_eq!(colls(ui.app()), ["orders", "events", "customers"], "the tab did not land after the one it was dropped on");
    assert_eq!(ui.app().cur, 2, "the dragged tab is no longer the current one");
    // A click on another tab still picks it, and Esc cancels a drag.
    ui.mouse_hold("Tab orders", 0);
    assert_eq!(ui.app().cur, 0);
    let (from, to) = (across_tab(&ui, "orders", 0.5), across_tab(&ui, "customers", 0.8));
    ui.press_and_move(from, to);
    ui.h.key_press(egui::Key::Escape);
    ui.h.step();
    ui.release(to);
    assert_eq!(colls(ui.app()), ["orders", "events", "customers"], "Esc did not cancel the tab drag");
}
