//! The dialog shell: a raised header band, a recessed body that scrolls, and a raised footer with the actions, plus
//! the cards, banners and choices that go in the body. Bodies call `dialog_head`, `dialog_body` and `dialog_foot` in
//! order rather than handing closures to one function, so the body and the footer can both borrow the app.

use super::glyphs::Glyph;
use super::theme::*;
use super::widgets::*;
use eframe::egui::{self, Color32, RichText, Sense, Stroke};

/// A dialog without padding on its frame: the bands lay out their own edges.
pub(crate) fn banded(id: egui::Id) -> egui::Modal {
    egui::Modal::new(id).frame(dialog_frame())
}

/// `banded` raised above everything else every frame, for windows opened from another window.
pub(crate) fn top_banded(ctx: &egui::Context, id: egui::Id) -> egui::Modal {
    ctx.move_to_top(egui::LayerId::new(egui::Order::Foreground, id));
    banded(id)
}

/// What the header band shows.
pub(crate) struct Head<'a> {
    title: String,
    sub: String,
    glyph: Option<Glyph>,
    tone: Option<Color32>,
    tabs: &'a [&'a str],
    tab: usize,
    dim: &'a [bool],
    closable: bool,
}

impl<'a> Head<'a> {
    pub(crate) fn new(title: impl Into<String>) -> Self {
        Self { title: title.into(), sub: String::new(), glyph: None, tone: None, tabs: &[], tab: 0, dim: &[], closable: true }
    }
    /// The context line under the title: the connection, the engine, a count.
    pub(crate) fn sub(mut self, sub: impl Into<String>) -> Self {
        self.sub = sub.into();
        self
    }
    pub(crate) fn glyph(mut self, g: Glyph) -> Self {
        self.glyph = Some(g);
        self
    }
    /// The tile's color, the accent unless given; destructive dialogs pass `ALERT()`.
    pub(crate) fn tone(mut self, c: Color32) -> Self {
        self.tone = Some(c);
        self
    }
    /// Underline tabs on the band's bottom edge, `active` chosen.
    pub(crate) fn tabs(mut self, tabs: &'a [&'a str], active: usize) -> Self {
        self.tabs = tabs;
        self.tab = active;
        self
    }
    /// No close button, for a window that cannot close yet (a tool still running).
    pub(crate) fn no_close(mut self) -> Self {
        self.closable = false;
        self
    }
    /// Tabs whose settings do not apply right now, drawn faint but still clickable.
    pub(crate) fn dim(mut self, dim: &'a [bool]) -> Self {
        self.dim = dim;
        self
    }
}

/// The header band; returns the tab clicked this frame. The close button closes the dialog like Esc.
pub(crate) fn dialog_head(ui: &mut egui::Ui, h: &Head) -> Option<usize> {
    dialog_head_with(ui, h, |_| {})
}

/// `dialog_head` with extra buttons left of the close button, added right to left.
pub(crate) fn dialog_head_with(ui: &mut egui::Ui, h: &Head, right: impl FnOnce(&mut egui::Ui)) -> Option<usize> {
    // The bands touch: spacing between them would show the frame's fill as a stripe.
    ui.spacing_mut().item_spacing.y = 0.0;
    let mut picked = None;
    let pad = DIALOG_PAD as i8;
    let bottom = if h.tabs.is_empty() { 14 } else { 0 };
    egui::Frame::new().inner_margin(egui::Margin { left: pad, right: pad - 4, top: 16, bottom }).show(ui, |ui| {
        ui.set_width(ui.available_width());
        band_spacing(ui);
        ui.horizontal(|ui| {
            if let Some(g) = h.glyph {
                tile(ui, g, h.tone.unwrap_or_else(PRIMARY));
                ui.add_space(4.0);
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.label(semibold(RichText::new(&h.title).size(TXT_SUBHEAD).color(BRIGHT())));
                if !h.sub.is_empty() {
                    ui.label(RichText::new(&h.sub).size(TXT_SMALL).color(MUTED()));
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if h.closable {
                    let r = close_x(ui);
                    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Close dialog"));
                    if r.on_hover_text("Close (Esc)").clicked() {
                        ui.close();
                    }
                }
                right(ui);
            });
        });
        if !h.tabs.is_empty() {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 20.0;
                for (i, t) in h.tabs.iter().enumerate() {
                    if view_tab_dim(ui, t, i == h.tab, h.dim.get(i).copied().unwrap_or(false)).clicked() {
                        picked = Some(i);
                    }
                }
            });
        }
    });
    rule(ui, -0.5);
    picked
}

/// The tinted square with the dialog's glyph.
fn tile(ui: &mut egui::Ui, g: Glyph, tone: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(HEAD_TILE, HEAD_TILE), Sense::hover());
    ui.painter().rect(rect, R_BAR, tint(tone, 36), Stroke::new(1.0, tint(tone, 72)), egui::StrokeKind::Inside);
    g(ui.painter(), rect.center(), tone);
    ui.add_space(8.0);
}

/// The global item spacing back on a band's content: `dialog_head` zeroes it on the dialog root so the bands touch.
pub(crate) fn band_spacing(ui: &mut egui::Ui) {
    ui.spacing_mut().item_spacing = ui.ctx().global_style().spacing.item_spacing;
}

/// The padding of the recessed body.
const BODY_MARGIN: egui::Margin = egui::Margin { left: DIALOG_PAD as i8, right: DIALOG_PAD as i8, top: 18, bottom: 20 };

/// A hairline across the whole dialog at the cursor, `dy` from it.
fn rule(ui: &egui::Ui, dy: f32) {
    let y = ui.cursor().top() + dy;
    ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0, RAISED_LINE()));
}

/// The recessed body, scrolling when it is taller than the screen allows.
pub(crate) fn dialog_body<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let max_h = (ui.ctx().content_rect().height() - 240.0).max(160.0);
    body_frame(ui, true, |ui| egui::ScrollArea::vertical().id_salt("dialog-body").max_height(max_h).auto_shrink([false, true]).show(ui, add).inner)
}

/// The recessed body without its own scrolling, for windows that lay out panes and scroll areas themselves.
pub(crate) fn dialog_pane<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    body_frame(ui, true, add)
}

/// The recessed body with no padding, for list and detail windows whose panes reach the edges.
pub(crate) fn dialog_flush<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    body_frame(ui, false, add)
}

fn body_frame<R>(ui: &mut egui::Ui, pad: bool, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let m = if pad { BODY_MARGIN } else { egui::Margin::ZERO };
    egui::Frame::new()
        .fill(PANEL())
        .inner_margin(m)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            band_spacing(ui);
            add(ui)
        })
        .inner
}

/// The footer band. `add` runs right to left, so the primary action goes first and lands rightmost; `foot_left`
/// inside it holds what sits on the left.
pub(crate) fn dialog_foot<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    dialog_foot_free(ui, |ui| ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), add).inner)
}

/// The left side of `dialog_foot`: status text or a destructive action.
pub(crate) fn foot_left<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), add).inner
}

/// Muted status text for the footer's left side.
pub(crate) fn foot_note(ui: &mut egui::Ui, text: &str) {
    foot_left(ui, |ui| ui.add(egui::Label::new(RichText::new(text).size(TXT_SMALL).color(MUTED())).truncate()));
}

/// The frame of a card on the dialog body.
pub(crate) fn card_frame() -> egui::Frame {
    egui::Frame::new().fill(RAISED()).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_BAR)
}

/// The recessed frame of code and statement previews.
pub(crate) fn well_frame() -> egui::Frame {
    egui::Frame::new().fill(WELL()).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_BAR)
}

/// A card of the given padding at full width, with the gap that follows every card.
fn card<R>(ui: &mut egui::Ui, margin: egui::Margin, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let r = card_frame()
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner;
    ui.add_space(16.0);
    r
}

/// A glyph in a square of `size`, as an icon in a row of widgets.
pub(crate) fn glyph_icon(ui: &mut egui::Ui, g: Glyph, color: Color32, size: f32) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(size, size), Sense::hover());
    g(ui.painter(), r.center(), color);
}

/// The caps caption over a card, with an optional faint note beside it.
pub(crate) fn card_caption(ui: &mut egui::Ui, caption: &str, note: &str) {
    if caption.is_empty() {
        return;
    }
    ui.horizontal(|ui| {
        ui.add_space(2.0);
        ui.label(caption_text(caption.to_uppercase()));
        if !note.is_empty() {
            ui.add_space(4.0);
            ui.label(RichText::new(note).size(TXT_SMALL).color(FAINT()));
        }
    });
    ui.add_space(6.0);
}

/// The rows of a form card: a label and an optional hint on the left, the control on the right, a hairline between rows.
pub(crate) struct Rows<'u> {
    ui: &'u mut egui::Ui,
    n: usize,
    label_w: f32,
}

/// A captioned card of rows. An empty caption leaves the caption out.
pub(crate) fn form_card<R>(ui: &mut egui::Ui, caption: &str, note: &str, add: impl FnOnce(&mut Rows) -> R) -> R {
    card_caption(ui, caption, note);
    let r = card_frame()
        .inner_margin(egui::Margin::symmetric(14, 0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            add(&mut Rows { ui, n: 0, label_w: CARD_LABEL_W })
        })
        .inner;
    ui.add_space(16.0);
    r
}

impl Rows<'_> {
    /// The width of the label column for the rows after this call.
    pub(crate) fn label_width(&mut self, w: f32) -> &mut Self {
        self.label_w = w;
        self
    }

    fn sep(&mut self) {
        if self.n > 0 {
            rule(self.ui, 0.5);
        }
        self.n += 1;
    }

    /// A label row; `control` runs right to left, so a field given `available_width()` fills the rest.
    pub(crate) fn row<R>(&mut self, label: &str, hint: &str, control: impl FnOnce(&mut egui::Ui) -> R) -> R {
        self.sep();
        let h = if hint.is_empty() { CARD_ROW_H } else { CARD_ROW_HINT_H };
        let lw = self.label_w;
        let w = self.ui.available_width();
        self.ui
            .allocate_ui_with_layout(egui::vec2(w, h), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_min_height(h);
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.vertical(|ui| {
                    ui.set_width(lw);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    // Centered by hand: a vertical block inside a centered row still starts at the row's top.
                    let content = if hint.is_empty() { TXT_BODY * 1.3 } else { TXT_BODY * 1.3 + 2.0 + TXT_SMALL * 1.3 };
                    ui.add_space(((h - content) / 2.0).max(0.0));
                    ui.label(medium(RichText::new(label).size(TXT_BODY).color(BRIGHT())));
                    if !hint.is_empty() {
                        ui.add(egui::Label::new(RichText::new(hint).size(TXT_SMALL).color(MUTED())).wrap());
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), control).inner
            })
            .inner
    }

    /// A row with its label above a full-width control, for editors and fields that read across.
    pub(crate) fn stacked<R>(&mut self, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
        self.sep();
        self.ui.add_space(10.0);
        if !label.is_empty() {
            self.ui.label(medium(RichText::new(label).size(TXT_SMALL).color(TEXT())));
            self.ui.add_space(6.0);
        }
        let r = self.ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            add(ui)
        });
        self.ui.add_space(12.0);
        r.inner
    }

    /// A row the caller lays out freely, padded like the others.
    pub(crate) fn full<R>(&mut self, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
        self.stacked("", add)
    }
}

/// A field with its label above it and an optional hint below, straight on the body: forms of one or two fields
/// need no card.
pub(crate) fn stacked_field<R>(ui: &mut egui::Ui, label: &str, hint: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.label(medium(RichText::new(label).size(TXT_SMALL).color(TEXT())));
    ui.add_space(2.0);
    let r = add(ui);
    if !hint.is_empty() {
        ui.add_space(2.0);
        ui.add(egui::Label::new(RichText::new(hint).size(TXT_SMALL).color(MUTED())).wrap());
    }
    ui.add_space(12.0);
    r
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Notice {
    Info,
    Locked,
    Warn,
    Danger,
    Ok,
}

/// A tinted notice across the body: a glyph, a bold lead, the message and an optional action. Returns whether the
/// action was clicked.
pub(crate) fn banner(ui: &mut egui::Ui, tone: Notice, lead: &str, msg: &str, action: Option<&str>) -> bool {
    let (col, g): (Color32, Glyph) = match tone {
        Notice::Info => (PRIMARY(), super::glyphs::info),
        Notice::Locked => (WARNING(), super::glyphs::lock),
        Notice::Warn => (WARNING(), super::glyphs::warn),
        Notice::Danger => (ALERT(), super::glyphs::warn),
        Notice::Ok => (SUCCESS(), super::glyphs::check),
    };
    let mut clicked = false;
    banner_frame(col).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.set_min_height(BUTTON_H);
                glyph_icon(ui, g, col, 16.0);
                ui.add_space(4.0);
                if !lead.is_empty() {
                    ui.label(semibold(RichText::new(lead).size(TXT_BODY).color(BRIGHT())));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(a) = action {
                        clicked = ui.button(a).clicked();
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(msg).size(TXT_BODY).color(TEXT())).wrap());
                    });
                });
            });
        });
    ui.add_space(14.0);
    clicked
}

/// The tinted frame of a banner in `col`, for notices that lay out their own text.
pub(crate) fn banner_frame(col: Color32) -> egui::Frame {
    egui::Frame::new().fill(tint(col, 20)).stroke(Stroke::new(1.0, tint(col, 84))).corner_radius(R_BAR).inner_margin(egui::Margin { left: 12, right: 8, top: 8, bottom: 8 })
}

/// One of a set of exclusive choices that each need a line of explanation: a card with a radio mark, a title and a
/// description, `width` wide.
pub(crate) fn radio_card(ui: &mut egui::Ui, width: f32, on: bool, title: &str, desc: &str) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(width, 56.0), Sense::click());
    let state = state_of(&r);
    let p = ui.painter();
    let fill = if on { tint(PRIMARY(), 18) } else { [RAISED(), shade(RAISED(), 0.06), shade(RAISED(), 0.12)][state] };
    let edge = if on { Stroke::new(1.5, PRIMARY()) } else if state > 0 { Stroke::new(1.0, MUTED()) } else { Stroke::new(1.0, BORDER()) };
    p.rect(rect, R_BAR, fill, edge, egui::StrokeKind::Inside);
    let dot = egui::pos2(rect.left() + 22.0, rect.center().y);
    if on {
        p.circle_filled(dot, 8.0, PRIMARY());
        p.circle_filled(dot, 3.2, ON_PRIMARY());
    } else {
        p.circle(dot, 7.5, WELL(), Stroke::new(1.0, TRACK()));
    }
    let g = p.layout_job(egui::text::LayoutJob::single_section(title.to_string(), ui_fmt(TXT_BODY, BRIGHT(), W_SEMIBOLD, 0.0)));
    galley_at(p, rect.left() + 42.0, rect.center().y - 8.0, g, BRIGHT());
    let mut job = egui::text::LayoutJob::simple_singleline(desc.to_string(), egui::FontId::proportional(TXT_SMALL), MUTED());
    job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width() - 54.0);
    let g = p.layout_job(job);
    galley_at(p, rect.left() + 42.0, rect.center().y + 9.0, g, MUTED());
    focus_ring(ui, &r, rect, R_BAR as f32);
    let enabled = ui.is_enabled();
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::RadioButton, enabled, on, title));
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A card holding a table; the caller paints `table_head` and its rows inside.
pub(crate) fn table_card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    card(ui, egui::Margin::ZERO, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        add(ui)
    })
}

/// The header row of a `table_card`: captions at the given widths, a width of 0 taking what is left. A right-aligned
/// caption starts with '>'.
pub(crate) fn table_head(ui: &mut egui::Ui, cols: &[(&str, f32)]) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), TABLE_HEAD_H), Sense::hover());
    let p = ui.painter();
    let top = egui::CornerRadius { nw: R_BAR - 1, ne: R_BAR - 1, sw: 0, se: 0 };
    p.rect_filled(rect.shrink(1.0), top, HDR());
    p.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, BORDER()));
    let fixed: f32 = cols.iter().map(|c| c.1).sum();
    let rest = (rect.width() - fixed).max(0.0);
    let mut x = rect.left();
    for &(name, w) in cols {
        let w = if w == 0.0 { rest } else { w };
        let (right, name) = match name.strip_prefix('>') {
            Some(n) => (true, n),
            None => (false, name),
        };
        let g = p.layout_job(egui::text::LayoutJob::single_section(name.to_string(), ui_fmt(TXT_SMALL, MUTED(), W_MEDIUM, 0.0)));
        let gx = if right { x + w - 12.0 - g.size().x } else { x + 12.0 };
        galley_at(p, gx, rect.center().y, g, MUTED());
        x += w;
    }
}

/// A hairline between table rows.
pub(crate) fn table_sep(ui: &mut egui::Ui) {
    rule(ui, 0.5);
}

/// The last row of a table card saying what is left out, with an optional action link; returns whether it was clicked.
pub(crate) fn table_foot_row(ui: &mut egui::Ui, text: &str, action: Option<&str>) -> bool {
    table_sep(ui);
    let mut clicked = false;
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 38.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_min_height(38.0);
        ui.add_space(12.0);
        ui.label(RichText::new(text).size(TXT_SMALL).color(MUTED()));
        if let Some(a) = action {
            let c = if next_state(ui) > 0 { BRIGHT() } else { PRIMARY() };
            clicked = ui.add(egui::Label::new(medium(RichText::new(a).size(TXT_SMALL).color(c))).sense(Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand).clicked();
        }
    });
    clicked
}

/// A recessed code card: a title strip when `title` is given, then `add`.
pub(crate) fn code_card<R>(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let r = well_frame()
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if !title.is_empty() {
                egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 8)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(medium(RichText::new(title).size(TXT_SMALL).color(MUTED())));
                });
                let y = ui.cursor().top() - ui.spacing().item_spacing.y / 2.0;
                ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0, BORDER()));
            }
            egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                add(ui)
            })
            .inner
        })
        .inner;
    ui.add_space(14.0);
    r
}

/// A folding statement preview: a header row with a chevron, the title, a count and `first` cut to one line (the first
/// statement, or why there is none); `body` draws the full text when open. Opens by itself each time the count goes
/// from none to some, so a new set of changes is always shown once; the user can close it again.
pub(crate) fn sql_drawer(ui: &mut egui::Ui, id: egui::Id, title: &str, count: usize, first: &str, body: impl FnOnce(&mut egui::Ui)) {
    // Whether it is open, and the count it last showed.
    let (mut open, last) = ui.data(|d| d.get_temp::<(bool, usize)>(id)).unwrap_or_default();
    if count > 0 && last == 0 {
        open = true;
    }
    well_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 38.0), Sense::click());
        if r.clicked() {
            open = !open;
        }
        let state = state_of(&r);
        let p = ui.painter();
        if state > 0 {
            p.rect_filled(rect.shrink(1.0), R_BAR - 1, hover_fill(state));
        }
        chevron(p, egui::pos2(rect.left() + 18.0, rect.center().y), open, MUTED());
        let g = p.layout_job(egui::text::LayoutJob::single_section(title.to_string(), ui_fmt(TXT_BODY, BRIGHT(), W_MEDIUM, 0.0)));
        let tw = g.size().x;
        galley_at(p, rect.left() + 34.0, rect.center().y, g, BRIGHT());
        let n = count.to_string();
        let ng = p.layout_job(egui::text::LayoutJob::single_section(n, mono_fmt(TXT_CAPTION, MUTED(), 400.0)));
        let pill = egui::Rect::from_min_size(egui::pos2(rect.left() + 42.0 + tw, rect.center().y - 9.0), egui::vec2(ng.size().x + 12.0, 18.0));
        p.rect_filled(pill, 9.0, tint(MUTED(), 40));
        galley_at(p, pill.left() + 6.0, rect.center().y, ng, MUTED());
        let line = if first.is_empty() { "-- No changes yet" } else { first.lines().next().unwrap_or("") };
        let room = rect.right() - 14.0 - (pill.right() + 16.0);
        if room > 40.0 {
            let mut job = egui::text::LayoutJob::simple_singleline(line.to_string(), egui::FontId::monospace(TXT_SMALL), FAINT());
            job.wrap = egui::text::TextWrapping::truncate_at_width(room);
            let g = p.layout_job(job);
            galley_at(p, rect.right() - 14.0 - g.size().x, rect.center().y, g, FAINT());
        }
        r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, open, title));
        if open {
            let y = ui.cursor().top();
            ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0, BORDER()));
            egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                body(ui);
            });
        }
    });
    ui.data_mut(|d| d.insert_temp(id, (open, count)));
    ui.add_space(14.0);
}

/// A summary tile: a muted label over a large value, with an optional status dot after it.
pub(crate) fn stat_tile(ui: &mut egui::Ui, width: f32, label: &str, value: &str, dot: Option<Color32>) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 58.0), Sense::hover());
    let p = ui.painter();
    p.rect(rect, R_BAR, RAISED(), Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
    let g = p.layout_job(egui::text::LayoutJob::single_section(label.to_string(), ui_fmt(TXT_SMALL, MUTED(), W_MEDIUM, 0.0)));
    galley_at(p, rect.left() + 14.0, rect.top() + 18.0, g, MUTED());
    let g = p.layout_job(egui::text::LayoutJob::single_section(value.to_string(), ui_fmt(TXT_HEADING, BRIGHT(), W_SEMIBOLD, 0.0)));
    let vw = g.size().x;
    galley_at(p, rect.left() + 14.0, rect.top() + 39.0, g, BRIGHT());
    if let Some(c) = dot {
        p.circle_filled(egui::pos2(rect.left() + 24.0 + vw, rect.top() + 39.0), 3.5, c);
    }
}

/// A rounded mono pill with a small cross that removes it; returns whether the cross was clicked.
pub(crate) fn pill_chip(ui: &mut egui::Ui, text: &str, removable: bool) -> bool {
    let g = ui.painter().layout_job(egui::text::LayoutJob::single_section(text.to_string(), mono_fmt(TXT_SMALL, BRIGHT(), 400.0)));
    let w = g.size().x + if removable { 34.0 } else { 20.0 };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, CHIP_H), Sense::hover());
    ui.painter().rect(rect, rect.height() / 2.0, tint(PRIMARY(), 30), Stroke::new(1.0, tint(PRIMARY(), 80)), egui::StrokeKind::Inside);
    galley_at(ui.painter(), rect.left() + 10.0, rect.center().y, g, BRIGHT());
    if !removable {
        return false;
    }
    let x = egui::Rect::from_center_size(egui::pos2(rect.right() - 13.0, rect.center().y), egui::vec2(16.0, 16.0));
    let r = ui.interact(x, ui.scope_id().with(("pill-x", text)), Sense::click());
    let col = if state_of(&r) > 0 { BRIGHT() } else { MUTED() };
    let (c, d) = (x.center(), 3.0);
    for (a, b) in [(egui::vec2(-d, -d), egui::vec2(d, d)), (egui::vec2(-d, d), egui::vec2(d, -d))] {
        ui.painter().line_segment([c + a, c + b], Stroke::new(1.3, col));
    }
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Remove {text}")));
    r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

/// The solid alert button of a destructive dialog's primary action.
pub(crate) fn danger_solid(ui: &mut egui::Ui, enabled: bool, text: &str) -> egui::Response {
    ui.add_enabled(enabled, primary_button(ui, text, ALERT()))
}

/// A two-column form grid inside a card, for forms that lay out label and field pairs with `form_label`.
pub(crate) fn form_grid<R>(ui: &mut egui::Ui, id: impl std::hash::Hash + std::fmt::Debug, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    plain_card(ui, |ui| egui::Grid::new(id).num_columns(2).spacing([14.0, 10.0]).min_col_width(FORM_LABEL_W + 14.0).show(ui, add).inner)
}

/// The recessed body opened without a closure, for dialogs whose footer reads values the body computes:
/// `let mut b = body_begin(ui); let outer = ui; let ui = &mut b.content_ui;` … `b.end(outer); let ui = outer;`
pub(crate) fn body_begin(ui: &mut egui::Ui) -> egui::containers::frame::Prepared {
    let mut p = egui::Frame::new().fill(PANEL()).inner_margin(BODY_MARGIN).begin(ui);
    let w = p.content_ui.available_width();
    p.content_ui.set_width(w);
    band_spacing(&mut p.content_ui);
    p
}

/// A card around content laid out freely, padded like a form card.
pub(crate) fn plain_card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    card(ui, egui::Margin::symmetric(14, 12), add)
}

/// The footer of a dialog that saves as it goes: one Done button.
pub(crate) fn dialog_done(ui: &mut egui::Ui) -> bool {
    dialog_foot(ui, |ui| ui.button("Done").on_hover_text("Esc").clicked())
}

/// The footer band laid out freely, for windows whose footer is more than a row of buttons.
pub(crate) fn dialog_foot_free<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    rule(ui, 0.5);
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(DIALOG_PAD as i8, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            band_spacing(ui);
            add(ui)
        })
        .inner
}
