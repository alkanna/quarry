//! SQL engines (MySQL, MariaDB and PostgreSQL). Engine differences live in enums here, not traits:
//! a new engine is a new arm.

mod dialect;
pub mod advanced;
pub mod pg_advanced;
#[cfg(test)]
mod pg_advanced_tests;
mod worker;
mod run;
pub mod pg;
mod pg_catalog;
mod pg_read;
#[cfg(test)]
mod pg_read_tests;
#[cfg(test)]
mod pg_shell_tests;
#[cfg(test)]
mod pg_write_tests;
#[cfg(test)]
mod pg_import_tests;
#[cfg(test)]
mod pg_pgbouncer_tests;
#[cfg(test)]
mod pg_ddl_tests;
pub mod exec;
mod relay;
pub mod structure;
pub mod write;
pub mod ddl;
mod ddl_my;
mod ddl_pg;
pub mod ddl_pg_trig;
pub mod create;
pub mod create_pg;
pub mod export;
pub mod lex;
pub mod format;
pub mod value;
pub mod csv;
pub mod import;
mod import_pg;
pub mod dump;
mod pg_script;
mod pg_dump;
#[cfg(test)]
mod pg_dump_tests;
pub mod server;
#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod review_final;
pub mod rules;
pub mod schema;
pub mod users;
pub use advanced::{Advanced, Charset, TestInfo, Zone};
pub use dialect::{Dialect, Feature, FUNCTIONS, KEYWORDS};
pub use structure::Structure;
pub use pg_read::PgSchema;
pub use relay::{Answer, Arg, Relay};
pub use run::RunHandle;

/// A server error worded for `db::explain`: the SQLSTATE, the message, the detail and hint when the server sends
/// them, and where in the user's statement it points. `shift` is how many characters were put in front of the
/// statement. Anything that is not a PostgreSQL error, MySQL included, keeps the plain wording. This is the one wording
/// for every call site; `se` is it without a statement to point into.
pub(crate) fn se_at(err: &sqlx::Error, stmt: &str, shift: usize) -> String {
    let Some(d) = err.as_database_error().and_then(|d| d.try_downcast_ref::<sqlx::postgres::PgDatabaseError>()) else { return crate::db::e(err) };
    let mut out = format!("error returned from database: {}: {}", d.code(), d.message());
    if let Some(x) = d.detail() {
        out.push_str(&format!("\nDetail: {x}"));
    }
    if let Some(x) = d.hint() {
        out.push_str(&format!("\nHint: {x}"));
    }
    if let Some(sqlx::postgres::PgErrorPosition::Original(p)) = d.position() {
        // The server counts characters from 1.
        if p > shift {
            let at = p - shift;
            let (line, col) = stmt.chars().take(at - 1).fold((1, 1), |(l, c), ch| if ch == '\n' { (l + 1, 1) } else { (l, c + 1) });
            out.push_str(&format!("\nPosition: {at} (line {line}, column {col})"));
        }
    }
    out
}

/// `se_at` for an error that has no statement to point into, so it carries no position.
pub(crate) fn se(err: sqlx::Error) -> String {
    se_at(&err, "", usize::MAX)
}

/// Which kind of server a saved connection talks to. Saved connections from before SQL support
/// have no engine field and load as Mongo.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Engine {
    #[default]
    Mongo,
    MySql,
    Postgres,
}

impl Engine {
    pub fn from_uri(uri: &str) -> Option<Engine> {
        let scheme = uri.split("://").next()?.to_ascii_lowercase();
        match scheme.as_str() {
            "mongodb" | "mongodb+srv" => Some(Engine::Mongo),
            "mysql" | "mariadb" => Some(Engine::MySql),
            "postgres" | "postgresql" => Some(Engine::Postgres),
            _ => None,
        }
    }

    pub fn is_sql(self) -> bool {
        self != Engine::Mongo
    }
}

/// What the Content bar asks of one page: the clauses of the SELECT as the user wrote them. Each
/// text is spliced on a line of its own, so a trailing `-- comment` cannot swallow the next clause.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct PageQuery {
    pub filter: String,
    pub order: String,
    /// The select list entries; empty means `*`.
    pub select: Vec<String>,
    pub distinct: bool,
    pub group: Vec<String>,
    pub having: String,
    /// Most rows the whole result may have; pages stop there.
    pub limit: Option<u64>,
    /// Rows skipped before the first page (the OFFSET chip).
    pub skip: u64,
    /// Index names for `USE INDEX`, comma separated.
    pub index_hint: String,
}

impl PageQuery {
    pub fn grouped(&self) -> bool {
        !self.group.is_empty()
    }

    /// `USE INDEX (...)` with every name quoted, so the text cannot carry anything else.
    fn hint(&self, d: Dialect) -> String {
        // PostgreSQL has no index hints.
        if d == Dialect::Postgres {
            return String::new();
        }
        let names: Vec<String> = self.index_hint.split(',').map(str::trim).filter(|n| !n.is_empty()).map(|n| d.quote_ident(n.trim_matches('`'))).collect();
        if names.is_empty() { String::new() } else { format!(" USE INDEX ({})", names.join(", ")) }
    }

    fn group_list(&self) -> String {
        self.group.iter().map(|g| g.trim()).filter(|g| !g.is_empty()).collect::<Vec<_>>().join(", ")
    }

    fn select_list(&self) -> String {
        let list: Vec<&str> = self.select.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        if !list.is_empty() {
            list.join(",\n  ")
        } else if self.grouped() {
            self.group_list()
        } else {
            "*".into()
        }
    }

    /// Everything up to the paging: SELECT, FROM, WHERE, GROUP BY and HAVING. `cols` is false for the count.
    fn head(&self, d: Dialect, schema: &str, table: &str) -> String {
        let mut sql = format!("SELECT {}{}\nFROM {}{}", if self.distinct { "DISTINCT " } else { "" }, self.select_list(), d.qualified(schema, table), self.hint(d));
        if !self.filter.trim().is_empty() {
            sql.push_str(&format!("\nWHERE (\n{}\n)", self.filter.trim()));
        }
        if self.grouped() {
            sql.push_str(&format!("\nGROUP BY {}", self.group_list()));
            if !self.having.trim().is_empty() {
                sql.push_str(&format!("\nHAVING (\n{}\n)", self.having.trim()));
            }
        }
        sql
    }

    /// The statement for one page, or for Copy as SQL when `page` is None (then only the LIMIT chip
    /// applies). `pk` is the tiebreak that keeps pages from overlapping or skipping rows.
    pub fn select_sql(&self, d: Dialect, schema: &str, table: &str, pk: &[String], page: Option<(u64, u64)>) -> String {
        let mut sql = self.head(d, schema, table);
        // DISTINCT with a select list cannot order by a column outside the list.
        let tie = if self.grouped() { self.group_list() } else if self.distinct && !self.select.is_empty() { String::new() } else { pk.iter().map(|p| d.quote_ident(p)).collect::<Vec<_>>().join(", ") };
        if !self.order.trim().is_empty() {
            sql.push_str(&format!("\nORDER BY {}", self.order.trim()));
            if !tie.is_empty() {
                sql.push_str(&format!("\n, {tie}"));
            }
        } else if !tie.is_empty() {
            sql.push_str(&format!("\nORDER BY {tie}"));
        }
        match page {
            Some((size, offset)) => {
                let start = self.skip + offset;
                let size = self.limit.map_or(size, |l| size.min(l.saturating_sub(offset)));
                sql.push_str(&format!("\n{}", d.page(size, start)));
            }
            None => {
                if let Some(l) = self.limit {
                    sql.push_str(&format!("\nLIMIT {l}{}", if self.skip > 0 { format!(" OFFSET {}", self.skip) } else { String::new() }));
                } else if self.skip > 0 {
                    sql.push_str(&format!("\nLIMIT {} OFFSET {}", d.no_limit(), self.skip));
                }
            }
        }
        sql
    }

    /// Counts what the pages come from. Groups are counted by wrapping the grouped SELECT.
    pub fn count_sql(&self, d: Dialect, schema: &str, table: &str) -> String {
        if self.grouped() || self.distinct {
            return format!("SELECT COUNT(*) FROM (\n{}\n) AS q", self.head(d, schema, table));
        }
        let mut sql = format!("SELECT COUNT(*) FROM {}{}", d.qualified(schema, table), self.hint(d));
        if !self.filter.trim().is_empty() {
            sql.push_str(&format!("\nWHERE (\n{}\n)", self.filter.trim()));
        }
        sql
    }

    /// Rows that match the filter before grouping.
    pub fn rows_sql(&self, d: Dialect, schema: &str, table: &str) -> String {
        PageQuery { filter: self.filter.clone(), ..Default::default() }.count_sql(d, schema, table)
    }

    /// Whether the table's row estimate can stand in for the count.
    pub fn plain(&self) -> bool {
        self.filter.trim().is_empty() && !self.distinct && !self.grouped() && self.limit.is_none() && self.skip == 0
    }
}

/// A connected SQL server. Cloning shares the pool.
#[derive(Clone)]
pub struct Db {
    pub pool: Pool,
    pub dialect: Dialect,
    /// How the pool connects, so Stop can open a connection of its own when the pool is busy.
    pub opts: exec::Opts,
    /// The server's `VERSION()`, such as "8.4.3" or "10.11.6-MariaDB".
    pub version: String,
    /// Whether the session is encrypted, which `ssl-mode=PREFERRED` leaves to the server.
    pub tls: bool,
    /// `@@sql_mode`, which changes how quotes and backslashes lex (ANSI_QUOTES, NO_BACKSLASH_ESCAPES).
    pub sql_mode: String,
    /// The Advanced tab's session setup, for connections opened outside the pool.
    pub(crate) setup: worker::Setup,
    /// The app's id of this connection, which tags the statements Quarry sends in the query log.
    pub conn: u64,
}

#[derive(Clone)]
pub enum Pool {
    My(sqlx::MySqlPool),
    /// One pool per database, made on first use, because a Postgres connection belongs to one database.
    Pg(std::sync::Arc<pg::PgPools>),
}

impl Db {
    /// Puts a statement that was just sent in the query log, with how long it took and how it ended. Takes the
    /// result by reference so the caller still has it to return.
    pub(crate) fn note<T, X: std::fmt::Display>(&self, schema: &str, sql: &str, start: std::time::Instant, res: &Result<T, X>) {
        let err = res.as_ref().err().map(|x| x.to_string());
        crate::db::log_sql(self.conn, schema, sql, start.elapsed().as_millis(), err.as_deref());
    }

    /// False under NO_BACKSLASH_ESCAPES, where a backslash in a string is a plain character.
    pub fn backslash_escapes(&self) -> bool {
        // The connect readback refuses a Postgres session with standard_conforming_strings off.
        self.dialect != Dialect::Postgres && !self.sql_mode.to_ascii_uppercase().contains("NO_BACKSLASH_ESCAPES")
    }

    /// "MySQL 8.4.3" or "MariaDB 10.11.6", without the build suffix of the version string.
    pub fn label(&self) -> String {
        let num = self.version.split('-').next().unwrap_or_default();
        format!("{} {num}", match self.dialect { Dialect::MySql => "MySQL", Dialect::MariaDb => "MariaDB", Dialect::Postgres => "PostgreSQL" })
    }
}

/// One result column. `ty` is the type name the server reported, such as "INT UNSIGNED".
#[derive(Clone, Debug)]
pub struct Col {
    /// The key in a row document. Unique within the result, so a column named `_id` or a repeated
    /// name is renamed here.
    pub name: String,
    /// The column's own name on the server, which every statement written back must use.
    pub src: String,
    pub ty: String,
    pub kind: value::Kind,
    pub nullable: bool,
    /// A generated column cannot be written, so inserts leave it out.
    pub generated: bool,
    pub auto_increment: bool,
}

impl Col {
    /// A column that is its own source name and has no table flags, for computed results.
    pub fn plain(name: &str, ty: &str, kind: value::Kind, nullable: bool) -> Col {
        Col { name: name.into(), src: name.into(), ty: ty.into(), kind, nullable, generated: false, auto_increment: false }
    }
}

/// The outcome of one statement of a Custom Query run. An `error` means the statement did not run
/// or failed; `affected` is set for statements that return no rows.
#[derive(Clone)]
pub struct ResultSet {
    pub stmt: String,
    pub cols: Vec<Col>,
    pub rows: Vec<crate::db::Row>,
    pub affected: Option<u64>,
    pub error: Option<String>,
    pub ms: u128,
    /// A statement that cannot be paged (SHOW, CALL) returned more than `RUN_ROWS_MAX` rows; the rest were dropped.
    pub capped: bool,
    /// Set for a SELECT read a page at a time.
    pub page: Option<Page>,
    /// The first keyword of `stmt` in capitals ("SELECT"), for the result tab.
    pub kind: String,
    /// The read-only check refused the statement, so it never reached the server. `error` holds the words.
    pub refused: bool,
    /// The longest cell per column among the first rows, in characters, read once by the grid for its column widths.
    pub widest: std::cell::OnceCell<Vec<usize>>,
}

impl ResultSet {
    /// An empty outcome of `stmt`.
    pub fn new(stmt: &str) -> ResultSet {
        let kind = lex::tokens(stmt, Dialect::MySql).into_iter().find(|(t, _)| *t == lex::Tok::Word).map(|(_, r)| stmt[r].to_ascii_uppercase()).unwrap_or_default();
        ResultSet { stmt: stmt.into(), cols: Vec::new(), rows: Vec::new(), affected: None, error: None, ms: 0, capped: false, page: None, kind, refused: false, widest: Default::default() }
    }
}

/// Where a paged result stands: the page shown, whether another follows, and the whole row count once known.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Page {
    pub n: usize,
    pub next: bool,
    pub total: Option<u64>,
    /// The statement has an ORDER BY, so rows keep their place between pages.
    pub ordered: bool,
}

/// Rows of one Custom Query page, and the most rows an unpageable statement keeps.
pub const RUN_ROWS_MAX: usize = 1000;

/// How hard the client insists on TLS, named like the mysql client's --ssl-mode.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum SslMode {
    Disabled,
    #[default]
    Preferred,
    Required,
    VerifyCa,
    VerifyIdentity,
}

impl SslMode {
    pub const ALL: [(SslMode, &'static str); 5] = [
        (SslMode::Disabled, "Off"),
        (SslMode::Preferred, "Preferred"),
        (SslMode::Required, "Required"),
        (SslMode::VerifyCa, "Verify CA"),
        (SslMode::VerifyIdentity, "Verify identity"),
    ];

    /// The same modes under their libpq `sslmode` names, for the PostgreSQL form. Built from `pg_name`, the one place the
    /// names are written.
    pub const PG_ALL: [(SslMode, &'static str); 5] = [
        (SslMode::Disabled, SslMode::Disabled.pg_name()),
        (SslMode::Preferred, SslMode::Preferred.pg_name()),
        (SslMode::Required, SslMode::Required.pg_name()),
        (SslMode::VerifyCa, SslMode::VerifyCa.pg_name()),
        (SslMode::VerifyIdentity, SslMode::VerifyIdentity.pg_name()),
    ];

    /// The libpq name of this mode, as `sslmode` spells it.
    pub const fn pg_name(self) -> &'static str {
        match self {
            SslMode::Disabled => "disable",
            SslMode::Preferred => "prefer",
            SslMode::Required => "require",
            SslMode::VerifyCa => "verify-ca",
            SslMode::VerifyIdentity => "verify-full",
        }
    }

    fn param(self) -> &'static str {
        match self {
            SslMode::Disabled => "DISABLED",
            SslMode::Preferred => "PREFERRED",
            SslMode::Required => "REQUIRED",
            SslMode::VerifyCa => "VERIFY_CA",
            SslMode::VerifyIdentity => "VERIFY_IDENTITY",
        }
    }

    fn from_param(s: &str) -> Option<SslMode> {
        SslMode::ALL.iter().map(|(m, _)| *m).find(|m| m.param().eq_ignore_ascii_case(s))
    }

    /// `allow` tries plain first, which Quarry has no mode for, so it reads as Preferred.
    fn from_pg_param(s: &str) -> Option<SslMode> {
        if s.eq_ignore_ascii_case("allow") {
            return Some(SslMode::Preferred);
        }
        SslMode::PG_ALL.iter().find(|(_, n)| n.eq_ignore_ascii_case(s)).map(|(m, _)| *m)
    }
}

/// A SQL address split into the fields the connection form edits. It is kept in the Keychain as
/// one URL, like Mongo addresses, so the password is percent-encoded there.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Addr {
    /// "mysql" or "mariadb" (the same driver, the scheme only records the user's pick), or "postgres".
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: String,
    pub ssl_mode: SslMode,
    pub ssl_ca: String,
}

pub const DEFAULT_PORT: u16 = 3306;
pub const PG_PORT: u16 = 5432;

impl Addr {
    pub fn engine(&self) -> Engine {
        Engine::from_uri(&format!("{}://", self.scheme)).unwrap_or(Engine::MySql)
    }

    pub fn default_port(scheme: &str) -> u16 {
        if matches!(Engine::from_uri(&format!("{scheme}://")), Some(Engine::Postgres)) { PG_PORT } else { DEFAULT_PORT }
    }

    pub fn parse(uri: &str) -> Option<Addr> {
        let (scheme, rest) = uri.trim().split_once("://")?;
        Engine::from_uri(uri).filter(|e| e.is_sql())?;
        let (main, query) = rest.split_once('?').unwrap_or((rest, ""));
        let (auth_host, database) = main.split_once('/').unwrap_or((main, ""));
        // The password may hold an @ once decoded, never encoded; the last @ ends the credentials.
        let (auth, host) = auth_host.rsplit_once('@').unwrap_or(("", auth_host));
        let (user, password) = auth.split_once(':').unwrap_or((auth, ""));
        // A bare IPv6 address has colons of its own; it needs brackets to carry a port.
        let (host, port) = match host.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') || h.ends_with(']') => (h, p.parse().ok()?),
            _ => (host, Addr::default_port(scheme)),
        };
        let mut a = Addr { scheme: scheme.to_ascii_lowercase(), host: host.into(), port, user: decode(user), password: decode(password), database: decode(database), ..Default::default() };
        let pg = a.engine() == Engine::Postgres;
        for kv in query.split('&').filter(|s| !s.is_empty()) {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            let k = k.to_ascii_lowercase();
            if pg {
                match k.as_str() {
                    "sslmode" => a.ssl_mode = SslMode::from_pg_param(v).unwrap_or_default(),
                    "sslrootcert" => a.ssl_ca = decode(v),
                    _ => {}
                }
                continue;
            }
            match k.replace('_', "-").as_str() {
                "ssl-mode" | "sslmode" => a.ssl_mode = SslMode::from_param(v).unwrap_or_default(),
                "ssl-ca" | "sslca" => a.ssl_ca = decode(v),
                _ => {}
            }
        }
        Some(a)
    }

    /// Reads what a user pastes into the form: a `postgresql://` or `postgres://` URI, or a libpq
    /// `key=value` string. The error text never repeats the input, which may hold a password.
    pub fn from_paste(text: &str) -> Result<Addr, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("Nothing to read".into());
        }
        let a = if let Some((scheme, _)) = text.split_once("://") {
            if !matches!(scheme.to_ascii_lowercase().as_str(), "postgres" | "postgresql") {
                return Err("Only postgresql:// and postgres:// strings can be pasted here".into());
            }
            Addr::parse(text).ok_or("Could not read this connection string, check the port")?
        } else if text.contains('=') {
            Addr::from_keywords(text)?
        } else {
            return Err("Paste a postgresql:// address or a host=... connection string".into());
        };
        if a.host.trim().is_empty() {
            return Err("The connection string has no host".into());
        }
        Ok(Addr { scheme: "postgres".into(), ..a })
    }

    /// The libpq `key=value` form. Values may be single quoted with backslash escapes. An error names the position of the
    /// item (the 1st, 2nd and so on) and never repeats any of the text, because a value may be the password.
    fn from_keywords(text: &str) -> Result<Addr, String> {
        let mut a = Addr { scheme: "postgres".into(), port: PG_PORT, ..Default::default() };
        let mut chars = text.chars().peekable();
        let mut item = 0;
        loop {
            item += 1;
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }
            if chars.peek().is_none() {
                break;
            }
            let mut key = String::new();
            while let Some(&c) = chars.peek() {
                if c == '=' || c.is_whitespace() {
                    break;
                }
                key.push(c);
                chars.next();
            }
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }
            if chars.next() != Some('=') {
                return Err(format!("Expected key=value in item {item} of the connection string"));
            }
            while chars.peek().is_some_and(|c| c.is_whitespace()) {
                chars.next();
            }
            let mut val = String::new();
            if chars.peek() == Some(&'\'') {
                chars.next();
                loop {
                    match chars.next() {
                        Some('\\') => val.extend(chars.next()),
                        Some('\'') => break,
                        Some(c) => val.push(c),
                        None => return Err(format!("The quoted value in item {item} of the connection string is not closed")),
                    }
                }
            } else {
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() {
                        break;
                    }
                    chars.next();
                    if c == '\\' {
                        val.extend(chars.next());
                    } else {
                        val.push(c);
                    }
                }
            }
            match key.to_ascii_lowercase().as_str() {
                "host" | "hostaddr" => {
                    // A host list is a failover list; the first entry is the one a form can show.
                    let first = val.split(',').next().unwrap_or_default();
                    a.host = if first.contains(':') && !first.starts_with('[') { format!("[{first}]") } else { first.into() };
                }
                "port" => a.port = val.split(',').next().unwrap_or_default().trim().parse().map_err(|_| "The port is not a number".to_string())?,
                "user" => a.user = val,
                "password" => a.password = val,
                "dbname" => a.database = val,
                "sslmode" => a.ssl_mode = SslMode::from_pg_param(&val).ok_or_else(|| format!("Unknown sslmode in item {item} of the connection string"))?,
                "sslrootcert" => a.ssl_ca = val,
                _ => {}
            }
        }
        Ok(a)
    }

    pub fn to_uri(&self) -> String {
        let scheme = if self.scheme.is_empty() { "mysql" } else { &self.scheme };
        let auth = match (self.user.is_empty(), self.password.is_empty()) {
            (true, true) => String::new(),
            (_, true) => format!("{}@", encode(&self.user)),
            _ => format!("{}:{}@", encode(&self.user), encode(&self.password)),
        };
        let port = if self.port == 0 { Addr::default_port(scheme) } else { self.port };
        let pg = self.engine() == Engine::Postgres;
        let mut q = Vec::new();
        if self.ssl_mode != SslMode::Preferred {
            q.push(if pg { format!("sslmode={}", self.ssl_mode.pg_name()) } else { format!("ssl-mode={}", self.ssl_mode.param()) });
        }
        if !self.ssl_ca.is_empty() {
            q.push(format!("{}={}", if pg { "sslrootcert" } else { "ssl-ca" }, encode(&self.ssl_ca)));
        }
        let q = if q.is_empty() { String::new() } else { format!("?{}", q.join("&")) };
        format!("{scheme}://{auth}{}:{port}/{}{q}", self.host.trim(), encode(&self.database))
    }
}

fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = (b[i] == b'%' && i + 2 < b.len()).then(|| std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok())).flatten();
        match hex {
            Some(v) => {
                out.push(v);
                i += 3;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_comes_from_the_scheme() {
        assert_eq!(Engine::from_uri("mongodb+srv://x"), Some(Engine::Mongo));
        assert_eq!(Engine::from_uri("MySQL://x"), Some(Engine::MySql));
        assert_eq!(Engine::from_uri("mariadb://x"), Some(Engine::MySql));
        assert_eq!(Engine::from_uri("postgres://x"), Some(Engine::Postgres));
        assert_eq!(Engine::from_uri("PostgreSQL://x"), Some(Engine::Postgres));
        assert_eq!(Engine::from_uri("sqlite://x"), None);
    }

    #[test]
    fn address_round_trips_through_the_url() {
        let a = Addr {
            scheme: "mariadb".into(),
            host: "db.internal".into(),
            port: 3307,
            user: "app".into(),
            password: "p@ss:w/rd%".into(),
            database: "shop".into(),
            ssl_mode: SslMode::VerifyCa,
            ssl_ca: "/certs/ca.pem".into(),
        };
        let uri = a.to_uri();
        assert_eq!(uri, "mariadb://app:p%40ss%3Aw%2Frd%25@db.internal:3307/shop?ssl-mode=VERIFY_CA&ssl-ca=%2Fcerts%2Fca.pem");
        assert_eq!(Addr::parse(&uri), Some(a));
    }

    #[test]
    fn postgres_address_uses_libpq_spellings_and_its_own_port() {
        let a = Addr::parse("postgresql://app:pw@db.internal/shop?sslmode=verify-full&sslrootcert=%2Fcerts%2Fca.pem").unwrap();
        assert_eq!((a.scheme.as_str(), a.port, a.ssl_mode, a.ssl_ca.as_str(), a.database.as_str()), ("postgresql", 5432, SslMode::VerifyIdentity, "/certs/ca.pem", "shop"));
        assert_eq!(a.engine(), Engine::Postgres);
        assert_eq!(a.to_uri(), "postgresql://app:pw@db.internal:5432/shop?sslmode=verify-full&sslrootcert=%2Fcerts%2Fca.pem");
        assert_eq!(Addr::parse(&a.to_uri()), Some(a));
        // allow tries plain first, which has no mode of its own here.
        assert_eq!(Addr::parse("postgres://h/d?sslmode=allow").unwrap().ssl_mode, SslMode::Preferred);
        for (word, mode) in [("disable", SslMode::Disabled), ("prefer", SslMode::Preferred), ("require", SslMode::Required), ("verify-ca", SslMode::VerifyCa), ("verify-full", SslMode::VerifyIdentity)] {
            let a = Addr { scheme: "postgres".into(), host: "h".into(), port: 6000, ssl_mode: mode, ..Default::default() };
            assert_eq!(Addr::parse(&a.to_uri()), Some(a.clone()), "{word}");
            assert_eq!(Addr::parse(&format!("postgres://h:6000/?sslmode={word}")).unwrap().ssl_mode, mode);
        }
        // The default port follows the scheme and a zero port is written out as it.
        assert_eq!(Addr::parse("postgres://u@h").unwrap().port, 5432);
        assert_eq!(Addr::parse("mysql://u@h").unwrap().port, 3306);
        assert_eq!(Addr { scheme: "postgres".into(), host: "h".into(), ..Default::default() }.to_uri(), "postgres://h:5432/");
        // The MySQL spelling means nothing on a Postgres address, and the reverse.
        assert_eq!(Addr::parse("postgres://h/?ssl-mode=REQUIRED").unwrap().ssl_mode, SslMode::Preferred);
        assert_eq!(Addr::parse("mysql://h/?sslmode=REQUIRED").unwrap().ssl_mode, SslMode::Required);
    }

    #[test]
    fn paste_reads_uris() {
        let a = Addr::from_paste("  postgresql://analyst:p%40ss%3Aw%2Frd@analytics.internal:6543/my%20db?sslmode=verify-full&sslrootcert=%2Fcerts%2Fca.pem ").unwrap();
        assert_eq!((a.scheme.as_str(), a.host.as_str(), a.port, a.user.as_str(), a.password.as_str(), a.database.as_str(), a.ssl_mode, a.ssl_ca.as_str()), ("postgres", "analytics.internal", 6543, "analyst", "p@ss:w/rd", "my db", SslMode::VerifyIdentity, "/certs/ca.pem"));
        // No port means the Postgres one, and no query means the default mode.
        let a = Addr::from_paste("postgres://u@h/d").unwrap();
        assert_eq!((a.port, a.ssl_mode, a.password.as_str()), (5432, SslMode::Preferred, ""));
        let a = Addr::from_paste("postgres://u:pw@[2001:db8::1]:5433/d").unwrap();
        assert_eq!((a.host.as_str(), a.port), ("[2001:db8::1]", 5433));
        assert_eq!(Addr::from_paste("postgres://u@[::1]/d").unwrap().port, 5432);
        assert_eq!(Addr::from_paste("POSTGRESQL://h").unwrap().host, "h");
    }

    #[test]
    fn paste_reads_libpq_keywords() {
        let a = Addr::from_paste(r"host=db.internal port=5433 dbname=shop user=app password='it\'s a secret' sslmode=verify-ca sslrootcert=/certs/ca.pem").unwrap();
        assert_eq!((a.host.as_str(), a.port, a.database.as_str(), a.user.as_str(), a.password.as_str(), a.ssl_mode, a.ssl_ca.as_str()), ("db.internal", 5433, "shop", "app", "it's a secret", SslMode::VerifyCa, "/certs/ca.pem"));
        let a = Addr::from_paste("host = 'h' dbname = 'my db'").unwrap();
        assert_eq!((a.port, a.database.as_str(), a.scheme.as_str()), (5432, "my db", "postgres"));
        assert_eq!(Addr::from_paste("host=::1").unwrap().host, "[::1]");
        assert!(Addr::from_paste("host='open").is_err());
        assert!(Addr::from_paste("host=h port=x").is_err());
        assert!(Addr::from_paste("user=u").is_err());
    }

    #[test]
    fn paste_refuses_other_input_without_echoing_it() {
        for bad in ["mysql://u:secret@h/d", "mongodb://u:secret@h", "secret", "", "postgres://u:secret@h:notaport/d", "postgres:///d"] {
            let e = Addr::from_paste(bad).unwrap_err();
            assert!(!e.contains("secret"), "{e}");
        }
        assert!(Addr::from_paste("mysql://h").unwrap_err().contains("postgresql://"));
    }

    #[test]
    fn a_keyword_string_that_fails_never_echoes_what_was_typed() {
        // A password with a space is read as a password and a stray word, and each of these breaks at a different place.
        for bad in [
            "host=h password=my secret", "secret", "host=h secret", "host=h password='sec ret", "host=h sslmode=secret", "host=h port=secret", "password=secret host",
            "host=h password='sec\\'ret",
        ] {
            let e = Addr::from_paste(bad).unwrap_err();
            assert!(!e.contains("secret") && !e.contains("sec ret"), "{bad}: {e}");
        }
        assert!(Addr::from_paste("host=h password=my secret").unwrap_err().contains("item 3"));
    }

    #[test]
    fn the_libpq_names_come_from_one_mapping() {
        for (m, name) in SslMode::PG_ALL {
            assert_eq!(m.pg_name(), name);
            assert_eq!(SslMode::from_pg_param(name), Some(m));
            assert_eq!(Addr { scheme: "postgres".into(), host: "h".into(), ssl_mode: m, ..Default::default() }.to_uri().contains(&format!("sslmode={name}")), m != SslMode::Preferred);
        }
        assert_eq!(SslMode::from_pg_param("ALLOW"), Some(SslMode::Preferred));
        assert_eq!(SslMode::from_pg_param("nope"), None);
    }

    #[test]
    fn missing_parts_get_defaults() {
        let a = Addr::parse("mysql://root@localhost").unwrap();
        assert_eq!((a.host.as_str(), a.port, a.user.as_str(), a.database.as_str(), a.ssl_mode), ("localhost", 3306, "root", "", SslMode::Preferred));
        assert!(Addr::parse("mongodb://localhost").is_none());
        assert!(Addr::parse("mysql://h:notaport/").is_none());
    }
}

/// One column of a table, as the shell's editor types need it.
#[derive(Clone, PartialEq, Debug)]
pub struct TableCol {
    pub name: String,
    /// COLUMN_TYPE, such as "enum('a','b')" or "decimal(10,2)".
    pub full_type: String,
    pub nullable: bool,
}

#[derive(Clone, PartialEq, Debug)]
pub struct TableCols {
    pub name: String,
    pub cols: Vec<TableCol>,
}
