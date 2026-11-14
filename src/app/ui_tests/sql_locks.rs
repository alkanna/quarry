//! The Locks tab of the MySQL and MariaDB Server window, offline on its demo data: a blocker that sits idle in a transaction with
//! two waiters under it, one of them blocking a third. Kill needs two clicks, and a locked connection offers none.

use super::*;

fn locks_window(a: &mut App) {
    a.load_sql_demo();
    a.sql_server = Some(SqlServerWin::demo(3, "processes"));
}

#[test]
fn the_locks_tab_names_the_blocker_chain_and_its_source() {
    let Some(mut ui) = demo_ui(locks_window) else { return };
    ui.click("Locks");
    ui.show_contains("performance_schema.data_lock_waits");
    // 8850 waits on 8841, which waits on 8790: the footnote says so instead of blaming the head directly.
    ui.show_contains("8850 queues behind 8841, not behind 8790 directly");
    assert!(!ui.shows("No thread waits for a lock"));
}

#[test]
fn an_idle_blocker_offers_kill_connection_only_and_it_takes_two_clicks() {
    let Some(mut ui) = demo_ui(locks_window) else { return };
    ui.click("Locks");
    ui.show("Kill connection");
    // The blocker sleeps in its transaction: there is no statement to stop, so only ending the connection frees the waiters.
    assert!(ui.disabled("Kill query"), "Kill query is on for a thread that runs nothing");
    assert!(!ui.disabled("Kill connection"));

    ui.click("Kill connection");
    ui.show_contains("End connection 8790?");
    ui.click("Keep");
    assert!(!ui.shows("End connection 8790?"), "Keep left the confirmation up");
    ui.show("Kill connection");

    ui.click("Kill connection");
    ui.show_contains("End connection 8790?");
    // The second click comes after the guard that keeps a double click from sending the first.
    ui.click("Kill connection");
    assert!(!ui.shows("End connection 8790?"), "the confirmed Kill left its question on screen");
    assert!(ui.app().sql_server.is_some(), "the window closed with the kill");
}

#[test]
fn a_locked_connection_offers_no_kill_and_unlocks_from_the_banner() {
    let Some(mut ui) = demo_ui(locks_window) else { return };
    ui.lock();
    ui.click("Locks");
    ui.show_contains("Unlock writes to kill queries or connections");
    assert!(ui.disabled("Kill connection") && ui.disabled("Kill query"), "a read-only connection can kill");
    ui.click("Unlock\u{2026}");
    assert!(matches!(ui.app().modal, Some(Modal::Unlock(..))), "the banner did not ask to unlock");
}
