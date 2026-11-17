//! The header's update pill, which says what the updater is doing, and the popover it opens with
//! the progress, the release's highlights and the action that fits.

use super::*;
use crate::update::{Available, Kind, Release};

/// The look of the pill, one per state worth showing.
#[derive(Clone, Copy, PartialEq)]
enum Look {
    /// Downloading, with its percent when the size is known.
    Ring(Option<u8>),
    Spin,
    Ready,
    Failed,
    UpToDate,
    Offer,
}

enum PopAct {
    Btn(UpdBtn),
    Notes(Release),
}

/// The release's notes as a Release, for the What's new dialog.
fn notes_of(a: &Available) -> Release {
    Release { version: a.version.clone(), date: String::new(), sections: a.notes.clone() }
}

impl App {
    fn pill_state(&self) -> Option<(String, Look)> {
        Some(match &self.upd {
            Upd::Idle | Upd::Checking { manual: false } => return None,
            Upd::Checking { manual: true } => ("Checking for updates".into(), Look::Spin),
            Upd::UpToDate(_) => ("Up to date".into(), Look::UpToDate),
            Upd::Available(a) => (format!("Update to {}", a.version), Look::Offer),
            Upd::Downloading { info, prog, .. } => {
                use std::sync::atomic::Ordering::Relaxed;
                if prog.verifying.load(Relaxed) {
                    (format!("Verifying {}", info.version), Look::Spin)
                } else {
                    (format!("Downloading {}", info.version), Look::Ring(crate::update::percent(prog.bytes.load(Relaxed), info.size)))
                }
            }
            Upd::Ready(a) => (format!("Restart for {}", a.version), Look::Ready),
            Upd::Failed { .. } => ("Update failed".into(), Look::Failed),
        })
    }

    /// Drawn in the header's right-to-left row, left of the search field.
    pub(super) fn update_pill(&mut self, ui: &mut egui::Ui) {
        // Taken even when the pill is hidden, so a stale request never opens the popover later on its own.
        let open = std::mem::take(&mut self.upd_popup);
        let Some((label, look)) = self.pill_state() else { return };
        let (fill, edge, ink) = match look {
            Look::Ready => (SUCCESS(), Color32::TRANSPARENT, ON_PRIMARY()),
            Look::Failed => (tint(WARNING(), 30), tint(WARNING(), 90), WARNING()),
            Look::Offer => (tint(PRIMARY(), 30), tint(PRIMARY(), 90), PRIMARY()),
            Look::UpToDate => (Color32::TRANSPARENT, Color32::TRANSPARENT, MUTED()),
            Look::Ring(_) | Look::Spin => (RAISED(), BORDER(), TEXT()),
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
        let icon = if look == Look::Ready { ON_PRIMARY() } else { ink };
        match look {
            Look::Ring(n) => {
                p.circle_stroke(ic, 5.5, Stroke::new(2.0, RAISED_LINE()));
                // Unknown size: a quarter arc that turns, like the spinner.
                let (start, sweep) = match n {
                    Some(n) => (-std::f32::consts::FRAC_PI_2, n as f32 / 100.0 * std::f32::consts::TAU),
                    None => (ui.input(|i| i.time) as f32 * 4.0, std::f32::consts::FRAC_PI_2),
                };
                arc(&p, ic, 5.5, start, sweep, Stroke::new(2.0, PRIMARY()));
            }
            Look::Spin => {
                p.circle_stroke(ic, 5.5, Stroke::new(2.0, RAISED_LINE()));
                arc(&p, ic, 5.5, ui.input(|i| i.time) as f32 * 4.0, std::f32::consts::FRAC_PI_2, Stroke::new(2.0, PRIMARY()));
                // About 20 fps keeps the turn smooth without redrawing every view at the display's rate.
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
            }
            Look::Ready => glyphs::undo(&p, ic, icon),
            Look::Failed => glyphs::warn(&p, ic, icon),
            Look::UpToDate => glyphs::check(&p, ic, SUCCESS()),
            Look::Offer => glyphs::download(&p, ic, icon),
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
            Some(PopAct::Notes(r)) => self.modal = Some(Modal::WhatsNew(Box::new(WhatsNew::new(vec![r], "")))),
            None => {}
        }
    }

    fn update_popover(&self, ui: &mut egui::Ui) -> Option<PopAct> {
        let mut act = None;
        let current = env!("CARGO_PKG_VERSION");
        let (title, sub, tone, glyph): (String, String, Color32, glyphs::Glyph) = match &self.upd {
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
            let (t, _) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), Sense::hover());
            ui.painter().rect(t, 7.0, tint(tone, 36), Stroke::new(1.0, tint(tone, 84)), egui::StrokeKind::Inside);
            glyph(ui.painter(), t.center(), tone);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.label(semibold(RichText::new(title).size(TXT_TITLE).color(BRIGHT())));
                ui.label(RichText::new(sub).size(TXT_SMALL).color(MUTED()));
            });
        });
        ui.add_space(8.0);
        let pad = |ui: &mut egui::Ui, add: &mut dyn FnMut(&mut egui::Ui)| {
            egui::Frame::new().inner_margin(egui::Margin { left: 8, right: 8, top: 0, bottom: 8 }).show(ui, |ui| {
                ui.set_width(ui.available_width());
                add(ui);
            });
        };
        let wrap = |ui: &mut egui::Ui, s: &str, c: Color32| {
            ui.add(egui::Label::new(RichText::new(s).size(TXT_SMALL).color(c)).wrap());
        };
        match &self.upd {
            Upd::Idle | Upd::Checking { .. } => pad(ui, &mut |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(12.0).color(MUTED()));
                    ui.label(RichText::new("Asking GitHub for the latest release…").size(TXT_SMALL).color(MUTED()));
                });
            }),
            Upd::Downloading { info, prog, started } => pad(ui, &mut |ui| {
                use std::sync::atomic::Ordering::Relaxed;
                let (bytes, verifying) = (prog.bytes.load(Relaxed), prog.verifying.load(Relaxed));
                let (bar, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 4.0), Sense::hover());
                ui.painter().rect_filled(bar, 2.0, TRACK());
                let pct = if verifying { Some(100) } else { crate::update::percent(bytes, info.size) };
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
            Upd::Ready(_) if self.busy_work() => pad(ui, &mut |ui| {
                banner(ui, Notice::Warn, "Your work is still open.", "Save or discard your edits and wait for running scripts first.", None);
            }),
            Upd::Ready(_) => pad(ui, &mut |ui| wrap(ui, "Open connections reconnect after the restart. The new version also starts the next time you open Quarry.", MUTED())),
            Upd::Failed { msg, .. } => pad(ui, &mut |ui| wrap(ui, msg, TEXT())),
            Upd::UpToDate(_) | Upd::Available(_) => {}
        }
        let info = match &self.upd {
            Upd::Available(a) | Upd::Ready(a) | Upd::Downloading { info: a, .. } | Upd::Failed { info: Some(a), .. } => Some(a),
            _ => None,
        };
        let notes: Vec<(Kind, &str)> = info.map(|a| a.notes.iter().flat_map(|s| s.entries.iter().map(move |e| (s.kind, e.text.as_str()))).collect()).unwrap_or_default();
        if !notes.is_empty() {
            ui.separator();
            pad(ui, &mut |ui| {
                ui.add_space(6.0);
                ui.label(caption_text("HIGHLIGHTS"));
                ui.add_space(4.0);
                for (k, text) in notes.iter().take(3) {
                    ui.horizontal_top(|ui| {
                        let (d, _) = ui.allocate_exact_size(egui::vec2(6.0, 16.0), Sense::hover());
                        ui.painter().circle_filled(d.center(), 2.5, if *k == Kind::Fixed { SUCCESS() } else { PRIMARY() });
                        ui.add(egui::Label::new(RichText::new(*text).size(TXT_SMALL).color(TEXT())).wrap());
                    });
                }
                if notes.len() > 3 {
                    ui.label(RichText::new(format!("and {} more", notes.len() - 3)).size(TXT_SMALL).color(FAINT()));
                }
            });
        }
        if matches!(self.upd, Upd::Idle | Upd::Checking { .. } | Upd::UpToDate(_)) {
            ui.add_space(2.0);
            return None;
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            if let Some(a) = info.filter(|a| !a.notes.is_empty()) {
                if quiet(ui, "Release notes").clicked() {
                    act = Some(PopAct::Notes(notes_of(a)));
                }
            }
            if let Upd::Failed { msg, .. } = &self.upd {
                if quiet(ui, "Copy details").clicked() {
                    ui.ctx().copy_text(msg.clone());
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(4.0);
                // The same button Settings shows for this state.
                let Some(b) = self.update_status().2.filter(|b| *b != UpdBtn::Check) else { return };
                let r = if b.primary() { ui.add_enabled(b != UpdBtn::Restart || !self.busy_work(), primary_button(ui, b.label(), PRIMARY())) } else { ui.button(b.label()) };
                if r.clicked() {
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

/// An arc of `sweep` radians from `start`, 0 pointing right and growing clockwise.
fn arc(p: &egui::Painter, c: egui::Pos2, r: f32, start: f32, sweep: f32, s: Stroke) {
    let n = ((sweep.abs() / std::f32::consts::TAU) * 32.0).ceil().max(2.0) as usize;
    let pts = (0..=n).map(|i| c + egui::Vec2::angled(start + sweep * i as f32 / n as f32) * r).collect();
    p.add(egui::Shape::line(pts, s));
}
