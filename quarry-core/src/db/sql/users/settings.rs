//! The account settings the Account and Limits tabs edit, and the plan they stage.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expire {
    Never,
    /// The server's default_password_lifetime.
    Default,
    Days(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tls {
    None,
    Ssl,
    X509,
    /// A cipher, issuer or subject requirement, held in `Settings::spec`.
    Specified,
}

/// The values of REQUIRE CIPHER, ISSUER and SUBJECT; empty ones are not required.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TlsSpec {
    pub cipher: String,
    pub issuer: String,
    pub subject: String,
}

pub const LIMIT_NAMES: [&str; 4] = ["MAX_QUERIES_PER_HOUR", "MAX_UPDATES_PER_HOUR", "MAX_CONNECTIONS_PER_HOUR", "MAX_USER_CONNECTIONS"];
pub const LIMIT_MAX: u64 = 4_294_967_295;

/// What ALTER USER can change besides the password: the lock, the password lifetime, the TLS requirement and the
/// four resource limits (0 is no limit).
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub locked: bool,
    pub expire: Expire,
    pub tls: Tls,
    /// What `Tls::Specified` requires.
    pub spec: TlsSpec,
    pub limits: [u64; 4],
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { locked: false, expire: Expire::Default, tls: Tls::None, spec: TlsSpec::default(), limits: [0; 4] }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AccountInfo {
    pub plugin: String,
    pub settings: Settings,
    /// MariaDB: the account has more than one way to log in (`unix_socket OR mysql_native_password`).
    pub chain: bool,
    /// Something that changes when the account is dropped and made again, to tell it from a namesake.
    pub ident: Option<String>,
}

/// What the grants of an account and its row in mysql.user say, for the window's tabs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Loaded {
    pub grants: Vec<Grant>,
    pub info: Option<AccountInfo>,
    /// Why `info` is missing: mysql.user is closed to this user, or the account has no row.
    pub info_err: Option<String>,
    /// lower_case_table_names is not 0, so `Shop` and `shop` are one database to the server.
    pub fold_case: bool,
}

/// What this server version can do, so only that is offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    pub alter_user: bool,
    pub lock: bool,
    pub lifetime: bool,
    pub roles: bool,
    pub dynamic: bool,
    /// IDENTIFIED WITH <plugin> BY: MySQL only; MariaDB changes the plugin another way.
    pub plugin_choice: bool,
    pub caching: bool,
    /// mysql_native_password is off by default from 8.4 and gone in 9.0.
    pub native_off: bool,
    pub sha256: bool,
}

impl Caps {
    pub fn of(d: Dialect, version: &str) -> Caps {
        // Older MariaDB servers report "5.5.5-10.11.6-MariaDB" for compatibility.
        let v = version.strip_prefix("5.5.5-").filter(|_| matches!(d, Dialect::MariaDb)).unwrap_or(version);
        let mut n = v.split(|c: char| !c.is_ascii_digit()).filter(|x| !x.is_empty()).map(|x| x.parse::<u32>().unwrap_or(0));
        let at = (n.next().unwrap_or(0), n.next().unwrap_or(0), n.next().unwrap_or(0));
        match d {
            Dialect::MySql => Caps { alter_user: at >= (5, 7, 6), lock: at >= (5, 7, 6), lifetime: at >= (5, 7, 4), roles: at >= (8, 0, 0), dynamic: at >= (8, 0, 0), plugin_choice: true, caching: at >= (8, 0, 0), native_off: at >= (8, 4, 0), sha256: ((5, 6, 0)..(9, 0, 0)).contains(&at) },
            Dialect::MariaDb => Caps { alter_user: at >= (10, 2, 0), lock: at >= (10, 4, 2), lifetime: at >= (10, 4, 3), roles: at >= (10, 0, 5), dynamic: false, plugin_choice: false, caching: false, native_off: false, sha256: false },
            // Nothing here applies to PostgreSQL roles
            Dialect::Postgres => Caps { alter_user: false, lock: false, lifetime: false, roles: false, dynamic: false, plugin_choice: false, caching: false, native_off: false, sha256: false },
        }
    }

    /// The plugins "Change password" offers: mysql_native_password only where it is on by default, or when the
    /// account already uses it.
    pub fn plugins(&self, current: &str) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.caching {
            v.push("caching_sha2_password");
        }
        if !self.native_off || current == "mysql_native_password" {
            v.push("mysql_native_password");
        }
        if self.sha256 {
            v.push("sha256_password");
        }
        v
    }
}

pub const MYSQL_PLUGINS: [&str; 3] = ["caching_sha2_password", "mysql_native_password", "sha256_password"];

/// What the Account and Limits tabs stage together: a rename, setting changes and role changes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccountPlan {
    pub rename: Option<Account>,
    pub settings: Option<(Settings, Settings)>,
    pub roles_grant: Vec<String>,
    pub roles_revoke: Vec<String>,
}

impl AccountPlan {
    pub fn is_empty(&self) -> bool {
        self.rename.is_none() && self.settings.as_ref().is_none_or(|(a, b)| a == b) && self.roles_grant.is_empty() && self.roles_revoke.is_empty()
    }
}
