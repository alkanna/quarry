//! The Export (SQL dump) and Run SQL file dialogs through the UI, on every SQL server whose environment variable is set:
//! MySQL, MariaDB, PostgreSQL 18 and 13. A table is dumped from the dialog on a locked connection, dropped on the server, and the
//! file is run back through the other dialog once writes are allowed. The native file dialogs are never opened: the dump's path
//! is set in the form, and the run dialog is opened with the file already chosen.

use super::sql_edit::{every_engine, ORIGINAL};
use super::*;
use db::sql::csv::Source;

/// Clicks the enabled button whose accessible name starts with `starts`, once it is on screen. The dialog's title shares the
/// start of the Export button's name, so a label query would find two.
fn press(ui: &mut UiApp, starts: &str) {
    let t = std::time::Instant::now();
    loop {
        ui.h.step();
        let hit = ui.h.query_all_by_role(egui::accesskit::Role::Button).find(|n| n.accesskit_node().label().is_some_and(|l| l.starts_with(starts)) && !n.accesskit_node().is_disabled());
        if let Some(n) = hit {
            n.click();
            break;
        }
        assert!(t.elapsed() < LIMIT, "timed out waiting for the button \"{starts}\"; last error: {}", ui.app().last_problem());
        std::thread::sleep(Duration::from_millis(10));
    }
    ui.settle();
}

#[test]
fn a_table_is_dumped_from_the_dialog_and_the_file_runs_back_from_the_other() {
    every_engine("sql dump", |fx, ui| {
        let table = fx.table();
        fx.open(ui, &table);
        let (db, _) = fx.place();
        let path = std::env::temp_dir().join(format!("quarry_ui_dump_{}_{table}.sql", std::process::id()));
        // The connection is locked, and a dump only reads.
        assert!(!ui.app().conn_writable(CONN), "the fixture connection starts locked");
        ui.h.state_mut().open_sql_dump(CONN, db.into(), Some(table.clone()));
        ui.h.state_mut().set_dump_path(&path.display().to_string());
        ui.settle();
        ui.show("Export");
        press(ui, "Export");
        ui.wait("the dump to finish", |a| a.export_job.is_none());
        let text = std::fs::read_to_string(&path).unwrap_or_else(|x| panic!("no dump file: {x}; last error: {}", ui.app().last_problem()));
        assert!(text.contains("CREATE TABLE") && text.contains("alpha") && text.contains("gamma"), "{text}");
        if fx.place().1 == "public" {
            assert!(text.contains(&format!("INSERT INTO \"public\".\"{table}\"")) && text.contains("SET search_path = '';"), "the PostgreSQL dump is qualified and pins its path: {text}");
        }

        // The table is gone, and the file brings it back.
        fx.sql(&format!("DROP TABLE {}", fx.qualified(&table)));
        ui.unlock();
        ui.h.state_mut().open_run_form(CONN, db.into(), Source::File(path.clone()), "dump.sql".into());
        ui.settle();
        ui.show("Run SQL file");
        press(ui, "Run file");
        ui.wait("the script to finish", |a| a.sql_job.is_none());
        fx.expect(ui, &table, ORIGINAL);
        let _ = std::fs::remove_file(path);
    });
}
