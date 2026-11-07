//! The command log panel.

use super::*;

pub(super) fn ago(t: std::time::SystemTime) -> String {
    let s = t.elapsed().map_or(0, |d| d.as_secs());
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        _ => format!("{}h", s / 3600),
    }
}

impl App {
    pub(super) fn query_log(&mut self, ui: &mut egui::Ui) {
        if !self.log.open {
            return;
        }
        let frame = egui::Frame::new().fill(BG()).inner_margin(egui::Margin::symmetric(12, 6));
        if !self.log.detached {
            egui::Panel::bottom("query-log").resizable(true).default_size(200.0).min_size(90.0).frame(frame).show(ui, |ui| self.log_body(ui));
            return;
        }
        let ctx = ui.ctx().clone();
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("query-log"), egui::ViewportBuilder::default().with_title("Query log").with_inner_size([960.0, 420.0]), |ui, _| {
            egui::CentralPanel::default_margins().frame(frame).show(ui, |ui| self.log_body(ui));
            if ui.input(|i| i.viewport().close_requested()) {
                self.log.open = false;
            }
        });
    }

    pub(super) fn log_body(&mut self, ui: &mut egui::Ui) {
        let mut copied = false;
        let (log, conns) = (&mut self.log, &self.conns);
        ui.horizontal(|ui| {
            caption(ui, "QUERY LOG");
            ui.add(egui::TextEdit::singleline(&mut log.filter).hint_text("filter").desired_width(200.0).font(FontId::monospace(TXT_SMALL)));
            ui.checkbox(&mut log.errors, "Errors only");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("🗙").on_hover_text("Hide (Cmd+L)").clicked() {
                    log.open = false;
                }
                let (label, tip) = if log.detached { ("Dock", "Show at the bottom of the main window") } else { ("Detach", "Open in its own window") };
                if ui.button(label).on_hover_text(tip).clicked() {
                    log.detached = !log.detached;
                }
                if ui.button("Clear").clicked() {
                    db::LOG.lock().map(|mut l| l.clear()).ok();
                }
            });
        });
        ui.separator();
        // The driver threads write to the log too, so it stays locked only while visible rows are drawn.
        let Ok(entries) = db::LOG.lock() else { return };
        let rows: Vec<&db::LogEntry> = entries
            .iter()
            .filter(|l| (!log.errors || l.err.is_some()) && (log.filter.is_empty() || [&l.name, &l.db, &l.cmd].iter().any(|s| s.contains(log.filter.as_str()))))
            .collect();
        if rows.is_empty() {
            ui.label(RichText::new(if entries.is_empty() { "Commands sent to the servers show up here." } else { "No command matches." }).color(FAINT()));
            return;
        }
        // Painted rows of a known height: stick_to_bottom then sees the same content height every
        // frame, and the whole row takes the hover instead of selectable labels.
        const LOG_ROW_H: f32 = 20.0;
        // When, duration, connection and database, command name; the error and command text follow.
        const COLS: [f32; 4] = [40.0, 68.0, 190.0, 130.0];
        let mut copy = None;
        // Commands the log has evicted no longer need their expanded flag.
        if !log.expanded.is_empty() {
            log.expanded.retain(|k| entries.iter().any(|e| (e.conn, e.at) == *k));
        }
        egui::ScrollArea::vertical().auto_shrink(false).stick_to_bottom(true).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let w = ui.available_width();
            let font = FontId::monospace(TXT_SMALL);
            let text_x = 6.0 + COLS.iter().sum::<f32>();
            for l in &rows {
                let key = (l.conn, l.at);
                let open = log.expanded.contains(&key);
                // Expanded rows wrap the error and command under the text column.
                let body = open.then(|| {
                    let mut job = LayoutJob::default();
                    if let Some(e) = &l.err {
                        job.append(&format!("{e}\n"), 0.0, egui::TextFormat::simple(font.clone(), ALERT()));
                    }
                    job.append(&l.cmd, 0.0, egui::TextFormat::simple(font.clone(), TEXT()));
                    job.wrap.max_width = (w - text_x - 6.0).max(80.0);
                    ui.painter().layout_job(job)
                });
                let h = body.as_ref().map_or(LOG_ROW_H, |g| LOG_ROW_H.max(g.size().y + 6.0));
                let (rect, r) = ui.allocate_exact_size(egui::vec2(w, h), Sense::click());
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                let p = ui.painter_at(rect);
                if r.hovered() || open {
                    p.rect_filled(rect, R_SM, if r.hovered() { HOVER() } else { tint(HOVER(), 120) });
                }
                let failed = l.err.is_some();
                let name = conns.iter().find(|c| c.meta.id == l.conn).map_or("test", |c| c.meta.name.as_str());
                let ms = l.ms.map_or("running".to_string(), |ms| format!("{ms} ms"));
                let when = ago(l.at);
                let cy = rect.top() + LOG_ROW_H / 2.0;
                let mut x = rect.left() + 6.0;
                for ((text, color), cw) in [
                    (when.clone(), FAINT()),
                    (ms.clone(), if failed { ALERT() } else { MUTED() }),
                    (format!("{name} · {}", l.db), MUTED()),
                    (l.name.clone(), if failed { ALERT() } else { PRIMARY() }),
                ]
                .into_iter()
                .zip(COLS)
                {
                    let g = truncated(&p, &text, font.clone(), color, cw - 12.0);
                    p.galley(egui::pos2(x, cy - g.size().y / 2.0), g, color);
                    x += cw;
                }
                let mut cut = false;
                match body {
                    Some(g) => p.galley(egui::pos2(x, rect.top() + 3.0), g, TEXT()),
                    None => {
                        let first = l.err.as_deref().map(|e| (e.lines().next().unwrap_or_default(), ALERT()));
                        for (text, color) in first.into_iter().chain([(l.cmd.as_str(), TEXT())]) {
                            let g = truncated(&p, text, font.clone(), color, rect.right() - x - 6.0);
                            cut |= g.elided;
                            let gw = g.size().x;
                            p.galley(egui::pos2(x, cy - g.size().y / 2.0), g, color);
                            x += gw + 12.0;
                        }
                    }
                }
                let toggles = open || cut;
                let r = if toggles { r.on_hover_cursor(egui::CursorIcon::PointingHand) } else { r };
                if r.clicked() && toggles && !log.expanded.remove(&key) {
                    log.expanded.insert(key);
                }
                r.context_menu(|ui| {
                    if ui.button("🗐 Copy command").clicked() {
                        copy = Some(l.cmd.clone());
                    }
                    if let Some(e) = &l.err
                        && ui.button("🗐 Copy error").clicked()
                    {
                        copy = Some(e.clone());
                    }
                    if ui.button("🗐 Copy line").clicked() {
                        let err = l.err.as_deref().map(|e| format!(" {e}")).unwrap_or_default();
                        copy = Some(format!("{when} {ms} {name} · {} {}{err} {}", l.db, l.name, l.cmd));
                    }
                });
            }
        });
        if let Some(text) = copy {
            ui.ctx().copy_text(text);
            copied = true;
        }
        drop(entries);
        if copied {
            self.ok("Copied");
        }
    }
}
