//! The Edit table options dialog and the MySQL New table dialog, offline on the demo data.

use super::sql_structure::replace;
use super::*;
use crate::app::sql::new_keys::NewFk;
use db::sql::ddl::{Change, TableOpt};
use db::sql::structure::Structure;
use db::sql::Dialect;

fn my_orders() -> Option<UiApp> {
    demo_ui(|a| {
        a.load_sql_demo();
        a.sql_structure_demo("structure");
    })
}

fn table_changes(ui: &UiApp) -> Vec<Change> {
    ui.app().staged().iter().filter(|c| matches!(c, Change::Table { .. })).cloned().collect()
}

fn open_opts(ui: &mut UiApp) {
    ui.h.state_mut().open_table_opts();
    ui.settle();
    assert!(matches!(ui.app().modal, Some(Modal::TableOpts(_))), "the dialog did not open");
}

#[test]
fn reopening_table_options_starts_from_the_staged_values_and_replaces_them() {
    let Some(mut ui) = my_orders() else { return };
    let server = ui.app().structure().expect("the structure").status.comment.clone();
    open_opts(&mut ui);
    replace(&mut ui, "Table comment", "first");
    ui.click("Stage change");
    assert!(matches!(table_changes(&ui).as_slice(), [Change::Table { opt: TableOpt::Comment(c), was: TableOpt::Comment(w) }] if c == "first" && *w == server));
    // the form shows what is staged and offers it again, not the server's value
    open_opts(&mut ui);
    assert!(ui.shows("COMMENT = 'first'"), "the dialog did not start from the staged comment");
    replace(&mut ui, "Table comment", "second");
    ui.click("Stage change");
    assert!(matches!(table_changes(&ui).as_slice(), [Change::Table { opt: TableOpt::Comment(c), .. }] if c == "second"), "a second Table change was stacked: {} entries", table_changes(&ui).len());
    // back to the server's value: nothing differs, and staging takes the entry back
    open_opts(&mut ui);
    replace(&mut ui, "Table comment", &server);
    if server.is_empty() {
        ui.h.key_press(egui::Key::Backspace);
    }
    ui.settle();
    ui.click("Take back");
    assert!(table_changes(&ui).is_empty(), "the staged option stayed");
}

#[test]
fn the_parent_of_a_foreign_key_reaches_the_mysql_new_table_dialog() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_sql_demo();
        a.sql_create_demo("newtable");
    }) else { return };
    let Some(Modal::SqlNewTable(f)) = ui.h.state_mut().modal.as_mut() else { panic!("the New table dialog is not open") };
    let mut k = NewFk::new("shop", Dialect::MySql);
    k.req = 9001;
    f.keys.fks.push(k);
    ui.app().worker.send(Msg::SqlStructure { req: 9001, res: Ok(Structure::default()) });
    ui.wait("the parent to arrive", |a| matches!(&a.modal, Some(Modal::SqlNewTable(f)) if f.keys.fks.first().is_some_and(|k| k.parent.is_some())));
}
