//! Vim mode in the Shell editor, on the offline demo.

use super::*;

fn shell(a: &mut App) {
    a.load_demo();
    a.prefs.vim = true;
    let t = &mut a.tabs[1];
    t.view = View::Shell;
    t.shell.code = "let a = foo;".into();
    a.cur = 1;
}

fn code(ui: &UiApp) -> String {
    ui.app().tabs[1].shell.code.clone()
}

#[test]
fn vim_keys_edit_the_shell_and_esc_leaves_insert_before_the_editor() {
    let Some(mut ui) = demo_ui(shell) else { return };
    ui.mouse_hold("Shell editor", 0);
    // A click into the editor starts in Normal mode, so letters are commands, not text.
    ui.type_text("0w");
    // Keys after one that starts Insert mode in the same frame are typed, not lost.
    ui.type_text("ciwbc");
    assert_eq!(code(&ui), "let bc = foo;");
    // The first Esc only leaves Insert mode: the editor keeps the focus and `x` deletes.
    ui.h.key_press(egui::Key::Escape);
    ui.settle();
    assert!(ui.h.ctx.memory(|m| m.focused().is_some()), "Esc in Insert mode left the editor");
    ui.type_text("x");
    assert_eq!(code(&ui), "let b = foo;");
    ui.type_text("u");
    assert_eq!(code(&ui), "let bc = foo;");
    // In Normal mode Esc leaves the editor as it always did.
    ui.h.key_press(egui::Key::Escape);
    ui.settle();
    assert!(ui.h.ctx.memory(|m| m.focused().is_none()), "Esc in Normal mode kept the focus");
}

#[test]
fn with_vim_off_the_shell_types_as_before() {
    let Some(mut ui) = demo_ui(|a| {
        shell(a);
        a.prefs.vim = false;
    }) else {
        return;
    };
    ui.mouse_hold("Shell editor", 0);
    ui.type_text("x");
    assert_eq!(code(&ui), "let a = foo;x");
}

#[test]
fn ctrl_shortcuts_still_reach_the_app_in_normal_mode() {
    let Some(mut ui) = demo_ui(shell) else { return };
    ui.mouse_hold("Shell editor", 0);
    // On macOS Ctrl is not the command key, so Vim must let Ctrl+Tab through as well as Cmd shortcuts.
    ui.h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Tab);
    ui.settle();
    assert_ne!(ui.app().cur, 1, "Ctrl+Tab did not switch tabs from a Vim editor");
}

#[test]
fn esc_into_normal_mode_closes_the_suggestions() {
    let Some(mut ui) = demo_ui(shell) else { return };
    ui.mouse_hold("Shell editor", 0);
    ui.type_text("A");
    ui.type_text(" orders.");
    assert!(ui.app().ac.id.is_some() && !ui.app().ac.items.is_empty(), "typing orders. opened no suggestions");
    ui.h.key_press(egui::Key::Escape);
    ui.settle();
    assert!(ui.app().ac.id.is_none() || ui.app().ac.items.is_empty(), "the suggestions stayed open in Normal mode");
    assert!(ui.h.ctx.memory(|m| m.focused().is_some()), "Esc with suggestions open left the editor");
}

#[test]
fn text_typed_with_esc_in_one_frame_goes_in_before_normal_mode() {
    let Some(mut ui) = demo_ui(shell) else { return };
    ui.mouse_hold("Shell editor", 0);
    ui.type_text("A");
    ui.h.event(egui::Event::Text("yz".into()));
    ui.h.key_press(egui::Key::Escape);
    ui.settle();
    assert_eq!(code(&ui), "let a = foo;yz");
    ui.type_text("x");
    assert_eq!(code(&ui), "let a = foo;y", "the keys after Esc were not Normal mode commands");
}

#[test]
fn ctrl_r_redoes_only_where_ctrl_is_not_the_command_key() {
    let Some(mut ui) = demo_ui(shell) else { return };
    ui.mouse_hold("Shell editor", 0);
    ui.type_text("0xu");
    assert_eq!(code(&ui), "let a = foo;");
    // On Windows and Linux Ctrl+R is Reload, so Vim leaves it to the app.
    ui.h.key_press_modifiers(egui::Modifiers { ctrl: true, command: true, ..Default::default() }, egui::Key::R);
    ui.settle();
    assert_eq!(code(&ui), "let a = foo;", "Ctrl+R as the command key redid in the editor");
    // On macOS Ctrl is its own key, and Ctrl+R is Vim's redo.
    ui.h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::R);
    ui.settle();
    assert_eq!(code(&ui), "et a = foo;");
}

#[test]
fn a_vim_edit_counts_as_an_edit_of_the_stage() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_demo();
        a.prefs.vim = true;
        let t = &mut a.tabs[1];
        t.view = View::Aggregate;
        t.stages = vec![Stage::new("$sort", "{ a: 1 }", true)];
        t.agg_txt = text_from_stages(&t.stages);
        t.stages_src = t.agg_txt.clone();
        a.cur = 1;
    }) else {
        return;
    };
    let before = ui.app().tabs[1].agg_txt.clone();
    ui.mouse_hold("Stage 1 editor", 0);
    ui.type_text("0x");
    assert_eq!(ui.app().tabs[1].stages[0].text, " a: 1 }");
    assert_ne!(ui.app().tabs[1].agg_txt, before, "the pipeline text missed the Vim edit of its stage");
}
