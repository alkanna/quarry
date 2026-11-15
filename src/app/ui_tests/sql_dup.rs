//! Show the other row on a SQL duplicate-key toast, offline on the demo. The click reads the table's indexes from the
//! server, which the demo has none of, so those answers are handed to `sql_dup_index` here.

use super::*;
use crate::db::sql::structure::{Index, IndexCol, Structure};

fn dup_toast(ui: &mut UiApp, raw: &str) -> Sel {
    let app = ui.h.state_mut();
    let sel = app.tabs[app.cur].sel.clone();
    let e = app.log_err(raw, Some(sel.conn), Some(sel.clone()), "Content".into(), "");
    app.err_toast(e);
    ui.settle();
    sel
}

fn offered(ui: &UiApp) -> bool {
    ui.h.query_all_by_label("Show the other row").next().is_some()
}

fn indexes(list: &[(&str, &[&str])]) -> Structure {
    let index = |name: &str, cols: &[&str]| Index { name: name.into(), cols: cols.iter().map(|c| IndexCol { name: c.to_string(), ..Default::default() }).collect(), ..Default::default() };
    Structure { indexes: list.iter().map(|(n, c)| index(n, c)).collect(), ..Default::default() }
}

fn filter(app: &App) -> String {
    app.t().map(|t| t.filter_txt.clone()).unwrap_or_default()
}

const PG_DUP: &str = "error returned from database: 23505: duplicate key value violates unique constraint \"orders_number_key\"\nDetail: Key (number)=(4410) already exists.";

#[test]
fn a_postgresql_duplicate_filters_on_the_column_its_index_covers() {
    let Some(mut ui) = demo_ui(App::load_pg_demo) else { return };
    let sel = dup_toast(&mut ui, PG_DUP);
    assert!(offered(&ui), "no button for a one-column key");
    let app = ui.h.state_mut();
    app.sql_dup_index(sel.clone(), "orders_number_key", "4410".into(), Ok(indexes(&[("orders_number_key", &["number"])])));
    assert!(app.t().is_some_and(|t| t.view == View::Documents), "the filter did not open Content");
    assert_eq!(filter(app), "\"number\" = '4410'");
    // A mixed-case column comes from the table, quoted once; a NULL key, printed as null, matches NULL too.
    app.sql_dup_index(sel.clone(), "u", "a@b.c".into(), Ok(indexes(&[("u", &["Email"])])));
    assert_eq!(filter(app), "\"Email\" = 'a@b.c'");
    app.sql_dup_index(sel, "u", "null".into(), Ok(indexes(&[("u", &["Email"])])));
    assert_eq!(filter(app), "(\"Email\" IS NULL OR \"Email\" = 'null')");
}

#[test]
fn a_postgresql_key_of_two_columns_offers_no_filter() {
    let Some(mut ui) = demo_ui(App::load_pg_demo) else { return };
    dup_toast(&mut ui, "error returned from database: 23505: duplicate key value violates unique constraint \"orders_a_b_key\"\nDetail: Key (a, b)=(1, 2) already exists.");
    assert!(!offered(&ui), "a composite key offered a filter");
}

#[test]
fn a_mysql_duplicate_filters_on_the_one_column_of_its_index() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    let sel = dup_toast(&mut ui, "error returned from database: 1062 (23000): Duplicate entry 'ana@example.org' for key 'orders.uq_email'");
    assert!(offered(&ui), "no button for a key of this table");
    let st = indexes(&[("uq_email", &["email"]), ("uq_pair", &["a", "b"])]);
    let app = ui.h.state_mut();
    app.sql_dup_index(sel.clone(), "uq_email", "ana@example.org".into(), Ok(st.clone()));
    assert_eq!(filter(app), "`email` = 'ana@example.org'");
    let before = filter(app);
    app.sql_dup_index(sel.clone(), "uq_pair", "1-2".into(), Ok(st.clone()));
    assert!(app.last_problem().contains("several columns"), "{}", app.last_problem());
    // An index this table does not have: the error was about another table.
    app.sql_dup_index(sel, "PRIMARY", "42".into(), Ok(st));
    assert!(app.last_problem().contains("another table"), "{}", app.last_problem());
    assert_eq!(filter(app), before, "a refused key wrote a filter");
}

#[test]
fn a_mysql_key_of_another_table_a_cut_value_or_binary_bytes_offer_no_filter() {
    for value in ["x".repeat(61), "\\x8F\\x01".to_string()] {
        let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
        dup_toast(&mut ui, &format!("error returned from database: 1062 (23000): Duplicate entry '{value}' for key 'orders.uq_note'"));
        assert!(!offered(&ui), "{value} offered a filter");
    }
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    dup_toast(&mut ui, "error returned from database: 1062 (23000): Duplicate entry '42' for key 'customers.PRIMARY'");
    assert!(!offered(&ui), "a key of another table offered a filter on this one");
}
