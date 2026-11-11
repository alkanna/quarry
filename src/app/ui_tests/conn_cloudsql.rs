//! The Cloud SQL browser and form, driven like a user. gcloud never runs: its answers are recorded and fed through the app's
//! own message path. Use on an instance fills the form, and Save keeps the instance and no password. With
//! QUARRY_TEST_POSTGRES set, a second instance goes end to end through the stub proxy (the test binary, forwarding to that
//! server): the form's Test, then Connect.

use super::*;
use crate::cloudsql::{CloudSql, Found, IpKind, Project, Start};

/// The browser's load id; set before it opens, so opening asks gcloud nothing.
const REQ: u64 = 9_000;

const PROD: &str = r#"[
  {"connectionName":"acme-prod:europe-west1:orders-db","name":"orders-db","region":"europe-west1","databaseVersion":"POSTGRES_17","state":"RUNNABLE","ipAddresses":[{"type":"PRIMARY"},{"type":"PRIVATE"}],"settings":{"databaseFlags":[{"name":"cloudsql.iam_authentication","value":"on"}]}},
  {"connectionName":"acme-prod:europe-west1:reporting","name":"reporting","region":"europe-west1","databaseVersion":"POSTGRES_16","state":"RUNNABLE","ipAddresses":[{"type":"PRIVATE"}],"settings":{"databaseFlags":[{"name":"cloudsql.iam_authentication","value":"on"}],"ipConfiguration":{"pscConfig":{"pscEnabled":true}}}},
  {"connectionName":"acme-prod:us-central1:billing-legacy","name":"billing-legacy","region":"us-central1","databaseVersion":"POSTGRES_13","state":"RUNNABLE","ipAddresses":[{"type":"PRIMARY"}],"settings":{"activationPolicy":"NEVER"}}
]"#;

/// The real app with no server and no connection.
fn bare() -> UiApp {
    let h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_accessibility_check(false).build_eframe(|cc| App::for_test(cc));
    UiApp { h }
}

fn form(app: &App) -> &ConnForm {
    match &app.modal {
        Some(Modal::Conn(f)) => f,
        _ => panic!("the connection form is not open"),
    }
}

fn form_mut(app: &mut App) -> &mut ConnForm {
    match app.modal.as_mut() {
        Some(Modal::Conn(f)) => f,
        _ => panic!("the connection form is not open"),
    }
}

/// Sends one gcloud answer the way the worker does and waits until the app took it.
fn feed(ui: &mut UiApp, found: Found, what: &str, taken: impl Fn(&App) -> bool) {
    ui.app().worker.send(Msg::CloudSql { req: REQ, found });
    ui.wait(what, taken);
}

#[test]
fn the_cloud_sql_browser_fills_the_form_and_saves_the_instance_without_a_password() {
    let mut ui = bare();
    ui.h.state_mut().cloud.req = REQ;
    ui.h.state_mut().open_cloud_browser(false);
    // The instance lists first: a project already answered is not asked again when the project list arrives.
    feed(&mut ui, Found::Instances { project: "acme-prod".into(), res: crate::cloudsql::parse_instances(PROD) }, "the acme-prod instances", |a| a.cloud.instances.contains_key("acme-prod"));
    let refused = "Cloud SQL: gcloud failed: ERROR: (gcloud.sql.instances.list) HTTPError 403: Cloud SQL Admin API has not been used in project acme-sandbox before or it is disabled.";
    feed(&mut ui, Found::Instances { project: "acme-sandbox".into(), res: Err(refused.into()) }, "the acme-sandbox refusal", |a| a.cloud.instances.contains_key("acme-sandbox"));
    let start = Start { gcloud: Some("/nonexistent/gcloud".into()), proxy: Some("/nonexistent/cloud-sql-proxy".into()), components: false, account: Some("jane@example.com".into()), projects: Ok(["acme-prod", "acme-sandbox"].map(|id| Project { id: id.into(), name: String::new() }).to_vec()), adc: false };
    feed(&mut ui, Found::Start(start), "the project list", |a| a.cloud.start.is_some());
    assert_eq!(ui.app().cloud.running, 0, "no gcloud was started for projects that had answered");

    ui.show("acme-prod:europe-west1:orders-db");
    ui.show("acme-sandbox has the Cloud SQL Admin API off.");
    assert!(ui.h.query_all_by_label("acme-prod:us-central1:billing-legacy").next().is_none(), "a stopped instance shows only on request");
    ui.click("Show stopped instances");
    ui.show("acme-prod:us-central1:billing-legacy");

    // The proxy signs in to IAM with Application Default Credentials only, so without them Use asks for them first.
    ui.click("Use acme-prod:europe-west1:orders-db");
    ui.show("IAM sign-in needs Application Default Credentials");
    assert!(ui.app().cloud_browser.is_some() && ui.app().modal.is_none(), "no form is filled for a sign-in that cannot work");
    ui.h.state_mut().cloud.start.as_mut().unwrap().adc = true;
    ui.settle();
    assert!(ui.h.query_all_by_label("IAM sign-in needs Application Default Credentials").next().is_none(), "the card goes once they exist");
    ui.click("Use acme-prod:europe-west1:orders-db");
    ui.wait("the form", |a| a.cloud_browser.is_none() && matches!(a.modal, Some(Modal::Conn(_))));
    let f = form(ui.app());
    assert_eq!(f.cloud_sql, Some(CloudSql { instance: "acme-prod:europe-west1:orders-db".into(), iam: true, ip: IpKind::Public }));
    assert_eq!((f.name.as_str(), f.addr.user.as_str(), f.engine), ("orders-db", "jane@example.com", db::sql::Engine::Postgres));
    ui.show("From gcloud, editable");
    ui.show("No password. The proxy signs in with Application Default Credentials, which must be this user's account");
    ui.show("PSC is off for orders-db");
    assert!(ui.h.get_by_label("PSC").accesskit_node().is_disabled(), "orders-db has no PSC");
    assert!(!ui.h.get_by_label("Private").accesskit_node().is_disabled(), "orders-db has a private IP");
    assert!(ui.h.query_all_by_label("Password").next().is_none(), "IAM sign-in has no password row");

    // A password left in the form, as from an address that had one, is not saved with an IAM sign-in.
    form_mut(ui.h.state_mut()).addr.password = "left-over".into();
    ui.click("Save and connect");
    ui.wait("the form to close", |a| a.modal.is_none());
    let c = ui.app().conns.last().expect("the saved connection");
    assert_eq!(c.meta.cloud_sql, Some(CloudSql { instance: "acme-prod:europe-west1:orders-db".into(), iam: true, ip: IpKind::Public }));
    assert!(c.meta.ssh.is_none());
    assert!(!c.masked_uri.contains("****") && c.masked_uri.contains("jane%40example.com@"), "the stored address has no password: {}", c.masked_uri);

    // Through the stub proxy to a real server: the test binary stands in for cloud-sql-proxy, set as in Settings.
    let Ok(uri) = std::env::var("QUARRY_TEST_POSTGRES") else { return };
    let live = db::sql::Addr::parse(&uri).unwrap();
    *crate::cloudsql::tests::STUB_TARGET.lock().unwrap() = Some(format!("{}:{}", live.host, live.port));
    ui.h.state_mut().prefs.cloud_sql_proxy = std::env::current_exe().unwrap().display().to_string();
    // The answers are kept for the session: reopening lists the instances at once and asks nothing.
    ui.h.state_mut().open_cloud_browser(false);
    ui.show("acme-prod:europe-west1:reporting");
    assert_eq!(ui.app().cloud.req, REQ);
    ui.click("Use acme-prod:europe-west1:reporting");
    ui.wait("the form", |a| matches!(a.modal, Some(Modal::Conn(_))));
    assert_eq!(form(ui.app()).cloud_sql.as_ref().map(|c| c.ip), Some(IpKind::Private), "reporting has no public IP, so Private is picked");
    // The test server signs in with a password, so IAM goes off and its user and password are typed.
    ui.click("Sign in with IAM");
    ui.show("Password");
    let f = form_mut(ui.h.state_mut());
    (f.addr.user, f.addr.password, f.addr.database) = (live.user.clone(), live.password.clone(), live.database.clone());
    ui.click("Test");
    ui.wait("the Test answer", |a| a.conn_test.is_some());
    match &ui.app().conn_test {
        Some((_, Ok(m))) => assert!(m.contains("through the Cloud SQL proxy"), "{m}"),
        other => panic!("Test through the stub proxy failed: {:?}", other.as_ref().map(|t| &t.1)),
    }
    ui.click("Save and connect");
    ui.wait("the form to close", |a| a.modal.is_none());
    let id = ui.app().conns.last().unwrap().meta.id;
    assert_eq!(ui.app().conn(id).unwrap().meta.cloud_sql.as_ref().map(|c| c.iam), Some(false));
    // The app keeps no keychain in tests, so Connect gets the address the form saved; the proxy replaces its host.
    let saved = db::sql::Addr { host: CLOUD_HOST.into(), port: 5432, ..live };
    ui.h.state_mut().connect_uri(id, saved.to_uri());
    ui.wait("the connection through the proxy", |a| a.conn(id).is_some_and(|c| c.status == Status::Ready));
    assert!(matches!(ui.app().conn(id).unwrap().tunnel, Some(db::Way::CloudSql(_))), "the proxy is kept with the connection");
    *crate::cloudsql::tests::STUB_TARGET.lock().unwrap() = None;
}

/// An answer to a connect the user stopped, or one a newer connect replaced, changes nothing: dropping it is what stops
/// its proxy or tunnel. No server is needed, since the answers are sent the way the worker sends them.
#[test]
fn a_connect_answer_after_disconnect_or_a_newer_connect_is_dropped() {
    let mut ui = bare();
    // 71 was disconnected while connecting, 72 was connected again since, 73 still waits for attempt 5.
    for (id, engine, status, attempt) in [(71, db::sql::Engine::Postgres, Status::Idle, 3), (72, db::sql::Engine::Mongo, Status::Connecting, 4), (73, db::sql::Engine::Postgres, Status::Connecting, 5)] {
        let mut c = Conn::new(ConnMeta { id, name: format!("c{id}"), engine, ..Default::default() }, status);
        c.attempt = attempt;
        ui.h.state_mut().conns.push(c);
    }
    let w = &ui.app().worker;
    w.send(Msg::SqlConnected { conn: 71, attempt: 3, res: Err("late".into()) });
    w.send(Msg::Connected { conn: 72, attempt: 2, res: Err("late".into()) });
    w.send(Msg::SqlConnected { conn: 73, attempt: 5, res: Err("refused".into()) });
    ui.wait("the answer that is still wanted", |a| a.conn(73).is_some_and(|c| matches!(c.status, Status::Failed(_))));
    assert!(ui.app().conn(71).unwrap().status == Status::Idle, "a disconnected connection stays disconnected");
    assert!(ui.app().conn(72).unwrap().status == Status::Connecting, "an older attempt does not end the newer one");
}

/// Settings looks for gcloud off the UI thread once it opens, and the demo shows a fixed path instead of the real one.
#[test]
fn settings_finds_gcloud_once_and_the_demo_shows_no_real_path() {
    let mut ui = bare();
    let app = ui.h.state_mut();
    app.demo = true;
    app.settings_cat = super::super::settings::SettingsCat::Tools;
    app.modal = Some(Modal::Settings);
    ui.settle();
    ui.show("Found automatically at ~/google-cloud-sdk/bin/gcloud. Set a path only to use another gcloud.");
    assert!(!ui.app().cloud.gcloud_asked, "the demo never looks for gcloud");
    ui.h.state_mut().demo = false;
    ui.wait("the answer from the worker", |a| a.cloud.gcloud_seen.is_some());
    ui.h.state_mut().modal = None;
    ui.settle();
    assert!(!ui.app().cloud.gcloud_asked && ui.app().cloud.gcloud_seen.is_none(), "closed, Settings looks again next time");
}
