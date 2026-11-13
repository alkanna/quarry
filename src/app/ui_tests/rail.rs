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

#[test]
fn undoing_a_new_group_puts_the_tiles_back_and_a_later_group_is_not_open() {
    let Some(mut ui) = demo_ui(without_group) else { return };
    ui.drag("Switch to alpha", "Switch to beta", 0.5, 1);
    ui.wait("the new group", |a| a.groups.groups.len() == 1);
    assert_eq!(ui.app().rail_order, [1, 6, 5, 7]);
    ui.click_pick("Undo", Pick::Last);
    ui.wait("the grouping to be undone", |a| a.groups.groups.is_empty());
    assert_eq!(ui.app().rail_order, [1, 5, 6, 7], "Undo left the tile where the drop put it");
    ui.settle();
    // The next group takes the undone one's id, and must start closed like any group made elsewhere.
    let a = ui.h.state_mut();
    let gid = a.groups.new_group("Later");
    let all = a.conn_ids();
    a.groups.place(5, Place::Into(gid, None), &all);
    a.groups.place(6, Place::Into(gid, None), &all);
    ui.settle();
    assert_eq!(gid, 1);
    assert!(!ui.app().rail_open.contains(&gid), "a new group showed open because an undone one had its id");
    ui.show("Later folder");
}

#[test]
fn esc_cancels_a_tile_drag() {
    let Some(mut ui) = demo_ui(without_group) else { return };
    ui.drag_esc("Switch to alpha", "Switch to beta", 0.5);
    assert!(ui.app().groups.groups.is_empty(), "Esc did not cancel the drop on a tile");
    assert_eq!(ui.app().rail_order, [1, 5, 6, 7]);
    // A click still switches.
    ui.click("Switch to gamma");
    assert_eq!(ui.app().active, Some(7));
}

#[test]
fn a_member_reorders_inside_its_open_folder() {
    let Some(mut ui) = demo_ui(with_group) else { return };
    ui.click("Fleet folder");
    ui.show("Switch to gamma");
    ui.drag("Switch to gamma", "Switch to beta", 0.1, 1);
    ui.wait("the members to swap", |a| a.groups.groups[0].members == [7, 6]);
    assert!(ui.app().rail_open.contains(&1), "the folder closed");
}

#[test]
fn a_member_dragged_out_of_its_folder_leaves_the_group_and_undo_brings_it_back() {
    let Some(mut ui) = demo_ui(with_group) else { return };
    ui.click("Fleet folder");
    ui.drag("Switch to gamma", "Switch to alpha", 0.1, 1);
    ui.wait("gamma to leave the group", |a| a.groups.groups[0].members == [6]);
    assert_eq!(ui.app().rail_order, [1, 7, 5, 6]);
    ui.click_pick("Undo", Pick::Last);
    ui.wait("the group to be back", |a| a.groups.groups[0].members == [6, 7]);
    assert_eq!(ui.app().rail_order, [1, 5, 6, 7]);
}

#[test]
fn a_tile_dropped_among_open_members_joins_at_that_place() {
    let Some(mut ui) = demo_ui(with_group) else { return };
    ui.click("Fleet folder");
    ui.drag("Switch to alpha", "Switch to gamma", 0.1, 1);
    ui.wait("alpha to join", |a| a.groups.groups[0].members.contains(&5));
    assert_eq!(ui.app().groups.groups[0].members, [6, 5, 7]);
}
