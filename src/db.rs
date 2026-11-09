//! Everything that talks to MongoDB or converts its data. The UI never blocks: each call is a
//! tokio task that sends one `Msg` back and asks for a repaint.

use futures_util::{StreamExt, TryStreamExt};
use mongodb::bson::{Bson, Document, RawBsonRef, RawDocument, RawDocumentBuf, doc};
use mongodb::options::ClientOptions;
use mongodb::Client;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

mod analysis;
mod code;
mod dates;
mod explain;
mod import;
mod js_ls;
mod log;
mod shell;
pub mod sql;
mod tools;
mod worker;
pub use analysis::*;
pub use code::*;
pub use dates::*;
pub use explain::*;
pub use import::*;
pub use js_ls::*;
pub use log::*;
pub use shell::*;
pub use tools::*;
pub use worker::*;

/// Whole-query column data actions read at most this many documents.
pub const COL_VALUES_CAP: i64 = 100_000;
/// Most values of one field kept for the query box's suggestions.
pub const FIELD_VALUES_MAX: usize = 200;
/// Largest collection whose field values are looked up by typed text without an index on the field.
pub const PREFIX_SCAN_MAX: u64 = 100_000;
pub const PAGE_SIZE: u64 = 50; // paginationLimit in the original
/// The most documents a Documents page can show, from the setting.
pub const PAGE_SIZE_MAX: u64 = 500;
pub const DEFAULT_TIMEOUT_MS: u64 = 5000; // Settings.connectionTimeout in the original
pub const UNDO_MAX: usize = 50; // maxActionsLength
pub const UNDO_MAX_AGE_S: u64 = 86_400; // maxActionsAge

pub type Res<T> = Result<T, String>;

/// Whether a server error means the user lacks a privilege, in the server's or Atlas's wording.
pub fn is_unauthorized(e: &str) -> bool {
    e.contains("(Unauthorized)") || e.contains("not authorized") || e.contains("is not allowed to do action")
}

/// A loaded document kept as its BSON bytes. Decoded, a document takes about 11 times the memory
/// (every field and subdocument is its own allocation), so only rows that are open keep a decoded copy.
#[derive(Clone)]
pub struct Row {
    raw: RawDocumentBuf,
    doc: std::cell::OnceCell<Document>,
}

impl Row {
    pub fn from_raw(raw: RawDocumentBuf) -> Row {
        Row { raw, doc: Default::default() }
    }

    /// Fails only for keys BSON cannot hold, such as ones with a NUL character.
    pub fn from_doc(d: &Document) -> Res<Row> {
        RawDocumentBuf::from_document(d).map(Row::from_raw).map_err(|err| err.to_string())
    }

    /// A decoded copy; the worker checks every page with this, so later decodes cannot fail.
    pub fn decode(&self) -> Res<Document> {
        match self.doc.get() {
            Some(d) => Ok(d.clone()),
            None => self.raw.to_document().map_err(|err| format!("A document could not be read: {err}")),
        }
    }

    /// A decoded copy for actions, not kept.
    pub fn owned(&self) -> Document {
        self.decode().unwrap_or_default()
    }

    /// The decoded document, kept until `forget`, for rows that are open.
    pub fn doc(&self) -> &Document {
        self.doc.get_or_init(|| self.raw.to_document().unwrap_or_default())
    }

    pub fn forget(&mut self) {
        self.doc.take();
    }

    /// The value at a dotted path, read from the bytes without decoding the rest of the document.
    pub fn value(&self, path: &str) -> Option<RawBsonRef<'_>> {
        get_path_raw(&self.raw, path)
    }

    pub fn id(&self) -> Option<Bson> {
        self.raw.get("_id").ok().flatten().and_then(|v| Bson::try_from(v).ok())
    }

    pub fn raw(&self) -> &RawDocument {
        &self.raw
    }
}

#[derive(Clone)]
pub struct Target {
    pub client: Client,
    pub db: String,
    pub coll: String,
    pub timeout: Duration,
}

impl Target {
    fn c(&self) -> mongodb::Collection<Document> {
        self.client.database(&self.db).collection(&self.coll)
    }

    fn at(&self) -> (String, String) {
        (self.db.clone(), self.coll.clone())
    }
}

#[derive(Clone, Default)]
pub struct Query {
    pub filter: Document,
    pub sort: Option<Document>,
    pub projection: Option<Document>,
    /// From a pasted shell query such as `.skip(10).limit(5)`; paging happens inside this window.
    pub skip: u64,
    pub limit: Option<u64>,
    /// The index to use: its keys as a document, or its name as a string.
    pub hint: Option<Bson>,
    pub collation: Option<Document>,
}

/// The driver's collation for a query's collation document, applied to every action on the
/// documents the table shows so they match the same ones. find_query checked it when the query
/// ran, so one that fails to convert here is left out instead of failing the action.
pub fn collation_of(d: Option<Document>) -> Option<mongodb::options::Collation> {
    d.and_then(|d| mongodb::bson::from_document(d).ok())
}

/// The documents an action works on: the shown query's filter and collation, so the action
/// matches the same documents as the table.
#[derive(Clone, Default)]
pub struct Scope {
    pub filter: Document,
    pub collation: Option<Document>,
}

impl Query {
    pub fn scope(&self) -> Scope {
        Scope { filter: self.filter.clone(), collation: self.collation.clone() }
    }

    pub fn driver_hint(&self) -> Option<mongodb::options::Hint> {
        match &self.hint {
            Some(Bson::Document(d)) => Some(mongodb::options::Hint::Keys(d.clone())),
            Some(Bson::String(s)) => Some(mongodb::options::Hint::Name(s.clone())),
            _ => None,
        }
    }

    /// Checked when the query is built, so a typo in a collation shows before anything runs.
    pub fn driver_collation(&self) -> Res<Option<mongodb::options::Collation>> {
        self.collation.clone().map(|d| mongodb::bson::from_document(d).map_err(|e| format!("Collation: {e}"))).transpose()
    }
}

pub enum UndoKind {
    Insert { id: Bson },
    /// An import: undo deletes these ids.
    InsertMany { ids: Vec<Bson> },
    Update { old: Document, new: Document },
    Delete { old: Document },
    /// Undo puts these documents back.
    DeleteMany { old: Vec<Document> },
}

pub struct UndoAction {
    pub at: Instant,
    pub conn: u64,
    pub db: String,
    pub coll: String,
    pub kind: UndoKind,
}

pub enum ExplainOf {
    Find(Box<Query>),
    Aggregate(Vec<Document>),
}

pub enum Msg {
    /// Connected (or not); a tunnel, if any, must be kept alive as long as the client is used.
    Connected { conn: u64, res: Res<(Client, Vec<String>, Option<crate::ssh::Tunnel>)> },
    /// Result of the connection form's Test button.
    Tested { res: Res<String> },
    /// Collection names (sorted) and which of them are views.
    Collections { conn: u64, db: String, res: Res<(Vec<String>, Vec<String>)> },
    /// A read command answered (users, roles, stats, view definition); `tag` says which.
    Command { tag: String, res: Res<Document> },
    /// A database-level change finished; the collection list of `db` is reloaded.
    /// `copied` names the collection a duplicate wrote, to end its spinner.
    DbChanged { conn: u64, db: String, copied: Option<String>, res: Res<String> },
    Docs { req: u64, res: Res<(Vec<Row>, u64, u128)> },
    Indexes { req: u64, res: Res<Vec<Document>> },
    /// Usage and suggestions for the collection named `key` ("db.coll").
    IndexInfo { key: String, res: Res<IndexInfo> },
    Aggregated { req: u64, res: Res<(Vec<Row>, u128)> },
    /// One pipeline builder stage's preview: documents out of the stage (none when counting them timed out), the first of them, time.
    StagePreview { req: u64, res: Res<(Option<u64>, Vec<Document>, u128)> },
    /// The server's explain output for a query or pipeline.
    Explained { req: u64, res: Res<Document> },
    /// Up to 5 matching documents as (before, after) the given update stages, and the total match count.
    UpdatePreview { stages: Vec<Document>, res: Res<(Vec<(Document, Document)>, u64)> },
    /// A write finished. `undo` is filled when the write can be reverted. `retry` is the insert
    /// dialog's (db, coll, text), to reopen it if the server refused the document. `at` is the
    /// written (db, coll), so the tabs showing it reload instead of whichever tab is current.
    Written { at: (String, String), res: Res<String>, undo: Option<UndoAction>, reload: bool, retry: Option<(String, String, String)> },
    /// A staged document edit was written (or refused); `tag` names the draft.
    Saved { tag: String, res: Res<String>, undo: Option<UndoAction> },
    Info(Res<String>),
    /// Schema analysis of `docs` documents.
    Schema { req: u64, res: Res<(Vec<FieldStat>, usize, u128)> },
    /// A line the Shell script printed (console.log/warn/error), or a progress note.
    ShellLog { req: u64, level: String, text: String },
    /// The Shell script finished: its return value as canonical Extended JSON `{"v": ...}`.
    ShellDone { req: u64, res: Res<String>, ms: u128 },
    /// One change event from the Watch view's change stream `req`.
    WatchEvent { req: u64, doc: Document },
    /// The change stream `req` ended: stopped (Ok) or failed.
    WatchDone { req: u64, res: Res<()> },
    /// The shell editor's language server answered request `req`.
    JsLs { req: u64, out: JsOut },
    /// The shell editor's language server stopped, idle or `failed`.
    JsLsStopped { failed: bool },
    /// A result to show in a dialog, such as a distinct-values count.
    Notice { title: String, body: String },
    /// "Group by and count" finished: one `{ _id, count }` per value, most frequent first.
    Grouped { conn: u64, db: String, coll: String, path: String, pipeline: Vec<Document>, res: Res<Vec<Document>> },
    /// A column data action over the whole query; the bool says the document cap was reached.
    ColumnValues { op: ColOp, coll: String, path: String, res: Res<(ColOut, bool)> },
    /// A field's values across the collection `key` ("conn/db/coll"), for suggestions.
    /// `prefix`: only string values starting with it, looked up while someone types one.
    FieldValues { key: String, path: String, prefix: Option<String>, res: Res<Option<Vec<Bson>>> },
    /// Preview values of referenced documents, keyed by `compact(_id)`.
    Related { req: u64, res: Res<Vec<(String, String)>> },
    /// Which collection of `db` holds `value` as an `_id`, and the field to preview it with.
    RefFound { conn: u64, db: String, coll: String, path: String, value: Bson, res: Res<Option<(String, String)>> },
    Compared { req: u64, res: Res<Compared> },
    /// Relations found by analysis: (collection, path, referenced collection, preview field).
    Analyzed { conn: u64, db: String, res: Res<Vec<(String, String, String, String)>> },
    /// A line printed by mongodump or mongorestore.
    ToolLog(String),
    ToolDone(Res<String>),
    /// Update check: Some(version) once a newer version is installed.
    Update(Res<Option<String>>),
    /// The MongoDB Database Tools were installed or updated to this version; None when already current.
    ToolsInstalled(Res<Option<String>>),
}

/// How to reach the server: directly, through an SSH tunnel, or through a SOCKS5 proxy.
#[derive(Clone, Default)]
pub struct Net {
    pub ssh: Option<(crate::ssh::SshConf, crate::ssh::SshSecrets)>,
    pub proxy: Option<Proxy>,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Debug)]
#[serde(default)]
pub struct Proxy {
    pub host: String,
    pub port: u16,
    pub user: String,
    #[serde(skip)]
    pub password: Option<String>,
}

pub fn short(b: &Bson) -> String {
    match b {
        Bson::ObjectId(o) => o.to_hex(),
        Bson::String(s) => s.clone(),
        other => other.clone().into_relaxed_extjson().to_string(),
    }
}

// ---------- text <-> BSON ----------

/// Accepts strict JSON, JSON5 (unquoted keys, single quotes, trailing commas) and the common
/// shell helpers ObjectId(), ISODate(), new Date(), NumberLong(), NumberInt(), NumberDecimal().
pub fn parse_value(text: &str) -> Res<Bson> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Bson::Document(Document::new()));
    }
    let v: serde_json::Value = json5::from_str(&shell_to_ejson(text)).map_err(|err| format!("Not valid JSON: {err}"))?;
    Bson::try_from(v).map_err(|err| format!("Not valid Extended JSON: {err}"))
}

/// Query text with its shortcuts (`~"text"`, `#today`, `@moment`) written out as shell syntax, for
/// JavaScript, which has none of them. None when it has none or does not parse.
pub fn expand_shortcuts(text: &str) -> Option<String> {
    if !text.contains(['~', '#', '@']) {
        return None;
    }
    parse_doc(text).ok().map(|d| shell_text(&Bson::Document(d)))
}

pub fn parse_doc(text: &str) -> Res<Document> {
    match parse_value(text)? {
        Bson::Document(d) => Ok(d),
        _ => Err("Expected an object like { field: value }".into()),
    }
}

pub fn parse_pipeline(text: &str) -> Res<Vec<Document>> {
    match parse_value(if text.trim().is_empty() { "[]" } else { text })? {
        Bson::Array(a) => a
            .into_iter()
            .map(|s| match s {
                Bson::Document(d) => Ok(d),
                _ => Err("Every pipeline stage must be an object".to_string()),
            })
            .collect(),
        Bson::Document(d) => Ok(vec![d]),
        _ => Err("Expected an array of stages like [{ $match: {} }]".into()),
    }
}

fn shell_to_ejson(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let chars: Vec<char> = src.chars().collect();
    let now = mongodb::bson::DateTime::now().timestamp_millis();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '#' || c == '@' {
            // Dates and ranges hold '-' and ':'; ISO moments also '+' and '.'.
            let part = |ch: char| ch.is_ascii_alphanumeric() || ch == '-' || ch == ':' || (c == '@' && (ch == '+' || ch == '.'));
            let end = (i + 1..chars.len()).find(|&k| !part(chars[k])).unwrap_or(chars.len());
            let word: String = chars[i + 1..end].iter().collect();
            if let Some(rep) = if c == '#' { date_shortcut(&word, now) } else { moment(&word, now) } {
                out.push_str(&rep);
                i = end;
                continue;
            }
        }
        // `~"some text"` or `~word`: contains, any case, with regex characters taken literally.
        if c == '~' {
            let q = chars.get(i + 1).copied().filter(|&q| q == '"' || q == '\'');
            let (mut text, mut k) = (String::new(), i + 1 + usize::from(q.is_some()));
            while k < chars.len() && q.map_or(chars[k].is_alphanumeric() || "-_.".contains(chars[k]), |q| chars[k] != q) {
                if chars[k] == '\\' && k + 1 < chars.len() {
                    k += 1;
                }
                text.push(chars[k]);
                k += 1;
            }
            // Nothing to look for: the `~` stays, so the query fails to parse instead of matching every text.
            if !text.is_empty() {
                let escaped = regex_escape(&text);
                out.push_str(&format!("{{\"$regex\":{},\"$options\":\"i\"}}", serde_json::Value::String(escaped)));
                i = (k + usize::from(q.is_some())).min(chars.len());
                continue;
            }
        }
        if c == '"' || c == '\'' {
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != c {
                i += if chars[i] == '\\' { 2 } else { 1 };
            }
            i = (i + 1).min(chars.len());
            out.extend(&chars[start..i]);
            continue;
        }
        // `a.b: 1`: a dotted path as a key, which JSON5 (like JavaScript) only takes quoted. The
        // suggestion list writes them unquoted, as people do.
        if (c.is_ascii_alphabetic() || c == '_' || c == '$') && out.trim_end().ends_with(['{', ',']) {
            let end = (i..chars.len()).find(|&k| !(chars[k].is_ascii_alphanumeric() || "_$.".contains(chars[k]))).unwrap_or(chars.len());
            let colon = chars[end..].iter().find(|ch| !ch.is_whitespace()) == Some(&':');
            if colon && chars[i..end].contains(&'.') {
                out.push('"');
                out.extend(&chars[i..end]);
                out.push('"');
                i = end;
                continue;
            }
        }
        if c.is_ascii_alphabetic() && (i == 0 || !(chars[i - 1].is_ascii_alphanumeric() || chars[i - 1] == '$' || chars[i - 1] == '_')) {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_alphanumeric() {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if word == "new" {
                continue; // `new Date(...)` is treated like `Date(...)`
            }
            if j < chars.len() && chars[j] == '(' {
                if let Some(close) = chars[j..].iter().position(|&ch| ch == ')') {
                    let arg: String = chars[j + 1..j + close].iter().collect();
                    let arg = arg.trim().trim_matches(|ch| ch == '"' || ch == '\'').to_string();
                    let rep = match word.as_str() {
                        "ObjectId" => Some(format!("{{\"$oid\":\"{arg}\"}}")),
                        "ISODate" | "Date" => Some(format!("{{\"$date\":\"{arg}\"}}")),
                        "NumberLong" => Some(format!("{{\"$numberLong\":\"{arg}\"}}")),
                        "NumberDecimal" => Some(format!("{{\"$numberDecimal\":\"{arg}\"}}")),
                        "NumberInt" => Some(arg.clone()),
                        _ => None,
                    };
                    if let Some(rep) = rep {
                        out.push_str(&rep);
                        i = j + close + 1;
                        continue;
                    }
                }
            }
            out.push_str(&word);
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Text for the editor: relaxed Extended JSON, except 64-bit integers keep their marker so
/// saving never silently turns a long into an int (relaxed form would drop that type).
pub fn to_edit_json(d: &Document) -> String {
    to_edit_value(&Bson::Document(d.clone()))
}

/// Pretty editor text for any value, with the same long-integer rule as `to_edit_json`.
pub fn to_edit_value(b: &Bson) -> String {
    serde_json::to_string_pretty(&edit_json(b)).unwrap_or_default()
}

/// One-line form of `to_edit_value`, for JSON lines.
pub fn to_edit_line(b: &Bson) -> String {
    edit_json(b).to_string()
}

fn edit_json(b: &Bson) -> serde_json::Value {
    match b {
        Bson::Int64(n) => serde_json::json!({ "$numberLong": n.to_string() }),
        Bson::Document(d) => serde_json::Value::Object(d.iter().map(|(k, v)| (k.clone(), edit_json(v))).collect()),
        Bson::Array(a) => serde_json::Value::Array(a.iter().map(edit_json).collect()),
        other => other.clone().into_relaxed_extjson(),
    }
}

/// Text with every regex character taken literally.
pub fn regex_escape(text: &str) -> String {
    text.chars().fold(String::new(), |mut s, ch| {
        if "\\^$.|?*+()[]{}".contains(ch) {
            s.push('\\');
        }
        s.push(ch);
        s
    })
}

pub fn compact(b: &Bson) -> String {
    b.clone().into_relaxed_extjson().to_string()
}

pub fn type_name(b: &Bson) -> &'static str {
    match b {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Document(_) => "object",
        Bson::Array(_) => "array",
        Bson::Boolean(_) => "bool",
        Bson::Null | Bson::Undefined => "null",
        Bson::Int32(_) => "int",
        Bson::Int64(_) => "long",
        Bson::ObjectId(_) => "objectId",
        Bson::DateTime(_) => "date",
        Bson::Decimal128(_) => "decimal",
        b @ Bson::Binary(_) if uuid_of(b).is_some() => "uuid",
        Bson::Binary(_) => "binary",
        Bson::RegularExpression(_) => "regex",
        Bson::Timestamp(_) => "timestamp",
        _ => "other",
    }
}

/// Field-level changes between two documents, as dotted paths, for the save preview.
pub fn diff(old: &Document, new: &Document) -> Vec<(String, Option<Bson>, Option<Bson>)> {
    fn walk(p: &str, a: &Document, b: &Document, out: &mut Vec<(String, Option<Bson>, Option<Bson>)>) {
        for (k, va) in a {
            let path = if p.is_empty() { k.clone() } else { format!("{p}.{k}") };
            match (va, b.get(k)) {
                (Bson::Document(x), Some(Bson::Document(y))) => walk(&path, x, y, out),
                (_, Some(vb)) if vb == va => {}
                (_, vb) => out.push((path, Some(va.clone()), vb.cloned())),
            }
        }
        for (k, vb) in b {
            if !a.contains_key(k) {
                let path = if p.is_empty() { k.clone() } else { format!("{p}.{k}") };
                out.push((path, None, Some(vb.clone())));
            }
        }
    }
    let mut out = Vec::new();
    walk("", old, new, &mut out);
    out
}

/// Pipeline-form update stages. `$unset` takes an array of paths, which every server with
/// pipeline updates accepts.
pub fn update_stages(set: Document, unset: Vec<String>) -> Vec<Document> {
    let mut v = Vec::new();
    if !set.is_empty() {
        v.push(doc! { "$set": set });
    }
    if !unset.is_empty() {
        v.push(doc! { "$unset": unset });
    }
    v
}

pub fn has_write_stage(p: &[Document]) -> bool {
    p.iter().any(|s| s.contains_key("$out") || s.contains_key("$merge"))
}

/// Documents a builder stage previews at first, and how many "More" adds (samplesCount, renderer
/// line 112206 and 109918).
pub const STAGE_SAMPLE: i64 = 10;

/// A preview larger than this many characters of JSON is cut, with the original's own formula
/// (getResultsForStage, renderer line 112058): keep floor(100000 / (chars / 10)) documents, at least 1.
pub fn trim_sample(mut docs: Vec<Document>) -> Vec<Document> {
    const MAX_CHARS: usize = 100_000;
    let chars: usize = docs.iter().map(|d| compact(&Bson::Document(d.clone())).len()).sum::<usize>() + docs.len().saturating_sub(1) + 2;
    if chars > MAX_CHARS {
        let keep = ((MAX_CHARS as f64 / (chars as f64 / 10.0)).floor() as usize).max(1);
        docs.truncate(keep);
    }
    docs
}

/// Stages offered by the builder, most used first (topStages, renderer line 111389), with the
/// original's examples (AggregationStages, 108071) in shell syntax.
pub const STAGES: &[(&str, &str)] = &[
    ("$match", "{ author: 'dave' }"),
    ("$group", "{\n  _id: null,\n  count: { $sum: 1 }\n}"),
    ("$unwind", "'$sizes'"),
    ("$project", "{ title: 1, author: 1 }"),
    ("$addFields", "{ totalScore: { $add: ['$totalHomework', '$totalQuiz'] } }"),
    ("$lookup", "{\n  from: 'inventory',\n  localField: 'item',\n  foreignField: 'sku',\n  as: 'inventory_docs'\n}"),
    ("$count", "'total'"),
    ("$sort", "{ age: -1, posts: 1 }"),
    ("$sample", "{ size: 3 }"),
    ("$limit", "5"),
    ("$skip", "5"),
    ("$set", "{ status: 'active' }"),
    ("$unset", "['isbn', 'copies']"),
    ("$replaceRoot", "{ newRoot: '$name' }"),
    ("$replaceWith", "'$name'"),
    ("$bucket", "{\n  groupBy: '$price',\n  boundaries: [0, 200, 400],\n  default: 'Other'\n}"),
    ("$bucketAuto", "{ groupBy: '$price', buckets: 4 }"),
    ("$facet", "{\n  byTag: [{ $sortByCount: '$tags' }]\n}"),
    ("$sortByCount", "'$tags'"),
    ("$graphLookup", "{\n  from: 'employees',\n  startWith: '$reportsTo',\n  connectFromField: 'reportsTo',\n  connectToField: 'name',\n  as: 'reportingHierarchy'\n}"),
    ("$unionWith", "{ coll: 'warehouses', pipeline: [] }"),
    ("$densify", "{ field: 'timestamp', range: { step: 1, unit: 'hour', bounds: 'full' } }"),
    ("$fill", "{ output: { score: { value: 0 } } }"),
    ("$setWindowFields", "{\n  partitionBy: '$state',\n  sortBy: { orderDate: 1 },\n  output: { total: { $sum: '$quantity', window: { documents: ['unbounded', 'current'] } } }\n}"),
    ("$redact", "{ $cond: { if: { $eq: ['$level', 5] }, then: '$$PRUNE', else: '$$DESCEND' } }"),
    ("$geoNear", "{\n  near: { type: 'Point', coordinates: [-73.99279, 40.719296] },\n  distanceField: 'dist.calculated',\n  maxDistance: 2,\n  spherical: true\n}"),
    ("$collStats", "{ storageStats: {} }"),
    ("$indexStats", "{}"),
    ("$out", "'authors'"),
    ("$merge", "{ into: 'reporting', on: '_id', whenMatched: 'replace', whenNotMatched: 'insert' }"),
];

/// Pipeline up to and including stage `i`, capped so a preview stays cheap.
/// Write stages are dropped so a preview can never modify data.
pub fn stage_preview(p: &[Document], i: usize) -> Option<Vec<Document>> {
    let mut out: Vec<Document> = p.get(..=i)?.iter().filter(|s| !has_write_stage(std::slice::from_ref(s))).cloned().collect();
    out.push(doc! { "$limit": 20 });
    Some(out)
}

// ---------- pasted shell queries ----------

// ---------- schema ----------

// ---------- import ----------

// ---------- field actions ----------

/// `items.2.sku` becomes `items.sku`, which is how MongoDB queries match inside arrays.
pub fn strip_indices(path: &str) -> String {
    path.split('.').filter(|s| s.parse::<usize>().is_err()).collect::<Vec<_>>().join(".")
}

/// The value as JavaScript's JSON.stringify would write it (ObjectId and dates as strings).
fn plain_json(b: &Bson) -> serde_json::Value {
    use serde_json::Value as V;
    match b {
        Bson::ObjectId(o) => V::String(o.to_hex()),
        Bson::DateTime(t) => V::String(t.try_to_rfc3339_string().unwrap_or_default()),
        Bson::Int64(n) => V::from(*n),
        Bson::Document(d) => V::Object(d.iter().map(|(k, v)| (k.clone(), plain_json(v))).collect()),
        Bson::Array(a) => V::Array(a.iter().map(plain_json).collect()),
        other => other.clone().into_relaxed_extjson(),
    }
}

/// The original's "Field size": JSON.stringify(value).length (renderer line 101291).
pub fn field_size(b: &Bson) -> usize {
    plain_json(b).to_string().len()
}

/// The original's groupCountField pipeline: match, unwind arrays, group by the field, sort by count.
pub fn group_count_pipeline(path: &str, value: &Bson, filter: Document) -> Res<Vec<Document>> {
    let segs: Vec<&str> = path.split('.').collect();
    let numeric: Vec<usize> = segs.iter().enumerate().filter(|(_, s)| s.parse::<usize>().is_ok()).map(|(i, _)| i).collect();
    if numeric.len() > 1 {
        return Err("Grouping does not support fields nested more than one array deep.".into());
    }
    let mut p = vec![doc! { "$match": filter }];
    let mut group_path = path.to_string();
    if let Some(&i) = numeric.first() {
        p.push(doc! { "$unwind": format!("${}", segs[..i].join(".")) });
        group_path = strip_indices(path);
    } else if matches!(value, Bson::Array(_)) {
        p.push(doc! { "$unwind": format!("${path}") });
    }
    p.push(doc! { "$group": { "_id": format!("${group_path}"), "count": { "$sum": 1 } } });
    p.push(doc! { "$sort": { "count": -1 } });
    Ok(p)
}

// ---------- typed values for inline editing ----------

// The useful subset of the original's fieldTypes table (renderer line 30729): string, number kinds,
// bool, date, ObjectId, null, object, array, regular expression and UUID.
pub const KINDS: [&str; 13] = ["string", "int", "long", "double", "decimal", "bool", "date", "objectId", "null", "object", "array", "regex", "uuid"];

fn uuid_of(b: &Bson) -> Option<mongodb::bson::Uuid> {
    match b {
        Bson::Binary(bin) if bin.subtype == mongodb::bson::spec::BinarySubtype::Uuid => bin.to_uuid().ok(),
        _ => None,
    }
}

/// The original flags whole numbers in this range as likely Unix timestamps in seconds
/// (fieldTypes.number.getInfo, renderer line 31150).
pub fn possible_timestamp(b: &Bson) -> Option<String> {
    let n = match b {
        Bson::Int32(n) => *n as i64,
        Bson::Int64(n) => *n,
        Bson::Double(f) if f.fract() == 0.0 => *f as i64,
        _ => return None,
    };
    (n > 1_000_000_000 && n < 3_000_000_000).then(|| mongodb::bson::DateTime::from_millis(n * 1000).try_to_rfc3339_string().unwrap_or_default())
}

/// The entry of `KINDS` that describes this value, used as the starting type when editing it.
pub fn type_name_kind(b: &Bson) -> &'static str {
    match b {
        Bson::String(_) => "string",
        Bson::Int32(_) => "int",
        Bson::Int64(_) => "long",
        Bson::Double(_) => "double",
        Bson::Decimal128(_) => "decimal",
        Bson::Boolean(_) => "bool",
        Bson::DateTime(_) => "date",
        Bson::ObjectId(_) => "objectId",
        Bson::Null | Bson::Undefined => "null",
        Bson::Array(_) => "array",
        Bson::RegularExpression(_) => "regex",
        b if uuid_of(b).is_some() => "uuid",
        _ => "object",
    }
}

/// A value as "Copy value" puts it on the clipboard: objects and arrays as shell text.
pub fn copy_text(v: &Bson) -> String {
    match v {
        Bson::Document(_) | Bson::Array(_) => shell_text(v),
        other => value_text(other),
    }
}

pub fn value_text(b: &Bson) -> String {
    match b {
        Bson::String(s) => s.clone(),
        Bson::ObjectId(o) => o.to_hex(),
        Bson::DateTime(t) => t.try_to_rfc3339_string().unwrap_or_default(),
        Bson::Null | Bson::Undefined => String::new(),
        Bson::Document(_) | Bson::Array(_) => compact(b),
        Bson::Int64(n) => n.to_string(),
        Bson::Decimal128(d) => d.to_string(),
        Bson::RegularExpression(r) => format!("/{}/{}", r.pattern, r.options),
        b if uuid_of(b).is_some() => uuid_of(b).unwrap().to_string(),
        other => compact(other),
    }
}

pub fn make_value(kind: &str, text: &str) -> Res<Bson> {
    let t = text.trim();
    let bad = |what: &str| format!("\"{t}\" is not a valid {what}");
    Ok(match kind {
        "string" => Bson::String(text.to_string()),
        "int" => Bson::Int32(t.parse().map_err(|_| bad("int (32-bit whole number)"))?),
        "long" => Bson::Int64(t.parse().map_err(|_| bad("long (64-bit whole number)"))?),
        "double" => Bson::Double(t.parse().map_err(|_| bad("double"))?),
        "decimal" => Bson::Decimal128(t.parse().map_err(|_| bad("decimal"))?),
        "bool" => Bson::Boolean(match t {
            "true" | "1" => true,
            "false" | "0" | "" => false,
            _ => return Err(bad("bool (true or false)")),
        }),
        "date" => Bson::DateTime(mongodb::bson::DateTime::parse_rfc3339_str(t).map_err(|_| bad("date (like 2024-05-01T12:00:00Z)"))?),
        "objectId" => Bson::ObjectId(mongodb::bson::oid::ObjectId::parse_str(t).map_err(|_| bad("ObjectId (24 hex characters)"))?),
        "null" => Bson::Null,
        "object" => Bson::Document(parse_doc(if t.is_empty() { "{}" } else { t })?),
        "array" => match parse_value(if t.is_empty() { "[]" } else { t })? {
            a @ Bson::Array(_) => a,
            _ => return Err(bad("array")),
        },
        "regex" => {
            // Accepts /pattern/flags or a bare pattern, like the original's new RegExp(text).
            let (pattern, flags) = match t.strip_prefix('/').and_then(|r| r.rfind('/').map(|i| (&r[..i], &r[i + 1..]))) {
                Some((p, f)) => (p, f),
                None => (t, ""),
            };
            let mut opts: Vec<char> = flags.chars().collect();
            opts.sort_unstable();
            Bson::RegularExpression(mongodb::bson::Regex { pattern: pattern.into(), options: opts.into_iter().collect() })
        }
        "uuid" => Bson::Binary(mongodb::bson::Binary::from_uuid(mongodb::bson::Uuid::parse_str(t).map_err(|_| bad("UUID"))?)),
        _ => return Err(format!("Unknown type {kind}")),
    })
}

/// Value at a dotted path such as `customer.name` or `items.0.sku`.
pub fn get_path<'a>(d: &'a Document, path: &str) -> Option<&'a Bson> {
    let mut cur: Option<&Bson> = None;
    for (i, seg) in path.split('.').enumerate() {
        cur = match (i, cur) {
            (0, _) => d.get(seg),
            (_, Some(Bson::Document(x))) => x.get(seg),
            (_, Some(Bson::Array(a))) => seg.parse::<usize>().ok().and_then(|n| a.get(n)),
            _ => None,
        };
        cur?;
    }
    cur
}

/// `get_path` on BSON bytes.
pub fn get_path_raw<'a>(d: &'a RawDocument, path: &str) -> Option<RawBsonRef<'a>> {
    let mut cur: Option<RawBsonRef<'a>> = None;
    for (i, seg) in path.split('.').enumerate() {
        cur = match (i, cur) {
            (0, _) => d.get(seg).ok().flatten(),
            (_, Some(RawBsonRef::Document(x))) => x.get(seg).ok().flatten(),
            (_, Some(RawBsonRef::Array(a))) => seg.parse::<usize>().ok().and_then(|n| a.get(n).ok().flatten()),
            _ => None,
        };
        cur?;
    }
    cur
}

fn child_mut<'a>(b: &'a mut Bson, seg: &str) -> Option<&'a mut Bson> {
    match b {
        Bson::Document(d) => d.get_mut(seg),
        Bson::Array(a) => seg.parse::<usize>().ok().and_then(|n| a.get_mut(n)),
        _ => None,
    }
}

/// Sets (or with `None` removes) the value at `path` inside a copy of the document.
pub fn edit_path(d: &Document, path: &[String], value: Option<Bson>) -> Res<Document> {
    let mut root = Bson::Document(d.clone());
    let (last, parents) = path.split_last().ok_or("Empty path")?;
    let mut cur = &mut root;
    for seg in parents {
        cur = child_mut(cur, seg).ok_or_else(|| format!("No field {seg}"))?;
    }
    match (cur, value) {
        (Bson::Document(p), Some(v)) => {
            p.insert(last.clone(), v);
        }
        (Bson::Document(p), None) => {
            p.remove(last);
        }
        (Bson::Array(a), v) => {
            let i: usize = last.parse().map_err(|_| "Bad array index")?;
            match v {
                Some(v) if i < a.len() => a[i] = v,
                Some(v) => a.push(v),
                None if i < a.len() => {
                    a.remove(i);
                }
                None => {}
            }
        }
        _ => return Err("That value cannot hold fields".into()),
    }
    match root {
        Bson::Document(d) => Ok(d),
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_stages() {
        // Every template parses as a stage body.
        for (op, body) in STAGES {
            assert!(parse_value(body).is_ok(), "{op}");
        }
        let v = parse_value("{ from: 'inventory', localField: 'item', foreignField: 'sku', as: 'inventory_docs', extra: [1, 2, 3] }").unwrap();
        let pretty = pretty_shell(&v);
        assert!(pretty.contains("\n  from: \"inventory\""), "{pretty}");
        assert_eq!(parse_value(&pretty).unwrap(), v);
        assert_eq!(pretty_shell(&Bson::Int32(5)), "5");
        let small: Vec<Document> = (0..10).map(|i| doc! { "i": i }).collect();
        assert_eq!(trim_sample(small).len(), 10);
        let big: Vec<Document> = (0..10).map(|_| doc! { "s": "x".repeat(30_000) }).collect();
        // 300k characters: floor(100000 / 30000) = 3 documents kept.
        assert_eq!(trim_sample(big).len(), 3);
    }

    #[test]
    fn stage_preview_cuts_limits_and_skips_writes() {
        let p = vec![doc! { "$match": {} }, doc! { "$out": "x" }, doc! { "$group": { "_id": 1 } }];
        assert_eq!(stage_preview(&p, 2).unwrap(), vec![doc! { "$match": {} }, doc! { "$group": { "_id": 1 } }, doc! { "$limit": 20 }]);
        assert_eq!(stage_preview(&p, 0).unwrap().len(), 2);
        assert!(stage_preview(&p, 3).is_none());
    }

    #[test]
    fn parses_shell_syntax() {
        let d = parse_doc(r#"{_id: ObjectId("64b7f0c2a1b2c3d4e5f60718"), n: NumberLong(5), 'a': {$gt: 2}, s: "ObjectId(x)",}"#).unwrap();
        assert!(matches!(d.get("_id"), Some(Bson::ObjectId(_))));
        assert_eq!(d.get("n"), Some(&Bson::Int64(5)));
        assert_eq!(d.get_str("s").unwrap(), "ObjectId(x)");
        // Dotted keys without quotes, as the suggestion list writes them; text values keep their dots.
        let d = parse_doc(r#"{booker.username: "a.b: c", $or: [{ x.y : 1 }, { z: 2 }]}"#).unwrap();
        assert_eq!(d.get_str("booker.username").unwrap(), "a.b: c");
        assert_eq!(d.get_array("$or").unwrap()[0].as_document().unwrap().get("x.y"), Some(&Bson::Int32(1)));
        assert!(parse_doc("{a: new Date('2024-01-02T03:04:05Z')}").unwrap().get_datetime("a").is_ok());
        assert!(parse_doc("[1]").is_err());
        let d = parse_doc(r#"{a: ~"1.5 (x)", b: ~abc, c: { $not: ~'it\'s' }, d: "~x"}"#).unwrap();
        assert_eq!(d.get_document("a").unwrap(), &doc! { "$regex": r"1\.5 \(x\)", "$options": "i" });
        assert_eq!(d.get_document("b").unwrap().get_str("$regex"), Ok("abc"));
        assert_eq!(d.get_document("c").unwrap().get_document("$not").unwrap().get_str("$regex"), Ok("it's"));
        assert_eq!(d.get_str("d"), Ok("~x"));
        assert_eq!(parse_doc("{a: ~Zürich}").unwrap().get_document("a").unwrap().get_str("$regex"), Ok("Zürich"));
        assert!(parse_doc(r#"{a: ~""}"#).is_err() && parse_doc("{a: ~}").is_err());
        assert_eq!(parse_pipeline("[{$match: {}}, {$out: 'x'}]").map(|p| has_write_stage(&p)), Ok(true));
    }

    #[test]
    fn edit_json_round_trips_types() {
        let d = doc! { "i": 1i32, "l": 2i64, "f": 3.0f64, "o": { "x": 1i64 } };
        assert_eq!(parse_doc(&to_edit_json(&d)).unwrap(), d);
    }

    #[test]
    fn diff_and_table() {
        let a = doc! { "x": 1, "o": { "y": 1, "z": 2 } };
        let b = doc! { "x": 1, "o": { "y": 2 }, "w": true };
        let paths: Vec<String> = diff(&a, &b).into_iter().map(|c| c.0).collect();
        assert_eq!(paths, ["o.y", "o.z", "w"]);
        let t = to_table(&[doc! { "a": "x,\"y\"" }, doc! { "b": false }], ',');
        assert_eq!(t, "a,b\n\"x,\"\"y\"\"\",\n,false");
        assert_eq!(mask_uri("mongodb://u:secret@h:27017/db"), "mongodb://u:****@h:27017/db");
        assert_eq!(to_edit_line(&Bson::Document(doc! { "n": 5i64, "s": "x" })), r#"{"n":{"$numberLong":"5"},"s":"x"}"#);
    }

    #[test]
    fn update_stage_building() {
        let st = update_stages(doc! { "a.b": 1 }, vec!["x".into(), "y.z".into()]);
        assert_eq!(st, vec![doc! { "$set": { "a.b": 1 } }, doc! { "$unset": ["x", "y.z"] }]);
        assert_eq!(update_stages(Document::new(), vec!["x".into()]), vec![doc! { "$unset": ["x"] }]);
        assert!(update_stages(Document::new(), vec![]).is_empty());
    }

    #[test]
    fn field_actions() {
        assert_eq!(strip_indices("items.2.sku"), "items.sku");
        assert_eq!(field_size(&Bson::String("abc".into())), 5);
        let d = doc! { "a b": 1, "_id": ObjectIdLike(), "n": 5i64 };
        assert_eq!(parse_doc(&shell_text(&Bson::Document(d.clone()))).unwrap(), d);
        let p = group_count_pipeline("items.0.sku", &Bson::Null, doc! {}).unwrap();
        assert_eq!(p[1].get_str("$unwind").unwrap(), "$items");
        assert!(group_count_pipeline("a.0.b.1.c", &Bson::Null, doc! {}).is_err());
        assert_eq!(group_count_pipeline("tags", &Bson::Array(vec![]), doc! {}).unwrap().len(), 4);
    }

    #[allow(non_snake_case)]
    fn ObjectIdLike() -> Bson {
        Bson::ObjectId(mongodb::bson::oid::ObjectId::parse_str("6650f1c2a1b2c3d4e5f60700").unwrap())
    }

    /// The driver must go through the SOCKS5 port and ask for the server by name (so replica
    /// sets and TLS keep working); a stand-in proxy records the request and refuses it.
    #[test]
    fn import_and_schema() {
        let csv = CsvOptions { separator: None, header: true, camel_case: true };
        let d = parse_import("First Name;age;ok;when;note\n\"Ann; B\";42;TRUE;2024-01-02T03:04:05Z;\n\nBob;3.5;false;x;\"say \"\"hi\"\"\"\n", ImportKind::Csv, &csv).unwrap();
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].get_str("firstName").unwrap(), "Ann; B");
        assert_eq!(d[0].get("age"), Some(&Bson::Int32(42)));
        assert_eq!(d[0].get("ok"), Some(&Bson::Boolean(true)));
        assert!(d[0].get_datetime("when").is_ok());
        assert_eq!(d[0].get("note"), Some(&Bson::Null));
        assert_eq!(d[1].get("age"), Some(&Bson::Double(3.5)));
        assert_eq!(d[1].get_str("note").unwrap(), "say \"hi\"");
        assert_eq!(csv_value("007"), Bson::String("007".into()));
        let j = parse_import("{a: 1}\n\n{a: 2, b: ObjectId('6650f1c2a1b2c3d4e5f60700')}", ImportKind::JsonLines, &csv).unwrap();
        assert_eq!(j.len(), 2);
        assert!(parse_import("[1]", ImportKind::Json, &csv).is_err());
        let s = analyze_schema(&[doc! { "a": 1, "o": { "x": "s" }, "l": [{ "y": 1 }, { "y": 2 }] }, doc! { "a": "two" }], &[]);
        let a = s.iter().find(|f| f.path == "a").unwrap();
        assert_eq!((a.docs, a.types.len()), (2, 2));
        let y = s.iter().find(|f| f.path == "l.y").unwrap();
        assert_eq!((y.docs, y.depth), (1, 1));
    }

    #[test]
    fn typed_edits() {
        let d = doc! { "a": { "b": [1, 2] }, "c": "x" };
        let e = edit_path(&d, &["a".into(), "b".into(), "1".into()], Some(make_value("long", "7").unwrap())).unwrap();
        assert_eq!(get_path(&e, "a.b.1"), Some(&Bson::Int64(7)));
        let e = edit_path(&e, &["c".into()], None).unwrap();
        assert!(!e.contains_key("c"));
        assert!(make_value("int", "x").is_err());
        assert!(make_value("objectId", "6650f1c2a1b2c3d4e5f60700").is_ok());
        let r = make_value("regex", "/ab+c/mi").unwrap();
        assert_eq!((type_name_kind(&r), value_text(&r)), ("regex", "/ab+c/im".to_string()));
        let u = make_value("uuid", "67e55044-10b1-426f-9247-bb680e5fe0c8").unwrap();
        assert_eq!(type_name_kind(&u), "uuid");
        assert!(possible_timestamp(&Bson::Int64(1_700_000_000)).is_some() && possible_timestamp(&Bson::Int32(42)).is_none());
    }

    #[test]
    fn compare_relations_and_tools() {
        let a = vec![doc! { "_id": 1, "x": 1, "y": 2 }, doc! { "_id": 2, "x": 1 }, doc! { "_id": 3 }];
        let b = vec![doc! { "_id": 1, "y": 2, "x": 1 }, doc! { "_id": 2, "x": 2 }, doc! { "_id": 4 }];
        let r = compare_docs(a, b);
        assert_eq!((r.same, r.changed.len(), r.only_src.len(), r.only_dst.len()), (1, 1, 1, 1));
        assert_eq!(r.diffs, vec![doc! { "_id": 2, "field": "x", "target": 2, "source": 1 }]);
        assert_eq!(preview_field(&doc! { "_id": 1, "owner": "6650f1c2a1b2c3d4e5f60700", "name": "Ada" }), "name");
        let raw = |ds: &[Document]| ds.iter().map(|d| RawDocumentBuf::from_document(d).unwrap()).collect::<Vec<_>>();
        assert_eq!(values_at(raw(&[doc! { "t": [1, 2] }, doc! { "t": 2 }]).iter().map(|d| d.as_ref()), "t"), vec![Bson::Int32(1), Bson::Int32(2)]);
        assert_eq!(values_at(raw(&[doc! { "items": [{ "p": 1 }, { "p": 3 }] }]).iter().map(|d| d.as_ref()), "items.p"), vec![Bson::Int32(1), Bson::Int32(3)]);
        let oid = Bson::ObjectId(mongodb::bson::oid::ObjectId::new());
        let found = proto_relations(&[doc! { "_id": oid.clone(), "owner": oid.clone(), "code": "abc123", "items": [{ "product": oid.clone() }], "tags": [oid.clone()] }], 6);
        let paths: Vec<&str> = found.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, ["owner", "code", "items.product", "tags"]);
        assert_eq!(uri_without_db("mongodb://u:p@h:1/shop?tls=true"), "mongodb://u:p@h:1/?tls=true&authSource=shop");
        assert_eq!(uri_without_db("mongodb://u:p@h/shop"), "mongodb://u:p@h/?authSource=shop");
        assert_eq!(uri_without_db("mongodb://u:p@h/shop?authsource=admin"), "mongodb://u:p@h/?authsource=admin");
        assert_eq!(uri_without_db("mongodb://h/shop?tls=true"), "mongodb://h/?tls=true");
        assert_eq!(uri_without_db("mongodb+srv://h/shop"), "mongodb+srv://h/");
        assert_eq!(uri_without_db("mongodb://h:1"), "mongodb://h:1");
        assert_eq!(tool_config("mongodb://u:p\"#@h/?a=1"), "uri: \"mongodb://u:p\\\"#@h/?a=1\"\n");
        assert_eq!(dump_args("mongodb://h", "shop", Some("orders"), "/tmp/a", true), ["--uri=mongodb://h", "--archive=/tmp/a", "--db=shop", "--collection=orders", "--gzip", "--forceTableScan"]);
        assert_eq!(restore_args("mongodb://h/shop", "/tmp/a", "shop", None, "copy", true, false), ["--uri=mongodb://h/", "--archive=/tmp/a", "--nsInclude=shop.*", "--nsFrom=shop.*", "--nsTo=copy.*", "--drop"]);
    }

}

