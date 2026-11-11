//! Importing a CSV file into a SQL table through the UI, on every SQL server whose environment variable is set: MySQL, MariaDB,
//! PostgreSQL 18 and 13. The file is handed to the dialog as memory, never through the system file picker, and what the server
//! holds is read with a plain query after each step.

use super::sql_edit::{every_engine, open_unlocked, ORIGINAL};
use super::sql_structure::undo_last;
use super::*;
use db::sql::csv::Source;

/// Opens the Import dialog of the open table over `csv` and waits until the check of the file has finished.
fn open_import(ui: &mut UiApp, csv: &str, rows: usize) {
    let sel = ui.app().t().map(|t| t.sel.clone()).expect("a table is open");
    let src = Source::Mem(Arc::new(csv.as_bytes().to_vec()));
    ui.h.state_mut().open_sql_import_with(CONN, sel.db, sel.schema, sel.coll, Some(src));
    ui.show(&format!("Import {rows} rows"));
}

#[test]
fn a_csv_file_imports_updates_and_undoes_on_every_engine() {
    every_engine("csv import", |fx, ui| {
        let t = open_unlocked(fx, ui);
        // A duplicate key stops the import with nothing written.
        open_import(ui, "id,name,qty\n4,delta,40\n1,dup,0\n", 2);
        ui.click("Import 2 rows");
        ui.wait("the refusal", |a| a.last_problem().to_lowercase().contains("duplicate"));
        fx.expect_unchanged(ui, &t, ORIGINAL);
        // New rows go in, the open table shows them, and one undo takes exactly them out again.
        open_import(ui, "id,name,qty\n4,delta,40\n5,eps,\n", 2);
        ui.click("Import 2 rows");
        fx.expect(ui, &t, "1:alpha:10,2:beta:20,3:gamma:30,4:delta:40");
        ui.wait("the open table to reload", |a| a.t().is_some_and(|t| t.docs.len() == 5));
        undo_last(ui, 1, &t);
        fx.expect(ui, &t, ORIGINAL);
        ui.wait("the open table to follow the undo", |a| a.t().is_some_and(|t| t.docs.len() == 3));
        // Skip leaves the row that exists, Update overwrites it.
        open_import(ui, "id,name,qty\n1,skipped,0\n6,zeta,60\n", 2);
        ui.click_last("Skip");
        ui.click("Import 2 rows");
        fx.expect(ui, &t, "1:alpha:10,2:beta:20,3:gamma:30,6:zeta:60");
        open_import(ui, "id,name,qty\n1,updated,11\n7,eta,70\n", 2);
        ui.click_last("Update");
        ui.click("Import 2 rows");
        fx.expect(ui, &t, "1:updated:11,2:beta:20,3:gamma:30,6:zeta:60,7:eta:70");
        if fx.is_pg() {
            // PostgreSQL records what an update overwrote, so its undo puts the old row back and removes the new one.
            undo_last(ui, 2, &t);
            fx.expect(ui, &t, "1:alpha:10,2:beta:20,3:gamma:30,6:zeta:60");
        }
    });
}
