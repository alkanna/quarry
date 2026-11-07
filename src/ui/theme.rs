//! Design tokens: palettes, type scale, radii and sizes, plus the egui style built from them.
//! Literal colors and sizes belong here; views use these names.

use eframe::egui::{self, Color32, CornerRadius, FontId, Stroke, TextFormat};

// Colors come from the palette of the theme in use, so they are functions, not constants.
// The dark one is the original's `.dark-mode` theme.
pub(crate) struct Palette {
    pub(crate) bg: Color32,
    pub(crate) panel: Color32,
    pub(crate) raised: Color32,
    pub(crate) hover: Color32,
    pub(crate) border: Color32,
    pub(crate) text: Color32,
    pub(crate) bright: Color32,
    pub(crate) muted: Color32,
    pub(crate) faint: Color32,
    pub(crate) primary: Color32,
    pub(crate) alert: Color32,
    pub(crate) success: Color32,
    pub(crate) warning: Color32,
    pub(crate) c_string: Color32,
    pub(crate) c_number: Color32,
    pub(crate) c_true: Color32,
    pub(crate) c_false: Color32,
    pub(crate) c_date: Color32,
    pub(crate) c_null: Color32,
    pub(crate) c_oid: Color32,
    pub(crate) c_obj: Color32,
    pub(crate) key: Color32,
    pub(crate) c_link: Color32,
    pub(crate) highlight: Color32,
    pub(crate) c_keyword: Color32,
    /// Alternate table rows.
    pub(crate) stripe: Color32,
    /// An off switch.
    pub(crate) track: Color32,
    pub(crate) faint_bg: Color32,
    /// Inputs and control troughs. In light mode the page white is too close to cards to use.
    pub(crate) well: Color32,
}

pub(crate) const DARK_PAL: Palette = Palette {
    bg: Color32::from_rgb(0x10, 0x12, 0x14),
    panel: Color32::from_rgb(0x16, 0x18, 0x1b),
    raised: Color32::from_rgb(0x1c, 0x1f, 0x22),
    hover: Color32::from_rgb(0x27, 0x2b, 0x2f),
    border: Color32::from_rgb(0x32, 0x37, 0x3c),
    text: Color32::from_rgb(0xcc, 0xcc, 0xcc),
    bright: Color32::from_rgb(0xfa, 0xfa, 0xfa),
    muted: Color32::from_rgb(0x8a, 0x8f, 0x96),
    faint: Color32::from_rgb(0x5f, 0x64, 0x69),
    primary: Color32::from_rgb(134, 201, 249),
    alert: Color32::from_rgb(247, 110, 133),
    success: Color32::from_rgb(142, 242, 140),
    warning: Color32::from_rgb(247, 208, 110),
    c_string: Color32::from_rgb(0xf2, 0xa3, 0x65),
    c_number: Color32::from_rgb(0x5c, 0xa5, 0xd6),
    c_true: Color32::from_rgb(0x83, 0xc7, 0x6b),
    c_false: Color32::from_rgb(0xe5, 0x4d, 0x56),
    c_date: Color32::from_rgb(0x66, 0xcc, 0xb4),
    c_null: Color32::from_rgb(0x9e, 0x9e, 0x9e),
    c_oid: Color32::from_rgb(0x00, 0xba, 0xc1),
    c_obj: Color32::from_rgb(0xa3, 0xaa, 0xb5),
    key: Color32::from_rgb(0xd7, 0xda, 0xe0),
    c_link: Color32::from_rgb(0x72, 0xc3, 0xec),
    highlight: Color32::from_rgba_premultiplied(40, 34, 18, 60),
    c_keyword: Color32::from_rgb(0xc4, 0xa7, 0xe7),
    stripe: Color32::from_rgb(0x19, 0x1b, 0x1e),
    track: Color32::from_rgb(0x3a, 0x40, 0x46),
    faint_bg: Color32::from_rgb(0x13, 0x15, 0x17),
    well: Color32::from_rgb(0x10, 0x12, 0x14),
};

/// Same roles as the dark palette, darkened where needed to stay readable on white.
pub(crate) const LIGHT_PAL: Palette = Palette {
    bg: Color32::from_rgb(0xff, 0xff, 0xff),
    panel: Color32::from_rgb(0xf3, 0xf4, 0xf6),
    raised: Color32::from_rgb(0xfa, 0xfb, 0xfc),
    hover: Color32::from_rgb(0xe4, 0xe7, 0xeb),
    border: Color32::from_rgb(0xd2, 0xd6, 0xdb),
    text: Color32::from_rgb(0x2b, 0x2f, 0x34),
    bright: Color32::from_rgb(0x0d, 0x0f, 0x12),
    muted: Color32::from_rgb(0x5f, 0x66, 0x6e),
    faint: Color32::from_rgb(0x8c, 0x93, 0x9a),
    primary: Color32::from_rgb(0x1f, 0x6f, 0xc0),
    alert: Color32::from_rgb(0xc8, 0x28, 0x46),
    success: Color32::from_rgb(0x1f, 0x87, 0x30),
    warning: Color32::from_rgb(0xa8, 0x6a, 0x00),
    c_string: Color32::from_rgb(0xb0, 0x55, 0x00),
    c_number: Color32::from_rgb(0x1d, 0x5f, 0xa8),
    c_true: Color32::from_rgb(0x2e, 0x7d, 0x1f),
    c_false: Color32::from_rgb(0xc6, 0x28, 0x28),
    c_date: Color32::from_rgb(0x00, 0x7d, 0x68),
    c_null: Color32::from_rgb(0x75, 0x75, 0x75),
    c_oid: Color32::from_rgb(0x00, 0x7f, 0x87),
    c_obj: Color32::from_rgb(0x5a, 0x63, 0x6e),
    key: Color32::from_rgb(0x24, 0x29, 0x2f),
    c_link: Color32::from_rgb(0x0b, 0x6a, 0xad),
    highlight: Color32::from_rgba_premultiplied(64, 52, 10, 64),
    c_keyword: Color32::from_rgb(0x7a, 0x3e, 0xb5),
    stripe: Color32::from_rgb(0xf6, 0xf7, 0xf8),
    track: Color32::from_rgb(0xc4, 0xc9, 0xcf),
    faint_bg: Color32::from_rgb(0xf8, 0xf9, 0xfa),
    well: Color32::from_rgb(0xec, 0xee, 0xf1),
};

/// Set at the start of each frame from egui's resolved theme.
pub(crate) static LIGHT_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn pal() -> &'static Palette {
    if LIGHT_MODE.load(std::sync::atomic::Ordering::Relaxed) { &LIGHT_PAL } else { &DARK_PAL }
}

macro_rules! colors {
    ($($name:ident: $field:ident),* $(,)?) => {
        $(
            #[allow(non_snake_case)]
            pub(crate) fn $name() -> Color32 {
                pal().$field
            }
        )*
    };
}

colors!(BG: bg, PANEL: panel, RAISED: raised, HOVER: hover, BORDER: border, TEXT: text, BRIGHT: bright, MUTED: muted, FAINT: faint, PRIMARY: primary, ALERT: alert, SUCCESS: success, WARNING: warning, C_STRING: c_string, C_NUMBER: c_number, C_TRUE: c_true, C_FALSE: c_false, C_DATE: c_date, C_NULL: c_null, C_OID: c_oid, C_OBJ: c_obj, KEY: key, C_LINK: c_link, HIGHLIGHT: highlight, C_KEYWORD: c_keyword);

/// `c` at `alpha` out of 255, for tints over the background.
pub(crate) fn tint(c: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha)
}

pub(crate) const SIDEBAR_W: f32 = 250.0;

// Type scale: every text size in the app is one of these.
/// Section captions, badges and hints.
pub(crate) const TXT_CAPTION: f32 = 11.0;

/// Secondary text: chips, notes, counts.
pub(crate) const TXT_SMALL: f32 = 12.0;

/// Body, buttons, inputs and code.
pub(crate) const TXT_BODY: f32 = 12.5;

/// Names in lists: connections, databases, collections.
pub(crate) const TXT_TITLE: f32 = 14.0;

/// Dialog titles and the collection heading.
pub(crate) const TXT_HEADING: f32 = 17.0;

/// Height of every button and single-line input.
pub(crate) const BUTTON_H: f32 = 24.0;

// Corner radii: small for rows and glyphs, medium for widgets and cards, large for popups and dialogs.
pub(crate) const R_SM: u8 = 4;

pub(crate) const R_MD: u8 = 8;

pub(crate) const R_LG: u8 = 12;

pub(crate) const SIDE_ROW_H: f32 = 28.0;

pub(crate) const HEADER_H: f32 = 40.0;

pub(crate) const ROW_H: f32 = 28.0;

pub(crate) const OPTIMAL_MIN_W: f32 = 40.0;

pub(crate) const OPTIMAL_MAX_W: f32 = 400.0;

pub(crate) const OPTIMAL_PAD: f32 = 10.0;

pub(crate) const CELL_PAD: f32 = 16.0;

pub(crate) const NUM_W: f32 = 40.0;

/// Fixed so an open editor never pushes its panel wider.
pub(crate) const EDITOR_W: f32 = 230.0;

/// Visuals for both themes; egui picks one from the system or the Appearance setting.
pub(crate) fn style(ctx: &egui::Context) {
    for (theme, p) in [(egui::Theme::Dark, &DARK_PAL), (egui::Theme::Light, &LIGHT_PAL)] {
        ctx.set_visuals_of(theme, visuals(theme, p));
    }
    // egui's default UI font draws about 0.75 pt high at body size, so text looked off center in
    // buttons and inputs. Shifting the glyphs only moves the ink, not the layout. The icon fonts
    // that fill in for glyphs Ubuntu lacks get the same shift, or icons sit above their labels.
    let mut fonts = egui::FontDefinitions::default();
    for name in ["Ubuntu-Light", "egui-icons", "NotoEmoji-Regular", "emoji-icon-font"] {
        if let Some(d) = fonts.font_data.get_mut(name) {
            let tweak = egui::FontTweak { y_offset_factor: 0.06, ..d.tweak.clone() };
            *d = std::sync::Arc::new((**d).clone().tweak(tweak));
        }
    }
    ctx.set_fonts(fonts);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 4.0);
        // Every button and single-line input is this tall, so toolbars line up.
        s.spacing.interact_size.y = BUTTON_H;
        use egui::{FontFamily::*, TextStyle};
        s.text_styles = [
            (TextStyle::Small, FontId::new(TXT_CAPTION, Proportional)),
            (TextStyle::Body, FontId::new(TXT_BODY, Proportional)),
            (TextStyle::Button, FontId::new(TXT_BODY, Proportional)),
            (TextStyle::Monospace, FontId::new(TXT_BODY, Monospace)),
            (TextStyle::Heading, FontId::new(TXT_HEADING, Proportional)),
        ]
        .into();
        // No bounce past the edge of lists.
        s.spacing.scroll.kinetic.rubber_band = false;
        // Scrollbars stay faintly visible wherever content overflows, so long lists and menus
        // show there is more without hovering them first.
        s.spacing.scroll.dormant_handle_opacity = 0.5;
    });
}

pub(crate) fn visuals(theme: egui::Theme, p: &Palette) -> egui::Visuals {
    let mut v = theme.default_visuals();
    v.panel_fill = p.panel;
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);
    v.window_fill = p.raised;
    v.extreme_bg_color = p.bg;
    v.faint_bg_color = p.faint_bg;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.selection.bg_fill = tint(p.primary, 48);
    v.selection.stroke = Stroke::new(1.0, p.primary);
    v.hyperlink_color = p.primary;
    v.override_text_color = Some(p.text);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    // Translucent so a button reads as a button on any surface: panel, card or dialog.
    v.widgets.inactive.weak_bg_fill = tint(p.text, 18);
    v.widgets.hovered.weak_bg_fill = tint(p.text, 34);
    v.widgets.active.weak_bg_fill = tint(p.text, 46);
    v.widgets.open.weak_bg_fill = tint(p.text, 46);
    // Faint outline on hover: the only hover cue for inputs, checkboxes and filled buttons.
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, tint(p.text, 60));
    v.widgets.active.bg_stroke = Stroke::new(1.0, tint(p.text, 90));
    v.widgets.open.bg_stroke = Stroke::new(1.0, tint(p.text, 60));
    v.text_edit_bg_color = Some(p.well);
    // Checkboxes and radios fill with bg_fill and have no stroke, so it must stand out from cards.
    v.widgets.inactive.bg_fill = p.track;
    v.widgets.hovered.bg_fill = p.track.lerp_to_gamma(p.text, 0.15);
    v.widgets.active.bg_fill = p.track.lerp_to_gamma(p.text, 0.3);
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.noninteractive] {
        w.corner_radius = CornerRadius::same(R_MD);
    }
    v
}

pub(crate) const CHIP_H: f32 = 22.0;

/// `c` moved by `t` away from its own lightness: toward white barely changes an already light color.
pub(crate) fn shade(c: Color32, t: f32) -> Color32 {
    let light = (0.3 * c.r() as f32 + 0.59 * c.g() as f32 + 0.11 * c.b() as f32) / 255.0 > 0.55;
    c.lerp_to_gamma(if light { Color32::BLACK } else { Color32::WHITE }, t)
}

/// The frame of floating cards over the editors: suggestion lists and docs.
pub(crate) fn popup_card(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::popup(ui.style()).fill(RAISED()).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_MD)
}

/// How far to move a one-line galley so its ink, from the tallest letter to the baseline, is
/// centered in its box. Box centering counts room for descenders, so all-caps labels sat high.
pub(crate) fn optical_dy(g: &egui::Galley) -> f32 {
    let Some(row) = g.rows.first() else { return 0.0 };
    let Some(base) = row.glyphs.first().map(|gl| gl.pos.y) else { return 0.0 };
    let top = row.glyphs.iter().filter(|gl| gl.uv_rect.size.y > 0.0).map(|gl| gl.pos.y + gl.uv_rect.offset.y).fold(f32::INFINITY, f32::min);
    if !top.is_finite() {
        return 0.0;
    }
    g.size().y / 2.0 - (row.pos.y + (top + base) / 2.0)
}

/// A fill for the next widget that follows hover and press. egui uses a button's own fill for
/// every state, so buttons with one would otherwise never react. Reads last frame's response
/// of the widget about to be added, which has the next auto id.
pub(crate) fn state_fill(ui: &egui::Ui, base: Color32) -> Color32 {
    let r = ui.ctx().read_response(ui.next_auto_id()).filter(|r| r.enabled() && ui.is_enabled());
    let k = match r {
        Some(r) if r.is_pointer_button_down_on() => 2,
        Some(r) if r.hovered() => 1,
        _ => 0,
    };
    if base.a() == 0 {
        // A transparent option shows the same hover as an ordinary button.
        [base, tint(TEXT(), 34), tint(TEXT(), 46)][k]
    } else if base.a() < 255 {
        let [r, g, b, a] = base.to_srgba_unmultiplied();
        let c = Color32::from_rgb(r, g, b);
        [base, tint(c, a.saturating_mul(2)), tint(c, a.saturating_mul(3))][k]
    } else {
        [base, shade(base, 0.12), shade(base, 0.24)][k]
    }
}

/// Label column width of dialog form grids, so fields in every section start at the same x.
pub(crate) const FORM_LABEL_W: f32 = 84.0;

pub(crate) const MONO_PX: f32 = TXT_BODY;

/// Width of the checkbox column in front of the row number.
pub(crate) const CHECK_W: f32 = 22.0;

pub(crate) fn mono(c: Color32) -> TextFormat {
    TextFormat { font_id: FontId::monospace(MONO_PX), color: c, ..Default::default() }
}
