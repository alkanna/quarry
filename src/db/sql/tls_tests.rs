//! Certificate checks against servers whose certificate names another host than the one connected to.
//!
//! QUARRY_TEST_TLS_DIR holds ca.crt (the CA that signed the servers' certificate) and other-ca.crt (an unrelated CA).
//! QUARRY_TEST_PGTLS is `postgres://postgres:quarry@host:port/postgres` and QUARRY_TEST_MYTLS `mysql://root:quarry@host:port`,
//! on servers whose certificate names only db.example.test and mysql.example.test. QUARRY_TEST_TLS_SSH is
//! `user:password@host:port` of an SSH server that reaches the PostgreSQL server as db.example.test and pg.other.test, and
//! the MySQL one as mysql.example.test and my.other.test. Commands in docs/sql/POSTGRES.md Verification.

use super::{Addr, SslMode};
use crate::db::{ErrCtx, Msg, Net, Worker, explain};
use std::time::Duration;

struct Live {
    w: Worker,
    rx: std::sync::mpsc::Receiver<Msg>,
    dir: String,
}

impl Live {
    fn new() -> Option<Live> {
        let dir = std::env::var("QUARRY_TEST_TLS_DIR").ok()?;
        let (tx, rx) = std::sync::mpsc::channel();
        Some(Live { w: Worker::new(tx, eframe::egui::Context::default()), rx, dir })
    }

    fn ca(&self, f: &str) -> String {
        std::path::Path::new(&self.dir).join(f).display().to_string()
    }

    fn test(&self, a: &Addr, net: Net) -> Result<String, String> {
        self.w.sql_test_with(0, a.to_uri(), Duration::from_secs(20), net, Default::default());
        match self.rx.recv_timeout(Duration::from_secs(40)) {
            Ok(Msg::Tested { res, .. }) => res,
            _ => panic!("expected Tested"),
        }
    }
}

fn ssh() -> Option<Net> {
    let s = std::env::var("QUARRY_TEST_TLS_SSH").ok()?;
    let (cred, hp) = s.rsplit_once('@')?;
    let (user, pass) = cred.split_once(':')?;
    let (host, port) = hp.rsplit_once(':')?;
    let conf = crate::ssh::SshConf { host: host.into(), port: port.parse().ok()?, user: user.into(), auth: crate::ssh::SshAuth::Password, ..Default::default() };
    Some(Net { ssh: Some((conf, crate::ssh::SshSecrets { password: Some(pass.into()), passphrase: None })), ..Default::default() })
}

fn title(r: &Result<String, String>) -> String {
    r.as_ref().err().map(|e| explain(e, &ErrCtx::default()).title).unwrap_or_else(|| "connected".into())
}

/// Every case runs before the test fails, so one run shows the whole picture.
fn check(var: &str, names: (&str, &str), ca_skips_name: bool) {
    let Some(l) = Live::new() else { return };
    let Ok(base) = std::env::var(var) else { return };
    let a = Addr::parse(&base).unwrap();
    let with = |mode: SslMode, ca: &str| Addr { ssl_mode: mode, ssl_ca: l.ca(ca), ..a.clone() };
    let mut bad = Vec::new();
    let mut expect = |case: &str, r: Result<String, String>, want: &str| {
        let got = title(&r);
        eprintln!("{var} {case}: {r:?}");
        if got != want {
            bad.push(format!("{case}: wanted {want:?}, got {got:?}"));
        }
    };
    // Connected by address, so the certificate's names never match.
    // MySQL's TLS is sqlx's own, which checks the name in Verify CA too; the error says so.
    let r = l.test(&with(SslMode::VerifyCa, "ca.crt"), Net::default());
    if !ca_skips_name {
        assert!(r.as_ref().is_err_and(|e| explain(e, &ErrCtx::default()).hint.contains("MySQL checks the host name in Verify CA too")), "{var}: {r:?}");
    }
    expect("verify-ca", r, &if ca_skips_name { "connected".to_string() } else { format!("The certificate is not for {}", a.host) });
    expect("verify-full", l.test(&with(SslMode::VerifyIdentity, "ca.crt"), Net::default()), &format!("The certificate is not for {}", a.host));
    expect("verify-ca with another CA", l.test(&with(SslMode::VerifyCa, "other-ca.crt"), Net::default()), "The server's certificate is not trusted");
    // Through the tunnel the certificate is checked against the name typed, not the tunnel's 127.0.0.1.
    if let Some(net) = ssh() {
        let far = |mode: SslMode, host: &str| Addr { host: host.into(), port: Addr::default_port(&a.scheme), ..with(mode, "ca.crt") };
        expect("verify-full through SSH", l.test(&far(SslMode::VerifyIdentity, names.0), net.clone()), "connected");
        expect("verify-ca through SSH", l.test(&far(SslMode::VerifyCa, names.0), net.clone()), "connected");
        // Another name of the same server, which the certificate does not hold.
        expect("verify-full through SSH by another name", l.test(&far(SslMode::VerifyIdentity, names.1), net.clone()), &format!("The certificate is not for {}", names.1));
    }
    assert!(bad.is_empty(), "{var}:\n{}", bad.join("\n"));
}

#[test]
fn live_postgres_certificate_checks() {
    check("QUARRY_TEST_PGTLS", ("db.example.test", "pg.other.test"), true);
}

#[test]
fn live_mysql_certificate_checks() {
    check("QUARRY_TEST_MYTLS", ("mysql.example.test", "my.other.test"), false);
}
