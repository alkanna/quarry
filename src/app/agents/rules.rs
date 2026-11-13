//! The Data protection window: the Secrets and Personal data presets item by item, the user's own rules for every
//! connection or for one, and a tester that runs them on a pasted document without sending it anywhere.

use super::conn_sheet::{matcher_kind, matcher_text};
use super::settings::{MODES, action_label, named_toggle, pick, preset_actions};
use super::*;
use crate::app::sql::create::plural;
use super::activity::cell;
use crate::agents::config::{Action, Detector, Matcher, PresetMode, RuleSpec, Tri};
use crate::agents::presets;
use crate::agents::scrub::{Fired, Rules};

/// Hashes in the tester use this key, not the keychain one: the tester must work in the demo, and a hash shown here
/// must not match what agents get.
const TESTER_KEY: [u8; 32] = [0; 32];

const SAMPLE: &str = "{\n  \"full_name\": \"Ada Lovelace\",\n  \"email\": \"ada@example.com\",\n  \"card_number\": \"4111 1111 1111 1111\",\n  \"notes\": \"SSN 123-45-6789, call back\",\n  \"password\": \"hunter2\",\n  \"avatar\": { \"$binary\": { \"base64\": \"iVBORw0KGgo=\", \"subType\": \"00\" } }\n}";

#[derive(Clone, Copy, PartialEq, Default)]
pub(in crate::app) enum RulesTab {
    Secrets,
    #[default]
    Pii,
    Custom,
}

pub(in crate::app) struct RulesWin {
    pub(super) tab: RulesTab,
    /// Whose custom rules show and which rules the tester runs: every connection, or one.
    pub(in crate::app) scope: Option<u64>,
    /// The custom rule open in the editor.
    pub(super) sel: Option<usize>,
    /// The editor's "where" globs as typed, and the rule they belong to.
    globs: (Option<usize>, String),
    pub(in crate::app) sample: String,
    /// The tester's lines with what they were computed from (sample, scope, config revision), so they are rebuilt only when one changes.
    tested: Option<((String, Option<u64>, u64), TesterLines)>,
}

type TesterLines = Result<Vec<(String, Option<String>)>, String>;

impl Default for RulesWin {
    fn default() -> Self {
        Self { tab: RulesTab::default(), scope: None, sel: None, globs: (None, String::new()), sample: SAMPLE.into(), tested: None }
    }
}

const DETECTORS: [(Detector, &str); 13] = [
    (Detector::CardNumber, "Card number, Luhn checked"),
    (Detector::Iban, "IBAN, mod-97 checked"),
    (Detector::Email, "Email address"),
    (Detector::Phone, "Phone number"),
    (Detector::Ssn, "US social security number"),
    (Detector::Nir, "French social security number"),
    (Detector::Nino, "UK National Insurance number"),
    (Detector::Jwt, "JSON Web Token"),
    (Detector::PemPrivateKey, "Private key (PEM)"),
    (Detector::CloudKey, "Cloud keys: AWS, GCP, Stripe"),
    (Detector::BearerToken, "Bearer token"),
    (Detector::UrlPassword, "Password in a URL"),
    (Detector::Ip, "IP address"),
];

pub(super) fn detector_label(d: Detector) -> &'static str {
    DETECTORS.iter().find(|x| x.0 == d).map_or("", |x| x.1)
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Path,
    Name,
    Value,
    Detect,
}

const KINDS: [(Kind, &str); 4] = [(Kind::Path, "Field path"), (Kind::Name, "Field name"), (Kind::Value, "Value regex"), (Kind::Detect, "Detector")];

fn kind_of(m: &Matcher) -> Kind {
    match m {
        Matcher::FieldPath(_) => Kind::Path,
        Matcher::FieldName(_) => Kind::Name,
        Matcher::ValueRegex(_) => Kind::Value,
        Matcher::Detector(_) => Kind::Detect,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Act {
    Remove,
    Replace,
    Mask,
    Hash,
    Redact,
}

const ACTS: [(Act, &str); 5] = [(Act::Replace, "Replace with text"), (Act::Mask, "Mask, keep the end"), (Act::Hash, "Hash"), (Act::Redact, "Redact the match"), (Act::Remove, "Remove the field")];

fn act_of(a: &Action) -> Act {
    match a {
        Action::Remove => Act::Remove,
        Action::Replace(_) => Act::Replace,
        Action::Mask(_) => Act::Mask,
        Action::Hash => Act::Hash,
        Action::Redact => Act::Redact,
    }
}

/// "Secrets: Password fields" for a preset item, "rule 3" for the user's third rule.
pub(super) fn fired_label(f: Fired) -> String {
    match f {
        Fired::Preset(id) => format!("{}: {}", if id.starts_with("secrets.") { "Secrets" } else { "PII" }, presets::label(id).unwrap_or(id)),
        Fired::Custom(i) => format!("rule {}", i + 1),
    }
}

/// One line per top-level field of what an agent gets, with the preset item or rule that changed it. None when the
/// rules do not compile or the sample does not parse, with the reason.
pub(in crate::app) fn tester_lines(cfg: &AgentsConfig, scope: Option<u64>, sample: &str) -> TesterLines {
    let rules = Rules::from_config(cfg, scope.and_then(|id| cfg.conns.get(&id)), &TESTER_KEY).map_err(|e| e.to_string())?;
    let doc = db::parse_doc(sample)?;
    let ns = rules.all();
    let (out, hits) = ns.explain_scrub(&doc);
    let mut lines = Vec::new();
    for (k, v) in &doc {
        let below = format!("{k}.");
        let fired = hits.iter().find(|(p, _)| p == k || p.starts_with(&below)).map(|(p, f)| if p == k { fired_label(*f) } else { format!("{}: {}", &p[below.len()..], fired_label(*f)) });
        let line = match out.get(k) {
            Some(after) => format!("\"{k}\": {},", db::compact(after)),
            None => format!("\"{k}\": removed"),
        };
        let binary = matches!(v, Bson::Binary(_)) && cfg.binary_as_size;
        lines.push((line, fired.or(binary.then(|| "binary".to_string()))));
    }
    Ok(lines)
}

impl App {
    /// Opens Data protection on the custom rules of `scope`, with rule `rule` in the editor, or a new one.
    /// Opens Data protection on a preset's checks, run as one connection sees them.
    pub(super) fn agents_open_rules(&mut self, scope: Option<u64>, tab: RulesTab) {
        self.agents.rules = Some(RulesWin { tab, scope, ..Default::default() });
    }

    pub(super) fn agents_rules_for(&mut self, scope: Option<u64>, rule: Option<usize>) {
        let i = rule.unwrap_or_else(|| {
            // Off until its pattern is filled in: a rule that does not compile stops every agent call.
            let r = RuleSpec { on: false, matcher: Matcher::FieldPath(String::new()), scope: Vec::new(), action: Action::Replace("hidden".into()), mode: PresetMode::Hide };
            let list = self.rule_list(scope);
            list.push(r);
            list.len() - 1
        });
        self.agents.rules = Some(RulesWin { tab: RulesTab::Custom, scope, sel: Some(i), ..Default::default() });
        if rule.is_none() {
            self.save_agents();
        }
    }

    fn rule_list(&mut self, scope: Option<u64>) -> &mut Vec<RuleSpec> {
        match scope {
            None => &mut self.agents.cfg.rules,
            Some(id) => {
                let cfg = &mut self.agents.cfg;
                &mut cfg.conns.entry(id).or_insert_with(|| cfg.defaults.clone()).rules
            }
        }
    }

    pub(in crate::app) fn agents_rules_window(&mut self, ctx: &egui::Context) {
        let Some(mut w) = self.agents.rules.take() else { return };
        let before = super::settings::had_input(ctx).then(|| self.agents.cfg.clone());
        let mut keep = true;
        let (mut import, mut export) = (false, false);
        let screen = ctx.content_rect();
        let width = 1080.0f32.min(screen.width() - 40.0);
        let body_h = (screen.height() - 200.0).clamp(300.0, 520.0);
        let scopes: Vec<(Option<u64>, String)> = std::iter::once((None, "All connections".to_string())).chain(self.conns.iter().filter(|c| self.agents.cfg.conn(c.meta.id).is_some() || w.scope == Some(c.meta.id)).map(|c| (Some(c.meta.id), c.meta.name.clone()))).collect();
        let layer = egui::Id::unique(("quarry-layer", "agents-rules"));
        let r = top_banded(ctx, layer).show(ctx, |ui| {
            ui.set_width(width);
            dialog_head_with(ui, &Head::new("Data protection").sub("What AI agents may read, and what they see masked").glyph(glyphs::shield), |ui| {
                let opts: Vec<(Option<u64>, &str)> = scopes.iter().map(|(id, n)| (*id, n.as_str())).collect();
                let mut s = w.scope;
                segmented(ui, &mut s, &opts);
                if s != w.scope {
                    (w.scope, w.sel) = (s, None);
                }
            });
            let mut b = body_begin(ui);
            let outer = ui;
            let ui = &mut b.content_ui;
            let width = ui.available_width();
            let left_w = (width * 0.62).floor();
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(egui::vec2(left_w, body_h), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_width(left_w);
                    let custom_n = match w.scope {
                        None => self.agents.cfg.rules.len(),
                        Some(id) => self.agents.cfg.conns.get(&id).map_or(0, |p| p.rules.len()),
                    };
                    let tabs = [(RulesTab::Secrets, format!("Secrets {}", presets::secrets().len())), (RulesTab::Pii, format!("Personal data {}", presets::pii().len())), (RulesTab::Custom, format!("Custom {custom_n}"))];
                    segmented(ui, &mut w.tab, &tabs.iter().map(|(t, l)| (*t, l.as_str())).collect::<Vec<_>>());
                    ui.add_space(6.0);
                    egui::ScrollArea::vertical().id_salt("agents-rules-left").max_height(body_h - 40.0).auto_shrink([false, false]).show(ui, |ui| {
                        ui.set_width(left_w - 12.0);
                        match w.tab {
                            RulesTab::Custom => self.rules_custom(ui, &mut w),
                            tab => self.rules_preset(ui, &w, tab == RulesTab::Secrets),
                        }
                    });
                });
                ui.add_space(20.0);
                ui.allocate_ui_with_layout(egui::vec2(width - left_w - 20.0, body_h), egui::Layout::top_down(egui::Align::Min), |ui| {
                    self.rules_tester(ui, &mut w);
                });
            });
            b.end(outer);
            let ui = outer;
            dialog_foot(ui, |ui| {
                let note = if w.tab == RulesTab::Custom { "Rules run top to bottom, after the presets. A rule that doesn't compile stops every agent call until it is fixed." } else { "Secrets first, then personal data, then your rules. A rule that doesn't compile stops every agent call until it is fixed." };
                {
                    if ui.button("Done").on_hover_text("Esc").clicked() {
                        keep = false;
                    }
                    if w.tab == RulesTab::Custom && primary(ui, "Add rule", PRIMARY()).clicked() {
                        let r = RuleSpec { on: false, matcher: Matcher::FieldPath(String::new()), scope: Vec::new(), action: Action::Replace("hidden".into()), mode: PresetMode::Hide };
                        let list = self.rule_list(w.scope);
                        list.push(r);
                        w.sel = Some(list.len() - 1);
                    }
                    export = ui.button("Export…").clicked();
                    import = ui.button("Import…").clicked();
                }
                foot_note(ui, note);
            });
        });
        if before.is_some_and(|b| b != self.agents.cfg) {
            self.save_agents();
            // The tester drew before the save moved the revision, so it redraws once more with the new rules.
            ctx.request_repaint();
        }
        if import {
            self.rules_import(w.scope);
        }
        if export {
            self.rules_export(w.scope);
        }
        if keep && !r.should_close() {
            self.agents.rules = Some(w);
        } else {
            self.drop_empty_rules();
        }
    }

    /// Add rule starts a rule with no pattern; one still empty when the window closes was never filled in.
    fn drop_empty_rules(&mut self) {
        let empty = |r: &RuleSpec| matches!(&r.matcher, Matcher::FieldPath(s) | Matcher::FieldName(s) | Matcher::ValueRegex(s) if s.trim().is_empty());
        let before = self.agents.cfg.clone();
        self.agents.cfg.rules.retain(|r| !empty(r));
        for p in self.agents.cfg.conns.values_mut() {
            p.rules.retain(|r| !empty(r));
        }
        if before != self.agents.cfg {
            self.save_agents();
        }
    }

    fn rules_preset(&mut self, ui: &mut egui::Ui, w: &RulesWin, secrets: bool) {
        let (title, items) = if secrets { ("Secrets", presets::secrets()) } else { ("Personal data", presets::pii()) };
        if let Some(id) = w.scope {
            let name = self.conn(id).map(|c| c.meta.name.clone()).unwrap_or_default();
            let global = if secrets { self.agents.cfg.presets.secrets.on } else { self.agents.cfg.presets.pii.on };
            let defaults = self.agents.cfg.defaults.clone();
            let p = self.agents.cfg.conns.entry(id).or_insert(defaults);
            let tri = if secrets { &mut p.presets.secrets } else { &mut p.presets.pii };
            card(ui, |ui| {
                ui.set_width(ui.available_width());
                setting_row(ui, &format!("On {name}"), "The checks below are shared by every connection.", |ui| {
                    segmented(ui, tri, &[(Tri::Inherit, if global { "Same as all (on)" } else { "Same as all (off)" }), (Tri::On, "On"), (Tri::Off, "Off")]);
                });
            });
            ui.add_space(6.0);
        }
        let p = if secrets { &mut self.agents.cfg.presets.secrets } else { &mut self.agents.cfg.presets.pii };
        card(ui, |ui| {
            ui.set_width(ui.available_width());
            let mode = match p.mode {
                PresetMode::Hide => "Hide: agents can't query, sort on or change these fields.",
                PresetMode::MaskOutput => "Mask output: agents can query and sort on these fields, and get every value masked.",
            };
            ui.horizontal(|ui| {
                let w = ui.available_width();
                ui.vertical(|ui| {
                    ui.set_width((w - 300.0).max(160.0));
                    ui.label(medium(RichText::new(title).size(TXT_TITLE).color(BRIGHT())));
                    ui.add(egui::Label::new(RichText::new("Checks turn off one by one. To change one, add a custom rule.").size(TXT_SMALL).color(MUTED())).wrap());
                    ui.add(egui::Label::new(RichText::new(mode).size(TXT_SMALL).color(FAINT())).wrap());
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    named_toggle(ui, &mut p.on, title);
                    ui.add_space(6.0);
                    ui.add_enabled_ui(p.on, |ui| {
                        pick(ui, ("rules-action", secrets), &format!("{title} action"), 110.0, &mut p.action, &preset_actions());
                        pick(ui, ("rules-mode", secrets), &format!("{title} mode"), 100.0, &mut p.mode, &MODES);
                    });
                });
            });
        });
        ui.add_space(6.0);
        let mut flip = None;
        card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                cell(ui, 46.0, egui::Label::new(caption_text("ON")));
                cell(ui, 100.0, egui::Label::new(caption_text("KIND")));
                caption(ui, "CHECKS");
            });
            for it in &items {
                ui.separator();
                let mut on = !p.off.iter().any(|o| o == it.id);
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(p.on, |ui| {
                        if named_toggle(ui, &mut on, it.label).changed() {
                            flip = Some(it.id);
                        }
                    });
                    ui.add_space(8.0);
                    let kind = if it.key.is_some() || it.gate.is_some() { "Field names" } else { "Detector" };
                    let c = if on && p.on { TEXT() } else { FAINT() };
                    cell(ui, 100.0, egui::Label::new(RichText::new(kind).size(TXT_SMALL).color(MUTED())));
                    ui.label(RichText::new(it.label).color(c));
                });
            }
        });
        if let Some(id) = flip {
            if let Some(i) = p.off.iter().position(|o| o == id) {
                p.off.remove(i);
            } else {
                p.off.push(id.to_string());
            }
        }
    }

    fn rules_custom(&mut self, ui: &mut egui::Ui, w: &mut RulesWin) {
        let names: HashMap<u64, String> = self.conns.iter().map(|c| (c.meta.id, c.meta.name.clone())).collect();
        let scope = w.scope;
        let err = Rules::from_config(&self.agents.cfg, scope.and_then(|id| self.agents.cfg.conns.get(&id)), &TESTER_KEY).err();
        // Global rules come first in the numbering the tester and compile errors use.
        let offset = if scope.is_some() { self.agents.cfg.rules.len() } else { 0 };
        let list = self.rule_list(scope);
        if w.sel.is_some_and(|i| i >= list.len()) {
            w.sel = None;
        }
        let mut flip = None;
        card(ui, |ui| {
            ui.set_width(ui.available_width());
            let full = ui.available_width();
            let cols = [46.0, 100.0, full - 46.0 - 100.0 - 120.0 - 130.0 - 24.0, 120.0];
            ui.horizontal(|ui| {
                for (t, cw) in ["ON", "MATCHES", "PATTERN", "WHERE"].into_iter().zip(cols) {
                    cell(ui, cw, egui::Label::new(caption_text(t)));
                }
                caption(ui, "DO");
            });
            if list.is_empty() {
                ui.separator();
                ui.label(RichText::new(if scope.is_some() { "No rules for this connection yet." } else { "No rules of yours yet." }).size(TXT_SMALL).color(MUTED()));
            }
            for (i, r) in list.iter().enumerate() {
                ui.separator();
                let on_sel = w.sel == Some(i);
                // The row is sensed first and its content drawn on top, so the switch inside still takes its own clicks.
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(full, 26.0), Sense::click());
                let state = state_of(&resp);
                if on_sel || state > 0 {
                    let fill = if on_sel { tint(PRIMARY(), 40) } else { hover_fill(state) };
                    ui.painter().rect_filled(rect.expand2(egui::vec2(4.0, 0.0)), R_SM, fill);
                }
                resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Row, true, on_sel, format!("Rule {}", offset + i + 1)));
                if resp.clicked() {
                    w.sel = Some(i);
                }
                put_at(ui, rect, |ui| ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    // Text that takes no clicks, so a click on it selects the row.
                    ui.style_mut().interaction.selectable_labels = false;
                    let mut on = r.on;
                    if named_toggle(ui, &mut on, &format!("Rule {} on", offset + i + 1)).changed() {
                        flip = Some(i);
                    }
                    ui.add_space(8.0);
                    let c = if r.on { TEXT() } else { FAINT() };
                    cell(ui, cols[1], egui::Label::new(RichText::new(matcher_kind(&r.matcher)).size(TXT_SMALL).color(MUTED())));
                    let pat = matcher_text(&r.matcher);
                    let pat = if pat.is_empty() { "(empty)".to_string() } else { pat };
                    cell(ui, cols[2], egui::Label::new(RichText::new(pat).font(FontId::monospace(TXT_SMALL)).color(c)).truncate());
                    let at = if r.scope.is_empty() { scope.and_then(|id| names.get(&id).cloned()).unwrap_or_else(|| "Everywhere".into()) } else { r.scope.join(", ") };
                    cell(ui, cols[3], egui::Label::new(RichText::new(at).size(TXT_SMALL).color(MUTED())).truncate());
                    ui.label(RichText::new(action_label(&r.action)).size(TXT_SMALL).color(c));
                }));
            }
        });
        if let Some(i) = flip {
            list[i].on = !list[i].on;
        }
        let Some(i) = w.sel else {
            if let Some(e) = err {
                field_note(ui, &e.to_string(), ALERT());
            }
            return;
        };
        if w.globs.0 != Some(i) {
            w.globs = (Some(i), list[i].scope.join(", "));
        }
        ui.add_space(8.0);
        let mut delete = false;
        let mut swap = None;
        let n = list.len();
        let r = &mut list[i];
        card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(medium(RichText::new(format!("Rule {}", offset + i + 1)).size(TXT_TITLE).color(BRIGHT())));
            egui::Grid::new(("agents-rule-edit", i)).num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                form_label(ui, "Matches");
                let mut kind = kind_of(&r.matcher);
                if pick(ui, "rule-kind", "Rule matches", 140.0, &mut kind, &KINDS) {
                    let text = match &r.matcher {
                        Matcher::Detector(_) => String::new(),
                        m => matcher_text(m),
                    };
                    r.matcher = match kind {
                        Kind::Path => Matcher::FieldPath(text),
                        Kind::Name => Matcher::FieldName(text),
                        Kind::Value => Matcher::ValueRegex(text),
                        Kind::Detect => Matcher::Detector(Detector::CardNumber),
                    };
                }
                ui.end_row();
                form_label(ui, if kind == Kind::Detect { "Detector" } else { "Pattern" });
                match &mut r.matcher {
                    Matcher::Detector(d) => {
                        pick(ui, "rule-detector", "Rule detector", 220.0, d, &DETECTORS);
                    }
                    Matcher::FieldPath(s) | Matcher::FieldName(s) | Matcher::ValueRegex(s) => {
                        let hint = match kind {
                            Kind::Path => "customers.**.ssn",
                            Kind::Name => "^card_",
                            _ => r"\bORD-\d{8}\b",
                        };
                        ui.add(egui::TextEdit::singleline(s).id(egui::Id::unique("agents-rule-pattern")).font(egui::TextStyle::Monospace).hint_text(hint).desired_width(300.0));
                    }
                }
                ui.end_row();
                form_label(ui, "Where");
                if ui.add(egui::TextEdit::singleline(&mut w.globs.1).id(egui::Id::unique("agents-rule-scope")).font(egui::TextStyle::Monospace).hint_text("Everywhere, or shop.*, *.users").desired_width(300.0)).changed() {
                    r.scope = w.globs.1.split(',').map(|g| g.trim().to_string()).filter(|g| !g.is_empty()).collect();
                }
                ui.end_row();
                form_label(ui, "Do");
                ui.horizontal(|ui| {
                    let mut act = act_of(&r.action);
                    if pick(ui, "rule-action", "Rule action", 160.0, &mut act, &ACTS) {
                        r.action = match act {
                            Act::Remove => Action::Remove,
                            Act::Replace => Action::Replace("hidden".into()),
                            Act::Mask => Action::Mask(4),
                            Act::Hash => Action::Hash,
                            Act::Redact => Action::Redact,
                        };
                    }
                    match &mut r.action {
                        Action::Replace(t) => {
                            ui.add(egui::TextEdit::singleline(t).id(egui::Id::unique("agents-rule-replace")).desired_width(120.0));
                        }
                        Action::Mask(k) => {
                            ui.label(RichText::new("keep").color(MUTED()));
                            ui.add(egui::DragValue::new(k).range(0..=8));
                        }
                        _ => {}
                    }
                });
                ui.end_row();
                if matches!(kind, Kind::Path | Kind::Name) {
                    form_label(ui, "Mode");
                    segmented(ui, &mut r.mode, &MODES);
                    ui.end_row();
                }
            });
            if let Some(e) = &err {
                field_note(ui, &e.to_string(), ALERT());
            } else if !r.on {
                ui.label(RichText::new("Off. Turn it on in the list once the pattern is ready.").size(TXT_SMALL).color(FAINT()));
            }
            ui.horizontal(|ui| {
                if quiet_enabled(ui, i > 0, "Move up").clicked() {
                    swap = Some(i - 1);
                }
                if quiet_enabled(ui, i + 1 < n, "Move down").clicked() {
                    swap = Some(i + 1);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    delete = danger(ui, "Delete rule").clicked();
                });
            });
        });
        if let Some(j) = swap {
            list.swap(i, j);
            w.sel = Some(j);
            w.globs.0 = None;
        }
        if delete {
            list.remove(i);
            w.sel = None;
        }
    }

    fn rules_tester(&mut self, ui: &mut egui::Ui, w: &mut RulesWin) {
        caption(ui, "TRY THEM");
        ui.label(RichText::new("Paste a document or a row. Nothing here is sent anywhere.").size(TXT_SMALL).color(MUTED()));
        let width = ui.available_width();
        egui::Frame::new().fill(pal().well).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_MD).inner_margin(8).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("agents-tester-in").max_height(170.0).show(ui, |ui| {
                ui.add(egui::TextEdit::multiline(&mut w.sample).id(egui::Id::unique("agents-tester-sample")).font(egui::TextStyle::Monospace).frame(egui::Frame::NONE).desired_width(width - 16.0).desired_rows(7));
            });
        });
        ui.add_space(10.0);
        caption(ui, "WHAT AN AGENT GETS");
        let key = (&w.sample, w.scope, self.agents.rev);
        if w.tested.as_ref().is_none_or(|(k, _)| (&k.0, k.1, k.2) != key) {
            w.tested = Some(((w.sample.clone(), w.scope, self.agents.rev), tester_lines(&self.agents.cfg, w.scope, &w.sample)));
        }
        let lines = &w.tested.as_ref().expect("computed above").1;
        egui::Frame::new().fill(pal().well).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_MD).inner_margin(8).show(ui, |ui| {
            ui.set_width(width - 16.0);
            egui::ScrollArea::vertical().id_salt("agents-tester-out").max_height(200.0).show(ui, |ui| {
                match lines {
                    Err(e) => field_note(ui, e, ALERT()),
                    Ok(lines) => {
                        // Painted rows of one line each: the field as an agent gets it, and at the right what changed it.
                        let w = ui.available_width();
                        let row = |ui: &mut egui::Ui, text: &str, indent: f32, color: Color32, fired: Option<&str>| {
                            let (rect, r) = ui.allocate_exact_size(egui::vec2(w, 17.0), Sense::hover());
                            let tag_w = fired.map_or(0.0, |f| {
                                let g = ui.painter().layout_job(egui::text::LayoutJob::single_section(f.to_string(), ui_fmt(TXT_CAPTION, FAINT(), 400.0, 0.0)));
                                let gw = g.size().x;
                                galley_at(ui.painter(), rect.right() - gw, rect.center().y, g, FAINT());
                                gw + 10.0
                            });
                            let mut job = egui::text::LayoutJob::single_section(text.to_string(), mono_fmt(TXT_SMALL, color, 400.0));
                            job.wrap = egui::text::TextWrapping::truncate_at_width(w - indent - tag_w);
                            let g = ui.painter().layout_job(job);
                            galley_at(ui.painter(), rect.left() + indent, rect.center().y, g, color);
                            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, text));
                        };
                        row(ui, "{", 0.0, TEXT(), None);
                        for (line, fired) in lines {
                            row(ui, line, 14.0, if fired.is_some() { SUCCESS() } else { TEXT() }, fired.as_deref());
                        }
                        row(ui, "}", 0.0, TEXT(), None);
                    }
                }
            });
        });
        if w.scope.is_some() {
            ui.label(RichText::new("Runs this connection's rules with every scope, as if each field were in reach.").size(TXT_SMALL).color(FAINT()));
        }
    }

    fn rules_import(&mut self, scope: Option<u64>) {
        if self.ephemeral {
            return;
        }
        let Some(path) = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file() else { return };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => return self.err(format!("Could not read {}: {e}", path.display())),
        };
        match self.import_rules(scope, &text) {
            Ok(n) => self.toast_ok(format!("Imported {}", plural(n as u64, "rule")), None),
            Err(e) => self.err(format!("{}: {e}", path.display())),
        }
    }

    /// Adds the rules of an exported file to the global list, or to connection `scope`'s. A file that does not parse,
    /// or holds a rule that does not compile, changes nothing: such a rule would stop every agent call.
    pub(in crate::app) fn import_rules(&mut self, scope: Option<u64>, text: &str) -> Result<usize, String> {
        let rules: Vec<RuleSpec> = serde_json::from_str(text).map_err(|e| format!("not a Quarry rules file: {e}"))?;
        // Tested alone before anything changes.
        let none = crate::agents::config::Presets { secrets: crate::agents::config::Preset { on: false, ..self.agents.cfg.presets.secrets.clone() }, pii: crate::agents::config::Preset { on: false, ..self.agents.cfg.presets.pii.clone() } };
        Rules::compile(&none, &Default::default(), &rules, &[], &TESTER_KEY).map_err(|e| format!("Nothing was imported. {e}"))?;
        let n = rules.len();
        self.rule_list(scope).extend(rules);
        self.save_agents();
        Ok(n)
    }

    fn rules_export(&mut self, scope: Option<u64>) {
        if self.ephemeral {
            return;
        }
        let rules = self.rule_list(scope).clone();
        let Some(path) = rfd::FileDialog::new().set_file_name("quarry-rules.json").add_filter("JSON", &["json"]).save_file() else { return };
        match serde_json::to_string_pretty(&rules).map_err(|e| e.to_string()).and_then(|t| std::fs::write(&path, t).map_err(|e| e.to_string())) {
            Ok(()) => self.toast_ok(format!("Exported {}", plural(rules.len() as u64, "rule")), None),
            Err(e) => self.err(format!("Could not export the rules: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_import_that_does_not_compile_changes_nothing() {
        let mut h = egui_kittest::Harness::builder().with_accessibility_check(false).build_eframe(|cc| App::for_test(cc));
        let app = h.state_mut();
        let ok = serde_json::to_string(&vec![RuleSpec { on: true, matcher: Matcher::FieldName("^token$".into()), scope: Vec::new(), action: Action::Remove, mode: PresetMode::Hide }]).unwrap();
        let bad = ok.replace("^token$", "(");
        assert!(app.import_rules(None, &bad).unwrap_err().contains("Nothing was imported"));
        assert!(app.import_rules(Some(4), "{ not json").unwrap_err().contains("not a Quarry rules file"));
        assert!(app.agents.cfg.rules.is_empty() && !app.agents.cfg.conns.contains_key(&4), "nothing changed");
        assert_eq!(app.import_rules(None, &ok), Ok(1));
        assert_eq!(app.import_rules(Some(4), &ok), Ok(1));
        assert_eq!((app.agents.cfg.rules.len(), app.agents.cfg.conns[&4].rules.len()), (1, 1));
    }
}
