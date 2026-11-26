//! The MySQL and PostgreSQL Server windows run headlessly, with made-up answers.

use super::sql_server::kill_toast;
use super::*;
use crate::db::sql::pg::server::{Activity, Answer as PgAnswer, Session};
use crate::db::sql::server::{Answer, Proc, Processes, Variables};
use crate::db::sql::Engine;
use crate::state::sql::pg_server::PgTab;
use crate::state::sql::server::SqlServerTab;
use crate::state::{Conn, ConnMeta, Status};
use std::collections::HashSet;

const CONN: u64 = 3;

fn core_with(engine: Engine) -> Core {
    let mut core = Core::for_test();
    core.conns.push(Conn::new(ConnMeta { id: CONN, name: "shop-prod".into(), engine, ..Default::default() }, Status::Ready));
    core
}

fn sql(core: &mut Core, a: SqlServerA) -> Result<Reply, Refusal> {
    core.execute(Action::Server(ServerA::Sql(a)))
}

fn pg(core: &mut Core, a: PgServerA) -> Result<Reply, Refusal> {
    core.execute(Action::Server(ServerA::Pg(a)))
}

fn procs(identity: &str) -> Processes {
    let p = Proc::new;
    Processes { rows: vec![p(7, "app", "10.0.0.1:1", "shop", "Query", 9.0, "executing", "SELECT SLEEP(9)"), p(8, "app", "10.0.0.2:1", "shop", "Sleep", 3.0, "", "")], me: 1, uptime: Some(60), identity: identity.into() }
}

#[test]
fn a_kill_answer_is_told_without_a_window_and_only_once() {
    let mut kills: HashSet<u64> = [5].into();
    let ok = Answer::Kill { whole: false, res: Ok("x".into()) };
    let bad = Answer::Kill { whole: true, res: Err("nope".into()) };
    assert_eq!(kill_toast(&mut kills, 5, &ok), Some(Ok("Stopped the statement".to_string())));
    assert!(kills.is_empty());
    // The same request answered again, or one that was never sent, says nothing.
    assert_eq!(kill_toast(&mut kills, 5, &ok), None);
    kills.insert(6);
    assert_eq!(kill_toast(&mut kills, 6, &bad), Some(Err("nope".to_string())));
    assert_eq!(kill_toast(&mut kills, 6, &Answer::Procs(Err("x".into()))), None);
}

/// Open, list, select and arm a Kill; refused while locked; the answer of a sent Kill is told after the window closed.
#[test]
fn sql_server_lists_arms_a_kill_and_refuses_it_while_locked() {
    let mut core = core_with(Engine::MySql);
    sql(&mut core, SqlServerA::Open { conn: CONN }).unwrap();
    core.sql_server.as_mut().unwrap().procs_req = 5;
    // An answer to an older read is dropped; the one the window waits for fills the list.
    core.inject(Msg::SqlServer { req: 4, ans: Answer::Procs(Ok(procs("a"))) });
    assert!(core.sql_server.as_ref().unwrap().procs.is_none());
    core.inject(Msg::SqlServer { req: 5, ans: Answer::Procs(Ok(procs("a"))) });
    assert!(matches!(&core.sql_server.as_ref().unwrap().procs, Some(Ok(p)) if p.rows.len() == 2));
    assert_eq!(core.sql_server.as_ref().unwrap().uptime, Some(60));

    sql(&mut core, SqlServerA::Select(7)).unwrap();
    // The second click without the first does nothing.
    assert_eq!(sql(&mut core, SqlServerA::Kill { id: 7 }), Ok(Reply::Done));
    sql(&mut core, SqlServerA::ArmKill { id: 7, whole: false, at: 1.0 }).unwrap();
    let s = core.sql_server.as_ref().unwrap();
    assert_eq!((s.sel, s.confirm.as_ref().map(|c| (c.p.id, c.identity.as_str(), c.whole))), (Some(7), Some((7, "a", false))));
    sql(&mut core, SqlServerA::Disarm).unwrap();
    assert!(core.sql_server.as_ref().unwrap().confirm.is_none());

    // Locked: refused with the read-only toast, and nothing is sent.
    sql(&mut core, SqlServerA::ArmKill { id: 7, whole: true, at: 2.0 }).unwrap();
    let r = sql(&mut core, SqlServerA::Kill { id: 7 }).unwrap_err();
    assert_eq!(r.kind, RefusalKind::ReadOnly { conn: CONN });
    assert!(core.sql_kills.is_empty());

    // A list from another server (same ids, a restart) drops the armed Kill.
    sql(&mut core, SqlServerA::ArmKill { id: 7, whole: false, at: 3.0 }).unwrap();
    core.sql_server.as_mut().unwrap().procs_req = 6;
    core.inject(Msg::SqlServer { req: 6, ans: Answer::Procs(Ok(procs("b"))) });
    assert!(core.sql_server.as_ref().unwrap().confirm.is_none());

    // A Kill sent earlier is told even after the window closed.
    sql(&mut core, SqlServerA::Close).unwrap();
    core.sql_kills.insert(9);
    core.inject(Msg::SqlServer { req: 9, ans: Answer::Kill { whole: true, res: Ok(String::new()) } });
    assert_eq!(core.status, "Ended the connection");
    assert!(core.sql_kills.is_empty());
}

#[test]
fn sql_server_tabs_and_the_variables_scope() {
    let mut core = core_with(Engine::MySql);
    sql(&mut core, SqlServerA::Open { conn: CONN }).unwrap();
    let s = core.sql_server.as_mut().unwrap();
    s.next = Some(std::time::Instant::now());
    s.vars_req = 4;
    sql(&mut core, SqlServerA::Tab(SqlServerTab::Variables)).unwrap();
    let s = core.sql_server.as_ref().unwrap();
    assert_eq!((s.tab, s.next), (SqlServerTab::Variables, None));
    core.inject(Msg::SqlServer { req: 4, ans: Answer::Vars(Ok(Variables { rows: Vec::new(), global: false, defaults: true, uptime: None })) });
    assert!(core.sql_server.as_ref().unwrap().vars.is_some());
    // The other scope has other values: they are read again, and an answer for the old scope is dropped.
    sql(&mut core, SqlServerA::Global(true)).unwrap();
    let s = core.sql_server.as_ref().unwrap();
    assert!(s.global && s.vars.is_none() && s.vars_req == 0 && !s.vars_wait);
    core.inject(Msg::SqlServer { req: 4, ans: Answer::Vars(Ok(Variables { rows: Vec::new(), global: false, defaults: true, uptime: None })) });
    assert!(core.sql_server.as_ref().unwrap().vars.is_none());
    sql(&mut core, SqlServerA::Unlock).unwrap();
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::Unlock(CONN))]));
}

fn session(pid: u64, state: &str) -> Session {
    let mut s = Session::new(pid, "app", "10.0.0.1", "shop", "api", state, "SELECT pg_sleep(9)", "client backend");
    (s.backend_start, s.signal) = ("2026-11-12 10:00:00+00".into(), true);
    s
}

fn activity(rows: Vec<Session>) -> PgAnswer {
    PgAnswer::Activity(Ok(Activity { rows, user: "app".into(), read_all: true, server: String::new(), own_app: None, blockers: true }))
}

#[test]
fn pg_server_arms_a_terminate_refuses_it_while_locked_and_tells_a_signal_once() {
    let mut core = core_with(Engine::Postgres);
    pg(&mut core, PgServerA::Open { conn: CONN }).unwrap();
    pg(&mut core, PgServerA::Tab(PgTab::Activity)).unwrap();
    core.pg_server.as_mut().unwrap().act_req = 5;
    core.inject(Msg::PgServer { req: 4, ans: activity(vec![session(41, "active")]) });
    assert!(core.pg_server.as_ref().unwrap().act.is_none(), "a stale answer is dropped");
    core.inject(Msg::PgServer { req: 5, ans: activity(vec![session(41, "active")]) });
    assert_eq!(core.pg_server.as_ref().unwrap().rows().len(), 1);

    pg(&mut core, PgServerA::ArmSignal { pid: 41, terminate: true, at: 1.0 }).unwrap();
    assert!(core.pg_server.as_ref().unwrap().confirm.as_ref().is_some_and(|c| c.terminate && c.s.pid == 41));
    let r = pg(&mut core, PgServerA::Signal { pid: 41 }).unwrap_err();
    assert_eq!(r.kind, RefusalKind::ReadOnly { conn: CONN });
    assert!(core.sql_kills.is_empty());

    // A tab change drops a pending signal; Locks asks for blockers at once.
    pg(&mut core, PgServerA::ArmSignal { pid: 41, terminate: false, at: 2.0 }).unwrap();
    core.pg_server.as_mut().unwrap().blk_at = Some(std::time::Instant::now());
    pg(&mut core, PgServerA::Tab(PgTab::Locks)).unwrap();
    let s = core.pg_server.as_ref().unwrap();
    assert!(s.confirm.is_none() && s.blk_at.is_none());

    core.sql_kills.insert(9);
    core.inject(Msg::PgServer { req: 9, ans: PgAnswer::Signal(Ok("Terminated session 41".into())) });
    assert_eq!(core.status, "Terminated session 41");
    assert!(core.pg_server.as_ref().unwrap().refresh);
    core.status.clear();
    core.inject(Msg::PgServer { req: 9, ans: PgAnswer::Signal(Ok("again".into())) });
    assert!(core.status.is_empty(), "a signal answer is told only once");
}

/// A focused, open window on a connection without a session asks nothing; demo mode never asks either.
#[test]
fn the_polls_ask_nothing_without_a_session() {
    let mut core = core_with(Engine::Postgres);
    pg(&mut core, PgServerA::Open { conn: CONN }).unwrap();
    assert!(!core.pg_server_poll());
    assert_eq!(core.pg_server.as_ref().unwrap().ov_req, 0);
    core.conns[0].meta.engine = Engine::MySql;
    sql(&mut core, SqlServerA::Open { conn: CONN }).unwrap();
    assert!(!core.sql_server_poll());
    core.server_windows_frame();
    assert!(!core.sql_server.as_ref().unwrap().writable && !core.pg_server.as_ref().unwrap().writable);
}
