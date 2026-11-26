//! The views of a SQL tab run headlessly: the Query view with made-up answers, and a run against a server that is not
//! there, which fails the way a dropped connection does.

use super::*;
use super::sql::FilterA;
use crate::db::sql::{Engine, ResultSet};
use crate::state::{Conn, ConnMeta, Sel, Status, Tab, View};

const CONN: u64 = 1;

/// One of each SqlA variant, for `all_actions`.
pub(super) fn sql_actions() -> Vec<SqlA> {
    let tab = 7;
    vec![
        SqlA::Run { tab, all: true, explain: false },
        SqlA::RunText { tab, text: "SELECT 1".into() },
        SqlA::Stop { tab },
        SqlA::PickSet { tab, set: 1 },
        SqlA::TextMode { tab, on: true },
        SqlA::GotoPage { tab, set: 0, page: 2 },
        SqlA::SortPage { tab, keys: vec![("id".into(), true)] },
        SqlA::Check { tab, row: 3, range: true },
        SqlA::CheckAll { tab, on: true },
        SqlA::ClearChecks { tab },
        SqlA::SelectRow { tab, row: 2 },
        SqlA::Apply { tab },
        SqlA::Page { tab, page: 3 },
        SqlA::Ungroup { tab },
        SqlA::Drill { tab, row: 0 },
        SqlA::WhereMode { tab, sql: true },
        SqlA::Filter { tab, a: FilterA::Move(1, 0, 2) },
        SqlA::OpenCell { tab, row: 1, col: "note".into() },
        SqlA::SetNull { tab, row: 1, col: "note".into() },
        SqlA::Save { tab },
        SqlA::Discard { tab },
        SqlA::NewRow { tab, from: Some(mongodb::bson::doc! { "id": 1 }) },
        SqlA::Insert { tab, rows: vec![vec![("note".into(), mongodb::bson::Bson::Null)]] },
        SqlA::DeleteRows { tab, rows: vec![mongodb::bson::doc! { "_id": { "id": 1 } }] },
        SqlA::CrumbDb { tab, db: "shop".into(), schema: String::new() },
        SqlA::CrumbSchema { tab, schema: "public".into() },
    ]
}

/// No wildcard, so a variant missing from `sql_actions` fails to compile here first.
pub(super) fn sql_variant(a: &SqlA) {
    match a {
        SqlA::Run { .. } | SqlA::RunText { .. } | SqlA::Stop { .. } | SqlA::PickSet { .. } | SqlA::TextMode { .. } | SqlA::GotoPage { .. } | SqlA::SortPage { .. } | SqlA::Check { .. } | SqlA::CheckAll { .. } | SqlA::ClearChecks { .. } | SqlA::SelectRow { .. } | SqlA::Apply { .. } | SqlA::Page { .. } | SqlA::Ungroup { .. } | SqlA::Drill { .. } | SqlA::WhereMode { .. } | SqlA::Filter { .. } | SqlA::OpenCell { .. } | SqlA::SetNull { .. } | SqlA::Save { .. } | SqlA::Discard { .. } | SqlA::NewRow { .. } | SqlA::Insert { .. } | SqlA::DeleteRows { .. } | SqlA::CrumbDb { .. } | SqlA::CrumbSchema { .. } => {}
    }
}

/// A session on a port nothing listens on: every statement sent to it fails to connect.
fn dead_db() -> crate::db::sql::Db {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    let _guard = rt.enter();
    let opts = "mysql://root@127.0.0.1:1/shop".parse::<sqlx::mysql::MySqlConnectOptions>().expect("address");
    let pool = crate::db::sql::Pool::My(sqlx::mysql::MySqlPoolOptions::new().acquire_timeout(std::time::Duration::from_secs(5)).connect_lazy_with(opts.clone()));
    crate::db::sql::Db { pool, dialect: crate::db::sql::Dialect::MySql, opts: crate::db::sql::exec::Opts::My(opts), version: "8.4.3".into(), tls: false, sql_mode: String::new(), setup: Default::default(), conn: CONN }
}

/// A core with a MySQL connection, a Query tab on database shop open, and its uid.
fn query_core(session: bool) -> (Core, u64) {
    let mut core = Core::for_test();
    let mut c = Conn::new(ConnMeta { id: CONN, name: "test".into(), engine: Engine::MySql, ..Default::default() }, Status::Ready);
    if session {
        c.sql = Some(dead_db());
    }
    core.conns.push(c);
    let mut t = Tab::new(Sel { conn: CONN, db: "shop".into(), ..Default::default() }).for_engine(Engine::MySql);
    t.view = View::Sql;
    let uid = t.uid;
    core.tabs.push(t);
    (core, uid)
}

fn sql(core: &mut Core, a: SqlA) -> Result<Reply, Refusal> {
    core.execute(Action::Sql(a))
}

fn st(core: &Core) -> &crate::state::SqlTab {
    core.tabs[core.cur].sql.as_ref().unwrap()
}

fn set_of(rows: usize, stmt: &str) -> ResultSet {
    let rows = (0..rows).flat_map(|i| crate::db::Row::from_doc(&mongodb::bson::doc! { "id": i as i64 })).collect();
    let cols = vec![crate::db::sql::Col::plain("id", "BIGINT", crate::db::sql::value::Kind::Int, false)];
    ResultSet { rows, cols, ..ResultSet::new(stmt) }
}

#[test]
fn an_action_for_another_tab_changes_nothing() {
    let (mut core, uid) = query_core(false);
    core.tabs[0].sql.as_mut().unwrap().query = "SELECT 1".into();
    let e = sql(&mut core, SqlA::Run { tab: uid + 1000, all: true, explain: false }).unwrap_err();
    assert_eq!(e.kind, RefusalKind::Invalid);
    assert_eq!(e.msg, "That tab is no longer open.");
    assert_eq!(st(&core).run_req, 0);
}

#[test]
fn run_without_a_session_asks_to_connect_and_sends_nothing() {
    let (mut core, uid) = query_core(false);
    sql(&mut core, SqlA::Run { tab: uid, all: true, explain: false }).unwrap();
    assert_eq!(core.toasts.last().unwrap().title, "Nothing to run");
    core.tabs[0].sql.as_mut().unwrap().query = "SELECT 1".into();
    sql(&mut core, SqlA::Run { tab: uid, all: true, explain: false }).unwrap();
    assert_eq!(core.toasts.last().unwrap().title, "Connect first");
    assert_eq!(st(&core).run_req, 0);
}

#[test]
fn explain_hands_the_filled_statement_to_the_app() {
    let (mut core, uid) = query_core(false);
    core.tabs[0].sql.as_mut().unwrap().query = "SELECT * FROM orders".into();
    sql(&mut core, SqlA::Run { tab: uid, all: true, explain: true }).unwrap();
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::SqlExplain(t))] if t == "SELECT * FROM orders"));
}

#[test]
fn a_run_on_a_dropped_server_ends_with_its_error_and_goes_in_the_history() {
    let (mut core, uid) = query_core(true);
    sql(&mut core, SqlA::RunText { tab: uid, text: "SELECT 1".into() }).unwrap();
    let req = st(&core).run_req;
    assert_ne!(req, 0, "the run was sent");
    assert_eq!(core.prefs.history.get(&format!("{CONN}/shop/#query")).map(|h| h.selector.clone()), Some(vec!["SELECT 1".to_string()]));
    // A second Run while the first is on is ignored.
    sql(&mut core, SqlA::RunText { tab: uid, text: "SELECT 2".into() }).unwrap();
    assert_eq!(st(&core).run_req, req);
    core.step_until("the failed run", |c| c.tabs[0].sql.as_ref().unwrap().run_req == 0);
    let s = st(&core);
    assert_eq!(s.sets.len(), 1);
    assert!(s.sets[0].error.is_some(), "a connect failure is the set's error");
}

#[test]
fn an_answer_to_an_older_run_is_dropped() {
    let (mut core, _) = query_core(false);
    core.tabs[0].sql.as_mut().unwrap().run_req = 5;
    core.inject(Msg::SqlRan { req: 4, sets: vec![set_of(2, "SELECT 4")] });
    assert!(st(&core).sets.is_empty());
    assert_eq!(st(&core).run_req, 5);
    core.inject(Msg::SqlRan { req: 5, sets: vec![set_of(2, "SELECT 5"), set_of(1, "SELECT 6")] });
    assert_eq!((st(&core).run_req, st(&core).sets.len(), st(&core).sets_req), (0, 2, 5));
    // A count for another run is dropped too.
    core.tabs[0].sql.as_mut().unwrap().sets[0].page = Some(Default::default());
    core.inject(Msg::SqlCount { req: 4, idx: 0, res: Ok(9) });
    assert_eq!(st(&core).sets[0].page.unwrap().total, None);
    core.inject(Msg::SqlCount { req: 5, idx: 0, res: Ok(9) });
    assert_eq!(st(&core).sets[0].page.unwrap().total, Some(9));
}

#[test]
fn checks_follow_clicks_and_a_picked_set_clears_them() {
    let (mut core, uid) = query_core(false);
    core.tabs[0].sql.as_mut().unwrap().sets = vec![set_of(5, "SELECT a"), set_of(2, "SELECT b")];
    sql(&mut core, SqlA::Check { tab: uid, row: 1, range: false }).unwrap();
    sql(&mut core, SqlA::Check { tab: uid, row: 3, range: true }).unwrap();
    assert_eq!(st(&core).qchecked.iter().copied().collect::<Vec<_>>(), [1, 2, 3]);
    sql(&mut core, SqlA::Check { tab: uid, row: 2, range: false }).unwrap();
    assert_eq!(st(&core).qchecked.iter().copied().collect::<Vec<_>>(), [1, 3]);
    sql(&mut core, SqlA::SelectRow { tab: uid, row: 4 }).unwrap();
    sql(&mut core, SqlA::CheckAll { tab: uid, on: true }).unwrap();
    assert_eq!(st(&core).qchecked.len(), 5);
    sql(&mut core, SqlA::PickSet { tab: uid, set: 1 }).unwrap();
    let s = st(&core);
    assert_eq!((s.set_sel, s.qchecked.len(), s.qsel, s.qanchor), (1, 0, None, None));
    sql(&mut core, SqlA::TextMode { tab: uid, on: true }).unwrap();
    assert!(st(&core).text_mode);
}

#[test]
fn a_client_sort_keeps_checks_on_their_rows() {
    let (mut core, uid) = query_core(false);
    core.tabs[0].sql.as_mut().unwrap().sets = vec![set_of(3, "SELECT id")];
    sql(&mut core, SqlA::Check { tab: uid, row: 0, range: false }).unwrap();
    sql(&mut core, SqlA::SortPage { tab: uid, keys: vec![("id".into(), true)] }).unwrap();
    assert_eq!(st(&core).qchecked.iter().copied().collect::<Vec<_>>(), [2], "row id 0 is last once sorted descending");
    assert!(!st(&core).sets[0].sort.is_empty());
}

#[test]
fn a_late_lint_answer_lands_on_the_tab_that_asked() {
    let (mut core, _) = query_core(false);
    let s = core.tabs[0].sql.as_mut().unwrap();
    s.query = "SELCT 1".into();
    (s.lint.req, s.lint.asked) = (3, "SELCT 1".into());
    core.inject(Msg::SqlLint { req: 2, lint: Ok(None) });
    assert_eq!(st(&core).lint.req, 3, "another request's answer is dropped");
    let lint = crate::db::sql::Lint { span: 0..5, msg: "syntax".into(), warn: false };
    core.inject(Msg::SqlLint { req: 3, lint: Ok(Some(lint)) });
    assert_eq!(st(&core).lint.req, 0);
    assert!(st(&core).lint.mark("SELCT 1").is_some());
}

/// A core with table `orders` of a MySQL connection open on its Content grid: columns id (the key) and note, three rows.
fn content_core(session: bool) -> (Core, u64) {
    use crate::db::sql::{value::Kind, Col};
    let (mut core, _) = query_core(session);
    core.tabs.clear();
    let mut t = Tab::new(Sel { conn: CONN, db: "shop".into(), coll: "orders".into(), ..Default::default() }).for_engine(Engine::MySql);
    t.docs = (1..=3).map(|i: i64| crate::db::Row::from_doc(&mongodb::bson::doc! { "_id": { "id": i }, "id": i, "note": format!("n{i}") }).unwrap()).collect();
    let s = t.sql.as_mut().unwrap();
    s.cols = std::sync::Arc::new(vec![Col::plain("id", "BIGINT", Kind::Int, false), Col::plain("note", "TEXT", Kind::Text, true)]);
    s.pk = vec!["id".into()];
    let uid = t.uid;
    core.tabs.push(t);
    (core, uid)
}

#[test]
fn the_panel_adds_and_removes_conditions_and_writes_the_where() {
    let (mut core, uid) = content_core(false);
    sql(&mut core, SqlA::Filter { tab: uid, a: FilterA::Add(0, true) }).unwrap();
    let written = |c: &Core| crate::db::sql::filter::to_sql(&c.tabs[0].sql.as_ref().unwrap().bar.tree, crate::db::sql::Dialect::MySql, true);
    let s = core.tabs[0].sql.as_ref().unwrap();
    assert_eq!(s.bar.tree.root.kids.len(), 1, "a group holding one row");
    let id = s.bar.panel.sel.unwrap();
    assert_eq!(core.tabs[0].filter_txt, written(&core), "the WHERE follows the tree");
    sql(&mut core, SqlA::Filter { tab: uid, a: FilterA::Unwrap(id) }).unwrap();
    sql(&mut core, SqlA::Filter { tab: uid, a: FilterA::Mode(0, crate::db::sql::filter::Match::Any) }).unwrap();
    assert_eq!(core.tabs[0].sql.as_ref().unwrap().bar.tree.root.mode, crate::db::sql::filter::Match::Any);
    assert_eq!(core.tabs[0].filter_txt, written(&core));
    sql(&mut core, SqlA::Filter { tab: uid, a: FilterA::Clear }).unwrap();
    assert_eq!(written(&core), "");
    assert!(core.tabs[0].sql.as_ref().unwrap().bar.panel.sel.is_none());
}

#[test]
fn a_locked_table_refuses_edits_quietly_so_the_app_can_offer_unlock() {
    let (mut core, uid) = content_core(false);
    let toasts = core.toasts.len();
    let e = sql(&mut core, SqlA::OpenCell { tab: uid, row: 0, col: "note".into() }).unwrap_err();
    assert_eq!(e.kind, RefusalKind::ReadOnly { conn: CONN });
    for a in [SqlA::SetNull { tab: uid, row: 0, col: "note".into() }, SqlA::Save { tab: uid }, SqlA::NewRow { tab: uid, from: None }] {
        assert_eq!(sql(&mut core, a).unwrap_err().kind, RefusalKind::ReadOnly { conn: CONN });
    }
    assert_eq!(core.toasts.len(), toasts, "Unlock opens instead of a toast");
    assert!(core.sql_cell.is_none() && core.drafts.is_empty());
    // A key column is refused before writes are asked for.
    let e = sql(&mut core, SqlA::OpenCell { tab: uid, row: 0, col: "id".into() }).unwrap_err();
    assert_eq!(e.kind, RefusalKind::Invalid);
}

#[test]
fn an_edit_is_staged_then_discarded_or_confirmed() {
    let (mut core, uid) = content_core(false);
    core.conns[0].writable = true;
    sql(&mut core, SqlA::OpenCell { tab: uid, row: 1, col: "note".into() }).unwrap();
    assert_eq!(core.sql_cell.as_ref().map(|e| e.text.as_str()), Some("n2"));
    core.sql_cell = None;
    sql(&mut core, SqlA::SetNull { tab: uid, row: 1, col: "note".into() }).unwrap();
    assert_eq!(core.tab_drafts().len(), 1);
    core.prefs.confirm_save = true;
    sql(&mut core, SqlA::Save { tab: uid }).unwrap();
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::SaveAll(k))] if k.len() == 1));
    sql(&mut core, SqlA::Discard { tab: uid }).unwrap();
    assert!(core.drafts.is_empty());
    sql(&mut core, SqlA::NewRow { tab: uid, from: None }).unwrap();
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::SqlRow(f))] if f.fields.len() == 2));
}

#[test]
fn a_page_load_answers_only_the_request_it_was_sent_for() {
    use crate::db::sql::{value::Kind, Col};
    let (mut core, uid) = content_core(true);
    sql(&mut core, SqlA::Page { tab: uid, page: 2 }).unwrap();
    let req = core.tabs[0].req;
    assert!(req != 0 && core.tabs[0].loading && core.tabs[0].page == 2);
    let cols = vec![Col::plain("id", "BIGINT", Kind::Int, false)];
    core.inject(Msg::SqlCols { req: req - 1, cols: cols.clone(), pk: vec![], estimated: true, matched: None });
    assert_eq!(core.tabs[0].sql.as_ref().unwrap().cols.len(), 2, "an older page's columns are dropped");
    core.inject(Msg::SqlCols { req, cols, pk: vec!["id".into()], estimated: true, matched: None });
    let s = core.tabs[0].sql.as_ref().unwrap();
    assert!(s.cols.len() == 1 && s.estimated && s.bar.table_cols.len() == 1);
    // The demo has no server: Apply sets the page and sends nothing.
    core.demo = true;
    sql(&mut core, SqlA::Apply { tab: uid }).unwrap();
    assert_eq!((core.tabs[0].page, core.tabs[0].req), (0, req));
}

/// One of each StructA variant, for `all_actions`.
pub(super) fn struct_actions() -> Vec<StructA> {
    let tab = 7;
    vec![
        StructA::DropColumn { tab, col: "note".into() },
        StructA::MoveColumn { tab, col: "note".into(), to: "id".into() },
        StructA::DropIndex { tab, index: "ix".into() },
        StructA::Unstage { tab, at: 0 },
        StructA::Discard { tab },
        StructA::Apply { tab },
        StructA::Run { tab },
        StructA::Refresh { tab },
        StructA::EditColumn { tab, col: Some("note".into()) },
        StructA::DuplicateColumn { tab, col: "note".into() },
        StructA::EditIndex { tab, index: None },
        StructA::EditCheck { tab, check: Some("chk".into()) },
        StructA::DropCheck { tab, check: "chk".into() },
        StructA::EditFk { tab, fk: None },
        StructA::DropFk { tab, fk: "fk".into() },
        StructA::EditTable { tab },
        StructA::Stage { tab, changes: vec![crate::db::sql::ddl::Change::Drop("note".into()), crate::db::sql::ddl::Change::AddIndex(Default::default())], replaces: Some("note".into()) },
        StructA::StageTable { tab, changes: vec![crate::db::sql::ddl::Change::Table { opt: crate::db::sql::ddl::TableOpt::Comment("c".into()), was: crate::db::sql::ddl::TableOpt::Comment(String::new()) }] },
        StructA::TrigNew { tab },
        StructA::TrigRevert { tab, key: String::new() },
        StructA::TrigSave { tab, key: "t".into() },
        StructA::TrigAskDrop { tab, name: "t".into() },
        StructA::TrigDrop { tab, name: "t".into() },
        StructA::Maintain { tab, what: crate::db::sql::Maint::Optimize },
        StructA::RunMaint { tab, what: crate::db::sql::Maint::Analyze },
    ]
}

pub(super) fn struct_variant(a: &StructA) {
    match a {
        StructA::DropColumn { .. } | StructA::MoveColumn { .. } | StructA::DropIndex { .. } | StructA::Unstage { .. } | StructA::Discard { .. } | StructA::Apply { .. } | StructA::Run { .. } | StructA::Refresh { .. } => {}
        StructA::EditColumn { .. } | StructA::DuplicateColumn { .. } | StructA::EditIndex { .. } | StructA::EditCheck { .. } | StructA::DropCheck { .. } | StructA::EditFk { .. } | StructA::DropFk { .. } | StructA::EditTable { .. } | StructA::Stage { .. } | StructA::StageTable { .. } => {}
        StructA::TrigNew { .. } | StructA::TrigRevert { .. } | StructA::TrigSave { .. } | StructA::TrigAskDrop { .. } | StructA::TrigDrop { .. } | StructA::Maintain { .. } | StructA::RunMaint { .. } => {}
    }
}

/// `content_core` with the table's structure read: columns id and note.
pub(super) fn structure_core() -> (Core, u64) {
    use crate::db::sql::structure::{Column, Structure};
    let (mut core, uid) = content_core(false);
    let col = |name: &str, ty: &str| Column { name: name.into(), full_type: ty.into(), ..Default::default() };
    core.tabs[0].sql.as_mut().unwrap().st.data = Some(Structure { columns: vec![col("id", "bigint"), col("note", "text")], ..Default::default() });
    (core, uid)
}

fn structure(core: &mut Core, a: StructA) -> Result<Reply, Refusal> {
    core.execute(Action::Structure(a))
}

#[test]
fn a_staged_drop_waits_for_apply_and_apply_asks_for_writes() {
    let (mut core, uid) = structure_core();
    structure(&mut core, StructA::DropColumn { tab: uid, col: "note".into() }).unwrap();
    structure(&mut core, StructA::DropColumn { tab: uid, col: "note".into() }).unwrap();
    assert_eq!(core.staged().len(), 1, "a column is dropped once");
    structure(&mut core, StructA::DropColumn { tab: uid, col: "gone".into() }).unwrap();
    assert_eq!(core.staged().len(), 1, "an unknown column stages nothing");
    let e = structure(&mut core, StructA::Apply { tab: uid }).unwrap_err();
    assert_eq!(e.kind, RefusalKind::ReadOnly { conn: CONN });
    // Writable, a drop loses data, so Apply asks for the table's name first.
    core.conns[0].writable = true;
    structure(&mut core, StructA::Apply { tab: uid }).unwrap();
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::Typed { expect, .. })] if expect == "orders"));
    structure(&mut core, StructA::Unstage { tab: uid, at: 0 }).unwrap();
    assert!(core.staged().is_empty());
    let e = structure(&mut core, StructA::Discard { tab: uid + 1000 }).unwrap_err();
    assert_eq!(e.kind, RefusalKind::Invalid);
}

#[test]
fn an_alter_answer_clears_the_staged_list_of_its_table_only() {
    let (mut core, uid) = structure_core();
    structure(&mut core, StructA::DropColumn { tab: uid, col: "note".into() }).unwrap();
    core.tabs[0].sql.as_mut().unwrap().st.applying = true;
    // Another table of the connection: nothing changes here.
    core.inject(Msg::SqlAltered { conn: CONN, database: String::new(), schema: "shop".into(), table: "customers".into(), res: Ok("done".into()), undo: None, sql_mode: None, alter: true, unsure: false });
    assert_eq!(core.staged().len(), 1);
    // No session, so no tab resolves to the answer; the sending tab still gets Apply back.
    core.inject(Msg::SqlAltered { conn: CONN, database: String::new(), schema: "shop".into(), table: "orders".into(), res: Err("refused".into()), undo: None, sql_mode: None, alter: true, unsure: false });
    let s = core.tabs[0].sql.as_ref().unwrap();
    assert!(!s.st.applying && s.st.staged.len() == 1, "a refused ALTER keeps what was staged");
}
