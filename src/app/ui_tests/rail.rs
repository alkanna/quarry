//! The group folders in the rail, on the offline demo: opening and closing a folder, and dragging a tile onto another.
use super::*;

/// The Mongo demo plus three Ready connections, with 6 and 7 folded into one group.
fn with_group(a: &mut App) {
    a.load_demo();
    for (id, name) in [(5, "alpha"), (6, "beta"), (7, "gamma")] {
        a.conns.push(Conn::new(ConnMeta { id, name: name.into(), engine: db::sql::Engine::MySql, ..Default::default() }, Status::Ready));
    }
    let gid = a.groups.new_group("Fleet");
    let all = a.conn_ids();
    a.groups.place(6, Place::Into(gid, None), &all);
    a.groups.place(7, Place::Into(gid, None), &all);
}

fn without_group(a: &mut App) {
    with_group(a);
    a.groups = Groups::default();
}

#[test]
fn clicking_a_folder_opens_and_closes_it() {
    let Some(mut ui) = demo_ui(with_group) else { return };
    ui.show("Fleet folder");
    assert!(ui.h.query_all_by_label("Switch to beta").next().is_none(), "a closed folder shows no member tiles");
    ui.click("Fleet folder");
    ui.show("Switch to beta");
    ui.show("Switch to gamma");
    assert!(ui.app().rail_open.contains(&1));
    ui.click("Fleet folder");
    ui.wait("the members to fold away", |a| !a.rail_open.contains(&1));
    ui.h.run_steps(2);
    assert!(ui.h.query_all_by_label("Switch to beta").next().is_none());
}

#[test]
fn dropping_a_tile_on_another_makes_a_group() {
    let Some(mut ui) = demo_ui(without_group) else { return };
    assert!(ui.app().groups.groups.is_empty());
    ui.drag("Switch to alpha", "Switch to beta", 0.5, 1);
    ui.wait("the new group", |a| a.groups.groups.len() == 1);
    let a = ui.app();
    assert_eq!(a.groups.groups[0].members, [6, 5]);
    // The toast offers Undo and Rename, and the new group shows open so the person sees where the tiles went.
    let _ = a;
    ui.show("Rename");
    ui.show("New group folder");
    ui.click_pick("Undo", Pick::Last);
    ui.wait("the grouping to be undone", |a| a.groups.groups.is_empty());
}

#[test]
fn dropping_a_tile_on_a_folder_keeps_the_folder_in_place() {
    let Some(mut ui) = demo_ui(with_group) else { return };
    assert_eq!(ui.app().rail_order, [1, 5, 6, 7]);
    ui.drag("Switch to Local sample", "Fleet folder", 0.5, 1);
    ui.wait("the tile to join the group", |a| a.groups.groups[0].members.contains(&1));
    // The tile follows the group, so the folder (at its first open member) stays below alpha.
    assert_eq!(ui.app().rail_order, [5, 6, 7, 1]);
}

#[test]
fn dropping_on_the_edge_of_a_tile_reorders_the_rail() {
    let Some(mut ui) = demo_ui(without_group) else { return };
    let before = ui.app().rail_order.clone();
    assert_eq!(before, [1, 5, 6, 7]);
    // Gamma onto the top third of alpha goes before it.
    ui.drag("Switch to gamma", "Switch to alpha", 0.1, 1);
    ui.wait("the new order", |a| a.rail_order == [1, 7, 5, 6]);
    assert!(ui.app().groups.groups.is_empty());
}
