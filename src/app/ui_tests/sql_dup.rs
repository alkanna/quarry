//! Show the other row on a SQL duplicate-key toast, offline on the demo: PostgreSQL names the column, MySQL the index,
//! whose columns the app reads before it can filter.

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

#[test]
fn a_postgresql_duplicate_opens_the_table_filtered_on_its_value() {
    let Some(mut ui) = demo_ui(App::load_pg_demo) else { return };
    dup_toast(&mut ui, "error returned from database: 23505: duplicate key value violates unique constraint \"orders_number_key\"\nDetail: Key (number)=(4410) already exists.");
    ui.click("Show the other row");
    let t = ui.app().t().expect("a tab");
    assert!(t.view == View::Documents, "the filter did not open Content");
    assert_eq!(t.filter_txt, "\"number\" = '4410'");
}

#[test]
fn a_quoted_postgresql_column_is_quoted_once() {
    let Some(mut ui) = demo_ui(App::load_pg_demo) else { return };
    dup_toast(&mut ui, "error returned from database: 23505: duplicate key value violates unique constraint \"u\"\nDetail: Key (\"Email\")=(a@b.c) already exists.");
    ui.click("Show the other row");
    assert_eq!(ui.app().t().map(|t| t.filter_txt.clone()).as_deref(), Some("\"Email\" = 'a@b.c'"));
}

#[test]
fn a_postgresql_key_of_two_columns_offers_no_filter() {
    let Some(mut ui) = demo_ui(App::load_pg_demo) else { return };
    dup_toast(&mut ui, "error returned from database: 23505: duplicate key value violates unique constraint \"orders_a_b_key\"\nDetail: Key (a, b)=(1, 2) already exists.");
    assert!(ui.h.query_all_by_label("Show the other row").next().is_none(), "a composite key offered a filter");
}

#[test]
fn a_mysql_duplicate_filters_on_the_one_column_of_its_index() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    let sel = dup_toast(&mut ui, "error returned from database: 1062 (23000): Duplicate entry 'ana@example.org' for key 'orders.uq_email'");
    ui.show("Show the other row");
    // The click reads the table's indexes from the server; the demo has none, so the answer is handed in here.
    let index = |name: &str, cols: &[&str]| Index { name: name.into(), cols: cols.iter().map(|c| IndexCol { name: c.to_string(), ..Default::default() }).collect(), ..Default::default() };
    let st = Structure { indexes: vec![index("uq_email", &["email"]), index("uq_pair", &["a", "b"])], ..Default::default() };
    let app = ui.h.state_mut();
    app.sql_dup_index(sel.clone(), "uq_email", "ana@example.org".into(), Ok(st.clone()));
    assert_eq!(app.t().map(|t| t.filter_txt.clone()).as_deref(), Some("`email` = 'ana@example.org'"));
    let before = app.t().map(|t| t.filter_txt.clone());
    app.sql_dup_index(sel, "uq_pair", "1-2".into(), Ok(st));
    assert_eq!(app.t().map(|t| t.filter_txt.clone()), before, "a two-column index wrote a filter");
    assert!(app.last_problem().contains("several columns"), "{}", app.last_problem());
}

#[test]
fn a_mysql_value_cut_at_64_characters_offers_no_filter() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    let long = "x".repeat(64);
    dup_toast(&mut ui, &format!("error returned from database: 1062 (23000): Duplicate entry '{long}' for key 'orders.uq_note'"));
    assert!(ui.h.query_all_by_label("Show the other row").next().is_none(), "a value MySQL may have cut offered a filter");
}
