//! The AI agents windows on made-up data: the approval dialog over Settings, the access picker, the rules tester and
//! the status bar pill.

use super::*;
use crate::agents::config::Access;
use crate::app::agents::{Answer, tester_lines};
use std::time::Instant;

fn approval_over_settings(a: &mut App) {
    a.load_demo();
    a.agents_demo_view("approval");
    a.modal = Some(Modal::Settings);
    a.settings_cat = settings::SettingsCat::Agents;
}

#[test]
fn approval_stacks_over_settings_and_takes_no_enter() {
    let Some(mut ui) = demo_ui(approval_over_settings) else { return };
    ui.show("Allow once");
    let top = ui.h.ctx.memory(|m| m.top_modal_layer());
    assert_eq!(top, Some(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique(("quarry-layer", "agents-approval")))), "the approval is not the top modal");
    // Enter is never a default.
    ui.h.key_press(egui::Key::Enter);
    ui.settle();
    assert_eq!(ui.app().agents.asks.len(), 1);
    assert!(ui.app().agents.answered.is_empty());
    // Within the first second after it shows, Allow does nothing.
    ui.h.state_mut().agents.asks[0].shown = Some(Instant::now());
    ui.h.get_by_label("Allow once").click();
    ui.settle();
    assert!(ui.app().agents.answered.is_empty(), "Allow worked in the first second");
    assert_eq!(ui.app().agents.asks.len(), 1);
    // Esc denies, and leaves Settings open behind it.
    ui.h.key_press(egui::Key::Escape);
    ui.settle();
    assert_eq!(ui.app().agents.answered, [(9, Answer::Deny)]);
    assert!(ui.app().agents.asks.is_empty());
    assert!(matches!(ui.app().modal, Some(Modal::Settings)), "Esc closed Settings too");
}

#[test]
fn allow_works_once_awake() {
    let Some(mut ui) = demo_ui(approval_over_settings) else { return };
    ui.show("Allow once");
    ui.h.state_mut().agents.asks[0].shown = Some(Instant::now() - Duration::from_secs(2));
    ui.settle();
    ui.h.get_by_label("Allow once").click();
    ui.settle();
    assert_eq!(ui.app().agents.answered, [(9, Answer::Once)]);
}

#[test]
fn the_access_picker_saves_into_the_agent_settings() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_demo();
        a.agents_demo_view("settings");
    }) else {
        return;
    };
    assert!(ui.app().agents.cfg.conn(2).is_none(), "Staging starts not shared");
    ui.click("Agent access for Staging");
    ui.click("Read and write");
    assert_eq!(ui.app().agents.cfg.conn(2).map(|p| p.access), Some(Access::ReadWrite));
    // Its own page opens from the row, and the access there is the same setting.
    ui.click("Open Staging");
    assert_eq!(ui.app().agents.page, Some(2));
    ui.click("Read");
    assert_eq!(ui.app().agents.cfg.conn(2).map(|p| p.access), Some(Access::Read));
}

#[test]
fn the_rules_tester_shows_masked_output() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_demo();
        a.agents_demo_view("rules");
    }) else {
        return;
    };
    let w = ui.app().agents.rules.as_ref().expect("the Data protection window");
    let lines = tester_lines(&ui.app().agents.cfg, w.scope, &w.sample).expect("the sample scrubs");
    let (email, fired) = lines.iter().find(|(l, _)| l.starts_with("\"email\"")).expect("an email line").clone();
    assert!(!email.contains("ada@example.com"), "{email}");
    assert!(fired.is_some_and(|f| f.starts_with("PII")), "{email}");
    let (password, _) = lines.iter().find(|(l, _)| l.starts_with("\"password\"")).expect("a password line").clone();
    assert!(!password.contains("hunter2"), "{password}");
    ui.settle();
    assert!(ui.h.query_all_by_value(&email).next().is_some(), "the tester does not show {email}");
    // The address is only in the sample box, never in what an agent gets.
    let shown = ui.h.query_all_by(|n| n.role() == egui::Role::Label && n.value().is_some_and(|v| v.contains("ada@example.com"))).count();
    assert_eq!(shown, 0);
}

#[test]
fn the_pill_opens_agent_activity() {
    let Some(mut ui) = demo_ui(|a| {
        a.load_demo();
        a.agents_demo_view("pill");
    }) else {
        return;
    };
    assert!(!ui.app().agents.activity_open);
    ui.click("AI agents: Claude Code, 14 calls");
    assert!(ui.app().agents.activity_open);
    // The window lists the calls; picking one shows its arguments.
    ui.click("update shop.orders at 22:41:07");
    assert_eq!(ui.app().agents.picked, Some(8));
}

/// The Data protection window with three custom rules of the person's, which match the field names a, b and c.
fn three_rules(a: &mut App) {
    use crate::agents::config::{Action, Matcher, RuleSpec};
    a.load_demo();
    a.agents_demo_view("rules");
    a.agents.cfg.rules = ["a", "b", "c"].iter().map(|n| RuleSpec { on: true, matcher: Matcher::FieldName((*n).into()), scope: Vec::new(), action: Action::Redact, mode: Default::default() }).collect();
}

fn rule_names(ui: &UiApp) -> Vec<String> {
    use crate::agents::config::Matcher;
    ui.app().agents.cfg.rules.iter().map(|r| match &r.matcher {
        Matcher::FieldName(n) => n.clone(),
        _ => String::new(),
    }).collect()
}

#[test]
fn custom_rules_drag_to_reorder_and_the_selection_follows() {
    let Some(mut ui) = demo_ui(three_rules) else { return };
    ui.click("Custom 3");
    ui.show("Rule 3");
    ui.drag_esc("Rule 3", "Rule 1", 0.1);
    assert_eq!(rule_names(&ui), ["a", "b", "c"], "Esc moved a rule");
    assert!(ui.app().agents.rules.is_some(), "Esc closed the window instead of cancelling the drag");
    // A click selects a rule; dragging it below the last one keeps it selected there.
    ui.click("Rule 1");
    ui.drag("Rule 1", "Rule 3", 0.9, 1);
    assert_eq!(rule_names(&ui), ["b", "c", "a"]);
    assert_eq!(ui.app().agents.rules.as_ref().and_then(|w| w.sel), Some(2), "the selection stayed on the old index");
}
