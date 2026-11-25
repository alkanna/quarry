//! Fixes in how a MongoDB connection is set up and kept: the form, URI options, masking, the SSH host key and Disconnect.
//! The form and the rail are driven as a person drives them; a connect that needs a server runs on the live
//! ones (QUARRY_TEST_MONGO, QUARRY_TEST_MONGO_RS, QUARRY_TEST_MONGO_TLS, QUARRY_TEST_SSH) and skips without them. Each test
//! uses its own harness and collection, so they run side by side.

use super::*;

const DB: &str = "quarry_ui_connfix";

/// Saves the open form as a new connection and returns its id. The test app has no keychain, so Save stores the connection
/// without connecting and the test connects with `connect_uri`.
fn save_form(ui: &mut UiApp) -> u64 {
    ui.click("Save and connect");
    assert!(ui.app().modal.is_none(), "the form stayed open");
    ui.app().core.active.expect("Save activates the connection")
}

/// Saves `uri` through the form and connects with the address the saved connection stored, waiting for the attempt to end.
fn connect_saved(uri: &str) -> (UiApp, u64) {
    let mut ui = open_form(uri, ConnTab::Standard);
    let id = save_form(&mut ui);
    let stored = ui.app().conn(id).unwrap().masked_uri.clone();
    ui.connect(id, &stored);
    (ui, id)
}

fn failure(ui: &UiApp, id: u64) -> String {
    match &ui.app().conn(id).unwrap().status {
        Status::Failed(e) => e.clone(),
        _ => panic!("expected a failed connection"),
    }
}

fn tls_uri() -> Option<String> {
    std::env::var("QUARRY_TEST_MONGO_TLS").ok()
}

/// The TLS server's address without its own allow-invalid flag.
fn strict_tls_uri() -> Option<String> {
    tls_uri().map(|u| u.replace("&tlsAllowInvalidCertificates=true", ""))
}

#[test]
fn a_uri_that_skips_only_the_host_name_check_is_refused_in_plain_words() {
    let Some(base) = strict_tls_uri() else { return };
    let (ui, id) = connect_saved(&format!("{base}&tlsAllowInvalidHostnames=true"));
    let e = failure(&ui, id);
    assert!(e.contains("Allow invalid certificates") && e.contains("tlsAllowInvalidHostnames"), "{e}");
}

#[test]
fn a_host_name_option_connects_when_off_or_when_every_check_is_off() {
    let Some(base) = tls_uri() else { return };
    // The base address already skips the certificate checks, so true asks for nothing more.
    for v in ["true", "false"] {
        let (ui, id) = connect_saved(&format!("{base}&tlsAllowInvalidHostnames={v}"));
        ui.assert_ready(id);
    }
}

#[test]
fn allow_invalid_certificates_on_an_address_with_tls_insecure_false_gives_one_that_connects() {
    let Some(base) = strict_tls_uri() else { return };
    let mut ui = open_form(&format!("{base}&tlsInsecure=false"), ConnTab::Tls);
    ui.click("Allow invalid certificates switch");
    let uri = form(ui.h.state_mut()).uri.clone();
    assert!(uri.contains("tlsAllowInvalidCertificates=true") && !uri.contains("tlsInsecure"), "{uri}");
    let id = save_form(&mut ui);
    ui.connect(id, &uri);
    ui.assert_ready(id);
}

#[test]
fn the_mongodb_card_has_no_paste_box_and_the_sql_cards_do() {
    let mut ui = open_form("mongodb://localhost:27017", ConnTab::Standard);
    ui.show("Hosts field");
    assert!(ui.h.query_all_by_label("Paste connection string").next().is_none(), "the MongoDB form shows a paste box");
    let addr = db::sql::Addr { scheme: "mysql".into(), port: 3306, ..Default::default() };
    let mut sql = open_form_with(ConnForm { name: "shop".into(), engine: db::sql::Engine::MySql, addr, tab: ConnTab::Standard, ..Default::default() });
    sql.show("Paste connection string");
}

#[test]
fn option_secrets_are_masked_in_the_saved_connection_and_its_failed_card() {
    let uri = "mongodb://app:s3cret@127.0.0.1:1/?proxyHost=px&proxyUsername=u&proxyPassword=pxsecret&tlsCertificateKeyFilePassword=keysecret&authMechanismProperties=AWS_SESSION_TOKEN:tok%40en&appName=a%40b";
    let mut ui = open_form(uri, ConnTab::Standard);
    let id = save_form(&mut ui);
    let masked = ui.app().conn(id).unwrap().masked_uri.clone();
    for secret in ["s3cret", "pxsecret", "keysecret", "tok%40en", "tok@en"] {
        assert!(!masked.contains(secret), "{secret} shows in {masked}");
    }
    // An at sign in an option value must not move the mask onto the wrong part.
    assert!(masked.contains("127.0.0.1:1") && masked.contains("appName=a%40b"), "{masked}");
    ui.h.state_mut().core.conns.iter_mut().find(|c| c.meta.id == id).unwrap().status = Status::Failed("The server did not answer".into());
    ui.settle();
    ui.show_contains("The server did not answer");
    for secret in ["s3cret", "pxsecret", "keysecret", "tok%40en"] {
        assert!(!ui.shows(secret), "{secret} is on the failed card");
    }
}

#[test]
fn a_uri_with_spaces_around_it_is_accepted_and_saved_trimmed() {
    let mut ui = open_form("  mongodb://localhost:27017/?w=1  \n", ConnTab::Standard);
    assert!(!ui.disabled("Save and connect"), "Save is off for an address with spaces around it");
    ui.show("Hosts field");
    assert_eq!(ui.h.get_by_label("Hosts field").value().unwrap_or_default(), "localhost:27017");
    let id = save_form(&mut ui);
    assert_eq!(ui.app().conn(id).unwrap().masked_uri, "mongodb://localhost:27017/?w=1");
}

#[test]
fn uri_timeouts_win_over_the_apps_query_timeout() {
    let uri = "mongodb://127.0.0.1:1/?serverSelectionTimeoutMS=500&connectTimeoutMS=500";
    let mut ui = open_form(uri, ConnTab::Standard);
    let id = save_form(&mut ui);
    ui.h.state_mut().core.prefs.timeout_ms = 60_000;
    let t = std::time::Instant::now();
    ui.connect(id, uri);
    assert!(t.elapsed() < Duration::from_secs(15), "the 60 s app timeout won over the 500 ms in the address: {:?}", t.elapsed());
    failure(&ui, id);
}

#[test]
fn aws_and_kerberos_sign_in_are_named_as_not_built_in_on_the_failed_card() {
    for (uri, title) in [("mongodb://127.0.0.1:1/?authMechanism=MONGODB-AWS&serverSelectionTimeoutMS=2000", "AWS IAM (MONGODB-AWS) sign-in is not built into Quarry"), ("mongodb://user@127.0.0.1:1/?authMechanism=GSSAPI&serverSelectionTimeoutMS=2000", "Kerberos (GSSAPI) sign-in is not built into Quarry")] {
        let (mut ui, id) = connect_saved(uri);
        failure(&ui, id);
        ui.show_contains(title);
    }
}

#[test]
fn disconnect_stops_a_running_watch_of_that_connection() {
    let Some(mut ui) = ui_app("QUARRY_TEST_MONGO_RS") else { return };
    let client = ui.app().conn(CONN).and_then(|c| c.client.clone()).expect("a client");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let coll = client.database(DB).collection::<mongodb::bson::Document>("watch_disconnect");
    rt.block_on(async {
        coll.drop().await.unwrap();
        coll.insert_one(mongodb::bson::doc! { "_id": 0 }).await.unwrap();
    });
    // The sidebar lists databases from the connect, so connect again to see the one just made.
    ui.connect(CONN, &std::env::var("QUARRY_TEST_MONGO_RS").unwrap());
    ui.assert_ready(CONN);
    ui.open_table(DB, "", "watch_disconnect");
    ui.h.state_mut().tm().unwrap().view = View::Watch;
    ui.settle();
    ui.click("\u{25b6} Start watching");
    ui.wait("the watch to run", |a| a.t().is_some_and(|t| t.watch.running()));
    rt.block_on(async { coll.insert_one(mongodb::bson::doc! { "_id": 1 }).await.unwrap() });
    ui.wait("the first change", |a| a.t().is_some_and(|t| t.watch.err.is_none() && !t.watch.rows.is_empty()));
    let seen = ui.app().t().unwrap().watch.rows.len();
    ui.h.state_mut().exec(quarry_core::action::Action::Conn(quarry_core::action::ConnA::Disconnect { conn: CONN }));
    ui.settle();
    assert!(!ui.app().core.tabs.iter().any(|t| t.watch.running()), "a watch kept running after Disconnect");
    rt.block_on(async { coll.insert_one(mongodb::bson::doc! { "_id": 2 }).await.unwrap() });
    ui.settle();
    assert_eq!(ui.app().t().unwrap().watch.rows.len(), seen, "a change arrived after Disconnect");
    rt.block_on(async { coll.drop().await.unwrap() });
}

/// QUARRY_TEST_SSH's server and password, and QUARRY_TEST_MONGO as that server reaches it.
fn ssh_env() -> Option<(crate::ssh::SshConf, String, String)> {
    let s = std::env::var("QUARRY_TEST_SSH").ok()?;
    let mongo = std::env::var("QUARRY_TEST_MONGO").ok()?;
    let (cred, hp) = s.rsplit_once('@')?;
    let (user, pass) = cred.split_once(':')?;
    let (host, port) = hp.rsplit_once(':')?;
    let mport = mongo.split_once("://")?.1.split(['/', '?']).next()?.rsplit_once(':')?.1.to_string();
    let target = std::env::var("QUARRY_TEST_SOCKS_TARGET").unwrap_or_else(|_| "host.docker.internal".into());
    let conf = crate::ssh::SshConf { host: host.into(), port: port.parse().ok()?, user: user.into(), auth: crate::ssh::SshAuth::Password, ..Default::default() };
    Some((conf, pass.into(), format!("mongodb://{target}:{mport}/?directConnection=true")))
}

/// A connection through SSH saved from the form, not connected yet.
fn ssh_saved(conf: &crate::ssh::SshConf, far: &str) -> (UiApp, u64) {
    let mut ui = open_form_with(ConnForm { name: "bastion".into(), uri: far.into(), ssh_on: true, ssh: conf.clone(), ..Default::default() });
    let id = save_form(&mut ui);
    (ui, id)
}

/// Connects through SSH with the password the test app cannot read from a keychain.
fn ssh_connect(ui: &mut UiApp, id: u64, pass: &str, far: &str) {
    let app = ui.h.state_mut();
    app.core.req += 1;
    let attempt = app.core.req;
    let timeout = app.timeout();
    let c = app.core.conns.iter_mut().find(|c| c.meta.id == id).unwrap();
    c.status = Status::Connecting;
    c.attempt = attempt;
    let net = db::Net { ssh: c.meta.ssh.clone().map(|s| (s, crate::ssh::SshSecrets { password: Some(pass.into()), passphrase: None })), ..Default::default() };
    app.core.worker.connect(id, attempt, far.to_string(), timeout, net);
    ui.wait("the SSH connect to end", |a| a.conn(id).is_some_and(|c| matches!(c.status, Status::Ready | Status::Failed(_))));
    ui.assert_ready(id);
}

fn known(ui: &UiApp, id: u64) -> Option<String> {
    ui.app().conn(id).unwrap().meta.ssh.as_ref().unwrap().known_host.clone()
}

#[test]
fn a_host_key_remembered_while_the_form_is_open_is_kept_on_save() {
    let Some((conf, pass, far)) = ssh_env() else { return };
    let (mut ui, id) = ssh_saved(&conf, &far);
    assert_eq!(known(&ui, id), None);
    ui.h.state_mut().edit_conn(id);
    ui.settle();
    assert_eq!(form(ui.h.state_mut()).ssh.known_host, None, "the form opened with a key");
    ssh_connect(&mut ui, id, &pass, &far);
    let key = known(&ui, id).expect("the first connect remembers the key");
    ui.click("Save and connect");
    assert_eq!(known(&ui, id), Some(key), "Save threw away the key the first connect remembered");
}

#[test]
fn forget_then_save_forgets_the_host_key() {
    let Some((conf, pass, far)) = ssh_env() else { return };
    let (mut ui, id) = ssh_saved(&conf, &far);
    ssh_connect(&mut ui, id, &pass, &far);
    assert!(known(&ui, id).is_some());
    ui.h.state_mut().edit_conn(id);
    form(ui.h.state_mut()).tab = ConnTab::Ssh;
    ui.settle();
    ui.click("Forget");
    ui.click("Save and connect");
    assert_eq!(known(&ui, id), None, "Forget then Save kept the key");
}

#[test]
fn changing_the_ssh_host_or_port_clears_the_host_key() {
    let Some((conf, pass, far)) = ssh_env() else { return };
    for change in ["port", "host"] {
        let (mut ui, id) = ssh_saved(&conf, &far);
        ssh_connect(&mut ui, id, &pass, &far);
        assert!(known(&ui, id).is_some());
        ui.h.state_mut().edit_conn(id);
        form(ui.h.state_mut()).tab = ConnTab::Ssh;
        ui.settle();
        let f = form(ui.h.state_mut());
        if change == "port" {
            f.ssh.port += 1;
        } else {
            // The same server under another name: a changed bastion must be trusted again, not matched to the old key.
            f.ssh.host = if conf.host == "127.0.0.1" { "localhost".into() } else { "127.0.0.1".into() };
        }
        ui.click("Save and connect");
        assert_eq!(known(&ui, id), None, "a changed SSH {change} kept the key");
    }
}
