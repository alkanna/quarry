//! Shell fixes driven through the UI: a script typed in a Shell tab, Run, Stop, closing the tab, Disconnect in the rail, then
//! the result panel and the server checked. The runs go through a PostgreSQL tab (QUARRY_TEST_POSTGRES), whose queries come back
//! to the app's relay: a MongoDB run reads its address from the keychain, and the tests' keychain is one map shared by every test
//! in the process. The MongoDB runner fixes are covered in src/db/shell_fix_tests.rs instead.

use super::sql_edit::Fx;
use super::*;

const PG: &str = "QUARRY_TEST_POSTGRES";
/// The first run of a process installs the Shell's packages, which can take minutes in a cold folder.
const RUN_LIMIT: Duration = Duration::from_secs(300);

/// A UI on PostgreSQL with a fresh fixture table open in a Shell tab, unlocked when `unlock` is set. None without the server or Bun.
fn pg_shell(unlock: bool) -> Option<(Fx, UiApp, String)> {
    if db::bun_missing() {
        return None;
    }
    let mut fx = Fx::new(PG)?;
    let mut ui = ui_app(PG)?;
    let table = fx.table();
    fx.open(&mut ui, &table);
    if unlock {
        ui.unlock();
    }
    ui.quiet();
    let a = ui.h.state_mut();
    let c = a.cur;
    a.tabs[c].view = View::Shell;
    ui.settle();
    ui.show("Shell editor");
    Some((fx, ui, table))
}

fn shell(a: &App) -> &ShellTab {
    &a.t().expect("a tab").shell
}

/// Puts `code` in the current tab's editor and clicks Run, confirming the write dialog a writing script on an unlocked
/// connection opens.
fn run(ui: &mut UiApp, code: &str) {
    let a = ui.h.state_mut();
    let c = a.cur;
    a.tabs[c].shell.code = code.into();
    let before = shell(ui.app()).req;
    ui.click("Run");
    if matches!(ui.app().modal, Some(Modal::ShellWrite(_))) {
        ui.click("Execute");
    }
    assert_ne!(shell(ui.app()).req, before, "Run did not start the script; last error: {}", ui.app().last_problem());
}

/// Runs `code` to its end and returns the tab's Shell state.
fn run_done<'a>(ui: &'a mut UiApp, code: &str) -> &'a ShellTab {
    run(ui, code);
    ui.wait_for("the script to end", RUN_LIMIT, |a| a.t().is_some_and(|t| t.shell.started.is_none()));
    shell(ui.app())
}

/// The document a script returned, once it ended without an error.
fn result_doc(sh: &ShellTab) -> Document {
    assert!(sh.error.is_none(), "{:?}", sh.error);
    let v = sh.result().expect("a result");
    v.as_document().cloned().unwrap_or_else(|| panic!("{v}"))
}

/// A script that inserts a row into `table` through `handle` every 50 ms and never ends.
fn looping(table: &str, handle: &str, extra: &str) -> String {
    r#"async function run() {
  for (let i = 1000; ; i++) {
    await HANDLE`INSERT INTO TABLE (id, name, qty) VALUES (${i}, 'loop', 0)`;
    EXTRA
    await new Promise((r) => setTimeout(r, 50));
  }
}"#
    .replace("HANDLE", handle)
    .replace("TABLE", table)
    .replace("EXTRA", extra)
}

fn loop_rows(fx: &Fx, table: &str) -> i64 {
    fx.one(&format!("SELECT count(*) FROM {table} WHERE name = 'loop'")).parse().unwrap()
}

/// Waits until the looping script has written a few rows, which proves it runs before the test stops it.
fn wait_writing(ui: &mut UiApp, fx: &Fx, table: &str) {
    ui.wait_for("the looping script to write", RUN_LIMIT, |a| {
        let ended = a.tabs.iter().find(|x| x.sel.coll == table).and_then(|x| x.shell.error.clone());
        assert!(ended.is_none(), "the looping script ended: {ended:?}");
        loop_rows(fx, table) >= 3
    });
}

/// The writes stopped: once a statement in flight has landed, the count holds still for 500 ms, 10 turns of the loop.
fn assert_stopped(ui: &mut UiApp, fx: &Fx, table: &str) {
    ui.run_for(Duration::from_millis(300));
    let a = loop_rows(fx, table);
    ui.run_for(Duration::from_millis(500));
    assert_eq!(a, loop_rows(fx, table), "the script kept writing");
}

/// Disconnect from a connection's rail tile menu.
fn disconnect(ui: &mut UiApp, conn: u64, name: &str) {
    let label = format!("Switch to {name}");
    ui.show(&label);
    let at = ui.h.get_by_label(&label).rect().center();
    ui.press_at(at, egui::PointerButton::Secondary, 1);
    ui.settle();
    ui.click("Disconnect");
    ui.wait("the connection to close", |a| a.conn(conn).is_some_and(|c| c.status == Status::Idle));
}

#[test]
fn shell_fix_the_console_keeps_the_latest_lines_each_cut_short() {
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

#[test]
fn shell_fix_a_script_that_logs_many_lines_keeps_the_latest_in_the_console() {
    let Some((_fx, mut ui, _t)) = pg_shell(false) else { return };
    let sh = run_done(&mut ui, "async function run() {\n  for (let i = 0; i < 1600; i++) console.log('line ' + i);\n  console.log('y'.repeat(5000));\n  return 1;\n}");
    assert!(sh.error.is_none(), "{:?}", sh.error);
    assert!(sh.logs.len() <= 1500, "{} lines kept", sh.logs.len());
    assert_eq!(sh.logs[sh.logs.len() - 2].1, "line 1599");
    assert_eq!(sh.logs.last().unwrap().1.chars().count(), 2000);
}

#[test]
fn shell_fix_closing_a_tab_stops_its_running_script() {
    let Some((fx, mut ui, t)) = pg_shell(true) else { return };
    run(&mut ui, &looping(&t, "sql", ""));
    wait_writing(&mut ui, &fx, &t);
    let cross = format!("Close tab {}", ui.app().tab_name(ui.app().cur));
    ui.mouse_hold(&cross, 0);
    assert!(ui.app().tabs.iter().all(|x| x.sel.coll != t), "the cross did not close the tab");
    assert_stopped(&mut ui, &fx, &t);
}

#[test]
fn shell_fix_disconnect_stops_the_scripts_of_its_tabs() {
    let Some((fx, mut ui, t)) = pg_shell(true) else { return };
    run(&mut ui, &looping(&t, "sql", ""));
    wait_writing(&mut ui, &fx, &t);
    disconnect(&mut ui, CONN, "test");
    assert_stopped(&mut ui, &fx, &t);
    ui.wait("the run to end", |a| a.tabs.iter().all(|x| x.shell.started.is_none()));
}

#[test]
fn shell_fix_disconnect_stops_the_scripts_of_tabs_that_attached_it() {
    let Some((fx, mut ui, t)) = pg_shell(true) else { return };
    let uri = std::env::var(PG).unwrap();
    let a = ui.h.state_mut();
    a.conns.push(Conn { masked_uri: db::mask_uri(&uri), ..Conn::new(ConnMeta { id: 2, name: "other".into(), engine: db::sql::Engine::Postgres, ..Default::default() }, Status::Idle) });
    a.rail_order.push(2);
    ui.connect(2, &uri);
    ui.assert_ready(2);
    let a = ui.h.state_mut();
    let c = a.cur;
    a.tabs[c].shell.attached.push(crate::app::shell_attach::Attached { conn: 2, db: "quarry_test".into(), name: "other".into(), allow_writes: false, schema: "public".into(), tables: Vec::new() });
    // The writes go through the tab's own connection, which stays open, so only the stop on Disconnect can end them.
    run(&mut ui, &looping(&t, "sql", "await other.sql`SELECT 1`;"));
    wait_writing(&mut ui, &fx, &t);
    disconnect(&mut ui, 2, "other");
    assert_stopped(&mut ui, &fx, &t);
    ui.wait("the run to end", |a| a.t().is_some_and(|x| x.shell.started.is_none()));
    assert!(ui.app().conn(CONN).is_some_and(|c| c.status == Status::Ready), "the tab's own connection closed too");
}

#[test]
fn shell_fix_lone_surrogates_in_logs_and_results_keep_the_run() {
    let Some((_fx, mut ui, _t)) = pg_shell(false) else { return };
    // The replacement character the runner writes has no glyph in the app's fonts, which is expected here.
    ui.h.ctx.set_missing_glyph_policy(egui::MissingGlyphPolicy::Tofu);
    let sh = run_done(&mut ui, r#"async function run() {
  console.log('a\uD800b');
  return { s: 'x\uDC00y', ['k\uD800']: 1 };
}"#);
    assert!(sh.logs.iter().any(|l| l.1 == "a\u{FFFD}b"), "{:?}", sh.logs);
    let d = result_doc(sh);
    assert_eq!(d.get_str("s").ok(), Some("x\u{FFFD}y"), "{d}");
    assert!(d.contains_key("k\u{FFFD}"), "{d}");
}

#[test]
fn shell_fix_a_sql_value_holding_a_lone_surrogate_is_refused() {
    let Some((_fx, mut ui, _t)) = pg_shell(false) else { return };
    // Before, the query line was refused by the app and the script waited for its answer forever.
    let sh = run_done(&mut ui, "async function run() {\n  return await sql`SELECT ${'a\\uD800'}::text AS t`;\n}");
    let e = sh.error.clone().unwrap_or_default();
    assert!(e.contains("surrogate"), "{e}");
}

#[test]
fn shell_fix_sql_columns_named_like_extended_json_come_back_raw() {
    let Some((_fx, mut ui, _t)) = pg_shell(false) else { return };
    // One query per name: in one row the $binary check would fail first and leave $numberLong untried.
    let sh = run_done(&mut ui, r#"async function run() {
  const [a] = await sql`SELECT 'abc' AS "$numberLong"`;
  const [b] = await sql`SELECT 'xyz' AS "$binary"`;
  return { long: a['$numberLong'], bin: b['$binary'] };
}"#);
    let d = result_doc(sh);
    assert_eq!((d.get_str("long").ok(), d.get_str("bin").ok()), (Some("abc"), Some("xyz")), "{d}");
}

/// Disconnect drops the client and its SSH tunnel; a run must not go on to the stored address, which without the tunnel would
/// reach whatever answers on that local port.
#[test]
fn shell_fix_a_mongo_run_is_refused_after_disconnect() {
    if db::bun_missing() {
        return;
    }
    let Some(mut ui) = ui_app("QUARRY_TEST_MONGO") else { return };
    let sel = Sel { conn: CONN, db: "quarry_ui_shell_fix".into(), coll: "c".into(), ..Default::default() };
    ui.h.state_mut().open_collection(sel, Open::Here);
    ui.quiet();
    let a = ui.h.state_mut();
    let c = a.cur;
    a.tabs[c].view = View::Shell;
    a.tabs[c].shell.code = "async function run() { return 1; }".into();
    disconnect(&mut ui, CONN, "test");
    ui.click("Run");
    let sh = shell(ui.app());
    assert!(sh.req == 0 && sh.started.is_none() && sh.cancel.is_none(), "a run started");
    assert!(ui.said("Not connected"), "{}", ui.app().last_problem());
}
