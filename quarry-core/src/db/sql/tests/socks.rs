//! Live connects through a SOCKS5 proxy. QUARRY_TEST_SOCKS is `user:password@host:port` of a proxy that asks for that login
//! and reaches the servers below at the host named by QUARRY_TEST_SOCKS_TARGET (default host.docker.internal), on the
//! ports the test URLs already publish. `make servers-up` starts one (serjs/go-socks5-proxy) and `make test-live` sets both.
//! One test for each of QUARRY_TEST_POSTGRES, QUARRY_TEST_MYSQL and QUARRY_TEST_MARIADB, each skipped when its variable is unset.

use super::Live;
use crate::db::sql::Addr;
use crate::db::{Net, Proxy};

fn proxy(password: Option<&str>) -> Option<Proxy> {
    let (user, pass, host, port) = super::creds("QUARRY_TEST_SOCKS")?;
    Some(Proxy { host, port, user, password: Some(password.map_or(pass, Into::into)) })
}

fn check(var: &str) {
    let (Some(good), Ok(url)) = (proxy(None), std::env::var(var)) else { return };
    let target = std::env::var("QUARRY_TEST_SOCKS_TARGET").unwrap_or_else(|_| "host.docker.internal".into());
    let a = Addr { host: target, ..Addr::parse(&url).unwrap() };
    let l = Live::new();
    let through = |p: Proxy| l.test(&a, Net { proxy: Some(p), ..Default::default() });
    let ok = through(good);
    assert!(ok.as_ref().is_ok_and(|m| m.contains("through the SOCKS5 proxy")), "{var}: {ok:?}");
    // A wrong proxy password is refused by the proxy, before the database is reached.
    let wrong = through(proxy(Some("wrong")).unwrap());
    assert!(wrong.as_ref().is_err_and(|e| e.contains("SOCKS5 proxy")), "{var} with a wrong proxy password: {wrong:?}");
}

#[test]
fn live_postgres_connects_through_a_socks5_proxy() {
    check("QUARRY_TEST_POSTGRES");
}

#[test]
fn live_mysql_connects_through_a_socks5_proxy() {
    check("QUARRY_TEST_MYSQL");
}

#[test]
fn live_mariadb_connects_through_a_socks5_proxy() {
    check("QUARRY_TEST_MARIADB");
}
