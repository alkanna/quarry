//! Autocomplete for query inputs and the JS shell: suggestion sources and the popup list.

use super::editor::*;
use super::theme::*;
use crate::db;
use eframe::egui::{self, FontId, Key, TextFormat, text::LayoutJob};
use mongodb::bson::{Bson, Document};
use std::collections::HashSet;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum FieldKind {
    Query,
    Sort,
    Project,
}

/// One autocomplete suggestion: what is shown, a short note, what is inserted and where the
/// caret lands inside it (in characters).
pub(crate) struct Sug {
    pub(crate) text: String,
    pub(crate) note: &'static str,
    pub(crate) insert: String,
    pub(crate) caret: usize,
}

/// Open suggestion list of one query input.
#[derive(Default)]
pub(crate) struct Ac {
    pub(crate) id: Option<egui::Id>,
    /// Length in characters of the word being completed, just before `cursor`.
    pub(crate) token: usize,
    pub(crate) items: Vec<Sug>,
    pub(crate) sel: usize,
    pub(crate) cursor: usize,
    pub(crate) rect: Option<egui::Rect>,
}

impl Ac {
    pub(crate) fn apply(&mut self, ctx: &egui::Context, id: egui::Id, text: &mut String, i: usize) {
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
pub(crate) const QUERY_OPS: [&str; 33] = [
    "$all: [  ]", "$and: [  ]", "$bitsAllClear: ", "$bitsAllSet: ", "$bitsAnyClear: ", "$bitsAnySet: ", "$comment: ",
    "$elemMatch: {  }", "$eq: ", "$exists: true", "$exists: false", "$expr: {  }", "$geoIntersects: {  }", "$geoWithin: {  }",
    "$gt: ", "$gte: ", "$in: [  ]", "$jsonSchema: ", "$lt: ", "$lte: ", "$meta: ", "$mod: [  ]", "$ne: ", "$near: {  }",
    "$nearSphere: {  }", "$nin: [  ]", "$nor: [  ]", "$not: ", "$or: [  ]", "$regex: ", "$size: ", "$text: ", "$type: ",
];

/// Every dotted path in the documents (array elements merged), capped so huge documents stay fast.
pub(crate) fn doc_paths(docs: &[Document]) -> Vec<String> {
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
pub(crate) fn fuzzy(token: &str, option: &str, anchored: bool) -> bool {
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
pub(crate) fn date_like(path: &str) -> bool {
    let last = path.rsplit('.').next().unwrap_or(path);
    let l = last.to_ascii_lowercase();
    (last.len() > 2 && last.ends_with("At")) || l.ends_with("date") || l.starts_with("date") || l.ends_with("time") || l.contains("timestamp")
}

/// Loaded fields whose values are dates, for date suggestions in the Query box.
pub(crate) fn date_paths(docs: &[Document], paths: &[String]) -> Vec<String> {
    paths.iter().filter(|p| docs.iter().take(50).any(|d| matches!(db::get_path(d, p), Some(Bson::DateTime(_))))).cloned().collect()
}

/// Suggestions for the text before the caret, following the original's setSuggestions
/// (renderer line 95645): after `{` or `,` field paths (operators when nested), after a dot the
/// sub-fields, after `:` values. Shortest first, at most 50.
pub(crate) fn suggest(before: &str, kind: FieldKind, paths: &[String], dates: &[String]) -> Option<(usize, Vec<Sug>)> {
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
pub(crate) fn ac_field(
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
                        job.append(&format!("   {}", s.note), 0.0, TextFormat { font_id: FontId::proportional(TXT_CAPTION), color: FAINT(), ..Default::default() });
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

pub(crate) const JS_KEYWORDS: [&str; 30] = [
    "async", "await", "break", "case", "catch", "class", "const", "continue", "default", "delete", "do", "else", "export", "finally", "for",
    "function", "if", "import", "in", "instanceof", "let", "new", "of", "return", "switch", "throw", "try", "typeof", "var", "while",
];

/// Names the Shell provides (see the runner) plus common JavaScript built-ins.
pub(crate) const JS_GLOBALS: [&str; 18] = ["db", "ObjectId", "ISODate", "console", "log", "randomId", "UUIDv4", "UUIDv1", "_", "dayjs", "require", "Math", "JSON", "Date", "Promise", "Object", "Array", "Number"];

pub(crate) const DB_METHODS: [&str; 10] = ["collection", "listCollections", "command", "aggregate", "createCollection", "dropCollection", "renameCollection", "stats", "admin", "dropDatabase"];

pub(crate) const COLL_METHODS: [&str; 28] = [
    "find", "findOne", "aggregate", "countDocuments", "estimatedDocumentCount", "distinct", "insertOne", "insertMany", "updateOne", "updateMany",
    "replaceOne", "deleteOne", "deleteMany", "bulkWrite", "findOneAndUpdate", "findOneAndReplace", "findOneAndDelete", "createIndex", "createIndexes",
    "indexes", "listIndexes", "dropIndex", "dropIndexes", "drop", "rename", "watch", "indexExists", "options",
];

pub(crate) const CURSOR_METHODS: [&str; 15] = ["toArray", "limit", "skip", "sort", "project", "batchSize", "hint", "collation", "count", "next", "hasNext", "forEach", "map", "explain", "close"];

pub(crate) const CONSOLE_METHODS: [&str; 4] = ["log", "warn", "error", "info"];

/// Shell autocomplete: methods after `db.`, a collection or a cursor; fields and operators inside a
/// call's object; otherwise keywords, globals and collection names.
/// The variable a package is loaded as: camelCase of its name without scope. Must match
/// packageIdent() in shell_runner.mjs.
pub(crate) fn package_ident(spec: &str) -> String {
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

pub(crate) fn suggest_js(before: &str, paths: &[String], colls: &[String], pkgs: &[String]) -> Option<(usize, Vec<Sug>)> {
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
        // Only the open call's arguments are passed, so braces of the function around the call
        // do not count as query nesting.
        let mut depth = 0;
        let open = before.char_indices().rev().find(|(_, c)| {
            match c {
                ')' => depth += 1,
                '(' if depth == 0 => return true,
                '(' => depth -= 1,
                _ => {}
            }
            false
        });
        let args = open.map_or(before, |(i, _)| &before[i + 1..]);
        return suggest(args, FieldKind::Query, paths, &[]);
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let (_, s) = suggest_js("async function run() {\n  return await orders.find({ sta", &paths, &colls, &[]).unwrap();
        assert_eq!(s[0].text, "status");
        let (_, s) = suggest_js("async function run() {\n  return await orders.find({ total: { $g", &paths, &colls, &[]).unwrap();
        assert_eq!(s[0].note, "operator");
        let (_, s) = suggest_js("  ord", &paths, &colls, &[]).unwrap();
        assert_eq!(s[0].text, "orders");
        let (_, s) = suggest_js("  aw", &paths, &colls, &[]).unwrap();
        assert_eq!(s[0].insert, "await ");
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
    fn package_names() {
        assert_eq!(package_ident("lodash"), "lodash");
        assert_eq!(package_ident("date-fns@3.6.0"), "dateFns");
        assert_eq!(package_ident("@faker-js/faker"), "faker");
        assert_eq!(package_ident("@scope/x-y@^1"), "xY");
        let (_, s) = suggest_js("  dateF", &[], &[], &["dateFns".into()]).unwrap();
        assert_eq!((s[0].text.as_str(), s[0].note), ("dateFns", "package"));
    }
}
