//! Column kinds and value conversion: server text to BSON, editor text to BSON, BSON to a bind
//! value. Exact types (DECIMAL, dates, JSON) stay as the server's own text so nothing is rounded.

use mongodb::bson::{spec::BinarySubtype, Binary, Bson};

/// A SQL string literal. Backslashes are escaped too, since MySQL reads them as escapes by default; `bs`
/// is false under NO_BACKSLASH_ESCAPES, where doubling them would change the value.
pub fn lit(s: &str, bs: bool) -> String {
    let s = if bs { s.replace('\\', "\\\\") } else { s.to_string() };
    format!("'{}'", s.replace('\'', "''"))
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

/// Converts the server's text-protocol bytes. Anything that does not parse as its kind falls back to
/// the server's text, so a value is never lost or silently changed.
pub fn to_bson(kind: Kind, raw: Option<&[u8]>) -> Bson {
    let Some(raw) = raw else { return Bson::Null };
    let text = || String::from_utf8_lossy(raw).into_owned();
    let s = std::str::from_utf8(raw).unwrap_or("");
    match kind {
        Kind::Int | Kind::Year => s.parse::<i64>().map_or_else(|_| Bson::String(text()), Bson::Int64),
        // Bson has no unsigned 64 bit integer, so values above i64::MAX stay exact as text.
        Kind::UInt => s.parse::<u64>().ok().and_then(|v| i64::try_from(v).ok()).map_or_else(|| Bson::String(text()), Bson::Int64),
        Kind::Float => s.parse::<f64>().map_or_else(|_| Bson::String(text()), Bson::Double),
        Kind::Decimal | Kind::Date | Kind::DateTime | Kind::Time | Kind::Json | Kind::Enum | Kind::Set => Bson::String(text()),
        Kind::Text => match std::str::from_utf8(raw) {
            Ok(s) => Bson::String(s.into()),
            Err(_) => bytes(raw),
        },
        Kind::Binary | Kind::Bit | Kind::Geometry => bytes(raw),
    }
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

/// Checks text typed into the grid editor and turns it into the value to stage. The word NULL, in
/// any case, is a NULL. Errors are short enough to show under the cell.
pub fn parse_edit(kind: Kind, text: &str) -> Result<Bson, String> {
    let t = text.trim();
    if t.eq_ignore_ascii_case("null") {
        return Ok(Bson::Null);
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
        Kind::Text | Kind::Enum | Kind::Set => Ok(Bson::String(text.into())),
    }
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
        if let (Kind::UInt, Bson::String(s)) = (kind, v) {
            if let Some(u) = s.parse::<u64>().ok().filter(|u| i64::try_from(*u).is_err()) {
                return Param::UInt(u);
            }
        }
        Param::from_bson(v)
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
    use super::*;

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

    fn conv(kind: Kind, s: &str) -> Bson {
        to_bson(kind, Some(s.as_bytes()))
    }

    #[test]
    fn server_text_becomes_bson() {
        assert_eq!(to_bson(Kind::Int, None), Bson::Null);
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
        assert_eq!(to_bson(Kind::Text, Some(&[0xff, 0xfe])), bytes(&[0xff, 0xfe]));
        assert_eq!(to_bson(Kind::Binary, Some(&[1, 2])), bytes(&[1, 2]));
        assert_eq!(to_bson(Kind::Bit, Some(&[1])), bytes(&[1]));
        assert_eq!(to_bson(Kind::Geometry, Some(&[0, 0, 0, 0])), bytes(&[0, 0, 0, 0]));
        // A value that does not parse keeps the server's text.
        assert_eq!(conv(Kind::Int, "oops"), Bson::String("oops".into()));
        assert_eq!(conv(Kind::Float, "nope"), Bson::String("nope".into()));
    }

    #[test]
    fn edits_are_checked_per_kind() {
        let p = parse_edit;
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
        let p = parse_edit;
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
