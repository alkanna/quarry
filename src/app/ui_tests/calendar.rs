//! The calendar of the inline editors, offline on the demo data: clicks inside it, fast or slow, must not close the
//! editor or the calendar.

use super::*;

/// Fast taps (0 frames held), plain clicks and slow ones (3 frames, 750 ms of egui time, still a click).
const HOLDS: [usize; 3] = [0, 1, 3];
const LABELS: [&str; 4] = ["Next month", "Choose month and year", "Back to days", "Day 15"];

#[test]
fn clicks_in_the_sql_calendar_keep_the_editor_open() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    ui.h.state_mut().sql_open_cell(0, "created_at".into());
    ui.wait("the cell editor", |a| a.sql_cell.is_some());
    ui.mouse_hold("Pick a date (Alt+Down)", 1);
    let open = |a: &App| a.sql_cell.as_ref().is_some_and(|e| e.pick.is_some());
    assert!(open(ui.app()), "the calendar did not open");
    for frames in HOLDS {
        for label in LABELS {
            ui.mouse_hold(label, frames);
            assert!(open(ui.app()), "{label} held {frames} frames closed the calendar");
        }
    }
    assert!(ui.app().sql_cell.as_ref().is_some_and(|e| e.text.contains("-15 ")), "the picked day is not in the cell");
    // Enter still saves from the calendar, and Esc-free: the edit is staged and the editor closes.
    ui.h.key_press(egui::Key::Enter);
    ui.wait("the edit to be staged", |a| a.sql_cell.is_none());
}

#[test]
fn clicks_in_the_mongo_calendar_keep_the_editor_open() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    ui.h.state_mut().select(false, 0, false);
    ui.h.step();
    let d = ui.app().t().unwrap().docs[0].owned();
    let v = d.get("createdAt").cloned().unwrap();
    ui.h.state_mut().start_inline(&d, vec!["createdAt".into()], v);
    ui.wait("the inline editor", |a| a.inline.is_some());
    ui.mouse_hold("Pick a date (Alt+Down)", 1);
    let open = |a: &App| a.inline.as_ref().is_some_and(|i| i.pick.is_some());
    assert!(open(ui.app()), "the calendar did not open");
    for frames in HOLDS {
        for label in LABELS {
            ui.mouse_hold(label, frames);
            assert!(open(ui.app()), "{label} held {frames} frames closed the Mongo calendar");
        }
    }
}
