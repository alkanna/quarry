//! The client certificate rows of the PostgreSQL TLS tab. They show on PostgreSQL only and are greyed with Cloud SQL. With
//! QUARRY_TEST_PGCERT and QUARRY_TEST_PGCERT_DIR set (see `db::sql::pg_cert`), the form's Test signs in with the
//! certificate and an encrypted key whose passphrase is in the form, then fails with the passphrase cleared.

use super::*;

fn open_form(ui: &mut UiApp, engine: db::sql::Engine, addr: db::sql::Addr) {
    ui.h.state_mut().modal = Some(Modal::Conn(Box::new(ConnForm { name: "certs".into(), engine, addr, tab: ConnTab::Tls, ..Default::default() })));
    ui.settle();
}

fn form_mut(app: &mut App) -> &mut ConnForm {
    match app.modal.as_mut() {
        Some(Modal::Conn(f)) => f,
        _ => panic!("the connection form is not open"),
    }
}

#[test]
fn the_tls_tab_takes_a_client_certificate_and_test_signs_in_with_it() {
    let h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_accessibility_check(false).build_eframe(|cc| App::for_test(cc));
    let mut ui = UiApp { h };
    open_form(&mut ui, db::sql::Engine::MySql, db::sql::Addr { scheme: "mysql".into(), port: 3306, ..Default::default() });
    ui.show("CA file");
    assert!(ui.h.query_all_by_label("Client cert").next().is_none(), "MySQL has no client certificate rows");

    let base = std::env::var("QUARRY_TEST_PGCERT").ok().and_then(|u| db::sql::Addr::parse(&u)).unwrap_or_else(|| db::sql::Addr { scheme: "postgres".into(), host: "h".into(), port: 5432, user: "u".into(), ..Default::default() });
    open_form(&mut ui, db::sql::Engine::Postgres, base.clone());
    for label in ["Client cert", "Client key", "Passphrase"] {
        ui.show(label);
    }
    // A passphrase goes to the worker only with the connection it belongs to.
    form_mut(ui.h.state_mut()).pg_key_pass = "s3cret".into();
    assert_eq!(form_mut(ui.h.state_mut()).net().key_passphrase.as_deref(), Some("s3cret"));
    form_mut(ui.h.state_mut()).engine = db::sql::Engine::MySql;
    assert_eq!(form_mut(ui.h.state_mut()).net().key_passphrase, None);
    form_mut(ui.h.state_mut()).engine = db::sql::Engine::Postgres;

    let Ok(dir) = std::env::var("QUARRY_TEST_PGCERT_DIR") else { return };
    if std::env::var("QUARRY_TEST_PGCERT").is_err() {
        return;
    }
    let p = |f: &str| std::path::Path::new(&dir).join(f).display().to_string();
    let f = form_mut(ui.h.state_mut());
    // require: the test server's certificate does not name the host, see live_client_certificate_sign_in.
    (f.addr.ssl_mode, f.addr.ssl_cert, f.addr.ssl_key) = (db::sql::SslMode::Required, p("client.crt"), p("client-enc.key"));
    ui.click("Test");
    ui.wait("the Test answer", |a| a.conn_test.is_some());
    match &ui.app().conn_test {
        Some((_, Ok(m))) => assert!(m.contains("with client certificate") && m.contains("CN=quarry_cert"), "{m}"),
        other => panic!("Test with the client certificate failed: {:?}", other.as_ref().map(|t| &t.1)),
    }
    // Without the passphrase the form says what is missing in words, not the driver's text.
    form_mut(ui.h.state_mut()).pg_key_pass.clear();
    ui.click("Test");
    ui.wait("the Test answer", |a| a.conn_test.is_some());
    ui.show("The client key needs its passphrase. The key file is encrypted. Type its passphrase on the connection's TLS tab.");
}
