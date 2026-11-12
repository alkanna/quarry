//! Checks run on every keystroke of the grid editor, so a value the column cannot hold is named before it is staged.
//! `parse_edit` stays the final gate; these add what it cannot know from the kind alone: the declared size of the
//! column, the days of each month, and words such as `yesterday 18:00`.

use super::Dialect;
use super::value::{self, Kind};
use crate::db::dates::{DAY_MS, MONTHS, hms, month_len, ymd};
use mongodb::bson::Bson;

/// The outcome of checking typed text against a column.
pub struct Checked {
    pub value: Result<Bson, String>,
    /// What the text turned into when it was not saved as typed, such as `Saves 2026-11-11 18:00:00`.
    pub note: Option<String>,
    /// A value that would pass, offered as a one-click fix.
    pub fix: Option<String>,
}

/// `now_ms` is the clock in the zone the column is read in, None when Quarry cannot know it (a TIMESTAMP column under
/// the server's own zone). Then words such as `now` are not turned into a time here.
pub fn check(d: Dialect, col: &super::Col, text: &str, now_ms: Option<i64>) -> Checked {
    let t = text.trim();
    let word = t.split(' ').next().unwrap_or("").to_ascii_lowercase();
    let is_word = matches!(word.as_str(), "now" | "today" | "yesterday" | "tomorrow");
    // PostgreSQL reads these words itself, in the session zone.
    if now_ms.is_none() && is_word && d != Dialect::Postgres && matches!(col.kind, Kind::Date | Kind::DateTime | Kind::Time) {
        return Checked { value: Err("The session uses the server's time zone, so type the date and time".into()), note: None, fix: None };
    }
    let resolved = resolve(col.kind, t, now_ms);
    let src = resolved.as_deref().unwrap_or(text);
    if let Some((msg, fix)) = problem(d, col, src) {
        return Checked { value: Err(msg), note: None, fix };
    }
    let value = value::parse_col(d, col, src);
    let note = resolved.filter(|_| value.is_ok()).map(|r| format!("Saves {r}"));
    Checked { value, note, fix: None }
}

/// Whether text in the column is read in the session's time zone: MySQL TIMESTAMP and PostgreSQL timestamptz.
pub fn zoned(d: Dialect, col: &super::Col) -> bool {
    let ty = col.ty.to_ascii_lowercase();
    col.kind == Kind::DateTime && if d == Dialect::Postgres { ty.contains("with time zone") || ty == "timestamptz" } else { ty.starts_with("timestamp") }
}

/// The text to stage for date and time words and short forms, words only when the clock `now` is known. None keeps
/// the text.
fn resolve(kind: Kind, t: &str, now: Option<i64>) -> Option<String> {
    let day = now.map(|n| n.div_euclid(DAY_MS));
    let secs = now.map(|n| n.rem_euclid(DAY_MS) / 1000);
    let lower = t.to_ascii_lowercase();
    let (word, rest) = lower.split_once(' ').map_or((lower.as_str(), ""), |(a, b)| (a, b.trim()));
    let offset = match word {
        "today" => Some(0),
        "yesterday" => Some(-1),
        "tomorrow" => Some(1),
        _ => None,
    };
    match kind {
        Kind::Date => match (word, offset) {
            ("now", _) if rest.is_empty() => Some(ymd(day?)),
            (_, Some(o)) if rest.is_empty() => Some(ymd(day? + o)),
            _ => None,
        },
        Kind::DateTime => {
            if word == "now" && rest.is_empty() {
                return Some(format!("{} {}", ymd(day?), hms(secs?)));
            }
            if let Some(o) = offset {
                let time = if rest.is_empty() { "00:00:00".to_string() } else { pad_time(rest)? };
                return Some(format!("{} {time}", ymd(day? + o)));
            }
            // A date alone, or a time without seconds, reads as the start of that day or minute.
            let (date, time) = t.split_once([' ', 'T']).map_or((t, ""), |(a, b)| (a, b));
            if !looks_like_date(date) {
                return None;
            }
            if time.is_empty() {
                return Some(format!("{date} 00:00:00"));
            }
            pad_time(time).filter(|p| p != time).map(|p| format!("{date} {p}"))
        }
        Kind::Time if word == "now" && rest.is_empty() => Some(hms(secs?)),
        Kind::Time => pad_time(t).filter(|p| p != t),
        _ => None,
    }
}

/// `HH:MM` as `HH:MM:00`; anything else as it is. None when it is not a time at all.
fn pad_time(t: &str) -> Option<String> {
    let parts: Vec<&str> = t.split(':').collect();
    let num = |s: &str| !s.is_empty() && s.len() <= 3 && s.bytes().all(|b| b.is_ascii_digit());
    match parts[..] {
        [h, m] if num(h) && num(m) => Some(format!("{h:0>2}:{m}:00")),
        [h, _, _] if num(h) => Some(t.to_string()),
        _ => None,
    }
}

fn looks_like_date(s: &str) -> bool {
    let p: Vec<&str> = s.split('-').collect();
    p.len() == 3 && p.iter().all(|x| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit()))
}


/// What is wrong with text in the column, in words, and a fix when there is an obvious one.
fn problem(d: Dialect, col: &super::Col, text: &str) -> Option<(String, Option<String>)> {
    let t = text.trim();
    if t.eq_ignore_ascii_case("null") {
        return None;
    }
    let ty = col.ty.to_ascii_lowercase();
    let only = |m: String| Some((m, None));
    match col.kind {
        Kind::Int | Kind::UInt => {
            let (lo, hi) = int_range(&ty)?;
            let n: i128 = t.parse().ok()?;
            (n < lo || n > hi).then(|| (format!("Holds {lo} to {hi}"), None))
        }
        Kind::Decimal => decimal_problem(&ty, t),
        Kind::Text => {
            let n = max_chars(&ty)?;
            let s = value::quoted_null(t).unwrap_or(text);
            let len = s.chars().count();
            (len > n).then(|| (format!("{len} characters, the column holds {n}"), None))
        }
        Kind::Date if t != "0000-00-00" => date_problem(t).and_then(only),
        Kind::DateTime if t != "0000-00-00 00:00:00" => {
            let (date, time) = t.split_once([' ', 'T'])?;
            date_problem(date).or_else(|| time_problem(time, 23)).and_then(only)
        }
        Kind::Time => time_problem(t.strip_prefix('-').unwrap_or(t), if d == Dialect::Postgres { 24 } else { 838 }).and_then(only),
        Kind::Json if t != "'null'" => serde_json::from_str::<serde_json::Value>(t).err().map(|e| value::json_message(&e)).and_then(only),
        Kind::Enum => {
            let opts = super::schema::enum_values(&col.ty);
            let s = value::quoted_null(t).unwrap_or(t);
            (!opts.is_empty() && !opts.iter().any(|o| o.eq_ignore_ascii_case(s))).then(|| (format!("Not one of the {} values of the column", opts.len()), None))
        }
        Kind::Uuid => {
            let body = t.strip_prefix('{').and_then(|r| r.strip_suffix('}')).unwrap_or(t);
            if body.chars().any(|c| c != '-' && !c.is_ascii_hexdigit()) {
                return only("Only hex digits and hyphens".into());
            }
            let n = body.chars().filter(char::is_ascii_hexdigit).count();
            (n != 32).then(|| (format!("Needs 32 hex digits, has {n}"), None))
        }
        _ => None,
    }
}

/// The smallest and largest value of an integer column, from its type text.
fn int_range(ty: &str) -> Option<(i128, i128)> {
    let base: String = ty.chars().take_while(char::is_ascii_alphanumeric).collect();
    let bits = match base.as_str() {
        "tinyint" => 8,
        "smallint" | "int2" => 16,
        "mediumint" => 24,
        "int" | "integer" | "int4" => 32,
        "bigint" | "int8" => 64,
        _ => return None,
    };
    Some(if ty.contains("unsigned") { (0, (1i128 << bits) - 1) } else { (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1) })
}

/// The length of a `varchar(n)`, `char(n)` or `character varying(n)` column.
fn max_chars(ty: &str) -> Option<usize> {
    let (base, rest) = ty.split_once('(')?;
    matches!(base.trim(), "varchar" | "char" | "character varying" | "character" | "nvarchar" | "nchar").then_some(())?;
    rest.split(')').next()?.trim().parse().ok()
}

fn decimal_problem(ty: &str, t: &str) -> Option<(String, Option<String>)> {
    let (base, rest) = ty.split_once('(')?;
    let mut ps = rest.trim_end_matches(')').split(',').map(|x| x.trim().parse::<usize>());
    let p = ps.next()?.ok()?;
    let s = ps.next().unwrap_or(Ok(0)).ok()?;
    let name = format!("{}({p},{s})", base.trim().to_ascii_uppercase());
    let (sign, body) = t.strip_prefix('-').map_or(("", t.strip_prefix('+').unwrap_or(t)), |b| ("-", b));
    let (int, frac) = body.split_once('.').unwrap_or((body, ""));
    if !int.bytes().chain(frac.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    let room = p.saturating_sub(s);
    let int_digits = int.trim_start_matches('0').len();
    if int_digits > room {
        return Some((format!("{name} holds {room} digit{} before the point", if room == 1 { "" } else { "s" }), None));
    }
    if frac.len() > s {
        let fixed = format!("{sign}{}", round_decimal(int, frac, s));
        // Rounding up can add a digit in front, so the fix is offered only when it fits.
        let fits = fixed.trim_start_matches('-').split('.').next().is_some_and(|i| i.trim_start_matches('0').len() <= room);
        let msg = if s == 0 { format!("{name} keeps no decimals") } else { format!("{name} keeps {s} decimal{}", if s == 1 { "" } else { "s" }) };
        return Some((msg, fits.then_some(fixed)));
    }
    None
}

/// `int.frac` rounded half away from zero to `s` decimals.
fn round_decimal(int: &str, frac: &str, s: usize) -> String {
    let int = if int.is_empty() { "0" } else { int };
    let mut digits: Vec<u8> = int.bytes().chain(frac.bytes().take(s)).map(|b| b - b'0').collect();
    if frac.as_bytes().get(s).is_some_and(|b| *b >= b'5') {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, 1);
                break;
            }
            i -= 1;
            if digits[i] == 9 {
                digits[i] = 0;
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    let text: String = digits.iter().map(|d| (d + b'0') as char).collect();
    let (a, b) = text.split_at(text.len() - s);
    if s == 0 { a.to_string() } else { format!("{a}.{b}") }
}

fn date_problem(s: &str) -> Option<String> {
    let p: Vec<&str> = s.split('-').collect();
    let [y, m, d] = p[..] else { return None };
    let (Ok(y), Ok(m), Ok(d)) = (y.parse::<i64>(), m.parse::<i64>(), d.parse::<i64>()) else { return None };
    if !(1..=12).contains(&m) {
        return Some("Months go from 01 to 12".into());
    }
    if d < 1 {
        return Some("Days start at 01".into());
    }
    let len = month_len(y, m);
    (d > len).then(|| format!("{} {y} has {len} days", MONTHS[m as usize - 1]))
}

fn time_problem(s: &str, max_hour: u32) -> Option<String> {
    let hms = s.split('.').next().unwrap_or(s);
    let p: Vec<Option<u32>> = hms.split(':').map(|x| x.parse().ok()).collect();
    match p[..] {
        [Some(h), ..] if h > max_hour => Some(format!("Hours go up to {max_hour}")),
        [_, Some(m), ..] if m > 59 => Some("Minutes go up to 59".into()),
        [_, _, Some(sec)] if sec > 59 => Some("Seconds go up to 59".into()),
        _ => None,
    }
}


/// A MySQL boolean column, which the server keeps as TINYINT(1).
pub fn is_bool(d: Dialect, col: &super::Col) -> bool {
    col.kind == Kind::Bool || (d != Dialect::Postgres && col.ty.eq_ignore_ascii_case("tinyint(1)"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sql::Col;

    // 2026-11-12 10:15:30 local.
    const NOW: Option<i64> = Some(1_794_478_530_000);

    fn col(ty: &str, kind: Kind, nullable: bool) -> Col {
        Col::plain("c", ty, kind, nullable)
    }

    fn err(ty: &str, kind: Kind, t: &str) -> String {
        check(Dialect::MySql, &col(ty, kind, true), t, NOW).value.unwrap_err()
    }

    #[test]
    fn sizes_and_ranges_are_named() {
        assert_eq!(err("tinyint unsigned", Kind::UInt, "300"), "Holds 0 to 255");
        assert_eq!(err("smallint", Kind::Int, "-40000"), "Holds -32768 to 32767");
        assert!(check(Dialect::MySql, &col("tinyint unsigned", Kind::UInt, true), "255", NOW).value.is_ok());
        assert_eq!(err("varchar(3)", Kind::Text, "EURO"), "4 characters, the column holds 3");
        assert_eq!(check(Dialect::Postgres, &col("character varying(3)", Kind::Text, true), "EURO", NOW).value.unwrap_err(), "4 characters, the column holds 3");
        assert!(check(Dialect::MySql, &col("varchar(3)", Kind::Text, true), "EUR", NOW).value.is_ok());
    }

    #[test]
    fn decimals_offer_the_rounded_value() {
        let c = check(Dialect::MySql, &col("decimal(10,2)", Kind::Decimal, true), "1204.105", NOW);
        assert_eq!(c.value.unwrap_err(), "DECIMAL(10,2) keeps 2 decimals");
        assert_eq!(c.fix.as_deref(), Some("1204.11"));
        assert_eq!(check(Dialect::MySql, &col("decimal(3,2)", Kind::Decimal, true), "-9.996", NOW).fix.as_deref(), None);
        assert_eq!(check(Dialect::MySql, &col("decimal(5,2)", Kind::Decimal, true), "-9.996", NOW).fix.as_deref(), Some("-10.00"));
        assert_eq!(err("decimal(5,2)", Kind::Decimal, "1234"), "DECIMAL(5,2) holds 3 digits before the point");
        assert_eq!(round_decimal("", "5", 0), "1");
    }

    #[test]
    fn dates_and_times_are_checked_by_part() {
        assert_eq!(err("date", Kind::Date, "2026-02-30"), "February 2026 has 28 days");
        assert_eq!(err("date", Kind::Date, "2024-13-01"), "Months go from 01 to 12");
        assert!(check(Dialect::MySql, &col("date", Kind::Date, true), "2024-02-29", NOW).value.is_ok());
        assert_eq!(err("time", Kind::Time, "12:75"), "Minutes go up to 59");
        assert_eq!(err("datetime", Kind::DateTime, "2026-01-01 24:00:00"), "Hours go up to 23");
        assert_eq!(err("char(36)", Kind::Uuid, "123e4567-e89b-12d3"), "Needs 32 hex digits, has 16");
        assert_eq!(err("json", Kind::Json, r#"{"a": [1, 2]"#), "Unclosed object at column 12");
    }

    #[test]
    fn words_and_short_forms_say_what_they_save() {
        let c = check(Dialect::MySql, &col("datetime", Kind::DateTime, true), "yesterday 18:00", NOW);
        assert_eq!(c.value.unwrap(), Bson::String("2026-11-11 18:00:00".into()));
        assert_eq!(c.note.as_deref(), Some("Saves 2026-11-11 18:00:00"));
        assert_eq!(check(Dialect::MySql, &col("date", Kind::Date, true), "Tomorrow", NOW).value.unwrap(), Bson::String("2026-11-13".into()));
        assert_eq!(check(Dialect::MySql, &col("time", Kind::Time, true), "now", NOW).value.unwrap(), Bson::String("10:15:30".into()));
        assert_eq!(check(Dialect::MySql, &col("time", Kind::Time, true), "9:05", NOW).value.unwrap(), Bson::String("09:05:00".into()));
        assert_eq!(check(Dialect::MySql, &col("datetime", Kind::DateTime, true), "2026-01-02", NOW).value.unwrap(), Bson::String("2026-01-02 00:00:00".into()));
        assert!(check(Dialect::MySql, &col("datetime", Kind::DateTime, true), "2026-01-02 10:00:00", NOW).note.is_none());
        // Under the server's zone the clock is unknown: MySQL refuses the word, PostgreSQL reads it itself.
        assert!(check(Dialect::MySql, &col("timestamp", Kind::DateTime, true), "now", None).value.is_err());
        assert_eq!(check(Dialect::Postgres, &col("timestamp with time zone", Kind::DateTime, true), "now", None).value.unwrap(), Bson::String("now".into()));
        assert_eq!(check(Dialect::MySql, &col("timestamp", Kind::DateTime, true), "2026-01-02 9:30", None).value.unwrap(), Bson::String("2026-01-02 09:30:00".into()));
    }

    #[test]
    fn not_null_and_choices() {
        let c = col("int", Kind::Int, false);
        assert!(check(Dialect::MySql, &c, "NULL", NOW).value.unwrap_err().starts_with("c cannot be NULL"));
        assert_eq!(err("enum('paid','shipped')", Kind::Enum, "lost"), "Not one of the 2 values of the column");
        assert!(check(Dialect::MySql, &col("enum('paid','shipped')", Kind::Enum, true), "Shipped", NOW).value.is_ok());
        assert!(is_bool(Dialect::MySql, &col("tinyint(1)", Kind::Int, true)));
        assert!(!is_bool(Dialect::MySql, &col("tinyint(4)", Kind::Int, true)));
    }
}
