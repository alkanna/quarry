//! The title bar path as crumbs and the two-line tabs, on the offline demo.
use super::*;

#[test]
fn the_second_line_puts_the_connection_before_the_database() {
    assert_eq!(super::super::crumbs::tab_second_line("Local sample", "shop"), "Local sample \u{b7} shop");
    assert_eq!(super::super::crumbs::tab_second_line("Local sample", ""), "Local sample");
    assert_eq!(super::super::crumbs::tab_second_line("", "shop"), "shop");
}

#[test]
fn the_same_table_on_two_connections_reads_differently() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    let a = ui.h.state_mut();
    a.conns.push(Conn::new(ConnMeta { id: 9, name: "Billing".into(), engine: db::sql::Engine::MySql, tag: "Production".into(), ..Default::default() }, Status::Ready));
    a.tabs.push(Tab::new(Sel { conn: 9, db: "shop".into(), coll: "orders".into(), ..Default::default() }));
    let line = |a: &App, i: usize| super::super::crumbs::tab_second_line(a.conn(a.tabs[i].sel.conn).map_or("", |c| c.meta.name.as_str()), a.tabs[i].sel.scope_label());
    let (first, last) = (a.tabs.iter().position(|t| t.sel.coll == "orders" && t.sel.conn == 1).unwrap(), a.tabs.len() - 1);
    assert_ne!(line(a, first), line(a, last));
    assert!(line(a, last).starts_with("Billing"));
    ui.settle();
    // Both tabs keep the accessible name the drag and tab tests rely on.
    assert_eq!(ui.h.get_all_by_label("Tab orders").count(), 2);
}

#[test]
fn picking_a_database_in_the_crumb_reveals_it_in_the_explorer() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    assert!(!ui.app().open_dbs.contains(&(1, "analytics".to_string())));
    ui.click("Database crumb");
    ui.click("Database analytics");
    ui.settle();
    assert!(ui.app().open_dbs.contains(&(1, "analytics".to_string())), "the explorer did not expand the picked database");
    assert_eq!(ui.app().prefs.recent_dbs(1).first().map(String::as_str), Some("analytics"));
}

#[test]
fn picking_a_database_in_a_query_tab_changes_its_database() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    let a = ui.h.state_mut();
    a.conns.push(Conn { dbs: vec![("shop".into(), Some(vec!["orders".into()])), ("billing".into(), Some(Vec::new()))], ..Conn::new(ConnMeta { id: 9, name: "Shop SQL".into(), engine: db::sql::Engine::MySql, ..Default::default() }, Status::Ready) });
    let mut t = Tab::new(Sel { conn: 9, db: "shop".into(), ..Default::default() });
    t.view = View::Sql;
    a.tabs.push(t);
    let last = a.tabs.len() - 1;
    a.set_cur(last);
    ui.settle();
    ui.click("Database crumb");
    ui.click("Database billing");
    ui.settle();
    assert_eq!(ui.app().t().map(|t| t.sel.db.as_str()), Some("billing"));
}

#[test]
fn the_table_crumb_opens_the_picked_table() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    ui.click("Table crumb");
    ui.click("Table carts");
    ui.settle();
    assert_eq!(ui.app().t().map(|t| t.sel.coll.as_str()), Some("carts"));
}

#[test]
fn the_title_bar_has_no_settings_button() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    ui.settle();
    assert!(ui.h.query_by_label("Settings").is_none());
    assert!(ui.h.query_by_label("Database crumb").is_some());
}
