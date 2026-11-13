//! Settings > AI agents > one connection: its access, what agents may do on it, its own data protection and scope.

use super::settings::{ACCESS, action_label, named_toggle, pick};
use super::*;
use super::activity::cell;
use super::rules::RulesTab;
use crate::agents::config::{ConnPolicy, Matcher, Mode, PresetMode, ScopeMode, Tri};

const MODES: [(Mode, &str); 3] = [(Mode::Never, "Never"), (Mode::Ask, "Ask"), (Mode::Allow, "Allow")];
const LARGE: [(u64, &str); 6] = [(10, "10"), (50, "50"), (100, "100"), (500, "500"), (1000, "1,000"), (10_000, "10,000")];
const SCOPES: [(ScopeMode, &str); 3] = [(ScopeMode::All, "All"), (ScopeMode::Only, "Only these"), (ScopeMode::Except, "All except these")];

pub(super) fn matcher_kind(m: &Matcher) -> &'static str {
    match m {
        Matcher::FieldPath(_) => "Field path",
        Matcher::FieldName(_) => "Field name",
        Matcher::ValueRegex(_) => "Value regex",
        Matcher::Detector(_) => "Detector",
    }
}

pub(super) fn matcher_text(m: &Matcher) -> String {
    match m {
        Matcher::FieldPath(s) | Matcher::FieldName(s) | Matcher::ValueRegex(s) => s.clone(),
        Matcher::Detector(d) => super::rules::detector_label(*d).into(),
    }
}

/// Whether the user's own Hide rules reach this connection. The presets hide only fields named like secrets, which
/// most tables do not have, so they alone do not turn free-form tools off; the executor checks the real columns.
fn own_rules_hide(cfg: &AgentsConfig, p: &ConnPolicy) -> bool {
    cfg.rules.iter().chain(&p.rules).any(|r| r.on && r.mode == PresetMode::Hide && matches!(r.matcher, Matcher::FieldPath(_) | Matcher::FieldName(_)))
}

/// `setting_row` with room for a wider control: the three-way choices here do not fit in its 230 points.
fn wide_row(ui: &mut egui::Ui, title: &str, desc: &str, reserve: f32, control: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        let w = ui.available_width();
        ui.vertical(|ui| {
            ui.set_width((w - reserve).max(140.0));
            ui.label(medium(RichText::new(title).size(TXT_TITLE).color(BRIGHT())));
            ui.add(egui::Label::new(RichText::new(desc).size(TXT_SMALL).color(MUTED())).wrap());
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), control);
    });
}

impl App {
    pub(super) fn agents_open_page(&mut self, id: u64) {
        self.agents.page = Some(id);
        self.agents.globs = self.agents.cfg.conns.get(&id).map(|p| p.scope.globs.join(", ")).unwrap_or_default();
    }

    pub(super) fn agents_conn_page(&mut self, ui: &mut egui::Ui, id: u64) {
        let Some(c) = self.conn(id) else { return };
        let name = c.meta.name.clone();
        let always = self.prefs.always_writable;
        let shared = self.agents.cfg.conns.get(&id).cloned();
        let mut p = shared.clone().unwrap_or_else(|| self.agents.cfg.defaults.clone());
        let mut back = false;
        let mut edit_rule: Option<Option<usize>> = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let link = RichText::new("AI agents").size(TXT_SUBHEAD).color(PRIMARY());
            back = ui.scope(|ui| {
                quiet_look(ui);
                ui.add(quiet_button(semibold(link)))
            }).inner.clicked();
            let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 18.0), Sense::hover());
            chevron(ui.painter(), r.center(), false, FAINT());
            ui.label(semibold(RichText::new(&name).size(TXT_SUBHEAD).color(BRIGHT())));
        });
        ui.add_space(6.0);
        let hides = own_rules_hide(&self.agents.cfg, &p);
        if hides && p.access != Access::NotShared {
            egui::Frame::new().fill(tint(WARNING(), 30)).stroke(Stroke::new(1.0, tint(WARNING(), 90))).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_top(|ui| {
                    let (r, _) = ui.allocate_exact_size(egui::vec2(8.0, 14.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), STATUS_DOT_R + 1.0, WARNING());
                    // Settings rows have no gap between items, and the dot needs one.
                    ui.add_space(6.0);
                    ui.add(egui::Label::new(RichText::new("Rules hide fields on this connection, so free SQL, commands and the shell are off wherever those fields are. Agents read through tools Quarry builds.").size(TXT_SMALL).color(TEXT())).wrap());
                });
            });
            ui.add_space(10.0);
        }
        settings_section(ui, "ACCESS", |ui| {
            let desc = if always { "Settings > Editing unlocks every connection, so agent writes need only the choices below." } else { "Read and write also needs the connection unlocked in Quarry." };
            wide_row(ui, "Agents can", desc, 270.0, |ui| {
                segmented(ui, &mut p.access, &ACCESS);
            });
        });
        let writes = p.access == Access::ReadWrite;
        let off = p.access == Access::NotShared;
        settings_section(ui, "WHAT AGENTS MAY DO", |ui| {
            let scripts = if hides { "Commands Quarry allows, SQL scripts and the shell. Off where rules hide data." } else { "Commands Quarry allows, SQL scripts and the shell. The shell always asks." };
            let rows: [(&str, &str, &mut Mode, bool); 4] = [
                ("Read data", "Find, aggregate, select rows, schema, indexes and explain.", &mut p.read, !off),
                ("Change data", "Insert, update and delete. Quarry keeps what it needs to undo.", &mut p.change, writes),
                ("Change structure", "Create or drop collections, tables and indexes. Always asks.", &mut p.structure, writes),
                ("Run scripts", scripts, &mut p.scripts, writes),
            ];
            for (i, (title, desc, mode, can)) in rows.into_iter().enumerate() {
                if i > 0 {
                    ui.separator();
                }
                setting_row(ui, title, desc, |ui| {
                    // A class the access level rules out shows Never, faint, so the row says what agents get.
                    if can {
                        segmented(ui, mode, &MODES);
                    } else {
                        let mut never = Mode::Never;
                        segmented_on(ui, &mut never, &MODES.map(|(m, l)| (m, l, false)));
                    }
                });
            }
            ui.separator();
            ui.add_enabled_ui(writes, |ui| {
                setting_row(ui, "Ask before a large change", "Even where changes are allowed, ask when one call would touch more than this many.", |ui| {
                    pick(ui, ("agent-large", id), "Ask before a large change", 90.0, &mut p.large_change, &LARGE);
                });
            });
        });
        let presets = &self.agents.cfg.presets;
        let mut open_checks: Option<RulesTab> = None;
        settings_section(ui, "DATA PROTECTION HERE", |ui| {
            for (i, (title, global, tri)) in [("Secrets", presets.secrets.on, &mut p.presets.secrets), ("Personal data", presets.pii.on, &mut p.presets.pii)].into_iter().enumerate() {
                if i > 0 {
                    ui.separator();
                }
                let desc = match (*tri, global) {
                    (Tri::Inherit, true) => "On for every connection.",
                    (Tri::Inherit, false) => "Off for every connection.",
                    (Tri::On, _) => "On for this connection, whatever the global setting.",
                    (Tri::Off, _) => "Off here: agents get these values as they are.",
                };
                let same = if global { "Same as all (on)" } else { "Same as all (off)" };
                let (tab, n) = if i == 0 { (RulesTab::Secrets, crate::agents::presets::secrets().len()) } else { (RulesTab::Pii, crate::agents::presets::pii().len()) };
                wide_row(ui, title, desc, 340.0, |ui| {
                    // The preset's own checks live in the Data protection window; this is the way there from a connection.
                    if ui.button(format!("{n} checks")).on_hover_text("See what this preset hides, and turn checks off").clicked() {
                        open_checks = Some(tab);
                    }
                    segmented(ui, tri, &[(Tri::Inherit, same), (Tri::On, "On"), (Tri::Off, "Off")]);
                });
            }
        });
        if let Some(tab) = open_checks {
            self.agents_open_rules(Some(id), tab);
        }
        let mut globs_changed = false;
        settings_section(ui, "DATABASES AND COLLECTIONS", |ui| {
            segmented(ui, &mut p.scope.mode, &SCOPES);
            if p.scope.mode != ScopeMode::All {
                let w = ui.available_width();
                let r = ui.add(egui::TextEdit::singleline(&mut self.agents.globs).id(egui::Id::unique(("agent-globs", id))).font(egui::TextStyle::Monospace).hint_text("shop.audit_*, *.sessions, billing").desired_width(w));
                globs_changed = r.changed();
            }
            ui.add(egui::Label::new(RichText::new("Globs, comma separated. System collections such as admin.system.users are never shared.").size(TXT_SMALL).color(FAINT())).wrap());
        });
        if globs_changed {
            p.scope.globs = self.agents.globs.split(',').map(|g| g.trim().to_string()).filter(|g| !g.is_empty()).collect();
        }
        let mut flip: Option<usize> = None;
        let mut drop: Option<usize> = None;
        settings_section(ui, "CUSTOM RULES FOR THIS CONNECTION", |ui| {
            for (i, r) in p.rules.iter().enumerate() {
                if i > 0 {
                    ui.separator();
                }
                ui.horizontal(|ui| {
                    let mut on = r.on;
                    if named_toggle(ui, &mut on, &format!("Rule {} on", i + 1)).changed() {
                        flip = Some(i);
                    }
                    ui.add_space(8.0);
                    cell(ui, 90.0, egui::Label::new(RichText::new(matcher_kind(&r.matcher)).size(TXT_SMALL).color(MUTED())));
                    let text = RichText::new(matcher_text(&r.matcher)).font(FontId::monospace(TXT_SMALL)).color(if r.on { TEXT() } else { FAINT() });
                    if ui.scope(|ui| {
                        quiet_look(ui);
                        ui.add(quiet_button(text))
                    }).inner.on_hover_text("Edit in Data protection").clicked() {
                        edit_rule = Some(Some(i));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let x = close_x(ui).on_hover_text("Delete rule");
                        x.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Delete rule {}", i + 1)));
                        if x.clicked() {
                            drop = Some(i);
                        }
                        ui.add_space(6.0);
                        ui.label(RichText::new(action_label(&r.action)).size(TXT_SMALL).color(TEXT()));
                    });
                });
            }
            if !p.rules.is_empty() {
                ui.separator();
            }
            ui.horizontal(|ui| {
                if ui.button("Add rule").clicked() {
                    edit_rule = Some(None);
                }
                ui.label(RichText::new("For fields the Secrets and Personal data checks above don't cover.").size(TXT_SMALL).color(FAINT()));
            });
        });
        if let Some(i) = flip {
            p.rules[i].on = !p.rules[i].on;
        }
        if let Some(i) = drop {
            p.rules.remove(i);
        }
        ui.label(RichText::new("Agent writes also need this connection unlocked in Quarry, as yours do.").size(TXT_SMALL).color(FAINT()));
        // A connection never shared stays out of the file until the user shares it, so "missing means Not shared" holds.
        let keep = shared.is_some() || p != self.agents.cfg.defaults;
        if keep && shared.as_ref() != Some(&p) {
            self.agents.cfg.conns.insert(id, p);
            self.save_agents();
        }
        if back {
            self.agents.page = None;
        }
        if let Some(rule) = edit_rule {
            self.agents_rules_for(Some(id), rule);
        }
    }
}
