//! The Advanced tab of the connection form: session settings every pooled connection starts with. Statements are
//! built from values that were validated here, never from raw text, except the init command, which is the user's
//! SQL by design and is checked statement by statement.

use super::lex::{self, Tok};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// The flags offered as chips; any other flag goes in the free field.
pub const MODE_FLAGS: [&str; 8] = ["STRICT_TRANS_TABLES", "ONLY_FULL_GROUP_BY", "NO_ZERO_DATE", "NO_ZERO_IN_DATE", "ERROR_FOR_DIVISION_BY_ZERO", "NO_ENGINE_SUBSTITUTION", "ANSI_QUOTES", "NO_BACKSLASH_ESCAPES"];

/// Longest timeout the form accepts, a day. Zero would read as "no limit" on one server and as an error on another.
pub const TIMEOUT_MAX: u32 = 86_400;

#[derive(Clone, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Zone {
    /// Sends nothing, so the server's own zone applies.
    #[default]
    Server,
    Utc,
    /// This computer's UTC offset at connect time.
    System,
    /// An offset such as +02:00 or a named zone.
    Custom(String),
}

/// Quarry decodes every result as UTF-8, so only the two UTF-8 charsets are offered.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Charset {
    #[default]
    Utf8mb4,
    Utf8mb3,
}

impl Charset {
    pub const ALL: [(Charset, &'static str); 2] = [(Charset::Utf8mb4, "utf8mb4"), (Charset::Utf8mb3, "utf8mb3")];

    pub fn as_str(self) -> &'static str {
        match self {
            Charset::Utf8mb4 => "utf8mb4",
            Charset::Utf8mb3 => "utf8mb3",
        }
    }
}

/// Every field defaults, so a connection saved before this tab loads and behaves as it always did.
#[derive(Clone, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Advanced {
    pub zone: Zone,
    pub charset: Charset,
    /// Empty is the charset's default collation.
    pub collation: String,
    /// None keeps the server's mode; Some is the whole mode, possibly empty.
    pub sql_mode: Option<Vec<String>>,
    pub init: String,
    pub connect_secs: Option<u32>,
    pub query_secs: Option<u32>,
    pub idle_secs: Option<u32>,
    /// PostgreSQL only: the rows of the Advanced tab as (name, value), names from `pg::advanced::ALLOWED`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pg_settings: Vec<(String, String)>,
    /// PostgreSQL only: the saved search_path list. The schema picked in the sidebar goes in front of it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub search_path: Vec<String>,
}

/// What the Test button learns beyond "it connected".
#[derive(Clone, Default, Debug)]
pub struct TestInfo {
    /// The server's own default sql_mode, where Custom starts from.
    pub sql_mode: String,
    /// (charset, collation) of the UTF-8 charsets.
    pub collations: Vec<(String, String)>,
    /// PostgreSQL: the schemas of the default database, for the search_path list to offer.
    pub schemas: Vec<String>,
    /// PostgreSQL: (setting, the server's refusal) of each row the server did not accept.
    pub setting_errors: Vec<(String, String)>,
    /// PostgreSQL: the subject of the client certificate the server saw, empty when none was sent.
    pub client_dn: String,
}

fn word(s: &str, extra: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || extra.contains(c))
}

/// "+02:00" style offsets in the range the server takes (-13:59 to +14:00) or a zone name. A name needs the
/// server's zone tables; the server says so itself when it has none.
pub fn check_zone(z: &str) -> Result<(), String> {
    let z = z.trim();
    if z.is_empty() {
        return Err("Time zone: enter an offset like +02:00 or a zone name like Europe/Paris".into());
    }
    if let Some(rest) = z.strip_prefix(['+', '-']) {
        let east = z.starts_with('+');
        let hm = rest.split_once(':').filter(|(h, m)| (1..=2).contains(&h.len()) && m.len() == 2 && h.bytes().all(|b| b.is_ascii_digit()) && m.bytes().all(|b| b.is_ascii_digit()));
        let ok = hm.and_then(|(h, m)| Some((h.parse::<u32>().ok()?, m.parse::<u32>().ok()?))).is_some_and(|(h, m)| m < 60 && if east { h * 60 + m <= 14 * 60 } else { h <= 13 });
        return if ok { Ok(()) } else { Err(format!("Time zone: {z:?} is not an offset from -13:59 to +14:00")) };
    }
    if z.starts_with(|c: char| c.is_ascii_alphabetic()) && word(z, "+-/") {
        Ok(())
    } else {
        Err(format!("Time zone: {z:?} is not an offset like +02:00 or a zone name like Europe/Paris"))
    }
}

pub fn check_collation(c: &str) -> Result<(), String> {
    if c.is_empty() || word(c, "") { Ok(()) } else { Err(format!("Collation: {c:?} may hold only letters, digits and _")) }
}

/// Upper cases, drops empties and repeats, and refuses anything that is not a flag name.
pub fn parse_flags(text: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for f in text.split(',').map(|f| f.trim().to_ascii_uppercase()).filter(|f| !f.is_empty()) {
        if !word(&f, "") {
            return Err(format!("SQL mode: {f:?} is not a flag name (letters, digits and _)"));
        }
        if !out.contains(&f) {
            out.push(f);
        }
    }
    Ok(out)
}

/// This computer's IANA zone name such as Europe/Paris, which is what PostgreSQL takes for "System": an offset would
/// freeze today's daylight saving state.
pub fn system_zone_name() -> Option<String> {
    // Tests read timestamptz text, which a Default zone renders in this zone, so they pin UTC to give the same text on
    // every machine; tests of other zones set them explicitly.
    if cfg!(test) {
        return Some("UTC".into());
    }
    // Read once: the form asks on every frame, and the platform call reads files or system settings.
    static ZONE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    ZONE.get_or_init(|| iana_time_zone::get_timezone().ok()).clone()
}

/// Seconds as "+HH:MM", the form `SET time_zone` takes for an offset.
pub fn offset_text(secs: i32) -> String {
    let (sign, a) = if secs < 0 { ('-', -secs) } else { ('+', secs) };
    format!("{sign}{:02}:{:02}", a / 3600, a % 3600 / 60)
}

/// This computer's current offset from UTC in seconds, east positive.
#[cfg(unix)]
pub fn local_offset_secs() -> Option<i32> {
    // SAFETY: time with a null pointer only returns the clock.
    local_offset_at(unsafe { libc::time(std::ptr::null_mut()) } as i64)
}

/// This computer's offset at `utc_secs`, which differs from today's on the other side of a daylight saving change.
#[cfg(unix)]
pub fn local_offset_at(utc_secs: i64) -> Option<i32> {
    // SAFETY: localtime_r fills the struct we pass and keeps no pointer to it.
    unsafe {
        let now = utc_secs as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return None;
        }
        Some(tm.tm_gmtoff as i32)
    }
}

/// Seconds east of UTC from what Windows reports: Bias is minutes west of UTC, and the daylight or standard bias
/// in force is added to it. Kept apart from the call so it is compiled and tested on every platform.
#[cfg(any(windows, test))]
pub fn bias_offset_secs(daylight: bool, bias: i32, standard_bias: i32, daylight_bias: i32) -> i32 {
    -(bias + if daylight { daylight_bias } else { standard_bias }) * 60
}

#[cfg(windows)]
pub fn local_offset_secs() -> Option<i32> {
    use windows_sys::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_ID_INVALID, TIME_ZONE_INFORMATION};
    // windows-sys 0.61 has this constant only in Win32::System::SystemServices; 2 is its documented value.
    const TIME_ZONE_ID_DAYLIGHT: u32 = 2;
    // SAFETY: the struct is plain data that the call fills.
    unsafe {
        let mut tz: TIME_ZONE_INFORMATION = std::mem::zeroed();
        let id = GetTimeZoneInformation(&mut tz);
        if id == TIME_ZONE_ID_INVALID {
            return None;
        }
        Some(bias_offset_secs(id == TIME_ZONE_ID_DAYLIGHT, tz.Bias, tz.StandardBias, tz.DaylightBias))
    }
}

#[cfg(not(any(unix, windows)))]
pub fn local_offset_secs() -> Option<i32> {
    None
}

// ponytail: today's offset on Windows, so dates across a daylight saving change are an hour off there; SystemTimeToTzSpecificLocalTime would fix it.
#[cfg(not(unix))]
pub fn local_offset_at(_utc_secs: i64) -> Option<i32> {
    local_offset_secs()
}

/// Session variables an init command may set. Each is session scoped, leaves the read-only guard and autocommit
/// alone, and does not change how Quarry quotes or decodes text (the SQL mode, Charset and Time zone fields own
/// those). Names are compared lower case, without quotes or a `@@session.` style prefix.
pub const INIT_VARS: &[&str] = &[
    "group_concat_max_len", "innodb_lock_wait_timeout", "lock_wait_timeout", "sql_safe_updates", "max_execution_time", "max_statement_time", "wait_timeout", "interactive_timeout", "net_read_timeout", "net_write_timeout",
    "max_heap_table_size", "tmp_table_size", "sort_buffer_size", "join_buffer_size", "read_buffer_size", "read_rnd_buffer_size", "cte_max_recursion_depth", "max_recursive_iterations", "div_precision_increment", "default_week_format",
    "lc_time_names", "max_sort_length", "optimizer_search_depth", "optimizer_prune_level", "eq_range_index_dive_limit", "long_query_time", "profiling", "transaction_isolation", "tx_isolation",
];

/// Why a variable that is not on the list is refused, for the ones a user is likely to try.
fn refusal(name: &str) -> String {
    if name == "time_zone" {
        "set the time zone with the Time zone field above".into()
    } else if name == "sql_mode" {
        "set the SQL mode with the SQL mode field above".into()
    } else if name == "names" || name == "character" || name == "charset" || name.starts_with("character_set") || name.starts_with("collation") {
        "set the character set with the Charset field above".into()
    } else if name.contains("read_only") || name == "transaction" {
        "it would lift the read-only guard".into()
    } else if matches!(name, "autocommit" | "completion_type") {
        format!("{name} stays as Quarry sets it, or a write would report success and then be rolled back")
    } else if matches!(name, "sql_select_limit" | "max_join_size" | "sql_big_selects") {
        format!("{name} would cut what Quarry itself reads, such as dumps, exports and table structure")
    } else {
        format!("{name} is not one of the session variables it may set ({})", INIT_VARS.join(", "))
    }
}

/// A value token an init command may use: literals and arithmetic on them, nothing that reads or calls.
fn literal_ok(t: Tok, s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    match t {
        Tok::Str => s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') && !s.contains('\\'),
        Tok::Word => matches!(l.as_str(), "on" | "off" | "default" | "true" | "false" | "null") || digits(&l) || l.strip_prefix("0x").is_some_and(|h| !h.is_empty() && h.bytes().all(|b| b.is_ascii_hexdigit())) || l.split_once('e').is_some_and(|(m, x)| digits(m) && digits(x)),
        Tok::Punct => matches!(s, "+" | "-" | "*" | "/" | "%" | "(" | ")" | "."),
        _ => false,
    }
}

/// Checks one statement's code tokens (no spaces or comments): `SET` and assignments separated by commas.
fn check_set(toks: &[(Tok, &str)], short: &str) -> Result<(), String> {
    let fail = |why: String| Err(format!("Init command: {why} ({short})"));
    let low = |i: usize| toks.get(i).map(|(_, s)| s.to_ascii_lowercase()).unwrap_or_default();
    if low(0) != "set" {
        return fail("only SET statements are allowed".into());
    }
    let mut i = 1;
    loop {
        let w = low(i);
        let at_scope = w.strip_prefix("@@").filter(|_| matches!(toks.get(i + 1), Some((Tok::Punct, ".")))).map(str::to_string);
        if matches!(w.as_str(), "global" | "persist" | "persist_only") || at_scope.as_deref().is_some_and(|s| s != "session" && s != "local") {
            return fail("only the session may be set, not GLOBAL or PERSIST".into());
        }
        if w == "session" || w == "local" {
            i += 1;
        } else if at_scope.is_some() {
            i += 2;
        }
        let Some(&(t, raw)) = toks.get(i) else { return fail("a variable name is missing".into()) };
        let user = t == Tok::Word && raw.starts_with('@') && !raw.starts_with("@@");
        if !user {
            let after_dot = at_scope.is_some() && matches!(toks.get(i - 1), Some((Tok::Punct, ".")));
            let bare = if after_dot { raw } else { raw.strip_prefix("@@").unwrap_or(raw) };
            if !matches!(t, Tok::Word | Tok::Quoted) || bare.starts_with('@') {
                return fail(format!("{raw} is not a variable name"));
            }
            // Backticks or ANSI double quotes around a name do not change which variable it is.
            let name = bare.chars().filter(|c| *c != '`' && *c != '"').collect::<String>().to_ascii_lowercase();
            if !INIT_VARS.contains(&name.as_str()) {
                return fail(refusal(&name));
            }
        }
        i += 1;
        match (toks.get(i).map(|(_, s)| *s), toks.get(i + 1).map(|(_, s)| *s)) {
            (Some(":"), Some("=")) => i += 2,
            (Some("="), _) => i += 1,
            _ => return fail(format!("{raw} must be followed by =")),
        }
        let (start, mut depth) = (i, 0i32);
        while let Some(&(t, s)) = toks.get(i) {
            if t == Tok::Punct && s == "," && depth == 0 {
                break;
            }
            if !literal_ok(t, s) {
                return fail(format!("{s} is not allowed in a value, which takes numbers, quoted strings without backslashes, ON, OFF, DEFAULT or NULL"));
            }
            depth += (s == "(") as i32 - (s == ")") as i32;
            if depth < 0 {
                return fail("unbalanced parentheses".into());
            }
            i += 1;
        }
        if i == start || depth != 0 {
            return fail(format!("{raw} has no complete value"));
        }
        if i == toks.len() {
            return Ok(());
        }
        i += 1;
    }
}

/// The init command's statements, checked. Every connection starts read-only and the init command runs right
/// after that guard, so it may only set user variables and the session variables in `INIT_VARS`, to literal
/// values. The text must lex the same under every reading a server may apply (quote modes, backslash escapes,
/// executable comments), so what was checked is what any server runs; the worker then sends each statement on
/// its own as a prepared statement, which the server parses as exactly one.
pub fn init_statements(text: &str) -> Result<Vec<String>, String> {
    let readings = lex::every_reading(text, super::Dialect::MySql);
    if readings[0].iter().any(|(t, _)| *t == Tok::ExecOpen) {
        return Err("Init command: executable comments (/*! and /*M!) are not allowed".into());
    }
    if readings.iter().any(|r| *r != readings[0]) {
        return Err("Init command: it reads differently under ANSI_QUOTES or NO_BACKSLASH_ESCAPES; use single quotes and no backslashes".into());
    }
    let mut out = Vec::new();
    let mut code: Vec<(Tok, &str)> = Vec::new();
    let mut span: Option<std::ops::Range<usize>> = None;
    let mut flush = |code: &mut Vec<(Tok, &str)>, span: &mut Option<std::ops::Range<usize>>| -> Result<(), String> {
        if let Some(r) = span.take() {
            let stmt = &text[r];
            check_set(code, &stmt.lines().next().unwrap_or("").chars().take(60).collect::<String>())?;
            out.push(stmt.to_string());
        }
        code.clear();
        Ok(())
    };
    for (t, r) in &readings[0] {
        match t {
            Tok::Space | Tok::Comment => {}
            Tok::Punct if &text[r.clone()] == ";" => flush(&mut code, &mut span)?,
            _ => {
                code.push((*t, &text[r.clone()]));
                span = Some(span.map_or(r.clone(), |s| s.start..r.end));
            }
        }
    }
    flush(&mut code, &mut span)?;
    Ok(out)
}

pub fn init_check(text: &str) -> Result<(), String> {
    init_statements(text).map(|_| ())
}

impl Advanced {
    pub fn is_default(&self) -> bool {
        *self == Advanced::default()
    }

    /// Everything the form must check before it saves or tests.
    pub fn validate(&self) -> Result<(), String> {
        if let Zone::Custom(z) = &self.zone {
            check_zone(z)?;
        }
        check_collation(&self.collation)?;
        if let Some(flags) = &self.sql_mode {
            for f in flags {
                if !word(f, "") || f.chars().any(|c| c.is_ascii_lowercase()) {
                    return Err(format!("SQL mode: {f:?} is not a flag name (upper case letters, digits and _)"));
                }
            }
        }
        for (name, v) in [("Connect", self.connect_secs), ("Query", self.query_secs), ("Idle", self.idle_secs)] {
            if v.is_some_and(|v| v == 0 || v > TIMEOUT_MAX) {
                return Err(format!("{name} timeout: use 1 to {TIMEOUT_MAX} seconds, or leave it empty"));
            }
        }
        init_check(&self.init)
    }

    /// The zone to send, if any. `system` is this computer's offset in seconds.
    /// The session's offset in seconds when it is fixed (UTC, System or a Custom offset). None for the server's zone or
    /// a named one, whose offset depends on rules Quarry does not carry.
    pub fn offset_secs(&self, system: Option<i32>) -> Option<i32> {
        let z = self.zone_value(system).ok()??;
        let sign = match z.as_bytes().first()? {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        let (h, m) = z[1..].split_once(':')?;
        Some(sign * (h.parse::<i32>().ok()? * 3600 + m.parse::<i32>().ok()? * 60))
    }

    pub fn zone_value(&self, system: Option<i32>) -> Result<Option<String>, String> {
        Ok(match &self.zone {
            Zone::Server => None,
            Zone::Utc => Some("+00:00".into()),
            Zone::System => Some(offset_text(system.ok_or("Time zone: this computer's offset could not be read, use Custom")?)),
            Zone::Custom(z) => Some(z.trim().to_string()),
        })
    }

    /// The session statements the settings above the init command call for, in the order they run. Nothing is
    /// sent for a default, so an old connection keeps its old session. Values were validated, so quoting them in
    /// single quotes is safe.
    pub fn session_statements(&self, system: Option<i32>) -> Result<Vec<String>, String> {
        self.validate()?;
        let mut out = Vec::new();
        if let Some(z) = self.zone_value(system)? {
            out.push(format!("SET SESSION time_zone = '{z}'"));
        }
        if self.charset != Charset::Utf8mb4 || !self.collation.is_empty() {
            out.push(if self.collation.is_empty() { format!("SET NAMES {}", self.charset.as_str()) } else { format!("SET NAMES {} COLLATE {}", self.charset.as_str(), self.collation) });
        }
        if let Some(flags) = &self.sql_mode {
            out.push(format!("SET SESSION sql_mode = '{}'", flags.join(",")));
        }
        Ok(out)
    }

    pub fn connect_timeout(&self, default: Duration) -> Duration {
        self.connect_secs.map_or(default, |s| Duration::from_secs(s as u64))
    }

    /// Milliseconds for the statement timeout.
    pub fn query_ms(&self, default_ms: u64) -> u64 {
        self.query_secs.map_or(default_ms, |s| s as u64 * 1000)
    }

    /// The connect timeout and the statement timeout in ms, from the Settings timeout. An empty field follows
    /// Settings, so an empty Query never takes the Connect field's value.
    pub fn timeouts(&self, settings: Duration) -> (Duration, u64) {
        (self.connect_timeout(settings), self.query_ms(settings.as_millis() as u64))
    }

    /// None keeps sqlx's own 600 seconds.
    pub fn idle_timeout(&self) -> Option<Duration> {
        self.idle_secs.map(|s| Duration::from_secs(s as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zones() {
        for ok in ["+02:00", "-05:30", "+0:00", "+14:00", "-13:59", "+13:59", "Europe/Paris", "UTC", "Etc/GMT+5", "EST5EDT", "America/Port-au-Prince", "SYSTEM"] {
            assert!(check_zone(ok).is_ok(), "{ok}");
        }
        // The server takes -13:59 to +14:00 only.
        for bad in ["", " ", "+2", "+15:00", "+14:59", "+14:01", "-14:00", "-13:60", "+02:60", "02:00", "+02:0", "+ab:cd", "Europe/Paris'; DROP", "1Europe", "a b", "+02:00 ", "'+00:00'", "+-02:00"] {
            if bad == "+02:00 " {
                continue; // surrounding space is trimmed
            }
            assert!(check_zone(bad).is_err(), "{bad}");
        }
        assert!(check_zone(&"a".repeat(65)).is_err());
    }

    #[test]
    fn offsets() {
        assert_eq!(offset_text(0), "+00:00");
        assert_eq!(offset_text(7200), "+02:00");
        assert_eq!(offset_text(-19800), "-05:30");
        assert_eq!(offset_text(20700), "+05:45");
        assert!(local_offset_secs().is_some_and(|s| s.abs() <= 14 * 3600));
        // What GetTimeZoneInformation gives for Paris (Bias -60, daylight -60), New York (300, -60) and India.
        assert_eq!(bias_offset_secs(false, -60, 0, -60), 3600);
        assert_eq!(bias_offset_secs(true, -60, 0, -60), 7200);
        assert_eq!(bias_offset_secs(true, 300, 0, -60), -4 * 3600);
        assert_eq!(bias_offset_secs(false, 300, 0, -60), -5 * 3600);
        assert_eq!(offset_text(bias_offset_secs(false, -330, 0, 0)), "+05:30");
    }

    #[test]
    fn collations() {
        assert!(check_collation("").is_ok());
        assert!(check_collation("utf8mb4_0900_ai_ci").is_ok());
        for bad in ["utf8mb4 general", "a;b", "x'y", "a-b", "\u{e9}"] {
            assert!(check_collation(bad).is_err(), "{bad}");
        }
        assert!(check_collation(&"a".repeat(65)).is_err());
    }

    #[test]
    fn flags() {
        assert_eq!(parse_flags(" no_zero_date, ONLY_FULL_GROUP_BY ,,no_zero_date ").unwrap(), ["NO_ZERO_DATE", "ONLY_FULL_GROUP_BY"]);
        assert!(parse_flags("").unwrap().is_empty());
        for bad in ["A B", "A'B", "A;B", "TIME-ZONE", "A,B)"] {
            assert!(parse_flags(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn timeouts() {
        let t = |c, q, i| Advanced { connect_secs: c, query_secs: q, idle_secs: i, ..Default::default() }.validate();
        assert!(t(None, None, None).is_ok());
        assert!(t(Some(1), Some(86_400), Some(600)).is_ok());
        assert!(t(Some(0), None, None).unwrap_err().starts_with("Connect"));
        assert!(t(None, Some(86_401), None).unwrap_err().starts_with("Query"));
        assert!(t(None, None, Some(0)).unwrap_err().starts_with("Idle"));
        let a = Advanced { query_secs: Some(5), ..Default::default() };
        assert_eq!((a.query_ms(30_000), Advanced::default().query_ms(30_000)), (5000, 30_000));
        assert_eq!(a.connect_timeout(Duration::from_secs(9)), Duration::from_secs(9));
        assert_eq!(Advanced { idle_secs: Some(60), ..Default::default() }.idle_timeout(), Some(Duration::from_secs(60)));
        assert_eq!(Advanced::default().idle_timeout(), None);
        // An empty Query follows Settings, not the Connect field.
        let settings = Duration::from_secs(30);
        assert_eq!(Advanced { connect_secs: Some(5), ..Default::default() }.timeouts(settings), (Duration::from_secs(5), 30_000));
        assert_eq!(Advanced { connect_secs: Some(5), query_secs: Some(2), ..Default::default() }.timeouts(settings), (Duration::from_secs(5), 2_000));
        assert_eq!(Advanced::default().timeouts(settings), (settings, 30_000));
    }

    #[test]
    fn statements_for_each_option() {
        let s = |a: Advanced| a.session_statements(Some(7200)).unwrap();
        assert!(s(Advanced::default()).is_empty());
        assert_eq!(s(Advanced { zone: Zone::Utc, ..Default::default() }), ["SET SESSION time_zone = '+00:00'"]);
        assert_eq!(s(Advanced { zone: Zone::System, ..Default::default() }), ["SET SESSION time_zone = '+02:00'"]);
        assert_eq!(s(Advanced { zone: Zone::Custom(" Europe/Paris ".into()), ..Default::default() }), ["SET SESSION time_zone = 'Europe/Paris'"]);
        assert_eq!(s(Advanced { charset: Charset::Utf8mb3, ..Default::default() }), ["SET NAMES utf8mb3"]);
        assert_eq!(s(Advanced { collation: "utf8mb4_bin".into(), ..Default::default() }), ["SET NAMES utf8mb4 COLLATE utf8mb4_bin"]);
        assert_eq!(s(Advanced { sql_mode: Some(vec!["ANSI_QUOTES".into(), "NO_BACKSLASH_ESCAPES".into()]), ..Default::default() }), ["SET SESSION sql_mode = 'ANSI_QUOTES,NO_BACKSLASH_ESCAPES'"]);
        assert_eq!(s(Advanced { sql_mode: Some(vec![]), ..Default::default() }), ["SET SESSION sql_mode = ''"]);
        assert!(Advanced { zone: Zone::System, ..Default::default() }.session_statements(None).is_err());
        assert!(Advanced { zone: Zone::Custom("x'; DROP".into()), ..Default::default() }.session_statements(None).is_err());
        assert!(Advanced { sql_mode: Some(vec!["a'b".into()]), ..Default::default() }.session_statements(None).is_err());
    }

    #[test]
    fn init_command_rules() {
        let design = "SET SESSION group_concat_max_len = 1048576;\nSET SESSION innodb_lock_wait_timeout = 10;\nSET SESSION sql_safe_updates = 1;";
        assert_eq!(init_statements(design).unwrap(), ["SET SESSION group_concat_max_len = 1048576", "SET SESSION innodb_lock_wait_timeout = 10", "SET SESSION sql_safe_updates = 1"]);
        assert!(init_statements("").unwrap().is_empty());
        for ok in [
            "SET @a = 1",
            "SET @@SESSION.max_execution_time = 10",
            "set local sort_buffer_size = 1024 * 1024, @x := 'it''s', @@lc_time_names = 'fr_FR'",
            "SET SESSION `group_concat_max_len` = DEFAULT; -- a note\nSET @@local.sql_safe_updates = ON",
            "SET SESSION transaction_isolation = 'READ-COMMITTED'",
            "SET SESSION div_precision_increment = (2 + 2) % 3, long_query_time = 0.5, tmp_table_size = 0x1000000, @n = NULL, @e = 1e3",
        ] {
            assert!(init_check(ok).is_ok(), "{ok}: {:?}", init_check(ok));
        }
        for bad in [
            "INSERT INTO t VALUES (1)",
            "CREATE TABLE t (a int)",
            "DROP DATABASE x",
            "USE shop",
            "SET GLOBAL max_connections = 10",
            "SET PERSIST x = 1",
            "SET @@global.sql_mode = ''",
            "SET SESSION TRANSACTION READ WRITE",
            "SET SESSION transaction_read_only = 0",
            "SET @@session.tx_read_only = 0",
            "SET PASSWORD = 'x'",
            "SET sql_mode = ''",
            "SET NAMES latin1",
            "PREPARE s FROM 'SET @a = 1'",
            "SELECT 1 INTO OUTFILE '/tmp/x'",
            "SET @a = 1; DELETE FROM t",
            "SELECT 1",
            "SET STATEMENT max_statement_time = 1 FOR SELECT 1",
            "SET SESSION no_such_var_x = 1",
            "SET @a = (SELECT 1)",
            "SET @a = SLEEP(5)",
            "SET @a = @@global.max_connections",
            "SET @a = `x`",
            "SET @a = 'x\\'",
            "SET @a = ",
            "SET @a = (1",
            "SET @a 1",
            "SET SESSION",
            "SET @@persist.max_connections = 10",
            "SET LOCAL @@session.wait_timeout = 1",
        ] {
            assert!(init_check(bad).is_err(), "{bad}");
        }
        // The text of the statement is in the error, trimmed to a line.
        assert!(init_check("USE shop").unwrap_err().contains("USE shop"));
    }

    /// The review's proven bypasses: each passed the old check, connected with transaction_read_only=0 and ran a
    /// CREATE DATABASE.
    #[test]
    fn init_command_refuses_the_proven_bypasses() {
        for bad in [
            // Executable comments: the old check read them as plain comments, the server as code.
            "SET @x = 1 /*!99999 ' */ ; SET SESSION TRANSACTION READ WRITE; CREATE DATABASE IF NOT EXISTS quarry_probe_pwn; -- '",
            "SET @x = 1 /*M! ' */ ; SET SESSION TRANSACTION READ WRITE; CREATE DATABASE IF NOT EXISTS quarry_probe_pwn; -- '",
            "SET @x = 1 /*!999999 ' */ ; SET SESSION TRANSACTION READ WRITE; CREATE DATABASE IF NOT EXISTS quarry_probe_pwn; -- '",
            // NO_BACKSLASH_ESCAPES ends the string at the backslash.
            "SET @x = 'a\\'; SET SESSION TRANSACTION READ WRITE; CREATE DATABASE IF NOT EXISTS quarry_probe_nbe; -- '",
            // ANSI_QUOTES makes the double quotes an identifier with no escape.
            "SET @x = (SELECT 1 AS \"a\\\"); SET SESSION TRANSACTION READ WRITE; -- \")",
            // Quoted names skipped the word check.
            "SET SESSION `transaction_read_only` = 0",
            "SET @@session.`transaction_read_only` = 0",
            "SET SESSION `tx_read_only` = 0",
            "SET SESSION \"transaction_read_only\" = 0",
            "SET @@`transaction_read_only` = 0",
        ] {
            let err = init_check(bad).expect_err(bad);
            assert!(err.starts_with("Init command:"), "{bad}: {err}");
        }
        assert!(init_check("SET SESSION `transaction_read_only` = 0").unwrap_err().contains("read-only guard"));
        assert!(init_check("SET @x = 1 /*!99999 ' */ ;").unwrap_err().contains("executable comments"));
    }

    /// Variables that lose data or move what another field owns are not on the list, and the refusal says where to go.
    #[test]
    fn init_command_refuses_session_breaking_and_owned_variables() {
        for (bad, says) in [
            ("SET SESSION autocommit = 0", "rolled back"),
            ("SET autocommit = 0", "rolled back"),
            ("SET SESSION sql_select_limit = 1", "cut what Quarry itself reads"),
            ("SET SESSION max_join_size = 1", "cut what Quarry itself reads"),
            ("SET SESSION sql_big_selects = 0", "cut what Quarry itself reads"),
            ("SET @@session.completion_type = 2", "rolled back"),
            ("SET SESSION sql_log_bin = 0", "not one of"),
            ("SET SESSION foreign_key_checks = 0", "not one of"),
            ("SET SESSION unique_checks = 0", "not one of"),
            ("SET SESSION character_set_results = latin1", "Charset field"),
            ("SET SESSION character_set_results = 'latin1'", "Charset field"),
            ("SET collation_connection = 'latin1_swedish_ci'", "Charset field"),
            ("SET character_set_database = 'latin1'", "Charset field"),
            ("SET @@session.character_set_client = 'latin1'", "Charset field"),
            ("SET character_set_connection = 'latin1'", "Charset field"),
            ("SET collation_database = 'latin1_bin'", "Charset field"),
            ("SET NAMES latin1", "Charset field"),
            ("SET CHARACTER SET latin1", "Charset field"),
            ("SET SESSION time_zone = '+01:00'", "Time zone field"),
            ("SET SESSION sql_mode = ''", "SQL mode field"),
        ] {
            let err = init_check(bad).expect_err(bad);
            assert!(err.contains(says), "{bad}: {err}");
        }
    }

    #[test]
    fn an_old_saved_connection_loads_with_defaults() {
        // Written before the tab existed: no `advanced` key at all, and an empty one.
        let a: Advanced = serde_json::from_str("{}").unwrap();
        assert!(a.is_default());
        let a: Advanced = serde_json::from_str(r#"{"zone":{"Custom":"+02:00"},"query_secs":5}"#).unwrap();
        assert_eq!((a.zone.clone(), a.query_secs, a.charset, a.sql_mode.clone()), (Zone::Custom("+02:00".into()), Some(5), Charset::Utf8mb4, None));
        let back: Advanced = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        assert_eq!(a, back);
    }
}

/// Live tests: each connects with one option set and reads back what the server session holds. They set nothing
/// global; the settings are the connection's own, as the app applies them. Needs QUARRY_TEST_MYSQL and
/// QUARRY_TEST_MARIADB (a server whose variable is unset is skipped).
#[cfg(test)]
mod live {
    use super::super::worker::{raw, session, Setup};
    use super::super::{testing::ENVS, Db, Dialect};
    use super::*;
    use crate::db::{Msg, Net, Worker};
    use sqlx::Connection;
    use std::time::Instant;

    struct Conn {
        w: Worker,
        rx: std::sync::mpsc::Receiver<Msg>,
        uri: String,
    }

    impl Conn {
        fn new(uri: &str) -> Conn {
            let (tx, rx) = std::sync::mpsc::channel();
            Conn { w: Worker::new(tx, eframe::egui::Context::default()), rx, uri: uri.into() }
        }

        fn open(&self, adv: &Advanced) -> Result<Db, String> {
            self.w.sql_connect_with(1, 0, self.uri.clone(), Duration::from_secs(20), Net::default(), adv.clone());
            let Msg::SqlConnected { res, .. } = self.rx.recv_timeout(Duration::from_secs(40)).unwrap() else { panic!("expected SqlConnected") };
            res.map(|(db, _, _)| db)
        }

        fn fails(&self, adv: &Advanced) -> String {
            self.open(adv).err().expect("the connect should fail")
        }

        fn one(&self, db: &Db, q: &str) -> String {
            let p = db.my_pool().unwrap();
            self.w.rt.block_on(async {
                let r = sqlx::raw_sql(q).fetch_one(p).await.unwrap_or_else(|x| panic!("{q}: {x}"));
                String::from_utf8_lossy(raw(&r, 0).unwrap_or_default()).into_owned()
            })
        }

        /// Runs `q` as the test login on a connection outside the app's setup, for fixtures and checks.
        fn admin(&self, q: &str) -> Vec<String> {
            self.w.rt.block_on(async {
                let mut c = sqlx::MySqlConnection::connect(&self.uri).await.unwrap();
                let rows = sqlx::raw_sql(q).fetch_all(&mut c).await.unwrap_or_else(|x| panic!("{q}: {x}"));
                let _ = c.close().await;
                rows.iter().map(|r| String::from_utf8_lossy(raw(r, 0).unwrap_or_default()).into_owned()).collect()
            })
        }

        /// `db` with a pool whose connections run `setup` as the app's own setup would, without the init check.
        fn with_setup(&self, db: &Db, setup: Setup) -> Db {
            let hook = setup.clone();
            let pool = self.w.rt.block_on(async {
                sqlx::mysql::MySqlPoolOptions::new()
                    .max_connections(2)
                    .after_connect(move |c, _| {
                        let hook = hook.clone();
                        Box::pin(async move { session(c, 30_000, &hook).await })
                    })
                    .connect_with(db.opts.my().unwrap().clone())
                    .await
                    .unwrap()
            });
            Db { pool: super::super::Pool::My(pool), setup, ..db.clone() }
        }
    }

    fn each(f: impl Fn(&Conn, &str)) {
        for var in ENVS {
            if let Ok(uri) = std::env::var(var) {
                f(&Conn::new(&uri), var);
            }
        }
    }

    #[test]
    fn live_defaults_send_nothing() {
        each(|c, var| {
            let db = c.open(&Advanced::default()).unwrap();
            let global = c.one(&db, "SELECT @@GLOBAL.sql_mode");
            // The driver's handshake adds IGNORE_SPACE on MySQL; that is today's behavior and is kept.
            assert_eq!(db.sql_mode.replace("IGNORE_SPACE,", ""), global, "{var}");
        });
    }

    #[test]
    fn live_time_zone() {
        each(|c, var| {
            for (zone, want) in [(Zone::Utc, "+00:00".to_string()), (Zone::Custom("+02:00".into()), "+02:00".into()), (Zone::System, offset_text(local_offset_secs().unwrap()))] {
                let db = c.open(&Advanced { zone: zone.clone(), ..Default::default() }).unwrap();
                assert_eq!(c.one(&db, "SELECT @@SESSION.time_zone"), want, "{var} {zone:?}");
            }
            // A named zone is the server's to accept; without zone tables its error comes back as the connect error.
            match c.open(&Advanced { zone: Zone::Custom("Europe/Paris".into()), ..Default::default() }) {
                Ok(db) => assert_eq!(c.one(&db, "SELECT @@SESSION.time_zone"), "Europe/Paris", "{var}"),
                Err(m) => assert!(m.contains("time_zone") && m.contains("Europe/Paris"), "{var}: {m}"),
            }
        });
    }

    #[test]
    fn live_sql_mode_custom_drives_quoting() {
        each(|c, var| {
            let adv = Advanced { sql_mode: Some(vec!["STRICT_TRANS_TABLES".into(), "ANSI_QUOTES".into(), "NO_BACKSLASH_ESCAPES".into()]), ..Default::default() };
            let db = c.open(&adv).unwrap();
            assert!(db.sql_mode.contains("ANSI_QUOTES") && db.sql_mode.contains("NO_BACKSLASH_ESCAPES") && db.sql_mode.contains("STRICT_TRANS_TABLES"), "{var}: {}", db.sql_mode);
            assert!(!db.backslash_escapes(), "{var}");
            // Under ANSI_QUOTES a double quote holds an identifier, so this is an alias and not a string.
            assert_eq!(c.one(&db, "SELECT 1 AS \"x\""), "1");
            let none = c.open(&Advanced { sql_mode: Some(vec![]), ..Default::default() }).unwrap();
            assert_eq!(none.sql_mode, "", "{var}");
            assert!(none.backslash_escapes());
        });
    }

    #[test]
    fn live_charset_and_collation() {
        each(|c, var| {
            let db = c.open(&Advanced { charset: Charset::Utf8mb3, ..Default::default() }).unwrap();
            assert_eq!(c.one(&db, "SELECT @@SESSION.character_set_client"), "utf8mb3", "{var}");
            assert_eq!(c.one(&db, "SELECT @@SESSION.character_set_results"), "utf8mb3", "{var}");
            let db = c.open(&Advanced { collation: "utf8mb4_bin".into(), ..Default::default() }).unwrap();
            assert_eq!(c.one(&db, "SELECT @@SESSION.collation_connection"), "utf8mb4_bin", "{var}");
            let err = c.fails(&Advanced { collation: "latin1_swedish_ci".into(), ..Default::default() });
            assert!(err.contains("SET NAMES utf8mb4 COLLATE latin1_swedish_ci"), "{var}: {err}");
        });
    }

    #[test]
    fn live_init_command_runs_on_every_pooled_session() {
        each(|c, var| {
            let adv = Advanced { init: "SET SESSION group_concat_max_len = 2048;\nSET @quarry_init = 7;".into(), ..Default::default() };
            let db = c.open(&adv).unwrap();
            let p = db.my_pool().unwrap();
            // Four connections held at once force the pool to open its whole size.
            let got = c.w.rt.block_on(async {
                let mut held = Vec::new();
                for _ in 0..4 {
                    held.push(p.acquire().await.unwrap());
                }
                let mut out = Vec::new();
                for h in held.iter_mut() {
                    let r = sqlx::raw_sql("SELECT @@SESSION.group_concat_max_len, @quarry_init, @@SESSION.transaction_read_only").fetch_one(&mut **h).await.unwrap();
                    out.push((0..3).map(|i| String::from_utf8_lossy(raw(&r, i).unwrap_or_default()).into_owned()).collect::<Vec<_>>());
                }
                out
            });
            assert_eq!(got.len(), 4);
            for g in got {
                // The guard is still on after the init command ran.
                assert_eq!(g, ["2048", "7", "1"], "{var}");
            }
        });
    }

    /// The review's proven bypasses, with the sql_mode each needs, as the connect sees them.
    const HOSTILE: [(&str, &str); 6] = [
        ("", "SET @x = 1 /*!99999 ' */ ; SET SESSION TRANSACTION READ WRITE; CREATE DATABASE IF NOT EXISTS quarry_probe_af2_pwn; -- '"),
        ("", "SET @x = 1 /*M! ' */ ; SET SESSION TRANSACTION READ WRITE; CREATE DATABASE IF NOT EXISTS quarry_probe_af2_pwn; -- '"),
        ("", "SET @x = 1 /*!999999 ' */ ; SET SESSION TRANSACTION READ WRITE; CREATE DATABASE IF NOT EXISTS quarry_probe_af2_pwn; -- '"),
        ("NO_BACKSLASH_ESCAPES", "SET @x = 'a\\'; SET SESSION TRANSACTION READ WRITE; CREATE DATABASE IF NOT EXISTS quarry_probe_af2_nbe; -- '"),
        ("ANSI_QUOTES", "SET @x = (SELECT 1 AS \"a\\\"); SET SESSION TRANSACTION READ WRITE; CREATE DATABASE IF NOT EXISTS quarry_probe_af2_ansi; -- \")"),
        ("", "SET SESSION `transaction_read_only` = 0"),
    ];

    fn probes(c: &Conn) -> Vec<String> {
        c.admin("SHOW DATABASES LIKE 'quarry_probe_af2%'")
    }

    #[test]
    fn live_init_command_cannot_lift_the_guard_or_write() {
        each(|c, var| {
            let mut bad: Vec<(&str, &str)> = ["SET SESSION TRANSACTION READ WRITE", "CREATE TABLE quarry_adv_x (a int)", "USE mysql", "SET SESSION autocommit = 0", "SET SESSION character_set_results = latin1", "SET SESSION tx_read_only = 0"].map(|b| ("", b)).to_vec();
            bad.extend(HOSTILE);
            for (mode, bad) in bad {
                let sql_mode = (!mode.is_empty()).then(|| vec![mode.to_string()]);
                let err = c.fails(&Advanced { init: bad.into(), sql_mode, ..Default::default() });
                assert!(err.starts_with("Init command:"), "{var} {bad}: {err}");
            }
            let err = c.fails(&Advanced { init: "SET SESSION character_set_results = latin1".into(), ..Default::default() });
            assert!(err.contains("Charset field"), "{var}: {err}");
            let err = c.fails(&Advanced { init: "SET SESSION autocommit = 0".into(), ..Default::default() });
            assert!(err.contains("rolled back"), "{var}: {err}");
            assert!(probes(c).is_empty(), "{var}");
        });
    }

    /// Even with the check skipped, the setup sends each statement prepared, then sets the guard and autocommit
    /// again and reads them back, so a hostile init command fails the connect or leaves the session read-only.
    #[test]
    fn live_session_setup_holds_without_the_init_check() {
        each(|c, var| {
            let mut all: Vec<(&str, &str)> = HOSTILE.to_vec();
            all.extend(["SET SESSION TRANSACTION READ WRITE", "SET SESSION transaction_read_only = 0", "SET @@session.tx_read_only = 0", "SET SESSION autocommit = 0", "SET SESSION completion_type = 2"].map(|b| ("", b)));
            for (mode, bad) in all {
                let statements = if mode.is_empty() { vec![] } else { vec![format!("SET SESSION sql_mode = '{mode}'")] };
                let setup = Setup::new(statements, vec![bad.to_string()]);
                let got = c.w.rt.block_on(async {
                    let mut conn = sqlx::MySqlConnection::connect(&c.uri).await.unwrap();
                    let res = session(&mut conn, 30_000, &setup).await;
                    let flags = sqlx::raw_sql("SELECT @@SESSION.transaction_read_only, @@SESSION.autocommit").fetch_one(&mut conn).await.ok().map(|r| (raw(&r, 0).map(<[u8]>::to_vec), raw(&r, 1).map(<[u8]>::to_vec)));
                    let _ = conn.close().await;
                    (res.map_err(|x| x.to_string()), flags)
                });
                match got {
                    (Ok(()), Some((ro, ac))) => assert_eq!((ro.as_deref(), ac.as_deref()), (Some(&b"1"[..]), Some(&b"1"[..])), "{var} {bad}"),
                    (Ok(()), None) => panic!("{var} {bad}: the flags could not be read"),
                    (Err(m), _) => assert!(!m.is_empty(), "{var} {bad}"),
                }
            }
            assert!(probes(c).is_empty(), "{var}: {:?}", probes(c));
        });
    }

    /// With autocommit off an unlocked INSERT reported one row and was rolled back on close. The setup forces it on.
    #[test]
    fn live_unlocked_insert_persists_with_autocommit_forced() {
        each(|c, var| {
            let schema = "quarry_af2_autocommit";
            c.admin(&format!("DROP DATABASE IF EXISTS {schema}; CREATE DATABASE {schema}; CREATE TABLE {schema}.t (id INT PRIMARY KEY)"));
            let base = c.open(&Advanced::default()).unwrap();
            let db = c.with_setup(&base, Setup::new(vec![], vec!["SET SESSION autocommit = 0".into()]));
            let n = c.w.rt.block_on(async {
                let mut w = super::super::write::writer_conn(&db, schema, false).await.unwrap().into_my().unwrap();
                let n = sqlx::query("INSERT INTO t VALUES (1)").execute(&mut w).await.unwrap().rows_affected();
                let _ = w.close().await;
                n
            });
            assert_eq!(n, 1, "{var}");
            assert_eq!(c.admin(&format!("SELECT COUNT(*) FROM {schema}.t")), ["1"], "{var}");
            c.admin(&format!("DROP DATABASE {schema}"));
        });
    }

    /// A failing setting or init command used to wait the whole connect timeout while the pool retried it.
    #[test]
    fn live_failing_setup_fails_fast_with_its_text() {
        each(|c, var| {
            let t = Instant::now();
            let err = c.fails(&Advanced { init: "SET @a = 1;\nSET SESSION group_concat_max_len = 'x';".into(), ..Default::default() });
            assert!(err.contains("Init command failed on") && err.contains("group_concat_max_len"), "{var}: {err}");
            assert!(t.elapsed() < Duration::from_secs(3), "{var} took {:?}", t.elapsed());
            let t = Instant::now();
            let err = c.fails(&Advanced { zone: Zone::Custom("Nowhere/Atlantis".into()), ..Default::default() });
            assert!(err.contains("time_zone") && err.contains("Nowhere/Atlantis"), "{var}: {err}");
            assert!(t.elapsed() < Duration::from_secs(3), "{var} took {:?}", t.elapsed());
        });
    }

    /// Value suggestions open their own connection, which now gets the same session as the grid.
    #[test]
    fn live_field_values_follow_the_custom_zone() {
        each(|c, var| {
            let schema = "quarry_af2_values";
            c.admin(&format!("DROP DATABASE IF EXISTS {schema}; CREATE DATABASE {schema}; CREATE TABLE {schema}.t (id INT PRIMARY KEY, ts TIMESTAMP NULL, KEY (ts)); SET time_zone = '+00:00'; INSERT INTO {schema}.t VALUES (1, '2024-01-01 00:00:00')"));
            let db = c.open(&Advanced { zone: Zone::Custom("+05:00".into()), ..Default::default() }).unwrap();
            c.w.sql_field_values(1, db, schema.into(), "t".into(), "ts".into(), None);
            let Msg::SqlValues { res, .. } = c.rx.recv_timeout(Duration::from_secs(20)).unwrap() else { panic!("expected SqlValues") };
            let vals = res.unwrap().expect("a small indexed table is looked in");
            assert_eq!(vals.iter().map(|(v, _)| v.as_str()).collect::<Vec<_>>(), ["2024-01-01 05:00:00"], "{var}");
            c.admin(&format!("DROP DATABASE {schema}"));
        });
    }

    #[test]
    fn live_query_timeout_stops_select_on_mysql_and_everything_on_mariadb() {
        each(|c, var| {
            let db = c.open(&Advanced { query_secs: Some(1), ..Default::default() }).unwrap();
            let p = db.my_pool().unwrap();
            let time = |q: &str| {
                let t = Instant::now();
                let _ = c.w.rt.block_on(async { sqlx::raw_sql(q).execute(p).await });
                t.elapsed()
            };
            let t = time("SELECT SLEEP(5)");
            assert!(t < Duration::from_millis(3500), "{var} select took {t:?}");
            let t = time("DO SLEEP(3)");
            if db.dialect == Dialect::MariaDb {
                assert!(t < Duration::from_millis(2500), "{var} do took {t:?}");
            } else {
                assert!(t >= Duration::from_millis(2900), "{var} MySQL max_execution_time only limits SELECT, took {t:?}");
            }
        });
    }

    #[test]
    fn live_idle_and_connect_timeouts_apply() {
        each(|c, var| {
            let db = c.open(&Advanced { idle_secs: Some(30), connect_secs: Some(5), ..Default::default() }).unwrap();
            let p = db.my_pool().unwrap();
            assert_eq!(p.options().get_idle_timeout(), Some(Duration::from_secs(30)), "{var}");
            assert_eq!(p.options().get_acquire_timeout(), Duration::from_secs(5), "{var}");
            let db = c.open(&Advanced::default()).unwrap();
            let p = db.my_pool().unwrap();
            assert_eq!(p.options().get_idle_timeout(), Some(Duration::from_secs(600)), "{var}");
        });
    }

    #[test]
    fn live_test_reads_the_server_mode_and_collations() {
        each(|c, var| {
            c.w.sql_test_with(0, c.uri.clone(), Duration::from_secs(10), Net::default(), Advanced::default());
            let Msg::Tested { res, info, .. } = c.rx.recv_timeout(Duration::from_secs(30)).unwrap() else { panic!("expected Tested") };
            res.unwrap();
            let info = info.unwrap();
            assert!(info.collations.iter().any(|(cs, n)| cs == "utf8mb4" && n == "utf8mb4_bin"), "{var}");
            assert!(info.collations.iter().any(|(cs, _)| cs == "utf8mb3"), "{var}");
            let db = c.open(&Advanced::default()).unwrap();
            assert_eq!(info.sql_mode, db.sql_mode.replace("IGNORE_SPACE,", ""), "{var}");
        });
    }
}
