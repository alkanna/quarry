//! JSON and JSON Lines import, the part that needs no server. A file or pasted text holding objects is flattened to the
//! CSV the import already reads: the keys become the header, nested objects and arrays become JSON text, so the column
//! mapping, the check, duplicate handling, batching and undo all work on it unchanged.
//!
//! The reader is written here, not taken from serde_json, because the import needs what that crate's value type loses:
//! a number keeps its own text (a DECIMAL must not pass through f64) and an object keeps its key order. The text is read
//! twice, one row at a time, so only the text and the CSV it becomes are in memory, never the parsed file.

use super::csv::{self, Source, MARK};
use std::io::Read;

/// The most a file may hold. It is read whole to find every key before the first row can be written out.
pub const JSON_MAX: u64 = 256 << 20;

/// How deep arrays and objects may nest, so a hostile file cannot overflow the stack.
const DEPTH_MAX: usize = 128;

/// A JSON value that keeps the order of an object's keys and a number's own text.
#[derive(Debug, PartialEq)]
enum J {
    Null,
    Bool(bool),
    Num(String),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    /// Compact JSON text, keys in their original order.
    fn write(&self, out: &mut String) {
        match self {
            J::Null => out.push_str("null"),
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Num(n) => out.push_str(n),
            J::Str(s) => write_str(s, out),
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
                    write_str(k, out);
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

fn write_str(s: &str, out: &mut String) {
    out.push_str(&serde_json::to_string(s).unwrap_or_default());
}

/// A reader over the text, remembering where it is so an error can say line and column.
struct P<'a> {
    t: &'a str,
    b: &'a [u8],
    i: usize,
}

type R<T> = Result<T, (usize, String)>;

impl<'a> P<'a> {
    fn new(t: &'a str) -> P<'a> {
        P { t, b: t.as_bytes(), i: 0 }
    }

    fn ws(&mut self) {
        while self.b.get(self.i).is_some_and(u8::is_ascii_whitespace) {
            self.i += 1;
        }
    }

    fn err<T>(&self, msg: &str) -> R<T> {
        Err((self.i, msg.to_string()))
    }

    fn lit(&mut self, word: &str, v: J) -> R<J> {
        if self.t[self.i..].starts_with(word) {
            self.i += word.len();
            Ok(v)
        } else {
            self.err("expected a value")
        }
    }

    fn value(&mut self, depth: usize) -> R<J> {
        if depth > DEPTH_MAX {
            return self.err("nested too deeply");
        }
        match self.b.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(J::Obj(v));
                }
                loop {
                    self.ws();
                    if self.b.get(self.i) != Some(&b'"') {
                        return self.err("expected a key in quotes");
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.b.get(self.i) != Some(&b':') {
                        return self.err("expected a colon after the key");
                    }
                    self.i += 1;
                    self.ws();
                    v.push((k, self.value(depth + 1)?));
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(J::Obj(v));
                        }
                        _ => return self.err("expected a comma or a closing brace"),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(J::Arr(v));
                }
                loop {
                    self.ws();
                    v.push(self.value(depth + 1)?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(J::Arr(v));
                        }
                        _ => return self.err("expected a comma or a closing bracket"),
                    }
                }
            }
            Some(b'"') => Ok(J::Str(self.string()?)),
            Some(b't') => self.lit("true", J::Bool(true)),
            Some(b'f') => self.lit("false", J::Bool(false)),
            Some(b'n') => self.lit("null", J::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => self.err("expected a value"),
            None => self.err("the text ends where a value should be"),
        }
    }

    /// The number as written: `-0`, `1E5` and 40 digits stay what they are.
    fn number(&mut self) -> R<J> {
        let start = self.i;
        let digits = |p: &mut P| {
            let from = p.i;
            while p.b.get(p.i).is_some_and(u8::is_ascii_digit) {
                p.i += 1;
            }
            p.i > from
        };
        if self.b.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        match self.b.get(self.i) {
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => {
                digits(self);
            }
            _ => return self.err("a number needs digits"),
        }
        if self.b.get(self.i) == Some(&b'.') {
            self.i += 1;
            if !digits(self) {
                return self.err("a number needs digits after the point");
            }
        }
        if matches!(self.b.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.b.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if !digits(self) {
                return self.err("a number needs digits in its exponent");
            }
        }
        Ok(J::Num(self.t[start..self.i].to_string()))
    }

    fn hex4(&mut self) -> R<u32> {
        let h = self.t.get(self.i..self.i + 4).filter(|h| h.bytes().all(|c| c.is_ascii_hexdigit()));
        match h.and_then(|h| u32::from_str_radix(h, 16).ok()) {
            Some(n) => {
                self.i += 4;
                Ok(n)
            }
            None => self.err("a \\u escape needs four hex digits"),
        }
    }

    fn string(&mut self) -> R<String> {
        self.i += 1;
        let mut out = String::new();
        loop {
            let from = self.i;
            while self.b.get(self.i).is_some_and(|&c| c != b'"' && c != b'\\' && c >= 0x20) {
                self.i += 1;
            }
            out.push_str(&self.t[from..self.i]);
            match self.b.get(self.i) {
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let c = self.b.get(self.i).copied();
                    self.i += 1;
                    match c {
                        Some(b'"') => out.push('"'),
                        Some(b'\\') => out.push('\\'),
                        Some(b'/') => out.push('/'),
                        Some(b'b') => out.push('\u{8}'),
                        Some(b'f') => out.push('\u{c}'),
                        Some(b'n') => out.push('\n'),
                        Some(b'r') => out.push('\r'),
                        Some(b't') => out.push('\t'),
                        Some(b'u') => {
                            let hi = self.hex4()?;
                            let n = if (0xD800..0xDC00).contains(&hi) {
                                if !self.t[self.i..].starts_with("\\u") {
                                    return self.err("a high surrogate needs its low half");
                                }
                                self.i += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return self.err("a high surrogate needs its low half");
                                }
                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                            } else {
                                hi
                            };
                            match char::from_u32(n) {
                                Some(c) => out.push(c),
                                None => return self.err("a lone surrogate"),
                            }
                        }
                        _ => return self.err("an unknown escape"),
                    }
                }
                Some(_) => return self.err("a line break or control character inside a string"),
                None => return self.err("the string is not closed"),
            }
        }
    }
}

/// Line and column (1-based) of byte `at` of `t`.
fn place(t: &str, at: usize) -> (usize, usize) {
    let head = &t.as_bytes()[..at.min(t.len())];
    let line = head.iter().filter(|&&b| b == b'\n').count() + 1;
    let col = head.iter().rev().take_while(|&&b| b != b'\n').count() + 1;
    (line, col)
}

/// Calls `f` with each object of the text in turn: the items of an array, or objects one after another. True when they
/// were not in an array (JSON Lines). Only one object is alive at a time.
fn each_row(t: &str, mut f: impl FnMut(Vec<(String, J)>)) -> Result<bool, String> {
    let t = t.strip_prefix('\u{feff}').unwrap_or(t);
    let mut p = P::new(t);
    let bad = |p: &P, (at, msg): (usize, String)| {
        let (l, c) = place(p.t, at);
        format!("Not valid JSON at line {l}, column {c}: {msg}")
    };
    let (mut seen, mut lines) = (0usize, false);
    loop {
        p.ws();
        if p.i >= p.b.len() {
            return Ok(lines);
        }
        seen += 1;
        if p.b[p.i] == b'[' {
            p.i += 1;
            let mut n = 0;
            p.ws();
            if p.b.get(p.i) == Some(&b']') {
                p.i += 1;
                continue;
            }
            loop {
                p.ws();
                let item = p.value(1).map_err(|e| bad(&p, e))?;
                n += 1;
                match item {
                    J::Obj(o) => f(o),
                    other => return Err(format!("Item {n} of the array is {}, not an object.", other.kind())),
                }
                p.ws();
                match p.b.get(p.i) {
                    Some(b',') => p.i += 1,
                    Some(b']') => {
                        p.i += 1;
                        break;
                    }
                    _ => return Err(bad(&p, (p.i, "expected a comma or a closing bracket".into()))),
                }
            }
        } else {
            match p.value(0).map_err(|e| bad(&p, e))? {
                J::Obj(o) => {
                    lines = true;
                    f(o);
                }
                other => return Err(format!("Value {seen} is {}, not an object. Expected an array of objects or one object per line.", other.kind())),
            }
        }
    }
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
        for (k, v) in row {
            let i = *at.entry(k.clone()).or_insert_with(|| {
                names.push(k);
                nested.push(false);
                names.len() - 1
            });
            if matches!(v, J::Arr(_) | J::Obj(_)) {
                nested[i] = true;
            }
        }
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
        let mut cells: Vec<Option<J>> = (0..names.len()).map(|_| None).collect();
        // A repeated key keeps its last value, as a parser that builds a map does.
        for (k, v) in row {
            cells[at[&k]] = Some(v);
        }
        let texts: Vec<String> = cells.iter().map(cell_text).collect();
        push_row(&mut out, texts.iter().map(String::as_str));
    })?;
    Ok(Flat { csv: out.into_bytes(), names, nested, rows, lines })
}

/// What a value is written as in the CSV, with the lead `csv::convert` reads (see `MARK`).
fn cell_text(c: &Option<J>) -> String {
    match c {
        None => String::new(),
        Some(J::Null) => format!("{MARK}N"),
        Some(J::Bool(b)) => b.to_string(),
        Some(J::Num(n)) => n.clone(),
        // Text that CSV would read as a NULL, or that opens like this lead, is marked as the text it is.
        Some(J::Str(s)) if s == "\\N" || s.starts_with(MARK) => format!("{MARK}S{s}"),
        Some(J::Str(s)) => s.clone(),
        Some(v) => {
            let mut t = String::new();
            v.write(&mut t);
            t
        }
    }
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
        assert!(flatten(&deep).unwrap_err().contains("nested too deeply"));
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
