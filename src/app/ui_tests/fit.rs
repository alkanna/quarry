//! Dialogs and popovers in a window too short for them, on the offline demo: each stays inside the window with the
//! margin, its body scrolls, and its footer stays on screen and takes a real pointer click.

use super::*;
use crate::ui::theme::DIALOG_MARGIN;

/// The area every app modal is drawn in, Settings included.
fn modal_id() -> egui::Id {
    egui::Id::unique(("quarry-layer", "modal"))
}

/// The demo `load` builds, at 1400x900 for its first frames and then resized to `size` with the dialog open.
fn small(load: fn(&mut App), size: egui::Vec2) -> Option<UiApp> {
    let mut ui = demo_ui(load)?;
    ui.h.set_size(size);
    ui.h.run_steps(6);
    Some(ui)
}

fn area(ui: &UiApp, id: egui::Id) -> egui::Rect {
    ui.h.ctx.memory(|m| m.area_rect(id)).expect("the dialog is not on screen")
}

/// The topmost widget with this name: the page behind a dialog can carry one of the same name.
fn rect_of(ui: &UiApp, label: &str) -> egui::Rect {
    ui.h.query_all_by_label(label).last().unwrap_or_else(|| panic!("no \"{label}\" on screen")).rect()
}

/// The dialog sits inside the window less the margin, its header at its top and `primary` in its footer at its bottom.
fn assert_fits(ui: &UiApp, id: egui::Id, primary: &str, what: &str) {
    let screen = ui.h.ctx.content_rect();
    let a = area(ui, id);
    // A pixel either way for rounding.
    let inside = screen.shrink(DIALOG_MARGIN - 1.0);
    assert!(inside.contains_rect(a), "{what}: the dialog at {a:?} is not inside the window less its margin, {inside:?}");
    let b = rect_of(ui, primary);
    assert!(a.contains_rect(b), "{what}: \"{primary}\" at {b:?} is outside the dialog at {a:?}");
    // The footer band pads its one row of buttons by 12 px, and the frame's border adds one.
    assert!(a.bottom() - b.bottom() <= 14.0, "{what}: the footer is not at the dialog's bottom: \"{primary}\" ends at {} and the dialog at {}", b.bottom(), a.bottom());
    let close = rect_of(ui, "Close dialog");
    assert!(close.top() - a.top() < 40.0, "{what}: the header is not at the dialog's top: its close button is at {}", close.top());
}

/// Checks `assert_fits` at two small sizes, then clicks `click` with the pointer and expects `done` to hold after.
fn check(load: fn(&mut App), primary: &str, click: &str, done: fn(&App) -> bool, what: &str) {
    for size in [egui::vec2(900.0, 500.0), egui::vec2(800.0, 420.0)] {
        let Some(mut ui) = small(load, size) else { return };
        ui.show(primary);
        assert_fits(&ui, modal_id(), primary, &format!("{what} at {size:?}"));
        let at = rect_of(&ui, click).center();
        ui.press_at(at, egui::PointerButton::Primary, 1);
        ui.settle();
        assert!(done(ui.app()), "{what} at {size:?}: a click on \"{click}\" did nothing");
    }
}

fn closed(a: &App) -> bool {
    a.modal.is_none()
}

#[test]
fn the_link_a_folder_dialog_fits_a_small_window() {
    check(
        |a| {
            a.load_demo();
            a.demo_library("link");
        },
        "Link folder",
        "Cancel",
        closed,
        "Link a folder",
    );
}

#[test]
fn the_edit_group_dialog_fits_and_saves() {
    check(
        |a| {
            a.load_demo();
            a.demo_conns("edit");
        },
        "Save",
        "Save",
        closed,
        "Edit group",
    );
}

#[test]
fn settings_fit_a_small_window() {
    check(
        |a| {
            a.load_demo();
            a.modal = Some(Modal::Settings);
        },
        "Done",
        "Done",
        closed,
        "Settings",
    );
}

#[test]
fn the_export_dialog_fits_a_small_window() {
    check(
        |a| {
            a.load_demo();
            a.open_export(Vec::new(), false);
        },
        "Export…",
        "Cancel",
        closed,
        "Export",
    );
}

#[test]
fn the_connection_form_fits_a_small_window() {
    check(
        |a| {
            a.load_demo();
            a.new_conn_modal();
        },
        "Save and connect",
        "Cancel",
        closed,
        "Connection form",
    );
}

#[test]
fn the_new_table_dialog_fits_a_small_window() {
    check(
        |a| {
            a.load_sql_demo();
            a.sql_create_demo("newtable");
        },
        "Create table",
        "Cancel",
        closed,
        "New table",
    );
}

#[test]
fn the_save_to_library_dialog_fits_and_saves() {
    check(
        |a| {
            a.load_demo();
            a.demo_library("save");
        },
        "Save",
        "Save",
        |a| a.modal.is_none() && !a.library.scripts.is_empty(),
        "Save to library",
    );
}

#[test]
fn a_dialog_refits_when_the_window_shrinks_and_grows_back() {
    let load = |a: &mut App| {
        a.load_demo();
        a.demo_library("link");
    };
    let Some(mut ui) = demo_ui(load) else { return };
    ui.show("Link folder");
    let tall = area(&ui, modal_id()).height();
    ui.h.set_size(egui::vec2(800.0, 420.0));
    ui.h.run_steps(3);
    assert_fits(&ui, modal_id(), "Link folder", "after shrinking");
    ui.h.set_size(egui::vec2(1400.0, 900.0));
    ui.h.run_steps(3);
    let back = area(&ui, modal_id()).height();
    assert!((back - tall).abs() < 2.0, "the dialog did not grow back: {back} against {tall}");
}

#[test]
fn a_tall_dialog_after_a_short_one_takes_its_height_at_once() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_demo();
        a.demo_dialog("unlock");
    }) else {
        return;
    };
    ui.settle();
    let short = area(&ui, modal_id()).height();
    ui.h.state_mut().demo_dialog("save");
    ui.h.run_steps(2);
    let first = area(&ui, modal_id()).height();
    assert!(first > short + 40.0, "the save dialog started squeezed to the unlock dialog's height: {first} against {short}");
    ui.settle();
    let later = area(&ui, modal_id()).height();
    assert!((later - first).abs() < 2.0, "the save dialog kept growing after it opened: {first}, then {later}");
}

/// Dialogs that fit must not grow to the window: inside the scroll area, content that fills the height it is offered
/// would now be offered the whole window.
#[test]
fn dialogs_that_fit_keep_their_own_height() {
    let screen = egui::vec2(1400.0, 900.0);
    // The window less the margins and the frame's border.
    let cap = screen.y - 2.0 * DIALOG_MARGIN - 2.0;
    let wide = screen.x - 2.0 * DIALOG_MARGIN - 2.0;
    for which in ["unlock", "delete", "typed", "newcoll", "newview", "rename", "duplicate", "save", "shell", "agg", "column", "text", "update", "undo"] {
        let Some(mut ui) = demo_ui(App::load_demo) else { return };
        ui.h.state_mut().demo_dialog(which);
        ui.settle();
        let Some(a) = ui.h.ctx.memory(|m| m.area_rect(modal_id())) else { continue };
        assert!(a.height() < cap - 2.0, "the {which} dialog grew to the window's height: {}", a.height());
        assert!(a.width() < wide - 2.0, "the {which} dialog grew to the window's width: {}", a.width());
    }
    // The Link a folder dialog is left out: it is about the window's height at this size.
    let loads: [(&str, fn(&mut App)); 5] = [
        ("group", |a| {
            a.load_demo();
            a.demo_conns("edit");
        }),
        ("export", |a| {
            a.load_demo();
            a.open_export(Vec::new(), false);
        }),
        ("conn", |a| {
            a.load_demo();
            a.new_conn_modal();
        }),
        ("newtable", |a| {
            a.load_sql_demo();
            a.sql_create_demo("newtable");
        }),
        ("save to library", |a| {
            a.load_demo();
            a.demo_library("save");
        }),
    ];
    for (which, load) in loads {
        let Some(mut ui) = demo_ui(load) else { return };
        ui.settle();
        let a = area(&ui, modal_id());
        assert!(a.height() < cap - 2.0, "the {which} dialog grew to the window's height: {}", a.height());
        assert!(a.width() < wide - 2.0, "the {which} dialog grew to the window's width: {}", a.width());
    }
}

#[test]
fn the_library_picker_fits_a_small_window_and_its_last_script_is_reachable() {
    let load = |a: &mut App| {
        a.load_sql_demo();
        a.sql_new_query(3, "shop".into());
        a.demo_scripts();
    };
    let Some(mut ui) = small(load, egui::vec2(800.0, 420.0)) else { return };
    ui.click("Library 5");
    ui.show("Search library");
    let search = rect_of(&ui, "Search library");
    let screen = ui.h.ctx.content_rect();
    let pop = ui.h.ctx.memory(|m| m.areas().visible_layer_ids()).into_iter().filter(|l| l.order == egui::Order::Foreground).filter_map(|l| ui.h.ctx.memory(|m| m.area_rect(l.id))).find(|r| r.contains(search.center())).expect("no popover around the search field");
    assert!(screen.contains_rect(pop), "the popover at {pop:?} runs off the window {screen:?}");
    assert!(pop.height() <= screen.height() - 2.0 * DIALOG_MARGIN + 1.0, "the popover is taller than the window allows: {}", pop.height());
    ui.click("Script Late shipments");
    assert!(ui.app().t().and_then(|t| t.sql.as_ref()).is_some_and(|s| s.query.starts_with("-- Paid orders that never shipped")), "the script did not load");
}
