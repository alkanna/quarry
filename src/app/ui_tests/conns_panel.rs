//! UI tests of the Connections panel and its groups, on the offline demo (`demo_conns`): the groups are Production (1,
//! red: prod-eu 11, prod-us 12, billing 13), Staging (2, amber: Staging 2, reports 14) and Local (3, teal, folded:
//! Local sample 1, scratch 15), and analytics (16) and sandbox (17) are in no group.

use super::*;

const PROD: u64 = 1;
const STAGE: u64 = 2;
const LOCAL: u64 = 3;

fn panel() -> Option<UiApp> {
    demo_ui(|a| {
        a.load_demo();
        a.demo_conns("panel");
    })
}

fn members(ui: &UiApp, g: u64) -> Vec<u64> {
    ui.app().groups.group(g).map(|g| g.members.clone()).unwrap_or_default()
}

/// Opens the menu of a row with a right click.
fn right_click(ui: &mut UiApp, label: &str) {
    ui.show(label);
    let at = ui.h.get_by_label(label).rect().center();
    ui.press_at(at, egui::PointerButton::Secondary, 1);
    ui.settle();
}

#[test]
fn the_rail_button_switches_the_sidebar_to_the_panel_and_hides_it() {
    let Some(mut ui) = panel() else { return };
    ui.h.state_mut().prefs.set_side_mode(SideMode::Explorer);
    ui.settle();
    assert!(ui.h.query_by_label("Group Production").is_none());
    ui.click("Saved connections");
    assert_eq!(ui.app().prefs.side_mode(), SideMode::Connections);
    ui.show("Group Production");
    // The active mode while the sidebar shows hides it, and the explorer button brings the explorer back.
    ui.click("Saved connections");
    assert!(ui.app().prefs.sidebar_hidden);
    ui.click("Database explorer");
    assert!(!ui.app().prefs.sidebar_hidden && ui.app().prefs.side_mode() == SideMode::Explorer);
}

#[test]
fn a_new_group_is_named_in_a_field_after_the_groups() {
    let Some(mut ui) = panel() else { return };
    ui.click("New group");
    ui.show("New group name");
    ui.type_text("Analytics");
    ui.h.key_press(egui::Key::Enter);
    ui.settle();
    let g = ui.app().groups.groups.last().cloned().expect("a group");
    assert_eq!((g.name.as_str(), g.members.len()), ("Analytics", 0));
    // It takes the first color no group has, and an empty one says where connections go.
    assert_eq!(g.color, "green");
    ui.show("Drag connections here");
    // Esc leaves the field without making a group.
    ui.click("New group");
    ui.type_text("Nope");
    ui.h.key_press(egui::Key::Escape);
    ui.settle();
    assert_eq!(ui.app().groups.groups.len(), 4);
}

#[test]
fn a_swatch_in_the_group_menu_recolors_the_group() {
    let Some(mut ui) = panel() else { return };
    right_click(&mut ui, "Group Production");
    ui.click("blue");
    assert_eq!(ui.app().groups.group(PROD).unwrap().color, "blue");
}

#[test]
fn deleting_a_group_frees_its_members_and_undo_puts_it_back() {
    let Some(mut ui) = panel() else { return };
    right_click(&mut ui, "Group Staging");
    ui.click("Delete group");
    assert!(ui.app().groups.group(STAGE).is_none());
    assert!(ui.app().groups.loose_order(&ui.app().conn_ids()).contains(&14));
    // The members are in the loose list now, under the caption.
    ui.show("reports");
    ui.click_last("Undo");
    assert_eq!(members(&ui, STAGE), [2, 14]);
    assert_eq!(ui.app().groups.groups.iter().map(|g| g.id).collect::<Vec<_>>(), [PROD, STAGE, LOCAL]);
}

#[test]
fn the_filter_opens_groups_with_a_match_and_leaves_the_folding_alone() {
    let Some(mut ui) = panel() else { return };
    assert!(ui.h.query_by_label("scratch").is_none(), "Local is folded");
    ui.fill("Filter connections", "scratch");
    ui.show("scratch");
    // Only the group holding the match is left, and the stored flag is not touched.
    assert!(ui.h.query_by_label("Group Staging").is_none() && ui.h.query_by_label("Group Production").is_none());
    ui.show("Group Local");
    assert!(!ui.app().groups.group(LOCAL).unwrap().open);
    ui.show("MATCHES");
    ui.h.state_mut().conns_ui.filter.clear();
    ui.settle();
    assert!(ui.h.query_by_label("scratch").is_none() && ui.h.query_by_label("MATCHES").is_none());
    ui.show("Group Staging");
}

#[test]
fn a_connection_is_dragged_into_a_group_out_of_it_and_within_it() {
    let Some(mut ui) = panel() else { return };
    // Into: the middle of the group row.
    ui.drag("sandbox", "Group Staging", 0.5, 1);
    assert_eq!(members(&ui, STAGE), [2, 14, 17]);
    // Within: the upper half of the first member puts the dragged one before it.
    ui.drag("prod-us", "prod-eu", 0.1, 1);
    assert_eq!(members(&ui, PROD), [12, 11, 13]);
    // Out: onto a loose row, before it.
    ui.drag("billing", "analytics", 0.1, 1);
    assert!(ui.app().groups.group_of(13).is_none());
    assert_eq!(ui.app().groups.loose_order(&ui.app().conn_ids()), [13, 16]);
    // A press that moves less than the drag threshold is a click on the row.
    ui.click("analytics");
    assert_eq!(ui.app().active, Some(16));
}

#[test]
fn a_drag_that_rests_on_a_folded_group_opens_it() {
    let Some(mut ui) = panel() else { return };
    ui.drag("sandbox", "Group Local", 0.5, 4);
    assert!(members(&ui, LOCAL).contains(&17));
    assert!(ui.app().groups.group(LOCAL).unwrap().open);
}

#[test]
fn the_edit_dialog_renames_recolors_reorders_and_removes_members() {
    let Some(mut ui) = panel() else { return };
    right_click(&mut ui, "Group Production");
    ui.click("Edit group…");
    ui.show("Group name");
    ui.click("Group name");
    ui.type_text(" EU");
    ui.click("Remove prod-us from the group");
    ui.click_last("Fold in the rail");
    ui.drag("Member billing", "Member prod-eu", 0.1, 1);
    ui.click("Save");
    let g = ui.app().groups.group(PROD).unwrap().clone();
    assert_eq!(g.name, "Production EU");
    assert!(!g.fold_rail);
    assert_eq!(g.members, [13, 11]);
    assert!(ui.app().groups.group_of(12).is_none(), "a removed member is in no group");
    assert!(ui.app().modal.is_none());
}

#[test]
fn cancel_leaves_the_group_as_it_was_and_add_pulls_a_connection_from_another_group() {
    let Some(mut ui) = panel() else { return };
    right_click(&mut ui, "Group Production");
    ui.click("Edit group…");
    ui.show("Group name");
    ui.click("Remove billing from the group");
    ui.click("Cancel");
    assert_eq!(members(&ui, PROD), [11, 12, 13]);
    right_click(&mut ui, "Group Production");
    ui.click("Edit group…");
    ui.click("Add connections…");
    // The panel behind the dialog has a row of the same name, and the popup is drawn last.
    ui.click_last("analytics");
    ui.click("Save");
    assert_eq!(members(&ui, PROD), [11, 12, 13, 16]);
}

#[test]
fn delete_group_in_the_dialog_deletes_it_with_an_undo() {
    let Some(mut ui) = panel() else { return };
    right_click(&mut ui, "Group Staging");
    ui.click("Edit group…");
    ui.click("Delete group");
    assert!(ui.app().groups.group(STAGE).is_none() && ui.app().modal.is_none());
    ui.click_last("Undo");
    assert!(ui.app().groups.group(STAGE).is_some());
}

#[test]
fn new_connection_in_a_group_remembers_the_group_for_the_form() {
    let Some(mut ui) = panel() else { return };
    right_click(&mut ui, "Group Production");
    ui.click("New connection in Production…");
    assert!(matches!(&ui.app().modal, Some(Modal::Conn(f)) if f.group == Some(PROD)));
}

#[test]
fn removing_a_connection_takes_it_out_of_its_group() {
    let Some(mut ui) = panel() else { return };
    ui.h.state_mut().danger(Danger::RemoveConnection(12));
    ui.settle();
    assert_eq!(members(&ui, PROD), [11, 13]);
    assert!(ui.h.query_by_label("prod-us").is_none());
}

#[test]
fn the_picker_groups_its_list_folds_it_and_opens_the_panel() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_demo();
        a.demo_conns("picker");
    }) else {
        return;
    };
    assert_eq!(ui.app().prefs.side_mode(), SideMode::Explorer);
    ui.show("Fold Production");
    ui.show("prod-eu");
    // A caption folds its connections in the picker only.
    ui.click("Fold Production");
    assert!(ui.h.query_by_label("prod-eu").is_none());
    assert!(ui.app().groups.group(PROD).unwrap().open);
    // The filter narrows the list and keeps the group of the match.
    ui.fill("Filter connections", "reports");
    ui.show("reports");
    assert!(ui.h.query_by_label("sandbox").is_none());
    ui.click("Manage groups…");
    assert_eq!(ui.app().prefs.side_mode(), SideMode::Connections);
}

#[test]
fn esc_cancels_a_connection_drag_and_a_click_still_activates() {
    let Some(mut ui) = panel() else { return };
    ui.drag_esc("sandbox", "Group Staging", 0.5);
    assert_eq!(members(&ui, STAGE), [2, 14], "Esc did not cancel the drop");
    assert!(ui.app().groups.group_of(17).is_none());
    ui.click("sandbox");
    assert_eq!(ui.app().active, Some(17));
}

#[test]
fn the_edit_dialog_reorders_members_by_dragging_and_esc_cancels_without_closing_it() {
    let Some(mut ui) = panel() else { return };
    right_click(&mut ui, "Group Production");
    ui.click("Edit group…");
    ui.show("Member billing");
    let form = |a: &App| match &a.modal {
        Some(Modal::GroupEdit(f)) => Some(f.members.clone()),
        _ => None,
    };
    ui.drag_esc("Member billing", "Member prod-eu", 0.1);
    assert_eq!(form(ui.app()), Some(vec![11, 12, 13]), "Esc moved a member or closed the dialog");
    ui.drag("Member prod-eu", "Member billing", 0.9, 1);
    assert_eq!(form(ui.app()), Some(vec![12, 13, 11]));
    ui.click("Save");
    assert_eq!(members(&ui, PROD), [12, 13, 11]);
}

#[test]
fn a_click_on_a_connection_shows_its_databases() {
    let Some(mut ui) = panel() else { return };
    ui.mouse_hold("analytics", 0);
    // Before, the panel stayed up and only the highlight moved.
    assert_eq!(ui.app().active, Some(16));
    assert_eq!(ui.app().prefs.side_mode(), SideMode::Explorer, "the click left the Connections panel up");
}

#[test]
fn a_hovered_row_shows_its_menu_button_with_browse_databases() {
    let Some(mut ui) = panel() else { return };
    ui.show("prod-us");
    let at = ui.h.get_by_label("prod-us").rect().center();
    ui.h.hover_at(at);
    ui.h.run_steps(2);
    ui.click("More for prod-us");
    let w = ui.h.get_by_label("Browse databases").rect().width();
    assert!(w < 320.0, "the row menu is {w} px wide");
    ui.click("Browse databases");
    assert_eq!((ui.app().active, ui.app().prefs.side_mode()), (Some(12), SideMode::Explorer));
    // A group row has one too, with the group's menu.
    ui.h.state_mut().prefs.set_side_mode(SideMode::Connections);
    ui.show("Group Staging");
    let at = ui.h.get_by_label("Group Staging").rect().center();
    ui.h.hover_at(at);
    ui.h.run_steps(2);
    ui.click("More for group Staging");
    ui.click("Delete group");
    assert!(ui.app().groups.group(STAGE).is_none());
}

#[test]
fn the_arrow_keys_walk_the_rows_and_return_opens_a_connection() {
    let Some(mut ui) = panel() else { return };
    let focused = |ui: &UiApp, label: &str| ui.h.get_by_label(label).is_focused();
    let none = egui::Modifiers::NONE;
    ui.show("Group Staging");
    ui.h.get_by_label("Group Staging").focus();
    ui.settle();
    assert!(focused(&ui, "Group Staging"));
    // Left folds the group, Right opens it again.
    ui.key(none, egui::Key::ArrowLeft);
    assert!(!ui.app().groups.group(STAGE).unwrap().open);
    ui.key(none, egui::Key::ArrowRight);
    assert!(ui.app().groups.group(STAGE).unwrap().open);
    // Down walks into the members, Left from a member goes back up to its group.
    ui.key(none, egui::Key::ArrowDown);
    ui.key(none, egui::Key::ArrowDown);
    assert!(focused(&ui, "reports"), "Down did not reach the second member");
    ui.key(none, egui::Key::ArrowLeft);
    assert!(focused(&ui, "Group Staging"));
    ui.key(none, egui::Key::ArrowDown);
    ui.key(none, egui::Key::ArrowDown);
    ui.key(none, egui::Key::Enter);
    assert_eq!((ui.app().active, ui.app().prefs.side_mode()), (Some(14), SideMode::Explorer));
}

#[test]
fn leaving_a_name_field_keeps_what_was_typed() {
    let Some(mut ui) = panel() else { return };
    ui.click("New group");
    ui.show("New group name");
    ui.type_text("Ops");
    // A press elsewhere ends the field: before, only Enter made the group.
    ui.mouse_hold("Group Production", 0);
    assert!(ui.app().groups.groups.iter().any(|g| g.name == "Ops"), "leaving the field threw the name away");
    // The same for a rename started with F2.
    ui.h.get_by_label("Group Local").focus();
    ui.settle();
    ui.key(egui::Modifiers::NONE, egui::Key::F2);
    ui.show("Group name");
    ui.h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    ui.type_text("Laptop");
    ui.mouse_hold("Group Production", 0);
    assert_eq!(ui.app().groups.group(LOCAL).map(|g| g.name.as_str()), Some("Laptop"));
}

#[test]
fn a_tag_keeps_its_word_in_an_indented_row() {
    // A group member in the default sidebar has about 190 px for its name and tag. The tag got 40 % of it, 76 px, which
    // cut PRODUCTION (about 80 px with its padding) to "PRODU...".
    assert!(tag_room(190.0) >= 95.0);
    // A wide row still gives most of its room to the name.
    assert!(tag_room(400.0) <= 310.0 && 400.0 - tag_room(400.0) >= 90.0);
}

#[test]
fn add_connections_filters_by_group_and_takes_one_from_another_group() {
    let Some(mut ui) = panel() else { return };
    right_click(&mut ui, "Group Production");
    ui.click("Edit group…");
    ui.click("Add connections…");
    // The list has its own filter, drawn last over the panel's.
    ui.click_last("Filter connections");
    ui.type_text("repo");
    ui.h.run_steps(2);
    assert!(ui.h.query_all_by_label("sandbox").count() == 1, "the filter left sandbox in the list");
    ui.click_last("reports");
    ui.click("Save");
    assert_eq!(members(&ui, PROD), [11, 12, 13, 14]);
    assert_eq!(members(&ui, STAGE), [2]);
}
