//! The New database and New collection dialog of a MongoDB connection, offline on the demo: how it opens from the sidebar and
//! the palette, what keeps Create off and says why, and how the collection kind hides the options it cannot take.

use super::sql_structure::replace;
use super::*;

fn create_off(ui: &UiApp) -> bool {
    // The last match is the topmost one: the dialog is drawn after the page.
    ui.h.query_all_by_label("Create").last().expect("no Create button").accesskit_node().is_disabled()
}

fn said(ui: &UiApp, part: &str) -> bool {
    ui.h.query_by_label_contains(part).is_some()
}

fn form(ui: &UiApp) -> &crate::app::mongo_create::MongoNewColl {
    match &ui.app().modal {
        Some(Modal::MongoNewColl(f)) => f,
        _ => panic!("the New database and collection dialog is not open"),
    }
}

#[test]
fn the_plus_button_opens_new_database_and_names_are_checked_before_create() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    ui.click("New database");
    assert!(form(&ui).new_db, "the plus button did not open the New database form");
    ui.show("Collection name");
    assert!(create_off(&ui), "Create is on with no name");
    // An empty form says nothing yet: a message on a form just opened is noise.
    assert!(!said(&ui, "Name the database"));

    replace(&mut ui, "Database name", "my shop");
    replace(&mut ui, "Collection name", "daily");
    assert!(create_off(&ui) && said(&ui, "cannot contain a space"), "a database name with a space was accepted");
    replace(&mut ui, "Database name", "admin");
    assert!(create_off(&ui) && said(&ui, "reserved database"));
    // Names that differ only by case collide on disk, so the existing shop database blocks SHOP.
    replace(&mut ui, "Database name", "SHOP");
    assert!(create_off(&ui) && said(&ui, "differs only by case"));
    replace(&mut ui, "Database name", "reports");
    assert!(!create_off(&ui), "a valid database and collection still leave Create off");
    assert!(!said(&ui, "cannot contain"));

    replace(&mut ui, "Collection name", "system.daily");
    assert!(create_off(&ui) && said(&ui, "reserved"), "a system. collection name was accepted");
    assert_eq!(form(&ui).db, "reports");
    ui.click("Cancel");
    assert!(ui.app().modal.is_none(), "Cancel left the dialog open");
}

#[test]
fn a_locked_connection_asks_to_unlock_before_the_new_database_form() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    for c in &mut ui.h.state_mut().conns {
        c.writable = false;
    }
    ui.click("New database");
    assert!(matches!(ui.app().modal, Some(Modal::Unlock(..))), "a locked connection opened the form or did nothing");
}

#[test]
fn the_palette_has_new_database_for_mongo_and_not_for_sql() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    assert!(ui.app().command_hints().iter().any(|(n, _)| n.starts_with("New database")));
    ui.h.state_mut().open_palette_with(">New database".into());
    ui.settle();
    ui.key(egui::Modifiers::NONE, egui::Key::Enter);
    assert!(matches!(&ui.app().modal, Some(Modal::MongoNewColl(f)) if f.new_db), "the palette command did not open the New database form");

    let Some(sql) = demo_ui(App::load_sql_demo) else { return };
    assert!(!sql.app().command_hints().iter().any(|(n, _)| n.starts_with("New database")), "SQL connections have their own New database flow");
}

#[test]
fn the_collection_kind_hides_the_options_it_cannot_take() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    ui.h.state_mut().mongo_new_coll(1, "shop".into());
    ui.settle();
    assert!(!form(&ui).new_db);
    replace(&mut ui, "Collection name", "invoices");
    assert!(!create_off(&ui));
    ui.click("Options");
    ui.show("Use a collation");
    ui.show("Validate documents");

    // A time series collection takes neither a collation nor a validator, so the form stops offering them.
    ui.click("Time series");
    ui.show("Time field");
    assert!(ui.h.query_all_by_label("Use a collation").next().is_none(), "a time series offers a collation");
    assert!(ui.h.query_all_by_label("Validate documents").next().is_none(), "a time series offers a validator");
    assert!(create_off(&ui), "a time series without its time field can be created");

    ui.click("Capped");
    ui.show("Size in bytes");
    ui.show("Use a collation");
    assert!(ui.h.query_all_by_label("Time field").next().is_none(), "the time series row stayed after the kind changed");
    assert!(create_off(&ui), "a capped collection without a size can be created");

    ui.click("Standard");
    assert!(!create_off(&ui), "back on Standard the form should be valid again");
}

#[test]
fn a_database_menu_opens_new_collection_for_that_database() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    ui.show("shop");
    let at = ui.h.get_by_label("shop").rect().center();
    ui.press_at(at, egui::PointerButton::Secondary, 1);
    ui.settle();
    ui.click("New collection\u{2026}");
    assert!(matches!(&ui.app().modal, Some(Modal::MongoNewColl(f)) if !f.new_db && f.db == "shop"), "the database menu did not open New collection for shop");
    replace(&mut ui, "Collection name", "carts");
    assert!(create_off(&ui) && said(&ui, "already exists in shop"), "a collection that exists was accepted");
}
