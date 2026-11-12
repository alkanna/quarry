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
