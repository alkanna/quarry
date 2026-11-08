//! The activity rail: a thin column of icons left of the sidebar for the app's panels.

use super::*;

#[derive(Clone, Copy)]
enum RailIcon {
    Explorer,
    Search,
    Log,
    Server,
    Settings,
}

impl App {
    pub(super) fn rail(&mut self, ui: &mut egui::Ui) {
        let frame = egui::Frame::new().fill(CHROME()).inner_margin(egui::Margin { left: 8, right: 8, top: 10, bottom: 10 });
        egui::Panel::left("rail").exact_size(RAIL_W).resizable(false).show_separator_line(false).frame(frame).show(ui, |ui| {
            let full = ui.max_rect().expand2(egui::vec2(8.0, 10.0));
            ui.painter().vline(full.right() - 0.5, full.y_range(), Stroke::new(1.0, LINE()));
            ui.spacing_mut().item_spacing.y = 8.0;
            let connected = self.active.and_then(|id| self.conn(id)).is_some_and(|c| c.client.is_some());
            let palette = matches!(self.modal, Some(Modal::Palette { .. }));
            if rail_button(ui, RailIcon::Explorer, !self.prefs.sidebar_hidden, true, "Database explorer").clicked() {
                self.prefs.sidebar_hidden = !self.prefs.sidebar_hidden;
                self.save_prefs();
            }
            if rail_button(ui, RailIcon::Search, palette, true, "Jump to a collection (Cmd+K)").clicked() {
                self.modal = Some(Modal::Palette { query: String::new(), sel: 0 });
            }
            if rail_button(ui, RailIcon::Log, self.log.open, true, "Query log (Cmd+L)").clicked() {
                self.log.open = !self.log.open;
            }
            let tip = if connected { "Server status" } else { "Server status (connect first)" };
            if rail_button(ui, RailIcon::Server, self.server.is_some(), connected, tip).clicked()
                && let Some(conn) = self.active
            {
                self.req += 1;
                self.server = Some(ServerWin::new(conn, self.req));
            }
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                if rail_button(ui, RailIcon::Settings, matches!(self.modal, Some(Modal::Settings)), true, "Settings (Cmd+,)").clicked() {
                    self.modal = Some(Modal::Settings);
                }
            });
        });
    }
}

/// A 28 px square icon button: a `HOVER` fill on hover, `PRESSED` while held, and an accent
/// tint with a bright icon while its panel is open.
fn rail_button(ui: &mut egui::Ui, icon: RailIcon, active: bool, enabled: bool, tip: &str) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), if enabled { Sense::click() } else { Sense::hover() });
    // A disabled button only senses hover, so it never reports a press.
    let state = state_of(&r);
    let fill = if active { tint(PRIMARY(), if state == 2 { 90 } else { 64 }) } else if enabled { hover_fill(state) } else { Color32::TRANSPARENT };
    let p = ui.painter();
    p.rect_filled(rect, R_MD, fill);
    let color = if !enabled { FAINT() } else if active || state > 0 { BRIGHT() } else { MUTED() };
    paint_icon(p, rect.center(), icon, Stroke::new(1.3, color));
    focus_ring(ui, &r, rect, R_MD as f32);
    let r = r.on_hover_text(tip);
    if enabled { r.on_hover_cursor(egui::CursorIcon::PointingHand) } else { r }
}

/// Line icons drawn in a 16 px box, so they need no icon font.
fn paint_icon(p: &egui::Painter, c: egui::Pos2, icon: RailIcon, s: Stroke) {
    let v = egui::vec2;
    match icon {
        // A window with a left panel.
        RailIcon::Explorer => {
            let r = egui::Rect::from_center_size(c, v(15.0, 13.0));
            p.rect_stroke(r, 2.5, s, egui::StrokeKind::Middle);
            p.vline(r.left() + 5.0, r.y_range(), s);
        }
        RailIcon::Search => {
            p.circle_stroke(c + v(-1.5, -1.5), 5.0, s);
            p.line_segment([c + v(2.2, 2.2), c + v(6.0, 6.0)], s);
        }
        // Rows of a log, with a bullet each.
        RailIcon::Log => {
            for dy in [-4.5, 0.0, 4.5] {
                p.circle_filled(c + v(-5.5, dy), 1.1, s.color);
                p.line_segment([c + v(-2.5, dy), c + v(6.5, dy)], s);
            }
        }
        // A pulse line.
        RailIcon::Server => {
            let pts = [v(-7.5, 0.5), v(-4.0, 0.5), v(-2.0, -5.0), v(1.0, 5.5), v(3.0, 0.5), v(7.5, 0.5)];
            p.line(pts.iter().map(|d| c + *d).collect(), s);
        }
        // A gear: ring, hub and eight teeth.
        RailIcon::Settings => {
            p.circle_stroke(c, 4.6, s);
            p.circle_stroke(c, 1.8, s);
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::FRAC_PI_4;
                let d = v(a.cos(), a.sin());
                p.line_segment([c + d * 4.6, c + d * 7.0], s);
            }
        }
    }
}
