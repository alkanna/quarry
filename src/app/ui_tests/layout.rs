//! Layout on the offline demo: the palette's and the tab list's rows start under their search field, and a stepper is
//! one field high.

use super::*;
use crate::ui::theme::{BUTTON_H, DIALOG_MARGIN};

/// No section caption, group head or row starts above the bottom of the search field in `area`. The palette's list
/// once started at the palette's top and drew over the field.
fn assert_below_field(ui: &UiApp, area: egui::Rect, what: &str) {
    // The field takes the keys as soon as the list opens.
    let field = ui.h.query_all_by(|n| n.role() == egui::Role::TextInput && n.is_focused()).map(|n| n.rect()).next().unwrap_or_else(|| panic!("{what}: no focused search field"));
    assert!(area.contains(field.center()), "{what}: the focused field at {field:?} is outside the list's popup at {area:?}");
    let items: Vec<egui::Rect> = ui.h.query_all_by(|n| matches!(n.role(), egui::Role::Heading | egui::Role::ListBoxOption)).map(|n| n.rect()).collect();
    assert!(items.len() > 1, "{what}: the list is empty");
    for r in items {
        assert!(r.top() >= field.bottom(), "{what}: a list item at {r:?} starts above the bottom of the search field at {field:?}");
    }
}

/// Opens the palette with `key` at `size`; it keeps its list under the field and stays inside the window's margin.
fn check_palette(key: egui::Key, size: egui::Vec2, what: &str) {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    ui.h.set_size(size);
    ui.h.run_steps(4);
    ui.h.key_press_modifiers(egui::Modifiers::COMMAND, key);
    ui.settle();
    assert!(matches!(ui.app().modal, Some(Modal::Palette { .. })), "{what}: the palette did not open");
    let pal = ui.h.ctx.memory(|m| m.area_rect(egui::Id::unique(("quarry-layer", "palette")))).expect("the palette is not on screen");
    assert_below_field(&ui, pal, what);
    let screen = ui.h.ctx.content_rect();
    assert!(pal.bottom() <= screen.bottom() - DIALOG_MARGIN + 1.0, "{what}: the palette at {pal:?} runs into the window's bottom margin");
}

/// The "n tabs" menu of an overflowing tab strip, at `size`.
fn check_tab_list(size: egui::Vec2) {
    let load = |a: &mut App| {
        a.load_demo();
        for _ in 0..20 {
            a.tabs.push(Tab::new(a.tabs[0].sel.clone()));
        }
    };
    let Some(mut ui) = demo_ui(load) else { return };
    ui.h.set_size(size);
    ui.h.run_steps(4);
    let n = ui.app().tabs.len();
    ui.click(&format!("{n} tabs"));
    ui.h.run_steps(4);
    let row = ui.h.query_all_by(|n| n.role() == egui::Role::ListBoxOption).map(|n| n.rect()).next().expect("the tab list shows no tab");
    let pop = ui.h.ctx.memory(|m| m.areas().visible_layer_ids()).into_iter().filter(|l| l.order == egui::Order::Foreground).filter_map(|l| ui.h.ctx.memory(|m| m.area_rect(l.id))).find(|r| r.contains(row.center())).expect("no popup around the tab list");
    assert_below_field(&ui, pop, &format!("the tab list at {size:?}"));
}

#[test]
fn the_palettes_list_starts_under_its_search_field() {
    for size in [egui::vec2(1400.0, 900.0), egui::vec2(800.0, 420.0)] {
        check_palette(egui::Key::K, size, &format!("Cmd+K at {size:?}"));
        check_palette(egui::Key::T, size, &format!("Cmd+T at {size:?}"));
    }
}

#[test]
fn the_tab_lists_rows_start_under_its_filter() {
    for size in [egui::vec2(1400.0, 900.0), egui::vec2(800.0, 420.0)] {
        check_tab_list(size);
    }
}

#[test]
fn the_columns_stepper_is_one_field_high_and_steps() {
    let load = |a: &mut App| {
        a.load_demo();
        a.modal = Some(Modal::Settings);
        a.settings_cat = crate::app::settings::SettingsCat::Documents;
    };
    let Some(mut ui) = demo_ui(load) else { return };
    ui.show("Columns shown");
    let well = ui.h.query_all_by(|n| n.role() == egui::Role::SpinButton).map(|n| n.rect()).next().expect("no stepper in Settings > Documents");
    assert!((well.height() - BUTTON_H).abs() <= 1.0, "the stepper is {} high, not one field ({BUTTON_H})", well.height());
    for b in ["Decrease", "Increase"] {
        let r = ui.h.get_by_label(b).rect();
        assert!(well.contains_rect(r), "\"{b}\" at {r:?} is outside the stepper at {well:?}");
        assert!((r.center().y - well.center().y).abs() <= 0.5, "\"{b}\" is not centered in the stepper: {} against {}", r.center().y, well.center().y);
    }
    let n = ui.app().prefs.default_cols;
    ui.click("Increase");
    assert_eq!(ui.app().prefs.default_cols, n + 1);
    ui.click("Decrease");
    assert_eq!(ui.app().prefs.default_cols, n);
}
