use crate::db::{self, Msg, PAGE_SIZE, Query, ShellKind, Target, UndoAction, UndoKind, Worker};
use eframe::egui::{self, Color32, CornerRadius, FontId, Key, RichText, Sense, Stroke, TextFormat, text::LayoutJob};
use mongodb::Client;
use mongodb::bson::{Bson, Document, doc, oid::ObjectId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// Colors come from the palette of the theme in use, so they are functions, not constants.
// The dark one is the original's `.dark-mode` theme.
struct Palette {
    bg: Color32,
    panel: Color32,
    raised: Color32,
    hover: Color32,
    border: Color32,
    text: Color32,
    bright: Color32,
    muted: Color32,
    faint: Color32,
    primary: Color32,
    alert: Color32,
    success: Color32,
    warning: Color32,
    c_string: Color32,
    c_number: Color32,
    c_true: Color32,
    c_false: Color32,
    c_date: Color32,
    c_null: Color32,
    c_oid: Color32,
    c_obj: Color32,
    key: Color32,
    c_link: Color32,
    highlight: Color32,
    c_keyword: Color32,
    /// Alternate table rows.
    stripe: Color32,
    /// An off switch.
    track: Color32,
    faint_bg: Color32,
    /// Inputs and control troughs. In light mode the page white is too close to cards to use.
    well: Color32,
}

const DARK_PAL: Palette = Palette {
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
const LIGHT_PAL: Palette = Palette {
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
static LIGHT_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn pal() -> &'static Palette {
    if LIGHT_MODE.load(std::sync::atomic::Ordering::Relaxed) { &LIGHT_PAL } else { &DARK_PAL }
}

macro_rules! colors {
    ($($name:ident: $field:ident),* $(,)?) => {
        $(
            #[allow(non_snake_case)]
            fn $name() -> Color32 {
                pal().$field
            }
        )*
    };
}

colors!(BG: bg, PANEL: panel, RAISED: raised, HOVER: hover, BORDER: border, TEXT: text, BRIGHT: bright, MUTED: muted, FAINT: faint, PRIMARY: primary, ALERT: alert, SUCCESS: success, WARNING: warning, C_STRING: c_string, C_NUMBER: c_number, C_TRUE: c_true, C_FALSE: c_false, C_DATE: c_date, C_NULL: c_null, C_OID: c_oid, C_OBJ: c_obj, KEY: key, C_LINK: c_link, HIGHLIGHT: highlight, C_KEYWORD: c_keyword);

/// `c` at `alpha` out of 255, for tints over the background.
fn tint(c: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha)
}
const SIDEBAR_W: f32 = 250.0;
const HEADER_H: f32 = 40.0;
const SYSTEM_DBS: [&str; 3] = ["admin", "config", "local"];
/// Columns shown before the user picks their own; drawing every field of a wide collection is slow.
const DEFAULT_COLS: usize = 8;
const ROW_H: f32 = 28.0;
const OPTIMAL_MIN_W: f32 = 40.0;
const OPTIMAL_MAX_W: f32 = 400.0;
const OPTIMAL_PAD: f32 = 10.0;
const CELL_PAD: f32 = 16.0;
const NUM_W: f32 = 40.0;
/// Fixed so an open editor never pushes its panel wider.
const EDITOR_W: f32 = 230.0;

/// Web links stand out from plain text. Hex ids stored as strings keep the string colour so the
/// colour always matches the type shown next to them.
#[derive(PartialEq)]
enum StrKind {
    Plain,
    Url,
}

fn str_kind(s: &str) -> StrKind {
    if (s.starts_with("http://") || s.starts_with("https://")) && !s.contains(char::is_whitespace) {
        StrKind::Url
    } else {
        StrKind::Plain
    }
}

fn color_of(b: &Bson) -> Color32 {
    match b {
        Bson::String(s) => match str_kind(s) {
            StrKind::Url => C_LINK(),
            StrKind::Plain if s.is_empty() => C_NULL(),
            StrKind::Plain => C_STRING(),
        },
        Bson::Double(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Decimal128(_) => C_NUMBER(),
        Bson::Boolean(true) => C_TRUE(),
        Bson::Boolean(false) => C_FALSE(),
        Bson::DateTime(_) | Bson::Timestamp(_) => C_DATE(),
        Bson::Null | Bson::Undefined => C_NULL(),
        Bson::ObjectId(_) => C_OID(),
        _ => C_OBJ(),
    }
}

fn leaf_text(b: &Bson) -> String {
    match b {
        Bson::String(s) => format!("\"{s}\""),
        Bson::ObjectId(o) => format!("ObjectId(\"{}\")", o.to_hex()),
        Bson::DateTime(t) => t.try_to_rfc3339_string().unwrap_or_else(|_| t.timestamp_millis().to_string()),
        Bson::Int64(n) => n.to_string(),
        Bson::Document(d) => format!("{{{} fields}}", d.len()),
        Bson::Array(a) => format!("[{} items]", a.len()),
        other => db::compact(other),
    }
}

/// The original's string display rule: lodash `_.truncate(value, {length: 100})`, so at most 100
/// characters including the trailing "..." (fieldTypes.string.toDisplayString, renderer line 30804).
const STRING_DISPLAY_MAX: usize = 100;

fn truncate_display(s: &str) -> String {
    if s.chars().count() <= STRING_DISPLAY_MAX {
        s.to_string()
    } else {
        s.chars().take(STRING_DISPLAY_MAX - 3).chain("...".chars()).collect()
    }
}

/// Colours JSON and shell-style query text by token, for every code editor in the app.
fn json_job(text: &str) -> LayoutJob {
    let fmt = |c: Color32| TextFormat { font_id: FontId::monospace(13.0), color: c, ..Default::default() };
    let next_is_colon = |rest: &str| rest.trim_start().starts_with(':');
    let mut job = LayoutJob::default();
    let mut i = 0;
    while i < text.len() {
        let c = text[i..].chars().next().unwrap();
        let start = i;
        i += c.len_utf8();
        let color = if c == '"' || c == '\'' {
            let mut esc = false;
            while i < text.len() {
                let ch = text[i..].chars().next().unwrap();
                i += ch.len_utf8();
                if esc {
                    esc = false;
                } else if ch == '\\' {
                    esc = true;
                } else if ch == c {
                    break;
                }
            }
            if next_is_colon(&text[i..]) {
                if text[start + 1..].starts_with('$') { PRIMARY() } else { KEY() }
            } else {
                C_STRING()
            }
        } else if c.is_ascii_digit() || (c == '-' && text[i..].starts_with(|d: char| d.is_ascii_digit())) {
            while i < text.len() && text[i..].starts_with(|d: char| d.is_ascii_alphanumeric() || d == '.' || d == '+' || d == '-') {
                i += 1;
            }
            C_NUMBER()
        } else if c.is_alphabetic() || c == '$' || c == '_' {
            while i < text.len() && text[i..].starts_with(|d: char| d.is_alphanumeric() || d == '$' || d == '_') {
                i += text[i..].chars().next().unwrap().len_utf8();
            }
            match &text[start..i] {
                "true" => C_TRUE(),
                "false" => C_FALSE(),
                "null" | "undefined" => C_NULL(),
                "ObjectId" => C_OID(),
                "ISODate" | "Date" | "new" => C_DATE(),
                "NumberLong" | "NumberInt" | "NumberDecimal" => C_NUMBER(),
                w if w.starts_with('$') => PRIMARY(),
                _ if next_is_colon(&text[i..]) => KEY(),
                _ => TEXT(),
            }
        } else if "{}[]()".contains(c) {
            C_OBJ()
        } else if ":,".contains(c) {
            FAINT()
        } else {
            TEXT()
        };
        job.append(&text[start..i], 0.0, fmt(color));
    }
    job
}

/// Closes brackets, parentheses and quotes as they are typed, and types over a closer that is
/// already there, so `{` gives `{|}` and typing `}` then just moves past it.
fn with_autopair(ui: &mut egui::Ui, id: egui::Id, text: &mut String, show: impl FnOnce(&mut egui::Ui, &mut String) -> egui::text_edit::TextEditOutput) -> egui::text_edit::TextEditOutput {
    let focused = ui.memory(|m| m.has_focus(id));
    let typed: Option<char> = if focused {
        ui.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Text(t) if t.chars().count() == 1 => t.chars().next(),
                _ => None,
            })
        })
    } else {
        None
    };
    let enter = focused && ui.input(|i| i.key_pressed(Key::Enter));
    let cursor = egui::TextEdit::load_state(ui.ctx(), id).and_then(|s| s.cursor.char_range()).filter(|r| r.is_empty()).map(|r| r.primary.index.0);
    if let (Some(c), Some(ci)) = (typed, cursor) {
        if ")]}\"'".contains(c) && text.chars().nth(ci) == Some(c) {
            ui.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Text(t) if t.chars().count() == 1)));
            if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), id) {
                st.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(ci + 1))));
                st.store(ui.ctx(), id);
            }
            return show(ui, text);
        }
    }
    let out = show(ui, text);
    let close = match typed {
        Some('{') => Some('}'),
        Some('[') => Some(']'),
        Some('(') => Some(')'),
        Some('"') => Some('"'),
        Some('\'') => Some('\''),
        _ => None,
    };
    if let (Some(close), Some(r), true) = (close, out.cursor_range, out.response.changed()) {
        let ci = r.primary.index.0;
        let next = text.chars().nth(ci);
        let prev = ci.checked_sub(2).and_then(|p| text.chars().nth(p));
        let fits = next.is_none_or(|n| n.is_whitespace() || ")]},:".contains(n));
        // A quote right after a word is more likely an apostrophe or the end of a string.
        let quote_ok = !matches!(close, '"' | '\'') || prev.is_none_or(|p| !p.is_alphanumeric());
        if fits && quote_ok {
            let byte = text.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(text.len());
            text.insert(byte, close);
            // Redraw this frame with the edit so it never shows half applied.
            ui.ctx().request_discard("autopair");
        }
    }
    // Auto indent: a new line keeps the indentation, goes one step deeper after an opening
    // bracket, and a closing bracket typed on an empty indented line steps back.
    if let (true, Some(r)) = (out.response.changed(), out.cursor_range) {
        let ci = r.primary.index.0;
        let fixed = if enter && ci > 0 && text.chars().nth(ci - 1) == Some('\n') {
            newline_indent(text, ci)
        } else if matches!(typed, Some('}' | ']' | ')')) {
            closer_dedent(text, ci)
        } else {
            None
        };
        if let Some((t2, c2)) = fixed {
            *text = t2;
            if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), id) {
                st.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(c2))));
                st.store(ui.ctx(), id);
            }
            ui.ctx().request_discard("auto indent");
        }
    }
    out
}

/// One indent step; the same two spaces the pretty-printed JSON uses.
const INDENT: &str = "  ";

fn byte_at(text: &str, ci: usize) -> usize {
    text.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(text.len())
}

/// After a newline was typed just before char `ci`: the text with the right indentation added
/// and where the caret goes.
fn newline_indent(text: &str, ci: usize) -> Option<(String, usize)> {
    let nl = byte_at(text, ci - 1);
    let line = &text[text[..nl].rfind('\n').map(|i| i + 1).unwrap_or(0)..nl];
    let indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
    let opener = line.trim_end().chars().last().filter(|c| "{[(".contains(*c));
    let rest = &text[nl + 1..];
    let next = rest.trim_start_matches([' ', '\t']).chars().next();
    let pair = |o: char| match o {
        '{' => '}',
        '[' => ']',
        _ => ')',
    };
    // Spaces that were after the caret on the old line would push the closer; drop them.
    let after = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    let (insert, caret) = match opener {
        Some(o) if next == Some(pair(o)) => (format!("{indent}{INDENT}\n{indent}"), indent.chars().count() + INDENT.len()),
        Some(_) => (format!("{indent}{INDENT}"), indent.chars().count() + INDENT.len()),
        None if indent.is_empty() => return None,
        None => (indent.clone(), indent.chars().count()),
    };
    let mut out = String::with_capacity(text.len() + insert.len());
    out.push_str(&text[..nl + 1]);
    out.push_str(&insert);
    out.push_str(&text[nl + 1 + after..]);
    Some((out, ci + caret))
}

/// After a closing bracket was typed just before char `ci`: one indent step less if the
/// bracket is the first thing on its line.
fn closer_dedent(text: &str, ci: usize) -> Option<(String, usize)> {
    let at = byte_at(text, ci - 1);
    let start = text[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let lead = &text[start..at];
    if lead.len() < INDENT.len() || !lead.chars().all(|c| c == ' ') {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..start]);
    out.push_str(&text[start + INDENT.len()..]);
    Some((out, ci - INDENT.len()))
}

#[derive(Clone, Copy, PartialEq)]
enum FieldKind {
    Query,
    Sort,
    Project,
}

/// One autocomplete suggestion: what is shown, a short note, what is inserted and where the
/// caret lands inside it (in characters).
struct Sug {
    text: String,
    note: &'static str,
    insert: String,
    caret: usize,
}

/// Open suggestion list of one query input.
#[derive(Default)]
struct Ac {
    id: Option<egui::Id>,
    /// Length in characters of the word being completed, just before `cursor`.
    token: usize,
    items: Vec<Sug>,
    sel: usize,
    cursor: usize,
    rect: Option<egui::Rect>,
}

impl ShellTab {
    fn set_value(&mut self, v: Bson) {
        self.docs = match &v {
            Bson::Array(a) if !a.is_empty() && a.iter().all(|x| matches!(x, Bson::Document(_))) => a.iter().filter_map(|x| x.as_document().cloned()).collect(),
            Bson::Document(d) => vec![d.clone()],
            _ => Vec::new(),
        };
        self.fields = auto_fields(&self.docs);
        self.count = match &v {
            Bson::Array(a) => Some(a.len()),
            _ => None,
        };
        self.page = 0;
        self.raw = self.docs.is_empty();
        self.raw_text = None;
        self.value = Some(v);
        self.error = None;
    }
}

impl Ac {
    fn apply(&mut self, ctx: &egui::Context, id: egui::Id, text: &mut String, i: usize) {
        let Some(s) = self.items.get(i) else { return };
        let start = self.cursor.saturating_sub(self.token);
        let byte = |c: usize| text.char_indices().nth(c).map(|(b, _)| b).unwrap_or(text.len());
        let (bs, be) = (byte(start), byte(self.cursor));
        text.replace_range(bs..be, &s.insert);
        if let Some(mut st) = egui::TextEdit::load_state(ctx, id) {
            st.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(start + s.caret))));
            st.store(ctx, id);
        }
        self.items.clear();
    }
}

/// Query operators offered inside a field's own object, as in the original (renderer line 95668).
const QUERY_OPS: [&str; 33] = [
    "$all: [  ]", "$and: [  ]", "$bitsAllClear: ", "$bitsAllSet: ", "$bitsAnyClear: ", "$bitsAnySet: ", "$comment: ",
    "$elemMatch: {  }", "$eq: ", "$exists: true", "$exists: false", "$expr: {  }", "$geoIntersects: {  }", "$geoWithin: {  }",
    "$gt: ", "$gte: ", "$in: [  ]", "$jsonSchema: ", "$lt: ", "$lte: ", "$meta: ", "$mod: [  ]", "$ne: ", "$near: {  }",
    "$nearSphere: {  }", "$nin: [  ]", "$nor: [  ]", "$not: ", "$or: [  ]", "$regex: ", "$size: ", "$text: ", "$type: ",
];

/// Every dotted path in the documents (array elements merged), capped so huge documents stay fast.
fn doc_paths(docs: &[Document]) -> Vec<String> {
    const MAX_PATHS: usize = 2000;
    const MAX_DEPTH: usize = 4;
    fn walk(d: &Document, prefix: &str, depth: usize, out: &mut Vec<String>, seen: &mut HashSet<String>) {
        for (k, v) in d {
            if out.len() >= MAX_PATHS {
                return;
            }
            let p = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
            if seen.insert(p.clone()) {
                out.push(p.clone());
            }
            if depth < MAX_DEPTH {
                match v {
                    Bson::Document(c) => walk(c, &p, depth + 1, out, seen),
                    Bson::Array(a) => {
                        for x in a.iter().take(20) {
                            if let Bson::Document(c) = x {
                                walk(c, &p, depth + 1, out, seen);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    let (mut out, mut seen) = (Vec::new(), HashSet::new());
    for d in docs {
        walk(d, "", 0, &mut out, &mut seen);
    }
    out
}

/// The original's loose match: the typed letters in order, anywhere (values: from the start).
fn fuzzy(token: &str, option: &str, anchored: bool) -> bool {
    let mut it = option.chars().map(|c| c.to_ascii_lowercase());
    for (n, t) in token.chars().map(|c| c.to_ascii_lowercase()).enumerate() {
        let found = if anchored && n == 0 { it.next() == Some(t) } else { it.any(|c| c == t) };
        if !found {
            return false;
        }
    }
    true
}

/// Whether a field name reads like a date (createdAt, syncedAt, startDate, timestamp), for fields
/// not in the loaded documents.
fn date_like(path: &str) -> bool {
    let last = path.rsplit('.').next().unwrap_or(path);
    let l = last.to_ascii_lowercase();
    (last.len() > 2 && last.ends_with("At")) || l.ends_with("date") || l.starts_with("date") || l.ends_with("time") || l.contains("timestamp")
}

/// Loaded fields whose values are dates, for date suggestions in the Query box.
fn date_paths(docs: &[Document], paths: &[String]) -> Vec<String> {
    paths.iter().filter(|p| docs.iter().take(50).any(|d| matches!(db::get_path(d, p), Some(Bson::DateTime(_))))).cloned().collect()
}

/// Suggestions for the text before the caret, following the original's setSuggestions
/// (renderer line 95645): after `{` or `,` field paths (operators when nested), after a dot the
/// sub-fields, after `:` values. Shortest first, at most 50.
fn suggest(before: &str, kind: FieldKind, paths: &[String], dates: &[String]) -> Option<(usize, Vec<Sug>)> {
    const MAX_SUGGESTIONS: usize = 50;
    let token_start = before.char_indices().rev().take_while(|(_, c)| c.is_ascii_alphanumeric() || "@#$._".contains(*c)).last().map(|(i, _)| i).unwrap_or(before.len());
    let token = &before[token_start..];
    let lead = &before[..token_start];
    let quoted = lead.ends_with(['"', '\'']);
    let lead = lead.trim_end_matches(['"', '\'']).trim_end();
    let caret_in = |s: &str| ["[  ]", "{  }", "(\"\")"].iter().find_map(|m| s.find(m).map(|i| s[..i].chars().count() + 2)).unwrap_or(s.chars().count());
    let field = |p: &String| {
        let insert = if quoted { p.clone() } else { format!("{p}: ") };
        Sug { text: p.clone(), note: "field", caret: insert.chars().count(), insert }
    };
    let (anchored, mut items): (bool, Vec<Sug>) = if token.contains('.') {
        let prefix = &token[..token.rfind('.').unwrap()];
        let kids = paths.iter().filter(|p| p.strip_prefix(prefix).and_then(|r| r.strip_prefix('.')).is_some_and(|r| !r.contains('.')));
        // The original lists at most 100 sub-fields after a dot (renderer line 95666).
        const MAX_CHILD_KEYS: usize = 100;
        (false, kids.take(MAX_CHILD_KEYS).map(field).collect())
    } else if lead.ends_with('{') || lead.ends_with(',') {
        if token.is_empty() {
            return None;
        }
        let depth = before.matches('{').count() as i64 - before.matches('}').count() as i64;
        if depth > 1 && kind == FieldKind::Query {
            (false, QUERY_OPS.iter().map(|o| Sug { text: o.trim_end().to_string(), note: "operator", insert: o.to_string(), caret: caret_in(o) }).collect())
        } else {
            (false, paths.iter().map(field).collect())
        }
    } else if lead.ends_with(':') && kind == FieldKind::Query && {
        // The field before the colon, quotes removed.
        let f = lead[..lead.len() - 1].trim_end().trim_end_matches(['"', '\'']);
        let f = &f[f.char_indices().rev().take_while(|(_, c)| c.is_ascii_alphanumeric() || "._$".contains(*c)).last().map_or(f.len(), |(i, _)| i)..];
        !f.is_empty() && (dates.iter().any(|d| d == f) || (!paths.iter().any(|p| p == f) && date_like(f)))
    } {
        // A date field lists date shortcuts and range templates as soon as the colon is typed.
        let now = mongodb::bson::DateTime::now().timestamp_millis();
        let mut v: Vec<Sug> = db::date_suggestions(now).into_iter().map(|(t, n)| Sug { caret: t.chars().count(), insert: t.clone(), text: t, note: n }).collect();
        v.push(Sug { text: "ISODate(\"\")".into(), note: "date", insert: "ISODate(\"\")".into(), caret: 9 });
        (true, v)
    } else if lead.ends_with(':') {
        let query_vals: Vec<(&str, &'static str)> = [("ObjectId(\"\")", "objectId"), ("ISODate(\"\")", "date"), ("true", "bool"), ("false", "bool"), ("null", "null")].into_iter().chain(db::DATE_SHORTCUTS).collect();
        let vals: &[(&str, &'static str)] = match kind {
            FieldKind::Query => &query_vals,
            FieldKind::Sort => &[("1", "ascending"), ("-1", "descending")],
            FieldKind::Project => &[("1", "include"), ("0", "exclude")],
        };
        if token.is_empty() && kind == FieldKind::Query {
            return None;
        }
        (true, vals.iter().map(|(v, n)| Sug { text: v.to_string(), note: n, insert: v.to_string(), caret: caret_in(v) }).collect())
    } else {
        return None;
    };
    items.retain(|s| fuzzy(token, &s.text, anchored));
    let mut seen = HashSet::new();
    items.retain(|s| seen.insert(s.text.clone()));
    // The original sorts by length only; words that start with what was typed come first here.
    let lower = token.to_ascii_lowercase();
    // Nothing typed yet keeps the list in its own order (the date list is ordered by use).
    if !token.is_empty() {
        items.sort_by_key(|s| (!s.text.to_ascii_lowercase().starts_with(&lower), s.text.len()));
    }
    items.truncate(MAX_SUGGESTIONS);
    (!items.is_empty()).then(|| (token.chars().count(), items))
}

/// A code input with brackets that close themselves and a suggestion list under the caret.
/// Up/Down move, Enter or Tab insert, Esc closes. Returns the editor output and whether a
/// suggestion was inserted this frame (so the caller does not also treat Enter as "run").
fn ac_field(
    ui: &mut egui::Ui,
    id: egui::Id,
    text: &mut String,
    ac: &mut Ac,
    compute: &dyn Fn(&str) -> Option<(usize, Vec<Sug>)>,
    show: impl FnOnce(&mut egui::Ui, &mut String, bool) -> egui::text_edit::TextEditOutput,
) -> (egui::text_edit::TextEditOutput, bool) {
    let open = ac.id == Some(id) && !ac.items.is_empty();
    let mut pick = None;
    let mut moved = false;
    if open && ui.memory(|m| m.has_focus(id)) {
        ui.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, Key::ArrowDown) {
                ac.sel = (ac.sel + 1).min(ac.items.len() - 1);
                moved = true;
            }
            if i.consume_key(egui::Modifiers::NONE, Key::ArrowUp) {
                ac.sel = ac.sel.saturating_sub(1);
                moved = true;
            }
            if i.consume_key(egui::Modifiers::NONE, Key::Enter) || i.consume_key(egui::Modifiers::NONE, Key::Tab) {
                pick = Some(ac.sel);
            }
            if i.consume_key(egui::Modifiers::NONE, Key::Escape) {
                ac.items.clear();
            }
        });
    }
    let out = with_autopair(ui, id, text, |ui, text| show(ui, text, open));
    let focused = out.response.has_focus();
    if let Some(i) = pick {
        ac.apply(ui.ctx(), id, text, i);
        return (out, true);
    }
    let cursor = out.cursor_range.filter(|r| r.is_empty()).map(|r| r.primary.index.0);
    if let (true, Some(ci)) = (focused, cursor) {
        if out.response.changed() {
            let before: String = text.chars().take(ci).collect();
            match compute(&before) {
                Some((token, items)) => *ac = Ac { id: Some(id), token, items, sel: 0, cursor: ci, rect: None },
                None if ac.id == Some(id) => ac.items.clear(),
                None => {}
            }
        } else if ac.id == Some(id) && ci != ac.cursor {
            ac.items.clear();
        }
    }
    let test_shot = std::env::var("QUARRY_TEST_AC").is_ok();
    let pointer_on_list = test_shot || ac.rect.zip(ui.input(|i| i.pointer.hover_pos())).is_some_and(|(r, p)| r.contains(p));
    if !focused && ac.id == Some(id) && !pointer_on_list {
        ac.items.clear();
    }
    let mut picked = false;
    if ac.id == Some(id) && !ac.items.is_empty() {
        let caret = out.galley.pos_from_cursor(egui::text::CCursor::new(ac.cursor));
        let pos = out.galley_pos + caret.left_bottom().to_vec2() + egui::vec2(-6.0, 4.0);
        let mut click = None;
        let r = egui::Area::new(id.with("ac")).order(egui::Order::Foreground).fixed_pos(pos).show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(240.0);
                egui::ScrollArea::vertical().max_height(230.0).show(ui, |ui| {
                    for (i, s) in ac.items.iter().enumerate() {
                        let mut job = LayoutJob::default();
                        job.append(&s.text, 0.0, mono(if s.text.starts_with('$') { PRIMARY() } else { BRIGHT() }));
                        job.append(&format!("   {}", s.note), 0.0, TextFormat { font_id: FontId::proportional(11.5), color: FAINT(), ..Default::default() });
                        let r = ui.selectable_label(i == ac.sel, job);
                        if i == ac.sel && moved {
                            r.scroll_to_me(None);
                        }
                        if r.clicked() {
                            click = Some(i);
                        }
                    }
                });
            });
        });
        ac.rect = Some(r.response.rect);
        if let Some(i) = click {
            ac.apply(ui.ctx(), id, text, i);
            ui.memory_mut(|m| m.request_focus(id));
            picked = true;
        }
    }
    (out, picked)
}

const JS_KEYWORDS: [&str; 30] = [
    "async", "await", "break", "case", "catch", "class", "const", "continue", "default", "delete", "do", "else", "export", "finally", "for",
    "function", "if", "import", "in", "instanceof", "let", "new", "of", "return", "switch", "throw", "try", "typeof", "var", "while",
];
/// Names the Shell provides (see the runner) plus common JavaScript built-ins.
const JS_GLOBALS: [&str; 18] = ["db", "ObjectId", "ISODate", "console", "log", "randomId", "UUIDv4", "UUIDv1", "_", "dayjs", "require", "Math", "JSON", "Date", "Promise", "Object", "Array", "Number"];
const DB_METHODS: [&str; 10] = ["collection", "listCollections", "command", "aggregate", "createCollection", "dropCollection", "renameCollection", "stats", "admin", "dropDatabase"];
const COLL_METHODS: [&str; 28] = [
    "find", "findOne", "aggregate", "countDocuments", "estimatedDocumentCount", "distinct", "insertOne", "insertMany", "updateOne", "updateMany",
    "replaceOne", "deleteOne", "deleteMany", "bulkWrite", "findOneAndUpdate", "findOneAndReplace", "findOneAndDelete", "createIndex", "createIndexes",
    "indexes", "listIndexes", "dropIndex", "dropIndexes", "drop", "rename", "watch", "indexExists", "options",
];
const CURSOR_METHODS: [&str; 15] = ["toArray", "limit", "skip", "sort", "project", "batchSize", "hint", "collation", "count", "next", "hasNext", "forEach", "map", "explain", "close"];
const CONSOLE_METHODS: [&str; 4] = ["log", "warn", "error", "info"];

/// Shell autocomplete: methods after `db.`, a collection or a cursor; fields and operators inside a
/// call's object; otherwise keywords, globals and collection names.
/// The variable a package is loaded as: camelCase of its name without scope. Must match
/// packageIdent() in shell_runner.mjs.
fn package_ident(spec: &str) -> String {
    let name = db::package_spec(spec).0;
    let base = name.strip_prefix('@').and_then(|r| r.split_once('/')).map(|(_, n)| n.to_string()).unwrap_or(name);
    let mut id: String = base
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|p| !p.is_empty())
        .enumerate()
        .map(|(i, p)| if i == 0 { p.to_string() } else { p[..1].to_ascii_uppercase() + &p[1..] })
        .collect();
    if id.starts_with(|c: char| c.is_ascii_digit()) {
        id.insert(0, '_');
    }
    if id.is_empty() { "pkg".into() } else { id }
}

fn suggest_js(before: &str, paths: &[String], colls: &[String], pkgs: &[String]) -> Option<(usize, Vec<Sug>)> {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    let tok_start = before.char_indices().rev().take_while(|(_, c)| ident(*c)).last().map(|(i, _)| i).unwrap_or(before.len());
    let token = &before[tok_start..];
    let lead = &before[..tok_start];
    let method = |m: &&str| Sug { text: m.to_string(), note: "method", insert: format!("{m}()"), caret: m.len() + 1 };
    let mut items: Vec<Sug> = if let Some(obj) = lead.strip_suffix('.') {
        let name_of = |s: &str| s[s.char_indices().rev().take_while(|(_, c)| ident(*c)).last().map(|(i, _)| i).unwrap_or(s.len())..].to_string();
        let list: &[&str] = if let Some(inner) = obj.strip_suffix(')') {
            // Find the call this ')' closes and look at its name: db.collection(), find(), aggregate()...
            let mut depth = 0;
            let open = inner.char_indices().rev().find(|(_, c)| {
                match c {
                    ')' => depth += 1,
                    '(' if depth == 0 => return true,
                    '(' => depth -= 1,
                    _ => {}
                }
                false
            });
            match open.map(|(i, _)| name_of(&inner[..i])).as_deref() {
                Some("collection") => &COLL_METHODS,
                Some("find" | "aggregate" | "sort" | "limit" | "skip" | "project" | "batchSize" | "hint" | "collation") => &CURSOR_METHODS,
                _ => &[],
            }
        } else {
            match name_of(obj).as_str() {
                "db" => &DB_METHODS,
                "console" => &CONSOLE_METHODS,
                n if colls.iter().any(|c| c == n) => &COLL_METHODS,
                _ => &[],
            }
        };
        list.iter().map(method).collect()
    } else if token.is_empty() {
        return None;
    } else if before.matches('(').count() > before.matches(')').count() && lead.trim_end().ends_with(['{', ',', ':']) {
        // Inside a call's arguments: the same field and operator suggestions as the Query box.
        return suggest(before, FieldKind::Query, paths, &[]);
    } else {
        let mut v: Vec<Sug> = JS_KEYWORDS.iter().map(|k| Sug { text: k.to_string(), note: "keyword", insert: format!("{k} "), caret: k.len() + 1 }).collect();
        v.extend(JS_GLOBALS.iter().map(|g| match *g {
            "ObjectId" | "ISODate" => Sug { text: g.to_string(), note: "helper", insert: format!("{g}(\"\")"), caret: g.len() + 2 },
            _ => Sug { text: g.to_string(), note: "global", insert: g.to_string(), caret: g.len() },
        }));
        v.extend(pkgs.iter().map(|p| Sug { text: p.clone(), note: "package", insert: p.clone(), caret: p.chars().count() }));
        v.extend(colls.iter().filter(|c| c.chars().all(ident) && !c.starts_with(|c: char| c.is_ascii_digit())).map(|c| Sug { text: c.clone(), note: "collection", insert: c.clone(), caret: c.chars().count() }));
        v
    };
    items.retain(|s| fuzzy(token, &s.text, false));
    let lower = token.to_ascii_lowercase();
    items.sort_by_key(|s| (!s.text.to_ascii_lowercase().starts_with(&lower), s.text.len()));
    items.truncate(50);
    if items.len() == 1 && items[0].text == token {
        return None;
    }
    (!items.is_empty()).then(|| (token.chars().count(), items))
}

/// JavaScript colouring for the Shell editor.
fn js_job(text: &str) -> LayoutJob {
    let fmt = |c: Color32| TextFormat { font_id: FontId::monospace(13.0), color: c, ..Default::default() };
    let mut job = LayoutJob::default();
    let mut i = 0;
    let mut prev_dot = false;
    while i < text.len() {
        let rest = &text[i..];
        let c = rest.chars().next().unwrap();
        let start = i;
        let color = if rest.starts_with("//") {
            i += rest.find('\n').unwrap_or(rest.len());
            FAINT()
        } else if rest.starts_with("/*") {
            i += rest[2..].find("*/").map(|e| e + 4).unwrap_or(rest.len());
            FAINT()
        } else if c == '"' || c == '\'' || c == '`' {
            i += 1;
            let mut esc = false;
            while i < text.len() {
                let ch = text[i..].chars().next().unwrap();
                i += ch.len_utf8();
                if esc {
                    esc = false;
                } else if ch == '\\' {
                    esc = true;
                } else if ch == c {
                    break;
                }
            }
            C_STRING()
        } else if c.is_ascii_digit() {
            while i < text.len() && text[i..].starts_with(|d: char| d.is_ascii_alphanumeric() || d == '.' || d == '_') {
                i += 1;
            }
            C_NUMBER()
        } else if c.is_alphabetic() || c == '_' || c == '$' {
            while i < text.len() && text[i..].starts_with(|d: char| d.is_alphanumeric() || d == '_' || d == '$') {
                i += text[i..].chars().next().unwrap().len_utf8();
            }
            let w = &text[start..i];
            let call = text[i..].trim_start().starts_with('(');
            match w {
                "true" => C_TRUE(),
                "false" => C_FALSE(),
                "null" | "undefined" => C_NULL(),
                _ if JS_KEYWORDS.contains(&w) && !prev_dot => C_KEYWORD(),
                _ if w.starts_with('$') => PRIMARY(),
                _ if call => C_DATE(),
                _ if JS_GLOBALS.contains(&w) && !prev_dot => C_OID(),
                _ if prev_dot => KEY(),
                _ if text[i..].trim_start().starts_with(':') => KEY(),
                _ => TEXT(),
            }
        } else {
            i += c.len_utf8();
            if "{}[]()".contains(c) { C_OBJ() } else if ".,:;".contains(c) { FAINT() } else if "=+-*/<>!&|?%".contains(c) { C_KEYWORD() } else { TEXT() }
        };
        if !text[start..i].trim().is_empty() {
            prev_dot = &text[start..i] == ".";
        }
        job.append(&text[start..i], 0.0, fmt(color));
    }
    job
}

fn js_layout(ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32) -> Arc<egui::Galley> {
    let mut job = js_job(buf.as_str());
    job.wrap.max_width = wrap;
    ui.fonts_mut(|f| f.layout_job(job))
}

fn code_layout(ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32) -> Arc<egui::Galley> {
    let mut job = json_job(buf.as_str());
    job.wrap.max_width = wrap;
    ui.fonts_mut(|f| f.layout_job(job))
}

/// Cmd+F for a big text area, kept in egui memory under the area's id.
#[derive(Clone, Default)]
struct Find {
    open: bool,
    query: String,
    cur: usize,
    scroll: bool,
    frame: u64,
}

impl Find {
    /// The bar above the text area. Enter and Shift+Enter step through the matches; Esc closes
    /// the bar here so it does not also close the window around it.
    fn bar(ui: &mut egui::Ui, id: egui::Id, text: &str) -> Find {
        let key = id.with("find");
        let field = key.with("field");
        let now = ui.ctx().cumulative_frame_nr();
        let mut f: Find = ui.data(|d| d.get_temp(key)).unwrap_or_default();
        // Not drawn last frame means the window was closed: start fresh when it opens again.
        if f.frame + 1 < now {
            f = Find::default();
        }
        f.frame = now;
        f.scroll = false;
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::F)) {
            f.open = true;
            ui.memory_mut(|m| m.request_focus(field));
        }
        if f.open && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
            f.open = false;
        }
        if f.open {
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut f.query).id(field).hint_text("Find").desired_width(240.0));
                if r.changed() {
                    f.cur = 0;
                    f.scroll = true;
                }
                let n = f.hits(text).len();
                if f.cur >= n {
                    f.cur = 0;
                }
                let step = |f: &mut Find, back: bool| {
                    if n > 0 {
                        f.cur = if back { (f.cur + n - 1) % n } else { (f.cur + 1) % n };
                        f.scroll = true;
                    }
                };
                // A single line field gives up focus on Enter; take it back to keep stepping.
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    step(&mut f, ui.input(|i| i.modifiers.shift));
                    r.request_focus();
                }
                if ui.small_button("Prev").on_hover_text("Shift+Enter").clicked() {
                    step(&mut f, true);
                }
                if ui.small_button("Next").on_hover_text("Enter").clicked() {
                    step(&mut f, false);
                }
                let label = if f.query.is_empty() { String::new() } else if n == 0 { "No matches".into() } else { format!("{} of {n}", f.cur + 1) };
                ui.label(RichText::new(label).size(12.0).color(MUTED()));
            });
        }
        ui.data_mut(|d| d.insert_temp(key, f.clone()));
        f
    }

    fn hits(&self, text: &str) -> Vec<std::ops::Range<usize>> {
        if !self.open || self.query.is_empty() {
            return Vec::new();
        }
        // ponytail: ASCII case folding only, so byte offsets stay those of the original text.
        let q = self.query.to_ascii_lowercase();
        text.to_ascii_lowercase().match_indices(&q).map(|(i, _)| i..i + q.len()).collect()
    }

    /// Layouter for the text area: `code` colors JSON, matches get a background.
    fn layouter(&self, code: bool) -> impl FnMut(&egui::Ui, &dyn egui::TextBuffer, f32) -> Arc<egui::Galley> + '_ {
        move |ui, buf, wrap| {
            let mut job = if code {
                json_job(buf.as_str())
            } else {
                let color = ui.visuals().override_text_color.unwrap_or_else(|| ui.visuals().widgets.inactive.text_color());
                LayoutJob::simple(buf.as_str().to_owned(), egui::TextStyle::Monospace.resolve(ui.style()), color, wrap)
            };
            self.mark(&mut job);
            job.wrap.max_width = wrap;
            ui.fonts_mut(|f| f.layout_job(job))
        }
    }

    /// Splits the sections at match edges. Hits are sorted, so one pass covers both lists.
    fn mark(&self, job: &mut LayoutJob) {
        let hits = self.hits(&job.text);
        if hits.is_empty() {
            return;
        }
        let mut out = Vec::with_capacity(job.sections.len() + 2 * hits.len());
        use egui::text::ByteIndex;
        let mut j = 0;
        for s in job.sections.drain(..) {
            let (start, end): (usize, usize) = (s.byte_range.start.into(), s.byte_range.end.into());
            let mut at = start;
            while j < hits.len() && hits[j].end <= at {
                j += 1;
            }
            let mut k = j;
            while at < end {
                let mut piece = s.clone();
                if at > start {
                    piece.leading_space = 0.0;
                }
                match hits.get(k).filter(|h| h.start < end) {
                    Some(h) if h.start <= at => {
                        let e = h.end.min(end);
                        if k == self.cur {
                            piece.format.background = WARNING();
                            piece.format.color = BG();
                        } else {
                            piece.format.background = WARNING().gamma_multiply(0.3);
                        }
                        piece.byte_range = ByteIndex(at)..ByteIndex(e);
                        at = e;
                        if h.end <= end {
                            k += 1;
                        }
                    }
                    Some(h) => {
                        piece.byte_range = ByteIndex(at)..ByteIndex(h.start);
                        at = h.start;
                    }
                    None => {
                        piece.byte_range = ByteIndex(at)..ByteIndex(end);
                        at = end;
                    }
                }
                out.push(piece);
            }
        }
        job.sections = out;
    }

    /// Scrolls the current match into view. Call inside the text area's ScrollArea.
    fn reveal(&self, ui: &egui::Ui, text: &str, out: &egui::text_edit::TextEditOutput) {
        if !self.scroll {
            return;
        }
        let Some(before) = self.hits(text).get(self.cur).and_then(|h| text.get(..h.start)) else { return };
        let r = out.galley.pos_from_cursor(egui::text::CCursor::new(before.chars().count()));
        ui.scroll_to_rect(r.translate(out.galley_pos.to_vec2()), Some(egui::Align::Center));
    }
}

/// A table cell. With `summarize`, an object or array whose JSON would not fit the cell shows as
/// "{12 fields}" or "[500 items]" instead of its first characters; small ones stay in full.
fn column_job(v: &Bson, summarize: bool) -> LayoutJob {
    let job = cell_job(v);
    if summarize && job.text.len() >= CELL_PREVIEW_MAX {
        let text = match v {
            Bson::Document(d) => format!("{{{} fields}}", d.len()),
            Bson::Array(a) => format!("[{} items]", a.len()),
            _ => return job,
        };
        let mut j = LayoutJob::default();
        j.append(&text, 0.0, mono(C_OBJ()));
        return j;
    }
    job
}

/// Characters of a cell preview, the original's cut for objects and arrays
/// (fieldTypes.object.toDisplayString, renderer line 30953: substring(0, 150)).
const CELL_PREVIEW_MAX: usize = 150;

/// A table cell as coloured text, like the original's table: strings in quotes, and objects and
/// arrays as a short coloured preview of their contents, so nested data keeps its type colours.
fn cell_job(v: &Bson) -> LayoutJob {
    const MAX: usize = CELL_PREVIEW_MAX;
    fn push(job: &mut LayoutJob, s: &str, c: Color32) -> bool {
        let room = MAX.saturating_sub(job.text.len());
        if room == 0 {
            return false;
        }
        let t: String = s.chars().take(room).collect();
        job.append(&t, 0.0, mono(c));
        job.text.len() < MAX
    }
    fn walk(job: &mut LayoutJob, v: &Bson) -> bool {
        match v {
            Bson::Document(d) => {
                if !push(job, "{", C_OBJ()) {
                    return false;
                }
                for (i, (k, x)) in d.iter().enumerate() {
                    if (i > 0 && !push(job, ", ", FAINT())) || !push(job, k, MUTED()) || !push(job, ": ", FAINT()) || !walk(job, x) {
                        return false;
                    }
                }
                push(job, "}", C_OBJ())
            }
            Bson::Array(a) => {
                if !push(job, "[", C_OBJ()) {
                    return false;
                }
                for (i, x) in a.iter().enumerate() {
                    if (i > 0 && !push(job, ", ", FAINT())) || !walk(job, x) {
                        return false;
                    }
                }
                push(job, "]", C_OBJ())
            }
            Bson::String(s) if str_kind(s) == StrKind::Url => push(job, s, C_LINK()),
            // Cut before replacing: a cell shows 100 characters, the string may hold megabytes.
            Bson::String(s) => push(job, &format!("\"{}\"", truncate_display(&s.chars().take(STRING_DISPLAY_MAX + 1).collect::<String>().replace('\n', " "))), color_of(v)),
            // The original shows binary from 100 bytes as "<too long binary>" (fieldTypes.binary, 31012).
            Bson::Binary(b) if b.bytes.len() >= 100 => push(job, &format!("BinData({} bytes)", b.bytes.len()), FAINT()),
            Bson::ObjectId(o) => push(job, &o.to_hex(), C_OID()),
            other => push(job, &leaf_text(other), color_of(other)),
        }
    }
    let mut job = LayoutJob::default();
    walk(&mut job, v);
    job
}

/// Short text for a table cell. Objects and arrays are summarised, never serialised, so wide
/// documents stay cheap to draw.
fn cell_text(b: &Bson) -> String {
    match b {
        Bson::String(s) => s.chars().take(120).collect(),
        Bson::ObjectId(o) => o.to_hex(),
        Bson::Document(d) => format!("{{{} fields}}", d.len()),
        Bson::Array(a) => format!("[{} items]", a.len()),
        other => leaf_text(other),
    }
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct ConnMeta {
    id: u64,
    name: String,
    /// Reach the server through an SSH tunnel; passwords and passphrases live in the Keychain.
    #[serde(default)]
    ssh: Option<crate::ssh::SshConf>,
    /// Reach the server through a SOCKS5 proxy; its password lives in the Keychain.
    #[serde(default)]
    proxy: Option<db::Proxy>,
}

#[derive(PartialEq)]
enum Status {
    Idle,
    Connecting,
    Ready,
    Failed(String),
}

struct Conn {
    meta: ConnMeta,
    client: Option<Client>,
    status: Status,
    /// Writes are locked at every launch and only unlocked for this session.
    writable: bool,
    dbs: Vec<(String, Option<Vec<String>>)>,
    masked_uri: String,
    /// Kept alive while connected through SSH; dropping it closes the tunnel.
    tunnel: Option<crate::ssh::Tunnel>,
}

#[derive(Clone, PartialEq)]
struct Sel {
    conn: u64,
    db: String,
    coll: String,
}

/// Shortcuts listed by Cmd+/. Cmd is Ctrl on Linux and Windows.
const SHORTCUTS: [(&str, &str); 22] = [
    ("Cmd+K, Cmd+T", "Jump to a connection, database or collection"),
    ("Cmd+R", "Reload the current tab"),
    ("Cmd+Shift+R", "Refresh the connection's databases"),
    ("Cmd+W", "Close the tab"),
    ("Cmd+Shift+T", "Reopen the last closed tab"),
    ("Cmd+1 to Cmd+8", "Go to that tab"),
    ("Cmd+9", "Go to the last tab"),
    ("Ctrl+Tab, Cmd+Shift+]", "Next tab"),
    ("Ctrl+Shift+Tab, Cmd+Shift+[", "Previous tab"),
    ("Cmd+Alt+1 to Cmd+Alt+7", "Documents, Aggregate, Indexes, Schema, Import, Compare, Shell"),
    ("Cmd+F", "Focus the query"),
    ("Cmd+Shift+F", "Find a collection in the sidebar"),
    ("Up, Down", "Select the previous or next document"),
    ("PageUp, PageDown", "Previous or next page"),
    ("Enter", "Run the query"),
    ("Cmd+E", "Explain the query or pipeline"),
    ("Cmd+Enter", "Run the pipeline or script"),
    ("Esc", "Close or deselect"),
    ("Cmd+,", "Settings"),
    ("Cmd+N", "New connection"),
    ("Cmd+L", "Show or hide the query log"),
    ("Cmd+/", "This list"),
];

/// Where the Cmd+K palette can jump to.
#[derive(Clone)]
enum Jump {
    Conn(u64),
    Db(u64, String),
    Coll(Sel),
}

/// How well one lowercase query token matches one lowercase name.
fn seg_score(t: &str, s: &str) -> Option<u32> {
    if s == t {
        Some(8)
    } else if s.starts_with(t) {
        Some(6)
    } else if s.match_indices(t).any(|(p, _)| s[..p].ends_with(['-', '_', '.', ' '])) {
        Some(4)
    } else if s.contains(t) {
        Some(2)
    } else {
        fuzzy(t, s, false).then_some(1)
    }
}

/// Scores an entry's lowercase names (connection, database, collection) against the query
/// tokens, or None when a token matches nowhere. Each token counts its best name, so
/// "vairon travels" finds thetreep-api-vairon.travels. A collection whose own name matches
/// the last token gets a bonus, as people usually type the collection last.
fn jump_score(tokens: &[String], segs: &[&str]) -> Option<u32> {
    let mut total = 0;
    for (n, t) in tokens.iter().enumerate() {
        let best = segs.iter().enumerate().filter_map(|(i, s)| seg_score(t, s).map(|sc| (sc, i))).max();
        let (sc, i) = match best {
            Some(b) => b,
            // Typed without separators, like "shopord", a token can span names.
            None if fuzzy(t, &segs.join("."), false) => (1, 0),
            None => return None,
        };
        total += sc;
        if n + 1 == tokens.len() && segs.len() == 3 && i == 2 {
            total += 3;
        }
    }
    Some(total)
}

/// Palette entries matching `q` as (label, hint, target), best match first. With an empty query
/// they keep tree order, active connection first.
fn jumps(conns: &[Conn], active: Option<u64>, q: &str) -> Vec<(String, String, Jump)> {
    const MAX: usize = 50;
    let q = q.to_lowercase();
    let tokens: Vec<String> = q.split(|c: char| c.is_whitespace() || c == '.' || c == '/').filter(|t| !t.is_empty()).map(String::from).collect();
    let mut out = Vec::new();
    let mut order: Vec<&Conn> = conns.iter().collect();
    order.sort_by_key(|c| Some(c.meta.id) != active);
    for c in order {
        let id = c.meta.id;
        let bonus = u32::from(Some(id) == active);
        let name = c.meta.name.to_lowercase();
        let mut push = |segs: &[&str], label: String, hint: &str, j: Jump| {
            if let Some(s) = jump_score(&tokens, segs) {
                out.push((s + bonus, label, hint.to_string(), j));
            }
        };
        push(&[&name], c.meta.name.clone(), &c.masked_uri, Jump::Conn(id));
        for (db, colls) in &c.dbs {
            let dbl = db.to_lowercase();
            push(&[&name, &dbl], db.clone(), &c.meta.name, Jump::Db(id, db.clone()));
            for coll in colls.iter().flatten() {
                push(&[&name, &dbl, &coll.to_lowercase()], format!("{db}.{coll}"), &c.meta.name, Jump::Coll(Sel { conn: id, db: db.clone(), coll: coll.clone() }));
            }
        }
    }
    if !tokens.is_empty() {
        out.sort_by_key(|(s, label, ..)| (std::cmp::Reverse(*s), label.len()));
    }
    out.truncate(MAX);
    out.into_iter().map(|(_, label, hint, j)| (label, hint, j)).collect()
}

#[derive(PartialEq, Clone, Copy)]
enum View {
    Documents,
    Aggregate,
    Indexes,
    Shell,
    Schema,
    Import,
    Compare,
}

/// Everything that belongs to one open tab.
struct Tab {
    sel: Sel,
    view: View,
    filter_txt: String,
    sort_txt: String,
    proj_txt: String,
    query: Query,
    page: u64,
    docs: Vec<Document>,
    fields: Vec<String>,
    /// Every dotted field path seen in the loaded documents, for autocomplete.
    paths: Vec<String>,
    /// Paths whose loaded values are dates; their Query values suggest date shortcuts.
    date_paths: Vec<String>,
    count: u64,
    ms: u128,
    req: u64,
    /// Index list requests have their own id, so they never cancel a documents load.
    idx_req: u64,
    loading: bool,
    selected: Option<(bool, usize)>,
    /// Rows of the current page checked for bulk actions, and the last one clicked for Shift ranges.
    checked: BTreeSet<usize>,
    check_anchor: Option<usize>,
    /// Rows open inline besides the selected one, keyed like `selected`.
    expanded: BTreeSet<(bool, usize)>,
    agg_txt: String,
    agg_docs: Vec<Document>,
    agg_fields: Vec<String>,
    agg_ms: Option<u128>,
    /// The running pipeline's request, 0 when idle. Kept apart from `req` so a page load and a
    /// pipeline never discard each other's answer.
    agg_req: u64,
    // Stage whose output the results pane shows; None after a full run.
    agg_stage: Option<usize>,
    /// Pipeline builder stages and the pipeline text they match; when `agg_txt` changes
    /// elsewhere (Text mode, library, a pasted query) the stages are rebuilt from it.
    stages: Vec<Stage>,
    stages_src: String,
    /// The collection's first documents and estimated count, shown above the first stage.
    agg_input: Stage,
    agg_text_mode: bool,
    indexes: Vec<Document>,
    shell: ShellTab,
    schema: SchemaTab,
    import: ImportTab,
    compare: CompareTab,
    /// Preview text of referenced documents, keyed by `compact(_id)`.
    related: HashMap<String, String>,
}

/// The Compare view: this tab's collection (source) against a chosen target.
#[derive(Default)]
struct CompareTab {
    conn: u64,
    db: String,
    coll: String,
    req: u64,
    running: bool,
    result: Option<db::Compared>,
    insert: bool,
    replace: bool,
    delete: bool,
}

/// A field holding ids of another collection's documents (the original's dataRelations).
#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
#[serde(default)]
struct Relation {
    coll: String,
    path: String,
    target: String,
    preview: String,
}

/// Relations as the original's export file (exportRelations, renderer line 135294), so files
/// move between Quarry and the original.
fn relations_export(db: &str, list: &[Relation]) -> String {
    let rel: Vec<serde_json::Value> = list.iter().map(|r| serde_json::json!({ "collection": r.coll, "path": r.path, "referencedCollection": r.target, "previewField": r.preview })).collect();
    let now = mongodb::bson::DateTime::now().try_to_rfc3339_string().unwrap_or_default();
    serde_json::to_string_pretty(&serde_json::json!({ "version": "1.0", "database": db, "exportedAt": now, "relations": rel })).unwrap_or_default()
}

/// Reads an export file; files without `version` and `relations` are refused, like the original
/// (importRelations, 135332). Array markers in the original's paths ("items[].id") are dropped.
fn relations_import(text: &str) -> db::Res<Vec<Relation>> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let (Some(_), Some(list)) = (v.get("version"), v.get("relations").and_then(|r| r.as_array())) else {
        return Err("Invalid data relations file format".into());
    };
    let s = |r: &serde_json::Value, k: &str| r.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
    Ok(list.iter().map(|r| Relation { coll: s(r, "collection"), path: s(r, "path").replace("[]", ""), target: s(r, "referencedCollection"), preview: s(r, "previewField") }).filter(|r| !r.coll.is_empty() && !r.path.is_empty() && !r.target.is_empty()).collect())
}

/// The Dump or Restore form.
#[derive(Default)]
struct ToolForm {
    conn: u64,
    db: String,
    coll: String,
    restore: bool,
    path: String,
    gzip: bool,
    drop: bool,
    /// Database name inside the archive; restoring renames it to `db` when different.
    from_db: String,
}

/// A running (or finished) mongodump or mongorestore.
struct ToolRun {
    title: String,
    log: Vec<String>,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    done: Option<db::Res<String>>,
}

/// The Schema view of one tab.
struct SchemaTab {
    req: u64,
    limit: i64,
    running: bool,
    stats: Vec<db::FieldStat>,
    docs: usize,
    ms: Option<u128>,
    /// Why the last analysis failed; also stops the automatic analysis from retrying every frame.
    err: Option<String>,
    /// Validator editor: the JSON, the level (0 off, 1 moderate, 2 strict) and whether failures only warn.
    vtext: String,
    vlevel: u8,
    vwarn: bool,
    vreq: u64,
    vloaded: bool,
}

impl Default for SchemaTab {
    fn default() -> Self {
        Self { req: 0, limit: SCHEMA_DEFAULT_LIMIT, running: false, stats: Vec::new(), docs: 0, ms: None, err: None, vtext: String::new(), vlevel: 2, vwarn: false, vreq: 0, vloaded: false }
    }
}

/// The original analyses every matching document (limit 0); on large production collections that
/// is slow, so the rebuild starts with the first 1000 and offers "All".
const SCHEMA_DEFAULT_LIMIT: i64 = 1000;

/// The Import view of one tab: the text to import, how to read it and the parsed preview.
struct ImportTab {
    text: String,
    file: Option<String>,
    kind: db::ImportKind,
    separator: Option<char>,
    header: bool,
    camel: bool,
    upsert: bool,
    upsert_by: String,
    parsed: Option<db::Res<Vec<Document>>>,
    /// What `parsed` was made from, so it is redone only when something changes.
    parsed_key: u64,
    fields: Vec<String>,
}

impl Default for ImportTab {
    fn default() -> Self {
        Self { text: String::new(), file: None, kind: db::ImportKind::Json, separator: None, header: true, camel: false, upsert: false, upsert_by: "_id".into(), parsed: None, parsed_key: 0, fields: Vec::new() }
    }
}

/// The Shell view of one tab: the script, its logs and its result.
#[derive(Default)]
struct ShellTab {
    code: String,
    logs: Vec<(String, String)>,
    /// The returned value; documents also go to `docs` for the table.
    value: Option<Bson>,
    docs: Vec<Document>,
    fields: Vec<String>,
    page: usize,
    /// Show the result as JSON text instead of the table.
    raw: bool,
    raw_text: Option<String>,
    error: Option<String>,
    req: u64,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    help: bool,
    started: Option<std::time::Instant>,
    ms: Option<u128>,
    count: Option<usize>,
}

impl Tab {
    fn new(sel: Sel) -> Self {
        Self {
            sel,
            view: View::Documents,
            filter_txt: String::new(),
            sort_txt: String::new(),
            proj_txt: String::new(),
            query: Query::default(),
            page: 0,
            docs: Vec::new(),
            fields: Vec::new(),
            paths: Vec::new(),
            date_paths: Vec::new(),
            count: 0,
            ms: 0,
            req: 0,
            idx_req: 0,
            loading: false,
            selected: None,
            checked: BTreeSet::new(),
            check_anchor: None,
            expanded: BTreeSet::new(),
            agg_txt: "[\n  { $match: {} },\n  { $limit: 20 }\n]".into(),
            agg_docs: Vec::new(),
            agg_fields: Vec::new(),
            agg_ms: None,
            agg_req: 0,
            agg_stage: None,
            stages: Vec::new(),
            stages_src: String::new(),
            agg_input: Stage::new("", "", true),
            agg_text_mode: false,
            indexes: Vec::new(),
            shell: ShellTab::default(),
            schema: SchemaTab::default(),
            import: ImportTab::default(),
            compare: CompareTab { insert: true, replace: true, ..Default::default() },
            related: HashMap::new(),
        }
    }
}

enum Danger {
    DeleteMatching(Document),
    DropCollection,
    DropIndex(String),
    RemoveConnection(u64),
    DropDatabase { conn: u64, db: String },
    DropCollAt { conn: u64, db: String, coll: String },
    DropUser { conn: u64, db: String, user: String },
    RenameMany { filter: Document, from: String, to: String },
    UpdateMany { filter: Document, stages: Vec<Document> },
    Upsert { key: String },
    Sync { conn: u64 },
    Restore { conn: u64, args: Vec<String> },
    Validator(Document),
}

/// What to run once writes are allowed.
type Retry = Box<dyn FnOnce(&mut App)>;

enum Modal {
    Conn(Box<ConnForm>),
    Settings,
    /// The second field reruns what asked for writes once they are allowed.
    Unlock(u64, Option<Retry>),
    Save { tag: String, changes: Vec<(String, Option<Bson>, Option<Bson>)> },
    SaveAll(Vec<String>),
    Delete(Vec<Document>),
    Typed { title: String, expect: String, typed: String, action: Danger },
    AggWrite(Vec<Document>),
    ShellWrite(Vec<String>),
    ImportInsert(usize),
    NewCollection { conn: u64, db: String, name: String, view: bool, source: String, pipeline: String },
    RenameColl { conn: u64, db: String, coll: String, name: String },
    Undo,
    Explain { summary: db::ExplainSummary, raw: String, show_raw: bool },
    /// `code` highlights the body as JSON.
    Text { title: String, body: String, code: bool },
    Rename { doc: Document, path: Vec<String>, name: String },
    /// `preview` keeps the stages it was made from; Apply only runs when they equal the current
    /// ones, so a late reply for older inputs cannot approve newer stages.
    UpdateMany { set_txt: String, unset_txt: String, preview: Option<(Vec<Document>, db::Res<(Vec<(Document, Document)>, u64)>)> },
    Tool(Box<ToolForm>),
    Relations { conn: u64, db: String },
    Palette { query: String, sel: usize },
    /// Values of a field with how many documents hold each, and the pipeline that counted them.
    Grouped { sel: Sel, path: String, pipeline: Vec<Document>, rows: Vec<Document> },
}

/// A value being edited in place in the document tree.
struct Inline {
    path: Vec<String>,
    text: String,
    kind: &'static str,
    focus: bool,
}

/// The large JSON editor for a whole document (empty path) or one object/array field.
struct JsonEdit {
    base: Document,
    path: Vec<String>,
    text: String,
    kind: &'static str,
    focus: bool,
}

/// One document's edits that are not written yet. `original` is what was loaded, so saving can
/// refuse to overwrite someone else's change.
#[derive(Clone)]
struct Draft {
    conn: u64,
    db: String,
    coll: String,
    original: Document,
    current: Document,
}

/// A field being added inside an object (or appended to an array).
struct NewField {
    parent: Vec<String>,
    name: String,
    text: String,
    kind: &'static str,
}

/// Per-collection column setup, saved between launches.
#[derive(Default, Serialize, Deserialize, Clone)]
#[serde(default)]
struct ColPrefs {
    /// Highlighted field paths (array positions removed), like the original's highlightedFields.
    highlight: Vec<String>,
    /// Chosen columns in order (dotted paths allowed). `None` means the first DEFAULT_COLS fields.
    shown: Option<Vec<String>>,
    /// Pinned columns always come first.
    pinned: Vec<String>,
    widths: HashMap<String, f32>,
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct UiPrefs {
    /// Open documents under their row (like the original) instead of in a side panel.
    open_inline: bool,
    table: bool,
    /// Show the list of changes before writing staged edits. Off by default, as asked.
    confirm_save: bool,
    /// Write every edit right away and offer Undo, instead of collecting unsaved edits.
    auto_save: bool,
    /// Every connection is writable and saving edits skips the change list.
    always_writable: bool,
    /// Columns shown before the person picks their own.
    default_cols: usize,
    timeout_ms: u64,
    /// Path to Bun for the Shell; empty means find it automatically.
    shell_runtime: String,
    /// Shell scripts run, newest first, per "connection/database" like the original's history refs.
    shell_histories: HashMap<String, Vec<String>>,
    /// npm packages available to Shell scripts.
    shell_packages: Vec<String>,
    /// Per collection ("conn/db/coll"): recent Query, Sort and Project inputs.
    history: HashMap<String, QueryHist>,
    /// Per collection: named saved queries and pipelines.
    library: HashMap<String, Vec<SavedQuery>>,
    /// Tabs open when the app was last used, restored at launch.
    open_tabs: Vec<SavedTab>,
    cur_tab: usize,
    cols: HashMap<String, ColPrefs>,
    /// The connection picked when the app was last used, connected again at launch.
    last_conn: Option<u64>,
    /// Per "conn/db": fields that point to documents of another collection.
    relations: HashMap<String, Vec<Relation>>,
    /// Paths to mongodump and mongorestore; empty means find them automatically.
    mongodump: String,
    mongorestore: String,
    /// Install new releases from GitHub and restart into them.
    auto_update: bool,
    /// Pipeline builder stages preview themselves after each change.
    auto_preview: bool,
    /// Frame rate and the time each frame takes, in the status bar.
    show_fps: bool,
    /// Show the first characters of big objects and arrays in columns, as the original does,
    /// instead of "{12 fields}".
    object_preview: bool,
    appearance: Appearance,
    /// The version that last ran, to show the changelog once after an update.
    last_version: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Default)]
enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    fn apply(self, ctx: &egui::Context) {
        ctx.set_theme(match self {
            Appearance::System => egui::ThemePreference::System,
            Appearance::Light => egui::ThemePreference::Light,
            Appearance::Dark => egui::ThemePreference::Dark,
        });
    }
}

/// The original keeps 30 recent inputs per collection and kind (HISTORY_LENGTH_BY_COLLECTION,
/// renderer line 195241), skipping empty ones and moving repeats to the top (addHistory, 195309).
const HISTORY_LENGTH_BY_COLLECTION: usize = 30;

#[derive(Default, Serialize, Deserialize, Clone)]
#[serde(default)]
struct QueryHist {
    selector: Vec<String>,
    sort: Vec<String>,
    project: Vec<String>,
}

fn push_history(list: &mut Vec<String>, value: &str) {
    let v = value.trim();
    if v.is_empty() || v == "{}" {
        return;
    }
    list.retain(|x| x != v);
    list.insert(0, v.to_string());
    list.truncate(HISTORY_LENGTH_BY_COLLECTION);
}

/// A named query (or pipeline) saved for a collection; saving under an existing name replaces it,
/// like the original's saved filters (CLS.saveFilter, renderer line 195332).
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
struct SavedQuery {
    name: String,
    filter: String,
    sort: String,
    project: String,
    pipeline: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Default)]
#[serde(default)]
struct SavedTab {
    /// Pipeline builder stages, kept so stages switched off survive a restart.
    stages: Vec<SavedStage>,
    /// The Aggregate view shows the pipeline as text instead of stage cards.
    agg_text_mode: bool,
    conn: u64,
    db: String,
    coll: String,
    view: String,
    filter: String,
    sort: String,
    project: String,
    agg: String,
    shell: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Default)]
#[serde(default)]
struct SavedStage {
    op: String,
    text: String,
    on: bool,
}

/// One stage of the pipeline builder with its own preview, like the original's AggregatorStage
/// (prepareNewStage, renderer line 112193).
struct Stage {
    /// Stable id for the editor, so reordering keeps each text box's cursor and focus.
    uid: u64,
    op: String,
    text: String,
    on: bool,
    req: u64,
    count: Option<u64>,
    sample: Vec<Document>,
    ms: Option<u128>,
    error: Option<String>,
    limit: i64,
    /// When the preview should run; edits wait a second like the original's 1000 ms debounce
    /// (updateStage, renderer line 112016).
    due: Option<std::time::Instant>,
    /// Sample rows shown whole.
    open: HashSet<usize>,
}

impl Stage {
    fn new(op: &str, text: &str, on: bool) -> Self {
        static UID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            uid: UID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            op: op.into(),
            text: text.into(),
            on,
            req: 0,
            count: None,
            sample: Vec::new(),
            ms: None,
            error: None,
            limit: db::STAGE_SAMPLE,
            due: Some(std::time::Instant::now()),
            open: HashSet::new(),
        }
    }
}

/// Stages from pipeline text; each stage must hold exactly one operator.
fn stages_from_text(text: &str) -> db::Res<Vec<Stage>> {
    db::parse_pipeline(text)?
        .iter()
        .enumerate()
        .map(|(i, d)| match (d.len(), d.iter().next()) {
            (1, Some((op, v))) => Ok(Stage::new(op, &db::pretty_shell(v), true)),
            _ => Err(format!("Stage {} must have exactly one operator, like {{ $match: {{}} }}", i + 1)),
        })
        .collect()
}

/// Pipeline text of the stages that are on, so Run, the library and code export see them.
fn text_from_stages(stages: &[Stage]) -> String {
    let body: Vec<String> = stages.iter().filter(|s| s.on).map(|s| format!("  {{ {}: {} }}", s.op, s.text.trim().replace('\n', "\n  "))).collect();
    if body.is_empty() { "[]".into() } else { format!("[\n{}\n]", body.join(",\n")) }
}

/// The pipeline that ends with stage `i`, stages that are off left out.
fn stage_pipeline(stages: &[Stage], i: usize) -> db::Res<Vec<Document>> {
    stages[..=i]
        .iter()
        .enumerate()
        .filter(|(_, s)| s.on)
        .map(|(k, s)| db::parse_value(&s.text).map(|v| doc! { s.op.clone(): v }).map_err(|e| format!("Stage {} ({}): {e}", k + 1, s.op)))
        .collect()
}

/// The new/edit connection form.
#[derive(Default)]
struct ConnForm {
    id: Option<u64>,
    name: String,
    uri: String,
    show: bool,
    ssh_on: bool,
    ssh: crate::ssh::SshConf,
    ssh_pwd: String,
    ssh_pass: String,
    proxy_on: bool,
    proxy: db::Proxy,
    proxy_pwd: String,
    testing: bool,
}

impl ConnForm {
    fn net(&self) -> db::Net {
        db::Net {
            ssh: self.ssh_on.then(|| (crate::ssh::SshConf { host: self.ssh.host.trim().into(), user: self.ssh.user.trim().into(), ..self.ssh.clone() }, crate::ssh::SshSecrets { password: Some(self.ssh_pwd.clone()), passphrase: Some(self.ssh_pass.clone()) })),
            proxy: (self.proxy_on && !self.ssh_on).then(|| db::Proxy { host: self.proxy.host.trim().into(), password: Some(self.proxy_pwd.clone()), ..self.proxy.clone() }),
        }
    }
}

/// The Users and roles window of one connection.
#[derive(Default)]
struct UsersWin {
    conn: u64,
    users: Option<db::Res<Vec<Document>>>,
    roles: Option<db::Res<Vec<Document>>>,
    new_name: String,
    new_pwd: String,
    new_db: String,
    new_roles: String,
    /// (user, database, new password or empty, roles text)
    edit: Option<(String, String, String, String)>,
}

/// "readWrite@shop, read" as role documents; a role without @db belongs to `default_db`.
fn parse_roles(text: &str, default_db: &str) -> Vec<Bson> {
    text.split(',')
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(|r| {
            let (role, db) = r.split_once('@').unwrap_or((r, default_db));
            Bson::Document(doc! { "role": role.trim(), "db": db.trim() })
        })
        .collect()
}

fn roles_text(user: &Document) -> String {
    user.get_array("roles")
        .map(|a| a.iter().filter_map(|r| r.as_document()).map(|r| format!("{}@{}", r.get_str("role").unwrap_or_default(), r.get_str("db").unwrap_or_default())).collect::<Vec<_>>().join(", "))
        .unwrap_or_default()
}

enum SideAct {
    NewColl(String, bool),
    Refresh(String),
    Users,
    DropDb(String),
    Stats(String, String),
    ViewDef(String, String),
    Rename(String, String),
    DropColl(String, String),
    Dump(String, String),
    Restore(String),
    Compare(String, String),
    Relations(String),
    /// Analyze relations of one collection, or of every collection when empty.
    Analyze(String, String),
}

enum QAct {
    Filter(String),
    Sort(String),
    Project(String),
    Load(SavedQuery),
    Save(String),
    Delete(String),
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self { open_inline: true, table: true, confirm_save: false, auto_save: false, always_writable: false, default_cols: DEFAULT_COLS, timeout_ms: db::DEFAULT_TIMEOUT_MS, shell_runtime: String::new(), shell_histories: HashMap::new(), shell_packages: Vec::new(), history: HashMap::new(), library: HashMap::new(), open_tabs: Vec::new(), cur_tab: 0, last_conn: None, cols: HashMap::new(), relations: HashMap::new(), mongodump: String::new(), mongorestore: String::new(), auto_update: true, auto_preview: true, show_fps: false, object_preview: false, appearance: Appearance::System, last_version: String::new() }
    }
}

enum TreeAct {
    Start(Vec<String>, Bson),
    Commit,
    Cancel,
    Remove(Vec<String>),
    AddField(Vec<String>),
    CommitNew,
    ShowFull(String, Bson),
    PinColumn(String),
    Highlight(String),
    Filter(String, Bson, bool),
    Group(String, Bson, bool),
    Distinct(String),
    Index(String),
    Rename(Vec<String>),
    GoRef(String, Bson),
    EditDoc,
}

#[derive(Clone, Copy)]
enum TabClose {
    This,
    Others,
    Left,
    Right,
    All,
}

enum ColAct {
    Pin(String),
    Hide(String),
    Move(String, i32),
    /// Drag and drop: the column goes before the one at this position.
    MoveTo(String, usize),
    Show(String),
    Highlight(String),
    Reset,
}

/// The query log panel: docked above the status bar, or in its own window.
#[derive(Default)]
struct LogUi {
    open: bool,
    detached: bool,
    filter: String,
    errors: bool,
}

fn ago(t: std::time::SystemTime) -> String {
    let s = t.elapsed().map_or(0, |d| d.as_secs());
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        _ => format!("{}h", s / 3600),
    }
}

pub struct App {
    worker: Worker,
    rx: Receiver<Msg>,
    conns: Vec<Conn>,
    active: Option<u64>,
    open_dbs: HashSet<(u64, String)>,
    tabs: Vec<Tab>,
    cur: usize,
    req: u64,
    json_edit: Option<JsonEdit>,
    drafts: HashMap<String, Draft>,
    saving: HashMap<String, Draft>,
    /// Paths changed in the document being drawn, for tinting.
    changed: HashSet<String>,
    inline: Option<Inline>,
    new_field: Option<NewField>,
    /// Pending expand/collapse from the tree menu: document, path prefix, open.
    fold: Option<(egui::Id, Vec<String>, bool)>,
    fold_busy: bool,
    /// Key column width and full row span of the tree being drawn, so values line up and hover bands cover the whole card.
    key_w: f32,
    char_w: f32,
    tree_x: egui::Rangef,
    insert: Option<String>,
    /// Database hidden from the sidebar while its drop runs, to show again if the drop fails.
    dropping_db: Option<(u64, String)>,
    prefs: UiPrefs,
    new_col: String,
    col_filter: String,
    #[cfg(target_os = "macos")]
    menu: Option<crate::menu::MenuBar>,
    /// Menu picks that are not key presses, handled at the start of the next frame.
    #[cfg(target_os = "macos")]
    menu_acts: Vec<crate::menu::Act>,
    new_pkg: String,
    new_lib: String,
    /// Views per (connection, database), shown differently in the sidebar.
    views: HashSet<(u64, String, String)>,
    users: Option<UsersWin>,
    conn_test: Option<db::Res<String>>,
    idx_keys: String,
    idx_unique: bool,
    /// Usage and suggestions of the collection "db.coll" the Indexes view last asked about.
    idx_info: Option<(String, db::IndexInfo)>,
    undo: Vec<UndoAction>,
    explain_req: u64,
    modal: Option<Modal>,
    status: (String, bool),
    status_at: std::time::Instant,
    log: LogUi,
    /// Message, when it was shown, and the `at` of the undo entry its Undo button reverts.
    toast: Option<(String, std::time::Instant, Option<std::time::Instant>)>,
    ac: Ac,
    tree_filter: String,
    tools_installing: bool,
    /// Tree row the palette jumped to, as (connection, database, collection), scrolled into view once drawn.
    reveal: Option<(u64, String, Option<String>)>,
    /// Scroll the selected document row into view on its next draw, after keyboard selection.
    scroll_sel: bool,
    demo: bool,
    tool: Option<ToolRun>,
    /// CPU time of the last frame in milliseconds, for the frame rate readout.
    frame_ms: f32,
    /// Measured column widths per result set and column.
    width_cache: HashMap<(usize, usize, String, bool), f32>,
    /// Recently closed tabs, newest first.
    closed: Vec<Tab>,
    /// Version installed by the updater, waiting for a restart.
    updated: Option<String>,
    update_note: String,
    update_checked: Option<std::time::Instant>,
    last_input: std::time::Instant,
    started: std::time::Instant,
    shot: Option<(PathBuf, u32, Arc<Mutex<Option<egui::ColorImage>>>)>,
}

/// Parses a config file. One that no longer parses is moved to `<name>.bak`, so the next save
/// cannot overwrite what the person had, and the reason comes back to be shown.
fn parse_config<T: serde::de::DeserializeOwned + Default>(path: &std::path::Path) -> (T, Option<String>) {
    let Ok(b) = std::fs::read(path) else { return (T::default(), None) };
    match serde_json::from_slice(&b) {
        Ok(v) => (v, None),
        Err(err) => {
            let bak = path.with_extension("json.bak");
            let moved = match std::fs::rename(path, &bak) {
                Ok(()) => format!("The file was kept as {}.", bak.display()),
                Err(e) => format!("Moving it aside failed too ({e}); it will be overwritten on the next save."),
            };
            (T::default(), Some(format!("Could not read {}: {err}. {moved}", path.display())))
        }
    }
}

fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("quarry")
}

/// Where Quarry installs the MongoDB Database Tools itself when they are missing.
fn tools_dir() -> PathBuf {
    config_dir().join("tools")
}

fn keychain(id: u64) -> Option<keyring::Entry> {
    keyring::Entry::new("quarry", &format!("connection-{id}")).ok()
}

/// Other secrets of a connection: "ssh-password", "ssh-passphrase", "proxy-password".
fn secret(id: u64, what: &str) -> Option<String> {
    keyring::Entry::new("quarry", &format!("connection-{id}-{what}")).ok()?.get_password().ok().filter(|s| !s.is_empty())
}

/// The connection's address from the Keychain.
fn stored_uri(id: u64) -> Result<String, keyring::Error> {
    keychain(id).ok_or(keyring::Error::NoEntry)?.get_password()
}

fn uri_err(e: keyring::Error) -> String {
    format!("Could not read the connection's address from the Keychain: {e}")
}

/// Stores a secret, or removes it when `value` is empty.
fn set_secret(id: u64, what: &str, value: &str) -> Result<(), String> {
    let r = keyring::Entry::new("quarry", &format!("connection-{id}-{what}")).and_then(|k| if value.is_empty() { k.delete_credential() } else { k.set_password(value) });
    match r {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("Could not store the {} in the Keychain: {e}", what.replace('-', " "))),
    }
}

fn net_of(meta: &ConnMeta) -> db::Net {
    db::Net {
        ssh: meta.ssh.clone().map(|c| (c, crate::ssh::SshSecrets { password: secret(meta.id, "ssh-password"), passphrase: secret(meta.id, "ssh-passphrase") })),
        proxy: meta.proxy.clone().map(|p| db::Proxy { password: secret(meta.id, "proxy-password"), ..p }),
    }
}

/// Top-level fields of the shown documents, `_id` first, in the order they appear.
fn auto_fields(docs: &[Document]) -> Vec<String> {
    let mut cols = vec!["_id".to_string()];
    let mut seen: HashSet<&str> = HashSet::from(["_id"]);
    for d in docs {
        for k in d.keys() {
            if seen.insert(k) {
                cols.push(k.clone());
            }
        }
    }
    cols
}

fn highlighted(prefs: &UiPrefs, key: &str, path: &[String]) -> bool {
    prefs.cols.get(key).is_some_and(|p| !p.highlight.is_empty() && p.highlight.contains(&db::strip_indices(&path.join("."))))
}

/// Right-click menu for one field, modelled on the original's fieldActions (renderer line 106562).
fn fold_menu(ui: &mut egui::Ui, path: &[String], container: bool) -> Option<(Vec<String>, bool)> {
    ui.separator();
    let mut f = None;
    if container && ui.button("Expand recursively").clicked() {
        f = Some((path.to_vec(), true));
    }
    if ui.button("Expand all").clicked() {
        f = Some((Vec::new(), true));
    }
    if ui.button("Collapse all").clicked() {
        f = Some((Vec::new(), false));
    }
    f
}

fn field_menu(ui: &mut egui::Ui, path: &[String], v: &Bson, editable: bool, highlighted: bool, acts: &mut Vec<TreeAct>) {
    let p = path.join(".");
    let is_id = path.first().is_some_and(|k| k == "_id");
    ui.set_min_width(230.0);
    ui.label(RichText::new(format!("Field: {p}")).size(12.0).color(MUTED()));
    ui.separator();
    if ui.button("Copy value").clicked() {
        ui.ctx().copy_text(match v {
            Bson::Document(_) | Bson::Array(_) => db::shell_text(v),
            other => db::value_text(other),
        });
        ui.close();
    }
    if ui.button("Copy path").clicked() {
        ui.ctx().copy_text(p.clone());
        ui.close();
    }
    if ui.button("Show full value").clicked() {
        acts.push(TreeAct::ShowFull(p.clone(), v.clone()));
    }
    if editable && !is_id && ui.button("Edit value…").clicked() {
        acts.push(TreeAct::Start(path.to_vec(), v.clone()));
    }
    if editable && ui.button("Edit full document as JSON…").clicked() {
        acts.push(TreeAct::EditDoc);
    }
    if !is_id && matches!(v, Bson::ObjectId(_) | Bson::String(_)) && ui.button("Go to referenced document").on_hover_text("Opens the document whose _id is this value; the collection is found automatically the first time").clicked() {
        acts.push(TreeAct::GoRef(p.clone(), v.clone()));
    }
    ui.separator();
    if ui.button("Pin as a new column").clicked() {
        acts.push(TreeAct::PinColumn(p.clone()));
    }
    // A checkbox so an active highlight reads as on and is clearly undone from here.
    if ui.checkbox(&mut highlighted.clone(), "Highlight field").clicked() {
        acts.push(TreeAct::Highlight(p.clone()));
        ui.close();
    }
    ui.menu_button("Filter by value", |ui| {
        if ui.button("All documents").clicked() {
            acts.push(TreeAct::Filter(p.clone(), v.clone(), false));
        }
        if ui.button("Filtered documents only").clicked() {
            acts.push(TreeAct::Filter(p.clone(), v.clone(), true));
        }
    });
    ui.menu_button("Group by and count", |ui| {
        if ui.button("All documents").clicked() {
            acts.push(TreeAct::Group(p.clone(), v.clone(), false));
        }
        if ui.button("Filtered documents only").clicked() {
            acts.push(TreeAct::Group(p.clone(), v.clone(), true));
        }
    });
    if ui.button("Count distinct values").on_hover_text("Number of distinct values in this field within the current query").clicked() {
        acts.push(TreeAct::Distinct(db::strip_indices(&p)));
    }
    if ui.button("Create index on this field").clicked() {
        acts.push(TreeAct::Index(p.clone()));
    }
    if editable && !is_id {
        ui.separator();
        if ui.button("Rename field…").clicked() {
            acts.push(TreeAct::Rename(path.to_vec()));
        }
        if ui.button(RichText::new("Delete field").color(ALERT())).clicked() {
            acts.push(TreeAct::Remove(path.to_vec()));
        }
        if ui.button("Add field").on_hover_text("Add a field next to this one").clicked() {
            let parent = match v {
                Bson::Document(_) | Bson::Array(_) => path.to_vec(),
                _ => path[..path.len() - 1].to_vec(),
            };
            acts.push(TreeAct::AddField(parent));
        }
    }
    ui.separator();
    ui.label(RichText::new(format!("Field size: {} bytes", db::field_size(v))).size(12.0).color(FAINT()));
}

/// Bun for the Shell: the path from Settings, else the usual install places. Apps opened from
/// Finder do not get the terminal's PATH, so the common folders are checked directly.
fn find_bun(setting: &str) -> Option<PathBuf> {
    find_exe("bun", setting)
}

/// A program from its Settings path, else from PATH and the usual install folders.
fn find_exe(name: &str, setting: &str) -> Option<PathBuf> {
    if !setting.trim().is_empty() {
        return Some(PathBuf::from(setting.trim()));
    }
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    if let Some(h) = dirs::home_dir() {
        dirs.push(h.join(".bun").join("bin"));
    }
    dirs.push(tools_dir());
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    dirs.into_iter().map(|d| d.join(&file)).find(|p| p.is_file())
}

/// Words the original treats as possibly destructive in the shell (`dangerousCommands`,
/// renderer line 131264: /drop|remove|replace|update|delete|insert|rename|\$out/i).
fn dangerous_words(code: &str) -> Vec<String> {
    let lower = code.to_lowercase();
    ["drop", "remove", "replace", "update", "delete", "insert", "rename", "$out"].iter().filter(|w| lower.contains(*w)).map(|w| w.to_string()).collect()
}

/// The original's "Open query in NodeShell" script (renderer line 96080), with its collection
/// reference rule (getSnippetCollectionRef): names that are not plain words go through db.collection().
fn query_script(coll: &str, filter: &str) -> String {
    let f = filter.trim().trim_end_matches(';');
    if db::is_shell(f) {
        return format!("async function run() {{\n\treturn await {f}\n}}");
    }
    let f = if f.is_empty() { "{}" } else { f };
    format!("async function run() {{\n\treturn await {}.find({f})\n}}", collection_ref(coll))
}

pub(crate) fn collection_ref(coll: &str) -> String {
    if coll.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$') && !coll.starts_with(|c: char| c.is_ascii_digit()) {
        coll.to_string()
    } else {
        format!("db.collection({})", serde_json::Value::String(coll.into()))
    }
}

/// The Shell's reference: how scripts work, every name the runner provides, the main methods and
/// examples that can be loaded into the editor. Returns an example to load, if one was picked.
fn shell_help(ui: &mut egui::Ui, coll: &str, writable: bool) -> Option<String> {
    let mut pick = None;
    let code = |s: &str| RichText::new(s).monospace().size(12.5).color(C_DATE());
    let head = |ui: &mut egui::Ui, s: &str| {
        ui.add_space(4.0);
        ui.label(RichText::new(s).size(11.0).strong().color(MUTED()));
    };
    egui::Frame::new().fill(PANEL()).corner_radius(8).inner_margin(12).show(ui, |ui| {
        ui.set_width(ui.available_width());
        head(ui, "HOW IT WORKS");
        ui.label("Write one function and return what you want to see. The script runs with Bun and the official MongoDB Node.js driver, so its whole API is available.");
        ui.label(code("async function run() {\n  return await orders.find({ status: 'paid' }).toArray();\n}"));
        ui.label(RichText::new("A returned cursor is cut to 100 documents. Logs from console.log/warn/error appear above the result. Cmd+Enter runs, Stop ends a long script.").color(MUTED()));
        if !writable {
            ui.label(RichText::new("This connection is read-only: insert, update, delete, drop, index changes, $out/$merge and other write commands are refused. Unlock writes in the top bar to allow them.").color(SUCCESS()));
        }
        head(ui, "NAMES YOU CAN USE");
        egui::Grid::new("shell-names").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
            for (n, what) in [
                ("db", "the current database (Db): db.collection('name'), db.command({ ping: 1 }), db.listCollections()"),
                ("<collection>", "every collection whose name is a plain word, e.g. orders.find()"),
                ("ObjectId(\"…\")", "an ObjectId, with or without new"),
                ("ISODate(\"…\")", "a Date from an ISO string"),
                ("console.log / warn / error, log", "print to the log"),
                ("randomId(n), UUIDv4(), UUIDv1()", "random ids"),
                ("_ , dayjs", "lodash and dayjs, always available like in the original shell"),
                ("packages", "every package added in Packages is ready by name: date-fns as dateFns, lodash as lodash and _"),
                ("require('pkg'), import('pkg')", "load a package another way, or a part of it"),
            ] {
                ui.label(code(n));
                ui.label(what);
                ui.end_row();
            }
        });
        head(ui, "COLLECTION METHODS");
        ui.label(RichText::new(COLL_METHODS.join("  ·  ")).monospace().size(12.0).color(TEXT()));
        head(ui, "CURSOR METHODS (after find / aggregate)");
        ui.label(RichText::new(CURSOR_METHODS.join("  ·  ")).monospace().size(12.0).color(TEXT()));
        head(ui, "EXAMPLES  ·  click Use to load one");
        let examples = [
            ("Find with a filter", format!("async function run() {{\n  return await {coll}.find({{ status: 'paid' }}).sort({{ _id: -1 }}).limit(20).toArray();\n}}")),
            ("Count", format!("async function run() {{\n  return await {coll}.countDocuments({{}});\n}}")),
            ("Group and count", format!("async function run() {{\n  return await {coll}.aggregate([\n    {{ $group: {{ _id: '$status', n: {{ $sum: 1 }} }} }},\n    {{ $sort: {{ n: -1 }} }}\n  ]).toArray();\n}}")),
            ("Distinct values", format!("async function run() {{\n  return await {coll}.distinct('status');\n}}")),
            ("Loop and log", format!("async function run() {{\n  for await (const d of {coll}.find().limit(5)) {{\n    console.log(d._id);\n  }}\n  return 'done';\n}}")),
            ("By id", format!("async function run() {{\n  return await {coll}.findOne({{ _id: ObjectId(\"\") }});\n}}")),
            ("Use lodash (built in as _)", format!("async function run() {{\n  const docs = await {coll}.find().limit(100).toArray();\n  return _.countBy(docs, 'status');\n}}")),
            ("Update (needs writes unlocked)", format!("async function run() {{\n  return await {coll}.updateMany({{ status: 'pending' }}, {{ $set: {{ status: 'paid' }} }});\n}}")),
        ];
        for (title, ex) in examples {
            ui.horizontal(|ui| {
                if ui.small_button("Use").clicked() {
                    pick = Some(ex.clone());
                }
                ui.label(RichText::new(title).color(BRIGHT()));
            });
        }
        ui.hyperlink_to("MongoDB Node.js driver reference", "https://www.mongodb.com/docs/drivers/node/current/");
    });
    pick
}

fn code_menu(ui: &mut egui::Ui, pick: &mut Option<db::Lang>) {
    ui.menu_button("Copy as code", |ui| {
        for l in db::Lang::ALL {
            if ui.button(l.name()).clicked() {
                *pick = Some(l);
                ui.close();
            }
        }
    });
}

fn default_script(coll: &str) -> String {
    let target = collection_ref(coll);
    format!("async function run() {{\n  return await {target}.find({{}}).limit(20).toArray();\n}}")
}

/// A menu that stays open while you type or click inside it, closing on a click outside or Esc.
/// Plain egui menus close on any click inside, which made their text boxes unusable.
fn sticky_menu(ui: &mut egui::Ui, title: String, content: impl FnOnce(&mut egui::Ui)) {
    use egui::containers::menu::{MenuButton, MenuConfig};
    MenuButton::new(title).config(MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)).ui(ui, content);
}

/// Recent Query, Sort and Project inputs of this collection; picking one fills that box.
/// Like the original's history menu (onHistory, renderer line 96157): each list starts with `{}`
/// to clear the box, and Sort also offers the key of every index of the collection.
fn history_menu(ui: &mut egui::Ui, h: &QueryHist, index_sorts: &[String], acts: &mut Vec<QAct>) {
    sticky_menu(ui, "History".into(), |ui| {
        ui.set_min_width(360.0);
        ui.set_max_width(520.0);
        let mut sort = h.sort.clone();
        for k in index_sorts {
            if !sort.contains(k) {
                sort.push(k.clone());
            }
        }
        let with_clear = |l: &Vec<String>| std::iter::once("{}".to_string()).chain(l.iter().cloned()).collect::<Vec<_>>();
        let (sel, sort, proj) = (with_clear(&h.selector), with_clear(&sort), with_clear(&h.project));
        for (title, list, kind) in [("QUERY", &sel, 0), ("SORT  ·  history and indexes", &sort, 1), ("PROJECT", &proj, 2)] {
            ui.label(RichText::new(title).size(11.0).color(MUTED()));
            egui::ScrollArea::vertical().id_salt(("hist", kind)).max_height(170.0).show(ui, |ui| {
                for v in list {
                    let one_line: String = v.split_whitespace().collect::<Vec<_>>().join(" ");
                    let mut job = json_job(&one_line.chars().take(90).collect::<String>());
                    job.wrap.max_rows = 1;
                    if ui.add(egui::Button::new(job).frame(false)).on_hover_text(v.as_str()).clicked() {
                        acts.push(match kind {
                            0 => QAct::Filter(v.clone()),
                            1 => QAct::Sort(v.clone()),
                            _ => QAct::Project(v.clone()),
                        });
                        ui.close();
                    }
                }
            });
            ui.separator();
        }
    });
}

/// Named queries (or pipelines) saved for this collection: load, delete, or save the current one.
fn library_menu(ui: &mut egui::Ui, lib: &[SavedQuery], new_name: &mut String, pipeline: bool, acts: &mut Vec<QAct>) {
    sticky_menu(ui, format!("Library ({})", lib.len()), |ui| {
        ui.set_min_width(320.0);
        if lib.is_empty() {
            ui.label(RichText::new(if pipeline { "No saved pipelines for this collection yet." } else { "No saved queries for this collection yet." }).color(MUTED()));
        }
        egui::ScrollArea::vertical().id_salt("lib").max_height(260.0).show(ui, |ui| {
            for q in lib {
                ui.horizontal(|ui| {
                    let preview = if pipeline { q.pipeline.clone().unwrap_or_default() } else { q.filter.clone() };
                    let preview: String = preview.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(60).collect();
                    if ui.add(egui::Button::new(RichText::new(&q.name).color(BRIGHT())).frame(false)).on_hover_text(preview.as_str()).clicked() {
                        acts.push(QAct::Load(q.clone()));
                        ui.close();
                    }
                    ui.label(RichText::new(preview).monospace().size(11.0).color(FAINT()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("×").on_hover_text("Delete").clicked() {
                            acts.push(QAct::Delete(q.name.clone()));
                        }
                    });
                });
            }
        });
        ui.separator();
        ui.label(RichText::new(if pipeline { "SAVE THIS PIPELINE" } else { "SAVE THIS QUERY (query, sort and project)" }).size(11.0).color(MUTED()));
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(new_name).hint_text("name").desired_width(200.0));
            let ok = !new_name.trim().is_empty();
            if ui.add_enabled(ok, egui::Button::new("Save")).clicked() || (ok && r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))) {
                acts.push(QAct::Save(new_name.trim().to_string()));
                new_name.clear();
            }
        });
        ui.label(RichText::new("Saving under an existing name replaces it.").size(11.0).color(FAINT()));
    });
}

/// A titled card holding a group of settings.
fn settings_section(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.label(RichText::new(title).size(11.0).strong().color(MUTED()));
    ui.add_space(2.0);
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 6.0;
        add(ui);
    });
    ui.add_space(10.0);
}

/// One setting: its name and a short explanation on the left, the control on the right.
fn setting_row(ui: &mut egui::Ui, title: &str, desc: &str, control: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        let w = ui.available_width();
        ui.vertical(|ui| {
            ui.set_width((w - 230.0).max(160.0));
            ui.label(RichText::new(title).size(14.0).color(BRIGHT()));
            ui.add(egui::Label::new(RichText::new(desc).size(12.0).color(MUTED())).wrap());
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), control);
    });
}

/// "−  n  +" for a small whole number (the control slot is right-to-left, so added in reverse).
fn stepper(ui: &mut egui::Ui, value: &mut usize, min: usize, max: usize) {
    egui::Frame::new().fill(pal().well).corner_radius(8).inner_margin(3).show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        if ui.add_enabled(*value < max, egui::Button::new(RichText::new("+").size(14.0)).corner_radius(6)).clicked() {
            *value += 1;
        }
        ui.add_sized([34.0, 22.0], egui::Label::new(RichText::new(value.to_string()).size(14.0).color(BRIGHT())));
        if ui.add_enabled(*value > min, egui::Button::new(RichText::new("−").size(14.0)).corner_radius(6)).clicked() {
            *value -= 1;
        }
    });
}

/// An on/off switch.
fn toggle(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
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
    ui.painter().rect_filled(rect, 11, bg);
    let x = egui::lerp(rect.left() + 11.0..=rect.right() - 11.0, t);
    // The knob grows a little while pressed, like a macOS switch.
    let knob = if enabled && r.is_pointer_button_down_on() { 9.0 } else { 8.0 };
    ui.painter().circle_filled(egui::pos2(x, rect.center().y), knob, if enabled { Color32::from_gray(0xfa) } else { MUTED() });
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A row of joined buttons where exactly one is chosen.
fn segmented<T: PartialEq + Copy>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)]) {
    egui::Frame::new().fill(pal().well).corner_radius(8).inner_margin(3).show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        // In a right-to-left row, add in reverse so the options still read left to right.
        let rtl = ui.layout().main_dir() == egui::Direction::RightToLeft;
        let order: Vec<&(T, &str)> = if rtl { options.iter().rev().collect() } else { options.iter().collect() };
        for (v, label) in order {
            let on = *value == *v;
            let b = egui::Button::new(RichText::new(*label).size(12.5).color(if on { BG() } else { TEXT() })).fill(state_fill(ui, if on { PRIMARY() } else { Color32::TRANSPARENT })).corner_radius(6);
            if ui.add(b).clicked() {
                *value = *v;
            }
        }
    });
}

/// The original's "optimal" column width (measureColumnWidth, renderer line 31273): the widest
/// cell, kept between 40 and 400 px, plus 10 px. Our cells add 16 px of inner padding on top.
fn optimal_width(ui: &egui::Ui, docs: &[Document], col: &str, summarize: bool) -> f32 {
    if docs.is_empty() {
        return 100.0 + CELL_PAD; // the original's fallback width
    }
    let widest = ui.fonts_mut(|f| {
        let head = f.layout_no_wrap(col.to_string(), FontId::proportional(12.5), TEXT()).size().x;
        docs.iter()
            .filter_map(|d| db::get_path(d, col))
            .map(|v| f.layout_job(column_job(v, summarize)).size().x)
            .fold(head, f32::max)
    });
    widest.clamp(OPTIMAL_MIN_W, OPTIMAL_MAX_W) + OPTIMAL_PAD + CELL_PAD
}

fn visible_cols(p: Option<&ColPrefs>, fields: &[String], n: usize) -> Vec<String> {
    let base: Vec<String> = match p.and_then(|p| p.shown.clone()) {
        Some(v) => v,
        None => fields.iter().take(n).cloned().collect(),
    };
    let pinned: Vec<String> = p.map(|p| p.pinned.clone()).unwrap_or_default();
    let mut out: Vec<String> = pinned.iter().filter(|c| base.contains(c)).cloned().collect();
    out.extend(base.into_iter().filter(|c| !pinned.contains(c)));
    out
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, shot: Option<PathBuf>) -> Self {
        style(&cc.egui_ctx);
        let (tx, rx) = channel();
        // QUARRY_DEMO=1 opens the offline demo to click around in, without screenshot mode closing it.
        let demo = shot.is_some() || std::env::var("QUARRY_DEMO").is_ok();
        let mut app = Self {
            worker: Worker::new(tx, cc.egui_ctx.clone()),
            rx,
            conns: Vec::new(),
            active: None,
            open_dbs: HashSet::new(),
            tabs: Vec::new(),
            cur: 0,
            req: 0,
            json_edit: None,
            drafts: HashMap::new(),
            saving: HashMap::new(),
            changed: HashSet::new(),
            inline: None,
            new_field: None,
            fold: None,
            fold_busy: false,
            key_w: 0.0,
            char_w: 0.0,
            tree_x: egui::Rangef::NOTHING,
            insert: None,
            dropping_db: None,
            prefs: UiPrefs::default(),
            new_col: String::new(),
            col_filter: String::new(),
            // Screenshot runs stay off the real menu bar.
            #[cfg(target_os = "macos")]
            menu: (!demo || std::env::var("QUARRY_DEMO").is_ok()).then(|| crate::menu::MenuBar::new(&cc.egui_ctx)),
            #[cfg(target_os = "macos")]
            menu_acts: Vec::new(),
            new_pkg: String::new(),
            new_lib: String::new(),
            views: HashSet::new(),
            users: None,
            conn_test: None,
            idx_keys: String::new(),
            idx_unique: false,
            idx_info: None,
            undo: Vec::new(),
            explain_req: 0,
            modal: None,
            status: ("Ready. All connections start read-only.".into(), false),
            toast: None,
            ac: Ac::default(),
            status_at: std::time::Instant::now(),
            log: LogUi::default(),
            tree_filter: String::new(),
            tools_installing: false,
            reveal: None,
            scroll_sel: false,
            demo,
            tool: None,
            width_cache: HashMap::new(),
            frame_ms: 0.0,
            closed: Vec::new(),
            updated: None,
            update_note: String::new(),
            update_checked: None,
            last_input: std::time::Instant::now(),
            started: std::time::Instant::now(),
            shot: shot.map(|p| (p, 0, Arc::new(Mutex::new(None)))),
        };
        // Test fixture: QUARRY_TEST_JSON=<file of Extended JSON documents> makes screenshot mode
        // show those documents through the same message path real query results take.
        let fixture = std::env::var("QUARRY_TEST_JSON").ok().filter(|_| demo);
        if let Some(path) = fixture {
            app.demo = false;
            let docs: Vec<Document> = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| db::parse_value(&t).ok())
                .and_then(|b| match b {
                    Bson::Array(a) => Some(a.into_iter().filter_map(|d| d.as_document().cloned()).collect()),
                    _ => None,
                })
                .unwrap_or_default();
            let sel = Sel { conn: 1, db: "fixture".into(), coll: "documents".into() };
            app.conns.push(Conn { meta: ConnMeta { id: 1, name: "Fixture".into(), ..Default::default() }, client: None, status: Status::Ready, writable: false, dbs: vec![("fixture".into(), Some(vec!["documents".into()]))], masked_uri: String::new() , tunnel: None });
            app.active = Some(1);
            app.open_dbs.insert((1, "fixture".into()));
            app.open_collection(sel, false);
            app.req += 1;
            app.tabs[0].req = app.req;
            let n = docs.len() as u64;
            app.worker.send(Msg::Docs { req: app.req, res: Ok((docs, n, 3)) });
        } else if demo {
            app.load_demo();
            // Test switches: the demo picture in the light theme, and with the find bar open.
            if std::env::var("QUARRY_TEST_LIGHT").is_ok() {
                Appearance::Light.apply(&cc.egui_ctx);
            }
            if let Ok(q) = std::env::var("QUARRY_TEST_FIND") {
                let f = Find { open: true, query: q, ..Default::default() };
                cc.egui_ctx.data_mut(|d| d.insert_temp(egui::Id::unique("json-editor-text").with("find"), f));
            }
        } else {
            crate::update::cleanup();
            let (metas, broken_conns) = parse_config::<Vec<ConnMeta>>(&config_dir().join("connections.json"));
            let ui_json = config_dir().join("ui.json").exists().then_some(());
            let (prefs, broken_prefs) = parse_config(&config_dir().join("ui.json"));
            app.prefs = prefs;
            app.prefs.appearance.apply(&cc.egui_ctx);
            for meta in metas {
                let masked = keychain(meta.id).and_then(|k| k.get_password().ok()).map(|u| db::mask_uri(&u)).unwrap_or_default();
                app.conns.push(Conn { meta, client: None, status: Status::Idle, writable: false, dbs: Vec::new(), masked_uri: masked , tunnel: None });
            }
            app.active = app.conns.first().map(|c| c.meta.id);
            // Reopen the tabs of the last session; they load once their connection is connected.
            for st in app.prefs.open_tabs.clone() {
                if app.conn(st.conn).is_none() {
                    continue;
                }
                let mut t = Tab::new(Sel { conn: st.conn, db: st.db, coll: st.coll });
                t.view = match st.view.as_str() {
                    "Aggregate" => View::Aggregate,
                    "Indexes" => View::Indexes,
                    "Shell" => View::Shell,
                    "Schema" => View::Schema,
                    "Import" => View::Import,
                    "Compare" => View::Compare,
                    _ => View::Documents,
                };
                (t.filter_txt, t.sort_txt, t.proj_txt, t.shell.code) = (st.filter, st.sort, st.project, st.shell);
                if !st.agg.is_empty() {
                    t.agg_txt = st.agg;
                }
                t.agg_text_mode = st.agg_text_mode;
                let stages: Vec<Stage> = st.stages.iter().map(|s| Stage::new(&s.op, &s.text, s.on)).collect();
                if !stages.is_empty() && text_from_stages(&stages) == t.agg_txt {
                    t.stages_src = t.agg_txt.clone();
                    t.stages = stages;
                }
                app.tabs.push(t);
            }
            app.cur = app.prefs.cur_tab.min(app.tabs.len().saturating_sub(1));
            if let Some(t) = app.tabs.get(app.cur) {
                app.active = Some(t.sel.conn);
            }
            let current = env!("CARGO_PKG_VERSION");
            if let Some(id) = app.prefs.last_conn.filter(|&id| app.conn(id).is_some()) {
                app.active = Some(id);
            }
            if let Some(id) = app.active {
                app.connect(id);
            }
            if app.prefs.last_version != current {
                // A fresh install has no ui.json, so it skips the notes. Settings from 0.2.0 and
                // earlier lack last_version, so updating from those shows them.
                if ui_json.is_some()
                    && let Some(body) = crate::update::changelog(current)
                {
                    app.modal = Some(Modal::Text { title: format!("What's new in Quarry {current}"), body, code: false });
                }
                app.prefs.last_version = current.into();
                app.save_prefs();
            }
            let broken: Vec<String> = [broken_conns, broken_prefs].into_iter().flatten().collect();
            if !broken.is_empty() {
                app.modal = Some(Modal::Text { title: "Some settings could not be loaded".into(), body: broken.join("\n\n"), code: false });
            }
        }
        app
    }

    fn write_config(&mut self, file: &str, bytes: Vec<u8>) {
        if self.demo {
            return;
        }
        let dir = config_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Err(err) = std::fs::write(dir.join(file), bytes) {
            self.err(format!("Could not save {file}: {err}"));
        }
    }

    fn save_conns(&mut self) {
        let metas: Vec<&ConnMeta> = self.conns.iter().map(|c| &c.meta).collect();
        let b = serde_json::to_vec_pretty(&metas).unwrap_or_default();
        self.write_config("connections.json", b);
    }

    fn save_prefs(&mut self) {
        let b = serde_json::to_vec_pretty(&self.prefs).unwrap_or_default();
        self.write_config("ui.json", b);
    }

    fn ok(&mut self, s: impl Into<String>) {
        self.status = (s.into(), false);
        self.status_at = std::time::Instant::now();
    }

    fn err(&mut self, s: impl Into<String>) {
        self.status = (s.into(), true);
        self.status_at = std::time::Instant::now();
    }

    fn conn(&self, id: u64) -> Option<&Conn> {
        self.conns.iter().find(|c| c.meta.id == id)
    }

    fn t(&self) -> Option<&Tab> {
        self.tabs.get(self.cur)
    }

    fn tm(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.cur)
    }

    fn sel_is_view(&self) -> bool {
        self.t().is_some_and(|t| self.views.contains(&(t.sel.conn, t.sel.db.clone(), t.sel.coll.clone())))
    }

    fn sel_writable(&self) -> bool {
        if self.sel_is_view() {
            return false;
        }
        self.t().and_then(|t| self.conn(t.sel.conn)).is_some_and(|c| c.writable)
    }

    fn timeout(&self) -> Duration {
        Duration::from_millis(self.prefs.timeout_ms)
    }

    fn target_for(&self, s: &Sel) -> Option<Target> {
        let client = self.conn(s.conn)?.client.clone()?;
        Some(Target { client, db: s.db.clone(), coll: s.coll.clone(), timeout: self.timeout() })
    }

    fn target(&self) -> Option<Target> {
        self.target_for(&self.t()?.sel)
    }

    /// The single gate every write goes through.
    fn writable_target(&mut self) -> Option<(u64, Target)> {
        let conn = self.t()?.sel.conn;
        if !self.sel_writable() {
            self.err("This connection is read-only. Unlock writes in the top bar first.");
            return None;
        }
        match self.target() {
            Some(t) => Some((conn, t)),
            None => {
                self.err("Not connected.");
                None
            }
        }
    }

    fn ask_unlock(&mut self) {
        if let Some(t) = self.t() {
            self.modal = Some(Modal::Unlock(t.sel.conn, None));
        }
    }

    /// Asks to allow writes, then runs `then` if they were allowed.
    fn ask_unlock_then(&mut self, then: impl FnOnce(&mut App) + 'static) {
        if let Some(t) = self.t() {
            self.modal = Some(Modal::Unlock(t.sel.conn, Some(Box::new(then))));
        }
    }

    fn connect(&mut self, id: u64) {
        let uri = match stored_uri(id) {
            Ok(u) => u,
            Err(e) => return self.err(uri_err(e)),
        };
        let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == id) else { return };
        c.status = Status::Connecting;
        c.client = None;
        c.tunnel = None;
        let net = net_of(&c.meta);
        self.worker.connect(id, uri, self.timeout(), net);
    }

    fn col_key(&self, agg: bool) -> String {
        let s = self.t().map(|t| format!("{}/{}/{}", t.sel.conn, t.sel.db, t.sel.coll)).unwrap_or_default();
        match (agg, self.t().map(|t| t.view)) {
            (true, Some(View::Shell)) => format!("{s}#shell"),
            (true, Some(View::Import)) => format!("{s}#import"),
            (true, _) => format!("{s}#agg"),
            _ => s,
        }
    }

    fn reset_doc_state(&mut self) {
        self.json_edit = None;
        self.inline = None;
        self.new_field = None;
    }

    /// Opens a collection. Like the original (treeOpenInNewTab), a new tab is used unless one
    /// already shows that collection; `replace` reuses the current tab instead.
    fn open_collection(&mut self, sel: Sel, replace: bool) {
        self.reset_doc_state();
        if let Some(i) = self.tabs.iter().position(|t| t.sel == sel) {
            self.cur = i;
            return;
        }
        if replace && !self.tabs.is_empty() {
            let view = self.tabs[self.cur].view;
            self.tabs[self.cur] = Tab { view, ..Tab::new(sel) };
        } else {
            self.tabs.push(Tab::new(sel));
            self.cur = self.tabs.len() - 1;
        }
        self.reload();
    }

    /// Keeps the 10 most recently closed tabs for "Reopen closed tab" (maxClosedTabsCount,
    /// renderer line 197320).
    fn remember_closed(&mut self, t: Tab) {
        self.closed.insert(0, t);
        self.closed.truncate(10);
    }

    fn reopen_tab(&mut self) {
        if self.closed.is_empty() {
            return;
        }
        let t = self.closed.remove(0);
        self.tabs.push(t);
        self.cur = self.tabs.len() - 1;
        self.reset_doc_state();
        self.reload();
    }

    fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        let t = self.tabs.remove(i);
        self.remember_closed(t);
        if self.cur > i || self.cur >= self.tabs.len() {
            self.cur = self.cur.saturating_sub(1);
        }
        self.reset_doc_state();
    }

    fn run_query(&mut self) {
        let Some(t) = self.t() else { return };
        if db::is_shell(&t.filter_txt) {
            let text = t.filter_txt.clone();
            return self.run_shell(&text);
        }
        let parse = |s: &str| -> db::Res<Option<Document>> { if s.trim().is_empty() { Ok(None) } else { db::parse_doc(s).map(Some) } };
        let q = (|| -> db::Res<Query> {
            Ok(Query {
                filter: db::parse_doc(&t.filter_txt).map_err(|e| format!("Filter: {e}"))?,
                sort: parse(&t.sort_txt).map_err(|e| format!("Sort: {e}"))?,
                projection: parse(&t.proj_txt).map_err(|e| format!("Projection: {e}"))?,
                ..Default::default()
            })
        })();
        match q {
            Ok(q) => {
                let (f, so, p) = (t.filter_txt.clone(), t.sort_txt.clone(), t.proj_txt.clone());
                let key = self.col_key(false);
                let h = self.prefs.history.entry(key).or_default();
                push_history(&mut h.selector, &f);
                push_history(&mut h.sort, &so);
                push_history(&mut h.project, &p);
                self.save_prefs();
                let t = self.tm().unwrap();
                t.query = q;
                t.page = 0;
                self.reload();
            }
            Err(e) => self.err(e),
        }
    }

    /// Opens the current query or pipeline as a snippet for one language.
    fn show_code(&mut self, lang: db::Lang, find: bool) {
        let Some(t) = self.t() else { return };
        let from_shell = |s: &str| db::parse_shell(s).map(|sh| match sh.kind {
            db::ShellKind::Find(q) => db::CodeOf::Find(q),
            db::ShellKind::Aggregate(p) => db::CodeOf::Aggregate(p),
        });
        let opt = |s: &str, what: &str| if s.trim().is_empty() { Ok(None) } else { db::parse_doc(s).map(Some).map_err(|e| format!("{what}: {e}")) };
        let what = if find && !db::is_shell(&t.filter_txt) {
            (|| Ok(db::CodeOf::Find(Query { filter: db::parse_doc(&t.filter_txt).map_err(|e| format!("Filter: {e}"))?, sort: opt(&t.sort_txt, "Sort")?, projection: opt(&t.proj_txt, "Projection")?, ..Default::default() })))()
        } else if find {
            from_shell(&t.filter_txt)
        } else if db::is_shell(&t.agg_txt) {
            from_shell(&t.agg_txt)
        } else {
            db::parse_pipeline(&t.agg_txt).map(db::CodeOf::Aggregate)
        };
        match what {
            Ok(w) => self.modal = Some(Modal::Text { title: format!("{} code", lang.name()), body: db::driver_code(lang, &t.sel.db, &t.sel.coll, &w), code: false }),
            Err(e) => self.err(e),
        }
    }

    /// Runs a pasted `db.<coll>.find(...)` or `db.<coll>.aggregate([...])`, switching collection if needed.
    fn run_shell(&mut self, text: &str) {
        let sh = match db::parse_shell(text) {
            Ok(s) => s,
            Err(e) => return self.err(e),
        };
        let Some(cur) = self.t().map(|t| t.sel.clone()) else { return self.err("Open a collection first.") };
        if sh.coll != cur.coll {
            self.open_collection(Sel { coll: sh.coll.clone(), ..cur }, true);
        }
        let t = self.tm().unwrap();
        t.selected = None;
        match sh.kind {
            ShellKind::Find(q) => {
                t.view = View::Documents;
                t.filter_txt = text.trim().to_string();
                t.sort_txt.clear();
                t.proj_txt.clear();
                t.query = q;
                t.page = 0;
                self.reload();
            }
            ShellKind::Aggregate(p) => {
                t.view = View::Aggregate;
                t.agg_txt = text.trim().to_string();
                self.run_pipeline(p);
            }
        }
    }

    /// Explains the Documents query or the Aggregate pipeline, whichever view is open.
    fn explain(&mut self) {
        let Some(t) = self.t() else { return };
        let what = match t.view {
            View::Documents if !db::is_shell(&t.filter_txt) => Ok(db::ExplainOf::Find(t.query.clone())),
            View::Aggregate if !db::is_shell(&t.agg_txt) => db::parse_pipeline(&t.agg_txt).and_then(|p| if db::has_write_stage(&p) { Err("Explain would run the write stage.".into()) } else { Ok(db::ExplainOf::Aggregate(p)) }),
            View::Documents | View::Aggregate => Err("Run the pasted query first, then explain it.".into()),
            _ => return,
        };
        match (what, self.target()) {
            _ if self.demo => self.ok("Offline demo: nothing to explain."),
            (Err(e), _) => self.err(e),
            (Ok(w), Some(target)) => {
                self.req += 1;
                self.explain_req = self.req;
                self.worker.explain(self.req, target, w);
            }
            _ => self.err("Not connected."),
        }
    }

    fn run_pipeline(&mut self, p: Vec<Document>) {
        self.tm().unwrap().agg_stage = None;
        if db::has_write_stage(&p) {
            if self.writable_target().is_some() {
                self.modal = Some(Modal::AggWrite(p));
            }
        } else if self.demo {
            self.ok("Offline demo: nothing to run.");
        } else {
            match self.target() {
                Some(target) => {
                    self.req += 1;
                    let req = self.req;
                    self.tm().unwrap().agg_req = req;
                    self.worker.aggregate(req, target, p);
                }
                None => self.err("Not connected."),
            }
        }
    }

    fn load_indexes(&mut self) {
        if let Some(target) = self.target() {
            self.req += 1;
            let req = self.req;
            self.tm().unwrap().idx_req = req;
            // Only the Indexes view shows usage, and it costs a profiler query.
            if self.t().is_some_and(|t| t.view == View::Indexes) {
                self.worker.index_info(target.clone());
            }
            self.worker.indexes(req, target);
        }
    }

    /// Asks for the preview of every referenced id on the page, like the original's
    /// relatedDocuments (renderer line 107207).
    fn load_related(&mut self, req: u64) {
        let Some(t) = self.tabs.iter().find(|t| t.req == req) else { return };
        let Some(client) = self.conn(t.sel.conn).and_then(|c| c.client.clone()) else { return };
        let rels = self.prefs.relations.get(&format!("{}/{}", t.sel.conn, t.sel.db)).into_iter().flatten().filter(|r| r.coll == t.sel.coll && !r.preview.is_empty());
        for r in rels {
            let ids = db::values_at(&t.docs, &r.path);
            if !ids.is_empty() {
                self.worker.related(req, Target { client: client.clone(), db: t.sel.db.clone(), coll: r.target.clone(), timeout: self.timeout() }, ids, r.preview.clone());
            }
        }
    }

    /// Opens the referenced document in its collection's tab.
    fn go_ref(&mut self, conn: u64, db: String, coll: String, id: Bson) {
        self.open_collection(Sel { conn, db, coll }, false);
        let t = self.tm().unwrap();
        t.view = View::Documents;
        t.filter_txt = format!("{{ _id: {} }}", db::shell_text(&id));
        t.sort_txt.clear();
        t.proj_txt.clear();
        self.run_query();
    }

    fn analyze_relations(&mut self, conn: u64, client: Client, db: String, coll: String) {
        let all: Vec<String> = self.conn(conn).and_then(|c| c.dbs.iter().find(|d| d.0 == db)).and_then(|d| d.1.clone()).unwrap_or_default().into_iter().filter(|c| !c.starts_with("system.") && !self.views.contains(&(conn, db.clone(), c.clone()))).collect();
        let colls = if coll.is_empty() { all.clone() } else { vec![coll.clone()] };
        self.ok(format!("Analyzing data relations of {}…", if coll.is_empty() { db.clone() } else { format!("{db}.{coll}") }));
        self.worker.analyze_relations(conn, Target { client, db, coll, timeout: self.timeout() }, colls, all);
    }

    /// Something a restart would lose: unsaved edits, writes in flight, running scripts or tools.
    fn busy(&self) -> bool {
        !self.drafts.is_empty()
            || !self.saving.is_empty()
            || self.json_edit.is_some()
            || self.insert.is_some()
            || self.inline.is_some()
            || self.new_field.is_some()
            || self.modal.is_some()
            || self.tool.as_ref().is_some_and(|t| t.done.is_none())
            || self.tabs.iter().any(|t| t.shell.cancel.is_some() || t.compare.running || t.loading)
    }

    /// Checks GitHub 5 seconds after launch and then daily, like the original (main process
    /// setTimeout 5 s and setInterval 24 h, main.js line 33757); restarts into an update once idle.
    fn auto_update(&mut self, ctx: &egui::Context) {
        if self.demo || cfg!(debug_assertions) {
            return;
        }
        if ctx.input(|i| !i.events.is_empty() || i.pointer.is_moving()) {
            self.last_input = std::time::Instant::now();
        }
        const FIRST_CHECK_S: u64 = 5;
        const CHECK_EVERY_S: u64 = 24 * 3600;
        let due = match self.update_checked {
            None => self.started.elapsed().as_secs() >= FIRST_CHECK_S,
            Some(t) => t.elapsed().as_secs() >= CHECK_EVERY_S,
        };
        if self.update_checked.is_none() && self.prefs.auto_update {
            ctx.request_repaint_after(std::time::Duration::from_secs(FIRST_CHECK_S));
        }
        if self.prefs.auto_update && self.updated.is_none() && due {
            self.check_update();
        }
        if self.updated.is_some() && self.prefs.auto_update {
            if self.last_input.elapsed().as_secs() >= 60 && !self.busy() {
                crate::update::restart();
            }
            // Wake up to test idleness even when nothing else repaints.
            ctx.request_repaint_after(std::time::Duration::from_secs(5));
        }
    }

    /// Menu items without a keyboard twin.
    #[cfg(target_os = "macos")]
    fn menu_act(&mut self, ctx: &egui::Context, a: crate::menu::Act) {
        use crate::menu::Act;
        match a {
            Act::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            // Same rule as Settings' "Check now": dev builds and the demo never install releases.
            Act::CheckUpdates => {
                if !self.demo && !cfg!(debug_assertions) && self.updated.is_none() {
                    self.check_update();
                }
                self.modal = Some(Modal::Settings);
            }
            Act::Export if !self.tabs.is_empty() => self.export(),
            Act::Insert if !self.tabs.is_empty() => self.insert = Some("{\n  \n}".into()),
            Act::Url(u) => ctx.open_url(egui::OpenUrl::new_tab(u)),
            _ => {}
        }
    }

    fn check_update(&mut self) {
        self.update_checked = Some(std::time::Instant::now());
        self.update_note = "Checking for updates…".into();
        self.worker.blocking(|| Msg::Update(crate::update::check_and_install()));
        // A running dump or restore keeps its program open; Windows cannot replace it then.
        if self.tool.as_ref().is_none_or(|t| t.done.is_some()) {
            self.worker.blocking(|| Msg::ToolsInstalled(crate::update::update_db_tools(&tools_dir())));
        }
    }

    fn install_tools(&mut self) {
        if !self.tools_installing {
            self.tools_installing = true;
            self.worker.blocking(|| Msg::ToolsInstalled(crate::update::install_db_tools(&tools_dir()).map(Some)));
        }
    }

    /// The Install button, or a spinner while the download runs.
    fn install_tools_button(&mut self, ui: &mut egui::Ui) {
        if self.tools_installing {
            ui.horizontal(|ui| {
                spin(ui);
                ui.label(RichText::new("Downloading the MongoDB Database Tools…").color(MUTED()));
            });
        } else if ui.button("Install the MongoDB Database Tools").on_hover_text(format!("Downloads MongoDB's official build into {}", tools_dir().display())).clicked() {
            self.install_tools();
        }
    }

    fn start_tool(&mut self, restore: bool, title: String, args: Vec<String>) {
        let (name, setting) = if restore { ("mongorestore", &self.prefs.mongorestore) } else { ("mongodump", &self.prefs.mongodump) };
        let Some(exe) = find_exe(name, setting) else {
            return self.err(format!("{name} was not found. Install the MongoDB Database Tools from Settings or set its path there."));
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tool = Some(ToolRun { title, log: Vec::new(), cancel: Some(tx), done: None });
        self.worker.tool(exe, args, rx);
    }

    fn reload(&mut self) {
        if self.demo {
            return;
        }
        let Some(target) = self.target() else { return };
        // Each answer is matched to its tab by id; a slower older one never overwrites a newer one.
        self.req += 1;
        let req = self.req;
        let t = self.tm().unwrap();
        t.req = req;
        t.loading = true;
        let (q, page, view, no_idx) = (t.query.clone(), t.page, t.view, t.indexes.is_empty());
        self.worker.find(req, target, q, page);
        // Indexes also feed the Sort history (as in the original), so load them once per tab.
        if view == View::Indexes || no_idx {
            self.load_indexes();
        }
    }

    fn push_undo(&mut self, a: UndoAction) {
        self.undo.insert(0, a);
        self.undo.retain(|a| a.at.elapsed().as_secs() <= db::UNDO_MAX_AGE_S);
        self.undo.truncate(db::UNDO_MAX);
    }

    fn apply_undo(&mut self, i: usize) {
        let a = &self.undo[i];
        let Some(c) = self.conn(a.conn) else { return };
        if !c.writable {
            self.err("Undo is a write: unlock writes on that connection first.");
            return;
        }
        let Some(client) = c.client.clone() else {
            self.err("That connection is closed.");
            return;
        };
        let a = self.undo.remove(i);
        let t = Target { client, db: a.db, coll: a.coll, timeout: self.timeout() };
        match a.kind {
            UndoKind::Insert { id } => self.worker.delete_by_id(t, id),
            UndoKind::InsertMany { ids } => self.worker.delete_ids(t, ids),
            UndoKind::Delete { old } => self.worker.insert(a.conn, t, old, None),
            UndoKind::DeleteMany { old } => self.worker.import(a.conn, t, old, None),
            UndoKind::Update { old, new } => self.worker.replace(a.conn, t, new, old, false),
        }
    }

    fn tab_by_req(&mut self, req: u64) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.req == req)
    }

    fn poll(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Connected { conn, res } => {
                    let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) else { continue };
                    match res {
                        Ok((client, dbs, tunnel)) => {
                            // Trust the SSH host key the first time; later a different key is refused.
                            let mut trusted = None;
                            if let (Some(t), Some(sc)) = (&tunnel, c.meta.ssh.as_mut()) {
                                if sc.known_host.is_none() && !t.fingerprint.is_empty() {
                                    sc.known_host = Some(t.fingerprint.clone());
                                    trusted = Some(t.fingerprint.clone());
                                }
                            }
                            c.tunnel = tunnel;
                            // Load every database's collections up front so the list can show counts.
                            for d in &dbs {
                                self.worker.collections(conn, client.clone(), d.clone());
                            }
                            c.client = Some(client);
                            c.status = Status::Ready;
                            c.dbs = dbs.into_iter().map(|d| (d, None)).collect();
                            let n = c.meta.name.clone();
                            self.ok(format!("Connected to {n} (read-only)"));
                            if let Some(fp) = trusted {
                                self.save_conns();
                                self.ok(format!("Connected to {n} through SSH (read-only). Host key remembered: {fp}"));
                            }
                            let here = self.cur;
                            for i in 0..self.tabs.len() {
                                if self.tabs[i].sel.conn == conn && self.tabs[i].docs.is_empty() {
                                    self.cur = i;
                                    self.run_query();
                                }
                            }
                            self.cur = here;

                        }
                        Err(e) => {
                            c.status = Status::Failed(e.clone());
                            self.err(e);
                        }
                    }
                }
                Msg::Collections { conn, db, res } => match res {
                    Ok((v, views)) => {
                        self.views.retain(|(c, d, _)| !(*c == conn && *d == db));
                        self.views.extend(views.into_iter().map(|n| (conn, db.clone(), n)));
                        if let Some(d) = self.conns.iter_mut().find(|c| c.meta.id == conn).and_then(|c| c.dbs.iter_mut().find(|d| d.0 == db)) {
                            d.1 = Some(v);
                        }
                    }
                    Err(e) => self.err(format!("Could not list the collections of {db}: {e}")),
                },
                Msg::Command { tag, res } => match (tag.as_str(), res) {
                    ("users", r) => {
                        if let Some(u) = &mut self.users {
                            u.users = Some(r.map(|d| d.get_array("users").map(|a| a.iter().filter_map(|x| x.as_document().cloned()).collect()).unwrap_or_default()));
                        }
                    }
                    ("roles", r) => {
                        if let Some(u) = &mut self.users {
                            u.roles = Some(r.map(|d| d.get_array("roles").map(|a| a.iter().filter_map(|x| x.as_document().cloned()).collect()).unwrap_or_default()));
                        }
                    }
                    ("stats", Ok(d)) => {
                        let title = format!("Statistics of {}", d.get_str("collection").unwrap_or_default());
                        let mut lines = Vec::new();
                        for (k, v) in d.iter().filter(|(k, _)| *k != "collection") {
                            let n = match v {
                                Bson::Int32(n) => Some(*n as f64),
                                Bson::Int64(n) => Some(*n as f64),
                                Bson::Double(n) => Some(*n),
                                _ => None,
                            };
                            let text = match (k.as_str(), n) {
                                ("size" | "avgObjSize" | "storageSize" | "totalIndexSize", Some(n)) => human_bytes(n as usize),
                                (_, Some(n)) => group(&(n as i64).to_string()),
                                ("indexSizes", _) => v.as_document().map(|d| d.iter().map(|(i, s)| format!("{i}: {}", human_bytes(match s { Bson::Int32(n) => *n as usize, Bson::Int64(n) => *n as usize, Bson::Double(n) => *n as usize, _ => 0 }))).collect::<Vec<_>>().join(", ")).unwrap_or_default(),
                                _ => db::value_text(v),
                            };
                            lines.push(format!("{k}: {text}"));
                        }
                        self.modal = Some(Modal::Text { title, body: lines.join("\n"), code: false });
                    }
                    ("viewdef", Ok(d)) => {
                        let opts = d.get_document("cursor").ok().and_then(|c| c.get_array("firstBatch").ok()).and_then(|a| a.first()).and_then(|x| x.as_document()).and_then(|x| x.get_document("options").ok()).cloned().unwrap_or_default();
                        let body = format!("viewOn: {}\npipeline:\n{}", opts.get_str("viewOn").unwrap_or_default(), db::to_edit_value(opts.get("pipeline").unwrap_or(&Bson::Array(vec![]))));
                        self.modal = Some(Modal::Text { title: "View definition".into(), body, code: true });
                    }
                    (tag, Ok(d)) if tag.starts_with("validator:") => {
                        let req: u64 = tag["validator:".len()..].parse().unwrap_or(0);
                        let opts = d.get_document("cursor").ok().and_then(|c| c.get_array("firstBatch").ok()).and_then(|a| a.first()).and_then(|x| x.as_document()).and_then(|x| x.get_document("options").ok()).cloned().unwrap_or_default();
                        if let Some(t) = self.tabs.iter_mut().find(|t| t.schema.vreq == req) {
                            let s = &mut t.schema;
                            s.vtext = opts.get_document("validator").map(db::to_edit_json).unwrap_or_default();
                            s.vlevel = match opts.get_str("validationLevel") {
                                Ok("off") => 0,
                                Ok("moderate") => 1,
                                _ => 2,
                            };
                            s.vwarn = opts.get_str("validationAction") == Ok("warn");
                        }
                    }
                    (_, Err(e)) => self.err(e),
                    _ => {}
                },
                Msg::DbChanged { conn, db, res } => {
                    let dropped = self.dropping_db.take_if(|d| d.0 == conn && d.1 == db);
                    match res {
                        Ok(m) => self.ok(m),
                        Err(e) => {
                            self.err(e);
                            if dropped.is_some() {
                                if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
                                    c.dbs.push((db.clone(), None));
                                    c.dbs.sort_by(|a, b| a.0.cmp(&b.0));
                                }
                            }
                        }
                    }
                    // The server may normalise the validator, so show what it kept.
                    for t in self.tabs.iter_mut().filter(|t| t.sel.conn == conn && t.sel.db == db) {
                        t.schema.vloaded = false;
                    }
                    if let Some(client) = self.conn(conn).and_then(|c| c.client.clone()) {
                        if db == "*" {
                            self.connect(conn);
                        } else {
                            self.worker.collections(conn, client, db);
                        }
                    }
                    if self.users.as_ref().is_some_and(|u| u.conn == conn) {
                        self.load_users(conn);
                    }
                    // Hiding an index or switching the profiler changes what the Indexes view shows.
                    if self.t().is_some_and(|t| t.view == View::Indexes) {
                        self.load_indexes();
                    }
                }
                Msg::IndexInfo { key, res } => match res {
                    Ok(i) => self.idx_info = Some((key, i)),
                    Err(e) => self.err(e),
                },
                Msg::Docs { req, res } => {
                    let Some(t) = self.tab_by_req(req) else { continue };
                    t.loading = false;
                    match res {
                        Ok((docs, count, ms)) => {
                            // A query that works again clears an earlier error.
                            if self.status.1 {
                                self.status = (format!("Loaded {} documents", docs.len()), false);
                            }
                            let t = self.tab_by_req(req).unwrap();
                            t.fields = auto_fields(&docs);
                            t.paths = doc_paths(&docs);
                            t.date_paths = date_paths(&docs, &t.paths);
                            // Checks and open rows follow their documents by _id, so actions that reload
                            // (duplicate, filter, edits) keep them; rows that left the results drop out.
                            if !t.checked.is_empty() {
                                t.checked = follow_ids(&t.docs, &docs, t.checked.iter().copied());
                                t.check_anchor = None;
                            }
                            // The open document follows its _id too; rows without one keep their position.
                            if let Some((false, i)) = t.selected {
                                if t.docs.get(i).is_some_and(|d| d.contains_key("_id")) {
                                    t.selected = follow_ids(&t.docs, &docs, std::iter::once(i)).first().map(|&j| (false, j));
                                }
                            }
                            let open = follow_ids(&t.docs, &docs, t.expanded.iter().filter(|e| !e.0).map(|e| e.1));
                            t.expanded.retain(|e| e.0);
                            t.expanded.extend(open.into_iter().map(|i| (false, i)));
                            t.docs = docs;
                            t.count = count;
                            t.ms = ms;
                            if matches!(t.selected, Some((false, i)) if i >= t.docs.len()) {
                                t.selected = None;
                            }
                            t.related.clear();
                            self.load_related(req);
                        }
                        Err(e) => {
                            t.docs.clear();
                            self.err(e);
                        }
                    }
                }
                Msg::Indexes { req, res } => match (res, self.tabs.iter_mut().find(|t| t.idx_req == req)) {
                    (Ok(v), Some(t)) => t.indexes = v,
                    (Err(e), _) => self.err(e),
                    _ => {}
                },
                Msg::Explained { req, res } if req == self.explain_req => match res {
                    Ok(raw) => self.modal = Some(Modal::Explain { summary: db::explain_summary(&raw), raw: db::to_edit_json(&raw), show_raw: false }),
                    Err(e) => self.err(e),
                },
                Msg::Explained { .. } => {}
                Msg::UpdatePreview { stages, res } => {
                    if let Some(Modal::UpdateMany { preview, .. }) = &mut self.modal {
                        *preview = Some((stages, res));
                    }
                }
                Msg::StagePreview { req, res } => {
                    let Some(st) = self.tabs.iter_mut().flat_map(|t| std::iter::once(&mut t.agg_input).chain(t.stages.iter_mut())).find(|s| s.req == req) else { continue };
                    st.req = 0;
                    match res {
                        Ok((n, docs, ms)) => {
                            // Rows stay open when a refresh brings back the same documents.
                            if st.sample != docs {
                                st.open.clear();
                            }
                            (st.count, st.sample, st.ms, st.error) = (Some(n), docs, Some(ms), None);
                        }
                        Err(e) => (st.count, st.sample, st.ms, st.error) = (None, Vec::new(), None, Some(e)),
                    }
                }
                Msg::Aggregated { req, res } => {
                    let Some(t) = self.tabs.iter_mut().find(|t| t.agg_req == req) else {
                        // A superseded run or a closed tab: its error still deserves a mention.
                        if let Err(e) = res {
                            self.err(e);
                        }
                        continue;
                    };
                    t.agg_req = 0;
                    match res {
                        Ok((d, ms)) => {
                            t.agg_fields = auto_fields(&d);
                            let n = d.len();
                            t.agg_docs = d;
                            t.expanded.retain(|e| !e.0);
                            t.agg_ms = Some(ms);
                            self.ok(format!("Pipeline returned {n} documents"));
                        }
                        Err(e) => {
                            // Old results under a failed run would read as its output.
                            (t.agg_docs, t.agg_ms) = (Vec::new(), None);
                            self.err(e);
                        }
                    }
                }
                Msg::Written { res, undo, reload, retry } => {
                    match res {
                        Ok(s) => {
                            self.reset_doc_state();
                            self.ok(s);
                        }
                        Err(e) => {
                            // Reopen the refused insert so the typed document is not lost, but
                            // only over the collection it was meant for.
                            if let Some((db, coll, text)) = retry {
                                if self.t().is_some_and(|t| t.sel.db == db && t.sel.coll == coll) {
                                    self.insert = Some(text);
                                }
                            }
                            self.err(e);
                        }
                    }
                    if let Some(u) = undo {
                        self.push_undo(u);
                    }
                    if reload {
                        self.reload();
                    } else {
                        self.load_indexes();
                    }
                }
                Msg::Saved { tag, res, undo } => {
                    match res {
                        Ok(s) => {
                            self.saving.remove(&tag);
                            self.toast = Some((s.clone(), std::time::Instant::now(), undo.as_ref().map(|u| u.at)));
                            self.ok(s);
                        }
                        Err(e) => {
                            // Keep the edits so nothing typed is lost; the person can reload and redo.
                            if let Some(dr) = self.saving.remove(&tag) {
                                self.drafts.insert(tag, dr);
                            }
                            self.err(e);
                        }
                    }
                    if let Some(u) = undo {
                        self.push_undo(u);
                    }
                    if self.saving.is_empty() {
                        self.reload();
                    }
                }
                Msg::Info(res) => match res {
                    Ok(s) => self.ok(s),
                    Err(e) => self.err(e),
                },
                Msg::Notice { title, body } => self.modal = Some(Modal::Text { title, body, code: false }),
                Msg::Grouped { conn, db, coll, path, pipeline, res } => match res {
                    Ok(rows) => {
                        self.ok(format!("{} distinct values of {path}", rows.len()));
                        self.modal = Some(Modal::Grouped { sel: Sel { conn, db, coll }, path, pipeline, rows });
                    }
                    Err(e) => self.err(e),
                },
                Msg::Related { req, res } => match (res, self.tab_by_req(req)) {
                    (Ok(v), Some(t)) => t.related.extend(v),
                    (Err(e), _) => self.err(format!("Could not load related documents: {e}")),
                    _ => {}
                },
                Msg::RefFound { conn, db, coll, path, value, res } => match res {
                    Ok(Some((target, preview))) => {
                        self.prefs.relations.entry(format!("{conn}/{db}")).or_default().push(Relation { coll: coll.clone(), path: path.clone(), target: target.clone(), preview: preview.clone() });
                        self.save_prefs();
                        let reqs: Vec<u64> = self.tabs.iter().filter(|t| t.sel.conn == conn && t.sel.db == db && t.sel.coll == coll).map(|t| t.req).collect();
                        for r in reqs {
                            self.load_related(r);
                        }
                        self.go_ref(conn, db, target.clone(), value);
                        self.ok(format!("Linked {path} to {target}, showing {preview} next to its ids. Edit it under Data relations in the database menu."));
                    }
                    Ok(None) => self.err(format!("No collection of {db} has a document with this _id.")),
                    Err(e) => self.err(e),
                },
                Msg::Analyzed { conn, db, res } => match res {
                    Ok(found) => {
                        let list = self.prefs.relations.entry(format!("{conn}/{db}")).or_default();
                        let mut added = 0;
                        for (coll, path, target, preview) in found {
                            if !list.iter().any(|r| r.coll == coll && r.path == path) {
                                list.push(Relation { coll, path, target, preview });
                                added += 1;
                            }
                        }
                        self.save_prefs();
                        let reqs: Vec<u64> = self.tabs.iter().filter(|t| t.sel.conn == conn && t.sel.db == db).map(|t| t.req).collect();
                        for r in reqs {
                            self.load_related(r);
                        }
                        self.ok(if added == 0 { "Analysis done: no new relations found.".to_string() } else { format!("Analysis done: {added} new relation{} found.", if added == 1 { "" } else { "s" }) });
                        if self.modal.is_none() {
                            self.modal = Some(Modal::Relations { conn, db });
                        }
                    }
                    Err(e) => self.err(format!("Could not analyze relations: {e}")),
                },
                Msg::Compared { req, res } => {
                    let Some(t) = self.tabs.iter_mut().find(|t| t.compare.req == req) else { continue };
                    t.compare.running = false;
                    match res {
                        Ok(r) => t.compare.result = Some(r),
                        Err(e) => self.err(e),
                    }
                }
                Msg::ToolLog(line) => {
                    if let Some(t) = &mut self.tool {
                        t.log.push(line);
                        if t.log.len() > 2000 {
                            t.log.drain(..500);
                        }
                    }
                }
                Msg::Update(res) => match res {
                    Ok(Some(v)) => {
                        self.update_note = format!("Version {v} is installed and starts on restart.");
                        self.updated = Some(v);
                    }
                    Ok(None) => self.update_note = format!("Up to date (version {}).", env!("CARGO_PKG_VERSION")),
                    // Offline or rate-limited checks stay quiet; the reason shows in Settings.
                    Err(e) => self.update_note = format!("Last check failed: {e}"),
                },
                Msg::ToolsInstalled(res) => {
                    // Only a click shows failures; a background update that fails stays quiet, like the app's own.
                    let manual = std::mem::take(&mut self.tools_installing);
                    match res {
                        Ok(Some(v)) => self.ok(format!("{} the MongoDB Database Tools {v} in {}", if manual { "Installed" } else { "Updated" }, tools_dir().display())),
                        Ok(None) => {}
                        Err(e) if manual => self.err(format!("Could not install the MongoDB Database Tools: {e}")),
                        Err(_) => {}
                    }
                }
                Msg::ToolDone(res) => {
                    if let Some(t) = &mut self.tool {
                        t.cancel = None;
                        t.done = Some(res.clone());
                    }
                    match res {
                        Ok(m) => self.ok(m),
                        Err(e) => self.err(e),
                    }
                }
                Msg::Tested { res } => self.conn_test = Some(res),
                Msg::Schema { req, res } => {
                    let Some(t) = self.tabs.iter_mut().find(|t| t.schema.req == req) else { continue };
                    t.schema.running = false;
                    match res {
                        Ok((stats, n, ms)) => (t.schema.stats, t.schema.docs, t.schema.ms, t.schema.err) = (stats, n, Some(ms), None),
                        Err(e) => (t.schema.stats, t.schema.ms, t.schema.err) = (Vec::new(), None, Some(e)),
                    }
                }
                Msg::ShellLog { req, level, text } => {
                    if let Some(t) = self.tabs.iter_mut().find(|t| t.shell.req == req) {
                        t.shell.logs.push((level, text));
                    }
                }
                Msg::ShellDone { req, res, ms } => {
                    let Some(t) = self.tabs.iter_mut().find(|t| t.shell.req == req) else { continue };
                    let sh = &mut t.shell;
                    sh.cancel = None;
                    sh.started = None;
                    sh.ms = Some(ms);
                    let parsed = res.and_then(|ejson| {
                        let v: serde_json::Value = serde_json::from_str(&ejson).map_err(|e| e.to_string())?;
                        match Bson::try_from(v).map_err(|e| e.to_string())? {
                            Bson::Document(mut d) => Ok(d.remove("v").unwrap_or(Bson::Null)),
                            _ => Err("Unexpected result from the shell".to_string()),
                        }
                    });
                    match parsed {
                        Ok(v) => sh.set_value(v),
                        Err(e) => {
                            sh.value = None;
                            sh.docs.clear();
                            sh.error = Some(e);
                        }
                    }
                }
            }
        }
    }

    fn selected_doc(&self) -> Option<&Document> {
        let t = self.t()?;
        match t.selected? {
            (false, i) => t.docs.get(i),
            (true, i) => t.agg_docs.get(i),
        }
    }

    fn draft_key(&self, d: &Document) -> String {
        let s = self.t().map(|t| format!("{}/{}/{}", t.sel.conn, t.sel.db, t.sel.coll)).unwrap_or_default();
        format!("{s}#{}", d.get("_id").map(db::compact).unwrap_or_default())
    }

    /// The document as it should be shown: with its unsaved edits, if any.
    fn shown_doc(&self, d: &Document) -> Option<Document> {
        if self.drafts.is_empty() {
            return None;
        }
        self.drafts.get(&self.draft_key(d)).map(|x| x.current.clone())
    }

    /// Keeps an edit as unsaved instead of writing it, so several edits go out in one save.
    fn stage(&mut self, d: &Document, new: db::Res<Document>) {
        let new = match new {
            Ok(n) => n,
            Err(e) => return self.err(e),
        };
        let key = self.draft_key(d);
        let Some(sel) = self.t().map(|t| t.sel.clone()) else { return };
        let original = self.drafts.get(&key).map(|x| x.original.clone()).unwrap_or_else(|| d.clone());
        self.inline = None;
        self.new_field = None;
        self.json_edit = None;
        if db::diff(&original, &new).is_empty() {
            self.drafts.remove(&key);
            return self.ok("No changes left on this document");
        }
        let n = db::diff(&original, &new).len();
        self.drafts.insert(key.clone(), Draft { conn: sel.conn, db: sel.db, coll: sel.coll, original, current: new });
        if self.prefs.auto_save && self.sel_writable() {
            self.write_draft(&key);
        } else {
            self.ok(format!("{n} unsaved change(s). Press Save on the document when ready."));
        }
    }

    fn save_draft(&mut self, key: &str) {
        let Some(dr) = self.drafts.get(key) else { return };
        if !self.conn(dr.conn).is_some_and(|c| c.writable) {
            let key = key.to_string();
            return self.modal = Some(Modal::Unlock(dr.conn, Some(Box::new(move |a: &mut App| a.save_draft(&key)))));
        }
        if self.prefs.confirm_save && !self.prefs.always_writable {
            let changes = db::diff(&dr.original, &dr.current);
            self.modal = Some(Modal::Save { tag: key.to_string(), changes });
        } else {
            self.write_draft(key);
        }
    }

    fn write_draft(&mut self, key: &str) {
        let Some(dr) = self.drafts.remove(key) else { return };
        let Some(target) = self.target_for(&Sel { conn: dr.conn, db: dr.db.clone(), coll: dr.coll.clone() }) else {
            self.drafts.insert(key.to_string(), dr);
            return self.err("Not connected.");
        };
        self.worker.save(key.to_string(), dr.conn, target, dr.original.clone(), dr.current.clone());
        self.saving.insert(key.to_string(), dr);
    }

    /// Unsaved documents of the open collection.
    fn tab_drafts(&self) -> Vec<String> {
        let Some(t) = self.t() else { return Vec::new() };
        let prefix = format!("{}/{}/{}#", t.sel.conn, t.sel.db, t.sel.coll);
        self.drafts.keys().filter(|k| k.starts_with(&prefix)).cloned().collect()
    }

    fn load_demo(&mut self) {
        let names = ["Ada Byron", "Grace Hopper", "Linus Ito", "Mira Okafor", "Sam Patel", "Noor Haddad", "Jun Park", "Eva Lind"];
        let status = ["paid", "shipped", "pending", "refunded"];
        let docs: Vec<Document> = (0..50)
            .map(|i: i32| {
                doc! {
                    "_id": ObjectId::parse_str(format!("6650f1c2a1b2c3d4e5f6{:04x}", 0x0700 + i)).unwrap(),
                    "number": 10_400 + i,
                    "customer": { "name": names[i as usize % names.len()], "tier": if i % 3 == 0 { "gold" } else { "standard" } },
                    "status": status[i as usize % status.len()],
                    "total": 19.5 + (i as f64 * 7.25) % 230.0,
                    "items": (0..(1 + i % 4)).map(|k| Bson::Document(doc! { "sku": format!("SKU-{:03}", 100 + k * 7 + i), "qty": 1 + k })).collect::<Vec<_>>(),
                    "gift": i % 5 == 0,
                    "note": if i % 4 == 1 { Bson::Null } else { Bson::String("leave at the door".into()) },
                    "createdAt": mongodb::bson::DateTime::from_millis(1_730_000_000_000 + i as i64 * 3_600_000),
                    "updatedAt": 1_730_000_000i64 + i as i64 * 3600,
                }
            })
            .collect();
        let colls = |v: &[&str]| Some(v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let dbs = vec![
            ("admin".to_string(), colls(&["system.users", "system.version"])),
            ("analytics".to_string(), colls(&["events", "funnels", "sessions"])),
            ("config".to_string(), colls(&["system.sessions"])),
            ("local".to_string(), colls(&["startup_log"])),
            ("shop".to_string(), colls(&["carts", "customers", "orders", "paid_orders", "products", "reviews", "shipments"])),
            ("users".to_string(), colls(&["accounts", "profiles"])),
        ];
        self.conns.push(Conn {
            meta: ConnMeta { id: 1, name: "Local sample".into(), ..Default::default() },
            client: None,
            status: Status::Ready,
            writable: false,
            dbs,
            masked_uri: "mongodb://localhost:27017".into(),
            tunnel: None,
        });
        self.conns.push(Conn {
            meta: ConnMeta { id: 2, name: "Staging".into(), ..Default::default() },
            client: None,
            status: Status::Idle,
            writable: false,
            dbs: Vec::new(),
            masked_uri: "mongodb+srv://app:****@staging.example.net".into(),
            tunnel: None,
        });
        self.active = Some(1);
        self.open_dbs.insert((1, "shop".into()));
        self.views.insert((1, "shop".into(), "paid_orders".into()));
        let sel = |db: &str, coll: &str| Sel { conn: 1, db: db.into(), coll: coll.into() };
        self.tabs.push(Tab::new(sel("shop", "customers")));
        let mut t = Tab::new(sel("shop", "orders"));
        t.fields = auto_fields(&docs);
        t.paths = doc_paths(&docs);
        t.date_paths = date_paths(&docs, &t.paths);
        t.docs = docs;
        t.count = 1284;
        t.ms = 14;
        t.selected = Some((false, 2));
        t.filter_txt = if std::env::var("QUARRY_TEST_AC").is_ok_and(|v| v == "date") { "{ createdAt: }".into() } else { "{ status: { $in: ['paid', 'shipped', 'pending'] } }".into() };
        t.sort_txt = "{ total: -1 }".into();
        t.query.sort = Some(doc! { "total": -1 });
        self.tabs.push(t);
        self.tabs.push(Tab::new(sel("analytics", "events")));
        self.cur = 1;
        let key = self.col_key(false);
        self.prefs.cols.insert(key, ColPrefs { pinned: vec!["status".into()], highlight: vec!["customer.name".into()], ..Default::default() });
        self.status = ("Offline demo: no database is connected.".into(), false);
        if std::env::var("QUARRY_TEST_CONNFORM").is_ok() {
            self.modal = Some(Modal::Conn(Box::new(ConnForm {
                id: Some(2),
                name: "Staging".into(),
                uri: "mongodb://app:secret@10.0.3.12:27017/?replicaSet=rs0".into(),
                ssh_on: true,
                ssh: crate::ssh::SshConf { host: "bastion.example.com".into(), port: 22, user: "deploy".into(), auth: crate::ssh::SshAuth::KeyFile, key_path: "~/.ssh/id_ed25519".into(), legacy_rsa: false, known_host: Some("SHA256:3n9Yk0mF1q2hV7Qp6zXcR4tL8wE5sA0dJ2uB1nM7kPo".into()) },
                ..Default::default()
            })));
        }
        if std::env::var("QUARRY_TEST_USERS").is_ok() {
            self.users = Some(UsersWin {
                conn: 1,
                users: Some(Ok(vec![
                    doc! { "user": "admin", "db": "admin", "roles": [ { "role": "root", "db": "admin" } ] },
                    doc! { "user": "shop_app", "db": "shop", "roles": [ { "role": "readWrite", "db": "shop" } ] },
                    doc! { "user": "analyst", "db": "admin", "roles": [ { "role": "read", "db": "shop" }, { "role": "read", "db": "analytics" } ] },
                ])),
                roles: Some(Ok(vec![doc! { "role": "reportsReader", "db": "admin", "roles": [ { "role": "read", "db": "analytics" } ] }])),
                new_db: "admin".into(),
                ..Default::default()
            });
        }
        if std::env::var("QUARRY_TEST_SCHEMA").is_ok() {
            self.tabs[1].view = View::Schema;
            self.tabs[1].selected = None;
        }
        // Test switch: a long generated validator, to check its box scrolls inside the window.
        if std::env::var("QUARRY_TEST_VALIDATOR").is_ok() {
            let t = &mut self.tabs[1];
            t.view = View::Schema;
            t.selected = None;
            let props: Document = (0..60).map(|i| (format!("field{i}"), Bson::Document(doc! { "bsonType": "string", "description": "generated" }))).collect();
            t.schema.vtext = db::to_edit_json(&doc! { "$jsonSchema": { "bsonType": "object", "properties": props } });
        }
        if std::env::var("QUARRY_TEST_IMPORT").is_ok() {
            let t = &mut self.tabs[1];
            t.view = View::Import;
            t.selected = None;
            t.import.kind = db::ImportKind::Csv;
            t.import.file = Some("new-orders.csv".into());
            t.import.text = "number,customer,status,total,createdAt\n10501,Ada Byron,paid,42.5,2024-11-02T10:00:00Z\n10502,Grace Hopper,pending,18,2024-11-02T11:30:00Z\n10503,Linus Ito,shipped,99.9,2024-11-03T08:15:00Z\n".into();
        }
        if std::env::var("QUARRY_TEST_SETTINGS").is_ok() {
            self.modal = Some(Modal::Settings);
        }
        // Test switch: show the Shell view with a finished run in the demo picture.
        if std::env::var("QUARRY_TEST_SHELL").is_ok() {
            let t = &mut self.tabs[1];
            t.view = View::Shell;
            t.shell.code = "async function run() {\n  const n = await orders.countDocuments({ status: 'paid' });\n  console.log('paid orders:', n);\n  return await orders.find({ status: 'paid' }).project({ number: 1, total: 1 }).limit(3).toArray();\n}".into();
            t.shell.logs = vec![("log".into(), "paid orders: 321".into())];
            let docs: Vec<Bson> = t.docs.iter().take(3).map(|d| Bson::Document(doc! { "_id": d.get("_id").cloned().unwrap(), "number": d.get("number").cloned().unwrap(), "total": d.get("total").cloned().unwrap() })).collect();
            t.shell.set_value(Bson::Array(docs));
            t.shell.ms = Some(42);
            t.shell.help = std::env::var("QUARRY_TEST_HELP").is_ok();
        }
        // Test switch: show the "Group by and count" window.
        if std::env::var("QUARRY_TEST_GROUP").is_ok() {
            let rows = vec![doc! { "_id": "paid", "count": 321 }, doc! { "_id": "shipped", "count": 321 }, doc! { "_id": "pending", "count": 321 }, doc! { "_id": "refunded", "count": 321 }];
            let pipeline = db::group_count_pipeline("status", &Bson::String("paid".into()), Document::new()).unwrap_or_default();
            self.modal = Some(Modal::Grouped { sel: self.tabs[1].sel.clone(), path: "status".into(), pipeline, rows });
        }
        // Test switch: show the pipeline builder with stage previews.
        if std::env::var("QUARRY_TEST_PIPELINE").is_ok() {
            let t = &mut self.tabs[1];
            t.view = View::Aggregate;
            t.selected = None;
            let docs = t.docs.clone();
            let done = |mut s: Stage, n: u64, sample: Vec<Document>, ms: u128| {
                (s.count, s.sample, s.ms, s.due) = (Some(n), sample, Some(ms), None);
                s
            };
            t.agg_input = done(Stage::new("", "", true), 1284, docs.iter().take(10).cloned().collect(), 3);
            let paid: Vec<Document> = docs.iter().filter(|d| matches!(d.get_str("status"), Ok("paid" | "shipped"))).take(10).cloned().collect();
            let groups = vec![doc! { "_id": "standard", "orders": 428, "revenue": 51210.75 }, doc! { "_id": "gold", "orders": 214, "revenue": 25431.5 }];
            t.stages = vec![
                done(Stage::new("$match", "{ status: { $in: ['paid', 'shipped'] } }", true), 642, paid, 12),
                done(Stage::new("$group", "{\n  _id: '$customer.tier',\n  orders: { $sum: 1 },\n  revenue: { $sum: '$total' }\n}", true), 2, groups.clone(), 31),
                done(Stage::new("$sort", "{ revenue: -1 }", true), 2, groups, 33),
            ];
            t.stages[1].open.insert(0);
            t.agg_txt = text_from_stages(&t.stages);
            t.stages_src = t.agg_txt.clone();
        }
        // Test switch: show the Compare view with a finished comparison.
        if std::env::var("QUARRY_TEST_COMPARE").is_ok() {
            let t = &mut self.tabs[1];
            t.view = View::Compare;
            t.selected = None;
            (t.compare.conn, t.compare.db, t.compare.coll) = (1, "shop".into(), "paid_orders".into());
            let src = t.docs.clone();
            let mut dst: Vec<Document> = src.iter().skip(3).cloned().collect();
            for d in dst.iter_mut().take(4) {
                d.insert("status", "refunded");
                d.remove("note");
            }
            dst.push(doc! { "_id": ObjectId::parse_str("6650f1c2a1b2c3d4e5f6ffff").unwrap(), "number": 9999, "status": "test" });
            t.compare.result = Some(db::compare_docs(src, dst));
        }
        // Test switch: show the Data relations window with a few relations.
        if std::env::var("QUARRY_TEST_RELATIONS").is_ok() {
            let r = |coll: &str, path: &str, target: &str, preview: &str| Relation { coll: coll.into(), path: path.into(), target: target.into(), preview: preview.into() };
            self.prefs.relations.insert("1/shop".into(), vec![r("orders", "customerId", "customers", "name"), r("orders", "items.productId", "products", "title"), r("reviews", "authorAccountId", "customers", "email")]);
            self.modal = Some(Modal::Relations { conn: 1, db: "shop".into() });
        }
        // Test switch: show the Dump dialog.
        if std::env::var("QUARRY_TEST_DUMP").is_ok() {
            self.modal = Some(Modal::Tool(Box::new(ToolForm { conn: 1, db: "shop".into(), from_db: "shop".into(), restore: true, path: "/Users/me/Downloads/shop.archive.gz".into(), gzip: true, ..Default::default() })));
        }
        // Test switch: show the JSON editor and an unsaved edit in the demo picture.
        if std::env::var("QUARRY_TEST_EDIT").is_ok() {
            let d = self.tabs[1].docs[2].clone();
            let mut changed = d.clone();
            changed.insert("status", "shipped");
            self.drafts.insert(self.draft_key(&d), Draft { conn: 1, db: "shop".into(), coll: "orders".into(), original: d.clone(), current: changed.clone() });
            let items = changed.get("items").cloned().unwrap();
            self.json_edit = Some(JsonEdit { base: changed, path: vec!["items".into()], text: db::to_edit_value(&items), kind: "array", focus: false });
        }
    }
}

/// Visuals for both themes; egui picks one from the system or the Appearance setting.
fn style(ctx: &egui::Context) {
    for (theme, p) in [(egui::Theme::Dark, &DARK_PAL), (egui::Theme::Light, &LIGHT_PAL)] {
        ctx.set_visuals_of(theme, visuals(theme, p));
    }
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 4.0);
        // No bounce past the edge of lists.
        s.spacing.scroll.kinetic.rubber_band = false;
    });
}

fn visuals(theme: egui::Theme, p: &Palette) -> egui::Visuals {
    let mut v = theme.default_visuals();
    v.panel_fill = p.panel;
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
        w.corner_radius = CornerRadius::same(6);
    }
    v
}

enum StageAct {
    Edited(usize),
    Toggled(usize),
    /// Move a stage one place, down when true.
    Move(usize, bool),
    Remove(usize),
    Add(String),
    Op(usize, String),
    /// Show 10 more documents; `usize::MAX` is the collection sample.
    More(usize),
    Preview(usize),
}

/// One stage of the pipeline builder: operator, on/off, order and removal on top, the stage body
/// on the left and what comes out of the stage on the right.
fn stage_card(ui: &mut egui::Ui, i: usize, n: usize, s: &mut Stage, auto: bool, acts: &mut Vec<StageAct>) {
    card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{}", i + 1)).monospace().color(FAINT()));
            ui.menu_button(RichText::new(&s.op).monospace().strong().color(if s.on { C_KEYWORD() } else { FAINT() }), |ui| {
                egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                    for (op, _) in db::STAGES {
                        if ui.button(RichText::new(*op).monospace()).clicked() {
                            acts.push(StageAct::Op(i, op.to_string()));
                            ui.close();
                        }
                    }
                });
            })
            .response
            .on_hover_text("Change the stage operator");
            if ui.checkbox(&mut s.on, "").on_hover_text("Include this stage in the pipeline").changed() {
                acts.push(StageAct::Toggled(i));
            }
            ui.hyperlink_to(RichText::new("Docs").size(12.0), format!("https://www.mongodb.com/docs/manual/reference/operator/aggregation/{}/", s.op.trim_start_matches('$')));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button(RichText::new("Remove…").color(ALERT()), |ui| {
                    if ui.button(RichText::new(format!("Remove stage {}", i + 1)).color(ALERT())).clicked() {
                        acts.push(StageAct::Remove(i));
                        ui.close();
                    }
                });
                if ui.add_enabled(i + 1 < n, egui::Button::new("Down").small()).clicked() {
                    acts.push(StageAct::Move(i, true));
                }
                if ui.add_enabled(i > 0, egui::Button::new("Up").small()).clicked() {
                    acts.push(StageAct::Move(i, false));
                }
            });
        });
        ui.horizontal_top(|ui| {
            let w = (ui.available_width() * 0.45).max(260.0);
            ui.allocate_ui_with_layout(egui::vec2(w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(w);
                let id = egui::Id::unique(("stage-editor", s.uid));
                let rows = s.text.lines().count().clamp(2, 16);
                let out = with_autopair(ui, id, &mut s.text, |ui, text| egui::TextEdit::multiline(text).id(id).code_editor().desired_width(f32::INFINITY).desired_rows(rows).layouter(&mut code_layout).show(ui));
                if out.response.changed() {
                    acts.push(StageAct::Edited(i));
                }
                if let Err(e) = db::parse_value(&s.text) {
                    ui.label(RichText::new(e).size(12.0).color(ALERT()));
                }
            });
            // A gap, not ui.separator(): in a row a separator takes all the height left and
            // stretches the card down the whole view.
            ui.add_space(14.0);
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    if !s.on {
                        ui.label(RichText::new("Off: the pipeline skips this stage.").color(FAINT()));
                    } else if s.req != 0 {
                        spin(ui);
                        ui.label(RichText::new("Previewing…").color(MUTED()));
                    } else if let Some(n) = s.count {
                        ui.label(RichText::new(format!("{} documents", group(&n.to_string()))).strong().color(BRIGHT()));
                        if let Some(ms) = s.ms {
                            ui.label(RichText::new(format!("{ms} ms")).color(FAINT()));
                        }
                    } else if s.error.is_none() && !auto && ui.small_button("Preview").clicked() {
                        acts.push(StageAct::Preview(i));
                    }
                });
                if s.on {
                    if let Some(e) = &s.error {
                        ui.label(RichText::new(e).size(12.0).color(if e.starts_with("This stage writes") { WARNING() } else { ALERT() }));
                    }
                    stage_samples(ui, s, i, acts);
                }
            });
        });
    });
}

/// The first documents out of a stage, one line each. The whole row toggles the document open
/// or closed, and several can be open at once.
fn stage_samples(ui: &mut egui::Ui, s: &mut Stage, i: usize, acts: &mut Vec<StageAct>) {
    if s.sample.is_empty() {
        if s.count == Some(0) {
            ui.label(RichText::new("No documents come out of this stage.").color(FAINT()));
        }
        return;
    }
    let max_h = if s.open.is_empty() { 220.0 } else { 440.0 };
    egui::ScrollArea::vertical().id_salt(("stage-sample", s.uid)).max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
        for (k, d) in s.sample.iter().enumerate() {
            let open = s.open.contains(&k);
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 20.0), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(rect, 4, HOVER());
            }
            chevron(ui.painter(), egui::pos2(rect.left() + 8.0, rect.center().y), open, if open { PRIMARY() } else { FAINT() });
            let g = ui.fonts_mut(|f| f.layout_job(cell_job(&Bson::Document(d.clone()))));
            let clip = ui.painter().with_clip_rect(rect.shrink2(egui::vec2(2.0, 0.0)));
            clip.galley(egui::pos2(rect.left() + 20.0, rect.center().y - g.size().y / 2.0), g, TEXT());
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(if open { "Click to close" } else { "Click to see the whole document" }).clicked() {
                if !s.open.remove(&k) {
                    s.open.insert(k);
                }
            }
            if open {
                let text = db::to_edit_json(d);
                let rows = text.lines().count().min(24);
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    // A &str buffer is read-only: the text can be selected and copied, not edited.
                    ui.add(egui::TextEdit::multiline(&mut text.as_str()).code_editor().desired_width(f32::INFINITY).desired_rows(rows).layouter(&mut code_layout));
                });
            }
        }
        if s.count.is_some_and(|n| n > s.sample.len() as u64) && s.sample.len() as i64 >= s.limit && ui.small_button(format!("Show {} more", db::STAGE_SAMPLE)).clicked() {
            acts.push(StageAct::More(i));
        }
    });
}

fn pill(ui: &mut egui::Ui, text: &str, fg: Color32) -> egui::Response {
    let bg = state_fill(ui, tint(fg, 30));
    ui.add(egui::Button::new(RichText::new(text).color(fg).strong().size(12.0)).fill(bg).stroke(Stroke::new(1.0, fg)).corner_radius(10))
}

/// `c` moved by `t` away from its own lightness: toward white barely changes an already light color.
fn shade(c: Color32, t: f32) -> Color32 {
    let light = (0.3 * c.r() as f32 + 0.59 * c.g() as f32 + 0.11 * c.b() as f32) / 255.0 > 0.55;
    c.lerp_to_gamma(if light { Color32::BLACK } else { Color32::WHITE }, t)
}

/// A fill for the next widget that follows hover and press. egui uses a button's own fill for
/// every state, so buttons with one would otherwise never react. Reads last frame's response
/// of the widget about to be added, which has the next auto id.
fn state_fill(ui: &egui::Ui, base: Color32) -> Color32 {
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

fn primary(ui: &mut egui::Ui, text: &str, fill: Color32) -> egui::Response {
    ui.add(primary_button(ui, text, fill))
}

/// For `add_enabled`; `primary` when it is always enabled.
fn primary_button(ui: &egui::Ui, text: impl Into<String>, fill: Color32) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text).strong().color(BG())).fill(state_fill(ui, fill))
}

/// Every dialog gets the same frame; egui's popup frame has only 6 px of margin.
fn dialog(id: egui::Id) -> egui::Modal {
    let shadow = egui::Shadow { offset: [0, 6], blur: 20, spread: 0, color: Color32::from_black_alpha(50) };
    egui::Modal::new(id).frame(egui::Frame::new().shadow(shadow).fill(RAISED()).stroke(Stroke::new(1.0, BORDER())).corner_radius(12).inner_margin(18))
}

/// The button row at the bottom of a dialog: primary action first, then the rest, Cancel last.
fn dialog_actions<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.add_space(8.0);
    ui.horizontal(add).inner
}

fn dialog_title(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.label(RichText::new(text).strong().size(16.0).color(BRIGHT()));
    ui.add_space(2.0);
}

/// The strip at the top of a view holding its inputs and actions.
fn toolbar<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<R> {
    egui::Frame::new().fill(PANEL()).corner_radius(8).inner_margin(10).show(ui, add)
}

/// A bordered box grouping related content: settings sections, pipeline stages, a document.
fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<R> {
    egui::Frame::new().fill(RAISED()).stroke(Stroke::new(1.0, BORDER())).corner_radius(8).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, add)
}

const MONO_PX: f32 = 12.5;

#[derive(Clone, Copy)]
enum DocsAct {
    CopyJs,
    CopyJson,
    CopyLines,
    Duplicate,
    Export,
    Filter,
    Update,
    Delete,
}

/// Positions in `new` of the documents at `rows` in `old`, matched by _id.
fn follow_ids(old: &[Document], new: &[Document], rows: impl Iterator<Item = usize>) -> BTreeSet<usize> {
    let ids: HashSet<String> = rows.filter_map(|i| old.get(i)?.get("_id").map(db::compact)).collect();
    if ids.is_empty() {
        return BTreeSet::new();
    }
    new.iter().enumerate().filter(|(_, d)| d.get("_id").is_some_and(|v| ids.contains(&db::compact(v)))).map(|(i, _)| i).collect()
}

/// Thumbtack toggle: filled when pinned, outlined when not.
fn pin_button(ui: &mut egui::Ui, pinned: bool) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), Sense::click());
    let color = if pinned { PRIMARY() } else if r.hovered() { MUTED() } else { FAINT() };
    let p = ui.painter();
    if r.hovered() {
        p.rect_filled(rect, 4, HOVER());
    }
    let c = rect.center();
    let head = egui::Rect::from_center_size(c + egui::vec2(0.0, -3.5), egui::vec2(8.0, 7.0));
    if pinned {
        p.rect_filled(head, 2, color);
    } else {
        p.rect_stroke(head, 2, Stroke::new(1.4, color), egui::StrokeKind::Inside);
    }
    p.line_segment([c + egui::vec2(-6.0, 1.0), c + egui::vec2(6.0, 1.0)], Stroke::new(1.6, color));
    p.line_segment([c + egui::vec2(0.0, 1.0), c + egui::vec2(0.0, 7.5)], Stroke::new(1.4, color));
    r
}

/// Six dots marking a drag handle; painted because the default fonts lack the braille glyph.
fn grip(ui: &mut egui::Ui) {
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
fn right_group(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let id = ui.scope_id().with("right-group-w");
    let need = ui.data(|d| d.get_temp::<f32>(id)).unwrap_or(160.0);
    if ui.available_size_before_wrap().x < need + 8.0 {
        ui.end_row();
    }
    let r = ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), add);
    ui.data_mut(|d| d.insert_temp(id, r.response.rect.width()));
}

/// A glyph button large enough to hit comfortably.
fn icon_button(ui: &mut egui::Ui, glyph: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(enabled, egui::Button::new(RichText::new(glyph).size(15.0)).min_size(egui::vec2(28.0, 26.0)))
}

/// Width of the checkbox column in front of the row number.
const CHECK_W: f32 = 22.0;

/// Actions on one document or the checked ones, shared by the selection bar and the row menus.
fn docs_menu(ui: &mut egui::Ui, n: usize, heading: bool) -> Option<DocsAct> {
    if heading {
        ui.label(RichText::new(if n == 1 { "Document".to_string() } else { format!("{n} selected documents") }).size(11.0).color(FAINT()));
    }
    let items: [(&str, DocsAct); 8] = [
        ("Copy as JS", DocsAct::CopyJs),
        ("Copy as JSON", DocsAct::CopyJson),
        ("Copy as JSON lines", DocsAct::CopyLines),
        ("Duplicate", DocsAct::Duplicate),
        ("Export…", DocsAct::Export),
        ("Filter to these", DocsAct::Filter),
        ("Update in shell", DocsAct::Update),
        ("Delete…", DocsAct::Delete),
    ];
    let mut act = None;
    for (label, a) in items {
        let text = if matches!(a, DocsAct::Delete) { RichText::new(label).color(ALERT()) } else { RichText::new(label) };
        if ui.button(text).clicked() {
            act = Some(a);
        }
    }
    act
}

/// The row menu: acts on every checked document when the row is one of them, else on that row.
fn row_menu(ui: &mut egui::Ui, i: usize, checked: &BTreeSet<usize>) -> Option<(usize, DocsAct)> {
    docs_menu(ui, if checked.contains(&i) { checked.len() } else { 1 }, true).map(|a| (i, a))
}

/// A painted checkbox for table rows; the egui widget would take part in the row layout.
fn check_box(ui: &egui::Ui, center: egui::Pos2, id: egui::Id, on: bool, partial: bool) -> egui::Response {
    let rect = egui::Rect::from_center_size(center, egui::vec2(14.0, 14.0));
    let r = ui.interact(rect.expand(3.0), id, Sense::click());
    let filled = on || partial;
    let edge = if filled { PRIMARY() } else if r.hovered() { MUTED() } else { FAINT() };
    let p = ui.painter();
    p.rect(rect, 3, if filled { PRIMARY() } else { Color32::TRANSPARENT }, Stroke::new(1.0, edge), egui::StrokeKind::Inside);
    let s = Stroke::new(1.6, Color32::WHITE);
    if on {
        p.line(vec![center + egui::vec2(-3.5, 0.0), center + egui::vec2(-1.0, 2.5), center + egui::vec2(3.5, -2.5)], s);
    } else if partial {
        p.hline(center.x - 3.5..=center.x + 3.5, center.y, s);
    }
    r
}

fn mono(c: Color32) -> TextFormat {
    TextFormat { font_id: FontId::monospace(MONO_PX), color: c, ..Default::default() }
}

/// egui's spinner asks for a new frame on every refresh. Behind other windows a static glyph
/// lets the app go idle instead of redrawing at full rate for something nobody is watching.
fn spin(ui: &mut egui::Ui) {
    if ui.input(|i| i.viewport().focused.unwrap_or(true)) {
        ui.spinner();
    } else {
        ui.label(RichText::new("…").color(FAINT()));
    }
}

fn row_job(d: &Document, width: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    if let Some(id) = d.get("_id") {
        job.append(&db::short(id), 0.0, mono(C_OID()));
        job.append("  ", 0.0, mono(MUTED()));
    }
    for (k, v) in d.iter().filter(|(k, _)| *k != "_id").take(12) {
        job.append(k, 0.0, mono(MUTED()));
        job.append(": ", 0.0, mono(MUTED()));
        job.append(&cell_text(v).chars().take(60).collect::<String>(), 0.0, mono(color_of(v)));
        job.append("   ", 0.0, mono(MUTED()));
    }
    job.wrap = egui::text::TextWrapping { max_width: width, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    job
}

/// One clickable sidebar row painted by hand, so it reads as a list rather than a form.
fn side_row(ui: &mut egui::Ui, indent: f32, active: bool, paint: impl FnOnce(&egui::Painter, egui::Rect)) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), Sense::click());
    let fill = if resp.is_pointer_button_down_on() {
        tint(PRIMARY(), 45)
    } else if active {
        tint(PRIMARY(), 30)
    } else if resp.hovered() {
        HOVER()
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 6, fill);
    let inner = egui::Rect::from_min_max(rect.min + egui::vec2(8.0 + indent, 0.0), rect.max - egui::vec2(8.0, 0.0));
    paint(ui.painter(), inner);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn chevron(p: &egui::Painter, c: egui::Pos2, open: bool, color: Color32) {
    let pts = if open {
        vec![c + egui::vec2(-4.0, -2.0), c + egui::vec2(4.0, -2.0), c + egui::vec2(0.0, 3.0)]
    } else {
        vec![c + egui::vec2(-2.0, -4.0), c + egui::vec2(3.0, 0.0), c + egui::vec2(-2.0, 4.0)]
    };
    p.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}

impl eframe::App for App {
    /// Test switch for the demo picture: QUARRY_TEST_POINTER="x,y" hovers that point and
    /// "x,y,down" also holds the primary button there, to check hover and press states.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        #[cfg(not(target_os = "macos"))]
        let _ = ctx;
        #[cfg(target_os = "macos")]
        if let Some(m) = &mut self.menu {
            let rest = m.feed(ctx, raw);
            self.menu_acts.extend(rest);
        }
        if self.shot.is_none() {
            return;
        }
        let Ok(spec) = std::env::var("QUARRY_TEST_POINTER") else { return };
        let mut parts = spec.split(',');
        let (Some(Ok(x)), Some(Ok(y))) = (parts.next().map(str::parse::<f32>), parts.next().map(str::parse::<f32>)) else { return };
        let pos = egui::pos2(x, y);
        raw.events.push(egui::Event::PointerMoved(pos));
        if parts.next() == Some("down") {
            raw.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        LIGHT_MODE.store(ctx.theme() == egui::Theme::Light, std::sync::atomic::Ordering::Relaxed);
        // macOS leaves an app started from a terminal or Finder behind the frontmost one; the first
        // frames ask for focus once the window is visible (a no-op when it already has it).
        if ctx.cumulative_frame_nr() < 3 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        #[cfg(target_os = "macos")]
        for a in std::mem::take(&mut self.menu_acts) {
            self.menu_act(ctx, a);
        }
        // Screenshot mode must keep drawing even if the window is covered or hidden, or it never finishes.
        if self.shot.is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
            ctx.request_repaint();
        }
        // Set every frame so connections added or reconnected later need no extra handling.
        if self.prefs.always_writable {
            self.conns.iter_mut().for_each(|c| c.writable = true);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.frame_ms = frame.info().cpu_usage.unwrap_or(0.0) * 1000.0;
        // QUARRY_FRAME_LOG prints each frame's CPU time, for measuring with a fixture.
        if std::env::var("QUARRY_FRAME_LOG").is_ok() {
            eprintln!("frame_ms {:.3}", self.frame_ms);
        }
        self.poll();
        self.remember_tabs();
        let ctx = ui.ctx().clone();
        // AppKit resets the window buttons on every layout, so re-centre them in our header each frame.
        frame.set_traffic_lights_position(&ctx, egui::Rangef::new(0.0, HEADER_H), 16.0);
        self.top_bar(ui);
        self.status_bar(ui);
        self.query_log(ui);
        self.sidebar(ui);
        let view = self.t().map(|t| t.view);
        if !self.prefs.open_inline && self.selected_doc().is_some() && view != Some(View::Indexes) {
            self.inspector(ui);
        }
        egui::CentralPanel::default_margins().frame(egui::Frame::new().fill(BG()).inner_margin(egui::Margin { left: 12, right: 12, top: 8, bottom: 8 })).show(ui, |ui| {
            self.tab_strip(ui);
            if self.tabs.is_empty() {
                self.welcome(ui);
            } else {
                self.collection(ui);
            }
        });
        self.auto_update(&ctx);
        // Before the modals, so one opened by a shortcut draws this frame, and an Esc that closes a
        // modal is not also taken as a second Esc by `keys`.
        self.keys(&ctx);
        self.modals(&ctx);
        self.users_window(&ctx);
        self.tool_window(&ctx);
        self.toast(&ctx);
        self.screenshot(&ctx);
    }
}

impl App {
    /// Keeps the open tabs and their inputs in the settings file, so the next launch reopens them.
    fn remember_tabs(&mut self) {
        if self.demo || self.shot.is_some() {
            return;
        }
        let tabs: Vec<SavedTab> = self
            .tabs
            .iter()
            .map(|t| SavedTab {
                conn: t.sel.conn,
                db: t.sel.db.clone(),
                coll: t.sel.coll.clone(),
                view: match t.view {
                    View::Documents => "Documents",
                    View::Aggregate => "Aggregate",
                    View::Indexes => "Indexes",
                    View::Shell => "Shell",
                    View::Schema => "Schema",
                    View::Import => "Import",
                    View::Compare => "Compare",
                }
                .into(),
                filter: t.filter_txt.clone(),
                sort: t.sort_txt.clone(),
                project: t.proj_txt.clone(),
                agg: t.agg_txt.clone(),
                shell: t.shell.code.clone(),
                stages: if t.stages_src == t.agg_txt { t.stages.iter().map(|s| SavedStage { op: s.op.clone(), text: s.text.clone(), on: s.on }).collect() } else { Vec::new() },
                agg_text_mode: t.agg_text_mode,
            })
            .collect();
        if tabs != self.prefs.open_tabs || self.cur != self.prefs.cur_tab || self.active != self.prefs.last_conn {
            self.prefs.open_tabs = tabs;
            self.prefs.cur_tab = self.cur;
            self.save_prefs();
            self.prefs.last_conn = self.active;
        }
    }

    fn keys(&mut self, ctx: &egui::Context) {
        if self.modal.is_some() || self.insert.is_some() || self.json_edit.is_some() || self.users.is_some() {
            return;
        }
        let k = |m: egui::Modifiers, key: Key| ctx.input_mut(|i| i.consume_key(m, key));
        let (none, cmd, shift, ctrl, alt) = (egui::Modifiers::NONE, egui::Modifiers::COMMAND, egui::Modifiers::SHIFT, egui::Modifiers::CTRL, egui::Modifiers::ALT);
        // Extra Shift and Alt still match a plain Cmd shortcut, so the longer combos go first.
        if k(cmd | shift, Key::T) {
            self.reopen_tab();
        }
        if k(cmd, Key::W) {
            self.close_tab(self.cur);
        }
        if k(cmd, Key::K) || k(cmd, Key::T) {
            self.modal = Some(Modal::Palette { query: String::new(), sel: 0 });
        }
        if k(cmd, Key::E) {
            self.explain();
        }
        if k(cmd | shift, Key::R) {
            if let Some(id) = self.active.filter(|_| !self.demo) {
                self.connect(id);
            }
        }
        if k(cmd, Key::R) && !self.tabs.is_empty() {
            self.reload();
        }
        if k(cmd | shift, Key::F) {
            ctx.memory_mut(|m| m.request_focus(egui::Id::unique("tree-filter")));
        }
        if k(cmd, Key::F) && !self.tabs.is_empty() {
            self.set_view(View::Documents);
            ctx.memory_mut(|m| m.request_focus(egui::Id::unique(("query", "QUERY"))));
        }
        if k(cmd, Key::Comma) {
            self.modal = Some(Modal::Settings);
        }
        if k(cmd, Key::N) {
            self.new_conn_modal();
        }
        if k(cmd, Key::L) {
            self.log.open = !self.log.open;
        }
        if k(cmd, Key::Slash) {
            let body = SHORTCUTS.iter().map(|(keys, what)| format!("{keys:<30}{what}")).collect::<Vec<_>>().join("\n");
            self.modal = Some(Modal::Text { title: "Keyboard shortcuts".into(), body, code: false });
        }
        let n = self.tabs.len();
        if n > 0 {
            if k(ctrl | shift, Key::Tab) || k(cmd | shift, Key::OpenBracket) || k(cmd | shift, Key::OpenCurlyBracket) {
                self.go_tab((self.cur + n - 1) % n);
            }
            if k(ctrl, Key::Tab) || k(cmd | shift, Key::CloseBracket) || k(cmd | shift, Key::CloseCurlyBracket) {
                self.go_tab((self.cur + 1) % n);
            }
            const DIGITS: [Key; 9] = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
            const VIEWS: [View; 7] = [View::Documents, View::Aggregate, View::Indexes, View::Schema, View::Import, View::Compare, View::Shell];
            for (d, v) in DIGITS.iter().zip(VIEWS) {
                if k(cmd | alt, *d) {
                    self.set_view(v);
                }
            }
            for (i, d) in DIGITS.iter().enumerate() {
                if k(cmd, *d) {
                    self.go_tab(if i == 8 { n - 1 } else { i.min(n - 1) });
                }
            }
        }
        // Plain keys only act when no text field or menu wants them.
        if ctx.memory(|m| m.focused().is_none()) && !ctx.any_popup_open() {
            let (up, down) = (k(none, Key::ArrowUp), k(none, Key::ArrowDown));
            let (prev, next) = (k(none, Key::PageUp), k(none, Key::PageDown));
            let nav = self.t().filter(|t| matches!(t.view, View::Documents | View::Aggregate)).map(|t| {
                let agg = t.view == View::Aggregate;
                let len = if agg { t.agg_docs.len() } else { t.docs.len() };
                let row = ((up || down) && len > 0).then(|| match t.selected.filter(|s| s.0 == agg) {
                    None => 0,
                    Some((_, i)) if down => (i + 1).min(len - 1),
                    Some((_, i)) => i.saturating_sub(1),
                });
                let (page, pages) = (t.page, t.count.div_ceil(PAGE_SIZE).max(1));
                let go = if agg { None } else if prev && page > 0 { Some(page - 1) } else if next && page + 1 < pages { Some(page + 1) } else { None };
                (agg, row, go)
            });
            if let Some((agg, row, go)) = nav {
                if let Some(i) = row {
                    // The open card moves with the keys instead of leaving the previous one open.
                    if let Some(prev) = self.t().and_then(|t| t.selected) {
                        self.tm().unwrap().expanded.remove(&prev);
                    }
                    self.select(agg, i, false);
                    self.scroll_sel = true;
                }
                if let Some(p) = go {
                    self.tm().unwrap().page = p;
                    self.reload();
                }
            }
        }
        // Esc closes the innermost thing that is open. Dialogs and menus handle their own Esc.
        if ctx.any_popup_open() || !ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
            return;
        }
        if self.inline.take().is_some() || self.new_field.take().is_some() {
            return;
        }
        if let Some(t) = self.tm() {
            if let Some(key) = t.selected {
                return self.close_row(key);
            }
            if !t.expanded.is_empty() {
                t.expanded.clear();
                return;
            }
        }
        self.tree_filter.clear();
    }

    fn go_tab(&mut self, i: usize) {
        if i != self.cur && i < self.tabs.len() {
            self.cur = i;
            self.reset_doc_state();
        }
    }

    fn set_view(&mut self, v: View) {
        let Some(t) = self.tm().filter(|t| t.view != v) else { return };
        t.view = v;
        t.selected = None;
        self.reset_doc_state();
        if v == View::Indexes {
            self.load_indexes();
        }
    }

    /// Users of all databases with their roles, custom roles, and user actions. Writes need the
    /// connection unlocked; dropping a user asks for its name.
    /// Progress and output of mongodump or mongorestore.
    fn tool_window(&mut self, ctx: &egui::Context) {
        if self.modal.is_some() {
            return;
        }
        let Some(t) = &mut self.tool else { return };
        let mut close = false;
        dialog(egui::Id::unique(("quarry-layer", "tool"))).show(ctx, |ui| {
            ui.set_width(640.0);
            ui.horizontal(|ui| {
                dialog_title(ui, &t.title);
                match &t.done {
                    None => {
                        spin(ui);
                    }
                    Some(Ok(m)) => {
                        ui.label(RichText::new(m).color(SUCCESS()));
                    }
                    Some(Err(e)) => {
                        ui.label(RichText::new(e).color(ALERT()));
                    }
                }
            });
            egui::Frame::new().fill(pal().well).corner_radius(6).inner_margin(8).show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(320.0).stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                    for l in &t.log {
                        ui.label(RichText::new(l).monospace().size(12.0).color(TEXT()));
                    }
                });
            });
            dialog_actions(ui, |ui| {
                if t.done.is_none() {
                    if ui.button(RichText::new("Stop").color(ALERT())).clicked() {
                        if let Some(c) = t.cancel.take() {
                            let _ = c.send(());
                        }
                    }
                } else {
                    close = ui.button("Close").clicked();
                }
                if ui.button("Copy log").clicked() {
                    ui.ctx().copy_text(t.log.join("\n"));
                }
            });
        });
        if close {
            self.tool = None;
        }
    }

    fn users_window(&mut self, ctx: &egui::Context) {
        let Some(mut u) = self.users.take() else { return };
        let conn = u.conn;
        let writable = self.conn(conn).is_some_and(|c| c.writable);
        let client = self.conn(conn).and_then(|c| c.client.clone());
        let atlas = self.conn(conn).is_some_and(|c| c.masked_uri.contains(".mongodb.net"));
        let mut open = true;
        let mut cmds: Vec<(String, Document, String)> = Vec::new();
        let mut drop_user = None;
        let mut unlock = false;
        let screen = ctx.content_rect();
        let r = dialog(egui::Id::unique(("quarry-layer", "users-window"))).show(ctx, |ui| {
            ui.set_width((screen.width() * 0.6).clamp(560.0, 900.0));
            ui.horizontal(|ui| {
                dialog_title(ui, "Users and roles");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    open = !ui.button("Done").on_hover_text("Esc").clicked();
                    if ui.button("Refresh").clicked() {
                        u.users = None;
                        u.roles = None;
                        cmds.push(("*reload".into(), Document::new(), String::new()));
                    }
                });
            });
            if !writable {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Read-only: unlock writes to add, change or drop users.").color(SUCCESS()));
                    unlock = ui.small_button("Unlock…").clicked();
                });
            }
            if atlas {
                ui.label(RichText::new("Atlas manages database users in its web console; it refuses adding, changing or dropping users from here, and may list none.").size(12.0).color(WARNING()));
            }
            egui::ScrollArea::vertical().max_height(screen.height() - 220.0).show(ui, |ui| {
                settings_section(ui, "USERS", |ui| match &u.users {
                    None => {
                        spin(ui);
                    }
                    Some(Err(e)) => {
                        ui.label(RichText::new(format!("Could not list users: {e}")).color(ALERT()));
                    }
                    Some(Ok(list)) => {
                        if list.is_empty() {
                            ui.label(RichText::new("No users.").color(MUTED()));
                        }
                        for user in list {
                            let (name, db) = (user.get_str("user").unwrap_or_default().to_string(), user.get_str("db").unwrap_or_default().to_string());
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&name).monospace().color(BRIGHT()));
                                ui.label(RichText::new(format!("@{db}")).monospace().color(FAINT()));
                                ui.label(RichText::new(roles_text(user)).monospace().size(12.0).color(C_STRING()));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.add_enabled(writable, egui::Button::new(RichText::new("Drop…").color(ALERT())).small()).clicked() {
                                        drop_user = Some((db.clone(), name.clone()));
                                    }
                                    if ui.add_enabled(writable, egui::Button::new("Edit").small()).clicked() {
                                        u.edit = Some((name.clone(), db.clone(), String::new(), roles_text(user)));
                                    }
                                });
                            });
                            if let Some((en, edb, pwd, roles)) = &mut u.edit {
                                if *en == name && *edb == db {
                                    egui::Frame::new().fill(pal().well).corner_radius(6).inner_margin(8).show(ui, |ui| {
                                        egui::Grid::new(("edit", &name)).num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                                            ui.label("New password");
                                            ui.add(egui::TextEdit::singleline(pwd).password(true).hint_text("leave empty to keep").desired_width(280.0));
                                            ui.end_row();
                                            ui.label("Roles");
                                            ui.add(egui::TextEdit::singleline(roles).hint_text("readWrite@shop, read@analytics").desired_width(280.0).font(FontId::monospace(12.5)));
                                            ui.end_row();
                                        });
                                        ui.horizontal(|ui| {
                                            if primary(ui, "Save", PRIMARY()).clicked() {
                                                let mut cmd = doc! { "updateUser": en.as_str(), "roles": parse_roles(roles, edb) };
                                                if !pwd.is_empty() {
                                                    cmd.insert("pwd", pwd.as_str());
                                                }
                                                cmds.push((edb.clone(), cmd, format!("Updated user {en}")));
                                            }
                                            if ui.button("Cancel").clicked() {
                                                cmds.push(("*close-edit".into(), Document::new(), String::new()));
                                            }
                                        });
                                    });
                                }
                            }
                            ui.separator();
                        }
                    }
                });
                settings_section(ui, "ADD USER", |ui| {
                    egui::Grid::new("adduser").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                        ui.label("Name");
                        ui.add(egui::TextEdit::singleline(&mut u.new_name).desired_width(280.0));
                        ui.end_row();
                        ui.label("Password");
                        ui.add(egui::TextEdit::singleline(&mut u.new_pwd).password(true).desired_width(280.0));
                        ui.end_row();
                        ui.label("Database");
                        ui.add(egui::TextEdit::singleline(&mut u.new_db).hint_text("admin").desired_width(280.0));
                        ui.end_row();
                        ui.label("Roles");
                        ui.add(egui::TextEdit::singleline(&mut u.new_roles).hint_text("readWrite@shop, read@analytics").desired_width(280.0).font(FontId::monospace(12.5)));
                        ui.end_row();
                    });
                    let ok = writable && !u.new_name.trim().is_empty() && !u.new_pwd.is_empty();
                    if ui.add_enabled(ok, primary_button(ui, "Add user", PRIMARY())).clicked() {
                        let db = if u.new_db.trim().is_empty() { "admin".to_string() } else { u.new_db.trim().to_string() };
                        let cmd = doc! { "createUser": u.new_name.trim(), "pwd": u.new_pwd.as_str(), "roles": parse_roles(&u.new_roles, &db) };
                        cmds.push((db, cmd, format!("Added user {}", u.new_name.trim())));
                        u.new_pwd.clear();
                    }
                });
                settings_section(ui, "CUSTOM ROLES", |ui| match &u.roles {
                    None => {
                        spin(ui);
                    }
                    Some(Err(e)) => {
                        ui.label(RichText::new(format!("Could not list roles: {e}")).color(ALERT()));
                    }
                    Some(Ok(list)) if list.is_empty() => {
                        ui.label(RichText::new("No custom roles; built-in roles like read, readWrite and dbAdmin can be given to users.").color(MUTED()));
                    }
                    Some(Ok(list)) => {
                        for r in list {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(format!("{}@{}", r.get_str("role").unwrap_or_default(), r.get_str("db").unwrap_or_default())).monospace().color(BRIGHT()));
                                ui.label(RichText::new(format!("inherits: {}", roles_text(r))).monospace().size(12.0).color(FAINT()));
                            });
                        }
                    }
                });
            });
        });
        if unlock {
            self.modal = Some(Modal::Unlock(conn, None));
        }
        for (db, cmd, done) in cmds {
            match db.as_str() {
                "*reload" => self.load_users(conn),
                "*close-edit" => u.edit = None,
                _ => {
                    if let Some(client) = client.clone() {
                        u.edit = None;
                        self.worker.db_change(conn, client, db, cmd, done);
                    }
                }
            }
        }
        if let Some((db, user)) = drop_user {
            self.modal = Some(Modal::Typed { title: format!("Drop the user {user} of {db}? Apps that log in with it will stop working."), expect: user.clone(), typed: String::new(), action: Danger::DropUser { conn, db, user } });
        }
        if open && !(r.should_close() && self.modal.is_none()) {
            self.users = Some(u);
        }
    }

    /// "Saved … Undo" notice after a write, for 8 seconds.
    fn toast(&mut self, ctx: &egui::Context) {
        const TOAST_S: u64 = 8;
        let Some((msg, at, undo_at)) = self.toast.clone() else { return };
        // Later writes push newer entries in front, so the toast's own entry is found by its time.
        let entry = undo_at.and_then(|k| self.undo.iter().position(|a| a.at == k));
        if at.elapsed().as_secs() >= TOAST_S {
            self.toast = None;
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(250));
        let mut undo = false;
        let mut close = false;
        egui::Area::new(egui::Id::unique(("quarry-layer", "toast"))).anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -40.0)).show(ctx, |ui| {
            egui::Frame::new().fill(RAISED()).stroke(Stroke::new(1.0, BORDER())).corner_radius(8).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("✓").color(SUCCESS()));
                    ui.label(RichText::new(&msg).color(TEXT()));
                    if entry.is_some() && ui.button(RichText::new("Undo").color(PRIMARY())).clicked() {
                        undo = true;
                    }
                    close = ui.small_button("×").clicked();
                });
            });
        });
        if let (true, Some(i)) = (undo, entry) {
            self.apply_undo(i);
            self.toast = None;
        } else if close {
            self.toast = None;
        }
    }

    fn screenshot(&mut self, ctx: &egui::Context) {
        let Some((path, frames, slot)) = &mut self.shot else { return };
        *frames += 1;
        ctx.request_repaint();
        if *frames == 120 {
            let slot = slot.clone();
            ctx.request_screenshot(move |img| *slot.lock().unwrap() = Some((*img).clone()));
        }
        let taken = slot.lock().unwrap().take();
        if let Some(img) = taken {
            let px: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            match image::RgbaImage::from_raw(img.size[0] as u32, img.size[1] as u32, px).map(|i| i.save(&*path)) {
                Some(Ok(())) => eprintln!("saved {}", path.display()),
                other => eprintln!("screenshot failed: {other:?}"),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        // Leaves room for the macOS window buttons, which sit over our header.
        let left = if cfg!(target_os = "macos") { 92 } else { 12 };
        let frame = egui::Frame::new().fill(PANEL()).inner_margin(egui::Margin { left, right: 12, top: 7, bottom: 7 });
        egui::Panel::top("top").exact_size(HEADER_H).frame(frame).show(ui, |ui| {
            // Registered before the widgets so they stay clickable; the empty header moves the window.
            let bar = ui.interact(ui.max_rect().expand2(egui::vec2(left as f32, 7.0)), egui::Id::unique("titlebar"), Sense::click_and_drag());
            if bar.drag_started_by(egui::PointerButton::Primary) {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            if bar.double_clicked() {
                let max = ui.input(|i| i.viewport().maximized.unwrap_or(false));
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
            }
            ui.horizontal_centered(|ui| {
                ui.label(RichText::new("Quarry").strong().size(16.0).color(BRIGHT()));
                ui.label(RichText::new("MongoDB workbench").color(MUTED()));
                ui.separator();
                if let Some(s) = self.t().map(|t| t.sel.clone()) {
                    let (name, writable) = self.conn(s.conn).map(|c| (c.meta.name.clone(), c.writable)).unwrap_or_default();
                    ui.label(RichText::new(format!("{name}  /  {}  /  {}", s.db, s.coll)).monospace().color(TEXT()));
                    if self.prefs.always_writable {
                        // No lock to toggle: writes are always on.
                    } else if writable {
                        if pill(ui, "WRITES ON", ALERT()).on_hover_text("Click to lock this connection again").clicked() {
                            if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == s.conn) {
                                c.writable = false;
                            }
                            self.ok("Writes locked");
                        }
                    } else if pill(ui, "READ-ONLY", SUCCESS()).on_hover_text("Click to allow edits on this connection for this session").clicked() {
                        self.modal = Some(Modal::Unlock(s.conn, None));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.undo.retain(|a| a.at.elapsed().as_secs() <= db::UNDO_MAX_AGE_S);
                    let n = self.undo.len();
                    if ui.add_enabled(n > 0, egui::Button::new(format!("Undo ({n})"))).clicked() {
                        self.modal = Some(Modal::Undo);
                    }
                    if ui.button("Settings").clicked() {
                        self.modal = Some(Modal::Settings);
                    }
                    if let Some(v) = &self.updated {
                        let r = ui.add(primary_button(ui, format!("Restart for {v}"), SUCCESS())).on_hover_text("The update is installed. Quarry restarts by itself once you have no unsaved edits and have been idle for a minute.");
                        if r.clicked() {
                            if self.busy() {
                                self.err("Save or discard your edits and wait for running scripts first.");
                            } else {
                                crate::update::restart();
                            }
                        }
                    }
                });
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("status").frame(egui::Frame::new().fill(PANEL()).inner_margin(egui::Margin::symmetric(12, 5))).show(ui, |ui| {
            ui.horizontal(|ui| {
                if self.prefs.show_fps {
                    let dt = ui.input(|i| i.stable_dt).max(0.000_1);
                    let budget = 1000.0 * dt;
                    let color = if self.frame_ms > budget * 0.8 { WARNING() } else { SUCCESS() };
                    ui.label(RichText::new(format!("{:.0} fps · {:.1} ms", 1.0 / dt, self.frame_ms)).monospace().size(11.0).color(color)).on_hover_text("Frames per second while something moves, and the time Quarry needs to build one frame. At 120 Hz each frame has 8.3 ms.");
                    ui.add_space(8.0);
                }
                // Errors fade after 15 s so an old problem does not look current.
                const ERROR_S: u64 = 15;
                if self.status.1 {
                    if self.status_at.elapsed().as_secs() >= ERROR_S {
                        self.status = ("Ready".into(), false);
                    } else {
                        ui.ctx().request_repaint_after(Duration::from_secs(1));
                        if ui.small_button("×").on_hover_text("Dismiss").clicked() {
                            self.status = ("Ready".into(), false);
                        }
                    }
                }
                let (msg, is_err) = &self.status;
                let first = msg.lines().next().unwrap_or_default();
                let r = ui.add(egui::Label::new(RichText::new(first).size(12.0).color(if *is_err { ALERT() } else { MUTED() })).truncate().sense(egui::Sense::click()));
                // Errors often carry advice on a second line or a long server message.
                if !*is_err {
                    r.on_hover_text(msg.as_str());
                } else if r.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(format!("{msg}\n\nClick to open the whole message.")).clicked() {
                    self.modal = Some(Modal::Text { title: "Error".into(), body: msg.clone(), code: false });
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.selectable_label(self.log.open, RichText::new("Query log").size(12.0)).on_hover_text("Every command sent to the servers (Cmd+L)").clicked() {
                        self.log.open = !self.log.open;
                    }
                    ui.label(RichText::new("Cmd+K jumps  ·  Enter runs  ·  double-click a value to edit  ·  Cmd+/ shortcuts").size(12.0).color(FAINT()));
                });
            });
        });
    }

    fn query_log(&mut self, ui: &mut egui::Ui) {
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

    fn log_body(&mut self, ui: &mut egui::Ui) {
        let mut copied = false;
        let (log, conns) = (&mut self.log, &self.conns);
        ui.horizontal(|ui| {
            ui.label(RichText::new("QUERY LOG").size(11.0).color(MUTED()));
            ui.add(egui::TextEdit::singleline(&mut log.filter).hint_text("filter").desired_width(200.0).font(FontId::monospace(12.0)));
            ui.checkbox(&mut log.errors, "Errors only");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("×").on_hover_text("Hide (Cmd+L)").clicked() {
                    log.open = false;
                }
                let (label, tip) = if log.detached { ("Dock", "Show at the bottom of the main window") } else { ("Detach", "Open in its own window") };
                if ui.small_button(label).on_hover_text(tip).clicked() {
                    log.detached = !log.detached;
                }
                if ui.small_button("Clear").clicked() {
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
        let row_h = 18.0;
        egui::ScrollArea::both().auto_shrink(false).stick_to_bottom(true).show_rows(ui, row_h, rows.len(), |ui, range| {
            for l in &rows[range] {
                let name = conns.iter().find(|c| c.meta.id == l.conn).map_or("test", |c| c.meta.name.as_str());
                // Painted under the row once its size is known, so a hovered row shows it copies.
                let bg = ui.painter().add(egui::Shape::Noop);
                let r = ui
                    .horizontal(|ui| {
                        ui.set_height(row_h);
                        let mono = |t: &str, c: Color32| RichText::new(t).monospace().size(12.0).color(c);
                        ui.add_sized([34.0, row_h], egui::Label::new(mono(&ago(l.at), FAINT())));
                        ui.add_sized([60.0, row_h], egui::Label::new(mono(&match (l.ms, &l.err) {
                            (None, _) => "running".into(),
                            (Some(ms), _) => format!("{ms} ms"),
                        }, if l.err.is_some() { ALERT() } else { MUTED() })).truncate());
                        ui.label(mono(&format!("{name} · {}", l.db), MUTED()));
                        ui.label(mono(&l.name, if l.err.is_some() { ALERT() } else { PRIMARY() }));
                        if let Some(e) = &l.err {
                            ui.add(egui::Label::new(mono(e.lines().next().unwrap_or_default(), ALERT())).truncate());
                        }
                        ui.add(egui::Label::new(mono(&l.cmd, TEXT())).truncate());
                    })
                    .response
                    .interact(egui::Sense::click());
                let r = r.on_hover_ui(|ui| {
                    ui.set_max_width(640.0);
                    if let Some(e) = &l.err {
                        ui.label(RichText::new(e).color(ALERT()));
                    }
                    ui.label(RichText::new(&l.cmd).monospace().size(12.0));
                    ui.label(RichText::new("Click to copy the command").size(11.0).color(FAINT()));
                });
                if r.hovered() {
                    ui.painter().set(bg, egui::Shape::rect_filled(r.rect.expand2(egui::vec2(4.0, 1.0)), 4, HOVER()));
                }
                let r = r.on_hover_cursor(egui::CursorIcon::PointingHand);
                if r.clicked() {
                    ui.ctx().copy_text(l.cmd.clone());
                    copied = true;
                }
            }
        });
        drop(entries);
        if copied {
            self.ok("Command copied");
        }
    }

    fn new_conn_modal(&mut self) {
        self.conn_test = None;
        self.modal = Some(Modal::Conn(Box::new(ConnForm { uri: "mongodb://localhost:27017".into(), ..Default::default() })));
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("tree")
            .default_size(SIDEBAR_W)
            .size_range(200.0..=480.0)
            .resizable(true)
            .frame(egui::Frame::new().fill(PANEL()).inner_margin(egui::Margin { left: 10, right: 10, top: 12, bottom: 10 }))
            .show(ui, |ui| {
                self.conn_picker(ui);
                ui.add_space(10.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.tree_filter)
                        .id(egui::Id::unique("tree-filter"))
                        .hint_text("Find a collection")
                        .desired_width(f32::INFINITY)
                        .margin(egui::Margin::symmetric(10, 6)),
                );
                ui.add_space(12.0);
                ui.label(RichText::new("DATABASES").size(11.0).strong().color(FAINT()));
                ui.add_space(2.0);
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| self.db_tree(ui));
            });
    }

    fn conn_picker(&mut self, ui: &mut egui::Ui) {
        let cur = self.active.and_then(|id| self.conn(id));
        let (name, dot) = match cur {
            Some(c) => (
                c.meta.name.clone(),
                match c.status {
                    Status::Ready => SUCCESS(),
                    Status::Connecting => WARNING(),
                    Status::Failed(_) => ALERT(),
                    Status::Idle => FAINT(),
                },
            ),
            None => ("Add a connection".to_string(), FAINT()),
        };
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 38.0), Sense::click());
        let p = ui.painter();
        let fill = if resp.is_pointer_button_down_on() { shade(HOVER(), 0.08) } else if resp.hovered() { HOVER() } else { RAISED() };
        p.rect(rect, 8, fill, Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
        p.circle_filled(egui::pos2(rect.left() + 16.0, rect.center().y), 4.0, dot);
        p.text(egui::pos2(rect.left() + 28.0, rect.center().y), egui::Align2::LEFT_CENTER, &name, FontId::proportional(14.5), BRIGHT());
        chevron(p, egui::pos2(rect.right() - 16.0, rect.center().y), true, MUTED());
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        if cur.is_none() && resp.clicked() {
            return self.new_conn_modal();
        }
        let mut pick = None;
        let mut new = false;
        let mut remove = None;
        let mut edit = None;
        let mut refresh = None;
        egui::Popup::menu(&resp).width(rect.width()).show(|ui| {
            for c in &self.conns {
                let label = format!("{}   {}", c.meta.name, c.masked_uri);
                if ui.selectable_label(Some(c.meta.id) == self.active, RichText::new(label).color(TEXT())).clicked() {
                    pick = Some(c.meta.id);
                }
            }
            ui.separator();
            if ui.button("+ New connection…").clicked() {
                new = true;
            }
            if let Some(c) = cur {
                if ui.button(format!("Edit \"{}\"…", c.meta.name)).clicked() {
                    edit = Some(c.meta.id);
                }
                if ui.button("Refresh databases").on_hover_text("Reconnect and reload the list of databases and collections").clicked() {
                    refresh = Some(c.meta.id);
                }
                if ui.button(RichText::new(format!("Remove \"{}\"…", c.meta.name)).color(ALERT())).clicked() {
                    remove = Some((c.meta.id, c.meta.name.clone()));
                }
            }
        });
        if let Some(id) = edit {
            if let Some(meta) = self.conn(id).map(|c| c.meta.clone()) {
                self.conn_test = None;
                // Opening the form with an empty address would overwrite the stored one on save,
                // unless there is none, which is when re-entering it is the way out.
                let uri = match stored_uri(id) {
                    Ok(u) => u,
                    Err(keyring::Error::NoEntry) => String::new(),
                    Err(e) => return self.err(uri_err(e)),
                };
                self.modal = Some(Modal::Conn(Box::new(ConnForm {
                    id: Some(id),
                    name: meta.name.clone(),
                    uri,
                    ssh_on: meta.ssh.is_some(),
                    ssh: meta.ssh.clone().unwrap_or_default(),
                    ssh_pwd: secret(id, "ssh-password").unwrap_or_default(),
                    ssh_pass: secret(id, "ssh-passphrase").unwrap_or_default(),
                    proxy_on: meta.proxy.is_some(),
                    proxy: meta.proxy.clone().unwrap_or_default(),
                    proxy_pwd: secret(id, "proxy-password").unwrap_or_default(),
                    show: false,
                    testing: false,
                })));
            }
        }
        if let Some(id) = refresh {
            if !self.demo {
                self.connect(id);
            }
        }
        if let Some(id) = pick {
            self.activate(id);
        }
        if new {
            self.new_conn_modal();
        }
        if let Some((id, name)) = remove {
            self.modal = Some(Modal::Typed {
                title: format!("Remove the saved connection \"{name}\"? Its address is deleted from the Keychain. No data is touched."),
                expect: name,
                typed: String::new(),
                action: Danger::RemoveConnection(id),
            });
        }
    }

    fn activate(&mut self, id: u64) {
        self.active = Some(id);
        if self.conn(id).is_some_and(|c| c.status == Status::Idle) && !self.demo {
            self.connect(id);
        }
    }

    fn jump(&mut self, j: Jump) {
        let (id, db) = match &j {
            Jump::Conn(id) => (*id, None),
            Jump::Db(id, db) => (*id, Some(db.clone())),
            Jump::Coll(s) => (s.conn, Some(s.db.clone())),
        };
        self.activate(id);
        self.reveal = db.clone().map(|d| (id, d, if let Jump::Coll(s) = &j { Some(s.coll.clone()) } else { None }));
        // Reveal the target in the tree, which a leftover filter could hide.
        if let Some(db) = db {
            self.tree_filter.clear();
            self.open_dbs.insert((id, db));
        }
        if let Jump::Coll(sel) = j {
            self.open_collection(sel, false);
        }
    }

    fn db_tree(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.active else {
            ui.add_space(8.0);
            ui.label(RichText::new("No connections yet.").color(MUTED()));
            return;
        };
        ui.spacing_mut().item_spacing.y = 2.0;
        let filter = self.tree_filter.to_lowercase();
        let cur_sel = self.t().map(|t| t.sel.clone());
        let mut toggle = None;
        let mut open = None;
        let mut side: Vec<SideAct> = Vec::new();
        let mut connect = false;
        let mut revealed = false;
        {
            let Some(c) = self.conn(id) else { return };
            match &c.status {
                Status::Ready => {}
                Status::Connecting => {
                    ui.horizontal(|ui| {
                        spin(ui);
                        ui.label(RichText::new("Connecting…").color(MUTED()));
                    });
                }
                st => {
                    ui.add_space(6.0);
                    if let Status::Failed(e) = st {
                        ui.add(egui::Label::new(RichText::new(e).size(12.0).color(ALERT())).wrap());
                    }
                    connect = primary(ui, "Connect", PRIMARY()).clicked();
                }
            }
            for (db_name, colls) in &c.dbs {
                let matches: Vec<&String> = colls.iter().flatten().filter(|n| filter.is_empty() || n.to_lowercase().contains(&filter)).collect();
                if !filter.is_empty() && matches.is_empty() {
                    continue;
                }
                let is_open = !filter.is_empty() || self.open_dbs.contains(&(id, db_name.clone()));
                let dim = SYSTEM_DBS.contains(&db_name.as_str());
                let count = colls.as_ref().map(|v| v.len().to_string()).unwrap_or_else(|| "…".into());
                let r = side_row(ui, 0.0, false, |p, r| {
                    chevron(p, egui::pos2(r.left() + 4.0, r.center().y), is_open, MUTED());
                    p.text(egui::pos2(r.left() + 16.0, r.center().y), egui::Align2::LEFT_CENTER, db_name, FontId::proportional(14.0), if dim { FAINT() } else { BRIGHT() });
                    p.text(egui::pos2(r.right(), r.center().y), egui::Align2::RIGHT_CENTER, &count, FontId::proportional(12.0), FAINT());
                });
                if matches!(&self.reveal, Some((c, d, None)) if *c == id && d == db_name) {
                    r.scroll_to_me(Some(egui::Align::Center));
                    revealed = true;
                }
                if r.clicked() {
                    toggle = Some(db_name.clone());
                }
                r.context_menu(|ui| {
                    ui.label(RichText::new(format!("Database: {db_name}")).size(12.0).color(MUTED()));
                    if ui.button("New collection…").clicked() {
                        side.push(SideAct::NewColl(db_name.clone(), false));
                    }
                    if ui.button("New view…").clicked() {
                        side.push(SideAct::NewColl(db_name.clone(), true));
                    }
                    if ui.button("Refresh").clicked() {
                        side.push(SideAct::Refresh(db_name.clone()));
                    }
                    if ui.button("Users and roles…").clicked() {
                        side.push(SideAct::Users);
                    }
                    if ui.button("Data relations…").clicked() {
                        side.push(SideAct::Relations(db_name.clone()));
                    }
                    ui.separator();
                    if ui.button("Dump database…").clicked() {
                        side.push(SideAct::Dump(db_name.clone(), String::new()));
                    }
                    if ui.button("Restore into this database…").clicked() {
                        side.push(SideAct::Restore(db_name.clone()));
                    }
                    ui.separator();
                    if ui.button(RichText::new("Drop database…").color(ALERT())).clicked() {
                        side.push(SideAct::DropDb(db_name.clone()));
                    }
                });
                if is_open {
                    for coll in matches {
                        let sel = Sel { conn: id, db: db_name.clone(), coll: coll.clone() };
                        let active = cur_sel.as_ref() == Some(&sel);
                        let is_view = self.views.contains(&(id, db_name.clone(), coll.clone()));
                        let r = side_row(ui, 16.0, active, |p, r| {
                            let c = egui::pos2(r.left() + 4.0, r.center().y);
                            if is_view {
                                // Views get a hollow marker so they read as derived data.
                                p.circle_stroke(c, 3.5, Stroke::new(1.5, if active { PRIMARY() } else { C_DATE() }));
                            } else {
                                p.circle_filled(c, 3.5, if active { PRIMARY() } else { WARNING() });
                            }
                            let w = r.width() - 16.0;
                            let galley = p.layout(coll.clone(), FontId::proportional(13.5), if active { BRIGHT() } else if is_view { C_DATE() } else { TEXT() }, w);
                            p.galley(egui::pos2(r.left() + 16.0, r.center().y - galley.size().y / 2.0), galley, TEXT());
                        });
                        let r = if is_view { r.on_hover_text("View (read-only)") } else { r };
                        if matches!(&self.reveal, Some((c, d, Some(k))) if *c == id && d == db_name && k == coll) {
                            r.scroll_to_me(Some(egui::Align::Center));
                            revealed = true;
                        }
                        if r.clicked() {
                            open = Some(sel.clone());
                        }
                        r.context_menu(|ui| {
                            ui.label(RichText::new(format!("{}: {coll}", if is_view { "View" } else { "Collection" })).size(12.0).color(MUTED()));
                            if ui.button("Open").clicked() {
                                open = Some(sel.clone());
                            }
                            if ui.button("Statistics").clicked() {
                                side.push(SideAct::Stats(db_name.clone(), coll.clone()));
                            }
                            if is_view && ui.button("View definition").clicked() {
                                side.push(SideAct::ViewDef(db_name.clone(), coll.clone()));
                            }
                            if !is_view && ui.button("Analyze data relations").on_hover_text("Find fields that hold ids of other collections' documents").clicked() {
                                side.push(SideAct::Analyze(db_name.clone(), coll.clone()));
                            }
                            if ui.button("Compare with…").clicked() {
                                side.push(SideAct::Compare(db_name.clone(), coll.clone()));
                            }
                            if !is_view && ui.button("Dump collection…").clicked() {
                                side.push(SideAct::Dump(db_name.clone(), coll.clone()));
                            }
                            ui.separator();
                            if !is_view && ui.button("Rename…").clicked() {
                                side.push(SideAct::Rename(db_name.clone(), coll.clone()));
                            }
                            if ui.button(RichText::new(if is_view { "Drop view…" } else { "Drop collection…" }).color(ALERT())).clicked() {
                                side.push(SideAct::DropColl(db_name.clone(), coll.clone()));
                            }
                        });
                    }
                    ui.add_space(4.0);
                }
            }
        }
        if revealed {
            self.reveal = None;
        }
        if connect {
            self.connect(id);
        }
        if let Some(d) = toggle {
            if !self.open_dbs.remove(&(id, d.clone())) {
                self.open_dbs.insert((id, d));
            }
        }
        if let Some(s) = open {
            self.open_collection(s, false);
        }
        for a in side {
            self.side_act(id, a);
        }
    }

    fn side_act(&mut self, conn: u64, a: SideAct) {
        let Some(client) = self.conn(conn).and_then(|c| c.client.clone()) else {
            return self.err("Connect first.");
        };
        let writable = self.conn(conn).is_some_and(|c| c.writable);
        let needs_write = matches!(a, SideAct::NewColl(..) | SideAct::DropDb(_) | SideAct::Rename(..) | SideAct::DropColl(..) | SideAct::Restore(_));
        if needs_write && !writable {
            return self.modal = Some(Modal::Unlock(conn, None));
        }
        match a {
            SideAct::NewColl(db, view) => self.modal = Some(Modal::NewCollection { conn, db, name: String::new(), view, source: String::new(), pipeline: "[\n  { $match: {} }\n]".into() }),
            SideAct::Refresh(db) => self.worker.collections(conn, client, db),
            SideAct::Users => {
                self.users = Some(UsersWin { conn, new_db: "admin".into(), ..Default::default() });
                self.load_users(conn);
            }
            SideAct::DropDb(db) => {
                self.modal = Some(Modal::Typed { title: format!("Drop the whole database {db} with all its collections? This cannot be undone."), expect: db.clone(), typed: String::new(), action: Danger::DropDatabase { conn, db } })
            }
            SideAct::Stats(db, coll) => self.worker.coll_stats(client, db, coll),
            SideAct::ViewDef(db, coll) => self.worker.command(client, db, doc! { "listCollections": 1, "filter": { "name": coll } }, "viewdef"),
            SideAct::Rename(db, coll) => self.modal = Some(Modal::RenameColl { conn, db, name: coll.clone(), coll }),
            SideAct::Dump(db, coll) => {
                let name = if coll.is_empty() { db.clone() } else { format!("{db}.{coll}") };
                let path = dirs::download_dir().unwrap_or_default().join(format!("{name}.archive.gz")).display().to_string();
                self.modal = Some(Modal::Tool(Box::new(ToolForm { conn, from_db: db.clone(), db, coll, path, gzip: true, ..Default::default() })));
            }
            SideAct::Restore(db) => self.modal = Some(Modal::Tool(Box::new(ToolForm { conn, from_db: db.clone(), db, restore: true, gzip: true, ..Default::default() }))),
            SideAct::Compare(db, coll) => {
                self.open_collection(Sel { conn, db: db.clone(), coll }, false);
                let t = self.tm().unwrap();
                t.view = View::Compare;
                if t.compare.conn == 0 {
                    (t.compare.conn, t.compare.db) = (conn, db);
                }
            }
            SideAct::Relations(db) => self.modal = Some(Modal::Relations { conn, db }),
            SideAct::Analyze(db, coll) => self.analyze_relations(conn, client, db, coll),
            SideAct::DropColl(db, coll) => {
                self.modal = Some(Modal::Typed { title: format!("Drop {db}.{coll} with all its documents and indexes? This cannot be undone."), expect: coll.clone(), typed: String::new(), action: Danger::DropCollAt { conn, db, coll } })
            }
        }
    }

    /// Users of every database and custom roles, read from admin like the original
    /// (usersInfo / rolesInfo with showBuiltinRoles false, renderer ~156395).
    fn load_users(&mut self, conn: u64) {
        let Some(client) = self.conn(conn).and_then(|c| c.client.clone()) else { return };
        self.worker.command(client.clone(), "admin".into(), doc! { "usersInfo": { "forAllDBs": true } }, "users");
        self.worker.command(client, "admin".into(), doc! { "rolesInfo": 1, "showBuiltinRoles": false }, "roles");
    }

    fn tab_strip(&mut self, ui: &mut egui::Ui) {
        let mut pick = None;
        let mut close = None;
        let mut dup = false;
        let mut many: Option<(usize, TabClose)> = None;
        let mut reopen = false;
        let has_closed = !self.closed.is_empty();
        egui::ScrollArea::horizontal().id_salt("tabs").max_height(38.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let n = self.tabs.len();
                for (i, t) in self.tabs.iter().enumerate() {
                    let active = i == self.cur;
                    let mut job = LayoutJob::default();
                    job.append(&t.sel.coll, 0.0, egui::TextFormat::simple(FontId::proportional(13.5), if active { BRIGHT() } else { TEXT() }));
                    job.append(&t.sel.db, 6.0, egui::TextFormat::simple(FontId::proportional(11.5), FAINT()));
                    if t.view != View::Documents {
                        let v = match t.view {
                            View::Aggregate => "aggregate",
                            View::Indexes => "indexes",
                            View::Shell => "shell",
                            View::Schema => "schema",
                            View::Import => "import",
                            View::Compare => "compare",
                            View::Documents => "",
                        };
                        job.append(v, 6.0, egui::TextFormat::simple(FontId::proportional(11.5), C_LINK().gamma_multiply(0.8)));
                    }
                    let title = ui.fonts_mut(|f| f.layout_job(job));
                    let w = (title.size().x + 50.0).clamp(96.0, 300.0);
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 30.0), Sense::click());
                    let rect = rect.shrink2(egui::vec2(0.0, 1.0));
                    let p = ui.painter();
                    let hovered = resp.hovered();
                    if active {
                        p.rect(rect, 7, RAISED(), Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
                        // The original marks the active tab with a yellow underline.
                        p.line_segment([rect.left_bottom() + egui::vec2(8.0, -1.0), rect.right_bottom() + egui::vec2(-8.0, -1.0)], Stroke::new(2.0, WARNING()));
                    } else if hovered {
                        p.rect_filled(rect, 7, PANEL());
                    }
                    let is_view = self.views.contains(&(t.sel.conn, t.sel.db.clone(), t.sel.coll.clone()));
                    let dot = egui::pos2(rect.left() + 13.0, rect.center().y);
                    let dc = if active { WARNING() } else { FAINT() };
                    if is_view {
                        p.circle_stroke(dot, 3.0, Stroke::new(1.3, dc));
                    } else {
                        p.circle_filled(dot, 3.0, dc);
                    }
                    let clip = egui::Rect::from_min_max(rect.min, egui::pos2(rect.right() - 24.0, rect.bottom()));
                    p.with_clip_rect(clip).galley(egui::pos2(rect.left() + 24.0, rect.center().y - title.size().y / 2.0), title, TEXT());
                    let xr = egui::Rect::from_center_size(egui::pos2(rect.right() - 13.0, rect.center().y), egui::vec2(16.0, 16.0));
                    let xresp = ui.interact(xr, ui.scope_id().with(("close", i)), Sense::click());
                    if active || hovered || xresp.hovered() {
                        if xresp.hovered() {
                            ui.painter().rect_filled(xr, 4, HOVER());
                        }
                        let c = xr.center();
                        let s = Stroke::new(1.4, if xresp.hovered() { BRIGHT() } else { MUTED() });
                        ui.painter().line_segment([c + egui::vec2(-3.5, -3.5), c + egui::vec2(3.5, 3.5)], s);
                        ui.painter().line_segment([c + egui::vec2(3.5, -3.5), c + egui::vec2(-3.5, 3.5)], s);
                    }
                    let conn_name = self.conns.iter().find(|c| c.meta.id == t.sel.conn).map(|c| c.meta.name.clone()).unwrap_or_default();
                    let resp = resp.on_hover_text(format!("{conn_name} / {} / {}\nMiddle-click closes, right-click for more", t.sel.db, t.sel.coll));
                    resp.context_menu(|ui| {
                        ui.set_min_width(190.0);
                        for (label, what, enabled) in [
                            ("Close", TabClose::This, true),
                            ("Close others", TabClose::Others, n > 1),
                            ("Close tabs to the left", TabClose::Left, i > 0),
                            ("Close tabs to the right", TabClose::Right, i + 1 < n),
                            ("Close all", TabClose::All, true),
                        ] {
                            if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                                many = Some((i, what));
                            }
                        }
                        ui.separator();
                        if ui.button("Duplicate").clicked() {
                            pick = Some(i);
                            dup = true;
                        }
                        if ui.add_enabled(has_closed, egui::Button::new("Reopen closed tab").shortcut_text("Cmd+Shift+T")).clicked() {
                            reopen = true;
                        }
                    });
                    if xresp.clicked() || resp.middle_clicked() {
                        close = Some(i);
                    } else if resp.clicked() {
                        pick = Some(i);
                    }
                }
                if !self.tabs.is_empty() && ui.add(egui::Button::new(RichText::new("+").size(16.0).color(MUTED())).frame(false)).on_hover_text("Duplicate this tab").clicked() {
                    dup = true;
                }
            });
        });
        ui.painter().hline(ui.max_rect().x_range(), ui.min_rect().bottom(), Stroke::new(1.0, BORDER()));
        ui.add_space(8.0);
        if reopen {
            return self.reopen_tab();
        }
        if let Some((i, what)) = many {
            let keep = |j: usize| match what {
                TabClose::This => j != i,
                TabClose::Others => j == i,
                TabClose::Left => j >= i,
                TabClose::Right => j <= i,
                TabClose::All => false,
            };
            let cur = self.cur;
            let all = std::mem::take(&mut self.tabs);
            for (j, t) in all.into_iter().enumerate() {
                if keep(j) {
                    self.tabs.push(t);
                } else {
                    self.remember_closed(t);
                }
            }
            // Stay on the same tab when it survives, else on the clicked one, else the nearest.
            self.cur = if keep(cur) { (0..cur).filter(|&k| keep(k)).count() } else if keep(i) { (0..i).filter(|&k| keep(k)).count() } else { i.saturating_sub(1) }.min(self.tabs.len().saturating_sub(1));
            self.reset_doc_state();
        } else if let Some(i) = close {
            self.close_tab(i);
        } else if let Some(i) = pick {
            self.go_tab(i);
        }
        if dup {
            let t = &self.tabs[self.cur];
            let mut n = Tab::new(t.sel.clone());
            (n.filter_txt, n.sort_txt, n.proj_txt, n.query, n.view, n.agg_txt) = (t.filter_txt.clone(), t.sort_txt.clone(), t.proj_txt.clone(), t.query.clone(), t.view, t.agg_txt.clone());
            self.tabs.insert(self.cur + 1, n);
            self.cur += 1;
            self.reset_doc_state();
            self.reload();
        }
    }

    fn welcome(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(120.0);
            ui.label(RichText::new("Pick a collection on the left").size(20.0).color(BRIGHT()));
            ui.add_space(6.0);
            ui.label(RichText::new("Each collection opens in its own tab. Every connection starts read-only; edits need an explicit unlock, show a change preview, and can be undone.").color(MUTED()));
            ui.add_space(16.0);
            if ui.button("+ Add a connection").clicked() {
                self.new_conn_modal();
            }
        });
    }

    fn collection(&mut self, ui: &mut egui::Ui) {
        let Some(t) = self.t() else { return };
        let (coll, view) = (t.sel.coll.clone(), t.view);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(&coll).size(18.0).strong().color(BRIGHT()));
            ui.add_space(12.0);
            for (v, label) in [(View::Documents, "Documents"), (View::Aggregate, "Aggregate"), (View::Indexes, "Indexes"), (View::Schema, "Schema"), (View::Import, "Import"), (View::Compare, "Compare"), (View::Shell, "Shell")] {
                if ui.selectable_label(view == v, label).clicked() {
                    self.set_view(v);
                }
            }
            if matches!(view, View::Documents | View::Aggregate) {
                right_group(ui, |ui| {
                    if self.prefs.table {
                        self.columns_menu(ui, view == View::Aggregate);
                    }
                    let mut table = self.prefs.table;
                    ui.selectable_value(&mut table, false, "JSON");
                    ui.selectable_value(&mut table, true, "Table");
                    if table != self.prefs.table {
                        self.prefs.table = table;
                        self.save_prefs();
                    }
                });
            }
        });
        ui.add_space(6.0);
        match view {
            View::Documents => self.documents(ui),
            View::Aggregate => self.aggregate(ui),
            View::Indexes => self.indexes_view(ui),
            View::Shell => self.shell_view(ui),
            View::Schema => self.schema_view(ui),
            View::Import => self.import_view(ui),
            View::Compare => self.compare_view(ui),
        }
    }

    /// A query input that grows with its content, with highlighting and autocomplete.
    /// Enter runs it (or picks a suggestion), Shift+Enter adds a line.
    #[allow(clippy::too_many_arguments)]
    fn query_field(ui: &mut egui::Ui, label: &str, text: &mut String, hint: &str, width: f32, kind: FieldKind, paths: &[String], dates: &[String], ac: &mut Ac) -> bool {
        ui.add_sized([56.0, 20.0], egui::Label::new(RichText::new(label).size(11.0).color(MUTED())));
        let id = egui::Id::unique(("query", label));
        // Test switch: show the suggestion list in the demo picture.
        if ac.id.is_none() && kind == FieldKind::Query && std::env::var("QUARRY_TEST_AC").is_ok() {
            // QUARRY_TEST_AC=date shows the date list after a date field's colon instead.
            let date = std::env::var("QUARRY_TEST_AC").is_ok_and(|v| v == "date");
            let before = if date { "{ createdAt: " } else { "{ status: { $i" };
            if let Some((token, items)) = suggest(before, kind, paths, dates) {
                *ac = Ac { id: Some(id), token, items, sel: 0, cursor: before.chars().count(), rect: None };
            }
        }
        let (out, picked) = ac_field(ui, id, text, ac, &|b| suggest(b, kind, paths, dates), |ui, text, open| {
            egui::TextEdit::multiline(text)
                .id(id)
                .font(FontId::monospace(13.0))
                .hint_text(hint)
                .desired_width(width)
                .desired_rows(1)
                .lock_focus(open)
                .return_key(egui::KeyboardShortcut::new(egui::Modifiers::SHIFT, Key::Enter))
                .layouter(&mut code_layout)
                .show(ui)
        });
        !picked && out.response.has_focus() && ui.input(|i| i.key_pressed(Key::Enter) && !i.modifiers.shift)
    }

    fn documents(&mut self, ui: &mut egui::Ui) {
        let mut run = false;
        let mut explain = false;
        let mut code_lang = None;
        let key = self.col_key(false);
        let hist = self.prefs.history.get(&key).cloned().unwrap_or_default();
        let index_sorts: Vec<String> = self.t().map(|t| t.indexes.iter().filter_map(|ix| ix.get_document("keys").ok()).map(|k| db::shell_text(&Bson::Document(k.clone()))).collect()).unwrap_or_default();
        let lib: Vec<SavedQuery> = self.prefs.library.get(&key).map(|v| v.iter().filter(|q| q.pipeline.is_none()).cloned().collect()).unwrap_or_default();
        let mut qacts = Vec::new();
        let (t, ac, new_lib) = (&mut self.tabs[self.cur], &mut self.ac, &mut self.new_lib);
        let paths = std::mem::take(&mut t.paths);
        let dates = t.date_paths.clone();
        toolbar(ui, |ui| {
            ui.horizontal_top(|ui| {
                let w = ui.available_width() - 490.0;
                run |= Self::query_field(ui, "QUERY", &mut t.filter_txt, "{ field: value }   or paste db.coll.find(...) / db.coll.aggregate([...])", w, FieldKind::Query, &paths, &dates, ac);
                if primary(ui, "  Find  ", PRIMARY()).on_hover_text("Enter").clicked() {
                    run = true;
                }
                explain |= ui.button("Explain").on_hover_text("Cmd+E").clicked();
                code_menu(ui, &mut code_lang);
                history_menu(ui, &hist, &index_sorts, &mut qacts);
                library_menu(ui, &lib, new_lib, false, &mut qacts);
            });
            // A pasted shell query carries its own sort and projection.
            if !db::is_shell(&t.filter_txt) {
                ui.horizontal_top(|ui| {
                    let w = (ui.available_width() - 160.0) / 2.0;
                    run |= Self::query_field(ui, "SORT", &mut t.sort_txt, "{ _id: -1 }", w, FieldKind::Sort, &paths, &[], ac);
                    run |= Self::query_field(ui, "PROJECT", &mut t.proj_txt, "{ name: 1 }", w, FieldKind::Project, &paths, &[], ac);
                });
            }
        });
        t.paths = paths;
        for a in qacts {
            let t = &mut self.tabs[self.cur];
            match a {
                QAct::Filter(v) => t.filter_txt = v,
                QAct::Sort(v) => t.sort_txt = v,
                QAct::Project(v) => t.proj_txt = v,
                QAct::Load(q) => {
                    (t.filter_txt, t.sort_txt, t.proj_txt) = (q.filter, q.sort, q.project);
                    run = true;
                }
                QAct::Save(name) => {
                    let q = SavedQuery { name: name.clone(), filter: t.filter_txt.clone(), sort: t.sort_txt.clone(), project: t.proj_txt.clone(), pipeline: None };
                    self.save_to_library(q);
                }
                QAct::Delete(name) => self.delete_from_library(&name, false),
            }
        }
        if run {
            self.run_query();
        }
        if explain {
            self.explain();
        }
        if let Some(l) = code_lang {
            self.show_code(l, true);
        }
        ui.add_space(6.0);
        let t = self.t().unwrap();
        let (count, ms, loading, page) = (t.count, t.ms, t.loading, t.page);
        let pages = count.div_ceil(PAGE_SIZE).max(1);
        let mut go: Option<u64> = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} documents", group(&count.to_string()))).color(BRIGHT()));
            ui.label(RichText::new(format!("{ms} ms")).color(MUTED()));
            if loading {
                spin(ui);
            }
            ui.separator();
            if ui.add_enabled(page > 0, egui::Button::new("‹")).clicked() {
                go = Some(page - 1);
            }
            ui.label(format!("page {} of {}", page + 1, pages));
            if ui.add_enabled(page + 1 < pages, egui::Button::new("›")).clicked() {
                go = Some(page + 1);
            }
            if ui.button("Reload").clicked() {
                go = Some(page);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button("More", |ui| {
                    if ui.button("Open query in Shell").clicked() {
                        let t = &mut self.tabs[self.cur];
                        t.shell.code = query_script(&t.sel.coll, &t.filter_txt);
                        t.view = View::Shell;
                        t.selected = None;
                        ui.close();
                    }
                    if ui.button("Update matching…").clicked() {
                        self.modal = Some(Modal::UpdateMany { set_txt: "{\n  \n}".into(), unset_txt: String::new(), preview: None });
                        ui.close();
                    }
                    ui.separator();
                    let t = self.t().unwrap();
                    let (coll, filter) = (t.sel.coll.clone(), t.query.filter.clone());
                    if ui.button("Delete all matching documents…").clicked() {
                        self.modal = Some(Modal::Typed {
                            title: format!("Delete every document in {coll} matching the current filter (about {count} documents)?"),
                            expect: coll.clone(),
                            typed: String::new(),
                            action: Danger::DeleteMatching(filter),
                        });
                        ui.close();
                    }
                    if ui.button("Drop collection…").clicked() {
                        self.modal = Some(Modal::Typed {
                            title: format!("Drop the collection {coll} with all its documents and indexes? This cannot be undone."),
                            expect: coll,
                            typed: String::new(),
                            action: Danger::DropCollection,
                        });
                        ui.close();
                    }
                });
                if ui.button("Export…").clicked() {
                    self.export();
                }
                if ui.button("+ Insert").clicked() {
                    self.insert = Some("{\n  \n}".into());
                }
                let pending = self.tab_drafts();
                if !pending.is_empty() && primary(ui, &format!("Save all ({})", pending.len()), WARNING()).on_hover_text("Documents with unsaved edits").clicked() {
                    if !self.sel_writable() {
                        self.ask_unlock();
                    } else if self.prefs.confirm_save && !self.prefs.always_writable {
                        self.modal = Some(Modal::SaveAll(pending));
                    } else {
                        for k in &pending {
                            self.write_draft(k);
                        }
                    }
                }
            });
        });
        if let Some(p) = go {
            self.tm().unwrap().page = p;
            self.reload();
        }
        ui.add_space(4.0);
        self.results(ui, false);
    }

    fn columns_menu(&mut self, ui: &mut egui::Ui, agg: bool) {
        let key = self.col_key(agg);
        let t = self.t().unwrap();
        let fields = if agg { t.agg_fields.clone() } else { t.fields.clone() };
        let prefs = self.prefs.cols.get(&key).cloned().unwrap_or_default();
        let n = self.prefs.default_cols;
        let shown = visible_cols(Some(&prefs), &fields, n);
        let mut acts = Vec::new();
        let mut add = None;
        sticky_menu(ui, format!("Columns ({})", shown.len()), |ui| {
            ui.set_min_width(360.0);
            ui.add(egui::TextEdit::singleline(&mut self.col_filter).hint_text("Filter fields").desired_width(f32::INFINITY).margin(egui::Margin::symmetric(10, 6)));
            let needle = self.col_filter.trim().to_lowercase();
            let keep = |c: &str| needle.is_empty() || c.to_lowercase().contains(&needle);
            ui.add_space(6.0);
            ui.label(RichText::new("SHOWN  ·  drag to reorder, pinned columns stay first").size(11.0).color(MUTED()));
            egui::ScrollArea::vertical().id_salt("shown").max_height(320.0).show(ui, |ui| {
                for (i, c) in shown.iter().enumerate().filter(|(_, c)| keep(c)) {
                    let row = ui.horizontal(|ui| {
                        ui.set_min_height(28.0);
                        ui.dnd_drag_source(egui::Id::unique(("col-drag", c)), i, grip).response.on_hover_cursor(egui::CursorIcon::Grab);
                        let pinned = prefs.pinned.contains(c);
                        if pin_button(ui, pinned).on_hover_text(if pinned { "Unpin" } else { "Pin: keep this column first" }).clicked() {
                            acts.push(ColAct::Pin(c.clone()));
                        }
                        ui.label(RichText::new(c).monospace().color(if fields.contains(c) { TEXT() } else { PRIMARY() }));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if icon_button(ui, "×", true).on_hover_text("Hide").clicked() {
                                acts.push(ColAct::Hide(c.clone()));
                            }
                            if icon_button(ui, "↓", i + 1 < shown.len()).on_hover_text("Move down").clicked() {
                                acts.push(ColAct::Move(c.clone(), 1));
                            }
                            if icon_button(ui, "↑", i > 0).on_hover_text("Move up").clicked() {
                                acts.push(ColAct::Move(c.clone(), -1));
                            }
                        });
                    }).response;
                    // Drop on the upper half to go before this row, the lower half to go after it.
                    let below = ui.input(|inp| inp.pointer.hover_pos()).is_some_and(|p| p.y > row.rect.center().y);
                    if row.dnd_hover_payload::<usize>().is_some() {
                        let y = if below { row.rect.bottom() } else { row.rect.top() };
                        ui.painter().hline(row.rect.x_range(), y, Stroke::new(2.0, PRIMARY()));
                    }
                    if let Some(from) = row.dnd_release_payload::<usize>() {
                        acts.push(ColAct::MoveTo(shown[*from].clone(), i + below as usize));
                    }
                }
            });
            let hidden: Vec<&String> = fields.iter().filter(|f| !shown.contains(f) && keep(f)).collect();
            if !hidden.is_empty() {
                ui.separator();
                ui.label(RichText::new(format!("MORE FIELDS ({})", hidden.len())).size(11.0).color(MUTED()));
                egui::ScrollArea::vertical().id_salt("more").max_height(200.0).show(ui, |ui| {
                    for f in hidden {
                        if ui.add(egui::Button::new(RichText::new(format!("+ {f}")).monospace()).frame(false).min_size(egui::vec2(ui.available_width(), 26.0))).clicked() {
                            acts.push(ColAct::Show(f.clone()));
                        }
                    }
                });
            }
            ui.separator();
            ui.label(RichText::new("CUSTOM FIELD").size(11.0).color(MUTED()));
            ui.horizontal(|ui| {
                let w = ui.available_width() - 64.0;
                let r = ui.add(egui::TextEdit::singleline(&mut self.new_col).hint_text("customer.name").desired_width(w).font(FontId::monospace(13.0)).margin(egui::Margin::symmetric(10, 6)));
                if (ui.add(egui::Button::new("Add").min_size(egui::vec2(56.0, 28.0))).clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)))) && !self.new_col.trim().is_empty() {
                    add = Some(self.new_col.trim().to_string());
                }
            });
            // Nested paths seen in the loaded documents, for paths the field list does not offer.
            let typed = self.new_col.trim().to_lowercase();
            if !typed.is_empty() {
                let paths = if agg { &fields } else { &self.tabs[self.cur].paths };
                for path in paths.iter().filter(|x| x.to_lowercase().contains(&typed) && !shown.contains(x)).take(8) {
                    if ui.add(egui::Button::new(RichText::new(path).monospace()).frame(false).min_size(egui::vec2(ui.available_width(), 24.0))).clicked() {
                        add = Some(path.clone());
                    }
                }
            }
            ui.label(RichText::new("Dotted paths reach inside objects and arrays, like items.0.sku").size(11.0).color(FAINT()));
            if ui.button(format!("Reset to the first {n} fields")).clicked() {
                acts.push(ColAct::Reset);
            }
        });
        if let Some(c) = add {
            acts.push(ColAct::Show(c));
            self.new_col.clear();
        }
        if !acts.is_empty() {
            self.apply_col_acts(&key, &fields, acts);
        }
    }

    fn apply_col_acts(&mut self, key: &str, fields: &[String], acts: Vec<ColAct>) {
        let n = self.prefs.default_cols;
        let p = self.prefs.cols.entry(key.to_string()).or_default();
        for a in acts {
            let mut shown = visible_cols(Some(p), fields, n);
            match a {
                ColAct::Highlight(c) => {
                    let c = db::strip_indices(&c);
                    if !p.highlight.contains(&c) {
                        p.highlight.push(c);
                    } else {
                        p.highlight.retain(|x| *x != c);
                    }
                    continue;
                }
                ColAct::Pin(c) => {
                    if !p.pinned.contains(&c) {
                        p.pinned.push(c);
                    } else {
                        p.pinned.retain(|x| *x != c);
                    }
                    continue;
                }
                ColAct::Hide(c) => {
                    shown.retain(|x| *x != c);
                    p.pinned.retain(|x| *x != c);
                }
                ColAct::Move(c, d) => {
                    if let Some(i) = shown.iter().position(|x| *x == c) {
                        let j = (i as i32 + d).clamp(0, shown.len() as i32 - 1) as usize;
                        let pinned_n = p.pinned.iter().filter(|x| shown.contains(x)).count();
                        shown.swap(i, j);
                        // Moving across the edge of the pinned block changes which columns are pinned.
                        p.pinned = shown[..pinned_n].to_vec();
                    }
                }
                ColAct::MoveTo(c, to) => {
                    if let Some(i) = shown.iter().position(|x| *x == c) {
                        let pinned_n = p.pinned.iter().filter(|x| shown.contains(x)).count();
                        let c = shown.remove(i);
                        shown.insert(if to > i { to - 1 } else { to }.min(shown.len()), c);
                        p.pinned = shown[..pinned_n].to_vec();
                    }
                }
                ColAct::Show(c) => {
                    if !shown.contains(&c) {
                        shown.push(c);
                    }
                }
                ColAct::Reset => {
                    *p = ColPrefs::default();
                    continue;
                }
            }
            p.shown = Some(shown);
        }
        self.save_prefs();
    }

    /// Table or JSON list of the current results, with the open document shown under its row.
    fn results(&mut self, ui: &mut egui::Ui, agg: bool) {
        let t = self.tm().unwrap();
        // Taken out for the frame so the open document can be edited through `self`.
        let docs = std::mem::take(if agg { &mut t.agg_docs } else { &mut t.docs });
        let fields = if agg { t.agg_fields.clone() } else { t.fields.clone() };
        let base = match (agg, t.view) {
            (true, View::Shell) => t.shell.page as u64 * PAGE_SIZE,
            (true, _) => 0,
            _ => t.page * PAGE_SIZE,
        };
        let loading = t.loading;
        let closed = self.t().and_then(|t| self.conn(t.sel.conn)).filter(|c| c.status != Status::Ready && !self.demo).map(|c| (c.meta.id, c.meta.name.clone(), c.status == Status::Connecting));
        if let (true, Some((id, name, connecting))) = (docs.is_empty(), closed) {
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                if connecting {
                    spin(ui);
                    ui.label(RichText::new(format!("Connecting to {name}…")).color(MUTED()));
                    return;
                }
                ui.label(RichText::new(format!("Connect to {name} to load this tab.")).color(MUTED()));
                if primary(ui, "Connect", PRIMARY()).clicked() {
                    self.active = Some(id);
                    self.connect(id);
                }
            });
        } else if docs.is_empty() {
            ui.add_space(20.0);
            ui.label(RichText::new(if loading { "Loading…" } else { "No documents" }).color(MUTED()));
        } else {
            if !agg {
                self.selection_bar(ui, &docs);
            }
            if self.prefs.table {
                self.doc_table(ui, agg, &docs, &fields, base);
            } else {
                self.doc_list(ui, agg, &docs);
            }
        }
        let t = self.tm().unwrap();
        *(if agg { &mut t.agg_docs } else { &mut t.docs }) = docs;
    }

    /// Row click: Cmd toggles the row's check, Shift checks a range, a plain click opens it.
    fn row_click(&mut self, ui: &egui::Ui, agg: bool, i: usize) {
        let m = ui.input(|i| i.modifiers);
        if !agg && (m.command || m.shift) {
            self.toggle_check(i, m.shift);
        } else if self.prefs.open_inline {
            // Rows open and close on their own; the last one opened takes edits and keys.
            let t = self.tm().unwrap();
            let key = (agg, i);
            if t.expanded.remove(&key) || t.selected == Some(key) {
                self.close_row(key);
            } else {
                t.expanded.insert(key);
                self.select(agg, i, false);
            }
        } else {
            self.select(agg, i, true);
        }
    }

    fn close_row(&mut self, key: (bool, usize)) {
        let t = self.tm().unwrap();
        t.expanded.remove(&key);
        if t.selected == Some(key) {
            t.selected = None;
            self.reset_doc_state();
        }
    }

    fn toggle_check(&mut self, i: usize, range: bool) {
        let t = self.tm().unwrap();
        match t.check_anchor.filter(|_| range) {
            Some(a) => t.checked.extend(a.min(i)..=a.max(i)),
            None => {
                if !t.checked.insert(i) {
                    t.checked.remove(&i);
                }
            }
        }
        t.check_anchor = Some(i);
    }

    /// The documents a row action applies to: every checked one when the row is checked, else that row.
    fn act_targets(checked: &BTreeSet<usize>, docs: &[Document], i: usize) -> Vec<Document> {
        if checked.contains(&i) { Self::checked_docs(checked, docs) } else { docs.get(i).cloned().into_iter().collect() }
    }

    fn checked_docs(checked: &BTreeSet<usize>, docs: &[Document]) -> Vec<Document> {
        checked.iter().filter_map(|&j| docs.get(j).cloned()).collect()
    }

    fn selection_bar(&mut self, ui: &mut egui::Ui, docs: &[Document]) {
        let n = self.t().unwrap().checked.len();
        // Hidden under dialogs, which would otherwise sit behind it.
        if n == 0 || self.modal.is_some() || self.insert.is_some() {
            return;
        }
        let mut cancel = false;
        // Floats over the bottom of the results so checking rows never shifts the table.
        let area = ui.available_rect_before_wrap();
        let act = egui::Area::new(egui::Id::unique("selection-toolbar"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::CENTER_BOTTOM)
            .fixed_pos(egui::pos2(area.center().x, area.bottom() - 16.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).inner_margin(egui::Margin::symmetric(12, 8)).corner_radius(10).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{n} selected")).strong().color(BRIGHT()));
                        ui.add_space(8.0);
                        let act = docs_menu(ui, n, false);
                        cancel = ui.button("Cancel").on_hover_text("Clear the selection").clicked();
                        act
                    })
                    .inner
                })
                .inner
            })
            .inner;
        if cancel {
            self.tm().unwrap().checked.clear();
        } else if let Some(a) = act {
            let sel = Self::checked_docs(&self.t().unwrap().checked, docs);
            self.docs_act(ui.ctx(), a, sel);
        }
    }

    fn docs_act(&mut self, ctx: &egui::Context, act: DocsAct, docs: Vec<Document>) {
        if docs.is_empty() {
            return;
        }
        let what = if docs.len() == 1 { "1 document".to_string() } else { format!("{} documents", docs.len()) };
        let value = || if docs.len() == 1 { Bson::Document(docs[0].clone()) } else { Bson::Array(docs.iter().cloned().map(Bson::Document).collect()) };
        let by_id = || {
            let mut ids: Vec<Bson> = docs.iter().filter_map(|d| d.get("_id").cloned()).collect();
            Bson::Document(if ids.len() == 1 { doc! { "_id": ids.remove(0) } } else { doc! { "_id": { "$in": ids } } })
        };
        match act {
            DocsAct::CopyJs => {
                ctx.copy_text(db::pretty_shell(&value()));
                self.ok(format!("Copied {what} as JS"));
            }
            DocsAct::CopyJson => {
                ctx.copy_text(db::to_edit_value(&value()));
                self.ok(format!("Copied {what} as JSON"));
            }
            DocsAct::CopyLines => {
                let lines: Vec<String> = docs.iter().map(|d| db::to_edit_line(&Bson::Document(d.clone()))).collect();
                ctx.copy_text(lines.join("\n"));
                self.ok(format!("Copied {what} as JSON lines"));
            }
            DocsAct::Duplicate => {
                let copies: Vec<Document> = docs.into_iter().map(|mut d| {
                    d.remove("_id");
                    d
                }).collect();
                // One copy opens the insert editor to tweak it first; many are written straight away.
                if let [c] = copies.as_slice() {
                    self.insert = Some(db::to_edit_json(c));
                } else if let Some((conn, target)) = self.writable_target() {
                    self.worker.import(conn, target, copies, None);
                }
            }
            DocsAct::Export => {
                let coll = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
                let file = rfd::FileDialog::new().set_file_name(format!("{coll}-selection.json")).add_filter("JSON", &["json"]).add_filter("CSV", &["csv"]).add_filter("TSV", &["tsv"]).save_file();
                let Some(path) = file else { return };
                let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("json").to_ascii_lowercase();
                match db::export_body(&docs, &ext).and_then(|b| std::fs::write(&path, b).map_err(|e| format!("Could not write {}: {e}", path.display()))) {
                    Ok(()) => self.ok(format!("Exported {what} to {}", path.display())),
                    Err(e) => self.err(e),
                }
            }
            DocsAct::Filter => {
                let t = self.tm().unwrap();
                t.filter_txt = db::shell_text(&by_id());
                t.page = 0;
                self.run_query();
            }
            DocsAct::Update => {
                let t = self.tm().unwrap();
                t.shell.code = format!(
                    "async function run() {{\n  return await {}.updateMany(\n    {},\n    {{ $set: {{  }} }}\n  );\n}}",
                    collection_ref(&t.sel.coll),
                    db::pretty_shell(&by_id())
                );
                t.view = View::Shell;
            }
            DocsAct::Delete => self.modal = Some(Modal::Delete(docs)),
        }
    }

    fn select(&mut self, agg: bool, i: usize, toggle: bool) {
        let t = self.tm().unwrap();
        let same = t.selected == Some((agg, i));
        t.selected = if same && toggle { None } else { Some((agg, i)) };
        if !same || toggle {
            self.reset_doc_state();
        }
    }

    fn doc_table(&mut self, ui: &mut egui::Ui, agg: bool, docs: &[Document], fields: &[String], base: u64) {
        let key = self.col_key(agg);
        let prefs = self.prefs.cols.get(&key).cloned().unwrap_or_default();
        let cols = visible_cols(Some(&prefs), fields, self.prefs.default_cols);
        let summarize = !self.prefs.object_preview;
        // Measuring lays out every loaded cell of the column, so it is done once per result set
        // (keyed by where the documents live and how many) instead of on every frame.
        let widths: Vec<f32> = cols
            .iter()
            .map(|c| {
                prefs.widths.get(c).copied().unwrap_or_else(|| {
                    let k = (docs.as_ptr() as usize, docs.len(), c.clone(), summarize);
                    if let Some(w) = self.width_cache.get(&k) {
                        return *w;
                    }
                    let w = optimal_width(ui, docs, c, summarize);
                    if self.width_cache.len() > 512 {
                        self.width_cache.clear();
                    }
                    self.width_cache.insert(k, w);
                    w
                })
            })
            .collect();
        // The documents view adds a checkbox in front of the row number.
        let num_w = if agg { NUM_W } else { NUM_W + CHECK_W };
        let total: f32 = num_w + widths.iter().sum::<f32>();
        let selected = self.t().unwrap().selected;
        let checked = if agg { BTreeSet::new() } else { self.t().unwrap().checked.clone() };
        let expanded = self.t().unwrap().expanded.clone();
        let mut check_click = None;
        let mut check_all = None;
        let mut doc_act = None;
        let view_w = ui.available_width();
        let mut click = None;
        let mut edit_cell = None;
        let mut col_acts = Vec::new();
        let mut resized = None;
        let mut save_widths = false;
        let mut fit = None;
        let mut sort_set: Option<(String, Option<i64>)> = None;
        let mut cell_acts: Vec<(usize, Vec<TreeAct>)> = Vec::new();
        // Single-field sort shown in the header; clicking cycles ascending, descending, off.
        let sort = if agg { None } else { self.t().and_then(|t| t.query.sort.clone()).filter(|s| s.len() == 1) };
        let dir_of = |c: &str| sort.as_ref().and_then(|s| s.get(c)).and_then(|v| match v {
            Bson::Int32(n) => Some(*n as i64),
            Bson::Int64(n) => Some(*n),
            Bson::Double(f) => Some(*f as i64),
            _ => None,
        });
        let hl_cols: Vec<bool> = cols.iter().map(|c| prefs.highlight.contains(&db::strip_indices(c))).collect();
        let related = if agg { HashMap::new() } else { self.t().map(|t| t.related.clone()).unwrap_or_default() };
        egui::Frame::new().fill(PANEL()).corner_radius(8).inner_margin(egui::Margin::symmetric(4, 4)).show(ui, |ui| {
            let mut area = egui::ScrollArea::both().id_salt(("res", agg)).auto_shrink(false);
            // QUARRY_SCROLL_TEST slides the table sideways every frame, to measure scrolling cost.
            if std::env::var("QUARRY_SCROLL_TEST").is_ok() {
                area = area.horizontal_scroll_offset((ui.ctx().cumulative_frame_nr() % 60) as f32 * 25.0);
            }
            area.show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let row_w = total.max(view_w - 12.0);
                // Header
                let (hr, _) = ui.allocate_exact_size(egui::vec2(row_w, 30.0), Sense::hover());
                let p = ui.painter();
                p.text(egui::pos2(hr.left() + 10.0, hr.center().y), egui::Align2::LEFT_CENTER, "#", FontId::proportional(11.5), FAINT());
                if !agg {
                    let all = checked.len() == docs.len();
                    let r = check_box(ui, egui::pos2(hr.left() + 13.0, hr.center().y), ui.scope_id().with("check-all"), all, !all && !checked.is_empty());
                    if r.on_hover_text("Select every document on this page").clicked() {
                        check_all = Some(!all);
                    }
                }
                let mut x = hr.left() + num_w;
                for (c, w) in cols.iter().zip(&widths) {
                    let cell = egui::Rect::from_min_size(egui::pos2(x, hr.top()), egui::vec2(*w, hr.height()));
                    let pinned = prefs.pinned.contains(c);
                    let color = if pinned { WARNING() } else if fields.contains(c) { MUTED() } else { PRIMARY() };
                    let dir = dir_of(c);
                    let hresp = ui.interact(cell.shrink2(egui::vec2(4.0, 0.0)), ui.scope_id().with(("h", c)), Sense::click());
                    if hresp.hovered() && !agg {
                        ui.painter().rect_filled(cell.shrink(2.0), 4, HOVER());
                    }
                    ui.painter().with_clip_rect(cell.shrink2(egui::vec2(6.0, 0.0))).text(egui::pos2(x + 8.0, cell.center().y), egui::Align2::LEFT_CENTER, c, FontId::proportional(12.5), if dir.is_some() { BRIGHT() } else { color });
                    if let Some(d) = dir {
                        let a = egui::pos2(cell.right() - 14.0, cell.center().y);
                        let pts = if d > 0 { vec![a + egui::vec2(-4.0, 2.5), a + egui::vec2(4.0, 2.5), a + egui::vec2(0.0, -3.0)] } else { vec![a + egui::vec2(-4.0, -2.5), a + egui::vec2(4.0, -2.5), a + egui::vec2(0.0, 3.0)] };
                        ui.painter().add(egui::Shape::convex_polygon(pts, PRIMARY(), Stroke::NONE));
                    }
                    let hresp = if agg { hresp.on_hover_text("Right-click for column options") } else { hresp.on_hover_text("Click to sort, right-click for column options") };
                    if hresp.clicked() && !agg {
                        let next = match dir {
                            None => Some(1),
                            Some(d) if d > 0 => Some(-1),
                            _ => None,
                        };
                        sort_set = Some((c.clone(), next));
                    }
                    hresp.context_menu(|ui| {
                        // Same three column actions as the original's table (renderer line 106057).
                        if !agg {
                            if ui.button("Sort ascending").clicked() {
                                sort_set = Some((c.clone(), Some(1)));
                            }
                            if ui.button("Sort descending").clicked() {
                                sort_set = Some((c.clone(), Some(-1)));
                            }
                            if ui.add_enabled(dir.is_some(), egui::Button::new("Unsort")).clicked() {
                                sort_set = Some((c.clone(), None));
                            }
                            ui.separator();
                        }
                        if ui.button(if pinned { "Unpin" } else { "Pin to the left" }).clicked() {
                            col_acts.push(ColAct::Pin(c.clone()));
                        }
                        if ui.button("Move left").clicked() {
                            col_acts.push(ColAct::Move(c.clone(), -1));
                        }
                        if ui.button("Move right").clicked() {
                            col_acts.push(ColAct::Move(c.clone(), 1));
                        }
                        if ui.button("Hide column").clicked() {
                            col_acts.push(ColAct::Hide(c.clone()));
                        }
                        ui.separator();
                        if ui.checkbox(&mut prefs.highlight.contains(&db::strip_indices(c)), "Highlight column").clicked() {
                            col_acts.push(ColAct::Highlight(c.clone()));
                            ui.close();
                        }
                    });
                    // Drag the right edge to resize.
                    let handle = egui::Rect::from_min_max(egui::pos2(cell.right() - 3.0, cell.top() + 6.0), egui::pos2(cell.right() + 3.0, cell.bottom() - 6.0));
                    let dr = ui.interact(handle, ui.scope_id().with(("w", c)), Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::ResizeColumn).on_hover_text("Drag to resize, double-click to fit");
                    if dr.double_clicked() {
                        fit = Some(c.clone());
                    }
                    ui.painter().vline(cell.right(), handle.y_range(), Stroke::new(1.0, if dr.hovered() || dr.dragged() { PRIMARY() } else { BORDER() }));
                    if dr.dragged() {
                        resized = Some((c.clone(), (*w + dr.drag_delta().x).clamp(50.0, 900.0)));
                    }
                    if dr.drag_stopped() {
                        save_widths = true;
                    }
                    x += w;
                }
                ui.painter().hline(hr.x_range(), hr.bottom(), Stroke::new(1.0, BORDER()));
                // Rows (at most one page, so no virtualisation is needed)
                for (i, d) in docs.iter().enumerate() {
                    let drafted = if agg { None } else { self.shown_doc(d) };
                    let d = drafted.as_ref().unwrap_or(d);
                    let active = selected == Some((agg, i));
                    let (rr, resp) = ui.allocate_exact_size(egui::vec2(row_w, ROW_H), Sense::click());
                    if active && std::mem::take(&mut self.scroll_sel) {
                        resp.scroll_to_me(None);
                    }
                    // Rows scrolled out of view keep their space but skip all layout and painting.
                    if ui.is_rect_visible(rr) {
                        let fill = if active {
                            tint(PRIMARY(), 32)
                        } else if checked.contains(&i) {
                            tint(PRIMARY(), 18)
                        } else if ui.rect_contains_pointer(rr) && !ui.ctx().any_popup_open() {
                            HOVER()
                        } else if i % 2 == 1 {
                            pal().stripe
                        } else {
                            Color32::TRANSPARENT
                        };
                        let p = ui.painter();
                        p.rect_filled(rr, 4, fill);
                        let off = num_w - NUM_W;
                        if self.prefs.open_inline {
                            chevron(p, egui::pos2(rr.left() + off + 9.0, rr.center().y), active || expanded.contains(&(agg, i)), if active { PRIMARY() } else { FAINT() });
                        }
                        let num = (base + i as u64 + 1).to_string();
                        let (num, nc) = if drafted.is_some() { (format!("{num}●"), WARNING()) } else { (num, FAINT()) };
                        p.text(egui::pos2(rr.left() + off + 18.0, rr.center().y), egui::Align2::LEFT_CENTER, num, FontId::proportional(11.5), nc);
                        if !agg && check_box(ui, egui::pos2(rr.left() + 13.0, rr.center().y), ui.scope_id().with(("check", i)), checked.contains(&i), false).clicked() {
                            check_click = Some(i);
                        }
                        let mut x = rr.left() + num_w;
                        for (ci, (c, w)) in cols.iter().zip(&widths).enumerate() {
                            let cell = egui::Rect::from_min_size(egui::pos2(x, rr.top()), egui::vec2(*w, ROW_H));
                            // Same for columns scrolled out sideways.
                            if !ui.is_rect_visible(cell) {
                                x += w;
                                continue;
                            }
                            if hl_cols[ci] {
                                ui.painter().rect_filled(cell, 0, HIGHLIGHT());
                            }
                            let clip = ui.painter().with_clip_rect(cell.shrink2(egui::vec2(6.0, 0.0)));
                            let v = db::get_path(d, c);
                            match v {
                                Some(v) => {
                                    let g = ui.fonts_mut(|f| f.layout_job(column_job(v, summarize)));
                                    let gw = g.size().x;
                                    clip.galley(egui::pos2(x + 8.0, cell.center().y - g.size().y / 2.0), g, TEXT());
                                    // Ids only, checked before compact(): that turns the whole value into JSON,
                                // which is costly for big objects on every frame.
                                let rel = (!related.is_empty() && matches!(v, Bson::ObjectId(_) | Bson::String(_))).then(|| related.get(&db::compact(v))).flatten();
                                if let Some(p) = rel {
                                        clip.text(egui::pos2(x + 14.0 + gw, cell.center().y), egui::Align2::LEFT_CENTER, p, FontId::proportional(12.0), C_LINK());
                                    }
                                }
                                None => {
                                    clip.text(egui::pos2(x + 8.0, cell.center().y), egui::Align2::LEFT_CENTER, "—", FontId::proportional(12.0), FAINT());
                                }
                            }
                            let cr = ui.interact(cell, ui.scope_id().with(("c", i, ci)), Sense::click());
                            if cr.double_clicked() {
                                edit_cell = Some((i, c.clone()));
                            } else if cr.clicked() {
                                click = Some(i);
                            }
                            let mut acts = Vec::new();
                            if let Some(v) = v {
                                let path: Vec<String> = c.split('.').map(String::from).collect();
                                cr.context_menu(|ui| {
                                    field_menu(ui, &path, v, !agg, hl_cols[ci], &mut acts);
                                    if !agg {
                                        ui.separator();
                                        doc_act = row_menu(ui, i, &checked).or(doc_act);
                                    }
                                });
                            } else if !agg {
                                cr.context_menu(|ui| {
                                    if ui.button("Edit full document as JSON…").clicked() {
                                        acts.push(TreeAct::EditDoc);
                                    }
                                    ui.separator();
                                    doc_act = row_menu(ui, i, &checked).or(doc_act);
                                });
                            }
                            if !acts.is_empty() {
                                cell_acts.push((i, acts));
                            }
                            x += w;
                        }
                    }
                    if resp.clicked() {
                        click = Some(i);
                    }
                    if !agg {
                        resp.context_menu(|ui| doc_act = row_menu(ui, i, &checked).or(doc_act));
                    }
                    if (active || expanded.contains(&(agg, i))) && self.prefs.open_inline {
                        ui.add_space(4.0);
                        // Spans every column like the rows above it, not just the visible part.
                        let w = row_w.max(320.0);
                        card(ui, |ui| {
                            // Card inner margin and stroke on both sides.
                            ui.set_width(w - 30.0);
                            ui.spacing_mut().item_spacing.y = 6.0;
                            self.doc_body(ui, d, agg, Some((agg, i)));
                        });
                        ui.add_space(6.0);
                    }
                }
            });
        });
        if let Some((c, w)) = resized {
            self.prefs.cols.entry(key.clone()).or_default().widths.insert(c, w);
        }
        if let Some(c) = fit {
            // Back to the measured width, like the original's "optimal" column width.
            self.prefs.cols.entry(key.clone()).or_default().widths.remove(&c);
            save_widths = true;
        }
        if save_widths {
            self.save_prefs();
        }
        if !col_acts.is_empty() {
            self.apply_col_acts(&key, fields, col_acts);
        }
        if let Some(i) = click {
            self.row_click(ui, agg, i);
        }
        if let Some(i) = check_click {
            self.toggle_check(i, ui.input(|i| i.modifiers.shift));
        }
        if let Some(on) = check_all {
            self.tm().unwrap().checked = if on { (0..docs.len()).collect() } else { BTreeSet::new() };
        }
        if let Some((i, a)) = doc_act {
            self.docs_act(&ui.ctx().clone(), a, Self::act_targets(&checked, docs, i));
        }
        for (i, acts) in cell_acts {
            self.select(agg, i, false);
            let d = self.shown_doc(&docs[i]).unwrap_or_else(|| docs[i].clone());
            self.handle_acts(&d, acts);
        }
        if let Some((c, next)) = sort_set {
            let t = self.tm().unwrap();
            t.query.sort = next.map(|d| doc! { c.clone(): d });
            t.sort_txt = t.query.sort.as_ref().map(|s| db::shell_text(&Bson::Document(s.clone()))).unwrap_or_default();
            t.page = 0;
            self.reload();
        }
        if let (Some((i, c)), false) = (edit_cell, agg) {
            self.select(agg, i, false);
            let path: Vec<String> = c.split('.').map(String::from).collect();
            let d = self.shown_doc(&docs[i]).unwrap_or_else(|| docs[i].clone());
            match db::get_path(&d, &c).cloned() {
                Some(v) if c != "_id" => self.start_inline(&d, path, v),
                Some(_) => {}
                None => self.err(format!("This document has no field {c}. Use + Add field.")),
            }
        }
    }

    fn doc_list(&mut self, ui: &mut egui::Ui, agg: bool, docs: &[Document]) {
        let selected = self.t().unwrap().selected;
        let checked = if agg { BTreeSet::new() } else { self.t().unwrap().checked.clone() };
        let expanded = self.t().unwrap().expanded.clone();
        let mut doc_act = None;
        let view_w = ui.available_width();
        let mut click = None;
        egui::Frame::new().fill(PANEL()).corner_radius(8).inner_margin(4).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt(("list", agg)).auto_shrink(false).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for (i, d) in docs.iter().enumerate() {
                    let drafted = if agg { None } else { self.shown_doc(d) };
                    let d = drafted.as_ref().unwrap_or(d);
                    let active = selected == Some((agg, i));
                    let w = ui.available_width();
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, ROW_H), Sense::click());
                    if active && std::mem::take(&mut self.scroll_sel) {
                        resp.scroll_to_me(None);
                    }
                    let fill = if active {
                        tint(PRIMARY(), 32)
                    } else if checked.contains(&i) {
                        tint(PRIMARY(), 18)
                    } else if resp.hovered() {
                        HOVER()
                    } else {
                        Color32::TRANSPARENT
                    };
                    ui.painter().rect_filled(rect, 4, fill);
                    let galley = ui.fonts_mut(|f| f.layout_job(row_job(d, w - 16.0)));
                    ui.painter().galley(rect.left_center() + egui::vec2(8.0, -galley.size().y / 2.0), galley, TEXT());
                    if resp.clicked() {
                        click = Some(i);
                    }
                    if !agg {
                        resp.context_menu(|ui| doc_act = row_menu(ui, i, &checked).or(doc_act));
                    }
                    if (active || expanded.contains(&(agg, i))) && self.prefs.open_inline {
                        ui.add_space(4.0);
                        card(ui, |ui| {
                            ui.set_width(view_w - 48.0);
                            ui.spacing_mut().item_spacing.y = 6.0;
                            self.doc_body(ui, d, agg, Some((agg, i)));
                        });
                        ui.add_space(6.0);
                    }
                }
            });
        });
        if let Some(i) = click {
            self.row_click(ui, agg, i);
        }
        if let Some((i, a)) = doc_act {
            self.docs_act(&ui.ctx().clone(), a, Self::act_targets(&checked, docs, i));
        }
    }

    /// Starts editing one value: small values in place, objects and arrays in the large JSON editor.
    fn start_inline(&mut self, d: &Document, path: Vec<String>, v: Bson) {
        if !self.sel_writable() {
            let d = d.clone();
            return self.ask_unlock_then(move |a| a.start_inline(&d, path, v));
        }
        self.new_field = None;
        if matches!(v, Bson::Document(_) | Bson::Array(_)) {
            self.inline = None;
            self.json_edit = Some(JsonEdit { base: d.clone(), path, text: db::to_edit_value(&v), kind: db::type_name_kind(&v), focus: true });
        } else {
            self.inline = Some(Inline { text: db::value_text(&v), kind: db::type_name_kind(&v), path, focus: true });
        }
    }

    /// Side panel mode: the open document on the right.
    fn inspector(&mut self, ui: &mut egui::Ui) {
        let Some(d) = self.selected_doc().cloned() else { return };
        let d = self.shown_doc(&d).unwrap_or(d);
        let from_agg = matches!(self.t().and_then(|t| t.selected), Some((true, _)));
        egui::Panel::right("inspector")
            .default_size(460.0)
            .size_range(320.0..=900.0)
            .resizable(true)
            .frame(egui::Frame::new().fill(PANEL()).inner_margin(12))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| self.doc_body(ui, &d, from_agg, None));
            });
    }

    /// The open document: actions, then its fields as a tree that can be edited in place.
    /// `row` is the inline row showing this document, None in the side panel. Only the selected
    /// document holds edit state; acting in another open one selects it first.
    fn doc_body(&mut self, ui: &mut egui::Ui, d: &Document, from_agg: bool, row: Option<(bool, usize)>) {
        let selected = self.t().and_then(|t| t.selected);
        let passive = row.is_some() && row != selected;
        // Edit state is keyed by field path only, so other open documents must not see it.
        let parked = passive.then(|| (self.inline.take(), self.new_field.take()));
        let mut acts = Vec::new();
        // The inline card can be wider than the table viewport; the header row stays within the
        // visible part so its buttons never sit behind the window edge.
        let full = ui.max_rect().x_range();
        let vis = ui.clip_rect().x_range();
        let (left, right) = (vis.min.max(full.min), vis.max.min(full.max) - 4.0);
        let w = (right - full.min).max(240.0);
        ui.allocate_ui_with_layout(egui::vec2(w, ui.spacing().interact_size.y), egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true), |ui| {
            ui.add_space((left - full.min).min(w - 240.0).max(0.0));
            let id = d.get("_id").map(db::short).unwrap_or_default();
            ui.label(RichText::new(id).monospace().color(C_OID()));
            ui.add_space(8.0);
            if !from_agg {
                if ui.small_button("Edit JSON").clicked() {
                    acts.push(TreeAct::EditDoc);
                }
                if ui.small_button("+ Add field").clicked() {
                    acts.push(TreeAct::AddField(Vec::new()));
                }
                if ui.small_button("Duplicate").clicked() {
                    self.docs_act(ui.ctx(), DocsAct::Duplicate, vec![d.clone()]);
                }
                if ui.small_button(RichText::new("Delete").color(ALERT())).clicked() {
                    self.docs_act(ui.ctx(), DocsAct::Delete, vec![d.clone()]);
                }
            }
            right_group(ui, |ui| {
                if ui.small_button("Close").on_hover_text("Esc").clicked() {
                    if let Some(key) = row.or(selected) {
                        self.close_row(key);
                    }
                }
                if ui.small_button("Copy JSON").clicked() {
                    self.docs_act(ui.ctx(), DocsAct::CopyJson, vec![d.clone()]);
                }
            });
        });
        ui.separator();
        let key = self.draft_key(d);
        self.changed = match self.drafts.get(&key) {
            Some(dr) if !from_agg => {
                let changes = db::diff(&dr.original, &dr.current);
                self.draft_bar(ui, &key, &changes);
                changes.into_iter().map(|c| c.0).collect()
            }
            _ => HashSet::new(),
        };
        let id = egui::Id::unique(("doc", d.get("_id").map(db::compact)));
        let mut path = Vec::new();
        self.fold_busy = false;
        // Runs every frame, so capped like doc_paths: deeper keys just truncate.
        fn longest(d: &Document, depth: usize) -> usize {
            d.iter().map(|(k, v)| k.chars().count().max(match v {
                _ if depth >= 4 => 0,
                Bson::Document(c) => longest(c, depth + 1),
                Bson::Array(a) => a.iter().take(20).filter_map(|v| v.as_document()).map(|c| longest(c, depth + 1)).max().unwrap_or(0),
                _ => 0,
            })).max().unwrap_or(0)
        }
        self.char_w = ui.fonts_mut(|f| f.glyph_width(&FontId::monospace(MONO_PX), 'x'));
        // Cached per document and field count, so open documents are not walked every frame.
        let cache = id.with(("key-chars", d.len()));
        let chars = ui.data(|m| m.get_temp::<usize>(cache)).unwrap_or_else(|| {
            let n = longest(d, 0).min(32);
            ui.data_mut(|m| m.insert_temp(cache, n));
            n
        });
        self.key_w = (chars as f32 * self.char_w + 24.0).clamp(96.0, 320.0);
        self.tree_x = ui.max_rect().x_range().expand(8.0);
        for (k, v) in d {
            path.push(k.clone());
            self.tree(ui, k, v, 0, id, &mut path, !from_agg, &mut acts);
            path.pop();
        }
        // Nested headers only render once their parent opens, so the fold stays forced until every animation settles.
        if !self.fold_busy && self.fold.as_ref().is_some_and(|f| f.0 == id) {
            self.fold = None;
        }
        if self.new_field.as_ref().is_some_and(|n| n.parent.is_empty()) {
            self.new_field_row(ui, &mut acts);
        }
        self.changed.clear();
        if let Some((inline, new_field)) = parked {
            self.inline = inline;
            self.new_field = new_field;
        }
        if let (true, Some((agg, i)), false) = (passive, row, acts.is_empty()) {
            self.select(agg, i, false);
        }
        self.handle_acts(d, acts);
    }

    /// Unsaved edits of one document: what changed, Save, Discard, and the confirm switch.
    fn draft_bar(&mut self, ui: &mut egui::Ui, key: &str, changes: &[(String, Option<Bson>, Option<Bson>)]) {
        let mut save = false;
        let mut discard = false;
        let mut confirm = self.prefs.confirm_save;
        egui::Frame::new().fill(tint(WARNING(), 22)).stroke(Stroke::new(1.0, WARNING().gamma_multiply(0.5))).corner_radius(6).inner_margin(8).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("● {} unsaved change(s)", changes.len())).color(WARNING()).strong());
                let list: Vec<&str> = changes.iter().take(5).map(|c| c.0.as_str()).collect();
                let more = if changes.len() > 5 { format!(" +{}", changes.len() - 5) } else { String::new() };
                ui.label(RichText::new(format!("{}{more}", list.join(", "))).monospace().size(12.0).color(MUTED()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    save = primary(ui, "Save", PRIMARY()).on_hover_text("Write all edits of this document at once (Cmd+S)").clicked() || ui.input(|i| i.modifiers.command && i.key_pressed(Key::S));
                    discard = ui.small_button("Discard").clicked();
                    if !self.prefs.always_writable {
                        ui.checkbox(&mut confirm, RichText::new("Ask before saving").size(12.0));
                    }
                });
            });
        });
        ui.add_space(4.0);
        if confirm != self.prefs.confirm_save {
            self.prefs.confirm_save = confirm;
            self.save_prefs();
        }
        if discard {
            self.drafts.remove(key);
            self.ok("Edits discarded");
        } else if save {
            self.save_draft(key);
        }
    }

    fn handle_acts(&mut self, d: &Document, acts: Vec<TreeAct>) {
        for a in acts {
            match a {
                TreeAct::Start(p, v) => self.start_inline(d, p, v),
                TreeAct::Cancel => {
                    self.inline = None;
                    self.new_field = None;
                }
                TreeAct::Commit => {
                    if let Some(i) = &self.inline {
                        let new = db::make_value(i.kind, &i.text).and_then(|v| db::edit_path(d, &i.path, Some(v)));
                        self.stage(d, new);
                    }
                }
                TreeAct::Remove(p) => {
                    if self.sel_writable() {
                        self.stage(d, db::edit_path(d, &p, None));
                    } else {
                        let d = d.clone();
                        self.ask_unlock_then(move |a| a.handle_acts(&d, vec![TreeAct::Remove(p)]));
                    }
                }
                TreeAct::AddField(parent) => {
                    if self.sel_writable() {
                        self.inline = None;
                        self.new_field = Some(NewField { parent, name: String::new(), text: String::new(), kind: "string" });
                    } else {
                        let d = d.clone();
                        self.ask_unlock_then(move |a| a.handle_acts(&d, vec![TreeAct::AddField(parent)]));
                    }
                }
                TreeAct::CommitNew => {
                    if let Some(n) = &self.new_field {
                        let is_array = !n.parent.is_empty() && matches!(db::get_path(d, &n.parent.join(".")), Some(Bson::Array(_)));
                        let name = n.name.trim().to_string();
                        if name.is_empty() && !is_array {
                            self.err("Give the new field a name.");
                            continue;
                        }
                        let mut path = n.parent.clone();
                        path.push(if is_array { usize::MAX.to_string() } else { name.clone() });
                        if !is_array && db::get_path(d, &path.join(".")).is_some() {
                            self.err(format!("{name} already exists. Double-click it to change it."));
                            continue;
                        }
                        let new = db::make_value(n.kind, &n.text).and_then(|v| db::edit_path(d, &path, Some(v)));
                        self.stage(d, new);
                    }
                }
                TreeAct::EditDoc => {
                    if self.sel_writable() {
                        self.inline = None;
                        self.json_edit = Some(JsonEdit { base: d.clone(), path: Vec::new(), text: db::to_edit_json(d), kind: "object", focus: true });
                    } else {
                        let d = d.clone();
                        self.ask_unlock_then(move |a| a.handle_acts(&d, vec![TreeAct::EditDoc]));
                    }
                }
                TreeAct::ShowFull(path, v) => {
                    let body = match &v {
                        Bson::String(s) => s.clone(),
                        Bson::Document(_) | Bson::Array(_) => db::to_edit_json(&doc! { "v": v.clone() }).lines().skip(1).collect::<Vec<_>>().join("\n").trim_start().trim_start_matches("\"v\": ").trim_end_matches('}').trim_end().to_string(),
                        other => db::value_text(other),
                    };
                    self.modal = Some(Modal::Text { title: path, body, code: !matches!(v, Bson::String(_)) });
                }
                TreeAct::PinColumn(path) => {
                    let key = self.col_key(false);
                    let fields = self.t().map(|t| t.fields.clone()).unwrap_or_default();
                    self.apply_col_acts(&key, &fields, vec![ColAct::Show(path.clone()), ColAct::Pin(path)]);
                    self.prefs.table = true;
                }
                TreeAct::Highlight(path) => {
                    let fields = self.t().map(|t| t.fields.clone()).unwrap_or_default();
                    self.apply_col_acts(&self.col_key(false), &fields, vec![ColAct::Highlight(path)]);
                }
                TreeAct::Filter(path, v, add) => {
                    let path = db::strip_indices(&path);
                    let t = self.tm().unwrap();
                    let mut f = if add { t.query.filter.clone() } else { Document::new() };
                    f.insert(path, v);
                    t.filter_txt = db::shell_text(&Bson::Document(f));
                    t.view = View::Documents;
                    t.selected = None;
                    self.reset_doc_state();
                    self.run_query();
                }
                TreeAct::Group(path, v, filtered) => {
                    let filter = if filtered { self.t().unwrap().query.filter.clone() } else { Document::new() };
                    // Counted in the background and shown in a window, so the tab's own pipeline is kept;
                    // "Open in Aggregate" opens the pipeline in a new tab, as the original does.
                    match (db::group_count_pipeline(&path, &v, filter), self.target()) {
                        (Err(e), _) => self.err(e),
                        (_, None) => self.err("Not connected."),
                        (Ok(p), Some(target)) => {
                            let conn = self.t().unwrap().sel.conn;
                            self.ok(format!("Counting the values of {path}…"));
                            self.worker.group_count(conn, target, path, p);
                        }
                    }
                }
                TreeAct::Distinct(path) => match self.target() {
                    Some(target) => {
                        let f = self.t().unwrap().query.filter.clone();
                        self.worker.distinct_count(target, path, f);
                    }
                    None => self.err("Not connected."),
                },
                TreeAct::Index(path) => {
                    self.idx_keys = format!("{{ {}: 1 }}", db::shell_text(&Bson::Document(doc! { db::strip_indices(&path): 1 })).trim_start_matches("{ ").trim_end_matches(": 1 }"));
                    let t = self.tm().unwrap();
                    t.view = View::Indexes;
                    t.selected = None;
                    self.reset_doc_state();
                    self.load_indexes();
                    self.ok("Check the keys, then press Create (writes must be unlocked).");
                }
                TreeAct::GoRef(path, v) => {
                    let Some(s) = self.t().map(|t| t.sel.clone()) else { continue };
                    let path = db::strip_indices(&path);
                    let rel = self.prefs.relations.get(&format!("{}/{}", s.conn, s.db)).and_then(|l| l.iter().find(|r| r.coll == s.coll && r.path == path)).cloned();
                    match (rel, self.conn(s.conn).and_then(|c| c.client.clone())) {
                        (_, None) => self.err("Not connected."),
                        (Some(r), _) => self.go_ref(s.conn, s.db, r.target, v),
                        (None, Some(client)) => {
                            let colls = self.conn(s.conn).and_then(|c| c.dbs.iter().find(|d| d.0 == s.db)).and_then(|d| d.1.clone()).unwrap_or_default();
                            self.ok("Looking for the collection that holds this id…");
                            self.worker.find_ref(s.conn, Target { client, db: s.db, coll: s.coll, timeout: self.timeout() }, colls, path, v);
                        }
                    }
                }
                TreeAct::Rename(path) => {
                    if self.sel_writable() {
                        let name = path.last().cloned().unwrap_or_default();
                        self.modal = Some(Modal::Rename { doc: d.clone(), path, name });
                    } else {
                        let d = d.clone();
                        self.ask_unlock_then(move |a| a.handle_acts(&d, vec![TreeAct::Rename(path)]));
                    }
                }
            }
        }
    }

    fn type_combo(ui: &mut egui::Ui, id: &'static str, kind: &mut &'static str) {
        egui::ComboBox::from_id_salt(id).width(86.0).selected_text(RichText::new(*kind).size(12.0)).show_ui(ui, |ui| {
            for k in db::KINDS {
                ui.selectable_value(kind, k, k);
            }
        });
    }

    fn new_field_row(&mut self, ui: &mut egui::Ui, acts: &mut Vec<TreeAct>) {
        let Some(n) = &mut self.new_field else { return };
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            let r = ui.add(egui::TextEdit::singleline(&mut n.name).hint_text("name").desired_width(120.0).font(FontId::monospace(12.5)));
            if n.name.is_empty() && !r.has_focus() && !ui.ctx().any_popup_open() {
                r.request_focus();
            }
            let v = ui.add(egui::TextEdit::singleline(&mut n.text).hint_text("value").desired_width(EDITOR_W - 60.0).font(FontId::monospace(12.5)));
            Self::type_combo(ui, "newkind", &mut n.kind);
            if ui.small_button("Add").clicked() || ((r.lost_focus() || v.lost_focus()) && ui.input(|i| i.key_pressed(Key::Enter))) {
                acts.push(TreeAct::CommitNew);
            }
            if ui.small_button("×").clicked() {
                acts.push(TreeAct::Cancel);
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn tree(&mut self, ui: &mut egui::Ui, key: &str, v: &Bson, depth: usize, id: egui::Id, path: &mut Vec<String>, editable: bool, acts: &mut Vec<TreeAct>) {
        let editing_here = self.inline.as_ref().is_some_and(|i| i.path == *path);
        match v {
            Bson::Document(_) | Bson::Array(_) if !editing_here => {
                let items: Vec<(String, &Bson)> = match v {
                    Bson::Document(d) => d.iter().map(|(k, v)| (k.clone(), v)).collect(),
                    Bson::Array(a) => a.iter().enumerate().map(|(i, v)| (i.to_string(), v)).collect(),
                    _ => unreachable!(),
                };
                let band = ui.painter().add(egui::Shape::Noop);
                let key_px = key.chars().count() as f32 * self.char_w;
                let mut job = LayoutJob::default();
                job.append(key, 0.0, mono(BRIGHT()));
                job.append(&leaf_text(v), (self.key_w - key_px).max(12.0), mono(FAINT()));
                let adding_here = self.new_field.as_ref().is_some_and(|n| n.parent == *path);
                let forced = self.fold.as_ref().filter(|f| f.0 == id && path.starts_with(&f.1)).map(|f| f.2);
                let r = egui::CollapsingHeader::new(job)
                    .id_salt(id.with(&*path))
                    .default_open(depth < 1)
                    .open(adding_here.then_some(true).or(forced))
                    .show(ui, |ui| {
                        for (k, child) in items {
                            path.push(k.clone());
                            self.tree(ui, &k, child, depth + 1, id, path, editable, acts);
                            path.pop();
                        }
                        if adding_here {
                            self.new_field_row(ui, acts);
                        }
                    });
                if highlighted(&self.prefs, &self.col_key(false), path) {
                    ui.painter().rect_filled(r.header_response.rect.expand(2.0), 4, HIGHLIGHT());
                }
                self.hover_band(ui, band, r.header_response.rect.y_range());
                if editable && r.header_response.double_clicked() && path.first().is_some_and(|k| k != "_id") {
                    acts.push(TreeAct::Start(path.clone(), v.clone()));
                }
                if forced.is_some_and(|o| r.openness != if o { 1.0 } else { 0.0 }) {
                    self.fold_busy = true;
                }
                let p = path.clone();
                let mut fold = None;
                let hl = highlighted(&self.prefs, &self.col_key(false), &p);
                r.header_response.on_hover_text("Double-click to edit as JSON, right-click for more").context_menu(|ui| {
                    field_menu(ui, &p, v, editable, hl, acts);
                    fold = fold_menu(ui, &p, true);
                });
                if let Some((p, open)) = fold {
                    self.fold = Some((id, p, open));
                }
            }
            leaf => {
                let hl = highlighted(&self.prefs, &self.col_key(false), path);
                let mut fold = None;
                let edited = !self.changed.is_empty() && self.changed.contains(&path.join("."));
                let band = ui.painter().add(egui::Shape::Noop);
                let row = egui::Frame::new().fill(if edited { tint(WARNING(), 34) } else if hl { HIGHLIGHT() } else { Color32::TRANSPARENT }).corner_radius(4).show(ui, |ui| ui.horizontal(|ui| {
                    // Same offsets as a collapsing header so keys and values line up across both.
                    let x0 = ui.cursor().left();
                    ui.add_space(ui.spacing().indent);
                    let key_w = self.key_w;
                    let key_r = ui.scope(|ui| {
                        ui.set_max_width(key_w - 8.0);
                        ui.add(egui::Label::new(RichText::new(key).font(FontId::monospace(MONO_PX)).color(MUTED())).truncate().sense(Sense::click()))
                    }).inner;
                    ui.add_space((x0 + ui.spacing().indent + key_w - ui.cursor().left()).max(0.0));
                    if editing_here {
                        let inl = self.inline.as_mut().unwrap();
                        let r = ui.add(egui::TextEdit::singleline(&mut inl.text).desired_width(EDITOR_W).font(FontId::monospace(12.5)).clip_text(true));
                        if inl.focus {
                            r.request_focus();
                            inl.focus = false;
                        }
                        Self::type_combo(ui, "inlinekind", &mut inl.kind);
                        if ui.small_button("✓").on_hover_text("Review and save (Enter)").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))) {
                            acts.push(TreeAct::Commit);
                        }
                        if ui.small_button("×").on_hover_text("Cancel (Esc)").clicked() {
                            acts.push(TreeAct::Cancel);
                        }
                    } else {
                        // A one-line label never shows more than this; laying out a megabyte string on
                        // every frame stalls scrolling. "Show full value" opens the whole thing.
                        const LEAF_MAX: usize = 2000;
                        let text = match leaf {
                            Bson::String(s) if s.len() > LEAF_MAX => format!("\"{}…\"", s.chars().take(LEAF_MAX).collect::<String>()),
                            _ => leaf_text(leaf),
                        };
                        let r = ui.add(egui::Label::new(RichText::new(&text).monospace().color(color_of(leaf))).truncate().sense(Sense::click()));
                        ui.label(RichText::new(db::type_name(leaf)).size(11.0).color(FAINT()));
                        let id_like = matches!(leaf, Bson::ObjectId(_)) || matches!(leaf, Bson::String(s) if s.len() <= 64);
                        if let Some(p) = self.t().filter(|t| id_like && !t.related.is_empty()).and_then(|t| t.related.get(&db::compact(leaf))) {
                            ui.label(RichText::new(p).size(12.0).color(C_LINK())).on_hover_text("Preview of the referenced document");
                        }
                        if let Some(ts) = db::possible_timestamp(leaf) {
                            ui.label(RichText::new(format!("= {ts}?")).size(11.0).color(C_DATE().gamma_multiply(0.7))).on_hover_text("This number looks like a Unix timestamp in seconds");
                        }
                        let r = if editable {
                            let r = r.on_hover_cursor(egui::CursorIcon::Text).on_hover_text(if text.len() > 60 { text.chars().take(600).collect::<String>() } else { "Double-click to edit, right-click for more".into() });
                            if r.double_clicked() && path.first().is_some_and(|k| k != "_id") {
                                acts.push(TreeAct::Start(path.clone(), leaf.clone()));
                            }
                            r
                        } else {
                            r
                        };
                        let p = path.clone();
                        r.union(key_r).context_menu(|ui| {
                            field_menu(ui, &p, leaf, editable, hl, acts);
                            fold = fold_menu(ui, &p, false);
                        });
                    }
                }));
                self.hover_band(ui, band, row.response.rect.y_range());
                if let Some((p, open)) = fold {
                    self.fold = Some((id, p, open));
                }
            }
        }
    }

    /// Paints a full-width band behind a tree row while the pointer is over it.
    fn hover_band(&self, ui: &egui::Ui, at: egui::layers::ShapeIdx, y: egui::Rangef) {
        let r = egui::Rect::from_x_y_ranges(self.tree_x, y.expand(2.0));
        if ui.rect_contains_pointer(r) {
            ui.painter().set(at, egui::epaint::RectShape::filled(r, 4, HOVER()));
        }
    }

    fn run_shell_script(&mut self) {
        let Some(t) = self.t() else { return };
        let code = t.shell.code.clone();
        let ops = dangerous_words(&code);
        // Same check as the original; on a read-only connection the runner refuses writes anyway.
        if !ops.is_empty() && self.sel_writable() {
            self.modal = Some(Modal::ShellWrite(ops));
        } else {
            self.start_shell();
        }
    }

    fn start_shell(&mut self) {
        if self.demo {
            return self.ok("Offline demo: the shell needs a real connection.");
        }
        let Some(runtime) = find_bun(&self.prefs.shell_runtime) else {
            return self.err("Bun was not found. Install it from bun.sh, or set its path in Settings.");
        };
        let Some(t) = self.t() else { return };
        let sel = t.sel.clone();
        let code = t.shell.code.clone();
        let uri = match stored_uri(sel.conn) {
            Ok(u) => u,
            Err(e) => return self.err(uri_err(e)),
        };
        let collections: Vec<String> = self.conn(sel.conn).and_then(|c| c.dbs.iter().find(|d| d.0 == sel.db)).and_then(|d| d.1.clone()).unwrap_or_default();
        let input = serde_json::json!({
            "uri": uri, "db": sel.db, "code": code, "collections": collections,
            "readOnly": !self.sel_writable(), "timeoutMs": self.prefs.timeout_ms,
            "packages": self.prefs.shell_packages.iter().map(|p| db::package_spec(p).0).collect::<Vec<_>>(),
            "proxy": self.shell_proxy(sel.conn),
        })
        .to_string();
        // The original's NodeShell.addHistory (renderer line 196411): skip scripts over 1000
        // characters, move a repeat to the top, keep MAX_HISTORY_LENGTH = 50 per database.
        const MAX_HISTORY_LENGTH: usize = 50;
        const MAX_HISTORY_SCRIPT: usize = 1000;
        if code.chars().count() <= MAX_HISTORY_SCRIPT {
            let h = self.prefs.shell_histories.entry(format!("{}/{}", sel.conn, sel.db)).or_default();
            h.retain(|x| *x != code);
            h.insert(0, code);
            h.truncate(MAX_HISTORY_LENGTH);
        }
        self.save_prefs();
        self.req += 1;
        let req = self.req;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let sh = &mut self.tabs[self.cur].shell;
        *sh = ShellTab { code: std::mem::take(&mut sh.code), req, cancel: Some(tx), started: Some(std::time::Instant::now()), ..Default::default() };
        let packages = self.prefs.shell_packages.clone();
        self.worker.shell(req, db::ShellJob { runtime, dir: config_dir().join("shell"), input, packages }, rx);
    }

    /// The Shell runs in its own process, so it reaches SSH-only servers through this app's
    /// tunnel (a local SOCKS5 port) or through the connection's own proxy.
    fn shell_proxy(&self, conn: u64) -> serde_json::Value {
        let Some(c) = self.conn(conn) else { return serde_json::Value::Null };
        if let Some(t) = &c.tunnel {
            return serde_json::json!({ "host": "127.0.0.1", "port": t.port });
        }
        match &c.meta.proxy {
            Some(p) => serde_json::json!({ "host": p.host, "port": if p.port == 0 { 1080 } else { p.port }, "user": p.user, "password": secret(conn, "proxy-password") }),
            None => serde_json::Value::Null,
        }
    }

    fn shell_view(&mut self, ui: &mut egui::Ui) {
        let writable = self.sel_writable();
        let coll = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
        let history = self.t().and_then(|t| self.prefs.shell_histories.get(&format!("{}/{}", t.sel.conn, t.sel.db)).cloned()).unwrap_or_default();
        let mut packages = self.prefs.shell_packages.clone();
        let mut pkg_names: Vec<String> = packages.iter().map(|p| package_ident(p)).collect();
        if packages.iter().any(|p| db::package_spec(p).0 == "lodash") {
            pkg_names.push("_".into());
        }
        let mut new_pkg = std::mem::take(&mut self.new_pkg);
        let colls: Vec<String> = self.t().and_then(|t| self.conn(t.sel.conn).and_then(|c| c.dbs.iter().find(|d| d.0 == t.sel.db)).and_then(|d| d.1.clone())).unwrap_or_default();
        let (t, ac) = (&mut self.tabs[self.cur], &mut self.ac);
        let paths = t.paths.clone();
        if t.shell.code.is_empty() {
            t.shell.code = default_script(&coll);
        }
        let running = t.shell.started;
        let mut run = false;
        let mut stop = false;
        toolbar(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("SHELL  ·  JavaScript run by Bun with the official MongoDB driver").size(11.0).color(MUTED()));
                ui.label(RichText::new("?").color(PRIMARY())).on_hover_text(
                    "Write a function and return a value; a cursor is cut to 100 documents.\nAvailable: db, every collection by name, ObjectId, ISODate, console.log, randomId, UUIDv4.",
                );
                if !writable {
                    ui.label(RichText::new("read-only: writes are refused").size(11.0).color(SUCCESS()));
                }
            });
            let id = ui.make_persistent_id("shell-editor");
            ac_field(ui, id, &mut t.shell.code, ac, &|b| suggest_js(b, &paths, &colls, &pkg_names), |ui, text, _open| {
                egui::TextEdit::multiline(text).id(id).code_editor().desired_width(f32::INFINITY).desired_rows(10).lock_focus(true).layouter(&mut js_layout).show(ui)
            });
            ui.horizontal(|ui| {
                match running {
                    Some(at) => {
                        spin(ui);
                        ui.label(RichText::new(format!("Running {:.1} s", at.elapsed().as_secs_f32())).color(MUTED()));
                        ui.ctx().request_repaint_after(Duration::from_millis(100));
                        stop = ui.button("Stop").clicked();
                    }
                    None => {
                        run = primary(ui, "Run", PRIMARY()).on_hover_text("Cmd+Enter").clicked() || ui.input(|i| i.modifiers.command && i.key_pressed(Key::Enter));
                        if let Some(ms) = t.shell.ms {
                            let n = t.shell.count.map(|n| format!("{n} results  ")).unwrap_or_default();
                            ui.label(RichText::new(format!("{n}{ms} ms")).color(MUTED()));
                        }
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.menu_button("History", |ui| {
                        ui.set_max_width(520.0);
                        if history.is_empty() {
                            ui.label(RichText::new("Nothing run yet").color(MUTED()));
                        }
                        for h in &history {
                            let first: String = h.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("async function")).take(1).collect();
                            if ui.button(RichText::new(first.chars().take(80).collect::<String>()).monospace()).on_hover_text(h.as_str()).clicked() {
                                t.shell.code = h.clone();
                                ui.close();
                            }
                        }
                    });
                    sticky_menu(ui, format!("Packages ({})", packages.len()), |ui| {
                        ui.set_min_width(300.0);
                        ui.label(RichText::new("npm packages for your scripts. Each one is loaded for you and ready by the name shown.").size(12.0).color(MUTED()));
                        let mut drop = None;
                        for (i, p) in packages.iter().enumerate() {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(p).monospace().color(BRIGHT()));
                                let alias = if db::package_spec(p).0 == "lodash" { format!("{}, _", package_ident(p)) } else { package_ident(p) };
                                ui.label(RichText::new(format!("ready as {alias}")).monospace().size(12.0).color(C_DATE()));
                                if ui.small_button("×").on_hover_text("Remove").clicked() {
                                    drop = Some(i);
                                }
                            });
                        }
                        if let Some(i) = drop {
                            packages.remove(i);
                        }
                        ui.horizontal(|ui| {
                            let r = ui.add(egui::TextEdit::singleline(&mut new_pkg).hint_text("lodash  or  dayjs@1.11").desired_width(190.0).font(FontId::monospace(12.5)));
                            let valid = db::valid_package(new_pkg.trim());
                            if (ui.add_enabled(valid, egui::Button::new("Add")).clicked() || (valid && r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)))) && !packages.contains(&new_pkg.trim().to_string()) {
                                packages.push(new_pkg.trim().to_string());
                                new_pkg.clear();
                            }
                        });
                        ui.label(RichText::new("Installed by Bun on the next run. Bun does not run install scripts of packages unless you trust them.").size(11.0).color(FAINT()));
                    });
                    if ui.selectable_label(t.shell.help, "Help").on_hover_text("What the shell can do, with examples").clicked() {
                        t.shell.help = !t.shell.help;
                    }
                    if ui.button("Reset").on_hover_text("Back to the starter script").clicked() {
                        t.shell.code = default_script(&coll);
                    }
                });
            });
        });
        ui.add_space(6.0);
        let mut use_example = None;
        let has_table = !t.shell.docs.is_empty() && !t.shell.raw;
        let top_h = if has_table { (ui.available_height() * 0.3).max(60.0) } else { f32::INFINITY };
        egui::ScrollArea::vertical().id_salt("shell-out").max_height(top_h).auto_shrink([false, true]).show(ui, |ui| {
            if t.shell.help {
                use_example = shell_help(ui, &collection_ref(&coll), writable);
                ui.add_space(6.0);
            }
            if !t.shell.logs.is_empty() {
                toolbar(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new("LOG").size(11.0).color(MUTED()));
                    for (level, text) in &t.shell.logs {
                        let c = match level.as_str() {
                            "error" => ALERT(),
                            "warn" => WARNING(),
                            "info" => PRIMARY(),
                            _ => TEXT(),
                        };
                        ui.label(RichText::new(text).monospace().size(12.5).color(c));
                    }
                });
                ui.add_space(6.0);
            }
            if let Some(e) = &t.shell.error {
                egui::Frame::new().fill(tint(ALERT(), 20)).corner_radius(8).inner_margin(10).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(e).monospace().color(ALERT()));
                });
            }
        });
        let mut export = false;
        let mut page_to = None;
        if let Some(v) = &t.shell.value {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("RESULT").size(11.0).color(MUTED()));
                let what = match (t.shell.docs.len(), t.shell.count) {
                    (0, Some(n)) => format!("{n} items"),
                    (0, None) => db::type_name(v).to_string(),
                    (n, _) => format!("{} documents", group(&n.to_string())),
                };
                ui.label(RichText::new(what).color(BRIGHT()));
                let pages = t.shell.docs.len().div_ceil(PAGE_SIZE as usize);
                if has_table && pages > 1 {
                    ui.separator();
                    if ui.add_enabled(t.shell.page > 0, egui::Button::new("‹")).clicked() {
                        page_to = Some(t.shell.page - 1);
                    }
                    ui.label(format!("page {} of {pages}", t.shell.page + 1));
                    if ui.add_enabled(t.shell.page + 1 < pages, egui::Button::new("›")).clicked() {
                        page_to = Some(t.shell.page + 1);
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    export = ui.button("Export…").clicked();
                    if ui.button("Copy").on_hover_text("Copy the whole result as JSON").clicked() {
                        ui.ctx().copy_text(db::to_edit_value(v));
                    }
                    if !t.shell.docs.is_empty() {
                        ui.selectable_value(&mut t.shell.raw, true, "JSON");
                        ui.selectable_value(&mut t.shell.raw, false, "Table");
                    }
                });
            });
            ui.add_space(4.0);
            if !has_table {
                // Very large results are cut for display; Export writes everything.
                const RAW_MAX: usize = 300_000;
                let text = t.shell.raw_text.get_or_insert_with(|| {
                    let mut s = db::to_edit_value(v);
                    if s.len() > RAW_MAX {
                        let cut = (0..=RAW_MAX).rev().find(|i| s.is_char_boundary(*i)).unwrap_or(0);
                        s.truncate(cut);
                        s.push_str("\n… cut for display. Use Export to save the whole result.");
                    }
                    s
                });
                toolbar(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("shell-raw").auto_shrink(false).show(ui, |ui| {
                        ui.add(egui::TextEdit::multiline(&mut text.as_str()).code_editor().desired_width(f32::INFINITY).layouter(&mut code_layout));
                    });
                });
            }
        }
        if let Some(p) = page_to {
            t.shell.page = p;
            t.selected = None;
        }
        if export {
            self.export_shell();
        }
        if has_table {
            // The result table is the same one documents use: open, colours, right-click menu.
            let t = &mut self.tabs[self.cur];
            let start = t.shell.page * PAGE_SIZE as usize;
            let slice = t.shell.docs[start.min(t.shell.docs.len())..(start + PAGE_SIZE as usize).min(t.shell.docs.len())].to_vec();
            let saved_docs = std::mem::replace(&mut t.agg_docs, slice);
            let saved_fields = std::mem::replace(&mut t.agg_fields, t.shell.fields.clone());
            self.results(ui, true);
            let t = &mut self.tabs[self.cur];
            t.agg_docs = saved_docs;
            t.agg_fields = saved_fields;
        }
        if let Some(code) = use_example {
            self.tabs[self.cur].shell.code = code;
        }
        self.new_pkg = new_pkg;
        if packages != self.prefs.shell_packages {
            self.prefs.shell_packages = packages;
            self.save_prefs();
        }
        if stop {
            if let Some(c) = self.tabs[self.cur].shell.cancel.take() {
                let _ = c.send(());
            }
        }
        if run {
            self.run_shell_script();
        }
    }

    fn save_to_library(&mut self, q: SavedQuery) {
        let key = self.col_key(false);
        let list = self.prefs.library.entry(key).or_default();
        let name = q.name.clone();
        list.retain(|x| !(x.name == q.name && x.pipeline.is_some() == q.pipeline.is_some()));
        list.push(q);
        list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        self.save_prefs();
        self.ok(format!("Saved \"{name}\" to the library"));
    }

    fn delete_from_library(&mut self, name: &str, pipeline: bool) {
        let key = self.col_key(false);
        if let Some(list) = self.prefs.library.get_mut(&key) {
            list.retain(|x| !(x.name == name && x.pipeline.is_some() == pipeline));
        }
        self.save_prefs();
    }

    /// Saves the whole Shell result: JSON for anything, CSV or TSV for document lists.
    fn export_shell(&mut self) {
        let t = &self.tabs[self.cur];
        let Some(v) = t.shell.value.clone() else { return };
        let docs = t.shell.docs.clone();
        let mut dialog = rfd::FileDialog::new().set_file_name(format!("{}-shell.json", t.sel.coll)).add_filter("JSON", &["json"]);
        if !docs.is_empty() {
            dialog = dialog.add_filter("CSV", &["csv"]).add_filter("TSV", &["tsv"]);
        }
        let Some(path) = dialog.save_file() else { return };
        let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("json").to_ascii_lowercase();
        let body = match ext.as_str() {
            "csv" if !docs.is_empty() => db::to_table(&docs, ','),
            "tsv" if !docs.is_empty() => db::to_table(&docs, '\t'),
            _ => serde_json::to_string_pretty(&v.into_relaxed_extjson()).unwrap_or_default(),
        };
        match std::fs::write(&path, body) {
            Ok(()) => self.ok(format!("Saved the shell result to {}", path.display())),
            Err(e) => self.err(format!("Could not save: {e}")),
        }
    }

    fn export(&mut self) {
        let Some(target) = self.target() else { return self.err("Not connected.") };
        let file = rfd::FileDialog::new()
            .set_file_name(format!("{}.json", target.coll))
            .add_filter("JSON", &["json"])
            .add_filter("CSV", &["csv"])
            .add_filter("TSV", &["tsv"])
            .save_file();
        if let Some(path) = file {
            self.ok(format!("Exporting to {}…", path.display()));
            let q = self.t().unwrap().query.clone();
            self.worker.export(target, q, path);
        }
    }

    fn aggregate(&mut self, ui: &mut egui::Ui) {
        let mut run = false;
        let mut explain = false;
        let key = self.col_key(false);
        let agg_lib: Vec<SavedQuery> = self.prefs.library.get(&key).map(|v| v.iter().filter(|q| q.pipeline.is_some()).cloned().collect()).unwrap_or_default();
        let mut agg_acts = Vec::new();
        let mut code_lang = None;
        let mut agg_new = std::mem::take(&mut self.new_lib);
        let mut stage = None;
        let mut sacts: Vec<StageAct> = Vec::new();
        self.sync_stages();
        let auto = self.prefs.auto_preview;
        let mut auto_now = auto;
        let t = self.tm().unwrap();
        let builder = !t.agg_text_mode;
        let labels: Vec<String> = if builder || db::is_shell(&t.agg_txt) {
            Vec::new()
        } else {
            db::parse_pipeline(&t.agg_txt).map(|p| p.iter().map(|s| s.keys().next().cloned().unwrap_or_default()).collect()).unwrap_or_default()
        };
        // Why the text cannot be shown as stages, if it cannot.
        let unsynced = (builder && t.stages_src != t.agg_txt).then(|| if db::is_shell(&t.agg_txt) { "this is a shell command".to_string() } else { stages_from_text(&t.agg_txt).err().unwrap_or_default() });
        let has_results = t.agg_ms.is_some() || t.agg_req != 0;
        let avail = ui.available_height();
        toolbar(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(if builder { "PIPELINE  ·  each stage shows what comes out of it" } else { "PIPELINE  ·  an array of stages, or paste db.coll.aggregate([...])" }).size(11.0).color(MUTED()));
                ui.add_space(8.0);
                segmented(ui, &mut t.agg_text_mode, &[(false, "Stages"), (true, "Text")]);
                if builder {
                    ui.checkbox(&mut auto_now, "Auto preview").on_hover_text("Preview each stage a second after you stop typing. Every preview runs two read-only aggregations on the server.");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    library_menu(ui, &agg_lib, &mut agg_new, true, &mut agg_acts);
                });
            });
            if builder {
                match &unsynced {
                    Some(e) => {
                        ui.label(RichText::new(format!("Cannot show this pipeline as stages: {e}. Fix or run it in Text.")).color(WARNING()));
                    }
                    None => {
                        let max_h = if has_results { (avail * 0.55).max(260.0) } else { (avail - 70.0).max(260.0) };
                        egui::ScrollArea::vertical().id_salt("stages").max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                            egui::Frame::new().fill(BG()).corner_radius(8).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                let n = t.agg_input.count.map_or(String::new(), |n| format!("  ·  about {} documents", group(&n.to_string())));
                                // Closed at first, like the original's collection sample (isOpen: false, renderer line 111386).
                                egui::CollapsingHeader::new(RichText::new(format!("Collection input{n}")).color(MUTED())).id_salt("agg-input").default_open(false).show(ui, |ui| {
                                    stage_samples(ui, &mut t.agg_input, usize::MAX, &mut sacts);
                                });
                            });
                            let n = t.stages.len();
                            for (i, s) in t.stages.iter_mut().enumerate() {
                                ui.add_space(6.0);
                                stage_card(ui, i, n, s, auto, &mut sacts);
                            }
                            ui.add_space(6.0);
                            ui.menu_button(RichText::new("+ Add stage").strong().color(PRIMARY()), |ui| {
                                egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                                    for (k, (op, _)) in db::STAGES.iter().enumerate() {
                                        // The first ten are the original's most used stages (topStages).
                                        if k == 10 {
                                            ui.separator();
                                        }
                                        if ui.button(RichText::new(*op).monospace()).clicked() {
                                            sacts.push(StageAct::Add(op.to_string()));
                                            ui.close();
                                        }
                                    }
                                });
                            });
                        });
                    }
                }
            } else {
                let id = ui.make_persistent_id("agg-editor");
                with_autopair(ui, id, &mut t.agg_txt, |ui, text| egui::TextEdit::multiline(text).id(id).code_editor().desired_width(f32::INFINITY).desired_rows(8).layouter(&mut code_layout).show(ui));
            }
            ui.horizontal(|ui| {
                run = primary(ui, "Run", PRIMARY()).on_hover_text("Run the whole pipeline (Cmd+Enter)").clicked() || ui.input(|i| i.modifiers.command && i.key_pressed(Key::Enter));
                explain = ui.button("Explain").on_hover_text("Cmd+E").clicked();
                code_menu(ui, &mut code_lang);
                if t.agg_req != 0 {
                    spin(ui);
                    ui.label(RichText::new("Running…").color(MUTED()));
                } else if let Some(ms) = t.agg_ms {
                    let from = t.agg_stage.map_or(String::new(), |i| format!("Output after stage {}  ", i + 1));
                    ui.label(RichText::new(format!("{from}{} results  {} ms  (first 1000 shown)", t.agg_docs.len(), ms)).color(MUTED()));
                }
            });
            if !labels.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for (i, l) in labels.iter().enumerate() {
                        let fg = if t.agg_stage == Some(i) { PRIMARY() } else { MUTED() };
                        if pill(ui, &format!("{} {l}", i + 1), fg).on_hover_text("Preview the output after this stage").clicked() {
                            stage = Some(i);
                        }
                    }
                });
            }
        });
        if auto_now != auto {
            self.prefs.auto_preview = auto_now;
            self.save_prefs();
            if auto_now {
                self.stages_changed(0, false);
            }
        }
        self.apply_stage_acts(sacts);
        if builder && unsynced.is_none() {
            self.pump_previews(ui.ctx());
        }
        self.new_lib = agg_new;
        if let Some(l) = code_lang {
            self.show_code(l, false);
        }
        for a in agg_acts {
            match a {
                QAct::Load(q) => {
                    self.tabs[self.cur].agg_txt = q.pipeline.unwrap_or_default();
                    run = true;
                }
                QAct::Save(name) => {
                    let pipeline = Some(self.tabs[self.cur].agg_txt.clone());
                    self.save_to_library(SavedQuery { name, pipeline, ..Default::default() });
                }
                QAct::Delete(name) => self.delete_from_library(&name, true),
                _ => {}
            }
        }
        let t = self.tm().unwrap();
        if let (Some(i), false) = (stage, run) {
            if let Some(p) = db::parse_pipeline(&t.agg_txt).ok().and_then(|p| db::stage_preview(&p, i)) {
                self.run_pipeline(p);
                self.tm().unwrap().agg_stage = Some(i);
            }
        }
        let t = self.tm().unwrap();
        if run {
            let text = t.agg_txt.clone();
            if db::is_shell(&text) {
                self.run_shell(&text);
            } else {
                match db::parse_pipeline(&text) {
                    Err(e) => self.err(e),
                    Ok(p) => self.run_pipeline(p),
                }
            }
        }
        if explain {
            self.explain();
        }
        ui.add_space(6.0);
        if !builder || has_results {
            self.results(ui, true);
        }
    }

    /// Rebuilds the builder's stages when the pipeline text changed elsewhere (Text mode, the
    /// library, a pasted query).
    fn sync_stages(&mut self) {
        let Some(t) = self.tm() else { return };
        if t.agg_text_mode || t.stages_src == t.agg_txt || db::is_shell(&t.agg_txt) {
            return;
        }
        if let Ok(st) = stages_from_text(&t.agg_txt) {
            t.stages = st;
            t.stages_src = t.agg_txt.clone();
        }
    }

    /// Stages from `from` on need a new preview (a second later while typing), and the pipeline
    /// text follows the stages.
    fn stages_changed(&mut self, from: usize, typing: bool) {
        let auto = self.prefs.auto_preview;
        let t = self.tm().unwrap();
        let at = std::time::Instant::now() + std::time::Duration::from_millis(if typing { 1000 } else { 0 });
        for s in t.stages.iter_mut().skip(from) {
            s.due = auto.then_some(at);
        }
        t.agg_txt = text_from_stages(&t.stages);
        t.stages_src = t.agg_txt.clone();
    }

    fn apply_stage_acts(&mut self, acts: Vec<StageAct>) {
        for a in acts {
            let t = self.tm().unwrap();
            match a {
                StageAct::Edited(i) => self.stages_changed(i, true),
                StageAct::Toggled(i) => self.stages_changed(i, false),
                StageAct::Move(i, down) => {
                    let j = if down { i + 1 } else { i.wrapping_sub(1) };
                    if j < t.stages.len() {
                        t.stages.swap(i, j);
                        self.stages_changed(i.min(j), false);
                    }
                }
                StageAct::Remove(i) => {
                    if i < t.stages.len() {
                        t.stages.remove(i);
                        self.stages_changed(i, false);
                    }
                }
                StageAct::Add(op) => {
                    let body = db::STAGES.iter().find(|(o, _)| *o == op).map_or("{}", |(_, b)| b);
                    t.stages.push(Stage::new(&op, body, true));
                    let i = t.stages.len() - 1;
                    self.stages_changed(i, false);
                }
                StageAct::Op(i, op) => {
                    let s = &mut t.stages[i];
                    // A body still equal to the old stage's example becomes the new stage's example.
                    let template = |o: &str| db::STAGES.iter().find(|(x, _)| *x == o).map(|(_, b)| *b);
                    if template(&s.op) == Some(s.text.as_str()) || s.text.trim().is_empty() {
                        s.text = template(&op).unwrap_or("{}").to_string();
                    }
                    s.op = op;
                    self.stages_changed(i, false);
                }
                StageAct::More(i) => {
                    let s = if i == usize::MAX { &mut t.agg_input } else { &mut t.stages[i] };
                    s.limit += db::STAGE_SAMPLE;
                    s.due = Some(std::time::Instant::now());
                }
                StageAct::Preview(i) => t.stages[i].due = Some(std::time::Instant::now()),
            }
        }
    }

    /// Runs the next stage preview that is due, one at a time and in order, like the original's
    /// getResultsForStages (renderer line 112079), so a pipeline never floods the server.
    fn pump_previews(&mut self, ctx: &egui::Context) {
        if self.demo {
            return;
        }
        let Some(target) = self.target() else { return };
        let now = std::time::Instant::now();
        let t = &mut self.tabs[self.cur];
        if t.agg_input.req == 0 && t.agg_input.due.is_some_and(|d| d <= now) {
            t.agg_input.due = None;
            self.req += 1;
            t.agg_input.req = self.req;
            self.worker.stage_preview(self.req, target.clone(), Vec::new(), t.agg_input.limit);
        }
        if t.stages.iter().any(|s| s.req != 0) {
            return;
        }
        if let Some(next) = t.stages.iter().filter_map(|s| s.due).min() {
            if next > now {
                ctx.request_repaint_after(next - now);
            }
        }
        let Some(k) = t.stages.iter().position(|s| s.due.is_some_and(|d| d <= now)) else { return };
        t.stages[k].due = None;
        if !t.stages[k].on {
            return;
        }
        match stage_pipeline(&t.stages, k) {
            Err(e) => (t.stages[k].error, t.stages[k].count, t.stages[k].sample) = (Some(e), None, Vec::new()),
            // The original never previews past $out or $merge (lastStages, renderer line 112045).
            Ok(p) if db::has_write_stage(&p) => {
                (t.stages[k].error, t.stages[k].count, t.stages[k].sample) = (Some("This stage writes, so it only runs with the whole pipeline (Run).".into()), None, Vec::new());
            }
            Ok(p) => {
                self.req += 1;
                t.stages[k].req = self.req;
                self.worker.stage_preview(self.req, target, p, t.stages[k].limit);
            }
        }
    }

    fn schema_view(&mut self, ui: &mut egui::Ui) {
        let mut analyze = false;
        let mut acts: Vec<(String, u8)> = Vec::new();
        let t = &mut self.tabs[self.cur];
        // Analyze once when the view opens; the limit keeps it light on big collections.
        if t.schema.stats.is_empty() && !t.schema.running && t.schema.ms.is_none() && t.schema.err.is_none() {
            analyze = true;
        }
        toolbar(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("SCHEMA  ·  fields of the documents matching the current query").size(11.0).color(MUTED()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    analyze |= primary(ui, "Analyze", PRIMARY()).clicked();
                    egui::ComboBox::from_id_salt("schema-limit")
                        .selected_text(if t.schema.limit == 0 { "all documents".to_string() } else { format!("first {}", group(&t.schema.limit.to_string())) })
                        .show_ui(ui, |ui| {
                            for n in [100, 1000, 10_000, 100_000, 0] {
                                ui.selectable_value(&mut t.schema.limit, n, if n == 0 { "all documents".to_string() } else { format!("first {}", group(&n.to_string())) });
                            }
                        });
                    if t.schema.running {
                        spin(ui);
                    }
                });
            });
            let filter = if t.query.filter.is_empty() { "{} (all documents)".to_string() } else { db::shell_text(&Bson::Document(t.query.filter.clone())) };
            ui.label(RichText::new(format!("Query: {filter}")).monospace().size(12.0).color(FAINT()));
        });
        ui.add_space(6.0);
        let (mut apply, mut generate, mut reload) = (false, false, false);
        // QUARRY_TEST_VALIDATOR opens it in the demo picture.
        egui::CollapsingHeader::new(RichText::new("VALIDATOR").size(11.0).color(MUTED())).id_salt("schema-validator").default_open(std::env::var("QUARRY_TEST_VALIDATOR").is_ok()).show(ui, |ui| {
            let s = &mut t.schema;
            ui.horizontal(|ui| {
                ui.label(RichText::new("Level").color(MUTED()));
                segmented(ui, &mut s.vlevel, &[(0, "off"), (1, "moderate"), (2, "strict")]);
                ui.add_space(12.0);
                ui.label(RichText::new("Action").color(MUTED()));
                segmented(ui, &mut s.vwarn, &[(false, "error"), (true, "warn")]);
            });
            let id = ui.make_persistent_id("validator-editor");
            // A generated validator can run to hundreds of lines; scroll inside a capped box so the
            // Apply buttons and the field list stay in the window.
            let max_h = (ui.ctx().content_rect().height() * 0.4).clamp(160.0, 420.0);
            egui::ScrollArea::vertical().id_salt("validator-scroll").max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                with_autopair(ui, id, &mut s.vtext, |ui, text| egui::TextEdit::multiline(text).id(id).code_editor().hint_text("{ $jsonSchema: { bsonType: \"object\", required: [...] } }").desired_width(f32::INFINITY).desired_rows(8).layouter(&mut code_layout).show(ui));
            });
            ui.horizontal(|ui| {
                apply = primary(ui, "Apply", PRIMARY()).on_hover_text("Runs collMod on this collection").clicked();
                generate = ui.add_enabled(!s.stats.is_empty(), egui::Button::new("Generate from analysis")).on_hover_text("Built from the sampled documents; review it before applying").clicked();
                reload = ui.button("Reload").clicked();
            });
        });
        ui.add_space(6.0);
        if let Some(e) = &t.schema.err {
            ui.label(RichText::new(format!("Analysis failed: {e}")).color(ALERT()));
        } else if let Some(ms) = t.schema.ms {
            ui.label(RichText::new(format!("{} fields in {} documents  ·  {ms} ms", t.schema.stats.len(), group(&t.schema.docs.to_string()))).color(MUTED()));
        }
        let n = t.schema.docs.max(1) as f32;
        egui::ScrollArea::vertical().id_salt("schema").auto_shrink(false).show(ui, |ui| {
            egui::Grid::new("schema-grid").num_columns(4).striped(true).spacing([18.0, 6.0]).min_col_width(60.0).show(ui, |ui| {
                for h in ["FIELD", "PRESENT IN", "TYPES", "EXAMPLES"] {
                    ui.label(RichText::new(h).size(11.0).color(MUTED()));
                }
                ui.end_row();
                for f in &t.schema.stats {
                    let name = f.path.rsplit('.').next().unwrap_or(&f.path);
                    let r = ui.horizontal(|ui| {
                        ui.add_space(f.depth as f32 * 16.0);
                        ui.label(RichText::new(name).monospace().color(BRIGHT())).on_hover_text(&f.path)
                    });
                    r.response.context_menu(|ui| {
                        ui.label(RichText::new(format!("Field: {}", f.path)).size(12.0).color(MUTED()));
                        if ui.button("Add as column").clicked() {
                            acts.push((f.path.clone(), 0));
                        }
                        if ui.button("Filter: has this field").clicked() {
                            acts.push((f.path.clone(), 1));
                        }
                        if ui.button("Filter: missing this field").clicked() {
                            acts.push((f.path.clone(), 2));
                        }
                    });
                    let share = f.docs as f32 / n;
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(90.0, 8.0), Sense::hover());
                        ui.painter().rect_filled(rect, 4, HOVER());
                        let mut fill = rect;
                        fill.set_width(rect.width() * share);
                        ui.painter().rect_filled(fill, 4, if share >= 0.999 { SUCCESS() } else { PRIMARY() });
                        ui.label(RichText::new(format!("{:.0}%", share * 100.0)).size(12.0).color(TEXT()));
                    });
                    ui.horizontal(|ui| {
                        let total: usize = f.types.iter().map(|t| t.1).sum::<usize>().max(1);
                        for (tn, c) in &f.types {
                            let col = color_of(&match *tn {
                                "string" => Bson::String(String::new()),
                                "int" | "long" | "double" | "decimal" => Bson::Int32(0),
                                "bool" => Bson::Boolean(true),
                                "date" | "timestamp" => Bson::DateTime(mongodb::bson::DateTime::from_millis(0)),
                                "null" => Bson::Null,
                                "objectId" => Bson::ObjectId(ObjectId::new()),
                                _ => Bson::Document(Document::new()),
                            });
                            let pct = *c as f32 * 100.0 / total as f32;
                            let label = if f.types.len() > 1 { format!("{tn} {pct:.0}%") } else { tn.to_string() };
                            ui.label(RichText::new(label).monospace().size(12.0).color(col));
                        }
                    });
                    ui.label(RichText::new(f.examples.join("   ")).monospace().size(12.0).color(FAINT()));
                    ui.end_row();
                }
            });
        });
        if generate {
            let s = &mut self.tabs[self.cur].schema;
            s.vtext = db::to_edit_json(&db::schema_from_stats(&s.stats, s.docs));
        }
        if reload {
            self.tabs[self.cur].schema.vloaded = false;
        }
        if apply {
            let t = &self.tabs[self.cur];
            match db::parse_doc(&t.schema.vtext) {
                Ok(v) => {
                    let level = ["off", "moderate", "strict"][t.schema.vlevel as usize];
                    let cmd = doc! { "collMod": &t.sel.coll, "validator": v, "validationLevel": level, "validationAction": if t.schema.vwarn { "warn" } else { "error" } };
                    self.modal = Some(Modal::Typed { title: format!("Change the validator of {}? Writes that fail it may be rejected.", t.sel.coll), expect: t.sel.coll.clone(), typed: String::new(), action: Danger::Validator(cmd) });
                }
                Err(e) => self.err(e),
            }
        }
        if !self.tabs[self.cur].schema.vloaded {
            if let Some(target) = self.target() {
                self.req += 1;
                let (req, coll) = (self.req, target.coll.clone());
                let s = &mut self.tabs[self.cur].schema;
                (s.vloaded, s.vreq) = (true, req);
                self.worker.command(target.client, target.db, doc! { "listCollections": 1, "filter": { "name": coll } }, &format!("validator:{req}"));
            }
        }
        for (path, what) in acts {
            match what {
                0 => {
                    let key = self.col_key(false);
                    let fields = self.t().map(|t| t.fields.clone()).unwrap_or_default();
                    self.apply_col_acts(&key, &fields, vec![ColAct::Show(path)]);
                    self.ok("Column added; open Documents to see it");
                }
                _ => {
                    let t = &mut self.tabs[self.cur];
                    t.filter_txt = db::shell_text(&Bson::Document(doc! { path: { "$exists": what == 1 } }));
                    t.view = View::Documents;
                    self.run_query();
                }
            }
        }
        if analyze {
            if let Some(target) = self.target() {
                self.req += 1;
                let req = self.req;
                let t = &mut self.tabs[self.cur];
                t.schema.req = req;
                t.schema.running = true;
                t.schema.err = None;
                self.worker.schema(req, target, t.query.filter.clone(), t.schema.limit);
            } else if self.demo {
                let t = &mut self.tabs[self.cur];
                t.schema.stats = db::analyze_schema(&t.docs);
                (t.schema.docs, t.schema.ms) = (t.docs.len(), Some(3));
            }
        }
    }

    /// Compares this tab's collection with another one, by `_id`, and copies differences over.
    fn compare_view(&mut self, ui: &mut egui::Ui) {
        let i = self.cur;
        let src = self.tabs[i].sel.clone();
        if self.tabs[i].compare.conn == 0 {
            let c = &mut self.tabs[i].compare;
            (c.conn, c.db) = (src.conn, src.db.clone());
        }
        let conns: Vec<(u64, String)> = self.conns.iter().filter(|c| c.client.is_some() || self.demo).map(|c| (c.meta.id, c.meta.name.clone())).collect();
        let dbs = self.conn(self.tabs[i].compare.conn).map(|c| c.dbs.clone()).unwrap_or_default();
        let mut load_colls = None;
        let mut run = false;
        let mut sync = false;
        let c = &mut self.tabs[i].compare;
        toolbar(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("COMPARE  ·  {}.{} is the source, documents are matched by _id and field order is ignored", src.db, src.coll)).size(11.0).color(MUTED()));
            ui.horizontal(|ui| {
            ui.label("Target");
            let name = conns.iter().find(|x| x.0 == c.conn).map(|x| x.1.clone()).unwrap_or_default();
            egui::ComboBox::from_id_salt("cmp-conn").selected_text(name).show_ui(ui, |ui| {
                for (id, n) in &conns {
                    if ui.selectable_label(c.conn == *id, n).clicked() && c.conn != *id {
                        (c.conn, c.db, c.coll) = (*id, String::new(), String::new());
                    }
                }
            });
            egui::ComboBox::from_id_salt("cmp-db").selected_text(if c.db.is_empty() { "database" } else { &c.db }).show_ui(ui, |ui| {
                for (d, colls) in &dbs {
                    if ui.selectable_label(c.db == *d, d).clicked() {
                        c.db = d.clone();
                        c.coll.clear();
                        if colls.is_none() {
                            load_colls = Some(d.clone());
                        }
                    }
                }
            });
            let colls = dbs.iter().find(|d| d.0 == c.db).and_then(|d| d.1.clone()).unwrap_or_default();
            egui::ComboBox::from_id_salt("cmp-coll").selected_text(if c.coll.is_empty() { "collection" } else { &c.coll }).show_ui(ui, |ui| {
                for n in &colls {
                    ui.selectable_value(&mut c.coll, n.clone(), n);
                }
            });
            let same = c.conn == src.conn && c.db == src.db && c.coll == src.coll;
            let ok = !c.coll.is_empty() && !same && !c.running;
            run = ui.add_enabled(ok, primary_button(ui, "Compare", PRIMARY())).on_disabled_hover_text(if same { "Pick a different collection" } else { "Pick a target collection" }).clicked();
            if c.running {
                spin(ui);
            }
            });
        });
        if let Some(r) = &c.result {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                for (label, n, color) in [("Identical", r.same, MUTED()), ("Only in source", r.only_src.len(), SUCCESS()), ("Only in target", r.only_dst.len(), WARNING()), ("Different", r.changed.len(), C_LINK())] {
                    egui::Frame::new().fill(RAISED()).corner_radius(6).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
                        ui.label(RichText::new(group(&n.to_string())).strong().size(16.0).color(color));
                        ui.label(RichText::new(label).size(12.0).color(MUTED()));
                    });
                }
            });
            ui.add_space(6.0);
            egui::ScrollArea::vertical().max_height((ui.available_height() - 110.0).max(120.0)).auto_shrink([false, true]).show(ui, |ui| {
                const SHOWN: usize = 200;
                let more = |ui: &mut egui::Ui, n: usize| {
                    if n > SHOWN {
                        ui.label(RichText::new(format!("and {} more", group(&(n - SHOWN).to_string()))).color(FAINT()));
                    }
                };
                egui::CollapsingHeader::new(format!("Different ({})", r.changed.len())).id_salt("cmp-changed").default_open(true).show(ui, |ui| {
                    for (a, b) in r.changed.iter().take(SHOWN) {
                        let d = db::diff(b, a);
                        egui::CollapsingHeader::new(RichText::new(format!("{}   {} field{} differ{}", db::short(a.get("_id").unwrap_or(&Bson::Null)), d.len(), if d.len() == 1 { "" } else { "s" }, if d.len() == 1 { "s" } else { "" })).monospace()).id_salt(("cmp-d", db::compact(a.get("_id").unwrap_or(&Bson::Null)))).show(ui, |ui| {
                            egui::Grid::new(("cmp-g", db::compact(a.get("_id").unwrap_or(&Bson::Null)))).striped(true).show(ui, |ui| {
                                ui.label(RichText::new("Field").color(FAINT()));
                                ui.label(RichText::new("Target now").color(FAINT()));
                                ui.label(RichText::new("Source").color(FAINT()));
                                ui.end_row();
                                for (path, old, new) in d {
                                    let t = |v: &Option<Bson>| v.as_ref().map_or("missing".to_string(), cell_text);
                                    ui.label(RichText::new(path).monospace().color(KEY()));
                                    ui.label(RichText::new(t(&old)).monospace().color(ALERT()));
                                    ui.label(RichText::new(t(&new)).monospace().color(SUCCESS()));
                                    ui.end_row();
                                }
                            });
                        });
                    }
                    more(ui, r.changed.len());
                });
                for (title, list) in [("Only in source", &r.only_src), ("Only in target", &r.only_dst)] {
                    egui::CollapsingHeader::new(format!("{title} ({})", list.len())).id_salt(title).show(ui, |ui| {
                        for d in list.iter().take(SHOWN) {
                            let mut t = db::compact(&Bson::Document(d.clone()));
                            if t.len() > 160 {
                                t = format!("{}…", &t[..t.floor_char_boundary(160)]);
                            }
                            ui.add(egui::Label::new(RichText::new(t).monospace().size(12.0).color(TEXT())).truncate());
                        }
                        more(ui, list.len());
                    });
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.checkbox(&mut c.insert, format!("Copy the {} only in source", r.only_src.len()));
                ui.checkbox(&mut c.replace, format!("Overwrite the {} different", r.changed.len()));
                let col = if c.delete { ALERT() } else { TEXT() };
                ui.checkbox(&mut c.delete, RichText::new(format!("Delete the {} only in target", r.only_dst.len())).color(col));
                let any = (c.insert && !r.only_src.is_empty()) || (c.replace && !r.changed.is_empty()) || (c.delete && !r.only_dst.is_empty());
                sync = ui.add_enabled(any, primary_button(ui, "Sync to target…", WARNING())).clicked();
            });
        } else if !c.running {
            ui.add_space(20.0);
            ui.label(RichText::new("Pick a target and press Compare. Nothing is written until you sync and type the target's name.").color(FAINT()));
        }
        let (tc, tdb, tcoll) = (c.conn, c.db.clone(), c.coll.clone());
        if let Some(d) = load_colls {
            if let Some(client) = self.conn(tc).and_then(|c| c.client.clone()) {
                self.worker.collections(tc, client, d);
            }
        }
        if run {
            let (Some(a), Some(b)) = (self.conn(src.conn).and_then(|c| c.client.clone()), self.conn(tc).and_then(|c| c.client.clone())) else {
                return self.err(if self.demo { "Offline demo: nothing to compare." } else { "Connect both sides first." });
            };
            self.req += 1;
            let req = self.req;
            let to = self.timeout();
            let c = &mut self.tabs[i].compare;
            (c.req, c.running, c.result) = (req, true, None);
            self.worker.compare(req, Target { client: a, db: src.db, coll: src.coll, timeout: to }, Target { client: b, db: tdb, coll: tcoll, timeout: to });
        } else if sync {
            if !self.conn(tc).is_some_and(|c| c.writable) {
                return self.modal = Some(Modal::Unlock(tc, None));
            }
            let c = &self.tabs[i].compare;
            let r = c.result.as_ref().unwrap();
            let n = |on: bool, k: usize| if on { k } else { 0 };
            let title = format!(
                "Write to {tdb}.{tcoll}: copy {}, overwrite {} and delete {} documents? Overwrites skip documents changed since the comparison. This has no Undo.",
                n(c.insert, r.only_src.len()),
                n(c.replace, r.changed.len()),
                n(c.delete, r.only_dst.len())
            );
            self.modal = Some(Modal::Typed { title, expect: tcoll, typed: String::new(), action: Danger::Sync { conn: tc } });
        }
    }

    fn import_view(&mut self, ui: &mut egui::Ui) {
        let mut open_file = false;
        let mut start = false;
        let writable = self.sel_writable();
        let t = &mut self.tabs[self.cur];
        let im = &mut t.import;
        toolbar(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("IMPORT").size(11.0).color(MUTED()));
                segmented(ui, &mut im.kind, &[(db::ImportKind::Json, "JSON"), (db::ImportKind::JsonLines, "JSON Lines"), (db::ImportKind::Csv, "CSV")]);
                if im.kind == db::ImportKind::Csv {
                    ui.add_space(8.0);
                    ui.checkbox(&mut im.header, "First row is the header");
                    ui.checkbox(&mut im.camel, "camelCase names");
                    egui::ComboBox::from_id_salt("csv-sep")
                        .selected_text(match im.separator {
                            None => "separator: auto",
                            Some('\t') => "separator: tab",
                            Some(',') => "separator: ,",
                            Some(';') => "separator: ;",
                            _ => "separator: |",
                        })
                        .show_ui(ui, |ui| {
                            for (v, l) in [(None, "auto"), (Some(','), ","), (Some(';'), ";"), (Some('\t'), "tab"), (Some('|'), "|")] {
                                ui.selectable_value(&mut im.separator, v, l);
                            }
                        });
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    open_file = ui.button("Open file…").clicked();
                    if let Some(f) = &im.file {
                        ui.label(RichText::new(f).size(12.0).color(MUTED()));
                    }
                });
            });
            let id = ui.make_persistent_id("import-text");
            // Very large files are not shown in full; the preview below shows what will be imported.
            const IMPORT_SHOW_MAX: usize = 200_000;
            if im.text.len() <= IMPORT_SHOW_MAX {
                egui::ScrollArea::vertical().id_salt("import-src").max_height(180.0).show(ui, |ui| {
                    with_autopair(ui, id, &mut im.text, |ui, text| {
                        egui::TextEdit::multiline(text).id(id).code_editor().hint_text("Paste JSON, JSON Lines or CSV here, or open a file").desired_width(f32::INFINITY).desired_rows(6).layouter(&mut code_layout).show(ui)
                    });
                });
            } else {
                ui.label(RichText::new(format!("{} loaded from the file (too large to show).", human_bytes(im.text.len()))).color(MUTED()));
            }
            ui.horizontal(|ui| {
                ui.radio_value(&mut im.upsert, false, "Insert as new documents");
                ui.radio_value(&mut im.upsert, true, "Upsert by");
                ui.add_enabled(im.upsert, egui::TextEdit::singleline(&mut im.upsert_by).desired_width(110.0).font(FontId::monospace(12.5)));
                ui.label(RichText::new("(update the match, add if none)").size(12.0).color(FAINT()));
            });
        });
        // Parse again only when the input or options changed.
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (&im.text, im.kind as u8, im.separator, im.header, im.camel).hash(&mut h);
            h.finish()
        };
        if key != im.parsed_key {
            im.parsed_key = key;
            im.parsed = (!im.text.trim().is_empty()).then(|| db::parse_import(&im.text, im.kind, &db::CsvOptions { separator: im.separator, header: im.header, camel_case: im.camel }));
            im.fields = match &im.parsed {
                Some(Ok(d)) => auto_fields(&d[..d.len().min(500)]),
                _ => Vec::new(),
            };
            t.selected = None;
        }
        ui.add_space(6.0);
        let mut preview = None;
        match &im.parsed {
            None => {
                ui.label(RichText::new("Nothing to import yet.").color(MUTED()));
            }
            Some(Err(e)) => {
                ui.label(RichText::new(e).monospace().color(ALERT()));
            }
            Some(Ok(docs)) => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} documents ready", group(&docs.len().to_string()))).color(BRIGHT()));
                    ui.label(RichText::new(format!("·  {} fields  ·  preview of the first {}", im.fields.len(), docs.len().min(PAGE_SIZE as usize))).color(MUTED()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let label = format!("{} {} documents", if im.upsert { "Upsert" } else { "Import" }, group(&docs.len().to_string()));
                        start = ui.add_enabled(!docs.is_empty(), primary_button(ui, label, if writable { PRIMARY() } else { FAINT() })).clicked();
                        if !writable {
                            ui.label(RichText::new("needs writes unlocked").size(12.0).color(FAINT()));
                        }
                    });
                });
                preview = Some((docs[..docs.len().min(PAGE_SIZE as usize)].to_vec(), im.fields.clone()));
            }
        }
        if open_file {
            if let Some(path) = rfd::FileDialog::new().add_filter("Data", &["json", "jsonl", "ndjson", "csv", "tsv", "txt"]).pick_file() {
                match std::fs::read_to_string(&path) {
                    Ok(text) => {
                        let im = &mut self.tabs[self.cur].import;
                        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
                        im.kind = match ext.as_str() {
                            "csv" | "tsv" => db::ImportKind::Csv,
                            "jsonl" | "ndjson" => db::ImportKind::JsonLines,
                            _ if text.trim_start().starts_with('{') && text.lines().filter(|l| !l.trim().is_empty()).count() > 1 && db::parse_doc(text.lines().next().unwrap_or_default()).is_ok() => db::ImportKind::JsonLines,
                            _ => db::ImportKind::Json,
                        };
                        if ext == "tsv" {
                            im.separator = Some('\t');
                        }
                        im.file = path.file_name().map(|f| f.to_string_lossy().to_string());
                        im.text = text;
                    }
                    Err(e) => self.err(format!("Could not read the file: {e}")),
                }
            }
        }
        if start {
            if !writable {
                self.ask_unlock();
            } else {
                let im = &self.tabs[self.cur].import;
                let n = match &im.parsed {
                    Some(Ok(d)) => d.len(),
                    _ => 0,
                };
                if im.upsert {
                    let key = im.upsert_by.trim().to_string();
                    let coll = self.tabs[self.cur].sel.coll.clone();
                    self.modal = Some(Modal::Typed {
                        title: format!("Upsert {n} documents into {coll} by {key}? Matching documents are updated with the imported fields."),
                        expect: coll,
                        typed: String::new(),
                        action: Danger::Upsert { key },
                    });
                } else {
                    self.modal = Some(Modal::ImportInsert(n));
                }
            }
        }
        if let Some((docs, fields)) = preview {
            // The preview uses the same table as documents.
            let t = &mut self.tabs[self.cur];
            let saved_docs = std::mem::replace(&mut t.agg_docs, docs);
            let saved_fields = std::mem::replace(&mut t.agg_fields, fields);
            self.results(ui, true);
            let t = &mut self.tabs[self.cur];
            t.agg_docs = saved_docs;
            t.agg_fields = saved_fields;
        }
    }

    fn indexes_view(&mut self, ui: &mut egui::Ui) {
        toolbar(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("NEW INDEX").size(11.0).color(MUTED()));
                ui.add(egui::TextEdit::singleline(&mut self.idx_keys).font(FontId::monospace(13.0)).hint_text("{ email: 1 }").desired_width(280.0));
                ui.checkbox(&mut self.idx_unique, "unique");
                if ui.button("Create").clicked() {
                    match db::parse_doc(&self.idx_keys) {
                        Ok(k) if k.is_empty() => self.err("Give at least one key, like { email: 1 }"),
                        Ok(k) => {
                            if let Some((_, target)) = self.writable_target() {
                                self.worker.create_index(target, k, self.idx_unique);
                            }
                        }
                        Err(e) => self.err(e),
                    }
                }
            });
        });
        ui.add_space(8.0);
        let mut drop = None;
        let mut hide = None;
        let t = self.t().unwrap();
        let key = format!("{}.{}", t.sel.db, t.sel.coll);
        let info = self.idx_info.as_ref().filter(|(k, _)| *k == key).map(|(_, i)| i);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
        let (mut create, mut profiling) = (None, false);
        egui::ScrollArea::both().id_salt("indexes").auto_shrink(false).show(ui, |ui| {
            egui::Grid::new("idx").striped(true).num_columns(6).spacing([24.0, 8.0]).show(ui, |ui| {
                for h in ["NAME", "KEYS", "OPTIONS", "USAGE", "SIZE", ""] {
                    ui.label(RichText::new(h).size(11.0).color(MUTED()));
                }
                ui.end_row();
                for ix in &t.indexes {
                    let name = ix.get_str("name").unwrap_or_default().to_string();
                    ui.label(RichText::new(&name).monospace().color(BRIGHT()));
                    ui.label(RichText::new(ix.get("keys").map(db::compact).unwrap_or_default()).monospace().color(C_STRING()));
                    let mut opts = ix.clone();
                    opts.remove("name");
                    opts.remove("keys");
                    ui.label(RichText::new(if opts.is_empty() { String::new() } else { db::compact(&Bson::Document(opts)) }).monospace().color(MUTED()));
                    let stat = info.and_then(|i| i.stats.iter().find(|s| s.name == name));
                    ui.horizontal(|ui| {
                        let Some(s) = stat else { return };
                        // A counter that restarted recently cannot prove an index is unused.
                        let recent = s.since.is_some_and(|t| now - t < 7 * 24 * 3600 * 1000);
                        ui.label(RichText::new(format!("{} ops{}", group(&s.ops.to_string()), if s.ops == 0 && recent { ", since restart" } else { "" })).monospace().color(MUTED()));
                        if name != "_id_" && db::is_unused(s.ops, s.since, now) {
                            pill(ui, "unused", WARNING());
                        }
                        if s.hidden {
                            pill(ui, "hidden", PRIMARY());
                        }
                    });
                    ui.label(RichText::new(stat.and_then(|s| s.size).map(human_bytes).unwrap_or_default()).monospace().color(MUTED()));
                    ui.horizontal(|ui| {
                        if name != "_id_" {
                            let hidden = stat.is_some_and(|s| s.hidden);
                            if ui.small_button(if hidden { "Unhide" } else { "Hide" }).clicked() {
                                hide = Some((name.clone(), !hidden));
                            }
                            if ui.small_button(RichText::new("Drop").color(ALERT())).clicked() {
                                drop = Some(name);
                            }
                        }
                    });
                    ui.end_row();
                }
            });
            if let Some(i) = info {
                ui.add_space(12.0);
                ui.label(RichText::new("SUGGESTIONS").size(11.0).color(MUTED()));
                // Skip queries an existing index already serves from its leading keys.
                let covered = |sug: &Document| t.indexes.iter().any(|ix| ix.get_document("keys").is_ok_and(|k| k.keys().zip(sug.keys()).filter(|(a, b)| a == b).count() == sug.len()));
                let shapes: Vec<_> = i.shapes.iter().filter(|s| !covered(&s.suggestion)).take(8).collect();
                if i.level == 0 && shapes.is_empty() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("The profiler is off, so slow queries are not recorded.").color(MUTED()));
                        profiling = ui.button("Enable profiling (slow ops >= 100 ms)").clicked();
                    });
                } else if shapes.is_empty() {
                    ui.label(RichText::new(format!("No unindexed or slow queries recorded (profiler level {}, slow >= {} ms).", i.level, i.slowms)).color(MUTED()));
                }
                for s in shapes {
                    ui.horizontal(|ui| {
                        let sort = if s.sort.is_empty() { String::new() } else { format!("  sort {}", db::compact(&Bson::Document(s.sort.clone()))) };
                        ui.label(RichText::new(format!("{}x  avg {:.0} ms  find {}{sort}", s.count, s.avg_ms, db::compact(&Bson::Document(s.filter.clone())))).monospace().color(MUTED()));
                        ui.label(RichText::new(db::compact(&Bson::Document(s.suggestion.clone()))).monospace().color(C_STRING()));
                        if ui.small_button("Create").clicked() {
                            create = Some(s.suggestion.clone());
                        }
                    });
                }
            }
        });
        if let Some(k) = create {
            self.idx_keys = db::shell_text(&Bson::Document(k));
        }
        if let Some((name, hidden)) = hide {
            if let Some((conn, target)) = self.writable_target() {
                let done = format!("{} index {name}", if hidden { "Hid" } else { "Unhid" });
                self.worker.db_change(conn, target.client, target.db, doc! { "collMod": target.coll, "index": { "name": name, "hidden": hidden } }, done);
            }
        }
        if profiling {
            if let Some((conn, target)) = self.writable_target() {
                self.worker.db_change(conn, target.client, target.db, doc! { "profile": 1, "slowms": 100 }, "Profiling slow operations".into());
            }
        }
        if let Some(name) = drop {
            self.modal = Some(Modal::Typed {
                title: format!("Drop the index {name}? Queries that rely on it may become slow."),
                expect: name.clone(),
                typed: String::new(),
                action: Danger::DropIndex(name),
            });
        }
    }

    fn modals(&mut self, ctx: &egui::Context) {
        if let Some(mut je) = self.json_edit.take() {
            let mut apply = false;
            let mut close = false;
            let screen = ctx.content_rect();
            let r = dialog(egui::Id::unique(("quarry-layer", "jsonedit"))).show(ctx, |ui| {
                ui.set_width((screen.width() * 0.72).clamp(480.0, 1200.0));
                ui.horizontal(|ui| {
                    let title = if je.path.is_empty() { "Edit document".to_string() } else { format!("Edit {}", je.path.join(".")) };
                    dialog_title(ui, title);
                    if !je.path.is_empty() {
                        ui.add_space(8.0);
                        Self::type_combo(ui, "jsonkind", &mut je.kind);
                    }
                });
                ui.label(RichText::new("JSON, JSON5 or shell syntax. Applying keeps it as an unsaved edit until you press Save.").size(12.0).color(MUTED()));
                ui.add_space(4.0);
                let rows = je.text.lines().count().clamp(12, 40);
                let id = egui::Id::unique("json-editor-text");
                let find = Find::bar(ui, id, &je.text);
                egui::ScrollArea::vertical().max_height(screen.height() * 0.62).show(ui, |ui| {
                    let out = with_autopair(ui, id, &mut je.text, |ui, text| egui::TextEdit::multiline(text).id(id).code_editor().desired_width(f32::INFINITY).desired_rows(rows).layouter(&mut find.layouter(true)).show(ui));
                    find.reveal(ui, &je.text, &out);
                    if je.focus {
                        out.response.request_focus();
                        je.focus = false;
                    }
                });
                ui.add_space(4.0);
                dialog_actions(ui, |ui| {
                    apply = primary(ui, "Apply", PRIMARY()).on_hover_text("Cmd+Enter").clicked() || ui.input(|i| i.modifiers.command && (i.key_pressed(Key::Enter) || i.key_pressed(Key::S)));
                    close = ui.button("Cancel").on_hover_text("Esc").clicked();
                });
            });
            if apply {
                let new = if je.path.is_empty() {
                    db::parse_doc(&je.text).and_then(|mut new| {
                        match (je.base.get("_id"), new.get("_id")) {
                            (Some(a), Some(b)) if a != b => return Err("The _id of a document cannot change. Use Duplicate to make a copy.".to_string()),
                            (Some(a), None) => {
                                // Keep _id first, as MongoDB stores it.
                                let mut d = doc! { "_id": a.clone() };
                                d.extend(new);
                                new = d;
                            }
                            _ => {}
                        }
                        Ok(new)
                    })
                } else {
                    db::make_value(je.kind, &je.text).and_then(|v| db::edit_path(&je.base, &je.path, Some(v)))
                };
                match new {
                    Ok(n) => {
                        let base = je.base.clone();
                        self.stage(&base, Ok(n));
                    }
                    Err(e) => {
                        self.err(e);
                        self.json_edit = Some(je);
                    }
                }
            } else if !(close || r.should_close()) {
                self.json_edit = Some(je);
            }
        }
        if let Some(text) = &mut self.insert {
            let mut close = false;
            let mut submit = false;
            let r = dialog(egui::Id::unique(("quarry-layer", "insert"))).show(ctx, |ui| {
                ui.set_width(560.0);
                dialog_title(ui, "Insert a document");
                ui.label(RichText::new("JSON, JSON5 or shell syntax: ObjectId(\"…\"), ISODate(\"…\"), NumberLong(…).").color(MUTED()));
                let id = egui::Id::unique("insert-editor-text");
                let find = Find::bar(ui, id, text);
                egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                    let out = with_autopair(ui, id, text, |ui, text| egui::TextEdit::multiline(text).id(id).code_editor().desired_width(f32::INFINITY).desired_rows(16).layouter(&mut find.layouter(true)).show(ui));
                    find.reveal(ui, text, &out);
                });
                dialog_actions(ui, |ui| {
                    submit = primary(ui, "Insert", PRIMARY()).clicked();
                    close = ui.button("Cancel").on_hover_text("Esc").clicked();
                });
            });
            if submit {
                match db::parse_doc(text) {
                    Ok(d) => {
                        if let Some((conn, target)) = self.writable_target() {
                            self.worker.insert(conn, target, d, self.insert.take());
                        }
                    }
                    Err(e) => self.err(e),
                }
            } else if close || r.should_close() {
                self.insert = None;
            }
        }

        let Some(mut m) = self.modal.take() else { return };
        let mut keep = true;
        let r = dialog(egui::Id::unique(("quarry-layer", "modal"))).show(ctx, |ui| {
            ui.set_max_width(620.0);
            match &mut m {
                Modal::Conn(f) => {
                    let mut save = false;
                    let mut test = false;
                    ui.set_width(560.0);
                    dialog_title(ui, if f.id.is_some() { "Edit connection" } else { "New connection" });
                    ui.label(RichText::new("Addresses, passwords and passphrases are stored in the macOS Keychain. The connection opens read-only.").size(12.0).color(MUTED()));
                    ui.add_space(8.0);
                    egui::ScrollArea::vertical().max_height(ctx.content_rect().height() - 220.0).show(ui, |ui| {
                        settings_section(ui, "SERVER", |ui| {
                            egui::Grid::new("cf").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                                ui.label("Name");
                                ui.add(egui::TextEdit::singleline(&mut f.name).hint_text("Production cluster").desired_width(400.0));
                                ui.end_row();
                                ui.label("URI");
                                ui.add(egui::TextEdit::singleline(&mut f.uri).password(!f.show).font(FontId::monospace(13.0)).desired_width(400.0));
                                ui.end_row();
                                ui.label("");
                                ui.checkbox(&mut f.show, "Show address");
                                ui.end_row();
                            });
                        });
                        settings_section(ui, "SSH TUNNEL", |ui| {
                            ui.checkbox(&mut f.ssh_on, "Connect through an SSH server (jump host)");
                            if f.ssh_on {
                                egui::Grid::new("ssh").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                                    ui.label("Host");
                                    ui.horizontal(|ui| {
                                        ui.add(egui::TextEdit::singleline(&mut f.ssh.host).hint_text("bastion.example.com").desired_width(280.0));
                                        ui.label("Port");
                                        let mut port = if f.ssh.port == 0 { "22".to_string() } else { f.ssh.port.to_string() };
                                        if ui.add(egui::TextEdit::singleline(&mut port).desired_width(60.0)).changed() {
                                            f.ssh.port = port.trim().parse().unwrap_or(22);
                                        }
                                    });
                                    ui.end_row();
                                    ui.label("User");
                                    ui.add(egui::TextEdit::singleline(&mut f.ssh.user).desired_width(280.0));
                                    ui.end_row();
                                    ui.label("Log in with");
                                    segmented(ui, &mut f.ssh.auth, &[(crate::ssh::SshAuth::Auto, "Auto"), (crate::ssh::SshAuth::Password, "Password"), (crate::ssh::SshAuth::KeyFile, "Key file"), (crate::ssh::SshAuth::Agent, "SSH agent")]);
                                    ui.end_row();
                                    use crate::ssh::SshAuth::*;
                                    if matches!(f.ssh.auth, Auto | Password) {
                                        ui.label("Password");
                                        ui.add(egui::TextEdit::singleline(&mut f.ssh_pwd).password(true).hint_text(if f.ssh.auth == Auto { "optional, tried last" } else { "" }).desired_width(280.0));
                                        ui.end_row();
                                    }
                                    if matches!(f.ssh.auth, Auto | KeyFile) {
                                        ui.label("Key file");
                                        ui.horizontal(|ui| {
                                            ui.add(egui::TextEdit::singleline(&mut f.ssh.key_path).hint_text(if f.ssh.auth == Auto { "optional; ~/.ssh/id_* are tried" } else { "~/.ssh/id_ed25519" }).desired_width(280.0));
                                            if ui.button("Choose…").clicked() {
                                                if let Some(p) = rfd::FileDialog::new().set_directory(dirs::home_dir().unwrap_or_default().join(".ssh")).pick_file() {
                                                    f.ssh.key_path = p.display().to_string();
                                                }
                                            }
                                        });
                                        ui.end_row();
                                        ui.label("Passphrase");
                                        ui.add(egui::TextEdit::singleline(&mut f.ssh_pass).password(true).hint_text("if the key has one").desired_width(280.0));
                                        ui.end_row();
                                    }
                                    ui.label("");
                                    ui.checkbox(&mut f.ssh.legacy_rsa, "Allow legacy ssh-rsa host keys (old servers)");
                                    ui.end_row();
                                    ui.label("Host key");
                                    ui.horizontal(|ui| match f.ssh.known_host.clone() {
                                        Some(fp) => {
                                            ui.label(RichText::new(fp).monospace().size(12.0).color(MUTED()));
                                            if ui.small_button("Forget").on_hover_text("Trust the next key the server shows, e.g. after a reinstall").clicked() {
                                                f.ssh.known_host = None;
                                            }
                                        }
                                        None => {
                                            ui.label(RichText::new("remembered on first connect; a different key later is refused").size(12.0).color(FAINT()));
                                        }
                                    });
                                    ui.end_row();
                                });
                            }
                        });
                        settings_section(ui, "SOCKS5 PROXY", |ui| {
                            ui.add_enabled(!f.ssh_on, egui::Checkbox::new(&mut f.proxy_on, "Connect through a SOCKS5 proxy"));
                            if f.ssh_on {
                                ui.label(RichText::new("Not needed with an SSH tunnel: the tunnel already carries the connection.").size(12.0).color(FAINT()));
                            } else if f.proxy_on {
                                egui::Grid::new("px").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                                    ui.label("Host");
                                    ui.horizontal(|ui| {
                                        ui.add(egui::TextEdit::singleline(&mut f.proxy.host).desired_width(280.0));
                                        ui.label("Port");
                                        let mut port = if f.proxy.port == 0 { "1080".to_string() } else { f.proxy.port.to_string() };
                                        if ui.add(egui::TextEdit::singleline(&mut port).desired_width(60.0)).changed() {
                                            f.proxy.port = port.trim().parse().unwrap_or(1080);
                                        }
                                    });
                                    ui.end_row();
                                    ui.label("User");
                                    ui.add(egui::TextEdit::singleline(&mut f.proxy.user).hint_text("optional").desired_width(280.0));
                                    ui.end_row();
                                    ui.label("Password");
                                    ui.add(egui::TextEdit::singleline(&mut f.proxy_pwd).password(true).desired_width(280.0));
                                    ui.end_row();
                                });
                            }
                        });
                    });
                    match &self.conn_test {
                        None if f.testing => {
                            ui.horizontal(|ui| {
                                spin(ui);
                                ui.label(RichText::new("Testing…").color(MUTED()));
                            });
                        }
                        Some(Ok(m)) => {
                            ui.label(RichText::new(m).color(SUCCESS()));
                        }
                        Some(Err(e)) => {
                            ui.add(egui::Label::new(RichText::new(e).color(ALERT())).wrap());
                        }
                        None => {}
                    }
                    dialog_actions(ui, |ui| {
                        let valid = !f.name.trim().is_empty() && (f.uri.starts_with("mongodb://") || f.uri.starts_with("mongodb+srv://")) && (!f.ssh_on || (!f.ssh.host.trim().is_empty() && !f.ssh.user.trim().is_empty())) && (!f.proxy_on || f.ssh_on || !f.proxy.host.trim().is_empty());
                        save = ui.add_enabled(valid, primary_button(ui, "Save and connect", PRIMARY())).clicked();
                        test = ui.add_enabled(valid, egui::Button::new("Test")).clicked();
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                    if test {
                        f.testing = true;
                        self.conn_test = None;
                        let net = f.net();
                        self.worker.test(f.uri.trim().to_string(), self.timeout(), net);
                    }
                    if save {
                        let id = f.id.unwrap_or_else(|| self.conns.iter().map(|c| c.meta.id).max().unwrap_or(0) + 1);
                        match keychain(id).map(|k| k.set_password(f.uri.trim())) {
                            Some(Ok(())) => {
                                let stored = [
                                    set_secret(id, "ssh-password", if f.ssh_on { &f.ssh_pwd } else { "" }),
                                    set_secret(id, "ssh-passphrase", if f.ssh_on { &f.ssh_pass } else { "" }),
                                    set_secret(id, "proxy-password", if f.proxy_on && !f.ssh_on { &f.proxy_pwd } else { "" }),
                                ];
                                let meta = ConnMeta {
                                    id,
                                    name: f.name.trim().into(),
                                    ssh: f.ssh_on.then(|| crate::ssh::SshConf { host: f.ssh.host.trim().into(), user: f.ssh.user.trim().into(), ..f.ssh.clone() }),
                                    proxy: (f.proxy_on && !f.ssh_on).then(|| db::Proxy { host: f.proxy.host.trim().into(), password: None, ..f.proxy.clone() }),
                                };
                                match self.conns.iter_mut().find(|c| c.meta.id == id) {
                                    Some(c) => {
                                        c.meta = meta;
                                        c.masked_uri = db::mask_uri(f.uri.trim());
                                        c.writable = false;
                                        c.dbs.clear();
                                    }
                                    None => self.conns.push(Conn { meta, client: None, status: Status::Idle, writable: false, dbs: Vec::new(), masked_uri: db::mask_uri(f.uri.trim()), tunnel: None }),
                                }
                                self.active = Some(id);
                                self.save_conns();
                                self.connect(id);
                                keep = false;
                                if let Some(Err(e)) = stored.into_iter().find(|r| r.is_err()) {
                                    self.err(e);
                                }
                            }
                            Some(Err(e)) => self.err(format!("Could not save the address to the Keychain: {e}")),
                            None => self.err("The Keychain is not available."),
                        }
                    }
                }
                Modal::Settings => {
                    ui.set_width(600.0);
                    ui.horizontal(|ui| {
                        dialog_title(ui, "Settings");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Done").on_hover_text("Esc").clicked() {
                                keep = false;
                            }
                        });
                    });
                    ui.label(RichText::new("Changes are saved right away.").size(12.0).color(FAINT()));
                    ui.add_space(8.0);
                    let before = (self.prefs.open_inline, self.prefs.auto_save, self.prefs.always_writable, self.prefs.confirm_save, self.prefs.default_cols, self.prefs.timeout_ms, self.prefs.shell_runtime.clone(), self.prefs.mongodump.clone(), self.prefs.mongorestore.clone(), self.prefs.auto_update, self.prefs.show_fps, self.prefs.object_preview);
                    // The sections scroll inside the dialog, which keeps its title and Done in the window.
                    let max_h = (ui.ctx().content_rect().height() - 150.0).max(200.0);
                    egui::ScrollArea::vertical().id_salt("settings-scroll").max_height(max_h).min_scrolled_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                        settings_section(ui, "APPEARANCE", |ui| {
                            setting_row(ui, "Theme", "System follows the macOS light or dark setting.", |ui| {
                                let was = self.prefs.appearance;
                                segmented(ui, &mut self.prefs.appearance, &[(Appearance::System, "System"), (Appearance::Light, "Light"), (Appearance::Dark, "Dark")]);
                                if self.prefs.appearance != was {
                                    self.prefs.appearance.apply(ui.ctx());
                                    self.save_prefs();
                                }
                            });
                        });
                        settings_section(ui, "DOCUMENTS", |ui| {
                            setting_row(ui, "Open documents", "Where a clicked row shows its document.", |ui| {
                                segmented(ui, &mut self.prefs.open_inline, &[(false, "Side panel"), (true, "Under the row")]);
                            });
                            ui.separator();
                            setting_row(ui, "Columns shown", "How many fields the table shows before you pick your own.", |ui| {
                                stepper(ui, &mut self.prefs.default_cols, 3, 30);
                            });
                            ui.separator();
                            setting_row(ui, "Big objects in columns", "Objects and arrays too long for a cell show their size, or their first characters as in Mingo. Small ones always show in full.", |ui| {
                                segmented(ui, &mut self.prefs.object_preview, &[(false, "Size"), (true, "First characters")]);
                            });
                        });
                        settings_section(ui, "EDITING", |ui| {
                            setting_row(ui, "Auto-save edits", "Write each edit right away and offer Undo for 8 seconds.", |ui| {
                                toggle(ui, &mut self.prefs.auto_save);
                            });
                            ui.separator();
                            setting_row(ui, "Always allow writes", "Skip read-only mode on every connection and save edits without showing the changes first.", |ui| {
                                toggle(ui, &mut self.prefs.always_writable);
                            });
                            ui.separator();
                            let auto = self.prefs.auto_save || self.prefs.always_writable;
                            setting_row(ui, "Show changes before saving", "List every changed field before writing unsaved edits.", |ui| {
                                ui.add_enabled_ui(!auto, |ui| toggle(ui, &mut self.prefs.confirm_save));
                            });
                            ui.label(RichText::new(if self.prefs.always_writable { "A save never overwrites someone else's change." } else { "Writes always need the connection unlocked, and a save never overwrites someone else's change." }).size(12.0).color(FAINT()));
                        });
                        settings_section(ui, "QUERIES", |ui| {
                            setting_row(ui, "Query timeout", "How long a query may run before it is stopped.", |ui| {
                                egui::ComboBox::from_id_salt("timeout").width(90.0).selected_text(format!("{} s", self.prefs.timeout_ms / 1000)).show_ui(ui, |ui| {
                                    // Same presets as the original's preferences.
                                    for s in [5, 10, 30, 60, 300, 3600] {
                                        ui.selectable_value(&mut self.prefs.timeout_ms, s * 1000, format!("{s} s"));
                                    }
                                });
                            });
                        });
                        settings_section(ui, "SHELL", |ui| {
                            let detected = find_bun("").map(|p| p.display().to_string());
                            let desc = match &detected {
                                Some(p) => format!("Found automatically at {p}. Set a path only to use another Bun."),
                                None => "Bun was not found. Install it from bun.sh or enter its path.".to_string(),
                            };
                            setting_row(ui, "Bun runtime", &desc, |ui| {
                                ui.add(egui::TextEdit::singleline(&mut self.prefs.shell_runtime).hint_text("automatic").desired_width(190.0));
                            });
                        });
                        settings_section(ui, "PERFORMANCE", |ui| {
                            setting_row(ui, "Show frame rate", "Frames per second and the time each frame takes, in the status bar. Quarry follows your display's refresh rate (vsync), up to 120 Hz on ProMotion screens.", |ui| {
                                toggle(ui, &mut self.prefs.show_fps);
                            });
                        });
                        settings_section(ui, "UPDATES", |ui| {
                            let desc = if self.update_note.is_empty() { format!("Version {}. New releases of {} are installed in the background, then Quarry restarts when idle with nothing unsaved.", env!("CARGO_PKG_VERSION"), crate::update::REPO) } else { format!("Version {}. {}", env!("CARGO_PKG_VERSION"), self.update_note) };
                            setting_row(ui, "Update automatically", &desc, |ui| {
                                toggle(ui, &mut self.prefs.auto_update);
                            });
                            if !self.demo && !cfg!(debug_assertions) && self.updated.is_none() && ui.button("Check now").clicked() {
                                self.check_update();
                            }
                        });
                        settings_section(ui, "DATABASE TOOLS", |ui| {
                            for (i, (name, path)) in [("mongodump", &mut self.prefs.mongodump), ("mongorestore", &mut self.prefs.mongorestore)].into_iter().enumerate() {
                                if i > 0 {
                                    ui.separator();
                                }
                                let desc = match find_exe(name, "") {
                                    Some(p) => format!("Found automatically at {}.", p.display()),
                                    None => "Not found. Install the MongoDB Database Tools or enter its path.".to_string(),
                                };
                                setting_row(ui, name, &desc, |ui| {
                                    ui.add(egui::TextEdit::singleline(path).hint_text("automatic").desired_width(190.0));
                                });
                            }
                        });
                        if find_exe("mongodump", "").is_none() || find_exe("mongorestore", "").is_none() {
                            self.install_tools_button(ui);
                        }
                    });
                    if before != (self.prefs.open_inline, self.prefs.auto_save, self.prefs.always_writable, self.prefs.confirm_save, self.prefs.default_cols, self.prefs.timeout_ms, self.prefs.shell_runtime.clone(), self.prefs.mongodump.clone(), self.prefs.mongorestore.clone(), self.prefs.auto_update, self.prefs.show_fps, self.prefs.object_preview) {
                        self.save_prefs();
                    }
                }
                Modal::Unlock(id, then) => {
                    let name = self.conn(*id).map(|c| c.meta.name.clone()).unwrap_or_default();
                    dialog_title(ui, format!("Allow writes on \"{name}\"?"));
                    ui.label(RichText::new("Edits, inserts, deletes and index changes become possible until you lock it again or quit. Each one still asks first.").color(MUTED()));
                    dialog_actions(ui, |ui| {
                        if primary(ui, "Allow writes", ALERT()).clicked() {
                            if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == *id) {
                                c.writable = true;
                            }
                            self.ok(format!("Writes allowed on {name} for this session"));
                            keep = false;
                            if let Some(then) = then.take() {
                                then(self);
                            }
                        }
                        if ui.button("Stay read-only").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::Save { tag, changes } => {
                    dialog_title(ui, format!("Save {} change(s)?", changes.len()));
                    ui.label(RichText::new("Saving is refused if someone else changed this document since you opened it.").color(MUTED()));
                    egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                        egui::Grid::new("chg").num_columns(3).striped(true).spacing([14.0, 6.0]).show(ui, |ui| {
                            for (path, a, b) in changes.iter() {
                                ui.label(RichText::new(path).monospace().color(BRIGHT()));
                                let show = |v: &Option<Bson>| v.as_ref().map(|v| format!("{} ({})", leaf_text(v), db::type_name(v))).unwrap_or("(none)".into());
                                ui.label(RichText::new(show(a)).monospace().color(ALERT()).strikethrough());
                                ui.label(RichText::new(show(b)).monospace().color(SUCCESS()));
                                ui.end_row();
                            }
                        });
                    });
                    dialog_actions(ui, |ui| {
                        if primary(ui, "Save", PRIMARY()).clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                            self.write_draft(tag);
                            keep = false;
                        }
                        if ui.button("Keep editing").on_hover_text("Esc").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::Delete(docs) => {
                    const SHOWN: usize = 5;
                    let ids: Vec<String> = docs.iter().take(SHOWN).map(|d| d.get("_id").map(db::short).unwrap_or_default()).collect();
                    let more = if docs.len() > SHOWN { format!(" and {} more", docs.len() - SHOWN) } else { String::new() };
                    dialog_title(ui, if docs.len() == 1 { "Delete this document?".to_string() } else { format!("Delete {} documents?", docs.len()) });
                    ui.label(RichText::new(format!("_id {}{more}. You can undo this for 24 hours.", ids.join(", "))).monospace().color(MUTED()));
                    dialog_actions(ui, |ui| {
                        if primary(ui, "Delete", ALERT()).clicked() {
                            if let Some((conn, target)) = self.writable_target() {
                                match docs.as_slice() {
                                    [d] => self.worker.delete(conn, target, d.clone(), true),
                                    _ => self.worker.delete_docs(conn, target, docs.clone()),
                                }
                                // The reload drops deleted rows from the checks; others stay checked.
                                if let Some(t) = self.tm() {
                                    t.selected = None;
                                }
                            }
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::Typed { title, expect, typed, action } => {
                    ui.label(RichText::new(title.as_str()).strong().color(BRIGHT()));
                    ui.label(RichText::new(format!("Type {expect} to confirm (case sensitive).")).color(MUTED()));
                    ui.add(egui::TextEdit::singleline(typed).font(FontId::monospace(13.0)).desired_width(320.0));
                    dialog_actions(ui, |ui| {
                        let ok = typed == expect;
                        if ui.add_enabled(ok, primary_button(ui, "Confirm", ALERT())).clicked() {
                            keep = false;
                            let action = std::mem::replace(action, Danger::DropCollection);
                            self.danger(action);
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::NewCollection { conn, db, name, view, source, pipeline } => {
                    ui.set_width(520.0);
                    dialog_title(ui, if *view { format!("New view in {db}") } else { format!("New collection in {db}") });
                    egui::Grid::new("newcoll").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        ui.label("Name");
                        ui.add(egui::TextEdit::singleline(name).desired_width(380.0));
                        ui.end_row();
                        if *view {
                            ui.label("Based on");
                            let colls: Vec<String> = self.conn(*conn).and_then(|c| c.dbs.iter().find(|d| d.0 == *db)).and_then(|d| d.1.clone()).unwrap_or_default();
                            egui::ComboBox::from_id_salt("view-src").width(380.0).selected_text(if source.is_empty() { "choose a collection" } else { source.as_str() }).show_ui(ui, |ui| {
                                for c in colls {
                                    ui.selectable_value(source, c.clone(), c);
                                }
                            });
                            ui.end_row();
                            ui.label("Pipeline");
                            let id = ui.make_persistent_id("view-pipeline");
                            with_autopair(ui, id, pipeline, |ui, text| egui::TextEdit::multiline(text).id(id).code_editor().desired_width(380.0).desired_rows(6).layouter(&mut code_layout).show(ui));
                            ui.end_row();
                        }
                    });
                    dialog_actions(ui, |ui| {
                        let ok = !name.trim().is_empty() && (!*view || !source.is_empty());
                        if ui.add_enabled(ok, primary_button(ui, "Create", PRIMARY())).clicked() {
                            let cmd = if *view {
                                db::parse_pipeline(pipeline).map(|p| doc! { "create": name.trim(), "viewOn": source.as_str(), "pipeline": p })
                            } else {
                                Ok(doc! { "create": name.trim() })
                            };
                            match (cmd, self.conn(*conn).and_then(|c| c.client.clone())) {
                                (Ok(cmd), Some(client)) => {
                                    self.worker.db_change(*conn, client, db.clone(), cmd, format!("Created {}", name.trim()));
                                    keep = false;
                                }
                                (Err(e), _) => self.err(e),
                                (_, None) => self.err("Not connected."),
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::Tool(f) => {
                    let name = if f.coll.is_empty() { f.db.clone() } else { format!("{}.{}", f.db, f.coll) };
                    ui.set_width(560.0);
                    dialog_title(ui, if f.restore { format!("Restore into {name}") } else { format!("Dump {name}") });
                    let meta = self.conn(f.conn).map(|c| c.meta.clone()).unwrap_or_default();
                    let tool = if f.restore { "mongorestore" } else { "mongodump" };
                    let exe = find_exe(tool, if f.restore { &self.prefs.mongorestore } else { &self.prefs.mongodump });
                    ui.label(RichText::new(match &exe {
                        Some(p) => format!("Runs {} with this connection's address.", p.display()),
                        None => format!("{tool} was not found. Install the MongoDB Database Tools here, or set its path in Settings."),
                    }).size(12.0).color(if exe.is_some() { MUTED() } else { WARNING() }));
                    if exe.is_none() {
                        self.install_tools_button(ui);
                    }
                    let tunneled = meta.ssh.is_some() || meta.proxy.is_some();
                    if tunneled {
                        ui.label(RichText::new("This connection goes through SSH or a proxy, which the MongoDB tools cannot use. Open an SSH port forward and use a direct connection instead.").color(WARNING()));
                    }
                    ui.add_space(6.0);
                    egui::Grid::new("toolform").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        ui.label("Archive file");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut f.path).desired_width(360.0));
                            if ui.button("Choose…").clicked() {
                                let d = rfd::FileDialog::new();
                                let picked = if f.restore { d.pick_file() } else { d.set_file_name(format!("{name}.archive.gz")).save_file() };
                                if let Some(p) = picked {
                                    f.path = p.display().to_string();
                                    if f.restore {
                                        f.gzip = f.path.ends_with(".gz");
                                    }
                                }
                            }
                        });
                        ui.end_row();
                        ui.label("");
                        ui.checkbox(&mut f.gzip, "Compressed (gzip)");
                        ui.end_row();
                        if f.restore {
                            ui.label("Database in the archive");
                            ui.add(egui::TextEdit::singleline(&mut f.from_db).desired_width(200.0)).on_hover_text("Restored under the name of this database when it differs");
                            ui.end_row();
                            ui.label("Only collection");
                            ui.add(egui::TextEdit::singleline(&mut f.coll).hint_text("all").desired_width(200.0));
                            ui.end_row();
                            ui.label("");
                            let col = if f.drop { ALERT() } else { TEXT() };
                            ui.checkbox(&mut f.drop, RichText::new("Drop each collection before restoring it").color(col));
                            ui.end_row();
                        }
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let ok = exe.is_some() && !tunneled && !f.path.trim().is_empty() && (!f.restore || !f.from_db.trim().is_empty());
                        if ui.add_enabled(ok, primary_button(ui, if f.restore { "Restore…" } else { "Dump" }, if f.restore { WARNING() } else { PRIMARY() })).clicked() {
                            match stored_uri(f.conn) {
                                Err(e) => self.err(uri_err(e)),
                                Ok(uri) => {
                                    let coll = Some(f.coll.trim()).filter(|c| !c.is_empty());
                                    let path = f.path.trim();
                                    if f.restore {
                                        let args = db::restore_args(&uri, path, f.from_db.trim(), coll, &f.db, f.drop, f.gzip);
                                        let what = if f.drop { "Existing collections with the same names are dropped first. " } else { "" };
                                        self.modal = Some(Modal::Typed { title: format!("Restore {path} into {}? {what}Type the database name.", f.db), expect: f.db.clone(), typed: String::new(), action: Danger::Restore { conn: f.conn, args } });
                                    } else {
                                        self.start_tool(false, format!("Dump {name}"), db::dump_args(&uri, &f.db, coll, path, f.gzip));
                                    }
                                    keep = false;
                                }
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::Relations { conn, db } => {
                    ui.set_max_width(860.0);
                    ui.set_width(820.0);
                    let colls: Vec<String> = self.conn(*conn).and_then(|c| c.dbs.iter().find(|d| d.0 == *db)).and_then(|d| d.1.clone()).unwrap_or_default();
                    dialog_title(ui, format!("Data relations in {db}"));
                    ui.label(RichText::new("A field that holds ids of another collection's documents. Its preview field is shown next to the ids, and \"Go to referenced document\" opens them. Add them by analyzing, or by right-clicking an id and choosing that action.").size(12.0).color(MUTED()));
                    ui.add_space(6.0);
                    let key = format!("{conn}/{db}");
                    let before = self.prefs.relations.get(&key).cloned().unwrap_or_default();
                    let list = self.prefs.relations.entry(key.clone()).or_default();
                    if list.is_empty() {
                        ui.label(RichText::new("None yet.").color(FAINT()));
                    }
                    let mut remove = None;
                    egui::ScrollArea::vertical().max_height(420.0).auto_shrink([false, true]).show(ui, |ui| {
                        egui::Grid::new("rels").num_columns(4).striped(true).spacing([12.0, 8.0]).min_col_width(60.0).show(ui, |ui| {
                            ui.label(RichText::new("FIELD").size(11.0).color(FAINT()));
                            ui.label(RichText::new("POINTS TO COLLECTION").size(11.0).color(FAINT()));
                            ui.label(RichText::new("PREVIEW FIELD").size(11.0).color(FAINT()));
                            ui.end_row();
                            for (i, r) in list.iter_mut().enumerate() {
                                let name = format!("{}.{}", r.coll, r.path);
                                ui.allocate_ui_with_layout(egui::vec2(260.0, 22.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                    ui.set_min_width(260.0);
                                    ui.add(egui::Label::new(RichText::new(&name).monospace().color(KEY())).truncate()).on_hover_text(&name);
                                });
                                egui::ComboBox::from_id_salt(("rel-target", i)).width(250.0).selected_text(&r.target).show_ui(ui, |ui| {
                                    for c in &colls {
                                        ui.selectable_value(&mut r.target, c.clone(), c);
                                    }
                                });
                                ui.add_sized([180.0, 22.0], egui::TextEdit::singleline(&mut r.preview).hint_text("_id"));
                                if ui.small_button(RichText::new("Remove").color(ALERT())).clicked() {
                                    remove = Some(i);
                                }
                                ui.end_row();
                            }
                        });
                    });
                    if let Some(i) = remove {
                        list.remove(i);
                    }
                    if *list != before {
                        self.save_prefs();
                    }
                    ui.add_space(6.0);
                    let mut analyze = None;
                    ui.horizontal(|ui| {
                        if ui.button("Analyze all collections").on_hover_text("Samples the 10 newest documents of each collection and looks their ids up in the others").clicked() {
                            analyze = Some(String::new());
                        }
                        let cur = self.t().filter(|t| t.sel.conn == *conn && t.sel.db == *db).map(|t| t.sel.coll.clone());
                        if let Some(c) = cur {
                            if ui.button(format!("Analyze {c}")).clicked() {
                                analyze = Some(c);
                            }
                        }
                    });
                    let mut note: Option<db::Res<String>> = None;
                    let mut changed = false;
                    ui.horizontal(|ui| {
                        let list = self.prefs.relations.entry(key.clone()).or_default();
                        if ui.add_enabled(!list.is_empty(), egui::Button::new("Export…")).clicked() {
                            if let Some(path) = rfd::FileDialog::new().set_file_name(format!("{db}-data-relations.json")).save_file() {
                                match std::fs::write(&path, relations_export(db, list)) {
                                    Ok(()) => note = Some(Ok(format!("Exported {} data relations", list.len()))),
                                    Err(e) => note = Some(Err(format!("Failed to export relations: {e}"))),
                                }
                            }
                        }
                        if ui.button("Import…").on_hover_text("A file exported here or from Mingo; relations already present are skipped").clicked() {
                            if let Some(path) = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file() {
                                match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| relations_import(&t)) {
                                    Ok(new) => {
                                        let mut added = 0;
                                        for r in new {
                                            if !list.iter().any(|x| x.coll == r.coll && x.path == r.path && x.target == r.target) {
                                                list.push(r);
                                                added += 1;
                                            }
                                        }
                                        changed = true;
                                        note = Some(Ok(format!("Imported {added} data relations")));
                                    }
                                    Err(e) => note = Some(Err(format!("Failed to import relations: {e}"))),
                                }
                            }
                        }
                        let n = self.prefs.relations.get(&key).map_or(0, Vec::len);
                        ui.add_enabled_ui(n > 0, |ui| {
                            ui.menu_button(RichText::new("Remove all…").color(ALERT()), |ui| {
                                if ui.button(RichText::new(format!("Remove all {n} relations of {db}")).color(ALERT())).clicked() {
                                    self.prefs.relations.remove(&key);
                                    changed = true;
                                    ui.close();
                                }
                            });
                        });
                    });
                    if changed {
                        self.save_prefs();
                    }
                    match note {
                        Some(Ok(m)) => self.ok(m),
                        Some(Err(e)) => self.err(e),
                        None => {}
                    }
                    if let Some(c) = analyze {
                        match self.conn(*conn).and_then(|x| x.client.clone()) {
                            Some(client) => self.analyze_relations(*conn, client, db.clone(), c),
                            None => self.err("Connect first."),
                        }
                    }
                    if dialog_actions(ui, |ui| ui.button("Done").clicked()) {
                        keep = false;
                        let reqs: Vec<u64> = self.tabs.iter().filter(|t| t.sel.conn == *conn && t.sel.db == *db).map(|t| t.req).collect();
                        for r in reqs {
                            if let Some(t) = self.tab_by_req(r) {
                                t.related.clear();
                            }
                            self.load_related(r);
                        }
                    }
                }
                Modal::RenameColl { conn, db, coll, name } => {
                    dialog_title(ui, format!("Rename {db}.{coll}"));
                    ui.label(RichText::new("Open tabs of this collection are closed, as in the original.").color(MUTED()));
                    ui.add(egui::TextEdit::singleline(name).desired_width(320.0));
                    dialog_actions(ui, |ui| {
                        let ok = !name.trim().is_empty() && name.trim() != coll.as_str();
                        if ui.add_enabled(ok, primary_button(ui, "Rename", PRIMARY())).clicked() {
                            if let Some(client) = self.conn(*conn).and_then(|c| c.client.clone()) {
                                let (from, to) = (format!("{db}.{coll}"), format!("{db}.{}", name.trim()));
                                self.tabs.retain(|t| !(t.sel.conn == *conn && t.sel.db == *db && t.sel.coll == *coll));
                                self.cur = self.cur.min(self.tabs.len().saturating_sub(1));
                                self.worker.db_change(*conn, client, db.clone(), doc! { "renameCollection": from, "to": to }, format!("Renamed {coll} to {}", name.trim()));
                            }
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::ImportInsert(n) => {
                    let coll = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
                    dialog_title(ui, format!("Insert {n} documents into {coll}?"));
                    ui.label(RichText::new("They are added as new documents. Undo removes exactly these documents again.").color(MUTED()));
                    dialog_actions(ui, |ui| {
                        if primary(ui, "Import", PRIMARY()).clicked() {
                            if let Some((conn, target)) = self.writable_target() {
                                if let Some(Ok(docs)) = self.tabs[self.cur].import.parsed.clone() {
                                    self.worker.import(conn, target, docs, None);
                                    self.ok("Importing…");
                                }
                            }
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::ShellWrite(ops) => {
                    dialog_title(ui, "Execute this command?");
                    ui.label(RichText::new("Your command contains potentially destructive operations:").color(MUTED()));
                    ui.label(RichText::new(ops.join(", ")).monospace().color(ALERT()));
                    dialog_actions(ui, |ui| {
                        if primary(ui, "Execute", ALERT()).clicked() {
                            self.start_shell();
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::AggWrite(p) => {
                    dialog_title(ui, "This pipeline writes data ($out or $merge)");
                    ui.label(RichText::new("It can replace or change a whole collection and cannot be undone here.").color(MUTED()));
                    dialog_actions(ui, |ui| {
                        if primary(ui, "Run it", ALERT()).clicked() {
                            if let Some((_, target)) = self.writable_target() {
                                self.req += 1;
                                let req = self.req;
                                self.tm().unwrap().agg_req = req;
                                self.worker.aggregate(req, target, p.clone());
                            }
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::SaveAll(keys) => {
                    let n: usize = keys.iter().filter_map(|k| self.drafts.get(k)).map(|d| db::diff(&d.original, &d.current).len()).sum();
                    dialog_title(ui, format!("Save {} documents ({n} changes)?", keys.len()));
                    ui.label(RichText::new("Each document is refused if someone else changed it since you opened it; its edits are kept.").color(MUTED()));
                    dialog_actions(ui, |ui| {
                        if primary(ui, "Save all", PRIMARY()).clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                            for k in keys.iter() {
                                self.write_draft(k);
                            }
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::Grouped { sel, path, pipeline, rows } => {
                    ui.set_width(560.0);
                    dialog_title(ui, format!("Distinct values for {}: {path}", sel.coll));
                    let total: u64 = rows.iter().filter_map(|r| r.get("count").and_then(|c| c.as_i64().or_else(|| c.as_i32().map(i64::from)))).map(|c| c as u64).sum();
                    ui.label(RichText::new(format!("{} values in {} documents{}", group(&rows.len().to_string()), group(&total.to_string()), if rows.len() >= 1000 { ", first 1000 values shown" } else { "" })).color(MUTED()));
                    egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                        egui::Grid::new("grouped").striped(true).num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
                            ui.label(RichText::new("VALUE").size(11.0).color(FAINT()));
                            ui.label(RichText::new("COUNT").size(11.0).color(FAINT()));
                            ui.end_row();
                            for r in rows.iter() {
                                // A fixed width: a truncating label in a grid otherwise shrinks to a few letters.
                                ui.allocate_ui_with_layout(egui::vec2(400.0, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                    ui.set_min_width(400.0);
                                    ui.add(egui::Label::new(cell_job(r.get("_id").unwrap_or(&Bson::Null))).truncate());
                                });
                                ui.label(RichText::new(r.get("count").map(db::compact).unwrap_or_default()).monospace().color(C_NUMBER()));
                                ui.end_row();
                            }
                        });
                    });
                    dialog_actions(ui, |ui| {
                        if primary(ui, "Open in Aggregate", PRIMARY()).on_hover_text("Open this pipeline in a new tab").clicked() {
                            let mut t = Tab::new(sel.clone());
                            t.view = View::Aggregate;
                            t.agg_txt = format!("[\n{}\n]", pipeline.iter().map(|s| format!("  {}", db::shell_text(&Bson::Document(s.clone())))).collect::<Vec<_>>().join(",\n"));
                            // Next to the current tab, like the original's nextToActive.
                            let at = (self.cur + 1).min(self.tabs.len());
                            self.tabs.insert(at, t);
                            self.cur = at;
                            self.reset_doc_state();
                            self.run_pipeline(pipeline.clone());
                            keep = false;
                        }
                        if ui.button("Copy").on_hover_text("Copy as JSON: value and count").clicked() {
                            ui.ctx().copy_text(db::to_edit_value(&Bson::Array(rows.iter().cloned().map(Bson::Document).collect())));
                        }
                        if ui.button("Close").on_hover_text("Esc").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::Text { title, body, code } => {
                    ui.set_width(560.0);
                    dialog_title(ui, title.as_str());
                    let find = Find::bar(ui, egui::Id::unique("text-modal"), body);
                    egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                        let mut text = body.as_str();
                        let out = egui::TextEdit::multiline(&mut text).code_editor().desired_width(f32::INFINITY).layouter(&mut find.layouter(*code)).show(ui);
                        find.reveal(ui, body, &out);
                    });
                    dialog_actions(ui, |ui| {
                        if ui.button("Copy").clicked() {
                            ui.ctx().copy_text(body.clone());
                        }
                        if ui.button("Close").on_hover_text("Esc").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::Rename { doc: d, path, name } => {
                    let from = path.join(".");
                    dialog_title(ui, format!("Rename {from}"));
                    ui.add(egui::TextEdit::singleline(name).font(FontId::monospace(13.0)).desired_width(320.0));
                    let valid = !name.trim().is_empty() && !name.contains('.') && !name.starts_with('$') && Some(name.trim()) != path.last().map(|s| s.as_str());
                    let mut to = path[..path.len() - 1].to_vec();
                    to.push(name.trim().to_string());
                    dialog_actions(ui, |ui| {
                        if ui.add_enabled(valid, primary_button(ui, "In this document", PRIMARY())).clicked() {
                            let new = db::get_path(d, &from).cloned().ok_or_else(|| "Field not found".to_string()).and_then(|v| db::edit_path(&db::edit_path(d, path, None)?, &to, Some(v)));
                            let base = d.clone();
                            self.stage(&base, new);
                            keep = false;
                        }
                        // $rename cannot reach into arrays, so this is offered only for plain paths.
                        let plain = path.iter().all(|s| s.parse::<usize>().is_err());
                        let (count, filter) = self.t().map(|t| (t.count, t.query.filter.clone())).unwrap_or_default();
                        if plain && ui.add_enabled(valid, egui::Button::new(format!("In all {count} matching documents…"))).clicked() {
                            let coll = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
                            self.modal = Some(Modal::Typed {
                                title: format!("Rename {from} to {} in every document of {coll} matching the current filter (about {count})?", to.join(".")),
                                expect: coll,
                                typed: String::new(),
                                action: Danger::RenameMany { filter, from: from.clone(), to: to.join(".") },
                            });
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::UpdateMany { set_txt, unset_txt, preview } => {
                    ui.set_width(560.0);
                    let filter = self.t().map(|t| t.query.filter.clone()).unwrap_or_default();
                    dialog_title(ui, "Update matching documents");
                    ui.label(RichText::new("Applies to every document matching the current filter. Values starting with $ are read as field paths.").color(MUTED()));
                    ui.label(RichText::new("Set fields").color(FAINT()));
                    let id = ui.make_persistent_id("um-set");
                    let mut changed = with_autopair(ui, id, set_txt, |ui, text| egui::TextEdit::multiline(text).id(id).code_editor().desired_width(f32::INFINITY).desired_rows(4).layouter(&mut code_layout).show(ui)).response.changed();
                    ui.label(RichText::new("Remove fields (comma separated paths)").color(FAINT()));
                    changed |= ui.add(egui::TextEdit::singleline(unset_txt).font(FontId::monospace(13.0)).desired_width(f32::INFINITY)).changed();
                    if changed {
                        *preview = None;
                    }
                    let stages = db::parse_doc(set_txt).map(|set| db::update_stages(set, unset_txt.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()));
                    match &stages {
                        Err(err) => {
                            ui.label(RichText::new(err).color(ALERT()));
                        }
                        Ok(st) if st.is_empty() => {
                            ui.label(RichText::new("Nothing to change yet.").color(MUTED()));
                        }
                        Ok(_) => {}
                    }
                    let ready = stages.as_ref().is_ok_and(|s| !s.is_empty());
                    let fresh = matches!((preview.as_ref(), stages.as_ref()), (Some((p, _)), Ok(s)) if p == s);
                    match preview.as_ref().filter(|_| fresh).map(|(_, r)| r) {
                        Some(Err(err)) => {
                            ui.label(RichText::new(&*err).color(ALERT()));
                        }
                        Some(Ok((rows, n))) => {
                            ui.label(RichText::new(format!("{} documents match. Showing {} of them after the change.", group(&n.to_string()), rows.len())).color(MUTED()));
                            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                                for (i, (old, new)) in rows.iter().enumerate() {
                                    ui.label(RichText::new(old.get("_id").map(db::short).unwrap_or_default()).monospace().color(BRIGHT()));
                                    egui::Grid::new(("um-chg", i)).num_columns(3).striped(true).spacing([14.0, 6.0]).show(ui, |ui| {
                                        for (path, a, b) in db::diff(old, new) {
                                            let show = |v: &Option<Bson>| v.as_ref().map(|v| format!("{} ({})", leaf_text(v), db::type_name(v))).unwrap_or("(none)".into());
                                            ui.label(RichText::new(path).monospace().color(KEY()));
                                            ui.label(RichText::new(show(&a)).monospace().color(ALERT()).strikethrough());
                                            ui.label(RichText::new(show(&b)).monospace().color(SUCCESS()));
                                            ui.end_row();
                                        }
                                    });
                                }
                            });
                        }
                        None => {}
                    }
                    dialog_actions(ui, |ui| {
                        if ui.add_enabled(ready, egui::Button::new("Preview")).clicked() {
                            if let (Ok(st), Some(t)) = (&stages, self.target()) {
                                self.worker.update_preview(t, filter.clone(), st.clone());
                            }
                        }
                        let applicable = ready && fresh && matches!(preview, Some((_, Ok(_))));
                        if ui.add_enabled(applicable, primary_button(ui, "Apply…", ALERT())).clicked() {
                            let coll = self.t().map(|t| t.sel.coll.clone()).unwrap_or_default();
                            let n = if let Some((_, Ok((_, n)))) = preview { *n } else { 0 };
                            self.modal = Some(Modal::Typed {
                                title: format!("Update every document in {coll} matching the current filter ({n})?"),
                                expect: coll,
                                typed: String::new(),
                                action: Danger::UpdateMany { filter, stages: stages.unwrap_or_default() },
                            });
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::Undo => {
                    dialog_title(ui, "Recent changes");
                    ui.label(RichText::new(format!("Last {} actions from the past 24 hours.", db::UNDO_MAX)).color(MUTED()));
                    let mut pick = None;
                    egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                        for (i, a) in self.undo.iter().enumerate() {
                            ui.horizontal(|ui| {
                                let what = match &a.kind {
                                    UndoKind::Insert { id } => format!("inserted {}", db::short(id)),
                                    UndoKind::InsertMany { ids } => format!("imported {} documents", ids.len()),
                                    UndoKind::Delete { old } => format!("deleted {}", old.get("_id").map(db::short).unwrap_or_default()),
                                    UndoKind::DeleteMany { old } => format!("deleted {} documents", old.len()),
                                    UndoKind::Update { old, .. } => format!("edited {}", old.get("_id").map(db::short).unwrap_or_default()),
                                };
                                let mins = a.at.elapsed().as_secs() / 60;
                                ui.label(RichText::new(format!("{}.{}  {what}", a.db, a.coll)).monospace());
                                ui.label(RichText::new(format!("{mins} min ago")).color(MUTED()));
                                if ui.small_button("Undo").clicked() {
                                    pick = Some(i);
                                }
                            });
                        }
                    });
                    if let Some(i) = pick {
                        self.apply_undo(i);
                    }
                    if dialog_actions(ui, |ui| ui.button("Close").clicked()) {
                        keep = false;
                    }
                }
                Modal::Explain { summary, raw, show_raw } => {
                    ui.set_width(720.0);
                    dialog_title(ui, "Explain");
                    ui.label(RichText::new(format!("{} returned  ·  {} keys examined  ·  {} documents examined  ·  {} ms", group(&summary.returned.to_string()), group(&summary.keys.to_string()), group(&summary.docs.to_string()), summary.ms)).color(MUTED()));
                    for w in &summary.warnings {
                        ui.label(RichText::new(w.as_str()).color(WARNING()));
                    }
                    ui.horizontal(|ui| {
                        segmented(ui, show_raw, &[(false, "Stages"), (true, "Raw JSON")]);
                    });
                    let find = if *show_raw { Find::bar(ui, egui::Id::unique("explain-raw"), raw) } else { Find::default() };
                    egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                        if *show_raw {
                            let mut text = raw.as_str();
                            let out = egui::TextEdit::multiline(&mut text).code_editor().desired_width(f32::INFINITY).layouter(&mut find.layouter(true)).show(ui);
                            find.reveal(ui, raw, &out);
                        } else if summary.rows.is_empty() {
                            ui.label(RichText::new("The server returned no execution stages. See Raw JSON.").color(MUTED()));
                        } else {
                            egui::Grid::new("explain-stages").num_columns(6).spacing([16.0, 4.0]).striped(true).show(ui, |ui| {
                                for h in ["STAGE", "INDEX", "KEYS", "DOCS", "RETURNED", "MS"] {
                                    ui.label(RichText::new(h).size(11.0).color(MUTED()));
                                }
                                ui.end_row();
                                let n = |v: Option<i64>| v.map(|v| group(&v.to_string())).unwrap_or_default();
                                for r in &summary.rows {
                                    let c = match r.stage.to_lowercase().as_str() {
                                        "ixscan" | "ixseek" => SUCCESS(),
                                        "collscan" | "scan" => ALERT(),
                                        _ => TEXT(),
                                    };
                                    ui.label(RichText::new(format!("{}{}", "  ".repeat(r.depth), r.stage)).monospace().color(c));
                                    ui.label(RichText::new(r.index.as_str()).monospace().color(MUTED()));
                                    for v in [r.keys, r.docs, r.returned, r.ms] {
                                        ui.label(RichText::new(n(v)).monospace());
                                    }
                                    ui.end_row();
                                }
                            });
                        }
                    });
                    dialog_actions(ui, |ui| {
                        if ui.button("Copy raw").clicked() {
                            ui.ctx().copy_text(raw.clone());
                        }
                        if ui.button("Close").on_hover_text("Esc").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::Palette { query, sel } => {
                    ui.set_width(560.0);
                    // Taken before the text field sees them, so arrows move the selection and Enter picks.
                    let (up, down, enter) = ui.input_mut(|i| (i.consume_key(egui::Modifiers::NONE, Key::ArrowUp), i.consume_key(egui::Modifiers::NONE, Key::ArrowDown), i.consume_key(egui::Modifiers::NONE, Key::Enter)));
                    let r = ui.add(egui::TextEdit::singleline(query).hint_text("Jump to a connection, database or collection").desired_width(f32::INFINITY).margin(egui::Margin::symmetric(10, 8)));
                    r.request_focus();
                    if r.changed() {
                        *sel = 0;
                    }
                    let items = jumps(&self.conns, self.active, query);
                    if down {
                        *sel = (*sel + 1).min(items.len().saturating_sub(1));
                    }
                    if up {
                        *sel = sel.saturating_sub(1);
                    }
                    let mut pick = if enter { items.get(*sel).map(|x| x.2.clone()) } else { None };
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                        if items.is_empty() {
                            ui.label(RichText::new("No matches").color(MUTED()));
                        }
                        for (i, (label, hint, j)) in items.iter().enumerate() {
                            let r = ui
                                .horizontal(|ui| {
                                    let r = ui.selectable_label(i == *sel, RichText::new(label).color(BRIGHT()));
                                    ui.label(RichText::new(hint).size(12.0).color(FAINT()));
                                    r
                                })
                                .inner;
                            if i == *sel && (up || down) {
                                r.scroll_to_me(None);
                            }
                            if r.clicked() {
                                pick = Some(j.clone());
                            }
                        }
                    });
                    if let Some(j) = pick {
                        keep = false;
                        self.jump(j);
                    }
                }
            }
        });
        if keep && !r.should_close() && self.modal.is_none() {
            self.modal = Some(m);
        }
    }

    fn danger(&mut self, action: Danger) {
        if let Danger::RemoveConnection(id) = action {
            if let Some(k) = keychain(id) {
                let _ = k.delete_credential();
            }
            // ponytail: best effort, a failure only leaves an orphaned Keychain entry behind.
            for what in ["ssh-password", "ssh-passphrase", "proxy-password"] {
                let _ = set_secret(id, what, "");
            }
            self.conns.retain(|c| c.meta.id != id);
            self.tabs.retain(|t| t.sel.conn != id);
            self.cur = self.cur.min(self.tabs.len().saturating_sub(1));
            if self.active == Some(id) {
                self.active = self.conns.first().map(|c| c.meta.id);
            }
            self.save_conns();
            return self.ok("Connection removed");
        }
        let conn_specific = match &action {
            Danger::DropDatabase { conn, .. } | Danger::DropCollAt { conn, .. } | Danger::DropUser { conn, .. } | Danger::Sync { conn } | Danger::Restore { conn, .. } => Some(*conn),
            _ => None,
        };
        if let Some(conn) = conn_specific {
            let Some(client) = self.conn(conn).filter(|c| c.writable).and_then(|c| c.client.clone()) else {
                return self.modal = Some(Modal::Unlock(conn, None));
            };
            match action {
                Danger::DropDatabase { db, .. } => {
                    self.tabs.retain(|t| !(t.sel.conn == conn && t.sel.db == db));
                    self.cur = self.cur.min(self.tabs.len().saturating_sub(1));
                    self.worker.db_change(conn, client, db.clone(), doc! { "dropDatabase": 1 }, format!("Dropped database {db}"));
                    if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
                        c.dbs.retain(|d| d.0 != db);
                    }
                    self.dropping_db = Some((conn, db));
                }
                Danger::DropCollAt { db, coll, .. } => {
                    self.tabs.retain(|t| !(t.sel.conn == conn && t.sel.db == db && t.sel.coll == coll));
                    self.cur = self.cur.min(self.tabs.len().saturating_sub(1));
                    self.worker.db_change(conn, client, db, doc! { "drop": &coll }, format!("Dropped {coll}"));
                }
                Danger::DropUser { db, user, .. } => self.worker.db_change(conn, client, db, doc! { "dropUser": &user }, format!("Dropped user {user}")),
                Danger::Sync { .. } => {
                    let timeout = self.timeout();
                    let c = &mut self.tabs[self.cur].compare;
                    let Some(r) = c.result.take() else { return };
                    let insert = if c.insert { r.only_src } else { Vec::new() };
                    let replace = if c.replace { r.changed } else { Vec::new() };
                    let delete = if c.delete { r.only_dst.iter().filter_map(|d| d.get("_id").cloned()).collect() } else { Vec::new() };
                    self.worker.sync(Target { client, db: c.db.clone(), coll: c.coll.clone(), timeout }, insert, replace, delete);
                    self.ok("Syncing… Run Compare again afterwards to check.");
                }
                Danger::Restore { args, .. } => self.start_tool(true, "Restore".into(), args),
                _ => unreachable!(),
            }
            return;
        }
        let Some((conn, target)) = self.writable_target() else { return };
        match action {
            Danger::DeleteMatching(f) => self.worker.delete_many(target, f),
            Danger::DropIndex(name) => self.worker.drop_index(target, name),
            Danger::Validator(cmd) => self.worker.db_change(conn, target.client, target.db, cmd, "Validator updated".into()),
            Danger::DropCollection => {
                // The sidebar's drop also reloads the collection list, which keeps a refused drop visible.
                let Sel { conn, db, coll } = self.t().unwrap().sel.clone();
                self.danger(Danger::DropCollAt { conn, db, coll });
            }
            Danger::RenameMany { filter, from, to } => self.worker.rename_many(target, filter, from, to),
            Danger::UpdateMany { filter, stages } => self.worker.update_many(target, filter, stages),
            Danger::Upsert { key } => {
                let conn = self.t().unwrap().sel.conn;
                if let Some(Ok(docs)) = self.tabs[self.cur].import.parsed.clone() {
                    self.worker.import(conn, target, docs, Some(key));
                }
            }
            Danger::RemoveConnection(_) | Danger::DropDatabase { .. } | Danger::DropCollAt { .. } | Danger::DropUser { .. } | Danger::Sync { .. } | Danger::Restore { .. } => unreachable!(),
        }
    }
}

fn human_bytes(n: usize) -> String {
    match n {
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.0} KB", n as f64 / 1024.0),
        n => format!("{n} bytes"),
    }
}

fn group(n: &str) -> String {
    let mut out = String::new();
    for (i, ch) in n.chars().enumerate() {
        if i > 0 && (n.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_jumps() {
        let conn = |id, name: &str, status, dbs| Conn { meta: ConnMeta { id, name: name.into(), ..Default::default() }, client: None, status, writable: false, dbs, masked_uri: String::new(), tunnel: None };
        let conns = [conn(1, "prod", Status::Ready, vec![("shop".into(), Some(vec!["users".into(), "orders".into()]))]), conn(2, "staging", Status::Idle, Vec::new())];
        let labels = |active, q| jumps(&conns, active, q).into_iter().map(|j| j.0).collect::<Vec<_>>();
        assert_eq!(labels(Some(2), ""), ["staging", "prod", "shop", "shop.users", "shop.orders"]);
        assert_eq!(labels(Some(1), "users"), ["shop.users"]);
        // "or" fuzzily matches shop.users too, but the substring match ranks first.
        assert_eq!(labels(Some(1), "or"), ["shop.orders", "shop.users"]);
        assert_eq!(labels(Some(1), "prod users")[0], "shop.users");
        assert_eq!(labels(Some(1), "shop")[0], "shop");

        let conns = [conn(1, "local", Status::Ready, vec![("thetreep-api-main".into(), Some(vec!["travels".into()])), ("thetreep-api-vairon".into(), Some(vec!["travels".into(), "users".into()]))])];
        let labels = |q| jumps(&conns, Some(1), q).into_iter().map(|j| j.0).collect::<Vec<_>>();
        assert_eq!(labels("vairon travels"), ["thetreep-api-vairon.travels"]);
        assert_eq!(labels("travels")[..2], ["thetreep-api-main.travels", "thetreep-api-vairon.travels"]);
    }

    #[test]
    fn autocomplete_contexts() {
        let paths: Vec<String> = ["status", "customer", "customer.name", "customer.tier", "total"].iter().map(|s| s.to_string()).collect();
        let (n, s) = suggest("{ cus", FieldKind::Query, &paths, &[]).unwrap();
        assert_eq!((n, s[0].text.as_str(), s[0].insert.as_str()), (3, "customer", "customer: "));
        let (_, s) = suggest("{ customer.n", FieldKind::Query, &paths, &[]).unwrap();
        assert_eq!(s[0].text, "customer.name");
        let (_, s) = suggest("{ total: { $g", FieldKind::Query, &paths, &[]).unwrap();
        assert!(s.iter().any(|x| x.text == "$gt:"));
        assert!(s[0].text.starts_with("$g"));
        let (_, s) = suggest("{ status: { $i", FieldKind::Query, &paths, &[]).unwrap();
        let i = s.iter().find(|x| x.text == "$in: [  ]").unwrap();
        assert_eq!(&i.insert[..i.insert.char_indices().nth(i.caret).unwrap().0], "$in: [ ");
        let (_, s) = suggest("{ total: ", FieldKind::Sort, &paths, &[]).unwrap();
        assert_eq!(s.len(), 2);
        let (_, s) = suggest("{ _id: Obj", FieldKind::Query, &paths, &[]).unwrap();
        assert_eq!((s[0].text.as_str(), s[0].caret), ("ObjectId(\"\")", 10));
        assert!(suggest("{ ", FieldKind::Query, &paths, &[]).is_none());
        let (_, s) = suggest("{ \"cus", FieldKind::Query, &paths, &[]).unwrap();
        assert_eq!(s[0].insert, "customer");
        let (n, s) = suggest("{ at: #last2m", FieldKind::Query, &paths, &[]).unwrap();
        assert_eq!((n, s[0].text.as_str()), (7, "#last2months"));
    }

    #[test]
    fn shell_autocomplete() {
        let colls = vec!["orders".to_string(), "users".to_string()];
        let paths = vec!["status".to_string(), "total".to_string()];
        let (_, s) = suggest_js("return await db.coll", &paths, &colls, &[]).unwrap();
        assert_eq!((s[0].text.as_str(), s[0].insert.as_str()), ("collection", "collection()"));
        let (_, s) = suggest_js("await orders.upd", &paths, &colls, &[]).unwrap();
        assert!(s[0].text.starts_with("update"));
        let (_, s) = suggest_js("await orders.find({}).", &paths, &colls, &[]).unwrap();
        assert!(s.iter().any(|x| x.text == "toArray"));
        let (_, s) = suggest_js("db.collection('x').", &paths, &colls, &[]).unwrap();
        assert!(s.iter().any(|x| x.text == "find"));
        let (_, s) = suggest_js("await orders.find({ sta", &paths, &colls, &[]).unwrap();
        assert_eq!(s[0].text, "status");
        let (_, s) = suggest_js("  ord", &paths, &colls, &[]).unwrap();
        assert_eq!(s[0].text, "orders");
        let (_, s) = suggest_js("  aw", &paths, &colls, &[]).unwrap();
        assert_eq!(s[0].insert, "await ");
    }

    #[test]
    fn js_colours() {
        let job = js_job("const x = await orders.find({}); // hi");
        let colour_of = |w: &str| {
            let at = job.text.find(w).unwrap();
            job.sections.iter().find(|s| s.byte_range.start.0 <= at && at < s.byte_range.end.0).unwrap().format.color
        };
        assert_eq!(colour_of("const"), C_KEYWORD());
        assert_eq!(colour_of("find"), C_DATE());
        assert_eq!(colour_of("// hi"), FAINT());
    }

    #[test]
    fn big_objects_show_their_size() {
        let small = Bson::Document(doc! { "name": "Ada", "tier": "gold" });
        assert_eq!(column_job(&small, true).text, "{name: \"Ada\", tier: \"gold\"}");
        let big = Bson::Document((0..40).map(|i| (format!("field{i}"), Bson::Int32(i))).collect());
        assert_eq!(column_job(&big, true).text, "{40 fields}");
        assert!(column_job(&big, false).text.starts_with("{field0: 0"));
        let list = Bson::Array((0..500).map(Bson::Int32).collect());
        assert_eq!(column_job(&list, true).text, "[500 items]");
    }

    #[test]
    fn date_field_suggestions() {
        let paths = vec!["status".to_string(), "createdAt".to_string(), "format".to_string()];
        let dates = vec!["createdAt".to_string()];
        // A date field lists date shortcuts right after the colon, with nothing typed yet.
        let (n, s) = suggest("{ createdAt: ", FieldKind::Query, &paths, &dates).unwrap();
        assert_eq!((n, s[0].text.as_str()), (0, "#today"));
        assert!(s.iter().any(|x| x.note == "between two days, both included"));
        // Fields not loaded yet are judged by their name.
        assert!(suggest("{ \"providerOrder.syncedAt\": ", FieldKind::Query, &paths, &dates).is_some());
        assert!(suggest("{ status: ", FieldKind::Query, &paths, &dates).is_none());
        assert!(suggest("{ format: ", FieldKind::Query, &paths, &dates).is_none());
        let (_, s) = suggest("{ createdAt: #last", FieldKind::Query, &paths, &dates).unwrap();
        assert!(s.iter().all(|x| x.text.to_ascii_lowercase().contains("last")));
        assert!(date_like("startDate") && date_like("syncedAt") && date_like("timestamp") && !date_like("status"));
    }

    #[test]
    fn pipeline_builder_text() {
        let mut st = stages_from_text("[{ $match: { a: 1 } }, { $limit: 5 }, { $project: { a: 1 } }]").unwrap();
        assert_eq!(st.iter().map(|s| s.op.as_str()).collect::<Vec<_>>(), ["$match", "$limit", "$project"]);
        st[1].on = false;
        // Stages that are off are left out of the text and of every preview pipeline.
        let text = text_from_stages(&st);
        assert_eq!(db::parse_pipeline(&text).unwrap().len(), 2);
        assert_eq!(stage_pipeline(&st, 2).unwrap(), vec![doc! { "$match": { "a": 1 } }, doc! { "$project": { "a": 1 } }]);
        st[0].text = "{ a: ".into();
        assert!(stage_pipeline(&st, 2).unwrap_err().starts_with("Stage 1 ($match)"));
        assert!(stages_from_text("[{ $match: {}, $limit: 1 }]").is_err());
        assert_eq!(text_from_stages(&[]), "[]");
    }

    #[test]
    fn relation_files() {
        let list = vec![Relation { coll: "orders".into(), path: "items.productId".into(), target: "products".into(), preview: "title".into() }];
        assert!(relations_import(&relations_export("shop", &list)).unwrap() == list);
        let mingo = r#"{"version":"1.0","database":"shop","relations":[{"collection":"orders","path":"items[].productId","referencedCollection":"products","previewField":"title"}]}"#;
        assert!(relations_import(mingo).unwrap() == list);
        assert!(relations_import(r#"{"relations":[]}"#).is_err());
    }

    #[test]
    fn package_names() {
        assert_eq!(package_ident("lodash"), "lodash");
        assert_eq!(package_ident("date-fns@3.6.0"), "dateFns");
        assert_eq!(package_ident("@faker-js/faker"), "faker");
        assert_eq!(package_ident("@scope/x-y@^1"), "xY");
        let (_, s) = suggest_js("  dateF", &[], &[], &["dateFns".into()]).unwrap();
        assert_eq!((s[0].text.as_str(), s[0].note), ("dateFns", "package"));
    }

    #[test]
    fn roles() {
        let r = parse_roles("readWrite@shop, read ,", "admin");
        assert_eq!(r.len(), 2);
        assert_eq!(r[1].as_document().unwrap().get_str("db").unwrap(), "admin");
        let u = doc! { "roles": [ { "role": "read", "db": "x" } ] };
        assert_eq!(roles_text(&u), "read@x");
    }

    #[test]
    fn query_history() {
        let mut h = Vec::new();
        for v in ["{ a: 1 }", "{}", " ", "{ b: 2 }", "{ a: 1 }"] {
            push_history(&mut h, v);
        }
        assert_eq!(h, ["{ a: 1 }", "{ b: 2 }"]);
        for i in 0..40 {
            push_history(&mut h, &format!("{{ n: {i} }}"));
        }
        assert_eq!(h.len(), HISTORY_LENGTH_BY_COLLECTION);
    }

    #[test]
    fn broken_config_is_moved_aside() {
        let dir = std::env::temp_dir().join(format!("quarry-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("connections.json");
        std::fs::write(&p, "[{").unwrap();
        let (v, err): (Vec<ConnMeta>, _) = parse_config(&p);
        assert!(v.is_empty() && err.is_some());
        assert!(!p.exists() && dir.join("connections.json.bak").exists());
        let (_, err): (Vec<ConnMeta>, _) = parse_config(&p);
        assert!(err.is_none(), "a missing file is a fresh install, not an error");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn auto_indent() {
        // Enter between {} : closer on its own line, caret indented one step.
        let t = "  run({\n})";
        let (out, c) = newline_indent(t, 8).unwrap();
        assert_eq!(out, "  run({\n    \n  })");
        assert_eq!(c, 12);
        // Plain line keeps its indent.
        let (out, c) = newline_indent("    a: 1,\n", 10).unwrap();
        assert_eq!((out.as_str(), c), ("    a: 1,\n    ", 14));
        assert!(newline_indent("a\n", 2).is_none());
        // Closer typed on an empty indented line steps back.
        let (out, c) = closer_dedent("{\n    }", 7).unwrap();
        assert_eq!((out.as_str(), c), ("{\n  }", 5));
        assert!(closer_dedent("{ a }", 5).is_none());
    }

    #[test]
    fn shell_scripts() {
        assert_eq!(query_script("orders", "{ a: 1 }"), "async function run() {\n\treturn await orders.find({ a: 1 })\n}");
        assert!(query_script("my.coll", "").contains("db.collection(\"my.coll\").find({})"));
        assert!(query_script("x", "db.x.find({});").ends_with("return await db.x.find({})\n}"));
        assert_eq!(dangerous_words("db.x.deleteMany({})"), ["delete"]);
    }

    #[test]
    fn string_display_truncates_like_lodash() {
        assert_eq!(truncate_display(&"a".repeat(100)).len(), 100);
        let t = truncate_display(&"a".repeat(150));
        assert_eq!((t.len(), t.ends_with("...")), (100, true));
    }

    #[test]
    fn columns_default_pin_and_move() {
        let fields: Vec<String> = (0..12).map(|i| format!("f{i}")).collect();
        assert_eq!(visible_cols(None, &fields, DEFAULT_COLS).len(), DEFAULT_COLS);
        let p = ColPrefs { pinned: vec!["f3".into()], ..Default::default() };
        assert_eq!(visible_cols(Some(&p), &fields, DEFAULT_COLS)[0], "f3");
        let p = ColPrefs { shown: Some(vec!["a".into(), "b".into()]), pinned: vec!["b".into(), "zz".into()], ..Default::default() };
        assert_eq!(visible_cols(Some(&p), &fields, DEFAULT_COLS), ["b", "a"]);
    }
}
