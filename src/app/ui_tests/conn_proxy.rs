//! The Proxy tab of the SQL connection forms: it is offered on MySQL and PostgreSQL, and its settings reach the connection
//! only while neither SSH nor Cloud SQL already carries it. Offline, on the real form.

use super::*;

fn open_form(engine: db::sql::Engine, scheme: &str) -> UiApp {
    let h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_accessibility_check(false).build_eframe(|cc| App::for_test(cc));
    let mut ui = UiApp { h };
    let addr = db::sql::Addr { scheme: scheme.into(), host: "db.internal".into(), user: "app".into(), port: db::sql::Addr::default_port(scheme), ..Default::default() };
    ui.h.state_mut().modal = Some(Modal::Conn(Box::new(ConnForm { name: "shop".into(), engine, addr, tab: ConnTab::Proxy, ..Default::default() })));
    ui.settle();
    ui
}

fn form(app: &mut App) -> &mut ConnForm {
    match app.modal.as_mut() {
        Some(Modal::Conn(f)) => f,
        _ => panic!("the connection form is not open"),
    }
}

#[test]
fn the_sql_forms_have_a_proxy_tab_that_sets_the_connections_proxy() {
    for (engine, scheme) in [(db::sql::Engine::MySql, "mysql"), (db::sql::Engine::Postgres, "postgres")] {
        let mut ui = open_form(engine, scheme);
        ui.show("Connect through a SOCKS5 proxy");
        assert!(form(ui.h.state_mut()).net().proxy.is_none(), "{scheme}: off until ticked");
        ui.click("Connect through a SOCKS5 proxy");
        assert!(form(ui.h.state_mut()).proxy_on);
        let f = form(ui.h.state_mut());
        f.proxy.host = "socks.internal".into();
        f.proxy_pwd = "s3cret".into();
        let p = f.net().proxy.expect("the proxy reaches the connection");
        assert_eq!((p.host.as_str(), p.password.as_deref()), ("socks.internal", Some("s3cret")), "{scheme}");
        // SSH already carries the connection, so the proxy is not used.
        f.ssh_on = true;
        assert!(f.net().proxy.is_none(), "{scheme}: SSH wins");
    }
}
