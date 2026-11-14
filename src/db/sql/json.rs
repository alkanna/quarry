//! JSON and JSON Lines import, the part that needs no server. A file or pasted text holding objects is flattened to the
//! CSV the import already reads: the keys become the header, nested objects and arrays become JSON text, so the column
//! mapping, the check, duplicate handling, batching and undo all work on it unchanged.
//!
//! The text is parsed with serde_json, but each value stays the raw text it was written as, because the import needs what
//! that crate's value type loses: a number keeps its own text (a DECIMAL must not pass through f64) and an object keeps
//! its key order. The text is read twice, one row at a time, so only the text and the CSV it becomes are in memory, never
//! the parsed file.

use super::csv::{self, Source, MARK};
use crate::db::copy::csv_field;
use serde::de::{self, Deserialize, Deserializer as _, MapAccess, SeqAccess, Visitor};
use serde_json::value::RawValue;
use std::io::Read;

/// The most a file may hold. It is read whole to find every key before the first row can be written out.
pub const JSON_MAX: u64 = 256 << 20;

/// An object's keys in their written order, each with its value as the text it was written as. A repeated key appears
/// twice, so the caller decides which one wins.
struct Row<'a>(Vec<(String, &'a RawValue)>);

impl<'de> Deserialize<'de> for Row<'de> {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Row<'de>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Row<'de>, A::Error> {
                let mut v = Vec::new();
                while let Some(kv) = m.next_entry::<String, &RawValue>()? {
                    v.push(kv);
                }
                Ok(Row(v))
            }
        }
        d.deserialize_map(V)
    }
}

/// Calls `f` with the number and raw text of each item of an array.
struct Items<'f, 'a> {
    f: &'f mut dyn FnMut(usize, &'a RawValue) -> Result<(), String>,
    /// Where the callback's own error goes, since a visitor can only fail with serde's error type.
    fail: &'f mut Option<String>,
}

impl<'de> Visitor<'de> for Items<'_, 'de> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("an array")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let mut n = 0;
        while let Some(item) = seq.next_element::<&RawValue>()? {
            n += 1;
            if let Err(m) = (self.f)(n, item) {
                *self.fail = Some(m);
                return Err(de::Error::custom("stopped"));
            }
        }
        Ok(())
    }
}

/// What kind of value a raw JSON text holds, from its first character. The text is already known to be valid.
fn kind(raw: &RawValue) -> &'static str {
    match raw.get().as_bytes()[0] {
        b'n' => "null",
        b't' | b'f' => "a boolean",
        b'"' => "a string",
        b'[' => "an array",
        b'{' => "an object",
        _ => "a number",
    }
}

fn is_nested(raw: &RawValue) -> bool {
    matches!(raw.get().as_bytes()[0], b'[' | b'{')
}

fn bad(e: serde_json::Error) -> String {
    let text = e.to_string();
    let msg = text.rsplit_once(" at line ").map_or(text.as_str(), |(m, _)| m);
    format!("Not valid JSON at line {}, column {}: {msg}", e.line(), e.column())
}

fn row(raw: &RawValue) -> Result<Row<'_>, String> {
    serde_json::from_str(raw.get()).map_err(bad)
}

/// Calls `f` with each object of the text in turn: the items of an array, or objects one after another. True when they
/// were not in an array (JSON Lines). Only one object is alive at a time.
fn each_row<'t>(t: &'t str, mut f: impl FnMut(Row<'t>) -> Result<(), String>) -> Result<bool, String> {
    let t = t.strip_prefix('\u{feff}').unwrap_or(t);
    let (mut seen, mut lines) = (0usize, false);
    for next in serde_json::Deserializer::from_str(t).into_iter::<&RawValue>() {
        let raw = next.map_err(bad)?;
        seen += 1;
        match raw.get().as_bytes()[0] {
            b'[' => {
                let mut fail = None;
                let mut item = |n: usize, item: &'t RawValue| {
                    if item.get().starts_with('{') {
                        f(row(item)?)
                    } else {
                        Err(format!("Item {n} of the array is {}, not an object.", kind(item)))
                    }
                };
                // The scan above already checked the text, so only the callback can fail here.
                let r = serde_json::Deserializer::from_str(raw.get()).deserialize_seq(Items { f: &mut item, fail: &mut fail });
                if let Some(m) = fail {
                    return Err(m);
                }
                r.map_err(bad)?;
            }
            b'{' => {
                lines = true;
                f(row(raw)?)?;
            }
            _ => return Err(format!("Value {seen} is {}, not an object. Expected an array of objects or one object per line.", kind(raw))),
        }
    }
    Ok(lines)
}

/// What a JSON file became.
#[derive(Debug)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct Flat {
    /// CSV text with a header row, comma separated and quoted where it needs to be.
    pub csv: Vec<u8>,
    /// The keys in the order they first appear, which are the header.
    pub names: Vec<String>,
    /// For each key whether any row holds an object or array there, written as JSON text.
    pub nested: Vec<bool>,
    pub rows: u64,
    /// Objects one after another (JSON Lines) rather than one array.
    pub lines: bool,
}

/// Whether a file or pasted text is JSON rather than CSV: a name that says so, or text that opens an array or an object.
pub fn looks_like_json(name: &str, head: &[u8]) -> bool {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    if matches!(ext.as_str(), "json" | "jsonl" | "ndjson") {
        return true;
    }
    if matches!(ext.as_str(), "csv" | "tsv") {
        return false;
    }
    let head = head.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(head);
    head.iter().find(|b| !b.is_ascii_whitespace()).is_some_and(|b| matches!(b, b'[' | b'{'))
}

/// The whole file as text, in the encoding its bytes show. UTF-8, the usual case, takes over the bytes without a copy.
pub fn read_text(src: &Source) -> Result<String, String> {
    if src.len() > JSON_MAX {
        return Err(format!("The file is over {} MB. JSON is read whole, so split it, or write it as CSV or JSON Lines in parts.", JSON_MAX >> 20));
    }
    let mut bytes = Vec::with_capacity(src.len() as usize);
    src.open().and_then(|mut r| r.read_to_end(&mut bytes)).map_err(|e| format!("Could not read the file: {e}"))?;
    let enc = csv::detect_enc(&bytes[..bytes.len().min(64 << 10)]);
    if enc == csv::Enc::Utf8 {
        return match String::from_utf8(bytes) {
            Ok(mut s) => {
                if s.starts_with('\u{feff}') {
                    s.drain(..3);
                }
                Ok(s)
            }
            Err(e) => csv::decode_text(e.as_bytes(), enc),
        };
    }
    csv::decode_text(&bytes, enc)
}

/// Flattens a JSON array of objects, or objects one after another, to CSV. Only the top-level keys of each object become
/// columns. A key an object lacks is an empty value, which the import reads as NULL or empty text as the dialog says; a
/// JSON null is a NULL whatever the column, and a string is always that string, even `\N`.
pub fn flatten(text: &str) -> Result<Flat, String> {
    // First pass: the keys in the order they first appear, and which hold nested values.
    let mut names: Vec<String> = Vec::new();
    let mut at = std::collections::HashMap::<String, usize>::new();
    let mut nested: Vec<bool> = Vec::new();
    let mut rows = 0u64;
    let lines = each_row(text, |row| {
        rows += 1;
        for (k, v) in row.0 {
            let i = *at.entry(k.clone()).or_insert_with(|| {
                names.push(k);
                nested.push(false);
                names.len() - 1
            });
            if is_nested(v) {
                nested[i] = true;
            }
        }
        Ok(())
    })?;
    if rows == 0 {
        return Err("The text holds no objects to import.".into());
    }
    if names.is_empty() {
        return Err("The objects have no keys.".into());
    }
    // Second pass: the same text again, a row at a time, written out under that header.
    let mut out = String::new();
    push_row(&mut out, names.iter().map(String::as_str));
    each_row(text, |row| {
        let mut cells: Vec<Option<&RawValue>> = vec![None; names.len()];
        // A repeated key keeps its last value, as a parser that builds a map does.
        for (k, v) in row.0 {
            cells[at[&k]] = Some(v);
        }
        let texts = cells.iter().map(|c| cell_text(*c)).collect::<Result<Vec<_>, _>>()?;
        push_row(&mut out, texts.iter().map(String::as_str));
        Ok(())
    })?;
    Ok(Flat { csv: out.into_bytes(), names, nested, rows, lines })
}

/// What a value is written as in the CSV, with the lead `csv::convert` reads (see `MARK`).
fn cell_text(c: Option<&RawValue>) -> Result<String, String> {
    let Some(raw) = c else { return Ok(String::new()) };
    let t = raw.get();
    Ok(match t.as_bytes()[0] {
        b'n' => format!("{MARK}N"),
        b'"' => {
            let s: String = serde_json::from_str(t).map_err(bad)?;
            // Text that CSV would read as a NULL, or that opens like this lead, is marked as the text it is.
            if s == "\\N" || s.starts_with(MARK) { format!("{MARK}S{s}") } else { s }
        }
        b'[' | b'{' => compact(t)?,
        // Booleans and numbers as written: `-0`, `1E5` and 40 digits stay what they are.
        _ => t.to_string(),
    })
}

/// Valid JSON text without the whitespace between tokens. Strings are written out again, so the escapes are the same
/// whatever the file used, while keys keep their order and numbers their own text.
fn compact(t: &str) -> Result<String, String> {
    let (b, mut out, mut i) = (t.as_bytes(), String::with_capacity(t.len()), 0);
    while i < b.len() {
        match b[i] {
            b'"' => {
                let mut j = i + 1;
                while b[j] != b'"' {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                let s: String = serde_json::from_str(&t[i..=j]).map_err(bad)?;
                out.push_str(&serde_json::to_string(&s).unwrap_or_default());
                i = j;
            }
            c if c.is_ascii_whitespace() => {}
            c => out.push(c as char),
        }
        i += 1;
    }
    Ok(out)
}

/// One CSV record. An empty value is quoted, since a line with nothing on it is not a record to the reader.
fn push_row<'a>(out: &mut String, cells: impl Iterator<Item = &'a str>) {
    for (i, c) in cells.enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&csv_field(c, true));
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sql::csv::{convert, DateFmt, Enc, Opts, Reader, TCol};
    use crate::db::sql::value::Param;

    /// The records the import would read back from the flattened text.
    fn read_back(f: &Flat) -> Vec<Vec<String>> {
        let mut r = Reader::new(std::io::Cursor::new(f.csv.clone()), Opts { delim: ',', quote: Some('"'), enc: Enc::Utf8 });
        let mut out = Vec::new();
        while let Some(rec) = r.next_record().unwrap() {
            out.push(rec);
        }
        out
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn col(ty: &str, pg: bool) -> TCol {
        TCol::from_column(&crate::db::sql::structure::Column { name: "c".into(), full_type: ty.into(), nullable: true, ..Default::default() }, pg)
    }

    #[test]
    fn an_array_of_objects_keeps_key_order_and_unions_the_keys() {
        let f = flatten(r#"[{"id": 1, "name": "Ada"}, {"name": "Grace", "id": 2, "extra": true}]"#).unwrap();
        assert_eq!(f.names, ["id", "name", "extra"]);
        assert_eq!((f.rows, f.lines), (2, false));
        // A key the first object lacks is empty there; the second's order does not change the columns.
        assert_eq!(read_back(&f), [s(&["id", "name", "extra"]), s(&["1", "Ada", ""]), s(&["2", "Grace", "true"])]);
    }

    #[test]
    fn json_lines_and_concatenated_objects_read_the_same() {
        let f = flatten("{\"a\":1}\n\n{\"a\":2,\"b\":\"x\"}\r\n{\"b\":null}\n").unwrap();
        assert!(f.lines);
        let n = format!("{MARK}N");
        assert_eq!(read_back(&f), [s(&["a", "b"]), s(&["1", ""]), s(&["2", "x"]), s(&["", &n])]);
    }

    #[test]
    fn nested_values_become_json_text_in_the_order_they_were_written() {
        let f = flatten(r#"[{"id": 1, "tags": ["b", "a"], "geo": {"z": 1.5, "a": {"k": null}}, "plain": "x"}]"#).unwrap();
        assert_eq!(f.nested, [false, true, true, false]);
        let rec = &read_back(&f)[1];
        assert_eq!(rec[1], r#"["b","a"]"#);
        assert_eq!(rec[2], r#"{"z":1.5,"a":{"k":null}}"#);
    }

    #[test]
    fn commas_quotes_line_breaks_and_empty_text_survive_the_csv_round_trip() {
        let f = flatten(r#"[{"a": "x,y", "b": "say \"hi\"", "c": "line\nbreak", "d": ""}]"#).unwrap();
        assert_eq!(read_back(&f)[1], s(&["x,y", "say \"hi\"", "line\nbreak", ""]));
        // A single column of empty text is still a record, not a blank line.
        let one = flatten(r#"[{"a": ""}, {"a": "z"}]"#).unwrap();
        assert_eq!(read_back(&one), [s(&["a"]), s(&[""]), s(&["z"])]);
    }

    #[test]
    fn numbers_keep_their_own_text_whatever_their_size() {
        let f = flatten(r#"[{"i": -7, "big": 340282366920938463463374607431768211456, "dec": 12345678901234567.89, "e": 1E5, "z": -0, "f": 12.50, "t": true}]"#).unwrap();
        assert_eq!(read_back(&f)[1], s(&["-7", "340282366920938463463374607431768211456", "12345678901234567.89", "1E5", "-0", "12.50", "true"]));
        // What the column then receives is that text, exact.
        let dec = col("decimal(30,2)", false);
        assert_eq!(convert(&dec, "12345678901234567.89", true, DateFmt::DayFirst), Ok(Param::Text("12345678901234567.89".into())));
    }

    #[test]
    fn a_json_null_is_null_and_the_string_backslash_n_stays_a_string() {
        let f = flatten(r#"[{"a": null, "b": "\\N", "c": "\u0001lead", "d": "N"}]"#).unwrap();
        let rec = read_back(&f)[1].clone();
        for ty in ["varchar(20)", "text"] {
            for pg in [false, true] {
                let t = col(ty, pg);
                let got: Vec<_> = rec.iter().map(|c| convert(&t, c, false, DateFmt::DayFirst)).collect();
                assert_eq!(got[0], Ok(Param::Null), "{ty} {pg}");
                assert_eq!(got[1], Ok(Param::Text("\\N".into())), "{ty} {pg}");
                assert_eq!(got[2], Ok(Param::Text("\u{1}lead".into())), "{ty} {pg}");
                assert_eq!(got[3], Ok(Param::Text("N".into())), "{ty} {pg}");
            }
        }
        // CSV keeps its own rule: a bare \N is a NULL there.
        assert_eq!(convert(&col("varchar(20)", false), "\\N", false, DateFmt::DayFirst), Ok(Param::Null));
    }

    #[test]
    fn a_repeated_key_keeps_its_last_value_and_a_dotted_key_is_one_column() {
        let f = flatten(r#"[{"a": 1, "a": 2, "x.y": 3}]"#).unwrap();
        assert_eq!(f.names, ["a", "x.y"]);
        assert_eq!(read_back(&f)[1], s(&["2", "3"]));
    }

    #[test]
    fn escapes_unicode_and_surrogate_pairs_are_read() {
        let f = flatten(r#"[{"a": "tab\there é 😀 \/"}]"#).unwrap();
        assert_eq!(read_back(&f)[1], s(&["tab\there \u{e9} \u{1F600} /"]));
        assert!(flatten(r#"[{"a": "\ud83d"}]"#).is_err());
    }

    #[test]
    fn what_is_not_an_array_of_objects_says_where_it_went_wrong() {
        assert_eq!(flatten("[{\"a\":1}, 5]").unwrap_err(), "Item 2 of the array is a number, not an object.");
        assert!(flatten("{\"a\":1}\n\"x\"").unwrap_err().starts_with("Value 2 is a string"));
        assert!(flatten("{\"a\":1}\n{\"a\":").unwrap_err().starts_with("Not valid JSON at line 2"));
        assert!(flatten("[{\"a\": 01}]").unwrap_err().starts_with("Not valid JSON at line 1, column"));
        assert!(flatten("[{\"a\": 1,}]").is_err() && flatten("[{\"a\": tru}]").is_err());
        assert_eq!(flatten("[]").unwrap_err(), "The text holds no objects to import.");
        assert_eq!(flatten("[{}]").unwrap_err(), "The objects have no keys.");
        assert_eq!(flatten("   ").unwrap_err(), "The text holds no objects to import.");
        let deep = format!("[{{\"a\": {}1{}}}]", "[".repeat(200), "]".repeat(200));
        assert!(flatten(&deep).unwrap_err().contains("recursion limit"));
    }

    #[test]
    fn a_byte_order_mark_does_not_hide_the_json() {
        let f = flatten("\u{feff}[{\"a\": 1}]").unwrap();
        assert_eq!(f.names, ["a"]);
        assert!(looks_like_json("pasted", b"\xEF\xBB\xBF  [{\"a\":1}]"));
        let text = read_text(&Source::Mem(std::sync::Arc::new(b"\xEF\xBB\xBF[{\"a\":1}]".to_vec()))).unwrap();
        assert_eq!(text, "[{\"a\":1}]");
    }

    #[test]
    fn the_name_or_the_first_character_tells_json_from_csv() {
        assert!(looks_like_json("rows.JSON", b"a,b") && looks_like_json("x.ndjson", b"") && looks_like_json("pasted", b"\n {\"a\":1}"));
        assert!(!looks_like_json("rows.csv", b"[1]") && !looks_like_json("pasted", b"id,name\n1,a"));
    }

    #[test]
    fn utf16_json_is_decoded_before_it_is_parsed() {
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend("[{\"n\": \"\u{e9}t\u{e9}\"}]".encode_utf16().flat_map(u16::to_le_bytes));
        let text = read_text(&Source::Mem(std::sync::Arc::new(bytes))).unwrap();
        assert_eq!(read_back(&flatten(&text).unwrap())[1], s(&["\u{e9}t\u{e9}"]));
    }
}
