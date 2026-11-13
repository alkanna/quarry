//! The PostgreSQL Server window's Activity tab on PostgreSQL 18 and 13: unlock from the window's note, select a sleeping session
//! and cancel its query through the two-step confirm, then check on the session itself that its statement was cancelled.

use super::*;
use std::sync::mpsc;

/// A `pg_sleep` on a connection of its own, tagged so the test finds it among other tests' sessions; sends back how it ended.
/// It sleeps far past the wait limit, so a slow run cannot see it finish before it is cancelled, and a connection the
/// server refuses (every slot taken by the tests running beside it) is retried, then reported instead of lost.
fn sleeper(uri: &str, tag: &str) -> mpsc::Receiver<Result<(), String>> {
    let (uri, sql) = (uri.to_string(), format!("SELECT pg_sleep(120) /* {tag} */"));
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            use sqlx::Connection;
            let t = std::time::Instant::now();
            let mut c = loop {
                match sqlx::PgConnection::connect(&uri).await {
                    Ok(c) => break c,
                    Err(e) if t.elapsed() > Duration::from_secs(20) => return drop(tx.send(Err(format!("the sleeping session could not connect: {e}")))),
                    Err(_) => tokio::time::sleep(Duration::from_millis(250)).await,
                }
            };
            let r = sqlx::Executor::execute(&mut c, sql.as_str()).await.map(|_| ()).map_err(|x| x.to_string());
            let _ = tx.send(r);
        });
    });
    rx
}

/// Waits for the window to list the session tagged `tag`, failing at once with the reason when the session never started.
fn wait_listed(ui: &mut UiApp, rx: &mpsc::Receiver<Result<(), String>>, tag: &str) -> u64 {
    let t = std::time::Instant::now();
    loop {
        ui.h.step();
        if let Some(pid) = listed(ui.app(), tag) {
            return pid;
        }
        if let Ok(r) = rx.try_recv() {
            panic!("the sleeping session ended before it was listed: {r:?}");
        }
        assert!(t.elapsed() < LIMIT, "timed out waiting for the sleeping session to be listed; window: {}; last error: {}", ui.app().pg_server.as_ref().map_or("closed".into(), |s| s.activity_state()), ui.app().last_problem());
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The pid of the session whose query carries `tag`, once the window lists it as running.
fn listed(a: &App, tag: &str) -> Option<u64> {
    a.pg_server.as_ref()?.listed(tag)
}

/// One frame at egui time `at`. Set by hand because a kittest step lasts 250 ms of egui time, too coarse for a double click.
fn frame_at(ui: &mut UiApp, at: f64) {
    ui.h.input_mut().time = Some(at);
    ui.h.step();
}

/// A click on the widget named `label`, pressed and released in the frame at egui time `at`.
fn click_at(ui: &mut UiApp, label: &str, at: f64) {
    let p = ui.h.get_by_label(label).rect().center();
    let press = |pressed| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    ui.h.input_mut().events.extend([egui::Event::PointerMoved(p), press(true), press(false)]);
    frame_at(ui, at);
}

#[test]
fn a_double_click_on_terminate_arms_it_and_does_not_send() {
    for var in ["QUARRY_TEST_POSTGRES", "QUARRY_TEST_POSTGRES13"] {
        let Some(mut ui) = ui_app(var) else { continue };
        let uri = std::env::var(var).unwrap();
        let tag = format!("ui_srv_dbl_{}", std::process::id());
        let rx = sleeper(&uri, &tag);
        ui.h.state_mut().open_server(CONN);
        ui.click("Activity");
        let pid = wait_listed(&mut ui, &rx, &tag);
        ui.click("Unlock…");
        ui.click("Allow writes");
        ui.wait("writes to be allowed", |a| a.conn(CONN).is_some_and(|c| c.writable));
        ui.click(&format!("Session {pid}"));
        // The red confirm takes the place of Terminate, so the second click of a double click lands on it 150 ms later.
        let t = ui.h.ctx.input(|i| i.time) + 1.0;
        click_at(&mut ui, "Terminate", t);
        assert!(ui.app().pg_server.as_ref().is_some_and(|s| s.armed()), "the first click arms");
        frame_at(&mut ui, t + 0.05);
        ui.show(&format!("End session {pid}?"));
        click_at(&mut ui, "Terminate", t + 0.15);
        frame_at(&mut ui, t + 0.2);
        assert!(ui.app().sql_kills.is_empty() && ui.app().pg_server.as_ref().is_some_and(|s| s.armed()), "a double click does not send");
        std::thread::sleep(Duration::from_millis(300));
        assert!(rx.try_recv().is_err(), "the session still runs");
        // A deliberate second click after the guard sends it.
        click_at(&mut ui, "Terminate", t + 0.75);
        ui.h.input_mut().time = None;
        assert!(!ui.app().sql_kills.is_empty(), "the click after the guard sends");
        let started = std::time::Instant::now();
        let out = loop {
            ui.h.step();
            if let Ok(r) = rx.try_recv() {
                break r;
            }
            assert!(started.elapsed() < LIMIT, "the session was not ended; last error: {}", ui.app().last_problem());
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(out.is_err(), "the terminated session's statement fails");
    }
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
        let pid = wait_listed(&mut ui, &rx, &tag);
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
