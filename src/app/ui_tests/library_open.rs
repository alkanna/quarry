//! UI tests of opening and running library scripts: the connection and database a script lands on, what Run does on a
//! read-only or a writable connection, a script that waits for its connection, and the keys of the Library tree. They need
//! no server: the SQL ones use the MySQL demo, the linked ones a folder of their own under the temp directory.

use super::*;
use crate::app::library::open::How;
use crate::library::{Link, Script, ScriptRef, disk};
use std::path::{Path, PathBuf};

const CMD: egui::Modifiers = egui::Modifiers::COMMAND;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("quarry-open-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn harness() -> Harness<'static, App> {
    Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_accessibility_check(false).build_eframe(|cc| App::for_test(cc))
}

/// The MySQL demo with an empty Query tab on shop current.
fn sql_query(a: &mut App) {
    a.load_sql_demo();
    a.sql_new_query(3, "shop".into());
}

fn add(a: &mut App, kind: &str, conn: u64, name: &str, body: &str, db: &str, coll: &str) -> u64 {
    let id = a.library.next_id();
    a.library.scripts.push(Script { id, name: name.into(), kind: kind.into(), body: body.into(), conn, db: db.into(), coll: coll.into(), ..Default::default() });
    id
}

fn open(ui: &mut UiApp, id: u64, how: How) {
    ui.h.state_mut().open_script(&ScriptRef::Saved(id), how);
    ui.settle();
}

/// Whether the app said `part`: in the status line, where the offline demo's notes go, or in a toast.
fn said(a: &App, part: &str) -> bool {
    a.status.contains(part) || a.last_problem().contains(part)
}

fn sql_of(a: &App) -> &SqlTab {
    a.t().and_then(|t| t.sql.as_ref()).expect("a SQL tab")
}

/// A scan as the worker makes it: the files open in tabs are read in full when they changed.
fn rescan(a: &mut App, link: u64, dir: &Path) {
    let open: Vec<(PathBuf, disk::Meta)> = a.tabs.iter().filter_map(|t| t.script.as_ref()).filter_map(|b| match &b.r {
        ScriptRef::File { rel, .. } => Some((rel.clone(), b.disk)),
        _ => None,
    }).collect();
    a.apply_scan(link, disk::scan_reading(dir, &["sql".to_string()], None, &open));
}

#[test]
fn a_restored_tab_on_a_linked_file_keeps_its_edits_when_the_file_is_read() {
    // After a restart the tab holds what was typed, and nothing says what the file held when it was opened. The first scan
    // must not take the edits for a clean tab and reload the file over them, even on a link that follows changes.
    let dir = temp_dir("restored");
    std::fs::write(dir.join("a.sql"), "select 1").unwrap();
    let mut h = harness();
    let a = h.state_mut();
    a.conns.push(Conn::new(ConnMeta { id: 1, name: "shop".into(), engine: db::sql::Engine::MySql, ..Default::default() }, Status::Idle));
    a.library.links.push(Link { id: 1, path: dir.clone(), name: "queries".into(), on: "all".into(), exts: vec!["sql".into()], follow: true, ..Default::default() });
    let mut t = Tab::new(Sel { conn: 1, db: "shop".into(), schema: String::new(), coll: String::new() }).for_engine(db::sql::Engine::MySql);
    t.view = View::Sql;
    t.sql.as_mut().unwrap().query = "select 1 -- mine".into();
    t.script = Some(a.rebind(&t, ScriptRef::File { link: 1, rel: "a.sql".into() }).expect("bound again"));
    a.tabs.push(t);
    a.cur = 0;
    rescan(a, 1, &dir);
    let t = &a.tabs[0];
    assert_eq!(t.sql.as_ref().unwrap().query, "select 1 -- mine", "the edits were replaced by the file");
    assert_eq!(t.script.as_ref().unwrap().conflict.as_deref(), Some("select 1"), "the difference shows as a conflict");
    // A restored tab that holds what the file holds is simply clean.
    let a = h.state_mut();
    a.tabs[0].sql.as_mut().unwrap().query = "select 1".into();
    let b = a.rebind(&a.tabs[0], ScriptRef::File { link: 1, rel: "a.sql".into() }).unwrap();
    a.tabs[0].script = Some(b);
    rescan(a, 1, &dir);
    assert!(a.tabs[0].script.as_ref().unwrap().conflict.is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_reused_query_tab_runs_the_script_in_its_own_database() {
    let Some(mut ui) = demo_ui(sql_query) else { return };
    let tabs = ui.app().tabs.len();
    {
        let a = ui.h.state_mut();
        let c = a.cur;
        a.tabs[c].sql.as_mut().unwrap().run_db = "analytics".into();
    }
    let id = add(ui.h.state_mut(), "sql", 3, "Late", "SELECT 1", "shop", "");
    open(&mut ui, id, How::Here);
    assert_eq!(ui.app().tabs.len(), tabs, "the empty tab is reused");
    let s = sql_of(ui.app());
    assert!(s.run_db.is_empty() || s.run_db == "shop", "the script runs in {:?}, not shop", s.run_db);
}

#[test]
fn cmd_s_writes_back_the_database_the_tab_runs_in() {
    let Some(mut ui) = demo_ui(sql_query) else { return };
    let id = add(ui.h.state_mut(), "sql", 3, "Late", "SELECT 1", "shop", "");
    open(&mut ui, id, How::Here);
    {
        let a = ui.h.state_mut();
        let c = a.cur;
        let s = a.tabs[c].sql.as_mut().unwrap();
        (s.query, s.run_db) = ("SELECT 2".into(), "analytics".into());
    }
    ui.key(CMD, egui::Key::S);
    let s = ui.app().library.scripts.iter().find(|s| s.id == id).unwrap();
    assert_eq!((s.body.as_str(), s.db.as_str()), ("SELECT 2", "analytics"));
}

#[test]
fn a_script_of_another_engine_is_refused_without_a_half_built_tab() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    let tabs = ui.app().tabs.len();
    let id = add(ui.h.state_mut(), "sql", 1, "Wrong engine", "SELECT 1", "", "");
    open(&mut ui, id, How::Here);
    assert_eq!(ui.app().tabs.len(), tabs, "a SQL script opened a tab on a MongoDB connection");
    assert!(ui.app().last_problem().contains("Not opened"), "{}", ui.app().last_problem());

    let Some(mut ui) = demo_ui(sql_query) else { return };
    let tabs = ui.app().tabs.len();
    let id = add(ui.h.state_mut(), "filter", 3, "Mongo filter", "{ a: 1 }", "shop", "orders");
    open(&mut ui, id, How::Here);
    assert_eq!(ui.app().tabs.len(), tabs, "a filter opened a tab on a MySQL connection");
    assert!(ui.app().last_problem().contains("Not opened"), "{}", ui.app().last_problem());
}

#[test]
fn a_migrated_filter_with_no_collection_says_so_and_opens_nothing() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    {
        let a = ui.h.state_mut();
        a.prefs.library.insert("1/shop".into(), vec![SavedQuery { name: "Loose".into(), filter: "{ a: 1 }".into(), ..Default::default() }]);
        a.migrate_library();
    }
    let s = ui.app().library.scripts.iter().find(|s| s.name == "Loose").cloned().expect("migrated");
    assert_eq!((s.kind.as_str(), s.db.as_str(), s.coll.as_str()), ("filter", "shop", ""));
    let tabs = ui.app().tabs.len();
    let sel = ui.app().t().map(|t| t.sel.clone());
    open(&mut ui, s.id, How::Here);
    assert_eq!(ui.app().tabs.len(), tabs);
    assert!(ui.app().t().map(|t| t.sel.clone()) == sel, "the current tab moved");
    assert!(ui.app().last_problem().contains("names no collection"), "{}", ui.app().last_problem());
}

#[test]
fn cmd_enter_in_the_editor_runs_the_query_after_a_click_in_the_library() {
    let Some(mut ui) = demo_ui(|a| {
        sql_query(a);
        a.demo_library("panel");
        a.lib_ui.panel.sel = None;
    }) else { return };
    {
        let a = ui.h.state_mut();
        let c = a.cur;
        a.tabs[c].sql.as_mut().unwrap().query = "SELECT 1".into();
    }
    ui.click("Top customers");
    assert!(ui.app().lib_ui.panel.focus);
    // The keyboard moves to the editor without a click, as Tab or a shortcut moves it.
    let id = App::sql_editor_id(ui.app().cur);
    ui.h.ctx.memory_mut(|m| m.request_focus(id));
    ui.settle();
    let before = ui.app().tabs.len();
    ui.key(CMD, egui::Key::Enter);
    assert_eq!(ui.app().tabs.len(), before, "the library script ran instead of the editor's query");
    assert!(said(ui.app(), "Offline demo"), "the editor's query did not run: {}", ui.app().last_problem());
}

#[test]
fn return_alt_return_and_cmd_enter_act_on_the_selected_script() {
    let Some(mut ui) = demo_ui(|a| {
        sql_query(a);
        a.demo_library("panel");
        a.lib_ui.panel.sel = None;
    }) else { return };
    let tabs = ui.app().tabs.len();
    // A script opened in a tab gives the tab its name, so the rows are picked as a click on one would pick them.
    let pick = |ui: &mut UiApp, id: u64| {
        let st = &mut ui.h.state_mut().lib_ui.panel;
        (st.sel, st.focus) = (Some(crate::library::tree::Node::Script(id)), true);
        ui.settle();
    };
    pick(&mut ui, 11);
    ui.key(egui::Modifiers::NONE, egui::Key::Enter);
    assert_eq!(ui.app().tabs.len(), tabs, "Return opens in the empty tab");
    assert!(sql_of(ui.app()).query.starts_with("SELECT customer_id"));
    pick(&mut ui, 11);
    ui.key(egui::Modifiers::ALT, egui::Key::Enter);
    assert_eq!(ui.app().tabs.len(), tabs + 1, "Alt+Return opens a new tab");
    // Monthly revenue only reads, so Run all runs it at once.
    pick(&mut ui, 10);
    ui.key(CMD, egui::Key::Enter);
    assert!(sql_of(ui.app()).query.contains("revenue per month"));
    assert!(said(ui.app(), "Offline demo"), "Cmd+Enter did not run it: {}", ui.app().last_problem());
}

/// Steps frames until the linked-file reads and writes on the worker are back.
fn io(h: &mut Harness<'static, App>) {
    let t = std::time::Instant::now();
    while !h.state().lib_ui.jobs.is_empty() {
        assert!(t.elapsed() < Duration::from_secs(20), "a linked-file job never came back");
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    h.step();
}

fn conn(id: u64, name: &str, engine: db::sql::Engine, status: Status) -> Conn {
    Conn::new(ConnMeta { id, name: name.into(), engine, ..Default::default() }, status)
}

fn writable(ui: &mut UiApp, id: u64, on: bool) {
    if let Some(c) = ui.h.state_mut().conns.iter_mut().find(|c| c.meta.id == id) {
        c.writable = on;
    }
}

#[test]
fn a_linked_file_runs_on_the_tab_or_active_connection_and_otherwise_asks() {
    let dir = temp_dir("target");
    std::fs::write(dir.join("a.sql"), "select 1").unwrap();
    let mut ui = UiApp { h: harness() };
    let a = ui.h.state_mut();
    use db::sql::Engine::{Mongo, MySql};
    a.conns.extend([conn(1, "local", Mongo, Status::Ready), conn(2, "staging", MySql, Status::Ready), conn(3, "prod", MySql, Status::Ready)]);
    a.library.links.push(Link { id: 1, path: dir.clone(), name: "queries".into(), on: "all".into(), exts: vec!["sql".into()], ..Default::default() });
    a.tabs.push(Tab::new(Sel { conn: 1, db: "shop".into(), schema: String::new(), coll: "orders".into() }));
    a.cur = 0;
    let file = ScriptRef::File { link: 1, rel: "a.sql".into() };
    use crate::app::library::open::Target;
    // The tab is on MongoDB and nothing else is active: neither MySQL connection is picked for the person.
    a.active = None;
    assert_eq!(a.script_target(&file), Target::Ask(vec![2, 3]));
    a.active = Some(2);
    assert_eq!(a.script_target(&file), Target::On(2));
    a.tabs.push(Tab::new(Sel { conn: 3, db: "shop".into(), schema: String::new(), coll: "orders".into() }).for_engine(MySql));
    a.cur = 1;
    assert_eq!(a.script_target(&file), Target::On(3), "the current tab's connection comes before the active one");
    // Asked: the chooser lists both, nothing is opened until one is picked.
    let a = ui.h.state_mut();
    (a.cur, a.active) = (0, None);
    let tabs = a.tabs.len();
    a.open_script(&file, How::Here);
    ui.h.run_steps(3);
    assert!(matches!(&ui.app().modal, Some(Modal::OpenOn(f)) if f.conns == [2, 3] && f.sel.is_none()));
    ui.click("staging");
    ui.click_last("Open");
    io(&mut ui.h);
    let a = ui.app();
    assert_eq!(a.tabs.len(), tabs + 1);
    assert_eq!((a.t().unwrap().sel.conn, sql_of(a).query.as_str()), (2, "select 1"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_script_with_no_database_opens_in_the_connections_default_not_the_first_listed() {
    let Some(ui) = demo_ui(App::load_sql_demo) else { return };
    // The demo lists information_schema and mysql before shop.
    assert_eq!(ui.app().conn(3).unwrap().dbs[0].0, "information_schema");
    assert_eq!(ui.app().default_db(3), "shop");
    use crate::app::library::open::uri_db;
    assert_eq!(uri_db("mysql://app:****@shop.internal:3306/analytics?ssl-mode=REQUIRED"), "analytics");
    assert_eq!(uri_db("mongodb+srv://u:p@cluster0.x.net/?retryWrites=true"), "");
    assert_eq!(uri_db("postgres://u:a/b@h:5432/app"), "app");
    assert_eq!(uri_db("mysql://h:3306"), "");
}

#[test]
fn a_removed_connections_scripts_never_pass_to_a_new_connection() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    let a = ui.h.state_mut();
    a.conns.push(conn(9, "old-prod", db::sql::Engine::MySql, Status::Idle));
    let id = add(a, "sql", 9, "Old report", "SELECT 1", "", "");
    a.danger(Danger::RemoveConnection(9));
    assert!(a.conn(9).is_none());
    let next = a.next_conn_id();
    assert!(next > 9, "a new connection would take id {next} and adopt the removed one's scripts");
    // The script stays, and opening it asks which connection to use instead of taking one.
    use crate::app::library::open::Target;
    assert!(matches!(a.script_target(&ScriptRef::Saved(id)), Target::Ask(c) if c == [3]));
}

#[test]
fn run_on_a_read_only_connection_goes_through_the_read_only_gates() {
    let Some(mut ui) = demo_ui(sql_query) else { return };
    writable(&mut ui, 3, false);
    let id = add(ui.h.state_mut(), "sql", 3, "Stale carts", "DELETE FROM carts WHERE id = 1", "shop", "");
    open(&mut ui, id, How::Run);
    // It runs, as a read-only run: the server refuses the write. Nothing is held back for a notice.
    assert!(said(ui.app(), "Offline demo"), "{}", ui.app().last_problem());
    assert!(ui.app().lib_ui.writes.is_empty());

    // A pipeline with a write stage stops at the read-only gate.
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    writable(&mut ui, 1, false);
    let id = add(ui.h.state_mut(), "pipeline", 1, "Copy out", "[{ $match: {} }, { $out: 'copy' }]", "shop", "orders");
    open(&mut ui, id, How::Run);
    assert!(ui.app().last_problem().contains("is read-only"), "{}", ui.app().last_problem());
    assert!(!matches!(ui.app().modal, Some(Modal::AggWrite(_))) && ui.app().lib_ui.writes.is_empty());

    // A Shell script runs read-only, without the write confirmation.
    let id = add(ui.h.state_mut(), "shell", 1, "Clean", "async function run() {\n  return await orders.deleteMany({ a: 1 });\n}", "shop", "");
    open(&mut ui, id, How::Run);
    assert!(!matches!(ui.app().modal, Some(Modal::ShellWrite(_))));
    assert!(said(ui.app(), "the shell needs a real connection"), "{}", ui.app().last_problem());
}

#[test]
fn run_on_a_writable_connection_opens_a_writing_script_with_a_notice() {
    let Some(mut ui) = demo_ui(sql_query) else { return };
    let id = add(ui.h.state_mut(), "sql", 3, "Stale carts", "SELECT 1;\nDELETE FROM carts WHERE id = 1", "shop", "");
    ui.h.state_mut().status.clear();
    open(&mut ui, id, How::Run);
    assert!(sql_of(ui.app()).query.contains("DELETE FROM carts"));
    assert!(!said(ui.app(), "Offline demo"), "it ran: {}", ui.app().last_problem());
    assert!(ui.app().lib_ui.writes.contains(&ui.app().t().unwrap().uid));
    ui.show("This script writes");
    ui.click_nth("Run all", 0);
    assert!(said(ui.app(), "Offline demo"), "Run all in the notice did not run it: {}", ui.app().last_problem());
    assert!(ui.app().lib_ui.writes.is_empty());

    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    let id = add(ui.h.state_mut(), "pipeline", 1, "Copy out", "[{ $match: {} }, { $out: 'copy' }]", "shop", "orders");
    open(&mut ui, id, How::Run);
    assert!(ui.app().t().is_some_and(|t| t.view == View::Aggregate && t.agg_txt.contains("$out")));
    assert!(ui.app().lib_ui.writes.contains(&ui.app().t().unwrap().uid));
    // Run all goes to the Aggregate gate, which needs the server the demo does not have.
    ui.click("Run all");
    assert!(ui.app().last_problem().contains("Not connected"), "{}", ui.app().last_problem());

    let id = add(ui.h.state_mut(), "shell", 1, "Clean", "async function run() {\n  return await orders.deleteMany({ a: 1 });\n}", "shop", "");
    open(&mut ui, id, How::Run);
    assert!(ui.app().t().is_some_and(|t| t.view == View::Shell && t.shell.code.contains("deleteMany")));
    assert!(ui.app().modal.is_none(), "the Shell asked before the person ran anything");
    ui.click("Run all");
    assert!(matches!(ui.app().modal, Some(Modal::ShellWrite(_))), "Run all skipped the Shell's write confirmation");
}

#[test]
fn opening_a_pipeline_loads_it_and_only_run_runs_it() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    let id = add(ui.h.state_mut(), "pipeline", 1, "Totals", "[{ $group: { _id: '$status', n: { $sum: 1 } } }]", "shop", "orders");
    ui.h.state_mut().status.clear();
    open(&mut ui, id, How::Here);
    assert!(ui.app().t().is_some_and(|t| t.view == View::Aggregate && t.agg_txt.contains("$group")));
    assert!(!said(ui.app(), "nothing to run"), "Open ran the pipeline: {}", ui.app().last_problem());
    open(&mut ui, id, How::Run);
    assert!(said(ui.app(), "nothing to run"), "Run did not run it: {}", ui.app().last_problem());
}

/// An app with no demo flag, where a script on a connection that is not ready waits for it.
fn waiting(status: Status) -> (UiApp, u64) {
    let mut ui = UiApp { h: harness() };
    let a = ui.h.state_mut();
    a.conns.push(conn(1, "shop-prod", db::sql::Engine::MySql, status));
    let id = add(a, "sql", 1, "Monthly revenue", "SELECT 1", "shop", "");
    a.open_script(&ScriptRef::Saved(id), How::Here);
    ui.h.run_steps(3);
    (ui, id)
}

fn set_status(ui: &mut UiApp, status: Status, attempt: u64) {
    let c = ui.h.state_mut().conns.iter_mut().find(|c| c.meta.id == 1).unwrap();
    (c.status, c.attempt) = (status, attempt);
    ui.h.run_steps(3);
}

fn count_toasts(a: &App, part: &str) -> usize {
    a.last_problem().matches(part).count()
}

#[test]
fn a_script_waits_for_its_connection_and_says_so() {
    let (mut ui, _) = waiting(Status::Idle);
    assert!(ui.app().lib_ui.pending.is_some() && ui.app().tabs.is_empty());
    assert!(ui.app().last_problem().contains("Opening Monthly revenue when shop-prod connects"), "{}", ui.app().last_problem());
    set_status(&mut ui, Status::Connecting, 1);
    set_status(&mut ui, Status::Ready, 1);
    assert!(ui.app().lib_ui.pending.is_none());
    assert_eq!(ui.app().t().and_then(|t| t.sql.as_ref()).map(|s| s.query.as_str()), Some("SELECT 1"));
}

#[test]
fn a_connection_that_never_starts_connecting_drops_the_wait() {
    let (mut ui, _) = waiting(Status::Idle);
    assert!(ui.app().lib_ui.pending.is_some());
    // Still idle after the time limit: the address could not be read, say, so no connect is coming.
    if let Some(p) = ui.h.state_mut().lib_ui.pending.as_mut() {
        p.since = Some(-1000.0);
    }
    ui.h.run_steps(3);
    assert!(ui.app().lib_ui.pending.is_none());
    assert!(ui.app().last_problem().contains("did not connect"), "{}", ui.app().last_problem());
    // A later connect opens nothing.
    set_status(&mut ui, Status::Ready, 1);
    assert!(ui.app().tabs.is_empty());
}

#[test]
fn a_failed_connection_is_tried_again_and_warns_once_when_it_fails_again() {
    let (mut ui, _) = waiting(Status::Failed("refused".into()));
    // The old failure is not news: the script waits for the new attempt.
    assert!(ui.app().lib_ui.pending.is_some(), "it gave up on the old failure: {}", ui.app().last_problem());
    set_status(&mut ui, Status::Connecting, 1);
    set_status(&mut ui, Status::Failed("refused".into()), 1);
    assert!(ui.app().lib_ui.pending.is_none());
    assert_eq!(count_toasts(ui.app(), "failed to connect"), 1, "{}", ui.app().last_problem());

    let (mut ui, _) = waiting(Status::Failed("refused".into()));
    set_status(&mut ui, Status::Connecting, 1);
    set_status(&mut ui, Status::Ready, 1);
    assert_eq!(ui.app().t().and_then(|t| t.sql.as_ref()).map(|s| s.query.as_str()), Some("SELECT 1"));
}

#[test]
fn a_waiting_run_only_opens_when_another_tab_became_current() {
    let mut ui = UiApp { h: harness() };
    let a = ui.h.state_mut();
    a.conns.push(conn(1, "shop-prod", db::sql::Engine::MySql, Status::Idle));
    a.tabs.push(Tab::new(Sel { conn: 1, db: "shop".into(), schema: String::new(), coll: "orders".into() }).for_engine(db::sql::Engine::MySql));
    a.cur = 0;
    let id = add(a, "sql", 1, "Monthly revenue", "SELECT 1", "shop", "");
    a.open_script(&ScriptRef::Saved(id), How::Run);
    ui.h.run_steps(2);
    // The person moved on to another tab while it connected.
    let a = ui.h.state_mut();
    a.tabs.push(Tab::new(Sel { conn: 1, db: "shop".into(), schema: String::new(), coll: "users".into() }).for_engine(db::sql::Engine::MySql));
    a.cur = 1;
    set_status(&mut ui, Status::Ready, 1);
    assert_eq!(ui.app().t().and_then(|t| t.sql.as_ref()).map(|s| s.query.as_str()), Some("SELECT 1"));
    assert!(!ui.app().last_problem().contains("Connect first"), "the Run went ahead: {}", ui.app().last_problem());
    // Cancel on the toast drops a wait.
    let (mut ui, _) = waiting(Status::Idle);
    ui.click("Cancel");
    assert!(ui.app().lib_ui.pending.is_none());
}

#[test]
fn the_conflict_banner_shows_on_a_query_tab_and_reload_can_be_undone() {
    let dir = temp_dir("banner");
    std::fs::write(dir.join("a.sql"), "select 1").unwrap();
    let mut ui = UiApp { h: harness() };
    let a = ui.h.state_mut();
    a.conns.push(conn(1, "shop", db::sql::Engine::MySql, Status::Ready));
    a.library.links.push(Link { id: 1, path: dir.clone(), name: "queries".into(), on: "all".into(), exts: vec!["sql".into()], ..Default::default() });
    a.lib_ui.frozen = true;
    let mut t = Tab::new(Sel { conn: 1, db: "shop".into(), schema: String::new(), coll: String::new() }).for_engine(db::sql::Engine::MySql);
    t.view = View::Sql;
    t.sql.as_mut().unwrap().query = "select 1 -- mine".into();
    t.script = Some(Bound { r: ScriptRef::File { link: 1, rel: "a.sql".into() }, view: View::Sql, base: "select 1".into(), disk: None, conflict: Some("select 2".into()) });
    a.tabs.push(t);
    a.cur = 0;
    std::fs::write(dir.join("a.sql"), "select 2").unwrap();
    ui.show("a.sql changed on disk");
    ui.click("Reload");
    io(&mut ui.h);
    assert_eq!(sql_of(ui.app()).query, "select 2");
    ui.click_last("Undo");
    assert_eq!(sql_of(ui.app()).query, "select 1 -- mine", "Undo did not bring the edits back");
    std::fs::remove_dir_all(&dir).unwrap();
}

fn panel(a: &mut App) {
    sql_query(a);
    a.demo_library("panel");
    a.lib_ui.panel.sel = None;
}

#[test]
fn the_library_menus_are_as_narrow_as_the_apps_other_menus() {
    let Some(mut ui) = demo_ui(panel) else { return };
    let max = crate::app::library::panel::MENU_MAX_W + 2.0;
    ui.click("Library menu");
    for item in ["Link a folder\u{2026}", "Fold all", "Show all connections"] {
        ui.show(item);
        let w = ui.h.get_by_label(item).rect().width();
        assert!(w <= max, "{item} is {w} px wide");
    }
    ui.key(egui::Modifiers::NONE, egui::Key::Escape);
    // The row menu, with its shortcuts on the right of each item, keeps the same width.
    ui.show("Top customers");
    let at = ui.h.get_by_label("Top customers").rect().center();
    ui.h.hover_at(at);
    ui.h.run_steps(2);
    ui.click("More for Top customers");
    for item in ["Open", "Open in a new tab", "Run all", "Duplicate"] {
        ui.show(item);
        let w = ui.h.get_by_label(item).rect().width();
        assert!(w <= max && w >= 120.0, "{item} is {w} px wide");
    }
}

#[test]
fn a_script_opens_on_another_connection_from_its_menu() {
    let Some(mut ui) = demo_ui(|a| {
        panel(a);
        a.conns.push(Conn { writable: true, ..conn(4, "shop-staging", db::sql::Engine::MySql, Status::Ready) });
    }) else { return };
    let tabs = ui.app().tabs.len();
    ui.show("Top customers");
    let at = ui.h.get_by_label("Top customers").rect().center();
    ui.h.hover_at(at);
    ui.h.run_steps(2);
    ui.click("More for Top customers");
    ui.click_contains("Open on");
    ui.click("shop-staging");
    let a = ui.app();
    assert!(a.tabs.len() > tabs || a.t().is_some_and(|t| t.sel.conn == 4));
    assert_eq!(a.t().map(|t| t.sel.conn), Some(4));
    assert!(sql_of(a).query.starts_with("SELECT customer_id"));
    // The script keeps its own connection.
    assert_eq!(a.library.scripts.iter().find(|s| s.id == 11).map(|s| s.conn), Some(3));
}
