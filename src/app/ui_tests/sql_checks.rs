//! The CHECK constraints section of the Structure view and its Add and Edit check dialog, and the PostgreSQL index form's Method,
//! Where, Include and CONCURRENTLY options with the index usage note, offline on the demo structures.

use super::sql_structure::replace;
use super::sql_tableops::{disabled, shows};
use super::*;
use db::sql::ddl::Change;

/// The MySQL demo on the Structure view of orders, with nothing selected in its grids so each button name is the section's own.
fn my_structure() -> Option<UiApp> {
    demo_ui(|a| {
        a.load_sql_demo();
        a.sql_structure_demo("structure");
        a.tabs[0].sql.as_mut().expect("the orders tab").st.sel = [None; 6];
    })
}

/// The PostgreSQL demo on the Structure view of orders, with nothing selected in its grids.
fn pg_structure() -> Option<UiApp> {
    demo_ui(|a| {
        a.load_pg_demo();
        a.pg_structure_demo("structure");
        a.tabs[0].sql.as_mut().expect("the orders tab").st.sel = [None; 6];
    })
}

/// Changes the structure the tab holds, as if the server had read it so.
fn touch(ui: &mut UiApp, f: impl FnOnce(&mut db::sql::structure::Structure)) {
    f(ui.h.state_mut().tabs[0].sql.as_mut().and_then(|s| s.st.data.as_mut()).expect("the structure"));
    ui.settle();
}

fn staged(ui: &UiApp) -> Vec<Change> {
    ui.app().staged().to_vec()
}

/// A double click on a row of a structure grid, which opens its edit dialog.
fn open_row(ui: &mut UiApp, row: &str) {
    ui.show(row);
    let at = ui.h.get_by_label(row).rect().center();
    ui.press_at(at, egui::PointerButton::Primary, 2);
    ui.settle();
}

#[test]
fn the_checks_of_the_table_are_listed_in_their_section() {
    let Some(mut ui) = my_structure() else { return };
    ui.show("CHECK CONSTRAINTS");
    ui.show("checks row 1");
    ui.show("checks row 2");
    assert!(!shows(&ui, "This table has no CHECK constraints."), "a table with checks says it has none");
    ui.h.state_mut().tabs[0].sql.as_mut().and_then(|s| s.st.data.as_mut()).expect("the structure").checks.clear();
    ui.settle();
    ui.show("This table has no CHECK constraints.");
    assert!(ui.h.query_by_label("checks row 1").is_none(), "a row is drawn for a check that is gone");
}

#[test]
fn a_locked_connection_cannot_add_a_check() {
    let Some(mut ui) = my_structure() else { return };
    ui.show("+ Check");
    assert!(!disabled(&ui, "+ Check"), "an unlocked connection cannot add a check");
    for c in &mut ui.h.state_mut().conns {
        c.writable = false;
    }
    ui.settle();
    assert!(disabled(&ui, "+ Check"), "a locked connection can add a check");
}

#[test]
fn adding_a_check_previews_its_statement_and_stages_one_change() {
    let Some(mut ui) = my_structure() else { return };
    ui.click("+ Check");
    assert!(matches!(ui.app().modal, Some(Modal::CheckEdit(_))), "+ Check did not open the dialog");
    // The name starts from the table's, so only the condition is left to type.
    ui.show("Check name");
    assert!(shows(&ui, "Write the condition every row must meet."), "an empty condition is not explained");
    assert!(disabled(&ui, "Stage change"), "an empty condition armed Stage change");
    replace(&mut ui, "Check condition", "total < 100000");
    assert!(shows(&ui, "chk_orders"), "the preview does not name the constraint");
    assert!(shows(&ui, "CHECK (total < 100000)"), "the preview does not hold the condition");
    assert!(!disabled(&ui, "Stage change"), "a condition did not arm Stage change");
    ui.click("Keep it without enforcing it (NOT ENFORCED)");
    assert!(shows(&ui, "NOT ENFORCED"), "the option is not in the preview");
    ui.click("Stage change");
    assert!(ui.app().modal.is_none(), "staging left the dialog open");
    match staged(&ui).as_slice() {
        [Change::AddCheck(c)] => assert!(c.name == "chk_orders" && c.expr == "total < 100000" && c.not_enforced, "the staged check is not what the dialog showed: {c:?}"),
        other => panic!("expected one staged AddCheck, got {} changes", other.len()),
    }
}

#[test]
fn a_name_the_table_already_has_blocks_the_check() {
    let Some(mut ui) = my_structure() else { return };
    ui.click("+ Check");
    ui.show("Check name");
    replace(&mut ui, "Check name", "chk_orders_total");
    replace(&mut ui, "Check condition", "total < 5");
    assert!(shows(&ui, "The table already has a check constraint called chk_orders_total."), "no note says the name is taken");
    assert!(disabled(&ui, "Stage change"), "a taken name armed Stage change");
    ui.click("Cancel");
    assert!(ui.app().modal.is_none() && staged(&ui).is_empty(), "Cancel left something behind");
}

#[test]
fn editing_a_check_stages_a_drop_and_an_add_together() {
    let Some(mut ui) = my_structure() else { return };
    open_row(&mut ui, "checks row 1");
    assert!(matches!(ui.app().modal, Some(Modal::CheckEdit(_))), "a double click on a check did not open its dialog");
    ui.show("Check condition");
    assert!(shows(&ui, "Nothing is changed yet."), "an untouched edit offers a statement");
    assert!(disabled(&ui, "Stage change"), "an untouched edit armed Stage change");
    replace(&mut ui, "Check condition", "`total` >= 1");
    assert!(!disabled(&ui, "Stage change"));
    ui.click("Stage change");
    match staged(&ui).as_slice() {
        [Change::DropCheck(old), Change::AddCheck(new)] => assert!(old == "chk_orders_total" && new.name == "chk_orders_total" && new.expr == "`total` >= 1", "the pair is not the old check and its replacement"),
        other => panic!("expected a staged drop and add, got {} changes", other.len()),
    }
}

#[test]
fn dropping_a_check_stages_the_drop_and_locks_the_check_against_another_edit() {
    let Some(mut ui) = my_structure() else { return };
    ui.click("checks row 2");
    ui.click("Drop");
    assert!(matches!(staged(&ui).as_slice(), [Change::DropCheck(n)] if n == "chk_orders_currency"), "the drop is not staged");
    open_row(&mut ui, "checks row 2");
    assert!(ui.app().modal.is_none(), "a check with a staged drop opened its dialog");
    assert!(ui.app().last_problem().contains("already has a staged change"), "no notice says why: {}", ui.app().last_problem());
}

#[test]
fn the_index_usage_note_says_since_when_the_counts_run_or_why_there_are_none() {
    let Some(mut ui) = pg_structure() else { return };
    touch(&mut ui, |d| {
        d.indexes[0].scans = Some(120);
        d.indexes[2].scans = Some(0);
        d.usage.reset = "2026-11-10T08:00:12Z".into();
    });
    ui.show("use counted since 2026-11-10 08:00");
    touch(&mut ui, |d| d.usage.unavailable = "pg_stat_user_indexes cannot be read".into());
    ui.show("use not counted");
    assert!(!shows(&ui, "use counted"), "an unreadable count shows a since note");
}

#[test]
fn the_postgres_index_form_offers_method_where_include_and_concurrently() {
    let Some(mut ui) = pg_structure() else { return };
    open_row(&mut ui, "indexes row 3");
    assert!(matches!(ui.app().modal, Some(Modal::IdxEdit(_))), "a double click on an index did not open its dialog");
    ui.show("Index method");
    assert!(shows(&ui, "Nothing is changed yet."), "an untouched edit offers a statement");
    ui.click("Index method");
    ui.click("hash");
    assert!(shows(&ui, "USING hash"), "the method is not in the preview");
    // Only btree takes INCLUDE, so the rest of the form is tried on it.
    ui.click("Index method");
    ui.click("btree");
    assert!(!shows(&ui, "USING hash"), "the method stayed after btree was picked");
    replace(&mut ui, "Index condition", "status <> 'cancelled'");
    assert!(shows(&ui, "WHERE status <> 'cancelled'"), "the condition is not in the preview");
    replace(&mut ui, "Included columns", "total, note");
    assert!(shows(&ui, r#"INCLUDE ("total", "note")"#), "the included columns are not in the preview");
    ui.click("Build concurrently");
    assert!(shows(&ui, r#"CREATE INDEX CONCURRENTLY "idx_orders_status""#), "CONCURRENTLY is not in the preview");
    assert!(shows(&ui, "cannot run inside a transaction block"), "the preview does not say it runs on its own");
    ui.click("Stage change");
    match staged(&ui).as_slice() {
        [Change::DropIndex(old), Change::AddIndex(new)] => {
            assert_eq!(old, "idx_orders_status");
            assert!(new.method.is_empty() && new.predicate == "status <> 'cancelled'" && new.include == ["total", "note"] && new.concurrently, "the staged index is not what the form showed");
        }
        other => panic!("expected a staged drop and add, got {} changes", other.len()),
    }
}

#[test]
fn a_key_constraint_takes_no_method_condition_or_concurrently() {
    let Some(mut ui) = pg_structure() else { return };
    // orders_public_id_key backs a unique constraint, so it is plain btree on its columns.
    open_row(&mut ui, "indexes row 2");
    ui.show("Index method");
    assert!(disabled(&ui, "Index method"), "a constraint's index offers a method");
    assert!(disabled(&ui, "Index condition"), "a constraint's index offers a condition");
    assert!(disabled(&ui, "Build concurrently"), "a constraint's index offers CONCURRENTLY");
    // INCLUDE is the one option a unique constraint has.
    assert!(!disabled(&ui, "Included columns"));
}
