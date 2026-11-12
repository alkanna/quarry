//! The PostgreSQL Server window's Activity tab on PostgreSQL 18 and 13: unlock from the window's note, select a sleeping session
//! and cancel its query through the two-step confirm, then check on the session itself that its statement was cancelled.

use super::*;
use std::sync::mpsc;

/// A `pg_sleep` on a connection of its own, tagged so the test finds it among other tests' sessions; sends back how it ended.
fn sleeper(uri: &str, tag: &str) -> mpsc::Receiver<Result<(), String>> {
    let (uri, sql) = (uri.to_string(), format!("SELECT pg_sleep(30) /* {tag} */"));
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            use sqlx::Connection;
            let mut c = sqlx::PgConnection::connect(&uri).await.unwrap();
            let r = sqlx::Executor::execute(&mut c, sql.as_str()).await.map(|_| ()).map_err(|x| x.to_string());
            let _ = tx.send(r);
        });
    });
    rx
}

/// The pid of the session whose query carries `tag`, once the window lists it as running.
fn listed(a: &App, tag: &str) -> Option<u64> {
    a.pg_server.as_ref()?.listed(tag)
}

#[test]
fn activity_cancels_a_sleeping_session_after_unlock_and_a_second_click() {
    for var in ["QUARRY_TEST_POSTGRES", "QUARRY_TEST_POSTGRES13"] {
        let Some(mut ui) = ui_app(var) else { continue };
        let uri = std::env::var(var).unwrap();
        let tag = format!("ui_srv_{}", std::process::id());
        let rx = sleeper(&uri, &tag);
        ui.h.state_mut().open_server(CONN);
        ui.click("Activity");
        ui.wait("the sleeping session to be listed", |a| listed(a, &tag).is_some());
        let pid = listed(ui.app(), &tag).unwrap();
        // The connection starts read-only: the window's note unlocks it.
        ui.click("Unlock…");
        ui.click("Allow writes");
        ui.wait("writes to be allowed", |a| a.conn(CONN).is_some_and(|c| c.writable));
        ui.click(&format!("Session {pid}"));
        ui.click("Cancel query");
        // The first click only arms it: the statement still runs and the toolbar asks again.
        ui.show(&format!("Cancel the query of {pid}?"));
        assert!(rx.try_recv().is_err(), "nothing is sent on the first click");
        ui.click("Cancel query");
        let t = std::time::Instant::now();
        let out = loop {
            ui.h.step();
            if let Ok(r) = rx.try_recv() {
                break r;
            }
            assert!(t.elapsed() < LIMIT, "the statement was not cancelled; last error: {}", ui.app().last_problem());
            std::thread::sleep(Duration::from_millis(20));
        };
        let err = out.unwrap_err();
        assert!(err.contains("57014") || err.contains("canceling statement"), "{err}");
    }
}
