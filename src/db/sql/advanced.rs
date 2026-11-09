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
}

/// What the Test button learns beyond "it connected".
#[derive(Clone, Default, Debug)]
pub struct TestInfo {
    /// The server's own default sql_mode, where Custom starts from.
    pub sql_mode: String,
    /// (charset, collation) of the UTF-8 charsets.
    pub collations: Vec<(String, String)>,
}

fn word(s: &str, extra: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || extra.contains(c))
}

/// "+02:00" style offsets (hours 0 to 14) or a zone name. A name needs the server's zone tables; the server
/// says so itself when it has none.
pub fn check_zone(z: &str) -> Result<(), String> {
    let z = z.trim();
    if z.is_empty() {
        return Err("Time zone: enter an offset like +02:00 or a zone name like Europe/Paris".into());
    }
    if let Some(rest) = z.strip_prefix(['+', '-']) {
        let ok = rest.split_once(':').is_some_and(|(h, m)| (1..=2).contains(&h.len()) && m.len() == 2 && h.bytes().all(|b| b.is_ascii_digit()) && m.bytes().all(|b| b.is_ascii_digit()) && h.parse::<u32>().is_ok_and(|h| h <= 14) && m.parse::<u32>().is_ok_and(|m| m < 60));
        return if ok { Ok(()) } else { Err(format!("Time zone: {z:?} is not an offset like +02:00")) };
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

/// Seconds as "+HH:MM", the form `SET time_zone` takes for an offset.
pub fn offset_text(secs: i32) -> String {
    let (sign, a) = if secs < 0 { ('-', -secs) } else { ('+', secs) };
    format!("{sign}{:02}:{:02}", a / 3600, a % 3600 / 60)
}

/// This computer's current offset from UTC in seconds, east positive.
#[cfg(unix)]
pub fn local_offset_secs() -> Option<i32> {
    // SAFETY: localtime_r fills the struct we pass and keeps no pointer to it.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return None;
        }
        Some(tm.tm_gmtoff as i32)
    }
}

#[cfg(windows)]
pub fn local_offset_secs() -> Option<i32> {
    use windows_sys::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION, TIME_ZONE_ID_DAYLIGHT, TIME_ZONE_ID_INVALID};
    // SAFETY: the struct is plain data that the call fills.
    unsafe {
        let mut tz: TIME_ZONE_INFORMATION = std::mem::zeroed();
        let id = GetTimeZoneInformation(&mut tz);
        if id == TIME_ZONE_ID_INVALID {
            return None;
        }
        // Bias is minutes west of UTC; daylight time adds its own bias.
        let bias = tz.Bias + if id == TIME_ZONE_ID_DAYLIGHT { tz.DaylightBias } else { tz.StandardBias };
        Some(-bias * 60)
    }
}

#[cfg(not(any(unix, windows)))]
pub fn local_offset_secs() -> Option<i32> {
    None
}

/// The init command's statements, each as the user wrote it.
pub fn init_statements(text: &str) -> Vec<String> {
    lex::split_statements(text).into_iter().map(|(_, s)| s.to_string()).collect()
}

/// Which statements an init command may hold. Every connection starts read-only and the init command runs right
/// after that guard, so it may configure the session but must not lift the guard, write, change where the session
/// points, or move the escaping that Quarry quotes by (the SQL mode and Charset fields own that). The server's
/// READ ONLY session stays the real boundary; this refuses early and says why.
pub fn init_check(text: &str) -> Result<(), String> {
    for (_, stmt) in lex::split_statements(text) {
        let words: Vec<String> = lex::tokens(stmt).into_iter().filter(|(t, _)| *t == Tok::Word).map(|(_, r)| stmt[r].to_ascii_lowercase()).collect();
        let first = words.first().map(String::as_str).unwrap_or("");
        let short = || stmt.lines().next().unwrap_or("").chars().take(60).collect::<String>();
        if first == "use" {
            return Err(format!("Init command: USE is not allowed, the connection picks its database elsewhere ({})", short()));
        }
        if lex::uses_prepared(stmt) {
            return Err(format!("Init command: prepared statements are not allowed ({})", short()));
        }
        if lex::changes_escaping(stmt) {
            return Err(format!("Init command: change the character set or SQL mode with the fields above ({})", short()));
        }
        if first == "set" {
            let reach = words.iter().any(|w| matches!(w.as_str(), "global" | "persist" | "persist_only") || w.starts_with("@@global") || w.starts_with("@@persist") || w.contains("read_only"));
            if reach || words.get(1).is_some_and(|w| w == "password" || w == "default") || words.windows(2).any(|w| w[0] == "read" && w[1] == "write") {
                return Err(format!("Init command: this SET changes more than the session or lifts the read-only guard ({})", short()));
            }
        } else {
            lex::read_only_check(stmt).map_err(|m| format!("Init command: {m} ({})", short()))?;
        }
    }
    Ok(())
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
        for ok in ["+02:00", "-05:30", "+0:00", "+14:00", "Europe/Paris", "UTC", "Etc/GMT+5", "EST5EDT", "America/Port-au-Prince", "SYSTEM"] {
            assert!(check_zone(ok).is_ok(), "{ok}");
        }
        for bad in ["", " ", "+2", "+15:00", "+02:60", "02:00", "+02:0", "+ab:cd", "Europe/Paris'; DROP", "1Europe", "a b", "+02:00 ", "'+00:00'", "+-02:00"] {
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
        assert!(init_check(design).is_ok());
        assert_eq!(init_statements(design).len(), 3);
        assert!(init_check("").is_ok());
        assert!(init_check("SET @a = 1; SELECT 1; SHOW VARIABLES LIKE 'x%'").is_ok());
        assert!(init_check("SET @@SESSION.max_execution_time = 10").is_ok());
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
        ] {
            assert!(init_check(bad).is_err(), "{bad}");
        }
        // The text of the statement is in the error, trimmed to a line.
        assert!(init_check("USE shop").unwrap_err().contains("USE shop"));
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
    use super::super::{testing::ENVS, worker::raw, Db, Dialect};
    use super::*;
    use crate::db::{Msg, Net, Worker};
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
            self.w.sql_connect_with(1, self.uri.clone(), Duration::from_secs(20), Net::default(), adv.clone());
            let Msg::SqlConnected { res, .. } = self.rx.recv_timeout(Duration::from_secs(40)).unwrap() else { panic!("expected SqlConnected") };
            res.map(|(db, _, _)| db)
        }

        fn fails(&self, adv: &Advanced) -> String {
            self.open(adv).err().expect("the connect should fail")
        }

        fn one(&self, db: &Db, q: &str) -> String {
            let crate::db::sql::Pool::My(p) = &db.pool;
            self.w.rt.block_on(async {
                let r = sqlx::raw_sql(q).fetch_one(p).await.unwrap_or_else(|x| panic!("{q}: {x}"));
                String::from_utf8_lossy(raw(&r, 0).unwrap_or_default()).into_owned()
            })
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
            let crate::db::sql::Pool::My(p) = &db.pool;
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

    #[test]
    fn live_init_command_cannot_lift_the_guard_or_write() {
        each(|c, var| {
            for bad in ["SET SESSION TRANSACTION READ WRITE", "CREATE TABLE quarry_adv_x (a int)", "USE mysql"] {
                let err = c.fails(&Advanced { init: bad.into(), ..Default::default() });
                assert!(err.starts_with("Init command:"), "{var} {bad}: {err}");
            }
        });
    }

    #[test]
    fn live_failing_init_command_fails_the_connect_with_its_text() {
        each(|c, var| {
            let err = c.fails(&Advanced { init: "SET @a = 1;\nSET SESSION no_such_variable_here = 1;".into(), ..Default::default() });
            assert!(err.contains("Init command failed on") && err.contains("no_such_variable_here"), "{var}: {err}");
        });
    }

    #[test]
    fn live_query_timeout_stops_select_on_mysql_and_everything_on_mariadb() {
        each(|c, var| {
            let db = c.open(&Advanced { query_secs: Some(1), ..Default::default() }).unwrap();
            let crate::db::sql::Pool::My(p) = &db.pool;
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
            let crate::db::sql::Pool::My(p) = &db.pool;
            assert_eq!(p.options().get_idle_timeout(), Some(Duration::from_secs(30)), "{var}");
            assert_eq!(p.options().get_acquire_timeout(), Duration::from_secs(5), "{var}");
            let db = c.open(&Advanced::default()).unwrap();
            let crate::db::sql::Pool::My(p) = &db.pool;
            assert_eq!(p.options().get_idle_timeout(), Some(Duration::from_secs(600)), "{var}");
        });
    }

    #[test]
    fn live_test_reads_the_server_mode_and_collations() {
        each(|c, var| {
            c.w.sql_test_with(c.uri.clone(), Duration::from_secs(10), Net::default(), Advanced::default());
            let Msg::Tested { res, info } = c.rx.recv_timeout(Duration::from_secs(30)).unwrap() else { panic!("expected Tested") };
            res.unwrap();
            let info = info.unwrap();
            assert!(info.collations.iter().any(|(cs, n)| cs == "utf8mb4" && n == "utf8mb4_bin"), "{var}");
            assert!(info.collations.iter().any(|(cs, _)| cs == "utf8mb3"), "{var}");
            let db = c.open(&Advanced::default()).unwrap();
            assert_eq!(info.sql_mode, db.sql_mode.replace("IGNORE_SPACE,", ""), "{var}");
        });
    }
}
