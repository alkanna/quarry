//! SQL engines (MySQL and MariaDB for now). Engine differences live in enums here, not traits:
//! a new engine is a new arm.

// The worker (step 4) and the views use these; until then they only have tests.
#[allow(dead_code)]
mod dialect;
#[allow(dead_code)]
pub mod lex;
#[allow(dead_code)]
pub mod value;
#[allow(unused_imports)]
pub use dialect::{Dialect, FUNCTIONS, KEYWORDS};

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
