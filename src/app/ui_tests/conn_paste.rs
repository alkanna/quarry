//! The paste box of the MySQL and MariaDB connection form: mysql:// and mariadb:// strings fill the fields, and a string of another
//! engine is refused with the card to pick. Offline, on the real form.

use super::sql_structure::replace;
use super::*;

fn open_form(scheme: &str) -> UiApp {
    let h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_accessibility_check(false).build_eframe(|cc| App::for_test(cc));
    let mut ui = UiApp { h };
    let addr = db::sql::Addr { scheme: scheme.into(), port: 3306, ..Default::default() };
    ui.h.state_mut().modal = Some(Modal::Conn(Box::new(ConnForm { name: "shop".into(), engine: db::sql::Engine::MySql, addr, tab: ConnTab::Standard, ..Default::default() })));
    ui.settle();
    ui
}

fn form(app: &App) -> &ConnForm {
    match &app.modal {
        Some(Modal::Conn(f)) => f,
        _ => panic!("the connection form is not open"),
    }
}

#[test]
fn a_mysql_string_fills_the_fields_and_moves_its_password_to_the_masked_field() {
    let mut ui = open_form("mysql");
    replace(&mut ui, "Paste connection string", "mysql://app:s3cret@db.internal:3307/shop");
    ui.show_contains("Filled host, port, user, password, database");
    let a = &form(ui.app()).addr;
    assert_eq!((a.host.as_str(), a.port, a.user.as_str(), a.password.as_str(), a.database.as_str()), ("db.internal", 3307, "app", "s3cret", "shop"));
    assert_eq!(a.scheme, "mysql");
}

#[test]
fn a_mariadb_string_keeps_its_scheme_and_takes_its_tls_mode() {
    let mut ui = open_form("mariadb");
    replace(&mut ui, "Paste connection string", "mariadb://app@maria.internal:3306/orders?ssl-mode=VERIFY_CA");
    ui.show_contains("and TLS");
    ui.show_contains("No password in the URL");
    let a = &form(ui.app()).addr;
    assert_eq!((a.host.as_str(), a.user.as_str(), a.database.as_str()), ("maria.internal", "app", "orders"));
    assert_eq!(a.scheme, "mariadb", "the MariaDB card lost its scheme");
    assert_eq!(a.ssl_mode, db::sql::SslMode::VerifyCa);
}

#[test]
fn a_string_of_another_engine_is_refused_and_leaves_the_form_alone() {
    let mut ui = open_form("mysql");
    replace(&mut ui, "Paste connection string", "postgresql://analyst@pg.internal:5432/analytics");
    ui.show_contains("Pick the PostgreSQL card to paste it");
    assert!(form(ui.app()).addr.host.is_empty(), "a postgresql:// string filled the MySQL form");
    replace(&mut ui, "Paste connection string", "mongodb://localhost:27017");
    ui.show_contains("Pick the MongoDB card to paste it");
    assert!(form(ui.app()).addr.host.is_empty());
}
