//! Autocomplete for query inputs and the JS shell: suggestion sources and the popup list.

use super::cells::{DocRef, Val};
use super::editor::*;
use super::theme::*;
use crate::db;
use eframe::egui::{self, Color32, FontId, Key, text::LayoutJob};
use mongodb::bson::Bson;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum FieldKind {
    Query,
    Sort,
    Project,
}

/// One autocomplete suggestion: what is shown, a short note, what is inserted and where the
/// caret lands inside it (in characters).
pub(crate) struct Sug {
    /// SQL only: what the row shows at the right instead of the kind, and the footer line while it is selected.
    pub(crate) sql: Option<SqlInfo>,
    /// Characters before the typed word that the insert also replaces, like the dot before `["my-field"]`.
    pub(crate) back: usize,
    pub(crate) text: String,
    pub(crate) note: &'static str,
    pub(crate) insert: String,
    pub(crate) caret: usize,
}

pub(crate) struct SqlInfo {
    pub(crate) detail: String,
    pub(crate) foot: String,
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
    /// Signature and docs of the item at this index, shown beside the list.
    pub(crate) doc: Option<(usize, db::JsDoc)>,
    /// The field's values are being fetched from the server; the list stays open and says so.
    pub(crate) loading: bool,
    /// Why the server was not asked for values matching the typed text, shown under the list.
    pub(crate) note: Option<&'static str>,
    /// SQL only: the heading row above the list and the footer when the selected row has none.
    pub(crate) title: String,
    pub(crate) foot: String,
    /// SQL only: the "table.column" whose values the list is waiting for.
    pub(crate) want: String,
}

impl Ac {
    pub(crate) fn apply(&mut self, ctx: &egui::Context, id: egui::Id, text: &mut String, i: usize) {
        let Some(s) = self.items.get(i) else { return };
        let start = self.cursor.saturating_sub(self.token + s.back);
        let byte = |c: usize| text.char_indices().nth(c).map(|(b, _)| b).unwrap_or(text.len());
        let (bs, be) = (byte(start), byte(self.cursor));
        text.replace_range(bs..be, &s.insert);
        if let Some(mut st) = egui::TextEdit::load_state(ctx, id) {
            st.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(start + s.caret))));
            st.store(ctx, id);
        }
        self.close();
    }

    pub(crate) fn close(&mut self) {
        self.items.clear();
        self.loading = false;
        self.note = None;
    }

    pub(crate) fn shown(&self, id: egui::Id) -> bool {
        self.id == Some(id) && (!self.items.is_empty() || self.loading || self.note.is_some())
    }
}

/// Query operators offered inside a field's own object, as in the original (renderer line 95668).
pub(crate) const QUERY_OPS: [&str; 33] = [
    "$all: [  ]", "$and: [  ]", "$bitsAllClear: ", "$bitsAllSet: ", "$bitsAnyClear: ", "$bitsAnySet: ", "$comment: ",
    "$elemMatch: {  }", "$eq: ", "$exists: true", "$exists: false", "$expr: {  }", "$geoIntersects: {  }", "$geoWithin: {  }",
    "$gt: ", "$gte: ", "$in: [  ]", "$jsonSchema: ", "$lt: ", "$lte: ", "$meta: ", "$mod: [  ]", "$ne: ", "$near: {  }",
    "$nearSphere: {  }", "$nin: [  ]", "$nor: [  ]", "$not: ", "$or: [  ]", "$regex: ", "$size: ", "$text: ", "$type: ",
];

/// Value shortcuts typed by name after a field's colon: the name, what it expands to with `|` at
/// the caret, a note, and the field kinds it suits (empty for any). `~"..."` is expanded when the
/// query runs, so its text is matched literally; startsWith and endsWith are plain regexes, where
/// special characters keep their meaning.
pub(crate) const VALUE_SNIPPETS: [(&str, &str, &str, &[&str]); 16] = [
    ("contains", "~\"|\"", "text contains, any case", STRINGS),
    ("notContains", "{ $not: ~\"|\" }", "text does not contain", STRINGS),
    ("startsWith", "{ $regex: \"^|\" }", "text starts with, uses an index", STRINGS),
    ("endsWith", "{ $regex: \"|$\" }", "text ends with", STRINGS),
    ("equalsAnyCase", "{ $regex: \"^|$\", $options: \"i\" }", "exact text, any case", STRINGS),
    ("between", "{ $gte: |, $lte:  }", "range, both ends included", ORDERED),
    ("hasItem", "{ $elemMatch: { | } }", "an item matches", ARRAY),
    ("hasAll", "{ $all: [|] }", "holds every value", ARRAY),
    ("size", "{ $size: | }", "number of items", ARRAY),
    ("exists", "{ $exists: true }|", "field is present", &[]),
    ("missing", "{ $exists: false }|", "field is absent", &[]),
    ("notEmpty", "{ $nin: [null, \"\", []] }|", "has a value", &[]),
    ("empty", "{ $in: [null, \"\", []] }|", "null, missing, \"\" or []", &[]),
    ("anyOf", "{ $in: [|] }", "one of a list", &[]),
    ("noneOf", "{ $nin: [|] }", "none of a list", &[]),
    ("type", "{ $type: \"|\" }", "stored as a type", &[]),
];
const STRINGS: &[&str] = &["string"];
const ORDERED: &[&str] = &["int", "long", "double", "decimal", "date"];
const ARRAY: &[&str] = &["array"];

/// Operators offered as field names at the top of the Query, same form as `VALUE_SNIPPETS`.
pub(crate) const TOP_SNIPPETS: [(&str, &str, &str); 5] = [
    ("$or", "$or: [{ | }, {  }]", "any condition matches"),
    ("$and", "$and: [{ | }, {  }]", "every condition matches"),
    ("$nor", "$nor: [{ | }]", "no condition matches"),
    ("$expr", "$expr: { | }", "compare fields"),
    ("$text", "$text: { $search: \"|\" }", "text index search"),
];

/// The Query help window's text, built from the same tables autocomplete uses so it stays current.
pub(crate) fn query_help() -> String {
    const COL: usize = 31;
    let row = |a: &str, b: &str| format!("  {a:<COL$}{b}\n");
    let snip = |a: &str, t: &str, b: &'static str| format!("  {a:<15}{:<33}{b}\n", snippet(a, t, b).insert);
    let mut s = String::from("Typing in the Query box\n");
    let tops: Vec<&str> = TOP_SNIPPETS.iter().map(|t| t.0).collect();
    s += &row("after { or ,", &format!("field names, and {}", tops.join(", ")));
    s += &row("after a dot", "sub-fields");
    s += &row("after a colon", "values from the loaded documents, shortcuts below");
    s += &row("after a colon and a quote", "the field's text values");
    s += &row("Up / Down, Enter or Tab, Esc", "move, insert, close");
    s += "\nSyntax\n";
    s += &row("{ status: 'paid', n: 2, }", "unquoted keys, single quotes, trailing commas");
    s += &row("ObjectId(\"...\") ISODate(\"...\")", "also new Date(), NumberLong(), NumberDecimal()");
    s += &row("db.orders.find({ ... })", "paste a shell query to run it as is");
    s += &row("~\"some text\"  or  ~word", "contains, any case, characters taken literally");
    s += "\nValue shortcuts: type the name after a field's colon\n";
    for (name, t, note, _) in VALUE_SNIPPETS {
        s += &snip(name, t, note);
    }
    s += "\nAt the top level: type the name after { or ,\n";
    for (name, t, note) in TOP_SNIPPETS {
        s += &snip(name, t, note);
    }
    s += "\nDates: offered after a date field's colon, worked out when the query runs\n";
    for (t, note) in db::date_suggestions(mongodb::bson::DateTime::now().timestamp_millis()) {
        // The explicit range template is too long for the column and is shown in the list anyway.
        if t.len() < COL {
            s += &row(&t, note);
        }
    }
    s
}

fn snippet(name: &str, template: &str, note: &'static str) -> Sug {
    let caret = template[..template.find('|').unwrap_or(template.len())].chars().count();
    Sug { sql: None, back: 0, text: name.to_string(), note, insert: template.replace('|', ""), caret }
}

/// What the loaded documents hold in one field: its most common kind and its most common values.
#[derive(Default, Clone)]
pub(crate) struct FieldInfo {
    pub(crate) kind: &'static str,
    pub(crate) values: Vec<Bson>,
    /// SQL only: how many rows hold each of `values`, in the same order.
    pub(crate) freq: Vec<u64>,
}

pub(crate) type Fields = HashMap<String, FieldInfo>;

/// Paths kept per page, so very wide documents stay fast.
const MAX_PATHS: usize = 2000;

/// Calls `f` with every dotted path in a document and its value, the documents inside arrays
/// walked under their array's path (at most 20 items, 4 levels deep). Raw documents are read
/// from their bytes, so a page is never decoded just to scan it.
fn each_field<'a>(d: DocRef<'a>, prefix: &str, depth: usize, f: &mut dyn FnMut(&str, Val<'a>)) {
    for (k, v) in d.fields() {
        let p = if prefix.is_empty() { k.to_string() } else { format!("{prefix}.{k}") };
        f(&p, v);
        if depth < 4 {
            if let Some(c) = v.object() {
                each_field(c, &p, depth + 1, f);
            } else if let Some(items) = v.items() {
                for c in items.take(20).filter_map(Val::object) {
                    each_field(c, &p, depth + 1, f);
                }
            }
        }
    }
}

/// Kinds and frequent values of every field, array items counted under their array's path.
pub(crate) fn field_info<'a>(docs: impl IntoIterator<Item = impl Into<DocRef<'a>>>) -> Fields {
    const MAX_VALUES: usize = 30;
    #[derive(Default)]
    struct Acc {
        kinds: HashMap<&'static str, usize>,
        vals: HashMap<String, (usize, Bson)>,
    }
    let mut acc: HashMap<String, Acc> = HashMap::new();
    // ponytail: first 200 documents only, enough for suggestions and fast on big pages.
    for d in docs.into_iter().take(200) {
        each_field(d.into(), "", 0, &mut |p, v| {
            if acc.len() >= MAX_PATHS && !acc.contains_key(p) {
                return;
            }
            let a = acc.entry(p.to_string()).or_default();
            *a.kinds.entry(v.kind()).or_default() += 1;
            let items: Vec<Val> = match v.items() {
                Some(items) => items.take(20).collect(),
                None => vec![v],
            };
            for x in items {
                let keep = match x.as_str() {
                    Some(s) => s.chars().count() <= 80,
                    None => matches!(x.kind(), "int" | "long" | "double" | "bool"),
                };
                if keep {
                    let x = x.bson().into_owned();
                    a.vals.entry(db::shell_text(&x)).or_insert_with(|| (0, x)).0 += 1;
                }
            }
        });
    }
    acc.into_iter()
        .map(|(p, a)| {
            let kind = a.kinds.into_iter().filter(|(k, _)| *k != "null").max_by_key(|(k, n)| (*n, *k)).map_or("null", |(k, _)| k);
            let mut vals: Vec<(String, (usize, Bson))> = a.vals.into_iter().collect();
            // Ties in text order, so the list keeps its order across reloads.
            vals.sort_by(|x, y| y.1.0.cmp(&x.1.0).then_with(|| x.0.cmp(&y.0)));
            (p, FieldInfo { kind, values: vals.into_iter().take(MAX_VALUES).map(|(_, (_, v))| v).collect(), freq: Vec::new() })
        })
        .collect()
}

/// The documents' shape as a TypeScript type, for the Shell's language server. Text fields list
/// their known values, kept open with `string & {}`, so a quote after a field's colon offers them.
pub(crate) fn ts_type(fields: &Fields) -> String {
    let mut keys: Vec<&str> = fields.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut kids: HashMap<&str, Vec<&str>> = HashMap::new();
    for k in keys {
        kids.entry(k.rfind('.').map_or("", |i| &k[..i])).or_default().push(k);
    }
    fn texts(vals: &[Bson]) -> Option<String> {
        let v: Vec<String> = vals.iter().filter_map(Bson::as_str).map(|s| serde_json::Value::from(s).to_string()).collect();
        (!v.is_empty()).then(|| format!("{} | (string & {{}})", v.join(" | ")))
    }
    fn obj(fields: &Fields, kids: &HashMap<&str, Vec<&str>>, parent: &str) -> Option<String> {
        let props: Vec<String> = kids.get(parent)?.iter().map(|p| {
            let i = &fields[*p];
            let ty = match i.kind {
                "string" => texts(&i.values).unwrap_or_else(|| "string".into()),
                "int" | "long" | "double" => "number".into(),
                "bool" => "boolean".into(),
                "date" => "Date".into(),
                "objectId" => "import(\"mongodb\").ObjectId".into(),
                "object" => obj(fields, kids, p).unwrap_or_else(|| "any".into()),
                "array" => obj(fields, kids, p).or_else(|| texts(&i.values)).map_or_else(|| "any[]".into(), |t| format!("Array<{t}>")),
                _ => "any".into(),
            };
            format!("{}?: {ty}", db::shell_key(&p[p.rfind('.').map_or(0, |i| i + 1)..]))
        }).collect();
        // Open to fields the loaded page did not show, like ones a projection left out.
        Some(format!("{{ {}; [key: string]: any }}", props.join("; ")))
    }
    obj(fields, &kids, "").unwrap_or_else(|| "import(\"mongodb\").Document".into())
}

/// Whether a value is worth suggesting: short text, numbers and booleans, as `field_info` keeps.
pub(crate) fn suggestible(v: &Bson) -> bool {
    match v {
        Bson::String(s) => s.chars().count() <= 80,
        Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Boolean(_) => true,
        _ => false,
    }
}

/// Adds values fetched from the server after the page's own, which come first because they are
/// counted. A field missing from the page gets an entry typed by its first value.
pub(crate) fn merge_values(fields: &mut Fields, server: &HashMap<String, Vec<Bson>>) {
    for (p, vals) in server {
        let Some(first) = vals.first() else { continue };
        let f = fields.entry(p.clone()).or_insert_with(|| FieldInfo { kind: db::type_name(first), values: Vec::new(), freq: Vec::new() });
        for v in vals {
            if !f.values.contains(v) {
                f.values.push(v.clone());
            }
        }
    }
}

/// The field whose value starts at the end of `before`: `status` in `{ status: "dr`. Operators
/// like `$in` are left out, their field is further back.
fn value_field(before: &str) -> Option<&str> {
    let lead = before.trim_end_matches(|c: char| c.is_ascii_alphanumeric() || "@#$._".contains(c)).trim_end_matches(['"', '\'']).trim_end();
    // Inside $elemMatch the key is relative to the array, not a path of the collection.
    let owner = innermost_open(before).and_then(|i| key_before_colon(before[..i].trim_end()));
    key_before_colon(lead).filter(|f| !f.is_empty() && !f.starts_with('$') && owner != Some("$elemMatch"))
}

/// Byte index of the quote that opens a string still open at the end of `s`.
pub(crate) fn open_quote(s: &str) -> Option<usize> {
    let (mut open, mut esc) = (None, false);
    for (i, c) in s.char_indices() {
        match open {
            Some(_) if esc => esc = false,
            Some(_) if c == '\\' => esc = true,
            Some((_, q)) if c == q => open = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => open = Some((i, c)),
            None => {}
        }
    }
    open.map(|(i, _)| i)
}

/// The field whose value the caret is in, and the text typed so far when that value is a string:
/// (`name`, Some("al ex")) in `{ name: "al ex`.
pub(crate) fn value_at(before: &str) -> Option<(&str, Option<&str>)> {
    match open_quote(before) {
        Some(q) => Some((value_field(&before[..=q])?, Some(&before[q + 1..]))),
        None => Some((value_field(before)?, None)),
    }
}

/// Every dotted path in the documents (array elements merged), capped so huge documents stay fast.
pub(crate) fn doc_paths<'a>(docs: impl IntoIterator<Item = impl Into<DocRef<'a>>>) -> Vec<String> {
    let (mut out, mut seen) = (Vec::new(), HashSet::new());
    for d in docs {
        each_field(d.into(), "", 0, &mut |p, _| {
            if out.len() < MAX_PATHS && seen.insert(p.to_string()) {
                out.push(p.to_string());
            }
        });
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

/// The key just before a trailing colon, quotes removed: `status` in `{ "status":`.
fn key_before_colon(s: &str) -> Option<&str> {
    let f = s.strip_suffix(':')?.trim_end().trim_end_matches(['"', '\'']);
    Some(&f[f.char_indices().rev().take_while(|(_, c)| c.is_ascii_alphanumeric() || "._$".contains(*c)).last().map_or(f.len(), |(i, _)| i)..])
}

/// Byte index of the innermost `{` still open at the end of `s`, quoted text skipped.
fn innermost_open(s: &str) -> Option<usize> {
    let (mut stack, mut quote, mut esc) = (Vec::new(), None, false);
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '{' => stack.push(i),
            '}' => {
                stack.pop();
            }
            _ => {}
        }
    }
    stack.pop()
}

/// Suggestions for the text before the caret, following the original's setSuggestions
/// (renderer line 95645): after `{` or `,` field paths (operators when nested), after a dot the
/// sub-fields, after `:` values (the field's own values from `fields`, inside quotes too). Shortest
/// first, at most 50.
pub(crate) fn suggest(before: &str, kind: FieldKind, paths: &[String], fields: &Fields) -> Option<(usize, Vec<Sug>)> {
    const MAX_SUGGESTIONS: usize = 50;
    let token_start = before.char_indices().rev().take_while(|(_, c)| c.is_ascii_alphanumeric() || "@#$._".contains(*c)).last().map(|(i, _)| i).unwrap_or(before.len());
    let token = &before[token_start..];
    let lead = &before[..token_start];
    let quote = lead.chars().last().filter(|c| matches!(c, '"' | '\''));
    let lead = lead.trim_end_matches(['"', '\'']).trim_end();
    let caret_in = |s: &str| ["[  ]", "{  }", "(\"\")"].iter().find_map(|m| s.find(m).map(|i| s[..i].chars().count() + 2)).unwrap_or(s.chars().count());
    let field = |p: &String| {
        let insert = if quote.is_some() { p.clone() } else { format!("{p}: ") };
        Sug { sql: None, back: 0, text: p.clone(), note: "field", caret: insert.chars().count(), insert }
    };
    let colon_field = key_before_colon(lead);
    let info = colon_field.and_then(|f| fields.get(f));
    // A dot in a key walks into sub-fields; in a value (after a colon) it is just text.
    let (anchored, mut items): (bool, Vec<Sug>) = if token.contains('.') && !lead.ends_with(':') {
        let prefix = &token[..token.rfind('.').unwrap()];
        let kids = paths.iter().filter(|p| p.strip_prefix(prefix).and_then(|r| r.strip_prefix('.')).is_some_and(|r| !r.contains('.')));
        // The original lists at most 100 sub-fields after a dot (renderer line 95666).
        const MAX_CHILD_KEYS: usize = 100;
        (false, kids.take(MAX_CHILD_KEYS).map(field).collect())
    } else if lead.ends_with('{') || lead.ends_with(',') {
        if token.is_empty() {
            return None;
        }
        // The innermost open object decides: a field's own object (after a colon) takes operators,
        // the query itself and each condition in an $or list take field names.
        let pre = innermost_open(before).map_or("", |i| before[..i].trim_end());
        let owner = key_before_colon(pre);
        if owner.is_some() && kind == FieldKind::Query {
            let mut v: Vec<Sug> = QUERY_OPS.iter().map(|o| Sug { sql: None, back: 0, text: o.trim_end().to_string(), note: "operator", insert: o.to_string(), caret: caret_in(o) }).collect();
            // $elemMatch also takes the array items' fields, named relative to the array.
            if owner == Some("$elemMatch")
                && let Some(arr) = innermost_open(pre).and_then(|i| key_before_colon(pre[..i].trim_end()))
            {
                v.extend(paths.iter().filter_map(|p| p.strip_prefix(arr)?.strip_prefix('.')).map(|r| field(&r.to_string())));
            }
            (false, v)
        } else {
            let mut v: Vec<Sug> = paths.iter().map(field).collect();
            if kind == FieldKind::Query && quote.is_none() {
                v.extend(TOP_SNIPPETS.iter().map(|(n, t, note)| snippet(n, t, note)));
            }
            (false, v)
        }
    } else if kind == FieldKind::Query
        && quote.is_none()
        && colon_field.is_some_and(|f| !f.is_empty() && (info.map_or(!paths.iter().any(|p| p == f) && date_like(f), |i| i.kind == "date")))
    {
        // A date field lists date shortcuts and range templates as soon as the colon is typed.
        let now = mongodb::bson::DateTime::now().timestamp_millis();
        let mut v: Vec<Sug> = db::date_suggestions(now).into_iter().map(|(t, n)| Sug { sql: None, back: 0, caret: t.chars().count(), insert: t.clone(), text: t, note: n }).collect();
        v.push(Sug { sql: None, back: 0, text: "ISODate(\"\")".into(), note: "date", insert: "ISODate(\"\")".into(), caret: 9 });
        (true, v)
    } else if lead.ends_with(':') && quote.is_some() {
        // Inside a string: the field's own text values, listed as soon as the quote is typed.
        if kind != FieldKind::Query {
            return None;
        }
        (true, info.into_iter().flat_map(|i| &i.values).filter_map(Bson::as_str).map(|text| {
            // JSON escapes, then adjusted for the quote the string was opened with.
            let json = serde_json::Value::from(text).to_string();
            let s = json[1..json.len() - 1].to_string();
            let s = if quote == Some('\'') { s.replace("\\\"", "\"").replace('\'', "\\'") } else { s };
            Sug { sql: None, back: 0, text: text.to_string(), note: "value", caret: s.chars().count(), insert: s }
        }).collect())
    } else if lead.ends_with(':') {
        let query_vals: Vec<(&str, &'static str)> = [("ObjectId(\"\")", "objectId"), ("ISODate(\"\")", "date"), ("true", "bool"), ("false", "bool"), ("null", "null")].into_iter().chain(db::DATE_SHORTCUTS).collect();
        let vals: &[(&str, &'static str)] = match kind {
            FieldKind::Query => &query_vals,
            FieldKind::Sort => &[("1", "ascending"), ("-1", "descending")],
            FieldKind::Project => &[("1", "include"), ("0", "exclude")],
        };
        let values = |i: &FieldInfo| -> Vec<Sug> {
            i.values.iter().map(|x| {
                let insert = db::shell_text(x);
                Sug { sql: None, back: 0, text: x.as_str().map_or_else(|| insert.clone(), str::to_string), note: "value", caret: insert.chars().count(), insert }
            }).collect()
        };
        // Right after the colon only the field's own values are listed, and nothing for a field
        // without any, so the list does not pop up with generic snippets on every colon.
        if token.is_empty() && kind == FieldKind::Query {
            let v = info.map(values).unwrap_or_default();
            if v.is_empty() {
                return None;
            }
            return Some((0, v));
        }
        let mut v: Vec<Sug> = vals.iter().map(|(v, n)| Sug { sql: None, back: 0, text: v.to_string(), note: n, insert: v.to_string(), caret: caret_in(v) }).collect();
        if kind == FieldKind::Query {
            v.extend(info.map(values).unwrap_or_default());
            // Snippets that suit the field's kind; all of them for a field not in the loaded documents.
            // None after an operator's colon, like `$gt:`, where they would nest a condition in a value.
            if !colon_field.is_some_and(|f| f.starts_with('$')) {
                v.extend(VALUE_SNIPPETS.iter().filter(|s| s.3.is_empty() || info.is_none_or(|i| s.3.contains(&i.kind))).map(|(n, t, note, _)| snippet(n, t, note)));
            }
        }
        (true, v)
    } else {
        return None;
    };
    items.retain(|s| fuzzy(token, &s.text, anchored));
    let mut seen = HashSet::new();
    items.retain(|s| seen.insert(s.text.clone()));
    // The original sorts by length only; words that start with what was typed come first here.
    let lower = token.to_ascii_lowercase();
    // Nothing typed yet keeps the list in its own order (the date list is ordered by use). Values
    // from the documents come before snippets, since they are what the field really holds.
    if !token.is_empty() {
        items.sort_by_cached_key(|s| (!s.text.to_ascii_lowercase().starts_with(&lower), s.note != "value", s.text.len()));
    }
    items.truncate(MAX_SUGGESTIONS);
    (!items.is_empty()).then(|| (token.chars().count(), items))
}

/// Takes Ctrl+Space, which asks for suggestions where typing alone shows none, and the space it may also type, so the
/// text box never sees either. Ctrl on macOS too, as in DBeaver and VS Code, since Cmd+Space is Spotlight.
fn take_ctrl_space(ui: &egui::Ui) -> bool {
    let hit = ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { key: Key::Space, pressed: true, modifiers, .. } if modifiers.ctrl)));
    if hit {
        ui.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Key { key: Key::Space, .. }) && !matches!(e, egui::Event::Text(t) if t == " ")));
    }
    hit
}

/// Suggestions for the text before the caret: the length of the word they replace, and the list. The flag is Ctrl+Space,
/// which asks for a list even where typing alone shows none.
pub(crate) type Suggest<'a> = dyn Fn(&str, bool) -> Option<(usize, Vec<Sug>)> + 'a;

/// A code input with brackets that close themselves and a suggestion list under the caret.
/// Up/Down move, Enter or Tab insert, Esc closes. Returns the editor output and whether a
/// suggestion was inserted this frame (so the caller does not also treat Enter as "run").
pub(crate) fn ac_field(
    ui: &mut egui::Ui,
    id: egui::Id,
    text: &mut String,
    ac: &mut Ac,
    compute: &Suggest,
    show: impl FnOnce(&mut egui::Ui, &mut String, bool) -> egui::text_edit::TextEditOutput,
) -> (egui::text_edit::TextEditOutput, bool) {
    let (out, picked, moved) = ac_input(ui, id, text, ac, compute, show);
    if picked || !ac.shown(id) {
        return (out, picked);
    }
    let caret = out.galley.pos_from_cursor(egui::text::CCursor::new(ac.cursor));
    let pos = out.galley_pos + caret.left_bottom().to_vec2() + egui::vec2(-6.0, 4.0);
    let mut picked = false;
    egui::Area::new(id.with("ac")).order(egui::Order::Foreground).fixed_pos(pos).show(ui.ctx(), |ui| {
        picked = ac_list(ui, id, text, ac, moved, None);
    });
    (out, picked)
}

/// The input half of `ac_field`: keys, recomputing suggestions on edits and closing the list.
/// For inputs inside a menu, which closes on any click outside its own layer: the caller draws the
/// list in the menu with `ac_list` instead of a popup. Returns the output, whether a suggestion was
/// picked by key, and whether the selection moved this frame.
pub(crate) fn ac_input(
    ui: &mut egui::Ui,
    id: egui::Id,
    text: &mut String,
    ac: &mut Ac,
    compute: &Suggest,
    show: impl FnOnce(&mut egui::Ui, &mut String, bool) -> egui::text_edit::TextEditOutput,
) -> (egui::text_edit::TextEditOutput, bool, bool) {
    ac_input_with(ui, id, text, ac, compute, true, show)
}

/// `ac_input`, where `pair` says whether brackets and quotes close themselves. SQL inputs leave
/// them alone: an apostrophe in a comment or a lone quote in a LIKE pattern is common there.
pub(crate) fn ac_input_with(
    ui: &mut egui::Ui,
    id: egui::Id,
    text: &mut String,
    ac: &mut Ac,
    compute: &Suggest,
    pair: bool,
    show: impl FnOnce(&mut egui::Ui, &mut String, bool) -> egui::text_edit::TextEditOutput,
) -> (egui::text_edit::TextEditOutput, bool, bool) {
    // Esc into Vim's Normal mode closes the list too: nothing is typed there for it to complete.
    if ac.id == Some(id) && crate::ui::vim::normal(ui.ctx(), id) {
        ac.close();
    }
    let open = ac.id == Some(id) && !ac.items.is_empty();
    let mut pick = None;
    let mut moved = false;
    let force = ui.memory(|m| m.has_focus(id)) && take_ctrl_space(ui);
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
                ac.close();
            }
        });
    }
    let out = if pair { with_autopair(ui, id, text, |ui, text| show(ui, text, open)) } else { show(ui, text, open) };
    let focused = out.response.has_focus();
    if let Some(i) = pick {
        ac.apply(ui.ctx(), id, text, i);
        return (out, true, moved);
    }
    let cursor = out.cursor_range.filter(|r| r.is_empty()).map(|r| r.primary.index.0);
    if let (true, Some(ci)) = (focused, cursor) {
        if out.response.changed() || force {
            let before: String = text.chars().take(ci).collect();
            match compute(&before, force) {
                Some((token, items)) => *ac = Ac { id: Some(id), token, items, cursor: ci, ..Default::default() },
                None if ac.id == Some(id) => ac.close(),
                None => {}
            }
        } else if ac.id == Some(id) && ci != ac.cursor {
            ac.close();
        }
    }
    let test_shot = std::env::var("QUARRY_TEST_AC").is_ok() || std::env::var("QUARRY_TEST_SHELL_SIG").is_ok() || std::env::var("QUARRY_TEST_SQL").is_ok_and(|v| v.starts_with("ac") || v == "shellac");
    let pointer_on_list = test_shot || ac.rect.zip(ui.input(|i| i.pointer.hover_pos())).is_some_and(|(r, p)| r.contains(p));
    if !focused && ac.id == Some(id) && !pointer_on_list {
        ac.close();
    }
    (out, false, moved)
}

/// Draws the open suggestion list of input `id` here and inserts the clicked one. `width` fixes the
/// list width, as inside a menu; otherwise it fits the longest suggestion. Returns whether one was picked.
pub(crate) fn ac_list(ui: &mut egui::Ui, id: egui::Id, text: &mut String, ac: &mut Ac, moved: bool, width: Option<f32>) -> bool {
    if !ac.shown(id) {
        return false;
    }
    let typed: String = text.chars().take(ac.cursor).skip(ac.cursor.saturating_sub(ac.token)).collect();
    let mut click = None;
    let doc = ac.doc.as_ref().filter(|d| d.0 == ac.sel).map(|d| &d.1);
    let r = ui
        .horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            // Each card stacks its own rows; only the list and the doc sit side by side.
            ui.vertical(|ui| popup_list(ui, ac, &typed, moved, width, &mut click));
            if let Some(d) = doc {
                ui.vertical(|ui| super::js_doc::doc_card(ui, d));
            }
        })
        .response;
    ac.rect = Some(r.rect);
    let Some(i) = click else { return false };
    ac.apply(ui.ctx(), id, text, i);
    ui.memory_mut(|m| m.request_focus(id));
    true
}

/// Badge letter and color per kind, so methods, fields and names read apart at a glance.
/// Colors follow the editor's syntax colors for the same things.
fn kind_badge(note: &str) -> (&'static str, Color32) {
    match note {
        "method" | "function" | "helper" | "constructor" => ("m", C_DATE()),
        "field" | "property" => ("f", KEY()),
        "operator" => ("$", PRIMARY()),
        "keyword" => ("k", C_KEYWORD()),
        "collection" => ("c", C_STRING()),
        "package" | "module" => ("p", C_NUMBER()),
        "class" | "interface" | "enum" => ("t", C_NUMBER()),
        "date" => ("d", C_DATE()),
        "global" | "variable" | "constant" => ("v", C_OID()),
        // SQL kinds, named apart from the Mongo ones so those badges stay as they were.
        "table" => ("t", PRIMARY()),
        "view" => ("w", PRIMARY()),
        "schema" => ("d", MUTED()),
        "column" => ("c", PRIMARY()),
        "pk" => ("PK", WARNING()),
        "fn" => ("f", C_DATE()),
        "literal" => ("v", C_STRING()),
        _ => ("s", MUTED()),
    }
}

/// The label with the typed letters picked out, matched the same way as `fuzzy`.
fn label_job(label: &str, typed: &str, selected: bool) -> LayoutJob {
    let mut job = LayoutJob::default();
    let mut want = typed.chars().map(|c| c.to_ascii_lowercase()).peekable();
    let base = if selected { BRIGHT() } else { TEXT() };
    for c in label.chars() {
        let hit = want.peek() == Some(&c.to_ascii_lowercase());
        if hit {
            want.next();
        }
        job.append(c.encode_utf8(&mut [0; 4]), 0.0, mono(if hit { PRIMARY() } else { base }));
    }
    job
}

/// The suggestion popup, drawn like an IDE completion list: one tight row per item with a kind
/// badge, the label, and the kind right-aligned, a scrollbar on the right edge, and a key hint.
fn popup_list(ui: &mut egui::Ui, ac: &Ac, typed: &str, moved: bool, width: Option<f32>, click: &mut Option<usize>) {
    let (items, sel) = (&ac.items, ac.sel);
    const ROW: f32 = 22.0;
    const BADGE: f32 = 16.0;
    const PAD: f32 = 6.0;
    const VISIBLE: usize = 10;
    let note_font = FontId::proportional(TXT_CAPTION);
    // Sized to the longest row, so the scrollbar sits on the edge instead of mid-list.
    let p = ui.painter();
    let fonts = items.iter().map(|s| p.layout_no_wrap(s.text.clone(), FontId::monospace(MONO_PX), TEXT()).size().x + p.layout_no_wrap(s.sql.as_ref().map_or(s.note, |i| i.detail.as_str()).to_string(), note_font.clone(), TEXT()).size().x).fold(0.0, f32::max);
    // A lookup in progress or a skipped one gets a row of its own, laid out like a suggestion.
    let status = match (ac.loading, ac.note) {
        (true, _) if items.is_empty() && !ac.title.is_empty() => Some("Looking up values…"),
        (true, _) if items.is_empty() => Some("Looking up values in the collection…"),
        (true, _) => Some("Looking up more values…"),
        (false, n) => n,
    };
    let status_w = status.map_or(0.0, |s| p.layout_no_wrap(s.to_string(), note_font.clone(), TEXT()).size().x);
    // The heading is drawn further in than the labels, so it needs its own room.
    let title_w = if ac.title.is_empty() { 0.0 } else { p.layout_no_wrap(ac.title.clone(), note_font.clone(), TEXT()).size().x };
    let fonts = fonts.max(status_w + 16.0).max(title_w);
    let w = width.unwrap_or((fonts + BADGE + PAD * 5.0 + 16.0).clamp(220.0, 520.0));
    popup_card(ui).inner_margin(egui::Margin::symmetric(0, 4)).show(ui, |ui| {
        ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
        ui.spacing_mut().scroll = egui::style::ScrollStyle::thin();
        // SQL lists name what they list in a heading row.
        if !ac.title.is_empty() {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(w, ROW), egui::Sense::hover());
            // Cut with an ellipsis past the width cap rather than spilling out of the card.
            let mut job = LayoutJob::simple_singleline(ac.title.clone(), note_font.clone(), FAINT());
            job.wrap = egui::text::TextWrapping::truncate_at_width(w - (PAD + 6.0) * 2.0);
            let galley = ui.painter().layout_job(job);
            ui.painter().galley(egui::pos2(rect.left() + PAD + 6.0, rect.center().y - galley.size().y / 2.0), galley, FAINT());
        }
        egui::ScrollArea::vertical().max_height(ROW * VISIBLE as f32).show(ui, |ui| {
            ui.set_width(w);
            for (i, s) in items.iter().enumerate() {
                let (rect, r) = ui.allocate_exact_size(egui::vec2(w, ROW), egui::Sense::click());
                let r = r.on_hover_cursor(egui::CursorIcon::PointingHand);
                let selected = i == sel;
                let p = ui.painter();
                let row = rect.shrink2(egui::vec2(4.0, 1.0));
                if selected {
                    p.rect_filled(row, R_SM, tint(PRIMARY(), 56));
                } else if r.is_pointer_button_down_on() {
                    p.rect_filled(row, R_SM, menu_pressed());
                } else if r.hovered() {
                    // The list sits on a raised card, where the page hover would not show.
                    p.rect_filled(row, R_SM, MENU_HOVER());
                }
                let (letter, color) = kind_badge(s.note);
                let badge = egui::Rect::from_center_size(egui::pos2(row.left() + PAD + BADGE / 2.0, row.center().y), egui::vec2(BADGE, BADGE));
                p.rect_filled(badge, R_SM, tint(color, 40));
                p.text(badge.center(), egui::Align2::CENTER_CENTER, letter, FontId::monospace(TXT_CAPTION), color);
                let note = p.text(egui::pos2(row.right() - PAD - 8.0, row.center().y), egui::Align2::RIGHT_CENTER, s.sql.as_ref().map_or(s.note, |i| i.detail.as_str()), note_font.clone(), if selected { MUTED() } else { FAINT() });
                let mut job = label_job(&s.text, typed, selected);
                // The popup width is capped, so long paths are cut with an ellipsis before the kind note.
                job.wrap = egui::text::TextWrapping::truncate_at_width(note.left() - PAD - badge.right() - PAD);
                let label = p.layout_job(job);
                p.galley(egui::pos2(badge.right() + PAD, row.center().y - label.size().y / 2.0), label, TEXT());
                if selected && moved {
                    r.scroll_to_me(None);
                }
                if r.clicked() {
                    *click = Some(i);
                }
            }
        });
        // With nothing to insert yet, the status row is the whole popup.
        if !items.is_empty() {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(w, if status.is_some() { 5.0 } else { 20.0 }), egui::Sense::hover());
            ui.painter().hline(rect.x_range(), rect.top() + 2.0, egui::Stroke::new(1.0, BORDER()));
            if status.is_none() {
                // The popup is sized to its rows, so a long footer is cut with an ellipsis instead of spilling out of the card.
                let foot = items.get(sel).and_then(|s| s.sql.as_ref()).map(|i| i.foot.as_str()).filter(|f| !f.is_empty()).or(Some(ac.foot.as_str()).filter(|f| !f.is_empty())).unwrap_or("Enter or Tab to insert, Esc to close");
                let mut job = LayoutJob::simple_singleline(foot.to_string(), note_font.clone(), FAINT());
                job.wrap = egui::text::TextWrapping::truncate_at_width(w - (PAD + 4.0) * 2.0);
                let galley = ui.painter().layout_job(job);
                ui.painter().galley(egui::pos2(rect.left() + PAD + 4.0, rect.center().y + 2.0 - galley.size().y / 2.0), galley, FAINT());
            }
        }
        if let Some(text) = status {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(w, ROW), egui::Sense::hover());
            let row = rect.shrink2(egui::vec2(4.0, 1.0));
            let icon = egui::Rect::from_center_size(egui::pos2(row.left() + PAD + BADGE / 2.0, row.center().y), egui::vec2(BADGE, BADGE));
            if ac.loading {
                egui::Spinner::new().size(12.0).color(MUTED()).paint_at(ui, icon);
            } else {
                ui.painter().rect_filled(icon, R_SM, tint(WARNING(), 40));
                ui.painter().text(icon.center(), egui::Align2::CENTER_CENTER, "!", FontId::monospace(TXT_CAPTION), WARNING());
            }
            ui.painter().text(egui::pos2(icon.right() + PAD, row.center().y), egui::Align2::LEFT_CENTER, text, note_font, MUTED());
        }
    });
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

/// A character of a JavaScript identifier, as far as completion cares.
pub(crate) fn is_js_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// The caret is inside a call's object argument, like `find({ sta`, where field names and
/// operators are what fits rather than JavaScript names.
pub(crate) fn in_query_args(before: &str) -> bool {
    let word = before.chars().rev().take_while(|c| is_js_ident(*c)).map(char::len_utf8).sum::<usize>();
    let lead = &before[..before.len() - word];
    before.matches('(').count() > before.matches(')').count() && lead.trim_end().ends_with(['{', ',', ':'])
}

/// Where the end of a script sits: in a string that is a query value, in another string, a regex
/// or a comment, or in code.
#[derive(PartialEq)]
pub(crate) enum JsSpot {
    QueryString,
    Inert,
    Code,
}

/// Reads the caret's line only: strings and comments rarely span lines, template literals aside.
pub(crate) fn js_spot(before: &str) -> JsSpot {
    let start = before.rfind('\n').map_or(0, |i| i + 1);
    // `prev` is the last character outside spaces: a slash after an operator or bracket opens a
    // regex, after a name or a number it divides.
    let (mut open, mut esc, mut prev) = (None, false, '(');
    let mut chars = before[start..].char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match open {
            Some(_) if esc => esc = false,
            Some(_) if c == '\\' => esc = true,
            Some((_, q)) if c == q => open = None,
            Some(_) => {}
            None if c == '/' && chars.peek().is_some_and(|(_, n)| *n == '/') => return JsSpot::Inert,
            None if c == '/' && "(,=:[!&|?{};+-*%<>~^".contains(prev) => open = Some((i, '/')),
            None if matches!(c, '"' | '\'' | '`') => open = Some((i, c)),
            None => {}
        }
        if !c.is_whitespace() {
            prev = c;
        }
    }
    match open {
        Some((q, '"' | '\'')) if in_query_args(&before[..start + q]) => JsSpot::QueryString,
        Some(_) => JsSpot::Inert,
        None => JsSpot::Code,
    }
}

/// Byte index of the `(` still open at the end of `s`.
fn open_paren(s: &str) -> Option<usize> {
    let mut depth = 0;
    s.char_indices().rev().find(|(_, c)| {
        match c {
            ')' => depth += 1,
            '(' if depth == 0 => return true,
            '(' => depth -= 1,
            _ => {}
        }
        false
    }).map(|(i, _)| i)
}

/// The collection a call is made on: `orders` in `orders.find`, `x` in `db.collection("x").find`.
fn call_target(call: &str) -> Option<&str> {
    let recv = call.trim_end_matches(is_js_ident).strip_suffix('.')?;
    let name = match recv.strip_suffix(')') {
        Some(inner) => {
            let inner = inner.trim_end();
            let q = inner.chars().last().filter(|c| matches!(c, '"' | '\''))?;
            let inner = &inner[..inner.len() - 1];
            &inner[inner.rfind(q)? + 1..]
        }
        None => &recv[recv.trim_end_matches(is_js_ident).len()..],
    };
    (!name.is_empty()).then_some(name)
}

/// Suggestions in the Shell. `known` holds the fields of the collections loaded in tabs of this
/// database; a query object takes those of the collection its call is made on.
pub(crate) fn suggest_js(before: &str, known: &[(String, std::sync::Arc<Fields>)], colls: &[String], pkgs: &[String]) -> Option<(usize, Vec<Sug>)> {
    let ident = is_js_ident;
    // Inside a string only a query value has suggestions here; the language server adds what it knows.
    let quoted = match js_spot(before) {
        JsSpot::QueryString => true,
        JsSpot::Inert => return None,
        JsSpot::Code => false,
    };
    let tok_start = before.char_indices().rev().take_while(|(_, c)| ident(*c)).last().map(|(i, _)| i).unwrap_or(before.len());
    let token = &before[tok_start..];
    let lead = &before[..tok_start];
    let method = |m: &&str| Sug { sql: None, back: 0, text: m.to_string(), note: "method", insert: format!("{m}()"), caret: m.len() + 1 };
    let mut items: Vec<Sug> = if quoted || in_query_args(before) {
        // Inside a call's arguments: the same field, operator and value suggestions as the Query box.
        // Only the open call's arguments are passed, so braces of the function around the call
        // do not count as query nesting.
        let open = open_paren(before);
        let args = open.map_or(before, |i| &before[i + 1..]);
        let empty = Fields::new();
        let target = open.and_then(|i| call_target(&before[..i]));
        let fields: &Fields = target.and_then(|c| known.iter().find(|k| k.0 == c)).map_or(&empty, |k| &k.1);
        let mut paths: Vec<String> = fields.keys().cloned().collect();
        paths.sort_unstable();
        // `~"..."` only means something to the Query box's parser; in JavaScript `~` is bitwise NOT,
        // so text matches become regex literals.
        let (n, mut items) = suggest(args, FieldKind::Query, &paths, fields)?;
        // Date shortcuts like #today are Query box syntax, not JavaScript.
        items.retain(|x| !x.insert.starts_with(['#', '@']));
        for x in &mut items {
            if let Some(i) = x.insert.find("~\"\"") {
                x.insert.replace_range(i..i + 3, "//i");
                x.caret -= 1;
            }
        }
        return (!items.is_empty()).then_some((n, items));
    } else if let Some(obj) = lead.strip_suffix('.') {
        let name_of = |s: &str| s[s.char_indices().rev().take_while(|(_, c)| ident(*c)).last().map(|(i, _)| i).unwrap_or(s.len())..].to_string();
        let list: &[&str] = if let Some(inner) = obj.strip_suffix(')') {
            // The call this ')' closes decides: db.collection(), find(), aggregate()...
            match open_paren(inner).map(|i| name_of(&inner[..i])).as_deref() {
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
    } else {
        let mut v: Vec<Sug> = JS_KEYWORDS.iter().map(|k| Sug { sql: None, back: 0, text: k.to_string(), note: "keyword", insert: format!("{k} "), caret: k.len() + 1 }).collect();
        v.extend(JS_GLOBALS.iter().map(|g| match *g {
            "ObjectId" | "ISODate" => Sug { sql: None, back: 0, text: g.to_string(), note: "helper", insert: format!("{g}(\"\")"), caret: g.len() + 2 },
            _ => Sug { sql: None, back: 0, text: g.to_string(), note: "global", insert: g.to_string(), caret: g.len() },
        }));
        v.extend(pkgs.iter().map(|p| Sug { sql: None, back: 0, text: p.clone(), note: "package", insert: p.clone(), caret: p.chars().count() }));
        v.extend(colls.iter().filter(|c| c.chars().all(ident) && !c.starts_with(|c: char| c.is_ascii_digit())).map(|c| Sug { sql: None, back: 0, text: c.clone(), note: "collection", insert: c.clone(), caret: c.chars().count() }));
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

    /// A view must not read as a table in the SQL completion list.
    #[test]
    fn views_have_their_own_badge() {
        assert_eq!(kind_badge("table").0, "t");
        assert_eq!(kind_badge("view").0, "w");
    }

    /// Scanning BSON bytes must find the same paths, kinds and values as scanning decoded documents.
    #[test]
    fn raw_pages_scan_like_decoded_ones() {
        use mongodb::bson::{DateTime, doc, oid::ObjectId};
        let docs: Vec<_> = (0..30)
            .map(|i| doc! { "_id": ObjectId::new(), "n": i, "s": format!("v{}", i % 4), "long": "x".repeat(200), "at": DateTime::from_millis(i), "tags": ["a", "b"], "o": { "p": { "q": [{ "r": i % 3 }] } }, "maybe": if i % 2 == 0 { Bson::Null } else { Bson::Int64(i) } })
            .collect();
        let rows: Vec<db::Row> = docs.iter().map(|d| db::Row::from_doc(d).unwrap()).collect();
        assert_eq!(doc_paths(&docs), doc_paths(&rows));
        let (b, r) = (field_info(&docs), field_info(&rows));
        assert_eq!(b.len(), r.len());
        for (p, fb) in &b {
            let fr = &r[p];
            assert_eq!((fb.kind, &fb.values), (fr.kind, &fr.values), "{p}");
        }
    }

    #[test]
    fn query_help_lists_every_shortcut() {
        let h = query_help();
        assert!(VALUE_SNIPPETS.iter().all(|s| h.contains(s.0)) && TOP_SNIPPETS.iter().all(|s| h.contains(s.0)) && h.contains("#today") && !h.contains('|'));
    }

    #[test]
    fn autocomplete_contexts() {
        let paths: Vec<String> = ["status", "customer", "customer.name", "customer.tier", "total"].iter().map(|s| s.to_string()).collect();
        let (n, s) = suggest("{ cus", FieldKind::Query, &paths, &Fields::new()).unwrap();
        assert_eq!((n, s[0].text.as_str(), s[0].insert.as_str()), (3, "customer", "customer: "));
        let (_, s) = suggest("{ customer.n", FieldKind::Query, &paths, &Fields::new()).unwrap();
        assert_eq!(s[0].text, "customer.name");
        let (_, s) = suggest("{ total: { $g", FieldKind::Query, &paths, &Fields::new()).unwrap();
        assert!(s.iter().any(|x| x.text == "$gt:"));
        assert!(s[0].text.starts_with("$g"));
        let (_, s) = suggest("{ status: { $i", FieldKind::Query, &paths, &Fields::new()).unwrap();
        let i = s.iter().find(|x| x.text == "$in: [  ]").unwrap();
        assert_eq!(&i.insert[..i.insert.char_indices().nth(i.caret).unwrap().0], "$in: [ ");
        let (_, s) = suggest("{ total: ", FieldKind::Sort, &paths, &Fields::new()).unwrap();
        assert_eq!(s.len(), 2);
        let (_, s) = suggest("{ _id: Obj", FieldKind::Query, &paths, &Fields::new()).unwrap();
        assert_eq!((s[0].text.as_str(), s[0].caret), ("ObjectId(\"\")", 10));
        assert!(suggest("{ ", FieldKind::Query, &paths, &Fields::new()).is_none());
        let (_, s) = suggest("{ \"cus", FieldKind::Query, &paths, &Fields::new()).unwrap();
        assert_eq!(s[0].insert, "customer");
        let (_, s) = suggest("{ customer.name: cont", FieldKind::Query, &paths, &Fields::new()).unwrap();
        let c = &s[0];
        assert_eq!((c.text.as_str(), &c.insert[..c.insert.char_indices().nth(c.caret).unwrap().0]), ("contains", "~\""));
        assert!(db::parse_doc(&format!("{{ name: {} }}", c.insert.replace("\"\"", "\"ab\""))).is_ok());
        let (n, s) = suggest("{ at: #last2m", FieldKind::Query, &paths, &Fields::new()).unwrap();
        assert_eq!((n, s[0].text.as_str()), (7, "#last2months"));
    }

    #[test]
    fn shell_autocomplete() {
        let colls = vec!["orders".to_string(), "users".to_string()];
        let known = [("orders".to_string(), std::sync::Arc::new(field_info(&[mongodb::bson::doc! { "status": "paid", "total": 1 }])))];
        let (_, s) = suggest_js("return await db.coll", &known, &colls, &[]).unwrap();
        assert_eq!((s[0].text.as_str(), s[0].insert.as_str()), ("collection", "collection()"));
        // Text shortcuts become regex literals in JavaScript.
        let (_, s) = suggest_js("await orders.find({ status: cont", &known, &colls, &[]).unwrap();
        assert_eq!((s[0].insert.as_str(), s[0].caret), ("//i", 1));
        let (_, s) = suggest_js("await orders.upd", &known, &colls, &[]).unwrap();
        assert!(s[0].text.starts_with("update"));
        let (_, s) = suggest_js("await orders.find({}).", &known, &colls, &[]).unwrap();
        assert!(s.iter().any(|x| x.text == "toArray"));
        let (_, s) = suggest_js("db.collection('x').", &known, &colls, &[]).unwrap();
        assert!(s.iter().any(|x| x.text == "find"));
        let (_, s) = suggest_js("await orders.find({ sta", &known, &colls, &[]).unwrap();
        assert_eq!(s[0].text, "status");
        let (_, s) = suggest_js("async function run() {\n  return await orders.find({ sta", &known, &colls, &[]).unwrap();
        assert_eq!(s[0].text, "status");
        let (_, s) = suggest_js("async function run() {\n  return await orders.find({ total: { $g", &known, &colls, &[]).unwrap();
        assert_eq!(s[0].note, "operator");
        let (_, s) = suggest_js("  ord", &known, &colls, &[]).unwrap();
        assert_eq!(s[0].text, "orders");
        let (_, s) = suggest_js("  aw", &known, &colls, &[]).unwrap();
        assert_eq!(s[0].insert, "await ");
    }

    #[test]
    fn date_field_suggestions() {
        let paths = vec!["status".to_string(), "createdAt".to_string(), "format".to_string()];
        let docs = [
            mongodb::bson::doc! { "createdAt": mongodb::bson::DateTime::now(), "status": "active", "format": "pdf", "tags": ["a", "b"], "n": 5 },
            mongodb::bson::doc! { "createdAt": mongodb::bson::DateTime::now(), "status": "active", "format": "say \"hi\"", "tags": ["a"], "n": 7 },
        ];
        let dates = field_info(&docs);
        // A date field lists date shortcuts right after the colon, with nothing typed yet.
        let (n, s) = suggest("{ createdAt: ", FieldKind::Query, &paths, &dates).unwrap();
        assert_eq!((n, s[0].text.as_str()), (0, "#today"));
        assert!(s.iter().any(|x| x.note == "between two days, both included"));
        // Fields not loaded yet are judged by their name.
        assert!(suggest("{ \"providerOrder.syncedAt\": ", FieldKind::Query, &paths, &dates).is_some());
        // Other fields list their own values right after the colon, and nothing else.
        let (_, s) = suggest("{ status: ", FieldKind::Query, &paths, &dates).unwrap();
        assert!(s.len() == 1 && s[0].text == "active");
        assert!(suggest("{ n: { $gt: ", FieldKind::Query, &paths, &dates).is_none());
        let (_, s) = suggest("{ createdAt: #last", FieldKind::Query, &paths, &dates).unwrap();
        assert!(s.iter().all(|x| x.text.to_ascii_lowercase().contains("last")));
        // Values from the loaded documents, most frequent first, and inside quotes as soon as one is typed.
        let (_, s) = suggest("{ status: a", FieldKind::Query, &paths, &dates).unwrap();
        assert_eq!((s[0].text.as_str(), s[0].insert.as_str(), s[0].note), ("active", "\"active\"", "value"));
        let (_, s) = suggest("{ format: \"", FieldKind::Query, &paths, &dates).unwrap();
        assert!(s.iter().any(|x| x.insert == "say \\\"hi\\\""));
        // Snippets suit the field's kind.
        let names = |b: &str| suggest(b, FieldKind::Query, &paths, &dates).map(|(_, s)| s.into_iter().map(|x| x.text).collect::<Vec<_>>()).unwrap_or_default();
        assert!(names("{ status: con").contains(&"contains".into()));
        assert!(!names("{ n: con").contains(&"contains".into()));
        assert!(names("{ n: bet").contains(&"between".into()));
        assert!(names("{ tags: has").contains(&"hasItem".into()) && !names("{ status: has").contains(&"hasItem".into()));
        let (_, s) = suggest("{ o", FieldKind::Query, &paths, &dates).unwrap();
        let or = s.iter().find(|x| x.text == "$or").unwrap();
        assert_eq!((or.insert.as_str(), or.caret), ("$or: [{  }, {  }]", 8));
        assert!(names("{ a: { o").iter().all(|x| x != "$or"));
        // Conditions inside $or take field names again; $elemMatch adds the items' own fields.
        assert!(names("{ $or: [{ sta").contains(&"status".into()));
        assert!(names("{ $or: [{ n: 1 }, { sta").contains(&"status".into()));
        let paths2 = vec!["tags".to_string(), "items.sku".to_string()];
        let (_, s) = suggest("{ items: { $elemMatch: { sk", FieldKind::Query, &paths2, &dates).unwrap();
        assert_eq!(s[0].insert, "sku: ");
        // No value shortcuts after an operator's colon.
        assert!(!names("{ n: { $gt: ex").contains(&"exists".into()));
        // The Shell gets the same values and kind-specific shortcuts inside find(), in JavaScript form.
        let users = field_info(&[mongodb::bson::doc! { "role": "admin", "email": "a.b@c.io" }]);
        let known = [("orders".to_string(), std::sync::Arc::new(dates.clone())), ("users".to_string(), std::sync::Arc::new(users))];
        let js = |b: &str| suggest_js(b, &known, &[], &[]).map(|(_, s)| s.into_iter().map(|x| x.insert).collect::<Vec<_>>()).unwrap_or_default();
        assert_eq!(js("orders.find({ status: "), ["\"active\""]);
        assert!(js("orders.find({ format: \"").contains(&"pdf".into()));
        assert!(js("orders.find({ status: con").contains(&"//i".into()) && !js("orders.find({ n: con").contains(&"//i".into()));
        assert!(js("orders.find({ createdAt: ").iter().all(|x| !x.starts_with(['#', '@'])) && !js("orders.find({ createdAt: ").is_empty());
        assert!(js("log(\"sta").is_empty());
        // No date templates inside a string: they are code.
        assert!(js("orders.find({ createdAt: \"").is_empty());
        // Each call takes its own collection's fields, also through db.collection().
        assert_eq!(js("users.find({ role: "), ["\"admin\""]);
        assert_eq!(js("db.collection(\"users\").find({ role: "), ["\"admin\""]);
        assert!(js("users.find({ status: ").is_empty() && js("unknown.find({ status: ").is_empty());
        // A dot inside a value is text, not a method call.
        assert_eq!(js("users.find({ email: \"a."), ["a.b@c.io"]);
        assert!(js("users.find({ email: \"db.").is_empty());
        // Strings that are not query values, comments and regexes are left alone.
        assert!(js_spot("console.log(\"some text") == JsSpot::Inert && js_spot("x // it's") == JsSpot::Inert);
        assert!(js_spot("users.find({ name: /it's/, sta") == JsSpot::Code && js_spot("a / b, 'x") == JsSpot::Inert);
        assert!(js_spot("users.find({ role: 'ad") == JsSpot::QueryString);
        assert_eq!(js("users.find({ name: /it's/, ro"), ["role: "]);
        // And the language server gets the documents' shape, text fields with their values.
        let ty = ts_type(&dates);
        assert!(ty.contains("status?: \"active\" | (string & {})") && ty.contains("tags?: Array<\"a\" | \"b\" | (string & {})>"), "{ty}");
        assert!(ty.contains("createdAt?: Date") && ty.contains("n?: number") && ty.contains("format?: \"pdf\" | \"say \\\"hi\\\"\""), "{ty}");
        let nested = ts_type(&field_info(&[mongodb::bson::doc! { "items": [{ "sku": "A" }], "my-key": { "x": 1 } }]));
        assert_eq!(nested, "{ items?: Array<{ sku?: \"A\" | (string & {}); [key: string]: any }>; \"my-key\"?: { x?: number; [key: string]: any }; [key: string]: any }");
        // A value picked inside single quotes is escaped for them.
        let docs2 = [mongodb::bson::doc! { "s": "it's" }];
        let (_, s) = suggest("{ s: 'it", FieldKind::Query, &paths, &field_info(&docs2)).unwrap();
        assert_eq!(s[0].insert, "it\\'s");
        assert!(date_like("startDate") && date_like("syncedAt") && date_like("timestamp") && !date_like("status"));
    }

    #[test]
    fn server_values() {
        assert_eq!(value_field("{ status: \"dr"), Some("status"));
        assert_eq!(value_field("{ a.b: 4"), Some("a.b"));
        assert_eq!((value_field("{ n: { $gt: "), value_field("{ sta")), (None, None));
        assert_eq!(value_field("{ items: { $elemMatch: { sku: \"a"), None);
        assert_eq!(value_at("{ a: 1, name: \"al ex"), Some(("name", Some("al ex"))));
        assert_eq!(value_at("{ name: 'it\\'s"), Some(("name", Some("it\\'s"))));
        assert_eq!(value_at("{ n: 4"), Some(("n", None)));
        assert_eq!(value_at("{ name: \"al\", "), None);
        let mut f = field_info(&[mongodb::bson::doc! { "status": "active" }]);
        let server = HashMap::from([("status".to_string(), vec![Bson::from("archived"), Bson::from("active")]), ("kind".to_string(), vec![Bson::from(3)])]);
        merge_values(&mut f, &server);
        assert_eq!(f["status"].values, [Bson::from("active"), Bson::from("archived")]);
        assert_eq!(f["kind"].kind, "int");
        // A field only the server knows still suggests its values.
        let (_, s) = suggest("{ status: \"ar", FieldKind::Query, &[], &f).unwrap();
        assert_eq!(s[0].text, "archived");
        // A dot typed in a value keeps matching values instead of looking for sub-fields.
        let server = HashMap::from([("user".to_string(), vec![Bson::from("alex.p@x.com"), Bson::from("alexAdmin")])]);
        merge_values(&mut f, &server);
        let (_, s) = suggest("{ user: \"alex.", FieldKind::Query, &["user".into()], &f).unwrap();
        assert_eq!(s.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), ["alex.p@x.com"]);
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
