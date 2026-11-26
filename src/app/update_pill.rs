//! The header's update pill and its popover.

use super::*;
use crate::update::Kind;
use quarry_core::action::{Action, UpdateA};

#[derive(Clone, Copy, PartialEq)]
enum Look {
    /// The percent done; None turns a quarter arc, for a check, a download of unknown size or the checksum.
    Ring(Option<u8>),
    Ready,
    Failed,
    UpToDate,
    Offer,
}

enum PopAct {
    Btn(UpdBtn),
    /// The notes of the release on offer.
    Notes,
}

impl App {
    fn pill_state(&self) -> Option<(String, Look)> {
        Some(match &self.core.upd {
            Upd::Idle | Upd::Checking { manual: false } => return None,
            Upd::Checking { manual: true } => ("Checking for updates".into(), Look::Ring(None)),
            Upd::UpToDate(_) => ("Up to date".into(), Look::UpToDate),
            Upd::Available(a) => (format!("Update to {}", a.version), Look::Offer),
            Upd::Downloading { info, prog, .. } => {
                use std::sync::atomic::Ordering::Relaxed;
                if prog.verifying.load(Relaxed) {
                    (format!("Verifying {}", info.version), Look::Ring(None))
                } else {
                    (format!("Downloading {}", info.version), Look::Ring(prog.percent(info.size)))
                }
            }
            Upd::Ready(a) => (format!("Restart for {}", a.version), Look::Ready),
            Upd::Failed { .. } => ("Update failed".into(), Look::Failed),
        })
    }

    pub(super) fn update_pill(&mut self, ui: &mut egui::Ui) {
        // Taken even when the pill is hidden, so a stale request never opens the popover later on its own.
        let open = std::mem::take(&mut self.ui.upd_popup);
        let Some((label, look)) = self.pill_state() else { return };
        let (fill, edge, ink) = match look {
            Look::Ready => (SUCCESS(), Color32::TRANSPARENT, ON_PRIMARY()),
            Look::Failed => (tint(WARNING(), 30), tint(WARNING(), 90), WARNING()),
            Look::Offer => (tint(PRIMARY(), 30), tint(PRIMARY(), 90), PRIMARY()),
            Look::UpToDate => (Color32::TRANSPARENT, Color32::TRANSPARENT, MUTED()),
            Look::Ring(_) => (RAISED(), BORDER(), TEXT()),
        };
        let p = ui.painter().clone();
        let weight = if look == Look::Ready { W_SEMIBOLD } else { W_MEDIUM };
        let g = p.layout_job(LayoutJob::single_section(label.clone(), ui_fmt(TXT_SMALL, ink, weight, 0.0)));
        let pct = match look {
            Look::Ring(Some(n)) => Some(p.layout_job(LayoutJob::single_section(format!("{n}%"), mono_fmt(TXT_CAPTION, MUTED(), 400.0)))),
            _ => None,
        };
        let w = 8.0 + 13.0 + 7.0 + g.size().x + pct.as_ref().map_or(0.0, |g| g.size().x + 7.0) + 10.0;
        let (rect, r) = ui.allocate_exact_size(egui::vec2(w, 26.0), Sense::click());
        let fill = if look == Look::UpToDate { hover_fill(state_of(&r)) } else { shade_fill(fill, state_of(&r)) };
        p.rect(rect, 13.0, fill, Stroke::new(1.0, edge), egui::StrokeKind::Inside);
        let ic = egui::pos2(rect.left() + 8.0 + 6.5, rect.center().y);
        match look {
            Look::Ring(n) => {
                p.circle_stroke(ic, 5.5, Stroke::new(2.0, RAISED_LINE()));
                let (start, sweep) = match n {
                    Some(n) => (-std::f32::consts::FRAC_PI_2, n as f32 / 100.0 * std::f32::consts::TAU),
                    None => {
                        // About 20 fps keeps the turn smooth; like `spin`, it rests while the window is in the background.
                        if ui.input(|i| i.viewport().focused.unwrap_or(true)) {
                            ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
                        }
                        (ui.input(|i| i.time) as f32 * 4.0, std::f32::consts::FRAC_PI_2)
                    }
                };
                arc(&p, ic, 5.5, start, sweep, Stroke::new(2.0, PRIMARY()));
            }
            Look::Ready => glyphs::undo(&p, ic, ink),
            Look::Failed => glyphs::warn(&p, ic, ink),
            Look::UpToDate => glyphs::check(&p, ic, SUCCESS()),
            Look::Offer => glyphs::download(&p, ic, ink),
        }
        let tx = rect.left() + 8.0 + 13.0 + 7.0;
        let dy = optical_dy(&g);
        let gw = g.size().x;
        p.galley(egui::pos2(tx, rect.center().y - g.size().y / 2.0 + dy), g, ink);
        if let Some(pg) = pct {
            let dy = optical_dy(&pg);
            p.galley(egui::pos2(tx + gw + 7.0, rect.center().y - pg.size().y / 2.0 + dy), pg, MUTED());
        }
        focus_ring(ui, &r, rect, 13.0);
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Update status"));
        let r = r.on_hover_cursor(egui::CursorIcon::PointingHand);
        if open {
            egui::Popup::open_id(ui.ctx(), egui::Popup::default_response_id(&r));
        }
        let mut act = None;
        egui::Popup::menu(&r).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).align(egui::RectAlign::BOTTOM_END).width(340.0).show(|ui| {
            menu_look(ui);
            act = self.update_popover(ui);
            if act.is_some() {
                ui.close();
            }
        });
        match act {
            Some(PopAct::Btn(b)) => self.upd_act(b),
            Some(PopAct::Notes) => self.exec(Action::Update(UpdateA::OfferedNotes)),
            None => {}
        }
    }

    fn update_popover(&self, ui: &mut egui::Ui) -> Option<PopAct> {
        let mut act = None;
        let current = env!("CARGO_PKG_VERSION");
        let (title, sub, tone, glyph): (String, String, Color32, glyphs::Glyph) = match &self.core.upd {
            Upd::Idle | Upd::Checking { .. } => ("Checking for updates".into(), format!("You have {current}"), PRIMARY(), glyphs::download),
            Upd::UpToDate(_) => (format!("Quarry {current}"), "This is the latest release".into(), SUCCESS(), glyphs::check),
            Upd::Available(a) => (format!("Quarry {}", a.version), format!("Ready to download  ·  you have {current}"), PRIMARY(), glyphs::download),
            Upd::Downloading { info, .. } => (format!("Quarry {}", info.version), format!("Downloading  ·  you have {current}"), PRIMARY(), glyphs::download),
            Upd::Ready(a) => (format!("Quarry {}", a.version), "Installed and matched to its published checksum".into(), SUCCESS(), glyphs::check),
            Upd::Failed { info, .. } => (info.as_ref().map_or("The update check failed".into(), |a| format!("Quarry {} was not installed", a.version)), format!("Nothing changed, you still have {current}"), WARNING(), glyphs::warn),
        };
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            head_tile(ui, glyph, tone);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.label(semibold(RichText::new(title).size(TXT_TITLE).color(BRIGHT())));
                ui.label(RichText::new(sub).size(TXT_SMALL).color(MUTED()));
            });
        });
        ui.add_space(8.0);
        match &self.core.upd {
            Upd::Idle | Upd::Checking { .. } => pad(ui, |ui| {
                ui.horizontal(|ui| {
                    spin(ui);
                    ui.label(RichText::new("Asking GitHub for the latest release…").size(TXT_SMALL).color(MUTED()));
                });
            }),
            Upd::Downloading { info, prog, started } => pad(ui, |ui| {
                use std::sync::atomic::Ordering::Relaxed;
                let (bytes, verifying) = (prog.bytes.load(Relaxed), prog.verifying.load(Relaxed));
                let pct = prog.percent(info.size);
                let (bar, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 4.0), Sense::hover());
                ui.painter().rect_filled(bar, 2.0, TRACK());
                if let Some(n) = pct {
                    let done = egui::Rect::from_min_size(bar.min, egui::vec2(bar.width() * n as f32 / 100.0, bar.height()));
                    ui.painter().rect_filled(done, 2.0, PRIMARY());
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let left = if verifying {
                        "Checking the download against its published checksum".to_string()
                    } else if info.size > 0 {
                        format!("{} of {}", human_bytes(bytes as usize), human_bytes(info.size as usize))
                    } else {
                        human_bytes(bytes as usize)
                    };
                    ui.label(RichText::new(left).size(TXT_SMALL).color(TEXT()));
                    let secs = started.elapsed().as_secs_f64();
                    if !verifying && info.size > bytes && bytes > 0 && secs > 1.0 {
                        let left = ((info.size - bytes) as f64 / (bytes as f64 / secs)).ceil() as u64;
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(format!("about {} left", if left < 60 { format!("{left} s") } else { format!("{} min", left.div_ceil(60)) })).size(TXT_SMALL).color(FAINT()));
                        });
                    }
                });
            }),
            Upd::Ready(_) if self.busy_work() => pad(ui, |ui| {
                banner(ui, Notice::Warn, "Your work is still open.", HELD, None);
            }),
            Upd::Ready(_) => pad(ui, |ui| wrap(ui, "Open connections reconnect after the restart. The new version also starts the next time you open Quarry.", MUTED())),
            Upd::Failed { msg, .. } => pad(ui, |ui| wrap(ui, msg, TEXT())),
            Upd::UpToDate(_) | Upd::Available(_) => {}
        }
        let info = self.core.upd.info();
        let total = info.map_or(0, |a| a.notes.iter().map(|s| s.entries.len()).sum::<usize>());
        if let Some(a) = info.filter(|_| total > 0) {
            ui.separator();
            pad(ui, |ui| {
                ui.add_space(6.0);
                ui.label(caption_text("HIGHLIGHTS"));
                ui.add_space(4.0);
                for (k, text) in a.notes.iter().flat_map(|s| s.entries.iter().map(move |e| (s.kind, e.text.as_str()))).take(3) {
                    ui.horizontal_top(|ui| {
                        let (d, _) = ui.allocate_exact_size(egui::vec2(6.0, 16.0), Sense::hover());
                        ui.painter().circle_filled(d.center(), 2.5, if k == Kind::Fixed { SUCCESS() } else { PRIMARY() });
                        ui.add(egui::Label::new(RichText::new(text).size(TXT_SMALL).color(TEXT())).wrap());
                    });
                }
                if total > 3 {
                    ui.label(RichText::new(format!("and {} more", total - 3)).size(TXT_SMALL).color(FAINT()));
                }
            });
        }
        if matches!(self.core.upd, Upd::Idle | Upd::Checking { .. } | Upd::UpToDate(_)) {
            ui.add_space(2.0);
            return None;
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            if info.is_some_and(|a| !a.notes.is_empty()) {
                if quiet(ui, "Release notes").clicked() {
                    act = Some(PopAct::Notes);
                }
            }
            if let Upd::Failed { msg, .. } = &self.core.upd {
                if quiet(ui, "Copy details").clicked() {
                    ui.ctx().copy_text(msg.clone());
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(4.0);
                let Some(b) = self.upd_btn().filter(|b| *b != UpdBtn::Check) else { return };
                if b.show(ui, self.busy_work()).clicked() {
                    act = Some(PopAct::Btn(b));
                }
                if b == UpdBtn::Restart && ui.button("Later").clicked() {
                    ui.close();
                }
            });
        });
        ui.add_space(4.0);
        act
    }
}

/// Popovers lay out justified, which stretches wrapped labels, so the content goes back to plain top-down.
fn pad(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new().inner_margin(egui::Margin { left: 8, right: 8, top: 0, bottom: 8 }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.with_layout(egui::Layout::top_down(egui::Align::Min), add);
    });
}

fn wrap(ui: &mut egui::Ui, s: &str, c: Color32) {
    ui.add(egui::Label::new(RichText::new(s).size(TXT_SMALL).color(c)).wrap());
}

/// An arc of `sweep` radians from `start`, 0 pointing right and growing clockwise.
fn arc(p: &egui::Painter, c: egui::Pos2, r: f32, start: f32, sweep: f32, s: Stroke) {
    let n = ((sweep.abs() / std::f32::consts::TAU) * 32.0).ceil().max(2.0) as usize;
    let pts = (0..=n).map(|i| c + egui::Vec2::angled(start + sweep * i as f32 / n as f32) * r).collect();
    p.add(egui::Shape::line(pts, s));
}
