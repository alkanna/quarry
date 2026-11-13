//! Folders that open and close with an animation, on the offline demo: a group of the Connections panel, a folder of the
//! Library and a folder of the rail each settle to their final layout within the animation's time, a click on a row
//! below a folder that is still moving lands on that row, and the children of a folder that has closed take no click.
//!
//! The other UI tests run with egui's animations off and a 250 ms frame, so they only ever see folders at rest. These
//! turn the animations on and step 1/60 s at a time, and click through raw pointer events at a row's place on the frame
//! before: egui hit-tests a press against where the widgets were drawn on the last frame.
use super::*;
use crate::library::tree::Node;
use crate::ui::theme::FOLD_S;

/// Frames of 1/60 s that a fold takes, with a few to spare.
const SETTLE: usize = (FOLD_S * 60.0) as usize + 4;

/// `demo_ui` with egui's animations on and frames of 1/60 s.
fn animated(load: fn(&mut App)) -> Option<UiApp> {
    if std::env::vars().any(|(k, _)| k.starts_with("QUARRY_TEST_")) {
        eprintln!("skipped: a QUARRY_TEST_ switch is set, which changes the demo");
        return None;
    }
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_step_dt(1.0 / 60.0).with_accessibility_check(false).build_eframe(|cc| App::for_test(cc));
    h.ctx.all_styles_mut(|s| s.animation_time = 0.2);
    let app = h.state_mut();
    app.demo = true;
    load(app);
    let mut ui = UiApp { h };
    ui.h.run_steps(10);
    Some(ui)
}

impl UiApp {
    /// Where the widget with this name was drawn on the last frame.
    fn rect_of(&self, label: &str) -> egui::Rect {
        self.h.get_all_by_label(label).next().unwrap_or_else(|| panic!("no widget named {label}")).rect()
    }

    fn has(&self, label: &str) -> bool {
        self.h.query_all_by_label(label).next().is_some()
    }

    /// Moves onto `at`, presses and lets go, all in one frame, so the rows have no frame to move in between.
    fn click_at(&mut self, at: egui::Pos2) {
        let ev = &mut self.h.input_mut().events;
        ev.push(egui::Event::PointerMoved(at));
        for pressed in [true, false] {
            ev.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        }
        self.h.step();
    }
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.6
}

#[test]
fn a_group_folds_away_smoothly_and_a_click_below_it_lands_on_the_row_there() {
    const PROD: u64 = 1;
    const STAGE: u64 = 2;
    let Some(mut ui) = animated(|a| {
        a.load_demo();
        a.demo_conns("panel");
    }) else {
        return;
    };
    let prod = ui.rect_of("Group Production");
    let stage0 = ui.rect_of("Group Staging");
    let us = ui.rect_of("prod-us");
    let gap = stage0.top() - ui.rect_of("billing").bottom();

    ui.click_at(prod.center());
    assert!(!ui.app().groups.group(PROD).unwrap().open);
    ui.h.run_steps(2);
    let stage = ui.rect_of("Group Staging");
    assert!(stage.top() < stage0.top() - 1.0 && stage.top() > prod.bottom() + gap + 1.0, "Staging is on its way up: {stage:?}");
    // The press goes where Staging was last drawn, part way up, and folds Staging, not a member of Production.
    ui.click_at(stage.center());
    assert!(!ui.app().groups.group(STAGE).unwrap().open, "the click folded Staging");
    assert_eq!(ui.app().active, Some(11));

    ui.h.run_steps(SETTLE);
    assert!(!ui.has("prod-us") && !ui.has("billing"), "a closed group's members are gone");
    assert!(near(ui.rect_of("Group Staging").top(), prod.bottom() + gap), "Staging sits right under Production");
    // Where prod-us was, nothing activates it any more.
    ui.click_at(us.center());
    ui.h.run_steps(SETTLE);
    assert_eq!(ui.app().active, Some(11));
    assert_eq!(ui.app().prefs.side_mode(), SideMode::Connections);
}

#[test]
fn a_library_folder_folds_away_smoothly_and_a_click_below_it_lands_on_the_row_there() {
    let Some(mut ui) = animated(|a| {
        a.conns.push(Conn::new(ConnMeta { id: 1, name: "Local sample".into(), ..Default::default() }, Status::Ready));
        a.demo_library("panel");
        a.lib_ui.panel.sel = None;
    }) else {
        return;
    };
    let reports = ui.rect_of("Reports");
    let maint0 = ui.rect_of("Maintenance");
    let monthly = ui.rect_of("Monthly revenue");
    let row_h = reports.height();

    ui.click_at(reports.center());
    assert!(!ui.app().lib_ui.panel.open.contains(&Node::Folder(1)));
    ui.h.run_steps(2);
    let maint = ui.rect_of("Maintenance");
    assert!(maint.top() < maint0.top() - 1.0 && maint.top() > reports.bottom() + 1.0, "Maintenance is on its way up: {maint:?}");
    ui.click_at(maint.center());
    assert!(ui.app().lib_ui.panel.open.contains(&Node::Folder(3)), "the click opened Maintenance");
    assert_eq!(ui.app().lib_ui.panel.sel, Some(Node::Folder(3)));

    ui.h.run_steps(SETTLE);
    assert!(!ui.has("Monthly revenue") && !ui.has("Weekly"), "a closed folder's rows are gone");
    assert!(ui.has("Stale carts"), "Maintenance opened");
    assert!(near(ui.rect_of("Maintenance").top(), reports.top() + row_h));
    assert!(near(ui.rect_of("Stale carts").top(), reports.top() + 2.0 * row_h));
    // Where Monthly revenue was, a click picks the row there now.
    ui.click_at(monthly.center());
    assert_ne!(ui.app().lib_ui.panel.sel, Some(Node::Script(10)));
}

#[test]
fn a_rail_folder_grows_into_its_capsule_and_a_click_below_it_lands_on_that_tile() {
    let Some(mut ui) = animated(|a| {
        a.load_demo();
        for (id, name) in [(5, "alpha"), (6, "beta"), (7, "gamma")] {
            a.conns.push(Conn::new(ConnMeta { id, name: name.into(), engine: db::sql::Engine::MySql, ..Default::default() }, Status::Ready));
        }
        let gid = a.groups.new_group("Fleet");
        let all = a.conn_ids();
        a.groups.place(6, Place::Into(gid, None), &all);
        a.groups.place(7, Place::Into(gid, None), &all);
        // The folder first, so a tile sits right below it.
        a.rail_order = vec![6, 7, 5];
    }) else {
        return;
    };
    let folder = ui.rect_of("Fleet folder");
    let alpha0 = ui.rect_of("Switch to alpha");
    assert!(!ui.has("Switch to beta"));

    ui.click_at(folder.center());
    assert!(ui.app().rail_open.contains(&1));
    ui.h.run_steps(2);
    let alpha = ui.rect_of("Switch to alpha");
    // Open, the capsule holds two member slots of 32 px and a 3 px margin.
    let grown = alpha0.top() + 2.0 * 32.0 + 3.0;
    assert!(alpha.top() > alpha0.top() + 1.0 && alpha.top() < grown - 1.0, "alpha is on its way down: {alpha:?}");
    ui.click_at(alpha.center());
    assert_eq!(ui.app().active, Some(5), "the click picked alpha");

    ui.h.run_steps(SETTLE);
    assert!(ui.has("Switch to beta") && ui.has("Switch to gamma"));
    assert!(near(ui.rect_of("Switch to alpha").top(), grown));
    let beta = ui.rect_of("Switch to beta");

    let folder = ui.rect_of("Fleet folder");
    ui.click_at(folder.center());
    ui.h.run_steps(SETTLE);
    assert!(!ui.app().rail_open.contains(&1));
    assert!(!ui.has("Switch to beta"), "a closed folder shows no member tiles");
    assert!(near(ui.rect_of("Switch to alpha").top(), alpha0.top()));
    // Where beta was, a click does not pick it.
    ui.click_at(beta.center());
    ui.h.run_steps(2);
    assert_ne!(ui.app().active, Some(6));
}

#[test]
fn a_press_made_while_a_group_folds_still_clicks_when_let_go_after_it_settled() {
    const STAGE: u64 = 2;
    let Some(mut ui) = animated(|a| {
        a.load_demo();
        a.demo_conns("panel");
    }) else {
        return;
    };
    let prod = ui.rect_of("Group Production");
    ui.click_at(prod.center());
    // Nearly closed: the rows below have almost stopped, and the press is held past the frame the fold settles on.
    ui.h.run_steps(6);
    let at = ui.rect_of("Group Staging").center();
    let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    let ev = &mut ui.h.input_mut().events;
    ev.push(egui::Event::PointerMoved(at));
    ev.push(button(true));
    ui.h.run_steps(SETTLE);
    ui.h.input_mut().events.push(button(false));
    ui.h.step();
    assert!(!ui.app().groups.group(STAGE).unwrap().open, "the press and the release made one click on Staging");
}

#[test]
fn a_folder_of_thousands_of_scripts_draws_only_the_rows_in_view_while_it_moves() {
    let Some(mut ui) = animated(|a| {
        a.conns.push(Conn::new(ConnMeta { id: 1, name: "Local sample".into(), ..Default::default() }, Status::Ready));
        a.demo_library("panel");
        a.library.folders.push(crate::library::Folder { id: 5, name: "Bulk".into(), parent: None });
        for i in 0..3000u64 {
            a.library.scripts.push(crate::library::Script { id: 1000 + i, name: format!("bulk-{i:04}"), kind: "sql".into(), conn: 3, folder: Some(5), ..Default::default() });
        }
        a.lib_ui.panel.open.insert(Node::Folder(5));
        a.lib_ui.panel.sel = None;
    }) else {
        return;
    };
    let cap = (ui.app().lib_ui.panel.view_h / ui.rect_of("Bulk").height()) as usize + 3;
    let drawn = |ui: &UiApp| ui.h.query_all_by_label_contains("bulk-").count();
    let at_rest = drawn(&ui);
    assert!(at_rest > 0 && at_rest <= cap, "{at_rest} rows drawn for a view of {cap}");
    let bulk = ui.rect_of("Bulk");
    ui.click_at(bulk.center());
    ui.h.run_steps(2);
    let closing = drawn(&ui);
    assert!(closing > 0 && closing <= cap, "{closing} rows drawn while closing, for a view of {cap}");
    ui.h.run_steps(SETTLE);
    assert_eq!(drawn(&ui), 0);
    let bulk = ui.rect_of("Bulk");
    ui.click_at(bulk.center());
    ui.h.run_steps(2);
    let opening = drawn(&ui);
    assert!(opening > 0 && opening <= cap, "{opening} rows drawn while opening, for a view of {cap}");
}
