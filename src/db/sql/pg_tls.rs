//! PostgreSQL TLS opened by Quarry, for the two checks sqlx 0.8 gets wrong.
//!
//! sqlx 0.8.6 skips verify-ca's host name check only on rustls's old NotValidForName error, and rustls 0.23.24 and later
//! report NotValidForNameContext, so its verify-ca checks the name like verify-full (fixed in sqlx 0.9, which breaks
//! too much else to take now). sqlx also checks the name it dials, which through an SSH tunnel is 127.0.0.1.
//!
//! PostgreSQL asks for TLS before anything else is said, so a local relay can open it: sqlx speaks plain PostgreSQL to a
//! private socket, and the relay asks the server for TLS, checks its certificate the way the mode says against the
//! host name the user typed, and copies bytes both ways.

use super::SslMode;
use crate::db::Res;
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// sqlx's wording for a TLS setup error, so `db::explain` reads both paths alike.
const TLS_FAILED: &str = "error occurred while attempting to establish a TLS connection";
/// The port in the socket's file name: sqlx names the file `.s.PGSQL.<port>` in the folder it is given.
#[cfg_attr(not(unix), allow(dead_code))]
const SOCKET_PORT: u16 = 5432;

/// Whether `open` needs the relay: verify-ca always, and verify-full when the address dialled is a tunnel's.
pub(super) fn needed(mode: SslMode, tunnel: bool) -> bool {
    mode == SslMode::VerifyCa || mode == SslMode::VerifyIdentity && tunnel
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

    /// What sqlx would have said for the failure the relay turned into a refusal, so the first connect reads the same
    /// with or without the relay.
    pub fn take_failure(&self) -> Option<String> {
        self.failure.lock().unwrap().take()
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

struct Upstream {
    host: String,
    port: u16,
    name: ServerName<'static>,
    tls: TlsConnector,
}

trait Duplex: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Duplex for T {}

/// Starts a relay to `to` that checks the certificate against `name` as `mode` says. Configuration errors (an
/// unreadable CA file, a key that does not match the certificate) are returned here, before anything connects.
pub(super) async fn start(to: (String, u16), name: &str, mode: SslMode, ca: &str, client: Option<(Vec<u8>, Vec<u8>)>) -> Res<Relay> {
    let config = config(mode, ca, client)?;
    let name = ServerName::try_from(name.to_string()).map_err(|e| tls(&e))?;
    let up = Arc::new(Upstream { host: to.0, port: to.1, name, tls: TlsConnector::from(Arc::new(config)) });
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

async fn serve(mut client: Box<dyn Duplex>, up: Arc<Upstream>, failure: Arc<Mutex<Option<String>>>) {
    match upstream(&up).await {
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

/// The roots sqlx trusts (the bundled web roots, plus the CA file when one is set) and the client certificate.
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
    let mut roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if !ca.trim().is_empty() {
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
    }
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
        assert!(needed(SslMode::VerifyCa, false));
        assert!(needed(SslMode::VerifyCa, true));
        assert!(needed(SslMode::VerifyIdentity, true));
        assert!(!needed(SslMode::VerifyIdentity, false));
        for m in [SslMode::Disabled, SslMode::Preferred, SslMode::Required] {
            assert!(!needed(m, true) && !needed(m, false));
        }
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
        // Without the CA the chain fails first, whatever the mode.
        let e = verify(SslMode::VerifyCa, "", "db.example.test").unwrap_err().to_string();
        assert!(e.contains("UnknownIssuer"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
