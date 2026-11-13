//! The Agent activity window: every agent call this session, allowed or refused, with its arguments and answer, and
//! Undo call for a change an agent made.

use super::*;
use crate::app::sql::create::plural;

#[derive(Clone, Copy, PartialEq, Default)]
pub(in crate::app) enum Filter {
    #[default]
    All,
    Changes,
    Refused,
    Waiting,
}

const FILTERS: [(Filter, &str); 4] = [(Filter::All, "All"), (Filter::Changes, "Changes"), (Filter::Refused, "Refused"), (Filter::Waiting, "Waiting")];

/// Tools that change data or structure, or run something that may.
const CHANGES: [&str; 16] = ["insert", "update", "delete", "aggregate_into", "sql_insert_rows", "sql_update_rows", "sql_delete_rows", "create_collection", "drop_collection", "create_index", "drop_index", "create_table", "drop_table", "run_command", "run_sql", "run_shell"];

pub(super) const REVEAL: &str = if cfg!(target_os = "macos") {
    "Show in Finder"
} else if cfg!(windows) {
    "Show in Explorer"
} else {
    "Open folder"
};

fn shows(a: &Activity, f: Filter) -> bool {
    match f {
        Filter::All => true,
        Filter::Changes => CHANGES.contains(&a.tool.as_str()),
        Filter::Refused => matches!(a.outcome, Outcome::Refused(_) | Outcome::Denied | Outcome::TimedOut),
        Filter::Waiting => a.outcome == Outcome::Waiting,
    }
}

/// The badge before the summary, when the outcome needs one: an allowed call says only what it returned.
pub(super) fn badge_of(o: &Outcome) -> Option<(&'static str, Color32)> {
    match o {
        Outcome::Running => Some(("Running", MUTED())),
        Outcome::Waiting => Some(("Waiting", WARNING())),
        Outcome::Allowed => None,
        Outcome::Approved => Some(("Allowed by you", SUCCESS())),
        Outcome::Denied => Some(("Denied by you", ALERT())),
        Outcome::TimedOut => Some(("No answer", ALERT())),
        Outcome::Cancelled => Some(("Cancelled", MUTED())),
        Outcome::Refused(_) => Some(("Refused", ALERT())),
        Outcome::Failed(_) => Some(("Failed", ALERT())),
    }
}

/// A table cell `w` wide with its content at the left; `add_sized` would center it.
pub(super) fn cell(ui: &mut egui::Ui, w: f32, add: impl egui::Widget) {
    ui.allocate_ui_with_layout(egui::vec2(w, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_width(w);
        ui.add(add);
    });
}

/// A status label in the outcome's color, painted like `badge` but not clickable.
fn outcome_badge(ui: &mut egui::Ui, text: &str, fg: Color32) {
    let g = ui.painter().layout_job(egui::text::LayoutJob::single_section(text.to_string(), ui_fmt(TXT_CAPTION, fg, W_MEDIUM, 0.0)));
    let (rect, _) = ui.allocate_exact_size(egui::vec2(g.size().x + 12.0, 18.0), Sense::hover());
    ui.painter().rect_filled(rect, R_SM, tint(fg, 38));
    let dy = optical_dy(&g);
    ui.painter().galley(rect.center() - g.size() / 2.0 + egui::vec2(0.0, dy), g, fg);
}

impl App {
    /// Opens the audit log's folder in the file manager.
    pub(super) fn agents_reveal(&mut self) {
        if self.ephemeral {
            return;
        }
        let dir = crate::agents::link::agents_dir();
        let _ = std::fs::create_dir_all(&dir);
        let cmd = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(windows) {
            "explorer"
        } else {
            "xdg-open"
        };
        if let Err(e) = std::process::Command::new(cmd).arg(&dir).spawn() {
            self.err(format!("Could not open {}: {e}", dir.display()));
        }
    }

    /// The status bar pill: who is connected and how many calls they made, with the count waiting for you.
    pub(in crate::app) fn agents_pill(&mut self, ui: &mut egui::Ui) {
        let waiting = self.agents.asks.len();
        if self.agents.sessions.is_empty() && waiting == 0 {
            return;
        }
        let client = match self.agents.sessions.as_slice() {
            [] => "An agent".to_string(),
            [s] if !s.client.is_empty() => s.client.clone(),
            [_] => "An agent".to_string(),
            s => format!("{} agents", s.len()),
        };
        let calls: u32 = self.agents.sessions.iter().map(|s| s.calls).sum();
        let calls_txt = plural(calls as u64, "call");
        let name = ui.painter().layout_job(egui::text::LayoutJob::single_section(client.clone(), ui_fmt(TXT_CAPTION, PRIMARY(), W_MEDIUM, 0.0)));
        let count = ui.painter().layout_job(egui::text::LayoutJob::single_section(calls_txt.clone(), ui_fmt(TXT_CAPTION, MUTED(), 400.0, 0.0)));
        let badge = (waiting > 0).then(|| ui.painter().layout_job(egui::text::LayoutJob::single_section(waiting.to_string(), ui_fmt(10.0, Color32::from_rgb(0x15, 0x13, 0x0f), W_SEMIBOLD, 0.0))));
        const ICON: f32 = 12.0;
        let badge_w = badge.as_ref().map_or(0.0, |g| (g.size().x + 8.0).max(16.0) + 6.0);
        let w = 8.0 + ICON + 6.0 + name.size().x + 8.0 + count.size().x + badge_w + 8.0;
        let (rect, r) = ui.allocate_exact_size(egui::vec2(w, STATUS_CHIP_H), Sense::click());
        let state = state_of(&r);
        let p = ui.painter();
        p.rect_filled(rect, rect.height() / 2.0, tint(PRIMARY(), [26, 46, 66][state]));
        let cy = rect.center().y;
        let mut x = rect.left() + 8.0;
        // A plug: two prongs over a rounded body, painted because a fallback glyph would sit off center.
        let c = egui::pos2(x + ICON / 2.0, cy);
        let s = Stroke::new(1.4, PRIMARY());
        p.line_segment([c + egui::vec2(-2.5, -5.5), c + egui::vec2(-2.5, -2.5)], s);
        p.line_segment([c + egui::vec2(2.5, -5.5), c + egui::vec2(2.5, -2.5)], s);
        p.rect_stroke(egui::Rect::from_min_max(c + egui::vec2(-4.5, -2.5), c + egui::vec2(4.5, 2.0)), 2.0, s, egui::StrokeKind::Middle);
        p.line_segment([c + egui::vec2(0.0, 2.0), c + egui::vec2(0.0, 5.5)], s);
        x += ICON + 6.0;
        for (g, color) in [(name, PRIMARY()), (count, MUTED())] {
            let gw = g.size().x;
            galley_at(p, x, cy, g, color);
            x += gw + 8.0;
        }
        if let Some(g) = badge {
            let bw = (g.size().x + 8.0).max(16.0);
            let b = egui::Rect::from_center_size(egui::pos2(x - 2.0 + bw / 2.0, cy), egui::vec2(bw, 16.0));
            p.rect_filled(b, 8.0, WARNING());
            let dy = optical_dy(&g);
            p.galley(b.center() - g.size() / 2.0 + egui::vec2(0.0, dy), g, Color32::from_rgb(0x15, 0x13, 0x0f));
        }
        focus_ring(ui, &r, rect, rect.height() / 2.0);
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, if waiting > 0 { format!("AI agents: {client}, {calls_txt}, {waiting} waiting") } else { format!("AI agents: {client}, {calls_txt}") }));
        let r = r.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Agent activity");
        if r.clicked() {
            // The waiting call is already in front in its dialog; the list shows what else is waiting.
            self.agents.filter = if waiting > 0 { Filter::Waiting } else { Filter::All };
            self.agents.activity_open = true;
        }
    }

    pub(in crate::app) fn agents_activity_window(&mut self, ctx: &egui::Context) {
        if !self.agents.activity_open {
            return;
        }
        let mut keep = true;
        let (mut clear, mut undo, mut reveal) = (false, None, false);
        let screen = ctx.content_rect();
        let width = 760.0f32.min(screen.width() - 40.0);
        let names: HashMap<u64, String> = self.conns.iter().map(|c| (c.meta.id, c.meta.name.clone())).collect();
        let connected = match self.agents.sessions.as_slice() {
            [] => None,
            [s] => Some(format!("{} connected", if s.client.is_empty() { "An agent" } else { &s.client })),
            s => Some(format!("{} agents connected", s.len())),
        };
        let layer = egui::Id::unique(("quarry-layer", "agents-activity"));
        let r = top_banded(ctx, layer).show(ctx, |ui| {
            ui.set_width(width + 40.0);
            dialog_head_with(ui, &Head::new("Agent activity").sub("Every call an AI agent made through Quarry").glyph(glyphs::list), |ui| {
                if let Some(t) = &connected {
                    ui.label(RichText::new(t).size(TXT_SMALL).color(TEXT()));
                    let (d, _) = ui.allocate_exact_size(egui::vec2(8.0, 14.0), Sense::hover());
                    ui.painter().circle_filled(d.center(), STATUS_DOT_R, SUCCESS());
                } else {
                    ui.label(RichText::new("No agent connected").size(TXT_SMALL).color(MUTED()));
                }
            });
            let mut b = body_begin(ui);
            let outer = ui;
            let ui = &mut b.content_ui;
            ui.horizontal(|ui| {
                segmented(ui, &mut self.agents.filter, &FILTERS);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    clear = ui.add_enabled(!self.agents.activity.is_empty(), egui::Button::new("Clear")).on_hover_text("Empties this list. The audit log keeps every call.").clicked();
                });
            });
            ui.add_space(6.0);
            let cols = [76.0, 140.0, 210.0];
            card_frame().show(ui, |ui| {
                ui.set_width(width);
                egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 6)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for (t, w) in ["TIME", "TOOL", "WHERE"].into_iter().zip(cols) {
                            cell(ui, w, egui::Label::new(caption_text(t)));
                        }
                        caption(ui, "RESULT");
                    });
                });
                let rows: Vec<&Activity> = self.agents.activity.iter().rev().filter(|a| shows(a, self.agents.filter)).collect();
                egui::ScrollArea::vertical().id_salt("agents-activity").max_height(300.0).min_scrolled_height(120.0).auto_shrink([false, true]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    if rows.is_empty() {
                        egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 14)).show(ui, |ui| {
                            ui.label(RichText::new(if self.agents.activity.is_empty() { "No agent calls yet." } else { "No calls match this filter." }).size(TXT_SMALL).color(MUTED()));
                        });
                    }
                    for a in rows {
                        let on = self.agents.picked == Some(a.id);
                        let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, ROW_H + 6.0), Sense::click());
                        let state = state_of(&resp);
                        let fill = if on { tint(PRIMARY(), 46) } else { hover_fill(state) };
                        ui.painter().rect_filled(rect, 0.0, fill);
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(12.0, 0.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
                        // Text that takes no clicks, so a click on it selects the row.
                        child.style_mut().interaction.selectable_labels = false;
                        let mono = |s: &str, c: Color32| egui::Label::new(RichText::new(s).font(FontId::monospace(TXT_SMALL)).color(c)).truncate();
                        cell(&mut child, cols[0], mono(&a.time, FAINT()));
                        cell(&mut child, cols[1], mono(&a.tool, BRIGHT()));
                        child.allocate_ui_with_layout(egui::vec2(cols[2], 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.set_width(cols[2]);
                            let conn = a.conn.and_then(|id| names.get(&id)).map_or("-", |n| n.as_str());
                            ui.label(RichText::new(conn).size(TXT_SMALL).color(TEXT()));
                            ui.add(egui::Label::new(RichText::new(&a.ns).size(TXT_SMALL).color(MUTED())).truncate());
                        });
                        if let Some((t, c)) = badge_of(&a.outcome) {
                            outcome_badge(&mut child, t, c);
                        }
                        let detail = match &a.outcome {
                            Outcome::Refused(why) | Outcome::Failed(why) if a.summary.is_empty() => why.as_str(),
                            _ => a.summary.as_str(),
                        };
                        child.add(egui::Label::new(RichText::new(detail).size(TXT_SMALL).color(TEXT())).truncate());
                        resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Row, true, on, format!("{} {} at {}", a.tool, a.ns, a.time)));
                        if resp.clicked() {
                            self.agents.picked = if on { None } else { Some(a.id) };
                        }
                    }
                });
            });
            let picked = self.agents.picked.and_then(|id| self.agents.activity.iter().find(|a| a.id == id));
            if let Some(a) = picked {
                ui.add_space(8.0);
                let half = (width - 12.0) / 2.0;
                ui.horizontal_top(|ui| {
                    for (title, text) in [("ARGUMENTS", &a.args), ("ANSWER", &a.answer)] {
                        ui.allocate_ui_with_layout(egui::vec2(half, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                            ui.set_width(half);
                            caption(ui, title);
                            egui::Frame::new().fill(pal().well).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_MD).inner_margin(8).show(ui, |ui| {
                                ui.set_width(half - 16.0);
                                egui::ScrollArea::vertical().id_salt(("agents-detail", title)).max_height(140.0).show(ui, |ui| {
                                    let t = if text.is_empty() { "(none)" } else { text.as_str() };
                                    ui.add(egui::Label::new(RichText::new(t).font(FontId::monospace(TXT_SMALL)).color(TEXT())).wrap());
                                });
                            });
                        });
                    }
                });
                if let Outcome::Refused(why) | Outcome::Failed(why) = &a.outcome {
                    ui.add(egui::Label::new(RichText::new(why).size(TXT_SMALL).color(ALERT())).wrap());
                }
            }
            b.end(outer);
            let ui = outer;
            dialog_foot(ui, |ui| {
                let note = if picked.is_some() { "Arguments and answers are shown as scrubbed for the agent. Kept until Quarry quits." } else { "Select a call to see its arguments and answer. Kept until Quarry quits." };
                if ui.button("Done").on_hover_text("Esc").clicked() {
                    keep = false;
                }
                let can = picked.and_then(|a| a.undo).is_some();
                let tip = if can { "Puts back what this call changed" } else { "Select a change an agent made that can still be undone" };
                if ui.add_enabled(can, egui::Button::new("Undo call")).on_hover_text(tip).on_disabled_hover_text(tip).clicked() {
                    undo = picked.map(|a| a.id);
                }
                reveal = ui.button(REVEAL).on_hover_text("The audit log on disk").clicked();
                foot_note(ui, note);
            });
        });
        if clear {
            self.agents.activity.retain(|a| a.outcome == Outcome::Waiting || a.outcome == Outcome::Running);
            self.agents.picked = None;
        }
        if let Some(id) = undo {
            self.agent_undo(id);
        }
        if reveal {
            self.agents_reveal();
        }
        if !keep || r.should_close() {
            self.agents.activity_open = false;
        }
    }
}
