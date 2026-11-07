//! Code text areas: JSON and JS highlighting, self-closing brackets with auto-indent, and the Cmd+F bar.

use super::complete::{JS_GLOBALS, JS_KEYWORDS};
use super::theme::*;
use eframe::egui::{self, Color32, FontId, Key, RichText, TextFormat, text::LayoutJob};
use std::sync::Arc;

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

pub(crate) fn js_layout(ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32) -> Arc<egui::Galley> {
    let mut job = js_job(buf.as_str());
    job.wrap.max_width = wrap;
    ui.fonts_mut(|f| f.layout_job(job))
}

pub(crate) fn code_layout(ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32) -> Arc<egui::Galley> {
    let mut job = json_job(buf.as_str());
    job.wrap.max_width = wrap;
    ui.fonts_mut(|f| f.layout_job(job))
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
