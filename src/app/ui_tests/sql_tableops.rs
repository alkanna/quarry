//! The Rename, Duplicate and PostgreSQL Empty table dialogs, opened from the sidebar menu of a table, offline on the demo data. They
//! check the statement each dialog previews and what arms its button, since the server's answer is out of reach without a connection.

use super::sql_structure::replace;
use super::*;
use db::sql::create::DropInfo;

/// A right click on the sidebar row named `label`. The tab strip may carry the same name, and the sidebar is the leftmost.
pub(super) fn right_click_row(ui: &mut UiApp, label: &str) {
    ui.show(label);
    let at = ui.h.get_all_by_label(label).map(|n| n.rect()).min_by(|a, b| a.left().total_cmp(&b.left())).expect("a sidebar row").center();
    ui.press_at(at, egui::PointerButton::Secondary, 1);
    ui.settle();
}

#[test]
fn a_postgres_rename_previews_its_statement_and_arms_only_on_a_new_free_name() {
    let Some(mut ui) = demo_ui(App::load_pg_demo) else { return };
    right_click_row(&mut ui, "products");
    ui.click("Rename…");
    assert!(matches!(ui.app().modal, Some(Modal::SqlRename(_))), "the menu item did not open the rename dialog");
    ui.show("New name");
    assert!(ui.disabled("Rename"), "Rename is armed before the name changed");
    replace(&mut ui, "New name", "items");
    assert!(ui.shows(r#"ALTER TABLE "public"."products" RENAME TO "items""#), "the preview does not hold the rename");
    assert!(!ui.disabled("Rename"), "a new free name did not arm Rename");
    // customers is a table of the schema, so the name is taken.
    replace(&mut ui, "New name", "customers");
    assert!(ui.shows("customers already exists here."), "no note says the name is taken");
    assert!(ui.disabled("Rename"), "a taken name arms Rename");
    // The dependents note tells what a rename leaves behind.
    assert!(ui.shows("Routines, trigger functions and queries that write products are not updated"));
    ui.click("Cancel");
    assert!(ui.app().modal.is_none(), "Cancel left the dialog open");
}

#[test]
fn a_rename_waits_for_staged_changes_in_a_tab_on_the_table() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    {
        let app = ui.h.state_mut();
        let s = app.tabs[0].sql.as_mut().expect("the orders tab");
        s.st.staged.push(db::sql::ddl::Change::Drop("note".into()));
    }
    right_click_row(&mut ui, "orders");
    ui.click("Rename…");
    ui.show("New name");
    replace(&mut ui, "New name", "orders_old");
    assert!(ui.shows("A tab on it holds staged changes."), "the dialog does not warn about the staged change");
    assert!(ui.disabled("Rename"), "Rename is armed with a staged change in a tab on the table");
}

#[test]
fn a_mysql_rename_and_duplicate_preview_their_statements() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    right_click_row(&mut ui, "products");
    ui.click("Rename…");
    ui.show("New name");
    replace(&mut ui, "New name", "items");
    assert!(ui.shows("RENAME TABLE `shop`.`products` TO `shop`.`items`;"), "the preview does not hold the rename");
    assert!(ui.shows("Foreign keys follow."), "a MySQL rename says its foreign keys follow");
    ui.click("Cancel");
    right_click_row(&mut ui, "products");
    ui.click("Duplicate…");
    assert!(matches!(ui.app().modal, Some(Modal::SqlDuplicate(_))), "the menu item did not open the duplicate dialog");
    assert!(ui.shows("CREATE TABLE `shop`.`products_copy` LIKE `shop`.`products`;"), "the preview does not hold the copy");
    assert!(ui.shows("INSERT INTO `shop`.`products_copy`"), "a copy with data previews no INSERT");
}

#[test]
fn a_postgres_duplicate_switches_between_structure_only_and_data() {
    let Some(mut ui) = demo_ui(App::load_pg_demo) else { return };
    right_click_row(&mut ui, "products");
    ui.click("Duplicate…");
    ui.show("Structure only");
    // The default copies the rows too, and says there is no undo for that.
    assert!(ui.shows(r#"CREATE TABLE "public"."products_copy" (LIKE "public"."products" INCLUDING ALL);"#));
    assert!(ui.shows("INSERT INTO"), "the default copy previews no INSERT");
    assert!(ui.shows("A copy with rows has no undo"));
    ui.click("Structure only");
    assert!(!ui.shows("INSERT INTO"), "structure only still previews the INSERT");
    assert!(!ui.shows("A copy with rows has no undo"), "structure only still warns about the missing undo");
    // A taken name blocks the button.
    let name = |ui: &UiApp| ui.h.get_by_role_and_label(egui::Role::TextInput, "New table name").rect().center();
    let at = name(&ui);
    ui.press_at(at, egui::PointerButton::Primary, 1);
    ui.h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    ui.type_text("customers");
    assert!(ui.shows("customers already exists here."));
    assert!(ui.disabled("Duplicate"), "a taken name arms Duplicate");
}

/// The counts of an empty table, as the dialog gets them from the server.
fn answer(ui: &mut UiApp, info: DropInfo) {
    let app = ui.h.state_mut();
    let t = &app.tabs[0];
    let (conn, db, schema) = (t.sel.conn, t.sel.db.clone(), t.sel.schema.clone());
    app.truncate_info_done(conn, &db, &schema, "products", &Ok(info));
    ui.settle();
}

#[test]
fn emptying_a_postgres_table_needs_the_counts_and_the_typed_name() {
    let Some(mut ui) = demo_ui(App::load_pg_demo) else { return };
    right_click_row(&mut ui, "products");
    ui.click("Empty table…");
    assert!(matches!(ui.app().modal, Some(Modal::PgTruncate(_))), "the menu item did not open the empty dialog");
    ui.show("Confirmation");
    assert!(ui.shows("Counting rows"), "the dialog does not say the counts are on their way");
    assert!(ui.shows(r#"TRUNCATE TABLE "public"."products";"#), "the preview does not hold the statement");
    // Typed in full, the button still waits for what the table reaches.
    ui.fill("Confirmation", "products");
    assert!(ui.disabled("Empty table"), "the button armed before the counts arrived");
    answer(&mut ui, DropInfo { exists: true, rows: Some(2318), ..Default::default() });
    assert!(ui.shows("This deletes every row of analytics.public.products (2,318 rows)"), "the lead does not give the row count");
    assert!(ui.shows("This connection is tagged Production."), "the tag banner is missing");
    assert!(!ui.disabled("Empty table"), "the exact name and the counts did not arm the button");
    replace(&mut ui, "Confirmation", "product");
    assert!(ui.disabled("Empty table"), "a name that is one letter short armed the button");
    replace(&mut ui, "Confirmation", "Products");
    assert!(ui.disabled("Empty table"), "the check ignores case");
    replace(&mut ui, "Confirmation", "products");
    ui.click("Restart the sequences of its columns (RESTART IDENTITY)");
    assert!(ui.shows(r#"TRUNCATE TABLE "public"."products" RESTART IDENTITY;"#), "the option is not in the preview");
    ui.click("Empty table");
    assert!(ui.app().modal.is_none(), "the confirmed dialog stayed open");
}

#[test]
fn emptying_a_table_that_others_point_at_waits_for_cascade() {
    let Some(mut ui) = demo_ui(App::load_pg_demo) else { return };
    right_click_row(&mut ui, "products");
    ui.click("Empty table…");
    ui.show("Confirmation");
    answer(&mut ui, DropInfo { exists: true, rows: Some(10), referenced_by: vec!["order_items".into()], ..Default::default() });
    assert!(ui.shows("1 table points at it with a foreign key: order_items."));
    ui.fill("Confirmation", "products");
    assert!(ui.disabled("Empty table"), "PostgreSQL refuses this without CASCADE, so the button must wait");
    ui.click("Empty them too (CASCADE)");
    assert!(ui.shows(r#"TRUNCATE TABLE "public"."products" CASCADE;"#), "CASCADE is not in the preview");
    assert!(ui.shows("CASCADE deletes every row of each table listed"));
    assert!(!ui.disabled("Empty table"), "ticking CASCADE did not arm the button");
}

#[test]
fn a_table_that_is_gone_cannot_be_emptied() {
    let Some(mut ui) = demo_ui(App::load_pg_demo) else { return };
    right_click_row(&mut ui, "products");
    ui.click("Empty table…");
    ui.show("Confirmation");
    answer(&mut ui, DropInfo { exists: false, ..Default::default() });
    assert!(ui.shows("The table products is not on analytics-prod any more."));
    ui.fill("Confirmation", "products");
    assert!(ui.disabled("Empty table"));
}

#[test]
fn a_locked_postgres_connection_asks_to_unlock_before_each_table_dialog() {
    for item in ["Rename…", "Duplicate…", "Empty table…"] {
        let Some(mut ui) = demo_ui(App::load_pg_demo) else { return };
        ui.lock();
        right_click_row(&mut ui, "products");
        ui.click(item);
        assert!(matches!(ui.app().modal, Some(Modal::Unlock(..))), "{item} did not ask to unlock");
    }
}
