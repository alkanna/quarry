//! Sorting and the column menu of Query results, offline on the demo data.

use super::*;

fn query_demo() -> Option<UiApp> {
    demo_ui(|a| {
        a.load_sql_demo();
        a.sql_query_demo();
    })
}

/// The `name` column of the first result, in the order the grid holds its rows.
fn names(ui: &UiApp) -> Vec<String> {
    let s = ui.app().t().and_then(|t| t.sql.as_ref()).expect("a SQL tab");
    s.sets[0].rows.iter().map(|r| r.raw().get_str("name").unwrap_or_default().to_string()).collect()
}

#[test]
fn the_column_menu_sorts_the_page_and_unsort_restores_the_servers_order() {
    let Some(mut ui) = query_demo() else { return };
    let server = names(&ui);
    ui.click("Result column orders");
    ui.click("Sort ascending on this page");
    ui.settle();
    let sorted = names(&ui);
    // Ties (two with 4 orders, two with 3) keep the order the server sent.
    assert_eq!(sorted.first().map(String::as_str), Some("Dennis Ritchie"));
    assert_eq!(sorted.last().map(String::as_str), Some("Ada Byron"));
    assert!(sorted.iter().position(|n| n == "Ken Thompson") < sorted.iter().position(|n| n == "Barbara Liskov"), "a tie lost the server's order");
    ui.show("Sorted on this page by orders ascending. The query did not run again.");
    ui.click("Unsort");
    ui.settle();
    assert_eq!(names(&ui), server, "Unsort did not bring the server's order back");
}

#[test]
fn a_selected_row_follows_its_row_through_a_sort() {
    let Some(mut ui) = query_demo() else { return };
    let cur = ui.app().cur;
    // Ada, the first row, goes to the end of an ascending sort on orders.
    if let Some(s) = ui.h.state_mut().tabs[cur].sql.as_mut() {
        s.qsel = Some(0);
        s.qchecked.insert(0);
    }
    ui.h.state_mut().sql_sort_page(vec![("orders".into(), false)]);
    ui.settle();
    let s = ui.app().t().and_then(|t| t.sql.as_ref()).expect("a SQL tab");
    let last = s.sets[0].rows.len() - 1;
    assert_eq!(s.qsel, Some(last));
    assert!(s.qchecked.contains(&last) && s.qchecked.len() == 1);
}

#[test]
fn hidden_columns_can_be_shown_again_and_a_pinned_one_comes_first() {
    let Some(mut ui) = query_demo() else { return };
    ui.click("Result column email");
    ui.click("Hide column");
    ui.settle();
    assert!(ui.h.query_all_by_label("Result column email").next().is_none(), "the hidden column is still drawn");
    ui.show("1 hidden");
    ui.click("Show all columns");
    ui.show("Result column email");
    ui.click("Result column spent");
    ui.click("Pin to the left");
    ui.settle();
    let s = ui.app().t().and_then(|t| t.sql.as_ref()).expect("a SQL tab");
    let names: Vec<String> = s.sets[0].cols.iter().map(|c| c.name.clone()).collect();
    let shown = crate::app::table::visible_cols(s.qprefs.get(&names), &names, usize::MAX);
    assert_eq!(shown.first().map(String::as_str), Some("spent"));
}
