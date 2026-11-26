//! The Windows header on the offline demo: the window buttons sit at the right edge, clear of the header's own controls.

use super::*;

#[test]
fn window_buttons_sit_at_the_right_of_the_header_clear_of_its_controls() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    ui.h.state_mut().ui.win_buttons = true;
    ui.h.run_steps(3);
    // Other widgets are named Close too, such as a dialog's; the window button is the one in the header.
    let rect = |ui: &UiApp, n: &str| ui.h.query_all_by_label(n).map(|n| n.rect()).find(|r| r.top() < HEADER_H).unwrap_or_else(|| panic!("no {n} button in the header"));
    let (min, max, close) = (rect(&ui, "Minimize"), rect(&ui, "Maximize"), rect(&ui, "Close"));
    assert!((close.right() - 1400.0).abs() < 0.5 && close.top().abs() < 0.5, "Close is not in the top right corner: {close:?}");
    assert!(min.right() <= max.left() + 0.5 && max.right() <= close.left() + 0.5, "the buttons are out of order: {min:?} {max:?} {close:?}");
    assert!((close.height() - crate::ui::theme::HEADER_H).abs() < 0.5, "Close is not as tall as the header: {close:?}");
    let undo = ui.h.query_all_by_label_contains("Undo").next().expect("no Undo button").rect();
    assert!(undo.right() <= min.left(), "Undo at {undo:?} runs under Minimize at {min:?}");
}
