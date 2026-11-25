//! The Roles and privileges window run headlessly: offline with made-up answers, and against the PostgreSQL servers of the
//! live tests when their variables are set.

use super::roles::demo_data;
use super::*;
use crate::db::sql::pg::roles::{Answer, Change, Did, Kind, Privs};
use crate::db::sql::Engine;
use crate::db::{UndoAction, UndoKind};
use crate::state::sql::pg_roles::PgRolesWin;
use crate::state::{Conn, ConnMeta, Status};
use mongodb::bson::Bson;

const CONN: u64 = 1;

fn pg_conn(core: &mut Core) {
    core.conns.push(Conn::new(ConnMeta { id: CONN, name: "test".into(), engine: Engine::Postgres, ..Default::default() }, Status::Ready));
}

/// A core with the window open on the demo server's roles and tables, writes locked.
fn offline() -> Core {
    let mut core = Core::for_test();
    pg_conn(&mut core);
    let mut w = PgRolesWin::new(CONN, "analytics".into());
    let (cl, objs, defs) = demo_data();
    w.set_cluster(Ok(cl));
    w.objs = Some(Ok(objs));
    w.defs = Some(Ok(defs));
    w.role = Some("app_writer".into());
    core.pg_roles = Some(w);
    core
}

/// SELECT granted to `role` on the table `table`, from what the window read.
fn grant(core: &Core, table: &str, role: &str) -> Change {
    let w = core.pg_roles.as_ref().unwrap();
    let o = w.objects().unwrap().objs.iter().find(|o| o.name == table).unwrap().clone();
    let mut to: Privs = o.direct(role);
    to.insert("SELECT".into(), false);
    Change::Acl { target: o.target(Kind::Tables), grantee: role.into(), from: o.direct(role), to }
}

fn roles(core: &mut Core, a: RolesA) -> Result<Reply, Refusal> {
    core.execute(Action::Roles(a))
}

fn staged(core: &Core) -> usize {
    core.pg_roles.as_ref().map_or(0, |w| w.staged.len())
}

fn writable(core: &mut Core, on: bool) {
    core.conns[0].writable = on;
}

#[test]
fn a_locked_connection_stages_nothing_and_refuses_apply() {
    let mut core = offline();
    let c = grant(&core, "payments", "app_reader");
    roles(&mut core, RolesA::Stage(c.clone())).unwrap();
    assert_eq!(staged(&core), 0, "a locked connection staged a grant");

    writable(&mut core, true);
    roles(&mut core, RolesA::Stage(c)).unwrap();
    assert_eq!(staged(&core), 1);

    // Locked again before Apply: refused with the read-only toast, which names the connection, and nothing sent.
    writable(&mut core, false);
    let r = roles(&mut core, RolesA::Apply).unwrap_err();
    assert_eq!(r.kind, RefusalKind::ReadOnly { conn: CONN });
    assert_eq!(r.msg, "test is read-only. Unlock writes to make this change.");
    assert!(core.toasts.iter().any(|t| t.title == "test is read-only"));
    assert!(!core.pg_roles.as_ref().unwrap().busy);

    // Unlocked: this connection has no session here, so it asks to connect.
    writable(&mut core, true);
    let r = roles(&mut core, RolesA::Apply).unwrap_err();
    assert_eq!(r.kind, RefusalKind::Offline);
    assert_eq!(staged(&core), 1, "a refused apply keeps what was staged");

    // The offline demo says it has no database and keeps the change.
    core.demo = true;
    assert_eq!(roles(&mut core, RolesA::Apply), Ok(Reply::Done));
    assert_eq!(core.status, "Offline demo: no database is connected.");

    roles(&mut core, RolesA::Discard).unwrap();
    assert_eq!(staged(&core), 0);
}

/// An Apply answer that is not the one the window waits for (from before Esc closed and reopened it) leaves what is staged now.
#[test]
fn a_late_apply_answer_keeps_newer_staged_changes() {
    let mut core = Core::for_test();
    let mut w = PgRolesWin::new(CONN, "shop".into());
    w.staged.push(Change::Member { group: "g".into(), member: "m".into(), from: None, to: Some(Default::default()) });
    (w.busy, w.req_apply) = (true, 7);
    core.pg_roles = Some(w);
    let done = || Msg::PgRoles { conn: CONN, req: 3, ans: Answer::Done { did: Did::Applied, res: Ok("Applied".into()), undo: None } };
    core.inject(done());
    assert_eq!(staged(&core), 1, "a stale answer cleared the staged change");
    core.inject(Msg::PgRoles { conn: CONN, req: 7, ans: Answer::Done { did: Did::Applied, res: Ok("Applied".into()), undo: None } });
    assert!(staged(&core) == 0 && !core.pg_roles.as_ref().unwrap().busy);
}

#[test]
fn a_stale_read_is_ignored_and_an_apply_answer_lands_on_the_undo_list() {
    let mut core = offline();
    let w = core.pg_roles.as_mut().unwrap();
    (w.req_cluster, w.req_apply, w.busy) = (10, 12, true);
    w.staged.push(Change::Member { group: "g".into(), member: "m".into(), from: None, to: Some(Default::default()) });
    let before = w.cl().unwrap().roles.len();
    // A cluster read from an earlier Refresh answers late: the window keeps what it has.
    core.inject(Msg::PgRoles { conn: CONN, req: 9, ans: Answer::Cluster(Ok(Default::default())) });
    assert_eq!(core.pg_roles.as_ref().unwrap().cl().unwrap().roles.len(), before);
    // Another connection's answer with the same request id is not this window's either.
    core.inject(Msg::PgRoles { conn: 2, req: 10, ans: Answer::Cluster(Ok(Default::default())) });
    assert_eq!(core.pg_roles.as_ref().unwrap().cl().unwrap().roles.len(), before);

    let undo = UndoAction { agent: None, at: std::time::Instant::now(), conn: CONN, db: String::new(), coll: String::new(), kind: UndoKind::Insert { id: Bson::Int32(1) } };
    core.inject(Msg::PgRoles { conn: CONN, req: 12, ans: Answer::Done { did: Did::Applied, res: Ok("Applied 1 change".into()), undo: Some(undo) } });
    let w = core.pg_roles.as_ref().unwrap();
    assert!(w.staged.is_empty() && !w.busy);
    assert_eq!((core.undo.len(), core.status.as_str()), (1, "Applied 1 change"));
    // The window reads the server again after a write; this core has no session, so it keeps what it showed.
    assert!(w.cluster.is_some());
}

#[test]
fn the_window_s_choices_are_actions() {
    let mut core = offline();
    writable(&mut core, true);
    roles(&mut core, RolesA::Cell(Some(("16501".into(), "app_writer".into())))).unwrap();
    roles(&mut core, RolesA::Inherited(false)).unwrap();
    roles(&mut core, RolesA::System(true)).unwrap();
    let w = core.pg_roles.as_ref().unwrap();
    assert!(w.cell.is_some() && !w.inherited && w.system);
    // A tab change closes the cell editor.
    roles(&mut core, RolesA::Tab(crate::state::sql::pg_roles::PTab::Roles)).unwrap();
    assert!(core.pg_roles.as_ref().unwrap().cell.is_none());

    // The Role tab's limit field: its text is filled for the role on show, an edit stages, a bad one explains.
    roles(&mut core, RolesA::OpenRole("app_writer".into())).unwrap();
    core.pg_roles_fields();
    assert_eq!(core.pg_roles.as_ref().unwrap().lim, "20");
    core.pg_roles.as_mut().unwrap().lim = "35".into();
    roles(&mut core, RolesA::LimitEdited).unwrap();
    assert!(matches!(core.pg_roles.as_ref().unwrap().staged.as_slice(), [Change::Attrs { to, .. }] if to.conn_limit == 35));
    core.pg_roles.as_mut().unwrap().lim = "many".into();
    roles(&mut core, RolesA::LimitEdited).unwrap();
    assert!(core.pg_roles.as_ref().unwrap().field_err.is_some());

    // New role and Drop open the app's dialogs; Unlock opens Unlock with nothing to run after.
    roles(&mut core, RolesA::NewRole).unwrap();
    roles(&mut core, RolesA::Unlock).unwrap();
    let fx = core.take_ui_fx();
    assert!(matches!(fx.as_slice(), [UiFx::Open(Dialog::PgNewRole(_)), UiFx::Open(Dialog::Unlock(CONN))]));
    writable(&mut core, false);
    let r = roles(&mut core, RolesA::Drop("jane".into())).unwrap_err();
    assert_eq!(r.kind, RefusalKind::ReadOnly { conn: CONN });
    assert!(core.take_ui_fx().is_empty(), "a refused drop opens nothing");

    roles(&mut core, RolesA::Close).unwrap();
    assert!(core.pg_roles.is_none());
}

#[test]
fn a_password_from_json_is_refused() {
    let mut core = offline();
    writable(&mut core, true);
    let json = serde_json::to_string(&Action::Roles(RolesA::SetPassword { conn: CONN, role: "jane".into(), oid: "16405".into(), password: Secret::new("Zq9secret".into()) })).unwrap();
    let r = core.execute(serde_json::from_str(&json).unwrap()).unwrap_err();
    assert_eq!(r.kind, RefusalKind::Invalid);
}

/// Every variant of every area, so a new one must be added here: the match below has no wildcard.
fn all_actions() -> Vec<Action> {
    use crate::state::sql::pg_roles::{DefForm, PTab, RTab};
    use crate::state::sql::users::{ExpKind, UTab};
    let c = Change::Member { group: "g".into(), member: "m".into(), from: None, to: Some(Default::default()) };
    let spec = crate::db::sql::pg::roles::NewRole { name: "r".into(), attrs: Default::default(), groups: vec!["g".into()] };
    let form = DefForm { owner: "o".into(), schema: String::new(), kind: crate::db::sql::pg::roles::DefKind::Tables, privs: ["SELECT".to_string()].into() };
    let pw = || Secret::new("pw".into());
    let r = [
        RolesA::Open { conn: 1 },
        RolesA::Tab(PTab::Roles),
        RolesA::RTab(RTab::Defaults),
        RolesA::Database("d".into()),
        RolesA::Schema("s".into()),
        RolesA::Kind(Kind::Functions),
        RolesA::Role("r".into()),
        RolesA::OpenRole("r".into()),
        RolesA::Stage(c),
        RolesA::Apply,
        RolesA::Discard,
        RolesA::Close,
        RolesA::Unlock,
        RolesA::Refresh,
        RolesA::NewRole,
        RolesA::Drop("r".into()),
        RolesA::Password("r".into()),
        RolesA::Inherited(false),
        RolesA::System(true),
        RolesA::Cell(Some(("1".into(), String::new()))),
        RolesA::LimitEdited,
        RolesA::UntilEdited,
        RolesA::DefForm(Some(form)),
        RolesA::Create { conn: 1, spec, password: pw() },
        RolesA::SetPassword { conn: 1, role: "r".into(), oid: "1".into(), password: pw() },
        RolesA::DropRun { conn: 1, info: Default::default(), mode: crate::db::sql::pg::roles::DropMode::Reassign("h".into()) },
    ];
    let u = [
        SqlUsersA::Open { conn: 1 },
        SqlUsersA::Pick { user: "u".into(), host: "%".into() },
        SqlUsersA::Tab(UTab::Global),
        SqlUsersA::Apply,
        SqlUsersA::Close,
        SqlUsersA::Refresh,
        SqlUsersA::Unlock,
        SqlUsersA::NewUser,
        SqlUsersA::Drop { user: "u".into(), host: "%".into() },
        SqlUsersA::DropRun { conn: 1, user: "u".into(), host: "%".into(), is_role: false },
        SqlUsersA::Password { user: "u".into(), host: "%".into() },
        SqlUsersA::Create { conn: 1, user: "u".into(), host: "%".into(), password: pw() },
        SqlUsersA::SetPassword { conn: 1, user: "u".into(), host: "%".into(), plugin: None, password: pw() },
        SqlUsersA::Lock(true),
        SqlUsersA::Expire(ExpKind::Days),
        SqlUsersA::Tls(crate::db::sql::users::Tls::X509),
        SqlUsersA::Role { name: "r@%".into(), on: true },
        SqlUsersA::RoleAdd(Some("r".into())),
        SqlUsersA::RoleAddDone,
        SqlUsersA::Global { privilege: "PROCESS".into(), on: true },
        SqlUsersA::PrivAdd(None),
        SqlUsersA::PrivAddDone,
        SqlUsersA::DbPriv { pattern: "shop".into(), privilege: "SELECT".into(), on: true },
        SqlUsersA::DbAdd(Some("shop".into())),
        SqlUsersA::DbAddDone,
    ];
    let m = [
        MongoUsersA::Open { conn: 1 },
        MongoUsersA::Refresh,
        MongoUsersA::Unlock,
        MongoUsersA::Edit { user: "u".into(), db: "admin".into() },
        MongoUsersA::CloseEdit,
        MongoUsersA::Save,
        MongoUsersA::Add,
        MongoUsersA::Drop { user: "u".into(), db: "admin".into() },
        MongoUsersA::DropRun { conn: 1, user: "u".into(), db: "admin".into() },
        MongoUsersA::Close,
    ];
    let s = [
        MongoServerA::Open { conn: 1 },
        MongoServerA::Ops(true),
        MongoServerA::Refresh,
        MongoServerA::AutoOps(true),
        MongoServerA::ArmKill { op: "7".into(), at: 1.5 },
        MongoServerA::Disarm,
        MongoServerA::Kill { op: "7".into() },
        MongoServerA::Unlock,
        MongoServerA::Close,
    ];
    let mut all: Vec<Action> = r.into_iter().map(Action::Roles).collect();
    all.extend(u.into_iter().map(|a| Action::Users(UsersA::Sql(a))));
    all.extend(m.into_iter().map(|a| Action::Users(UsersA::Mongo(a))));
    all.extend(s.into_iter().map(|a| Action::Server(ServerA::Mongo(a))));
    let q = [
        SqlServerA::Open { conn: 1 },
        SqlServerA::Tab(crate::state::sql::server::SqlServerTab::Locks),
        SqlServerA::Refresh,
        SqlServerA::Auto(false),
        SqlServerA::HideSleeping(false),
        SqlServerA::Select(7),
        SqlServerA::ArmKill { id: 7, whole: true, at: 1.5 },
        SqlServerA::Disarm,
        SqlServerA::Kill { id: 7 },
        SqlServerA::Global(true),
        SqlServerA::StGroup(crate::db::sql::server::StatusGroup::InnoDb),
        SqlServerA::StAuto(false),
        SqlServerA::Unlock,
        SqlServerA::Close,
    ];
    all.extend(q.into_iter().map(|a| Action::Server(ServerA::Sql(a))));
    let p = [
        PgServerA::Open { conn: 1 },
        PgServerA::Tab(crate::state::sql::pg_server::PgTab::Counters),
        PgServerA::Refresh,
        PgServerA::Auto(false),
        PgServerA::HideIdle(false),
        PgServerA::ChangedOnly(false),
        PgServerA::Select(41),
        PgServerA::Setting(Some("work_mem".into())),
        PgServerA::ArmSignal { pid: 41, terminate: true, at: 1.5 },
        PgServerA::Disarm,
        PgServerA::Signal { pid: 41 },
        PgServerA::FetchQuery { pid: 41 },
        PgServerA::Unlock,
        PgServerA::Close,
    ];
    all.extend(p.into_iter().map(|a| Action::Server(ServerA::Pg(a))));
    let sh = [
        ShellA::Run { tab: 1 },
        ShellA::Start { tab: 1 },
        ShellA::Stop { tab: 1 },
        ShellA::Code { tab: 1, code: "x".into() },
        ShellA::Detach { tab: 1, name: "a".into() },
        ShellA::Attach { tab: 1, conn: 2, db: "d".into(), name: "a".into(), allow_writes: false, schema: String::new() },
        ShellA::Help { tab: 1, on: true },
        ShellA::Queries { tab: 1, on: true },
        ShellA::Raw { tab: 1, on: true },
        ShellA::Page { tab: 1, page: 2 },
        ShellA::Filled { tab: 1, on: true },
        ShellA::Packages(vec!["zod".into()]),
    ];
    all.extend(sh.into_iter().map(Action::Shell));
    all.extend([WatchA::Start { tab: 1 }, WatchA::Stop { tab: 1 }, WatchA::Clear { tab: 1 }].into_iter().map(Action::Watch));
    let tools = [ToolsA::Install, ToolsA::Dump { conn: 1, db: "d".into(), coll: None, path: "/tmp/a".into(), gzip: true }, ToolsA::Stop, ToolsA::Close];
    all.extend(tools.into_iter().map(Action::Tools));
    all.extend(super::conn_tests::conn_actions());
    use crate::library::ops::DropAt;
    use crate::library::tree::Node;
    use crate::state::settings::SettingsCat;
    all.extend([UpdateA::CheckNow, UpdateA::Press(UpdBtn::Restart), UpdateA::ShowNotes, UpdateA::OfferedNotes].map(Action::Update));
    all.extend([AppA::Dismiss(1), AppA::Details(1), AppA::ErrorsSeen, AppA::ClearErrors, AppA::RaiseLimit].map(Action::App));
    all.extend([PrefsA::Open(SettingsCat::Tools), PrefsA::Cat(SettingsCat::Reading), PrefsA::Zoom(1.25)].map(Action::Prefs));
    all.extend(Pref::samples().into_iter().map(|p| Action::Prefs(PrefsA::Set(p))));
    let rel = || std::path::PathBuf::from("reports/a.sql");
    let l = [
        LibA::CancelPending,
        LibA::ConflictReload { tab: 1 },
        LibA::ConflictKeep { tab: 1 },
        LibA::ConflictCompare { tab: 1 },
        LibA::Commit { node: Node::File { link: 1, rel: rel() }, text: "b".into() },
        LibA::Duplicate(1),
        LibA::Move { node: Node::Script(1), at: DropAt::Before(Node::Folder(2)) },
        LibA::MoveToNew(Node::Script(1)),
        LibA::Delete(Node::Folder(1)),
        LibA::NewFolder(Some(1)),
        LibA::Scope(true),
        LibA::NewScript { link: 1, dir: "reports".into() },
        LibA::NewDir { link: 1, dir: "".into() },
        LibA::Reload(1),
        LibA::Unlink(1),
        LibA::ScriptToLink { id: 1, link: 1, dir: "".into() },
        LibA::MoveFile { link: 1, rel: rel(), dir: "old".into() },
        LibA::FileToQuarry { link: 1, rel: rel(), at: DropAt::Into(3) },
        LibA::Save(Box::new(crate::state::library::save_dialog::SaveForm { name: "a".into(), kind: "sql".into(), body: "SELECT 1".into(), ..Default::default() })),
        LibA::SaveLink { edit: Some(1), locate: true, link: crate::library::Link { name: "q".into(), ..Default::default() } },
        LibA::SnippetValues { id: 1, values: [("n".to_string(), "3".to_string())].into() },
        LibA::SnippetDefault { id: 1, conn: 1, db: "shop".into(), schema: String::new() },
        LibA::MakeValue { tab: 1, src: crate::state::library::snippet_strip::Src::Query, range: 0..1, literal: "1".into(), placeholder: "{{n:int=1}}".into() },
    ];
    all.extend(l.into_iter().map(Action::Library));
    all.extend(super::sql_tests::sql_actions().into_iter().map(Action::Sql));
    all.extend(super::sql_tests::struct_actions().into_iter().map(Action::Structure));
    all.extend(super::structure_tests::routine_actions().into_iter().map(Action::Routines));
    all.extend(tab_and_docs_actions());
    all.extend(super::ddl_tests::ddl_actions().into_iter().map(Action::Ddl));
    all.extend(super::ddl_tests::ops_actions().into_iter().map(Action::SqlOps));
    all.extend(super::agents_tests::agents_actions().into_iter().map(Action::Agents));
    all
}

/// The tab, Documents and undo variants of `all_actions`. Each EditA is round-tripped in docs_tests.rs.
fn tab_and_docs_actions() -> Vec<Action> {
    use crate::state::{Open, Sel, TabClose, View};
    let sel = Sel { conn: 1, db: "shop".into(), coll: "orders".into(), ..Default::default() };
    let d = || vec![mongodb::bson::doc! { "_id": 1, "a": "x" }];
    let t = [
        TabA::Open { sel: sel.clone(), how: Open::Link },
        TabA::Pick { tab: 1 },
        TabA::Close { tab: 1 },
        TabA::CloseMany { tab: 1, what: TabClose::Right },
        TabA::Duplicate { tab: 1 },
        TabA::Move { tab: 1, to: 2 },
        TabA::Reopen,
        TabA::Nav { back: true },
        TabA::View { tab: 1, view: View::Indexes },
    ];
    let k = [
        DocsA::Run { tab: 1 },
        DocsA::Page { tab: 1, page: 2 },
        DocsA::History { tab: 1, field: BarField::Sort, text: "{ a: 1 }".into() },
        DocsA::Layout { table: false },
        DocsA::Click { tab: 1, agg: false, row: 3, cmd: true, shift: false },
        DocsA::Select { tab: 1, agg: true, row: 0, toggle: true },
        DocsA::CloseRow { tab: 1, agg: false, row: 0 },
        DocsA::Check { tab: 1, row: 2, range: true },
        DocsA::CheckAll { tab: 1, rows: 50, on: true },
        DocsA::ClearChecks { tab: 1 },
        DocsA::Stage { tab: 1, doc: d().remove(0), new: Err("no".into()) },
        DocsA::FilterField { tab: 1, path: "a.0.b".into(), value: mongodb::bson::Bson::Int32(3), add: true },
        DocsA::Edit { tab: 1, doc: d().remove(0), act: EditA::Remove { path: vec!["a".into()] } },
        DocsA::Value { tab: 1, act: ValueA::Null },
        DocsA::Save { key: "k".into() },
        DocsA::Write { keys: vec!["k".into()] },
        DocsA::Discard { key: "k".into() },
        DocsA::AskBeforeSave(false),
        DocsA::SaveAll { tab: 1 },
        DocsA::Insert { tab: 1 },
        DocsA::InsertRun { tab: 1 },
        DocsA::InsertClose,
        DocsA::Duplicate { tab: 1, docs: d() },
        DocsA::Delete { tab: 1, docs: d() },
        DocsA::Filter { tab: 1, docs: d() },
        DocsA::DeleteRun { at: sel, docs: d() },
    ];
    let mut all: Vec<Action> = t.into_iter().map(Action::Tab).collect();
    all.extend(k.into_iter().map(Action::Docs));
    all.push(Action::Undo(UndoA::Apply { at: 7, confirmed: false }));
    let g = [
        AggA::Stages { tab: 1, acts: vec![StageA::Edited(0), StageA::Toggled(1), StageA::Move(0, 1), StageA::Remove(0), StageA::Add("$match".into()), StageA::Op(0, "$sort".into()), StageA::More(usize::MAX), StageA::Preview(0)] },
        AggA::TextMode { tab: 1, on: true },
        AggA::AutoPreview(false),
        AggA::Run { tab: 1 },
        AggA::RunTo { tab: 1, stage: 2 },
    ];
    all.extend(g.into_iter().map(Action::Aggregate));
    let x = [IdxA::Create { tab: 1, spec: mongodb::bson::doc! { "key": { "a": 1 }, "name": "a_1" } }, IdxA::Hide { tab: 1, name: "a_1".into(), hidden: true }, IdxA::Profile { tab: 1 }];
    all.extend(x.into_iter().map(Action::Indexes));
    all.extend(super::mongo_tests::all_mongo());
    all
}

/// The name of each variant; no wildcard, so a variant missing from `all_actions` fails to compile here first.
fn variant(a: &Action) -> String {
    let d = format!("{a:?}");
    match a {
        Action::Roles(r) => match r {
            RolesA::Open { .. } | RolesA::Tab(_) | RolesA::RTab(_) | RolesA::Database(_) | RolesA::Schema(_) | RolesA::Kind(_) | RolesA::Role(_) | RolesA::OpenRole(_) | RolesA::Stage(_) | RolesA::Apply | RolesA::Discard | RolesA::Close | RolesA::Unlock | RolesA::Refresh | RolesA::NewRole | RolesA::Drop(_) | RolesA::Password(_) | RolesA::Inherited(_) | RolesA::System(_) | RolesA::Cell(_) | RolesA::LimitEdited | RolesA::UntilEdited | RolesA::DefForm(_) | RolesA::Create { .. } | RolesA::SetPassword { .. } | RolesA::DropRun { .. } => {}
        },
        Action::Users(UsersA::Sql(u)) => match u {
            SqlUsersA::Open { .. } | SqlUsersA::Pick { .. } | SqlUsersA::Tab(_) | SqlUsersA::Apply | SqlUsersA::Close | SqlUsersA::Refresh | SqlUsersA::Unlock | SqlUsersA::NewUser | SqlUsersA::Drop { .. } | SqlUsersA::DropRun { .. } | SqlUsersA::Password { .. } | SqlUsersA::Create { .. } | SqlUsersA::SetPassword { .. } | SqlUsersA::Lock(_) | SqlUsersA::Expire(_) | SqlUsersA::Tls(_) | SqlUsersA::Role { .. } | SqlUsersA::RoleAdd(_) | SqlUsersA::RoleAddDone | SqlUsersA::Global { .. } | SqlUsersA::PrivAdd(_) | SqlUsersA::PrivAddDone | SqlUsersA::DbPriv { .. } | SqlUsersA::DbAdd(_) | SqlUsersA::DbAddDone => {}
        },
        Action::Users(UsersA::Mongo(m)) => match m {
            MongoUsersA::Open { .. } | MongoUsersA::Refresh | MongoUsersA::Unlock | MongoUsersA::Edit { .. } | MongoUsersA::CloseEdit | MongoUsersA::Save | MongoUsersA::Add | MongoUsersA::Drop { .. } | MongoUsersA::DropRun { .. } | MongoUsersA::Close => {}
        },
        Action::Server(ServerA::Mongo(s)) => match s {
            MongoServerA::Open { .. } | MongoServerA::Ops(_) | MongoServerA::Refresh | MongoServerA::AutoOps(_) | MongoServerA::ArmKill { .. } | MongoServerA::Disarm | MongoServerA::Kill { .. } | MongoServerA::Unlock | MongoServerA::Close => {}
        },
        Action::Server(ServerA::Sql(s)) => match s {
            SqlServerA::Open { .. } | SqlServerA::Tab(_) | SqlServerA::Refresh | SqlServerA::Auto(_) | SqlServerA::HideSleeping(_) | SqlServerA::Select(_) | SqlServerA::ArmKill { .. } | SqlServerA::Disarm | SqlServerA::Kill { .. } | SqlServerA::Global(_) | SqlServerA::StGroup(_) | SqlServerA::StAuto(_) | SqlServerA::Unlock | SqlServerA::Close => {}
        },
        Action::Server(ServerA::Pg(s)) => match s {
            PgServerA::Open { .. } | PgServerA::Tab(_) | PgServerA::Refresh | PgServerA::Auto(_) | PgServerA::HideIdle(_) | PgServerA::ChangedOnly(_) | PgServerA::Select(_) | PgServerA::Setting(_) | PgServerA::ArmSignal { .. } | PgServerA::Disarm | PgServerA::Signal { .. } | PgServerA::FetchQuery { .. } | PgServerA::Unlock | PgServerA::Close => {}
        },
        Action::Shell(s) => match s {
            ShellA::Run { .. } | ShellA::Start { .. } | ShellA::Stop { .. } | ShellA::Code { .. } | ShellA::Detach { .. } | ShellA::Attach { .. } | ShellA::Help { .. } | ShellA::Queries { .. } | ShellA::Raw { .. } | ShellA::Page { .. } | ShellA::Filled { .. } | ShellA::Packages(_) => {}
        },
        Action::Watch(w) => match w {
            WatchA::Start { .. } | WatchA::Stop { .. } | WatchA::Clear { .. } => {}
        },
        Action::Tools(t) => match t {
            ToolsA::Install | ToolsA::Dump { .. } | ToolsA::Stop | ToolsA::Close => {}
        },
        Action::Conn(c) => match c {
            ConnA::Connect { .. } | ConnA::Disconnect { .. } | ConnA::Activate { .. } | ConnA::Unlock { .. } | ConnA::Lock { .. } | ConnA::Switch { .. } | ConnA::Save { .. } | ConnA::Test { .. } => {}
            ConnA::Group(g) => match g {
                GroupA::Toggle { .. } | GroupA::FoldAll | GroupA::Move { .. } | GroupA::New { .. } | GroupA::Rename { .. } | GroupA::Color { .. } | GroupA::ConnectAll { .. } | GroupA::DisconnectAll { .. } | GroupA::RailOpen { .. } | GroupA::Unfold { .. } | GroupA::Save { .. } | GroupA::Delete { .. } | GroupA::Drop(_) | GroupA::RailOrder { .. } => {}
            },
        },
        Action::Side(s) => match s {
            SideA::ToggleDb { .. } | SideA::OpenDb { .. } | SideA::Fold { .. } | SideA::Revealed | SideA::Asked { .. } | SideA::Schema { .. } | SideA::MongoNewDb { .. } | SideA::MongoNewColl { .. } | SideA::MongoCreate { .. } | SideA::MongoCreateView { .. } | SideA::Rename { .. } | SideA::Duplicate { .. } | SideA::DropDb { .. } | SideA::DropColl { .. } | SideA::Stats { .. } | SideA::ViewDef { .. } => {}
        },
        Action::Update(u) => match u {
            UpdateA::CheckNow | UpdateA::Press(_) | UpdateA::ShowNotes | UpdateA::OfferedNotes => {}
        },
        Action::App(t) => match t {
            AppA::Dismiss(_) | AppA::Details(_) | AppA::ErrorsSeen | AppA::ClearErrors | AppA::RaiseLimit => {}
        },
        // `Pref::samples` lists each setting, from the same macro that declares them.
        Action::Prefs(p) => match p {
            PrefsA::Open(_) | PrefsA::Cat(_) | PrefsA::Set(_) | PrefsA::Zoom(_) => {}
        },
        Action::Library(l) => match l {
            LibA::CancelPending | LibA::ConflictReload { .. } | LibA::ConflictKeep { .. } | LibA::ConflictCompare { .. } | LibA::Commit { .. } | LibA::Duplicate(_) | LibA::Move { .. } | LibA::MoveToNew(_) | LibA::Delete(_) | LibA::NewFolder(_) | LibA::Scope(_) | LibA::NewScript { .. } | LibA::NewDir { .. } | LibA::Reload(_) | LibA::Unlink(_) | LibA::ScriptToLink { .. } | LibA::MoveFile { .. } | LibA::FileToQuarry { .. } | LibA::Save(_) | LibA::SaveLink { .. } | LibA::SnippetValues { .. } | LibA::SnippetDefault { .. } | LibA::MakeValue { .. } => {}
        },
        Action::Sql(a) => super::sql_tests::sql_variant(a),
        Action::Structure(a) => super::sql_tests::struct_variant(a),
        Action::Tab(t) => match t {
            TabA::Open { .. } | TabA::Pick { .. } | TabA::Close { .. } | TabA::CloseMany { .. } | TabA::Duplicate { .. } | TabA::Move { .. } | TabA::Reopen | TabA::Nav { .. } | TabA::View { .. } => {}
        },
        Action::Docs(k) => match k {
            DocsA::Edit { act, .. } => match act {
                EditA::Start { .. } | EditA::Commit | EditA::Expand | EditA::Cancel | EditA::Remove { .. } | EditA::AddField { .. } | EditA::CommitNew | EditA::EditDoc => {}
            },
            DocsA::Run { .. } | DocsA::Page { .. } | DocsA::History { .. } | DocsA::Layout { .. } | DocsA::Click { .. } | DocsA::Select { .. } | DocsA::CloseRow { .. } | DocsA::Check { .. } | DocsA::CheckAll { .. } | DocsA::ClearChecks { .. } | DocsA::Stage { .. } | DocsA::FilterField { .. } | DocsA::Value { .. } | DocsA::Save { .. } | DocsA::Write { .. } | DocsA::Discard { .. } | DocsA::AskBeforeSave(_) | DocsA::SaveAll { .. } | DocsA::Insert { .. } | DocsA::InsertRun { .. } | DocsA::InsertClose | DocsA::Duplicate { .. } | DocsA::Delete { .. } | DocsA::Filter { .. } | DocsA::DeleteRun { .. } => {}
        },
        Action::Undo(u) => match u {
            UndoA::Apply { .. } => {}
        },
        Action::Aggregate(g) => match g {
            AggA::Stages { .. } | AggA::TextMode { .. } | AggA::AutoPreview(_) | AggA::Run { .. } | AggA::RunTo { .. } => {}
        },
        Action::Indexes(x) => match x {
            IdxA::Create { .. } | IdxA::Hide { .. } | IdxA::Profile { .. } => {}
        },
        Action::Mongo(m) => super::mongo_tests::mongo_variant(m),
        Action::Grid(g) => super::mongo_tests::grid_variant(g),
        Action::Routines(r) => match r {
            RoutineA::Load { .. } | RoutineA::Reload { .. } | RoutineA::Fold { .. } | RoutineA::Open { .. } | RoutineA::New { .. } | RoutineA::Pane(_) | RoutineA::Save { .. } | RoutineA::Drop | RoutineA::Run => {}
        },
        Action::Ddl(a) => super::ddl_tests::ddl_variant(a),
        Action::SqlOps(a) => super::ddl_tests::ops_variant(a),
        Action::Agents(a) => super::agents_tests::agents_variant(a),
    }
    d.split(|c: char| c == '(' || c == ' ' || c == '{').take(3).collect::<Vec<_>>().join(" ")
}

#[test]
fn every_action_survives_a_serde_round_trip() {
    let all = all_actions();
    let names: std::collections::HashSet<String> = all.iter().map(variant).collect();
    assert_eq!(names.len(), all.len(), "each variant once");
    for a in all {
        let json = serde_json::to_value(&a).unwrap();
        let back: Action = serde_json::from_value(json.clone()).unwrap_or_else(|e| panic!("{a:?}: {e}"));
        assert_eq!(serde_json::to_value(&back).unwrap(), json, "{a:?}");
        assert!(!json.to_string().contains("\"pw\""), "a password reached the JSON of {a:?}");
    }
}

/// Stage a grant, see Apply refused while locked, apply it after unlocking, check the server, then undo it.
#[test]
fn live_grant_apply_and_undo() {
    use crate::db::sql::testing::{live_pg, unique_name, PG_ENVS};
    for var in PG_ENVS {
        let Some(l) = live_pg(var) else { continue };
        let (role, schema) = (unique_name("qr_core"), unique_name("qr_core_s"));
        l.qsql(&l.pool, &format!("CREATE ROLE \"{role}\"; CREATE SCHEMA \"{schema}\"; CREATE TABLE \"{schema}\".t (a int)"));
        let can = || l.one(&format!("SELECT has_table_privilege('{role}', '\"{schema}\".t', 'SELECT')::text"));

        let mut core = Core::for_test();
        core.worker.sql_connect(CONN, l.uri.clone(), std::time::Duration::from_secs(30), crate::db::Net::default());
        let Ok(Msg::SqlConnected { res: Ok((db, _, _)), .. }) = core.rx.recv_timeout(std::time::Duration::from_secs(20)) else { panic!("{var}: connect") };
        pg_conn(&mut core);
        core.conns[0].sql = Some(db);
        let loaded = |c: &Core| c.pg_roles.as_ref().is_some_and(|w| matches!(w.cluster, Some(Ok(_))) && matches!(w.objs, Some(Ok(_))) && !w.busy);

        assert!(matches!(roles(&mut core, RolesA::Open { conn: CONN }), Ok(Reply::Pending(_))));
        core.step_until("the roles and the tables", loaded);
        roles(&mut core, RolesA::Schema(schema.clone())).unwrap();
        core.step_until("the tables of the schema", |c| loaded(c) && c.pg_roles.as_ref().unwrap().objects().is_some_and(|o| o.schema == schema));

        let c = grant(&core, "t", &role);
        roles(&mut core, RolesA::Stage(c.clone())).unwrap();
        assert_eq!(staged(&core), 0, "{var}: a locked connection staged a grant");
        writable(&mut core, true);
        roles(&mut core, RolesA::Stage(c)).unwrap();
        writable(&mut core, false);
        assert_eq!(roles(&mut core, RolesA::Apply).unwrap_err().kind, RefusalKind::ReadOnly { conn: CONN });
        assert_eq!(can(), "false", "{var}: nothing runs before Apply");

        writable(&mut core, true);
        assert!(matches!(roles(&mut core, RolesA::Apply), Ok(Reply::Pending(_))));
        core.step_until("the apply to finish", |c| staged(c) == 0 && loaded(c));
        assert_eq!(can(), "true", "{var}: {}", core.status);

        // Recent changes has the inverse; running it takes the grant back.
        assert_eq!(core.undo.len(), 1, "{var}");
        let a = core.undo.remove(0);
        let (database, pk) = match &a.kind {
            UndoKind::Sql { database, undo, .. } => (database.clone(), undo.pk_names()),
            _ => panic!("{var}: not a SQL undo"),
        };
        let sql = core.conns[0].sql.as_ref().unwrap().at(&database);
        core.worker.sql_undo(sql, a, pk, false);
        let t = std::time::Instant::now();
        while can() != "false" {
            assert!(t.elapsed() < std::time::Duration::from_secs(40), "{var}: the undo never landed; {}", core.status);
            core.tick();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        l.qsql(&l.pool, &format!("DROP SCHEMA \"{schema}\" CASCADE; DROP ROLE \"{role}\""));
    }
}
