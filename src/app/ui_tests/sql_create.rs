//! Creating and dropping through the UI, on every SQL server whose environment variable is set: MySQL, MariaDB, PostgreSQL 18 and 13.
//! Each flow opens the dialog the sidebar's menu opens (`side_act` is what a click on its item calls), types and clicks like a person,
//! waits for the answer instead of a number of frames, and reads the server's own catalog to see what happened.

use super::sql_edit::{every_engine, Fx, Server};
use super::sql_structure::{is_pg, one, replace, until};
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn name(tag: &str) -> String {
    static N: AtomicUsize = AtomicUsize::new(0);
    format!("ui_cr_{tag}_{}_{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

fn table_exists(fx: &Fx, table: &str) -> bool {
    let n = if is_pg(fx) {
        one(fx, &format!("SELECT count(*) FROM pg_class WHERE relname = '{table}' AND relnamespace = 'public'::regnamespace"))
    } else {
        one(fx, &format!("SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = 'quarry_test' AND TABLE_NAME = '{table}'"))
    };
    n.trim() != "0"
}

/// Waits until no tab has a read in flight. A tab that is still reading its table holds a lock on it, and an undo or a drop takes
/// ACCESS EXCLUSIVE under a 3 s lock timeout, so a loaded machine could make the test fail on the lock and not on what it checks.
fn quiet(ui: &mut UiApp) {
    ui.wait("the open tabs to have no read in flight", |a| a.tabs.iter().all(|t| !t.loading && t.sql.as_ref().is_none_or(|s| s.run_req == 0 && !matches!(s.st.load, crate::app::sql_structure::Load::Loading))));
    ui.settle();
    ui.wait("the open tabs to have no read in flight", |a| a.tabs.iter().all(|t| !t.loading && t.sql.as_ref().is_none_or(|s| s.run_req == 0 && !matches!(s.st.load, crate::app::sql_structure::Load::Loading))));
}

fn dialog_ready(a: &App) -> bool {
    match &a.modal {
        Some(Modal::SqlNewTable(f)) => f.meta.is_some(),
        Some(Modal::PgNewTable(_)) => true,
        _ => false,
    }
}

/// Opens the New table dialog from the sidebar's action and waits until its form can be typed into.
fn open_builder(fx: &Fx, ui: &mut UiApp) {
    let (db, _) = fx.place();
    ui.h.state_mut().side_act(CONN, SideAct::NewTable(db.to_string()));
    ui.wait("the New table dialog", dialog_ready);
    ui.settle();
}

#[test]
fn a_table_is_built_through_the_dialog_and_taken_back() {
    every_engine("a table is built through the dialog and taken back", |fx, ui| {
        let anchor = fx.table();
        fx.open(ui, &anchor);
        ui.unlock();
        let table = name("make");
        open_builder(fx, ui);
        // The name field has the focus and the first column is the key every table starts with.
        ui.type_text(&table);
        if is_pg(fx) {
            replace(ui, "Table comment", "built by a test");
            ui.click("Preview SQL");
            ui.show("Create table");
        }
        ui.click("Create table");
        until(fx, ui, "the table to be made", |fx| table_exists(fx, &table));
        ui.wait("the new table to open in Structure", |a| a.t().is_some_and(|t| t.sel.coll == table && t.view == View::Structure));
        if is_pg(fx) {
            assert_eq!(one(fx, &format!("SELECT string_agg(attname || ':' || format_type(atttypid, atttypmod) || ':' || attidentity::text, ',') FROM pg_attribute WHERE attrelid = 'public.{table}'::regclass AND attnum > 0")), "id:integer:d");
            assert_eq!(one(fx, &format!("SELECT obj_description('public.{table}'::regclass, 'pg_class')")), "built by a test");
        } else {
            assert_eq!(one(fx, &format!("SELECT COLUMN_NAME FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = 'quarry_test' AND TABLE_NAME = '{table}'")), "id");
        }
        assert_eq!(ui.app().undo.len(), 1, "the create is in the list of recent changes");
        // Take it back through the list: the table is empty, so it goes and its tab closes.
        quiet(ui);
        ui.click("Undo (1)");
        ui.click("Undo change 1");
        until(fx, ui, "the undo to land", |fx| !table_exists(fx, &table));
        ui.wait("the tab of the removed table to close", |a| !a.tabs.iter().any(|t| t.sel.coll == table));
        assert!(ui.app().undo.is_empty(), "the used entry is gone");
    });
}

#[test]
fn a_table_with_rows_is_not_taken_back() {
    every_engine("a table with rows is not taken back", |fx, ui| {
        let anchor = fx.table();
        fx.open(ui, &anchor);
        ui.unlock();
        let table = name("rows");
        open_builder(fx, ui);
        ui.type_text(&table);
        ui.click("Create table");
        until(fx, ui, "the table to be made", |fx| table_exists(fx, &table));
        ui.wait("the entry", |a| a.undo.len() == 1);
        let q = fx.qualified(&table);
        fx.sql(&format!("INSERT INTO {q} (id) VALUES (1)"));
        quiet(ui);
        ui.click("Undo (1)");
        ui.click("Undo change 1");
        ui.wait("the refusal", |a| a.last_problem().contains("has rows"));
        assert!(table_exists(fx, &table), "a refused undo drops nothing");
        assert_eq!(ui.app().undo.len(), 1, "the entry stays so it can be tried again");
        fx.sql(&format!("DROP TABLE {q}"));
    });
}

#[test]
fn a_table_is_dropped_only_through_the_typed_confirmation() {
    every_engine("a table is dropped only through the typed confirmation", |fx, ui| {
        let table = fx.table();
        fx.open(ui, &table);
        ui.unlock();
        let (db, _) = fx.place();
        ui.h.state_mut().side_act(CONN, SideAct::SqlDrop(db.to_string(), table.clone(), DropKind::Table));
        ui.wait("the counts", |a| matches!(&a.modal, Some(Modal::SqlDrop(f)) if f.info.is_some()));
        ui.settle();
        quiet(ui);
        assert!(ui.h.get_by_label("Drop table").accesskit_node().is_disabled(), "nothing is dropped before the name is typed");
        ui.fill("Confirmation", "not the name");
        ui.settle();
        assert!(ui.h.get_by_label("Drop table").accesskit_node().is_disabled(), "a wrong name does not confirm");
        assert!(table_exists(fx, &table));
        replace(ui, "Confirmation", &table);
        ui.settle();
        ui.click("Drop table");
        until(fx, ui, "the table to be dropped", |fx| !table_exists(fx, &table));
        ui.wait("its tab to close", |a| !a.tabs.iter().any(|t| t.sel.coll == table));
        assert!(ui.app().undo.is_empty(), "a drop has no undo");
    });
}

#[test]
fn a_locked_connection_asks_to_unlock_before_creating_or_dropping() {
    every_engine("a locked connection asks to unlock before creating or dropping", |fx, ui| {
        let table = fx.table();
        fx.open(ui, &table);
        let (db, _) = fx.place();
        // Locked: the dialog does not open, the question does, and staying read-only goes no further.
        ui.h.state_mut().side_act(CONN, SideAct::NewTable(db.to_string()));
        ui.settle();
        let which = match &ui.app().modal { None => "none", Some(Modal::SqlNewTable(_)) => "sqlnewtable", Some(Modal::Unlock(..)) => "unlock", Some(Modal::Palette { .. }) => "palette", _ => "other" };
        assert!(matches!(ui.app().modal, Some(Modal::Unlock(..))), "a create on a locked connection asks to unlock, modal is {which}, writable {}", ui.app().conn(CONN).unwrap().writable);
        ui.click("Stay read-only");
        assert!(ui.app().modal.is_none() && !ui.app().conn(CONN).unwrap().writable);
        ui.h.state_mut().side_act(CONN, SideAct::SqlDrop(db.to_string(), table.clone(), DropKind::Table));
        ui.settle();
        assert!(matches!(ui.app().modal, Some(Modal::Unlock(..))), "a drop asks too");
        // Allowing writes goes on with the drop's dialog, which still needs the name.
        ui.click("Allow writes");
        ui.wait("the drop dialog", |a| matches!(a.modal, Some(Modal::SqlDrop(_))));
        ui.click("Cancel");
        ui.settle();
        assert!(table_exists(fx, &table), "nothing was dropped");
    });
}

#[test]
fn a_drop_that_slips_past_the_dialog_on_a_locked_connection_is_refused_by_the_worker() {
    every_engine("a drop on a locked connection is refused by the worker", |fx, ui| {
        if !is_pg(fx) {
            return;
        }
        let table = fx.table();
        fx.open(ui, &table);
        quiet(ui);
        assert!(!ui.app().conn(CONN).unwrap().writable);
        let sql = ui.app().pg_session(CONN).expect("a session");
        ui.h.state_mut().pg_drop_run(CONN, sql, fx.place().0.to_string(), "public".into(), table.clone(), DropKind::Table, false);
        ui.wait("the refusal", |a| !a.last_problem().is_empty());
        assert!(table_exists(fx, &table), "the worker dropped a table on a locked connection");
    });
}

fn pg_only(name: &str, flow: impl Fn(&mut Fx, &mut UiApp)) {
    every_engine(name, |fx, ui| {
        if matches!(fx.server, Server::Pg(..)) {
            flow(fx, ui);
        }
    });
}

#[test]
fn a_schema_is_made_and_dropped_with_cascade_through_the_dialogs() {
    pg_only("a schema is made and dropped through the dialogs", |fx, ui| {
        let anchor = fx.table();
        fx.open(ui, &anchor);
        ui.unlock();
        let schema = name("sch");
        ui.h.state_mut().side_act(CONN, SideAct::NewSchema("quarry_test".into()));
        ui.wait("the New schema dialog", |a| matches!(a.modal, Some(Modal::PgNewSchema(_))));
        ui.settle();
        ui.type_text(&schema);
        ui.click("Create schema");
        until(fx, ui, "the schema to be made", |fx| one(fx, &format!("SELECT count(*) FROM pg_namespace WHERE nspname = '{schema}'")).trim() == "1");
        ui.wait("the sidebar to pick it", |a| a.conn(CONN).and_then(|c| c.pg.get("quarry_test")).is_some_and(|p| p.picked == schema));
        fx.sql(&format!("CREATE TABLE {schema}.t (id int)"));
        ui.h.state_mut().side_act(CONN, SideAct::DropSchema("quarry_test".into(), schema.clone()));
        ui.wait("the counts", |a| matches!(&a.modal, Some(Modal::SqlDrop(f)) if f.info.as_ref().is_some_and(|i| i.as_ref().is_ok_and(|i| i.dependents_total > 0))));
        ui.settle();
        replace(ui, "Confirmation", &schema);
        ui.settle();
        assert!(ui.h.get_by_label("Drop schema").accesskit_node().is_disabled(), "a schema with a table in it cannot be dropped until CASCADE is ticked");
        ui.click("Drop them too (CASCADE)");
        ui.click("Drop schema");
        until(fx, ui, "the schema to be dropped", |fx| one(fx, &format!("SELECT count(*) FROM pg_namespace WHERE nspname = '{schema}'")).trim() == "0");
    });
}

#[test]
fn a_database_is_made_and_dropped_with_its_connections_closed() {
    pg_only("a database is made and dropped through the dialogs", |fx, ui| {
        let anchor = fx.table();
        fx.open(ui, &anchor);
        ui.unlock();
        let db = name("db");
        let exists = |fx: &Fx| one(fx, &format!("SELECT count(*) FROM pg_database WHERE datname = '{db}'")).trim() == "1";
        ui.h.state_mut().side_act(CONN, SideAct::NewDb);
        ui.wait("the New database dialog", |a| matches!(a.modal, Some(Modal::PgNewDb(_))));
        ui.settle();
        ui.type_text(&db);
        {
            // Two CREATE DATABASE at once fail on template1 being in use, so the live tests take turns.
            let _turn = crate::db::sql::testing::db_turn();
            ui.wait("the template list", |a| matches!(&a.modal, Some(Modal::PgNewDb(f)) if f.templates.is_some()));
            ui.click("Create");
            until(fx, ui, "the database to be made", |fx| exists(fx));
            ui.wait("the sidebar list", |a| a.conn(CONN).is_some_and(|c| c.dbs.iter().any(|d| d.0 == db)));
        }
        // Open it, so Quarry's own pool holds a session on it, then drop it: the pool is closed first and no FORCE is needed.
        ui.click(&db);
        ui.wait("its schemas", |a| a.conn(CONN).is_some_and(|c| c.pg.contains_key(db.as_str())));
        ui.h.state_mut().side_act(CONN, SideAct::SqlDrop(db.clone(), String::new(), DropKind::Database));
        ui.wait("the counts", |a| matches!(&a.modal, Some(Modal::SqlDrop(f)) if f.info.is_some()));
        ui.settle();
        replace(ui, "Confirmation", &db);
        ui.settle();
        ui.click("Drop database");
        until(fx, ui, "the database to be dropped", |fx| !exists(fx));
        ui.wait("the sidebar list", |a| a.conn(CONN).is_some_and(|c| !c.dbs.iter().any(|d| d.0 == db)));
        assert!(ui.app().last_problem().is_empty() || !ui.app().last_problem().contains("accessed"), "Quarry's own connections did not stop the drop");
    });
}

#[test]
fn the_drop_of_a_table_names_its_database_and_schema() {
    pg_only("the drop of a table names its database and schema", |fx, ui| {
        let table = fx.table();
        fx.open(ui, &table);
        ui.unlock();
        let (db, schema) = fx.place();
        ui.h.state_mut().side_act(CONN, SideAct::SqlDrop(db.to_string(), table.clone(), DropKind::Table));
        ui.wait("the counts", |a| matches!(&a.modal, Some(Modal::SqlDrop(f)) if f.info.is_some()));
        ui.settle();
        // The same table name can be in several schemas and databases, so the title and the sentence under it say which one this is.
        let full = format!("{db}.{schema}.{table}");
        ui.h.get_by_label(&format!("Drop table {full}?"));
        ui.h.get_by_label_contains(&format!("This deletes the table {full}"));
        ui.click("Cancel");
    });
}

#[test]
fn a_database_drop_the_server_refuses_leaves_its_tabs_and_their_text_alone() {
    pg_only("a refused database drop keeps its tabs", |fx, ui| {
        let anchor = fx.table();
        fx.open(ui, &anchor);
        ui.unlock();
        let db = name("keep");
        let Server::Pg(l, _) = &fx.server else { return };
        let exists = |fx: &Fx| one(fx, &format!("SELECT count(*) FROM pg_database WHERE datname = '{db}'")).trim() == "1";
        ui.h.state_mut().side_act(CONN, SideAct::NewDb);
        ui.wait("the New database dialog", |a| matches!(a.modal, Some(Modal::PgNewDb(_))));
        ui.settle();
        ui.type_text(&db);
        {
            let _turn = crate::db::sql::testing::db_turn();
            ui.wait("the template list", |a| matches!(&a.modal, Some(Modal::PgNewDb(f)) if f.templates.is_some()));
            ui.click("Create");
            until(fx, ui, "the database to be made", |fx| exists(fx));
            ui.wait("the sidebar list", |a| a.conn(CONN).is_some_and(|c| c.dbs.iter().any(|d| d.0 == db)));
        }
        // A Query tab with text that was never run, and another session of someone else, which makes the server refuse the drop.
        ui.h.state_mut().side_act(CONN, SideAct::NewQuery(db.clone()));
        ui.wait("the Query tab", |a| a.tabs.iter().any(|t| t.sel.db == db));
        ui.h.state_mut().tabs.iter_mut().filter(|t| t.sel.db == db).for_each(|t| {
            if let Some(s) = t.sql.as_mut() {
                s.query = "select 'unsaved work'".into();
            }
        });
        let o = {
            use std::str::FromStr;
            sqlx::postgres::PgConnectOptions::from_str(&l.uri).unwrap().database(&db)
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let other = rt.block_on(async {
            use sqlx::Connection;
            sqlx::PgConnection::connect_with(&o).await.unwrap()
        });
        ui.h.state_mut().side_act(CONN, SideAct::SqlDrop(db.clone(), String::new(), DropKind::Database));
        ui.wait("the counts", |a| matches!(&a.modal, Some(Modal::SqlDrop(f)) if f.info.is_some()));
        ui.settle();
        assert_eq!(ui.app().pg_unsaved_in(CONN, &db), (0, 0, 1), "the confirmation counts the query text");
        ui.h.get_by_label_contains("1 query with text in its editor");
        replace(ui, "Confirmation", &db);
        ui.settle();
        ui.click("Drop database");
        ui.wait("the server's refusal", |a| a.last_problem().contains("55006"));
        assert!(exists(fx), "a refused drop drops nothing");
        assert!(ui.app().tabs.iter().any(|t| t.sel.db == db && t.sql.as_ref().is_some_and(|s| s.query == "select 'unsaved work'")), "the tab and its text are still there");
        // With the other session gone the same drop works, and only then do the tabs close.
        rt.block_on(async {
            use sqlx::Connection;
            other.close().await.unwrap()
        });
        ui.h.state_mut().side_act(CONN, SideAct::SqlDrop(db.clone(), String::new(), DropKind::Database));
        ui.wait("the counts", |a| matches!(&a.modal, Some(Modal::SqlDrop(f)) if f.info.is_some()));
        ui.settle();
        replace(ui, "Confirmation", &db);
        ui.settle();
        ui.click("Drop database");
        until(fx, ui, "the database to be dropped", |fx| !exists(fx));
        ui.wait("its tabs to close", |a| !a.tabs.iter().any(|t| t.sel.db == db));
    });
}
