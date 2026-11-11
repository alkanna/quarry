//! Editing a SQL table's foreign keys through the Relations view, on every SQL server whose environment variable is set:
//! MySQL, MariaDB, PostgreSQL 18 and 13. Each flow fills the Add relation dialog like a person does, applies it, reads the
//! server's own catalog to see the key, and takes it back through the undo list.

use super::sql_edit::{every_engine, open_unlocked, Fx};
use super::sql_structure::{undo_last, until};
use super::*;

/// The names of the foreign keys of a table, joined with commas.
fn keys(fx: &Fx, table: &str) -> String {
    if fx.is_pg() {
        fx.one(&format!("SELECT COALESCE(string_agg(conname::text, ',' ORDER BY conname), '') FROM pg_constraint WHERE contype = 'f' AND conrelid = 'public.{table}'::regclass"))
    } else {
        fx.one(&format!("SELECT COALESCE(GROUP_CONCAT(CONSTRAINT_NAME ORDER BY CONSTRAINT_NAME SEPARATOR ','), '') FROM information_schema.TABLE_CONSTRAINTS WHERE CONSTRAINT_TYPE = 'FOREIGN KEY' AND TABLE_SCHEMA = 'quarry_test' AND TABLE_NAME = '{table}'"))
    }
}

/// Two fixture tables, the child made first so it is dropped first. Both hold ids 1 to 3.
fn pair(fx: &mut Fx) -> (String, String) {
    let child = fx.table();
    let parent = fx.table();
    (child, parent)
}

fn open_relations(ui: &mut UiApp) {
    ui.click("Relations");
    ui.wait("the relations", |a| a.t().is_some_and(|t| t.view == View::Relations) && a.structure().is_some());
}

/// Steps until a widget whose name contains `part` is on screen.
fn see(ui: &mut UiApp, part: &str) {
    let t = std::time::Instant::now();
    while ui.h.query_all_by_label_contains(part).next().is_none() {
        assert!(t.elapsed() < LIMIT, "timed out waiting for \"{part}\" on screen; last error: {}", ui.app().last_problem());
        ui.h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Selects a row of a Relations grid by clicking its Name cell. The middle of the row is the link to the other table, which a
/// click there would follow.
fn select_row(ui: &mut UiApp, row: &str) {
    ui.show(row);
    let r = ui.h.get_by_label(Box::leak(row.to_string().into_boxed_str())).rect();
    let at = egui::pos2(r.left() + 80.0, r.center().y);
    ui.h.hover_at(at);
    ui.h.step();
    let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    ui.h.input_mut().events.extend([press(true), press(false)]);
    ui.h.step();
    ui.settle();
}

/// Clicks Apply now once it is enabled, which it is when the SQL is there and the dialog's own count of the rows with no match
/// has answered. The space of that answer is reserved, so the button does not move under the click.
fn apply_now(ui: &mut UiApp) {
    ui.show("Apply now");
    let t = std::time::Instant::now();
    while ui.h.get_by_label("Apply now").accesskit_node().is_disabled() {
        assert!(t.elapsed() < LIMIT, "Apply now stayed disabled: {}", ui.app().last_problem());
        ui.h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    ui.click("Apply now");
}

/// Opens the dialog and points the first column at `column` of `parent`, leaving the first column of the child as it is.
fn point_at(ui: &mut UiApp, parent: &str, column: &str) {
    ui.click("+ Relation");
    ui.click("Referenced table");
    ui.click_last(parent);
    ui.click("Referenced column 1");
    ui.click_last(column);
}

#[test]
fn a_relation_is_added_through_the_dialog_and_taken_back() {
    every_engine("a relation is added through the dialog and taken back", |fx, ui| {
        let (child, parent) = pair(fx);
        fx.open(ui, &child);
        ui.unlock();
        assert_eq!(keys(fx, &child), "");
        open_relations(ui);
        point_at(ui, &parent, "id");
        // the statement shown is the one that runs, and the rows already there have their match
        see(ui, "Every existing row has a match");
        apply_now(ui);
        let name = format!("fk_{child}_id");
        until(fx, ui, "the key to land", |fx| keys(fx, &child) == name);
        ui.wait("the staged changes to clear", |a| a.staged().is_empty());
        // the Relations view lists it from the server
        ui.wait("the key to be listed", |a| a.structure().is_some_and(|s| s.foreign_keys.iter().any(|k| k.name == name)));
        undo_last(ui, 1, &child);
        until(fx, ui, "the undo to land", |fx| keys(fx, &child).is_empty());
        assert_eq!(fx.rows(&child), "1:alpha:10,2:beta:20,3:gamma:30", "the rows were never touched");
    });
}

#[test]
fn rows_that_point_at_nothing_are_shown_and_the_key_is_not_added() {
    every_engine("rows that point at nothing are shown", |fx, ui| {
        let (child, parent) = pair(fx);
        fx.open(ui, &child);
        ui.unlock();
        open_relations(ui);
        // qty holds 10, 20 and 30, and the parent has ids 1 to 3
        ui.click("+ Relation");
        ui.click("Column 1");
        ui.click_last("qty");
        ui.click("Referenced table");
        ui.click_last(&parent);
        ui.click("Referenced column 1");
        ui.click_last("id");
        see(ui, "3 rows have values in qty missing from");
        if fx.is_pg() {
            // PostgreSQL's check names the rows and offers NOT VALID
            see(ui, "For example (");
            see(ui, "add the key as NOT VALID");
        }
        apply_now(ui);
        ui.wait("the server to refuse the key", |a| !a.errors.is_empty());
        assert_eq!(keys(fx, &child), "", "the key was added over rows that point at nothing");
        if fx.is_pg() {
            assert!(ui.app().last_problem().contains("3 rows of"), "{}", ui.app().last_problem());
        }
        assert_eq!(fx.rows(&child), "1:alpha:10,2:beta:20,3:gamma:30");
    });
}

#[test]
fn the_buttons_stay_put_and_apply_waits_while_the_rows_are_counted() {
    every_engine("the buttons stay put while the rows are counted", |fx, ui| {
        let (child, parent) = pair(fx);
        fx.open(ui, &child);
        ui.unlock();
        open_relations(ui);
        ui.click("+ Relation");
        ui.click("Column 1");
        ui.click_last("qty");
        ui.click("Referenced table");
        ui.click_last(&parent);
        ui.click("Referenced column 1");
        ui.click_last("id");
        // the answer is a box of a few lines, and Apply now is there to be clicked
        see(ui, "3 rows have values in qty missing from");
        ui.h.run_steps(2);
        let answered = ui.h.get_by_label("Apply now").rect();
        assert!(!ui.h.get_by_label("Apply now").accesskit_node().is_disabled());
        // the count is asked again and not answered: the box gives way to a line, and the buttons neither move nor work yet,
        // since the count holds a read lock that a change of the table would meet as another session
        if let Some(Modal::FkEdit(f)) = ui.h.state_mut().modal.as_mut() {
            f.forget_orphan_answer();
        }
        see(ui, "Checking the existing rows");
        ui.h.run_steps(2);
        assert_eq!(ui.h.get_by_label("Apply now").rect(), answered, "the buttons moved when the answer went away");
        assert!(ui.h.get_by_label("Apply now").accesskit_node().is_disabled(), "Apply now works while the rows are being counted");
    });
}

#[test]
fn a_locked_connection_asks_to_unlock_before_a_relation_is_applied() {
    every_engine("a locked connection asks to unlock before a relation is applied", |fx, ui| {
        let (child, parent) = pair(fx);
        fx.open(ui, &child);
        ui.unlock();
        open_relations(ui);
        point_at(ui, &parent, "id");
        see(ui, "Every existing row has a match");
        // locked while the dialog is open: Apply now still works and asks
        ui.h.state_mut().conns.iter_mut().find(|c| c.meta.id == CONN).unwrap().writable = false;
        ui.h.run_steps(2);
        assert!(!ui.h.get_by_label("Apply now").accesskit_node().is_disabled(), "Apply now is disabled on a locked connection");
        apply_now(ui);
        ui.show("Stay read-only");
        ui.click("Stay read-only");
        ui.h.run_steps(8);
        assert_eq!(keys(fx, &child), "", "the key was added on a locked connection");
        assert_eq!(ui.app().staged().len(), 1, "the staged change was lost");
        // allowing writes runs it
        ui.click("Apply");
        ui.click("Allow writes");
        until(fx, ui, "the key to land", |fx| !keys(fx, &child).is_empty());
    });
}

#[test]
fn a_relation_can_point_into_another_schema() {
    every_engine("a relation can point into another schema", |fx, ui| {
        // only PostgreSQL has schemas inside a database
        if !fx.is_pg() {
            return;
        }
        // The popup lists every schema of the database and scrolls, and the click helpers scroll an item into view. A schema left
        // behind by a failed run is dropped by the next one.
        let other = "ui_rel_other".to_string();
        fx.sql(&format!("DROP SCHEMA IF EXISTS {other} CASCADE; CREATE SCHEMA {other}; CREATE TABLE {other}.q (id int PRIMARY KEY)"));
        fx.sql(&format!("INSERT INTO {other}.q VALUES (1), (2), (3)"));
        let child = open_unlocked(fx, ui);
        open_relations(ui);
        ui.click("+ Relation");
        ui.click("Referenced schema");
        ui.click_last(&other);
        ui.wait("the tables of the other schema", |a| a.conn(CONN).is_some_and(|c| c.tables_in("quarry_test", &other).is_some_and(|t| t.iter().any(|n| n == "q"))));
        ui.click("Referenced table");
        ui.click_last("q");
        ui.click("Referenced column 1");
        ui.click_last("id");
        see(ui, "Every existing row has a match");
        // PostgreSQL's own options are on the form, and deferrable is one of them
        ui.click("Deferrable");
        apply_now(ui);
        let name = format!("fk_{child}_id");
        until(fx, ui, "the key to land", |fx| keys(fx, &child) == name);
        assert_eq!(fx.one(&format!("SELECT confrelid::regclass::text || '|' || condeferrable FROM pg_constraint WHERE conname = '{name}'")), format!("{other}.q|true"));
        ui.wait("the staged changes to clear", |a| a.staged().is_empty());
        ui.wait("the key to be listed", |a| a.structure().is_some_and(|s| s.foreign_keys.iter().any(|k| k.name == name && k.ref_db == other)));
        undo_last(ui, 1, &child);
        until(fx, ui, "the undo to land", |fx| keys(fx, &child).is_empty());
        fx.sql(&format!("DROP SCHEMA {other} CASCADE"));
    });
}

#[test]
fn a_relation_is_changed_through_the_dialog_and_the_undo_brings_it_back() {
    every_engine("a relation is changed through the dialog", |fx, ui| {
        // changing a key is a drop and an add in one transaction, which only PostgreSQL does
        if !fx.is_pg() {
            return;
        }
        let (child, parent) = pair(fx);
        let name = format!("fk_{child}_id");
        fx.sql(&format!("ALTER TABLE {child} ADD CONSTRAINT {name} FOREIGN KEY (id) REFERENCES {parent} (id) DEFERRABLE"));
        fx.open(ui, &child);
        ui.unlock();
        open_relations(ui);
        ui.wait("the key to be listed", |a| a.structure().is_some_and(|s| s.foreign_keys.iter().any(|k| k.name == name)));
        let actions = |fx: &Fx| fx.one(&format!("SELECT confupdtype::text || confdeltype::text FROM pg_constraint WHERE conname = '{name}'"));
        assert_eq!(actions(fx), "aa");
        select_row(ui, "fk-out row 1");
        ui.click("Edit");
        // either Cascade will do: one is On update and one On delete, and the test does not rely on which comes first
        ui.click_nth("Cascade", 0);
        see(ui, "Change relation");
        // the referenced table is read again for the edit and its rows counted, and Apply now waits for both
        see(ui, "Every existing row has a match");
        apply_now(ui);
        let started = std::time::Instant::now();
        while actions(fx) == "aa" {
            ui.h.step();
            let asked = match &ui.app().modal {
                Some(Modal::Typed { title, .. }) => format!("a confirmation: {title}"),
                Some(_) => "a dialog".to_string(),
                None => "no dialog".to_string(),
            };
            assert!(started.elapsed() < LIMIT, "the change did not land: {asked}; staged {}; keys {}; {}", ui.app().staged().len(), keys(fx, &child), ui.app().last_problem());
            std::thread::sleep(Duration::from_millis(20));
        }
        // one of the two actions is CASCADE now, and the key came back whole: still deferrable, still on the same columns
        assert!(actions(fx).contains('c'), "{}", actions(fx));
        assert_eq!(fx.one(&format!("SELECT condeferrable::text || '|' || confrelid::regclass::text FROM pg_constraint WHERE conname = '{name}'")), format!("true|{parent}"));
        ui.wait("the staged changes to clear", |a| a.staged().is_empty());
        undo_last(ui, 1, &child);
        until(fx, ui, "the undo to land", |fx| actions(fx) == "aa");
        assert_eq!(keys(fx, &child), name);
    });
}
