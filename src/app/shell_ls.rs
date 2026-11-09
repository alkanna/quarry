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
    /// The field info the server's collection types were built from. Holding it keeps each
    /// pointer unique, so pointer equality means unchanged.
    typed: Vec<(String, Arc<Fields>)>,
    /// The schema a SQL tab's types were built from. Holding the Arc keeps its pointer unique.
    typed_sql: Option<Arc<db::sql::schema::Schema>>,
    /// The same for each attached connection, in the order of the tab's attachments.
    typed_att: Vec<AttTyped>,
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

/// Hash of what the globals declarations are built from, besides the collection types.
/// The Shell's colours, with the text of SQL templates coloured as SQL and their `${ }` parts as JS.
/// The JS around is lexed in pieces that each end at an opening backtick, so the closing one of a
/// template cannot start a string that swallows the rest.
pub(super) fn js_sql_job(text: &str) -> LayoutJob {
    let tpls = sql_templates(text);
    if tpls.is_empty() {
        return js_job(text);
    }
    let mut job = LayoutJob::default();
    let mut add = |part: LayoutJob, sql: bool| {
        for s in part.sections {
            let mut fmt = s.format;
            // One size per line so the columns stay aligned with the JS beside it.
            if sql {
                fmt.font_id = FontId::monospace(TXT_BODY);
            }
            job.append(&part.text[s.byte_range.start.0..s.byte_range.end.0], 0.0, fmt);
        }
    };
    let mut at = 0;
    // The JS between templates, minus the closing backtick the previous one left at its start.
    let js = |add: &mut dyn FnMut(LayoutJob, bool), from: usize, to: usize| {
        let skip = (from > 0 && text[from..to].starts_with('`')) as usize;
        if skip > 0 {
            add(js_job("`"), false);
        }
        add(js_job(&text[from + skip..to]), false);
    };
    for t in &tpls {
        // Nested templates sit inside a part of the outer one and are coloured as JS there.
        if t.range.start < at {
            continue;
        }
        js(&mut add, at, t.range.start);
        let mut seg = t.range.start;
        for h in &t.holes {
            let (hs, he) = (h.start.max(seg), h.end.min(t.range.end));
            add(sql_job(&text[seg..hs], TXT_SMALL), true);
            add(js_job(&text[hs..he]), false);
            seg = he;
        }
        add(sql_job(&text[seg..t.range.end], TXT_SMALL), true);
        at = t.range.end;
    }
    js(&mut add, at, text.len());
    job
}

/// The rest of the template after the caret as SQL, as it was before this frame's edit, which is what
/// aliases after the caret are looked up in. The caret is the one the editor held then.
pub(super) fn template_tail(ctx: &egui::Context, id: egui::Id, code: &str) -> String {
    let caret = egui::TextEdit::load_state(ctx, id).and_then(|s| s.cursor.char_range()).map(|r| usize::from(r.primary.index));
    let at = caret.map_or(code.len(), |c| code.char_indices().nth(c).map_or(code.len(), |x| x.0));
    let Some(tpl) = sql_at(code, at) else { return String::new() };
    let (before, full) = sql_text(code, &tpl, at);
    full[before.len()..].to_string()
}

/// QUARRY_TEST_SQL=shellac: opens the list as if `FROM ord` had just been typed inside the script's template.
pub(super) fn shell_ac_demo(ui: &egui::Ui, id: egui::Id, code: &str, ac: &mut Ac, scope: &crate::ui::sql_complete::Scope, values: &Fields) {
    let Some(at) = code.find("FROM ord").map(|i| i + 8).filter(|_| ac.id.is_none()) else { return };
    let Some(tpl) = sql_at(code, at) else { return };
    let (before, full) = sql_text(code, &tpl, at);
    let (b, f) = crate::ui::sql_complete::split_statement(&before, &full[before.len()..]);
    if let Some(o) = crate::ui::sql_complete::analyze(&b, &f, scope, values).filter(|o| !o.sugs.is_empty()) {
        let n = code[..at].chars().count();
        *ac = Ac { id: Some(id), token: b[o.from..].chars().count(), items: o.sugs, cursor: n, title: o.title, foot: o.foot, ..Default::default() };
        ui.memory_mut(|m| m.request_focus(id));
        let mut st = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
        st.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(n))));
        st.store(ui.ctx(), id);
    }
}

fn globals_key(colls: &[String], packages: &[String], attached: &[AttachedInfo]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (colls, packages).hash(&mut h);
    for a in attached {
        (&a.name, &a.names, matches!(a.typed, AttTyped::Sql(_))).hash(&mut h);
    }
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
    /// Fields of the collections loaded in tabs of the current tab's database, the current tab's
    /// first so its fields win for its collection.
    pub(super) fn db_fields(&self) -> Vec<(String, Arc<Fields>)> {
        let cur = &self.tabs[self.cur];
        self.fields_of(cur.sel.conn, &cur.sel.db)
    }

    /// The same for any connection and database, so an attached one is typed from tabs that show it.
    pub(super) fn fields_of(&self, conn: u64, db: &str) -> Vec<(String, Arc<Fields>)> {
        let cur = &self.tabs[self.cur];
        let mut out: Vec<(String, Arc<Fields>)> = Vec::new();
        for t in std::iter::once(cur).chain(&self.tabs) {
            if t.sel.conn == conn && t.sel.db == db && !t.field_info.is_empty() && !out.iter().any(|o| o.0 == t.sel.coll) {
                out.push((t.sel.coll.clone(), t.field_info.clone()));
            }
        }
        out
    }

    /// The key of the current tab's database in the column cache.
    fn sql_key(&self) -> (u64, String) {
        let t = &self.tabs[self.cur];
        (t.sel.conn, t.sel.db.clone())
    }

    /// Asks for the schema of the current SQL tab's database, which autocomplete shares. `again`
    /// refreshes one already held, as a run does so that a table added since shows up.
    pub(super) fn sql_columns_ask(&mut self, again: bool) {
        let key = self.sql_key();
        if self.demo || (self.sql_schema_asked.contains(&key) && !again) {
            return;
        }
        let Some(db) = self.conn(key.0).and_then(|c| c.sql.clone()) else { return };
        self.sql_schema_asked.insert(key.clone());
        self.worker.sql_schema(key.0, db, key.1);
    }

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
        let sql = self.tabs[self.cur].sql.is_some();
        let attached = self.attached_infos();
        let key = globals_key(colls, &names, &attached) ^ sql as u64;
        // Rebuilt when an attachment was added, removed or retyped by a new schema or field list.
        let att_same = attached.len() == self.js.typed_att.len() && attached.iter().zip(&self.js.typed_att).all(|(a, b)| a.typed.same(b));
        // SQL tabs are typed from the autocomplete schema, which is replaced on every reload, so
        // pointer equality means unchanged.
        let sql_cols = sql.then(|| self.sql_schema.get(&self.sql_key()).cloned()).flatten();
        let (known, same) = if sql {
            let same = match (&sql_cols, &self.js.typed_sql) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            };
            (Vec::new(), same)
        } else {
            // Collections loaded in a tab of this database are typed from their fields, so the server
            // completes their fields and text values. Field info is replaced on every change, so the
            // types are rebuilt only when a pointer differs.
            let known = self.db_fields();
            let same = known.len() == self.js.typed.len() && known.iter().zip(&self.js.typed).all(|(a, b)| a.0 == b.0 && Arc::ptr_eq(&a.1, &b.1));
            (known, same)
        };
        // Rebuilt and sent only when something changed; the server keeps the last one.
        let globals = (self.js.globals != Some(key) || !same || !att_same).then(|| {
            let extra = attached_decls(&attached, sql);
            if sql {
                // Tables with no known columns yet still get a global, typed loosely.
                let tables: Vec<db::sql::TableCols> = sql_cols.as_deref().map(|s| s.tables.iter().map(|t| db::sql::TableCols { name: t.name.clone(), cols: t.cols.iter().map(|c| db::sql::TableCol { name: c.name.clone(), full_type: c.ty.clone(), nullable: c.nullable }).collect() }).collect()).unwrap_or_else(|| colls.iter().map(|c| db::sql::TableCols { name: c.clone(), cols: Vec::new() }).collect());
                return db::sql_globals(&tables, &names, package_ident, &JS_RESERVED) + &extra;
            }
            let schemas = known.iter().map(|(c, f)| (c.clone(), ts_type(f))).collect();
            db::js_globals(colls, &schemas, &names, package_ident, &JS_RESERVED) + &extra
        });
        (self.js.globals, self.js.typed, self.js.typed_sql) = (Some(key), known, sql_cols);
        self.js.typed_att = attached.into_iter().map(|a| a.typed).collect();
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
    /// a dot, inside a name or inside a string. In a query object the built-in field, operator and
    /// value suggestions stay, as they also cover dotted paths and the value shortcuts.
    fn js_ask(&mut self, id: egui::Id, caret: usize, colls: &[String]) {
        self.js.complete = None;
        self.js.resolve = None;
        let before: String = self.tabs[self.cur].shell.code.chars().take(caret).collect();
        // SQL templates complete from the schema, not from the TypeScript server.
        if self.tabs[self.cur].sql.is_some() && sql_at(&before, before.len()).is_some() {
            self.js.sig = None;
            return;
        }
        let req = self.js_send(db::JsAsk::Signature, caret, colls);
        // The hints on screen stay until the answer replaces them, so they do not flicker while typing.
        let doc = self.js.sig.take().and_then(|s| s.doc);
        self.js.sig = Some(Sig { caret, req, doc });
        let token = before.chars().rev().take_while(|c| is_js_ident(*c)).count();
        let ask = match js_spot(&before) {
            // A query value's string: the server knows the field's values from its type.
            JsSpot::QueryString => true,
            JsSpot::Inert => false,
            JsSpot::Code => (token > 0 || before.ends_with('.')) && !in_query_args(&before),
        };
        if !ask {
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
                (Sug { sql: None, back, text: i.label.clone(), note: kind_note(i.kind), insert, caret }, i)
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
