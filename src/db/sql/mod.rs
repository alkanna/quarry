//! SQL engines (MySQL and MariaDB for now). Engine differences live in enums here, not traits:
//! a new engine is a new arm.

// The views (step 5 on) use parts of these; until then they only have tests.
#[allow(dead_code)]
mod dialect;
mod worker;
pub mod structure;
pub mod write;
#[allow(dead_code)]
pub mod export;
pub mod lex;
#[allow(dead_code)]
pub mod value;
pub mod rules;
pub mod schema;
#[allow(unused_imports)]
pub use dialect::{Dialect, FUNCTIONS, KEYWORDS};
pub use structure::Structure;
pub use worker::{Answer, Relay};

/// Which kind of server a saved connection talks to. Saved connections from before SQL support
/// have no engine field and load as Mongo.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Engine {
    #[default]
    Mongo,
    MySql,
}

impl Engine {
    pub fn from_uri(uri: &str) -> Option<Engine> {
        let scheme = uri.split("://").next()?.to_ascii_lowercase();
        match scheme.as_str() {
            "mongodb" | "mongodb+srv" => Some(Engine::Mongo),
            "mysql" | "mariadb" => Some(Engine::MySql),
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
                    sql.push_str(&format!("\nLIMIT 18446744073709551615 OFFSET {}", self.skip));
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
// Read by the views from step 5 on.
#[allow(dead_code)]
#[derive(Clone)]
pub struct Db {
    pub pool: Pool,
    pub dialect: Dialect,
    /// How the pool connects, so Stop can open a connection of its own when the pool is busy.
    pub opts: sqlx::mysql::MySqlConnectOptions,
    /// The server's `VERSION()`, such as "8.4.3" or "10.11.6-MariaDB".
    pub version: String,
    /// Whether the session is encrypted, which `ssl-mode=PREFERRED` leaves to the server.
    pub tls: bool,
    /// `@@sql_mode`, which changes how quotes and backslashes lex (ANSI_QUOTES, NO_BACKSLASH_ESCAPES).
    pub sql_mode: String,
}

#[derive(Clone)]
pub enum Pool {
    My(sqlx::MySqlPool),
}

impl Db {
    /// False under NO_BACKSLASH_ESCAPES, where a backslash in a string is a plain character.
    pub fn backslash_escapes(&self) -> bool {
        !self.sql_mode.to_ascii_uppercase().contains("NO_BACKSLASH_ESCAPES")
    }

    /// "MySQL 8.4.3" or "MariaDB 10.11.6", without the build suffix of the version string.
    pub fn label(&self) -> String {
        let num = self.version.split('-').next().unwrap_or_default();
        format!("{} {num}", if self.dialect == Dialect::MariaDb { "MariaDB" } else { "MySQL" })
    }
}

/// One result column. `ty` is the type name the server reported, such as "INT UNSIGNED".
#[allow(dead_code)]
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
#[allow(dead_code)]
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
#[allow(dead_code)]
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
}

/// A MySQL address split into the fields the connection form edits. It is kept in the Keychain as
/// one URL, like Mongo addresses, so the password is percent-encoded there.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Addr {
    /// "mysql" or "mariadb"; both reach the same driver, the scheme only records the user's pick.
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

impl Addr {
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
            _ => (host, DEFAULT_PORT),
        };
        let mut a = Addr { scheme: scheme.to_ascii_lowercase(), host: host.into(), port, user: decode(user), password: decode(password), database: decode(database), ..Default::default() };
        for kv in query.split('&').filter(|s| !s.is_empty()) {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            match k.to_ascii_lowercase().replace('_', "-").as_str() {
                "ssl-mode" | "sslmode" => a.ssl_mode = SslMode::from_param(v).unwrap_or_default(),
                "ssl-ca" | "sslca" => a.ssl_ca = decode(v),
                _ => {}
            }
        }
        Some(a)
    }

    pub fn to_uri(&self) -> String {
        let scheme = if self.scheme.is_empty() { "mysql" } else { &self.scheme };
        let auth = match (self.user.is_empty(), self.password.is_empty()) {
            (true, true) => String::new(),
            (_, true) => format!("{}@", encode(&self.user)),
            _ => format!("{}:{}@", encode(&self.user), encode(&self.password)),
        };
        let port = if self.port == 0 { DEFAULT_PORT } else { self.port };
        let mut q = Vec::new();
        if self.ssl_mode != SslMode::Preferred {
            q.push(format!("ssl-mode={}", self.ssl_mode.param()));
        }
        if !self.ssl_ca.is_empty() {
            q.push(format!("ssl-ca={}", encode(&self.ssl_ca)));
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
        assert_eq!(Engine::from_uri("postgres://x"), None);
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
