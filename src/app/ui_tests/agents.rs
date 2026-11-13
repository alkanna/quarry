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
    app.worker.send(Msg::Agent(db::AgentEvent::Call { session: 1, id: link, tool: tool.into(), args, client: ClientInfo { name: "claude-code".into(), version: "1".into() }, reply }));
    rx
}

fn share(app: &mut App, conn: u64) {
    app.agents.cfg.enabled = true;
    app.agents.cfg.conns.insert(conn, ConnPolicy { access: Access::Read, read: Mode::Ask, ..Default::default() });
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
    ui.wait("the approval", |a| !a.agents.asks.is_empty());
    let ask = ui.app().agents.asks[0].clone();
    assert_eq!((ask.tool.as_str(), ask.ns.as_str(), ask.reason.as_deref()), ("find", "quarry_test.agent_ui", Some("look at users")));
    assert!(rx.try_recv().is_err(), "nothing answered before the user");
    ui.h.state_mut().agent_answer(ask.id, Answer::Once);
    ui.wait("the call to finish", |a| a.agents.activity.iter().any(|x| x.id == ask.id && x.outcome == Outcome::Approved));
    let v = rx.try_recv().expect("answered").expect("found");
    let docs = v["documents"].as_array().unwrap();
    assert_eq!(docs.len(), 3);
    assert_eq!(docs[0]["password"], json!("hidden"));
    let act = ui.app().agents.activity.iter().find(|x| x.id == ask.id).unwrap().clone();
    assert_eq!(act.summary, "3 documents");
    assert!(!act.answer.contains("pw0") && act.answer.contains("n0"), "{}", act.answer);
    assert!(ui.app().agents.asks.is_empty());
}

#[test]
fn an_unanswered_approval_is_denied_by_the_timer() {
    let mut h = Harness::builder().with_accessibility_check(false).build_eframe(|cc| App::for_test(cc));
    let app = h.state_mut();
    app.conns.push(Conn::new(ConnMeta { id: 3, name: "prod".into(), ..Default::default() }, Status::Ready));
    share(app, 3);
    // The real wait is 50 s; the timer is the same tokio timer, shortened.
    app.agents.exec.ask_wait = Duration::from_millis(300);
    let mut rx = inject(h.state(), 1, "count", json!({ "connection": 3, "database": "shop", "collection": "users" }));
    let t = std::time::Instant::now();
    while !h.state().agents.asks.front().is_some_and(|k| k.gone) {
        assert!(t.elapsed() < LIMIT, "the approval never timed out");
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(rx.try_recv().unwrap().unwrap_err().message, crate::app::agents::exec::NO_ANSWER);
    assert_eq!(h.state().agents.activity[0].outcome, Outcome::TimedOut);
    // Allow on a call that timed out only closes it.
    let id = h.state().agents.asks[0].id;
    h.state_mut().agent_answer(id, Answer::Once);
    assert!(h.state().agents.asks.is_empty());
    assert_eq!(h.state().agents.activity[0].outcome, Outcome::TimedOut);
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
    app.agents.cfg.enabled = true;
    app.agents.cfg.conns.insert(CONN, ConnPolicy { access: Access::ReadWrite, change: Mode::Allow, ..Default::default() });
    // Agent writes need the connection unlocked, like the user's.
    app.conns.iter_mut().find(|c| c.meta.id == CONN).unwrap().writable = true;
    let mut rx = inject(ui.app(), 1, "update", json!({ "connection": "test", "database": "quarry_test", "collection": "agent_ui_undo", "filter": { "n": { "$lt": 2 } }, "update": { "$set": { "n": 50 } }, "many": true }));
    ui.wait("the update to finish with an undo", |a| a.agents.activity.iter().any(|x| x.tool == "update" && x.undo.is_some()));
    let v = rx.try_recv().expect("answered").expect("updated");
    assert_eq!(v["modified"], json!(2));
    assert_eq!(rt.block_on(async { col.count_documents(doc! { "n": 50 }).await }).unwrap(), 2);
    let act = ui.app().agents.activity.iter().find(|x| x.tool == "update").unwrap().clone();
    assert_eq!(act.outcome, Outcome::Allowed);
    ui.h.state_mut().agents.activity_open = true;
    ui.wait("the Activity window", |_| true);
    ui.click(&format!("{} {} at {}", act.tool, act.ns, act.time));
    ui.click("Undo call");
    ui.wait("the undo", |a| a.agents.activity.iter().any(|x| x.id == act.id && x.summary.ends_with("undone")));
    assert_eq!(rt.block_on(async { col.count_documents(doc! { "n": 50 }).await }).unwrap(), 0);
    assert_eq!(rt.block_on(async { col.find_one(doc! { "_id": 1 }).await }).unwrap().unwrap().get_i32("n").unwrap(), 1);
    assert!(ui.app().undo.is_empty(), "agent changes never join the user's undo list");
}

#[test]
fn structure_changes_always_ask_and_run_once_allowed() {
    let Some(mut ui) = ui_app("QUARRY_TEST_MONGO") else { return };
    let client = ui.app().conn(CONN).and_then(|c| c.client.clone()).expect("a client");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let d = client.database("quarry_test");
    rt.block_on(async { d.collection::<Document>("agent_ui_made").drop().await.unwrap() });
    let app = ui.h.state_mut();
    app.agents.cfg.enabled = true;
    // Allow is set, yet structure changes still ask, every time.
    app.agents.cfg.conns.insert(CONN, ConnPolicy { access: Access::ReadWrite, structure: Mode::Allow, ..Default::default() });
    app.conns.iter_mut().find(|c| c.meta.id == CONN).unwrap().writable = true;
    let ns = json!({ "connection": "test", "database": "quarry_test", "collection": "agent_ui_made" });
    let with = |extra: Value| {
        let mut v = ns.clone();
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        v
    };
    for (link, tool, args) in [(1, "create_collection", ns.clone()), (2, "create_index", with(json!({ "keys": { "n": 1 }, "name": "by_n" }))), (3, "drop_index", with(json!({ "name": "by_n" }))), (4, "drop_collection", ns.clone())] {
        let mut rx = inject(ui.app(), link, tool, args);
        ui.wait("the approval", |a| !a.agents.asks.is_empty());
        let ask = ui.app().agents.asks[0].clone();
        assert!(ask.why.always && !ask.why.session_ok, "{tool}: structure never takes a session grant");
        assert!(ask.facts.iter().any(|f| f.contains("can't be undone")), "{tool}: {:?}", ask.facts);
        ui.h.state_mut().agent_answer(ask.id, Answer::Once);
        ui.wait("the call", |a| a.agents.activity.iter().any(|x| x.id == ask.id && x.outcome != Outcome::Running));
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
    ui.wait("the approval", |a| !a.agents.asks.is_empty());
    let id = ui.app().agents.asks[0].id;
    ui.h.state_mut().agent_answer(id, Answer::Once);
    ui.wait("the call", |a| a.agents.activity.iter().any(|x| x.id == id && x.outcome != Outcome::Running));
    assert!(rx.try_recv().unwrap().unwrap_err().message.contains("already exists"));
    rt.block_on(async { d.collection::<Document>("agent_ui_made").drop().await.unwrap() });
}
