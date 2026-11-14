//! Update matching and Delete all matching rows of the SQL Content view, on every SQL server whose environment variable is
//! set. Each flow reads the table back from the server, so a WHERE that reached past its parentheses would show.

use super::sql_edit::{ORIGINAL, every_engine, open_unlocked};
use super::*;

/// Puts `filter` in the Content bar and waits for the page it loads.
fn filter_to(ui: &mut UiApp, filter: &str) {
    let t = ui.h.state_mut().tm().unwrap();
    (t.filter_txt, t.page) = (filter.into(), 0);
    ui.h.state_mut().reload();
    ui.quiet();
}

/// Waits for the dialog's count, then checks its Confirm waits for the table name before it types it.
fn confirm(ui: &mut UiApp, table: &str, button: &str) {
    ui.wait("the matching rows to be counted", |a| matches!(&a.modal, Some(Modal::SqlBulk(f)) if f.prep.is_some()));
    ui.show(button);
    assert!(ui.h.get_by_label(button).accesskit_node().is_disabled(), "{button} waits for the table name");
    ui.fill("Confirmation", table);
    ui.click(button);
}

fn undo_last(ui: &mut UiApp) {
    ui.click("Undo (1)");
    ui.click("Undo change 1");
    ui.click("Close");
}

#[test]
fn delete_matching_rows_reaches_only_the_or_filter_and_undoes() {
    every_engine("delete matching rows", |fx, ui| {
        let t = open_unlocked(fx, ui);
        filter_to(ui, "qty > 25 OR name = 'alpha'");
        // The page that loaded is in the table's history.
        let key = ui.app().t().unwrap().coll_key();
        assert_eq!(ui.app().prefs.history.get(&key).and_then(|h| h.selector.first()).map(String::as_str), Some("qty > 25 OR name = 'alpha'"));
        ui.h.state_mut().open_bulk(false);
        confirm(ui, &t, "Delete 2 rows");
        fx.expect(ui, &t, "2:beta:20");
        ui.quiet();
        undo_last(ui);
        fx.expect(ui, &t, ORIGINAL);
    });
}

#[test]
fn update_matching_rows_sets_the_value_and_undoes() {
    every_engine("update matching rows", |fx, ui| {
        let t = open_unlocked(fx, ui);
        filter_to(ui, "id = 1 OR id = 3");
        ui.h.state_mut().open_bulk(true);
        ui.wait("the matching rows to be counted", |a| matches!(&a.modal, Some(Modal::SqlBulk(f)) if f.prep.is_some()));
        ui.fill("Value of name", "zeta");
        confirm(ui, &t, "Update 2 rows");
        fx.expect(ui, &t, "1:zeta:10,2:beta:20,3:zeta:30");
        ui.quiet();
        undo_last(ui);
        fx.expect(ui, &t, ORIGINAL);
    });
}

#[test]
fn a_filter_with_a_second_statement_writes_nothing() {
    every_engine("a filter with a second statement", |fx, ui| {
        let t = open_unlocked(fx, ui);
        let q = fx.qualified(&t);
        filter_to(ui, &format!("1 = 1); DELETE FROM {q}; --"));
        ui.h.state_mut().open_bulk(false);
        ui.wait("the dialog to refuse the filter", |a| a.modal.is_none());
        fx.expect_unchanged(ui, &t, ORIGINAL);
    });
}
