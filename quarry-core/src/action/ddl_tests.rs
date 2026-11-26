//! Creating, dropping, renaming and emptying SQL objects, the bulk dialog and the Explain dialog's answers, headlessly:
//! the asks a locked connection refuses, the forms they open, and the answers the server sends back.

use super::*;
use crate::db::sql::create::{Ddl, Meta};
use crate::db::sql::view::ViewDone;
use crate::db::sql::{Dialect, Engine, ResultSet};
use crate::state::sql::create::DropKind;
use crate::state::sql::explain::SqlExplain;
use crate::state::{Conn, ConnMeta, Sel, Status, Tab, View};

const CONN: u64 = 1;

/// One of each DdlA variant, for `all_actions`.
pub(super) fn ddl_actions() -> Vec<DdlA> {
    let (conn, db, name) = (CONN, || "shop".to_string(), || "orders".to_string());
    vec![
        DdlA::NewDb { conn },
        DdlA::NewTable { conn, db: db() },
        DdlA::NewSchema { conn, db: db() },
        DdlA::NewView { conn, db: db(), body: "SELECT 1".into() },
        DdlA::EditView { conn, db: db(), name: name() },
        DdlA::QueryAsView { tab: 7 },
        DdlA::Drop { conn, db: db(), name: name(), kind: DropKind::Table },
        DdlA::Rename { conn, db: db(), name: name(), kind: DropKind::View },
        DdlA::Duplicate { conn, db: db(), name: name() },
        DdlA::Truncate { conn, db: db(), table: name() },
        DdlA::DropRun { conn, database: String::new(), schema: db(), table: name(), kind: DropKind::Truncate, opt: false },
        DdlA::RenameRun { conn, database: db(), schema: "public".into(), from: name(), to: "o2".into(), kind: DropKind::Schema },
        DdlA::DuplicateRun { conn, database: String::new(), schema: db(), from: name(), to: "o2".into(), data: true },
        DdlA::TruncateRun { conn, database: db(), schema: "public".into(), table: name(), restart: true, cascade: false },
        DdlA::CreateDb { conn, name: "n".into(), charset: "utf8mb4".into(), collation: "utf8mb4_bin".into(), meta: Meta { engines: vec!["InnoDB".into()], ..Default::default() } },
        DdlA::CreatePgDb { conn, name: "n".into(), opts: crate::db::sql::pg::create::DbOpts { template: Some("template0".into()), ..Default::default() } },
        DdlA::CreateSchema { conn, database: db(), name: "s".into() },
        DdlA::CreateTable { conn, spec: Default::default(), meta: Default::default() },
        DdlA::CreatePgTable { conn, database: db(), spec: Default::default() },
        DdlA::SaveView { conn, database: db(), spec: Default::default() },
    ]
}

/// No wildcard, so a variant missing from `ddl_actions` fails to compile here first.
pub(super) fn ddl_variant(a: &DdlA) {
    match a {
        DdlA::NewDb { .. } | DdlA::NewTable { .. } | DdlA::NewSchema { .. } | DdlA::NewView { .. } | DdlA::EditView { .. } | DdlA::QueryAsView { .. } | DdlA::Drop { .. } | DdlA::Rename { .. } | DdlA::Duplicate { .. } | DdlA::Truncate { .. } | DdlA::DropRun { .. } | DdlA::RenameRun { .. } | DdlA::DuplicateRun { .. } | DdlA::TruncateRun { .. } | DdlA::CreateDb { .. } | DdlA::CreatePgDb { .. } | DdlA::CreateSchema { .. } | DdlA::CreateTable { .. } | DdlA::CreatePgTable { .. } | DdlA::SaveView { .. } => {}
    }
}

/// One of each SqlOpsA variant, for `all_actions`.
pub(super) fn ops_actions() -> Vec<SqlOpsA> {
    vec![SqlOpsA::Bulk { tab: 7, update: true, col: Some("note".into()) }, SqlOpsA::ReadColumn { tab: 7, col: "note".into(), op: crate::db::ColOp::ALL[0] }, SqlOpsA::BulkRun { sel: Default::default(), ns: "shop".into(), plan: crate::db::sql::bulk::BulkPlan { op: crate::db::sql::bulk::BulkOp::Delete, filter: String::new(), cols: Vec::new(), pk: Vec::new(), bs: true, expected: 1, undo: false, engine: String::new() } }]
}

pub(super) fn ops_variant(a: &SqlOpsA) {
    match a {
        SqlOpsA::Bulk { .. } | SqlOpsA::ReadColumn { .. } | SqlOpsA::BulkRun { .. } => {}
    }
}

/// One of each SqlAskA variant, for `all_actions`.
pub(super) fn ask_actions() -> Vec<SqlAskA> {
    let sel = crate::state::Sel { conn: 1, db: "shop".into(), coll: "orders".into(), ..Default::default() };
    vec![
        SqlAskA::AskSchemas { conn: 1, db: "shop".into() },
        SqlAskA::LoadTables { conn: 1, db: "shop".into(), schema: "public".into() },
        SqlAskA::Refresh { conn: 1, db: "shop".into() },
        SqlAskA::Serve { conn: 1, db: "shop".into(), schema: String::new(), wants: Default::default() },
        SqlAskA::ServeFacts { conn: 1, db: "shop".into(), schema: String::new(), table: "orders".into(), column: "id".into(), kind: Default::default() },
        SqlAskA::ForgetSchema { conn: 1, db: "shop".into() },
        SqlAskA::Tables { conn: 1, schema: "shop".into() },
        SqlAskA::TableSizes { conn: 1, db: "shop".into(), schema: String::new() },
        SqlAskA::TableInfo { sel: sel.clone() },
        SqlAskA::Structure { sel: sel.clone(), ns: "shop".into(), table: "orders".into() },
        SqlAskA::PgStructure { conn: 1, database: "shop".into(), schema: "public".into(), table: "orders".into() },
        SqlAskA::Orphans { sel: sel.clone(), schema: "shop".into(), table: "orders".into(), fk: Default::default() },
        SqlAskA::FilterTick { now: 1.5 },
        SqlAskA::LintTick { now: 1.5, stmt: 0..4 },
        SqlAskA::KeySearch { fk: Default::default(), q: "a".into(), known: None },
        SqlAskA::ShowDup { sel, index: "PRIMARY".into(), value: "1".into() },
        SqlAskA::Dump { conn: 1, db: "shop".into(), plan: crate::db::sql::dump::DumpPlan { schema: "shop".into(), tables: Vec::new(), create_db: false, extras: false, rows_per_insert: 1, snapshot: false, filter: None, grants: false, owners: false }, path: "a.sql".into(), total: 0 },
        SqlAskA::CancelJob,
        SqlAskA::ExplainSend { conn: 1, database: "shop".into(), schema: "shop".into(), stmt: "SELECT 1".into(), analyze: Some(crate::db::sql::plan::Analyze::Direct) },
        SqlAskA::ExplainStop,
        SqlAskA::ExplainEvidence { conn: 1, database: "shop".into(), schema: "shop".into(), wants: vec![crate::db::sql::plan::evidence::Want { schema: None, table: "orders".into(), names: vec![("o".into(), "orders".into())], cols: vec!["id".into()] }] },
        SqlAskA::ExplainHypo { conn: 1, database: "shop".into(), schema: "public".into(), create: "CREATE INDEX ON orders (id)".into(), stmt: "SELECT 1".into(), locked: true },
        SqlAskA::RunFile { conn: 1, db: "shop".into(), schema: String::new(), src: crate::db::sql::csv::Source::Mem(std::sync::Arc::new(b"SELECT 1;".to_vec())), name: "a.sql".into() },
        SqlAskA::StartImport {
            sel: crate::state::Sel { conn: 1, db: "shop".into(), coll: "orders".into(), ..Default::default() },
            plan: crate::db::sql::import::ImportPlan { source: crate::db::sql::csv::Source::File("a.csv".into()), opts: crate::db::sql::csv::Opts { delim: ',', quote: Some('"'), enc: crate::db::sql::csv::Enc::Utf8 }, header: true, info: Default::default(), map: crate::db::sql::csv::Mapping { map: vec![Some(0)], fmts: vec![crate::db::sql::csv::DateFmt::DayFirst], empty_null: true }, dup: crate::db::sql::import::Dup::Stop, want_undo: false },
            rows: 3,
            size: 40,
        },
    ]
}

pub(super) fn ask_variant(a: &SqlAskA) {
    match a {
        SqlAskA::AskSchemas { .. } | SqlAskA::LoadTables { .. } | SqlAskA::Refresh { .. } | SqlAskA::Serve { .. } | SqlAskA::ServeFacts { .. } | SqlAskA::ForgetSchema { .. } | SqlAskA::Tables { .. } | SqlAskA::TableSizes { .. } | SqlAskA::TableInfo { .. } | SqlAskA::Structure { .. } | SqlAskA::PgStructure { .. } | SqlAskA::Orphans { .. } | SqlAskA::FilterTick { .. } | SqlAskA::LintTick { .. } | SqlAskA::KeySearch { .. } | SqlAskA::ShowDup { .. } | SqlAskA::Dump { .. } | SqlAskA::CancelJob | SqlAskA::ExplainSend { .. } | SqlAskA::ExplainStop | SqlAskA::ExplainEvidence { .. } | SqlAskA::ExplainHypo { .. } | SqlAskA::RunFile { .. } | SqlAskA::StartImport { .. } => {}
    }
}

/// A session on a port nothing listens on, which the asks only hand to the worker.
fn dead_db(d: Dialect) -> crate::db::sql::Db {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    let _guard = rt.enter();
    let opts = "mysql://root@127.0.0.1:1/shop".parse::<sqlx::mysql::MySqlConnectOptions>().expect("address");
    let pool = crate::db::sql::Pool::My(sqlx::mysql::MySqlPoolOptions::new().acquire_timeout(std::time::Duration::from_secs(5)).connect_lazy_with(opts.clone()));
    crate::db::sql::Db { pool, dialect: d, opts: crate::db::sql::exec::Opts::My(opts), version: "8.4.3".into(), tls: false, sql_mode: String::new(), setup: Default::default(), conn: CONN }
}

/// A core with one connection of `engine`, writable or locked, with a session when `session`.
fn core_on(engine: Engine, writable: bool, session: bool) -> Core {
    let mut core = Core::for_test();
    let mut c = Conn::new(ConnMeta { id: CONN, name: "test".into(), engine, ..Default::default() }, Status::Ready);
    c.writable = writable;
    if session {
        c.sql = Some(dead_db(if engine == Engine::Postgres { Dialect::Postgres } else { Dialect::MySql }));
    }
    core.conns.push(c);
    core
}

fn ddl(core: &mut Core, a: DdlA) -> Result<Reply, Refusal> {
    core.execute(Action::Ddl(a))
}

/// The form an ask opened, if one did.
fn opened(core: &mut Core) -> Option<SqlForm> {
    core.take_ui_fx().into_iter().find_map(|fx| match fx {
        UiFx::Open(Dialog::SqlForm(f)) => Some(f),
        _ => None,
    })
}

fn tab_on(core: &mut Core, db: &str, coll: &str) -> u64 {
    let mut t = Tab::new(Sel { conn: CONN, db: db.into(), coll: coll.into(), ..Default::default() }).for_engine(Engine::MySql);
    t.view = View::Sql;
    let uid = t.uid;
    core.tabs.push(t);
    uid
}

#[test]
fn every_ask_on_a_locked_connection_asks_to_unlock_and_opens_nothing() {
    let mut core = core_on(Engine::MySql, false, true);
    let asks = [
        DdlA::NewDb { conn: CONN },
        DdlA::NewTable { conn: CONN, db: "shop".into() },
        DdlA::NewView { conn: CONN, db: "shop".into(), body: String::new() },
        DdlA::EditView { conn: CONN, db: "shop".into(), name: "v".into() },
        DdlA::Drop { conn: CONN, db: "shop".into(), name: "orders".into(), kind: DropKind::Table },
        DdlA::Rename { conn: CONN, db: "shop".into(), name: "orders".into(), kind: DropKind::Table },
        DdlA::Duplicate { conn: CONN, db: "shop".into(), name: "orders".into() },
    ];
    for a in asks {
        let e = ddl(&mut core, a.clone()).unwrap_err();
        assert_eq!(e.kind, RefusalKind::ReadOnly { conn: CONN }, "{a:?}");
        assert!(opened(&mut core).is_none(), "{a:?} opened a form while locked");
    }
    assert!(core.toasts.is_empty(), "these asks open Unlock without a toast first");
}

#[test]
fn a_drop_ask_opens_its_typed_confirmation_with_the_names_it_was_given() {
    let mut core = core_on(Engine::MySql, true, true);
    ddl(&mut core, DdlA::Drop { conn: CONN, db: "shop".into(), name: "orders".into(), kind: DropKind::Truncate }).unwrap();
    match opened(&mut core) {
        Some(SqlForm::Drop(f)) => assert_eq!((f.database.as_str(), f.schema.as_str(), f.table.as_str(), f.kind, f.typed.as_str()), ("", "shop", "orders", DropKind::Truncate, "")),
        _ => panic!("the drop dialog did not open"),
    }
}

#[test]
fn a_postgres_table_ask_takes_the_schema_the_sidebar_shows() {
    let mut core = core_on(Engine::Postgres, true, false);
    core.conns[0].pg.entry("shop".into()).or_default().picked = "billing".into();
    ddl(&mut core, DdlA::Drop { conn: CONN, db: "shop".into(), name: "invoices".into(), kind: DropKind::Table }).unwrap();
    match opened(&mut core) {
        Some(SqlForm::Drop(f)) => assert_eq!((f.database.as_str(), f.schema.as_str(), f.table.as_str()), ("shop", "billing", "invoices")),
        _ => panic!("the drop dialog did not open"),
    }
    ddl(&mut core, DdlA::Rename { conn: CONN, db: "shop".into(), name: "billing".into(), kind: DropKind::Schema }).unwrap();
    match opened(&mut core) {
        Some(SqlForm::Rename(f)) => assert_eq!((f.database.as_str(), f.schema.as_str(), f.from.as_str()), ("shop", "billing", "billing")),
        _ => panic!("the rename dialog did not open"),
    }
    ddl(&mut core, DdlA::NewTable { conn: CONN, db: "shop".into() }).unwrap();
    assert!(matches!(opened(&mut core), Some(SqlForm::PgNewTable(f)) if f.schema == "billing"), "PostgreSQL's builder opens in the picked schema");
}

#[test]
fn a_confirmed_drop_on_a_connection_locked_since_asks_to_unlock_without_running_it_again() {
    let mut core = core_on(Engine::MySql, false, true);
    let r = ddl(&mut core, DdlA::DropRun { conn: CONN, database: String::new(), schema: "shop".into(), table: "orders".into(), kind: DropKind::Table, opt: false });
    assert_eq!(r, Ok(Reply::Done), "no retry: the name is typed again after unlocking");
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::Unlock(CONN))]));
    let r = ddl(&mut core, DdlA::RenameRun { conn: CONN, database: String::new(), schema: "shop".into(), from: "a".into(), to: "b".into(), kind: DropKind::Table });
    assert_eq!(r, Ok(Reply::Done));
    assert!(matches!(core.take_ui_fx().as_slice(), [UiFx::Open(Dialog::Unlock(CONN))]));
}

#[test]
fn the_demo_runs_no_confirmed_drop() {
    let mut core = core_on(Engine::MySql, true, true);
    core.demo = true;
    ddl(&mut core, DdlA::DropRun { conn: CONN, database: String::new(), schema: "shop".into(), table: "orders".into(), kind: DropKind::Table, opt: false }).unwrap();
    assert_eq!(core.status, "Offline demo: no database is connected.");
}

#[test]
fn a_postgres_drop_waits_for_a_running_shell_script_in_that_database() {
    let mut core = core_on(Engine::Postgres, true, false);
    tab_on(&mut core, "shop", "");
    core.tabs[0].shell.cancel = Some(tokio::sync::oneshot::channel().0);
    let e = ddl(&mut core, DdlA::Drop { conn: CONN, db: "shop".into(), name: String::new(), kind: DropKind::Database }).unwrap_err();
    assert_eq!(e.kind, RefusalKind::Invalid);
    assert_eq!(e.msg, "Quarry is running a shell script in shop. Stop it first, then drop.");
    assert!(opened(&mut core).is_none());
}

#[test]
fn a_dropped_table_closes_its_tabs_and_a_failed_drop_keeps_them() {
    let mut core = core_on(Engine::MySql, true, false);
    tab_on(&mut core, "shop", "orders");
    tab_on(&mut core, "shop", "items");
    let done = |res: crate::db::Res<String>| Msg::SqlDdl { conn: CONN, database: String::new(), schema: "shop".into(), table: "orders".into(), what: Ddl::DroppedTable, res, undo: None, dbs: None, sql_mode: None };
    core.inject(done(Err("Cannot drop".into())));
    assert_eq!(core.tabs.len(), 2, "a refused drop leaves the tabs");
    assert_eq!(core.errors.len(), 1);
    core.inject(done(Ok("Dropped orders".into())));
    assert_eq!(core.tabs.iter().map(|t| t.sel.coll.as_str()).collect::<Vec<_>>(), ["items"]);
    assert_eq!(core.status, "Dropped orders");
}

#[test]
fn a_created_table_opens_on_its_structure() {
    let mut core = core_on(Engine::MySql, true, false);
    core.inject(Msg::SqlDdl { conn: CONN, database: String::new(), schema: "shop".into(), table: "refunds".into(), what: Ddl::CreatedTable, res: Ok("Created refunds".into()), undo: None, dbs: None, sql_mode: None });
    let t = core.t().expect("a tab on the new table");
    assert_eq!((t.sel.db.as_str(), t.sel.coll.as_str(), t.view), ("shop", "refunds", View::Structure));
    // Without a session the sidebar's refresh says the connection is gone, as the menu's Refresh does.
    assert!(core.toasts.iter().any(|t| t.title.contains("Not connected")), "{:?}", core.toasts.iter().map(|t| &t.title).collect::<Vec<_>>());
}

#[test]
fn a_rename_moves_the_tabs_on_the_old_name() {
    let mut core = core_on(Engine::MySql, true, false);
    tab_on(&mut core, "shop", "orders");
    core.inject(Msg::SqlRenamed { conn: CONN, database: String::new(), schema: "shop".into(), from: "orders".into(), to: "orders_old".into(), whole: false, res: Ok("Renamed".into()), undo: None });
    assert_eq!(core.tabs[0].sel.coll, "orders_old");
    core.inject(Msg::SqlRenamed { conn: CONN, database: String::new(), schema: "shop".into(), from: "orders_old".into(), to: "x".into(), whole: false, res: Err("no".into()), undo: None });
    assert_eq!(core.tabs[0].sel.coll, "orders_old", "a refused rename moves nothing");
}

#[test]
fn a_failed_view_save_goes_back_to_its_dialog_while_it_waits() {
    let mut core = core_on(Engine::MySql, true, false);
    let saved = |c: &mut Core, res: crate::db::Res<String>, waiting| c.sql_view_saved(CONN, String::new(), "shop".into(), "v".into(), ViewDone::Created, res, None, None, waiting);
    assert_eq!(saved(&mut core, Err("bad SELECT".into()), true), Some("bad SELECT".into()));
    assert!(core.errors.is_empty(), "the dialog shows it, not a toast");
    assert_eq!(saved(&mut core, Err("bad SELECT".into()), false), None);
    assert_eq!(core.errors.len(), 1, "with the dialog gone it is an error");
    assert_eq!(saved(&mut core, Ok("Created v".into()), true), None);
    assert_eq!(core.t().map(|t| t.sel.coll.as_str()), Some("v"), "a new view opens");
}

/// The Content grid of `orders` on a MySQL connection, with key column id.
fn grid(writable: bool, session: bool) -> (Core, u64) {
    let mut core = core_on(Engine::MySql, writable, session);
    let uid = tab_on(&mut core, "shop", "orders");
    let s = core.tabs[0].sql.as_mut().unwrap();
    s.cols = std::sync::Arc::new(vec![crate::db::sql::Col::plain("id", "BIGINT", crate::db::sql::value::Kind::Int, false)]);
    s.pk = vec!["id".into()];
    (core, uid)
}

#[test]
fn bulk_asks_to_unlock_and_opens_with_its_count_requested() {
    let (mut core, uid) = grid(false, true);
    let bulk = |c: &mut Core| c.execute(Action::SqlOps(SqlOpsA::Bulk { tab: uid, update: true, col: Some("id".into()) }));
    assert_eq!(bulk(&mut core).unwrap_err().kind, RefusalKind::ReadOnly { conn: CONN });
    core.conns[0].writable = true;
    let before = core.req;
    bulk(&mut core).unwrap();
    match opened(&mut core) {
        Some(SqlForm::Bulk(f)) => {
            assert_eq!(f.req, before + 1, "the count is asked under the form's request id");
            assert!(f.update && f.prep.is_none());
            assert_eq!(f.pairs.iter().map(|p| p.col.as_str()).collect::<Vec<_>>(), ["id"]);
        }
        _ => panic!("the bulk dialog did not open"),
    }
    core.tabs[0].sql.as_mut().unwrap().matched = Some(3);
    let e = bulk(&mut core).unwrap_err();
    assert_eq!((e.kind, e.msg.as_str()), (RefusalKind::Invalid, "Grouped rows are summaries, not table rows. Ungroup to edit."));
    let e = core.execute(Action::SqlOps(SqlOpsA::Bulk { tab: uid + 1, update: false, col: None })).unwrap_err();
    assert_eq!(e.msg, "That tab is no longer open.");
}

/// The Explain dialog of a statement, as if its first request `req` were out.
fn explain(core: &mut Core, req: u64) -> SqlExplain {
    let mut x = SqlExplain::new("SELECT * FROM orders".into(), CONN, "shop".into(), "shop".into(), Dialect::MySql, dead_db(Dialect::MySql), "shop".into(), Err("no".into()));
    core.req = req;
    (x.req, x.busy) = (req, true);
    x
}

fn explained(x: &mut SqlExplain, req: u64, sets: Vec<ResultSet>) -> crate::action::sql::explain::Answered {
    x.explained(&mut Default::default(), req, sets)
}

fn plan_set(plan: &str) -> ResultSet {
    let col = crate::db::sql::Col::plain("EXPLAIN", "TEXT", crate::db::sql::value::Kind::Text, false);
    ResultSet { cols: vec![col], rows: crate::db::Row::from_doc(&mongodb::bson::doc! { "EXPLAIN": plan }).into_iter().collect(), ..ResultSet::new("EXPLAIN") }
}

#[test]
fn an_explain_answer_for_another_request_changes_nothing() {
    let mut core = core_on(Engine::MySql, true, false);
    let mut x = explain(&mut core, 5);
    assert!(explained(&mut x, 4, vec![plan_set("{}")]).keep);
    assert!(x.busy && x.raw.is_empty(), "a stale answer is dropped");
}

#[test]
fn a_first_explain_that_fails_closes_the_dialog_and_says_why_unless_stopped() {
    let mut core = core_on(Engine::MySql, true, false);
    let mut x = explain(&mut core, 5);
    let a = explained(&mut x, 5, vec![ResultSet { error: Some("syntax".into()), ..ResultSet::new("EXPLAIN") }]);
    assert!(!a.keep, "nothing to show");
    assert!(a.err.is_some());
    let mut x = explain(&mut core, 6);
    x.stopped = true;
    let a = explained(&mut x, 6, vec![ResultSet { error: Some("cancelled".into()), ..ResultSet::new("EXPLAIN") }]);
    assert!(!a.keep && a.err.is_none(), "after Stop the error is the cancel itself");
}

#[test]
fn an_explain_answer_fills_the_dialog_and_a_stale_catalog_answer_is_dropped() {
    let mut core = core_on(Engine::MySql, true, false);
    let mut x = explain(&mut core, 5);
    let plan = include_str!("../db/sql/plan/fixtures/pg18-fix-before.json");
    x.dialect = Dialect::Postgres;
    x.demo = true;
    assert!(explained(&mut x, 5, vec![plan_set(plan)]).keep);
    assert!(!x.busy && x.plan.is_some(), "the plan is read: {:?}", x.unread);
    x.ev_req = Some(9);
    let ev = x.ev.clone().expect("the demo's evidence");
    x.evidence_in(&mut Default::default(), 8, ev.clone());
    assert_eq!(x.ev_req, Some(9), "another request's catalog is dropped");
    x.evidence_in(&mut Default::default(), 9, ev);
    assert_eq!(x.ev_req, None);
}
