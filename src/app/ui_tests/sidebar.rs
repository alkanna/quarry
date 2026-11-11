//! The sidebar while a database's list is on its way, and after it failed.

use super::*;
use crate::db::sql::testing::{ENVS, PG_ENVS, live, live_pg};

/// Opens `db` by clicking its row and checks the frames before the answer show a loading row, then fails the list on
/// purpose and checks the failure row's retry lists it again.
fn loading_then_failure_and_retry(var: &str, db: &str, fail: impl Fn(&App) -> Msg) {
    let mut ui = ui_app(var).unwrap();
    ui.show(db);
    ui.h.get_by_label(db).click();
    // The click opens the database at the end of its frame and the next frame asks for the list, so this frame
    // draws the open database before any answer can have come back.
    ui.h.run_steps(2);
    assert!(ui.h.query_by_label("Loading…").is_some(), "{var}: no loading row while the list is on its way");
    ui.wait("the list", |a| a.side_err.is_empty() && a.h_loaded(db));
    assert!(ui.h.query_by_label("Loading…").is_none(), "{var}: the loading row stays after the list arrived");
    let m = fail(ui.app());
    ui.app().worker.send(m);
    ui.show("Could not list. Retry");
    ui.click("Could not list. Retry");
    ui.wait("the retried list", |a| a.side_err.is_empty() && a.h_loaded(db));
    assert!(ui.h.query_by_label("Could not list. Retry").is_none(), "{var}: the failure row stays after the retry");
}

impl App {
    /// Whether the sidebar holds the list of an open database (on PostgreSQL, the picked schema's tables).
    fn h_loaded(&self, db: &str) -> bool {
        let Some(c) = self.conn(CONN) else { return false };
        if c.meta.engine == db::sql::Engine::Postgres {
            let picked = self.ns_of(CONN, db);
            return c.pg.contains_key(db) && c.tables_in(db, picked).is_some();
        }
        c.dbs.iter().any(|d| d.0 == db && d.1.is_some())
    }
}

#[test]
fn an_open_postgres_database_shows_loading_then_a_failure_with_retry() {
    for var in PG_ENVS {
        let Some(l) = live_pg(var) else { continue };
        l.quarry_test();
        loading_then_failure_and_retry(var, "quarry_test", |a| {
            let schema = a.ns_of(CONN, "quarry_test").to_string();
            Msg::SqlTables { conn: CONN, database: "quarry_test".into(), schema, res: Err("listing refused".into()) }
        });
    }
}

#[test]
fn an_open_mysql_database_shows_loading_then_a_failure_with_retry() {
    for var in ENVS {
        let Some(l) = live(var) else { continue };
        l.sql("CREATE DATABASE IF NOT EXISTS quarry_test");
        loading_then_failure_and_retry(var, "quarry_test", |_| Msg::Collections { conn: CONN, db: "quarry_test".into(), res: Err("listing refused".into()) });
    }
}
