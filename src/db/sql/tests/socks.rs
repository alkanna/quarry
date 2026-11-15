//! Live connects through a SOCKS5 proxy. QUARRY_TEST_SOCKS is `user:password@host:port` of a proxy that asks for that login
//! and reaches the servers below at the host named by QUARRY_TEST_SOCKS_TARGET (default host.docker.internal), on the
//! ports the test URLs already publish. `make servers-up` starts one (serjs/go-socks5-proxy) and `make test-live` sets both.
//! Runs for each of QUARRY_TEST_POSTGRES, QUARRY_TEST_MYSQL and QUARRY_TEST_MARIADB that is set.

use crate::db::sql::Addr;
use crate::db::{Msg, Net, Proxy, Worker};
use std::time::Duration;

fn proxy(password: Option<&str>) -> Option<Proxy> {
    let s = std::env::var("QUARRY_TEST_SOCKS").ok()?;
    let (cred, hp) = s.rsplit_once('@')?;
    let (user, pass) = cred.split_once(':')?;
    let (host, port) = hp.rsplit_once(':')?;
    Some(Proxy { host: host.into(), port: port.parse().ok()?, user: user.into(), password: Some(password.unwrap_or(pass).into()) })
}

fn test(addr: &Addr, p: Proxy) -> Result<String, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let w = Worker::new(tx, eframe::egui::Context::default());
    w.sql_test_with(0, addr.to_uri(), Duration::from_secs(20), Net { proxy: Some(p), ..Default::default() }, Default::default());
    match rx.recv_timeout(Duration::from_secs(40)) {
        Ok(Msg::Tested { res, .. }) => res,
        _ => panic!("expected Tested"),
    }
}

#[test]
fn live_sql_connects_through_a_socks5_proxy() {
    let Some(good) = proxy(None) else { return };
    let target = std::env::var("QUARRY_TEST_SOCKS_TARGET").unwrap_or_else(|_| "host.docker.internal".into());
    let mut bad = Vec::new();
    for var in ["QUARRY_TEST_POSTGRES", "QUARRY_TEST_MYSQL", "QUARRY_TEST_MARIADB"] {
        let Ok(url) = std::env::var(var) else { continue };
        let a = Addr { host: target.clone(), ..Addr::parse(&url).unwrap() };
        match test(&a, good.clone()) {
            Ok(m) if m.contains("through the SOCKS5 proxy") => {}
            other => bad.push(format!("{var}: {other:?}")),
        }
        // A wrong proxy password is refused by the proxy, before the database is reached.
        match test(&a, proxy(Some("wrong")).unwrap()) {
            Err(e) if e.contains("SOCKS5 proxy") => {}
            other => bad.push(format!("{var} with a wrong proxy password: {other:?}")),
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
