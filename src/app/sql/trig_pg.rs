//! The PostgreSQL half of the trigger editor: the fields above the body (name, timing, events, columns, row or statement,
//! WHEN, enabled, and the function the trigger runs). The statements come from `db::sql::pg::ddl_trig`, which also checks the
//! draft; this file only draws it and says what the person should know before saving.

use crate::app::*;
use db::sql::ddl::TrigDef;
use db::sql::pg::ddl_trig::{canonical, cols_of, events_of, func_name};
use db::sql::structure::PgTrig;

/// What the head needs besides the draft.
pub(in crate::app) struct Env<'a> {
    pub editable: bool,
    pub live: Option<&'a PgTrig>,
    pub table: &'a str,
}

/// The UPDATE OF list names a column the table does not have: PostgreSQL would refuse it, and the message names it.
pub(in crate::app) fn column_problem(def: &TrigDef, cols: &[String]) -> Option<String> {
    // A column whose own name holds a comma is typed as that name, which the split below would take for two.
    if cols.contains(&def.pg.of_cols) {
        return None;
    }
    cols_of(&def.pg.of_cols).into_iter().find(|c| !cols.contains(c)).map(|c| format!("The table has no column {c}."))
}

/// One monospace line of the head, with the name a screen reader gives it.
fn field(ui: &mut egui::Ui, enabled: bool, text: &mut String, width: f32, hint: &str, label: &str) {
    ui.add_enabled(enabled, egui::TextEdit::singleline(text).font(egui::TextStyle::Monospace).desired_width(width).hint_text(hint)).widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, enabled, label));
}

pub(in crate::app) fn head(ui: &mut egui::Ui, def: &mut TrigDef, env: &Env) {
    let (editable, exists) = (env.editable, env.live.is_some());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        ui.label(RichText::new("Name").color(MUTED()));
        field(ui, editable && !exists, &mut def.name, 240.0, "orders_audit", "Trigger name");
        ui.add_space(8.0);
        ui.label(RichText::new("Timing").color(MUTED()));
        ui.add_enabled_ui(editable, |ui| {
            let timings = ["BEFORE", "AFTER", "INSTEAD OF"];
            let mut i = timings.iter().position(|t| *t == def.timing).unwrap_or(0);
            segmented(ui, &mut i, &[(0, "BEFORE"), (1, "AFTER"), (2, "INSTEAD OF")]);
            def.timing = timings[i].into();
            // INSTEAD OF fires for each row, which the segmented control for row or statement below shows.
            if i == 2 {
                def.pg.per_row = true;
            }
        });
        ui.add_space(8.0);
        ui.label(RichText::new("For each").color(MUTED()));
        ui.add_enabled_ui(editable, |ui| {
            let mut i = usize::from(!def.pg.per_row);
            segmented(ui, &mut i, &[(0, "ROW"), (1, "STATEMENT")]);
            def.pg.per_row = i == 0;
        });
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(RichText::new("Events").color(MUTED()));
        let mut events = events_of(&def.event);
        ui.add_enabled_ui(editable, |ui| {
            for e in ["INSERT", "UPDATE", "DELETE", "TRUNCATE"] {
                let on = events.iter().any(|x| x == e);
                if chip(ui, e, C_KEYWORD(), on).clicked() {
                    if on {
                        events.retain(|x| x != e);
                    } else {
                        events.push(e.to_string());
                        // TRUNCATE has no rows to fire for.
                        if e == "TRUNCATE" {
                            def.pg.per_row = false;
                        }
                    }
                }
            }
        });
        def.event = canonical(&events);
        if events.iter().any(|e| e == "UPDATE") {
            ui.add_space(8.0);
            ui.label(RichText::new("Columns").color(MUTED()));
            field(ui, editable, &mut def.pg.of_cols, 220.0, "any column", "Trigger columns");
        } else {
            def.pg.of_cols.clear();
        }
        ui.add_space(8.0);
        ui.label(RichText::new("Enabled").color(MUTED()));
        let was = env.live.map_or("O", |l| l.mode.as_str());
        let mut on = def.pg.mode != "D";
        // The server refuses ALTER TABLE .. DISABLE TRIGGER on a view, so its INSTEAD OF triggers are always enabled.
        let switch = editable && env.live.is_none_or(|l| l.rel_kind != "v");
        let r = ui.add_enabled_ui(switch, |ui| toggle(ui, &mut on)).inner.on_disabled_hover_text("PostgreSQL cannot disable or enable a trigger on a view.");
        r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Switch, switch, on, "Trigger enabled"));
        // A trigger that fires only on a replica or always keeps that when it is turned on again.
        def.pg.mode = if !on { "D".into() } else if matches!(was, "R" | "A") { was.into() } else { "O".into() };
        if matches!(def.pg.mode.as_str(), "R" | "A") {
            ui.label(RichText::new(if def.pg.mode == "R" { "fires on replicas only" } else { "fires always" }).size(TXT_SMALL).color(FAINT()));
        }
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        ui.label(RichText::new("When").color(MUTED()));
        field(ui, editable, &mut def.pg.when, f32::INFINITY, "NEW.status IS DISTINCT FROM OLD.status", "Trigger when");
    });
    ui.add_space(8.0);
    function_line(ui, def, env);
    notes(ui, env);
}

/// The function the trigger runs: its name where it exists, the name field where it is about to be made.
fn function_line(ui: &mut egui::Ui, def: &mut TrigDef, env: &Env) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        ui.label(RichText::new("Function").color(MUTED()));
        match env.live {
            Some(p) => {
                ui.label(RichText::new(format!("{}.{}()", p.func_schema, p.func)).font(FontId::monospace(TXT_SMALL)).color(TEXT()));
                let mut facts = vec![p.lang.clone()];
                if p.func_others > 0 {
                    facts.push(if p.func_others == 1 { "also run by 1 other trigger".into() } else { format!("also run by {} other triggers", p.func_others) });
                }
                if p.func_secdef {
                    facts.push("SECURITY DEFINER".into());
                }
                if !p.func_mine {
                    facts.push(format!("owned by {}", p.func_owner));
                }
                ui.label(RichText::new(facts.join(", ")).size(TXT_SMALL).color(FAINT()));
            }
            None => {
                let auto = func_name(env.table, &def.name);
                field(ui, env.editable, &mut def.pg.func, 300.0, &auto, "Function name");
                ui.label(RichText::new("a new plpgsql function that returns trigger").size(TXT_SMALL).color(FAINT()));
            }
        }
    });
}

/// Why a trigger cannot be changed, or what part of it can.
fn notes(ui: &mut egui::Ui, env: &Env) {
    let Some(p) = env.live else { return };
    if !p.refuse.is_empty() {
        ui.add_space(6.0);
        ui.label(RichText::new(p.refuse.as_str()).size(TXT_SMALL).color(WARNING()));
    } else if !p.no_rewrite.is_empty() {
        ui.add_space(6.0);
        ui.label(RichText::new(format!("{} Its function body and enabled state can still be changed.", p.no_rewrite)).size(TXT_SMALL).color(WARNING()));
    } else if p.lang != "plpgsql" {
        ui.add_space(6.0);
        ui.label(RichText::new(format!("The function is written in {}, so its body is shown but not changed here.", p.lang)).size(TXT_SMALL).color(WARNING()));
    }
}
