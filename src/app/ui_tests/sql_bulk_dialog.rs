//! The Update matching and Delete all matching rows dialog, offline on the demo data: what it says before anything runs, the
//! statement it previews and what arms its button. The live tests in `sql_bulk.rs` run the writes against servers; here the count the
//! worker would send back is handed to the dialog the way its answer is.

use super::sql_structure::replace;
use super::*;
use crate::app::sql::bulk::BulkForm;
use db::sql::bulk::BulkPrep;

/// The dialog open on orders for the WHERE `filter`, with `count` rows counted on a table of `engine`.
fn open(ui: &mut UiApp, update: bool, filter: &str, count: u64, engine: &str) {
    let app = ui.h.state_mut();
    let sel = app.tabs[0].sel.clone();
    let s = app.tabs[0].sql.as_ref().expect("the orders tab");
    let prep = BulkPrep { count, cols: s.cols.to_vec(), pk: s.pk.clone(), engine: engine.into() };
    app.modal = Some(Modal::SqlBulk(Box::new(BulkForm { update, sel, ns: "shop".into(), filter: filter.into(), capped: false, req: 7, prep: None, pairs: Vec::new(), typed: String::new() })));
    app.on_bulk_prep(7, Ok(prep));
    ui.settle();
}

fn demo() -> Option<UiApp> {
    demo_ui(App::load_sql_demo)
}

#[test]
fn delete_matching_waits_for_the_count_and_the_typed_table_name() {
    let Some(mut ui) = demo() else { return };
    open(&mut ui, false, "status = 'paid'", 2, "InnoDB");
    ui.show("2 rows match");
    assert!(ui.shows("DELETE FROM `shop`.`orders`"), "the preview is not the DELETE");
    assert!(ui.shows("status = 'paid'"), "the preview does not hold the WHERE");
    assert!(ui.shows("Undo is available"), "no note says the rows can be taken back");
    assert!(!ui.shows("No WHERE."), "a filtered delete warns that it has no WHERE");
    assert!(ui.disabled("Delete 2 rows"), "the button armed before the name was typed");
    ui.fill("Confirmation", "orde");
    assert!(ui.disabled("Delete 2 rows"), "a name that is letters short armed the button");
    replace(&mut ui, "Confirmation", "ORDERS");
    assert!(ui.disabled("Delete 2 rows"), "the check ignores case");
    replace(&mut ui, "Confirmation", "orders");
    assert!(!ui.disabled("Delete 2 rows"), "the exact name did not arm the button");
    ui.click("Delete 2 rows");
    assert!(ui.app().modal.is_none(), "the confirmed dialog stayed open");
}

#[test]
fn an_empty_filter_warns_that_every_row_is_reached() {
    let Some(mut ui) = demo() else { return };
    open(&mut ui, false, "", 4410, "InnoDB");
    ui.show("No WHERE.");
    assert!(ui.shows("This deletes every row of orders."), "the banner does not say what a delete reaches");
    assert!(ui.shows("4,410 rows match"), "the count is not grouped");
    let Some(mut ui) = demo() else { return };
    open(&mut ui, true, "  ", 4410, "InnoDB");
    ui.show("No WHERE.");
    assert!(ui.shows("This changes every row of orders."), "the banner does not say what an update reaches");
}

#[test]
fn update_matching_takes_a_value_before_it_arms() {
    let Some(mut ui) = demo() else { return };
    open(&mut ui, true, "status = 'paid'", 2, "InnoDB");
    // The first column an update may set is picked, and the key never is: changing it would break undo.
    ui.show("Value of customer_id");
    assert!(ui.h.query_by_label("Value of id").is_none(), "the primary key is offered for an update");
    ui.fill("Confirmation", "orders");
    assert!(ui.disabled("Update 2 rows"), "an update with no value armed the button");
    ui.fill("Value of customer_id", "7");
    assert!(ui.shows("UPDATE `shop`.`orders`"), "the preview is not the UPDATE");
    assert!(ui.shows("`customer_id` = 7"), "the preview does not set the column");
    assert!(ui.shows("status = 'paid'"), "the preview does not hold the WHERE");
    assert!(!ui.disabled("Update 2 rows"), "a value and the name did not arm the button");
    // A value the column cannot hold is refused where it is typed.
    replace(&mut ui, "Value of customer_id", "seven");
    assert!(ui.disabled("Update 2 rows"), "a value that does not parse armed the button");
}

#[test]
fn nothing_matching_leaves_nothing_to_confirm() {
    let Some(mut ui) = demo() else { return };
    open(&mut ui, false, "status = 'gone'", 0, "InnoDB");
    ui.show("0 rows match");
    ui.fill("Confirmation", "orders");
    assert!(ui.disabled("Delete 0 rows"), "a delete of nothing armed the button");
    assert!(!ui.shows("Undo is available"), "an undo is promised for no rows");
}

#[test]
fn the_banners_say_when_a_write_cannot_roll_back_or_be_undone() {
    let Some(mut ui) = demo() else { return };
    open(&mut ui, false, "status = 'paid'", 2, "MyISAM");
    ui.show("No rollback.");
    let Some(mut ui) = demo() else { return };
    open(&mut ui, false, "status = 'paid'", 2, "InnoDB");
    ui.show("2 rows match");
    assert!(!ui.shows("No rollback."), "a transactional engine is said to have no rollback");
    let Some(mut ui) = demo() else { return };
    open(&mut ui, false, "status = 'paid'", db::sql::import::UNDO_MAX_ROWS + 1, "InnoDB");
    ui.show("No undo.");
    assert!(!ui.shows("Undo is available"), "a delete too big to keep is said to be undoable");
    let Some(mut ui) = demo() else { return };
    open(&mut ui, false, "status = 'paid'", 2, "InnoDB");
    if let Some(Modal::SqlBulk(f)) = &mut ui.h.state_mut().modal {
        f.capped = true;
    }
    ui.show("LIMIT and OFFSET only cap what the grid shows. Every row the WHERE matches is changed.");
}

#[test]
fn cancel_closes_the_dialog_without_acting() {
    let Some(mut ui) = demo() else { return };
    open(&mut ui, false, "status = 'paid'", 2, "InnoDB");
    ui.fill("Confirmation", "orders");
    ui.click("Cancel");
    assert!(ui.app().modal.is_none(), "Cancel left the dialog open");
}

#[test]
fn opening_it_is_refused_on_grouped_rows_and_asks_to_unlock_when_locked() {
    let Some(mut ui) = demo() else { return };
    ui.h.state_mut().status = "idle".into();
    ui.h.state_mut().open_bulk(false);
    assert!(!matches!(ui.app().modal, Some(Modal::SqlBulk(_))), "the offline demo has no server to count on");
    assert!(ui.app().status.contains("Offline demo"), "nothing says why nothing opened: {}", ui.app().status);
    ui.lock();
    ui.h.state_mut().open_bulk(true);
    assert!(matches!(ui.app().modal, Some(Modal::Unlock(..))), "a locked connection did not ask to unlock");
    let Some(mut ui) = demo() else { return };
    ui.h.state_mut().tabs[0].sql.as_mut().expect("the orders tab").matched = Some(148);
    ui.h.state_mut().open_bulk(false);
    assert!(ui.app().modal.is_none(), "grouped rows opened the dialog");
    assert!(ui.app().last_problem().contains("Grouped rows are summaries"), "no notice says why: {}", ui.app().last_problem());
}
