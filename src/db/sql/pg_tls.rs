//! PostgreSQL TLS opened by Quarry, for the two checks sqlx 0.8 gets wrong.
//!
//! sqlx 0.8.6 skips verify-ca's host name check only on rustls's old NotValidForName error, and rustls 0.23.24 and later
//! report NotValidForNameContext, so its verify-ca checks the name like verify-full (fixed in sqlx 0.9, which breaks
//! too much else to take now). sqlx also checks the name it dials, which through an SSH tunnel is 127.0.0.1.
//!
//! PostgreSQL asks for TLS before anything else is said, so a local relay can open it: sqlx speaks plain PostgreSQL to a
//! private socket, and the relay asks the server for TLS, checks its certificate the way the mode says against the
//! host name the user typed, and copies bytes both ways.
//!
//! Trust follows libpq: with a CA file only that file is trusted, and verify-ca without one is refused, because public
//! roots with no name check would accept any site's certificate. The bundled web roots serve verify-full alone.

use super::SslMode;
use crate::db::Res;
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// sqlx's wording for a TLS setup error, so `db::explain` reads both paths alike.
const TLS_FAILED: &str = "error occurred while attempting to establish a TLS connection";
/// The port in the socket's file name: sqlx names the file `.s.PGSQL.<port>` in the folder it is given.
#[cfg_attr(not(unix), allow(dead_code))]
const SOCKET_PORT: u16 = 5432;

/// Whether `open` needs the relay: verify-ca always, and verify-full when the address dialled is a tunnel's or a CA
/// file is set, since sqlx would trust the web roots beside the file.
pub(super) fn needed(mode: SslMode, tunnel: bool, ca: bool) -> bool {
    mode == SslMode::VerifyCa || mode == SslMode::VerifyIdentity && (tunnel || ca)
}

/// The local end sqlx connects to. Dropping it stops accepting and removes the socket folder; connections already
/// relayed end with their pool.
pub(super) struct Relay {
    /// The localhost port on Windows; with a socket, the port in its file name.
    pub port: u16,
    dir: Option<PathBuf>,
    task: tokio::task::JoinHandle<()>,
    /// Why the last attempt to reach the server failed, in sqlx's wording.
    failure: Arc<Mutex<Option<String>>>,
}

impl Relay {
    /// The folder of the socket, which sqlx takes in place of a host; None when the relay listens on a port.
    pub fn socket_dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// The relay's own text for `m` when `m` is the refusal the relay sent, so the first connect reads the same with or
    /// without the relay. Any other error stays as it is, even after an earlier refusal.
    pub fn explain(&self, m: String) -> String {
        pick(self.failure.lock().unwrap().take(), m)
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
        if let Some(d) = &self.dir {
            crate::update::remove(d);
        }
    }
}

/// The refusal travels to sqlx as the message of an ErrorResponse, so sqlx's error holds it word for word.
fn pick(failure: Option<String>, m: String) -> String {
    match failure {
        Some(f) if m.contains(&f) => f,
        _ => m,
    }
}

struct Upstream {
    host: String,
    port: u16,
    name: ServerName<'static>,
    tls: TlsConnector,
    /// The connection's timeout, so a server that stops answering does not leave relay tasks waiting forever.
    timeout: Duration,
}

trait Duplex: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Duplex for T {}

/// Starts a relay to `to` that checks the certificate against `name` as `mode` says. Configuration errors (an
/// unreadable CA file, a key that does not match the certificate) are returned here, before anything connects.
pub(super) async fn start(to: (String, u16), name: &str, mode: SslMode, ca: &str, client: Option<(Vec<u8>, Vec<u8>)>, timeout: Duration) -> Res<Relay> {
    let config = config(mode, ca, client)?;
    let name = ServerName::try_from(name.to_string()).map_err(|e| tls(&e))?;
    let up = Arc::new(Upstream { host: to.0, port: to.1, name, tls: TlsConnector::from(Arc::new(config)), timeout });
    let failure = Arc::new(Mutex::new(None));
    let (listener, dir, port) = listen().await?;
    let fail = failure.clone();
    let task = tokio::spawn(async move {
        loop {
            let client: Box<dyn Duplex> = match &listener {
                #[cfg(unix)]
                Listener::Unix(l) => match l.accept().await {
                    Ok((s, _)) => Box::new(s),
                    Err(_) => break,
                },
                Listener::Tcp(l) => match l.accept().await {
                    #[cfg(windows)]
                    Ok((_, peer)) if !l.local_addr().is_ok_and(|me| own_process(peer, me)) => continue,
                    Ok((s, _)) => Box::new(s),
                    Err(_) => break,
                },
            };
            tokio::spawn(serve(client, up.clone(), fail.clone()));
        }
    });
    Ok(Relay { port, dir, task, failure })
}

enum Listener {
    #[cfg(unix)]
    Unix(tokio::net::UnixListener),
    #[cfg_attr(unix, allow(dead_code))]
    Tcp(tokio::net::TcpListener),
}

/// A socket in a fresh folder only this user may open, so no other local user can borrow the relay's TLS and client
/// certificate. Windows has no unix socket in sqlx, so there it is a localhost port, like the SSH tunnel's.
async fn listen() -> Res<(Listener, Option<PathBuf>, u16)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // A short prefix: the whole socket path must fit in sun_path.
        let dir = crate::update::fresh_temp_dir("qtls")?;
        let bound = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string()).and_then(|_| tokio::net::UnixListener::bind(dir.join(format!(".s.PGSQL.{SOCKET_PORT}"))).map_err(|e| format!("Could not open the local TLS relay: {e}")));
        match bound {
            Ok(l) => Ok((Listener::Unix(l), Some(dir), SOCKET_PORT)),
            Err(e) => {
                crate::update::remove(&dir);
                Err(e)
            }
        }
    }
    #[cfg(not(unix))]
    {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.map_err(|e| format!("Could not open the local TLS relay: {e}"))?;
        let port = l.local_addr().map_err(|e| e.to_string())?.port();
        Ok((Listener::Tcp(l), None, port))
    }
}

/// Whether the client at `peer`, connected to the relay at `me`, is a socket of this process. A localhost port is open
/// to every user of the machine, and each session presents the user's client certificate, so another local user must
/// not get one. Fails closed: when the table cannot be read, nobody is served.
#[cfg(windows)]
fn own_process(peer: std::net::SocketAddr, me: std::net::SocketAddr) -> bool {
    use windows_sys::Win32::NetworkManagement::IpHelper::{GetExtendedTcpTable, TCP_TABLE_OWNER_PID_CONNECTIONS};
    let (std::net::SocketAddr::V4(peer), std::net::SocketAddr::V4(me)) = (peer, me) else { return false };
    const AF_INET: u32 = 2;
    const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
    // MIB_TCPTABLE_OWNER_PID read as u32 words: the row count, then rows of state, local address, local port, remote
    // address, remote port and owning pid. Addresses are stored in network order, ports too in their low 16 bits.
    let mut buf: Vec<u32> = vec![0; 1];
    let mut ok = false;
    // The table can grow between asking its size and reading it.
    for _ in 0..4 {
        let mut size = (buf.len() * 4) as u32;
        match unsafe { GetExtendedTcpTable(buf.as_mut_ptr().cast(), &mut size, 0, AF_INET, TCP_TABLE_OWNER_PID_CONNECTIONS, 0) } {
            0 => {
                ok = true;
                break;
            }
            ERROR_INSUFFICIENT_BUFFER => buf = vec![0; (size as usize).div_ceil(4) + 6],
            _ => return false,
        }
    }
    let n = buf[0] as usize;
    if !ok || buf.len() < 1 + n * 6 {
        return false;
    }
    let addr = |a: &std::net::SocketAddrV4| (u32::from_ne_bytes(a.ip().octets()), a.port());
    let port = |w: u32| u16::from_be(w as u16);
    buf[1..1 + n * 6].chunks_exact(6).any(|r| (r[1], port(r[2])) == addr(&peer) && (r[3], port(r[4])) == addr(&me) && r[5] == std::process::id())
}

async fn serve(mut client: Box<dyn Duplex>, up: Arc<Upstream>, failure: Arc<Mutex<Option<String>>>) {
    let late = || format!("error communicating with database: connection timed out: no TLS session with the server within {} s", up.timeout.as_secs());
    match tokio::time::timeout(up.timeout, upstream(&up)).await.unwrap_or_else(|_| Err(late())) {
        Ok(mut tls) => {
            let _ = tokio::io::copy_bidirectional(&mut client, &mut tls).await;
        }
        Err(msg) => {
            *failure.lock().unwrap() = Some(msg.clone());
            refuse(client, &msg).await;
        }
    }
}

async fn upstream(up: &Upstream) -> Result<tokio_rustls::client::TlsStream<TcpStream>, String> {
    let io = |e: std::io::Error| format!("error communicating with database: {e}");
    let mut s = TcpStream::connect((up.host.as_str(), up.port)).await.map_err(io)?;
    let _ = s.set_nodelay(true);
    // SSLRequest: length 8, then the code 80877103.
    s.write_all(&[0, 0, 0, 8, 4, 210, 22, 47]).await.map_err(io)?;
    let mut answer = [0u8];
    s.read_exact(&mut answer).await.map_err(io)?;
    if answer[0] != b'S' {
        return Err(format!("{TLS_FAILED}: server does not support TLS"));
    }
    // sqlx reports a refused certificate as an I/O error too ("invalid peer certificate: ..."), so the wording matches.
    up.tls.connect(up.name.clone(), s).await.map_err(io)
}

/// Tells sqlx why as a server would, with a FATAL error after its startup message. Reading the startup message first
/// matters: closing a socket with unread data resets it, and the reset can discard the error before sqlx reads it.
async fn refuse(mut client: Box<dyn Duplex>, msg: &str) {
    let mut len = [0u8; 4];
    if client.read_exact(&mut len).await.is_ok() {
        let n = (u32::from_be_bytes(len) as usize).saturating_sub(4).min(64 * 1024);
        let mut rest = vec![0u8; n];
        let _ = client.read_exact(&mut rest).await;
    }
    let _ = client.write_all(&error_response(msg)).await;
    let _ = client.shutdown().await;
}

/// An ErrorResponse with SQLSTATE 08001, sqlclient_unable_to_establish_sqlconnection.
fn error_response(msg: &str) -> Vec<u8> {
    let mut body = Vec::new();
    for (k, v) in [(b'S', "FATAL"), (b'V', "FATAL"), (b'C', "08001"), (b'M', &msg.replace('\0', ""))] {
        body.push(k);
        body.extend_from_slice(v.as_bytes());
        body.push(0);
    }
    body.push(0);
    let mut out = vec![b'E'];
    out.extend_from_slice(&(body.len() as u32 + 4).to_be_bytes());
    out.extend(body);
    out
}

/// The roots the mode trusts (see `verifier_with`) and the client certificate.
fn config(mode: SslMode, ca: &str, client: Option<(Vec<u8>, Vec<u8>)>) -> Res<ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let b = ClientConfig::builder_with_provider(provider.clone()).with_safe_default_protocol_versions().map_err(|e| tls(&e))?.dangerous().with_custom_certificate_verifier(verifier(mode, ca, provider)?);
    match client {
        Some((cert, key)) => {
            let certs = CertificateDer::pem_slice_iter(&cert).collect::<Result<Vec<_>, _>>().map_err(|e| tls(&e))?;
            let key = PrivateKeyDer::from_pem_slice(&key).map_err(|e| tls(&e))?;
            b.with_client_auth_cert(certs, key).map_err(|e| tls(&e))
        }
        None => Ok(b.with_no_client_auth()),
    }
}

fn tls(e: &dyn std::fmt::Display) -> String {
    format!("{TLS_FAILED}: {e}")
}

fn verifier(mode: SslMode, ca: &str, provider: Arc<rustls::crypto::CryptoProvider>) -> Res<Arc<Verifier>> {
    verifier_with(mode, ca, provider, || RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()))
}

/// What libpq trusts: the CA file alone when one is set, whatever the mode, and the public roots (`web`) only for
/// verify-full without a file, where the name check is what makes them safe. verify-ca without a file is refused.
fn verifier_with(mode: SslMode, ca: &str, provider: Arc<rustls::crypto::CryptoProvider>, web: impl FnOnce() -> RootCertStore) -> Res<Arc<Verifier>> {
    let roots = if !ca.trim().is_empty() {
        let mut roots = RootCertStore::empty();
        let path = super::pg_cert::expand(ca.trim());
        let data = std::fs::read(&path).map_err(|e| tls(&format!("could not read the CA file {}: {e}", path.display())))?;
        let mut any = false;
        for c in CertificateDer::pem_slice_iter(&data) {
            let c = c.map_err(|_| tls(&format!("Invalid certificate {}", path.display())))?;
            roots.add(c).map_err(|e| tls(&e))?;
            any = true;
        }
        if !any {
            return Err(tls(&format!("Invalid certificate {}: no BEGIN CERTIFICATE block", path.display())));
        }
        roots
    } else if mode == SslMode::VerifyIdentity {
        web()
    } else {
        return Err(super::pg_cert::VERIFY_CA_NO_CA.into());
    };
    let inner = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider).build().map_err(|e| tls(&e))?;
    Ok(Arc::new(Verifier { inner, name: mode == SslMode::VerifyIdentity }))
}

/// rustls's own check, which tests the chain before the name, so a name error means the chain already passed.
#[derive(Debug)]
struct Verifier {
    inner: Arc<WebPkiServerVerifier>,
    name: bool,
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(&self, end_entity: &CertificateDer<'_>, intermediates: &[CertificateDer<'_>], server_name: &ServerName<'_>, ocsp: &[u8], now: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        match self.inner.verify_server_cert(end_entity, intermediates, server_name, ocsp, now) {
            Err(rustls::Error::InvalidCertificate(CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. })) if !self.name => Ok(ServerCertVerified::assertion()),
            r => r,
        }
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_modes_need_the_relay() {
        for ca in [false, true] {
            assert!(needed(SslMode::VerifyCa, false, ca));
            assert!(needed(SslMode::VerifyCa, true, ca));
            assert!(needed(SslMode::VerifyIdentity, true, ca));
            for m in [SslMode::Disabled, SslMode::Preferred, SslMode::Required] {
                assert!(!needed(m, true, ca) && !needed(m, false, ca));
            }
        }
        // sqlx would trust the web roots beside the file, so verify-full with a CA file goes through the relay.
        assert!(needed(SslMode::VerifyIdentity, false, true));
        assert!(!needed(SslMode::VerifyIdentity, false, false));
    }

    #[test]
    fn only_the_relays_own_refusal_is_reworded() {
        let f = "error communicating with database: invalid peer certificate: UnknownIssuer".to_string();
        assert_eq!(pick(Some(f.clone()), format!("error returned from database: {f}")), f);
        // An earlier refusal does not replace an unrelated error of a later attempt.
        let other = "error returned from database: password authentication failed for user \"app\"".to_string();
        assert_eq!(pick(Some(f), other.clone()), other);
        assert_eq!(pick(None, other.clone()), other);
    }

    /// A server that answers the SSLRequest and then says nothing: the relay gives up within the timeout and refuses.
    #[test]
    fn a_server_that_goes_silent_is_given_up_on() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = l.local_addr().unwrap().port();
            let server = tokio::spawn(async move {
                let (mut s, _) = l.accept().await.unwrap();
                let mut req = [0u8; 8];
                s.read_exact(&mut req).await.unwrap();
                s.write_all(b"S").await.unwrap();
                tokio::time::sleep(Duration::from_secs(60)).await;
            });
            let tls = TlsConnector::from(Arc::new(config(SslMode::VerifyIdentity, "", None).unwrap()));
            let up = Arc::new(Upstream { host: "127.0.0.1".into(), port, name: ServerName::try_from("db.example.test").unwrap(), tls, timeout: Duration::from_millis(300) });
            let (client, mut sqlx) = tokio::io::duplex(4096);
            // A startup message, which the refusal reads before it answers.
            sqlx.write_all(&[0, 0, 0, 8, 0, 3, 0, 0]).await.unwrap();
            let failure = Arc::new(Mutex::new(None));
            tokio::time::timeout(Duration::from_secs(5), serve(Box::new(client), up, failure.clone())).await.expect("the relay waited past its timeout");
            let f = failure.lock().unwrap().clone().unwrap();
            assert!(f.contains("timed out"), "{f}");
            let mut answer = Vec::new();
            sqlx.read_to_end(&mut answer).await.unwrap();
            assert_eq!(answer.first(), Some(&b'E'));
            server.abort();
        });
    }

    /// Windows only: the relay's port is open to every local user, so it serves only sockets of this process.
    #[cfg(windows)]
    #[test]
    fn a_socket_of_this_process_is_recognised() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let me = l.local_addr().unwrap();
        let c = std::net::TcpStream::connect(me).unwrap();
        let (_s, peer) = l.accept().unwrap();
        assert_eq!(peer, c.local_addr().unwrap());
        assert!(own_process(peer, me));
        assert!(!own_process("127.0.0.1:1".parse().unwrap(), me));
    }

    /// A public CA stands in for the bundled web roots and has signed a certificate for another site.
    #[test]
    fn a_ca_file_is_the_only_root_and_verify_ca_needs_one() {
        use super::super::pg_cert::tests::{make_certs, temp_dir};
        let d = temp_dir("roots");
        let run = |args: &[&str]| std::process::Command::new("openssl").args(args).current_dir(&d).output().ok().filter(|o| o.status.success()).map(|_| ());
        let made = make_certs(&d, "db.example.test", "analyst").and_then(|_| {
            run(&["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", "public-ca.key", "-out", "public-ca.crt", "-days", "2", "-subj", "/CN=quarry-public-ca"])?;
            std::fs::write(d.join("site.ext"), "subjectAltName=DNS:site.example.org\n").ok()?;
            run(&["req", "-newkey", "rsa:2048", "-nodes", "-keyout", "site.key", "-out", "site.csr", "-subj", "/CN=site.example.org"])?;
            run(&["x509", "-req", "-in", "site.csr", "-CA", "public-ca.crt", "-CAkey", "public-ca.key", "-CAcreateserial", "-out", "site.crt", "-days", "2", "-extfile", "site.ext"])
        });
        if made.is_none() {
            eprintln!("openssl not found, skipped");
            return;
        }
        let ca = d.join("ca.crt").display().to_string();
        let public = CertificateDer::from_pem_file(d.join("public-ca.crt")).unwrap();
        let site = CertificateDer::from_pem_file(d.join("site.crt")).unwrap();
        let verify = |mode: SslMode, ca: &str, host: &str| {
            let web = || {
                let mut r = RootCertStore::empty();
                r.add(public.clone()).unwrap();
                r
            };
            let v = verifier_with(mode, ca, Arc::new(rustls::crypto::ring::default_provider()), web)?;
            v.verify_server_cert(&site, &[], &ServerName::try_from(host.to_string()).unwrap(), &[], UnixTime::now()).map(|_| ()).map_err(|e| e.to_string())
        };
        // With the web roots beside the file, verify-ca forgave the name and took any public site's certificate.
        let e = verify(SslMode::VerifyCa, &ca, "db.example.test").unwrap_err();
        assert!(e.contains("UnknownIssuer"), "{e}");
        // verify-full with a file trusts the file alone, even for the name the certificate holds.
        let e = verify(SslMode::VerifyIdentity, &ca, "site.example.org").unwrap_err();
        assert!(e.contains("UnknownIssuer"), "{e}");
        assert_eq!(verify(SslMode::VerifyCa, "", "db.example.test").unwrap_err(), super::super::pg_cert::VERIFY_CA_NO_CA);
        assert_eq!(verify(SslMode::VerifyCa, "  ", "site.example.org").unwrap_err(), super::super::pg_cert::VERIFY_CA_NO_CA);
        // verify-full without a file keeps the public roots, with the name checked.
        assert!(verify(SslMode::VerifyIdentity, "", "site.example.org").is_ok());
        let e = verify(SslMode::VerifyIdentity, "", "db.example.test").unwrap_err();
        assert!(e.contains("not valid for name"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_refusal_is_a_postgres_error_response() {
        let r = error_response("bad\0 cert");
        assert_eq!(r[0], b'E');
        assert_eq!(u32::from_be_bytes(r[1..5].try_into().unwrap()) as usize, r.len() - 1);
        let body = String::from_utf8_lossy(&r[5..]);
        assert!(body.contains("SFATAL\0") && body.contains("C08001\0") && body.contains("Mbad cert\0"));
        assert!(r.ends_with(&[0, 0]));
    }

    /// Checks the verifier against certificates made with openssl: a server certificate for db.example.test only.
    #[test]
    fn verify_ca_skips_the_name_and_verify_full_does_not() {
        let d = super::super::pg_cert::tests::temp_dir("verify");
        if super::super::pg_cert::tests::make_certs(&d, "db.example.test", "analyst").is_none() {
            eprintln!("openssl not found, skipped");
            return;
        }
        let ca = d.join("ca.crt").display().to_string();
        let leaf = CertificateDer::from_pem_file(d.join("server.crt")).unwrap();
        let verify = |mode: SslMode, ca: &str, host: &str| {
            let v = verifier(mode, ca, Arc::new(rustls::crypto::ring::default_provider())).unwrap();
            let name = ServerName::try_from(host.to_string()).unwrap();
            v.verify_server_cert(&leaf, &[], &name, &[], UnixTime::now())
        };
        // make_certs names db.example.test, localhost and 127.0.0.1, so another address stands for a mismatch.
        assert!(verify(SslMode::VerifyCa, &ca, "10.9.8.7").is_ok());
        assert!(config(SslMode::VerifyCa, &ca, None).is_ok());
        assert!(verify(SslMode::VerifyIdentity, &ca, "db.example.test").is_ok());
        let e = verify(SslMode::VerifyIdentity, &ca, "10.9.8.7").unwrap_err().to_string();
        assert!(e.contains("certificate not valid for name \"10.9.8.7\""), "{e}");
        // Without the CA the chain fails first.
        let e = verify(SslMode::VerifyIdentity, "", "db.example.test").unwrap_err().to_string();
        assert!(e.contains("UnknownIssuer"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
