//! How a BSON value reads in a table cell or a tree leaf: its text and its type color.

use super::theme::*;
use crate::db;
use eframe::egui::{self, Color32, text::LayoutJob};
use mongodb::bson::{Bson, Document, RawBsonRef};
use std::borrow::Cow;

/// Web links stand out from plain text. Hex ids stored as strings keep the string colour so the
/// colour always matches the type shown next to them.
#[derive(PartialEq)]
pub(crate) enum StrKind {
    Plain,
    Url,
}

pub(crate) fn str_kind(s: &str) -> StrKind {
    if (s.starts_with("http://") || s.starts_with("https://")) && !s.contains(char::is_whitespace) {
        StrKind::Url
    } else {
        StrKind::Plain
    }
}

/// A value from a decoded document, or read straight from a row's BSON bytes (see `db::Row`).
#[derive(Clone, Copy)]
pub(crate) enum Val<'a> {
    B(&'a Bson),
    R(RawBsonRef<'a>),
}

impl<'a> Val<'a> {
    /// The whole value. A raw object or array gets decoded, so this is for one value at a time,
    /// not for every cell on every frame.
    pub(crate) fn bson(self) -> Cow<'a, Bson> {
        match self {
            Val::B(b) => Cow::Borrowed(b),
            Val::R(r) => Cow::Owned(Bson::try_from(r).unwrap_or(Bson::Null)),
        }
    }

    /// Whether it is an ObjectId or a string, the values a reference can be.
    pub(crate) fn id_like(self) -> bool {
        matches!(self, Val::B(Bson::ObjectId(_) | Bson::String(_)) | Val::R(RawBsonRef::ObjectId(_) | RawBsonRef::String(_)))
    }

    /// `cell_text` and `color_of`, reading a raw string or object without decoding it.
    fn text_and_color(self) -> (String, Color32) {
        match self {
            Val::B(b) => (cell_text(b), color_of(b)),
            Val::R(RawBsonRef::String(s)) => (s.chars().take(120).collect(), str_color(s)),
            Val::R(RawBsonRef::Document(_) | RawBsonRef::Array(_)) => (self.size_text().unwrap_or_default(), C_OBJ()),
            Val::R(_) => Val::B(&self.bson()).text_and_color(),
        }
    }

    /// "{n fields}" or "[n items]" for an object or array.
    fn size_text(self) -> Option<String> {
        Some(match self {
            Val::B(Bson::Document(d)) => format!("{{{} fields}}", d.len()),
            Val::B(Bson::Array(a)) => format!("[{} items]", a.len()),
            Val::R(RawBsonRef::Document(d)) => format!("{{{} fields}}", d.iter().count()),
            Val::R(RawBsonRef::Array(a)) => format!("[{} items]", a.into_iter().count()),
            _ => return None,
        })
    }
}

fn str_color(s: &str) -> Color32 {
    match str_kind(s) {
        StrKind::Url => C_LINK(),
        StrKind::Plain if s.is_empty() => C_NULL(),
        StrKind::Plain => C_STRING(),
    }
}

pub(crate) fn color_of(b: &Bson) -> Color32 {
    match b {
        Bson::String(s) => str_color(s),
        Bson::Double(_) | Bson::Int32(_) | Bson::Int64(_) | Bson::Decimal128(_) => C_NUMBER(),
        Bson::Boolean(true) => C_TRUE(),
        Bson::Boolean(false) => C_FALSE(),
        Bson::DateTime(_) | Bson::Timestamp(_) => C_DATE(),
        Bson::Null | Bson::Undefined => C_NULL(),
        Bson::ObjectId(_) => C_OID(),
        _ => C_OBJ(),
    }
}

pub(crate) fn leaf_text(b: &Bson) -> String {
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
pub(crate) const STRING_DISPLAY_MAX: usize = 100;

pub(crate) fn truncate_display(s: &str) -> String {
    if s.chars().count() <= STRING_DISPLAY_MAX {
        s.to_string()
    } else {
        s.chars().take(STRING_DISPLAY_MAX - 3).chain("...".chars()).collect()
    }
}

/// A table cell. With `summarize`, an object or array whose JSON would not fit the cell shows as
/// "{12 fields}" or "[500 items]" instead of its first characters; small ones stay in full.
pub(crate) fn column_job(v: Val, summarize: bool) -> LayoutJob {
    let job = cell_job_of(v);
    if summarize && job.text.len() >= CELL_PREVIEW_MAX {
        let Some(text) = v.size_text() else { return job };
        let mut j = LayoutJob::default();
        j.append(&text, 0.0, mono(C_OBJ()));
        return j;
    }
    job
}

/// Characters of a cell preview, the original's cut for objects and arrays
/// (fieldTypes.object.toDisplayString, renderer line 30953: substring(0, 150)).
pub(crate) const CELL_PREVIEW_MAX: usize = 150;

/// A table cell as coloured text, like the original's table: strings in quotes, and objects and
/// arrays as a short coloured preview of their contents, so nested data keeps its type colours.
pub(crate) fn cell_job(v: &Bson) -> LayoutJob {
    cell_job_of(Val::B(v))
}

fn cell_job_of(v: Val) -> LayoutJob {
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
    fn object<'a>(job: &mut LayoutJob, fields: impl Iterator<Item = (&'a str, Val<'a>)>) -> bool {
        if !push(job, "{", C_OBJ()) {
            return false;
        }
        for (i, (k, x)) in fields.enumerate() {
            if (i > 0 && !push(job, ", ", FAINT())) || !push(job, k, MUTED()) || !push(job, ": ", FAINT()) || !walk(job, x) {
                return false;
            }
        }
        push(job, "}", C_OBJ())
    }
    fn array<'a>(job: &mut LayoutJob, items: impl Iterator<Item = Val<'a>>) -> bool {
        if !push(job, "[", C_OBJ()) {
            return false;
        }
        for (i, x) in items.enumerate() {
            if (i > 0 && !push(job, ", ", FAINT())) || !walk(job, x) {
                return false;
            }
        }
        push(job, "]", C_OBJ())
    }
    fn string(job: &mut LayoutJob, s: &str) -> bool {
        if str_kind(s) == StrKind::Url {
            return push(job, s, C_LINK());
        }
        // Cut before replacing: a cell shows 100 characters, the string may hold megabytes.
        push(job, &format!("\"{}\"", truncate_display(&s.chars().take(STRING_DISPLAY_MAX + 1).collect::<String>().replace('\n', " "))), str_color(s))
    }
    // The original shows binary from 100 bytes as "<too long binary>" (fieldTypes.binary, 31012).
    fn binary(job: &mut LayoutJob, n: usize) -> bool {
        push(job, &format!("BinData({n} bytes)"), FAINT())
    }
    // Raw objects and arrays are walked in place, so a preview never decodes more than it shows.
    fn walk(job: &mut LayoutJob, v: Val) -> bool {
        match v {
            Val::B(Bson::Document(d)) => object(job, d.iter().map(|(k, x)| (k.as_str(), Val::B(x)))),
            Val::R(RawBsonRef::Document(d)) => object(job, d.iter().map_while(Result::ok).map(|(k, x)| (k, Val::R(x)))),
            Val::B(Bson::Array(a)) => array(job, a.iter().map(Val::B)),
            Val::R(RawBsonRef::Array(a)) => array(job, a.into_iter().map_while(Result::ok).map(Val::R)),
            Val::B(Bson::String(s)) => string(job, s),
            Val::R(RawBsonRef::String(s)) => string(job, s),
            Val::B(Bson::Binary(b)) if b.bytes.len() >= 100 => binary(job, b.bytes.len()),
            Val::R(RawBsonRef::Binary(b)) if b.bytes.len() >= 100 => binary(job, b.bytes.len()),
            Val::B(Bson::ObjectId(o)) => push(job, &o.to_hex(), C_OID()),
            Val::B(other) => push(job, &leaf_text(other), color_of(other)),
            // Other raw values are small, so they go through the decoded path.
            Val::R(_) => walk(job, Val::B(&v.bson())),
        }
    }
    let mut job = LayoutJob::default();
    walk(&mut job, v);
    job
}

/// Short text for a table cell. Objects and arrays are summarised, never serialised, so wide
/// documents stay cheap to draw.
pub(crate) fn cell_text(b: &Bson) -> String {
    match b {
        Bson::String(s) => s.chars().take(120).collect(),
        Bson::ObjectId(o) => o.to_hex(),
        Bson::Document(_) | Bson::Array(_) => Val::B(b).size_text().unwrap_or_default(),
        other => leaf_text(other),
    }
}

pub(crate) fn row_job(d: &Document, width: f32) -> LayoutJob {
    row_job_of(d.get("_id"), d.iter().map(|(k, v)| (k.as_str(), Val::B(v))), width)
}

/// A JSON list row from its _id and fields, which may be read straight from BSON bytes.
pub(crate) fn row_job_of<'a>(id: Option<&Bson>, fields: impl Iterator<Item = (&'a str, Val<'a>)>, width: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    if let Some(id) = id {
        job.append(&db::short(id), 0.0, mono(C_OID()));
        job.append("  ", 0.0, mono(MUTED()));
    }
    for (k, v) in fields.filter(|(k, _)| *k != "_id").take(12) {
        let (text, color) = v.text_and_color();
        job.append(k, 0.0, mono(MUTED()));
        job.append(": ", 0.0, mono(MUTED()));
        job.append(&text.chars().take(60).collect::<String>(), 0.0, mono(color));
        job.append("   ", 0.0, mono(MUTED()));
    }
    job.wrap = egui::text::TextWrapping { max_width: width, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    job
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::doc;

    #[test]
    fn big_objects_show_their_size() {
        let small = Bson::Document(doc! { "name": "Ada", "tier": "gold" });
        assert_eq!(column_job(Val::B(&small), true).text, "{name: \"Ada\", tier: \"gold\"}");
        let big = Bson::Document((0..40).map(|i| (format!("field{i}"), Bson::Int32(i))).collect());
        assert_eq!(column_job(Val::B(&big), true).text, "{40 fields}");
        assert!(column_job(Val::B(&big), false).text.starts_with("{field0: 0"));
        let list = Bson::Array((0..500).map(Bson::Int32).collect());
        assert_eq!(column_job(Val::B(&list), true).text, "[500 items]");
    }

    /// Rows read from BSON bytes must look exactly like decoded ones, for every kind of value.
    #[test]
    fn raw_rows_render_like_decoded_ones() {
        use mongodb::bson::{Binary, DateTime, RawDocumentBuf, Timestamp, oid::ObjectId, spec::BinarySubtype};
        let d = doc! {
            "_id": ObjectId::parse_str("65a1b2c3d4e5f60718293a4b").unwrap(),
            "s": "hello\nworld", "empty": "", "url": "https://example.com/a", "long": "x".repeat(5000),
            "i": 1, "l": 2_i64, "f": 1.5, "t": true, "n": Bson::Null, "at": DateTime::from_millis(1_700_000_000_000),
            "ts": Timestamp { time: 1, increment: 2 }, "bin": Binary { subtype: BinarySubtype::Generic, bytes: vec![7; 150] },
            "small_bin": Binary { subtype: BinarySubtype::Generic, bytes: vec![1, 2] },
            "obj": { "a": 1, "b": { "c": [1, 2, { "d": "e" }] } },
            "big": (0..40).map(|i| (format!("f{i}"), Bson::Int32(i))).collect::<Document>(),
            "list": (0..500).map(Bson::Int32).collect::<Vec<_>>(),
            "items": [{ "sku": "A", "qty": 2 }, { "sku": "B", "qty": 3 }],
        };
        let raw = RawDocumentBuf::from_document(&d).unwrap();
        let mut paths: Vec<String> = d.keys().cloned().collect();
        paths.extend(["obj.b.c.2.d", "items.1.sku", "items.9", "obj.missing", "list.499"].map(String::from));
        for p in &paths {
            let (b, r) = (db::get_path(&d, p), db::get_path_raw(&raw, p));
            assert_eq!(b.is_some(), r.is_some(), "{p}");
            let (Some(b), Some(r)) = (b, r) else { continue };
            assert_eq!(&Bson::try_from(r).unwrap(), b, "{p}");
            for summarize in [true, false] {
                let (jb, jr) = (column_job(Val::B(b), summarize), column_job(Val::R(r), summarize));
                assert_eq!(jb.text, jr.text, "{p}");
                assert_eq!(jb.sections.iter().map(|s| (s.byte_range.clone(), s.format.color)).collect::<Vec<_>>(), jr.sections.iter().map(|s| (s.byte_range.clone(), s.format.color)).collect::<Vec<_>>(), "{p}");
            }
        }
        let row = db::Row::from_raw(raw.clone());
        let (jb, jr) = (row_job(&d, 500.0), row_job_of(row.id().as_ref(), row.raw().iter().map_while(Result::ok).map(|(k, v)| (k, Val::R(v))), 500.0));
        assert_eq!(jb.text, jr.text);
        assert_eq!(jb.sections.iter().map(|s| s.format.color).collect::<Vec<_>>(), jr.sections.iter().map(|s| s.format.color).collect::<Vec<_>>());
    }

    #[test]
    fn string_display_truncates_like_lodash() {
        assert_eq!(truncate_display(&"a".repeat(100)).len(), 100);
        let t = truncate_display(&"a".repeat(150));
        assert_eq!((t.len(), t.ends_with("...")), (100, true));
    }
}
