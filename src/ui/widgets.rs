//! Shared widgets: buttons, dialogs, cards, chips and painted controls.
//!
//! Every interactive element follows one state model: idle, hovered (brighter text or a `HOVER`
//! fill), pressed (a `PRESSED` fill or a darker shade) and focused (an accent outline).

use super::theme::*;
use eframe::egui::{self, Color32, FontId, RichText, Sense, Stroke};

/// A menu that stays open while you type or click inside it, closing on a click outside or Esc.
/// Plain egui menus close on any click inside, which made their text boxes unusable.
pub(crate) fn sticky_menu(ui: &mut egui::Ui, title: String, content: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    use egui::containers::menu::{MenuButton, MenuConfig};
    ui.scope(|ui| {
        quiet_look(ui);
        MenuButton::new(title).config(MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)).ui(ui, |ui| {
            menu_look(ui);
            content(ui)
        }).0
    }).inner
}

/// Menu content that scrolls once it is taller than the window. egui keeps a popup inside the
/// window, so a taller one is cut off at the edge with its last items out of reach.
pub(crate) fn menu_scroll<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let max_h = ui.ctx().content_rect().height() - 24.0;
    egui::ScrollArea::vertical().max_height(max_h).show(ui, add).inner
}

/// Menu and dropdown items: tight corners, no outline, and a hover fill that shows on the raised
/// popup. egui builds menu styles from the app style, so each menu calls this first.
pub(crate) fn menu_look(ui: &mut egui::Ui) {
    let w = &mut ui.visuals_mut().widgets;
    for s in [&mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        s.corner_radius = egui::CornerRadius::same(R_SM);
        s.bg_stroke = Stroke::NONE;
    }
    w.hovered.weak_bg_fill = MENU_HOVER();
    w.active.weak_bg_fill = menu_pressed();
    w.open.weak_bg_fill = MENU_HOVER();
}

/// Buttons without a frame at rest: muted text that brightens on hover over a `HOVER` fill.
/// Applies to what is added to `ui` after it, so callers scope it.
pub(crate) fn quiet_look(ui: &mut egui::Ui) {
    let w = &mut ui.visuals_mut().widgets;
    w.inactive.fg_stroke.color = MUTED();
    w.inactive.weak_bg_fill = Color32::TRANSPARENT;
    w.inactive.bg_stroke = Stroke::NONE;
    w.hovered.weak_bg_fill = HOVER();
    w.hovered.bg_stroke = Stroke::NONE;
    w.active.weak_bg_fill = PRESSED();
    w.active.bg_stroke = Stroke::NONE;
    w.open.weak_bg_fill = PRESSED();
    w.open.bg_stroke = Stroke::NONE;
}

/// The one style for labels over a group: sections, lists, menus and column headers.
pub(crate) fn caption_text(text: impl Into<String>) -> RichText {
    semibold(RichText::new(text).size(TXT_CAPTION).color(MUTED()).extra_letter_spacing(0.6))
}

pub(crate) fn caption(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    ui.label(caption_text(text))
}

/// A titled card holding a group of settings.
pub(crate) fn settings_section(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    caption(ui, title);
    ui.add_space(2.0);
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 6.0;
        add(ui);
    });
    ui.add_space(10.0);
}

/// One setting: its name and a short explanation on the left, the control on the right.
pub(crate) fn setting_row(ui: &mut egui::Ui, title: &str, desc: &str, control: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        let w = ui.available_width();
        ui.vertical(|ui| {
            ui.set_width((w - 230.0).max(160.0));
            ui.label(medium(RichText::new(title).size(TXT_TITLE).color(BRIGHT())));
            ui.add(egui::Label::new(RichText::new(desc).size(TXT_SMALL).color(MUTED())).wrap());
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), control);
    });
}

/// "−  n  +" for a small whole number (the control slot is right-to-left, so added in reverse).
pub(crate) fn stepper(ui: &mut egui::Ui, value: &mut usize, min: usize, max: usize) {
    egui::Frame::new().fill(pal().well).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_MD).inner_margin(2).show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        quiet_look(ui);
        if ui.add_enabled(*value < max, egui::Button::new(RichText::new("+").size(TXT_TITLE)).corner_radius(R_SM)).clicked() {
            *value += 1;
        }
        ui.add_sized([34.0, 20.0], egui::Label::new(RichText::new(value.to_string()).font(FontId::monospace(TXT_BODY)).color(BRIGHT())));
        if ui.add_enabled(*value > min, egui::Button::new(RichText::new("−").size(TXT_TITLE)).corner_radius(R_SM)).clicked() {
            *value -= 1;
        }
    });
}

/// An on/off switch.
pub(crate) fn toggle(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let (rect, mut r) = ui.allocate_exact_size(egui::vec2(38.0, 22.0), Sense::click());
    if r.clicked() {
        *on = !*on;
        r.mark_changed();
    }
    let t = ui.ctx().animate_bool(r.id, *on);
    let enabled = ui.is_enabled();
    let mut bg = if enabled { pal().track.lerp_to_gamma(PRIMARY(), t) } else { HOVER() };
    if enabled && r.hovered() {
        bg = shade(bg, if r.is_pointer_button_down_on() { 0.20 } else { 0.10 });
    }
    ui.painter().rect_filled(rect, rect.height() / 2.0, bg);
    let x = egui::lerp(rect.left() + 11.0..=rect.right() - 11.0, t);
    // The knob grows a little while pressed, like a macOS switch.
    let knob = if enabled && r.is_pointer_button_down_on() { 9.0 } else { 8.0 };
    let knob_c = if !enabled { MUTED() } else if *on { ON_PRIMARY() } else { knob_off() };
    ui.painter().circle_filled(egui::pos2(x, rect.center().y), knob, knob_c);
    focus_ring(ui, &r, rect, rect.height() / 2.0);
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The knob of an off switch: white on the light track, light grey on the dark one.
fn knob_off() -> Color32 {
    if is_light() { RAISED() } else { BRIGHT() }
}

/// An accent outline around a custom-painted widget that has keyboard focus.
pub(crate) fn focus_ring(ui: &egui::Ui, r: &egui::Response, rect: egui::Rect, radius: f32) {
    if r.has_focus() {
        ui.painter().rect_stroke(rect.expand(2.0), radius + 2.0, Stroke::new(1.0, PRIMARY()), egui::StrokeKind::Outside);
    }
}

/// 0 idle, 1 hovered, 2 pressed.
pub(crate) fn state_of(r: &egui::Response) -> usize {
    if r.is_pointer_button_down_on() {
        2
    } else if r.hovered() {
        1
    } else {
        0
    }
}

/// A row of joined buttons where exactly one is chosen: a bordered track with the chosen option
/// raised inside it.
pub(crate) fn segmented<T: PartialEq + Copy>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)]) {
    // Painted at a fixed height: a Frame grew with the row, leaving the track taller than its options.
    const PAD: f32 = 2.0;
    const GAP: f32 = 2.0;
    let pad_x = ui.spacing().button_padding.x;
    let galleys: Vec<_> = options.iter().map(|(v, l)| ui.painter().layout_job(egui::text::LayoutJob::single_section(l.to_string(), ui_fmt(TXT_SMALL, Color32::PLACEHOLDER, if *value == *v { W_MEDIUM } else { 400.0 }, 0.0)))).collect();
    let widths: Vec<f32> = galleys.iter().map(|g| g.size().x + pad_x * 2.0).collect();
    let total = widths.iter().sum::<f32>() + GAP * (options.len().saturating_sub(1)) as f32 + PAD * 2.0;
    let base = ui.next_auto_id();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(total, BUTTON_H), Sense::hover());
    ui.painter().rect(rect, R_MD, PANEL(), Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
    let mut x = rect.left() + PAD;
    for (i, (((v, label), g), w)) in options.iter().zip(galleys).zip(widths).enumerate() {
        let seg = egui::Rect::from_min_size(egui::pos2(x, rect.top() + PAD), egui::vec2(w, BUTTON_H - PAD * 2.0));
        x += w + GAP;
        let r = ui.interact(seg, base.with(i), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
        let on = *value == *v;
        let state = state_of(&r);
        let fill = if on { [RAISED(), RAISED(), PRESSED()][state] } else { [Color32::TRANSPARENT, HOVER(), PRESSED()][state] };
        ui.painter().rect_filled(seg, R_SM, fill);
        if on && is_light() {
            ui.painter().rect_stroke(seg, R_SM, Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
        }
        let dy = optical_dy(&g);
        let text_c = if on || state > 0 { BRIGHT() } else { MUTED() };
        ui.painter().galley(seg.center() - g.size() / 2.0 + egui::vec2(0.0, dy), g, text_c);
        focus_ring(ui, &r, seg, R_SM as f32);
        let enabled = ui.is_enabled();
        r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, enabled, on, *label));
        if r.clicked() {
            *value = *v;
        }
    }
}

/// The pill look shared by `chip`, `badge` and `paint_tag`. Only colors change with `state` (0 idle,
/// 1 hovered, 2 pressed); the rect never does, because egui buttons with a custom stroke lose a pixel
/// on hover. `fg` only colors a chip that is on; one that is off is muted.
pub(crate) fn paint_chip(p: &egui::Painter, rect: egui::Rect, text: std::sync::Arc<egui::Galley>, fg: Color32, on: bool, state: usize) {
    let (text_c, line, fill) = if on {
        (fg, tint(fg, 90), tint(fg, [31, 51, 71][state]))
    } else {
        (if state > 0 { BRIGHT() } else { MUTED() }, BORDER(), [Color32::TRANSPARENT, HOVER(), PRESSED()][state])
    };
    p.rect(rect, rect.height() / 2.0, fill, Stroke::new(1.0, line), egui::StrokeKind::Inside);
    let dy = optical_dy(&text);
    p.galley(rect.center() - text.size() / 2.0 + egui::vec2(0.0, dy), text, text_c);
}

/// A rounded label in `fg`, or muted with a plain border when off, as in one-of choices like connection tags.
pub(crate) fn chip(ui: &mut egui::Ui, text: &str, fg: Color32, on: bool) -> egui::Response {
    let g = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(TXT_SMALL), Color32::PLACEHOLDER);
    let (rect, r) = ui.allocate_exact_size(egui::vec2(g.size().x + 20.0, CHIP_H), Sense::click());
    paint_chip(ui.painter(), rect, g, fg, on, state_of(&r));
    focus_ring(ui, &r, rect, rect.height() / 2.0);
    let enabled = ui.is_enabled();
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, enabled, on, text));
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A small square-cornered status label in mono caps, like READ-ONLY. Clickable: it tints
/// stronger on hover and press.
pub(crate) fn badge(ui: &mut egui::Ui, text: &str, fg: Color32) -> egui::Response {
    let g = ui.painter().layout_job(egui::text::LayoutJob::single_section(text.to_string(), mono_fmt(10.0, fg, W_SEMIBOLD)));
    let (rect, r) = ui.allocate_exact_size(egui::vec2(g.size().x + 16.0, 18.0), Sense::click());
    let state = state_of(&r);
    ui.painter().rect_filled(rect, R_SM, tint(fg, [31, 51, 71][state]));
    let dy = optical_dy(&g);
    ui.painter().galley(rect.center() - g.size() / 2.0 + egui::vec2(0.0, dy), g, fg);
    focus_ring(ui, &r, rect, R_SM as f32);
    let enabled = ui.is_enabled();
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, text));
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub(crate) fn paint_kbd(p: &egui::Painter, rect: egui::Rect, g: std::sync::Arc<egui::Galley>) {
    p.rect(rect, R_SM, RAISED(), Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
    let dy = optical_dy(&g);
    p.galley(rect.center() - g.size() / 2.0 + egui::vec2(0.0, dy), g, MUTED());
}

/// One of a row of underline tabs: muted at rest, a faint underline on hover, the accent
/// underline and bright text when chosen.
pub(crate) fn view_tab(ui: &mut egui::Ui, label: &str, active: bool) -> egui::Response {
    let g = ui.painter().layout_job(egui::text::LayoutJob::single_section(label.to_string(), ui_fmt(13.0, Color32::PLACEHOLDER, if active { W_MEDIUM } else { 400.0 }, 0.0)));
    let (rect, r) = ui.allocate_exact_size(egui::vec2(g.size().x, 30.0), Sense::click());
    let state = state_of(&r);
    let color = if active { BRIGHT() } else if state > 0 { TEXT() } else { MUTED() };
    let dy = optical_dy(&g);
    ui.painter().galley(egui::pos2(rect.left(), rect.center().y - g.size().y / 2.0 + dy - 1.0), g, color);
    let line = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - 2.0), rect.max);
    if active {
        ui.painter().rect_filled(line, 1.0, PRIMARY());
    } else if state > 0 {
        ui.painter().rect_filled(line, 1.0, BORDER());
    }
    focus_ring(ui, &r, rect, R_SM as f32);
    let enabled = ui.is_enabled();
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, enabled, active, label));
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

// Buttons come in five kinds, all BUTTON_H tall: `primary` for the main action, `ui.button` for
// ordinary actions (raised and outlined), `quiet` for toolbar extras and list rows (framed only on
// hover), `danger` for destructive actions, and painted controls above.

/// Framed only while hovered or pressed: rows in lists and menus, and low-key toolbar actions.
pub(crate) fn quiet_button<'a>(text: impl Into<egui::WidgetText>) -> egui::Button<'a> {
    egui::Button::new(text).frame_when_inactive(false)
}

/// A quiet button added in its own scope, so its look does not leak to later widgets.
pub(crate) fn quiet(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) -> egui::Response {
    quiet_enabled(ui, true, text)
}

pub(crate) fn quiet_enabled(ui: &mut egui::Ui, enabled: bool, text: impl Into<egui::WidgetText>) -> egui::Response {
    ui.scope(|ui| {
        quiet_look(ui);
        ui.add_enabled(enabled, quiet_button(text))
    })
    .inner
}

/// A destructive action: alert text inside a faint alert outline, tinting on hover and press.
pub(crate) fn danger_button<'a>(text: impl Into<String>) -> egui::Button<'a> {
    egui::Button::new(RichText::new(text).color(ALERT())).fill(Color32::TRANSPARENT).stroke(Stroke::new(1.0, tint(ALERT(), 90)))
}

pub(crate) fn danger(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    let fill = danger_fill(ui);
    ui.add(danger_button(text).fill(fill))
}

/// The fill of the next danger button for its current state.
fn danger_fill(ui: &egui::Ui) -> Color32 {
    [Color32::TRANSPARENT, tint(ALERT(), 31), tint(ALERT(), 56)][next_state(ui)]
}

/// A destructive menu item: alert text, with an alert-tinted hover instead of the menu's.
pub(crate) fn danger_item(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    ui.scope(|ui| {
        let w = &mut ui.visuals_mut().widgets;
        w.hovered.weak_bg_fill = tint(ALERT(), 31);
        w.active.weak_bg_fill = tint(ALERT(), 56);
        ui.add(egui::Button::new(RichText::new(text).color(ALERT())).frame_when_inactive(false))
    })
    .inner
}

/// The main action of a query card: a play mark, the label and its key hint, on the accent.
/// Painted, not a Button with one text job: the glyph, the label and the smaller key hint share
/// a baseline there, so the triangle (from a fallback font) and the hint sat off center. Here
/// each part is centered on its own.
pub(crate) fn run_button(ui: &mut egui::Ui, text: &str, keys: &str) -> egui::Response {
    const TRI: f32 = 8.0;
    let label = ui.painter().layout_job(egui::text::LayoutJob::single_section(text.to_string(), ui_fmt(TXT_BODY, ON_PRIMARY(), W_SEMIBOLD, 0.0)));
    let hint = ui.painter().layout_job(egui::text::LayoutJob::single_section(keys.to_string(), mono_fmt(TXT_CAPTION, tint(ON_PRIMARY(), 170), 400.0)));
    let content = TRI + 8.0 + label.size().x + 10.0 + hint.size().x;
    let (rect, r) = ui.allocate_exact_size(egui::vec2((content + 28.0).max(96.0), 26.0), Sense::click());
    let fill = [PRIMARY(), shade(PRIMARY(), 0.10), shade(PRIMARY(), 0.20)][state_of(&r)];
    let p = ui.painter();
    p.rect_filled(rect, R_MD, fill);
    let (cy, mut x) = (rect.center().y, rect.center().x - content / 2.0);
    p.add(egui::Shape::convex_polygon(vec![egui::pos2(x, cy - TRI * 0.62), egui::pos2(x + TRI, cy), egui::pos2(x, cy + TRI * 0.62)], ON_PRIMARY(), Stroke::NONE));
    x += TRI + 8.0;
    let centered = |g: &std::sync::Arc<egui::Galley>, x: f32| egui::pos2(x, cy - g.size().y / 2.0 + optical_dy(g));
    p.galley(centered(&label, x), label.clone(), ON_PRIMARY());
    x += label.size().x + 10.0;
    p.galley(centered(&hint, x), hint, ON_PRIMARY());
    focus_ring(ui, &r, rect, R_MD as f32);
    let enabled = ui.is_enabled();
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, text));
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub(crate) fn primary(ui: &mut egui::Ui, text: &str, fill: Color32) -> egui::Response {
    ui.add(primary_button(ui, text, fill))
}

/// For `add_enabled`; `primary` when it is always enabled.
pub(crate) fn primary_button(ui: &egui::Ui, text: impl Into<String>, fill: Color32) -> egui::Button<'static> {
    egui::Button::new(semibold(RichText::new(text).color(on_fill(fill)))).fill(state_fill(ui, fill)).stroke(Stroke::NONE)
}

/// Text color that reads on `fill`: the theme's on-accent white, or dark ink on a light fill
/// like the warning amber.
fn on_fill(fill: Color32) -> Color32 {
    let lum = 0.3 * fill.r() as f32 + 0.59 * fill.g() as f32 + 0.11 * fill.b() as f32;
    if lum > 170.0 { Color32::from_rgb(0x15, 0x13, 0x0f) } else { ON_PRIMARY() }
}

/// Every dialog gets the same frame; egui's popup frame has only 6 px of margin.
pub(crate) fn dialog(id: egui::Id) -> egui::Modal {
    egui::Modal::new(id).frame(egui::Frame::new().shadow(window_shadow()).fill(RAISED()).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_LG).inner_margin(18))
}

/// A form row's label, at the top of its cell so it lines up with the first line of a tall field.
pub(crate) fn form_label(ui: &mut egui::Ui, text: &str) {
    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.add_space(4.0);
        ui.label(RichText::new(text).color(MUTED()));
    });
}

/// The Done row of a dialog that saves as it goes.
pub(crate) fn done(ui: &mut egui::Ui) -> bool {
    dialog_actions(ui, |ui| ui.button("Done").on_hover_text("Esc").clicked())
}

/// The button row at the bottom of a dialog: primary action first, then the rest, Cancel last.
pub(crate) fn dialog_actions<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.add_space(8.0);
    ui.horizontal(add).inner
}

pub(crate) fn dialog_title(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.label(semibold(RichText::new(text).size(TXT_HEADING).color(BRIGHT())));
    ui.add_space(2.0);
}

/// The card at the top of a view holding its inputs and actions.
pub(crate) fn toolbar<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<R> {
    egui::Frame::new().fill(PANEL()).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, add)
}

/// A bordered box grouping related content: settings sections, pipeline stages, a document.
pub(crate) fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<R> {
    egui::Frame::new().fill(PANEL()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, add)
}

/// Thumbtack toggle: filled when pinned, outlined when not.
pub(crate) fn pin_button(ui: &mut egui::Ui, pinned: bool) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(BUTTON_H, BUTTON_H), Sense::click());
    let state = state_of(&r);
    let color = if pinned { PRIMARY() } else if state > 0 { BRIGHT() } else { FAINT() };
    let p = ui.painter();
    p.rect_filled(rect, R_SM, hover_fill(state));
    let c = rect.center();
    let head = egui::Rect::from_center_size(c + egui::vec2(0.0, -3.5), egui::vec2(8.0, 7.0));
    if pinned {
        p.rect_filled(head, R_SM, color);
    } else {
        p.rect_stroke(head, R_SM, Stroke::new(1.4, color), egui::StrokeKind::Inside);
    }
    p.line_segment([c + egui::vec2(-6.0, 1.0), c + egui::vec2(6.0, 1.0)], Stroke::new(1.6, color));
    p.line_segment([c + egui::vec2(0.0, 1.0), c + egui::vec2(0.0, 7.5)], Stroke::new(1.4, color));
    focus_ring(ui, &r, rect, R_SM as f32);
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Six dots marking a drag handle; painted because the default fonts lack the braille glyph.
pub(crate) fn grip(ui: &mut egui::Ui) {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(14.0, 26.0), Sense::hover());
    let color = if r.hovered() { MUTED() } else { FAINT() };
    for dx in [-2.5, 2.5] {
        for dy in [-5.0, 0.0, 5.0] {
            ui.painter().circle_filled(rect.center() + egui::vec2(dx, dy), 1.3, color);
        }
    }
}

/// Right-aligned end of a wrapping row: moves to its own line when the room left would make it
/// overlap the items before it. Its width is remembered from the previous frame.
pub(crate) fn right_group(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let id = ui.scope_id().with("right-group-w");
    let need = ui.data(|d| d.get_temp::<f32>(id)).unwrap_or(160.0);
    if ui.available_size_before_wrap().x < need + 8.0 {
        ui.end_row();
    }
    let r = ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), add);
    ui.data_mut(|d| d.insert_temp(id, r.response.rect.width()));
}

/// A quiet "more actions" menu button. The dots are painted: the ellipsis glyph comes from a
/// fallback font and sits above the middle of the button.
pub(crate) fn dots_menu(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    icon_menu(ui, paint_dots, false, content)
}

/// A quiet menu button showing a painted icon, muted at rest and bright on hover or press.
/// A sticky menu stays open while you click or type inside it, for menus with text boxes.
pub(crate) fn icon_menu(ui: &mut egui::Ui, paint: fn(&egui::Painter, egui::Pos2, Color32), sticky: bool, content: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    use egui::containers::menu::{MenuButton, MenuConfig};
    ui.scope(|ui| {
        quiet_look(ui);
        let mut b = MenuButton::from_button(egui::Button::new("").min_size(egui::vec2(28.0, BUTTON_H)));
        if sticky {
            b = b.config(MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside));
        }
        let (r, _) = b.ui(ui, |ui| {
            menu_look(ui);
            content(ui)
        });
        let color = if r.hovered() || r.is_pointer_button_down_on() { BRIGHT() } else { MUTED() };
        paint(ui.painter(), r.rect.center(), color);
        r
    })
    .inner
}

pub(crate) fn paint_dots(p: &egui::Painter, c: egui::Pos2, color: Color32) {
    for dx in [-5.0, 0.0, 5.0] {
        p.circle_filled(c + egui::vec2(dx, 0.0), 1.5, color);
    }
}

pub(crate) fn paint_clock(p: &egui::Painter, c: egui::Pos2, color: Color32) {
    let s = Stroke::new(1.5, color);
    p.circle_stroke(c, 6.5, s);
    p.line_segment([c, c + egui::vec2(0.0, -4.0)], s);
    p.line_segment([c, c + egui::vec2(3.0, 0.0)], s);
}

pub(crate) fn paint_book(p: &egui::Painter, c: egui::Pos2, color: Color32) {
    let r = egui::Rect::from_center_size(c, egui::vec2(10.0, 13.0));
    p.rect_stroke(r, 1.5, Stroke::new(1.5, color), egui::StrokeKind::Middle);
    p.rect_filled(egui::Rect::from_min_size(egui::pos2(c.x - 2.0, r.top()), egui::vec2(4.0, 5.5)), 0.0, color);
}

/// A read-only figure in mono on a panel fill, like a collection's size.
pub(crate) fn stat_chip(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let g = ui.painter().layout_job(egui::text::LayoutJob::single_section(text.to_string(), mono_fmt(TXT_CAPTION, MUTED(), 400.0)));
    let (rect, r) = ui.allocate_exact_size(egui::vec2(g.size().x + 16.0, CHIP_H), Sense::hover());
    ui.painter().rect_filled(rect, R_SM, PANEL());
    let dy = optical_dy(&g);
    ui.painter().galley(rect.center() - g.size() / 2.0 + egui::vec2(0.0, dy), g, MUTED());
    r
}

/// The amber lightning bolt marking an indexed field, with `left` the middle of its left edge.
/// Two triangles, since a filled path must be convex.
pub(crate) fn paint_bolt(p: &egui::Painter, left: egui::Pos2) {
    let (s, color) = (BOLT_H / 12.0, WARNING());
    let min = left - egui::vec2(0.0, BOLT_H / 2.0);
    let pt = |x: f32, y: f32| min + egui::vec2(x * s, y * s);
    p.add(egui::Shape::convex_polygon(vec![pt(5.5, 0.0), pt(0.5, 7.0), pt(4.5, 7.0)], color, Stroke::NONE));
    p.add(egui::Shape::convex_polygon(vec![pt(3.5, 5.0), pt(7.5, 5.0), pt(2.5, 12.0)], color, Stroke::NONE));
}

/// A short vertical rule between groups of toolbar buttons, with `pad` on each side.
pub(crate) fn vsep(ui: &mut egui::Ui, pad: f32) {
    ui.add_space(pad);
    let (r, _) = ui.allocate_exact_size(egui::vec2(1.0, BUTTON_H * 0.75), Sense::hover());
    ui.painter().vline(r.center().x, r.y_range(), Stroke::new(1.0, LINE()));
    ui.add_space(pad);
}

/// A small painted cross that removes something, with a round hover fill.
pub(crate) fn close_x(ui: &mut egui::Ui) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(CLOSE_W, CLOSE_W), Sense::click());
    let state = state_of(&r);
    ui.painter().circle_filled(rect.center(), CLOSE_W / 2.0 - 1.0, hover_fill(state));
    let color = if state > 0 { BRIGHT() } else { FAINT() };
    let (c, d) = (rect.center(), 3.0);
    for (a, b) in [(egui::vec2(-d, -d), egui::vec2(d, d)), (egui::vec2(-d, d), egui::vec2(d, -d))] {
        ui.painter().line_segment([c + a, c + b], Stroke::new(1.3, color));
    }
    focus_ring(ui, &r, rect, CLOSE_W / 2.0);
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// An outlined mono pill that adds something, like "+ limit": `paint_chip`'s off look with a
/// painted plus in front, since the "+" glyph sits off center.
pub(crate) fn add_chip(ui: &mut egui::Ui, name: &str) -> egui::Response {
    const PLUS: f32 = 7.0;
    const GAP: f32 = 6.0;
    let g = ui.painter().layout_job(egui::text::LayoutJob::single_section(name.to_string(), mono_fmt(TXT_CAPTION, Color32::PLACEHOLDER, 400.0)));
    let pad = 9.0;
    let (rect, r) = ui.allocate_exact_size(egui::vec2(pad + PLUS + GAP + g.size().x + pad, BUTTON_H), Sense::click());
    let state = state_of(&r);
    let color = if state > 0 { BRIGHT() } else { MUTED() };
    let p = ui.painter();
    p.rect(rect, rect.height() / 2.0, hover_fill(state), Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
    let c = egui::pos2(rect.left() + pad + PLUS / 2.0, rect.center().y);
    p.line_segment([c - egui::vec2(PLUS / 2.0, 0.0), c + egui::vec2(PLUS / 2.0, 0.0)], Stroke::new(1.2, color));
    p.line_segment([c - egui::vec2(0.0, PLUS / 2.0), c + egui::vec2(0.0, PLUS / 2.0)], Stroke::new(1.2, color));
    let dy = optical_dy(&g);
    p.galley(egui::pos2(rect.left() + pad + PLUS + GAP, rect.center().y - g.size().y / 2.0 + dy), g, color);
    focus_ring(ui, &r, rect, rect.height() / 2.0);
    let enabled = ui.is_enabled();
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, format!("Add {name}")));
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A quiet glyph button large enough to hit comfortably.
pub(crate) fn icon_button(ui: &mut egui::Ui, glyph: &str, enabled: bool) -> egui::Response {
    ui.scope(|ui| {
        quiet_look(ui);
        ui.add_enabled(enabled, quiet_button(RichText::new(glyph).size(TXT_TITLE)).min_size(egui::vec2(28.0, BUTTON_H)))
    })
    .inner
}

/// A painted checkbox for table rows; the egui widget would take part in the row layout.
pub(crate) fn check_box(ui: &egui::Ui, center: egui::Pos2, id: egui::Id, on: bool, partial: bool) -> egui::Response {
    let rect = egui::Rect::from_center_size(center, egui::vec2(14.0, 14.0));
    let r = ui.interact(rect.expand(3.0), id, Sense::click());
    let filled = on || partial;
    let state = state_of(&r);
    let edge = if filled { PRIMARY() } else if state > 0 { MUTED() } else { BORDER() };
    let fill = if filled { [PRIMARY(), shade(PRIMARY(), 0.10), shade(PRIMARY(), 0.20)][state] } else { [BG(), BG(), PRESSED()][state] };
    let p = ui.painter();
    p.rect(rect, R_SM, fill, Stroke::new(1.0, edge), egui::StrokeKind::Inside);
    let s = Stroke::new(1.6, ON_PRIMARY());
    if on {
        p.line(vec![center + egui::vec2(-3.5, 0.0), center + egui::vec2(-1.0, 2.5), center + egui::vec2(3.5, -2.5)], s);
    } else if partial {
        p.hline(center.x - 3.5..=center.x + 3.5, center.y, s);
    }
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// egui's spinner asks for a new frame on every refresh. Behind other windows a static glyph
/// lets the app go idle instead of redrawing at full rate for something nobody is watching.
pub(crate) fn spin(ui: &mut egui::Ui) {
    if ui.input(|i| i.viewport().focused.unwrap_or(true)) {
        ui.spinner();
    } else {
        ui.label(RichText::new("…").color(FAINT()));
    }
}

/// `spin` painted into a rect, for rows painted by hand.
pub(crate) fn spin_at(ui: &egui::Ui, rect: egui::Rect) {
    if ui.input(|i| i.viewport().focused.unwrap_or(true)) {
        egui::Spinner::new().color(FAINT()).paint_at(ui, rect);
    } else {
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "…", FontId::proportional(TXT_SMALL), FAINT());
    }
}

/// One clickable sidebar row painted by hand, so it reads as a list rather than a form.
pub(crate) fn side_row(ui: &mut egui::Ui, height: f32, indent: f32, active: bool, paint: impl FnOnce(&egui::Painter, egui::Rect)) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::click());
    let fill = match (active, state_of(&resp)) {
        (true, 0) => SEL(),
        (true, _) => SEL_HOVER(),
        (false, 2) => PRESSED(),
        (false, 1) => HOVER(),
        _ => Color32::TRANSPARENT,
    };
    ui.painter().rect_filled(rect, R_SM, fill);
    focus_ring(ui, &resp, rect, R_SM as f32);
    let inner = egui::Rect::from_min_max(rect.min + egui::vec2(8.0 + indent, 0.0), rect.max - egui::vec2(8.0, 0.0));
    paint(ui.painter(), inner);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A small table: a frame with a header line and one column divider. Painted as shapes since a
/// glyph from a fallback font sits off center.
pub(crate) fn table_glyph(p: &egui::Painter, c: egui::Pos2, color: Color32) {
    let r = egui::Rect::from_center_size(c, egui::vec2(11.0, 9.0));
    let s = Stroke::new(1.0, color);
    p.rect_stroke(r, 1, s, egui::StrokeKind::Inside);
    p.hline(r.x_range(), r.top() + 3.0, s);
    p.vline(r.left() + 4.5, r.y_range(), s);
}

pub(crate) fn chevron(p: &egui::Painter, c: egui::Pos2, open: bool, color: Color32) {
    let pts = if open {
        vec![c + egui::vec2(-4.0, -2.0), c + egui::vec2(4.0, -2.0), c + egui::vec2(0.0, 3.0)]
    } else {
        vec![c + egui::vec2(-2.0, -4.0), c + egui::vec2(3.0, 0.0), c + egui::vec2(-2.0, 4.0)]
    };
    p.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// The first, previous, "n / total", next and last control shared by every paged result, with
/// a page number that can be typed over (Enter or leaving the box goes there, Esc cancels).
/// Returns the page to go to.
pub(crate) fn pager(ui: &mut egui::Ui, page: usize, pages: usize) -> Option<usize> {
    let mut go = None;
    let last = pages.saturating_sub(1);
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        quiet_look(ui);
        let nav = |t: &str| quiet_button(RichText::new(t).font(FontId::monospace(TXT_BODY))).min_size(egui::vec2(22.0, BUTTON_H));
        if ui.add_enabled(page > 0, nav("«")).on_hover_text("First page").clicked() {
            go = Some(0);
        }
        if ui.add_enabled(page > 0, nav("‹")).on_hover_text("Previous page").clicked() {
            go = Some(page - 1);
        }
        // The typed text lives in temp memory only while the box is being edited.
        let id = ui.scope_id().with("pager-input");
        let mut text = ui.data(|d| d.get_temp::<String>(id)).unwrap_or_else(|| (page + 1).to_string());
        let digits = pages.max(1).to_string().len() as f32;
        let r = ui
            .add(egui::TextEdit::singleline(&mut text).font(egui::TextStyle::Monospace).desired_width(digits * 8.0 + 8.0).align(egui::Align2::CENTER_CENTER).frame(egui::Frame::new()))
            .on_hover_text("Type a page number, then Enter");
        // The box has no frame at rest so it reads as part of "1 / 26"; it shows one on hover
        // and focus so it is clearly editable.
        if r.has_focus() {
            ui.painter().rect_stroke(r.rect.expand(1.0), R_SM, Stroke::new(1.0, PRIMARY()), egui::StrokeKind::Outside);
        } else if r.hovered() {
            ui.painter().rect_stroke(r.rect.expand(1.0), R_SM, Stroke::new(1.0, BORDER()), egui::StrokeKind::Outside);
        }
        if r.lost_focus() {
            if !ui.input(|i| i.key_pressed(egui::Key::Escape))
                && let Ok(n) = text.trim().parse::<usize>()
            {
                go = Some(n.clamp(1, pages.max(1)) - 1);
            }
            ui.data_mut(|d| d.remove::<String>(id));
        } else if r.has_focus() {
            ui.data_mut(|d| d.insert_temp(id, text));
        }
        ui.label(RichText::new(format!("/ {pages}")).font(FontId::monospace(TXT_BODY)).color(MUTED()));
        if ui.add_enabled(page < last, nav("›")).on_hover_text("Next page").clicked() {
            go = Some(page + 1);
        }
        if ui.add_enabled(page < last, nav("»")).on_hover_text("Last page").clicked() {
            go = Some(last);
        }
    });
    go.filter(|&p| p != page)
}

/// A painted check mark.
pub(crate) fn paint_check(p: &egui::Painter, c: egui::Pos2, color: Color32) {
    let pts = [c + egui::vec2(-4.0, 0.0), c + egui::vec2(-1.5, 3.0), c + egui::vec2(4.0, -3.5)];
    p.line_segment([pts[0], pts[1]], Stroke::new(1.6, color));
    p.line_segment([pts[1], pts[2]], Stroke::new(1.6, color));
}

/// One row of a menu: a check column, the label in `color`, a faint note on the right.
pub(crate) fn menu_row(ui: &mut egui::Ui, label: &str, color: Color32, checked: bool, note: &str) -> egui::Response {
    menu_row_in(ui, label, color, checked, note, "", false)
}

/// A menu row whose label shows the typed part `hl` in the primary color, and that can be held
/// highlighted (`active`) by the keyboard selection of a searchable list.
pub(crate) fn menu_row_in(ui: &mut egui::Ui, label: &str, color: Color32, checked: bool, note: &str, hl: &str, active: bool) -> egui::Response {
    let w = ui.available_width().max(190.0);
    let (rect, r) = ui.allocate_exact_size(egui::vec2(w, 24.0), Sense::click());
    let state = state_of(&r);
    if !ui.is_rect_visible(rect) {
        return r;
    }
    if state > 0 || active {
        ui.painter().rect_filled(rect, R_SM, if state == 2 { menu_pressed() } else { MENU_HOVER() });
    }
    if checked {
        paint_check(ui.painter(), egui::pos2(rect.left() + 12.0, rect.center().y), TEXT());
    }
    // The note is laid out first so a long label is cut with an ellipsis before it instead of running under it.
    let mut note_left = rect.right() - 8.0;
    if !note.is_empty() {
        let n = ui.painter().layout_no_wrap(note.to_string(), FontId::monospace(TXT_CAPTION), FAINT());
        let dy = optical_dy(&n);
        note_left -= n.size().x;
        ui.painter().galley(egui::pos2(note_left, rect.center().y - n.size().y / 2.0 + dy), n, FAINT());
    }
    let mut job = egui::text::LayoutJob::default();
    let fmt = |c: Color32| egui::TextFormat::simple(FontId::monospace(TXT_SMALL), c);
    match find_ci(label, hl) {
        Some((i, j)) => {
            job.append(&label[..i], 0.0, fmt(color));
            job.append(&label[i..j], 0.0, fmt(PRIMARY()));
            job.append(&label[j..], 0.0, fmt(color));
        }
        None => job.append(label, 0.0, fmt(color)),
    }
    job.wrap = egui::text::TextWrapping::truncate_at_width((note_left - 12.0 - (rect.left() + 26.0)).max(20.0));
    let g = ui.painter().layout_job(job);
    let dy = optical_dy(&g);
    ui.painter().galley(egui::pos2(rect.left() + 26.0, rect.center().y - g.size().y / 2.0 + dy), g, color);
    focus_ring(ui, &r, rect, R_SM as f32);
    let label = label.to_string();
    let enabled = ui.is_enabled();
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::MenuItem, enabled, checked, label.clone()));
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Byte range of the first case-insensitive (ASCII) occurrence of `q` in `name`. ASCII lowering keeps
/// byte offsets, so the range indexes `name` itself.
pub(crate) fn find_ci(name: &str, q: &str) -> Option<(usize, usize)> {
    if q.is_empty() {
        return None;
    }
    let i = name.to_ascii_lowercase().find(&q.to_ascii_lowercase())?;
    Some((i, i + q.len()))
}

/// A clause popover. egui keeps one memory popup open at a time, so opening the column or direction
/// menu inside would close it; it keeps its own open flag instead and closes on Esc or a click on the page.
/// `open_first` is the state before the first frame stores one.
pub(crate) fn popover<R>(r: &egui::Response, w: f32, open_first: bool, add: impl FnOnce(&mut egui::Ui) -> R) -> Option<R> {
    let ctx = r.ctx.clone();
    let id = r.id.with("clause-popover");
    let mut open = ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(open_first);
    if r.clicked() {
        open = !open;
    }
    let out = if open {
        egui::Popup::new(id, ctx.clone(), r, r.layer_id).kind(egui::PopupKind::Menu).layout(egui::Layout::top_down_justified(egui::Align::Min)).style(egui::containers::menu::menu_style).gap(0.0).open_bool(&mut open).close_behavior(egui::PopupCloseBehavior::IgnoreClicks).width(w).show(add)
    } else {
        None
    };
    // Its own menus are popups too, so only a click on a page layer counts as outside.
    if open && !r.clicked() {
        let page_click = ctx.input(|i| i.pointer.any_click().then(|| i.pointer.interact_pos()).flatten()).is_some_and(|p| !r.rect.contains(p) && ctx.layer_id_at(p).is_none_or(|l| l.order != egui::Order::Foreground));
        if page_click || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            open = false;
        }
    }
    ctx.data_mut(|d| d.insert_temp(id, open));
    out.map(|o| o.inner)
}

/// Text cut to its cell with an ellipsis, centered on its ink, `pad` clear on each side. Returns where it ended up.
pub(crate) fn place(ui: &egui::Ui, cell: egui::Rect, s: &str, font: FontId, color: Color32, right: bool, pad: f32) -> egui::Rect {
    let area = cell.shrink2(egui::vec2(pad, 0.0));
    let mut job = egui::text::LayoutJob::single_section(s.to_string(), egui::TextFormat { font_id: font, color, ..Default::default() });
    job.wrap = egui::text::TextWrapping { max_width: area.width().max(1.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    let g = ui.painter().layout_job(job);
    let x = if right { area.right() - g.size().x } else { area.left() };
    let pos = egui::pos2(x, cell.center().y - g.size().y / 2.0 + optical_dy(&g));
    let rect = egui::Rect::from_min_size(pos, g.size());
    ui.painter().galley(pos, g, color);
    rect
}
