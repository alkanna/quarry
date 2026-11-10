//! Design tokens: palettes, type scale, radii and sizes, plus the egui style built from them.
//! Literal colors and sizes belong here; views use these names.

use eframe::egui::{self, Color32, CornerRadius, FontId, RichText, Stroke, TextFormat};

// Colors come from the palette of the theme in use, so they are functions, not constants.
// Dark is "Workbench" and light is "Paper"; both share one layout, only colors differ.
pub(crate) struct Palette {
    /// The workspace: central page, table body, active tab and inputs.
    pub(crate) bg: Color32,
    /// Sidebar, tab strip and the query card.
    pub(crate) panel: Color32,
    /// Title bar, status bar and the activity rail.
    pub(crate) chrome: Color32,
    /// Controls that sit above a surface: pickers, kbd hints, popups, dialogs.
    pub(crate) raised: Color32,
    pub(crate) hover: Color32,
    pub(crate) pressed: Color32,
    /// Hover inside popups and menus, which sit on `raised`.
    pub(crate) menu_hover: Color32,
    /// Control outlines.
    pub(crate) border: Color32,
    /// Hairlines and dividers, quieter than `border`.
    pub(crate) line: Color32,
    /// Dividers inside popups and dialogs; `line` vanishes on `raised` in the dark theme.
    pub(crate) raised_line: Color32,
    /// The table header row.
    pub(crate) hdr: Color32,
    pub(crate) text: Color32,
    pub(crate) bright: Color32,
    pub(crate) muted: Color32,
    pub(crate) faint: Color32,
    pub(crate) primary: Color32,
    /// Text and marks drawn on a `primary` fill.
    pub(crate) on_primary: Color32,
    /// The selected row, opaque so it reads the same over stripes.
    pub(crate) sel: Color32,
    pub(crate) sel_hover: Color32,
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
    /// An off switch and checkbox troughs.
    pub(crate) track: Color32,
    /// Inputs and control troughs.
    pub(crate) well: Color32,
}

const fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

pub(crate) const DARK_PAL: Palette = Palette {
    bg: hex(0x1e1f22),
    panel: hex(0x2b2d30),
    chrome: hex(0x2b2d30),
    raised: hex(0x393b40),
    hover: hex(0x35373b),
    pressed: hex(0x3e4045),
    menu_hover: hex(0x45474d),
    border: hex(0x43454a),
    line: hex(0x393b40),
    raised_line: hex(0x43454a),
    hdr: hex(0x2b2d30),
    text: hex(0xbcbec4),
    bright: hex(0xdfe1e5),
    muted: hex(0x8c8f96),
    faint: hex(0x6f737a),
    primary: hex(0x3574f0),
    on_primary: hex(0xffffff),
    sel: hex(0x2e436e),
    sel_hover: hex(0x34497a),
    alert: hex(0xf75464),
    success: hex(0x5fb865),
    warning: hex(0xe0a84e),
    c_string: hex(0x6aab73),
    c_number: hex(0x2aacb8),
    c_true: hex(0x5fb865),
    c_false: hex(0xf75464),
    c_date: hex(0x56a8f5),
    c_null: hex(0x8c8f96),
    c_oid: hex(0xc77dbb),
    c_obj: hex(0x8c8f96),
    key: hex(0xbcbec4),
    c_link: hex(0x56a8f5),
    // #E0A84E at 25%, premultiplied.
    highlight: Color32::from_rgba_premultiplied(56, 42, 20, 64),
    c_keyword: hex(0xcf8e6d),
    stripe: hex(0x232427),
    track: hex(0x4e5157),
    well: hex(0x1e1f22),
};

pub(crate) const LIGHT_PAL: Palette = Palette {
    bg: hex(0xfbfaf7),
    panel: hex(0xf5f3ee),
    chrome: hex(0xf1eee8),
    raised: hex(0xffffff),
    hover: hex(0xece8e1),
    pressed: hex(0xe3ded5),
    menu_hover: hex(0xf1eee8),
    border: hex(0xdcd7ce),
    line: hex(0xe5e1d9),
    raised_line: hex(0xe5e1d9),
    hdr: hex(0xf3f1ec),
    text: hex(0x3a3631),
    bright: hex(0x15130f),
    muted: hex(0x6e685f),
    faint: hex(0xa39c91),
    primary: hex(0xd9480f),
    on_primary: hex(0xffffff),
    // The accent at 8% and 12% over `bg`.
    sel: hex(0xf8ece4),
    sel_hover: hex(0xf7e5db),
    alert: hex(0xc62828),
    success: hex(0x2f8a3e),
    warning: hex(0xa86a00),
    c_string: hex(0x2f7d32),
    c_number: hex(0x1d5fa8),
    c_true: hex(0x2e7d1f),
    c_false: hex(0xc62828),
    c_date: hex(0x00796b),
    c_null: hex(0x8c939a),
    c_oid: hex(0x9a5b13),
    c_obj: hex(0x6e685f),
    key: hex(0x3a3631),
    c_link: hex(0x0b6aad),
    // #A86A00 at 20%, premultiplied.
    highlight: Color32::from_rgba_premultiplied(34, 21, 0, 51),
    c_keyword: hex(0x8a3fb5),
    stripe: hex(0xf9f7f3),
    track: hex(0xcfc9bf),
    well: hex(0xfbfaf7),
};

/// Set at the start of each frame from egui's resolved theme.
pub(crate) static LIGHT_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn is_light() -> bool {
    LIGHT_MODE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Dialogs and floating windows: a deeper shadow on the dark theme, where a light one vanishes.
pub(crate) fn window_shadow() -> egui::Shadow {
    egui::Shadow { offset: [0, 6], blur: 20, spread: 0, color: Color32::from_black_alpha(if is_light() { 30 } else { 90 }) }
}

pub(crate) fn pal() -> &'static Palette {
    if is_light() { &LIGHT_PAL } else { &DARK_PAL }
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

colors!(BG: bg, PANEL: panel, CHROME: chrome, RAISED: raised, HOVER: hover, PRESSED: pressed, MENU_HOVER: menu_hover, BORDER: border, LINE: line, RAISED_LINE: raised_line, HDR: hdr, ON_PRIMARY: on_primary, SEL: sel, SEL_HOVER: sel_hover, TEXT: text, BRIGHT: bright, MUTED: muted, FAINT: faint, PRIMARY: primary, ALERT: alert, SUCCESS: success, WARNING: warning, C_STRING: c_string, C_NUMBER: c_number, C_TRUE: c_true, C_FALSE: c_false, C_DATE: c_date, C_NULL: c_null, C_OID: c_oid, C_OBJ: c_obj, KEY: key, C_LINK: c_link, HIGHLIGHT: highlight, C_KEYWORD: c_keyword);

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
pub(crate) const TXT_TITLE: f32 = 13.0;

/// Dialog titles and the collection heading.
pub(crate) const TXT_HEADING: f32 = 18.0;

/// Titles of error panels and dialogs, between list names and headings.
pub(crate) const TXT_SUBHEAD: f32 = 15.0;

/// The Cmd+K search field, a step above titles so it reads as the palette's heading.
pub(crate) const TXT_SEARCH: f32 = 15.0;

/// Height of every button and single-line input.
pub(crate) const BUTTON_H: f32 = 24.0;
/// Height of the bolt marking an indexed field, and the room it takes beside a label.
pub(crate) const BOLT_H: f32 = 10.0;
pub(crate) const BOLT_ROOM: f32 = 12.0;
/// Size of the small painted cross that removes a chip or a tab.
pub(crate) const CLOSE_W: f32 = 18.0;

// Corner radii: small for rows and glyphs, medium for widgets and cards, large for popups and dialogs.
pub(crate) const R_SM: u8 = 4;

pub(crate) const R_MD: u8 = 6;

pub(crate) const R_LG: u8 = 10;
/// The Documents find bar, a step rounder than inputs since it frames a whole row.
pub(crate) const R_BAR: u8 = R_MD + 2;

pub(crate) const SIDE_ROW_H: f32 = 26.0;

/// Also where the macOS traffic lights are centered, so it stays 40.
pub(crate) const HEADER_H: f32 = 40.0;

pub(crate) const TAB_STRIP_H: f32 = 34.0;

pub(crate) const STATUS_H: f32 = 26.0;

pub(crate) const RAIL_W: f32 = 44.0;

pub(crate) const TABLE_HEAD_H: f32 = 30.0;

// The Cmd+K palette: search row, two-line result, one-line command, section caption and key-hint footer.
pub(crate) const PALETTE_HEAD_H: f32 = 54.0;
pub(crate) const PALETTE_ROW_H: f32 = 44.0;
pub(crate) const PALETTE_CMD_H: f32 = 34.0;
pub(crate) const PALETTE_CAPTION_H: f32 = 26.0;
pub(crate) const PALETTE_FOOT_H: f32 = 36.0;

// Font weights, applied through the variable fonts' wght axis.
pub(crate) const W_MEDIUM: f32 = 500.0;

pub(crate) const W_SEMIBOLD: f32 = 600.0;

pub(crate) fn medium(t: impl Into<RichText>) -> RichText {
    t.into().variation(b"wght", W_MEDIUM)
}

pub(crate) fn semibold(t: impl Into<RichText>) -> RichText {
    t.into().variation(b"wght", W_SEMIBOLD)
}

/// Monospace at `size` and `weight` (400 for regular).
pub(crate) fn mono_fmt(size: f32, color: Color32, weight: f32) -> TextFormat {
    TextFormat { font_id: FontId::monospace(size), color, coords: egui::epaint::text::VariationCoords::new([(b"wght", weight)]), ..Default::default() }
}

/// Proportional at `size` and `weight`, with optional letter spacing for captions.
pub(crate) fn ui_fmt(size: f32, color: Color32, weight: f32, spacing: f32) -> TextFormat {
    TextFormat { font_id: FontId::proportional(size), color, extra_letter_spacing: spacing, coords: egui::epaint::text::VariationCoords::new([(b"wght", weight)]), ..Default::default() }
}

pub(crate) const ROW_H: f32 = 28.0;

/// Rows in popovers: one line, and a name over a code preview.
pub(crate) const POP_ROW_H: f32 = 30.0;
pub(crate) const POP_ROW2_H: f32 = 48.0;

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
    ctx.set_fonts(fonts());
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

static INTER: &[u8] = include_bytes!("../../assets/fonts/InterVariable.ttf");
static JB_MONO: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Variable.ttf");

/// Inter for the interface and JetBrains Mono for data, both variable so one file covers every
/// weight. egui's own fonts stay behind them as fallbacks for icons and emoji. Inter's metrics
/// center in buttons as they are, so it needs none of the shift egui's Ubuntu font did.
fn fonts() -> egui::FontDefinitions {
    use egui::{FontData, FontFamily, FontTweak, epaint::text::VariationCoords};
    let mut fonts = egui::FontDefinitions::default();
    // Without an explicit weight a variable font draws its default instance, which for
    // JetBrains Mono is not Regular.
    let regular = |bytes: &'static [u8]| std::sync::Arc::new(FontData::from_static(bytes).tweak(FontTweak { coords: VariationCoords::new([(b"wght", 400.0)]), ..Default::default() }));
    fonts.font_data.insert("Inter".into(), regular(INTER));
    fonts.font_data.insert("JetBrainsMono".into(), regular(JB_MONO));
    fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "Inter".into());
    fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "JetBrainsMono".into());
    fonts
}

pub(crate) fn visuals(theme: egui::Theme, p: &Palette) -> egui::Visuals {
    let mut v = theme.default_visuals();
    v.panel_fill = p.panel;
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);
    v.window_fill = p.raised;
    v.extreme_bg_color = p.bg;
    v.faint_bg_color = p.stripe;
    v.code_bg_color = p.hover;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(R_LG);
    v.menu_corner_radius = CornerRadius::same(R_MD);
    let shadow = egui::Shadow { offset: [0, 6], blur: 20, spread: 0, color: Color32::from_black_alpha(if theme == egui::Theme::Dark { 90 } else { 30 }) };
    v.window_shadow = shadow;
    v.popup_shadow = egui::Shadow { offset: [0, 4], blur: 12, ..shadow };
    v.selection.bg_fill = tint(p.primary, 56);
    // Also the focus outline of inputs.
    v.selection.stroke = Stroke::new(1.0, p.primary);
    v.hyperlink_color = p.c_link;
    v.text_edit_bg_color = Some(p.well);
    // Labels and separators. Separators use the border color: menus and dialogs sit on `raised`,
    // which in the dark palette is the same color as `line`, so a line separator vanished there.
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    // Buttons are raised and outlined at rest, so they read as buttons on any surface; hover
    // brightens the outline and text, press darkens the fill.
    v.widgets.inactive.weak_bg_fill = p.raised;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.hovered.weak_bg_fill = p.raised.lerp_to_gamma(p.hover, 0.5);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.muted);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, p.bright);
    v.widgets.hovered.expansion = 0.0;
    v.widgets.active.weak_bg_fill = p.pressed;
    v.widgets.active.bg_stroke = Stroke::new(1.0, p.muted);
    v.widgets.active.fg_stroke = Stroke::new(1.0, p.bright);
    v.widgets.active.expansion = 0.0;
    v.widgets.open.weak_bg_fill = p.pressed;
    v.widgets.open.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.open.fg_stroke = Stroke::new(1.0, p.bright);
    // Checkboxes, radios and slider rails fill with bg_fill, so it must stand out from cards.
    v.widgets.inactive.bg_fill = p.track;
    v.widgets.hovered.bg_fill = p.track.lerp_to_gamma(p.text, 0.15);
    v.widgets.active.bg_fill = p.track.lerp_to_gamma(p.text, 0.3);
    v.widgets.open.bg_fill = p.track;
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open, &mut v.widgets.noninteractive] {
        w.corner_radius = CornerRadius::same(R_MD);
    }
    v
}

pub(crate) const CHIP_H: f32 = 22.0;
/// The status bar's own chips, like the error count, which must fit its 26 pt height.
pub(crate) const STATUS_CHIP_H: f32 = 17.0;

/// `c` moved by `t` away from its own lightness: toward white barely changes an already light color.
pub(crate) fn shade(c: Color32, t: f32) -> Color32 {
    let light = (0.3 * c.r() as f32 + 0.59 * c.g() as f32 + 0.11 * c.b() as f32) / 255.0 > 0.55;
    c.lerp_to_gamma(if light { Color32::BLACK } else { Color32::WHITE }, t)
}

/// The frame of floating cards over the editors: suggestion lists and docs.
pub(crate) fn popup_card(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::popup(ui.style()).fill(RAISED()).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_MD)
}

/// Width of `text` on one line in `font`.
pub(crate) fn text_w(ui: &egui::Ui, text: &str, font: FontId) -> f32 {
    ui.fonts_mut(|f| f.layout_no_wrap(text.to_string(), font, Color32::PLACEHOLDER).size().x)
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
    // Glyph bitmaps round their top up to a whole pixel, so the real ink starts up to a pixel lower.
    // Taking the middle of that pixel moved caps from 1 px low to centered at 1x.
    let top = top + 0.5 / g.pixels_per_point;
    g.size().y / 2.0 - (row.pos.y + (top + base) / 2.0)
}

/// 0 idle, 1 hovered, 2 pressed, for the widget about to be added, from last frame's response
/// of the next auto id.
pub(crate) fn next_state(ui: &egui::Ui) -> usize {
    match ui.ctx().read_response(ui.next_auto_id()).filter(|r| r.enabled() && ui.is_enabled()) {
        Some(r) if r.is_pointer_button_down_on() => 2,
        Some(r) if r.hovered() => 1,
        _ => 0,
    }
}

/// The fill of a frameless control on a page or panel for state 0 idle, 1 hovered, 2 pressed.
pub(crate) fn hover_fill(state: usize) -> Color32 {
    [Color32::TRANSPARENT, HOVER(), PRESSED()][state]
}

/// `hover_fill` for the layer `ui` draws on. Popups, menus and dialogs sit on `RAISED`, where a
/// hovered row is already `MENU_HOVER`, so a control's states there step past it.
pub(crate) fn hover_fill_on(ui: &egui::Ui, state: usize) -> Color32 {
    if ui.layer_id().order == egui::Order::Background {
        hover_fill(state)
    } else {
        [Color32::TRANSPARENT, menu_pressed(), menu_pressed().lerp_to_gamma(TEXT(), 0.08)][state]
    }
}

/// A pressed row inside a raised popup or menu.
pub(crate) fn menu_pressed() -> Color32 {
    MENU_HOVER().lerp_to_gamma(TEXT(), 0.08)
}

/// A fill for the next widget that follows hover and press. egui uses a button's own fill for
/// every state, so buttons with one would otherwise never react. Reads last frame's response
/// of the widget about to be added, which has the next auto id.
pub(crate) fn state_fill(ui: &egui::Ui, base: Color32) -> Color32 {
    let k = next_state(ui);
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
