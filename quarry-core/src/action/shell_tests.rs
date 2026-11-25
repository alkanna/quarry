//! The Shell, Watch and database tools run headlessly, offline with made-up answers.

use super::shell::{dangerous_words, shell_ops};
use super::*;
use crate::db;
use crate::state::{Conn, ConnMeta, Sel, ShellTab, Status, Tab, ToolRun, View};
use mongodb::bson::{doc, Bson};

const CONN: u64 = 1;

/// A core with one MongoDB connection, writes locked, and one tab on its collection `orders`.
fn offline() -> (Core, u64) {
    let mut core = Core::for_test();
    core.conns.push(Conn::new(ConnMeta { id: CONN, name: "test".into(), ..Default::default() }, Status::Ready));
    let t = Tab::new(Sel { conn: CONN, db: "shop".into(), coll: "orders".into(), ..Default::default() });
    let uid = t.uid;
    core.tabs.push(t);
    (core, uid)
}

fn shell(core: &mut Core, a: ShellA) -> Result<Reply, Refusal> {
    core.execute(Action::Shell(a))
}

fn confirms(core: &mut Core) -> Option<Vec<String>> {
    core.take_ui_fx().into_iter().find_map(|fx| match fx {
        UiFx::Open(Dialog::ShellWrite(ops)) => Some(ops),
        _ => None,
    })
}

#[test]
fn destructive_words_by_engine() {
    assert_eq!(dangerous_words("db.x.deleteMany({})", false), ["delete"]);
    assert_eq!(dangerous_words("db.x.aggregate([{ $merge: { into: 'y' } }])", false), ["$merge"]);
    assert_eq!(dangerous_words("await orders.delete({ id: 1 }); sql`TRUNCATE t`", true), ["delete", "truncate"]);
    assert_eq!(dangerous_words("sql`CALL p(); LOCK TABLES t READ; KILL 5; LOAD DATA INFILE 'x' INTO TABLE t`", true), ["call", "load", "lock", "kill"]);
    assert!(dangerous_words("sql`SELECT created_at, updated_at FROM orders`", true).is_empty());
    // A script with both engines is checked for both, each word once.
    assert_eq!(shell_ops("x.deleteMany({}); sql`DELETE FROM t`", false, [true].into_iter()), ["delete"]);
    assert_eq!(shell_ops("db.x.aggregate([{ $out: 'y' }]); sql`TRUNCATE t`", true, [false].into_iter()), ["$out", "truncate"]);
    assert!(shell_ops("sql`SELECT 1`", true, std::iter::empty()).is_empty());
}

/// Run asks first only when the script may write and something it reaches is unlocked; a locked run goes straight on, since
/// the runner refuses writes there. The Shell never refuses a run as read-only.
#[test]
fn run_confirms_a_writing_script_only_when_unlocked() {
    let (mut core, tab) = offline();
    core.demo = true;
    core.tabs[0].shell.code = "async function run() { return await orders.deleteMany({}); }".into();
    assert_eq!(shell(&mut core, ShellA::Run { tab }), Ok(Reply::Done));
    assert!(confirms(&mut core).is_none(), "a locked connection asked to confirm");
    assert_eq!(core.status, "Offline demo: the shell needs a real connection.");

    core.conns[0].writable = true;
    assert_eq!(shell(&mut core, ShellA::Run { tab }), Ok(Reply::Done));
    assert_eq!(confirms(&mut core), Some(vec!["delete".to_string()]));

    core.tabs[0].shell.code = "async function run() { return 1; }".into();
    shell(&mut core, ShellA::Run { tab }).unwrap();
    assert!(confirms(&mut core).is_none(), "a reading script asked to confirm");
}

/// A value that does not read stops the run with a warning, before any confirm.
#[test]
fn a_snippet_value_that_does_not_read_stops_the_run() {
    let (mut core, tab) = offline();
    core.conns[0].writable = true;
    core.tabs[0].shell.code = "async function run() { return await orders.deleteMany({ n: {{n:int}} }); }".into();
    core.tabs[0].snip.vals.insert("n".into(), "x".into());
    let r = shell(&mut core, ShellA::Run { tab }).unwrap_err();
    assert_eq!(r.kind, RefusalKind::Invalid);
    assert!(confirms(&mut core).is_none());
}

/// Answers of a run reach its tab; those of a run replaced since are dropped.
#[test]
fn shell_answers_go_to_their_run_only() {
    let (mut core, _) = offline();
    let (tx, _rx) = tokio::sync::oneshot::channel();
    let sh = &mut core.tabs[0].shell;
    (sh.req, sh.cancel, sh.started) = (5, Some(tx), Some(std::time::Instant::now()));
    core.inject(Msg::ShellLog { req: 4, level: "log".into(), text: "old".into() });
    core.inject(Msg::ShellDone { req: 4, res: Ok(db::ShellOut::Value(Bson::Int32(1))), ms: 3, query_ms: None });
    let sh = &core.tabs[0].shell;
    assert!(sh.logs.is_empty() && sh.value.is_none() && sh.started.is_some(), "a stale answer landed");

    core.inject(Msg::ShellLog { req: 5, level: "log".into(), text: "hi".into() });
    core.inject(Msg::ShellQuery { req: 5, q: db::ShellQuery { conn: "test".into(), text: "x".repeat(3000), params: String::new(), rows: Some(1), affected: None, error: None, ms: 2 } });
    core.inject(Msg::ShellDone { req: 5, res: Ok(db::ShellOut::Value(Bson::Array(vec![doc! { "a": 1 }.into()]))), ms: 9, query_ms: Some(4) });
    let sh = &core.tabs[0].shell;
    assert_eq!(sh.logs, [("log".to_string(), "hi".to_string())]);
    assert_eq!(sh.queries[0].text.len(), 2000, "a long query is cut");
    assert_eq!((sh.ms, sh.query_ms, sh.count), (Some(9), Some(4), Some(1)));
    assert!(sh.started.is_none() && sh.cancel.is_none());

    core.tabs[0].shell.req = 6;
    core.inject(Msg::ShellDone { req: 6, res: Err("boom".into()), ms: 1, query_ms: None });
    let sh = &core.tabs[0].shell;
    assert_eq!(sh.error.as_deref(), Some("boom"));
    assert!(sh.value.is_none() && sh.count.is_none());
}

#[test]
fn the_console_keeps_the_latest_lines_each_cut_short() {
    let mut sh = ShellTab::default();
    for i in 0..1600 {
        sh.push_log("log".into(), format!("line {i}"));
    }
    // Two bytes a character, so a cut by bytes would land inside one or keep only 1000 characters.
    sh.push_log("log".into(), "é".repeat(3000));
    assert!(sh.logs.len() <= 1500, "{} lines kept", sh.logs.len());
    assert_eq!(sh.logs[sh.logs.len() - 2].1, "line 1599");
    assert!(sh.logs_dropped > 0 && sh.logs.len() + sh.logs_dropped >= 1600, "{} dropped", sh.logs_dropped);
    assert_eq!(sh.logs.last().unwrap().1.chars().count(), 2000);
    assert!(!sh.logs.iter().any(|l| l.1 == "line 0"), "the oldest lines were kept");
}

/// The toolbar and result choices, by the tab's uid; attachments are named, never counted.
#[test]
fn toolbar_and_result_choices() {
    let (mut core, tab) = offline();
    shell(&mut core, ShellA::Attach { tab, conn: CONN, db: "shop".into(), name: "a".into(), allow_writes: false, schema: "public".into() }).unwrap();
    shell(&mut core, ShellA::Attach { tab, conn: CONN, db: "app".into(), name: "b".into(), allow_writes: true, schema: String::new() }).unwrap();
    assert_eq!(core.tabs[0].shell.attached[0].schema, "", "a MongoDB attachment keeps no schema");
    shell(&mut core, ShellA::Detach { tab, name: "a".into() }).unwrap();
    assert_eq!(core.tabs[0].shell.attached.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["b"]);

    core.tabs[0].shell_checked.insert(3);
    core.tabs[0].selected = Some((true, 2));
    shell(&mut core, ShellA::Page { tab, page: 2 }).unwrap();
    let t = &core.tabs[0];
    assert_eq!(t.shell.page, 2);
    assert!(t.shell_checked.is_empty() && t.selected.is_none(), "checks and the open row of the old page stayed");

    for a in [ShellA::Help { tab, on: true }, ShellA::Raw { tab, on: true }, ShellA::Queries { tab, on: true }, ShellA::Filled { tab, on: true }, ShellA::Code { tab, code: "x".into() }] {
        shell(&mut core, a).unwrap();
    }
    let t = &core.tabs[0];
    assert!(t.shell.help && t.shell.raw && t.shell.queries_tab && t.snip.filled);
    assert_eq!(t.shell.code, "x");

    shell(&mut core, ShellA::Packages(vec!["zod".into()])).unwrap();
    assert_eq!(core.prefs.shell_packages, ["zod"]);

    // A tab that closed meanwhile is nothing to act on.
    assert_eq!(shell(&mut core, ShellA::Help { tab: tab + 1000, on: false }), Ok(Reply::Done));
}

#[test]
fn watch_starts_only_with_a_server_and_takes_its_own_events() {
    let (mut core, tab) = offline();
    core.tabs[0].view = View::Watch;
    let r = core.execute(Action::Watch(WatchA::Start { tab })).unwrap_err();
    assert_eq!(r.kind, RefusalKind::Offline);
    assert!(core.toasts.iter().any(|t| t.title == "Not connected"));
    core.demo = true;
    assert_eq!(core.execute(Action::Watch(WatchA::Start { tab })), Ok(Reply::Done));
    assert_eq!(core.status, "Offline demo: nothing to watch.");

    let (tx, _rx) = tokio::sync::oneshot::channel();
    (core.tabs[0].watch.req, core.tabs[0].watch.cancel) = (7, Some(tx));
    core.inject(Msg::WatchEvent { req: 6, doc: doc! { "operationType": "insert" } });
    assert!(core.tabs[0].watch.rows.is_empty(), "a stale event landed");
    core.inject(Msg::WatchEvent { req: 7, doc: doc! { "operationType": "insert", "documentKey": { "_id": 1 } } });
    assert_eq!(core.tabs[0].watch.rows.len(), 1);

    core.inject(Msg::WatchDone { req: 7, res: Err("Command failed: Error code 40573 (Location40573): The $changeStream stage is only supported on replica sets".into()) });
    assert!(core.tabs[0].watch.err.as_deref().is_some_and(|e| e.contains("standalone")));
    assert!(!core.tabs[0].watch.running());

    // Leaving the view stops the stream.
    let (tx, _rx) = tokio::sync::oneshot::channel();
    (core.tabs[0].watch.req, core.tabs[0].watch.cancel) = (8, Some(tx));
    core.stop_hidden_watches();
    assert!(core.tabs[0].watch.running(), "a stream on screen stopped");
    core.tabs[0].view = View::Documents;
    core.stop_hidden_watches();
    assert!(!core.tabs[0].watch.running());
    core.execute(Action::Watch(WatchA::Clear { tab })).unwrap();
    assert!(core.tabs[0].watch.rows.is_empty());
}

#[test]
fn tool_output_and_end() {
    let mut core = Core::for_test();
    let (tx, rx) = tokio::sync::oneshot::channel();
    core.tool = Some(ToolRun { title: "Dump shop".into(), log: Vec::new(), cancel: Some(tx), done: None });
    for i in 0..2001 {
        core.inject(Msg::ToolLog(format!("line {i}")));
    }
    assert_eq!(core.tool.as_ref().unwrap().log.len(), 1501, "the log is trimmed in batches");
    core.execute(Action::Tools(ToolsA::Stop)).unwrap();
    assert!(rx.blocking_recv().is_ok(), "Stop did not reach the tool");
    core.inject(Msg::ToolDone(Err("exit 1".into())));
    assert!(matches!(core.tool.as_ref().unwrap().done, Some(Err(_))));
    core.execute(Action::Tools(ToolsA::Close)).unwrap();
    assert!(core.tool.is_none());

    // A background update that fails stays quiet; a clicked install says so.
    core.toasts.clear();
    core.inject(Msg::ToolsInstalled(Err("offline".into())));
    assert!(core.toasts.is_empty());
    core.tools_installing = true;
    core.inject(Msg::ToolsInstalled(Err("offline".into())));
    assert!(!core.tools_installing);
    assert!(core.errors.iter().any(|e| e.raw.contains("Could not install the MongoDB Database Tools")));
}
