//! Shared widgets: buttons, dialogs, cards, chips and painted controls.

use super::theme::*;
use eframe::egui::{self, Color32, FontId, RichText, Sense, Stroke};

/// A menu that stays open while you type or click inside it, closing on a click outside or Esc.
/// Plain egui menus close on any click inside, which made their text boxes unusable.
pub(crate) fn sticky_menu(ui: &mut egui::Ui, title: String, content: impl FnOnce(&mut egui::Ui)) {
    use egui::containers::menu::{MenuButton, MenuConfig};
    MenuButton::new(title).config(MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)).ui(ui, |ui| {
        menu_look(ui);
        content(ui)
    });
}

/// Menu content that scrolls once it is taller than the window. egui keeps a popup inside the
/// window, so a taller one is cut off at the edge with its last items out of reach.
pub(crate) fn menu_scroll<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let max_h = ui.ctx().content_rect().height() - 24.0;
    egui::ScrollArea::vertical().max_height(max_h).show(ui, add).inner
}

/// Tighter corners for menu and dropdown items. egui builds menu styles from the app style, so
/// each menu calls this first; the button radius looked like a pill on a full-width row.
pub(crate) fn menu_look(ui: &mut egui::Ui) {
    let w = &mut ui.visuals_mut().widgets;
    for s in [&mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        s.corner_radius = egui::CornerRadius::same(R_SM);
    }
}

/// The one style for uppercase labels over a group: sections, lists, menus and column headers.
pub(crate) fn caption_text(text: impl Into<String>) -> RichText {
    RichText::new(text).size(TXT_CAPTION).strong().color(MUTED())
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
            ui.label(RichText::new(title).size(TXT_TITLE).color(BRIGHT()));
            ui.add(egui::Label::new(RichText::new(desc).size(TXT_SMALL).color(MUTED())).wrap());
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), control);
    });
}

/// "−  n  +" for a small whole number (the control slot is right-to-left, so added in reverse).
pub(crate) fn stepper(ui: &mut egui::Ui, value: &mut usize, min: usize, max: usize) {
    egui::Frame::new().fill(pal().well).corner_radius(R_MD).inner_margin(3).show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        if ui.add_enabled(*value < max, egui::Button::new(RichText::new("+").size(TXT_TITLE)).corner_radius(R_SM)).clicked() {
            *value += 1;
        }
        ui.add_sized([34.0, 22.0], egui::Label::new(RichText::new(value.to_string()).size(TXT_TITLE).color(BRIGHT())));
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
        bg = shade(bg, if r.is_pointer_button_down_on() { 0.24 } else { 0.12 });
    }
    ui.painter().rect_filled(rect, rect.height() / 2.0, bg);
    let x = egui::lerp(rect.left() + 11.0..=rect.right() - 11.0, t);
    // The knob grows a little while pressed, like a macOS switch.
    let knob = if enabled && r.is_pointer_button_down_on() { 9.0 } else { 8.0 };
    ui.painter().circle_filled(egui::pos2(x, rect.center().y), knob, if enabled { Color32::from_gray(0xfa) } else { MUTED() });
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A row of joined buttons where exactly one is chosen.
pub(crate) fn segmented<T: PartialEq + Copy>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)]) {
    // Painted at a fixed height: a Frame grew with the row, leaving the track taller than its options.
    const PAD: f32 = 3.0;
    const GAP: f32 = 2.0;
    let pad_x = ui.spacing().button_padding.x;
    let galleys: Vec<_> = options.iter().map(|(_, l)| ui.painter().layout_no_wrap(l.to_string(), FontId::proportional(TXT_BODY), Color32::PLACEHOLDER)).collect();
    let widths: Vec<f32> = galleys.iter().map(|g| g.size().x + pad_x * 2.0).collect();
    let total = widths.iter().sum::<f32>() + GAP * (options.len().saturating_sub(1)) as f32 + PAD * 2.0;
    let base = ui.next_auto_id();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(total, BUTTON_H), Sense::hover());
    ui.painter().rect_filled(rect, R_MD, pal().well);
    let mut x = rect.left() + PAD;
    for (i, (((v, label), g), w)) in options.iter().zip(galleys).zip(widths).enumerate() {
        let seg = egui::Rect::from_min_size(egui::pos2(x, rect.top() + PAD), egui::vec2(w, BUTTON_H - PAD * 2.0));
        x += w + GAP;
        let r = ui.interact(seg, base.with(i), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
        let on = *value == *v;
        let state = if r.is_pointer_button_down_on() { 2 } else if r.hovered() { 1 } else { 0 };
        let fill = if on { [PRIMARY(), shade(PRIMARY(), 0.12), shade(PRIMARY(), 0.24)][state] } else { [Color32::TRANSPARENT, tint(TEXT(), 34), tint(TEXT(), 46)][state] };
        ui.painter().rect_filled(seg, R_SM, fill);
        let dy = optical_dy(&g);
        ui.painter().galley(seg.center() - g.size() / 2.0 + egui::vec2(0.0, dy), g, if on { BG() } else { TEXT() });
        if r.has_focus() {
            ui.painter().rect_stroke(seg.expand(1.0), R_SM, ui.visuals().selection.stroke, egui::StrokeKind::Outside);
        }
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
        (fg, tint(fg, 150), tint(fg, [34, 56, 78][state]))
    } else {
        (if state > 0 { TEXT() } else { MUTED() }, BORDER(), [Color32::TRANSPARENT, tint(TEXT(), 34), tint(TEXT(), 46)][state])
    };
    p.rect(rect, rect.height() / 2.0, fill, Stroke::new(1.0, line), egui::StrokeKind::Inside);
    let dy = optical_dy(&text);
    p.galley(rect.center() - text.size() / 2.0 + egui::vec2(0.0, dy), text, text_c);
}

/// A rounded label in `fg`, or muted with a plain border when off, as in one-of choices like connection tags.
pub(crate) fn chip(ui: &mut egui::Ui, text: &str, fg: Color32, on: bool) -> egui::Response {
    let g = ui.painter().layout_no_wrap(text.to_string(), FontId::proportional(TXT_SMALL), Color32::PLACEHOLDER);
    let (rect, r) = ui.allocate_exact_size(egui::vec2(g.size().x + 20.0, CHIP_H), Sense::click());
    let state = if r.is_pointer_button_down_on() { 2 } else if r.hovered() { 1 } else { 0 };
    paint_chip(ui.painter(), rect, g, fg, on, state);
    if r.has_focus() {
        ui.painter().rect_stroke(rect.expand(2.0), rect.height() / 2.0 + 2.0, ui.visuals().selection.stroke, egui::StrokeKind::Outside);
    }
    let enabled = ui.is_enabled();
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, enabled, on, text));
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

// Buttons come in four kinds, all BUTTON_H tall: `primary` for the main action, `ui.button` for the
// rest, `quiet_button` for list rows and toolbar extras (framed only on hover), and `danger` for
// destructive actions.

/// Framed only while hovered or pressed: rows in lists and menus, and low-key toolbar actions.
pub(crate) fn quiet_button<'a>(text: impl Into<egui::WidgetText>) -> egui::Button<'a> {
    egui::Button::new(text).frame_when_inactive(false)
}

/// A destructive action: red text on an ordinary button, so it keeps the same height and hover.
pub(crate) fn danger_button<'a>(text: impl Into<String>) -> egui::Button<'a> {
    egui::Button::new(RichText::new(text).color(ALERT()))
}

pub(crate) fn danger(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    ui.add(danger_button(text))
}

pub(crate) fn primary(ui: &mut egui::Ui, text: &str, fill: Color32) -> egui::Response {
    ui.add(primary_button(ui, text, fill))
}

/// For `add_enabled`; `primary` when it is always enabled.
pub(crate) fn primary_button(ui: &egui::Ui, text: impl Into<String>, fill: Color32) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text).strong().color(BG())).fill(state_fill(ui, fill))
}

/// Every dialog gets the same frame; egui's popup frame has only 6 px of margin.
pub(crate) fn dialog(id: egui::Id) -> egui::Modal {
    let shadow = egui::Shadow { offset: [0, 6], blur: 20, spread: 0, color: Color32::from_black_alpha(50) };
    egui::Modal::new(id).frame(egui::Frame::new().shadow(shadow).fill(RAISED()).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_LG).inner_margin(18))
}

/// A form row's label, at the top of its cell so it lines up with the first line of a tall field.
pub(crate) fn form_label(ui: &mut egui::Ui, text: &str) {
    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.add_space(4.0);
        ui.label(text);
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
    ui.label(RichText::new(text).strong().size(TXT_HEADING).color(BRIGHT()));
    ui.add_space(2.0);
}

/// The strip at the top of a view holding its inputs and actions.
pub(crate) fn toolbar<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<R> {
    egui::Frame::new().fill(PANEL()).corner_radius(R_MD).inner_margin(10).show(ui, add)
}

/// A bordered box grouping related content: settings sections, pipeline stages, a document.
pub(crate) fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<R> {
    egui::Frame::new().fill(RAISED()).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, add)
}

/// Thumbtack toggle: filled when pinned, outlined when not.
pub(crate) fn pin_button(ui: &mut egui::Ui, pinned: bool) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(BUTTON_H, BUTTON_H), Sense::click());
    let color = if pinned { PRIMARY() } else if r.hovered() { MUTED() } else { FAINT() };
    let p = ui.painter();
    if r.hovered() {
        p.rect_filled(rect, R_SM, HOVER());
    }
    let c = rect.center();
    let head = egui::Rect::from_center_size(c + egui::vec2(0.0, -3.5), egui::vec2(8.0, 7.0));
    if pinned {
        p.rect_filled(head, R_SM, color);
    } else {
        p.rect_stroke(head, R_SM, Stroke::new(1.4, color), egui::StrokeKind::Inside);
    }
    p.line_segment([c + egui::vec2(-6.0, 1.0), c + egui::vec2(6.0, 1.0)], Stroke::new(1.6, color));
    p.line_segment([c + egui::vec2(0.0, 1.0), c + egui::vec2(0.0, 7.5)], Stroke::new(1.4, color));
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

/// A glyph button large enough to hit comfortably.
pub(crate) fn icon_button(ui: &mut egui::Ui, glyph: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(enabled, egui::Button::new(RichText::new(glyph).size(TXT_TITLE)).min_size(egui::vec2(28.0, BUTTON_H)))
}

/// A painted checkbox for table rows; the egui widget would take part in the row layout.
pub(crate) fn check_box(ui: &egui::Ui, center: egui::Pos2, id: egui::Id, on: bool, partial: bool) -> egui::Response {
    let rect = egui::Rect::from_center_size(center, egui::vec2(14.0, 14.0));
    let r = ui.interact(rect.expand(3.0), id, Sense::click());
    let filled = on || partial;
    let edge = if filled { PRIMARY() } else if r.hovered() { MUTED() } else { FAINT() };
    let p = ui.painter();
    p.rect(rect, R_SM, if filled { PRIMARY() } else { Color32::TRANSPARENT }, Stroke::new(1.0, edge), egui::StrokeKind::Inside);
    let s = Stroke::new(1.6, Color32::WHITE);
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

/// One clickable sidebar row painted by hand, so it reads as a list rather than a form.
pub(crate) fn side_row(ui: &mut egui::Ui, height: f32, indent: f32, active: bool, paint: impl FnOnce(&egui::Painter, egui::Rect)) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::click());
    let fill = if resp.is_pointer_button_down_on() {
        tint(PRIMARY(), 45)
    } else if active {
        tint(PRIMARY(), 30)
    } else if resp.hovered() {
        HOVER()
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, R_MD, fill);
    let inner = egui::Rect::from_min_max(rect.min + egui::vec2(8.0 + indent, 0.0), rect.max - egui::vec2(8.0, 0.0));
    paint(ui.painter(), inner);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub(crate) fn chevron(p: &egui::Painter, c: egui::Pos2, open: bool, color: Color32) {
    let pts = if open {
        vec![c + egui::vec2(-4.0, -2.0), c + egui::vec2(4.0, -2.0), c + egui::vec2(0.0, 3.0)]
    } else {
        vec![c + egui::vec2(-2.0, -4.0), c + egui::vec2(3.0, 0.0), c + egui::vec2(-2.0, 4.0)]
    };
    p.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// The « ‹ page x of y › » control shared by every paged result, with first and last buttons and
/// a page number that can be typed over (Enter or leaving the box goes there, Esc cancels).
/// Returns the page to go to.
pub(crate) fn pager(ui: &mut egui::Ui, page: usize, pages: usize) -> Option<usize> {
    let mut go = None;
    let last = pages.saturating_sub(1);
    if ui.add_enabled(page > 0, egui::Button::new("«")).on_hover_text("First page").clicked() {
        go = Some(0);
    }
    if ui.add_enabled(page > 0, egui::Button::new("‹")).clicked() {
        go = Some(page - 1);
    }
    ui.label("page");
    // The typed text lives in temp memory only while the box is being edited.
    let id = ui.scope_id().with("pager-input");
    let mut text = ui.data(|d| d.get_temp::<String>(id)).unwrap_or_else(|| (page + 1).to_string());
    let digits = pages.max(1).to_string().len() as f32;
    let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(digits * 8.0 + 8.0).align(egui::Align2::CENTER_CENTER)).on_hover_text("Type a page number, then Enter");
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
    ui.label(format!("of {pages}"));
    if ui.add_enabled(page < last, egui::Button::new("›")).clicked() {
        go = Some(page + 1);
    }
    if ui.add_enabled(page < last, egui::Button::new("»")).on_hover_text("Last page").clicked() {
        go = Some(last);
    }
    go.filter(|&p| p != page)
}
