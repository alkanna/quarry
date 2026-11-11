//! Editing a SQL table's triggers through the Triggers view, on every SQL server whose environment variable is set: MySQL,
//! MariaDB, PostgreSQL 18 and 13. Each flow fills the editor like a person does, saves, reads the server's own catalog to see the
//! trigger (and on PostgreSQL its function), makes a row to see it fire, and takes it back through the undo list.

use super::sql_edit::{every_engine, open_unlocked, Fx};
use super::sql_structure::{replace, undo_last, until};
use super::*;

/// How many triggers of this name the table has, as text.
fn triggers(fx: &Fx, table: &str, name: &str) -> String {
    if fx.is_pg() {
        fx.one(&format!("SELECT count(*) FROM pg_trigger WHERE tgname = '{name}' AND tgrelid = 'public.{table}'::regclass"))
    } else {
        fx.one(&format!("SELECT COUNT(*) FROM information_schema.TRIGGERS WHERE EVENT_OBJECT_SCHEMA = 'quarry_test' AND EVENT_OBJECT_TABLE = '{table}' AND TRIGGER_NAME = '{name}'"))
    }
}

/// How many functions the schema has with this name: PostgreSQL's triggers run one, and the editor makes it with the trigger.
fn functions(fx: &Fx, name: &str) -> String {
    if fx.is_pg() { fx.one(&format!("SELECT count(*) FROM pg_proc WHERE proname = '{name}' AND pronamespace = 'public'::regnamespace")) } else { "0".into() }
}

/// The name a new row gets from the trigger: the row is made, read and deleted again, so the table keeps its rows.
fn fired(fx: &Fx, table: &str) -> String {
    let q = fx.qualified(table);
    fx.sql(&format!("INSERT INTO {q} VALUES (9, 'plain', 1)"));
    let name = fx.one(&format!("SELECT name FROM {q} WHERE id = 9"));
    fx.sql(&format!("DELETE FROM {q} WHERE id = 9"));
    name
}

/// A body that sets the name of the new row, written for the engine.
fn setter(fx: &Fx, to: &str) -> String {
    if fx.is_pg() { format!("BEGIN NEW.name := '{to}'; RETURN NEW; END") } else { format!("SET NEW.name = '{to}'") }
}

/// Makes a trigger the way a person would have before opening the editor, with its function on PostgreSQL.
fn make_trigger(fx: &Fx, table: &str, name: &str, to: &str) {
    let q = fx.qualified(table);
    if fx.is_pg() {
        fx.sql(&format!("CREATE FUNCTION public.{name}_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.name := '{to}'; RETURN NEW; END $$; CREATE TRIGGER {name} BEFORE INSERT ON {q} FOR EACH ROW EXECUTE FUNCTION public.{name}_fn()"));
    } else {
        fx.sql(&format!("CREATE TRIGGER quarry_test.{name} BEFORE INSERT ON {q} FOR EACH ROW SET NEW.name = '{to}'"));
    }
}

/// A fixture table that has a trigger before the UI opens it, because the structure is read once when the table opens.
fn open_with_trigger(fx: &mut Fx, ui: &mut UiApp, to: &str) -> (String, String) {
    let t = fx.table();
    let name = format!("trg_{t}");
    make_trigger(fx, &t, &name, to);
    fx.open(ui, &t);
    ui.unlock();
    (t, name)
}

fn open_triggers(ui: &mut UiApp) {
    ui.click("Triggers");
    ui.wait("the triggers", |a| a.t().is_some_and(|t| t.view == View::Triggers) && a.structure().is_some());
}

/// Fills the new trigger's name and body, then waits for Save trigger to be on.
fn fill_new(fx: &Fx, ui: &mut UiApp, name: &str, to: &str) {
    ui.click("+ Trigger");
    replace(ui, "Trigger name", name);
    replace(ui, "Trigger body", &setter(fx, to));
    ui.show("Save trigger");
    let t = std::time::Instant::now();
    while ui.h.get_by_label("Save trigger").accesskit_node().is_disabled() {
        assert!(t.elapsed() < LIMIT, "Save trigger stayed disabled: {}", ui.app().last_problem());
        ui.h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_trigger_is_created_through_the_editor_fires_and_is_taken_back() {
    every_engine("a trigger is created through the editor", |fx, ui| {
        let t = open_unlocked(fx, ui);
        let name = format!("trg_{t}");
        open_triggers(ui);
        assert_eq!(triggers(fx, &t, &name), "0");
        fill_new(fx, ui, &name, "ui");
        if fx.is_pg() {
            // the preview shows the function and the trigger that Save sends, and the function's name
            ui.show("SQL TO RUN");
            let fname = format!("{t}_{name}_fn");
            let t0 = std::time::Instant::now();
            while ui.h.query_all_by_label_contains("CREATE FUNCTION").next().is_none() {
                assert!(t0.elapsed() < LIMIT, "the preview never showed the function");
                ui.h.step();
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(ui.h.query_all_by_label_contains(&fname).next().is_some(), "the preview names the function {fname}");
        }
        ui.click("Save trigger");
        until(fx, ui, "the trigger to land", |fx| triggers(fx, &t, &name) == "1");
        if fx.is_pg() {
            assert_eq!(functions(fx, &format!("{t}_{name}_fn")), "1", "the function was made with the trigger");
        }
        assert_eq!(fired(fx, &t), "ui", "the trigger fires");
        // the structure lists it from the server, and the draft is gone
        ui.wait("the trigger to be listed", |a| a.structure().is_some_and(|s| s.triggers.iter().any(|x| x.name == name)) && a.t().and_then(|t| t.sql.as_ref()).is_some_and(|s| !s.st.trig_new));
        undo_last(ui, 1, &t);
        until(fx, ui, "the undo to land", |fx| triggers(fx, &t, &name) == "0" && functions(fx, &format!("{t}_{name}_fn")) == "0");
        assert_eq!(fx.rows(&t), "1:alpha:10,2:beta:20,3:gamma:30", "the rows were never touched");
    });
}

#[test]
fn a_trigger_is_edited_through_the_editor_and_the_undo_puts_it_back() {
    every_engine("a trigger is edited through the editor", |fx, ui| {
        let (t, name) = open_with_trigger(fx, ui, "old");
        assert_eq!(fired(fx, &t), "old");
        open_triggers(ui);
        ui.show("triggers row 1");
        replace(ui, "Trigger body", &setter(fx, "new"));
        ui.show("Save trigger");
        let w = std::time::Instant::now();
        while ui.h.get_by_label("Save trigger").accesskit_node().is_disabled() {
            assert!(w.elapsed() < LIMIT, "Save trigger stayed disabled: {}", ui.app().last_problem());
            ui.h.step();
            std::thread::sleep(Duration::from_millis(10));
        }
        ui.click("Save trigger");
        until(fx, ui, "the new body to run", |fx| fired(fx, &t) == "new");
        assert_eq!(triggers(fx, &t, &name), "1");
        ui.wait("the save to finish", |a| !a.applying());
        undo_last(ui, 1, &t);
        until(fx, ui, "the old body to run again", |fx| fired(fx, &t) == "old");
        assert_eq!(triggers(fx, &t, &name), "1");
        if fx.is_pg() {
            fx.sql(&format!("DROP TRIGGER {name} ON {t}; DROP FUNCTION public.{name}_fn()"));
        }
    });
}

#[test]
fn a_postgresql_trigger_is_disabled_through_the_switch_and_dropped_with_its_function() {
    every_engine("a postgresql trigger is disabled and dropped", |fx, ui| {
        if !fx.is_pg() {
            return;
        }
        let (t, name) = open_with_trigger(fx, ui, "old");
        open_triggers(ui);
        ui.show("triggers row 1");
        // Delete asks for the name, and says the function goes with it
        ui.click("Delete");
        ui.show("Confirmation");
        assert!(matches!(&ui.app().modal, Some(Modal::Typed { title, .. }) if title.contains("function")), "the dialog tells that the function goes too");
        ui.fill("Confirmation", &name);
        ui.click("Confirm");
        until(fx, ui, "the trigger and its function to go", |fx| triggers(fx, &t, &name) == "0" && functions(fx, &format!("{name}_fn")) == "0");
        ui.wait("the drop to finish", |a| !a.applying());
        undo_last(ui, 1, &t);
        until(fx, ui, "both to come back", |fx| triggers(fx, &t, &name) == "1" && functions(fx, &format!("{name}_fn")) == "1");
        assert_eq!(fired(fx, &t), "old");
        // the Recent changes dialog stays open after an undo and covers the page
        ui.click("Close");
        ui.wait("the trigger to be listed again", |a| a.structure().is_some_and(|s| s.triggers.len() == 1));
        ui.show("triggers row 1");
        ui.click("Trigger enabled");
        ui.click("Save trigger");
        until(fx, ui, "the trigger to be disabled", |fx| fx.one(&format!("SELECT tgenabled::text FROM pg_trigger WHERE tgname = '{name}' AND tgrelid = 'public.{t}'::regclass")) == "D");
        assert_eq!(fired(fx, &t), "plain", "a disabled trigger does not fire");
        ui.wait("the save to finish", |a| !a.applying());
        undo_last(ui, 1, &t);
        until(fx, ui, "the trigger to be enabled again", |fx| fx.one(&format!("SELECT tgenabled::text FROM pg_trigger WHERE tgname = '{name}' AND tgrelid = 'public.{t}'::regclass")) == "O");
        assert_eq!(fired(fx, &t), "old");
        fx.sql(&format!("DROP TRIGGER {name} ON {t}; DROP FUNCTION public.{name}_fn()"));
    });
}

#[test]
fn a_locked_connection_asks_to_unlock_before_saving_a_trigger() {
    every_engine("a locked connection asks to unlock before saving a trigger", |fx, ui| {
        let t = fx.table();
        fx.open(ui, &t);
        let name = format!("trg_{t}");
        open_triggers(ui);
        // locked, the editor still opens, and Save is what asks
        assert!(!ui.h.get_by_label("+ Trigger").accesskit_node().is_disabled(), "+ Trigger is off on a locked connection");
        fill_new(fx, ui, &name, "ui");
        ui.click("Save trigger");
        ui.show("Stay read-only");
        assert_eq!(triggers(fx, &t, &name), "0", "nothing ran before the answer");
        ui.click("Stay read-only");
        ui.h.run_steps(8);
        assert_eq!(triggers(fx, &t, &name), "0", "the server changed");
        // allowing writes runs it
        ui.click("Save trigger");
        ui.click("Allow writes");
        until(fx, ui, "the trigger to land", |fx| triggers(fx, &t, &name) == "1");
        ui.wait("the save to finish", |a| !a.applying());
        undo_last(ui, 1, &t);
        until(fx, ui, "the undo to land", |fx| triggers(fx, &t, &name) == "0");
    });
}
