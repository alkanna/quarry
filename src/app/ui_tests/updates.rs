//! The update pill, its popover, Settings > Updates, What's new and the rail's Settings tile, on the offline demo.
use super::*;
use crate::update::{Available, Entry, Kind, Release, Section};

fn notes(text: &str) -> Vec<Section> {
    vec![Section { kind: Kind::New, title: "Features".into(), entries: vec![Entry { scope: Some("agents".into()), text: text.into(), pr: Some(279) }] }]
}

fn release() -> Available {
    Available { version: "9.1.0".into(), name: "a.zip".into(), url: String::new(), size: 1000, sum_url: String::new(), notes: notes("Saved filters can be pinned") }
}

fn ready(a: &mut App) {
    a.load_demo();
    a.upd = Upd::Ready(release());
}

fn ready_with_work(a: &mut App) {
    ready(a);
    a.tabs[0].compare.running = true;
}

fn settings_ready(a: &mut App) {
    ready(a);
    a.modal = Some(Modal::Settings);
    a.settings_cat = settings::SettingsCat::Updates;
}

fn two_releases(a: &mut App) {
    a.load_demo();
    let r = |v: &str, t: &str| Release { version: v.into(), date: "2026-11-17".into(), sections: notes(t) };
    a.modal = Some(Modal::WhatsNew(Box::new(WhatsNew::new(vec![r("9.1.0", "Newest entry"), r("9.0.0", "Older entry")], "8.9.0"))));
}

#[test]
fn the_settings_tile_is_as_big_as_connect() {
    let Some(ui) = demo_ui(App::load_demo) else { return };
    let size = |l: &str| ui.h.get_by_label(l).rect().size();
    assert_eq!(size("Settings (Cmd+,)"), size("Connect"));
}

#[test]
fn the_pill_opens_restart_and_open_work_holds_it() {
    let Some(mut ui) = demo_ui(ready) else { return };
    ui.click("Update status");
    ui.show("Restart now");
    assert!(!ui.disabled("Restart now"));
    assert!(ui.shows("Saved filters can be pinned"), "the popover lists the release's highlights");
    let Some(mut ui) = demo_ui(ready_with_work) else { return };
    ui.click("Update status");
    ui.show("Restart now");
    assert!(ui.disabled("Restart now"), "a running comparison would be lost");
}

#[test]
fn settings_offers_restart_once_installed() {
    let Some(mut ui) = demo_ui(settings_ready) else { return };
    ui.show("Restart now");
    assert!(ui.shows("Quarry 9.1.0 is installed"));
    ui.click("Show");
    ui.wait("What's new to open", |a| matches!(a.modal, Some(Modal::WhatsNew(_))));
}

#[test]
fn whats_new_folds_older_releases() {
    let Some(mut ui) = demo_ui(two_releases) else { return };
    ui.show("Close");
    assert!(ui.shows("Newest entry") && !ui.shows("Older entry"));
    ui.click("Quarry 9.0.0");
    ui.wait("the older release to unfold", |a| matches!(&a.modal, Some(Modal::WhatsNew(w)) if w.open.contains("9.0.0")));
    ui.h.run_steps(2);
    assert!(ui.shows("Older entry"));
}
