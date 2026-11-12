//! Live and cross-module tests of the SQL code. Each file skips itself when its server variable is unset.

mod my_session_settings;
mod pg_advanced;
mod pg_ddl;
mod pg_dump;
mod pg_import;
mod pg_pgbouncer;
mod pg_read;
mod pg_roles;
mod pg_shell;
mod pg_write;
mod tls;

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
        ..Default::default()
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
fn client_certificate_paths_come_from_the_string_and_go_back_into_it() {
    let a = Addr::from_paste("postgresql://analyst@db.internal/shop?sslmode=verify-full&sslcert=%2Fhome%2Fa%2Fmy%20cert.crt&sslkey=~%2F.postgresql%2Fanalyst.key&sslpassword=nope").unwrap();
    assert_eq!((a.ssl_cert.as_str(), a.ssl_key.as_str()), ("/home/a/my cert.crt", "~/.postgresql/analyst.key"));
    // sslpassword is a secret, so a string never carries it into the Keychain URL; the TLS tab's field does.
    assert_eq!(a.to_uri(), "postgres://analyst@db.internal:5432/shop?sslmode=verify-full&sslcert=%2Fhome%2Fa%2Fmy%20cert.crt&sslkey=~%2F.postgresql%2Fanalyst.key");
    assert_eq!(Addr::parse(&a.to_uri()), Some(a));
    let a = Addr::from_paste(r"host=h sslmode=require sslcert='/c/my cert.crt' sslkey=/c/k.key").unwrap();
    assert_eq!((a.ssl_mode, a.ssl_cert.as_str(), a.ssl_key.as_str()), (SslMode::Required, "/c/my cert.crt", "/c/k.key"));
    // MySQL has no such fields in Quarry, so they are neither read nor written there.
    let m = Addr::parse("mysql://u@h/?sslcert=%2Fc.crt").unwrap();
    assert_eq!(m.ssl_cert, "");
    assert!(!Addr { ssl_cert: "/c.crt".into(), ..m }.to_uri().contains("sslcert"));
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
