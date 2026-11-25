//! The group folders in the rail, on the offline demo: opening and closing a folder, and dragging a tile onto another.
use super::*;

/// The Mongo demo plus three Ready connections, with 6 and 7 folded into one group.
fn with_group(a: &mut App) {
    a.load_demo();
    for (id, name) in [(5, "alpha"), (6, "beta"), (7, "gamma")] {
        a.core.conns.push(Conn::new(ConnMeta { id, name: name.into(), engine: db::sql::Engine::MySql, ..Default::default() }, Status::Ready));
    }
    let gid = a.core.groups.new_group("Fleet");
    let all = a.conn_ids();
    a.core.groups.place(6, Place::Into(gid, None), &all);
    a.core.groups.place(7, Place::Into(gid, None), &all);
}

fn without_group(a: &mut App) {
    with_group(a);
    a.core.groups = Groups::default();
}

#[test]
fn clicking_a_folder_opens_and_closes_it() {
    let Some(mut ui) = demo_ui(with_group) else { return };
    ui.show("Fleet folder");
    assert!(ui.h.query_all_by_label("Switch to beta").next().is_none(), "a closed folder shows no member tiles");
    ui.click("Fleet folder");
    ui.show("Switch to beta");
    ui.show("Switch to gamma");
    assert!(ui.app().core.groups.rail_open(1));
    ui.click("Fleet folder");
    ui.wait("the members to fold away", |a| !a.core.groups.rail_open(1));
    ui.h.run_steps(2);
    assert!(ui.h.query_all_by_label("Switch to beta").next().is_none());
}

#[test]
fn a_folder_with_one_open_member_shows_its_tile() {
    let Some(mut ui) = demo_ui(|a| {
        with_group(a);
        a.core.conns.retain(|c| c.meta.id != 7);
    }) else {
        return;
    };
    ui.show("Switch to beta");
    // Clicking its name does not fold the lone tile away.
    ui.click("Fleet folder");
    ui.h.run_steps(2);
    ui.show("Switch to beta");
    assert!(!ui.app().core.groups.rail_open(1));
}

#[test]
fn dropping_a_tile_on_another_makes_a_group() {
    let Some(mut ui) = demo_ui(without_group) else { return };
    assert!(ui.app().core.groups.groups.is_empty());
    ui.drag("Switch to alpha", "Switch to beta", 0.5, 1);
    ui.wait("the new group", |a| a.core.groups.groups.len() == 1);
    let a = ui.app();
    assert_eq!(a.core.groups.groups[0].members, [6, 5]);
    // The toast offers Undo and Rename, and the new group shows open so the person sees where the tiles went.
    let _ = a;
    ui.show("Rename");
    ui.show("New group folder");
    ui.click_pick("Undo", Pick::Last);
    ui.wait("the grouping to be undone", |a| a.core.groups.groups.is_empty());
}

#[test]
fn dropping_a_tile_on_a_folder_keeps_the_folder_in_place() {
    let Some(mut ui) = demo_ui(with_group) else { return };
    assert_eq!(ui.app().core.rail_order, [1, 5, 6, 7]);
    ui.drag("Switch to Local sample", "Fleet folder", 0.5, 1);
    ui.wait("the tile to join the group", |a| a.core.groups.groups[0].members.contains(&1));
    // The tile follows the group, so the folder (at its first open member) stays below alpha.
    assert_eq!(ui.app().core.rail_order, [5, 6, 7, 1]);
}

#[test]
fn dropping_on_the_edge_of_a_tile_reorders_the_rail() {
    let Some(mut ui) = demo_ui(without_group) else { return };
    let before = ui.app().core.rail_order.clone();
    assert_eq!(before, [1, 5, 6, 7]);
    // Gamma onto the top third of alpha goes before it.
    ui.drag("Switch to gamma", "Switch to alpha", 0.1, 1);
    ui.wait("the new order", |a| a.core.rail_order == [1, 7, 5, 6]);
    assert!(ui.app().core.groups.groups.is_empty());
}

#[test]
fn undoing_a_new_group_puts_the_tiles_back_and_a_later_group_is_not_open() {
    let Some(mut ui) = demo_ui(without_group) else { return };
    ui.drag("Switch to alpha", "Switch to beta", 0.5, 1);
    ui.wait("the new group", |a| a.core.groups.groups.len() == 1);
    assert_eq!(ui.app().core.rail_order, [1, 6, 5, 7]);
    ui.click_pick("Undo", Pick::Last);
    ui.wait("the grouping to be undone", |a| a.core.groups.groups.is_empty());
    assert_eq!(ui.app().core.rail_order, [1, 5, 6, 7], "Undo left the tile where the drop put it");
    ui.settle();
    // The next group takes the undone one's id, and must start closed like any group made elsewhere.
    let a = ui.h.state_mut();
    let gid = a.core.groups.new_group("Later");
    let all = a.conn_ids();
    a.core.groups.place(5, Place::Into(gid, None), &all);
    a.core.groups.place(6, Place::Into(gid, None), &all);
    ui.settle();
    assert_eq!(gid, 1);
    assert!(!ui.app().core.groups.rail_open(gid), "a new group showed open because an undone one had its id");
    ui.show("Later folder");
}

#[test]
fn undoing_a_drop_keeps_a_group_edit_made_since() {
    let Some(mut ui) = demo_ui(with_group) else { return };
    ui.drag("Switch to alpha", "Switch to Local sample", 0.5, 1);
    ui.wait("the new group", |a| a.core.groups.groups.len() == 2);
    // While the toast is up, the other group is renamed.
    ui.h.state_mut().core.groups.group_mut(1).unwrap().name = "Renamed".into();
    ui.click_pick("Undo", Pick::Last);
    ui.wait("the grouping to be undone", |a| a.core.groups.groups.len() == 1);
    let a = ui.app();
    assert_eq!((a.core.groups.groups[0].name.as_str(), a.core.groups.groups[0].members.clone()), ("Renamed", vec![6, 7]), "Undo threw away the rename");
    assert!(a.core.groups.group_of(1).is_none() && a.core.groups.group_of(5).is_none());
    assert_eq!(a.core.rail_order, [1, 5, 6, 7]);
}

#[test]
fn esc_cancels_a_tile_drag() {
    let Some(mut ui) = demo_ui(without_group) else { return };
    ui.drag_esc("Switch to alpha", "Switch to beta", 0.5);
    assert!(ui.app().core.groups.groups.is_empty(), "Esc did not cancel the drop on a tile");
    assert_eq!(ui.app().core.rail_order, [1, 5, 6, 7]);
    // A click still switches.
    ui.click("Switch to gamma");
    assert_eq!(ui.app().core.active, Some(7));
}

#[test]
fn a_member_reorders_inside_its_open_folder() {
    let Some(mut ui) = demo_ui(with_group) else { return };
    ui.click("Fleet folder");
    ui.show("Switch to gamma");
    ui.drag("Switch to gamma", "Switch to beta", 0.1, 1);
    ui.wait("the members to swap", |a| a.core.groups.groups[0].members == [7, 6]);
    assert!(ui.app().core.groups.rail_open(1), "the folder closed");
}

#[test]
fn a_member_dragged_out_of_its_folder_leaves_the_group_and_undo_brings_it_back() {
    let Some(mut ui) = demo_ui(with_group) else { return };
    ui.click("Fleet folder");
    ui.drag("Switch to gamma", "Switch to alpha", 0.1, 1);
    ui.wait("gamma to leave the group", |a| a.core.groups.groups[0].members == [6]);
    assert_eq!(ui.app().core.rail_order, [1, 7, 5, 6]);
    ui.click_pick("Undo", Pick::Last);
    ui.wait("the group to be back", |a| a.core.groups.groups[0].members == [6, 7]);
    assert_eq!(ui.app().core.rail_order, [1, 5, 6, 7]);
}

#[test]
fn a_tile_dropped_among_open_members_joins_at_that_place() {
    let Some(mut ui) = demo_ui(with_group) else { return };
    ui.click("Fleet folder");
    ui.drag("Switch to alpha", "Switch to gamma", 0.1, 1);
    ui.wait("alpha to join", |a| a.core.groups.groups[0].members.contains(&5));
    assert_eq!(ui.app().core.groups.groups[0].members, [6, 5, 7]);
}

/// Opens the menu of a rail tile with a right click.
fn right_click(ui: &mut UiApp, label: &str) {
    ui.show(label);
    let at = ui.h.get_by_label(label).rect().center();
    ui.press_at(at, egui::PointerButton::Secondary, 1);
    ui.settle();
}

#[test]
fn a_folder_menu_disconnects_its_members_and_stops_folding() {
    let Some(mut ui) = demo_ui(with_group) else { return };
    right_click(&mut ui, "Fleet folder");
    // The standard menu look: proportional text in a menu that hugs its items, not a value list across the window.
    let w = ui.h.get_by_label("Don't fold in the rail").rect().width();
    assert!(w < 320.0, "the folder menu is {w} px wide");
    ui.click("Don't fold in the rail");
    assert!(!ui.app().core.groups.groups[0].fold_rail);
    // The members are tiles of their own now.
    ui.show("Switch to beta");
    assert!(ui.h.query_all_by_label("Fleet folder").next().is_none());
    // Folded again, Disconnect all closes both members and the folder leaves the rail.
    ui.h.state_mut().core.groups.groups[0].fold_rail = true;
    right_click(&mut ui, "Fleet folder");
    ui.click("Disconnect all");
    assert!([6, 7].iter().all(|id| ui.app().conn(*id).is_some_and(|c| c.status == Status::Idle)));
    ui.h.run_steps(3);
    assert!(ui.h.query_all_by_label("Fleet folder").next().is_none());
}

#[test]
fn a_folder_menu_opens_the_folder_and_edits_the_group() {
    let Some(mut ui) = demo_ui(with_group) else { return };
    right_click(&mut ui, "Fleet folder");
    ui.click("Open");
    assert!(ui.app().core.groups.rail_open(1));
    right_click(&mut ui, "Fleet folder");
    ui.click("Edit group\u{2026}");
    assert!(matches!(ui.app().modal, Some(Modal::GroupEdit(_))));
}

/// The Mongo demo plus 40 Ready connections, more tiles than the rail has room for.
fn crowded(a: &mut App) {
    a.load_demo();
    for i in 0..40 {
        a.core.conns.push(Conn::new(ConnMeta { id: 100 + i, name: format!("c{i}"), engine: db::sql::Engine::MySql, ..Default::default() }, Status::Ready));
    }
}

#[test]
fn the_rail_scrolls_its_tiles_and_keeps_settings_on_screen() {
    let Some(mut ui) = demo_ui(crowded) else { return };
    ui.show("Switch to c39");
    let rect = |ui: &UiApp, label: &str| ui.h.get_by_label_contains(label).rect();
    // The tiles scroll between the top of the rail and the Connect tile, which stays pinned above Settings.
    let (top, bottom) = (rect(&ui, "Switch to c0").top() - 1.0, ui.h.get_by_label("Connect").rect().top());
    let settings = rect(&ui, "Settings (Cmd+,)");
    assert!(settings.bottom() <= 900.0, "Settings is off screen at {settings:?}");
    let first = rect(&ui, "Switch to c0");
    assert!(first.top() >= top && first.bottom() <= bottom, "the first tile starts in view, at {first:?}");
    assert!(rect(&ui, "Switch to c39").top() > bottom, "the last tile starts below the fold");
    // A wheel turn over the tiles brings the last one into view and takes the first out of it.
    ui.h.hover_at(first.center());
    ui.h.step();
    for _ in 0..10 {
        ui.h.event(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, -400.0), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: egui::Modifiers::NONE });
        ui.h.step();
    }
    ui.h.run_steps(8);
    let last = rect(&ui, "Switch to c39");
    assert!(last.top() >= top && last.bottom() <= bottom, "the last tile scrolled into view, at {last:?} between {top} and {bottom}");
    assert!(rect(&ui, "Switch to c0").bottom() < top, "the first tile scrolled out of view");
    assert_eq!(rect(&ui, "Settings (Cmd+,)"), settings, "Settings stays put");
    assert_eq!(ui.h.get_by_label("Connect").rect().top(), bottom, "Connect stays put");
    // And back up.
    for _ in 0..10 {
        ui.h.event(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, 400.0), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: egui::Modifiers::NONE });
        ui.h.step();
    }
    ui.h.run_steps(8);
    let first = rect(&ui, "Switch to c0");
    assert!(first.top() >= top && first.bottom() <= bottom, "the first tile scrolled back, at {first:?}");
}

#[test]
fn the_title_bar_toggle_hides_and_shows_the_sidebar() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    let was = ui.app().core.prefs.sidebar_hidden;
    ui.click("Toggle sidebar");
    assert_eq!(ui.app().core.prefs.sidebar_hidden, !was);
    ui.click("Toggle sidebar");
    assert_eq!(ui.app().core.prefs.sidebar_hidden, was);
}

#[test]
fn the_status_bar_server_button_waits_for_a_connection() {
    // The demo has no live session, so the button is there but does nothing.
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    ui.show("Server");
    assert!(ui.h.get_by_label("Server").accesskit_node().is_disabled());
    assert!(ui.app().core.server.is_none());
}

#[test]
fn the_status_bar_server_button_opens_the_server_window() {
    let Some(mut ui) = ui_app("QUARRY_TEST_MONGO") else { return };
    ui.click("Server");
    assert!(ui.app().core.server.is_some(), "Server in the status bar opened nothing");
}

/// The Mongo demo, the MySQL demo and the saved but closed connections of the Connections panel demo, with the explorer showing.
fn with_saved(a: &mut App) {
    a.load_demo();
    a.demo_conns("panel");
    a.core.prefs.set_side_mode(SideMode::Explorer);
}

#[test]
fn connect_lists_the_saved_connections_and_activates_a_closed_one() {
    let Some(mut ui) = demo_ui(with_saved) else { return };
    let closed = ui.app().core.conns.iter().find(|c| c.status == Status::Idle).map(|c| (c.meta.id, c.meta.name.clone())).expect("a closed connection");
    assert!(ui.h.query_all_by_label(&format!("Switch to {}", closed.1)).next().is_none(), "a closed connection has no tile");
    ui.click("Connect");
    ui.show("Filter connections");
    ui.show("Manage\u{2026}");
    ui.show("New connection\u{2026}");
    ui.click(&closed.1);
    assert_eq!(ui.app().core.active, Some(closed.0), "picking a saved connection did not activate it");
}

#[test]
fn connect_manage_opens_the_connections_panel() {
    let Some(mut ui) = demo_ui(with_saved) else { return };
    ui.click("Connect");
    ui.click("Manage\u{2026}");
    assert_eq!(ui.app().core.prefs.side_mode(), SideMode::Connections);
    assert!(!ui.app().core.prefs.sidebar_hidden);
}

#[test]
fn a_tile_menu_opens_the_server_window_of_its_connection() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    let tile = format!("Switch to {}", ui.app().core.conns[0].meta.name);
    right_click(&mut ui, &tile);
    ui.click("Server status");
    assert!(ui.app().core.sql_server.is_some(), "Server status in the tile menu opened nothing");
}

#[test]
fn the_manage_tile_toggles_the_connections_panel_and_the_rail_menu_starts_a_group() {
    let Some(mut ui) = demo_ui(with_saved) else { return };
    ui.h.state_mut().core.prefs.set_side_mode(SideMode::Explorer);
    ui.click("Manage connections");
    assert_eq!(ui.app().core.prefs.side_mode(), SideMode::Connections);
    ui.click("Manage connections");
    assert_eq!(ui.app().core.prefs.side_mode(), SideMode::Explorer);
    // Empty rail below the pinned tiles.
    let at = ui.h.get_by_label("Manage connections").rect().center() + egui::vec2(0.0, 80.0);
    ui.press_at(at, egui::PointerButton::Secondary, 1);
    ui.settle();
    ui.show("New connection\u{2026}");
    ui.click("New group\u{2026}");
    assert_eq!(ui.app().core.prefs.side_mode(), SideMode::Connections);
    assert!(ui.app().core.conns_ui.adding.is_some());
}
