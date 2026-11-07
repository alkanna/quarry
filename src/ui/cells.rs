//! How a BSON value reads in a table cell or a tree leaf: its text and its type color.

use super::theme::*;
use crate::db;
use eframe::egui::{self, Color32, text::LayoutJob};
use mongodb::bson::{Bson, Document};

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

pub(crate) fn color_of(b: &Bson) -> Color32 {
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
pub(crate) fn column_job(v: &Bson, summarize: bool) -> LayoutJob {
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
pub(crate) const CELL_PREVIEW_MAX: usize = 150;

/// A table cell as coloured text, like the original's table: strings in quotes, and objects and
/// arrays as a short coloured preview of their contents, so nested data keeps its type colours.
pub(crate) fn cell_job(v: &Bson) -> LayoutJob {
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
pub(crate) fn cell_text(b: &Bson) -> String {
    match b {
        Bson::String(s) => s.chars().take(120).collect(),
        Bson::ObjectId(o) => o.to_hex(),
        Bson::Document(d) => format!("{{{} fields}}", d.len()),
        Bson::Array(a) => format!("[{} items]", a.len()),
        other => leaf_text(other),
    }
}

pub(crate) fn row_job(d: &Document, width: f32) -> LayoutJob {
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

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::doc;

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
    fn string_display_truncates_like_lodash() {
        assert_eq!(truncate_display(&"a".repeat(100)).len(), 100);
        let t = truncate_display(&"a".repeat(150));
        assert_eq!((t.len(), t.ends_with("...")), (100, true));
    }
}
