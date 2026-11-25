//! The users windows and the MongoDB Server window run headlessly: with made-up answers, and against MySQL and MariaDB when
//! their variables are set.

use super::users::{parse_roles, roles_text, self_users, short_err};
use super::*;
use crate::db::sql::users::{Account, Loaded, Op, UserList};
use crate::db::sql::Engine;
use crate::db::{self, UndoAction, UndoKind};
use crate::state::sql::users::SqlUsersWin;
use crate::state::{Conn, ConnMeta, Status};
use mongodb::bson::{doc, Bson};

const CONN: u64 = 3;

fn sql(core: &mut Core, a: SqlUsersA) -> Result<Reply, Refusal> {
    core.execute(Action::Users(UsersA::Sql(a)))
}

/// A MySQL connection without a session, and the window waiting for its account list as request 5.
fn offline() -> Core {
    let mut core = Core::for_test();
    core.conns.push(Conn::new(ConnMeta { id: CONN, name: "shop-prod".into(), engine: Engine::MySql, ..Default::default() }, Status::Ready));
    let mut w = SqlUsersWin::new(CONN);
    w.list_req = 5;
    core.sql_users = Some(w);
    core
}

fn list() -> UserList {
    let acct = |u: &str, h: &str| Account { user: u.into(), host: h.into(), ..Default::default() };
    UserList { accounts: vec![acct("app", "%"), acct("report", "10.0.%")], current: Some(("report".into(), "10.0.%".into())), whole: true, why: None }
}

#[test]
fn sql_users_pick_the_current_account_stage_a_privilege_and_apply_once_unlocked() {
    let mut core = offline();
    // An answer to an earlier read is dropped; the one the window waits for picks the signed-in account.
    core.inject(Msg::SqlUsers { conn: CONN, req: 4, res: Ok(list()) });
    assert!(core.sql_users.as_ref().unwrap().list.is_none());
    core.inject(Msg::SqlUsers { conn: CONN, req: 5, res: Ok(list()) });
    let w = core.sql_users.as_mut().unwrap();
    assert_eq!(w.sel, Some(("report".into(), "10.0.%".into())));
    w.grants_req = 6;
    core.inject(Msg::SqlUserGrants { conn: CONN, req: 6, res: Ok(Loaded { grants: Vec::new(), info: None, info_err: None, fold_case: false }) });
    assert!(matches!(core.sql_users.as_ref().unwrap().grants, Some(Ok(_))));

    sql(&mut core, SqlUsersA::Tab(crate::state::sql::users::UTab::Global)).unwrap();
    sql(&mut core, SqlUsersA::Global { privilege: "PROCESS".into(), on: true }).unwrap();
    assert_eq!(core.sql_users.as_ref().unwrap().global_edits().len(), 1);

    // Locked: refused with the read-only toast; the change stays staged.
    let r = sql(&mut core, SqlUsersA::Apply).unwrap_err();
    assert_eq!(r.kind, RefusalKind::ReadOnly { conn: CONN });
    assert!(core.toasts.iter().any(|t| t.title == "shop-prod is read-only"));
    // Unlocked but without a session: asks to connect.
    core.conns[0].writable = true;
    assert_eq!(sql(&mut core, SqlUsersA::Apply).unwrap_err().kind, RefusalKind::Offline);
    assert!(!core.sql_users.as_ref().unwrap().busy);

    // The answer of an Apply: told, put on Recent changes, and the window reads the server again.
    core.sql_users.as_mut().unwrap().busy = true;
    let undo = UndoAction { agent: None, at: std::time::Instant::now(), conn: CONN, db: String::new(), coll: String::new(), kind: UndoKind::Insert { id: Bson::Int32(1) } };
    core.inject(Msg::SqlUserDone { conn: CONN, account: Account::named("report", "10.0.%"), op: Op::Granted, res: Ok("Granted PROCESS".into()), undo: vec![undo] });
    let w = core.sql_users.as_ref().unwrap();
    assert!(!w.busy && w.grants.is_none());
    assert_eq!((core.undo.len(), core.status.as_str()), (1, "Granted PROCESS"));
}

#[test]
fn sql_users_new_user_keeps_its_read_only_wording() {
    let mut core = offline();
    let r = sql(&mut core, SqlUsersA::NewUser).unwrap_err();
    assert_eq!((r.kind, r.msg.as_str()), (RefusalKind::ReadOnly { conn: CONN }, super::users::NOTE_USER));
    core.conns[0].writable = true;
    sql(&mut core, SqlUsersA::NewUser).unwrap();
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::SqlNewUser(f))] if f.conn == CONN && f.host == "%"));
}

#[test]
fn sql_users_edits_are_actions() {
    let mut core = offline();
    core.inject(Msg::SqlUsers { conn: CONN, req: 5, res: Ok(list()) });
    sql(&mut core, SqlUsersA::DbAdd(Some("shop".into()))).unwrap();
    sql(&mut core, SqlUsersA::DbAddDone).unwrap();
    sql(&mut core, SqlUsersA::DbPriv { pattern: "shop".into(), privilege: "SELECT".into(), on: true }).unwrap();
    let w = core.sql_users.as_ref().unwrap();
    assert!(w.adding.is_none() && w.edits().len() == 1);
    // A MySQL role typed without a host is the role at %.
    sql(&mut core, SqlUsersA::RoleAdd(Some("dash".into()))).unwrap();
    sql(&mut core, SqlUsersA::RoleAddDone).unwrap();
    sql(&mut core, SqlUsersA::Lock(true)).unwrap();
    let w = core.sql_users.as_ref().unwrap();
    assert_eq!(w.roles_now, ["dash@%"]);
    assert!(w.locked && w.role_add.is_none());
    sql(&mut core, SqlUsersA::Close).unwrap();
    assert!(core.sql_users.is_none());
}

#[test]
fn mongo_users_open_reads_and_a_refused_listing_falls_back_to_the_logged_in_user() {
    let mut core = Core::for_test();
    core.execute(Action::Users(UsersA::Mongo(MongoUsersA::Open { conn: 1 }))).unwrap();
    assert_eq!(core.users.as_ref().unwrap().new_db, "admin");
    core.inject(Msg::Command { tag: "users-self".into(), res: Ok(doc! { "authInfo": { "authenticatedUsers": [{ "user": "ada", "db": "admin" }], "authenticatedUserRoles": [{ "role": "read", "db": "shop" }] } }) });
    core.inject(Msg::Command { tag: "roles".into(), res: Ok(doc! { "roles": [] }) });
    let u = core.users.as_ref().unwrap();
    assert_eq!(u.users.as_ref().unwrap().as_ref().unwrap().len(), 1);
    assert!(u.roles.as_ref().unwrap().as_ref().unwrap().is_empty());
    // Saving an edit is a write: refused while locked.
    core.execute(Action::Users(UsersA::Mongo(MongoUsersA::Edit { user: "ada".into(), db: "admin".into() }))).unwrap();
    assert_eq!(core.users.as_ref().unwrap().edit.as_ref().map(|e| e.3.as_str()), Some("read@shop"));
    core.conns.push(Conn::new(ConnMeta { id: 1, name: "local".into(), ..Default::default() }, Status::Ready));
    let r = core.execute(Action::Users(UsersA::Mongo(MongoUsersA::Save))).unwrap_err();
    assert_eq!(r.kind, RefusalKind::ReadOnly { conn: 1 });
}

#[test]
fn refused_listing_falls_back_to_the_logged_in_user() {
    let e = "Command failed: Error code 13 (Unauthorized): not authorized on admin to execute command { usersInfo: { forAllDBs: true }, $db: \"admin\" }";
    assert!(db::is_unauthorized(e));
    assert_eq!(short_err(e), "Command failed: Error code 13 (Unauthorized): not authorized on admin");
    let status = doc! { "authInfo": { "authenticatedUsers": [{ "user": "khiman", "db": "admin" }], "authenticatedUserRoles": [{ "role": "atlasAdmin", "db": "admin" }] } };
    let me = self_users(&status);
    assert_eq!(me.len(), 1);
    assert_eq!(roles_text(&me[0]), "atlasAdmin@admin");
}

#[test]
fn roles() {
    let r = parse_roles("readWrite@shop, read ,", "admin");
    assert_eq!(r.len(), 2);
    assert_eq!(r[1].as_document().unwrap().get_str("db").unwrap(), "admin");
    let u = doc! { "roles": [ { "role": "read", "db": "x" } ] };
    assert_eq!(roles_text(&u), "read@x");
}

#[test]
fn mongo_server_kill_needs_its_second_click() {
    let mut core = Core::for_test();
    core.execute(Action::Server(ServerA::Mongo(MongoServerA::Open { conn: 1 }))).unwrap();
    let id = core.server.as_ref().unwrap().id;
    core.inject(Msg::Command { tag: format!("server:{id}:ops"), res: Ok(doc! { "inprog": [{ "opid": 7, "microsecs_running": 5_i64 }] }) });
    assert_eq!(core.server.as_ref().unwrap().ops.as_ref().unwrap().as_ref().unwrap().len(), 1);
    // An answer for another window's id is dropped.
    core.inject(Msg::Command { tag: format!("server:{}:ops", id + 1), res: Ok(doc! { "inprog": [] }) });
    assert_eq!(core.server.as_ref().unwrap().ops.as_ref().unwrap().as_ref().unwrap().len(), 1);
    let server = |a| Action::Server(ServerA::Mongo(a));
    core.execute(server(MongoServerA::Kill { op: "7".into() })).unwrap();
    assert!(core.server.as_ref().unwrap().confirm_kill.is_none(), "Kill it without Kill… does nothing");
    core.execute(server(MongoServerA::ArmKill { op: "7".into(), at: 2.0 })).unwrap();
    core.execute(server(MongoServerA::Disarm)).unwrap();
    assert!(core.server.as_ref().unwrap().confirm_kill.is_none());
    core.inject(Msg::Command { tag: format!("server:{id}:kill"), res: Ok(doc! {}) });
    assert_eq!(core.status, "Asked the server to kill the operation");
    assert!(core.server.as_ref().unwrap().refresh);
}

/// Grant SELECT on a database to a new account through the window, refused while locked, applied once unlocked, then undone.
#[test]
fn live_sql_users_grant_apply_and_undo() {
    use crate::db::sql::testing::{live, ENVS};
    use crate::db::sql::users::pattern_of;
    for var in ENVS {
        let Some(l) = live(var) else { continue };
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let user = format!("qu_core_{:x}", n & 0xffff_ffff_ffff);
        l.sql(&format!("CREATE USER '{user}'@'%' IDENTIFIED BY 'pw-for-tests'"));
        let held = || l.one(&format!("SELECT COUNT(*) FROM mysql.db WHERE User = '{user}' AND Db = 'shop\\\\_x' AND Select_priv = 'Y'"));

        let mut core = Core::for_test();
        core.worker.sql_connect(CONN, std::env::var(var).unwrap(), std::time::Duration::from_secs(30), crate::db::Net::default());
        let Ok(Msg::SqlConnected { res: Ok((db, _, _)), .. }) = core.rx.recv_timeout(std::time::Duration::from_secs(20)) else { panic!("{var}: connect") };
        core.conns.push(Conn::new(ConnMeta { id: CONN, name: "test".into(), engine: Engine::MySql, ..Default::default() }, Status::Ready));
        core.conns[0].sql = Some(db);
        let loaded = |c: &Core| c.sql_users.as_ref().is_some_and(|w| matches!(w.list, Some(Ok(_))) && matches!(w.grants, Some(Ok(_))) && !w.busy);

        assert!(matches!(sql(&mut core, SqlUsersA::Open { conn: CONN }), Ok(Reply::Pending(_))));
        core.step_until("the accounts", loaded);
        sql(&mut core, SqlUsersA::Pick { user: user.clone(), host: "%".into() }).unwrap();
        core.step_until("the new account's grants", |c| loaded(c) && c.sql_users.as_ref().unwrap().sel == Some((user.clone(), "%".into())));
        sql(&mut core, SqlUsersA::DbAdd(Some("shop_x".into()))).unwrap();
        sql(&mut core, SqlUsersA::DbAddDone).unwrap();
        sql(&mut core, SqlUsersA::DbPriv { pattern: pattern_of("shop_x"), privilege: "SELECT".into(), on: true }).unwrap();
        assert_eq!(core.sql_users.as_ref().unwrap().edits().len(), 1, "{var}");

        assert_eq!(sql(&mut core, SqlUsersA::Apply).unwrap_err().kind, RefusalKind::ReadOnly { conn: CONN });
        assert_eq!(held(), "0", "{var}: nothing runs before Apply");
        core.conns[0].writable = true;
        sql(&mut core, SqlUsersA::Apply).unwrap();
        assert!(core.sql_users.as_ref().unwrap().busy);
        core.step_until("the apply and the read after it", |c| loaded(c) && c.sql_users.as_ref().unwrap().edits().is_empty() && !c.undo.is_empty());
        assert_eq!(held(), "1", "{var}: {}", core.status);

        let a = core.undo.remove(0);
        let UndoKind::Sql { database, undo, .. } = &a.kind else { panic!("{var}: not a SQL undo") };
        let (sql_db, pk) = (core.conns[0].sql.as_ref().unwrap().at(database), undo.pk_names());
        core.worker.sql_undo(sql_db, a, pk, false);
        let t = std::time::Instant::now();
        while held() != "0" {
            assert!(t.elapsed() < std::time::Duration::from_secs(40), "{var}: the undo never landed; {}", core.status);
            core.tick();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        l.sql(&format!("DROP USER '{user}'@'%'"));
    }
}
