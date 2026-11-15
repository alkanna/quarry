//! Certificate checks against servers whose certificate names another host than the one connected to.
//!
//! QUARRY_TEST_TLS_DIR holds ca.crt (the CA that signed the servers' certificate) and other-ca.crt (an unrelated CA).
//! QUARRY_TEST_PGTLS is `postgres://postgres:quarry@host:port/postgres` and QUARRY_TEST_MYTLS `mysql://root:quarry@host:port`,
//! on servers whose certificate names only db.example.test and mysql.example.test. QUARRY_TEST_TLS_SSH is
//! `user:password@host:port` of an SSH server that reaches the PostgreSQL server as db.example.test and pg.other.test, and
//! the MySQL one as mysql.example.test and my.other.test. Commands in docs/sql/POSTGRES.md Verification.

use crate::db::sql::{Addr, SslMode};
use super::Live;
use crate::db::{ErrCtx, Net, explain};
use std::time::Duration;

fn ca(dir: &str, f: &str) -> String {
    std::path::Path::new(dir).join(f).display().to_string()
}

fn ssh() -> Option<Net> {
    let (user, pass, host, port) = super::creds("QUARRY_TEST_TLS_SSH")?;
    let conf = crate::ssh::SshConf { host, port, user, auth: crate::ssh::SshAuth::Password, ..Default::default() };
    Some(Net { ssh: Some((conf, crate::ssh::SshSecrets { password: Some(pass), passphrase: None })), ..Default::default() })
}

fn title(r: &Result<String, String>) -> String {
    r.as_ref().err().map(|e| explain(e, &ErrCtx::default()).title).unwrap_or_else(|| "connected".into())
}

/// Every case runs before the test fails, so one run shows the whole picture.
fn check(var: &str, names: (&str, &str), ca_skips_name: bool) {
    let Ok(dir) = std::env::var("QUARRY_TEST_TLS_DIR") else { return };
    let l = Live::new();
    let Ok(base) = std::env::var(var) else { return };
    let a = Addr::parse(&base).unwrap();
    let with = |mode: SslMode, ca_file: &str| Addr { ssl_mode: mode, ssl_ca: ca(&dir, ca_file), ..a.clone() };
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
    // PostgreSQL trusts the CA file alone (libpq), so verify-full with another CA fails on the chain, and verify-ca
    // without a file is refused before connecting.
    if ca_skips_name {
        expect("verify-full with another CA", l.test(&with(SslMode::VerifyIdentity, "other-ca.crt"), Net::default()), "The server's certificate is not trusted");
        expect("verify-ca without a CA file", l.test(&Addr { ssl_mode: SslMode::VerifyCa, ssl_ca: String::new(), ..a.clone() }, Net::default()), "verify-ca needs a CA file");
    }
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

/// The tunnel makes its private socket only when asked, which only MySQL with a certificate check does.
#[test]
fn live_ssh_tunnel_socket_only_when_asked() {
    let Some((conf, secrets)) = ssh().and_then(|n| n.ssh) else { return };
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let to = Some(("db.example.test".to_string(), 5432));
        let t = crate::ssh::open(&conf, &secrets, Duration::from_secs(20), to.clone(), false).await.unwrap();
        assert!(t.socket.is_none());
        let t = crate::ssh::open(&conf, &secrets, Duration::from_secs(20), to, true).await.unwrap();
        assert_eq!(t.socket.is_some(), cfg!(unix));
    });
}
