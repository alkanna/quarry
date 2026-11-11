//! Editing SQL grid rows through the UI, on every SQL server whose environment variable is set: MySQL,
//! MariaDB, PostgreSQL 18 and 13. Each flow runs on its own fixture table and ends by reading the rows from the
//! server with a plain query, so what the grid claims is checked against what was written.

use super::*;
use crate::db::sql::testing::{ENVS, Live, LivePg, PG_ENVS, live, live_pg};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The rows of a fresh fixture table, as `Fx::rows` reports them.
const ORIGINAL: &str = "1:alpha:10,2:beta:20,3:gamma:30";

enum Server {
    My(Live),
    Pg(LivePg, sqlx::PgPool),
}

/// Fixture tables on one server. They are dropped with it, also when a flow fails half way.
struct Fx {
    server: Server,
    tables: Vec<String>,
}

impl Fx {
    fn new(var: &'static str) -> Option<Fx> {
        let server = if ENVS.contains(&var) {
            let l = live(var)?;
            l.sql("CREATE DATABASE IF NOT EXISTS quarry_test");
            Server::My(l)
        } else {
            let l = live_pg(var)?;
            l.quarry_test();
            let pool = l.fixture_pool();
            Server::Pg(l, pool)
        };
        Some(Fx { server, tables: Vec::new() })
    }

    /// The database and schema of the sidebar: MySQL has no schemas.
    fn place(&self) -> (&'static str, &'static str) {
        match self.server {
            Server::My(_) => ("quarry_test", ""),
            Server::Pg(..) => ("quarry_test", "public"),
        }
    }

    fn sql(&self, s: &str) {
        match &self.server {
            Server::My(l) => l.sql(s),
            Server::Pg(l, pool) => l.qsql(pool, s),
        }
    }

    /// The table name as the server's SQL wants it.
    fn qualified(&self, table: &str) -> String {
        match self.server {
            Server::My(_) => format!("quarry_test.{table}"),
            Server::Pg(..) => table.to_string(),
        }
    }

    /// A uniquely named table holding the ORIGINAL rows. Tests run in parallel on shared servers, so no two share a name.
    fn table(&mut self) -> String {
        static N: AtomicUsize = AtomicUsize::new(0);
        let name = format!("ui_edit_{}_{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed));
        self.tables.push(name.clone());
        let q = self.qualified(&name);
        self.sql(&format!("CREATE TABLE {q} (id int PRIMARY KEY, name varchar(50), qty int)"));
        self.sql(&format!("INSERT INTO {q} VALUES (1, 'alpha', 10), (2, 'beta', 20), (3, 'gamma', 30)"));
        name
    }

    /// Every row as `id:name:qty`, ordered by id and joined with commas, read with a plain query.
    fn rows(&self, table: &str) -> String {
        let q = self.qualified(table);
        let one = match &self.server {
            Server::My(l) => l.one(&format!("SELECT COALESCE(GROUP_CONCAT(CONCAT(id, ':', name, ':', qty) ORDER BY id SEPARATOR ','), '') FROM {q}")),
            Server::Pg(l, pool) => l.qone(pool, &format!("SELECT COALESCE(string_agg(id || ':' || name || ':' || qty, ',' ORDER BY id), '') FROM {q}")),
        };
        one
    }

    /// Steps the UI until the server holds `want`: a write goes out on a worker task, so it lands a few frames after the click.
    fn expect(&self, ui: &mut UiApp, table: &str, want: &str) {
        let t = std::time::Instant::now();
        loop {
            ui.h.step();
            let got = self.rows(table);
            if got == want {
                return;
            }
            assert!(t.elapsed() < LIMIT, "the server holds \"{got}\", wanted \"{want}\"; last error: {}", ui.app().last_problem());
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Steps the UI for a while and then checks that the server still holds `want`, for flows that must write nothing.
    fn expect_unchanged(&self, ui: &mut UiApp, table: &str, want: &str) {
        ui.h.run_steps(8);
        assert_eq!(self.rows(table), want, "the server changed");
    }

    /// Opens the fixture table in a UI with the connection unlocked or not.
    fn open(&self, ui: &mut UiApp, table: &str) {
        let (db, schema) = self.place();
        ui.open_table(db, schema, table);
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        for t in &self.tables {
            let q = self.qualified(t);
            match &self.server {
                Server::My(l) => l.sql(&format!("DROP TABLE IF EXISTS {q}")),
                Server::Pg(l, pool) => l.qsql(pool, &format!("DROP TABLE IF EXISTS {q}")),
            }
        }
    }
}

/// Runs `flow` once per configured server, with a UI connected to it. A failure is reported with the flow and the server.
fn every_engine(name: &str, flow: impl Fn(&mut Fx, &mut UiApp)) {
    for var in ENVS.into_iter().chain(PG_ENVS) {
        let Some(mut fx) = Fx::new(var) else { continue };
        let mut ui = ui_app(var).unwrap();
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| flow(&mut fx, &mut ui)));
        if let Err(p) = run {
            let msg = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
            panic!("{name} on {var}: {msg}");
        }
    }
}

/// Opens the fixture table unlocked.
fn open_unlocked(fx: &mut Fx, ui: &mut UiApp) -> String {
    let table = fx.table();
    fx.open(ui, &table);
    ui.unlock();
    table
}

#[test]
fn inline_edit_saves() {
    every_engine("inline edit saves", |fx, ui| {
        let t = open_unlocked(fx, ui);
        ui.edit_cell("name", 1, "zeta");
        ui.save();
        fx.expect(ui, &t, "1:zeta:10,2:beta:20,3:gamma:30");
    });
}

#[test]
fn editor_opens_again_after_esc() {
    every_engine("editor opens again after Esc", |fx, ui| {
        let t = open_unlocked(fx, ui);
        ui.open_cell("name", 1);
        ui.h.key_press(egui::Key::Escape);
        ui.wait("the editor to close", |a| a.sql_cell.is_none());
        assert!(ui.app().drafts.is_empty(), "Esc staged something");
        ui.edit_cell("name", 1, "zeta");
        ui.save();
        fx.expect(ui, &t, "1:zeta:10,2:beta:20,3:gamma:30");
    });
}

#[test]
fn edits_on_several_rows_save_together() {
    every_engine("edits on several rows save together", |fx, ui| {
        let t = open_unlocked(fx, ui);
        ui.edit_cell("name", 1, "zeta");
        ui.edit_cell("qty", 2, "99");
        ui.edit_cell("name", 3, "eta");
        ui.edit_cell("qty", 3, "7");
        assert_eq!(ui.app().drafts.len(), 3, "three rows are staged");
        fx.expect_unchanged(ui, &t, ORIGINAL);
        ui.save();
        fx.expect(ui, &t, "1:zeta:10,2:beta:99,3:eta:7");
    });
}

#[test]
fn delete_one_row() {
    every_engine("delete one row", |fx, ui| {
        let t = open_unlocked(fx, ui);
        ui.click("Select row 2");
        ui.click("Delete");
        fx.expect_unchanged(ui, &t, ORIGINAL);
        ui.click_last("Delete");
        fx.expect(ui, &t, "1:alpha:10,3:gamma:30");
    });
}

#[test]
fn delete_several_rows_asks_for_the_table_name() {
    every_engine("delete several rows asks for the table name", |fx, ui| {
        let t = open_unlocked(fx, ui);
        ui.click("Select row 1");
        ui.click("Select row 3");
        ui.click("Delete");
        ui.fill("Confirmation", "not the table");
        assert!(ui.h.get_by_label("Confirm").accesskit_node().is_disabled(), "Confirm waits for the exact name");
        fx.expect_unchanged(ui, &t, ORIGINAL);
        ui.h.get_by_label("Confirmation").focus();
        ui.h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        ui.type_text(&t);
        ui.click("Confirm");
        fx.expect(ui, &t, "2:beta:20");
    });
}

/// Fills the "+ Row" dialog: every column gets a value, in column order.
fn insert_row(ui: &mut UiApp, id: &str, name: &str, qty: &str) {
    ui.click("+ Row");
    for (i, (col, v)) in [("id", id), ("name", name), ("qty", qty)].into_iter().enumerate() {
        ui.click_nth("Value", i);
        ui.fill(&format!("Value of {col}"), v);
    }
    ui.click("Insert");
}

#[test]
fn insert_a_row() {
    every_engine("insert a row", |fx, ui| {
        let t = open_unlocked(fx, ui);
        insert_row(ui, "4", "delta", "40");
        fx.expect(ui, &t, "1:alpha:10,2:beta:20,3:gamma:30,4:delta:40");
    });
}

/// Takes back the newest change through the toolbar button and its dialog.
fn undo_last(ui: &mut UiApp, count: usize) {
    ui.click(&format!("Undo ({count})"));
    ui.click("Undo change 1");
    ui.click("Close");
}

#[test]
fn undo_an_edit() {
    every_engine("undo an edit", |fx, ui| {
        let t = open_unlocked(fx, ui);
        ui.edit_cell("name", 1, "zeta");
        ui.save();
        fx.expect(ui, &t, "1:zeta:10,2:beta:20,3:gamma:30");
        undo_last(ui, 1);
        fx.expect(ui, &t, ORIGINAL);
    });
}

#[test]
fn undo_a_delete() {
    every_engine("undo a delete", |fx, ui| {
        let t = open_unlocked(fx, ui);
        ui.click("Select row 2");
        ui.click("Delete");
        ui.click_last("Delete");
        fx.expect(ui, &t, "1:alpha:10,3:gamma:30");
        undo_last(ui, 1);
        fx.expect(ui, &t, ORIGINAL);
    });
}

#[test]
fn undo_an_insert() {
    every_engine("undo an insert", |fx, ui| {
        let t = open_unlocked(fx, ui);
        insert_row(ui, "4", "delta", "40");
        fx.expect(ui, &t, "1:alpha:10,2:beta:20,3:gamma:30,4:delta:40");
        undo_last(ui, 1);
        fx.expect(ui, &t, ORIGINAL);
    });
}

#[test]
fn switching_the_view_closes_the_editor() {
    every_engine("switching the view closes the editor", |fx, ui| {
        let t = open_unlocked(fx, ui);
        ui.open_cell("name", 1);
        ui.type_text("zeta");
        ui.click("Structure");
        ui.wait("the view to change", |a| a.t().is_some_and(|t| t.view == View::Structure));
        assert!(ui.app().sql_cell.is_none(), "the editor stayed open");
        assert!(ui.app().drafts.is_empty(), "something was staged");
        ui.click("Content");
        ui.show("name row 1");
        assert!(ui.app().sql_cell.is_none(), "the editor came back");
        fx.expect_unchanged(ui, &t, ORIGINAL);
    });
}

#[test]
fn switching_the_table_closes_the_editor() {
    every_engine("switching the table closes the editor", |fx, ui| {
        // Both exist before the sidebar lists the tables of the database.
        let (first, second) = (fx.table(), fx.table());
        fx.open(ui, &first);
        ui.unlock();
        ui.open_cell("name", 1);
        ui.type_text("zeta");
        ui.click(&second);
        ui.wait("the other table", |a| a.t().is_some_and(|t| t.sel.coll == second));
        assert!(ui.app().sql_cell.is_none(), "the editor stayed open");
        assert!(ui.app().drafts.is_empty(), "something was staged");
        fx.expect_unchanged(ui, &first, ORIGINAL);
        fx.expect_unchanged(ui, &second, ORIGINAL);
    });
}

#[test]
fn a_locked_connection_asks_before_any_write() {
    every_engine("a locked connection asks before any write", |fx, ui| {
        let t = fx.table();
        fx.open(ui, &t);
        let stay = |ui: &mut UiApp, fx: &Fx, what: &str| {
            ui.show("Stay read-only");
            assert!(ui.app().modal.is_some(), "{what}: no dialog");
            ui.click("Stay read-only");
            assert!(ui.app().modal.is_none(), "{what}: the dialog stayed");
            assert!(!ui.app().conn(CONN).unwrap().writable, "{what}: the connection was unlocked");
            assert!(ui.app().sql_cell.is_none() && ui.app().drafts.is_empty(), "{what}: something was opened or staged");
            fx.expect_unchanged(ui, &t, ORIGINAL);
        };
        ui.double_click_cell("name", 1);
        stay(ui, fx, "double click");
        ui.click("Select row 2");
        ui.click("Delete");
        stay(ui, fx, "Delete");
        ui.click("+ Row");
        stay(ui, fx, "+ Row");
    });
}
