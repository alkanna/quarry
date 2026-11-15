//! The MongoDB connection form's fields, a view over the one stored address: typing in a field rewrites the address and
//! keeps its other options, and a pasted address fills the fields. Offline, on the real form.

use super::sql_structure::replace;
use super::*;

const URI: &str = "mongodb://app:s3cret@a:27017,b:27018/shop?retryWrites=true&w=majority&appName=quarry";

fn open_form(uri: &str, tab: ConnTab) -> UiApp {
    let h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_accessibility_check(false).build_eframe(|cc| App::for_test(cc));
    let mut ui = UiApp { h };
    ui.h.state_mut().modal = Some(Modal::Conn(Box::new(ConnForm { name: "docs".into(), uri: uri.into(), tab, ..Default::default() })));
    ui.settle();
    ui
}

fn form(app: &mut App) -> &mut ConnForm {
    match app.modal.as_mut() {
        Some(Modal::Conn(f)) => f,
        _ => panic!("the connection form is not open"),
    }
}

fn value_of(ui: &mut UiApp, field: &str) -> String {
    ui.show(field);
    ui.h.get_by_label(field).value().unwrap_or_default().to_string()
}

#[test]
fn typing_in_a_field_rewrites_the_address_and_keeps_its_other_options() {
    let mut ui = open_form(URI, ConnTab::Standard);
    assert_eq!(value_of(&mut ui, "Hosts field"), "a:27017,b:27018");
    assert_eq!(value_of(&mut ui, "Database field"), "shop");
    replace(&mut ui, "Replica set field", "rs0");
    ui.settle();
    assert_eq!(form(ui.h.state_mut()).uri, "mongodb://app:s3cret@a:27017,b:27018/shop?retryWrites=true&w=majority&appName=quarry&replicaSet=rs0");
    replace(&mut ui, "Auth source field", "admin");
    ui.settle();
    assert!(form(ui.h.state_mut()).uri.ends_with("&replicaSet=rs0&authSource=admin"), "{}", form(ui.h.state_mut()).uri);
    // A password with characters the address must encode is written encoded and read back as typed.
    replace(&mut ui, "Password field", "p@ss/w0rd");
    ui.settle();
    assert!(form(ui.h.state_mut()).uri.starts_with("mongodb://app:p%40ss%2Fw0rd@a:27017"), "{}", form(ui.h.state_mut()).uri);
    assert_eq!(db::mongo_uri::MongoUri::parse(&form(ui.h.state_mut()).uri).unwrap().password(), "p@ss/w0rd");
}

#[test]
fn a_pasted_address_fills_the_fields() {
    let mut ui = open_form("mongodb://localhost:27017", ConnTab::Standard);
    assert_eq!(value_of(&mut ui, "Hosts field"), "localhost:27017");
    replace(&mut ui, "URI field", "mongodb+srv://ada:pw@cluster0.abcde.mongodb.net/orders?replicaSet=atlas-1&readPreference=nearest&authSource=admin");
    ui.settle();
    assert_eq!(value_of(&mut ui, "Hosts field"), "cluster0.abcde.mongodb.net");
    assert_eq!(value_of(&mut ui, "User field"), "ada");
    assert_eq!(value_of(&mut ui, "Database field"), "orders");
    assert_eq!(value_of(&mut ui, "Replica set field"), "atlas-1");
    assert_eq!(value_of(&mut ui, "Auth source field"), "admin");
}

#[test]
fn the_tls_tab_sets_tls_and_drops_the_files_when_it_goes_off() {
    let mut ui = open_form("mongodb://h/?w=1", ConnTab::Tls);
    ui.click("Encrypt the connection");
    assert_eq!(form(ui.h.state_mut()).uri, "mongodb://h/?w=1&tls=true");
    replace(&mut ui, "CA file field", "/certs/ca.pem");
    ui.settle();
    ui.click("Allow invalid certificates");
    assert_eq!(form(ui.h.state_mut()).uri, "mongodb://h/?w=1&tls=true&tlsCAFile=/certs/ca.pem&tlsAllowInvalidCertificates=true");
    ui.click("Encrypt the connection");
    assert_eq!(form(ui.h.state_mut()).uri, "mongodb://h/?w=1&tls=false");
}

#[test]
fn an_address_that_does_not_parse_is_left_to_its_own_field() {
    let mut ui = open_form("not a uri", ConnTab::Standard);
    ui.show_contains("only the URI above is edited");
    assert_eq!(form(ui.h.state_mut()).uri, "not a uri");
}
