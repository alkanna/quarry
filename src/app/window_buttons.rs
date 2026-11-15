//! Minimize, Maximize and Close at the right of the header, on Windows where the header replaces the title bar
//! (`crate::win_frame`). Minimize and Close are ordinary egui buttons. Maximize is one too, for the tests and the
//! keyboard, but on Windows the window procedure answers the pointer there, so its hover and press come from there.

use super::*;

/// Each button's width, as Windows draws its own caption buttons.
pub(super) const BUTTON_W: f32 = 46.0;
/// The room the three take at the right of the header.
pub(super) const BUTTONS_W: f32 = BUTTON_W * 3.0;

fn flag() -> egui::Id {
    egui::Id::unique("quarry-win-frame")
}

/// Whether the header draws the window buttons: the window's own title bar is gone.
pub(super) fn active(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(flag())).unwrap_or(false)
}

/// Turns the window buttons on. Used once the frame is installed, and by tests and the demo switch.
pub(super) fn enable(ctx: &egui::Context) {
    ctx.data_mut(|d| d.insert_temp(flag(), true));
}

/// Replaces the native title bar on Windows. The demo's QUARRY_TEST_WINFRAME switch shows the buttons on any
/// platform, for screenshots.
pub(super) fn install(cc: &eframe::CreationContext<'_>, demo: bool) {
    if demo && std::env::var("QUARRY_TEST_WINFRAME").is_ok() {
        enable(&cc.egui_ctx);
    }
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let hwnd = cc.window_handle().ok().and_then(|h| match h.as_raw() {
            RawWindowHandle::Win32(w) => Some(w.hwnd.get()),
            _ => None,
        });
        let ctx = cc.egui_ctx.clone();
        match hwnd {
            Some(h) if crate::win_frame::install(h, move || ctx.request_repaint()) => enable(&cc.egui_ctx),
            _ => eprintln!("Quarry keeps the Windows title bar: the window could not be subclassed"),
        }
    }
}

/// Paints the three buttons at the right of the header, whose height `full` spans, and publishes where Maximize is.
pub(super) fn window_buttons(ui: &mut egui::Ui, full: egui::Rect) {
    let ctx = ui.ctx().clone();
    // The window's right edge: on macOS, where the demo switch shows them, `full` reaches past it by the wider left inset.
    let right = ctx.content_rect().right();
    let (maxed, focused) = ui.input(|i| (i.viewport().maximized.unwrap_or(false), i.viewport().focused.unwrap_or(true)));
    let (nc_hover, nc_press) = crate::win_frame::max_state();
    let glyph = if focused { TEXT() } else { FAINT() };
    for (i, name) in ["Minimize", if maxed { "Restore" } else { "Maximize" }, "Close"].into_iter().enumerate() {
        let x = right - BUTTONS_W + BUTTON_W * i as f32;
        let rect = egui::Rect::from_min_max(egui::pos2(x, full.top()), egui::pos2(x + BUTTON_W, full.bottom()));
        let r = ui.interact(rect, egui::Id::unique(("window-button", i)), Sense::click());
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, name));
        let mut state = state_of(&r);
        if i == 1 {
            state = state.max(if nc_press { 2 } else if nc_hover { 1 } else { 0 });
            let ppp = ctx.pixels_per_point();
            crate::win_frame::set_max_rect([rect.left(), rect.top(), rect.right(), rect.bottom()].map(|v| (v * ppp).round() as i32));
        }
        let (fill, ink) = if i == 2 && state > 0 { (shade_fill(ALERT(), state - 1), Color32::WHITE) } else { (hover_fill(state), glyph) };
        let p = ui.painter();
        p.rect_filled(rect, 0.0, fill);
        // Glyphs are drawn as shapes on whole pixels so their 1 px strokes stay sharp at every scale.
        let c = egui::pos2(p.round_to_pixel_center(rect.center().x), p.round_to_pixel_center(rect.center().y));
        let s = Stroke::new(1.0, ink);
        match i {
            0 => {
                p.hline(c.x - 5.0..=c.x + 5.0, c.y, s);
            }
            1 if maxed => {
                p.rect_stroke(egui::Rect::from_min_size(egui::pos2(c.x - 5.0, c.y - 3.0), egui::vec2(8.0, 8.0)), 1.0, s, egui::StrokeKind::Middle);
                p.line(vec![egui::pos2(c.x - 3.0, c.y - 3.0), egui::pos2(c.x - 3.0, c.y - 5.0), egui::pos2(c.x + 5.0, c.y - 5.0), egui::pos2(c.x + 5.0, c.y + 3.0), egui::pos2(c.x + 3.0, c.y + 3.0)], s);
            }
            1 => {
                p.rect_stroke(egui::Rect::from_center_size(c, egui::vec2(10.0, 10.0)), 1.0, s, egui::StrokeKind::Middle);
            }
            _ => {
                p.line_segment([egui::pos2(c.x - 5.0, c.y - 5.0), egui::pos2(c.x + 5.0, c.y + 5.0)], s);
                p.line_segment([egui::pos2(c.x - 5.0, c.y + 5.0), egui::pos2(c.x + 5.0, c.y - 5.0)], s);
            }
        }
        focus_ring(ui, &r, rect, 0.0);
        if r.clicked() {
            ctx.send_viewport_cmd(match i {
                0 => egui::ViewportCommand::Minimized(true),
                1 => egui::ViewportCommand::Maximized(!maxed),
                _ => egui::ViewportCommand::Close,
            });
        }
    }
}
