//! The client certificate and key of a PostgreSQL connection. Both files are read before connecting, so a missing file
//! or a locked key is named at once instead of surfacing as a handshake error, and the key is handed to sqlx in memory:
//! sqlx reads unencrypted PEM keys only, so an encrypted PKCS#8 key is decrypted here with its passphrase.

use crate::db::Res;
use pkcs8::{EncryptedPrivateKeyInfoRef, LineEnding, SecretDocument};
use std::path::PathBuf;

// The start of each message this module returns. `db::explain` words them by these, so they stay in one place.
pub(crate) const CERT_UNREADABLE: &str = "Could not read the client certificate file";
pub(crate) const KEY_UNREADABLE: &str = "Could not read the client key file";
pub(crate) const CERT_NOT_PEM: &str = "No PEM certificate in the client certificate file";
pub(crate) const KEY_NOT_PEM: &str = "No PEM private key in the client key file";
pub(crate) const HALF_SET: &str = "Only one of the client certificate and the client key is set";
pub(crate) const KEY_LOCKED: &str = "The client key is encrypted and no passphrase is set";
pub(crate) const KEY_WRONG_PASS: &str = "The client key passphrase is wrong";
pub(crate) const KEY_OLD_CIPHER: &str = "The client key is encrypted in a format Quarry cannot read";

/// A leading `~/` is the home folder, as people type it; libpq's own default paths use it too.
pub(super) fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/").zip(dirs::home_dir()) {
        Some((rest, home)) => home.join(rest),
        None => PathBuf::from(p),
    }
}

fn read(path: &str, err: &str) -> Res<Vec<u8>> {
    std::fs::read(expand(path)).map_err(|e| format!("{err} {path}: {e}"))
}

/// The text of the first PEM block with this label, from its BEGIN line to its END line. A file may carry other text
/// around the block, which the strict decoder refuses.
fn block<'a>(text: &'a str, label: &str) -> Option<&'a str> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let i = text.find(&begin)?;
    let j = text[i..].find(&end)? + i + end.len();
    Some(&text[i..j])
}

/// Reads the certificate chain and the private key as PEM. Empty paths on both sides mean no client certificate.
pub(super) fn load(cert: &str, key: &str, passphrase: Option<&str>) -> Res<Option<(Vec<u8>, Vec<u8>)>> {
    let (cert, key) = (cert.trim(), key.trim());
    match (cert.is_empty(), key.is_empty()) {
        (true, true) => return Ok(None),
        (false, false) => {}
        _ => return Err(HALF_SET.into()),
    }
    let c = read(cert, CERT_UNREADABLE)?;
    if block(&String::from_utf8_lossy(&c), "CERTIFICATE").is_none() {
        return Err(format!("{CERT_NOT_PEM} {cert}"));
    }
    let k = read(key, KEY_UNREADABLE)?;
    let text = String::from_utf8_lossy(&k);
    // The traditional OpenSSL form encrypts inside an RSA or EC block, which neither sqlx nor pkcs8 can open.
    if text.contains("Proc-Type: 4,ENCRYPTED") {
        return Err(KEY_OLD_CIPHER.into());
    }
    if let Some(b) = block(&text, "ENCRYPTED PRIVATE KEY") {
        let pass = passphrase.filter(|p| !p.is_empty()).ok_or(KEY_LOCKED)?;
        return Ok(Some((c, decrypt(b, pass)?)));
    }
    if !text.contains("PRIVATE KEY-----") {
        return Err(format!("{KEY_NOT_PEM} {key}"));
    }
    Ok(Some((c, k)))
}

/// An encrypted PKCS#8 block decrypted to a plain `PRIVATE KEY` block, in memory only.
fn decrypt(block: &str, pass: &str) -> Res<Vec<u8>> {
    let (_, doc) = SecretDocument::from_pem(block).map_err(|_| format!("{KEY_NOT_PEM} (the encrypted block does not decode)"))?;
    let info = EncryptedPrivateKeyInfoRef::try_from(doc.as_bytes()).map_err(|_| KEY_OLD_CIPHER.to_string())?;
    let plain = info.decrypt(pass).map_err(|e| match e {
        // A wrong passphrase fails the padding check or yields bytes that are not a key.
        pkcs8::Error::EncryptedPrivateKey(pkcs8::pkcs5::Error::DecryptFailed) | pkcs8::Error::Asn1(_) => KEY_WRONG_PASS.to_string(),
        _ => KEY_OLD_CIPHER.to_string(),
    })?;
    let pem = plain.to_pem("PRIVATE KEY", LineEnding::LF).map_err(|_| KEY_WRONG_PASS.to_string())?;
    Ok(pem.as_bytes().to_vec())
}

/// The certificate subject as the server reports it in pg_stat_ssl ("/O=Acme/CN=analyst"), written "O=Acme, CN=analyst".
pub(crate) fn subject(dn: &str) -> String {
    dn.trim().trim_start_matches('/').split('/').filter(|p| !p.is_empty()).collect::<Vec<_>>().join(", ")
}

/// What Test adds about the client certificate: the subject the server saw, or why none was seen.
pub(crate) fn test_note(uri: &str, dn: &str, tls: bool, cloud: bool) -> String {
    let set = super::Addr::parse(uri).is_some_and(|a| a.engine() == super::Engine::Postgres && !a.ssl_cert.trim().is_empty() && a.ssl_mode != super::SslMode::Disabled);
    if !dn.is_empty() {
        format!(" with client certificate {dn}")
    } else if !set || cloud {
        String::new()
    } else if !tls {
        " (without TLS, so the client certificate was not sent)".into()
    } else {
        " (the server did not report a client certificate)".into()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::process::Command;

    /// A throwaway CA, a server certificate for `host` and a client certificate for `user`, made with openssl in `dir`.
    /// The client key is written plain (client.key), PKCS#8 encrypted with "s3cret" (client-enc.key) and in the
    /// traditional encrypted form (client-old.key); other.key matches no certificate. None when openssl is missing.
    pub(crate) fn make_certs(dir: &std::path::Path, host: &str, user: &str) -> Option<()> {
        let run = |args: &[&str]| Command::new("openssl").args(args).current_dir(dir).output().ok().filter(|o| o.status.success()).map(|_| ());
        run(&["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", "ca.key", "-out", "ca.crt", "-days", "2", "-subj", "/CN=quarry-test-ca"])?;
        std::fs::write(dir.join("server.ext"), format!("subjectAltName=DNS:{host},DNS:localhost,IP:127.0.0.1\n")).ok()?;
        run(&["req", "-newkey", "rsa:2048", "-nodes", "-keyout", "server.key", "-out", "server.csr", "-subj", &format!("/CN={host}")])?;
        run(&["x509", "-req", "-in", "server.csr", "-CA", "ca.crt", "-CAkey", "ca.key", "-CAcreateserial", "-out", "server.crt", "-days", "2", "-extfile", "server.ext"])?;
        run(&["req", "-newkey", "rsa:2048", "-nodes", "-keyout", "client.key", "-out", "client.csr", "-subj", &format!("/O=Quarry tests/CN={user}")])?;
        run(&["x509", "-req", "-in", "client.csr", "-CA", "ca.crt", "-CAkey", "ca.key", "-CAcreateserial", "-out", "client.crt", "-days", "2"])?;
        run(&["pkcs8", "-topk8", "-v2", "aes-256-cbc", "-in", "client.key", "-out", "client-enc.key", "-passout", "pass:s3cret"])?;
        // -traditional exists on OpenSSL 3; LibreSSL writes the traditional form without it.
        run(&["rsa", "-aes256", "-traditional", "-in", "client.key", "-out", "client-old.key", "-passout", "pass:s3cret"]).or_else(|| run(&["rsa", "-aes256", "-in", "client.key", "-out", "client-old.key", "-passout", "pass:s3cret"]))?;
        run(&["genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out", "other.key"])?;
        Some(())
    }

    pub(crate) fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("quarry-pgcert-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn files_are_read_and_an_encrypted_key_is_opened_with_its_passphrase() {
        let d = temp_dir("load");
        if make_certs(&d, "db.test", "analyst").is_none() {
            eprintln!("openssl not found, skipped");
            return;
        }
        let p = |f: &str| d.join(f).display().to_string();
        let plain = std::fs::read(d.join("client.key")).unwrap();
        let (c, k) = load(&p("client.crt"), &p("client.key"), None).unwrap().unwrap();
        assert!(c.starts_with(b"-----BEGIN CERTIFICATE-----"));
        assert_eq!(k, plain);
        // The decrypted key is the same key, so it parses to the same DER as the plain file.
        let (_, k) = load(&p("client.crt"), &p("client-enc.key"), Some("s3cret")).unwrap().unwrap();
        let der = |pem: &[u8]| SecretDocument::from_pem(block(std::str::from_utf8(pem).unwrap(), "PRIVATE KEY").unwrap()).unwrap().1.as_bytes().to_vec();
        assert_eq!(der(&k), der(&plain));
        assert_eq!(load(&p("client.crt"), &p("client-enc.key"), None).unwrap_err(), KEY_LOCKED);
        assert_eq!(load(&p("client.crt"), &p("client-enc.key"), Some("")).unwrap_err(), KEY_LOCKED);
        assert_eq!(load(&p("client.crt"), &p("client-enc.key"), Some("wrong")).unwrap_err(), KEY_WRONG_PASS);
        assert_eq!(load(&p("client.crt"), &p("client-old.key"), Some("s3cret")).unwrap_err(), KEY_OLD_CIPHER);
        assert!(load(&p("missing.crt"), &p("client.key"), None).unwrap_err().starts_with(&format!("{CERT_UNREADABLE} {}: ", p("missing.crt"))));
        assert!(load(&p("client.crt"), &p("missing.key"), None).unwrap_err().starts_with(KEY_UNREADABLE));
        assert!(load(&p("client.key"), &p("client.key"), None).unwrap_err().starts_with(CERT_NOT_PEM));
        assert!(load(&p("client.crt"), &p("client.crt"), None).unwrap_err().starts_with(KEY_NOT_PEM));
        assert_eq!(load(&p("client.crt"), "  ", None).unwrap_err(), HALF_SET);
        assert_eq!(load("", "", Some("x")).unwrap(), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn paths_and_subjects() {
        assert_eq!(expand("/a/b"), PathBuf::from("/a/b"));
        if let Some(h) = dirs::home_dir() {
            assert_eq!(expand("~/.postgresql/postgresql.crt"), h.join(".postgresql/postgresql.crt"));
        }
        assert_eq!(expand("~user/x"), PathBuf::from("~user/x"));
        assert_eq!(subject("/O=Quarry tests/CN=analyst"), "O=Quarry tests, CN=analyst");
        assert_eq!(subject("/CN=analyst"), "CN=analyst");
        assert_eq!(subject(""), "");
        assert_eq!(block("junk\n-----BEGIN X-----\nab\n-----END X-----\nmore", "X"), Some("-----BEGIN X-----\nab\n-----END X-----"));
        assert_eq!(block("-----BEGIN X-----\nab", "X"), None);
    }

    #[test]
    fn test_reports_the_certificate() {
        let uri = "postgres://a@h:5432/d?sslmode=verify-full&sslcert=%2Fc.crt&sslkey=%2Fc.key";
        assert_eq!(test_note(uri, "CN=a", true, false), " with client certificate CN=a");
        assert_eq!(test_note(uri, "", true, false), " (the server did not report a client certificate)");
        assert_eq!(test_note(uri, "", false, false), " (without TLS, so the client certificate was not sent)");
        assert_eq!(test_note(uri, "", false, true), "");
        assert_eq!(test_note("postgres://a@h:5432/d", "", true, false), "");
        assert_eq!(test_note("postgres://a@h:5432/d?sslmode=disable&sslcert=%2Fc.crt&sslkey=%2Fc.key", "", false, false), "");
    }

    /// QUARRY_TEST_PGCERT and QUARRY_TEST_PGCERT13 are `postgres://quarry_cert:quarry@host:port/postgres` on servers whose
    /// pg_hba.conf has `hostssl all quarry_cert all scram-sha-256 clientcert=verify-full`, and QUARRY_TEST_PGCERT_DIR holds
    /// the files of docs/sql/POSTGRES.md Verification: ca.crt, client.crt (CN=quarry_cert), client.key, client-enc.key
    /// (passphrase s3cret) and other.key.
    #[test]
    fn live_client_certificate_sign_in() {
        use crate::db::{Msg, Net, Worker, explain, ErrCtx};
        use std::time::Duration;
        let Ok(dir) = std::env::var("QUARRY_TEST_PGCERT_DIR") else { return };
        let p = |f: &str| std::path::Path::new(&dir).join(f).display().to_string();
        // require is sqlx's own TLS; verify-ca goes through pg_tls, which sends the certificate itself.
        let modes = [(super::super::SslMode::Required, String::new()), (super::super::SslMode::VerifyCa, p("ca.crt"))];
        for (var, (mode, ca)) in ["QUARRY_TEST_PGCERT", "QUARRY_TEST_PGCERT13"].into_iter().flat_map(|v| modes.clone().map(|m| (v, m))) {
            let Ok(base) = std::env::var(var) else { continue };
            let (tx, rx) = std::sync::mpsc::channel();
            let w = Worker::new(tx, eframe::egui::Context::default());
            let uri = |cert: &str, key: &str| {
                let a = super::super::Addr::parse(&base).unwrap();
                super::super::Addr { ssl_mode: mode, ssl_ca: ca.clone(), ssl_cert: cert.into(), ssl_key: key.into(), ..a }.to_uri()
            };
            let net = |pass: Option<&str>| Net { key_passphrase: pass.map(Into::into), ..Default::default() };
            let test = |cert: &str, key: &str, pass: Option<&str>| {
                w.sql_test_with(0, uri(cert, key), Duration::from_secs(20), net(pass), Default::default());
                match rx.recv_timeout(Duration::from_secs(40)) {
                    Ok(Msg::Tested { res, .. }) => res,
                    _ => panic!("{var}: expected Tested"),
                }
            };
            let title = |r: Result<String, String>| explain(&r.expect_err(var), &ErrCtx::default()).title;
            let ok = test(&p("client.crt"), &p("client.key"), None).unwrap_or_else(|e| panic!("{var}: {e}"));
            eprintln!("{var}: {ok}");
            assert!(ok.contains(" with client certificate ") && ok.contains("CN=quarry_cert"), "{var}: {ok}");
            let ok = test(&p("client.crt"), &p("client-enc.key"), Some("s3cret")).unwrap_or_else(|e| panic!("{var}: {e}"));
            assert!(ok.contains("CN=quarry_cert"), "{var}: {ok}");
            assert_eq!(title(test("", "", None)), "The server wants a client certificate", "{var}");
            assert_eq!(title(test(&p("client.crt"), &p("other.key"), None)), "The client key does not match the certificate", "{var}");
            assert_eq!(title(test(&p("client.crt"), &p("client-enc.key"), Some("nope"))), "The client key passphrase is wrong", "{var}");
            assert_eq!(title(test(&p("client.crt"), &p("client-enc.key"), None)), "The client key needs its passphrase", "{var}");
            assert_eq!(title(test(&p("gone.crt"), &p("client.key"), None)), "Could not read the client certificate file", "{var}");
            // The pool opens its later connections with the same certificate, without reading the files again.
            w.sql_connect_with(1, 0, uri(&p("client.crt"), &p("client-enc.key")), Duration::from_secs(20), net(Some("s3cret")), Default::default());
            let Ok(Msg::SqlConnected { res: Ok((db, _, _)), .. }) = rx.recv_timeout(Duration::from_secs(40)) else { panic!("{var}: expected SqlConnected") };
            w.sql_tables(1, db, "public".into());
            match rx.recv_timeout(Duration::from_secs(40)) {
                Ok(Msg::SqlTables { res, .. }) => assert!(res.is_ok(), "{var}: {res:?}"),
                Ok(Msg::Collections { res, .. }) => assert!(res.is_ok(), "{var}: {res:?}"),
                _ => panic!("{var}: expected SqlTables"),
            }
        }
    }
}
