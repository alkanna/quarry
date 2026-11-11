//! The Advanced tab of the connection form on PostgreSQL, driven like a user: a setting added through the picker, a value the
//! server refuses shown under its row after Test, the search_path list reordered, the form saved, and the reconnected
//! session read back with SHOW. Run against the PostgreSQL servers and the PgBouncer, whichever environment variables are set.
//! The native dialogs are never opened.

use super::*;
use crate::db::sql::testing::{PGBOUNCER_ENV, PG_ENVS};

fn form(app: &mut App) -> &mut ConnForm {
    match app.modal.as_mut() {
        Some(Modal::Conn(f)) => f,
        _ => panic!("the connection form is not open"),
    }
}

/// The picker row of a setting, then its value box.
fn add_setting(ui: &mut UiApp, name: &str, value: &str) {
    // The picker may still be open from a search that found nothing to add; its filter is emptied for the next one.
    match form(ui.h.state_mut()).pg_picker.as_mut() {
        Some(q) => q.clear(),
        None => ui.click("Add setting"),
    }
    ui.fill("Setting filter", name);
    ui.click(name);
    ui.fill(&format!("{name} value"), value);
}

fn run(var: &str) {
    let Some(mut ui) = ui_app(var) else { return };
    let uri = std::env::var(var).unwrap();
    let addr = db::sql::Addr::parse(&uri).unwrap();
    let mut f = ConnForm { id: Some(CONN), name: "test".into(), engine: db::sql::Engine::Postgres, addr, tab: ConnTab::Advanced, ..Default::default() };
    f.set_advanced(&Default::default());
    ui.h.state_mut().modal = Some(Modal::Conn(Box::new(f)));

    // A kept setting is not on offer: it shows greyed with its reason and cannot be added.
    ui.click("Add setting");
    ui.fill("Setting filter", "role");
    ui.show("whose rights the lock checks");
    ui.show("Nothing allowed matches. A setting that is not listed is never sent.");
    assert!(form(ui.h.state_mut()).pg_rows.iter().all(|(n, _)| n != "role"), "{var}");

    // A value shaped right but refused by the server shows under its row after Test.
    add_setting(&mut ui, "work_mem", "1kB");
    ui.click("Test");
    ui.wait("the Test answer", |a| a.conn_test.is_some());
    ui.show("Connected, but the server refused 1 setting. See the marked rows on the Advanced tab.");
    let t = std::time::Instant::now();
    while ui.h.query_all_by_label_contains("outside the valid range").next().is_none() {
        assert!(t.elapsed() < LIMIT, "{var}: the server's refusal never showed under the row");
        ui.h.step();
    }
    ui.click("Remove work_mem");
    add_setting(&mut ui, "work_mem", "9MB");

    // A value of the wrong shape is refused as typed, before any Test.
    add_setting(&mut ui, "jit", "maybe");
    ui.show("jit: Expected on or off");
    ui.click("Remove jit");

    // The search_path list: reset it to "$user", public, drop the first, add one schema, then move it to the top.
    ui.click("Reset to \"$user\", public");
    ui.click("Remove $user");
    ui.fill("Schema name", "adv_ui2");
    ui.click("Add");
    ui.wait("the schema to be added", |a| matches!(&a.modal, Some(Modal::Conn(f)) if f.pg_path == ["public", "adv_ui2"]));
    ui.click("Move adv_ui2 up");
    assert_eq!(form(ui.h.state_mut()).pg_path, ["adv_ui2", "public"], "{var}");

    ui.click("Save and connect");
    ui.wait("the form to close", |a| a.modal.is_none());
    let saved = ui.app().conn(CONN).unwrap().meta.advanced.clone();
    assert_eq!(saved.pg_settings, [("work_mem".to_string(), "9MB".to_string())], "{var}");
    assert_eq!(saved.search_path, ["adv_ui2", "public"], "{var}");

    // Reconnecting uses the saved settings; the session answers with them.
    ui.h.state_mut().connect_uri(CONN, uri);
    ui.wait("the reconnect", |a| a.conn(CONN).is_some_and(|c| c.status == Status::Ready && c.sql.is_some()));
    let db = ui.app().conn(CONN).unwrap().sql.clone().unwrap();
    // Through a worker of the test's own, so the answer does not go to the app. A run reads in the session the app's would.
    let (tx, rx) = std::sync::mpsc::channel();
    let w = crate::db::Worker::new(tx, egui::Context::default());
    let read = |q: &str| {
        // No tab schema, so the path is the saved list alone; with one, that schema would come first.
        w.sql_run(9, db.clone(), "".into(), q.into(), true, 0, db::sql::RunHandle::default());
        loop {
            if let Msg::SqlRan { sets, .. } = rx.recv_timeout(Duration::from_secs(20)).unwrap() {
                assert!(sets[0].error.is_none(), "{var}: {:?}", sets[0].error);
                return sets[0].rows[0].decode().unwrap().get_str("v").unwrap().to_string();
            }
        }
    };
    assert_eq!(read("SELECT current_setting('work_mem') AS v"), "9MB", "{var}");
    assert_eq!(read("SELECT current_setting('search_path') AS v").replace('"', ""), "adv_ui2, public", "{var}");
}

#[test]
fn the_postgres_advanced_tab_adds_a_setting_shows_a_refusal_reorders_the_path_and_saves() {
    for var in PG_ENVS.into_iter().chain([PGBOUNCER_ENV]) {
        run(var);
    }
}
