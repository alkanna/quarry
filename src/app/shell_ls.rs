//! The shell editor's link to its language server: completions with docs, hover docs and
//! parameter hints. Every answer is matched to the newest request of its kind, so a late one
//! never lands on text that changed, a list that was dismissed, or another tab.

use super::*;
use crate::ui::js_doc::doc_popup;
use std::time::Instant;

/// Starts after one failed start or crash; past this many it stays off for the session.
const MAX_FAILS: u32 = 3;
/// How long the pointer rests on a word before its docs are asked for.
const HOVER_DELAY: Duration = Duration::from_millis(450);
/// CompletionItemKind for plain words from the file, which TypeScript offers when it knows no members.
const KIND_TEXT: u32 = 1;
/// Every function has these; on a callable package like dayjs they only push its own API down.
const FUNCTION_PROTO: [&str; 9] = ["apply", "bind", "call", "caller", "arguments", "length", "name", "prototype", "toString"];

#[derive(Default)]
pub(super) struct JsState {
    tx: Option<tokio::sync::mpsc::UnboundedSender<db::JsLsReq>>,
    fails: u32,
    /// Hash of the collections and packages whose declarations the server holds.
    globals: Option<u64>,
    /// Newest completion request.
    complete: Option<Pending>,
    /// Server items of the list on screen, in list order, to ask for the selected one's docs.
    raw: Vec<serde_json::Value>,
    /// Newest docs request for a list item: request and item index.
    resolve: Option<(u64, usize)>,
    sig: Option<Sig>,
    hover: Option<Hover>,
}

/// Parameter hints for the caret they were asked at.
struct Sig {
    caret: usize,
    req: Option<u64>,
    doc: Option<db::JsDoc>,
}

#[derive(Clone, Copy)]
struct Pending {
    req: u64,
    id: egui::Id,
    tab: usize,
    caret: usize,
    token: usize,
}

struct Hover {
    /// Character range of the word under the pointer.
    span: (usize, usize),
    since: Instant,
    pos: egui::Pos2,
    /// Asked once per word, even when the server is off.
    asked: bool,
    req: Option<u64>,
    doc: Option<db::JsDoc>,
}

/// What the shell editor reported this frame.
pub(super) struct EditorFrame {
    pub(super) id: egui::Id,
    /// The caret when the text changed this frame.
    pub(super) edited: Option<usize>,
    /// Esc or a picked suggestion: answers still on their way must not reopen the list.
    pub(super) esc: bool,
    pub(super) picked: bool,
    pub(super) focused: bool,
    pub(super) caret: Option<usize>,
    /// Screen position of the caret's top left, where parameter hints sit above.
    pub(super) caret_pos: Option<egui::Pos2>,
    /// Character under the pointer and the pointer position.
    pub(super) hover: Option<(usize, egui::Pos2)>,
}

/// The identifier around character `at`, as a character range.
fn word_span(code: &str, at: usize) -> Option<(usize, usize)> {
    let (byte, c) = code.char_indices().nth(at)?;
    if !is_js_ident(c) {
        return None;
    }
    let start = at - code[..byte].chars().rev().take_while(|c| is_js_ident(*c)).count();
    Some((start, at + code[byte..].chars().take_while(|c| is_js_ident(*c)).count()))
}

/// Hash of what the globals declarations are built from.
fn globals_key(colls: &[String], packages: &[String]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (colls, packages).hash(&mut h);
    h.finish()
}

fn kind_note(kind: u32) -> &'static str {
    match kind {
        2 => "method",
        3 => "function",
        4 => "constructor",
        5 | 10 => "property",
        6 => "variable",
        7 => "class",
        8 => "interface",
        9 => "module",
        13 => "enum",
        14 => "keyword",
        21 => "constant",
        _ => "symbol",
    }
}

impl App {
    /// Sends a request about character `at` of the current script, starting the server first if
    /// needed. Returns the request number, or None when the server is off or cannot start.
    fn js_send(&mut self, ask: db::JsAsk, at: usize, colls: &[String]) -> Option<u64> {
        if self.js.tx.as_ref().is_none_or(|tx| tx.is_closed()) {
            if self.js.fails >= MAX_FAILS {
                return None;
            }
            let runtime = find_bun(&self.prefs.shell_runtime)?;
            self.js.tx = Some(self.worker.js_ls(runtime, config_dir().join("shell"), self.prefs.shell_packages.clone()));
            self.js.globals = None;
        }
        let names: Vec<String> = self.prefs.shell_packages.iter().map(|p| db::package_spec(p).0).collect();
        let key = globals_key(colls, &names);
        // Rebuilt and sent only when the collections or packages changed.
        let globals = (self.js.globals != Some(key)).then(|| db::js_globals(colls, &names, package_ident, &JS_RESERVED));
        self.js.globals = Some(key);
        let code = self.tabs[self.cur].shell.code.clone();
        let (line, col) = db::lsp_pos(&code, at);
        self.req += 1;
        self.js.tx.as_ref()?.send(db::JsLsReq { req: self.req, ask, code, line, col, globals }).ok()?;
        Some(self.req)
    }

    /// The server stopped. Idle stops are normal; failures count toward turning it off.
    pub(super) fn js_stopped(&mut self, failed: bool) {
        if failed {
            self.js.fails += 1;
        }
    }

    /// Package changes need a fresh server, which installs them so their types load.
    pub(super) fn js_restart(&mut self) {
        self.js.tx = None;
    }

    /// Follows the editor each frame: asks for suggestions and hints after edits, docs for the
    /// selected suggestion and the word under the pointer, and draws hints and hover docs.
    pub(super) fn js_editor(&mut self, ctx: &egui::Context, f: EditorFrame, colls: &[String]) {
        if f.esc || f.picked {
            self.js.complete = None;
        }
        // Closed by Esc, leaving the editor, or the caret moving away without typing.
        if f.esc || !f.focused || self.js.sig.as_ref().is_some_and(|s| Some(s.caret) != f.caret) {
            self.js.sig = None;
        }
        if let Some(caret) = f.edited {
            self.js.hover = None;
            self.js_ask(f.id, caret, colls);
        }
        self.js_resolve(f.id, colls);
        self.js_hover(ctx, f.hover, colls);
        if let (Some(Sig { doc: Some(d), .. }), Some(pos)) = (&self.js.sig, f.caret_pos) {
            doc_popup(ctx, f.id.with("sig"), pos - egui::vec2(6.0, 4.0), egui::Align2::LEFT_BOTTOM, d);
        }
    }

    /// After an edit: parameter hints (the server has none outside a call), and suggestions after
    /// a dot or inside a name. In a query object the built-in field and operator suggestions stay,
    /// as TypeScript knows no fields.
    fn js_ask(&mut self, id: egui::Id, caret: usize, colls: &[String]) {
        self.js.complete = None;
        self.js.resolve = None;
        let req = self.js_send(db::JsAsk::Signature, caret, colls);
        // The hints on screen stay until the answer replaces them, so they do not flicker while typing.
        let doc = self.js.sig.take().and_then(|s| s.doc);
        self.js.sig = Some(Sig { caret, req, doc });
        let before: String = self.tabs[self.cur].shell.code.chars().take(caret).collect();
        let token = before.chars().rev().take_while(|c| is_js_ident(*c)).count();
        if (token == 0 && !before.ends_with('.')) || in_query_args(&before) {
            return;
        }
        let tab = self.cur;
        if let Some(req) = self.js_send(db::JsAsk::Complete, caret, colls) {
            self.js.complete = Some(Pending { req, id, tab, caret, token });
        }
    }

    /// Asks for the docs of the selected suggestion when the list came from the server.
    fn js_resolve(&mut self, id: egui::Id, colls: &[String]) {
        if self.ac.id != Some(id) || self.ac.doc.as_ref().is_some_and(|d| d.0 == self.ac.sel) || self.js.resolve.is_some_and(|r| r.1 == self.ac.sel) {
            return;
        }
        let (sel, caret) = (self.ac.sel, self.ac.cursor);
        let Some(raw) = self.js.raw.get(sel).cloned() else { return };
        self.js.resolve = self.js_send(db::JsAsk::Resolve(raw), caret, colls).map(|r| (r, sel));
    }

    /// Docs for the word under the pointer once it rests there, shown under the pointer.
    fn js_hover(&mut self, ctx: &egui::Context, at: Option<(usize, egui::Pos2)>, colls: &[String]) {
        // Still on the same word: nothing to look up again.
        let same = at.zip(self.js.hover.as_ref()).is_some_and(|((i, _), h)| (h.span.0..h.span.1).contains(&i));
        if !same {
            let span = at.and_then(|(i, pos)| word_span(&self.tabs[self.cur].shell.code, i).map(|span| (span, pos)));
            self.js.hover = span.map(|(span, pos)| Hover { span, since: Instant::now(), pos, asked: false, req: None, doc: None });
        }
        let Some(h) = &self.js.hover else { return };
        if !h.asked {
            let left = HOVER_DELAY.saturating_sub(h.since.elapsed());
            if !left.is_zero() {
                ctx.request_repaint_after(left);
                return;
            }
            let start = h.span.0;
            let req = self.js_send(db::JsAsk::Hover, start, colls);
            if let Some(h) = &mut self.js.hover {
                (h.asked, h.req) = (true, req);
            }
        }
        if let Some(Hover { pos, doc: Some(d), .. }) = &self.js.hover
            && self.ac.items.is_empty()
        {
            doc_popup(ctx, egui::Id::unique("js-hover"), *pos + egui::vec2(0.0, 18.0), egui::Align2::LEFT_TOP, d);
        }
    }

    /// A server answer; only the newest request of each kind is used.
    pub(super) fn js_answer(&mut self, ctx: &egui::Context, req: u64, out: db::JsOut) {
        match out {
            db::JsOut::Items(items) => {
                let Some(p) = self.js.complete.filter(|p| p.req == req) else { return };
                self.js.complete = None;
                if p.tab == self.cur && ctx.memory(|m| m.has_focus(p.id)) {
                    self.js_list(p, items);
                }
            }
            db::JsOut::Doc(doc) => {
                if let Some((_, sel)) = self.js.resolve.filter(|r| r.0 == req) {
                    self.js.resolve = None;
                    if let Some(d) = doc.filter(|_| self.ac.sel == sel) {
                        self.ac.doc = Some((sel, d));
                    }
                } else if let Some(s) = self.js.sig.as_mut().filter(|s| s.req == Some(req)) {
                    s.doc = doc;
                } else if let Some(h) = self.js.hover.as_mut().filter(|h| h.req == Some(req)) {
                    h.doc = doc;
                }
            }
        }
    }

    /// Turns server items into the suggestion list, replacing the built-in one when there are any.
    fn js_list(&mut self, p: Pending, items: Vec<db::JsItem>) {
        let code = &self.tabs[self.cur].shell.code;
        let typed: String = code.chars().take(p.caret).skip(p.caret - p.token).collect();
        let callable = ["bind", "apply"].iter().all(|n| items.iter().any(|i| i.label == *n));
        let word_start = p.caret - p.token;
        let mut list: Vec<(Sug, db::JsItem)> = items
            .into_iter()
            .filter(|i| i.kind != KIND_TEXT && !(callable && FUNCTION_PROTO.contains(&i.label.as_str())) && fuzzy(&typed, &i.label, false))
            .map(|i| {
                // The item's own edit can start before the typed word, like `["my-field"]` over the dot.
                let back = i.from.map_or(0, |(l, c)| word_start.saturating_sub(db::char_at(code, l, c)));
                let call = i.from.is_none() && i.insert == i.label && matches!(i.kind, 2 | 3);
                let insert = if call { format!("{}()", i.insert) } else { i.insert.clone() };
                let caret = insert.chars().count() - call as usize;
                (Sug { back, text: i.label.clone(), note: kind_note(i.kind), insert, caret }, i)
            })
            .collect();
        let lower = typed.to_ascii_lowercase();
        list.sort_by(|(a, x), (b, y)| {
            let key = |s: &Sug| !s.text.to_ascii_lowercase().starts_with(&lower);
            (key(a), &x.sort, a.text.len()).cmp(&(key(b), &y.sort, b.text.len()))
        });
        list.truncate(50);
        // Nothing useful, or only the word already typed: keep the built-in list.
        if list.is_empty() || (list.len() == 1 && list[0].0.text == typed) {
            return;
        }
        let (items, raw): (Vec<Sug>, Vec<_>) = list.into_iter().map(|(s, i)| (s, i.raw)).unzip();
        self.js.raw = raw;
        self.js.resolve = None;
        self.ac = Ac { id: Some(p.id), token: p.token, items, cursor: p.caret, ..Default::default() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_under_the_pointer() {
        let code = "é dayjs().add(1, ";
        assert_eq!(word_span(code, 4), Some((2, 7)));
        assert_eq!(word_span(code, 1), None);
        assert_eq!(word_span(code, 99), None);
    }
}
