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

/// Unreserved characters, plus `extra`, stay as they are; every other byte becomes %XX.
fn encode(s: &str, extra: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) || extra.as_bytes().contains(&b) {
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
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%' && i + 2 < b.len() && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

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

    /// An SRV address names one host and has no port, so turning it on keeps the first host without its port.
    pub fn set_srv(&mut self, on: bool) {
        if on && !self.srv {
            self.hosts.truncate(1);
            if let Some(h) = self.hosts.first_mut() && let Some((name, _)) = h.rsplit_once(':') && !h.ends_with(']') {
                *h = name.to_string();
            }
        }
        self.srv = on;
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
            _ => self.srv || ["tlsCAFile", "tlsCertificateKeyFile", "tlsAllowInvalidCertificates"].iter().any(|k| self.opt(k).is_some()),
        }
    }

    /// Turning TLS off also drops the files and the invalid-certificate flag, which the driver refuses beside tls=false.
    pub fn set_tls(&mut self, on: bool) {
        self.set_opt("ssl", None);
        if !on {
            for k in ["tlsCAFile", "tlsCertificateKeyFile", "tlsAllowInvalidCertificates"] {
                self.set_opt(k, None);
            }
        }
        self.set_opt("tls", Some(if on { "true" } else { "false" }));
    }

    pub fn allow_invalid(&self) -> bool {
        self.opt("tlsAllowInvalidCertificates").is_some_and(|v| v.eq_ignore_ascii_case("true"))
    }

    pub fn set_allow_invalid(&mut self, on: bool) {
        self.set_opt("tlsAllowInvalidCertificates", on.then_some("true"));
    }
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
        assert_eq!(m.to_string(), "mongodb://a/db?x=1");
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
}
