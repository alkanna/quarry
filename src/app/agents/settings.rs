//! Settings > AI agents: the switch, the listener's status, the line that adds Quarry to each client, and what
//! agents may do on each saved connection.

use super::*;
use crate::app::sql::create::plural;
use crate::agents::config::{Action, ConnPolicy, Mode, PresetMode, Tri};

#[derive(Clone, Copy, PartialEq, Default)]
pub(in crate::app) enum ClientKind {
    #[default]
    ClaudeCode,
    ClaudeDesktop,
    Cursor,
    Codex,
    Json,
}

const CLIENTS: [(ClientKind, &str); 5] = [(ClientKind::ClaudeCode, "Claude Code"), (ClientKind::ClaudeDesktop, "Claude Desktop"), (ClientKind::Cursor, "Cursor"), (ClientKind::Codex, "Codex"), (ClientKind::Json, "JSON")];

pub(super) const ACCESS: [(Access, &str); 3] = [(Access::NotShared, "Not shared"), (Access::Read, "Read"), (Access::ReadWrite, "Read and write")];

/// The executable agents start. Never current_exe after an update: on Linux it then names the moved-aside old
/// copy. On macOS install_target is the bundle, and the binary is inside it.
fn bridge_path(demo: bool) -> String {
    if demo {
        return "/Applications/Quarry.app/Contents/MacOS/quarry".into();
    }
    let p = match crate::update::install_target() {
        Some(t) if cfg!(target_os = "macos") => t.join("Contents").join("MacOS").join("quarry"),
        Some(t) => t,
        // A cargo build, which is never updated.
        None => std::env::current_exe().unwrap_or_else(|_| "quarry".into()),
    };
    p.display().to_string()
}

/// Quoted for the shell the user pastes into, only when it needs it.
fn shell_arg(s: &str, windows: bool) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/\\._-:+".contains(c)) {
        s.into()
    } else if windows {
        format!("\"{s}\"")
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// What the user is told to do with the snippet, and the snippet.
fn snippet(kind: ClientKind, path: &str, windows: bool) -> (&'static str, String) {
    let servers = || serde_json::to_string_pretty(&json!({"mcpServers": {"quarry": {"command": path, "args": ["mcp"]}}})).unwrap_or_default();
    match kind {
        ClientKind::ClaudeCode => ("Run this once in a terminal. Claude Code then starts Quarry's MCP bridge itself.", format!("claude mcp add --scope user quarry -- {} mcp", shell_arg(path, windows))),
        ClientKind::ClaudeDesktop => ("Add this to claude_desktop_config.json (Settings > Developer > Edit Config), then restart Claude Desktop.", servers()),
        ClientKind::Cursor => ("Add this to ~/.cursor/mcp.json for every project, or to .cursor/mcp.json in one project.", servers()),
        // A JSON string is a valid TOML basic string, escapes included.
        ClientKind::Codex => ("Add this to ~/.codex/config.toml.", format!("[mcp_servers.quarry]\ncommand = {}\nargs = [\"mcp\"]", serde_json::to_string(path).unwrap_or_default())),
        ClientKind::Json => ("For any other MCP client: a stdio server started with this command.", serde_json::to_string_pretty(&json!({"command": path, "args": ["mcp"]})).unwrap_or_default()),
    }
}

/// One line under each connection in the list: its access, then what differs from a plain read-only share.
pub(super) fn summary(p: Option<&ConnPolicy>) -> String {
    let Some(p) = p else { return "Agents can't see it".into() };
    let mut parts: Vec<String> = Vec::new();
    if p.access == Access::Read {
        parts.push("Reads only".into());
    } else {
        parts.push(match p.change {
            Mode::Ask => "Changes ask first",
            Mode::Allow => "Changes allowed",
            Mode::Never => "Reads only, changes off",
        }.into());
        parts.push(match p.scripts {
            Mode::Never => "scripts off",
            Mode::Ask => "scripts ask first",
            Mode::Allow => "scripts allowed",
        }.into());
    }
    for (t, name) in [(p.presets.secrets, "secrets"), (p.presets.pii, "personal data")] {
        match t {
            Tri::Off => parts.push(format!("{name} not protected")),
            Tri::On => parts.push(format!("{name} always protected")),
            Tri::Inherit => {}
        }
    }
    if !p.rules.is_empty() {
        parts.push(format!("{} of its own", plural(p.rules.len() as u64, "rule")));
    }
    parts.join(", ")
}

/// Whether this frame brought any input. Settings widgets change only on input, so the windows that save on change
/// copy and compare their config only then.
pub(super) fn had_input(ctx: &egui::Context) -> bool {
    ctx.input(|i| !i.events.is_empty() || i.pointer.any_pressed() || i.pointer.any_released())
}

/// A small tinted label painted in its own color, for engine marks and environment tags.
pub(super) fn chip(ui: &mut egui::Ui, text: &str, color: Color32, mono: bool) {
    let fmt = if mono { mono_fmt(10.0, color, W_SEMIBOLD) } else { ui_fmt(TXT_CAPTION, color, W_MEDIUM, 0.0) };
    let g = ui.painter().layout_job(egui::text::LayoutJob::single_section(text.to_string(), fmt));
    let (rect, _) = ui.allocate_exact_size(egui::vec2((g.size().x + 12.0).max(22.0), 18.0), Sense::hover());
    ui.painter().rect_filled(rect, R_SM, tint(color, 40));
    let dy = optical_dy(&g);
    ui.painter().galley(rect.center() - g.size() / 2.0 + egui::vec2(0.0, dy), g, color);
}

/// The engine mark of a saved connection, as the rail shows it.
pub(super) fn engine_chip(ui: &mut egui::Ui, c: &Conn) {
    let (mark, _) = super::super::rail::engine_mark(c.meta.engine, &c.server.product);
    chip(ui, mark, super::super::rail::mark_color(mark), true);
}

/// `toggle` with an accessible name: the switch is painted, so screen readers and the UI tests need one.
pub(super) fn named_toggle(ui: &mut egui::Ui, on: &mut bool, name: &str) -> egui::Response {
    let r = toggle(ui, on);
    let (enabled, state) = (ui.is_enabled(), *on);
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Switch, enabled, state, name));
    r
}

/// A dropdown of fixed choices. Returns true when the user picked another one.
pub(super) fn pick<T: PartialEq + Clone>(ui: &mut egui::Ui, id: impl std::hash::Hash + std::fmt::Debug, name: &str, w: f32, cur: &mut T, opts: &[(T, &str)]) -> bool {
    let label = opts.iter().find(|o| o.0 == *cur).map_or_else(String::new, |o| o.1.to_string());
    let mut to = None;
    let r = egui::ComboBox::from_id_salt(id).width(w).selected_text(label).show_ui(ui, |ui| {
        menu_look(ui);
        for (v, l) in opts {
            if ui.selectable_label(*v == *cur, *l).clicked() {
                to = Some(v.clone());
            }
        }
    });
    let enabled = ui.is_enabled();
    r.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, enabled, name));
    match to {
        Some(v) if v != *cur => {
            *cur = v;
            true
        }
        _ => false,
    }
}

/// A painted right chevron that opens something, framed on hover like a quiet button.
const ACCESS_W: f32 = 130.0;
const OPEN_W: f32 = 22.0;

pub(super) fn open_button(ui: &mut egui::Ui, name: &str) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(OPEN_W, OPEN_W), Sense::click());
    let state = state_of(&r);
    ui.painter().rect_filled(rect, R_SM, hover_fill(state));
    chevron(ui.painter(), rect.center(), false, if state > 0 { BRIGHT() } else { MUTED() });
    focus_ring(ui, &r, rect, R_SM as f32);
    let enabled = ui.is_enabled();
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, name));
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub(super) fn action_label(a: &Action) -> String {
    match a {
        Action::Remove => "Remove".into(),
        Action::Replace(t) => format!("Replace \"{t}\""),
        Action::Mask(n) => format!("Mask, keep {n}"),
        Action::Hash => "Hash".into(),
        Action::Redact => "Redact match".into(),
    }
}

/// What a preset does to a value it catches. The replacement text and the kept characters are fixed here; a custom
/// rule can choose others.
pub(super) fn preset_actions() -> [(Action, &'static str); 4] {
    [(Action::Replace("hidden".into()), "Replace"), (Action::Mask(4), "Mask, keep 4"), (Action::Hash, "Hash"), (Action::Remove, "Remove")]
}

pub(super) const MODES: [(PresetMode, &str); 2] = [(PresetMode::Hide, "Hide"), (PresetMode::MaskOutput, "Mask output")];

const ROWS: [(u32, &str); 5] = [(50, "50"), (100, "100"), (200, "200"), (500, "500"), (1000, "1,000")];
const BYTES: [(u32, &str); 4] = [(64 << 10, "64 KB"), (256 << 10, "256 KB"), (1 << 20, "1 MB"), (4 << 20, "4 MB")];
const SECS: [(u32, &str); 4] = [(10, "10 s"), (30, "30 s"), (60, "60 s"), (120, "2 min")];
const UNDO: [((u32, u64), &str); 3] = [((500, 32 << 20), "500 docs, 32 MB"), ((1000, 64 << 20), "1,000 docs, 64 MB"), ((5000, 256 << 20), "5,000 docs, 256 MB")];
const AUDIT: [(u64, &str); 3] = [(10 << 20, "10 MB"), (50 << 20, "50 MB"), (200 << 20, "200 MB")];

impl App {
    pub(in crate::app) fn agents_settings(&mut self, ui: &mut egui::Ui) {
        if let Some(id) = self.agents.page {
            if self.conn(id).is_some() && self.agents.cfg.enabled {
                return self.agents_conn_page(ui, id);
            }
            self.agents.page = None;
        }
        if let Some(t) = self.agents.distrust {
            self.agents_tampered_banner(ui, t);
        }
        let mut on = self.agents.cfg.enabled;
        settings_section(ui, "MCP SERVER", |ui| {
            let desc = if on { "Agents connect through MCP and see only the connections you share below." } else { "Agents such as Claude Code connect through MCP and work only with the connections you share with them. Off until you turn it on." };
            setting_row(ui, "Let AI agents use Quarry", desc, |ui| {
                named_toggle(ui, &mut on, "Let AI agents use Quarry");
            });
            if on {
                ui.separator();
                self.agents_status(ui);
            }
        });
        if on != self.agents.cfg.enabled {
            self.set_agents_enabled(on);
        }
        if !on {
            ui.add(egui::Label::new(RichText::new("While this is off, the quarry mcp command refuses every client and nothing about your connections leaves Quarry.").size(TXT_SMALL).color(MUTED())).wrap());
            return;
        }
        settings_section(ui, "CONNECT A CLIENT", |ui| {
            segmented(ui, &mut self.agents.client, &CLIENTS);
            let (how, text) = snippet(self.agents.client, &bridge_path(self.demo), cfg!(windows));
            ui.add(egui::Label::new(RichText::new(how).size(TXT_SMALL).color(MUTED())).wrap());
            let w = ui.available_width();
            egui::Frame::new().fill(pal().well).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
                ui.set_width(w - 20.0);
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(egui::vec2(w - 100.0, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.add(egui::Label::new(RichText::new(&text).font(FontId::monospace(TXT_SMALL)).color(TEXT())).wrap());
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                        if ui.button("Copy").clicked() {
                            ui.ctx().copy_text(text.clone());
                            self.toast_ok("Copied".into(), None);
                        }
                    });
                });
            });
            ui.label(RichText::new("The bridge talks to this window, so Quarry has to be open for agents to work.").size(TXT_SMALL).color(FAINT()));
        });
        self.agents_connections(ui);
        self.agents_protection(ui);
        self.agents_limits(ui);
        self.agents_audit(ui);
    }

    fn agents_tampered_banner(&mut self, ui: &mut egui::Ui, t: crate::agents::store::Trust) {
        let text = if t == crate::agents::store::Trust::NoKeychain {
            "Quarry could not read its key from the keychain to check the agent settings, so agents are off and nothing is shared. Allow keychain access, then review and save."
        } else {
            "Agent settings were changed outside Quarry, so agents are off, nothing is shared and every setting except your custom rules is back to its default. Review them, then share what you want."
        };
        let mut review = false;
        egui::Frame::new().fill(tint(WARNING(), 30)).stroke(Stroke::new(1.0, tint(WARNING(), 90))).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    review = ui.button("Review").clicked();
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        let (r, _) = ui.allocate_exact_size(egui::vec2(8.0, 14.0), Sense::hover());
                        ui.painter().circle_filled(r.center(), STATUS_DOT_R + 1.0, WARNING());
                        // Settings rows have no gap between items, and the dot needs one.
                        ui.add_space(6.0);
                        ui.add(egui::Label::new(RichText::new(text).size(TXT_SMALL).color(TEXT())).wrap());
                    });
                });
            });
        });
        ui.add_space(10.0);
        // Saving signs the settings as they are now, with nothing shared, so the banner does not come back.
        if review {
            self.save_agents();
        }
    }

    fn agents_status(&mut self, ui: &mut egui::Ui) {
        let n = self.agents.sessions.len();
        let (dot, title, sub) = match &self.agents.link {
            LinkState::Listening | LinkState::Idle => {
                let title = if n == 0 { "Running, no client connected".to_string() } else { format!("Running, {} connected", plural(n as u64, "client")) };
                let sub = self.agents.sessions.iter().map(|s| format!("{}, {}", if s.client.is_empty() { "An agent" } else { &s.client }, plural(s.calls as u64, "call"))).collect::<Vec<_>>().join("; ");
                (SUCCESS(), title, sub)
            }
            LinkState::Starting => (FAINT(), "Starting".to_string(), String::new()),
            LinkState::Busy => (WARNING(), "Served by another Quarry window".to_string(), "Agents work through that window. Quit it to serve them from this one.".to_string()),
            LinkState::Failed(e) => (ALERT(), "Could not start".to_string(), e.clone()),
        };
        let mut open = false;
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                open = ui.button("Activity").clicked();
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let (spot, _) = ui.allocate_exact_size(egui::vec2(8.0, 1.0), Sense::hover());
                    ui.add_space(6.0);
                    // The dot sits on the title's letters, not on the middle of the title and subtitle together.
                    let ink_y = ui
                        .vertical(|ui| {
                            let g = egui::WidgetText::from(medium(RichText::new(title).size(TXT_TITLE).color(BRIGHT()))).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::FontSelection::Default);
                            let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                            let ink_y = r.center().y - optical_dy(&g);
                            ui.painter().galley(r.min, g, BRIGHT());
                            if !sub.is_empty() {
                                ui.add(egui::Label::new(RichText::new(sub).size(TXT_SMALL).color(MUTED())).wrap());
                            }
                            ink_y
                        })
                        .inner;
                    ui.painter().circle_filled(egui::pos2(spot.center().x, ink_y), STATUS_DOT_R + 1.0, dot);
                });
            });
        });
        if open {
            self.agents.activity_open = true;
        }
    }

    fn agents_connections(&mut self, ui: &mut egui::Ui) {
        let mut picked = None;
        let mut page = None;
        settings_section(ui, "CONNECTIONS", |ui| {
            if self.conns.is_empty() {
                ui.label(RichText::new("No saved connections yet.").size(TXT_SMALL).color(MUTED()));
            }
            ui.horizontal(|ui| {
                // Same spacing and widths as the rows below, so the caption starts where the access combo does.
                ui.spacing_mut().item_spacing.x = 6.0;
                caption(ui, "CONNECTION");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // add_space adds no item spacing, unlike the open button it stands for.
                    ui.add_space(OPEN_W + ui.spacing().item_spacing.x);
                    ui.allocate_ui_with_layout(egui::vec2(ACCESS_W, ui.available_height()), egui::Layout::left_to_right(egui::Align::Center), |ui| caption(ui, "AGENTS CAN"));
                });
            });
            for c in &self.conns {
                ui.separator();
                let id = c.meta.id;
                let p = self.agents.cfg.conn(id);
                let mut cur = p.map_or(Access::NotShared, |p| p.access);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    engine_chip(ui, c);
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(medium(RichText::new(&c.meta.name).size(TXT_TITLE).color(BRIGHT())));
                            if !c.meta.tag.is_empty() {
                                chip(ui, &c.meta.tag, super::super::sidebar::tag_color(&c.meta.tag), false);
                            }
                        });
                        ui.label(RichText::new(summary(p)).size(TXT_SMALL).color(MUTED()));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if open_button(ui, &format!("Open {}", c.meta.name)).on_hover_text("What agents may do on this connection").clicked() {
                            page = Some(id);
                        }
                        if pick(ui, ("agent-access", id), &format!("Agent access for {}", c.meta.name), ACCESS_W, &mut cur, &ACCESS) {
                            picked = Some((id, cur));
                        }
                    });
                });
            }
        });
        ui.label(RichText::new("Open a connection to choose what agents may do on it. New connections start Not shared.").size(TXT_SMALL).color(FAINT()));
        ui.add_space(10.0);
        if let Some((id, a)) = picked {
            self.set_agent_access(id, a);
        }
        if let Some(id) = page {
            self.agents_open_page(id);
        }
    }

    /// Shares a connection with the defaults the first time, then only changes its access.
    pub(super) fn set_agent_access(&mut self, id: u64, a: Access) {
        let defaults = self.agents.cfg.defaults.clone();
        self.agents.cfg.conns.entry(id).or_insert(defaults).access = a;
        self.save_agents();
    }

    fn agents_protection(&mut self, ui: &mut egui::Ui) {
        let before = had_input(ui.ctx()).then(|| (self.agents.cfg.presets.clone(), self.agents.cfg.binary_as_size));
        let mut edit = false;
        let (n_secrets, n_pii) = (crate::agents::presets::secrets().len(), crate::agents::presets::pii().len());
        let custom = self.agents.cfg.rules.len();
        let own: Vec<(String, usize)> = self.conns.iter().filter_map(|c| self.agents.cfg.conns.get(&c.meta.id).filter(|p| !p.rules.is_empty()).map(|p| (c.meta.name.clone(), p.rules.len()))).collect();
        settings_section(ui, "DATA PROTECTION", |ui| {
            let presets = &mut self.agents.cfg.presets;
            for (i, (title, what, n, p)) in [("Secrets", "Passwords, tokens, API and cloud keys, private keys, and passwords inside connection strings.", n_secrets, &mut presets.secrets), ("Personal data", "Names, emails, phones, addresses, birth dates, national and tax IDs, cards and bank accounts.", n_pii, &mut presets.pii)].into_iter().enumerate() {
                if i > 0 {
                    ui.separator();
                }
                let off = if p.off.is_empty() { String::new() } else { format!(", {} off", p.off.len()) };
                setting_row(ui, title, &format!("{what} {n} checks{off}."), |ui| {
                    named_toggle(ui, &mut p.on, title);
                    ui.add_space(6.0);
                    ui.add_enabled_ui(p.on, |ui| {
                        pick(ui, ("preset-action", i), &format!("{title} action"), 100.0, &mut p.action, &preset_actions());
                    });
                });
            }
            ui.separator();
            let mut desc = match custom {
                0 => "None yet. Rules match field paths, field names, values or detectors".to_string(),
                n => format!("{} of yours everywhere", plural(n as u64, "rule")),
            };
            for (name, n) in &own {
                desc.push_str(&format!(", {n} on {name}"));
            }
            desc.push('.');
            setting_row(ui, "Custom rules", &desc, |ui| {
                edit = ui.button("Edit rules").clicked();
            });
            ui.separator();
            setting_row(ui, "Binary values", "Sent as their size only, like <binary 2,048 bytes>.", |ui| {
                named_toggle(ui, &mut self.agents.cfg.binary_as_size, "Binary values");
            });
        });
        ui.add(egui::Label::new(RichText::new("Checked on everything that leaves Quarry: rows, schema samples, plans and error messages. The tester in Edit rules shows what an agent would get.").size(TXT_SMALL).color(FAINT())).wrap());
        ui.add_space(10.0);
        if before.is_some_and(|b| b != (self.agents.cfg.presets.clone(), self.agents.cfg.binary_as_size)) {
            self.save_agents();
        }
        if edit {
            self.agents.rules = Some(RulesWin::default());
        }
    }

    fn agents_limits(&mut self, ui: &mut egui::Ui) {
        let before = had_input(ui.ctx()).then(|| self.agents.cfg.limits.clone());
        let l = &mut self.agents.cfg.limits;
        settings_section(ui, "LIMITS", |ui| {
            setting_row(ui, "Rows per answer", "Longer results are cut, and the agent is told how many were left out.", |ui| {
                pick(ui, "agent-rows", "Rows per answer", 90.0, &mut l.rows, &ROWS);
            });
            ui.separator();
            setting_row(ui, "Answer size", "Cut on a row boundary.", |ui| {
                pick(ui, "agent-bytes", "Answer size", 90.0, &mut l.bytes, &BYTES);
            });
            ui.separator();
            setting_row(ui, "Time per call", "The server stops the query after this long.", |ui| {
                pick(ui, "agent-secs", "Time per call", 90.0, &mut l.secs, &SECS);
            });
            ui.separator();
            setting_row(ui, "Undo for agent changes", "Old values are kept in memory until Quarry quits. Larger changes ask, and can't be undone.", |ui| {
                let mut cur = (l.undo_docs, l.undo_total_bytes);
                if pick(ui, "agent-undo", "Undo for agent changes", 150.0, &mut cur, &UNDO) {
                    (l.undo_docs, l.undo_total_bytes) = cur;
                    // One call may take at most a quarter of the total, as with the defaults.
                    l.undo_call_bytes = cur.1 / 4;
                }
            });
        });
        if before.is_some_and(|b| b != self.agents.cfg.limits) {
            self.save_agents();
        }
    }

    fn agents_audit(&mut self, ui: &mut egui::Ui) {
        let before = had_input(ui.ctx()).then(|| self.agents.cfg.audit.clone());
        let mut reveal = false;
        let a = &mut self.agents.cfg.audit;
        settings_section(ui, "AUDIT LOG", |ui| {
            setting_row(ui, "Keep an audit log", "Every call, its decision and a summary of the answer, never the data itself.", |ui| {
                named_toggle(ui, &mut a.on, "Keep an audit log");
            });
            ui.separator();
            ui.add_enabled_ui(a.on, |ui| {
                setting_row(ui, "Size on disk", "Oldest entries go first.", |ui| {
                    pick(ui, "agent-audit", "Audit log size", 90.0, &mut a.max_bytes, &AUDIT);
                });
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("audit.jsonl, 5 files of {} MB", (a.max_bytes / 5) >> 20)).font(FontId::monospace(TXT_SMALL)).color(FAINT()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    reveal = ui.button(super::activity::REVEAL).clicked();
                });
            });
        });
        if before.is_some_and(|b| b != self.agents.cfg.audit) {
            self.save_agents();
        }
        if reveal {
            self.agents_reveal();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippets_quote_and_escape_the_path() {
        let (_, cc) = snippet(ClientKind::ClaudeCode, "/Applications/Quarry.app/Contents/MacOS/quarry", false);
        assert_eq!(cc, "claude mcp add --scope user quarry -- /Applications/Quarry.app/Contents/MacOS/quarry mcp");
        let (_, cc) = snippet(ClientKind::ClaudeCode, "/home/o'neil/My Apps/quarry", false);
        assert_eq!(cc, r"claude mcp add --scope user quarry -- '/home/o'\''neil/My Apps/quarry' mcp");
        let win = r"C:\Program Files\Quarry\quarry.exe";
        let (_, cc) = snippet(ClientKind::ClaudeCode, win, true);
        assert_eq!(cc, r#"claude mcp add --scope user quarry -- "C:\Program Files\Quarry\quarry.exe" mcp"#);
        for kind in [ClientKind::ClaudeDesktop, ClientKind::Cursor] {
            let v: Value = serde_json::from_str(&snippet(kind, win, true).1).unwrap();
            assert_eq!(v["mcpServers"]["quarry"]["command"], win);
            assert_eq!(v["mcpServers"]["quarry"]["args"], json!(["mcp"]));
        }
        let v: Value = serde_json::from_str(&snippet(ClientKind::Json, win, true).1).unwrap();
        assert_eq!(v["command"], win);
        let (_, toml) = snippet(ClientKind::Codex, win, true);
        assert_eq!(toml, "[mcp_servers.quarry]\ncommand = \"C:\\\\Program Files\\\\Quarry\\\\quarry.exe\"\nargs = [\"mcp\"]");
    }
}
