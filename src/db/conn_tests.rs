//! Each way a MongoDB connection reaches its server, used the way the app uses it: the app's own client from open_client,
//! then the Shell's runner in Bun on the same connection (through the same SSH tunnel). Each side writes and reads back
//! 20 MB, so flow control in the tunnel and proxies is exercised, and the Shell also returns 2 MB through its pipe. A run
//! that never ends fails the test instead of hanging it. A last test stalls a connection after its handshake.
//!
//! QUARRY_TEST_MONGO is the plain server, QUARRY_TEST_MONGO_SET a replica set whose member name the tests reach,
//! QUARRY_TEST_MONGO_TLS a TLS-only server, QUARRY_TEST_SOCKS `user:password@host:port` of a SOCKS5 proxy and
//! QUARRY_TEST_SSH `user:password@host:port` of an SSH server. The proxy and the SSH server reach QUARRY_TEST_MONGO's port at
//! QUARRY_TEST_SOCKS_TARGET (default host.docker.internal). `make servers-up` starts them all and `make test-live` sets the
//! variables. Each test skips itself when its variable is unset; the Shell half also when Bun is missing.
//!
//! Not covered: mongodb+srv, which needs DNS SRV records the test machine resolves.

use super::shell::{ShellJob, run_shell};
use super::worker::open_client;
use super::{Msg, Net, Proxy};
use futures_util::TryStreamExt;
use mongodb::bson::{Document, doc};
use serde_json::json;
use std::time::{Duration, Instant};

const DOCS: usize = 200;
const PAD: usize = 100_000;

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap()
}

/// `user:password@host:port`.
fn creds(var: &str) -> Option<(String, String, String, u16)> {
    let s = std::env::var(var).ok()?;
    let (cred, hp) = s.rsplit_once('@')?;
    let (user, pass) = cred.split_once(':')?;
    let (host, port) = hp.rsplit_once(':')?;
    Some((user.into(), pass.into(), host.into(), port.parse().ok()?))
}

/// QUARRY_TEST_MONGO's host and port.
fn mongo_addr() -> Option<(String, u16)> {
    let uri = std::env::var("QUARRY_TEST_MONGO").ok()?;
    let hp = uri.split_once("://")?.1.split(['/', '?']).next()?;
    let (host, port) = hp.rsplit_once(':')?;
    Some((host.into(), port.parse().ok()?))
}

/// QUARRY_TEST_MONGO as the proxy and the SSH server reach it.
fn far_uri() -> Option<String> {
    let (_, port) = mongo_addr()?;
    let target = std::env::var("QUARRY_TEST_SOCKS_TARGET").unwrap_or_else(|_| "host.docker.internal".into());
    Some(format!("mongodb://{target}:{port}/?directConnection=true"))
}

/// Runs `code` in the Shell on `uri` through `proxy`, or None when Bun is missing.
fn shell(uri: &str, proxy: serde_json::Value, code: &str, timeout_ms: u64) -> Option<(Result<String, String>, Duration)> {
    if std::process::Command::new("bun").arg("--version").output().is_err() {
        println!("skipped: bun is not installed");
        return None;
    }
    let input = json!({ "mongo": [{ "name": "test", "uri": uri, "db": "quarry_test", "proxy": proxy, "collections": [], "readOnly": false }], "sql": [], "code": code, "packages": [], "timeoutMs": timeout_ms, "main": "test" });
    let job = ShellJob { runtime: "bun".into(), dir: std::env::temp_dir().join("quarry-shell-live-test"), input: input.to_string(), packages: Vec::new(), sql: Vec::new() };
    // Kept alive: a dropped sender reads as Stop.
    let (_stop, cancel) = tokio::sync::oneshot::channel();
    let t = Instant::now();
    let send = |_: Msg| {};
    // The first run in a fresh folder installs the packages, so the deadline leaves room for that.
    let r = rt().block_on(async { tokio::time::timeout(Duration::from_secs(180), run_shell(1, &job, &send, cancel)).await });
    let r = r.unwrap_or_else(|_| panic!("the shell never answered on {uri}"));
    Some((r.map(|(ejson, _)| ejson), t.elapsed()))
}

/// The app's client and then the Shell, each writing and reading back DOCS documents of PAD bytes on `uri`.
fn round_trip(way: &str, uri: &str, net: Net) {
    let rt = rt();
    let deadline = Duration::from_secs(120);
    let (client, _, tunnel) = rt.block_on(async { tokio::time::timeout(deadline, open_client(1, uri, Duration::from_secs(15), &net)).await }).unwrap_or_else(|_| panic!("{way}: connecting never ended")).unwrap_or_else(|e| panic!("{way}: {e}"));
    let c = client.database("quarry_test").collection::<Document>(&format!("conn_{way}_app"));
    let pad = "x".repeat(PAD);
    let read = rt.block_on(async {
        tokio::time::timeout(deadline, async {
            c.drop().await?;
            c.insert_many((0..DOCS).map(|i| doc! { "_id": i as i64, "pad": pad.as_str() })).await?;
            c.find(doc! {}).await?.try_collect::<Vec<_>>().await
        })
        .await
    });
    let docs = read.unwrap_or_else(|_| panic!("{way}: the app's read never ended")).unwrap_or_else(|e| panic!("{way}: {e}"));
    assert_eq!(docs.len(), DOCS, "{way}: the app read back");
    // The Shell goes through the app's tunnel, else through the connection's own proxy, as shell_proxy decides.
    let proxy = match (&tunnel, &net.proxy) {
        (Some(t), _) => json!({ "host": "127.0.0.1", "port": t.port }),
        (None, Some(p)) => json!({ "host": p.host, "port": p.port, "user": p.user, "password": p.password }),
        (None, None) => serde_json::Value::Null,
    };
    let code = format!(
        "async function run() {{ const c = db.collection('conn_{way}_shell'); await c.drop().catch(() => {{}}); \
         await c.insertMany(Array.from({{ length: {DOCS} }}, (_, i) => ({{ _id: i, pad: 'x'.repeat({PAD}) }}))); \
         const all = await c.find({{}}).toArray(); return {{ n: all.length, some: all.slice(0, 20) }}; }}"
    );
    let Some((r, _)) = shell(uri, proxy, &code, 30_000) else { return };
    let v = r.unwrap_or_else(|e| panic!("{way} in the Shell: {e}"));
    // Canonical Extended JSON, as the runner sends it.
    assert!(v.contains(&format!("\"n\":{{\"$numberInt\":\"{DOCS}\"}}")) && v.len() > 20 * PAD, "{way} in the Shell: {}", &v[..v.len().min(200)]);
    drop(tunnel);
}

#[test]
fn live_plain_connection_carries_megabytes() {
    let Ok(uri) = std::env::var("QUARRY_TEST_MONGO") else { return };
    round_trip("plain", &uri, Net::default());
}

#[test]
fn live_replica_set_it_discovers_carries_megabytes() {
    let Ok(uri) = std::env::var("QUARRY_TEST_MONGO_SET") else { return };
    round_trip("set", &uri, Net::default());
}

#[test]
fn live_tls_connection_carries_megabytes() {
    let Ok(uri) = std::env::var("QUARRY_TEST_MONGO_TLS") else { return };
    round_trip("tls", &uri, Net::default());
}

#[test]
fn live_socks5_proxy_carries_megabytes() {
    let (Some((user, password, host, port)), Some(uri)) = (creds("QUARRY_TEST_SOCKS"), far_uri()) else { return };
    round_trip("socks", &uri, Net { proxy: Some(Proxy { host, port, user, password: Some(password) }), ..Default::default() });
}

#[test]
fn live_ssh_tunnel_carries_megabytes() {
    let (Some((user, password, host, port)), Some(uri)) = (creds("QUARRY_TEST_SSH"), far_uri()) else { return };
    let conf = crate::ssh::SshConf { host, port, user, auth: crate::ssh::SshAuth::Password, ..Default::default() };
    let secrets = crate::ssh::SshSecrets { password: Some(password), passphrase: None };
    round_trip("ssh", &uri, Net { ssh: Some((conf, secrets)), ..Default::default() });
}

/// A relay to QUARRY_TEST_MONGO that passes each connection's handshake reply and then holds every later reply, like a
/// tunnel or port forward that stops carrying data. Before the runner set a socket timeout this run never ended.
#[test]
fn live_shell_stops_on_a_connection_that_stalls_after_the_handshake() {
    let (Some((host, port)), Ok(plain)) = (mongo_addr(), std::env::var("QUARRY_TEST_MONGO")) else { return };
    // A first run installs the packages, so the timed run below measures only the stall.
    let Some((warm, _)) = shell(&plain, serde_json::Value::Null, "async function run() { return 1; }", 15_000) else { return };
    warm.unwrap();
    let rt = rt();
    let relay = rt.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let relay_port = relay.local_addr().unwrap().port();
    rt.spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut client, _)) = relay.accept().await {
            let Ok(mut server) = tokio::net::TcpStream::connect((host.as_str(), port)).await else { continue };
            tokio::spawn(async move {
                let (mut cr, mut cw) = client.split();
                let (mut sr, mut sw) = server.split();
                let up = tokio::io::copy(&mut cr, &mut sw);
                // A MongoDB message starts with its length: pass the first whole reply, then read and drop the rest.
                let down = async {
                    let mut first = Vec::new();
                    let mut buf = [0u8; 16 << 10];
                    loop {
                        let n = sr.read(&mut buf).await?;
                        if n == 0 {
                            return Ok::<_, std::io::Error>(());
                        }
                        first.extend_from_slice(&buf[..n]);
                        if first.len() >= 4 && first.len() >= i32::from_le_bytes(first[..4].try_into().unwrap()) as usize {
                            break;
                        }
                    }
                    let len = i32::from_le_bytes(first[..4].try_into().unwrap()) as usize;
                    cw.write_all(&first[..len]).await?;
                    while sr.read(&mut buf).await? > 0 {}
                    Ok(())
                };
                let _ = tokio::join!(up, down);
            });
        }
    });
    let uri = format!("mongodb://127.0.0.1:{relay_port}/?directConnection=true");
    let code = "async function run() { return await db.collection('conn_stall').find({}).limit(20).toArray(); }";
    let Some((r, took)) = shell(&uri, serde_json::Value::Null, code, 2_000) else { return };
    assert!(r.is_err(), "a stalled connection gave a result: {r:?}");
    // About two socket timeouts (the driver retries a read once) plus starting Bun.
    assert!(took < Duration::from_secs(20), "the stall took {took:?}: {r:?}");
}
