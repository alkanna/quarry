//! Editing a SQL table's structure through the UI, on every SQL server whose environment variable is set: MySQL,
//! MariaDB, PostgreSQL 18 and 13. Each flow stages changes in the Structure view, applies them, reads the server's own
//! catalog to see what happened, and takes them back through the undo list.

use super::sql_edit::{every_engine, open_unlocked, Fx};
use super::*;

/// The column names in table order, joined with commas.
fn names(fx: &Fx, table: &str) -> String {
    if fx.is_pg() {
        fx.one(&format!("SELECT string_agg(attname::text, ',' ORDER BY attnum) FROM pg_attribute WHERE attrelid = 'public.{table}'::regclass AND attnum > 0 AND NOT attisdropped"))
    } else {
        fx.one(&format!("SELECT GROUP_CONCAT(COLUMN_NAME ORDER BY ORDINAL_POSITION SEPARATOR ',') FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = 'quarry_test' AND TABLE_NAME = '{table}'"))
    }
}

/// The type of one column as the server writes it.
fn ctype(fx: &Fx, table: &str, col: &str) -> String {
    if fx.is_pg() {
        fx.one(&format!("SELECT format_type(atttypid, atttypmod) FROM pg_attribute WHERE attrelid = 'public.{table}'::regclass AND attname = '{col}' AND NOT attisdropped"))
    } else {
        fx.one(&format!("SELECT COLUMN_TYPE FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = 'quarry_test' AND TABLE_NAME = '{table}' AND COLUMN_NAME = '{col}'"))
    }
}

fn has_index(fx: &Fx, table: &str, name: &str) -> bool {
    let n = if fx.is_pg() {
        fx.one(&format!("SELECT count(*) FROM pg_indexes WHERE schemaname = 'public' AND tablename = '{table}' AND indexname = '{name}'"))
    } else {
        fx.one(&format!("SELECT COUNT(*) FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = 'quarry_test' AND TABLE_NAME = '{table}' AND INDEX_NAME = '{name}'"))
    };
    n.trim() != "0"
}

/// Steps the UI until `cond` holds: a structure change runs on a worker task and lands a few frames after the click.
pub(super) fn until(fx: &Fx, ui: &mut UiApp, what: &str, cond: impl Fn(&Fx) -> bool) {
    ui.wait(what, |_| cond(fx));
}

/// Replaces the text of a field by its accessible name.
pub(super) fn replace(ui: &mut UiApp, field: &str, text: &str) {
    ui.show(field);
    ui.h.get_by_label(field).focus();
    ui.h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    ui.type_text(text);
}

fn open_structure(ui: &mut UiApp) {
    ui.click("Structure");
    ui.wait("the structure", |a| a.t().is_some_and(|t| t.view == View::Structure) && a.structure().is_some());
}

/// Adds a column called `name` with the dialog's default type.
fn stage_new_column(ui: &mut UiApp, name: &str) {
    ui.click("+ Column");
    replace(ui, "Column name", name);
    ui.click("Stage change");
}

/// Takes back the newest change through the toolbar button. An undo that converts a type back asks for the table name.
pub(super) fn undo_last(ui: &mut UiApp, count: usize, table: &str) {
    ui.click(&format!("Undo ({count})"));
    ui.click("Undo change 1");
    if matches!(ui.app().modal, Some(Modal::Typed { .. })) {
        ui.fill("Confirmation", table);
        ui.click("Confirm");
    }
}

#[test]
fn staged_structure_changes_apply_and_undo() {
    every_engine("staged structure changes apply and undo", |fx, ui| {
        let t = open_unlocked(fx, ui);
        let (cols, qty) = (names(fx, &t), ctype(fx, &t, "qty"));
        assert_eq!(cols, "id,name,qty");
        open_structure(ui);
        // a new column, a renamed one, a retyped one and a new index in one go
        stage_new_column(ui, "extra");
        ui.click("columns row 2");
        ui.click("Edit");
        replace(ui, "Column name", "title");
        ui.click("Stage change");
        ui.click("columns row 3");
        ui.click("Edit");
        if fx.is_pg() {
            replace(ui, "Column type", "bigint");
        } else {
            ui.click("Column type");
            ui.click("BIGINT");
        }
        ui.click("Stage change");
        ui.click("+ Index");
        ui.click("Add a column");
        ui.click_contains("title  ");
        ui.click("Stage change");
        assert_eq!(ui.app().staged().len(), 4, "four changes are staged");
        assert_eq!(names(fx, &t), cols, "nothing runs before Apply");
        ui.click("Apply");
        until(fx, ui, "the changes to land", |fx| names(fx, &t) == "id,title,qty,extra" && has_index(fx, &t, "idx_title"));
        assert!(ctype(fx, &t, "qty").starts_with("bigint"), "qty is {}", ctype(fx, &t, "qty"));
        // the answer lands a frame after the catalog shows the change, and clears what was staged
        ui.wait("the staged changes to clear", |a| a.staged().is_empty());
        undo_last(ui, 1, &t);
        until(fx, ui, "the undo to land", |fx| names(fx, &t) == cols && !has_index(fx, &t, "idx_title"));
        assert_eq!(ctype(fx, &t, "qty"), qty, "the type is back");
        assert_eq!(fx.rows(&t), "1:alpha:10,2:beta:20,3:gamma:30", "the rows were never touched");
    });
}

#[test]
fn a_locked_connection_asks_to_unlock_before_applying() {
    every_engine("a locked connection asks to unlock before applying", |fx, ui| {
        let t = fx.table();
        fx.open(ui, &t);
        open_structure(ui);
        // locked, nothing can be staged
        assert!(ui.h.get_by_label("+ Column").accesskit_node().is_disabled(), "+ Column works on a locked connection");
        ui.unlock();
        stage_new_column(ui, "extra");
        assert_eq!(ui.app().staged().len(), 1);
        // locked again with a change staged: Apply asks, and staying read-only runs nothing
        ui.h.state_mut().conns.iter_mut().find(|c| c.meta.id == CONN).unwrap().writable = false;
        ui.click("Apply");
        ui.show("Stay read-only");
        ui.click("Stay read-only");
        ui.h.run_steps(8);
        assert_eq!(names(fx, &t), "id,name,qty", "the server changed");
        assert_eq!(ui.app().staged().len(), 1, "the staged change was lost");
        // allowing writes runs it
        ui.click("Apply");
        ui.click("Allow writes");
        until(fx, ui, "the column to land", |fx| names(fx, &t) == "id,name,qty,extra");
    });
}

#[test]
fn a_default_that_carries_a_second_statement_cannot_be_staged() {
    every_engine("a default that carries a second statement cannot be staged", |fx, ui| {
        let t = open_unlocked(fx, ui);
        open_structure(ui);
        ui.click("+ Column");
        replace(ui, "Column name", "extra");
        ui.click("Expression");
        ui.fill("Column default", &format!("1); DROP TABLE {t}; --"));
        assert!(ui.h.get_by_label("Stage change").accesskit_node().is_disabled(), "the statement was accepted");
        assert!(ui.h.get_by_label("Apply now").accesskit_node().is_disabled(), "the statement can run");
        ui.click("Cancel");
        assert!(ui.app().staged().is_empty());
        fx.expect_unchanged(ui, &t, "1:alpha:10,2:beta:20,3:gamma:30");
        assert_eq!(names(fx, &t), "id,name,qty");
    });
}

#[test]
fn apply_now_on_a_connection_locked_after_the_dialog_opened_asks_to_unlock() {
    every_engine("apply now asks to unlock", |fx, ui| {
        let t = open_unlocked(fx, ui);
        open_structure(ui);
        ui.click("+ Column");
        replace(ui, "Column name", "extra");
        // locked while the dialog is open: the button must still work, and ask, as the Apply bar does
        ui.h.state_mut().conns.iter_mut().find(|c| c.meta.id == CONN).unwrap().writable = false;
        ui.h.run_steps(2);
        assert!(!ui.h.get_by_label("Apply now").accesskit_node().is_disabled(), "Apply now is disabled on a locked connection");
        ui.click("Apply now");
        ui.show("Allow writes");
        assert_eq!(names(fx, &t), "id,name,qty", "nothing ran before the answer");
        ui.click("Allow writes");
        until(fx, ui, "the column to land", |fx| names(fx, &t) == "id,name,qty,extra");
    });
}
