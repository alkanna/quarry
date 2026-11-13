//! The dialog shell on the offline demo: the close button, typed confirmations, short forms, the export dialogs' radio
//! cards and switches, the server window's hidden-rows row, removable role pills and the connections menu.

use super::*;

fn disabled(ui: &UiApp, label: &str) -> bool {
    // The last match is the topmost one: the page behind a dialog can carry a button of the same name, such as the document view's Duplicate.
    ui.h.query_all_by_label(label).last().expect("no widget with this name").accesskit_node().is_disabled()
}

#[test]
fn the_close_button_closes_a_dialog() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_demo();
        a.demo_dialog("unlock");
    }) else {
        return;
    };
    ui.show("Allow writes");
    ui.click("Close dialog");
    assert!(ui.app().modal.is_none(), "the dialog stayed open");
}

#[test]
fn a_typed_confirmation_arms_only_on_the_exact_name() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_demo();
        a.demo_dialog("typed");
    }) else {
        return;
    };
    ui.show("Confirm");
    assert!(disabled(&ui, "Confirm"), "Confirm is armed before the name is typed");
    ui.fill("Confirmation", "ERS");
    assert!(disabled(&ui, "Confirm"), "the check is case sensitive");
    let Some(Modal::Typed { typed, .. }) = &mut ui.h.state_mut().modal else { panic!("the dialog closed") };
    *typed = "orders".into();
    ui.settle();
    assert!(!disabled(&ui, "Confirm"), "Confirm stays off with the exact name typed");
    ui.h.key_press(egui::Key::Escape);
    ui.settle();
    assert!(ui.app().modal.is_none(), "Esc left the confirmation open");
}

#[test]
fn delete_cancel_keeps_the_dialog_from_acting() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_demo();
        a.demo_dialog("delete");
    }) else {
        return;
    };
    let before = ui.app().tabs[1].docs.len();
    ui.click("Cancel");
    assert!(ui.app().modal.is_none());
    assert_eq!(ui.app().tabs[1].docs.len(), before);
}

#[test]
fn a_taken_name_blocks_duplicate_and_says_why() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_demo();
        a.demo_dialog("duplicate");
    }) else {
        return;
    };
    ui.show("Duplicate");
    assert!(disabled(&ui, "Duplicate"), "Duplicate onto an existing collection is offered");
    assert!(ui.h.query_by_label_contains("already exists").is_some(), "no note says the name is taken");
    let Some(Modal::DuplicateColl { name, .. }) = &mut ui.h.state_mut().modal else { panic!("the dialog closed") };
    *name = "customers_copy".into();
    ui.settle();
    assert!(!disabled(&ui, "Duplicate"));
    assert!(ui.h.query_by_label_contains("already exists").is_none());
}

#[test]
fn a_new_view_needs_a_source_and_a_pipeline_that_parses() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_demo();
        a.demo_dialog("newview");
    }) else {
        return;
    };
    ui.show("Create view");
    assert!(!disabled(&ui, "Create view"));
    assert!(ui.h.query_by_label("Pipeline parses").is_some());
    let Some(Modal::NewCollection { source, pipeline, .. }) = &mut ui.h.state_mut().modal else { panic!("the dialog closed") };
    source.clear();
    *pipeline = "[ { $match: ".into();
    ui.settle();
    assert!(disabled(&ui, "Create view"), "a view with no source can be created");
    assert!(ui.h.query_by_label("Pipeline parses").is_none(), "a broken pipeline is said to parse");
}

#[test]
fn the_dump_switches_and_radio_cards_change_the_form() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_sql_demo();
        a.demo_sql_dump();
    }) else {
        return;
    };
    let path = |ui: &UiApp| match &ui.app().modal {
        Some(Modal::SqlDump(f)) => f.path().to_string(),
        _ => panic!("the dump dialog closed"),
    };
    assert!(!path(&ui).ends_with(".gz"));
    // The option row also has a title label of the same name, drawn before its switch.
    ui.click_last("Compress with gzip");
    assert!(path(&ui).ends_with(".sql.gz"), "the switch did not change the file name: {}", path(&ui));
    // The option row also has a title label of the same name, drawn before its switch.
    ui.click_last("Compress with gzip");
    assert!(path(&ui).ends_with(".sql"), "the switch did not take .gz off: {}", path(&ui));
    ui.click("Whole database");
    assert!(matches!(&ui.app().modal, Some(Modal::SqlDump(f)) if f.whole()), "the radio card did not pick the whole database");
    // Table sizes arrive after the dialog opens and show in the footer.
    ui.h.state_mut().sql_sizes_done(0, Ok(vec![("customers".into(), 5 * 1024 * 1024)]));
    ui.settle();
    assert!(ui.h.query_by_label_contains("on the server").is_some(), "the size estimate is missing from the footer");
}

#[test]
fn hidden_sleeping_connections_are_counted_and_can_be_shown() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_sql_demo();
        a.sql_server = Some(sql::server::SqlServerWin::demo(3, "processes"));
    }) else {
        return;
    };
    ui.show("Show them");
    assert!(ui.h.query_by_label_contains("sleeping connections hidden").is_some());
    ui.click("Show them");
    assert!(ui.h.query_by_label("Show them").is_none(), "the hidden row stayed after Show them");
    assert_eq!(ui.h.get_by_label("Hide sleeping").accesskit_node().toggled(), Some(egui::accesskit::Toggled::False), "Hide sleeping is still on");
}

#[test]
fn a_role_pill_cross_takes_the_role_away_and_the_chip_gives_it_back() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_sql_demo();
        a.sql_users_demo("useraccount");
    }) else {
        return;
    };
    ui.show("Remove app_read");
    ui.click("Remove app_read");
    let roles = |ui: &UiApp| ui.app().sql_users.as_ref().map(|w| w.roles_now.clone()).unwrap_or_default();
    assert!(!roles(&ui).iter().any(|r| r == "app_read@%"), "the cross did not take the role away");
    // Taken away, it stays as a chip that gives it back.
    ui.click("app_read");
    assert!(roles(&ui).iter().any(|r| r == "app_read@%"), "the chip did not give the role back");
}

#[test]
fn the_connections_menu_keeps_its_actions_on_screen_with_many_connections() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_demo();
        for i in 0..40u64 {
            a.conns.push(Conn::new(ConnMeta { id: 100 + i, name: format!("cluster-{i:02}"), ..Default::default() }, Status::Idle));
        }
    }) else {
        return;
    };
    ui.h.get_by_label("Connections").click();
    ui.settle();
    ui.show("+  New connection…");
    let screen = ui.h.ctx.content_rect();
    let action = ui.h.get_by_label("+  New connection…").rect();
    assert!(screen.contains_rect(action), "New connection is off screen at {action:?} in {screen:?}");
    // The far end of the list is still reachable, by scrolling the list.
    ui.click("cluster-39");
    assert_eq!(ui.app().active, Some(139), "the last connection could not be picked");
}

#[test]
fn the_close_button_of_an_agent_approval_denies_it() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_demo();
        a.agents_demo_view("approval");
    }) else {
        return;
    };
    ui.show("Allow once");
    ui.click("Close dialog");
    assert_eq!(ui.app().agents.answered.len(), 1, "the close button did nothing");
    assert!(matches!(ui.app().agents.answered[0].1, crate::app::agents::Answer::Deny), "the close button did not deny");
}

#[test]
fn a_short_dialog_after_a_tall_one_shrinks_to_its_content() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_demo();
        a.demo_dialog("unlock");
    }) else {
        return;
    };
    // Every modal shares this Area, so it carries the last dialog's size into the next one.
    let id = egui::Id::unique(("quarry-layer", "modal"));
    let height = |ui: &UiApp| ui.h.ctx.memory(|m| m.area_rect(id)).expect("no modal area").height();
    ui.settle();
    let short = height(&ui);
    ui.h.state_mut().demo_dialog("save");
    ui.settle();
    assert!(height(&ui) > short + 40.0, "the save dialog is not taller than the unlock one, so this test proves nothing");
    ui.h.state_mut().demo_dialog("unlock");
    ui.settle();
    assert!((height(&ui) - short).abs() < 2.0, "the unlock dialog kept the save dialog's height: {} against {short}", height(&ui));
    // The footer row must sit inside the band's padding, not overhang the dialog.
    let area = ui.h.ctx.memory(|m| m.area_rect(id)).expect("no modal area");
    let button = ui.h.get_by_label("Allow writes").rect();
    // 460 px plus the frame's 1 px border on each side.
    assert!(area.width() <= 463.0, "the unlock dialog grew past its 460 px: {}", area.width());
    assert!(button.right() <= area.right() - crate::ui::theme::DIALOG_PAD + 1.0, "the primary button overhangs the footer padding: {} against {}", button.right(), area.right());
}
