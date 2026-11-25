//! A MongoDB connection string as fields, for the connection form. The string stays the one stored value: the form parses
//! it every frame and writes edits back through the setters here. Everything the form has no field for, in its order and
//! spelling, is kept as written, so a string this module does not understand fully still prints back unchanged.

#[derive(Clone, Debug, PartialEq)]
pub struct MongoUri {
    pub srv: bool,
    /// User and password as written, percent-encoded. `user()` and `password()` read them decoded.
    user: Option<String>,
    password: Option<String>,
    /// Each host as written, `host:port` or a percent-encoded socket path. Empty entries are kept so a half-typed list
    /// does not lose its comma.
    pub hosts: Vec<String>,
    /// Whether a `/` follows the hosts, which `host/?tls=true` has and `host?tls=true` has not.
    slash: bool,
    db: String,
    /// The options after `?` in order, each key and its raw value; None without a `?`. Keys compare ignoring case, as the
    /// driver does.
    query: Option<Vec<(String, Option<String>)>>,
}

const SRV: &str = "mongodb+srv://";
const PLAIN: &str = "mongodb://";

/// The options that imply TLS when the driver sees them without `tls`.
const TLS_IMPLYING: [&str; 5] = ["tlsCAFile", "tlsCertificateKeyFile", "tlsAllowInvalidCertificates", "tlsAllowInvalidHostnames", "tlsInsecure"];
/// The options that skip certificate checks.
const TLS_INVALID: [&str; 3] = ["tlsInsecure", "tlsAllowInvalidCertificates", "tlsAllowInvalidHostnames"];

use super::pct::{decode, encode};

impl MongoUri {
    /// None when the text is not a `mongodb://` or `mongodb+srv://` string.
    pub fn parse(s: &str) -> Option<MongoUri> {
        let (srv, rest) = match (s.strip_prefix(SRV), s.strip_prefix(PLAIN)) {
            (Some(r), _) => (true, r),
            (None, Some(r)) => (false, r),
            _ => return None,
        };
        let (rest, query) = match rest.split_once('?') {
            Some((a, q)) => (a, Some(q.split('&').map(|p| match p.split_once('=') {
                Some((k, v)) => (k.to_string(), Some(v.to_string())),
                None => (p.to_string(), None),
            }).collect())),
            None => (rest, None),
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], Some(&rest[i + 1..])),
            None => (rest, None),
        };
        let (userinfo, hosts) = match authority.rfind('@') {
            Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
            None => (None, authority),
        };
        let (user, password) = match userinfo {
            Some(u) => match u.split_once(':') {
                Some((a, b)) => (Some(a.to_string()), Some(b.to_string())),
                None => (Some(u.to_string()), None),
            },
            None => (None, None),
        };
        Some(MongoUri {
            srv,
            user,
            password,
            hosts: if hosts.is_empty() { Vec::new() } else { hosts.split(',').map(str::to_string).collect() },
            slash: path.is_some(),
            db: path.unwrap_or_default().to_string(),
            query,
        })
    }

    pub fn user(&self) -> String {
        self.user.as_deref().map(decode).unwrap_or_default()
    }

    pub fn password(&self) -> String {
        self.password.as_deref().map(decode).unwrap_or_default()
    }

    pub fn database(&self) -> String {
        decode(&self.db)
    }

    pub fn hosts_text(&self) -> String {
        self.hosts.join(",")
    }

    pub fn set_user(&mut self, v: &str) {
        self.user = (!v.is_empty()).then(|| encode(v, ""));
    }

    pub fn set_password(&mut self, v: &str) {
        self.password = (!v.is_empty()).then(|| encode(v, ""));
    }

    pub fn set_database(&mut self, v: &str) {
        self.db = encode(v, "");
        self.slash |= !v.is_empty();
    }

    /// The hosts as typed, comma separated.
    pub fn set_hosts(&mut self, v: &str) {
        self.hosts = if v.is_empty() { Vec::new() } else { v.split(',').map(|h| h.trim().to_string()).collect() };
    }

    /// An SRV address names one host and has no port, so turning it on keeps the first host without its port, and drops
    /// directConnection, which SRV refuses. Turning it off keeps TLS on, which SRV implied and a plain address does not.
    pub fn set_srv(&mut self, on: bool) {
        if on && !self.srv {
            self.hosts.truncate(1);
            if let Some(h) = self.hosts.first_mut() && let Some((name, _)) = h.rsplit_once(':') && !h.ends_with(']') {
                *h = name.to_string();
            }
            self.set_opt("directConnection", None);
        }
        if !on && self.srv && !self.has_opt("tls") && !self.has_opt("ssl") {
            self.set_opt("tls", Some("true"));
        }
        self.srv = on;
    }

    /// Whether an option is present, whatever its value.
    pub fn has_opt(&self, key: &str) -> bool {
        self.query.as_ref().is_some_and(|q| q.iter().any(|(k, _)| k.eq_ignore_ascii_case(key)))
    }

    /// The decoded value of an option, matching the key ignoring case.
    pub fn opt(&self, key: &str) -> Option<String> {
        self.query.as_ref()?.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| decode(v.as_deref().unwrap_or("")))
    }

    /// Sets an option where it already is, or at the end; None removes it. A repeated key is an error for the driver, so
    /// any later copy goes too. Values keep `/` and `:` readable, which file paths have.
    pub fn set_opt(&mut self, key: &str, value: Option<&str>) {
        let mut found = false;
        if let Some(q) = self.query.as_mut() {
            q.retain_mut(|(k, v)| {
                if !k.eq_ignore_ascii_case(key) {
                    return true;
                }
                let keep = !found && value.is_some();
                if keep {
                    *v = value.map(|x| encode(x, "/:"));
                }
                found = true;
                keep
            });
        }
        if let (false, Some(x)) = (found, value) {
            self.query.get_or_insert_with(Vec::new).push((key.to_string(), Some(encode(x, "/:"))));
        }
        // Nothing left after the last removal prints no bare `?`.
        if self.query.as_ref().is_some_and(|q| q.is_empty()) {
            self.query = None;
        }
        self.slash |= self.query.is_some();
    }

    /// Whether TLS is on: the explicit option, else what the driver infers from SRV or from any TLS file or flag.
    pub fn tls_on(&self) -> bool {
        match self.opt("tls").or_else(|| self.opt("ssl")).map(|v| v.to_ascii_lowercase()).as_deref() {
            Some("true") => true,
            Some("false") => false,
            _ => self.srv || TLS_IMPLYING.iter().any(|k| self.has_opt(k)),
        }
    }

    /// Turning TLS off drops every other tls option and the ssl alias, which the driver refuses beside tls=false.
    pub fn set_tls(&mut self, on: bool) {
        self.set_opt("ssl", None);
        if !on {
            let keys: Vec<String> = self.query.iter().flatten().map(|(k, _)| k.clone()).filter(|k| k.to_ascii_lowercase().starts_with("tls") && !k.eq_ignore_ascii_case("tls")).collect();
            for k in keys {
                self.set_opt(&k, None);
            }
        }
        self.set_opt("tls", Some(if on { "true" } else { "false" }));
    }

    /// Any of the three options that skip certificate checks; tlsInsecure skips the name check too.
    pub fn allow_invalid(&self) -> bool {
        TLS_INVALID.iter().any(|k| self.opt(k).is_some_and(|v| v.eq_ignore_ascii_case("true")))
    }

    /// Off removes all three options. On adds tlsAllowInvalidCertificates unless tlsInsecure already says so, because the
    /// driver refuses the two together.
    pub fn set_allow_invalid(&mut self, on: bool) {
        if !on {
            for k in TLS_INVALID {
                self.set_opt(k, None);
            }
        } else if !self.opt("tlsInsecure").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
            // A leftover tlsInsecure=false would still make the pair the driver refuses.
            self.set_opt("tlsInsecure", None);
            self.set_opt("tlsAllowInvalidCertificates", Some("true"));
        }
    }
}

/// The address as the Rust driver accepts it. Its rustls build has no tlsAllowInvalidHostnames and refuses the whole
/// string with a raw parse error, while Compass and the Shell's Node driver take it. Off, it changes nothing and is
/// dropped. On, it is refused in plain words: rustls cannot skip only the name check, and turning off every
/// certificate check instead must be the user's choice.
pub fn for_driver(uri: &str) -> Result<String, String> {
    let Some(mut m) = MongoUri::parse(uri).filter(|m| m.has_opt("tlsAllowInvalidHostnames")) else { return Ok(uri.to_string()) };
    let on = |k: &str| m.opt(k).is_some_and(|v| v.eq_ignore_ascii_case("true"));
    // With every certificate check already off, the name check goes too, so the option adds nothing.
    if on("tlsAllowInvalidHostnames") && !on("tlsAllowInvalidCertificates") && !on("tlsInsecure") {
        return Err("Quarry cannot skip only the host name check (tlsAllowInvalidHostnames). Edit the connection and turn on Allow invalid certificates, which skips every certificate check, or remove the option.".into());
    }
    m.set_opt("tlsAllowInvalidHostnames", None);
    Ok(m.to_string())
}

impl std::fmt::Display for MongoUri {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.srv { SRV } else { PLAIN })?;
        if self.user.is_some() || self.password.is_some() {
            f.write_str(self.user.as_deref().unwrap_or(""))?;
            if let Some(p) = &self.password {
                write!(f, ":{p}")?;
            }
            f.write_str("@")?;
        }
        f.write_str(&self.hosts.join(","))?;
        if self.slash {
            write!(f, "/{}", self.db)?;
        }
        if let Some(q) = &self.query {
            let parts: Vec<String> = q.iter().map(|(k, v)| match v {
                Some(v) => format!("{k}={v}"),
                None => k.clone(),
            }).collect();
            write!(f, "?{}", parts.join("&"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URIS: &[&str] = &[
        "mongodb://localhost:27017",
        "mongodb://localhost:27017/",
        "mongodb://localhost",
        "mongodb://a:27017,b:27018,c:27019/shop?replicaSet=rs0&readPreference=secondaryPreferred",
        "mongodb+srv://app:pw@cluster0.abcde.mongodb.net/?retryWrites=true&w=majority&appName=Cluster0",
        "mongodb+srv://cluster0.abcde.mongodb.net",
        "mongodb://app:p%40ss%2Fw0rd%3A@h1:27017/db?authSource=admin",
        "mongodb://app:p@ss@h1/db",
        "mongodb://user@h1/?x",
        "mongodb://user:@h1",
        "mongodb://h1/?",
        "mongodb://h1/?a=b&",
        "mongodb://h1?tls=true&tlsCAFile=%2Fetc%2Fca%20file.pem&unknownOption=1&UNKNOWN2=%zz",
        "mongodb://%2Ftmp%2Fmongodb-27017.sock",
        "mongodb://[::1]:27017,h2/db?directConnection=true",
        "mongodb://",
    ];

    #[test]
    fn an_untouched_string_prints_back_unchanged() {
        for u in URIS {
            assert_eq!(MongoUri::parse(u).unwrap().to_string(), *u);
        }
    }

    #[test]
    fn other_strings_do_not_parse() {
        assert!(MongoUri::parse("postgres://h/db").is_none());
        assert!(MongoUri::parse("localhost:27017").is_none());
        assert!(MongoUri::parse("").is_none());
    }

    #[test]
    fn the_fields_read_decoded() {
        let m = MongoUri::parse("mongodb://app:p%40ss%2Fw0rd@a:1,b:2/shop?replicaSet=rs0&authSource=admin&readPreference=nearest").unwrap();
        assert_eq!((m.user().as_str(), m.password().as_str(), m.database().as_str()), ("app", "p@ss/w0rd", "shop"));
        assert_eq!(m.hosts_text(), "a:1,b:2");
        assert_eq!((m.opt("replicaset").as_deref(), m.opt("authSource").as_deref(), m.opt("readPreference").as_deref()), (Some("rs0"), Some("admin"), Some("nearest")));
        assert_eq!(m.opt("w"), None);
    }

    #[test]
    fn setting_replica_set_keeps_every_other_option_in_place() {
        let mut m = MongoUri::parse("mongodb://h/db?retryWrites=true&replicaSet=old&w=majority&custom=%2F").unwrap();
        m.set_opt("replicaSet", Some("rs1"));
        assert_eq!(m.to_string(), "mongodb://h/db?retryWrites=true&replicaSet=rs1&w=majority&custom=%2F");
        m.set_opt("replicaSet", None);
        assert_eq!(m.to_string(), "mongodb://h/db?retryWrites=true&w=majority&custom=%2F");
        // A new option goes last, and the slash the query needs appears.
        let mut m = MongoUri::parse("mongodb://h").unwrap();
        m.set_opt("authSource", Some("admin"));
        assert_eq!(m.to_string(), "mongodb://h/?authSource=admin");
        m.set_opt("authSource", None);
        assert_eq!(m.to_string(), "mongodb://h/");
    }

    #[test]
    fn a_repeated_key_collapses_to_one() {
        let mut m = MongoUri::parse("mongodb://h/?authsource=a&AuthSource=b").unwrap();
        m.set_opt("authSource", Some("c"));
        assert_eq!(m.to_string(), "mongodb://h/?authsource=c");
    }

    #[test]
    fn user_password_and_database_are_encoded_when_set() {
        let mut m = MongoUri::parse("mongodb://h:27017").unwrap();
        m.set_user("app");
        m.set_password("p@ss/w:rd é");
        m.set_database("shop");
        assert_eq!(m.to_string(), "mongodb://app:p%40ss%2Fw%3Ard%20%C3%A9@h:27017/shop");
        assert_eq!(MongoUri::parse(&m.to_string()).unwrap().password(), "p@ss/w:rd é");
        m.set_user("");
        m.set_password("");
        assert_eq!(m.to_string(), "mongodb://h:27017/shop");
    }

    #[test]
    fn srv_keeps_one_host_without_a_port() {
        let mut m = MongoUri::parse("mongodb://a:27017,b:27018/db?x=1").unwrap();
        m.set_srv(true);
        assert_eq!(m.to_string(), "mongodb+srv://a/db?x=1");
        m.set_srv(false);
        assert_eq!(m.to_string(), "mongodb://a/db?x=1&tls=true");
    }

    #[test]
    fn srv_drops_direct_connection_and_leaving_it_keeps_tls() {
        let mut m = MongoUri::parse("mongodb://a:1/?directConnection=true&w=1").unwrap();
        m.set_srv(true);
        assert_eq!(m.to_string(), "mongodb+srv://a/?w=1");
        m.set_srv(false);
        assert_eq!(m.to_string(), "mongodb://a/?w=1&tls=true");
        // An explicit choice is not overridden.
        let mut m = MongoUri::parse("mongodb+srv://a/?tls=false").unwrap();
        m.set_srv(false);
        assert_eq!(m.to_string(), "mongodb://a/?tls=false");
        let mut m = MongoUri::parse("mongodb+srv://a/?ssl=false").unwrap();
        m.set_srv(false);
        assert_eq!(m.to_string(), "mongodb://a/?ssl=false");
    }

    #[test]
    fn tls_off_drops_every_tls_option_and_the_ssl_alias() {
        let mut m = MongoUri::parse("mongodb://h/?ssl=true&tlsInsecure=true&TLSAllowInvalidHostnames=true&tlsCertificateKeyFilePassword=x&tlsCAFile=%2Fca&w=1").unwrap();
        m.set_tls(false);
        assert_eq!(m.to_string(), "mongodb://h/?w=1&tls=false");
    }

    #[test]
    fn invalid_certificates_count_every_flag_the_driver_has() {
        let mut m = MongoUri::parse("mongodb://h/?tlsInsecure=true").unwrap();
        assert!(m.tls_on() && m.allow_invalid());
        // Already on through tlsInsecure, and the driver refuses both together.
        m.set_allow_invalid(true);
        assert_eq!(m.to_string(), "mongodb://h/?tlsInsecure=true");
        m.set_allow_invalid(false);
        assert_eq!(m.to_string(), "mongodb://h/");
        let mut m = MongoUri::parse("mongodb://h/?tlsAllowInvalidHostnames=true&tlsAllowInvalidCertificates=true&tlsInsecure=false").unwrap();
        assert!(m.tls_on() && m.allow_invalid());
        m.set_allow_invalid(false);
        assert!(!m.allow_invalid());
        assert_eq!(m.to_string(), "mongodb://h/");
        assert!(MongoUri::parse("mongodb://h/?tlsAllowInvalidHostnames=true").unwrap().tls_on());
        let mut m = MongoUri::parse("mongodb://h/?tls=true").unwrap();
        m.set_allow_invalid(true);
        assert_eq!(m.to_string(), "mongodb://h/?tls=true&tlsAllowInvalidCertificates=true");
    }

    #[test]
    fn hosts_keep_a_half_typed_comma() {
        let mut m = MongoUri::parse("mongodb://a:1").unwrap();
        m.set_hosts("a:1,");
        assert_eq!(m.hosts_text(), "a:1,");
        m.set_hosts("a:1, b:2");
        assert_eq!(m.to_string(), "mongodb://a:1,b:2");
    }

    #[test]
    fn tls_follows_the_drivers_rules() {
        let on = |u: &str| MongoUri::parse(u).unwrap().tls_on();
        assert!(!on("mongodb://h"));
        assert!(on("mongodb+srv://h"));
        assert!(on("mongodb://h/?tls=true"));
        assert!(on("mongodb://h/?ssl=TRUE"));
        assert!(!on("mongodb+srv://h/?tls=false"));
        assert!(on("mongodb://h/?tlsCAFile=%2Fca.pem"));
        let mut m = MongoUri::parse("mongodb://h/?tls=true&tlsCAFile=%2Fca.pem&tlsCertificateKeyFile=%2Fc.pem&tlsAllowInvalidCertificates=true&w=1").unwrap();
        assert!(m.allow_invalid());
        m.set_tls(false);
        assert_eq!(m.to_string(), "mongodb://h/?tls=false&w=1");
        m.set_tls(true);
        m.set_opt("tlsCAFile", Some("/my certs/ca.pem"));
        assert_eq!(m.to_string(), "mongodb://h/?tls=true&w=1&tlsCAFile=/my%20certs/ca.pem");
        assert_eq!(m.opt("tlsCAFile").as_deref(), Some("/my certs/ca.pem"));
    }

    #[test]
    fn the_driver_never_sees_allow_invalid_hostnames() {
        assert!(for_driver("mongodb://h/?tls=true&tlsAllowInvalidHostnames=true&w=1").unwrap_err().contains("Allow invalid certificates"));
        assert_eq!(for_driver("mongodb://h/?tls=true&tlsAllowInvalidHostnames=false&w=1").unwrap(), "mongodb://h/?tls=true&w=1");
        assert_eq!(for_driver("mongodb://h/?tls=true").unwrap(), "mongodb://h/?tls=true");
        assert_eq!(for_driver("mongodb://h/?tls=true&tlsAllowInvalidCertificates=true&tlsAllowInvalidHostnames=true").unwrap(), "mongodb://h/?tls=true&tlsAllowInvalidCertificates=true");
        let mut m = MongoUri::parse("mongodb://h/?tlsInsecure=false").unwrap();
        m.set_allow_invalid(true);
        assert_eq!(m.to_string(), "mongodb://h/?tlsAllowInvalidCertificates=true");
    }
}
