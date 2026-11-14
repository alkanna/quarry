//! CSV import, the part that needs no server: reading a file of any size one record at a time, finding its
//! format, matching its columns to a table's, and checking every value against the type of its column before
//! anything is sent. Values leave here as `Param`s that are bound to placeholders, never written into SQL.

use super::structure::{self, Structure};
use super::value::{self, Kind, Param};
use super::Dialect;
use mongodb::bson::Bson;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Shared bytes that a cursor can read.
pub struct Shared(Arc<Vec<u8>>);

impl AsRef<[u8]> for Shared {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// A source being read. An enum rather than a boxed reader, which the compiler cannot prove Send inside the
/// futures that run an import.
pub enum Open {
    File(std::fs::File),
    Mem(std::io::Cursor<Shared>),
    /// A gzip file read as the text inside it. Boxed because it holds an `Open` itself.
    Gz(Box<flate2::read::MultiGzDecoder<Open>>),
}

impl Read for Open {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Open::File(f) => f.read(buf),
            Open::Mem(c) => c.read(buf),
            Open::Gz(g) => g.read(buf),
        }
    }
}

/// Where the bytes come from. The demo and the tests use memory, the dialog a file.
#[derive(Clone)]
pub enum Source {
    File(PathBuf),
    Mem(Arc<Vec<u8>>),
}

impl Source {
    pub fn open(&self) -> std::io::Result<Open> {
        Ok(match self {
            Source::File(p) => Open::File(std::fs::File::open(p)?),
            Source::Mem(b) => Open::Mem(std::io::Cursor::new(Shared(b.clone()))),
        })
    }

    pub fn len(&self) -> u64 {
        match self {
            Source::File(p) => std::fs::metadata(p).map_or(0, |m| m.len()),
            Source::Mem(b) => b.len() as u64,
        }
    }

    /// Whether the bytes are gzip, told by their magic number and not the file name, so a dump renamed without .gz still runs.
    fn is_gzip(&self) -> bool {
        self.head(2).is_ok_and(|h| h == [0x1f, 0x8b])
    }

    /// `open` for a SQL script: a gzip file is decompressed as it is read, so a dump written with the .gz option runs as it is.
    pub fn open_script(&self) -> std::io::Result<Open> {
        let raw = self.open()?;
        Ok(if self.is_gzip() { Open::Gz(Box::new(flate2::read::MultiGzDecoder::new(raw))) } else { raw })
    }

    /// The size of the script `open_script` reads, which the progress of a run is measured against. A gzip file ends with the
    /// size of its text modulo 4 GiB, which is as close as a stream gets without reading it twice.
    pub fn script_len(&self) -> u64 {
        use std::io::{Seek, SeekFrom};
        let len = self.len();
        if len < 18 || !self.is_gzip() {
            return len;
        }
        let mut tail = [0u8; 4];
        let read = match self {
            Source::File(p) => std::fs::File::open(p).and_then(|mut f| f.seek(SeekFrom::End(-4)).and_then(|_| f.read_exact(&mut tail))),
            Source::Mem(b) => {
                tail.copy_from_slice(&b[b.len() - 4..]);
                Ok(())
            }
        };
        if read.is_err() { len } else { u32::from_le_bytes(tail) as u64 }
    }

    /// The first `max` bytes, enough to find the format.
    pub fn head(&self, max: usize) -> std::io::Result<Vec<u8>> {
        let mut out = Vec::new();
        self.open()?.take(max as u64).read_to_end(&mut out)?;
        Ok(out)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Enc {
    Utf8,
    Utf16Le,
    Utf16Be,
    Windows1252,
}

impl Enc {
    pub const ALL: [(Enc, &'static str); 4] = [(Enc::Utf8, "UTF-8"), (Enc::Utf16Le, "UTF-16 LE"), (Enc::Utf16Be, "UTF-16 BE"), (Enc::Windows1252, "Windows-1252")];

    pub fn label(self) -> &'static str {
        Enc::ALL.iter().find(|(e, _)| *e == self).map_or("", |(_, l)| l)
    }
}

/// How a file is read. The same options drive the preview, the check and the import.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Opts {
    pub delim: char,
    /// None reads every quote character as plain text.
    pub quote: Option<char>,
    pub enc: Enc,
}

/// Windows-1252 differs from Latin-1 only in 0x80 to 0x9F; the five holes keep their control characters.
const CP1252: [char; 32] = [
    '\u{20AC}', '\u{81}', '\u{201A}', '\u{192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}', '\u{2C6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{8D}', '\u{17D}', '\u{8F}', '\u{90}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}', '\u{2DC}', '\u{2122}', '\u{161}', '\u{203A}', '\u{153}', '\u{9D}', '\u{17E}', '\u{178}',
];

/// The encoding of `sample`: a byte order mark decides, else valid UTF-8 (a character cut by the end of the
/// sample is fine), else UTF-16 when every other byte is NUL, else Windows-1252, which reads any byte.
pub fn detect_enc(sample: &[u8]) -> Enc {
    if sample.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Enc::Utf8;
    }
    if sample.starts_with(&[0xFF, 0xFE]) {
        return Enc::Utf16Le;
    }
    if sample.starts_with(&[0xFE, 0xFF]) {
        return Enc::Utf16Be;
    }
    match std::str::from_utf8(sample) {
        Ok(_) => {}
        Err(e) if e.error_len().is_none() => {}
        Err(_) => return Enc::Windows1252,
    }
    let pairs = sample.len() / 2;
    if pairs >= 4 {
        let (even, odd) = (sample.iter().step_by(2).filter(|b| **b == 0).count(), sample.iter().skip(1).step_by(2).filter(|b| **b == 0).count());
        if odd * 10 >= pairs * 4 && even == 0 {
            return Enc::Utf16Le;
        }
        if even * 10 >= pairs * 4 && odd == 0 {
            return Enc::Utf16Be;
        }
    }
    Enc::Utf8
}

/// Bytes to characters, a buffer at a time.
struct Chars<R: Read> {
    r: R,
    enc: Enc,
    buf: Vec<u8>,
    pos: usize,
    len: usize,
    eof: bool,
    consumed: u64,
    started: bool,
}

impl<R: Read> Chars<R> {
    fn new(r: R, enc: Enc) -> Chars<R> {
        Chars { r, enc, buf: vec![0; 64 << 10], pos: 0, len: 0, eof: false, consumed: 0, started: false }
    }

    fn refill(&mut self, want: usize) -> Result<(), String> {
        if self.pos > 0 && self.len - self.pos < want {
            self.buf.copy_within(self.pos..self.len, 0);
            self.len -= self.pos;
            self.pos = 0;
        }
        while !self.eof && self.len - self.pos < want.max(1) {
            match self.r.read(&mut self.buf[self.len..]) {
                Ok(0) => self.eof = true,
                Ok(n) => self.len += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(format!("Could not read the file: {e}")),
            }
        }
        Ok(())
    }

    fn byte(&mut self) -> Result<Option<u8>, String> {
        if self.pos >= self.len {
            self.refill(1)?;
        }
        if self.pos >= self.len {
            return Ok(None);
        }
        let b = self.buf[self.pos];
        self.pos += 1;
        self.consumed += 1;
        Ok(Some(b))
    }

    fn start(&mut self) -> Result<(), String> {
        self.started = true;
        self.refill(4)?;
        let head = &self.buf[self.pos..self.len];
        let skip = match self.enc {
            Enc::Utf8 if head.starts_with(&[0xEF, 0xBB, 0xBF]) => 3,
            Enc::Utf16Le if head.starts_with(&[0xFF, 0xFE]) => 2,
            Enc::Utf16Be if head.starts_with(&[0xFE, 0xFF]) => 2,
            _ => 0,
        };
        self.pos += skip;
        self.consumed += skip as u64;
        Ok(())
    }

    fn next(&mut self) -> Result<Option<char>, String> {
        if !self.started {
            self.start()?;
        }
        let at = self.consumed;
        let Some(b0) = self.byte()? else { return Ok(None) };
        match self.enc {
            Enc::Windows1252 => Ok(Some(if (0x80..0xA0).contains(&b0) { CP1252[(b0 - 0x80) as usize] } else { b0 as char })),
            Enc::Utf8 => {
                if b0 < 0x80 {
                    return Ok(Some(b0 as char));
                }
                let need = match b0 {
                    0xC2..=0xDF => 1,
                    0xE0..=0xEF => 2,
                    0xF0..=0xF4 => 3,
                    _ => 0,
                };
                let mut bytes = vec![b0];
                for _ in 0..need {
                    match self.byte()? {
                        Some(b) => bytes.push(b),
                        None => break,
                    }
                }
                match std::str::from_utf8(&bytes) {
                    Ok(s) if need > 0 => Ok(s.chars().next()),
                    _ => Err(format!("The file is not valid UTF-8 near byte {at}. Choose the encoding Windows-1252 instead.")),
                }
            }
            Enc::Utf16Le | Enc::Utf16Be => {
                let le = self.enc == Enc::Utf16Le;
                let unit = move |a: u8, b: u8| if le { u16::from_le_bytes([a, b]) } else { u16::from_be_bytes([a, b]) };
                let odd = || format!("The file ends in the middle of a UTF-16 character near byte {at}.");
                let b1 = self.byte()?.ok_or_else(odd)?;
                let u = unit(b0, b1);
                let mut units = vec![u];
                if (0xD800..0xDC00).contains(&u) {
                    let (c, d) = (self.byte()?.ok_or_else(odd)?, self.byte()?.ok_or_else(odd)?);
                    units.push(unit(c, d));
                }
                char::decode_utf16(units).next().and_then(Result::ok).map(Some).ok_or_else(|| format!("The file is not valid UTF-16 near byte {at}."))
            }
        }
    }
}

/// All of `bytes` as text in the encoding `enc`, a byte order mark dropped. For files read whole, like JSON.
pub fn decode_text(bytes: &[u8], enc: Enc) -> Result<String, String> {
    if enc == Enc::Utf8 && let Ok(s) = std::str::from_utf8(bytes) {
        return Ok(s.strip_prefix('\u{feff}').unwrap_or(s).to_string());
    }
    let mut ch = Chars::new(std::io::Cursor::new(bytes), enc);
    let mut out = String::with_capacity(bytes.len());
    while let Some(c) = ch.next()? {
        out.push(c);
    }
    Ok(out)
}

/// The largest value a field may hold. A quote that never closes would otherwise swallow the whole file.
const FIELD_MAX: usize = 16 << 20;

/// Reads records one at a time. A quoted value may hold the delimiter, quotes (doubled) and line breaks; a
/// backslash is a plain character, as in RFC 4180. Empty lines are skipped.
pub struct Reader<R: Read> {
    ch: Chars<R>,
    peeked: Option<Option<char>>,
    opts: Opts,
    /// Records read, the header included, so a row number matches the one a spreadsheet shows.
    pub records: u64,
}

impl<R: Read> Reader<R> {
    pub fn new(r: R, opts: Opts) -> Reader<R> {
        Reader { ch: Chars::new(r, opts.enc), peeked: None, opts, records: 0 }
    }

    /// Bytes read so far.
    pub fn bytes(&self) -> u64 {
        self.ch.consumed
    }

    fn take(&mut self) -> Result<Option<char>, String> {
        match self.peeked.take() {
            Some(c) => Ok(c),
            None => self.ch.next(),
        }
    }

    fn peek(&mut self) -> Result<Option<char>, String> {
        if self.peeked.is_none() {
            self.peeked = Some(self.ch.next()?);
        }
        Ok(self.peeked.flatten())
    }

    pub fn next_record(&mut self) -> Result<Option<Vec<String>>, String> {
        loop {
            let (mut fields, mut field) = (Vec::new(), String::new());
            let (mut quoted, mut seen) = (false, false);
            let mut in_quotes = false;
            loop {
                let Some(c) = self.take()? else {
                    if in_quotes {
                        return Err(format!("The file ends inside a quoted value that starts in record {}. The quote character may be wrong.", self.records + 1));
                    }
                    if !seen {
                        return Ok(None);
                    }
                    fields.push(field);
                    self.records += 1;
                    return Ok(Some(fields));
                };
                seen = true;
                if field.len() > FIELD_MAX {
                    return Err(format!("A value in record {} is over 16 MB. The quote character may be wrong.", self.records + 1));
                }
                if in_quotes {
                    if Some(c) == self.opts.quote {
                        if self.peek()? == Some(c) {
                            self.take()?;
                            field.push(c);
                        } else {
                            in_quotes = false;
                        }
                    } else {
                        field.push(c);
                    }
                } else if Some(c) == self.opts.quote && field.is_empty() && !quoted {
                    in_quotes = true;
                    quoted = true;
                } else if c == self.opts.delim {
                    fields.push(std::mem::take(&mut field));
                    quoted = false;
                } else if c == '\n' || c == '\r' {
                    if c == '\r' && self.peek()? == Some('\n') {
                        self.take()?;
                    }
                    // A line with nothing on it is not a record.
                    if fields.is_empty() && field.is_empty() && !quoted {
                        break;
                    }
                    fields.push(field);
                    self.records += 1;
                    return Ok(Some(fields));
                } else {
                    field.push(c);
                }
            }
        }
    }
}

/// Records of `sample` up to `max`, stopping quietly at a cut character or an open quote at its end.
fn sample_records(sample: &[u8], opts: Opts, max: usize) -> Vec<Vec<String>> {
    let mut r = Reader::new(std::io::Cursor::new(sample), opts);
    let mut out = Vec::new();
    while out.len() < max {
        match r.next_record() {
            Ok(Some(rec)) => out.push(rec),
            _ => break,
        }
    }
    out
}

pub(super) const DELIMS: [char; 4] = [',', ';', '\t', '|'];

/// The delimiter that splits the sample into the most rows of one width. A comma wins a tie.
pub(super) fn detect_delim(sample: &[u8], enc: Enc, quote: Option<char>) -> char {
    let mut best = (',', 0usize);
    for d in DELIMS {
        let recs = sample_records(sample, Opts { delim: d, quote, enc }, 30);
        let mut counts = std::collections::HashMap::new();
        for r in recs.iter().skip(1).chain(recs.first()) {
            *counts.entry(r.len()).or_insert(0usize) += 1;
        }
        let score = counts.into_iter().filter(|(w, _)| *w > 1).map(|(w, n)| n * w).max().unwrap_or(0);
        if score > best.1 {
            best = (d, score);
        }
    }
    best.0
}

/// Whether the first record looks like column names: all filled in, all different, none a number.
pub(super) fn guess_header(rows: &[Vec<String>]) -> bool {
    let Some(first) = rows.first() else { return true };
    let mut seen = std::collections::HashSet::new();
    first.iter().all(|c| !c.trim().is_empty() && c.trim().parse::<f64>().is_err() && seen.insert(c.trim().to_lowercase()))
}

/// The first records of a file read with `opts`.
pub fn sample(src: &Source, opts: Opts, max: usize) -> Result<Vec<Vec<String>>, String> {
    let head = src.head(1 << 20).map_err(|e| format!("Could not read the file: {e}"))?;
    Ok(sample_records(&head, opts, max))
}

/// What a file looks like: its encoding, delimiter and whether it starts with column names.
pub fn sniff(src: &Source) -> Result<(Opts, bool), String> {
    let head = src.head(64 << 10).map_err(|e| format!("Could not read the file: {e}"))?;
    let enc = detect_enc(&head);
    let delim = detect_delim(&head, enc, Some('"'));
    let opts = Opts { delim, quote: Some('"'), enc };
    let rows = sample_records(&head, opts, 3);
    Ok((opts, guess_header(&rows)))
}

/// One column of the table as the check needs it.
#[derive(Clone, Debug, PartialEq)]
pub struct TCol {
    pub name: String,
    pub full_type: String,
    /// PostgreSQL: the type a value is cast to, `full_type` without its length, precision or bit width. An explicit cast to
    /// `varchar(3)` or `bit(4)` cuts or pads silently, so the cast goes to the bare type and the INSERT applies the length
    /// by assignment, which refuses a value that does not fit. Equal to `full_type` on MySQL.
    pub cast_type: String,
    pub kind: Kind,
    pub nullable: bool,
    pub auto_increment: bool,
    pub generated: bool,
    /// A default, an auto-increment or NULL fills the column when the file leaves it out.
    pub fills_itself: bool,
    /// The most characters a CHAR or VARCHAR holds.
    pub max_chars: Option<usize>,
    /// The values of an ENUM or SET.
    pub choices: Vec<String>,
    /// A PostgreSQL column: `full_type` is what format_type wrote, so values are checked by the server's cast and not by MySQL's rules.
    pub pg: bool,
}

/// The values listed in `'a','b''c'`, the way COLUMN_TYPE writes an ENUM or SET.
pub(super) fn quoted_list(args: &str) -> Vec<String> {
    let (mut out, mut cur, mut open) = (Vec::new(), String::new(), false);
    let mut it = args.chars().peekable();
    while let Some(c) = it.next() {
        match (c, open) {
            ('\'', false) => open = true,
            ('\'', true) if it.peek() == Some(&'\'') => {
                it.next();
                cur.push('\'');
            }
            ('\'', true) => {
                open = false;
                out.push(std::mem::take(&mut cur));
            }
            // COLUMN_TYPE writes a backslash in a member escaped.
            ('\\', true) => cur.extend(it.next()),
            (_, true) => cur.push(c),
            _ => {}
        }
    }
    out
}

impl TCol {
    pub fn from_column(c: &structure::Column, pg: bool) -> TCol {
        let extra = c.extra.to_ascii_uppercase();
        let (base, args) = (c.base_type(), c.args());
        // The catalog's own kind tells a domain over int or an enum apart, which the type name alone cannot.
        let kind = c.kind.unwrap_or_else(|| Kind::from_column_type(&c.full_type));
        let auto_increment = extra.contains("AUTO_INCREMENT");
        // `character varying(20)[]` has the length of its elements, not of the text of the array.
        let sized = if pg { matches!(base.as_str(), "CHARACTER" | "VARCHAR") && kind == Kind::Text && !c.full_type.ends_with(']') } else { matches!(base.as_str(), "CHAR" | "VARCHAR") };
        let max_chars = sized.then(|| args.trim().parse().ok()).flatten();
        let choices = if !pg && matches!(kind, Kind::Enum | Kind::Set) { quoted_list(&args) } else { Vec::new() };
        // A computed column and an identity that is always generated cannot be given a value.
        let generated = if pg { extra.contains("GENERATED ALWAYS AS") } else { extra.contains("VIRTUAL") || extra.contains("STORED GENERATED") || extra.contains("PERSISTENT") };
        let cast_type = c.cast_type.clone().filter(|_| pg).unwrap_or_else(|| c.full_type.clone());
        TCol { name: c.name.clone(), cast_type, full_type: c.full_type.clone(), kind, nullable: c.nullable, auto_increment, generated, fills_itself: c.default.is_some() || c.default_expr || c.nullable || auto_increment, max_chars, choices, pg }
    }
}

/// What the import dialog needs to know about a table.
#[derive(Clone, Debug, Default)]
pub struct TableInfo {
    pub cols: Vec<TCol>,
    pub pk: Vec<String>,
    /// ENGINE of information_schema.TABLES, which decides whether a failed import can be rolled back.
    pub engine: String,
    /// A PostgreSQL table: every write there is transactional and the keys of new rows come from RETURNING.
    pub pg: bool,
    /// The primary key is DEFERRABLE, which ON CONFLICT refuses, so Skip and Update cannot work.
    pub pk_deferrable: bool,
}

impl TableInfo {
    pub fn from_structure(s: &Structure) -> TableInfo {
        let pg = s.is_pg();
        // A PostgreSQL primary key is flagged on its index, whatever the constraint is called.
        let pk = s.indexes.iter().filter(|i| if pg { i.primary } else { i.name == "PRIMARY" }).flat_map(|i| i.cols.iter().map(|c| c.name.clone())).collect();
        let pk_deferrable = pg && s.indexes.iter().any(|i| i.primary && i.deferrable);
        TableInfo { cols: s.columns.iter().map(|c| TCol::from_column(c, pg)).collect(), pk, engine: s.status.engine.clone(), pg, pk_deferrable }
    }

    /// Whether rows written inside a transaction go away on a rollback. MyISAM, Aria, MEMORY, CSV and ARCHIVE
    /// keep every row the moment it is written. An unknown engine counts as transactional, like InnoDB, the default.
    pub fn transactional(&self) -> bool {
        engine_rolls_back(&self.engine)
    }

    pub fn col(&self, name: &str) -> Option<&TCol> {
        self.cols.iter().find(|c| c.name == name)
    }
}

/// `TableInfo::transactional` for a bare engine name. Empty, as on PostgreSQL, rolls back.
pub fn engine_rolls_back(engine: &str) -> bool {
    engine.is_empty() || ["InnoDB", "XtraDB", "NDB", "NDBCLUSTER", "TokuDB", "RocksDB"].iter().any(|e| e.eq_ignore_ascii_case(engine))
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// For each CSV column the table column of the same name, ignoring case and punctuation (`Order ID` finds
/// `order_id`). A table column is used once, and generated columns are never filled. Exact names are matched first, so a
/// table with `Name` and `name` gives each its own file column whatever order the header is in.
pub fn auto_map(names: &[String], cols: &[TCol]) -> Vec<Option<usize>> {
    let mut used = vec![false; cols.len()];
    let mut out: Vec<Option<usize>> = vec![None; names.len()];
    for exact in [true, false] {
        for (ni, n) in names.iter().enumerate() {
            if out[ni].is_some() {
                continue;
            }
            let key = norm(n);
            let at = (!key.is_empty()).then(|| cols.iter().enumerate().position(|(i, c)| !used[i] && !c.generated && if exact { c.name == *n } else { norm(&c.name) == key })).flatten();
            if let Some(i) = at {
                used[i] = true;
                out[ni] = Some(i);
            }
        }
    }
    out
}

/// How a date written with slashes or dots is read: 09/11/2026 is the 9th of November or the 11th of September.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum DateFmt {
    DayFirst,
    MonthFirst,
}

/// The pieces of `a/b/2026 12:41` as (a, b, year, time), or None for text that is not written that way.
fn split_date(s: &str) -> Option<(u32, u32, u32, &str)> {
    let s = s.trim();
    let (d, t) = s.split_once([' ', 'T']).map_or((s, ""), |(d, t)| (d, t.trim()));
    let p: Vec<&str> = d.split(['/', '.', '-']).collect();
    let n = |x: &str, len: usize| (x.len() == len && x.bytes().all(|b| b.is_ascii_digit())).then(|| x.parse::<u32>().ok()).flatten();
    let two = |x: &str| (1..=2).contains(&x.len()).then(|| x.parse::<u32>().ok()).flatten().filter(|_| x.bytes().all(|b| b.is_ascii_digit()));
    match p[..] {
        [a, b, y] if y.len() == 4 => Some((two(a)?, two(b)?, n(y, 4)?, t)),
        _ => None,
    }
}

/// Text already in the server's own form, YYYY-MM-DD.
fn is_iso(s: &str) -> bool {
    let b = s.trim().as_bytes();
    b.len() >= 10 && b[4] == b'-' && b[7] == b'-' && b[..4].iter().all(u8::is_ascii_digit)
}

/// Which way round the dates of a column are written. A part above 12 settles it; otherwise day first, the
/// reading most of the world uses, which the dialog shows so it can be turned round.
pub fn guess_date_fmt(values: &[&str]) -> DateFmt {
    let parts: Vec<(u32, u32)> = values.iter().filter_map(|v| split_date(v)).map(|(a, b, _, _)| (a, b)).collect();
    if parts.iter().any(|(a, _)| *a > 12) {
        DateFmt::DayFirst
    } else if parts.iter().any(|(_, b)| *b > 12) {
        DateFmt::MonthFirst
    } else {
        DateFmt::DayFirst
    }
}

/// Whether any of the values needs reading as day/month or month/day.
pub fn has_local_dates(values: &[&str]) -> bool {
    values.iter().any(|v| !v.is_empty() && !is_iso(v) && split_date(v).is_some())
}

/// `2026-11-09 12:41:00` for `09/11/2026 12:41`, or the text as it is when it is already in the server's form
/// or is not a date at all (the type check then reports it).
pub(super) fn to_iso(s: &str, fmt: DateFmt) -> String {
    if is_iso(s) {
        return s.trim().to_string();
    }
    let Some((a, b, y, t)) = split_date(s) else { return s.trim().to_string() };
    let (d, m) = if fmt == DateFmt::DayFirst { (a, b) } else { (b, a) };
    let date = format!("{y:04}-{m:02}-{d:02}");
    if t.is_empty() {
        return date;
    }
    let parts: Vec<&str> = t.split(':').collect();
    let hms = match parts[..] {
        [h, mi] => format!("{h:0>2}:{mi:0>2}:00"),
        [h, mi, se] => format!("{h:0>2}:{mi:0>2}:{se:0>2}"),
        _ => return format!("{date} {t}"),
    };
    format!("{date} {hms}")
}

/// What a cell becomes for its column: a value to bind, or why it cannot be one.
pub fn convert(col: &TCol, raw: &str, empty_null: bool, fmt: DateFmt) -> Result<Param, String> {
    if col.pg {
        return convert_pg(col, raw, empty_null, fmt);
    }
    let text_like = matches!(col.kind, Kind::Text | Kind::Binary);
    let null = raw == "\\N" || (raw.is_empty() && (empty_null || !text_like));
    if null {
        return if col.nullable || col.auto_increment { Ok(Param::Null) } else { Err("Empty, but the column does not allow NULL. Skip it or fill it in the file.".into()) };
    }
    let parsed = match col.kind {
        // Text is taken as it is: parse_edit reads the word NULL as a NULL, which would turn a name into one.
        Kind::Text | Kind::Enum | Kind::Set => Bson::String(raw.into()),
        Kind::Json => {
            serde_json::from_str::<serde_json::Value>(raw).map_err(|e| format!("Not valid JSON: {e}"))?;
            Bson::String(raw.into())
        }
        Kind::Binary if !raw.starts_with("0x") && !raw.starts_with("0X") => Bson::String(raw.into()),
        Kind::Int | Kind::UInt | Kind::Year if raw.eq_ignore_ascii_case("true") => Bson::Int64(1),
        Kind::Int | Kind::UInt | Kind::Year if raw.eq_ignore_ascii_case("false") => Bson::Int64(0),
        Kind::Date | Kind::DateTime => value::parse_edit(Dialect::MySql, col.kind, &to_iso(raw, fmt))?,
        k => value::parse_edit(Dialect::MySql, k, raw)?,
    };
    // parse_edit reads the word NULL as a NULL for numbers and dates, which a NOT NULL column refuses or, outside
    // strict mode, stores as 0.
    if parsed == Bson::Null && !col.nullable && !col.auto_increment {
        return Err("NULL, but the column does not allow NULL. Skip it or fill it in the file.".into());
    }
    if let (Some(max), Bson::String(s)) = (col.max_chars, &parsed) {
        let n = s.chars().count();
        if n > max {
            return Err(format!("{n} characters, the column holds {max}."));
        }
    }
    if let (Kind::Enum, Bson::String(s)) = (col.kind, &parsed) && !col.choices.iter().any(|c| c.eq_ignore_ascii_case(s)) {
        return Err(format!("Not one of {}.", col.choices.iter().take(4).cloned().collect::<Vec<_>>().join(", ")));
    }
    if let (Kind::Set, Bson::String(s)) = (col.kind, &parsed) && let Some(bad) = s.split(',').filter(|v| !v.is_empty()).find(|v| !col.choices.iter().any(|c| c.eq_ignore_ascii_case(v))) {
        return Err(format!("{bad} is not one of {}.", col.choices.iter().take(4).cloned().collect::<Vec<_>>().join(", ")));
    }
    Ok(Param::for_kind(col.kind, &parsed))
}

/// `convert` for a PostgreSQL column. Every value travels as text and the `CAST($n AS type)` around its placeholder makes the
/// value, so what is checked here is what the server would refuse late and with a worse message: a number that is not one, a
/// UUID, JSON, hex. Dates and times are left to the cast, which reads more spellings than MySQL does.
fn convert_pg(col: &TCol, raw: &str, empty_null: bool, fmt: DateFmt) -> Result<Param, String> {
    let text_like = matches!(col.kind, Kind::Text | Kind::Binary);
    let null = raw == "\\N" || (raw.is_empty() && (empty_null || !text_like));
    if null {
        return if col.nullable || col.auto_increment { Ok(Param::Null) } else { Err("Empty, but the column does not allow NULL. Skip it or fill it in the file.".into()) };
    }
    if raw.contains('\0') {
        return Err("PostgreSQL text cannot hold a NUL character.".into());
    }
    let word = raw.trim().to_ascii_lowercase();
    let parsed = match col.kind {
        // Text is taken as it is: parse_edit reads the word NULL as a NULL, which would turn a name into one.
        Kind::Text | Kind::Enum | Kind::Set => Bson::String(raw.into()),
        Kind::Json => {
            serde_json::from_str::<serde_json::Value>(raw).map_err(|e| format!("Not valid JSON: {e}"))?;
            Bson::String(raw.into())
        }
        // bytea reads hex written as \x.. (what the server prints) or 0x.. (what Export writes). Any other text is its own bytes,
        // never the escape format, where a backslash would mean something else.
        Kind::Binary => match raw.strip_prefix("\\x").or_else(|| raw.strip_prefix("0x")).or_else(|| raw.strip_prefix("0X")) {
            Some(_) => value::parse_edit(Dialect::Postgres, Kind::Binary, &format!("0x{}", raw[2..].trim())).map_err(|_| "Hex needs pairs of digits after \\x or 0x.".to_string())?,
            None => Bson::Binary(mongodb::bson::Binary { subtype: mongodb::bson::spec::BinarySubtype::Generic, bytes: raw.as_bytes().to_vec() }),
        },
        // The words the server writes and its cast reads, with the sign it also takes. numeric reads Infinity since PostgreSQL 14,
        // and an older server says so itself when it refuses it.
        Kind::Float | Kind::Decimal if matches!(word.as_str(), "nan" | "infinity" | "+infinity" | "-infinity" | "inf" | "+inf" | "-inf") => Bson::String(word),
        // The whole text goes to the cast: an exponent is a number to numeric, which parse_edit's plain decimal check refuses.
        Kind::Decimal if decimal_ok(raw.trim()) => Bson::String(raw.trim().into()),
        Kind::Decimal => return Err("Enter a number, like -12.50 or 1.5e3.".into()),
        // The spellings and the prefixes of them that the server's bool input takes.
        Kind::Bool => Bson::Boolean(pg_bool(&word).ok_or_else(|| "Enter true or false (also yes, no, on, off, t, f, y, n, 1, 0).".to_string())?),
        Kind::Geometry => Bson::String(raw.trim().into()),
        Kind::Date | Kind::DateTime => value::parse_edit(Dialect::Postgres, col.kind, &to_iso(raw, fmt))?,
        k => value::parse_edit(Dialect::Postgres, k, raw)?,
    };
    if parsed == Bson::Null && !col.nullable && !col.auto_increment {
        return Err("NULL, but the column does not allow NULL. Skip it or fill it in the file.".into());
    }
    if let (Some(max), Bson::String(s)) = (col.max_chars, &parsed) {
        // The server drops spaces past the length of a char(n) or varchar(n) without an error, so only other characters count.
        let n = s.trim_end_matches(' ').chars().count();
        if n > max {
            return Err(format!("{n} characters, the column holds {max}."));
        }
    }
    Ok(Param::for_pg(&parsed))
}

/// What numeric accepts: digits with an optional point, sign and exponent, as `-12.50`, `.5`, `5.` and `1.5e-3`.
fn decimal_ok(t: &str) -> bool {
    let body = t.strip_prefix(['-', '+']).unwrap_or(t);
    let (mant, exp) = match body.split_once(['e', 'E']) {
        Some((m, e)) => (m, Some(e.strip_prefix(['-', '+']).unwrap_or(e))),
        None => (body, None),
    };
    let (int, frac) = mant.split_once('.').unwrap_or((mant, ""));
    let digits = |x: &str| x.bytes().all(|b| b.is_ascii_digit());
    !(int.is_empty() && frac.is_empty()) && digits(int) && digits(frac) && exp.is_none_or(|e| !e.is_empty() && digits(e))
}

/// The server's boolean input: true, yes, on, 1, false, no, off, 0, and any unambiguous start of those words.
fn pg_bool(word: &str) -> Option<bool> {
    let is = |full: &str| !word.is_empty() && full.starts_with(word);
    match word {
        "1" => Some(true),
        "0" => Some(false),
        // "o" could be on or off, which the server refuses too.
        "of" | "off" => Some(false),
        "on" => Some(true),
        _ if is("true") || is("yes") => Some(true),
        _ if is("false") || is("no") => Some(false),
        _ => None,
    }
}

/// How the columns of a file go into the table.
#[derive(Clone, Debug, PartialEq)]
pub struct Mapping {
    /// For each CSV column the table column it fills.
    pub map: Vec<Option<usize>>,
    /// For each CSV column how its dates are read.
    pub fmts: Vec<DateFmt>,
    pub empty_null: bool,
}

impl Mapping {
    /// (CSV column, table column) of every mapped column, in file order.
    pub fn targets(&self) -> Vec<(usize, usize)> {
        self.map.iter().enumerate().filter_map(|(i, m)| m.map(|t| (i, t))).collect()
    }

    /// The table columns that two CSV columns both fill, which no statement can do.
    pub fn duplicate_target(&self) -> Option<usize> {
        let t = self.targets();
        t.iter().find(|a| t.iter().filter(|b| b.1 == a.1).count() > 1).map(|a| a.1)
    }

    fn one(&self, cols: &[TCol], rec: &[String], ci: usize, ti: usize) -> Result<Param, String> {
        convert(&cols[ti], rec.get(ci).map_or("", String::as_str), self.empty_null, self.fmts.get(ci).copied().unwrap_or(DateFmt::DayFirst))
    }

    /// The values of one record in the order of `targets`. The error names the CSV column.
    pub fn convert_record(&self, cols: &[TCol], rec: &[String]) -> Result<Vec<Param>, (usize, String)> {
        self.targets().into_iter().map(|(ci, ti)| self.one(cols, rec, ci, ti).map_err(|e| (ci, e))).collect()
    }

    /// Every value of the record that cannot be imported, by CSV column.
    pub fn errors(&self, cols: &[TCol], rec: &[String]) -> Vec<(usize, String)> {
        self.targets().into_iter().filter_map(|(ci, ti)| self.one(cols, rec, ci, ti).err().map(|e| (ci, e))).collect()
    }
}

/// One value that cannot be imported. `row` counts records as a spreadsheet does, the header being row 1.
#[derive(Clone, Debug, PartialEq)]
pub struct Problem {
    pub row: u64,
    pub col: Option<usize>,
    pub value: String,
    pub msg: String,
}

#[derive(Clone, Debug, Default)]
pub struct ColReport {
    pub bad: u64,
    pub first: Vec<Problem>,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    /// Rows to import: records after the header.
    pub rows: u64,
    pub cols: Vec<ColReport>,
    /// The first problems of the whole file in row order.
    pub first: Vec<Problem>,
    pub bad_rows: u64,
    /// The file could not be read to its end.
    pub error: Option<String>,
    pub cancelled: bool,
}

const FIRST_PER_COL: usize = 3;
const FIRST_ALL: usize = 8;

/// A value cut for the list of problems.
pub fn short(s: &str) -> String {
    let t: String = s.chars().take(40).collect();
    if s.chars().count() > 40 { format!("{t}...") } else { t }
}

/// Reads the whole file and checks every value of every mapped column. Nothing is sent anywhere.
pub fn analyze(src: &Source, opts: Opts, header: bool, cols: &[TCol], m: &Mapping, width: usize, cancel: &AtomicBool) -> Report {
    let mut rep = Report { cols: vec![ColReport::default(); width], ..Default::default() };
    let r = match src.open() {
        Ok(r) => r,
        Err(e) => {
            rep.error = Some(format!("Could not read the file: {e}"));
            return rep;
        }
    };
    let mut rd = Reader::new(r, opts);
    let note = |rep: &mut Report, p: Problem| {
        if let Some(c) = p.col {
            let cr = &mut rep.cols[c];
            cr.bad += 1;
            if cr.first.len() < FIRST_PER_COL {
                cr.first.push(p.clone());
            }
        }
        if rep.first.len() < FIRST_ALL {
            rep.first.push(p);
        }
    };
    loop {
        if cancel.load(Ordering::Relaxed) {
            rep.cancelled = true;
            return rep;
        }
        let rec = match rd.next_record() {
            Ok(Some(r)) => r,
            Ok(None) => break,
            Err(e) => {
                rep.error = Some(e);
                break;
            }
        };
        if header && rd.records == 1 {
            continue;
        }
        rep.rows += 1;
        let row = rd.records;
        if rec.len() != width {
            rep.bad_rows += 1;
            note(&mut rep, Problem { row, col: None, value: String::new(), msg: format!("Has {} values, the first row has {width}.", rec.len()) });
            continue;
        }
        let bad = m.errors(cols, &rec);
        if !bad.is_empty() {
            // Every bad value of the row counts for its column, but a row is one problem in the total.
            rep.bad_rows += 1;
            for (ci, msg) in bad {
                note(&mut rep, Problem { row, col: Some(ci), value: short(&rec[ci]), msg });
            }
        }
    }
    rep
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> Opts {
        Opts { delim: ',', quote: Some('"'), enc: Enc::Utf8 }
    }

    fn read_all(bytes: &[u8], o: Opts) -> Result<Vec<Vec<String>>, String> {
        let mut r = Reader::new(std::io::Cursor::new(bytes.to_vec()), o);
        let mut out = Vec::new();
        while let Some(rec) = r.next_record()? {
            out.push(rec);
        }
        Ok(out)
    }

    fn rows(s: &str) -> Vec<Vec<String>> {
        read_all(s.as_bytes(), opts()).unwrap()
    }

    fn col(name: &str, ty: &str, nullable: bool) -> TCol {
        TCol { name: name.into(), full_type: ty.into(), cast_type: ty.into(), kind: Kind::from_column_type(ty), nullable, auto_increment: false, generated: false, fills_itself: nullable, max_chars: None, choices: Vec::new(), pg: false }
    }

    #[test]
    fn quoted_values_keep_delimiters_quotes_and_line_breaks() {
        let got = rows("a,\"b,c\",\"say \"\"hi\"\"\"\r\n\"line\nbreak\",x,\n");
        assert_eq!(got, [vec!["a", "b,c", "say \"hi\""], vec!["line\nbreak", "x", ""]]);
        // No newline at the end, CR alone, and blank lines between records.
        assert_eq!(rows("a,b\n\n\nc,d"), [vec!["a", "b"], vec!["c", "d"]]);
        assert_eq!(rows("a\rb\r"), [vec!["a"], vec!["b"]]);
        // A line holding only an empty quoted value is a record of one empty value.
        assert_eq!(rows("\"\"\nx"), [vec![""], vec!["x"]]);
        // A quote inside an unquoted value is text, and backslashes are never escapes.
        assert_eq!(rows("5\" pipe,C:\\dir\\,\\N"), [vec!["5\" pipe", "C:\\dir\\", "\\N"]]);
    }

    #[test]
    fn mysql_null_word_in_a_not_null_number_and_escaped_enum_members() {
        assert!(convert(&col("qty", "int", false), "NULL", true, DateFmt::DayFirst).is_err());
        assert_eq!(convert(&col("qty", "int", true), "NULL", true, DateFmt::DayFirst), Ok(Param::Null));
        assert_eq!(quoted_list("'C:\\\\dir','it''s'"), ["C:\\dir", "it's"]);
    }

    #[test]
    fn a_value_that_tries_to_end_the_statement_is_only_text() {
        let got = rows("1,\"x'); DROP TABLE t; --\",\"a\\'\"\n");
        assert_eq!(got[0][1], "x'); DROP TABLE t; --");
        assert_eq!(got[0][2], "a\\'");
        let t = col("name", "varchar(100)", true);
        for v in ["x'); DROP TABLE t; --", "a\\'", "'", "\\", "`; DELETE FROM t; #", "\u{0}"] {
            assert_eq!(convert(&t, v, true, DateFmt::DayFirst), Ok(Param::Text(v.into())), "{v:?}");
        }
    }

    #[test]
    fn an_open_quote_is_an_error_not_a_swallowed_file() {
        let e = read_all(b"a,b\n\"never closed,c\nd,e\n", opts()).unwrap_err();
        assert!(e.contains("quoted value") && e.contains("record 2"), "{e}");
        // With quoting off the same text is plain.
        let plain = Opts { quote: None, ..opts() };
        assert_eq!(read_all(b"a,\"b\n", plain).unwrap(), [vec!["a", "\"b"]]);
    }

    #[test]
    fn other_delimiters_and_encodings_are_read() {
        let semi = Opts { delim: ';', ..opts() };
        assert_eq!(read_all(b"a;b,c;\"d;e\"\n", semi).unwrap(), [vec!["a", "b,c", "d;e"]]);
        let mut utf16: Vec<u8> = vec![0xFF, 0xFE];
        utf16.extend("id,nom\n1,\u{e9}t\u{e9} \u{1F600}\n".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(detect_enc(&utf16), Enc::Utf16Le);
        assert_eq!(read_all(&utf16, Opts { enc: Enc::Utf16Le, ..opts() }).unwrap(), [vec!["id", "nom"], vec!["1", "\u{e9}t\u{e9} \u{1F600}"]]);
        let latin = b"id,nom\n1,caf\xe9 \x80\n";
        assert_eq!(detect_enc(latin), Enc::Windows1252);
        assert_eq!(read_all(latin, Opts { enc: Enc::Windows1252, ..opts() }).unwrap()[1], ["1", "caf\u{e9} \u{20ac}"]);
        // A byte order mark is not part of the first name.
        assert_eq!(read_all(b"\xEF\xBB\xBFid,x\n", opts()).unwrap()[0][0], "id");
        // Wrong encoding says so instead of guessing.
        let e = read_all(latin, opts()).unwrap_err();
        assert!(e.contains("not valid UTF-8") && e.contains("Windows-1252"), "{e}");
        // A character cut by the end of a sample is still UTF-8.
        assert_eq!(detect_enc(&"caf\u{e9}".as_bytes()[..4]), Enc::Utf8);
    }

    #[test]
    fn the_format_is_found_from_the_first_rows() {
        let src = |s: &str| Source::Mem(Arc::new(s.as_bytes().to_vec()));
        for (text, d) in [("a,b,c\n1,2,3\n4,5,6\n", ','), ("a;b;c\n1,5;2;3\n4;5;6\n", ';'), ("a\tb\tc\n1\t2\t3\n", '\t'), ("a|b|c\n1|2|3\n4|5|6\n", '|'), ("only\none\ncolumn\n", ',')] {
            assert_eq!(sniff(&src(text)).unwrap().0.delim, d, "{text:?}");
        }
        // Delimiters inside quotes do not count.
        assert_eq!(sniff(&src("a,b\n\"x;y;z\",\"p;q\"\n\"1;2;3\",\"4;5\"\n")).unwrap().0.delim, ',');
        assert!(sniff(&src("id,name\n1,ann\n")).unwrap().1);
        assert!(!sniff(&src("1,2\n3,4\n")).unwrap().1);
        assert!(!sniff(&src("a,a\n1,2\n")).unwrap().1);
        assert!(sniff(&src("")).unwrap().1);
    }

    #[test]
    fn postgres_text_is_read_as_the_server_reads_it() {
        let c = |ty: &str, kind: Kind| TCol { cast_type: ty.into(), kind, max_chars: None, pg: true, ..col("c", ty, true) };
        let p = |col: &TCol, raw: &str| convert(col, raw, true, DateFmt::DayFirst);
        let t = |s: &str| Ok(Param::Text(s.into()));
        let b = c("boolean", Kind::Bool);
        for (raw, want) in [("on", "true"), ("Y", "true"), ("tr", "true"), ("1", "true"), ("of", "false"), ("off", "false"), ("n", "false"), ("f", "false"), (" NO ", "false")] {
            assert_eq!(p(&b, raw), t(want), "{raw}");
        }
        assert!(p(&b, "o").is_err() && p(&b, "maybe").is_err() && p(&b, "2").is_err());
        let n = c("numeric", Kind::Decimal);
        for raw in ["1e3", "1.5E-2", "+5", ".5", "5.", "-1.5e+2", "Infinity", "-infinity", "+inf", "NaN"] {
            assert!(p(&n, raw).is_ok(), "{raw}");
        }
        for raw in ["e3", "1e", "1.2.3", "1,5", "--1", "."] {
            assert!(p(&n, raw).is_err(), "{raw}");
        }
        let f = c("double precision", Kind::Float);
        for raw in ["+Infinity", "-inf", "1e3", "NaN"] {
            assert!(p(&f, raw).is_ok(), "{raw}");
        }
        // Spaces past the length are dropped by the server, other characters are an error.
        let mut v = c("character varying(3)", Kind::Text);
        v.max_chars = Some(3);
        assert!(p(&v, "abc  ").is_ok() && p(&v, "abcd").is_err());
    }

    #[test]
    fn columns_are_matched_by_name_once() {
        let cols = [col("id", "int", false), col("Order Date", "date", true), col("total", "decimal(10,2)", true)];
        let names: Vec<String> = ["ID", "order_date", "id", "Total", "extra"].map(String::from).to_vec();
        // The file's own `id` is the exact match, so the first column `ID` is the one left without a table column.
        assert_eq!(auto_map(&names, &cols), [None, Some(1), Some(0), Some(2), None]);
        // MySQL names are not case sensitive, so a lone `ID` still finds `id`.
        assert_eq!(auto_map(&["ID".to_string(), "Order_Date".to_string()], &cols), [Some(0), Some(1)]);
        // Columns that differ only in case each get their own, in either order of the header.
        let two = [col("id", "int", false), col("Name", "text", true), col("name", "text", true)];
        assert_eq!(auto_map(&["name", "Name"].map(String::from), &two), [Some(2), Some(1)]);
        assert_eq!(auto_map(&["Name", "name"].map(String::from), &two), [Some(1), Some(2)]);
        // Neither spelling is exact: the first takes the first free column.
        assert_eq!(auto_map(&["NAME".to_string(), "nAME".to_string()], &two), [Some(1), Some(2)]);
        let mut generated_col = col("g", "int", true);
        generated_col.generated = true;
        assert_eq!(auto_map(&["g".to_string()], &[generated_col]), [None]);
        assert_eq!(auto_map(&["".to_string()], &[col("", "int", true)]), [None]);
    }

    #[test]
    fn values_are_checked_against_the_column() {
        let c = |ty: &str| col("c", ty, true);
        let ok = |ty: &str, v: &str| convert(&c(ty), v, true, DateFmt::DayFirst);
        assert_eq!(ok("int", "42"), Ok(Param::Int(42)));
        assert!(ok("int", "4.2").is_err() && ok("int", "abc").is_err() && ok("int", "9 9").is_err());
        assert_eq!(ok("int", ""), Ok(Param::Null));
        assert_eq!(ok("tinyint(1)", "TRUE"), Ok(Param::Int(1)));
        assert_eq!(ok("bigint unsigned", "18446744073709551615"), Ok(Param::UInt(u64::MAX)));
        assert!(ok("bigint unsigned", "-1").is_err());
        assert_eq!(ok("decimal(10,2)", "-12.50"), Ok(Param::Text("-12.50".into())));
        assert!(ok("decimal(10,2)", "1,5").is_err() && ok("decimal(10,2)", "1e5").is_err());
        assert_eq!(ok("double", "1.5"), Ok(Param::Float(1.5)));
        assert!(ok("double", "inf").is_err());
        assert_eq!(ok("date", "2026-11-09"), Ok(Param::Text("2026-11-09".into())));
        assert!(ok("date", "2026-13-09").is_err() && ok("date", "tomorrow").is_err());
        assert_eq!(ok("datetime", "2026-11-09 12:41:05"), Ok(Param::Text("2026-11-09 12:41:05".into())));
        assert_eq!(ok("json", "null"), Ok(Param::Text("null".into())));
        assert!(ok("json", "{oops").is_err());
        assert_eq!(ok("blob", "0xCAFE"), Ok(Param::Bytes(vec![0xCA, 0xFE])));
        assert!(ok("blob", "0xCAF").is_err());
        assert_eq!(ok("blob", "plain"), Ok(Param::Text("plain".into())));
    }

    #[test]
    fn nulls_follow_the_choice_and_the_column() {
        let text = col("t", "varchar(10)", true);
        assert_eq!(convert(&text, "", true, DateFmt::DayFirst), Ok(Param::Null));
        assert_eq!(convert(&text, "", false, DateFmt::DayFirst), Ok(Param::Text(String::new())));
        assert_eq!(convert(&text, "\\N", false, DateFmt::DayFirst), Ok(Param::Null));
        // The word is only a NULL where the column is not text.
        assert_eq!(convert(&text, "NULL", true, DateFmt::DayFirst), Ok(Param::Text("NULL".into())));
        assert_eq!(convert(&col("n", "int", true), "null", true, DateFmt::DayFirst), Ok(Param::Null));
        // A number cannot be empty text, whatever the choice.
        assert_eq!(convert(&col("n", "int", true), "", false, DateFmt::DayFirst), Ok(Param::Null));
        let strict = col("n", "int", false);
        assert!(convert(&strict, "", true, DateFmt::DayFirst).is_err());
        let mut auto = strict.clone();
        auto.auto_increment = true;
        assert_eq!(convert(&auto, "", true, DateFmt::DayFirst), Ok(Param::Null));
        assert_eq!(convert(&col("t", "varchar(10)", false), "", false, DateFmt::DayFirst), Ok(Param::Text(String::new())));
    }

    #[test]
    fn lengths_and_enum_values_are_checked() {
        let mut v = col("v", "varchar(3)", true);
        v.max_chars = Some(3);
        assert!(convert(&v, "abc", true, DateFmt::DayFirst).is_ok());
        assert!(convert(&v, "caf\u{e9}s", true, DateFmt::DayFirst).unwrap_err().contains("5 characters"));
        // Characters, not bytes.
        assert!(convert(&v, "\u{e9}\u{e9}\u{e9}", true, DateFmt::DayFirst).is_ok());
        let e = TCol::from_column(&structure::Column { name: "s".into(), full_type: "enum('new','it''s','a,b')".into(), nullable: false, ..Default::default() }, false);
        assert_eq!(e.choices, ["new", "it's", "a,b"]);
        assert!(convert(&e, "IT'S", true, DateFmt::DayFirst).is_ok());
        assert!(convert(&e, "old", true, DateFmt::DayFirst).unwrap_err().contains("Not one of"));
        let s = TCol::from_column(&structure::Column { name: "s".into(), full_type: "set('a','b','c')".into(), nullable: true, ..Default::default() }, false);
        assert!(convert(&s, "a,c", true, DateFmt::DayFirst).is_ok());
        assert!(convert(&s, "a,z", true, DateFmt::DayFirst).unwrap_err().starts_with("z is not"));
        let ch = TCol::from_column(&structure::Column { name: "c".into(), full_type: "char(2)".into(), nullable: true, ..Default::default() }, false);
        assert_eq!(ch.max_chars, Some(2));
    }

    #[test]
    fn local_dates_are_read_the_way_the_column_says() {
        let c = col("d", "datetime", true);
        assert_eq!(convert(&c, "09/11/2026 12:41", true, DateFmt::DayFirst), Ok(Param::Text("2026-11-09 12:41:00".into())));
        assert_eq!(convert(&c, "09/11/2026 12:41", true, DateFmt::MonthFirst), Ok(Param::Text("2026-09-11 12:41:00".into())));
        assert_eq!(convert(&col("d", "date", true), "9.1.2026", true, DateFmt::DayFirst), Ok(Param::Text("2026-01-09".into())));
        assert!(convert(&col("d", "date", true), "13/13/2026", true, DateFmt::DayFirst).is_err());
        assert_eq!(guess_date_fmt(&["09/11/2026", "25/11/2026"]), DateFmt::DayFirst);
        assert_eq!(guess_date_fmt(&["09/11/2026", "11/25/2026"]), DateFmt::MonthFirst);
        assert_eq!(guess_date_fmt(&["09/11/2026"]), DateFmt::DayFirst);
        assert!(has_local_dates(&["2026-01-01", "09/11/2026"]) && !has_local_dates(&["2026-01-01", "", "x"]));
    }

    #[test]
    fn the_whole_file_is_checked_and_problems_name_their_row_and_column() {
        let mut state = col("state", "varchar(4)", true);
        state.max_chars = Some(4);
        let cols = [col("id", "int", false), col("total", "decimal(10,2)", true), state];
        let mut csv = String::from("id,total,state,extra\n");
        for i in 1..=2000 {
            csv.push_str(&format!("{i},{}.50,ok,x\n", i));
        }
        csv.push_str("2001,twelve,toolong,x\n2002,1.5,ok\n2003,2.5,ok,x\n");
        let m = Mapping { map: vec![Some(0), Some(1), Some(2), None], fmts: vec![DateFmt::DayFirst; 4], empty_null: true };
        let rep = analyze(&Source::Mem(Arc::new(csv.into_bytes())), opts(), true, &cols, &m, 4, &AtomicBool::new(false));
        assert_eq!(rep.rows, 2003);
        assert_eq!(rep.bad_rows, 2);
        // Row 2002 of the file is the 2003rd record counting the header.
        assert_eq!(rep.cols[1].bad, 1);
        assert_eq!(rep.cols[1].first[0], Problem { row: 2002, col: Some(1), value: "twelve".into(), msg: "Enter a plain decimal number, like -12.50.".into() });
        assert_eq!(rep.cols[2].bad, 1);
        assert_eq!(rep.first.len(), 3);
        assert_eq!(rep.first[2].col, None);
        assert!(rep.first[2].msg.contains("Has 3 values"));
        assert_eq!(rep.cols[0].bad, 0);
        assert!(rep.error.is_none());
        // A file that stops inside a quote reports it with the rows read so far.
        let rep = analyze(&Source::Mem(Arc::new(b"id,total\n1,2\n\"3,4\n".to_vec())), opts(), true, &cols, &Mapping { map: vec![Some(0), Some(1)], fmts: vec![DateFmt::DayFirst; 2], empty_null: true }, 2, &AtomicBool::new(false));
        assert_eq!((rep.rows, rep.error.is_some()), (1, true));
        let stopped = AtomicBool::new(true);
        assert!(analyze(&Source::Mem(Arc::new(b"a\n1\n".to_vec())), opts(), true, &cols, &m, 1, &stopped).cancelled);
    }

    #[test]
    fn two_file_columns_cannot_fill_one_table_column() {
        let m = Mapping { map: vec![Some(1), Some(1), None], fmts: vec![], empty_null: true };
        assert_eq!(m.duplicate_target(), Some(1));
        assert_eq!(Mapping { map: vec![Some(0), None, Some(1)], ..m }.duplicate_target(), None);
    }

    #[test]
    fn a_gzip_script_reads_back_as_its_text() {
        use std::io::Write;
        let text = "CREATE TABLE t (a int);\nINSERT INTO t VALUES (1);\n".repeat(50);
        let gz = |t: &str| {
            let mut w = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            w.write_all(t.as_bytes()).unwrap();
            w.finish().unwrap()
        };
        let read = |src: &Source| {
            let mut out = String::new();
            src.open_script().unwrap().read_to_string(&mut out).unwrap();
            out
        };
        let zipped = Source::Mem(Arc::new(gz(&text)));
        assert_eq!(read(&zipped), text);
        assert_eq!(zipped.script_len(), text.len() as u64);
        // Plain text passes through, and so does a file whose name has no .gz but whose bytes are gzip.
        let plain = Source::Mem(Arc::new(text.clone().into_bytes()));
        assert_eq!((read(&plain), plain.script_len()), (text.clone(), text.len() as u64));
        // Concatenated members, which `cat a.sql.gz b.sql.gz` makes, read as one script.
        let mut two = gz("SELECT 1;\n");
        two.extend(gz("SELECT 2;\n"));
        assert_eq!(read(&Source::Mem(Arc::new(two))), "SELECT 1;\nSELECT 2;\n");
        // A file on disk takes the same path.
        let path = std::env::temp_dir().join(format!("quarry-gz-test-{}.sql.gz", std::process::id()));
        std::fs::write(&path, gz(&text)).unwrap();
        let file = Source::File(path.clone());
        assert_eq!((read(&file), file.script_len()), (text.clone(), text.len() as u64));
        let _ = std::fs::remove_file(&path);
        // A damaged file is an error and not an empty script.
        let mut bad = gz(&text);
        bad.truncate(bad.len() / 2);
        let mut out = String::new();
        assert!(Source::Mem(Arc::new(bad)).open_script().unwrap().read_to_string(&mut out).is_err());
    }
}
