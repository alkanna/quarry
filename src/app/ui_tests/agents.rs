//! Agent calls through the real App: injected as the link would send them, answered through the approval queue.

use super::*;
use crate::agents::config::{Access, ConnPolicy, Mode};
use crate::agents::link::{ClientInfo, ToolErr};
use crate::app::agents::{Answer, Outcome};
use serde_json::{Value, json};
use tokio::sync::oneshot;

type Rx = oneshot::Receiver<Result<Value, ToolErr>>;

/// Sends a call through the worker channel, as the link listener does.
fn inject(app: &App, link: u64, tool: &str, args: Value) -> Rx {
    let (reply, rx) = oneshot::channel();
    app.core.worker.send(Msg::Agent(db::AgentEvent::Call { session: 1, id: link, tool: tool.into(), args, client: ClientInfo { name: "claude-code".into(), version: "1".into() }, reply }));
    rx
}

fn share(app: &mut App, conn: u64) {
    app.core.agents.cfg.enabled = true;
    app.core.agents.cfg.conns.insert(conn, ConnPolicy { access: Access::Read, read: Mode::Ask, ..Default::default() });
}

#[test]
fn an_agent_find_waits_for_approval_then_answers_scrubbed() {
    let Some(mut ui) = ui_app("QUARRY_TEST_MONGO") else { return };
    let client = ui.app().conn(CONN).and_then(|c| c.client.clone()).expect("a client");
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let col = client.database("quarry_test").collection::<Document>("agent_ui");
        col.drop().await.unwrap();
        col.insert_many((0..5).map(|i| doc! { "_id": i, "name": format!("n{i}"), "password": format!("pw{i}") })).await.unwrap();
    });
    share(ui.h.state_mut(), CONN);
    let mut rx = inject(ui.app(), 1, "find", json!({ "connection": "test", "database": "quarry_test", "collection": "agent_ui", "filter": { "_id": { "$lt": 3 } }, "reason": "look at users" }));
    ui.wait("the approval", |a| !a.core.agents.asks.is_empty());
    let ask = ui.app().core.agents.asks[0].clone();
    assert_eq!((ask.tool.as_str(), ask.ns.as_str(), ask.reason.as_deref()), ("find", "quarry_test.agent_ui", Some("look at users")));
    assert!(rx.try_recv().is_err(), "nothing answered before the user");
    ui.h.state_mut().agent_answer(ask.id, Answer::Once);
    ui.wait("the call to finish", |a| a.core.agents.activity.iter().any(|x| x.id == ask.id && x.outcome == Outcome::Approved));
    let v = rx.try_recv().expect("answered").expect("found");
    let docs = v["documents"].as_array().unwrap();
    assert_eq!(docs.len(), 3);
    assert_eq!(docs[0]["password"], json!("hidden"));
    let act = ui.app().core.agents.activity.iter().find(|x| x.id == ask.id).unwrap().clone();
    assert_eq!(act.summary, "3 documents");
    assert!(!act.answer.contains("pw0") && act.answer.contains("n0"), "{}", act.answer);
    assert!(ui.app().core.agents.asks.is_empty());
}

#[test]
fn an_unanswered_approval_is_denied_by_the_timer() {
    let mut h = Harness::builder().with_accessibility_check(false).build_eframe(|cc| App::for_test(cc));
    let app = h.state_mut();
    app.core.conns.push(Conn::new(ConnMeta { id: 3, name: "prod".into(), ..Default::default() }, Status::Ready));
    share(app, 3);
    // The real wait is 50 s; the timer is the same tokio timer, shortened.
    app.core.agents.exec.ask_wait = Duration::from_millis(300);
    let mut rx = inject(h.state(), 1, "count", json!({ "connection": 3, "database": "shop", "collection": "users" }));
    let t = std::time::Instant::now();
    while !h.state().core.agents.asks.front().is_some_and(|k| k.gone) {
        assert!(t.elapsed() < LIMIT, "the approval never timed out");
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(rx.try_recv().unwrap().unwrap_err().message, crate::app::agents::exec::NO_ANSWER);
    assert_eq!(h.state().core.agents.activity[0].outcome, Outcome::TimedOut);
    // Allow on a call that timed out only closes it.
    let id = h.state().core.agents.asks[0].id;
    h.state_mut().agent_answer(id, Answer::Once);
    assert!(h.state().core.agents.asks.is_empty());
    assert_eq!(h.state().core.agents.activity[0].outcome, Outcome::TimedOut);
}

#[test]
fn undo_call_in_the_activity_window_reverts_an_agent_update() {
    let Some(mut ui) = ui_app("QUARRY_TEST_MONGO") else { return };
    let client = ui.app().conn(CONN).and_then(|c| c.client.clone()).expect("a client");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let col = client.database("quarry_test").collection::<Document>("agent_ui_undo");
    rt.block_on(async {
        col.drop().await.unwrap();
        col.insert_many((0..4).map(|i| doc! { "_id": i, "n": i })).await.unwrap();
    });
    let app = ui.h.state_mut();
    app.core.agents.cfg.enabled = true;
    app.core.agents.cfg.conns.insert(CONN, ConnPolicy { access: Access::ReadWrite, change: Mode::Allow, ..Default::default() });
    // Agent writes need the connection unlocked, like the user's.
    app.core.conns.iter_mut().find(|c| c.meta.id == CONN).unwrap().writable = true;
    let mut rx = inject(ui.app(), 1, "update", json!({ "connection": "test", "database": "quarry_test", "collection": "agent_ui_undo", "filter": { "n": { "$lt": 2 } }, "update": { "$set": { "n": 50 } }, "many": true }));
    ui.wait("the update to finish with an undo", |a| a.core.agents.activity.iter().any(|x| x.tool == "update" && x.undo.is_some()));
    let v = rx.try_recv().expect("answered").expect("updated");
    assert_eq!(v["modified"], json!(2));
    assert_eq!(rt.block_on(async { col.count_documents(doc! { "n": 50 }).await }).unwrap(), 2);
    let act = ui.app().core.agents.activity.iter().find(|x| x.tool == "update").unwrap().clone();
    assert_eq!(act.outcome, Outcome::Allowed);
    ui.h.state_mut().core.agents.activity_open = true;
    ui.wait("the Activity window", |_| true);
    ui.click(&format!("{} {} at {}", act.tool, act.ns, act.time));
    ui.click("Undo call");
    ui.wait("the undo", |a| a.core.agents.activity.iter().any(|x| x.id == act.id && x.summary.ends_with("undone")));
    assert_eq!(rt.block_on(async { col.count_documents(doc! { "n": 50 }).await }).unwrap(), 0);
    assert_eq!(rt.block_on(async { col.find_one(doc! { "_id": 1 }).await }).unwrap().unwrap().get_i32("n").unwrap(), 1);
    assert!(ui.app().core.undo.is_empty(), "agent changes never join the user's undo list");
}

#[test]
fn structure_changes_always_ask_and_run_once_allowed() {
    let Some(mut ui) = ui_app("QUARRY_TEST_MONGO") else { return };
    let client = ui.app().conn(CONN).and_then(|c| c.client.clone()).expect("a client");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let d = client.database("quarry_test");
    rt.block_on(async { d.collection::<Document>("agent_ui_made").drop().await.unwrap() });
    let app = ui.h.state_mut();
    app.core.agents.cfg.enabled = true;
    // Allow is set, yet structure changes still ask, every time.
    app.core.agents.cfg.conns.insert(CONN, ConnPolicy { access: Access::ReadWrite, structure: Mode::Allow, ..Default::default() });
    app.core.conns.iter_mut().find(|c| c.meta.id == CONN).unwrap().writable = true;
    let ns = json!({ "connection": "test", "database": "quarry_test", "collection": "agent_ui_made" });
    let with = |extra: Value| {
        let mut v = ns.clone();
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        v
    };
    for (link, tool, args) in [(1, "create_collection", ns.clone()), (2, "create_index", with(json!({ "keys": { "n": 1 }, "name": "by_n" }))), (3, "drop_index", with(json!({ "name": "by_n" }))), (4, "drop_collection", ns.clone())] {
        let mut rx = inject(ui.app(), link, tool, args);
        ui.wait("the approval", |a| !a.core.agents.asks.is_empty());
        let ask = ui.app().core.agents.asks[0].clone();
        assert!(ask.why.always && !ask.why.session_ok, "{tool}: structure never takes a session grant");
        assert!(ask.facts.iter().any(|f| f.contains("can't be undone")), "{tool}: {:?}", ask.facts);
        ui.h.state_mut().agent_answer(ask.id, Answer::Once);
        ui.wait("the call", |a| a.core.agents.activity.iter().any(|x| x.id == ask.id && x.outcome != Outcome::Running));
        let v = rx.try_recv().expect("answered").unwrap_or_else(|e| panic!("{tool}: {}", e.message));
        assert_eq!(v["undo"], json!(false));
        if tool == "create_index" {
            let names: Vec<String> = rt.block_on(async { d.collection::<Document>("agent_ui_made").list_index_names().await }).unwrap();
            assert!(names.contains(&"by_n".to_string()), "{names:?}");
        }
    }
    let left = rt.block_on(async { d.list_collection_names().await }).unwrap();
    assert!(!left.contains(&"agent_ui_made".to_string()));
    // A second create of a name that exists is refused rather than a silent success.
    rt.block_on(async { d.create_collection("agent_ui_made").await.unwrap() });
    let mut rx = inject(ui.app(), 5, "create_collection", ns.clone());
    ui.wait("the approval", |a| !a.core.agents.asks.is_empty());
    let id = ui.app().core.agents.asks[0].id;
    ui.h.state_mut().agent_answer(id, Answer::Once);
    ui.wait("the call", |a| a.core.agents.activity.iter().any(|x| x.id == id && x.outcome != Outcome::Running));
    assert!(rx.try_recv().unwrap().unwrap_err().message.contains("already exists"));
    rt.block_on(async { d.collection::<Document>("agent_ui_made").drop().await.unwrap() });
}

/// On PostgreSQL `database` names a database of the server, read through its own pool; a schema there is refused with
/// how to name it, and a two-part scope pattern covers only the database the connection opens.
#[test]
fn an_agent_reads_another_postgres_database() {
    let Ok(uri) = std::env::var("QUARRY_TEST_POSTGRES") else { return };
    let name = "agent_ui_other";
    let rt = tokio::runtime::Runtime::new().unwrap();
    let on = |db: Option<&str>, s: String| {
        rt.block_on(async {
            let mut o = <sqlx::postgres::PgConnectOptions as std::str::FromStr>::from_str(&uri).unwrap();
            if let Some(d) = db {
                o = o.database(d);
            }
            let p = sqlx::postgres::PgPoolOptions::new().max_connections(1).connect_with(o).await.unwrap();
            let r = sqlx::raw_sql(sqlx::AssertSqlSafe(s)).execute(&p).await.map(|_| ());
            p.close().await;
            r
        })
    };
    let _ = on(None, format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"));
    {
        let _turn = crate::db::sql::testing::db_turn();
        on(None, format!("CREATE DATABASE \"{name}\" TEMPLATE template0")).unwrap();
    }
    on(Some(name), "CREATE TABLE events (id int PRIMARY KEY, kind text); INSERT INTO events VALUES (1, 'a'), (2, 'b');".into()).unwrap();
    let Some(mut ui) = ui_app("QUARRY_TEST_POSTGRES") else { return };
    let app = ui.h.state_mut();
    app.core.agents.cfg.enabled = true;
    app.core.agents.cfg.conns.insert(CONN, ConnPolicy { access: Access::Read, read: Mode::Allow, ..Default::default() });
    let answer = |ui: &mut UiApp, link: u64, args: Value| {
        let mut rx = inject(ui.app(), link, "select_rows", args);
        ui.wait("the answer", |a| a.core.agents.activity.iter().filter(|x| x.tool == "select_rows" && x.outcome != Outcome::Running).count() as u64 == link);
        rx.try_recv().expect("answered")
    };
    // Without a schema, the schemas of the database and no tables.
    let mut rx = inject(ui.app(), 9, "list_collections", json!({ "connection": "test", "database": name }));
    ui.wait("the schemas", |a| a.core.agents.activity.iter().any(|x| x.tool == "list_collections" && x.outcome != Outcome::Running));
    let v = rx.try_recv().expect("answered").expect("listed");
    assert!(v["schemas"].as_array().unwrap().contains(&json!("public")) && v["tables"] == json!([]), "{v}");
    let v = answer(&mut ui, 1, json!({ "connection": "test", "database": name, "table": "events", "where": [{ "column": "kind", "value": "b" }] })).expect("read");
    assert_eq!(v["rows"], json!([{ "id": 2, "kind": "b" }]));
    let ns = ui.app().core.agents.activity.iter().find(|x| x.tool == "select_rows").map(|x| x.ns.clone()).unwrap_or_default();
    assert!(ns.starts_with(&format!("{name}.public")), "{ns}");
    let e = answer(&mut ui, 2, json!({ "connection": "test", "database": "public", "table": "events" })).unwrap_err();
    assert!(e.message.contains("is not a database") && e.message.contains(name), "{}", e.message);
    ui.h.state_mut().core.agents.cfg.conns.get_mut(&CONN).unwrap().scope = crate::agents::config::Scope { mode: crate::agents::config::ScopeMode::Only, globs: vec!["public.*".into()] };
    ui.h.state_mut().core.agents.rev += 1;
    let e = answer(&mut ui, 3, json!({ "connection": "test", "database": name, "table": "events" })).unwrap_err();
    assert!(e.message.contains("not shared"), "{}", e.message);
    let _ = on(None, format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"));
}
