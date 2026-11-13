//! The approval dialog for an agent call that asks first. It has its own queue and is drawn last in `ui()`, so it
//! stacks above Settings and every other dialog. Quarry's own facts come first; the agent's reason is untrusted
//! text shown below them. Nothing is a default for Enter, Esc denies, and Allow wakes up only after a second so a
//! keystroke meant for something else can't approve.

use super::*;

/// The executor denies a call this long after it was queued, before the agent's own 60 s timeout.
const DENY_AFTER: Duration = Duration::from_secs(50);
const WAKE: Duration = Duration::from_secs(1);

const SHELL_WARNING: &str = if cfg!(target_os = "macos") {
    "Scripts run in Quarry's shell with this connection's password and can read files on this Mac. Read the whole script before you allow it."
} else {
    "Scripts run in Quarry's shell with this connection's password and can read files on this computer. Read the whole script before you allow it."
};

const READS: [&str; 10] = ["list_connections", "list_databases", "list_collections", "describe", "find", "aggregate", "count", "explain", "select_rows", "query_sql"];
const STRUCTURE: [&str; 6] = ["create_collection", "drop_collection", "create_index", "drop_index", "create_table", "drop_table"];

/// "wants to ..." for the title, from the tool the agent called.
pub(super) fn verb(tool: &str) -> &'static str {
    match tool {
        "run_shell" => "run a shell script",
        "run_sql" | "run_command" => "run a script",
        t if STRUCTURE.contains(&t) => "change the structure",
        t if READS.contains(&t) => "read data",
        _ => "change data",
    }
}

/// One fact line: a dot, the line, and an optional dimmer second line under it.
fn fact(ui: &mut egui::Ui, dot: Color32, main: &str, sub: Option<&str>) {
    ui.horizontal_top(|ui| {
        let (r, _) = ui.allocate_exact_size(egui::vec2(8.0, 16.0), Sense::hover());
        ui.painter().circle_filled(r.center(), STATUS_DOT_R, dot);
        ui.vertical(|ui| {
            ui.add(egui::Label::new(RichText::new(main).color(TEXT())).wrap());
            if let Some(s) = sub {
                ui.add(egui::Label::new(RichText::new(s).size(TXT_SMALL).color(MUTED())).wrap());
            }
        });
    });
}

impl App {
    pub(in crate::app) fn agents_approval(&mut self, ctx: &egui::Context) {
        let Some(ask) = self.agents.asks.front_mut() else { return };
        let now = std::time::Instant::now();
        let shown = *ask.shown.get_or_insert(now);
        let ask = ask.clone();
        let total = self.agents.asks.len();
        let awake = now.saturating_duration_since(shown) >= WAKE;
        let conn = self.conn(ask.conn);
        let name = conn.map_or_else(|| "A connection".to_string(), |c| c.meta.name.clone());
        let id = egui::Id::unique(("quarry-layer", "agents-approval"));
        // Kept above Settings and any dialog opened after it (top_dialog), and the only modal that takes keys.
        // No default button: Enter must never approve, so it is taken here before any widget sees it.
        ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Enter));
        let esc = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
        let mut answer = esc.then_some(if ask.gone { Answer::Close } else { Answer::Deny });
        let w = 520.0f32.min(ctx.content_rect().width() - 40.0);
        let shell = ask.tool == "run_shell";
        let p = self.agents.cfg.conn(ask.conn);
        let large_change = p.map_or(0, |p| p.large_change);
        let undo_docs = self.agents.cfg.limits.undo_docs;
        let client = if ask.client.is_empty() { "An agent".to_string() } else { ask.client.clone() };
        top_dialog(ctx, id).show(ctx, |ui| {
            ui.set_width(w);
            ui.horizontal(|ui| {
                let title = if ask.gone { format!("{client} stopped waiting") } else { format!("{client} wants to {}", verb(&ask.tool)) };
                dialog_title(ui, title);
                if total > 1 {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(format!("1 of {total}")).size(TXT_SMALL).color(MUTED()));
                    });
                }
            });
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if let Some(c) = conn {
                    super::settings::engine_chip(ui, c);
                }
                ui.label(RichText::new(&name).color(TEXT()));
                if !ask.ns.is_empty() {
                    let (r, _) = ui.allocate_exact_size(egui::vec2(8.0, 16.0), Sense::hover());
                    chevron(ui.painter(), r.center(), false, FAINT());
                    ui.label(RichText::new(&ask.ns).color(TEXT()));
                }
            });
            ui.add_space(10.0);
            ui.scope(|ui| {
                if ask.gone {
                    ui.multiply_opacity(0.45);
                }
                if shell && !ask.gone {
                    egui::Frame::new().fill(tint(ALERT(), 34)).stroke(Stroke::new(1.0, tint(ALERT(), 130))).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
                        ui.set_width(w - 20.0);
                        ui.horizontal_top(|ui| {
                            let (r, _) = ui.allocate_exact_size(egui::vec2(8.0, 16.0), Sense::hover());
                            ui.painter().circle_filled(r.center(), STATUS_DOT_R, ALERT());
                            ui.add(egui::Label::new(RichText::new(SHELL_WARNING).color(TEXT())).wrap());
                        });
                    });
                    ui.add_space(8.0);
                }
                if !ask.op.is_empty() {
                    caption(ui, &ask.op);
                }
                egui::Frame::new().fill(pal().well).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
                    ui.set_width(w - 20.0);
                    egui::ScrollArea::vertical().id_salt("agents-approval-code").max_height(if shell { 260.0 } else { 160.0 }).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(&ask.code).font(FontId::monospace(TXT_SMALL)).color(TEXT())).wrap());
                    });
                });
                ui.add_space(8.0);
                if ask.gone {
                    fact(ui, FAINT(), "Nothing was run. The agent cancelled the call or gave up waiting.", None);
                    return;
                }
                for f in &ask.facts {
                    let (main, sub) = f.split_once('\n').map_or((f.as_str(), None), |(a, b)| (a, Some(b)));
                    fact(ui, FAINT(), main, sub);
                }
                let why = &ask.why;
                if let Some(n) = why.large {
                    fact(ui, WARNING(), &format!("Touches {} rows or documents, above your limit of {}.", group(&n.to_string()), group(&large_change.to_string())), Some("This asks even where changes are allowed."));
                }
                if let Some(why) = &ask.no_undo {
                    let main = if why.is_empty() { format!("More than the {} Quarry keeps to undo an agent change.", group(&undo_docs.to_string())) } else { why.clone() };
                    fact(ui, WARNING(), &main, Some("Allowing it means this can't be undone from Agent activity."));
                }
                if why.unlock {
                    fact(ui, WARNING(), &format!("{name} is locked for writes in Quarry."), Some("Allowing unlocks it for this session, for you and for agents."));
                }
                if why.connect {
                    fact(ui, WARNING(), &format!("{name} is not open."), Some("Allowing connects to it first."));
                }
                if shell {
                    fact(ui, FAINT(), "Its output is checked by the detectors, not by field rules.", None);
                }
                if let Some(reason) = ask.reason.as_deref().filter(|r| !r.trim().is_empty()) {
                    ui.add_space(4.0);
                    caption(ui, "AGENT'S STATED REASON");
                    ui.add(egui::Label::new(RichText::new(format!("\u{201c}{reason}\u{201d}")).color(TEXT())).wrap());
                    ui.label(RichText::new("The reason comes from the agent. Check what runs, not the reason.").size(TXT_SMALL).color(FAINT()));
                }
            });
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let left = if ask.gone {
                    "The agent stopped waiting".to_string()
                } else if !awake {
                    "Allow wakes up after 1 s".to_string()
                } else {
                    let left = DENY_AFTER.saturating_sub(now.saturating_duration_since(ask.at)).as_secs();
                    format!("Denied in {}:{:02}", left / 60, left % 60)
                };
                ui.label(RichText::new(left).size(TXT_SMALL).color(FAINT()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ask.gone {
                        if ui.button("Close").clicked() {
                            answer = Some(Answer::Close);
                        }
                        return;
                    }
                    let (text, a, fill) = if ask.no_undo.is_some() {
                        ("Allow without undo", Answer::WithoutUndo, ALERT())
                    } else if ask.why.unlock {
                        ("Unlock writes and allow once", Answer::UnlockOnce, PRIMARY())
                    } else {
                        ("Allow once", Answer::Once, PRIMARY())
                    };
                    if ui.add_enabled(awake, primary_button(ui, text, fill)).clicked() && awake {
                        answer = Some(a);
                    }
                    if ask.why.session_ok && ask.no_undo.is_none() && !ask.why.unlock && !shell && ui.add_enabled(awake, egui::Button::new("Allow for this session")).on_hover_text("Allows this kind of call on this connection until the agent disconnects").clicked() && awake {
                        answer = Some(Answer::Session);
                    }
                    if ui.button("Deny").on_hover_text("Esc").clicked() {
                        answer = Some(Answer::Deny);
                    }
                });
            });
        });
        // The countdown, and Allow waking up.
        ctx.request_repaint_after(if awake { Duration::from_millis(500) } else { WAKE.saturating_sub(now.saturating_duration_since(shown)) });
        if let Some(a) = answer {
            #[cfg(test)]
            self.agents.answered.push((ask.id, a));
            self.agent_answer(ask.id, a);
        }
    }
}
