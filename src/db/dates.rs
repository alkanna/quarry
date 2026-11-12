//! Date shortcuts such as "today" or "last 3 months" for query inputs.

pub(crate) const DAY_MS: i64 = 86_400_000;

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

pub(crate) fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, doy - (153 * mp + 2) / 5 + 1)
}

/// Days in month `m` of year `y`.
pub(crate) fn month_len(y: i64, m: i64) -> i64 {
    days_from_civil(y + i64::from(m == 12), m % 12 + 1, 1) - days_from_civil(y, m, 1)
}

/// `ms` moved back by whole calendar months, the day clamped so Mar 31 minus one month is Feb 28.
pub(crate) fn months_back(ms: i64, months: i64) -> i64 {
    let (y, m, d) = civil_from_days(ms.div_euclid(DAY_MS));
    let idx = y * 12 + m - 1 - months;
    let (y, m) = (idx.div_euclid(12), idx.rem_euclid(12) + 1);
    days_from_civil(y, m, d.min(month_len(y, m))) * DAY_MS + ms.rem_euclid(DAY_MS)
}

/// Mingo's query shortcuts (dehumanizeSelector, renderer line 2129), shown by autocomplete.
pub const DATE_SHORTCUTS: [(&str, &str); 18] = [
    ("#today", "today"),
    ("#yesterday", "yesterday"),
    ("#thisWeek", "this week, from Monday"),
    ("#lastWeek", "last week, Monday to Sunday"),
    ("#thisMonth", "this calendar month"),
    ("#lastMonth", "last calendar month"),
    ("#thisYear", "this calendar year"),
    ("#lastYear", "last calendar year"),
    ("#last24hours", "rolling 24 hours"),
    ("#last7days", "rolling 7 days"),
    ("#last30days", "rolling 30 days"),
    ("#last2months", "rolling 2 months"),
    ("#last3months", "rolling 3 months"),
    ("#last6months", "rolling 6 months"),
    ("#last12months", "rolling 12 months"),
    ("#next7days", "the coming 7 days"),
    ("#tomorrow", "tomorrow"),
    ("#now", "this instant"),
];

pub(crate) const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// Seconds into a day as `HH:MM:SS`.
pub(crate) fn hms(secs: i64) -> String {
    format!("{:02}:{:02}:{:02}", secs / 3600, secs % 3600 / 60, secs % 60)
}

pub(crate) fn ymd(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Shortcuts plus templates filled with real dates, for a date field's value: "between two days",
/// "since", "until", one day, month or year, an exact moment and an explicit range.
pub fn date_suggestions(now: i64) -> Vec<(String, &'static str)> {
    let day = now.div_euclid(DAY_MS);
    let (y, m, _) = civil_from_days(day);
    let month = format!("{y:04}-{m:02}");
    let mut v: Vec<(String, &'static str)> = DATE_SHORTCUTS.iter().map(|(t, n)| (t.to_string(), *n)).collect();
    // The two range templates go right after today and yesterday, where they are seen first.
    v.insert(2, (format!("#{}:{}", ymd(day - 30), ymd(day)), "between two days, both included"));
    v.insert(3, (format!("{{ $gte: ISODate(\"{}T00:00:00Z\"), $lt: ISODate(\"{}T00:00:00Z\") }}", ymd(day - 7), ymd(day + 1)), "custom range, edit the dates"));
    v.extend([
        (format!("#since{}", ymd(days_from_civil(y, m, 1))), "from the start of a day on"),
        (format!("#until{}", ymd(day)), "up to the end of a day"),
        (format!("#after{}", ymd(day - 1)), "after a day"),
        (format!("#before{}", ymd(day)), "before a day"),
        (format!("#{}", ymd(day)), "one whole day"),
        (format!("#{month}"), "one whole month"),
        (format!("#{y:04}-01:{month}"), "between two months"),
        (format!("#{y:04}"), "one whole year"),
        (format!("@{}T09:00:00Z", ymd(day)), "an exact moment"),
    ]);
    v
}

/// A day ("2026-10-03"), month ("2026-10") or year ("2026") as its first millisecond and the
/// first millisecond after it.
pub(crate) fn period(s: &str) -> Option<(i64, i64)> {
    let num = |t: &str, len: usize| (t.len() == len && t.chars().all(|c| c.is_ascii_digit())).then(|| t.parse::<i64>().ok()).flatten();
    let next_month = |y: i64, m: i64| if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    let parts: Vec<&str> = s.split('-').collect();
    let (a, b) = match parts.as_slice() {
        [y] => {
            let y = num(y, 4)?;
            (days_from_civil(y, 1, 1), days_from_civil(y + 1, 1, 1))
        }
        [y, m] => {
            let (y, m) = (num(y, 4)?, num(m, 2).filter(|m| (1..=12).contains(m))?);
            let (ny, nm) = next_month(y, m);
            (days_from_civil(y, m, 1), days_from_civil(ny, nm, 1))
        }
        [y, m, d] => {
            let (y, m, d) = (num(y, 4)?, num(m, 2).filter(|m| (1..=12).contains(m))?, num(d, 2)?);
            let (ny, nm) = next_month(y, m);
            let len = days_from_civil(ny, nm, 1) - days_from_civil(y, m, 1);
            if d < 1 || d > len {
                return None;
            }
            let a = days_from_civil(y, m, d);
            (a, a + 1)
        }
        _ => return None,
    };
    Some((a * DAY_MS, b * DAY_MS))
}

/// Extended JSON for a `#` shortcut (the text after `#`), relative to `now` in epoch ms, with the
/// original's rules (dehumanizeSelector, renderer line 2129): calendar words become `$gte`/`$lt`
/// ranges, `lastN<unit>` is `$gte` N units ago, `nextN<unit>` is between now and N units ahead,
/// dates, months and years (alone or as `a:b` ranges) cover whole periods with `$gte`/`$lte`, and
/// `since`, `until`, `before`, `after` take a day. Case is ignored.
// ponytail: days and months are UTC where the original uses local time; add the local offset if people expect local midnight
pub fn date_shortcut(word: &str, now: i64) -> Option<String> {
    let date = |ms: i64| format!("{{\"$date\":{{\"$numberLong\":\"{ms}\"}}}}");
    let range = |a: i64, b: i64| format!("{{\"$gte\":{},\"$lt\":{}}}", date(a), date(b));
    // The original ends periods with endOf(), the last millisecond, and $lte.
    let whole = |a: i64, b: i64| format!("{{\"$gte\":{},\"$lte\":{}}}", date(a), date(b - 1));
    let day = now.div_euclid(DAY_MS);
    let (y, m, _) = civil_from_days(day);
    let month = |y: i64, m: i64| days_from_civil(y, m, 1) * DAY_MS;
    let prev = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
    let next = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    // ISO weeks start on Monday; 1970-01-01 was a Thursday.
    let week = (day - (day + 3).rem_euclid(7)) * DAY_MS;
    let d0 = day * DAY_MS;
    let w = word.to_ascii_lowercase();
    match w.as_str() {
        "now" => return Some(date(now)),
        "today" => return Some(range(d0, d0 + DAY_MS)),
        "tomorrow" => return Some(range(d0 + DAY_MS, d0 + 2 * DAY_MS)),
        "yesterday" => return Some(range(d0 - DAY_MS, d0)),
        "thisweek" => return Some(range(week, week + 7 * DAY_MS)),
        "lastweek" => return Some(range(week - 7 * DAY_MS, week)),
        "thismonth" => return Some(range(month(y, m), month(next.0, next.1))),
        "lastmonth" => return Some(range(month(prev.0, prev.1), month(y, m))),
        "thisyear" => return Some(range(month(y, 1), month(y + 1, 1))),
        "lastyear" => return Some(range(month(y - 1, 1), month(y, 1))),
        _ => {}
    }
    if let Some((a, b)) = w.split_once(':') {
        let ((a, _), (_, b)) = (period(a)?, period(b)?);
        return (a < b).then(|| whole(a, b));
    }
    if let Some((a, b)) = period(&w) {
        return Some(whole(a, b));
    }
    for (prefix, op) in [("until", 0), ("since", 1), ("before", 2), ("after", 3)] {
        if let Some((a, b)) = w.strip_prefix(prefix).filter(|r| r.len() == 10).and_then(period) {
            return Some(match op {
                0 => format!("{{\"$lte\":{}}}", date(b - 1)),
                1 => format!("{{\"$gte\":{}}}", date(a)),
                // The original compares "before" with the day's first millisecond using $lte.
                2 => format!("{{\"$lte\":{}}}", date(a)),
                _ => format!("{{\"$gt\":{}}}", date(b - 1)),
            });
        }
    }
    let (ahead, rest) = match (w.strip_prefix("last"), w.strip_prefix("next")) {
        (Some(r), _) => (false, r),
        (_, Some(r)) => (true, r),
        _ => return None,
    };
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    // Capped so the date math below can never overflow.
    let n: i64 = if digits == 0 { 1 } else { rest[..digits].parse().ok().filter(|n| *n <= 100_000)? };
    let at = match rest[digits..].strip_suffix('s').unwrap_or(&rest[digits..]) {
        "minute" => now + if ahead { n } else { -n } * 60_000,
        "hour" => now + if ahead { n } else { -n } * 3_600_000,
        "day" => now + if ahead { n } else { -n } * DAY_MS,
        "week" => now + if ahead { n } else { -n } * 7 * DAY_MS,
        "month" => months_back(now, if ahead { -n } else { n }),
        "year" => months_back(now, if ahead { -n } else { n } * 12),
        _ => return None,
    };
    Some(if ahead { format!("{{\"$lte\":{},\"$gt\":{}}}", date(at), date(now)) } else { format!("{{\"$gte\":{}}}", date(at)) })
}

/// Extended JSON for an `@` moment (the text after `@`): `now`, a date or ISO date-time (UTC
/// unless it names a zone), or a time today as `HH:MM` or `HH:MM:SS` (original: renderer line 2242).
pub fn moment(word: &str, now: i64) -> Option<String> {
    let date = |ms: i64| format!("{{\"$date\":{{\"$numberLong\":\"{ms}\"}}}}");
    if word.eq_ignore_ascii_case("now") {
        return Some(date(now));
    }
    let digits = |t: &str| t.len() == 2 && t.chars().all(|c| c.is_ascii_digit());
    if let [h, mi, rest @ ..] = word.split(':').collect::<Vec<_>>().as_slice() && word.len() <= 8 && digits(h) && digits(mi) && rest.iter().all(|s| digits(s)) && rest.len() <= 1 {
        let secs = h.parse::<i64>().ok()? * 3600 + mi.parse::<i64>().ok()? * 60 + rest.first().map_or(Some(0), |s| s.parse::<i64>().ok())?;
        return (secs < 86_400).then(|| date(now.div_euclid(DAY_MS) * DAY_MS + secs * 1000));
    }
    if word.len() == 10 {
        return period(word).map(|(a, _)| date(a));
    }
    let up = word.to_ascii_uppercase();
    let parsed = mongodb::bson::DateTime::parse_rfc3339_str(&up).or_else(|_| mongodb::bson::DateTime::parse_rfc3339_str(format!("{up}Z"))).or_else(|_| mongodb::bson::DateTime::parse_rfc3339_str(format!("{up}:00Z")));
    parsed.ok().map(|d| date(d.timestamp_millis()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{parse_doc, parse_value};

    #[test]
    fn date_shortcuts() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        let at = |y, m, d, h: i64| days_from_civil(y, m, d) * DAY_MS + h * 3_600_000;
        let now = at(2026, 3, 31, 12);
        let gte = |s: Option<String>| parse_value(&s.unwrap()).unwrap().as_document().unwrap().get_datetime("$gte").unwrap().timestamp_millis();
        assert_eq!(gte(date_shortcut("last2months", now)), at(2026, 1, 31, 12));
        assert_eq!(gte(date_shortcut("last1month", now)), at(2026, 2, 28, 12));
        // Like the original, lastMonth and lastYear are the previous calendar month and year.
        assert_eq!(gte(date_shortcut("lastMonth", now)), at(2026, 2, 1, 0));
        assert_eq!(gte(date_shortcut("lastYear", now)), at(2025, 1, 1, 0));
        assert_eq!(gte(date_shortcut("last7days", now)), at(2026, 3, 24, 12));
        assert_eq!(gte(date_shortcut("today", now)), at(2026, 3, 31, 0));
        assert_eq!(gte(date_shortcut("thisYear", now)), at(2026, 1, 1, 0));
        assert!(date_shortcut("last2fortnights", now).is_none());
        assert!(date_shortcut("last999999999999days", now).is_none());
        let get = |s: Option<String>, k: &str| parse_value(&s.unwrap()).unwrap().as_document().unwrap().get_datetime(k).unwrap().timestamp_millis();
        // 2026-03-31 is a Tuesday: the week starts Monday the 30th.
        assert_eq!(get(date_shortcut("thisWeek", now), "$gte"), at(2026, 3, 30, 0));
        assert_eq!(get(date_shortcut("lastWeek", now), "$lt"), at(2026, 3, 30, 0));
        assert_eq!(get(date_shortcut("today", now), "$lt"), at(2026, 4, 1, 0));
        assert_eq!(get(date_shortcut("tomorrow", now), "$gte"), at(2026, 4, 1, 0));
        assert_eq!(get(date_shortcut("thisMonth", now), "$lt"), at(2026, 4, 1, 0));
        assert_eq!(get(date_shortcut("next7days", now), "$lte"), at(2026, 4, 7, 12));
        assert_eq!(get(date_shortcut("next7days", now), "$gt"), now);
        assert_eq!(get(date_shortcut("2026-10-03", now), "$gte"), at(2026, 10, 3, 0));
        assert_eq!(get(date_shortcut("2026-10-03", now), "$lte"), at(2026, 10, 4, 0) - 1);
        assert_eq!(get(date_shortcut("2026-02", now), "$lte"), at(2026, 3, 1, 0) - 1);
        assert_eq!(get(date_shortcut("2024", now), "$lte"), at(2025, 1, 1, 0) - 1);
        assert_eq!(get(date_shortcut("2026-10-01:2026-10-31", now), "$lte"), at(2026, 11, 1, 0) - 1);
        assert_eq!(get(date_shortcut("2026-01:2026-03", now), "$gte"), at(2026, 1, 1, 0));
        assert_eq!(get(date_shortcut("2017:2018", now), "$lte"), at(2019, 1, 1, 0) - 1);
        assert_eq!(get(date_shortcut("until2026-10-03", now), "$lte"), at(2026, 10, 4, 0) - 1);
        assert_eq!(get(date_shortcut("since2026-10-03", now), "$gte"), at(2026, 10, 3, 0));
        assert_eq!(get(date_shortcut("before2026-10-03", now), "$lte"), at(2026, 10, 3, 0));
        assert_eq!(get(date_shortcut("after2026-10-03", now), "$gt"), at(2026, 10, 4, 0) - 1);
        assert!(date_shortcut("2026-02-30", now).is_none());
        assert!(date_shortcut("2026-10-31:2026-10-01", now).is_none());
        let ms = |s: Option<String>| parse_value(&s.unwrap()).unwrap().as_datetime().unwrap().timestamp_millis();
        assert_eq!(ms(moment("17:29", now)), at(2026, 3, 31, 17) + 29 * 60_000);
        assert_eq!(ms(moment("2020-01-29", now)), days_from_civil(2020, 1, 29) * DAY_MS);
        assert_eq!(ms(moment("2020-01-29T17:29:50+02:00", now)), days_from_civil(2020, 1, 29) * DAY_MS + (15 * 3600 + 29 * 60 + 50) * 1000);
        assert_eq!(ms(moment("2020-01-29T17:29", now)), days_from_civil(2020, 1, 29) * DAY_MS + (17 * 3600 + 29 * 60) * 1000);
        assert!(moment("hello", now).is_none());
        let q = parse_doc("{ a: #2026-10-01:2026-10-31, b: #since2026-10-01, c: @2026-10-01T10:00:00Z, d: \"@x #2026\" }").unwrap();
        assert!(q.get_document("a").unwrap().contains_key("$lte"));
        assert!(q.get_datetime("c").is_ok());
        assert_eq!(q.get_str("d").unwrap(), "@x #2026");
        for (t, _) in date_suggestions(now) {
            let v = if t.starts_with('{') { t.clone() } else { format!("{{ x: {t} }}") };
            assert!(parse_doc(&v).is_ok(), "{t}");
        }
        let q = parse_doc("{ at: #yesterday, tag: \"#today\" }").unwrap();
        assert!(q.get_document("at").unwrap().contains_key("$lt"));
        assert_eq!(q.get_str("tag").unwrap(), "#today");
    }
}
