//! Every state of the QUARRY_TEST_CONNS, QUARRY_TEST_LIBRARY and QUARRY_TEST_RAIL switches, loaded headlessly and drawn for
//! a few frames, so a screenshot switch never breaks unseen: the loaders place connections and scripts by id and open
//! dialogs, which a change elsewhere can leave pointing at nothing.

use super::*;

const CONNS: [&str; 7] = ["panel", "filter", "menu", "new", "edit", "drag", "picker"];
const LIBRARY: [&str; 16] = ["panel", "all", "search", "empty", "menu", "folder-menu", "drag", "linked", "missing", "link", "save-linked", "conflict", "save", "picker", "picker-docs", "picker-shell"];
const RAIL: [&str; 4] = ["1", "groups", "open", "tip"];

/// The demo as the app builds it for a switch: `load_demo`, then `then`, then a few frames.
fn draw(then: impl FnOnce(&mut App)) -> Option<UiApp> {
    if std::env::vars().any(|(k, _)| k.starts_with("QUARRY_TEST_")) {
        eprintln!("skipped: a QUARRY_TEST_ switch is set, which changes the demo");
        return None;
    }
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_accessibility_check(false).build_eframe(|cc| App::for_test(cc));
    let a = h.state_mut();
    a.demo = true;
    a.load_demo();
    then(a);
    h.run_steps(4);
    Some(UiApp { h })
}

#[test]
fn every_connections_state_draws() {
    for s in CONNS {
        let Some(ui) = draw(|a| a.demo_conns(s)) else { return };
        // Every member the loader placed is a saved connection, so no group row counts one the panel cannot show.
        let alive = ui.app().conn_ids();
        for g in &ui.app().groups.groups {
            assert!(g.members.iter().all(|m| alive.contains(m)), "{s}: group {} holds a connection that does not exist: {:?}", g.name, g.members);
        }
    }
}

#[test]
fn every_library_state_draws() {
    for s in LIBRARY {
        let Some(ui) = draw(|a| a.demo_library(s)) else { return };
        let a = ui.app();
        assert!(a.library.links.iter().all(|l| a.lib_ui.scans.contains_key(&l.id)), "{s}: a demo link has no made-up scan, so it would read the disk");
    }
}

#[test]
fn every_rail_state_draws() {
    for s in RAIL {
        let Some(ui) = draw(|a| a.demo_rail(s)) else { return };
        // A closed folder hides its members; elsewhere the connecting reports tile shows.
        if matches!(s, "groups" | "tip") {
            ui.show("Production folder");
        } else {
            ui.show("Switch to reports");
        }
    }
}
