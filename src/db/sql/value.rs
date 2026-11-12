//! Column kinds and value conversion: server text to BSON, editor text to BSON, BSON to a bind
//! value. Exact types (DECIMAL, dates, JSON) stay as the server's own text so nothing is rounded.

use super::Dialect;
use mongodb::bson::{spec::BinarySubtype, Binary, Bson};

/// A SQL string literal. Backslashes are escaped too, since MySQL reads them as escapes by default; `bs`
/// is false under NO_BACKSLASH_ESCAPES, where doubling them would change the value. NUL and Ctrl-Z are
/// written as \0 and \Z like mysqldump does, since a raw NUL ends the text for C tools and a raw Ctrl-Z ends
/// a file read in text mode on Windows. Under NO_BACKSLASH_ESCAPES there is no escape, so they stay raw.
pub fn lit(s: &str, bs: bool) -> String {
    let s = if bs { s.replace('\\', "\\\\").replace('\0', "\\0").replace('\u{1a}', "\\Z") } else { s.to_string() };
    format!("'{}'", s.replace('\'', "''"))
}

/// A PostgreSQL string literal that reads the same whatever standard_conforming_strings is: with a backslash in it the text goes
/// in `E'...'`, where backslashes are escapes under both settings, and without one a plain string is already the same.
pub fn pg_lit(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 3);
    pg_lit_into(&mut out, s);
    out
}

/// `pg_lit` appended to `out`, for a writer that builds a long text and should not allocate per value.
pub fn pg_lit_into(out: &mut String, s: &str) {
    out.push_str(if s.contains('\\') { "E'" } else { "'" });
    let mut last = 0;
    // Both specials are ASCII, so the pieces between them are copied whole. A backslash can only be found in the E form.
    for (i, c) in s.match_indices(['\'', '\\']) {
        out.push_str(&s[last..i]);
        out.push_str(if c == "'" { "''" } else { "\\\\" });
        last = i + 1;
    }
    out.push_str(&s[last..]);
    out.push('\'');
}

/// Bytes as hex digits, the part after `0x` or inside `X'..'`.
pub fn hex_digits(b: &[u8], upper: bool) -> String {
    use std::fmt::Write;
    b.iter().fold(String::with_capacity(b.len() * 2), |mut s, x| {
        let _ = if upper { write!(s, "{x:02X}") } else { write!(s, "{x:02x}") };
        s
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Int,
    UInt,
    Float,
    Decimal,
    Text,
    Binary,
    Date,
    DateTime,
    Time,
    Year,
    Json,
    Bit,
    Enum,
    Set,
    Geometry,
    /// PostgreSQL `bool`, sent as `t` or `f`.
    Bool,
    Uuid,
    /// Arrays of any element type, shown as the server's own `{1,2,3}` text.
    Array,
    Interval,
    /// inet, cidr, macaddr and macaddr8.
    Inet,
    /// Ranges and multiranges, shown as the server's own text.
    Range,
}

impl Kind {
    /// Maps a type name as sqlx reports it (`INT UNSIGNED`, `VARCHAR`, ...). Unknown names are Text,
    /// which shows the server's text as is.
    pub fn from_type_name(name: &str) -> Kind {
        let up = name.trim().to_ascii_uppercase();
        let (base, unsigned) = match up.strip_suffix(" UNSIGNED") {
            Some(b) => (b, true),
            None => (up.as_str(), false),
        };
        match base {
            "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "INTEGER" | "BIGINT" | "BOOLEAN" | "BOOL" => if unsigned { Kind::UInt } else { Kind::Int },
            "FLOAT" | "DOUBLE" | "REAL" => Kind::Float,
            "DECIMAL" | "NUMERIC" | "NEWDECIMAL" => Kind::Decimal,
            "BINARY" | "VARBINARY" | "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB" => Kind::Binary,
            "DATE" => Kind::Date,
            "DATETIME" | "TIMESTAMP" => Kind::DateTime,
            "TIME" => Kind::Time,
            "YEAR" => Kind::Year,
            "JSON" => Kind::Json,
            "BIT" => Kind::Bit,
            "ENUM" => Kind::Enum,
            "SET" => Kind::Set,
            "GEOMETRY" | "POINT" | "LINESTRING" | "POLYGON" | "MULTIPOINT" | "MULTILINESTRING" | "MULTIPOLYGON" | "GEOMETRYCOLLECTION" => Kind::Geometry,
            _ => Kind::Text,
        }
    }
}

impl Kind {
    /// A kind from a PostgreSQL type. `name` is `pg_type.typname` of the base type (a domain is resolved to what it
    /// wraps), or the name sqlx reports for a result column. `category` and `typtype` come from `pg_type` and are empty
    /// for a result column, which has only the name. A user enum is `typtype` e and arrays are category A.
    pub fn from_pg(name: &str, category: &str, typtype: &str) -> Kind {
        let n = name.trim().trim_matches('"').to_ascii_lowercase();
        if n.ends_with("[]") || category == "A" || (category.is_empty() && n.starts_with('_')) {
            return Kind::Array;
        }
        match n.as_str() {
            "int2" | "int4" | "int8" | "oid" => return Kind::Int,
            "float4" | "float8" => return Kind::Float,
            "numeric" => return Kind::Decimal,
            "bool" => return Kind::Bool,
            "text" | "varchar" | "bpchar" | "name" | "citext" | "char" => return Kind::Text,
            "bytea" => return Kind::Binary,
            "date" => return Kind::Date,
            "timestamp" | "timestamptz" => return Kind::DateTime,
            "time" | "timetz" => return Kind::Time,
            "json" | "jsonb" => return Kind::Json,
            "uuid" => return Kind::Uuid,
            "interval" => return Kind::Interval,
            "inet" | "cidr" | "macaddr" | "macaddr8" => return Kind::Inet,
            _ => {}
        }
        match (typtype, category) {
            ("e", _) => Kind::Enum,
            ("r" | "m", _) | (_, "R") => Kind::Range,
            ("", "") if n.ends_with("range") => Kind::Range,
            _ => Kind::Text,
        }
    }

    /// A kind from information_schema's COLUMN_TYPE ("varchar(20)", "bigint unsigned"), which stays right
    /// for `_bin` text columns that sqlx reports as binary.
    pub fn from_column_type(full: &str) -> Kind {
        let l = full.to_ascii_lowercase();
        let base: String = l.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
        Kind::from_type_name(&if l.contains(" unsigned") { format!("{base} UNSIGNED") } else { base })
    }
}

fn bytes(b: &[u8]) -> Bson {
    Bson::Binary(Binary { subtype: BinarySubtype::Generic, bytes: b.to_vec() })
}

/// Converts the server's text-protocol bytes. Anything that does not parse as its kind falls back to the server's text,
/// so a value is never lost or silently changed. A Postgres row holds a bytea as `\x` hex text, so it is decoded here, at
/// the edge, and the rest of the app sees the same bytes as on MySQL.
pub fn to_bson(d: Dialect, kind: Kind, raw: Option<&[u8]>) -> Bson {
    let pg = d == Dialect::Postgres;
    if pg && kind == Kind::Binary && let Some(b) = raw.and_then(unhex) {
        return bytes(&b);
    }
    let Some(raw) = raw else { return Bson::Null };
    let text = || String::from_utf8_lossy(raw).into_owned();
    // PostgreSQL sends a bit string as `0101` and a geometry as its own text, which only the cast it is written back
    // through can read. Bytes of that text would bind as `\x3031..` and fail the cast.
    if pg && matches!(kind, Kind::Bit | Kind::Geometry) {
        return Bson::String(text());
    }
    let s = std::str::from_utf8(raw).unwrap_or("");
    match kind {
        Kind::Int | Kind::Year => s.parse::<i64>().map_or_else(|_| Bson::String(text()), Bson::Int64),
        // Bson has no unsigned 64 bit integer, so values above i64::MAX stay exact as text.
        Kind::UInt => s.parse::<u64>().ok().and_then(|v| i64::try_from(v).ok()).map_or_else(|| Bson::String(text()), Bson::Int64),
        Kind::Float => s.parse::<f64>().map_or_else(|_| Bson::String(text()), Bson::Double),
        Kind::Decimal | Kind::Date | Kind::DateTime | Kind::Time | Kind::Json | Kind::Enum | Kind::Set | Kind::Uuid | Kind::Array | Kind::Interval | Kind::Inet | Kind::Range => Bson::String(text()),
        Kind::Bool => match s {
            "t" => Bson::Boolean(true),
            "f" => Bson::Boolean(false),
            _ => Bson::String(text()),
        },
        Kind::Text => match std::str::from_utf8(raw) {
            Ok(s) => Bson::String(s.into()),
            Err(_) => bytes(raw),
        },
        Kind::Binary | Kind::Bit | Kind::Geometry => bytes(raw),
    }
}

/// The bytes of PostgreSQL's hex bytea text: a backslash, an x and pairs of digits.
fn unhex(raw: &[u8]) -> Option<Vec<u8>> {
    let h = raw.strip_prefix(b"\\x")?;
    if h.len() % 2 != 0 || !h.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let d = |b: u8| (b as char).to_digit(16).unwrap_or(0) as u8;
    Some(h.chunks(2).map(|p| d(p[0]) << 4 | d(p[1])).collect())
}

fn digits(s: &str, min: usize, max: usize) -> Option<u32> {
    (s.len() >= min && s.len() <= max && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten()
}

fn date_ok(s: &str) -> bool {
    let p: Vec<&str> = s.split('-').collect();
    // Month and day 00 only exist in the whole zero date, which the callers check first.
    matches!(p[..], [y, m, d] if digits(y, 4, 4).is_some() && digits(m, 2, 2).is_some_and(|m| (1..=12).contains(&m)) && digits(d, 2, 2).is_some_and(|d| (1..=31).contains(&d)))
}

/// [-]H:MM:SS[.ffffff], with the hour limits of the column type.
fn time_ok(s: &str, hour_digits: usize, max_hour: u32) -> bool {
    let (hms, frac) = s.split_once('.').map_or((s, None), |(a, f)| (a, Some(f)));
    if frac.is_some_and(|f| digits(f, 1, 6).is_none()) {
        return false;
    }
    let p: Vec<&str> = hms.split(':').collect();
    matches!(p[..], [h, m, sec] if digits(h, 2, hour_digits).is_some_and(|h| h <= max_hour) && digits(m, 2, 2).is_some_and(|m| m < 60) && digits(sec, 2, 2).is_some_and(|s| s < 60))
}

/// Checks text typed into the grid editor and turns it into the value to stage. The word NULL, in any case, is a NULL,
/// and in quotes it is the text NULL for text columns.
/// Errors are short enough to show under the cell. PostgreSQL's dates and times have more spellings than MySQL's
/// (`2024-01-01 00:00:00+02`, `infinity`, `12:00:00+01`, five digit years), and the cast the write is made through
/// reads all of them and names what it refuses, so they go through as typed.
pub fn parse_edit(d: Dialect, kind: Kind, text: &str) -> Result<Bson, String> {
    let t = text.trim();
    if t.eq_ignore_ascii_case("null") {
        return Ok(Bson::Null);
    }
    if d == Dialect::Postgres && matches!(kind, Kind::Date | Kind::DateTime | Kind::Time) {
        return if t.is_empty() { Err("Enter a date or time, like 2024-01-31 12:30:00.".into()) } else { Ok(Bson::String(t.into())) };
    }
    match kind {
        Kind::Int | Kind::Year => t.parse::<i64>().map(Bson::Int64).map_err(|_| "Enter a whole number.".to_string()),
        Kind::UInt => {
            if t.starts_with('-') {
                return Err("Enter a whole number of zero or more.".into());
            }
            let v = t.parse::<u64>().map_err(|_| "Enter a whole number.".to_string())?;
            Ok(i64::try_from(v).map_or_else(|_| Bson::String(t.into()), Bson::Int64))
        }
        Kind::Float => t.parse::<f64>().ok().filter(|v| v.is_finite()).map(Bson::Double).ok_or_else(|| "Enter a number.".to_string()),
        Kind::Decimal => {
            let body = t.strip_prefix(['-', '+']).unwrap_or(t);
            let (int, frac) = body.split_once('.').unwrap_or((body, ""));
            let ok = !(int.is_empty() && frac.is_empty()) && int.bytes().chain(frac.bytes()).all(|b| b.is_ascii_digit());
            if ok { Ok(Bson::String(t.into())) } else { Err("Enter a plain decimal number, like -12.50.".into()) }
        }
        Kind::Date => if t == "0000-00-00" || date_ok(t) { Ok(Bson::String(t.into())) } else { Err("Use the form YYYY-MM-DD.".into()) },
        Kind::DateTime => {
            let ok = t == "0000-00-00 00:00:00" || t.split_once([' ', 'T']).is_some_and(|(d, tm)| date_ok(d) && time_ok(tm, 2, 23));
            if ok { Ok(Bson::String(t.into())) } else { Err("Use the form YYYY-MM-DD HH:MM:SS.".into()) }
        }
        Kind::Time => if time_ok(t.strip_prefix('-').unwrap_or(t), 3, 838) { Ok(Bson::String(t.into())) } else { Err("Use the form HH:MM:SS.".into()) },
        // Bare null is SQL NULL, and "null" is a JSON string, so the JSON literal null is typed in single quotes.
        Kind::Json if t == "'null'" => Ok(Bson::String("null".into())),
        Kind::Json => serde_json::from_str::<serde_json::Value>(t).map(|_| Bson::String(t.into())).map_err(|e| format!("Not valid JSON: {e}")),
        Kind::Binary => match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
            Some(h) => {
                if h.len() % 2 != 0 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err("Hex needs pairs of digits after 0x.".into());
                }
                let v = (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(0)).collect::<Vec<u8>>();
                Ok(bytes(&v))
            }
            None => Ok(Bson::String(text.into())),
        },
        Kind::Bit => {
            let (digits, radix) = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                (h, 16)
            } else if let Some(q) = t.strip_prefix("b'").or_else(|| t.strip_prefix("B'")).and_then(|q| q.strip_suffix('\'')) {
                (q, 2)
            } else {
                (t, 10)
            };
            if digits.is_empty() || !digits.bytes().all(|b| (b as char).is_digit(radix)) {
                return Err("Enter a number, b'0101' or 0x1F.".into());
            }
            // Bson has no unsigned 64 bit integer, so BIT(64) values above i64::MAX cannot be staged.
            match u64::from_str_radix(digits, radix).ok().and_then(|v| i64::try_from(v).ok()) {
                Some(v) => Ok(Bson::Int64(v)),
                None => Err("Too large to edit here, BIT values above 63 bits are not supported. Use SQL and a hex value like 0xFFFF.".into()),
            }
        }
        Kind::Geometry => Err("Geometry values cannot be edited as text yet.".into()),
        Kind::Bool => match t.to_ascii_lowercase().as_str() {
            "true" | "t" | "1" | "yes" => Ok(Bson::Boolean(true)),
            "false" | "f" | "0" | "no" => Ok(Bson::Boolean(false)),
            _ => Err("Enter true or false.".into()),
        },
        Kind::Uuid => uuid_text(t).map(Bson::String).ok_or_else(|| "Enter a UUID like 123e4567-e89b-12d3-a456-426614174000.".to_string()),
        // The server's cast is the only parser that knows every spelling of these, and it names what it refused.
        k if text_like(k) => Ok(Bson::String(quoted_null(t).unwrap_or(text).into())),
        k => Err(format!("{k:?} values cannot be edited as text.")),
    }
}

/// `parse_edit` for a column, refusing NULL when the column is NOT NULL so the error shows before Save.
pub fn parse_col(d: Dialect, col: &super::Col, text: &str) -> Result<Bson, String> {
    match parse_edit(d, col.kind, text)? {
        // MySQL reads NULL in an AUTO_INCREMENT column as "the next id".
        Bson::Null if !col.nullable && !col.auto_increment => Err(format!("{} cannot be NULL. Put 'NULL' in quotes for the text NULL.", col.src)),
        v => Ok(v),
    }
}

/// The text NULL in single or double quotes, so the word itself can still be stored in a text column.
/// One layer comes off, so text that is itself a quoted null ('NULL' with its quotes) is typed with one more pair.
pub fn quoted_null(t: &str) -> Option<&str> {
    let inner = t.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')).or_else(|| t.strip_prefix('"').and_then(|r| r.strip_suffix('"')))?;
    null_like(inner).then_some(inner)
}

/// Text an editor must show in quotes, or reading it back would give null or drop a pair of quotes.
pub fn null_like(s: &str) -> bool {
    let s = s.trim();
    s.eq_ignore_ascii_case("null") || quoted_null(s).is_some()
}

/// Kinds whose text is stored as typed, the ones that read a quoted null as text.
pub fn text_like(k: Kind) -> bool {
    matches!(k, Kind::Text | Kind::Enum | Kind::Set | Kind::Array | Kind::Interval | Kind::Inet | Kind::Range)
}

/// A UUID in the canonical lowercase form, from the spellings PostgreSQL reads: optional braces, and hyphens after any
/// group of four digits. Written back canonical so a cell edited to the value it already has stays unchanged.
fn uuid_text(t: &str) -> Option<String> {
    let body = t.strip_prefix('{').and_then(|r| r.strip_suffix('}')).unwrap_or(t);
    let hex: String = body.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) || body.starts_with('-') || body.ends_with('-') || body.contains("--") {
        return None;
    }
    let h = hex.to_ascii_lowercase();
    Some(format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..]))
}

/// A value to bind to a `?` placeholder. The worker maps it to its driver, so this module stays free
/// of sqlx.
#[derive(Clone, PartialEq, Debug)]
pub enum Param {
    Null,
    Int(i64),
    /// An unsigned value above i64::MAX, which BSON cannot hold.
    UInt(u64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
}

impl Param {
    /// Like `from_bson`, but a UInt column's text above i64::MAX binds as a number, not as text.
    pub fn for_kind(kind: Kind, v: &Bson) -> Param {
        if let (Kind::UInt, Bson::String(s)) = (kind, v) && let Some(u) = s.parse::<u64>().ok().filter(|u| i64::try_from(*u).is_err()) {
            return Param::UInt(u);
        }
        Param::from_bson(v)
    }

    /// A value for PostgreSQL, where every bound value travels as text and the `CAST($n AS type)` around its placeholder
    /// converts it: the server refuses a text value for an int column, but never a cast from text. A boolean is `true`
    /// or `false`, bytes are the `\x` hex that bytea reads, and NULL stays NULL.
    pub fn for_pg(v: &Bson) -> Param {
        match v {
            Bson::Null | Bson::Undefined => Param::Null,
            Bson::Boolean(b) => Param::Text(b.to_string()),
            Bson::Int32(i) => Param::Text(i.to_string()),
            Bson::Int64(i) => Param::Text(i.to_string()),
            Bson::Double(f) => Param::Text(f.to_string()),
            Bson::String(s) => Param::Text(s.clone()),
            Bson::Binary(b) => Param::Text(format!("\\x{}", hex_digits(&b.bytes, false))),
            other => Param::Text(other.to_string()),
        }
    }

    pub fn from_bson(v: &Bson) -> Param {
        match v {
            Bson::Null | Bson::Undefined => Param::Null,
            Bson::Int32(i) => Param::Int(*i as i64),
            Bson::Int64(i) => Param::Int(*i),
            Bson::Boolean(b) => Param::Int(*b as i64),
            Bson::Double(f) => Param::Float(*f),
            Bson::String(s) => Param::Text(s.clone()),
            Bson::Binary(b) => Param::Bytes(b.bytes.clone()),
            other => Param::Text(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_postgres_literal_reads_the_same_under_both_string_settings() {
        for (text, want) in [("plain", "'plain'"), ("it's", "'it''s'"), (r"a\b", r"E'a\\b'"), (r"it's \", r"E'it''s \\'"), ("", "''"), ("é'\n", "'é''\n'")] {
            assert_eq!(pg_lit(text), want);
            let mut out = String::from("x=");
            pg_lit_into(&mut out, text);
            assert_eq!(out, format!("x={want}"));
        }
    }

    use super::*;

    #[test]
    fn literals_escape_nul_and_ctrl_z_like_mysqldump() {
        assert_eq!(lit("a\0b\u{1a}c\\d'e", true), "'a\\0b\\Zc\\\\d''e'");
        // Without backslash escapes there is no escape for them, and a backslash is a plain character.
        assert_eq!(lit("a\0b\u{1a}c\\d'e", false), "'a\0b\u{1a}c\\d''e'");
    }

    #[test]
    fn postgres_bit_and_geometry_text_binds_back_as_the_same_text() {
        for kind in [Kind::Bit, Kind::Geometry] {
            let v = to_bson(Dialect::Postgres, kind, Some(b"0101"));
            assert_eq!(v, Bson::String("0101".into()));
            assert_eq!(Param::for_pg(&v), Param::Text("0101".into()));
        }
        // MySQL keeps bytes.
        assert!(matches!(to_bson(Dialect::MySql, Kind::Bit, Some(&[5])), Bson::Binary(_)));
    }

    #[test]
    fn type_names_map_to_kinds() {
        let k = Kind::from_type_name;
        for (n, want) in [
            ("INT", Kind::Int), ("TINYINT", Kind::Int), ("BOOLEAN", Kind::Int), ("BIGINT", Kind::Int), ("INT UNSIGNED", Kind::UInt), ("BIGINT UNSIGNED", Kind::UInt), ("tinyint unsigned", Kind::UInt),
            ("DOUBLE", Kind::Float), ("FLOAT", Kind::Float), ("DECIMAL", Kind::Decimal),
            ("VARCHAR", Kind::Text), ("CHAR", Kind::Text), ("TEXT", Kind::Text), ("LONGTEXT", Kind::Text), ("NULL", Kind::Text), ("SOMETHING NEW", Kind::Text),
            ("VARBINARY", Kind::Binary), ("BINARY", Kind::Binary), ("BLOB", Kind::Binary), ("LONGBLOB", Kind::Binary),
            ("DATE", Kind::Date), ("DATETIME", Kind::DateTime), ("TIMESTAMP", Kind::DateTime), ("TIME", Kind::Time), ("YEAR", Kind::Year),
            ("JSON", Kind::Json), ("BIT", Kind::Bit), ("ENUM", Kind::Enum), ("SET", Kind::Set), ("GEOMETRY", Kind::Geometry),
        ] {
            assert_eq!(k(n), want, "{n}");
        }
    }

    #[test]
    fn postgres_types_map_to_kinds() {
        let k = Kind::from_pg;
        for (n, cat, tt, want) in [
            ("int2", "N", "b", Kind::Int), ("int4", "N", "b", Kind::Int), ("int8", "N", "b", Kind::Int), ("oid", "N", "b", Kind::Int),
            ("float4", "N", "b", Kind::Float), ("float8", "N", "b", Kind::Float), ("numeric", "N", "b", Kind::Decimal), ("bool", "B", "b", Kind::Bool),
            ("text", "S", "b", Kind::Text), ("varchar", "S", "b", Kind::Text), ("bpchar", "S", "b", Kind::Text), ("name", "S", "b", Kind::Text), ("citext", "S", "b", Kind::Text),
            ("bytea", "U", "b", Kind::Binary), ("date", "D", "b", Kind::Date), ("timestamp", "D", "b", Kind::DateTime), ("timestamptz", "D", "b", Kind::DateTime),
            ("time", "D", "b", Kind::Time), ("timetz", "D", "b", Kind::Time), ("json", "U", "b", Kind::Json), ("jsonb", "U", "b", Kind::Json),
            ("uuid", "U", "b", Kind::Uuid), ("_int4", "A", "b", Kind::Array), ("interval", "T", "b", Kind::Interval),
            ("inet", "I", "b", Kind::Inet), ("cidr", "I", "b", Kind::Inet), ("macaddr", "U", "b", Kind::Inet),
            ("int4range", "R", "r", Kind::Range), ("tstzmultirange", "R", "m", Kind::Range), ("mood", "E", "e", Kind::Enum),
            ("money", "N", "b", Kind::Text), ("point", "G", "b", Kind::Text), ("tsvector", "U", "b", Kind::Text), ("mycomposite", "C", "c", Kind::Text),
        ] {
            assert_eq!(k(n, cat, tt), want, "{n}");
        }
        // The names sqlx gives a result column have no category.
        for (n, want) in [("INT4", Kind::Int), ("TIMESTAMPTZ", Kind::DateTime), ("INT4[]", Kind::Array), ("INT4RANGE", Kind::Range), ("BOOL", Kind::Bool), ("\"CHAR\"", Kind::Text), ("?", Kind::Text)] {
            assert_eq!(k(n, "", ""), want, "{n}");
        }
    }

    #[test]
    fn postgres_text_becomes_bson() {
        assert_eq!(conv(Kind::Bool, "t"), Bson::Boolean(true));
        assert_eq!(conv(Kind::Bool, "f"), Bson::Boolean(false));
        assert_eq!(conv(Kind::Bool, "?"), Bson::String("?".into()));
        assert_eq!(to_bson(Dialect::Postgres, Kind::Binary, Some(b"\\x00ff10")), bytes(&[0, 255, 16]));
        assert_eq!(to_bson(Dialect::Postgres, Kind::Binary, Some(b"\\x")), bytes(&[]));
        // MySQL bytes that happen to start with a backslash are not hex.
        assert_eq!(to_bson(Dialect::MySql, Kind::Binary, Some(b"\\x00ff")), bytes(b"\\x00ff"));
        assert_eq!(to_bson(Dialect::Postgres, Kind::Binary, Some(b"\\xzz")), bytes(b"\\xzz"));
        for k in [Kind::Uuid, Kind::Array, Kind::Interval, Kind::Inet, Kind::Range] {
            assert_eq!(conv(k, "{1,2}"), Bson::String("{1,2}".into()));
        }
        for k in [Kind::Array, Kind::Interval, Kind::Inet, Kind::Range] {
            assert_eq!(parse_edit(Dialect::MySql, k, "{1,2}"), Ok(Bson::String("{1,2}".into())));
        }
    }

    #[test]
    fn postgres_edits_are_checked() {
        for t in ["true", "T", "1", "Yes", " TRUE "] {
            assert_eq!(parse_edit(Dialect::MySql, Kind::Bool, t), Ok(Bson::Boolean(true)), "{t}");
        }
        for f in ["false", "F", "0", "no", "No"] {
            assert_eq!(parse_edit(Dialect::MySql, Kind::Bool, f), Ok(Bson::Boolean(false)), "{f}");
        }
        assert!(parse_edit(Dialect::MySql, Kind::Bool, "maybe").is_err() && parse_edit(Dialect::MySql, Kind::Bool, "").is_err() && parse_edit(Dialect::MySql, Kind::Bool, "2").is_err());
        assert_eq!(parse_edit(Dialect::MySql, Kind::Bool, "null"), Ok(Bson::Null));
        let canon = Bson::String("123e4567-e89b-12d3-a456-426614174000".into());
        for u in ["123e4567-e89b-12d3-a456-426614174000", "123E4567-E89B-12D3-A456-426614174000", "{123e4567-e89b-12d3-a456-426614174000}", "123e4567e89b12d3a456426614174000", "123e-4567-e89b-12d3-a456-4266-14174000"] {
            assert_eq!(parse_edit(Dialect::MySql, Kind::Uuid, u), Ok(canon.clone()), "{u}");
        }
        for bad in ["", "abc", "123e4567-e89b-12d3-a456-42661417400", "123e4567-e89b-12d3-a456-42661417400g", "-123e4567e89b12d3a456426614174000", "123e4567--e89b12d3a456426614174000"] {
            assert!(parse_edit(Dialect::MySql, Kind::Uuid, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn postgres_dates_and_times_go_to_the_server_as_typed() {
        for (k, t) in [(Kind::DateTime, "2024-01-01 00:00:00+02"), (Kind::DateTime, "infinity"), (Kind::Date, "-infinity"), (Kind::Time, "12:00:00+01"), (Kind::Date, "12345-01-01")] {
            assert_eq!(parse_edit(Dialect::Postgres, k, &format!(" {t} ")), Ok(Bson::String(t.into())), "{t}");
            assert!(parse_edit(Dialect::MySql, k, t).is_err(), "{t}");
        }
        assert_eq!(parse_edit(Dialect::Postgres, Kind::Date, "NULL"), Ok(Bson::Null));
        assert!(parse_edit(Dialect::Postgres, Kind::Date, "  ").is_err());
        assert_eq!(parse_edit(Dialect::Postgres, Kind::Int, "4"), Ok(Bson::Int64(4)));
        assert_eq!(parse_edit(Dialect::MySql, Kind::Date, "2024-02-29"), parse_edit(Dialect::MySql, Kind::Date, "2024-02-29"));
    }

    #[test]
    fn postgres_params_are_text() {
        assert_eq!(Param::for_pg(&Bson::Boolean(true)), Param::Text("true".into()));
        assert_eq!(Param::for_pg(&Bson::Boolean(false)), Param::Text("false".into()));
        assert_eq!(Param::for_pg(&Bson::Int64(-4)), Param::Text("-4".into()));
        assert_eq!(Param::for_pg(&Bson::Double(0.5)), Param::Text("0.5".into()));
        assert_eq!(Param::for_pg(&bytes(&[0, 255, 16])), Param::Text("\\x00ff10".into()));
        assert_eq!(Param::for_pg(&bytes(&[])), Param::Text("\\x".into()));
        assert_eq!(Param::for_pg(&Bson::Null), Param::Null);
        // MySQL keeps its integer for a boolean.
        assert_eq!(Param::from_bson(&Bson::Boolean(true)), Param::Int(1));
    }

    fn conv(kind: Kind, s: &str) -> Bson {
        to_bson(Dialect::MySql, kind, Some(s.as_bytes()))
    }

    #[test]
    fn server_text_becomes_bson() {
        assert_eq!(to_bson(Dialect::MySql, Kind::Int, None), Bson::Null);
        assert_eq!(conv(Kind::Int, "-42"), Bson::Int64(-42));
        assert_eq!(conv(Kind::Year, "2024"), Bson::Int64(2024));
        assert_eq!(conv(Kind::UInt, "9223372036854775807"), Bson::Int64(i64::MAX));
        assert_eq!(conv(Kind::UInt, "18446744073709551615"), Bson::String("18446744073709551615".into()));
        assert_eq!(conv(Kind::Float, "1.5"), Bson::Double(1.5));
        assert_eq!(conv(Kind::Decimal, "12.3400"), Bson::String("12.3400".into()));
        assert_eq!(conv(Kind::Date, "2024-02-29"), Bson::String("2024-02-29".into()));
        assert_eq!(conv(Kind::DateTime, "2024-02-29 01:02:03.500000"), Bson::String("2024-02-29 01:02:03.500000".into()));
        assert_eq!(conv(Kind::Time, "-838:59:59"), Bson::String("-838:59:59".into()));
        assert_eq!(conv(Kind::Json, r#"{"a": 1}"#), Bson::String(r#"{"a": 1}"#.into()));
        assert_eq!(conv(Kind::Enum, "red"), Bson::String("red".into()));
        assert_eq!(conv(Kind::Set, "a,b"), Bson::String("a,b".into()));
        assert_eq!(conv(Kind::Text, "héllo"), Bson::String("héllo".into()));
        assert_eq!(to_bson(Dialect::MySql, Kind::Text, Some(&[0xff, 0xfe])), bytes(&[0xff, 0xfe]));
        assert_eq!(to_bson(Dialect::MySql, Kind::Binary, Some(&[1, 2])), bytes(&[1, 2]));
        assert_eq!(to_bson(Dialect::MySql, Kind::Bit, Some(&[1])), bytes(&[1]));
        assert_eq!(to_bson(Dialect::MySql, Kind::Geometry, Some(&[0, 0, 0, 0])), bytes(&[0, 0, 0, 0]));
        // A value that does not parse keeps the server's text.
        assert_eq!(conv(Kind::Int, "oops"), Bson::String("oops".into()));
        assert_eq!(conv(Kind::Float, "nope"), Bson::String("nope".into()));
    }

    #[test]
    fn edits_are_checked_per_kind() {
        let p = |k, t| parse_edit(Dialect::MySql, k, t);
        assert_eq!(p(Kind::Text, " NuLl "), Ok(Bson::Null));
        assert_eq!(p(Kind::Int, "NULL"), Ok(Bson::Null));
        assert_eq!(p(Kind::Int, " 7 "), Ok(Bson::Int64(7)));
        assert_eq!(p(Kind::Int, "-7"), Ok(Bson::Int64(-7)));
        assert!(p(Kind::Int, "1.5").is_err());
        assert!(p(Kind::Int, "").is_err());
        assert_eq!(p(Kind::UInt, "5"), Ok(Bson::Int64(5)));
        assert!(p(Kind::UInt, "-5").is_err());
        assert_eq!(p(Kind::UInt, "18446744073709551615"), Ok(Bson::String("18446744073709551615".into())));
        assert!(p(Kind::UInt, "18446744073709551616").is_err());
        assert_eq!(p(Kind::Float, "2.5e3"), Ok(Bson::Double(2500.0)));
        assert!(p(Kind::Float, "abc").is_err());
        assert!(p(Kind::Float, "inf").is_err());
        assert_eq!(p(Kind::Decimal, "-12.50"), Ok(Bson::String("-12.50".into())));
        assert_eq!(p(Kind::Decimal, ".5"), Ok(Bson::String(".5".into())));
        for bad in ["", "-", ".", "1e3", "1,5", "12.3.4", "a"] {
            assert!(p(Kind::Decimal, bad).is_err(), "{bad}");
        }
        assert_eq!(p(Kind::Date, "2024-02-29"), Ok(Bson::String("2024-02-29".into())));
        for bad in ["2024-2-29", "24-02-29", "2024-13-01", "2024-02-32", "2024/02/29", "today"] {
            assert!(p(Kind::Date, bad).is_err(), "{bad}");
        }
        assert_eq!(p(Kind::DateTime, "2024-02-29 23:59:59"), Ok(Bson::String("2024-02-29 23:59:59".into())));
        assert_eq!(p(Kind::DateTime, "2024-02-29 01:02:03.123456"), Ok(Bson::String("2024-02-29 01:02:03.123456".into())));
        for bad in ["2024-02-29", "2024-02-29 24:00:00", "2024-02-29 01:60:00", "2024-02-29 01:02:03.1234567", "2024-02-29 1:02:03"] {
            assert!(p(Kind::DateTime, bad).is_err(), "{bad}");
        }
        assert_eq!(p(Kind::Time, "-838:59:59"), Ok(Bson::String("-838:59:59".into())));
        assert_eq!(p(Kind::Time, "01:02:03.5"), Ok(Bson::String("01:02:03.5".into())));
        for bad in ["839:00:00", "1:2:3", "12:60:00", "12:00", "ab:cd:ef"] {
            assert!(p(Kind::Time, bad).is_err(), "{bad}");
        }
        assert_eq!(p(Kind::Json, r#"{"a":[1]}"#), Ok(Bson::String(r#"{"a":[1]}"#.into())));
        assert!(p(Kind::Json, "{a:1}").unwrap_err().starts_with("Not valid JSON"));
        assert_eq!(p(Kind::Binary, "0xDEAD"), Ok(bytes(&[0xde, 0xad])));
        assert_eq!(p(Kind::Binary, "0x"), Ok(bytes(&[])));
        assert!(p(Kind::Binary, "0xABC").is_err());
        assert!(p(Kind::Binary, "0xZZ").is_err());
        assert_eq!(p(Kind::Binary, "plain"), Ok(Bson::String("plain".into())));
        assert_eq!(p(Kind::Enum, "red"), Ok(Bson::String("red".into())));
        assert_eq!(p(Kind::Text, "  keep spaces "), Ok(Bson::String("  keep spaces ".into())));
        assert_eq!(p(Kind::Year, "1999"), Ok(Bson::Int64(1999)));
    }

    #[test]
    fn bson_becomes_a_bind_value() {
        assert_eq!(Param::from_bson(&Bson::Null), Param::Null);
        assert_eq!(Param::from_bson(&Bson::Int32(3)), Param::Int(3));
        assert_eq!(Param::from_bson(&Bson::Int64(-3)), Param::Int(-3));
        assert_eq!(Param::from_bson(&Bson::Double(0.5)), Param::Float(0.5));
        assert_eq!(Param::from_bson(&Bson::String("x".into())), Param::Text("x".into()));
        assert_eq!(Param::from_bson(&Bson::Boolean(true)), Param::Int(1));
        assert_eq!(Param::from_bson(&bytes(&[9])), Param::Bytes(vec![9]));
    }

    #[test]
    fn bit_geometry_and_zero_dates() {
        let p = |k, t| parse_edit(Dialect::MySql, k, t);
        assert_eq!(p(Kind::Bit, "1"), Ok(Bson::Int64(1)));
        assert_eq!(p(Kind::Bit, "b'0101'"), Ok(Bson::Int64(5)));
        assert_eq!(p(Kind::Bit, "0x1F"), Ok(Bson::Int64(31)));
        assert!(p(Kind::Bit, "x").is_err());
        assert!(p(Kind::Bit, "b''").is_err());
        assert!(p(Kind::Bit, "18446744073709551615").unwrap_err().contains("hex"));
        assert!(p(Kind::Geometry, "POINT(1 1)").unwrap_err().contains("Geometry"));
        assert!(p(Kind::Date, "2024-00-10").is_err());
        assert!(p(Kind::Date, "2024-10-00").is_err());
        assert_eq!(p(Kind::Date, "0000-00-00"), Ok(Bson::String("0000-00-00".into())));
        assert!(p(Kind::DateTime, "2024-00-10 01:02:03").is_err());
        assert_eq!(p(Kind::DateTime, "0000-00-00 00:00:00"), Ok(Bson::String("0000-00-00 00:00:00".into())));
    }

    #[test]
    fn unsigned_params_follow_the_kind() {
        let big = Bson::String("18446744073709551615".into());
        assert_eq!(Param::for_kind(Kind::UInt, &big), Param::UInt(u64::MAX));
        assert_eq!(Param::for_kind(Kind::UInt, &Bson::String("5".into())), Param::Text("5".into()));
        assert_eq!(Param::for_kind(Kind::Decimal, &big), Param::Text("18446744073709551615".into()));
        assert_eq!(Param::for_kind(Kind::UInt, &Bson::Int64(3)), Param::Int(3));
    }
}
