//! Aggregate stage cards drag to reorder with the shared engine, offline on the demo data.

use super::*;

fn three_stages(a: &mut App) {
    a.load_demo();
    let t = &mut a.tabs[1];
    t.view = View::Aggregate;
    t.stages = vec![Stage::new("$match", "{ a: 1 }", true), Stage::new("$group", "{ _id: '$a' }", true), Stage::new("$sort", "{ _id: 1 }", true)];
    t.agg_txt = text_from_stages(&t.stages);
    t.stages_src = t.agg_txt.clone();
    a.cur = 1;
}

fn ops(ui: &UiApp) -> Vec<String> {
    ui.app().tabs[1].stages.iter().map(|s| s.op.clone()).collect()
}

/// A point on the header of stage `n` (from 1), left of its operator, which no widget of the card takes.
fn head_of(ui: &UiApp, n: usize) -> egui::Pos2 {
    let e = ui.h.get_by_label(&format!("Stage {n} editor")).rect();
    egui::pos2(e.left() + 3.0, e.top() - 14.0)
}

#[test]
fn a_stage_moves_by_dragging_its_card_and_esc_cancels() {
    let Some(mut ui) = demo_ui(three_stages) else { return };
    ui.show("Stage 3 editor");
    ui.settle();
    let to = ui.h.get_by_label("Stage 3 editor").rect().left_bottom() + egui::vec2(3.0, 0.0);
    let from = head_of(&ui, 1);
    ui.press_and_move(from, to);
    ui.h.key_press(egui::Key::Escape);
    ui.h.step();
    ui.release(to);
    assert_eq!(ops(&ui), ["$match", "$group", "$sort"], "Esc did not cancel the drag");
    let from = head_of(&ui, 1);
    ui.press_and_move(from, to);
    ui.release(to);
    assert_eq!(ops(&ui), ["$group", "$sort", "$match"], "the stage did not land after the last one");
    let t = &ui.app().tabs[1];
    assert!(t.agg_txt.find("$group").unwrap() < t.agg_txt.find("$match").unwrap(), "the pipeline text did not follow the stages");
    // A click in an editor still edits it rather than starting a drag.
    ui.mouse_hold("Stage 1 editor", 0);
    ui.type_text("x");
    assert!(ui.app().tabs[1].stages[0].text.contains('x'), "a click in the editor did not focus it");
}
