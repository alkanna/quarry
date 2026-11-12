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
