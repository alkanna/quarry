//! Design tokens: palettes, type scale, radii and sizes, plus the egui style built from them.
//! Literal colors and sizes belong here; views use these names.

use eframe::egui::{self, Color32, CornerRadius, FontId, RichText, Stroke, TextFormat};

// Colors come from the palette of the theme in use, so they are functions, not constants.
// Every theme shares one layout, only colors differ. Designs are in docs/themes.figcraft.
#[derive(Clone, Copy)]
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
    /// The PostgreSQL engine accent.
    pub(crate) engine_pg: Color32,
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

/// `v` at `a` out of 255, premultiplied, for the search highlight.
const fn hl(v: u32, a: u32) -> Color32 {
    Color32::from_rgba_premultiplied(((v >> 16 & 0xff) * a / 255) as u8, ((v >> 8 & 0xff) * a / 255) as u8, ((v & 0xff) * a / 255) as u8, a as u8)
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
    engine_pg: hex(0x2aacb8),
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
    engine_pg: hex(0x00796b),
    stripe: hex(0xf9f7f3),
    track: hex(0xcfc9bf),
    well: hex(0xfbfaf7),
};

/// The Soft background: less contrast between text and background, with no near-black or near-white.
pub(crate) const DARK_SOFT_PAL: Palette = Palette {
    bg: hex(0x25272b),
    panel: hex(0x2e3034),
    chrome: hex(0x2e3034),
    raised: hex(0x3a3c41),
    hover: hex(0x383a3f),
    pressed: hex(0x404247),
    menu_hover: hex(0x46484d),
    border: hex(0x45474c),
    line: hex(0x3a3c41),
    raised_line: hex(0x45474c),
    hdr: hex(0x2e3034),
    text: hex(0xb3b6bb),
    bright: hex(0xcfd1d5),
    muted: hex(0x8a8d93),
    stripe: hex(0x292b2f),
    well: hex(0x25272b),
    ..DARK_PAL
};

pub(crate) const LIGHT_SOFT_PAL: Palette = Palette {
    bg: hex(0xf7f0e1),
    panel: hex(0xf1e9d7),
    chrome: hex(0xede4d0),
    raised: hex(0xfbf6ea),
    hover: hex(0xe9dfc9),
    pressed: hex(0xe0d5bd),
    menu_hover: hex(0xede4d0),
    border: hex(0xd8ccb4),
    line: hex(0xe3d9c4),
    raised_line: hex(0xe3d9c4),
    hdr: hex(0xefe7d4),
    text: hex(0x45403a),
    bright: hex(0x2b2722),
    muted: hex(0x6d665b),
    faint: hex(0xa0978a),
    sel: hex(0xf3e2cf),
    sel_hover: hex(0xf1dcc6),
    stripe: hex(0xf4ecdb),
    track: hex(0xcdc2ad),
    well: hex(0xf7f0e1),
    ..LIGHT_PAL
};

// The accents of Midnight, Ember, Dusk and Carbon are bright, so text on them is dark: white is under 4.5:1.
const MIDNIGHT_PAL: Palette = Palette {
    bg: hex(0x10141c),
    panel: hex(0x161b25),
    chrome: hex(0x131821),
    raised: hex(0x1f2633),
    hover: hex(0x1c2230),
    pressed: hex(0x232b3a),
    menu_hover: hex(0x2a3343),
    border: hex(0x283040),
    line: hex(0x1f2633),
    raised_line: hex(0x2c3546),
    hdr: hex(0x161b25),
    text: hex(0xc3cad8),
    bright: hex(0xe6ebf5),
    muted: hex(0x8590a6),
    faint: hex(0x5f6a80),
    primary: hex(0x38bdf8),
    on_primary: hex(0x04121c),
    sel: hex(0x173049),
    sel_hover: hex(0x1b3854),
    alert: hex(0xff6b7a),
    success: hex(0x4fd18b),
    warning: hex(0xf2b552),
    c_string: hex(0x7ee0a1),
    c_number: hex(0xf5a97f),
    c_true: hex(0x4fd18b),
    c_false: hex(0xff6b7a),
    c_date: hex(0x82aaff),
    c_null: hex(0x6c7891),
    c_oid: hex(0xc79bff),
    c_obj: hex(0x8590a6),
    key: hex(0xc3cad8),
    c_link: hex(0x38bdf8),
    highlight: hl(0xf2b552, 64),
    c_keyword: hex(0xc79bff),
    engine_pg: hex(0x5eead4),
    stripe: hex(0x131821),
    track: hex(0x334056),
    well: hex(0x0d1118),
};

/// Paper's dark twin.
const EMBER_PAL: Palette = Palette {
    bg: hex(0x1a1714),
    panel: hex(0x221e1a),
    chrome: hex(0x1e1b17),
    raised: hex(0x2e2924),
    hover: hex(0x2a2520),
    pressed: hex(0x332d27),
    menu_hover: hex(0x3a342d),
    border: hex(0x3a332c),
    line: hex(0x2e2924),
    raised_line: hex(0x40392f),
    hdr: hex(0x221e1a),
    text: hex(0xd2c8bc),
    bright: hex(0xf2ebe2),
    muted: hex(0x9a8f82),
    faint: hex(0x6e655a),
    primary: hex(0xf0743e),
    on_primary: hex(0x1c1009),
    sel: hex(0x3d2618),
    sel_hover: hex(0x472c1b),
    alert: hex(0xff6f61),
    success: hex(0x8cc265),
    warning: hex(0xf2b14c),
    c_string: hex(0xb5cc6a),
    c_number: hex(0xf2a65a),
    c_true: hex(0x8cc265),
    c_false: hex(0xff6f61),
    c_date: hex(0x7fbfb4),
    c_null: hex(0x8a8076),
    c_oid: hex(0xd59ac0),
    c_obj: hex(0x9a8f82),
    key: hex(0xd2c8bc),
    c_link: hex(0x7fbfb4),
    highlight: hl(0xf2b14c, 64),
    c_keyword: hex(0xe8b86b),
    engine_pg: hex(0x7fbfb4),
    stripe: hex(0x1e1b17),
    track: hex(0x4a4239),
    well: hex(0x161310),
};

const DUSK_PAL: Palette = Palette {
    bg: hex(0x1a1722),
    panel: hex(0x211d2b),
    chrome: hex(0x1d1a26),
    raised: hex(0x2c2738),
    hover: hex(0x282334),
    pressed: hex(0x302a3e),
    menu_hover: hex(0x383148),
    border: hex(0x3a3348),
    line: hex(0x2c2738),
    raised_line: hex(0x403852),
    hdr: hex(0x211d2b),
    text: hex(0xccc4da),
    bright: hex(0xefe9f7),
    muted: hex(0x958ca8),
    faint: hex(0x6a6280),
    primary: hex(0xe56fa8),
    on_primary: hex(0x1e0a15),
    sel: hex(0x3d2443),
    sel_hover: hex(0x472a4e),
    alert: hex(0xff6e7f),
    success: hex(0x6fd6a6),
    warning: hex(0xf3c06b),
    c_string: hex(0xa6da95),
    c_number: hex(0xf5a97f),
    c_true: hex(0x6fd6a6),
    c_false: hex(0xff6e7f),
    c_date: hex(0x8bb8ff),
    c_null: hex(0x7c7392),
    c_oid: hex(0xf0a6ca),
    c_obj: hex(0x958ca8),
    key: hex(0xccc4da),
    c_link: hex(0x8bb8ff),
    highlight: hl(0xf3c06b, 64),
    c_keyword: hex(0xc3a6ff),
    engine_pg: hex(0x7dd3fc),
    stripe: hex(0x1d1a26),
    track: hex(0x463e58),
    well: hex(0x16131d),
};

/// High contrast: true black, near-white text and outlines that stand out.
const CARBON_PAL: Palette = Palette {
    bg: hex(0x000000),
    panel: hex(0x0d0d0d),
    chrome: hex(0x0a0a0a),
    raised: hex(0x1a1a1a),
    hover: hex(0x1c1c1c),
    pressed: hex(0x262626),
    menu_hover: hex(0x2e2e2e),
    border: hex(0x5c5c5c),
    line: hex(0x2b2b2b),
    raised_line: hex(0x3d3d3d),
    hdr: hex(0x0d0d0d),
    text: hex(0xf0f0f0),
    bright: hex(0xffffff),
    muted: hex(0xbdbdbd),
    faint: hex(0x8f8f8f),
    primary: hex(0xffb000),
    on_primary: hex(0x000000),
    sel: hex(0x3a2a00),
    sel_hover: hex(0x4a3600),
    alert: hex(0xff5c5c),
    success: hex(0x3ee07a),
    warning: hex(0xffd23f),
    c_string: hex(0x7cf29a),
    c_number: hex(0x5cd6ff),
    c_true: hex(0x3ee07a),
    c_false: hex(0xff5c5c),
    c_date: hex(0x8fb8ff),
    c_null: hex(0xa6a6a6),
    c_oid: hex(0xff8fe0),
    c_obj: hex(0xbdbdbd),
    key: hex(0xf0f0f0),
    c_link: hex(0x5cd6ff),
    highlight: hl(0xffd23f, 80),
    c_keyword: hex(0xff9a5c),
    engine_pg: hex(0x5cd6ff),
    stripe: hex(0x0a0a0a),
    track: hex(0x4d4d4d),
    well: hex(0x000000),
};

const GLACIER_PAL: Palette = Palette {
    bg: hex(0xf8fafc),
    panel: hex(0xf1f4f8),
    chrome: hex(0xeaeef4),
    raised: hex(0xffffff),
    hover: hex(0xe5eaf1),
    pressed: hex(0xdae1ea),
    menu_hover: hex(0xeef2f7),
    border: hex(0xd2dae5),
    line: hex(0xe1e7ef),
    raised_line: hex(0xe1e7ef),
    hdr: hex(0xeff2f7),
    text: hex(0x2c3442),
    bright: hex(0x0f1623),
    muted: hex(0x5d6878),
    faint: hex(0x95a0b0),
    primary: hex(0x3b5bdb),
    on_primary: hex(0xffffff),
    sel: hex(0xe6ebfb),
    sel_hover: hex(0xdde4fa),
    alert: hex(0xc92a3a),
    success: hex(0x2b8a3e),
    warning: hex(0xb06d00),
    c_string: hex(0x2b8a3e),
    c_number: hex(0x0b7285),
    c_true: hex(0x2b8a3e),
    c_false: hex(0xc92a3a),
    c_date: hex(0x6741d9),
    c_null: hex(0x8a94a3),
    c_oid: hex(0xa35400),
    c_obj: hex(0x5d6878),
    key: hex(0x2c3442),
    c_link: hex(0x1c64d8),
    highlight: hl(0xb06d00, 51),
    c_keyword: hex(0xae3ec9),
    engine_pg: hex(0x0b7285),
    stripe: hex(0xf3f6fa),
    track: hex(0xc5cfdc),
    well: hex(0xffffff),
};

const SAGE_PAL: Palette = Palette {
    bg: hex(0xf7f8f4),
    panel: hex(0xeff1ea),
    chrome: hex(0xe9ece3),
    raised: hex(0xfdfdfb),
    hover: hex(0xe3e7dc),
    pressed: hex(0xd8ddcf),
    menu_hover: hex(0xeceee6),
    border: hex(0xd0d6c6),
    line: hex(0xdfe3d7),
    raised_line: hex(0xdfe3d7),
    hdr: hex(0xedf0e8),
    text: hex(0x333a30),
    bright: hex(0x141a12),
    muted: hex(0x606a5b),
    faint: hex(0x9aa292),
    primary: hex(0x2f7a5f),
    on_primary: hex(0xffffff),
    sel: hex(0xe4efe7),
    sel_hover: hex(0xdae9df),
    alert: hex(0xb83a2e),
    success: hex(0x2f7a3a),
    warning: hex(0x9c6a00),
    c_string: hex(0x4d7c1f),
    c_number: hex(0x1f5fa0),
    c_true: hex(0x2f7a3a),
    c_false: hex(0xb83a2e),
    c_date: hex(0x0f766e),
    c_null: hex(0x8a9284),
    c_oid: hex(0x9a4f1a),
    c_obj: hex(0x606a5b),
    key: hex(0x333a30),
    c_link: hex(0x1f6a9a),
    highlight: hl(0x9c6a00, 51),
    c_keyword: hex(0x8b3fa8),
    engine_pg: hex(0x0f766e),
    stripe: hex(0xf2f4ee),
    track: hex(0xc6ccbb),
    well: hex(0xfcfcfa),
};

/// Deep forest green with a mint accent.
const GROVE_PAL: Palette = Palette {
    bg: hex(0x111813),
    panel: hex(0x172019),
    chrome: hex(0x141c16),
    raised: hex(0x212b23),
    hover: hex(0x1d261f),
    pressed: hex(0x243026),
    menu_hover: hex(0x2b372d),
    border: hex(0x2b362d),
    line: hex(0x212b23),
    raised_line: hex(0x303c32),
    hdr: hex(0x172019),
    text: hex(0xc4d0c6),
    bright: hex(0xe6efe8),
    muted: hex(0x869488),
    faint: hex(0x5f6d62),
    primary: hex(0x4fd69c),
    on_primary: hex(0x06170e),
    sel: hex(0x1c3a2c),
    sel_hover: hex(0x1f4433),
    alert: hex(0xff6b6b),
    success: hex(0x6fd88f),
    warning: hex(0xe9b949),
    c_string: hex(0xa3d977),
    c_number: hex(0xf2a86b),
    c_true: hex(0x6fd88f),
    c_false: hex(0xff6b6b),
    c_date: hex(0x7cc4f0),
    c_null: hex(0x6f7d72),
    c_oid: hex(0xd7a0e0),
    c_obj: hex(0x869488),
    key: hex(0xc4d0c6),
    c_link: hex(0x7cc4f0),
    highlight: hl(0xe9b949, 64),
    c_keyword: hex(0xb9a6ff),
    engine_pg: hex(0x6fd3e0),
    stripe: hex(0x141c16),
    track: hex(0x33433a),
    well: hex(0x0d130f),
};

/// Warm neutral graphite with a teal accent.
const ASH_PAL: Palette = Palette {
    bg: hex(0x1c1b1a),
    panel: hex(0x242322),
    chrome: hex(0x201f1e),
    raised: hex(0x2f2d2c),
    hover: hex(0x2a2928),
    pressed: hex(0x333130),
    menu_hover: hex(0x3b3938),
    border: hex(0x3d3b39),
    line: hex(0x2f2d2c),
    raised_line: hex(0x45423f),
    hdr: hex(0x242322),
    text: hex(0xcfcac4),
    bright: hex(0xf0ece7),
    muted: hex(0x96908a),
    faint: hex(0x6c6762),
    primary: hex(0x2ec4b0),
    on_primary: hex(0x04181a),
    sel: hex(0x1f3935),
    sel_hover: hex(0x20423d),
    alert: hex(0xf2716b),
    success: hex(0x7cc77f),
    warning: hex(0xe3ac54),
    c_string: hex(0xa8c97f),
    c_number: hex(0xe9a26c),
    c_true: hex(0x7cc77f),
    c_false: hex(0xf2716b),
    c_date: hex(0x86b4e8),
    c_null: hex(0x85807a),
    c_oid: hex(0xd49ac2),
    c_obj: hex(0x96908a),
    key: hex(0xcfcac4),
    c_link: hex(0x5fd0c0),
    highlight: hl(0xe3ac54, 64),
    c_keyword: hex(0xc9a0e8),
    engine_pg: hex(0x5fd0c0),
    stripe: hex(0x201f1e),
    track: hex(0x4a4744),
    well: hex(0x171615),
};

/// Blush white with a raspberry accent.
const BLOSSOM_PAL: Palette = Palette {
    bg: hex(0xfdf8f8),
    panel: hex(0xf8f0f1),
    chrome: hex(0xf4eaec),
    raised: hex(0xffffff),
    hover: hex(0xf0e3e6),
    pressed: hex(0xe8d8dc),
    menu_hover: hex(0xf6ecee),
    border: hex(0xe3d2d6),
    line: hex(0xeee1e4),
    raised_line: hex(0xeee1e4),
    hdr: hex(0xf6eef0),
    text: hex(0x3d3236),
    bright: hex(0x1c1316),
    muted: hex(0x6f6166),
    faint: hex(0xa6969b),
    primary: hex(0xc2255c),
    on_primary: hex(0xffffff),
    sel: hex(0xf8e7ec),
    sel_hover: hex(0xf6dfe5),
    alert: hex(0xc92a2a),
    success: hex(0x2b8a3e),
    warning: hex(0xa86a00),
    c_string: hex(0x2f7d32),
    c_number: hex(0x1d5fa8),
    c_true: hex(0x267a37),
    c_false: hex(0xc92a2a),
    c_date: hex(0x0b7285),
    c_null: hex(0x9a8c91),
    c_oid: hex(0x9a5b13),
    c_obj: hex(0x6f6166),
    key: hex(0x3d3236),
    c_link: hex(0xa61e4d),
    highlight: hl(0xa86a00, 51),
    c_keyword: hex(0x7b3fb5),
    engine_pg: hex(0x0b7285),
    stripe: hex(0xfbf4f5),
    track: hex(0xdccbcf),
    well: hex(0xffffff),
};

/// Lavender mist with a violet accent.
const LILAC_PAL: Palette = Palette {
    bg: hex(0xfaf9fd),
    panel: hex(0xf3f1fa),
    chrome: hex(0xeeebf7),
    raised: hex(0xffffff),
    hover: hex(0xe9e5f4),
    pressed: hex(0xdfd9ee),
    menu_hover: hex(0xf1eef9),
    border: hex(0xd9d3ea),
    line: hex(0xe6e2f2),
    raised_line: hex(0xe6e2f2),
    hdr: hex(0xf2f0f9),
    text: hex(0x33304a),
    bright: hex(0x15121f),
    muted: hex(0x645f7d),
    faint: hex(0x9d98b3),
    primary: hex(0x6741d9),
    on_primary: hex(0xffffff),
    sel: hex(0xeeeafa),
    sel_hover: hex(0xe8e3f9),
    alert: hex(0xc92a3a),
    success: hex(0x2b8a3e),
    warning: hex(0xa86a00),
    c_string: hex(0x267a37),
    c_number: hex(0x0b6e99),
    c_true: hex(0x267a37),
    c_false: hex(0xc92a3a),
    c_date: hex(0x087f5b),
    c_null: hex(0x918ca6),
    c_oid: hex(0xa35400),
    c_obj: hex(0x645f7d),
    key: hex(0x33304a),
    c_link: hex(0x5f3dc4),
    highlight: hl(0xa86a00, 51),
    c_keyword: hex(0x9c36b5),
    engine_pg: hex(0x0b7285),
    stripe: hex(0xf5f3fb),
    track: hex(0xcdc6e0),
    well: hex(0xffffff),
};

/// High contrast light: pure white, black text and outlines that stand out. Carbon's daytime twin.
const CHALK_PAL: Palette = Palette {
    bg: hex(0xffffff),
    panel: hex(0xf5f5f5),
    chrome: hex(0xefefef),
    raised: hex(0xffffff),
    hover: hex(0xe8e8e8),
    pressed: hex(0xdcdcdc),
    menu_hover: hex(0xececec),
    border: hex(0x767676),
    line: hex(0xd0d0d0),
    raised_line: hex(0xbdbdbd),
    hdr: hex(0xf0f0f0),
    text: hex(0x111111),
    bright: hex(0x000000),
    muted: hex(0x3d3d3d),
    faint: hex(0x5c5c5c),
    primary: hex(0x0047b3),
    on_primary: hex(0xffffff),
    sel: hex(0xd9e3f4),
    sel_hover: hex(0xccdaf0),
    alert: hex(0xb00020),
    success: hex(0x00701a),
    warning: hex(0x8a5300),
    c_string: hex(0x00701a),
    c_number: hex(0x005f73),
    c_true: hex(0x00701a),
    c_false: hex(0xb00020),
    c_date: hex(0x7a1fa2),
    c_null: hex(0x555555),
    c_oid: hex(0x8a3b00),
    c_obj: hex(0x3d3d3d),
    key: hex(0x111111),
    c_link: hex(0x0047b3),
    highlight: hl(0xffc400, 90),
    c_keyword: hex(0xa3005c),
    engine_pg: hex(0x005f73),
    stripe: hex(0xf6f6f6),
    track: hex(0x8f8f8f),
    well: hex(0xffffff),
};

/// Catppuccin flavors (MIT, catppuccin.com) mapped onto Palette: base, mantle and crust are the surfaces, mauve is the accent. Hover and raised fills are stepped apart where the palette has no shade between them.
const MOCHA_PAL: Palette = Palette {
    bg: hex(0x1e1e2e),
    panel: hex(0x181825),
    chrome: hex(0x11111b),
    raised: hex(0x313244),
    hover: hex(0x292a3b),
    pressed: hex(0x45475a),
    menu_hover: hex(0x45475a),
    border: hex(0x45475a),
    line: hex(0x313244),
    raised_line: hex(0x45475a),
    hdr: hex(0x181825),
    text: hex(0xcdd6f4),
    bright: hex(0xe1e6f8),
    muted: hex(0xa6adc8),
    faint: hex(0x6c7086),
    primary: hex(0xcba6f7),
    on_primary: hex(0x11111b),
    sel: hex(0x413956),
    sel_hover: hex(0x4b4162),
    alert: hex(0xf38ba8),
    success: hex(0xa6e3a1),
    warning: hex(0xf9e2af),
    c_string: hex(0xa6e3a1),
    c_number: hex(0xfab387),
    c_true: hex(0xa6e3a1),
    c_false: hex(0xf38ba8),
    c_date: hex(0x74c7ec),
    c_null: hex(0x7f849c),
    c_oid: hex(0xf5c2e7),
    c_obj: hex(0x9399b2),
    key: hex(0xcdd6f4),
    c_link: hex(0x89b4fa),
    highlight: hl(0xf9e2af, 64),
    c_keyword: hex(0xcba6f7),
    engine_pg: hex(0x94e2d5),
    stripe: hex(0x1b1b2a),
    track: hex(0x585b70),
    well: hex(0x11111b),
};

const MACCHIATO_PAL: Palette = Palette {
    bg: hex(0x24273a),
    panel: hex(0x1e2030),
    chrome: hex(0x181926),
    raised: hex(0x363a4f),
    hover: hex(0x2f3247),
    pressed: hex(0x494d64),
    menu_hover: hex(0x494d64),
    border: hex(0x494d64),
    line: hex(0x363a4f),
    raised_line: hex(0x494d64),
    hdr: hex(0x1e2030),
    text: hex(0xcad3f5),
    bright: hex(0xdfe5f9),
    muted: hex(0xa5adcb),
    faint: hex(0x6e738d),
    primary: hex(0xc6a0f6),
    on_primary: hex(0x181926),
    sel: hex(0x443f60),
    sel_hover: hex(0x4e466b),
    alert: hex(0xed8796),
    success: hex(0xa6da95),
    warning: hex(0xeed49f),
    c_string: hex(0xa6da95),
    c_number: hex(0xf5a97f),
    c_true: hex(0xa6da95),
    c_false: hex(0xed8796),
    c_date: hex(0x7dc4e4),
    c_null: hex(0x8087a2),
    c_oid: hex(0xf5bde6),
    c_obj: hex(0x939ab7),
    key: hex(0xcad3f5),
    c_link: hex(0x8aadf4),
    highlight: hl(0xeed49f, 64),
    c_keyword: hex(0xc6a0f6),
    engine_pg: hex(0x8bd5ca),
    stripe: hex(0x212435),
    track: hex(0x5b6078),
    well: hex(0x181926),
};

const FRAPPE_PAL: Palette = Palette {
    bg: hex(0x303446),
    panel: hex(0x292c3c),
    chrome: hex(0x232634),
    raised: hex(0x414559),
    hover: hex(0x3a3e51),
    pressed: hex(0x51576d),
    menu_hover: hex(0x51576d),
    border: hex(0x51576d),
    line: hex(0x414559),
    raised_line: hex(0x51576d),
    hdr: hex(0x292c3c),
    text: hex(0xc6d0f5),
    bright: hex(0xdde3f9),
    muted: hex(0xa5adce),
    faint: hex(0x737994),
    primary: hex(0xca9ee6),
    on_primary: hex(0x232634),
    sel: hex(0x4f4966),
    sel_hover: hex(0x585070),
    alert: hex(0xe78284),
    success: hex(0xa6d189),
    warning: hex(0xe5c890),
    c_string: hex(0xa6d189),
    c_number: hex(0xef9f76),
    c_true: hex(0xa6d189),
    c_false: hex(0xe78284),
    c_date: hex(0x85c1dc),
    c_null: hex(0x838ba7),
    c_oid: hex(0xf4b8e4),
    c_obj: hex(0x949cbb),
    key: hex(0xc6d0f5),
    c_link: hex(0x8caaee),
    highlight: hl(0xe5c890, 64),
    c_keyword: hex(0xca9ee6),
    engine_pg: hex(0x81c8be),
    stripe: hex(0x2c3041),
    track: hex(0x626880),
    well: hex(0x232634),
};

/// Catppuccin Latte keeps its own pastel data colors, which sit under 4.5:1 on its base. Raised surfaces and wells are lifted toward white so dialogs stand off the page.
const LATTE_PAL: Palette = Palette {
    bg: hex(0xeff1f5),
    panel: hex(0xe6e9ef),
    chrome: hex(0xdce0e8),
    raised: hex(0xf9f9fb),
    hover: hex(0xccd0da),
    pressed: hex(0xbcc0cc),
    menu_hover: hex(0xd9dce4),
    border: hex(0xbcc0cc),
    line: hex(0xccd0da),
    raised_line: hex(0xccd0da),
    hdr: hex(0xe6e9ef),
    text: hex(0x4c4f69),
    bright: hex(0x2e2f3f),
    muted: hex(0x6c6f85),
    faint: hex(0x9ca0b0),
    primary: hex(0x8839ef),
    on_primary: hex(0xeff1f5),
    sel: hex(0xe3dbf4),
    sel_hover: hex(0xdcd0f4),
    alert: hex(0xd20f39),
    success: hex(0x40a02b),
    warning: hex(0xdf8e1d),
    c_string: hex(0x40a02b),
    c_number: hex(0xfe640b),
    c_true: hex(0x40a02b),
    c_false: hex(0xd20f39),
    c_date: hex(0x209fb5),
    c_null: hex(0x8c8fa1),
    c_oid: hex(0xea76cb),
    c_obj: hex(0x7c7f93),
    key: hex(0x4c4f69),
    c_link: hex(0x1e66f5),
    highlight: hl(0xdf8e1d, 51),
    c_keyword: hex(0x8839ef),
    engine_pg: hex(0x179299),
    stripe: hex(0xeaedf2),
    track: hex(0xacb0be),
    well: hex(0xf9f9fb),
};
pub(crate) struct Theme {
    pub(crate) name: &'static str,
    pal: Palette,
    /// The Soft background. Workbench and Paper have hand-tuned ones; the others derive theirs.
    soft: Option<Palette>,
}

/// Which list a theme is in is its kind. The first of each is the default.
pub(crate) static DARK_THEMES: [Theme; 10] = [
    Theme { name: "Workbench", pal: DARK_PAL, soft: Some(DARK_SOFT_PAL) },
    Theme { name: "Midnight", pal: MIDNIGHT_PAL, soft: None },
    Theme { name: "Ember", pal: EMBER_PAL, soft: None },
    Theme { name: "Dusk", pal: DUSK_PAL, soft: None },
    Theme { name: "Carbon", pal: CARBON_PAL, soft: None },
    Theme { name: "Grove", pal: GROVE_PAL, soft: None },
    Theme { name: "Ash", pal: ASH_PAL, soft: None },
    Theme { name: "Mocha", pal: MOCHA_PAL, soft: None },
    Theme { name: "Macchiato", pal: MACCHIATO_PAL, soft: None },
    Theme { name: "Frappé", pal: FRAPPE_PAL, soft: None },
];
pub(crate) static LIGHT_THEMES: [Theme; 7] = [
    Theme { name: "Paper", pal: LIGHT_PAL, soft: Some(LIGHT_SOFT_PAL) },
    Theme { name: "Glacier", pal: GLACIER_PAL, soft: None },
    Theme { name: "Sage", pal: SAGE_PAL, soft: None },
    Theme { name: "Blossom", pal: BLOSSOM_PAL, soft: None },
    Theme { name: "Lilac", pal: LILAC_PAL, soft: None },
    Theme { name: "Chalk", pal: CHALK_PAL, soft: None },
    Theme { name: "Latte", pal: LATTE_PAL, soft: None },
];

pub(crate) fn themes(light: bool) -> &'static [Theme] {
    if light { &LIGHT_THEMES } else { &DARK_THEMES }
}

/// The theme named `name` among that kind, or the default, so an unknown name in ui.json still loads.
pub(crate) fn theme_index(name: &str, light: bool) -> usize {
    themes(light).iter().position(|t| t.name == name).unwrap_or(0)
}

/// Less contrast the way the hand-tuned Soft palettes do it: the page moves toward the panel, the surfaces on it
/// follow by a third of that step, and text moves toward muted. Moving every surface by an offset rather than a
/// mix keeps the gaps between them, so stripes and hovers stay visible.
fn soften(p: &Palette) -> Palette {
    let bg = p.bg.lerp_to_gamma(p.panel, 0.45);
    let d = [bg.r() as i16 - p.bg.r() as i16, bg.g() as i16 - p.bg.g() as i16, bg.b() as i16 - p.bg.b() as i16];
    let by = |c: Color32, k: i16| {
        let ch = |v: u8, d: i16| (v as i16 + d / k).clamp(0, 255) as u8;
        Color32::from_rgb(ch(c.r(), d[0]), ch(c.g(), d[1]), ch(c.b(), d[2]))
    };
    Palette {
        bg,
        well: by(p.well, 1),
        stripe: by(p.stripe, 1),
        panel: by(p.panel, 3),
        chrome: by(p.chrome, 3),
        hdr: by(p.hdr, 3),
        hover: by(p.hover, 3),
        pressed: by(p.pressed, 3),
        line: by(p.line, 3),
        border: by(p.border, 3),
        text: p.text.lerp_to_gamma(p.muted, 0.15),
        bright: p.bright.lerp_to_gamma(p.text, 0.35),
        ..*p
    }
}

/// Every palette a choice in Settings can make, for the dark and the light themes: each theme, Standard then Soft,
/// with its own accent then each of ACCENTS. Built once so `pal()` stays a lock-free lookup on the drawing path.
static PALS: std::sync::LazyLock<[Vec<Palette>; 2]> = std::sync::LazyLock::new(|| {
    [false, true].map(|l| {
        let mut v = vec![DARK_PAL; pal_index(themes(l).len(), false, 0)];
        for (i, t) in themes(l).iter().enumerate() {
            for (soft, p) in [(false, t.pal), (true, t.soft.unwrap_or_else(|| soften(&t.pal)))] {
                for a in 0..=ACCENTS.len() {
                    v[pal_index(i, soft, a)] = with_accent(&p, a, l);
                }
            }
        }
        v
    })
});

/// Where theme `i` with Soft and accent `accent` (0 for the theme's own) sits in its kind's PALS.
fn pal_index(i: usize, soft: bool, accent: usize) -> usize {
    (i * 2 + soft as usize) * (ACCENTS.len() + 1) + accent
}

pub(crate) fn palette_of(light: bool, i: usize, soft: bool, accent: usize) -> &'static Palette {
    &PALS[light as usize][pal_index(i, soft, accent)]
}

/// WCAG contrast ratio of two colors, 1 to 21.
fn contrast(a: Color32, b: Color32) -> f32 {
    let l = |c: Color32| {
        let c = egui::Rgba::from(c);
        0.2126 * c.r() + 0.7152 * c.g() + 0.0722 * c.b() + 0.05
    };
    let (x, y) = (l(a), l(b));
    x.max(y) / x.min(y)
}

/// Accents that replace each theme's own: name, the shade on dark themes, the shade on light ones. The light shades
/// are deeper so the accent still reads on white.
pub(crate) const ACCENTS: [(&str, u32, u32); 6] = [
    ("Blue", 0x3b82f6, 0x1d5fd1),
    ("Cyan", 0x22c3ee, 0x0b7ea0),
    ("Green", 0x22b573, 0x1e7f4a),
    ("Orange", 0xf97316, 0xc2410c),
    ("Pink", 0xf472b6, 0xc0267a),
    ("Violet", 0xa78bfa, 0x6d3fd9),
];

/// Colors a connection group can take: name, the shade on dark themes, the shade on light ones. Stored by name in
/// groups.json, so reordering the list or changing a shade never changes what a group was saved with.
pub(crate) const GROUP_COLORS: [(&str, u32, u32); 8] = [
    ("red", 0xef6a6a, 0xc43d3d),
    ("amber", 0xf2b035, 0xb7791f),
    ("green", 0x3fc27a, 0x2a8a52),
    ("teal", 0x2fc4b8, 0x1b8a82),
    ("blue", 0x5b9cf6, 0x2f6fd0),
    ("purple", 0xa98bf5, 0x7449d6),
    ("pink", 0xf07cb8, 0xc22f80),
    ("gray", 0x8b93a1, 0x6b7280),
];

/// The color named in a group, in the shade of the current mode. An unknown name, such as one from a newer build, is gray.
#[allow(dead_code)]
pub(crate) fn group_color(name: &str) -> Color32 {
    let (_, dark, light) = GROUP_COLORS.iter().find(|c| c.0 == name).unwrap_or(&GROUP_COLORS[GROUP_COLORS.len() - 1]);
    hex(if is_light() { *light } else { *dark })
}

/// 0 keeps the theme's accent; i picks ACCENTS[i - 1]. An unknown name keeps the theme's.
pub(crate) fn accent_index(name: &str) -> usize {
    ACCENTS.iter().position(|a| a.0 == name).map_or(0, |i| i + 1)
}

/// The inverse of `accent_index`: "Theme" for the theme's own, which `accent_index` reads back as 0.
pub(crate) fn accent_name(i: usize) -> &'static str {
    if i == 0 { "Theme" } else { ACCENTS[i - 1].0 }
}

/// `p` with accent `i` in place of its own. The accent also draws focus rings and accent text, so it is moved away
/// from bg until it reaches 4.5:1 there (Blue on Frappé starts at 3.4:1). The selection is the accent over bg, and
/// text on the accent is white only where white reaches 4.5:1, dark otherwise.
fn with_accent(p: &Palette, i: usize, light: bool) -> Palette {
    if i == 0 {
        return *p;
    }
    let (_, dark_shade, light_shade) = ACCENTS[i - 1];
    let mut a = hex(if light { light_shade } else { dark_shade });
    let away = if light { Color32::BLACK } else { Color32::WHITE };
    for _ in 0..20 {
        if contrast(a, p.bg) >= 4.5 {
            break;
        }
        a = a.lerp_to_gamma(away, 0.05);
    }
    let (t0, t1) = if light { (0.1, 0.15) } else { (0.2, 0.26) };
    let on = if contrast(a, Color32::WHITE) >= 4.5 { Color32::WHITE } else { hex(0x0b0d12) };
    Palette { primary: a, on_primary: on, sel: p.bg.lerp_to_gamma(a, t0), sel_hover: p.bg.lerp_to_gamma(a, t1), ..*p }
}

/// Set at the start of each frame from egui's resolved theme.
pub(crate) static LIGHT_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn is_light() -> bool {
    LIGHT_MODE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Display choices from Settings, mirrored in statics because free functions draw cells and rows.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Look {
    pub(crate) scale: f32,
    /// Indexes into DARK_THEMES, LIGHT_THEMES, UI_FONTS and DATA_FONTS.
    pub(crate) dark_theme: usize,
    pub(crate) light_theme: usize,
    pub(crate) ui_font: usize,
    pub(crate) data_font: usize,
    pub(crate) soft: bool,
    /// 0 for each theme's own, else an index into ACCENTS plus one.
    pub(crate) accent: usize,
    pub(crate) reading_line: bool,
    pub(crate) group_digits: bool,
}

impl Default for Look {
    fn default() -> Self {
        Self { scale: 1.0, dark_theme: 0, light_theme: 0, ui_font: 0, data_font: 0, soft: false, accent: 0, reading_line: false, group_digits: false }
    }
}

/// Where the dark and the light palette in use sit in PALS, indexed by `is_light()`.
static CUR: [std::sync::atomic::AtomicUsize; 2] = [std::sync::atomic::AtomicUsize::new(0), std::sync::atomic::AtomicUsize::new(0)];
static READING_LINE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static GROUP_DIGITS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static LOOK_GEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Changes each time the fonts, colors or number display change, for caches of laid-out text.
pub(crate) fn look_gen() -> u32 {
    LOOK_GEN.load(std::sync::atomic::Ordering::Relaxed)
}

/// The whole row under the pointer stands out, so the eye keeps its place across a wide table.
pub(crate) fn reading_line() -> bool {
    READING_LINE.load(std::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn group_digits() -> bool {
    GROUP_DIGITS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Interface sizes; 100 % is the designed size.
pub(crate) const SCALES: [(f32, &str); 4] = [(1.0, "100%"), (1.1, "110%"), (1.25, "125%"), (1.5, "150%")];

/// Dialogs and floating windows: a deeper shadow on the dark theme, where a light one vanishes.
pub(crate) fn window_shadow() -> egui::Shadow {
    egui::Shadow { offset: [0, 6], blur: 20, spread: 0, color: Color32::from_black_alpha(if is_light() { 30 } else { 90 }) }
}

pub(crate) fn pal() -> &'static Palette {
    let light = is_light();
    &PALS[light as usize][CUR[light as usize].load(std::sync::atomic::Ordering::Relaxed)]
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

colors!(BG: bg, PANEL: panel, CHROME: chrome, RAISED: raised, HOVER: hover, PRESSED: pressed, MENU_HOVER: menu_hover, BORDER: border, LINE: line, RAISED_LINE: raised_line, HDR: hdr, ON_PRIMARY: on_primary, SEL: sel, SEL_HOVER: sel_hover, TEXT: text, BRIGHT: bright, MUTED: muted, FAINT: faint, PRIMARY: primary, ALERT: alert, SUCCESS: success, WARNING: warning, C_STRING: c_string, C_NUMBER: c_number, C_TRUE: c_true, C_FALSE: c_false, C_DATE: c_date, C_NULL: c_null, C_OID: c_oid, C_OBJ: c_obj, KEY: key, C_LINK: c_link, HIGHLIGHT: highlight, C_KEYWORD: c_keyword, ENGINE_PG: engine_pg, WELL: well, TRACK: track);

/// `c` at `alpha` out of 255, for tints over the background.
pub(crate) fn tint(c: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha)
}

pub(crate) const SIDEBAR_W: f32 = 250.0;

// Type scale: every text size in the app is one of these.
/// Small caps headers over a sidebar panel or a group ("DATABASE EXPLORER", "LIBRARY") and the faint counts on tree rows.
pub(crate) const TXT_SECTION: f32 = 10.5;

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
/// The square button at the end of an inline editor that opens the large editor.
pub(crate) const EXPAND_BTN: f32 = 20.0;
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
/// Vim's block caret, just off square like a terminal cursor.
pub(crate) const R_CARET: u8 = 1;
/// Padding inside the Vim mode badge, and its distance from the editor's corner.
pub(crate) const VIM_BADGE_PAD: f32 = 6.0;

pub(crate) const SIDE_ROW_H: f32 = 26.0;

// The PostgreSQL schema switcher under an open database: indented to the tree's children, `PICK_PAD` inside its edges,
// `PICK_GAP` between its label and the name, and `PICK_CHEVRON` of room on the right for the chevron.
pub(crate) const PICK_INDENT: f32 = 18.0;
pub(crate) const PICK_H: f32 = BUTTON_H + 2.0;
pub(crate) const PICK_PAD: f32 = 8.0;
pub(crate) const PICK_GAP: f32 = 6.0;
// The calendar under a date editor: its inner width, the row of weekday names and one row of days.
pub(crate) const DATE_PICK_W: f32 = 252.0;
pub(crate) const DATE_HEAD_H: f32 = 18.0;
pub(crate) const DATE_DAY_H: f32 = 28.0;
pub(crate) const PICK_CHEVRON: f32 = 26.0;

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
/// The message shown in a list with no matches.
pub(crate) const EMPTY_LIST_H: f32 = 64.0;
// The tab list behind the strip's "N tabs" button: width, filter row and connection header.
pub(crate) const TAB_LIST_W: f32 = 380.0;
pub(crate) const TAB_LIST_HEAD_H: f32 = 40.0;
pub(crate) const TAB_GROUP_H: f32 = 30.0;

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
pub(crate) fn style(ctx: &egui::Context, look: Look) {
    use std::sync::atomic::Ordering::Relaxed;
    READING_LINE.store(look.reading_line, Relaxed);
    GROUP_DIGITS.store(look.group_digits, Relaxed);
    for (l, i, theme) in [(false, look.dark_theme, egui::Theme::Dark), (true, look.light_theme, egui::Theme::Light)] {
        CUR[l as usize].store(pal_index(i, look.soft, look.accent), Relaxed);
        ctx.set_visuals_of(theme, visuals(theme, palette_of(l, i, look.soft, look.accent)));
    }
    // set_fonts compares whole font files, so it runs only when the font choice changes. The choice is kept
    // in the context, not in a static, because fonts belong to a context and tests build many.
    let id = egui::Id::unique("quarry-fonts");
    let pick = (look.ui_font, look.data_font);
    if ctx.data(|d| d.get_temp::<(usize, usize)>(id)) != Some(pick) {
        ctx.set_fonts(fonts(look.ui_font, look.data_font));
        ctx.data_mut(|d| d.insert_temp(id, pick));
    }
    LOOK_GEN.fetch_add(1, Relaxed);
    // egui's own Cmd+= zoom steps by 10 % and is not saved; App::zoom_keys steps through SCALES instead.
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
    ctx.set_zoom_factor(look.scale);
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

/// Interface fonts for Settings: name and file. Inter is first, the default and the fallback.
pub(crate) const UI_FONTS: [(&str, &[u8]); 3] = [
    ("Inter", include_bytes!("../../assets/fonts/InterVariable.ttf")),
    ("Geist", include_bytes!("../../assets/fonts/Geist-Variable.ttf")),
    ("OpenDyslexic", include_bytes!("../../assets/fonts/OpenDyslexic-Regular.otf")),
];
/// Data fonts. JetBrains Mono is first, the default and the fallback for glyphs the others lack.
pub(crate) const DATA_FONTS: [(&str, &[u8]); 4] = [
    ("JetBrains Mono", include_bytes!("../../assets/fonts/JetBrainsMono-Variable.ttf")),
    ("Geist Mono", include_bytes!("../../assets/fonts/GeistMono-Variable.ttf")),
    ("Cascadia Mono", include_bytes!("../../assets/fonts/CascadiaMono-Variable.ttf")),
    ("Fira Code", include_bytes!("../../assets/fonts/FiraCode-Variable.ttf")),
];

/// The index of the font named `name`, or 0 (the default) for an unknown name.
pub(crate) fn font_index(list: &[(&str, &[u8])], name: &str) -> usize {
    list.iter().position(|f| f.0 == name).unwrap_or(0)
}

/// The chosen interface and data fonts, each in front of the default one so glyphs it lacks still draw.
/// All but OpenDyslexic are variable, so one file covers every weight. egui's own fonts stay behind them
/// as fallbacks for icons and emoji. Inter's metrics center in buttons as they are, so it needs none of
/// the shift egui's Ubuntu font did. OpenDyslexic is for interface text only: it has no mono version, and
/// data columns need one. It has one weight, so medium and semibold draw as Regular.
fn fonts(ui_font: usize, data_font: usize) -> egui::FontDefinitions {
    use egui::{FontData, FontFamily, FontTweak, epaint::text::VariationCoords};
    let mut fonts = egui::FontDefinitions::default();
    // Without an explicit weight a variable font draws its default instance, which for
    // JetBrains Mono is not Regular. The tweak is harmless on OpenDyslexic, which has no axes.
    let regular = |bytes: &'static [u8]| std::sync::Arc::new(FontData::from_static(bytes).tweak(FontTweak { coords: VariationCoords::new([(b"wght", 400.0)]), ..Default::default() }));
    for (family, list, pick) in [(FontFamily::Proportional, &UI_FONTS[..], ui_font), (FontFamily::Monospace, &DATA_FONTS[..], data_font)] {
        let pick = pick.min(list.len() - 1);
        for &(name, bytes) in &[list[0], list[pick]][..1 + (pick != 0) as usize] {
            fonts.font_data.insert(name.into(), regular(bytes));
            fonts.families.entry(family.clone()).or_default().insert(0, name.into());
        }
    }
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

/// Side padding of a dialog's header, body and footer bands.
pub(crate) const DIALOG_PAD: f32 = 20.0;
/// The least room kept between the window's edge and a dialog, window, popover or menu. egui cannot draw outside the
/// window, so anything taller is capped to the window less this on both sides and scrolls inside.
pub(crate) const DIALOG_MARGIN: f32 = 24.0;
/// The shortest a capped dialog body or popover list gets, so a tiny window still shows a few rows of it.
pub(crate) const FIT_MIN_H: f32 = 64.0;
/// The tinted square holding a dialog header's glyph.
pub(crate) const HEAD_TILE: f32 = 34.0;
/// A form card row with only a label, and one with a hint under the label.
pub(crate) const CARD_ROW_H: f32 = 40.0;
pub(crate) const CARD_ROW_HINT_H: f32 = 52.0;
/// The label column of a form card row.
pub(crate) const CARD_LABEL_W: f32 = 170.0;
/// Label column width of dialog form grids, so fields in every section start at the same x.
pub(crate) const FORM_LABEL_W: f32 = 84.0;
/// The gap between a form's label column and its fields, which fields that span both columns must add back.
pub(crate) const FORM_GAP: f32 = 14.0;
/// Inner margin of a single-line form field, and of the taller connection string fields.
pub(crate) const FIELD_MARGIN: egui::Margin = egui::Margin::symmetric(8, 5);
pub(crate) const FIELD_MARGIN_TALL: egui::Margin = egui::Margin::symmetric(8, 6);
/// Radius of the filled dot beside a form's status line, and of the disc behind an info note's "i".
pub(crate) const STATUS_DOT_R: f32 = 3.0;
pub(crate) const INFO_DOT_R: f32 = 6.0;

pub(crate) const MONO_PX: f32 = TXT_BODY;

/// Width of the checkbox column in front of the row number.
pub(crate) const CHECK_W: f32 = 22.0;

/// Theme cards in Settings: the miniature window, the name under it and the check badge on the chosen one.
pub(crate) const THEME_CARD_MIN_W: f32 = 84.0;
pub(crate) const THEME_CARD_MAX_W: f32 = 110.0;
pub(crate) const THEME_THUMB_H: f32 = 58.0;
pub(crate) const THEME_LABEL_H: f32 = 24.0;
pub(crate) const THEME_BADGE_R: f32 = 7.0;
/// Settings > Appearance > Accent: a swatch and the square cell that holds it with its ring.
pub(crate) const ACCENT_DOT_R: f32 = 9.0;
pub(crate) const ACCENT_CELL: f32 = 30.0;

pub(crate) fn mono(c: Color32) -> TextFormat {
    TextFormat { font_id: FontId::monospace(MONO_PX), color: c, ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accent_overrides_stay_readable_in_every_theme() {
        for light in [false, true] {
            for (i, t) in themes(light).iter().enumerate() {
                for soft in [false, true] {
                    // Workbench and Paper keep their original white-on-accent buttons, at 4.3:1 and 3.6:1.
                    for a in (i == 0) as usize..=ACCENTS.len() {
                        let p = palette_of(light, i, soft, a);
                        assert!(contrast(p.primary, p.on_primary) >= 4.5, "text on {} with accent {a}", t.name);
                        if a > 0 {
                            assert!(contrast(p.primary, p.bg) >= 4.5, "accent {a} on {}", t.name);
                        }
                    }
                }
            }
        }
        assert_eq!((accent_index("Pink"), accent_index(""), accent_index("Teal")), (5, 0, 0));
    }
}
