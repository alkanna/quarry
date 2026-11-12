//! The PostgreSQL Roles and privileges window driven like a user: open it, unlock from its banner, pick the schema, grant SELECT
//! to a role through a cell's editor, Apply, check the server, then undo from Recent changes and check the grant is gone. Run
//! against the PostgreSQL servers and the PgBouncer, whichever environment variables are set.

use super::*;
use crate::db::sql::testing::{unique_name, PGBOUNCER_ENV, PG_ENVS};

fn sql(uri: &str, s: &str) -> String {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let pool = sqlx::PgPool::connect(uri).await.unwrap();
        let rows = sqlx::raw_sql(s).fetch_all(&pool).await.unwrap_or_else(|x| panic!("{s}: {x}"));
        pool.close().await;
        rows.first().and_then(|r| sqlx::Row::try_get::<String, _>(r, 0).ok()).unwrap_or_default()
    })
}

fn loaded(a: &App) -> bool {
    a.pg_roles.as_ref().is_some_and(|w| matches!(w.cluster, Some(Ok(_))) && matches!(w.objs, Some(Ok(_))) && !w.busy)
}

fn run(var: &str) {
    let Some(mut ui) = ui_app(var) else { return };
    let uri = std::env::var(var).unwrap();
    let (role, schema) = (unique_name("qr_ui"), unique_name("qr_ui_s"));
    sql(&uri, &format!("CREATE ROLE \"{role}\"; CREATE SCHEMA \"{schema}\"; CREATE TABLE \"{schema}\".t (a int)"));
    let can = |uri: &str| sql(uri, &format!("SELECT has_table_privilege('{role}', '\"{schema}\".t', 'SELECT')::text"));

    ui.h.state_mut().open_sql_users(CONN);
    ui.wait("the roles and the tables", loaded);
    ui.click("Schema picker");
    ui.click(&schema);
    ui.wait("the tables of the schema", |a| loaded(a) && a.pg_roles.as_ref().is_some_and(|w| w.objs.as_ref().is_some_and(|o| o.as_ref().is_ok_and(|o| o.schema == schema))));

    // Locked: the editor opens but nothing can be staged; the banner offers Unlock.
    // Other tests make roles on the same server, so the column is found through the filter rather than scrolled to.
    ui.fill("Filter roles", &role);
    ui.click(&format!("{role} on t"));
    ui.click("Granted SELECT");
    assert!(ui.app().pg_roles.as_ref().unwrap().staged.is_empty(), "{var}: a locked connection staged a grant");
    ui.click_last("Unlock…");
    ui.click("Allow writes");
    ui.wait("writes to be allowed", |a| a.conn(CONN).is_some_and(|c| c.writable));

    if ui.app().pg_roles.as_ref().unwrap().cell.is_none() {
        ui.click(&format!("{role} on t"));
    }
    ui.click("Granted SELECT");
    ui.wait("the grant to be staged", |a| a.pg_roles.as_ref().is_some_and(|w| w.staged.len() == 1));
    assert_eq!(can(&uri), "false", "{var}: nothing runs before Apply");
    ui.click_last("Apply");
    ui.wait("the apply to finish", |a| a.pg_roles.as_ref().is_some_and(|w| w.staged.is_empty()) && loaded(a));
    assert_eq!(can(&uri), "true", "{var}: {}", ui.app().last_problem());

    ui.click_last("Close");
    ui.wait("the window to close", |a| a.pg_roles.is_none());
    ui.click("Undo (1)");
    ui.click("Undo change 1");
    ui.click("Close");
    let t = std::time::Instant::now();
    while can(&uri) != "false" {
        assert!(t.elapsed() < LIMIT, "{var}: the undo never landed; {}", ui.app().last_problem());
        ui.h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    sql(&uri, &format!("DROP SCHEMA \"{schema}\" CASCADE; DROP ROLE \"{role}\""));
}

#[test]
fn pg_roles_grant_through_the_cell_editor_apply_and_undo() {
    for var in PG_ENVS.into_iter().chain([PGBOUNCER_ENV]) {
        run(var);
    }
}

/// Esc closes the Change password dialog without its buttons: the typed password and egui's state of its fields, the undo
/// history of their text included, must go all the same. Needs no server.
#[test]
fn pg_password_is_forgotten_when_esc_closes_the_dialog() {
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1200.0, 800.0)).build_eframe(|cc| App::for_test(cc));
    let serial = 77;
    h.state_mut().modal = Some(Modal::PgPassword(Box::new(crate::app::pg_roles::PgPwForm::for_test(CONN, "jane", serial))));
    h.run_steps(3);
    h.event(egui::Event::Text("Zq9secret".into()));
    h.run_steps(2);
    let id = crate::app::sql_users::secret_id("pg-password", serial, 0);
    assert!(matches!(&h.state().modal, Some(Modal::PgPassword(f)) if f.typed() == "Zq9secret"), "the field took the typing");
    assert!(egui::TextEdit::load_state(&h.ctx, id).is_some(), "egui keeps the field's state while it is open");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.state().modal.is_none(), "Esc closed the dialog");
    assert!(egui::TextEdit::load_state(&h.ctx, id).is_none(), "the field's state, with its undo history, is gone");
}

/// An Apply answer that is not the one the window waits for (from before Esc closed and reopened it) leaves what is staged now.
#[test]
fn pg_a_late_apply_answer_keeps_newer_staged_changes() {
    use crate::db::sql::pg::roles::{Answer, Change, Did};
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(800.0, 600.0)).build_eframe(|cc| App::for_test(cc));
    let app = h.state_mut();
    let mut w = crate::app::pg_roles::PgRolesWin::new(CONN, "shop".into());
    w.staged.push(Change::Member { group: "g".into(), member: "m".into(), from: None, to: Some(Default::default()) });
    (w.busy, w.req_apply) = (true, 7);
    app.pg_roles = Some(w);
    let done = || Answer::Done { did: Did::Applied, res: Ok("Applied".into()), undo: None };
    app.pg_roles_answer(CONN, 3, done());
    assert_eq!(app.pg_roles.as_ref().unwrap().staged.len(), 1, "a stale answer cleared the staged change");
    app.pg_roles_answer(CONN, 7, done());
    assert!(app.pg_roles.as_ref().unwrap().staged.is_empty() && !app.pg_roles.as_ref().unwrap().busy);
}
