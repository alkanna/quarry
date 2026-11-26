//! AI agent tool calls on Core, with no window: the executor's refusals, approvals, writes and undo, and open_shell.

use super::agents::exec::*;
use super::agents::{AgentsA, ToolArgs};
use super::{Action, Refusal, RefusalKind};
use crate::agents::audit::{Entry, Summary};
use crate::agents::config::{Access, ConnPolicy, Mode, OpClass};
use crate::agents::link::ToolErr;
use crate::agents::tools::{self, Call, ConnRef};
use crate::core::Core;
use crate::db::sql::Dialect;
use crate::db::{self, Msg};
use crate::state::agents::{Activity, Answer, Outcome};
use crate::state::{Conn, ConnMeta, Sel, Status, Tab, View};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

const CONN: u64 = 21;

pub(super) fn agents_actions() -> Vec<AgentsA> {
    vec![
        AgentsA::Answer { id: 1, answer: Answer::Session },
        AgentsA::Undo { id: 1 },
        AgentsA::Clear,
        AgentsA::SetEnabled(true),
        AgentsA::SetAccess { conn: 1, access: Access::Read },
        AgentsA::Save,
        AgentsA::Config(Box::default()),
        AgentsA::Rules(None),
        AgentsA::OpenRules { scope: None, tab: Default::default() },
        AgentsA::RulesFor { scope: None, rule: None },
        AgentsA::ImportRules { scope: None, text: "[]".into() },
        AgentsA::Activity { open: true, filter: None },
        AgentsA::ActivityFilter(Default::default()),
        AgentsA::Pick(None),
        AgentsA::Page(None),
        AgentsA::Globs(String::new()),
        AgentsA::Client(Default::default()),
        AgentsA::EditRules,
        AgentsA::ToolCall { session: 1, call: 1, tool: "count".into(), args: ToolArgs(json!({ "collection": "orders" })), client: "claude-code".into() },
    ]
}

pub(super) fn agents_variant(a: &AgentsA) {
    match a {
        AgentsA::Answer { .. } | AgentsA::Undo { .. } | AgentsA::Clear | AgentsA::SetEnabled(_) | AgentsA::SetAccess { .. } | AgentsA::Save | AgentsA::Config(_) | AgentsA::Rules(_) | AgentsA::OpenRules { .. } | AgentsA::RulesFor { .. } | AgentsA::ImportRules { .. } | AgentsA::Activity { .. } | AgentsA::ActivityFilter(_) | AgentsA::Pick(_) | AgentsA::Page(_) | AgentsA::Globs(_) | AgentsA::Client(_) | AgentsA::EditRules | AgentsA::ToolCall { .. } => {}
    }
}

/// A MongoDB connection marked open without a client, shared with `read` as the read mode.
fn shared(app: &mut Core, read: Mode) {
    app.conns.push(Conn::new(ConnMeta { id: CONN, name: "prod".into(), ..Default::default() }, Status::Ready));
    app.agents.cfg.enabled = true;
    app.agents.cfg.conns.insert(CONN, ConnPolicy { access: Access::ReadWrite, read, change: Mode::Allow, structure: Mode::Ask, ..Default::default() });
}

fn call(app: &mut Core, link: u64, tool: &str, args: Value) -> oneshot::Receiver<Result<Value, ToolErr>> {
    let (reply, rx) = oneshot::channel();
    app.agent_tool_call(1, link, tool.into(), args, "claude-code".into(), reply);
    rx
}

fn refused(rx: &mut oneshot::Receiver<Result<Value, ToolErr>>) -> ToolErr {
    rx.try_recv().expect("answered at once").expect_err("refused")
}

fn find(filter: Value) -> Value {
    json!({ "connection": "prod", "database": "shop", "collection": "users", "filter": filter })
}

fn answer(app: &mut Core, id: u64, answer: Answer) {
    app.execute(Action::Agents(AgentsA::Answer { id, answer })).expect("an answer is never refused");
}

#[test]
fn refusals_are_answered_at_once_and_logged() {
    let mut core = Core::for_test();
    let app = &mut core;
    shared(app, Mode::Allow);
    let e = refused(&mut call(app, 1, "find", json!({ "connection": "prod", "database": "shop" })));
    assert_eq!(e.code, "bad_args");
    let e = refused(&mut call(app, 2, "find", json!({ "connection": "nope", "database": "shop", "collection": "users" })));
    assert!(e.message.contains("No connection named nope"), "{}", e.message);
    let e = refused(&mut call(app, 3, "select_rows", json!({ "connection": CONN, "database": "shop", "table": "users" })));
    assert_eq!(e.code, "wrong_engine");
    // The Secrets preset hides password: filtering on it would probe the value.
    let e = refused(&mut call(app, 4, "find", find(json!({ "password": { "$regex": "^a" } }))));
    assert_eq!(e.code, "guard");
    let e = refused(&mut call(app, 5, "run_command", json!({ "connection": "prod", "database": "shop", "command": { "shutdown": 1 } })));
    assert!(e.message.contains("not available to agents"), "{}", e.message);
    let e = refused(&mut call(app, 6, "find", json!({ "connection": "prod", "database": "local", "collection": "oplog.rs" })));
    assert!(e.message.contains("internals"), "{}", e.message);
    app.agents.cfg.conns.get_mut(&CONN).unwrap().read = Mode::Never;
    let e = refused(&mut call(app, 7, "count", find(json!({}))));
    assert!(e.message.contains("Never"), "{}", e.message);
    app.conns[0].status = Status::Idle;
    app.agents.cfg.conns.get_mut(&CONN).unwrap().read = Mode::Allow;
    let e = refused(&mut call(app, 8, "count", find(json!({}))));
    assert_eq!(e.code, "not_open");
    assert_eq!(app.agents.activity.len(), 8);
    assert!(app.agents.activity.iter().all(|a| matches!(a.outcome, Outcome::Refused(_))));
    // Arguments are scrubbed before they are shown or logged.
    let a = &app.agents.activity[3];
    assert!(a.args.contains("password") && !a.args.contains("^a"), "{}", a.args);
    app.agents.cfg.enabled = false;
    assert_eq!(refused(&mut call(app, 9, "count", find(json!({})))).code, "off");
}

#[test]
fn list_connections_needs_no_connection() {
    let mut core = Core::for_test();
    let app = &mut core;
    shared(app, Mode::Ask);
    let v = call(app, 1, "list_connections", json!({})).try_recv().unwrap().unwrap();
    assert_eq!(v["connections"][0]["name"], "prod");
    assert_eq!(app.agents.activity[0].outcome, Outcome::Allowed);
}

#[test]
fn ask_then_deny_then_cancel() {
    let mut core = Core::for_test();
    let app = &mut core;
    shared(app, Mode::Ask);
    let mut rx = call(app, 1, "count", json!({ "connection": "prod", "database": "shop", "collection": "users", "reason": "count\nusers" }));
    assert!(rx.try_recv().is_err(), "waits for the user");
    let ask = app.agents.asks[0].clone();
    assert_eq!((ask.op.as_str(), ask.reason.as_deref(), ask.why.setting, ask.why.session_ok), ("COUNT", Some("count users"), true, true));
    assert_eq!(app.agents.activity[0].outcome, Outcome::Waiting);
    app.agent_answer(ask.id, Answer::Deny);
    assert_eq!(rx.try_recv().unwrap().unwrap_err().message, DENIED);
    assert!(app.agents.asks.is_empty());
    assert_eq!(app.agents.activity[0].outcome, Outcome::Denied);
    // A cancel closes the approval as gone and nothing runs.
    let mut rx = call(app, 2, "count", find(json!({})));
    let id = app.agents.asks[0].id;
    app.inject(Msg::Agent(db::AgentEvent::Cancel { session: 1, id: 2 }));
    assert!(app.agents.asks[0].gone);
    assert_eq!(app.agent_activity(id).unwrap().outcome, Outcome::Cancelled);
    app.agent_answer(id, Answer::Once);
    assert!(rx.try_recv().is_err(), "the reply was dropped, never answered");
    assert!(app.agents.exec.running.is_empty());
    assert!(app.agents.asks.is_empty(), "Once on a gone call only closes it");
}

#[test]
fn allowing_rechecks_the_policy_and_session_grants() {
    let mut core = Core::for_test();
    let app = &mut core;
    shared(app, Mode::Ask);
    let mut rx = call(app, 1, "count", find(json!({})));
    let id = app.agents.asks[0].id;
    // Unshared while the dialog was open.
    app.agents.cfg.conns.get_mut(&CONN).unwrap().access = Access::NotShared;
    app.agent_answer(id, Answer::Once);
    assert!(rx.try_recv().unwrap().unwrap_err().message.contains("not shared"));
    app.agents.cfg.conns.get_mut(&CONN).unwrap().access = Access::Read;
    let mut rx = call(app, 2, "count", find(json!({})));
    let id = app.agents.asks[0].id;
    app.agent_answer(id, Answer::Session);
    // Allowed, then it fails to run since the test connection has no client; the grant stays.
    assert_eq!(rx.try_recv().unwrap().unwrap_err().code, "not_open");
    let mut rx = call(app, 3, "count", find(json!({})));
    assert!(app.agents.asks.is_empty(), "the session grant covers the next read");
    assert_eq!(rx.try_recv().unwrap().unwrap_err().code, "not_open");
    app.inject(Msg::Agent(db::AgentEvent::Closed { session: 1 }));
    let _rx = call(app, 4, "count", find(json!({})));
    assert_eq!(app.agents.asks.len(), 1, "grants end with their link session");
}

#[test]
fn rules_that_do_not_compile_refuse_and_recompile_on_change() {
    let mut core = Core::for_test();
    let app = &mut core;
    shared(app, Mode::Allow);
    app.agents.cfg.rules.push(crate::agents::config::RuleSpec { on: true, matcher: crate::agents::config::Matcher::FieldName("(".into()), scope: Vec::new(), action: crate::agents::config::Action::Remove, mode: Default::default() });
    app.agents.rev += 1;
    assert_eq!(refused(&mut call(app, 1, "count", find(json!({})))).code, "rules");
    app.agents.cfg.rules.clear();
    app.agents.rev += 1;
    let mut rx = call(app, 2, "count", find(json!({})));
    assert_eq!(rx.try_recv().unwrap().unwrap_err().code, "not_open", "allowed this time, then no client");
}

#[test]
fn activity_is_capped() {
    let mut core = Core::for_test();
    let app = &mut core;
    shared(app, Mode::Allow);
    for i in 0..(ACTIVITY_MAX as u64 + 20) {
        let _ = call(app, i, "find", json!({}));
    }
    assert_eq!(app.agents.activity.len(), ACTIVITY_MAX);
    assert_eq!(app.agents.activity.back().unwrap().id, ACTIVITY_MAX as u64 + 20);
}

#[test]
fn the_audit_tail_comes_first() {
    let mut core = Core::for_test();
    let app = &mut core;
    shared(app, Mode::Allow);
    let _ = call(app, 1, "list_connections", json!({}));
    let e = |d: &str, err: Option<&str>| Entry { time: 1_700_000_000, session: 9, client: "cursor".into(), tool: "find".into(), decision: d.into(), result: Summary { error: err.map(String::from), count: Some(3), ..Default::default() }, ..Default::default() };
    app.inject(Msg::Agent(db::AgentEvent::Tail(vec![e("allowed", None), e("refused", Some("x")), e("approved", Some("boom"))])));
    let o: Vec<Outcome> = app.agents.activity.iter().map(|a| a.outcome.clone()).collect();
    assert_eq!(o, [Outcome::Allowed, Outcome::Refused(String::new()), Outcome::Failed("boom".into()), Outcome::Allowed]);
    assert_eq!(app.agents.activity[0].summary, "3 returned");
}

fn insert(n: usize) -> Value {
    json!({ "connection": "prod", "database": "shop", "collection": "users", "documents": (0..n).map(|i| json!({ "a": i })).collect::<Vec<_>>() })
}

#[test]
fn a_write_too_large_to_undo_always_asks_and_runs_only_without_undo() {
    let mut core = Core::for_test();
    let app = &mut core;
    shared(app, Mode::Allow);
    app.conns[0].writable = true;
    app.agents.cfg.limits.undo_docs = 2;
    // Change data is set to Allow, but nothing could take this back.
    let mut rx = call(app, 1, "insert", insert(3));
    assert!(rx.try_recv().is_err(), "waits for the user");
    let ask = app.agents.asks[0].clone();
    assert!(ask.no_undo.as_deref().is_some_and(|w| w.contains("more than the 2")), "{:?}", ask.no_undo);
    assert_eq!(ask.facts, ["Inserts 3 documents."]);
    assert!(!ask.why.session_ok);
    app.agent_answer(ask.id, Answer::WithoutUndo);
    // Allowed, then it fails to run since the test connection has no client.
    assert_eq!(rx.try_recv().unwrap().unwrap_err().code, "not_open");
    // Within the cap it runs at once.
    let mut rx = call(app, 2, "insert", insert(2));
    assert!(app.agents.asks.is_empty());
    assert_eq!(rx.try_recv().unwrap().unwrap_err().code, "not_open");
}

#[test]
fn a_write_on_a_locked_connection_asks_to_unlock() {
    let mut core = Core::for_test();
    let app = &mut core;
    shared(app, Mode::Allow);
    let mut rx = call(app, 1, "insert", insert(1));
    let ask = app.agents.asks[0].clone();
    assert!(ask.why.unlock && ask.no_undo.is_none());
    app.agent_answer(ask.id, Answer::UnlockOnce);
    assert!(app.conns[0].writable, "Unlock writes and allow unlocks the connection");
    assert_eq!(rx.try_recv().unwrap().unwrap_err().code, "not_open");
}

/// A script can undo the shell's read-only guard, so a locked connection is never offered as "runs read-only": the
/// approval asks to unlock, and allowing unlocks it.
#[test]
fn run_shell_on_a_locked_connection_asks_to_unlock() {
    let mut core = Core::for_test();
    let app = &mut core;
    shared(app, Mode::Allow);
    app.agents.cfg.presets = crate::agents::scrub::tests_support::no_presets();
    app.agents.cfg.conns.get_mut(&CONN).unwrap().scripts = Mode::Allow;
    let mut rx = call(app, 1, "run_shell", json!({ "connection": "prod", "database": "shop", "script": "async function run() { return 1 }" }));
    let ask = app.agents.asks[0].clone();
    assert!(ask.why.unlock && ask.why.always && !ask.why.session_ok, "{:?}", ask.why);
    assert!(ask.facts.iter().all(|f| !f.contains("read-only")), "{:?}", ask.facts);
    app.agent_answer(ask.id, Answer::UnlockOnce);
    assert!(app.conns[0].writable);
    assert_eq!(rx.try_recv().unwrap().unwrap_err().code, "not_open", "allowed, then no client in the test");
}

/// A MySQL connection marked open without a pool, shared for reading and writing, with no rule presets so nothing hides
/// data.
fn shared_sql(app: &mut Core, engine: db::sql::Engine) {
    app.conns.push(Conn::new(ConnMeta { id: CONN, name: "shop".into(), engine, ..Default::default() }, Status::Ready));
    app.agents.cfg.enabled = true;
    app.agents.cfg.presets = crate::agents::scrub::tests_support::no_presets();
    app.agents.cfg.conns.insert(CONN, ConnPolicy { access: Access::ReadWrite, scripts: Mode::Allow, structure: Mode::Never, ..Default::default() });
}

fn sql_script() -> Value {
    json!({ "connection": "shop", "database": "shop", "script": "async function run() { return await sql`SELECT id FROM users` }" })
}

/// run_shell reaches SQL connections. A locked one never asks to unlock: the script's statements come back through
/// Quarry's relay, which runs them read-only, and allowing it leaves the connection locked. The approval says so and
/// names what each statement is checked for.
#[test]
fn run_shell_on_sql_runs_read_only_when_locked() {
    for engine in [db::sql::Engine::MySql, db::sql::Engine::Postgres] {
        let mut core = Core::for_test();
        let app = &mut core;
        shared_sql(app, engine);
        // A PostgreSQL call needs the session to know its database; this one never connects.
        let pg = engine == db::sql::Engine::Postgres;
        if pg {
            app.conns[0].sql = Some(db::sql::Db::offline_pg(false, Default::default()));
        }
        let mut rx = call(app, 1, "run_shell", sql_script());
        assert!(rx.try_recv().is_err(), "{engine:?}: every script asks");
        let ask = app.agents.asks[0].clone();
        assert!(!ask.why.unlock && ask.why.always && !ask.why.session_ok, "{engine:?}: {:?}", ask.why);
        assert!(ask.facts.iter().any(|f| f.starts_with("Runs read-only")), "{engine:?}: {:?}", ask.facts);
        let checks = ask.facts.iter().find(|f| f.starts_with("Quarry checks each statement")).expect("the statement checks are listed");
        assert!(checks.contains("user and role") && checks.contains("structure changes") && !checks.contains("data changes"), "{checks}");
        assert!(!app.agent_run_writable(&app.agents.exec.pending[&ask.id]), "{engine:?}: queued locked, runs read-only");
        app.agent_answer(ask.id, Answer::Once);
        assert!(!app.conns[0].writable, "{engine:?}: allowing a SQL script never unlocks");
        // Allowed, then no pool (MySQL) or no shell in a test window (PostgreSQL).
        assert_eq!(rx.try_recv().unwrap().unwrap_err().code, if pg { "failed" } else { "not_open" }, "{engine:?}");
        // Unlocked when it was queued, it may write.
        app.conns[0].writable = true;
        let _rx = call(app, 2, "run_shell", sql_script());
        let ask = app.agents.asks[0].clone();
        assert!(app.agent_run_writable(&app.agents.exec.pending[&ask.id]));
        assert!(!ask.facts.iter().any(|f| f.starts_with("Runs read-only")), "{:?}", ask.facts);
    }
}

/// The same settings hold for SQL scripts as for free SQL: rules that hide data turn them off, shared read only and
/// Never refuse them.
#[test]
fn run_shell_on_sql_follows_the_connection_settings() {
    let mut core = Core::for_test();
    let app = &mut core;
    shared_sql(app, db::sql::Engine::MySql);
    // The Secrets preset hides password, and with no catalog loaded every column could be one.
    app.agents.cfg.presets = Default::default();
    app.agents.rev += 1;
    let e = refused(&mut call(app, 1, "run_shell", sql_script()));
    assert!(e.message.contains("the shell are off"), "{}", e.message);
    app.agents.cfg.presets = crate::agents::scrub::tests_support::no_presets();
    app.agents.rev += 1;
    app.agents.cfg.conns.get_mut(&CONN).unwrap().access = Access::Read;
    let e = refused(&mut call(app, 2, "run_shell", sql_script()));
    assert!(e.message.contains("shared read only"), "{}", e.message);
    app.agents.cfg.conns.get_mut(&CONN).unwrap().access = Access::ReadWrite;
    app.agents.cfg.conns.get_mut(&CONN).unwrap().scripts = Mode::Never;
    let e = refused(&mut call(app, 3, "run_shell", sql_script()));
    assert!(e.message.contains("Run scripts is set to Never"), "{}", e.message);
    // MongoDB tools still refuse a SQL connection, and say the shell works there.
    let e = refused(&mut call(app, 4, "find", json!({ "connection": "shop", "database": "shop", "collection": "users" })));
    assert!(e.code == "wrong_engine" && e.message.contains("run_shell"), "{}", e.message);
}

/// A structure change has no undo, so a cancel never stops it half way: it finishes and the Activity says how it
/// ended, here failing on a server nobody answers at.
#[test]
fn a_cancel_lets_a_change_without_undo_finish() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let client = rt.block_on(mongodb::Client::with_uri_str("mongodb://127.0.0.1:9/?serverSelectionTimeoutMS=300&connectTimeoutMS=300")).unwrap();
    let mut core = Core::for_test();
    let id = {
        let app = &mut core;
        shared(app, Mode::Allow);
        app.conns[0].writable = true;
        app.conns[0].client = Some(client);
        let _rx = call(app, 1, "drop_collection", json!({ "connection": "prod", "database": "shop", "collection": "old" }));
        let id = app.agents.asks[0].id;
        app.agent_answer(id, Answer::Once);
        assert!(app.agents.exec.running.get(&id).is_some_and(|r| r.write && r.structure));
        app.inject(Msg::Agent(db::AgentEvent::Cancel { session: 1, id: 1 }));
        assert!(app.agents.exec.running.contains_key(&id), "still running after the cancel");
        assert_eq!(app.agent_activity(id).unwrap().outcome, Outcome::Running);
        id
    };
    let t = Instant::now();
    let outcome = loop {
        core.tick();
        let o = core.agents.activity.iter().find(|a| a.id == id).unwrap().outcome.clone();
        if o != Outcome::Running {
            break o;
        }
        assert!(t.elapsed() < Duration::from_secs(30), "the drop never ended");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(matches!(outcome, Outcome::Failed(_)), "{outcome:?}");
    assert!(core.agents.exec.running.is_empty());
}

/// An index build Quarry stops waiting for at the time limit goes on on the server, so the Activity says it is still
/// running rather than failed.
#[test]
fn a_structure_change_past_the_time_limit_is_still_running() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    // Nobody answers there, and the driver keeps looking for a server well past the 1 s limit.
    let client = rt.block_on(mongodb::Client::with_uri_str("mongodb://127.0.0.1:9/?serverSelectionTimeoutMS=60000")).unwrap();
    let mut core = Core::for_test();
    let id = {
        let app = &mut core;
        shared(app, Mode::Allow);
        app.agents.cfg.limits.secs = 1;
        app.conns[0].writable = true;
        app.conns[0].client = Some(client);
        let _rx = call(app, 1, "create_index", json!({ "connection": "prod", "database": "shop", "collection": "orders", "keys": { "at": -1 } }));
        let id = app.agents.asks[0].id;
        app.agent_answer(id, Answer::Once);
        id
    };
    let t = Instant::now();
    let a = loop {
        core.tick();
        let a = core.agents.activity.iter().find(|a| a.id == id).unwrap().clone();
        if a.outcome != Outcome::Running {
            break a;
        }
        assert!(t.elapsed() < Duration::from_secs(30), "the call never ended");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(a.outcome, Outcome::Approved);
    assert!(a.summary.starts_with("Still running on the server"), "{}", a.summary);
}

/// run_sql runs as the dialog said when it was queued: unlocking while the dialog is open never turns an approved
/// read-only run into a write, and locking meanwhile makes it read-only.
#[test]
fn run_sql_keeps_the_lock_it_was_asked_under() {
    let mut core = Core::for_test();
    let app = &mut core;
    app.conns.push(Conn::new(ConnMeta { id: CONN, name: "shop".into(), engine: db::sql::Engine::MySql, ..Default::default() }, Status::Ready));
    app.agents.cfg.enabled = true;
    app.agents.cfg.presets = crate::agents::scrub::tests_support::no_presets();
    app.agents.cfg.conns.insert(CONN, ConnPolicy { access: Access::ReadWrite, scripts: Mode::Ask, structure: Mode::Ask, ..Default::default() });
    let sql = json!({ "connection": "shop", "sql": "UPDATE t SET a = 1" });
    let _rx = call(app, 1, "run_sql", sql.clone());
    let locked = app.agents.asks[0].clone();
    assert!(!locked.why.unlock && locked.facts.iter().any(|f| f.starts_with("Runs read-only")), "{:?}", locked.facts);
    app.conns[0].writable = true;
    assert!(!app.agent_run_writable(&app.agents.exec.pending[&locked.id]), "unlocked while the dialog was open: still read-only");
    let _rx = call(app, 2, "run_sql", sql);
    let open = app.agents.asks[1].clone();
    assert!(app.agent_run_writable(&app.agents.exec.pending[&open.id]));
    app.conns[0].writable = false;
    assert!(!app.agent_run_writable(&app.agents.exec.pending[&open.id]), "locked meanwhile: read-only");
    // A call whose approval unlocks the connection writes once it did.
    let _rx = call(app, 3, "create_table", json!({ "connection": "shop", "database": "shop", "table": "t2", "columns": [{ "name": "a", "type": "int" }] }));
    let unlock = app.agents.asks[2].clone();
    assert!(unlock.why.unlock);
    assert!(!app.agent_run_writable(&app.agents.exec.pending[&unlock.id]));
    app.conns[0].writable = true;
    assert!(app.agent_run_writable(&app.agents.exec.pending[&unlock.id]));
}

/// What a cancel lets finish and what marks open tabs as changed by an agent.
#[test]
fn what_counts_as_a_change() {
    let c = |call: Call, writable: bool| {
        let class = tools::classify(&call, Some(Dialect::MySql));
        changes_server(&call, class, writable, true)
    };
    let run = || Call::RunSql { conn: ConnRef::Id(1), db: None, sql: "UPDATE t SET a = 1".into() };
    assert!(!c(run(), false), "free SQL on a locked connection runs read-only and marks no tab");
    assert!(c(run(), true));
    let shell = || Call::RunShell { conn: ConnRef::Id(1), db: None, script: "async function run() { return 1 }".into() };
    assert!(c(shell(), true));
    assert!(!c(shell(), false), "a SQL script on a locked connection runs read-only");
    assert!(changes_server(&shell(), OpClass::RunScripts, false, false), "the MongoDB shell's guard is not a boundary");
    let cmd = |d: mongodb::bson::Document| Call::RunCommand { conn: ConnRef::Id(1), db: "shop".into(), command: d };
    assert!(c(cmd(mongodb::bson::doc! { "drop": "x" }), true));
    assert!(!c(cmd(mongodb::bson::doc! { "ping": 1 }), true));
    let ns = || crate::agents::tools::Ns { conn: ConnRef::Id(1), db: "shop".into(), coll: "t".into() };
    assert!(c(Call::DropTable { ns: ns() }, true));
    assert!(c(Call::SqlDeleteRows { ns: ns(), wher: Vec::new() }, true));
    assert!(!c(Call::SelectRows { ns: ns(), columns: Vec::new(), wher: Vec::new(), order: Vec::new(), limit: None, offset: 0 }, true));
}

#[test]
fn undo_over_the_total_cap_expires_the_oldest() {
    let mut core = Core::for_test();
    let app = &mut core;
    for id in 1..=3 {
        app.agent_push(Activity { id, time: String::new(), session: 1, client: String::new(), tool: "insert".into(), conn: Some(CONN), ns: "shop.users".into(), database: String::new(), outcome: Outcome::Allowed, summary: "1 inserted".into(), args: String::new(), answer: String::new(), undo: None });
    }
    let u = || db::AgentUndo::Inserted { db: "shop".into(), coll: "users".into(), docs: vec![mongodb::bson::doc! { "_id": 1, "pad": "x".repeat(100) }] };
    let one = u().bytes();
    app.agents.cfg.limits.undo_total_bytes = one * 2;
    app.agent_keep_undo(1, u());
    app.agent_keep_undo(2, u());
    assert_eq!(app.agent_activity(1).unwrap().undo, Some(1));
    app.agent_keep_undo(3, u());
    let first = app.agent_activity(1).unwrap().clone();
    assert_eq!(first.undo, None);
    assert!(first.summary.ends_with("undo expired"), "{}", first.summary);
    assert_eq!((app.agent_activity(2).unwrap().undo, app.agent_activity(3).unwrap().undo), (Some(2), Some(3)));
    assert_eq!(app.agents.undo.total(), one * 2);
}

#[test]
fn hash_keys_are_derived_not_copied() {
    let k = [9u8; 32];
    assert_ne!(hash_key_from(&k), k);
    assert_eq!(hash_key_from(&k), hash_key_from(&k));
}

/// Refused at once, asked then approved, asked then denied: each through the actions the approval dialog sends.
#[test]
fn refuse_ask_then_approve_and_ask_then_deny() {
    let mut core = Core::for_test();
    let app = &mut core;
    shared(app, Mode::Never);
    let e = refused(&mut call(app, 1, "count", find(json!({}))));
    assert_eq!(e.code, "refused");
    assert!(app.agents.asks.is_empty());
    app.agents.cfg.conns.get_mut(&CONN).unwrap().read = Mode::Ask;
    let mut rx = call(app, 2, "count", find(json!({})));
    assert!(rx.try_recv().is_err(), "waits for the user");
    let id = app.agents.asks[0].id;
    answer(app, id, Answer::Once);
    assert!(app.agents.asks.is_empty());
    // Allowed after the policy ran again, then it cannot start: the test connection has no client.
    assert_eq!(rx.try_recv().unwrap().unwrap_err().code, "not_open");
    assert!(matches!(app.agent_activity(id).unwrap().outcome, Outcome::Failed(_)));
    let mut rx = call(app, 3, "count", find(json!({})));
    let id = app.agents.asks[0].id;
    answer(app, id, Answer::Deny);
    assert_eq!(rx.try_recv().unwrap().unwrap_err().message, DENIED);
    assert_eq!(app.agent_activity(id).unwrap().outcome, Outcome::Denied);
    assert!(app.agents.exec.pending.is_empty() && app.agents.exec.running.is_empty());
    // Only a call the link handed over runs: an action naming no waiting call is refused and runs nothing.
    let before = app.agents.activity.len();
    let r = app.execute(Action::Agents(AgentsA::ToolCall { session: 1, call: 99, tool: "count".into(), args: ToolArgs(find(json!({}))), client: "claude-code".into() }));
    assert!(matches!(r, Err(Refusal { kind: RefusalKind::Invalid, .. })));
    assert_eq!(app.agents.activity.len(), before);
}

/// A write on a locked connection asks to unlock; denying it or allowing without the unlock leaves it locked.
#[test]
fn a_write_on_a_locked_connection_stays_locked_unless_allowed_with_unlock() {
    let mut core = Core::for_test();
    let app = &mut core;
    shared(app, Mode::Allow);
    let ins = || json!({ "connection": "prod", "database": "shop", "collection": "users", "documents": [{ "a": 1 }] });
    let mut rx = call(app, 1, "insert", ins());
    let ask = app.agents.asks[0].clone();
    assert!(ask.why.unlock, "{:?}", ask.why);
    answer(app, ask.id, Answer::Deny);
    assert!(!app.conns[0].writable, "denying never unlocks");
    assert_eq!(rx.try_recv().unwrap().unwrap_err().code, "denied");
    let mut rx = call(app, 2, "insert", ins());
    let id = app.agents.asks[0].id;
    answer(app, id, Answer::Once);
    let e = rx.try_recv().unwrap().unwrap_err();
    assert!(e.code == "refused" && e.message.contains("locked"), "{e:?}");
    assert!(!app.conns[0].writable);
    assert!(app.agents.exec.running.is_empty());
}

/// A field the rules hide is refused wherever the call names it, before anything is read or queued, and its value
/// never reaches the Activity list.
#[test]
fn a_hidden_field_is_refused() {
    let mut core = Core::for_test();
    let app = &mut core;
    shared(app, Mode::Allow);
    app.conns[0].writable = true;
    let e = refused(&mut call(app, 1, "find", json!({ "connection": "prod", "database": "shop", "collection": "users", "filter": {}, "sort": { "password": 1 } })));
    assert_eq!(e.code, "guard");
    let e = refused(&mut call(app, 2, "update", json!({ "connection": "prod", "database": "shop", "collection": "users", "filter": {}, "update": { "$set": { "password": "hunter2" } } })));
    assert_eq!(e.code, "guard");
    assert!(app.agents.exec.preparing.is_empty() && app.agents.asks.is_empty(), "nothing read or queued");
    assert!(app.agents.activity.iter().all(|a| matches!(a.outcome, Outcome::Refused(_)) && !a.args.contains("hunter2")));
}

/// Undo from the agents' own store: a write, so a locked connection only warns and keeps the entry; once it runs, its
/// answer comes back as an agent event and never lands on the user's undo list.
#[test]
fn undo_from_the_agent_store() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let client = rt.block_on(mongodb::Client::with_uri_str("mongodb://127.0.0.1:9/?serverSelectionTimeoutMS=300&connectTimeoutMS=300")).unwrap();
    let mut core = Core::for_test();
    let app = &mut core;
    shared(app, Mode::Allow);
    app.conns[0].status = Status::Idle;
    app.agent_push(Activity { id: 1, time: String::new(), session: 1, client: String::new(), tool: "insert".into(), conn: Some(CONN), ns: "shop.users".into(), database: String::new(), outcome: Outcome::Allowed, summary: "1 inserted".into(), args: String::new(), answer: String::new(), undo: None });
    app.agent_keep_undo(1, db::AgentUndo::Inserted { db: "shop".into(), coll: "users".into(), docs: vec![mongodb::bson::doc! { "_id": 1 }] });
    let undo = || Action::Agents(AgentsA::Undo { id: 1 });
    let e: Refusal = app.execute(undo()).unwrap_err();
    assert_eq!(e.kind, RefusalKind::Invalid, "a warning, not the Unlock dialog");
    assert!(e.msg.contains("Undo is a write"), "{}", e.msg);
    assert_eq!(app.agent_activity(1).unwrap().undo, Some(1));
    app.conns[0].writable = true;
    assert_eq!(app.execute(undo()).unwrap_err().kind, RefusalKind::Offline);
    assert!(app.agents.undo.total() > 0, "kept while it cannot run");
    app.conns[0].client = Some(client);
    app.conns[0].status = Status::Ready;
    app.tabs.push(Tab::new(Sel { conn: CONN, db: "shop".into(), coll: "users".into(), schema: String::new() }));
    app.execute(undo()).unwrap();
    assert_eq!(app.agents.undo.total(), 0);
    let a = app.agent_activity(1).unwrap();
    assert_eq!((a.undo, a.summary.as_str()), (None, "1 inserted · undoing"));
    app.inject(Msg::Agent(db::AgentEvent::Undone { activity: 1, res: Ok("Undone.".into()), back: None }));
    assert_eq!(app.agent_activity(1).unwrap().summary, "1 inserted · undone");
    assert!(app.tabs[0].agent_changed, "the open tab says an agent changed it");
    assert!(app.undo.is_empty(), "never on the user's undo list");
}

#[test]
fn agents_see_only_shared_connections_without_secrets() {
    let mut app = Core::for_test();
    for (id, name) in [(11, "shared"), (12, "hidden"), (13, "read")] {
        app.conns.push(Conn { masked_uri: "mongodb://u:***@db.example.com".into(), ..Conn::new(ConnMeta { id, name: name.into(), tag: "Production".into(), ..Default::default() }, Status::Idle) });
    }
    app.agents.cfg.enabled = true;
    app.agents.cfg.conns.insert(11, ConnPolicy { access: Access::ReadWrite, ..Default::default() });
    app.agents.cfg.conns.insert(12, ConnPolicy::default());
    app.agents.cfg.conns.insert(13, ConnPolicy { access: Access::Read, ..Default::default() });
    let v = app.agent_connections();
    let list = v["connections"].as_array().unwrap();
    assert_eq!(list.iter().map(|c| c["name"].as_str().unwrap()).collect::<Vec<_>>(), ["shared", "read"]);
    assert_eq!(list[0]["access"], "read_write");
    assert_eq!(list[0]["id"], "11");
    assert!(!v.to_string().contains("example.com"));
}

mod open_shell {
    use super::*;

    fn call(app: &mut Core, args: Value) -> Result<Value, ToolErr> {
        let (reply, mut rx) = oneshot::channel();
        app.agent_tool_call(1, 1, "open_shell".into(), args, "claude-code".into(), reply);
        rx.try_recv().expect("answered at once")
    }

    /// prod (MongoDB, open) and pg (PostgreSQL, closed) shared with agents, hidden (MySQL) not.
    fn setup(app: &mut Core) {
        app.agents.cfg.enabled = true;
        for (id, name, engine, status, shared) in [(21, "prod", db::sql::Engine::default(), Status::Ready, true), (22, "pg", db::sql::Engine::Postgres, Status::Idle, true), (23, "hidden", db::sql::Engine::MySql, Status::Ready, false)] {
            app.conns.push(Conn::new(ConnMeta { id, name: name.into(), engine, ..Default::default() }, status));
            if shared {
                app.agents.cfg.conns.insert(id, ConnPolicy { access: Access::Read, scripts: Mode::Ask, ..Default::default() });
            }
        }
    }

    const SCRIPT: &str = "async function run() { return await db.collection(\"users\").find().toArray() }";

    #[test]
    fn opens_edits_and_leaves_the_users_tabs_alone() {
        let mut core = Core::for_test();
        let app = &mut core;
        setup(app);
        let attach = json!([{ "connection": "pg", "database": "sales" }, { "connection": "prod", "database": "app", "name": "travels" }]);
        let v = call(app, json!({ "connection": "prod", "database": "shop", "script": SCRIPT, "attach": attach })).unwrap();
        assert_eq!(v["shown"], true);
        assert_eq!(v["attached"], json!(["sales", "travels"]));
        let uid = v["tab"].as_u64().unwrap();
        // Sitting on a Shell tab is no reason to open the next one behind it.
        let second = call(app, json!({ "connection": "prod", "database": "shop", "script": SCRIPT })).unwrap();
        assert_eq!(second["shown"], true);
        let first = app.tabs.iter().position(|t| t.uid == uid).unwrap();
        app.set_cur(first);
        let t = app.t().unwrap();
        assert!(t.view == View::Shell);
        assert_eq!((t.uid, t.shell.code.as_str()), (uid, SCRIPT));
        let a = &t.shell.attached;
        assert_eq!((a[0].conn, a[0].schema.as_str(), a[1].schema.as_str()), (22, crate::state::shell_attach::PG_SCHEMA, ""));
        assert!(a.iter().all(|a| !a.allow_writes), "only the user allows writes");

        let tabs = app.tabs.len();
        let refuse = |app: &mut Core, args: Value| call(app, args).unwrap_err();
        let e = refuse(app, json!({ "connection": "prod", "database": "shop", "script": SCRIPT, "attach": [{ "connection": "hidden", "database": "x" }] }));
        assert_eq!(e.code, "not_shared");
        let e = refuse(app, json!({ "connection": "prod", "database": "shop", "script": SCRIPT, "attach": [{ "connection": "pg", "database": "x", "name": "db" }] }));
        assert!(e.message.contains("db is already"), "{}", e.message);
        let e = refuse(app, json!({ "connection": "prod", "database": "shop", "schema": "public", "script": SCRIPT }));
        assert_eq!(e.code, "bad_args");
        assert_eq!(refuse(app, json!({ "connection": "pg", "database": "sales", "script": SCRIPT })).code, "not_open");
        app.agents.cfg.conns.get_mut(&21).unwrap().scope = crate::agents::config::Scope { mode: crate::agents::config::ScopeMode::Only, globs: vec!["shop".into(), "app".into()] };
        assert!(refuse(app, json!({ "connection": "prod", "database": "payroll", "script": SCRIPT })).message.contains("payroll is not shared"));
        app.agents.cfg.conns.get_mut(&21).unwrap().scope = Default::default();
        assert_eq!(app.tabs.len(), tabs, "a refusal opens nothing");

        // Writes the user allowed for the script they read do not carry over to the agent's next one.
        app.tm().unwrap().shell.attached[1].allow_writes = true;
        let edited = SCRIPT.replace("users", "trips");
        let v = call(app, json!({ "connection": "prod", "database": "shop", "script": edited, "tab": uid })).unwrap();
        assert_eq!(v["tab"], uid);
        assert_eq!(app.t().unwrap().shell.code, edited);
        assert_eq!(app.t().unwrap().shell.attached.len(), 2, "an edit without `attach` keeps the attachments");
        assert!(app.t().unwrap().shell.attached.iter().all(|a| !a.allow_writes));

        app.tm().unwrap().shell.code.push_str("\n// mine");
        let e = refuse(app, json!({ "connection": "prod", "database": "shop", "script": SCRIPT, "tab": uid }));
        assert!(e.message.contains("The user changed"), "{}", e.message);
        let mine = app.tabs.iter().find(|t| t.shell.agent_code.is_none()).map(|t| t.uid);
        if let Some(mine) = mine {
            assert!(refuse(app, json!({ "connection": "prod", "database": "shop", "script": SCRIPT, "tab": mine })).message.contains("not opened by open_shell"));
        }

        // A dialog open: the tab opens behind it and the current tab stays.
        app.agents.ui.modal = true;
        let cur = app.cur;
        let v = call(app, json!({ "connection": "prod", "database": "shop", "script": SCRIPT })).unwrap();
        assert_eq!((v["shown"].as_bool(), app.cur), (Some(false), cur));
        assert_eq!(app.tabs.last().unwrap().uid, v["tab"].as_u64().unwrap());
        // The Run confirm runs the code as it is when confirmed, so no edit lands under a dialog.
        assert_eq!(refuse(app, json!({ "connection": "prod", "database": "shop", "script": SCRIPT, "tab": v["tab"] })).code, "busy");
        assert_eq!(app.agents.activity.len(), 11);
    }
}

/// Tool arguments can hold passwords; the action's Debug, which tests and panics print, shows only their size.
#[test]
fn a_tool_calls_arguments_never_show_in_debug() {
    let a = Action::Agents(AgentsA::ToolCall { session: 1, call: 1, tool: "run_sql".into(), args: ToolArgs(json!({ "sql": "CREATE USER app IDENTIFIED BY 'hunter2'" })), client: "claude-code".into() });
    let shown = format!("{a:?}");
    assert!(!shown.contains("hunter2") && shown.contains("bytes"), "{shown}");
}
