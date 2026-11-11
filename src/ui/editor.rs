//! Code text areas: JSON and JS highlighting, self-closing brackets with auto-indent, and the Cmd+F bar.

use super::complete::{JS_GLOBALS, JS_KEYWORDS, open_quote};
use super::theme::*;
use eframe::egui::{self, Color32, FontId, Key, RichText, TextFormat, text::LayoutJob};
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

/// Types the keyword list does not hold, colored like keywords in a CREATE statement.
const SQL_TYPES: &[&str] = &["TINYINT", "SMALLINT", "MEDIUMINT", "INT", "INTEGER", "BIGINT", "DECIMAL", "NUMERIC", "FLOAT", "DOUBLE", "BIT", "CHAR", "VARCHAR", "BINARY", "VARBINARY", "TINYTEXT", "TEXT", "MEDIUMTEXT", "LONGTEXT", "TINYBLOB", "BLOB", "MEDIUMBLOB", "LONGBLOB", "ENUM", "SET", "DATE", "TIME", "DATETIME", "TIMESTAMP", "YEAR", "JSON", "BOOLEAN", "BOOL", "CURRENT_TIMESTAMP", "ENGINE", "CHARSET"];

/// Every SQL keyword and type, lowercase, so a word is looked up without scanning the lists.
fn sql_words() -> &'static HashSet<String> {
    static WORDS: OnceLock<HashSet<String>> = OnceLock::new();
    WORDS.get_or_init(|| crate::db::sql::KEYWORDS.iter().chain(SQL_TYPES).map(|w| w.to_ascii_lowercase()).collect())
}

fn is_sql_word(w: &str) -> bool {
    let mut buf = [0u8; 32];
    let Some(dst) = buf.get_mut(..w.len()) else { return false };
    dst.copy_from_slice(w.as_bytes());
    dst.make_ascii_lowercase();
    std::str::from_utf8(dst).is_ok_and(|l| sql_words().contains(l))
}

/// SQL text colored by token: keywords, types, numbers, strings and comments, in mono at `size`.
pub(crate) fn sql_job(text: &str, size: f32) -> LayoutJob {
    sql_job_in(text, size, crate::db::sql::Dialect::default())
}

/// Like `sql_job`, with the strings and comments of that dialect: dollar quotes on PostgreSQL, `#` comments on MySQL.
pub(crate) fn sql_job_in(text: &str, size: f32, d: crate::db::sql::Dialect) -> LayoutJob {
    use crate::db::sql::lex::{self, Tok};
    let mut job = LayoutJob::default();
    for (tok, r) in lex::tokens(text, d) {
        let s = &text[r];
        let color = match tok {
            Tok::Word if is_sql_word(s) => C_KEYWORD(),
            Tok::Word if s.starts_with(|c: char| c.is_ascii_digit()) => C_NUMBER(),
            Tok::Str => C_STRING(),
            Tok::Comment => FAINT(),
            Tok::Punct => MUTED(),
            _ => TEXT(),
        };
        job.append(s, 0.0, mono_fmt(size, color, 400.0));
    }
    job
}

/// A job cut into one job per line. Strings and comments can span lines, so a highlighter colors the
/// whole text first and the cut comes after.
pub(crate) fn split_lines(job: &LayoutJob) -> Vec<LayoutJob> {
    let mut lines = vec![LayoutJob::default()];
    for sec in &job.sections {
        for (i, part) in job.text[sec.byte_range.start.0..sec.byte_range.end.0].split('\n').enumerate() {
            if i > 0 {
                lines.push(LayoutJob::default());
            }
            if !part.is_empty() {
                lines.last_mut().unwrap().append(part, 0.0, sec.format.clone());
            }
        }
    }
    lines
}

/// Colours JSON and shell-style query text by token, for every code editor in the app.
pub(crate) fn json_job(text: &str) -> LayoutJob {
    let fmt = |c: Color32| TextFormat { font_id: FontId::monospace(TXT_BODY), color: c, ..Default::default() };
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
pub(crate) fn with_autopair(ui: &mut egui::Ui, id: egui::Id, text: &mut String, show: impl FnOnce(&mut egui::Ui, &mut String) -> egui::text_edit::TextEditOutput) -> egui::text_edit::TextEditOutput {
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
    // Backspace between an empty pair removes both halves; the editor itself removes the opening one.
    if let Some(ci) = cursor.filter(|&ci| ci > 0 && focused && ui.input(|i| i.key_pressed(Key::Backspace) && i.modifiers.is_none())) {
        let mut around = text.chars().skip(ci - 1);
        if let (Some(a), Some(b)) = (around.next(), around.next())
            && ["()", "[]", "{}", "\"\"", "''"].iter().any(|p| p.starts_with(a) && p.ends_with(b))
        {
            text.remove(byte_at(text, ci));
        }
    }
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
        // A quote right after a word is more likely an apostrophe, and one typed inside a string ends
        // it (or is part of it), so neither gets a partner.
        let quote_ok = !matches!(close, '"' | '\'') || (prev.is_none_or(|p| !p.is_alphanumeric()) && !in_string(&text[..byte_at(text, ci.saturating_sub(1))]));
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

/// Whether the end of `before` is inside a quoted string on its last line, escapes followed.
fn in_string(before: &str) -> bool {
    open_quote(before.rsplit('\n').next().unwrap_or(before)).is_some()
}

/// One indent step; the same two spaces the pretty-printed JSON uses.
pub(crate) const INDENT: &str = "  ";

pub(crate) fn byte_at(text: &str, ci: usize) -> usize {
    text.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(text.len())
}

/// After a newline was typed just before char `ci`: the text with the right indentation added
/// and where the caret goes.
pub(crate) fn newline_indent(text: &str, ci: usize) -> Option<(String, usize)> {
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
pub(crate) fn closer_dedent(text: &str, ci: usize) -> Option<(String, usize)> {
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

/// The drag bar between an editor and the results under it: a 6 px hit area with a hairline that
/// darkens on hover and takes the accent while dragged. Returns the response for the caller to size with.
pub(crate) fn splitter(ui: &mut egui::Ui) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 6.0), egui::Sense::click_and_drag());
    if r.hovered() || r.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
    }
    let color = if r.dragged() { PRIMARY() } else if r.hovered() { BORDER() } else { LINE() };
    ui.painter().hline(rect.x_range(), rect.center().y, egui::Stroke::new(1.0, color));
    r
}

/// Sizes an editor with its `splitter`. `pref` is the saved height, 0 for sized to the text, and
/// `rendered` the height drawn this frame, which a drag starts from. Returns true when the drag
/// ended or was reset, which is when to save.
pub(crate) fn resize_editor(r: &egui::Response, pref: &mut f32, rendered: f32, lo: f32, hi: f32) -> bool {
    // The drawn height snaps to whole lines, so adding each frame's delta to it fights the pointer
    // and the bar jumps back. The height is taken from where the drag started instead.
    let id = r.id.with("drag-start");
    if r.drag_started() {
        let y = r.interact_pointer_pos().map_or(0.0, |p| p.y);
        r.ctx.data_mut(|d| d.insert_temp(id, (rendered, y)));
    }
    if r.dragged() {
        if let (Some((h0, y0)), Some(p)) = (r.ctx.data(|d| d.get_temp::<(f32, f32)>(id)), r.interact_pointer_pos()) {
            *pref = (h0 + p.y - y0).clamp(lo, hi.max(lo));
        }
    }
    if r.double_clicked() {
        *pref = 0.0;
    }
    r.drag_stopped() || r.double_clicked()
}

/// A SQL template in a script: the byte range of its text between the quotes, and the byte ranges of
/// its `${...}` parts, delimiters included. An unclosed template runs to the end, as while typing.
#[derive(Debug, PartialEq)]
pub(crate) struct SqlTpl {
    pub(crate) range: std::ops::Range<usize>,
    pub(crate) holes: Vec<std::ops::Range<usize>>,
}

enum Fr {
    /// Code, with the depth of `{` opened inside it. A `${` part starts one.
    Code(usize),
    Tpl { tagged: bool, start: usize, holes: Vec<std::ops::Range<usize>>, hole: usize },
}

/// Whether the quote at the end of `before` opens SQL: sql`...` or the string of sql.unsafe(...).
fn sql_tag(before: &str, quote: u8) -> bool {
    let b = before.trim_end();
    // A longer name or a member access like db.sql is not the tag.
    let at = |w: &str| b.strip_suffix(w).is_some_and(|h| !h.ends_with(|c: char| c.is_alphanumeric() || matches!(c, '_' | '$' | '.')));
    at("sql.unsafe(") || (quote == b'`' && at("sql"))
}

/// Every SQL template of a script. The scan follows JS well enough that comments, strings, escaped
/// quotes and templates nested in `${ }` parts do not fool it.
pub(crate) fn sql_templates(code: &str) -> Vec<SqlTpl> {
    let b = code.as_bytes();
    let (mut out, mut st, mut i) = (Vec::new(), vec![Fr::Code(0)], 0);
    while i < b.len() {
        if matches!(st.last(), Some(Fr::Tpl { .. })) {
            match b[i] {
                b'\\' => i += 2,
                b'`' => {
                    if let Some(Fr::Tpl { tagged: true, start, holes, .. }) = st.pop() {
                        out.push(SqlTpl { range: start..i, holes });
                    }
                    i += 1;
                }
                b'$' if b.get(i + 1) == Some(&b'{') => {
                    if let Some(Fr::Tpl { hole, .. }) = st.last_mut() {
                        *hole = i;
                    }
                    st.push(Fr::Code(0));
                    i += 2;
                }
                _ => i += 1,
            }
            continue;
        }
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => i += code[i..].find('\n').unwrap_or(b.len() - i),
            b'/' if b.get(i + 1) == Some(&b'*') => i += code[i + 2..].find("*/").map_or(b.len() - i, |e| e + 4),
            q @ (b'"' | b'\'') => {
                let (tagged, start) = (sql_tag(&code[..i], q), i + 1);
                i = start;
                while i < b.len() && b[i] != q && b[i] != b'\n' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                let end = i.min(b.len());
                if tagged {
                    out.push(SqlTpl { range: start..end, holes: Vec::new() });
                }
                i = end + (end < b.len() && b[end] == q) as usize;
            }
            b'`' => {
                st.push(Fr::Tpl { tagged: sql_tag(&code[..i], b'`'), start: i + 1, holes: Vec::new(), hole: 0 });
                i += 1;
            }
            b'{' => {
                if let Some(Fr::Code(d)) = st.last_mut() {
                    *d += 1;
                }
                i += 1;
            }
            b'}' => {
                let nested = matches!(st.last(), Some(Fr::Code(d)) if *d > 0);
                if nested {
                    if let Some(Fr::Code(d)) = st.last_mut() {
                        *d -= 1;
                    }
                } else if st.len() > 1 {
                    // The close of a `${` part.
                    st.pop();
                    if let Some(Fr::Tpl { holes, hole, .. }) = st.last_mut() {
                        holes.push(*hole..i + 1);
                    }
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    // Open templates while typing. A part still open reaches one past the end, so a caret at the end is inside it.
    let open: Vec<bool> = (0..st.len()).map(|k| matches!(st.get(k + 1), Some(Fr::Code(_)))).collect();
    for (k, f) in st.into_iter().enumerate() {
        if let Fr::Tpl { tagged: true, start, mut holes, hole } = f {
            if open[k] {
                holes.push(hole..b.len() + 1);
            }
            out.push(SqlTpl { range: start.min(b.len())..b.len(), holes });
        }
    }
    out.sort_by_key(|t| t.range.start);
    out
}

/// The innermost SQL template the caret (a byte offset) is in, none while it is inside a `${ }` part.
pub(crate) fn sql_at(code: &str, caret: usize) -> Option<SqlTpl> {
    sql_templates(code).into_iter().filter(|t| t.range.start <= caret && caret <= t.range.end && !t.holes.iter().any(|h| h.start < caret && caret < h.end)).min_by_key(|t| t.range.len())
}

/// A template's text as one SQL statement text: up to the caret, and whole. Each `${ }` part becomes
/// a `?`, which the SQL completer reads as a value and which cannot run into a neighbouring word.
pub(crate) fn sql_text(code: &str, t: &SqlTpl, caret: usize) -> (String, String) {
    let (mut before, mut full, mut at) = (String::new(), String::new(), t.range.start);
    let text = |from: usize, to: usize, before: &mut String, full: &mut String| {
        full.push_str(&code[from..to]);
        if from < caret {
            before.push_str(&code[from..to.min(caret)]);
        }
    };
    for h in &t.holes {
        let (hs, he) = (h.start.max(at), h.end.min(t.range.end));
        text(at, hs, &mut before, &mut full);
        full.push('?');
        if he <= caret {
            before.push('?');
        }
        at = he;
    }
    text(at, t.range.end, &mut before, &mut full);
    (before, full)
}

/// JavaScript colouring for the Shell editor.
pub(crate) fn js_job(text: &str) -> LayoutJob {
    let fmt = |c: Color32| TextFormat { font_id: FontId::monospace(TXT_BODY), color: c, ..Default::default() };
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

pub(crate) fn code_layout(ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32) -> Arc<egui::Galley> {
    memo_galley(ui, 0, buf.as_str(), wrap, || json_job(buf.as_str()))
}

/// Lays out a highlighted job, keeping the last galleys by text, theme, wrap width and scale. A layouter runs
/// every frame and the highlighter lexes the whole text, so a still editor should not pay for that again.
/// `kind` tells apart highlighters that would color the same text differently.
pub(crate) fn memo_galley(ui: &egui::Ui, kind: u8, text: &str, wrap: f32, build: impl FnOnce() -> LayoutJob) -> Arc<egui::Galley> {
    use std::hash::{Hash, Hasher};
    thread_local! {
        static MEMO: std::cell::RefCell<std::collections::HashMap<u64, Arc<egui::Galley>>> = Default::default();
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (kind, is_light(), look_gen(), wrap.to_bits(), ui.ctx().pixels_per_point().to_bits()).hash(&mut h);
    text.hash(&mut h);
    let key = h.finish();
    if let Some(g) = MEMO.with(|m| m.borrow().get(&key).cloned()) {
        return g;
    }
    let mut job = build();
    job.wrap.max_width = wrap;
    let g = ui.fonts_mut(|f| f.layout_job(job));
    MEMO.with(|m| {
        let mut m = m.borrow_mut();
        // Small on purpose: only the editors on screen are re-asked each frame.
        if m.len() >= 24 {
            m.clear();
        }
        m.insert(key, g.clone());
    });
    g
}

/// Width the line numbers of a code editor take on the left.
pub(crate) const GUTTER_W: f32 = 52.0;

/// The text area frame of a code editor: room on the left for the line numbers.
pub(crate) fn gutter_frame() -> egui::Frame {
    egui::Frame::new().inner_margin(egui::Margin { left: GUTTER_W as i8, right: 12, top: 10, bottom: 10 })
}

/// Paints the line numbers of an editor shown with `gutter_frame`, one per source line, the caret line brighter.
pub(crate) fn paint_gutter(ui: &egui::Ui, out: &egui::text_edit::TextEditOutput, cur_line: Option<usize>) {
    let (gp, rect) = (out.galley_pos, out.response.rect);
    let (mut n, mut fresh) = (0, true);
    for row in &out.galley.rows {
        if fresh {
            n += 1;
            let y = gp.y + row.pos.y + row.height() / 2.0;
            ui.painter().text(egui::pos2(rect.left() + GUTTER_W - 14.0, y), egui::Align2::RIGHT_CENTER, n.to_string(), FontId::monospace(TXT_CAPTION), if Some(n) == cur_line { MUTED() } else { FAINT() });
        }
        fresh = row.ends_with_newline;
    }
}

/// Cmd+F for a big text area, kept in egui memory under the area's id.
#[derive(Clone, Default)]
pub(crate) struct Find {
    pub(crate) open: bool,
    pub(crate) query: String,
    pub(crate) cur: usize,
    pub(crate) scroll: bool,
    pub(crate) frame: u64,
}

impl Find {
    /// The bar above the text area. Enter and Shift+Enter step through the matches; Esc closes
    /// the bar here so it does not also close the window around it.
    pub(crate) fn bar(ui: &mut egui::Ui, id: egui::Id, text: &str) -> Find {
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
                if ui.button("Prev").on_hover_text("Shift+Enter").clicked() {
                    step(&mut f, true);
                }
                if ui.button("Next").on_hover_text("Enter").clicked() {
                    step(&mut f, false);
                }
                let label = if f.query.is_empty() { String::new() } else if n == 0 { "No matches".into() } else { format!("{} of {n}", f.cur + 1) };
                ui.label(RichText::new(label).size(TXT_SMALL).color(MUTED()));
            });
        }
        ui.data_mut(|d| d.insert_temp(key, f.clone()));
        f
    }

    pub(crate) fn hits(&self, text: &str) -> Vec<std::ops::Range<usize>> {
        if !self.open || self.query.is_empty() {
            return Vec::new();
        }
        // ponytail: ASCII case folding only, so byte offsets stay those of the original text.
        let q = self.query.to_ascii_lowercase();
        text.to_ascii_lowercase().match_indices(&q).map(|(i, _)| i..i + q.len()).collect()
    }

    /// Layouter for the text area: `code` colors JSON, matches get a background.
    pub(crate) fn layouter(&self, code: bool) -> impl FnMut(&egui::Ui, &dyn egui::TextBuffer, f32) -> Arc<egui::Galley> + '_ {
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
    pub(crate) fn mark(&self, job: &mut LayoutJob) {
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
    pub(crate) fn reveal(&self, ui: &egui::Ui, text: &str, out: &egui::text_edit::TextEditOutput) {
        if !self.scroll {
            return;
        }
        let Some(before) = self.hits(text).get(self.cur).and_then(|h| text.get(..h.start)) else { return };
        let r = out.galley.pos_from_cursor(egui::text::CCursor::new(before.chars().count()));
        ui.scroll_to_rect(r.translate(out.galley_pos.to_vec2()), Some(egui::Align::Center));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_inside_strings() {
        assert!(!in_string("{ a: "));
        assert!(in_string("{ a: \""));
        assert!(in_string("{ a: \"x\\\""));
        assert!(!in_string("{ a: \"x\", b: "));
        assert!(in_string("{ a: \"it's"));
        assert!(!in_string("x = \"a\nb: "));
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

    fn tpl_at(code: &str, marker: &str) -> Option<String> {
        let caret = code.find(marker).unwrap();
        let t = sql_at(code, caret)?;
        Some(sql_text(code, &t, caret).1)
    }

    #[test]
    fn finds_sql_templates() {
        let code = "const r = await sql`SELECT * FROM Tick WHERE id = ${id} AND x = ${ f(`a`) }`;";
        let t = sql_templates(code);
        assert_eq!(t.len(), 1);
        assert_eq!(&code[t[0].range.clone()], "SELECT * FROM Tick WHERE id = ${id} AND x = ${ f(`a`) }");
        assert_eq!(t[0].holes.len(), 2);
        assert_eq!(tpl_at(code, "FROM").unwrap(), "SELECT * FROM Tick WHERE id = ? AND x = ?");
        // Inside a part the caret is in JS.
        assert!(tpl_at(code, "id}").is_none());
        // Plain templates and other tags are not SQL.
        assert!(sql_templates("const a = `SELECT 1`; html`<b>`; mysql`x`; db.sql`x`").is_empty());
    }

    #[test]
    fn sql_template_scan_ignores_js_noise() {
        // A backtick in a comment or a string, and an escaped one in the template.
        let code = "// sql`no`\nconst s = \"sql`no`\"; /* sql`no` */ await sql`SELECT '\\`' FROM t`; x";
        let t = sql_templates(code);
        assert_eq!(t.len(), 1);
        assert_eq!(&code[t[0].range.clone()], "SELECT '\\`' FROM t");
        // A nested template with its own part, and braces of an object in a part.
        let code = "sql`A ${ sql`B ${y}` } C ${ {a: 1}.a } D`";
        let t = sql_templates(code);
        assert_eq!(t.len(), 2);
        assert_eq!(&code[t[0].range.clone()], "A ${ sql`B ${y}` } C ${ {a: 1}.a } D");
        assert_eq!(&code[t[1].range.clone()], "B ${y}");
        assert_eq!(tpl_at(code, "B ").unwrap(), "B ?");
        assert_eq!(tpl_at(code, " C").unwrap(), "A ? C ? D");
        // The string argument of sql.unsafe.
        assert_eq!(tpl_at("await sql.unsafe(\"SELECT 1 FROM t\")", "1 FROM").unwrap(), "SELECT 1 FROM t");
    }

    #[test]
    fn unclosed_template_while_typing() {
        let code = "await sql`SELECT * FROM T";
        let t = sql_at(code, code.len()).unwrap();
        assert_eq!(sql_text(code, &t, code.len()).0, "SELECT * FROM T");
        // Typing inside a part that is still open is JS.
        let code = "await sql`SELECT ${ a.";
        assert!(sql_at(code, code.len()).is_none());
        // Once closed, the template ends at its backtick and the code after it is JS.
        let code = "await sql`SELECT 1`; foo";
        assert!(sql_at(code, code.len()).is_none());
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
}
