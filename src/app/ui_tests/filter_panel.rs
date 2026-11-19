//! The filter panel of the SQL Content bar, offline on the demo: opening and closing it, adding rows and groups, the keys of
//! its rows, Apply, and Edit as SQL and back.

use super::*;
use crate::app::run::query_dirty;
use crate::db::sql::filter::{self, Item, Match};
use crate::db::sql::rules::Op;

fn bar(ui: &UiApp) -> &crate::app::sql::bar::Bar {
    ui.app().t().and_then(|t| t.sql.as_ref()).map(|s| &s.bar).expect("a SQL tab")
}

pub(super) fn bar_mut(ui: &mut UiApp) -> &mut crate::app::sql::bar::Bar {
    let app = ui.h.state_mut();
    let cur = app.cur;
    &mut app.tabs[cur].sql.as_mut().expect("a SQL tab").bar
}

fn filter_txt(ui: &UiApp) -> String {
    ui.app().t().map(|t| t.filter_txt.clone()).unwrap_or_default()
}

/// The demo's orders tab, its WHERE read into the panel, which is open.
pub(super) fn opened() -> Option<UiApp> {
    let mut ui = demo_ui(App::load_sql_demo)?;
    ui.settle();
    ui.click("Filters");
    ui.settle();
    assert!(bar(&ui).panel.open, "the Filters button did not open the panel");
    Some(ui)
}

/// Selects the row or group `i` in drawing order, as a click on its background does.
pub(super) fn select(ui: &mut UiApp, i: usize) {
    let b = bar_mut(ui);
    b.panel.sel = Some(b.tree.rows_in_order()[i]);
    ui.settle();
}

const BOTH: &str = "`status` = 'paid' AND `total` > 50";

#[test]
fn the_filters_button_opens_the_panel_and_esc_closes_it_keeping_the_where() {
    let Some(mut ui) = opened() else { return };
    assert_eq!(filter_txt(&ui), BOTH);
    ui.key(egui::Modifiers::NONE, egui::Key::Escape);
    ui.settle();
    assert!(!bar(&ui).panel.open, "Esc left the panel open");
    assert_eq!(filter_txt(&ui), BOTH);
}

#[test]
fn a_new_condition_writes_the_where_and_marks_the_line_edited() {
    let Some(mut ui) = opened() else { return };
    assert!(!ui.app().t().is_some_and(query_dirty), "an untouched bar is out of date");
    ui.click("+ Condition");
    ui.settle();
    let row = bar(&ui).tree.rows_in_order()[2];
    // The new row opens its column picker; Esc keeps the first column, and the value field takes the number.
    ui.key(egui::Modifiers::NONE, egui::Key::Escape);
    ui.settle();
    assert!(bar(&ui).panel.open, "Esc in the column picker closed the panel");
    let v0 = crate::app::sql::filter_panel::row_salt(row).with("v0");
    ui.h.ctx.memory_mut(|m| m.request_focus(v0));
    ui.settle();
    ui.type_text("7");
    ui.settle();
    assert_eq!(filter_txt(&ui), format!("{BOTH} AND `id` = 7"));
    assert!(ui.app().t().is_some_and(query_dirty), "the new condition did not mark the line edited");
}

#[test]
fn a_press_outside_the_column_picker_closes_it() {
    let Some(mut ui) = opened() else { return };
    ui.click("+ Condition");
    ui.settle();
    let row = bar(&ui).tree.rows_in_order()[2];
    let picker = crate::app::sql::filter_panel::row_salt(row).with("col");
    let open = |ui: &UiApp| crate::app::sql::filter_cells::col_pick_open(&ui.h.ctx, picker);
    assert!(open(&ui), "the new row did not open its column picker");
    // The panel's caption takes no focus, so only the press itself can close the list.
    let at = ui.h.get_by_label("of these conditions").rect().center();
    ui.press_at(at, egui::PointerButton::Primary, 1);
    ui.settle();
    assert!(!open(&ui), "the column list stayed open after a press outside it");
    assert!(bar(&ui).panel.open, "the press outside the picker closed the panel");
}

#[test]
fn group_of_conditions_in_the_plus_menu_adds_an_any_group() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    ui.settle();
    ui.click("Add a condition or clause");
    ui.click("Group of conditions");
    ui.settle();
    let b = bar(&ui);
    assert!(b.panel.open, "the panel did not open on the new group");
    assert!(b.tree.root.kids.iter().any(|n| matches!(&n.item, Item::Group(g) if g.mode == Match::Any && g.kids.len() == 1)), "no any group with a row");
}

#[test]
fn space_turns_the_selected_row_off_and_the_preview_comments_it() {
    let Some(mut ui) = opened() else { return };
    select(&mut ui, 0);
    ui.key(egui::Modifiers::NONE, egui::Key::Space);
    ui.settle();
    assert_eq!(filter_txt(&ui), "`total` > 50");
    let preview = filter::preview(&bar(&ui).tree, crate::db::sql::Dialect::MySql, true);
    assert!(preview.contains("-- off: `status` = 'paid'"), "{preview}");
}

#[test]
fn cmd_g_groups_the_selected_row_and_cmd_shift_g_takes_the_group_apart() {
    let Some(mut ui) = opened() else { return };
    select(&mut ui, 1);
    ui.key(egui::Modifiers::COMMAND, egui::Key::G);
    ui.settle();
    assert_eq!(filter_txt(&ui), "`status` = 'paid' AND (`total` > 50)");
    ui.key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::G);
    ui.settle();
    assert_eq!(filter_txt(&ui), BOTH);
}

#[test]
fn cmd_backspace_removes_the_selected_row() {
    let Some(mut ui) = opened() else { return };
    select(&mut ui, 0);
    ui.key(egui::Modifiers::COMMAND, egui::Key::Backspace);
    ui.settle();
    assert_eq!(filter_txt(&ui), "`total` > 50");
    assert_eq!(bar(&ui).tree.rows_in_order().len(), 1);
}

#[test]
fn cmd_enter_applies_and_closes() {
    let Some(mut ui) = opened() else { return };
    select(&mut ui, 0);
    ui.key(egui::Modifiers::NONE, egui::Key::Space);
    ui.settle();
    assert!(ui.app().t().is_some_and(query_dirty), "turning a row off did not mark the line edited");
    // The demo reads no rows, so the run shows as the page going back to the first.
    let app = ui.h.state_mut();
    let cur = app.cur;
    app.tabs[cur].page = 3;
    ui.key(egui::Modifiers::COMMAND, egui::Key::Enter);
    ui.settle();
    assert!(!bar(&ui).panel.open, "Cmd+Enter left the panel open");
    assert_eq!(ui.app().t().map(|t| t.page), Some(0), "Cmd+Enter did not run the filter");
}

#[test]
fn edit_as_sql_and_back_keeps_the_off_rows() {
    let Some(mut ui) = opened() else { return };
    select(&mut ui, 0);
    ui.key(egui::Modifiers::NONE, egui::Key::Space);
    ui.settle();
    ui.click("Edit as SQL");
    ui.settle();
    assert!(bar(&ui).c.sql && !bar(&ui).panel.open, "Edit as SQL did not switch the line");
    ui.click("Rules");
    ui.settle();
    let b = bar(&ui);
    assert!(!b.c.sql);
    assert_eq!(b.tree.rows_in_order().len(), 2, "the off row was lost on the way through SQL mode");
}

#[test]
fn the_filter_demo_views_draw() {
    for which in ["rules", "filters", "filters-cells", "filters-ops"] {
        let Some(mut ui) = demo_ui(|_| {}) else { return };
        ui.h.state_mut().sql_demo(which);
        ui.settle();
        assert!(ui.h.query_by_label("Filters").is_some(), "QUARRY_TEST_SQL={which} has no Filters button");
        let Some(mut ui) = demo_ui(|_| {}) else { return };
        ui.h.state_mut().pg_demo(which);
        ui.settle();
        assert!(ui.h.query_by_label("Filters").is_some(), "QUARRY_TEST_PG={which} has no Filters button");
        if which != "rules" {
            assert!(bar(&ui).panel.open, "QUARRY_TEST_PG={which} did not open the panel");
            assert!(!bar(&ui).c.sql, "QUARRY_TEST_PG={which} fell back to SQL mode");
        }
    }
}

/// A row wider than the panel pushes the SQL pane and Apply off the window, so every demo view checks they are on it.
#[test]
fn the_panel_fits_the_window_on_every_demo_view() {
    for which in ["filters", "filters-cells", "filters-ops"] {
        for pg in [false, true] {
            let Some(mut ui) = demo_ui(|_| {}) else { return };
            if pg {
                ui.h.state_mut().pg_demo(which);
            } else {
                ui.h.state_mut().sql_demo(which);
            }
            ui.settle();
            let screen = ui.h.ctx.content_rect();
            for label in ["Apply", "Edit as SQL"] {
                let r = ui.h.get_by_label(label).rect();
                assert!(screen.contains_rect(r), "{which} ({}): {label} at {r:?} is outside the window {screen:?}", if pg { "PostgreSQL" } else { "MySQL" });
            }
        }
    }
}

#[test]
fn a_row_dragged_onto_a_group_header_moves_into_the_group() {
    let Some(mut ui) = opened() else { return };
    ui.click("+ Group");
    ui.settle();
    let status = bar(&ui).tree.rows_in_order()[0];
    ui.drag("Condition status", "Group", 0.5, 2);
    let b = bar(&ui);
    let group = b.tree.root.kids.iter().find_map(|n| matches!(n.item, Item::Group(_)).then_some(n.id)).expect("the group");
    assert_eq!(b.tree.parent_of(status), Some(group), "the row did not land in the group");
    assert_eq!(filter_txt(&ui), "`total` > 50 AND (`status` = 'paid')", "the new row of the group is not complete, so only the moved one writes SQL");
}

fn first_rule(ui: &UiApp) -> crate::db::sql::rules::Rule {
    let b = bar(ui);
    let id = b.tree.rows_in_order()[0];
    match &b.tree.find(id).expect("the first row").item {
        Item::Cond(c) => c.rule.clone(),
        _ => panic!("the first row is a group"),
    }
}

#[test]
fn typing_in_the_operator_pill_finds_and_picks_an_operator() {
    for pg in [false, true] {
        let Some(mut ui) = demo_ui(|_| {}) else { return };
        if pg {
            ui.h.state_mut().pg_demo("filters-ops");
        } else {
            ui.h.state_mut().sql_demo("filters-ops");
        }
        ui.settle();
        // The demo opens the picker of the note row, so typing goes to its field.
        ui.type_text("starts");
        ui.show("Writes");
        ui.h.key_press(egui::Key::Enter);
        ui.settle();
        assert_eq!(first_rule(&ui).op, Op::StartsWith, "Enter did not pick the operator found by typing");
        ui.click("Operator starts with");
        ui.type_text("null");
        // Down moves to the second match.
        ui.h.key_press(egui::Key::ArrowDown);
        ui.h.key_press(egui::Key::Enter);
        ui.settle();
        assert_eq!(first_rule(&ui).op, Op::IsNull, "null then down did not pick the second match, IS NULL");
        ui.click("Operator IS NULL");
        ui.type_text("contains");
        ui.h.key_press(egui::Key::Escape);
        ui.settle();
        assert_eq!(first_rule(&ui).op, Op::IsNull, "Esc changed the operator");
        assert!(bar(&ui).panel.open, "Esc in the picker closed the panel");
    }
}

#[test]
fn the_operator_footer_writes_the_rows_condition() {
    let Some(mut ui) = demo_ui(|_| {}) else { return };
    ui.h.state_mut().sql_demo("filters-ops");
    ui.settle();
    ui.type_text("ends");
    ui.show("Writes");
    assert!(ui.shows("`note`"), "the footer does not write the row's column");
    assert!(ui.shows("gift"), "the footer does not keep the row's value");
}
