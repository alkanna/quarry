//! Vim mode in the Shell editor, on the offline demo.

use super::*;

fn shell(a: &mut App) {
    a.load_demo();
    a.core.prefs.vim = true;
    let t = &mut a.core.tabs[1];
    t.view = View::Shell;
    t.shell.code = "let a = foo;".into();
    a.core.cur = 1;
}

fn code(ui: &UiApp) -> String {
    ui.app().core.tabs[1].shell.code.clone()
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
        a.core.prefs.vim = false;
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
    assert_ne!(ui.app().core.cur, 1, "Ctrl+Tab did not switch tabs from a Vim editor");
}

#[test]
fn esc_into_normal_mode_closes_the_suggestions() {
    let Some(mut ui) = demo_ui(shell) else { return };
    ui.mouse_hold("Shell editor", 0);
    ui.type_text("A");
    ui.type_text(" orders.");
    assert!(ui.app().core.ac.id.is_some() && !ui.app().core.ac.items.is_empty(), "typing orders. opened no suggestions");
    ui.h.key_press(egui::Key::Escape);
    ui.settle();
    assert!(ui.app().core.ac.id.is_none() || ui.app().core.ac.items.is_empty(), "the suggestions stayed open in Normal mode");
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
        a.core.prefs.vim = true;
        let t = &mut a.core.tabs[1];
        t.view = View::Aggregate;
        t.stages = vec![Stage::new("$sort", "{ a: 1 }", true)];
        t.agg_txt = text_from_stages(&t.stages);
        t.stages_src = t.agg_txt.clone();
        a.core.cur = 1;
    }) else {
        return;
    };
    let before = ui.app().core.tabs[1].agg_txt.clone();
    ui.mouse_hold("Stage 1 editor", 0);
    ui.type_text("0x");
    assert_eq!(ui.app().core.tabs[1].stages[0].text, " a: 1 }");
    assert_ne!(ui.app().core.tabs[1].agg_txt, before, "the pipeline text missed the Vim edit of its stage");
}

fn sql_vim(a: &mut App) {
    a.load_sql_demo();
    a.core.prefs.vim = true;
}

fn sel(ui: &UiApp) -> Option<usize> {
    ui.app().t().and_then(|t| t.selected).map(|s| s.1)
}

fn vline(ui: &UiApp) -> usize {
    ui.app().t().map_or(0, |t| t.vline)
}

fn vcol(ui: &UiApp) -> usize {
    ui.app().t().map_or(0, |t| t.vcol)
}

#[test]
fn vim_keys_move_the_grid_cursor_by_row_and_column() {
    let Some(mut ui) = demo_ui(sql_vim) else { return };
    ui.type_text("j");
    assert_eq!(sel(&ui), Some(0), "the first j did not pick the first row");
    ui.type_text("2j");
    assert_eq!(sel(&ui), Some(2));
    ui.type_text("k");
    assert_eq!(sel(&ui), Some(1));
    ui.type_text("5l");
    assert_eq!((sel(&ui), vcol(&ui)), (Some(1), 5), "l moved the row or missed the column");
    ui.type_text("$");
    assert_eq!(vcol(&ui), 6);
    ui.type_text("0");
    assert_eq!(vcol(&ui), 0);
    ui.type_text("G");
    assert_eq!(sel(&ui), Some(12));
    // A g waits for the next key, even when it comes in a later frame.
    ui.type_text("g");
    ui.type_text("g");
    assert_eq!(sel(&ui), Some(0));
    ui.type_text("4G");
    assert_eq!(sel(&ui), Some(3));
}

#[test]
fn i_edits_the_cell_under_the_cursor() {
    let Some(mut ui) = demo_ui(sql_vim) else { return };
    ui.type_text("j5li");
    ui.wait("the cell editor on note", |a| a.core.sql_cell.as_ref().is_some_and(|e| e.col == "note"));
}

#[test]
fn in_the_record_j_and_k_walk_its_lines_and_tab_goes_back_to_the_grid() {
    let Some(mut ui) = demo_ui(sql_vim) else { return };
    // The record opens on the field of the grid's cursor.
    ui.type_text("jl");
    ui.h.key_press(egui::Key::Tab);
    ui.settle();
    assert!(ui.app().t().is_some_and(|t| t.record), "Tab did not open the record");
    assert_eq!(vline(&ui), 1, "the record did not open on the grid cursor's field");
    ui.type_text("3j");
    assert_eq!((sel(&ui), vline(&ui)), (Some(0), 4), "j in the record moved the row instead of its line");
    ui.type_text("k");
    assert_eq!(vline(&ui), 3);
    ui.type_text("G");
    assert_eq!((sel(&ui), vline(&ui), vcol(&ui)), (Some(0), 6, 1), "walking the record moved the grid's cursor");
    // i edits the record's line, drawn in the record.
    ui.type_text("ki");
    ui.wait("the cell editor on note", |a| a.core.sql_cell.as_ref().is_some_and(|e| e.col == "note"));
    ui.h.key_press(egui::Key::Escape);
    ui.wait("the editor to close", |a| a.core.sql_cell.is_none());
    ui.h.key_press(egui::Key::Tab);
    ui.settle();
    assert!(ui.app().t().is_some_and(|t| !t.record), "Tab did not close the record");
    // Back in the grid the cursor is where it was before the record.
    ui.type_text("j");
    assert_eq!((sel(&ui), vcol(&ui)), (Some(1), 1));
}

#[test]
fn with_vim_off_letters_do_not_move_the_grid() {
    let Some(mut ui) = demo_ui(App::load_sql_demo) else { return };
    ui.type_text("jjl");
    assert_eq!(sel(&ui), None);
}

#[test]
fn ctrl_h_and_ctrl_l_move_the_keys_between_the_sidebar_and_the_grid() {
    let Some(mut ui) = demo_ui(sql_vim) else { return };
    ui.h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::H);
    ui.settle();
    let first = ui.h.ctx.memory(|m| m.focused());
    assert!(first.is_some() && !ui.h.ctx.text_edit_focused(), "Ctrl+H focused no sidebar row");
    // In the sidebar j is Down, which moves egui's focus to the next row.
    ui.type_text("j");
    let next = ui.h.ctx.memory(|m| m.focused());
    assert!(next.is_some() && next != first, "j did not move the focus in the sidebar");
    assert_eq!(sel(&ui), None, "j in the sidebar moved the grid");
    ui.h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::L);
    ui.settle();
    assert!(ui.h.ctx.memory(|m| m.focused().is_none()), "Ctrl+L left the focus in the sidebar");
    assert_eq!(sel(&ui), Some(0), "Ctrl+L did not put the cursor in the grid");
    ui.type_text("j");
    assert_eq!(sel(&ui), Some(1));
}

#[test]
fn ctrl_k_focuses_the_views_editor_and_ctrl_j_leaves_it() {
    let Some(mut ui) = demo_ui(shell) else { return };
    // The Shell editor is drawn after the first frame, which is when Ctrl+K can find it.
    ui.settle();
    ui.h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::K);
    ui.settle();
    assert!(ui.h.ctx.text_edit_focused(), "Ctrl+K did not focus the Shell editor");
    // In Normal mode Ctrl+J leaves the editor; the editor's own keys never see it.
    ui.h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::J);
    ui.settle();
    assert!(!ui.h.ctx.text_edit_focused(), "Ctrl+J left the focus in the editor");
    assert_eq!(code(&ui), "let a = foo;");
}

#[test]
fn ctrl_k_focuses_the_mongo_query() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_demo();
        a.core.prefs.vim = true;
    }) else {
        return;
    };
    ui.h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::K);
    ui.settle();
    assert!(ui.h.ctx.memory(|m| m.has_focus(crate::app::query::query_id("QUERY"))), "Ctrl+K did not focus the query");
}

fn modal_title(ui: &UiApp) -> Option<String> {
    match &ui.app().modal {
        Some(Modal::Text { title, .. }) => Some(title.clone()),
        _ => None,
    }
}

#[test]
fn ctrl_l_leaves_an_open_cell_editor_alone() {
    let Some(mut ui) = demo_ui(sql_vim) else { return };
    ui.type_text("j5li");
    ui.wait("the cell editor on note", |a| a.core.sql_cell.as_ref().is_some_and(|e| e.col == "note"));
    ui.type_text("Side gate");
    ui.h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::L);
    ui.settle();
    assert!(ui.app().core.sql_cell.as_ref().is_some_and(|e| e.col == "note"), "Ctrl+L closed the cell editor and dropped what was typed");
}

#[test]
fn gg_and_g_in_the_sidebar_leave_the_focus_on_its_row() {
    let Some(mut ui) = demo_ui(sql_vim) else { return };
    ui.h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::H);
    ui.settle();
    let row = ui.h.ctx.memory(|m| m.focused());
    assert!(row.is_some(), "Ctrl+H focused no sidebar row");
    ui.type_text("G");
    ui.type_text("gg");
    assert_eq!(ui.h.ctx.memory(|m| m.focused()), row, "G or gg moved the focus out of the sidebar");
}

#[test]
fn the_query_record_opens_on_the_pinned_column_the_grid_cursor_is_on() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_sql_demo();
        a.sql_query_demo();
        a.core.prefs.vim = true;
        let s = a.tm().and_then(|t| t.sql.as_mut()).unwrap();
        let keys = crate::app::sql::query::result_keys(&s.sets[0].cols);
        s.qprefs.insert(keys, ColPrefs { pinned: vec!["email".into()], ..Default::default() });
    }) else {
        return;
    };
    // The first column on screen is the pinned email, not the result's first.
    ui.type_text("j");
    ui.h.key_press(egui::Key::Tab);
    ui.settle();
    assert!(ui.app().t().is_some_and(|t| t.record), "Tab did not open the record");
    ui.type_text("v");
    assert_eq!(modal_title(&ui).as_deref(), Some("email"), "the record's cursor was not on the grid cursor's column");
}

/// Where Alt stands in for Ctrl, winit sends the letter as text after the key; it must not also run as a Vim key.
#[cfg(not(target_os = "macos"))]
#[test]
fn the_letter_after_an_alt_screen_key_is_not_a_second_command() {
    let Some(mut ui) = demo_ui(sql_vim) else { return };
    ui.type_text("5jll");
    assert_eq!((sel(&ui), vcol(&ui)), (Some(4), 2));
    ui.h.event(egui::Event::Key { key: egui::Key::B, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::ALT });
    ui.h.event(egui::Event::Text("b".into()));
    ui.settle();
    assert_eq!((sel(&ui), vcol(&ui)), (Some(0), 2), "Alt+B moved a column as well as a screen up");
}
