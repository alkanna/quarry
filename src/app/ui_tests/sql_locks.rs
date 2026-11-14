//! The Locks tab of the MySQL and MariaDB Server window, offline on its demo data: a blocker that sits idle in a transaction with
//! two waiters under it, one of them blocking a third. Kill needs two clicks, and a locked connection offers none.

use super::*;

fn locks_window(a: &mut App) {
    a.load_sql_demo();
    a.sql_server = Some(SqlServerWin::demo(3, "processes"));
}

fn off(ui: &UiApp, label: &str) -> bool {
    ui.h.query_all_by_label(label).last().unwrap_or_else(|| panic!("no widget named {label}")).accesskit_node().is_disabled()
}

fn present(ui: &UiApp, part: &str) -> bool {
    ui.h.query_all_by_label_contains(part).next().is_some()
}

#[test]
fn the_locks_tab_names_the_blocker_chain_and_its_source() {
    let Some(mut ui) = demo_ui(locks_window) else { return };
    ui.click("Locks");
    ui.show_contains("performance_schema.data_lock_waits");
    // 8850 waits on 8841, which waits on 8790: the footnote says so instead of blaming the head directly.
    ui.show_contains("8850 queues behind 8841, not behind 8790 directly");
    assert!(!present(&ui, "No thread waits for a lock"));
}

#[test]
fn an_idle_blocker_offers_kill_connection_only_and_it_takes_two_clicks() {
    let Some(mut ui) = demo_ui(locks_window) else { return };
    ui.click("Locks");
    ui.show("Kill connection");
    // The blocker sleeps in its transaction: there is no statement to stop, so only ending the connection frees the waiters.
    assert!(off(&ui, "Kill query"), "Kill query is on for a thread that runs nothing");
    assert!(!off(&ui, "Kill connection"));

    ui.click("Kill connection");
    ui.show_contains("End connection 8790?");
    ui.click("Keep");
    assert!(!present(&ui, "End connection 8790?"), "Keep left the confirmation up");
    ui.show("Kill connection");

    ui.click("Kill connection");
    ui.show_contains("End connection 8790?");
    // The second click comes after the guard that keeps a double click from sending the first.
    ui.click("Kill connection");
    assert!(!present(&ui, "End connection 8790?"), "the confirmed Kill left its question on screen");
    assert!(ui.app().sql_server.is_some(), "the window closed with the kill");
}

#[test]
fn a_locked_connection_offers_no_kill_and_unlocks_from_the_banner() {
    let Some(mut ui) = demo_ui(locks_window) else { return };
    for c in &mut ui.h.state_mut().conns {
        c.writable = false;
    }
    ui.click("Locks");
    ui.show_contains("Unlock writes to kill queries or connections");
    assert!(off(&ui, "Kill connection") && off(&ui, "Kill query"), "a read-only connection can kill");
    ui.click("Unlock\u{2026}");
    assert!(matches!(ui.app().modal, Some(Modal::Unlock(..))), "the banner did not ask to unlock");
}
