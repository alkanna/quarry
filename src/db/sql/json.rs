//! JSON and JSON Lines import, the part that needs no server. A file or pasted text holding objects is flattened to the
//! CSV the import already reads: the keys become the header, nested objects and arrays become JSON text, so the column
//! mapping, the check, duplicate handling, batching and undo all work on it unchanged.

use super::csv::{self, Source};
use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use std::io::Read;

/// The most a file may hold. It is read whole to find every key before the first row can be written out.
pub const JSON_MAX: u64 = 256 << 20;

/// A JSON value that keeps the order of an object's keys, which serde_json's own value does not.
#[derive(Debug, PartialEq)]
enum J {
    Null,
    Bool(bool),
    /// The number as it will be written.
    Num(String),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl<'de> Deserialize<'de> for J {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<J, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = J;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("any JSON value")
            }

            fn visit_unit<E>(self) -> Result<J, E> {
                Ok(J::Null)
            }

            fn visit_bool<E>(self, v: bool) -> Result<J, E> {
                Ok(J::Bool(v))
            }

            fn visit_i64<E>(self, v: i64) -> Result<J, E> {
                Ok(J::Num(v.to_string()))
            }

            fn visit_u64<E>(self, v: u64) -> Result<J, E> {
                Ok(J::Num(v.to_string()))
            }

            fn visit_f64<E>(self, v: f64) -> Result<J, E> {
                Ok(J::Num(v.to_string()))
            }

            fn visit_str<E>(self, v: &str) -> Result<J, E> {
                Ok(J::Str(v.to_string()))
            }

            fn visit_string<E>(self, v: String) -> Result<J, E> {
                Ok(J::Str(v))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<J, A::Error> {
                let mut v = Vec::new();
                while let Some(x) = a.next_element()? {
                    v.push(x);
                }
                Ok(J::Arr(v))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<J, A::Error> {
                let mut v = Vec::new();
                while let Some(kv) = a.next_entry::<String, J>()? {
                    v.push(kv);
                }
                Ok(J::Obj(v))
            }
        }
        d.deserialize_any(V)
    }
}

impl J {
    /// Compact JSON text, keys in their original order.
    fn write(&self, out: &mut String) {
        match self {
            J::Null => out.push_str("null"),
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Num(n) => out.push_str(n),
            J::Str(s) => out.push_str(&serde_json::to_string(s).unwrap_or_default()),
            J::Arr(a) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    x.write(out);
                }
                out.push(']');
            }
            J::Obj(o) => {
                out.push('{');
                for (i, (k, x)) in o.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(k).unwrap_or_default());
                    out.push(':');
                    x.write(out);
                }
                out.push('}');
            }
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            J::Null => "null",
            J::Bool(_) => "a boolean",
            J::Num(_) => "a number",
            J::Str(_) => "a string",
            J::Arr(_) => "an array",
            J::Obj(_) => "an object",
        }
    }
}

/// What a JSON file became.
#[derive(Debug)]
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

/// The whole file as text, in the encoding its bytes show.
pub fn read_text(src: &Source) -> Result<String, String> {
    if src.len() > JSON_MAX {
        return Err(format!("The file is over {} MB. JSON is read whole, so split it, or write it as CSV or JSON Lines in parts.", JSON_MAX >> 20));
    }
    let mut bytes = Vec::new();
    src.open().and_then(|mut r| r.read_to_end(&mut bytes)).map_err(|e| format!("Could not read the file: {e}"))?;
    let enc = csv::detect_enc(&bytes[..bytes.len().min(64 << 10)]);
    csv::decode_text(&bytes, enc)
}

/// Flattens a JSON array of objects, or objects one after another, to CSV. Only the top-level keys of each object become
/// columns. A key an object lacks is an empty value, which the import reads as NULL or empty text as the dialog says, and
/// a JSON null is always NULL.
pub fn flatten(text: &str) -> Result<Flat, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows: Vec<Vec<(String, J)>> = Vec::new();
    let (mut seen, mut lines) = (0usize, false);
    for item in serde_json::Deserializer::from_str(text).into_iter::<J>() {
        let item = item.map_err(|e| format!("Not valid JSON at line {}, column {}: {e}", e.line(), e.column()))?;
        seen += 1;
        match item {
            J::Arr(a) => {
                for (i, x) in a.into_iter().enumerate() {
                    match x {
                        J::Obj(o) => rows.push(o),
                        other => return Err(format!("Item {} of the array is {}, not an object.", i + 1, other.kind())),
                    }
                }
            }
            J::Obj(o) => {
                lines = true;
                rows.push(o);
            }
            other => return Err(format!("Value {seen} is {}, not an object. Expected an array of objects or one object per line.", other.kind())),
        }
    }
    if rows.is_empty() {
        return Err("The text holds no objects to import.".into());
    }
    let mut names: Vec<String> = Vec::new();
    let mut at = std::collections::HashMap::<String, usize>::new();
    for row in &rows {
        for (k, _) in row {
            if !at.contains_key(k) {
                at.insert(k.clone(), names.len());
                names.push(k.clone());
            }
        }
    }
    if names.is_empty() {
        return Err("The objects have no keys.".into());
    }
    let mut nested = vec![false; names.len()];
    let mut out = String::new();
    push_row(&mut out, names.iter().map(String::as_str));
    let mut cells: Vec<Option<&J>> = vec![None; names.len()];
    for row in &rows {
        cells.iter_mut().for_each(|c| *c = None);
        // A repeated key keeps its last value, as a parser that builds a map does.
        for (k, v) in row {
            cells[at[k]] = Some(v);
        }
        let mut texts: Vec<String> = Vec::with_capacity(names.len());
        for (i, c) in cells.iter().enumerate() {
            texts.push(match c {
                None => String::new(),
                // Spelled the way the import reads a NULL, which no column refuses to take as one for lack of a type.
                Some(J::Null) => "\\N".into(),
                Some(J::Bool(b)) => b.to_string(),
                Some(J::Num(n)) => n.clone(),
                Some(J::Str(s)) => s.clone(),
                Some(v) => {
                    nested[i] = true;
                    let mut t = String::new();
                    v.write(&mut t);
                    t
                }
            });
        }
        push_row(&mut out, texts.iter().map(String::as_str));
    }
    Ok(Flat { csv: out.into_bytes(), names, nested, rows: rows.len() as u64, lines })
}

/// One CSV record. An empty value is quoted, since a line with nothing on it is not a record to the reader.
fn push_row<'a>(out: &mut String, cells: impl Iterator<Item = &'a str>) {
    for (i, c) in cells.enumerate() {
        if i > 0 {
            out.push(',');
        }
        if c.is_empty() || c.contains([',', '"', '\n', '\r']) {
            out.push('"');
            out.push_str(&c.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(c);
        }
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sql::csv::{Enc, Opts, Reader};

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
        assert_eq!(read_back(&f), [s(&["a", "b"]), s(&["1", ""]), s(&["2", "x"]), s(&["", "\\N"])]);
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
    fn numbers_booleans_and_big_integers_keep_their_text() {
        let f = flatten(r#"[{"i": -7, "big": 18446744073709551615, "f": 12.5, "t": true, "n": null}]"#).unwrap();
        assert_eq!(read_back(&f)[1], s(&["-7", "18446744073709551615", "12.5", "true", "\\N"]));
    }

    #[test]
    fn a_repeated_key_keeps_its_last_value_and_a_dotted_key_is_one_column() {
        let f = flatten(r#"[{"a": 1, "a": 2, "x.y": 3}]"#).unwrap();
        assert_eq!(f.names, ["a", "x.y"]);
        assert_eq!(read_back(&f)[1], s(&["2", "3"]));
    }

    #[test]
    fn what_is_not_an_array_of_objects_says_where_it_went_wrong() {
        assert_eq!(flatten("[{\"a\":1}, 5]").unwrap_err(), "Item 2 of the array is a number, not an object.");
        assert!(flatten("{\"a\":1}\n\"x\"").unwrap_err().starts_with("Value 2 is a string"));
        assert!(flatten("{\"a\":1}\n{\"a\":").unwrap_err().starts_with("Not valid JSON at line 2"));
        assert_eq!(flatten("[]").unwrap_err(), "The text holds no objects to import.");
        assert_eq!(flatten("[{}]").unwrap_err(), "The objects have no keys.");
        assert_eq!(flatten("   ").unwrap_err(), "The text holds no objects to import.");
    }

    #[test]
    fn a_byte_order_mark_does_not_hide_the_json() {
        let f = flatten("\u{feff}[{\"a\": 1}]").unwrap();
        assert_eq!(f.names, ["a"]);
        assert!(looks_like_json("pasted", b"\xEF\xBB\xBF  [{\"a\":1}]"));
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
