//! The Advanced tab of a PostgreSQL connection: session settings every pooled connection starts with. Names come only
//! from the allow-list below and values are checked for shape here and by the server on Test. Nothing the user types is
//! ever a setting name or SQL: a value is bound to `set_config` on a direct connection and written as a quoted literal
//! in the SET LOCAL a pooler needs, and a value that holds a backslash or a control character is refused.

use super::advanced::{Advanced, Zone};
use super::Dialect;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Planner,
    Memory,
    Locale,
    Limits,
    /// The rows that are always shown.
    Session,
}

impl Group {
    pub fn label(self) -> &'static str {
        match self {
            Group::Planner => "Planner",
            Group::Memory => "Memory",
            Group::Locale => "Locale",
            Group::Limits => "Limits",
            Group::Session => "Session",
        }
    }
}

/// What a value looks like, for the message under a row. The server has the last word on Test.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Bool,
    Int(i64, i64),
    Real,
    /// A number with an optional unit: kB, MB, GB or TB.
    Size,
    /// A number with an optional unit: us, ms, s, min, h or d.
    Time,
    /// A locale, a configuration or another name made of letters, digits and . _ @ -
    Name,
    Text,
}

#[derive(Clone, Copy, Debug)]
pub struct Def {
    pub name: &'static str,
    pub group: Group,
    pub desc: &'static str,
    pub kind: Kind,
    /// Shown in the empty box of the row.
    pub hint: &'static str,
}

const fn d(name: &'static str, group: Group, desc: &'static str, kind: Kind, hint: &'static str) -> Def {
    Def { name, group, desc, kind, hint }
}

use Group::{Limits, Locale, Memory, Planner, Session};

/// Every setting the form offers, and the only names that ever reach the server besides TimeZone and search_path.
pub const ALLOWED: &[Def] = &[
    d("statement_timeout", Session, "Longest time a statement may run", Kind::Time, "Settings timeout"),
    d("lock_timeout", Session, "Longest wait for a lock", Kind::Time, "0"),
    d("application_name", Session, "Name shown in pg_stat_activity", Kind::Text, "Quarry"),
    d("random_page_cost", Planner, "Cost of a page fetched out of order", Kind::Real, "4"),
    d("seq_page_cost", Planner, "Cost of a page read in sequence", Kind::Real, "1"),
    d("cpu_tuple_cost", Planner, "Cost of processing one row", Kind::Real, "0.01"),
    d("cpu_index_tuple_cost", Planner, "Cost of processing one index entry", Kind::Real, "0.005"),
    d("cpu_operator_cost", Planner, "Cost of one operator or function call", Kind::Real, "0.0025"),
    d("effective_cache_size", Planner, "Cache the planner assumes is available", Kind::Size, "4GB"),
    d("enable_seqscan", Planner, "Allow sequential scans", Kind::Bool, "on"),
    d("enable_indexscan", Planner, "Allow index scans", Kind::Bool, "on"),
    d("enable_indexonlyscan", Planner, "Allow index-only scans", Kind::Bool, "on"),
    d("enable_bitmapscan", Planner, "Allow bitmap scans", Kind::Bool, "on"),
    d("enable_tidscan", Planner, "Allow TID scans", Kind::Bool, "on"),
    d("enable_sort", Planner, "Allow explicit sort steps", Kind::Bool, "on"),
    d("enable_hashagg", Planner, "Allow hashed aggregation", Kind::Bool, "on"),
    d("enable_hashjoin", Planner, "Allow hash joins", Kind::Bool, "on"),
    d("enable_mergejoin", Planner, "Allow merge joins", Kind::Bool, "on"),
    d("enable_nestloop", Planner, "Allow nested loop joins", Kind::Bool, "on"),
    d("enable_material", Planner, "Allow materialization", Kind::Bool, "on"),
    d("enable_memoize", Planner, "Allow memoize nodes (14 and later)", Kind::Bool, "on"),
    d("enable_gathermerge", Planner, "Allow gather merge", Kind::Bool, "on"),
    d("enable_incremental_sort", Planner, "Allow incremental sorts", Kind::Bool, "on"),
    d("enable_partitionwise_join", Planner, "Allow joins between matching partitions", Kind::Bool, "off"),
    d("enable_partitionwise_aggregate", Planner, "Allow aggregation per partition", Kind::Bool, "off"),
    d("enable_partition_pruning", Planner, "Allow skipping partitions a query cannot reach", Kind::Bool, "on"),
    d("enable_parallel_append", Planner, "Allow parallel append", Kind::Bool, "on"),
    d("enable_parallel_hash", Planner, "Allow parallel hash joins", Kind::Bool, "on"),
    d("jit", Planner, "Allow just-in-time compilation", Kind::Bool, "on"),
    d("jit_above_cost", Planner, "Query cost above which JIT is used", Kind::Real, "100000"),
    d("jit_inline_above_cost", Planner, "Query cost above which JIT inlines functions", Kind::Real, "500000"),
    d("jit_optimize_above_cost", Planner, "Query cost above which JIT optimizes", Kind::Real, "500000"),
    d("max_parallel_workers_per_gather", Planner, "Parallel workers one query step may use", Kind::Int(0, 1024), "2"),
    d("max_parallel_workers", Planner, "Parallel workers in use at once", Kind::Int(0, 1024), "8"),
    d("max_parallel_maintenance_workers", Planner, "Parallel workers for index builds", Kind::Int(0, 1024), "2"),
    d("parallel_setup_cost", Planner, "Cost of starting parallel workers", Kind::Real, "1000"),
    d("parallel_tuple_cost", Planner, "Cost of passing a row from a worker", Kind::Real, "0.1"),
    d("parallel_leader_participation", Planner, "Let the leader process run the plan too", Kind::Bool, "on"),
    d("min_parallel_table_scan_size", Planner, "Smallest table a parallel scan is used for", Kind::Size, "8MB"),
    d("min_parallel_index_scan_size", Planner, "Smallest index a parallel scan is used for", Kind::Size, "512kB"),
    d("work_mem", Memory, "Memory for one sort or hash before it spills", Kind::Size, "4MB"),
    d("maintenance_work_mem", Memory, "Memory for index builds and vacuum", Kind::Size, "64MB"),
    d("temp_buffers", Memory, "Memory for temporary tables", Kind::Size, "8MB"),
    d("hash_mem_multiplier", Memory, "How far a hash may exceed work_mem", Kind::Real, "2"),
    d("lc_messages", Locale, "Language of server messages", Kind::Name, "en_US.UTF-8"),
    d("lc_numeric", Locale, "Format of numbers in to_char", Kind::Name, "en_US.UTF-8"),
    d("lc_time", Locale, "Format of dates and times in to_char", Kind::Name, "en_US.UTF-8"),
    d("default_text_search_config", Locale, "Text search configuration", Kind::Name, "pg_catalog.english"),
    d("idle_in_transaction_session_timeout", Limits, "Ends a session left idle inside a transaction", Kind::Time, "0"),
    d("log_min_duration_statement", Limits, "Logs statements slower than this", Kind::Time, "-1"),
    d("temp_file_limit", Limits, "Disk a session may use for temporary files", Kind::Size, "-1"),
];

/// Settings Quarry keeps for itself, each with the one reason the form shows. They are refused in a saved connection too.
pub const KEPT: &[(&str, &str)] = &[
    ("default_transaction_read_only", "the read-only lock"),
    ("transaction_read_only", "the read-only lock"),
    ("role", "whose rights the lock checks"),
    ("session_authorization", "whose rights the lock checks"),
    ("client_encoding", "results are decoded as UTF-8"),
    ("standard_conforming_strings", "how values are quoted in SQL"),
    ("DateStyle", "how values are read and edited"),
    ("IntervalStyle", "how values are read and edited"),
    ("bytea_output", "how values are read and edited"),
    // Floats are read back from their text and money is parsed from its text, so a different spelling loses digits or
    // makes an edit's WHERE miss the row it was read from.
    ("extra_float_digits", "how values are read and edited"),
    ("lc_monetary", "how values are read and edited"),
];

pub fn def(name: &str) -> Option<&'static Def> {
    ALLOWED.iter().find(|d| d.name.eq_ignore_ascii_case(name.trim()))
}

pub(super) fn kept_reason(name: &str) -> Option<&'static str> {
    KEPT.iter().find(|(n, _)| n.eq_ignore_ascii_case(name.trim())).map(|(_, r)| *r)
}

/// The shape message of a value, as the form words it under the row.
fn shape(kind: Kind) -> String {
    match kind {
        Kind::Bool => "Expected on or off".into(),
        Kind::Int(lo, hi) => format!("Expected a whole number from {lo} to {hi}"),
        Kind::Real => "Expected a number like 1.1".into(),
        Kind::Size => "Expected a size like 64MB or 1GB".into(),
        Kind::Time => "Expected a time like 500ms, 30s or 5min".into(),
        Kind::Name => "Expected a name made of letters, digits and . _ @ -".into(),
        Kind::Text => "Expected plain printable text".into(),
    }
}

fn number(s: &str) -> bool {
    let s = s.strip_prefix('-').unwrap_or(s);
    let (whole, frac) = s.split_once('.').map_or((s, None), |(w, f)| (w, Some(f)));
    !whole.is_empty() && whole.bytes().all(|b| b.is_ascii_digit()) && frac.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
}

/// A number and an optional unit of `units`, with spaces allowed between them as the server allows.
fn with_unit(v: &str, units: &[&str]) -> bool {
    let end = v.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(v.len());
    let (n, u) = (v[..end].trim_end(), &v[end..]);
    number(n) && (u.is_empty() || units.contains(&u))
}

/// Checks that a value has the shape of its setting. Every value is also free of what could break out of a quoted
/// literal, so the SET LOCAL a pooler needs can be written safely whatever the setting.
pub fn check_value(name: &str, value: &str) -> Result<(), String> {
    let Some(def) = def(name) else {
        return Err(match kept_reason(name) {
            Some(why) => format!("{name} is kept by Quarry: {why}"),
            None => format!("{name} is not a setting Quarry offers"),
        });
    };
    let v = value.trim();
    if v.is_empty() {
        return Err(format!("{}: enter a value or remove the row", def.name));
    }
    if v.len() > 128 || v.chars().any(|c| c.is_control() || c == '\\') {
        return Err(format!("{}: the value is too long or holds a backslash or a control character", def.name));
    }
    let ok = match def.kind {
        Kind::Bool => ["on", "off", "true", "false", "yes", "no", "1", "0"].iter().any(|b| v.eq_ignore_ascii_case(b)),
        Kind::Int(lo, hi) => v.parse::<i64>().is_ok_and(|n| (lo..=hi).contains(&n)),
        Kind::Real => number(v),
        Kind::Size => with_unit(v, &["B", "kB", "MB", "GB", "TB"]),
        Kind::Time => with_unit(v, &["us", "ms", "s", "min", "h", "d"]),
        Kind::Name => v.len() <= 64 && v.chars().all(|c| c.is_ascii_alphanumeric() || "._@-".contains(c)),
        Kind::Text => v.len() <= 63 && v.chars().all(|c| (' '..='~').contains(&c)),
    };
    if ok { Ok(()) } else { Err(format!("{}: {}", def.name, shape(def.kind))) }
}

/// The zone text of a Custom choice: a name or an offset the server then validates.
pub fn check_zone(z: &str) -> Result<(), String> {
    let z = z.trim();
    if !z.is_empty() && z.len() <= 64 && z.chars().all(|c| c.is_ascii_alphanumeric() || "_+-/:".contains(c)) {
        Ok(())
    } else {
        Err("Time zone: enter a zone name like Europe/Paris or an offset like +02:00".into())
    }
}

/// An offset such as +02:00 or -0330 in the form PostgreSQL reads as meant. A bare signed offset is POSIX there, where the
/// sign counts west of Greenwich, so +02:00 would mean UTC-2. The angle bracket spelling names the zone and flips the
/// sign. A zone name and anything else is left as typed.
fn posix_offset(z: &str) -> String {
    let (sign, rest) = match z.as_bytes().first() {
        Some(b'+') => ('+', &z[1..]),
        Some(b'-') => ('-', &z[1..]),
        _ => return z.to_string(),
    };
    let (h, m) = match rest.split_once(':') {
        Some((h, m)) => (h, m),
        None if rest.len() == 4 => rest.split_at(2),
        None => (rest, "00"),
    };
    let digits = |t: &str| !t.is_empty() && t.len() <= 2 && t.bytes().all(|b| b.is_ascii_digit());
    if !digits(h) || !digits(m) {
        return z.to_string();
    }
    let (h, m) = (format!("{h:0>2}"), format!("{m:0>2}"));
    let flipped = if sign == '+' { '-' } else { '+' };
    format!("<{sign}{h}{m}>{flipped}{h}:{m}")
}

/// One schema of the search_path list. Every pg_ name is refused, because users cannot create such a schema: pg_temp
/// holds what belongs to one backend (which a pooler may hand to another client), and pg_catalog listed after another
/// schema would stop being searched first, so that schema's functions could shadow the built-ins Quarry's own
/// unqualified catalog SQL calls.
pub fn check_schema(s: &str) -> Result<(), String> {
    let s = s.trim();
    if s.is_empty() || s.len() > 63 || s.chars().any(|c| c.is_control()) {
        return Err(format!("search_path: {s:?} is not a schema name"));
    }
    if s.to_ascii_lowercase().starts_with("pg_") {
        return Err(format!("search_path: {s} is a system schema, which the server searches first on its own, and is not offered"));
    }
    Ok(())
}

pub(super) fn check_connection(a: &Advanced) -> Result<(), String> {
    for (name, v) in [("Connect", a.connect_secs), ("Idle", a.idle_secs)] {
        if v.is_some_and(|v| v == 0 || v > super::advanced::TIMEOUT_MAX) {
            return Err(format!("{name} timeout: use 1 to {} seconds, or leave it empty", super::advanced::TIMEOUT_MAX));
        }
    }
    Ok(())
}

/// A value as a SQL literal that reads the same whatever standard_conforming_strings says.
pub fn literal(v: &str) -> String {
    super::value::pg_lit(v)
}

/// The search_path value as the server reads it: each schema a quoted identifier.
pub(super) fn path_value(schemas: &[String]) -> String {
    schemas.iter().map(|s| Dialect::Postgres.quote_ident(s)).collect::<Vec<_>>().join(", ")
}

/// The settings of one connection as they are applied, checked and in the order they run.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct PgSession {
    /// (name, value) of TimeZone, search_path (the saved list) and the rows. Names are the allow-list's spelling.
    pub settings: Vec<(String, String)>,
    /// The saved search_path list, which follows the schema of the tab wherever Quarry sets the path.
    pub tail: Vec<String>,
}

impl PgSession {
    /// `system` is this computer's IANA zone name.
    pub fn new(adv: &Advanced, system: Option<String>) -> Result<PgSession, String> {
        check_connection(adv)?;
        let mut settings: Vec<(String, String)> = Vec::new();
        match &adv.zone {
            // sqlx starts every session in UTC, so a server default would never show: this computer's zone is what a
            // reader expects, and a zone that cannot be read leaves the session in UTC as before.
            Zone::Server => {
                if let Some(z) = system.filter(|z| check_zone(z).is_ok()) {
                    settings.push(("TimeZone".into(), z));
                }
            }
            Zone::Utc => settings.push(("TimeZone".into(), "UTC".into())),
            Zone::System => settings.push(("TimeZone".into(), system.filter(|z| check_zone(z).is_ok()).ok_or("Time zone: this computer's zone name could not be read, use Custom")?)),
            Zone::Custom(z) => {
                check_zone(z)?;
                settings.push(("TimeZone".into(), posix_offset(z.trim())));
            }
        }
        let mut tail: Vec<String> = Vec::new();
        for s in &adv.search_path {
            check_schema(s)?;
            let s = s.trim().to_string();
            if !tail.contains(&s) {
                tail.push(s);
            }
        }
        if !tail.is_empty() {
            settings.push(("search_path".into(), path_value(&tail)));
        }
        for (name, value) in &adv.pg_settings {
            check_value(name, value)?;
            let canon = def(name).map_or(name.as_str(), |d| d.name);
            if settings.iter().any(|(n, _)| n.eq_ignore_ascii_case(canon)) {
                return Err(format!("{canon} appears twice"));
            }
            settings.push((canon.into(), value.trim().into()));
        }
        Ok(PgSession { settings, tail })
    }

    /// The search_path of a tab: its schema first, then the saved list without a repeat. An empty schema is just the
    /// saved list, and with no list either, nothing (the server's own).
    pub fn path(&self, schema: &str) -> String {
        let mut all: Vec<String> = Vec::new();
        // pg_catalog is never listed: the server searches it first when it is absent, and a listed one sits where it is put.
        if !schema.is_empty() && !schema.eq_ignore_ascii_case("pg_catalog") {
            all.push(schema.to_string());
        }
        all.extend(self.tail.iter().filter(|t| t.as_str() != schema && !t.eq_ignore_ascii_case("pg_catalog")).cloned());
        path_value(&all)
    }

    /// The statements of a transaction behind a pooler, as SET LOCAL. A writer leaves out what it sets for itself:
    /// its statement timeout is none, its lock timeout is its own, and its schema is the table's. A path is a list of
    /// identifiers, not a literal, because SET reads one literal as one schema.
    pub fn locals(&self, writer: bool) -> String {
        let own = |n: &str| writer && ["statement_timeout", "lock_timeout", "search_path"].contains(&n);
        self.settings
            .iter()
            .filter(|(n, _)| !own(n))
            .map(|(n, v)| if n == "search_path" { format!("SET LOCAL search_path = {v}") } else { format!("SET LOCAL {n} = {}", literal(v)) })
            .collect::<Vec<_>>()
            .join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_allow_list_has_unique_names_and_none_of_the_kept_ones() {
        for (i, a) in ALLOWED.iter().enumerate() {
            assert!(ALLOWED[i + 1..].iter().all(|b| !b.name.eq_ignore_ascii_case(a.name)), "{} twice", a.name);
            assert!(kept_reason(a.name).is_none(), "{} is kept", a.name);
            assert!(a.name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_' || b.is_ascii_digit()), "{}", a.name);
        }
        for need in ["work_mem", "random_page_cost", "jit", "enable_seqscan", "lc_time", "temp_file_limit", "statement_timeout"] {
            assert!(def(need).is_some(), "{need}");
        }
        for k in ["default_transaction_read_only", "role", "session_authorization", "client_encoding", "standard_conforming_strings", "DateStyle", "IntervalStyle", "bytea_output", "transaction_read_only"] {
            assert!(def(k).is_none() && kept_reason(k).is_some(), "{k}");
        }
    }

    #[test]
    fn values_have_the_shape_of_their_setting() {
        for (n, v) in [("work_mem", "64MB"), ("work_mem", "64 MB"), ("work_mem", "1024"), ("temp_file_limit", "-1"), ("random_page_cost", "1.1"), ("jit", "off"), ("JIT", "On"), ("statement_timeout", "30s"), ("statement_timeout", "5min"), ("statement_timeout", "0"), ("lc_time", "en_US.UTF-8"), ("application_name", "my app"), ("log_min_duration_statement", "-1"), ("max_parallel_workers_per_gather", "0")] {
            check_value(n, v).unwrap_or_else(|x| panic!("{n} {v}: {x}"));
        }
        for (n, v) in [("work_mem", "64 megs"), ("work_mem", "MB"), ("work_mem", "1.5.2"), ("random_page_cost", "fast"), ("jit", "maybe"), ("statement_timeout", "30 sec"), ("lc_time", "a b"), ("application_name", "a\\b"), ("application_name", "tab\there"), ("work_mem", ""), ("max_parallel_workers_per_gather", "-1")] {
            assert!(check_value(n, v).is_err(), "{n} {v}");
        }
        assert_eq!(check_value("work_mem", "64 megs").unwrap_err(), "work_mem: Expected a size like 64MB or 1GB");
    }

    #[test]
    fn a_name_outside_the_allow_list_is_refused_with_its_reason() {
        assert!(check_value("default_transaction_read_only", "off").unwrap_err().contains("read-only lock"));
        assert!(check_value("search_path", "x").is_err());
        assert!(check_value("work_mem; DROP TABLE t", "1").is_err());
        assert!(check_value("wal_level", "minimal").unwrap_err().contains("not a setting Quarry offers"));
    }

    #[test]
    fn a_value_is_written_as_one_quoted_literal() {
        assert_eq!(literal("it's"), "'it''s'");
        let adv = Advanced { pg_settings: vec![("application_name".into(), "o'brien; DROP TABLE x".into())], ..Default::default() };
        let s = PgSession::new(&adv, None).unwrap();
        assert_eq!(s.locals(false), "SET LOCAL application_name = 'o''brien; DROP TABLE x'");
    }

    #[test]
    fn the_session_lists_zone_path_and_rows_in_order() {
        let adv = Advanced {
            zone: Zone::System,
            search_path: vec!["reporting".into(), "public".into(), "reporting".into(), "we\"ird".into()],
            pg_settings: vec![("WORK_MEM".into(), " 64MB ".into()), ("statement_timeout".into(), "30s".into())],
            ..Default::default()
        };
        let s = PgSession::new(&adv, Some("Europe/Paris".into())).unwrap();
        assert_eq!(s.settings[0], ("TimeZone".into(), "Europe/Paris".into()));
        assert_eq!(s.settings[1], ("search_path".into(), "\"reporting\", \"public\", \"we\"\"ird\"".into()));
        assert_eq!(s.settings[2], ("work_mem".into(), "64MB".into()));
        assert_eq!(s.tail, ["reporting", "public", "we\"ird"]);
        assert_eq!(s.locals(false), "SET LOCAL TimeZone = 'Europe/Paris'; SET LOCAL search_path = \"reporting\", \"public\", \"we\"\"ird\"; SET LOCAL work_mem = '64MB'; SET LOCAL statement_timeout = '30s'");
        assert_eq!(s.locals(true), "SET LOCAL TimeZone = 'Europe/Paris'; SET LOCAL work_mem = '64MB'");
        assert!(PgSession::new(&Advanced { zone: Zone::System, ..Default::default() }, None).is_err());
        assert_eq!(PgSession::new(&Advanced::default(), None).unwrap(), PgSession::default());
        let d = PgSession::new(&Advanced::default(), Some("Asia/Kolkata".into())).unwrap();
        assert_eq!(d.settings, [("TimeZone".to_string(), "Asia/Kolkata".to_string())]);
        assert_eq!(PgSession::new(&Advanced::default(), Some("bad zone;".into())).unwrap(), PgSession::default());
        assert_eq!(PgSession::new(&Advanced { zone: Zone::Utc, ..Default::default() }, Some("Asia/Kolkata".into())).unwrap().settings[0].1, "UTC");
    }

    #[test]
    fn pg_catalog_is_never_listed_so_the_server_keeps_searching_it_first() {
        let s = PgSession::new(&Advanced { search_path: vec!["evil".into(), "public".into()], ..Default::default() }, None).unwrap();
        for tab in ["", "evil", "sales", "pg_catalog", "PG_CATALOG"] {
            assert!(!s.path(tab).to_lowercase().contains("pg_"), "{tab}: {}", s.path(tab));
        }
        assert_eq!(s.path("pg_catalog"), "\"evil\", \"public\"");
        assert_eq!(PgSession::default().path("pg_catalog"), "");
    }

    #[test]
    fn a_schema_is_stored_trimmed() {
        let s = PgSession::new(&Advanced { search_path: vec![" sales ".into(), "sales".into(), "public ".into()], ..Default::default() }, None).unwrap();
        assert_eq!(s.tail, ["sales", "public"]);
        assert_eq!(s.path("sales"), "\"sales\", \"public\"");
    }

    #[test]
    fn a_custom_offset_is_sent_the_way_postgres_reads_it_east_of_greenwich() {
        let zone = |z: &str| PgSession::new(&Advanced { zone: Zone::Custom(z.into()), ..Default::default() }, None).unwrap().settings[0].1.clone();
        assert_eq!(zone("+02:00"), "<+0200>-02:00");
        assert_eq!(zone("-03:30"), "<-0330>+03:30");
        assert_eq!(zone("+0530"), "<+0530>-05:30");
        assert_eq!(zone("+2"), "<+0200>-02:00");
        assert_eq!(zone(" +02:00 "), "<+0200>-02:00");
        assert_eq!(zone("Europe/Paris"), "Europe/Paris");
        assert_eq!(zone("UTC"), "UTC");
    }

    #[test]
    fn what_decides_how_values_are_read_is_kept_not_offered() {
        for k in ["extra_float_digits", "lc_monetary"] {
            assert!(def(k).is_none() && kept_reason(k) == Some("how values are read and edited"), "{k}");
            assert!(check_value(k, "C").unwrap_err().contains("kept by Quarry"), "{k}");
            assert!(PgSession::new(&Advanced { pg_settings: vec![(k.into(), "1".into())], ..Default::default() }, None).is_err(), "{k}");
        }
        assert!(def("lc_messages").is_some() && def("lc_numeric").is_some());
    }

    #[test]
    fn the_sidebar_schema_comes_first_in_the_path() {
        let s = PgSession::new(&Advanced { search_path: vec!["reporting".into(), "public".into()], ..Default::default() }, None).unwrap();
        assert_eq!(s.path("sales"), "\"sales\", \"reporting\", \"public\"");
        assert_eq!(s.path("public"), "\"public\", \"reporting\"");
        assert_eq!(s.path(""), "\"reporting\", \"public\"");
        let none = PgSession::default();
        assert_eq!(none.path("sales"), "\"sales\"");
        assert_eq!(none.path(""), "");
    }

    #[test]
    fn a_kept_setting_cannot_be_smuggled_in_by_name_or_by_value() {
        for name in ["role", " Role ", "ROLE", "SEARCH_PATH", "search_path", "DateStyle", "datestyle", "default_transaction_read_only", "work_mem, role", "work_mem\u{0}", "\"work_mem\""] {
            let adv = Advanced { pg_settings: vec![(name.into(), "x".into())], ..Default::default() };
            assert!(PgSession::new(&adv, None).is_err(), "{name:?}");
        }
        // A value that carries another setting or breaks out of its literal is refused or stays one quoted literal.
        for (name, v) in [("lc_time", "C, role=postgres"), ("lc_time", "C';SET role=x;--"), ("default_text_search_config", "a,b"), ("work_mem", "1MB; SET role = x"), ("application_name", "a\\'; SET default_transaction_read_only = off; --")] {
            let adv = Advanced { pg_settings: vec![(name.into(), v.into())], ..Default::default() };
            match PgSession::new(&adv, None) {
                Err(_) => {}
                Ok(s) => assert_eq!(s.locals(false), format!("SET LOCAL {name} = {}", literal(v)), "{name} {v}"),
            }
        }
        let ok = PgSession::new(&Advanced { pg_settings: vec![("application_name".into(), "a'; SET role = x; --".into())], ..Default::default() }, None).unwrap();
        assert_eq!(ok.locals(false), "SET LOCAL application_name = 'a''; SET role = x; --'");
    }

    #[test]
    fn refusals() {
        let bad = |a: Advanced| PgSession::new(&a, None).unwrap_err();
        assert!(bad(Advanced { search_path: vec!["pg_temp".into()], ..Default::default() }).contains("pg_temp"));
        for s in ["pg_catalog", "PG_Catalog", " pg_catalog ", "pg_toast", "pg_temp_3", "pg_whatever"] {
            assert!(bad(Advanced { search_path: vec!["public".into(), s.into()], ..Default::default() }).contains("system schema"), "{s}");
        }
        assert!(bad(Advanced { search_path: vec!["".into()], ..Default::default() }).contains("search_path"));
        assert!(bad(Advanced { zone: Zone::Custom("a'b".into()), ..Default::default() }).contains("Time zone"));
        assert!(bad(Advanced { pg_settings: vec![("work_mem".into(), "1MB".into()), ("work_mem".into(), "2MB".into())], ..Default::default() }).contains("twice"));
        assert!(bad(Advanced { pg_settings: vec![("role".into(), "postgres".into())], ..Default::default() }).contains("kept by Quarry"));
        assert!(bad(Advanced { connect_secs: Some(0), ..Default::default() }).contains("Connect"));
    }
}
