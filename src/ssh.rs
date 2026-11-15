//! SSH tunnels for connections that are only reachable through a jump server.
//!
//! The original forwards one local port per MongoDB host and forces a direct connection
//! (`prepareSsh`, renderer line 29225). Here the SSH session instead backs a small local SOCKS5
//! gateway (like `ssh -D`), and the MongoDB driver connects through it. The driver keeps the real
//! host names, so replica sets, `mongodb+srv://` addresses and TLS checks keep working.

use russh::client;
use russh::keys::{self, Algorithm, EcdsaCurve, HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize, Default, Debug)]
pub enum SshAuth {
    /// The SSH agent, then the usual key files in ~/.ssh, then the password if one is set.
    #[default]
    Auto,
    Password,
    KeyFile,
    Agent,
}

#[derive(Clone, Serialize, Deserialize, Default, Debug, PartialEq)]
#[serde(default)]
pub struct SshConf {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
    pub key_path: String,
    /// Also accept the old `ssh-rsa` host key type, like the original's "legacy RSA" option.
    pub legacy_rsa: bool,
    /// SHA256 fingerprint of the server's host key, remembered the first time (trust on first use).
    pub known_host: Option<String>,
}

/// Secrets kept in the Keychain, passed in only when connecting.
#[derive(Clone, Default)]
pub struct SshSecrets {
    pub password: Option<String>,
    pub passphrase: Option<String>,
}

/// An open tunnel: a local SOCKS5 port backed by the SSH session. Dropping it closes both.
pub struct Tunnel {
    pub port: u16,
    /// On macOS and Linux, with a fixed destination, a socket in a folder only this user may open that forwards like
    /// the port. MySQL dials it with the host name typed, so its TLS checks the certificate against that name.
    pub socket: Option<std::path::PathBuf>,
    pub fingerprint: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.task.abort();
        if let Some(dir) = self.socket.as_deref().and_then(|s| s.parent()) {
            crate::update::remove(dir);
        }
    }
}

struct HostKey {
    expected: Option<String>,
    seen: Arc<Mutex<Option<String>>>,
}

impl client::Handler for HostKey {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let data = match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.key_data().clone(),
            PublicKeyOrCertificate::Certificate(c) => c.public_key().clone(),
        };
        let fp = data.fingerprint(HashAlg::Sha256).to_string();
        *self.seen.lock().unwrap() = Some(fp.clone());
        // A changed host key may mean someone is in the middle; refuse instead of asking.
        Ok(self.expected.as_ref().is_none_or(|e| *e == fp))
    }
}

/// Host key types in the original's order (rsa-sha2-512, rsa-sha2-256, [ssh-rsa], ecdsa, ed25519).
fn host_key_types(legacy_rsa: bool) -> Vec<Algorithm> {
    let mut v = vec![Algorithm::Rsa { hash: Some(HashAlg::Sha512) }, Algorithm::Rsa { hash: Some(HashAlg::Sha256) }];
    if legacy_rsa {
        v.push(Algorithm::Rsa { hash: None });
    }
    v.extend([
        Algorithm::Ecdsa { curve: EcdsaCurve::NistP256 },
        Algorithm::Ecdsa { curve: EcdsaCurve::NistP384 },
        Algorithm::Ecdsa { curve: EcdsaCurve::NistP521 },
        Algorithm::Ed25519,
    ]);
    v
}

type Session = client::Handle<HostKey>;

async fn auth_password(h: &mut Session, user: &str, password: &str) -> Result<bool, String> {
    if h.authenticate_password(user, password).await.map_err(|e| e.to_string())?.success() {
        return Ok(true);
    }
    // Many servers only offer keyboard-interactive for passwords; answer every prompt with it.
    let mut r = h.authenticate_keyboard_interactive_start(user, None).await.map_err(|e| e.to_string())?;
    for _ in 0..5 {
        match r {
            client::KeyboardInteractiveAuthResponse::Success => return Ok(true),
            client::KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(false),
            client::KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                let answers = prompts.iter().map(|_| password.to_string()).collect();
                r = h.authenticate_keyboard_interactive_respond(answers).await.map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(false)
}

async fn auth_key(h: &mut Session, user: &str, path: &str, passphrase: Option<&str>) -> Result<bool, String> {
    let path = expand_home(path);
    let key = keys::load_secret_key(&path, passphrase.filter(|p| !p.is_empty())).map_err(|e| match e {
        keys::Error::KeyIsEncrypted => format!("The key {path} needs its passphrase."),
        other => format!("Could not read the key {path}: {other}"),
    })?;
    let hash = h.best_supported_rsa_hash().await.map_err(|e| e.to_string())?.flatten();
    Ok(h.authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash)).await.map_err(|e| e.to_string())?.success())
}

async fn auth_agent(h: &mut Session, user: &str) -> Result<bool, String> {
    #[cfg(unix)]
    let mut agent = keys::agent::client::AgentClient::connect_env().await.map_err(|e| format!("No SSH agent available ({e}). Is SSH_AUTH_SOCK set?"))?;
    // Windows' built-in OpenSSH agent listens on this named pipe.
    #[cfg(windows)]
    let mut agent = keys::agent::client::AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await.map_err(|e| format!("No SSH agent available ({e}). Is the OpenSSH Authentication Agent service running?"))?;
    let ids = agent.request_identities().await.map_err(|e| e.to_string())?;
    if ids.is_empty() {
        return Err("The SSH agent has no keys. Add one with ssh-add.".into());
    }
    let hash = h.best_supported_rsa_hash().await.map_err(|e| e.to_string())?.flatten();
    for id in ids {
        if let keys::agent::AgentIdentity::PublicKey { key, .. } = id && let Ok(r) = h.authenticate_publickey_with(user, key, hash, &mut agent).await && r.success() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn expand_home(p: &str) -> String {
    match (p.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest).display().to_string(),
        _ => p.to_string(),
    }
}

async fn authenticate(h: &mut Session, conf: &SshConf, s: &SshSecrets) -> Result<(), String> {
    let pwd = s.password.as_deref().filter(|p| !p.is_empty());
    let ok = match conf.auth {
        SshAuth::Password => auth_password(h, &conf.user, pwd.ok_or("Enter the SSH password.")?).await?,
        SshAuth::KeyFile => auth_key(h, &conf.user, &conf.key_path, s.passphrase.as_deref()).await?,
        SshAuth::Agent => auth_agent(h, &conf.user).await?,
        SshAuth::Auto => {
            let mut ok = auth_agent(h, &conf.user).await.unwrap_or(false);
            if !conf.key_path.is_empty() && !ok {
                ok = auth_key(h, &conf.user, &conf.key_path, s.passphrase.as_deref()).await.unwrap_or(false);
            }
            for f in ["~/.ssh/id_ed25519", "~/.ssh/id_ecdsa", "~/.ssh/id_rsa"] {
                if ok {
                    break;
                }
                if std::path::Path::new(&expand_home(f)).exists() {
                    ok = auth_key(h, &conf.user, f, s.passphrase.as_deref()).await.unwrap_or(false);
                }
            }
            if let (false, Some(p)) = (ok, pwd) {
                ok = auth_password(h, &conf.user, p).await?;
            }
            ok
        }
    };
    if ok { Ok(()) } else { Err(format!("The SSH server refused the login for {}.", conf.user)) }
}

/// Opens the SSH session and a local gateway. With `to` set, every connection is forwarded to that
/// host and port as is, for clients that cannot speak SOCKS5. Otherwise the gateway is SOCKS5. `want_socket` also asks
/// for `Tunnel::socket` when `to` is set; on Windows, or when the socket cannot be made, there is only the port.
pub async fn open(conf: &SshConf, secrets: &SshSecrets, timeout: std::time::Duration, to: Option<(String, u16)>, want_socket: bool) -> Result<Tunnel, String> {
    let seen = Arc::new(Mutex::new(None));
    let config = client::Config {
        keepalive_interval: Some(std::time::Duration::from_secs(30)),
        inactivity_timeout: None,
        preferred: russh::Preferred { key: Cow::Owned(host_key_types(conf.legacy_rsa)), ..Default::default() },
        ..Default::default()
    };
    let handler = HostKey { expected: conf.known_host.clone(), seen: seen.clone() };
    let port = if conf.port == 0 { 22 } else { conf.port };
    let connect = client::connect(Arc::new(config), (conf.host.as_str(), port), handler);
    let mut h = match tokio::time::timeout(timeout.max(std::time::Duration::from_secs(10)), connect).await {
        Err(_) => return Err(format!("The SSH server {}:{port} did not answer in time.", conf.host)),
        Ok(Err(e)) => {
            let fp = seen.lock().unwrap().clone();
            return Err(match (fp, &conf.known_host) {
                (Some(now), Some(before)) if now != *before => format!("The SSH host key of {} changed (was {before}, now {now}). Not connecting. If the server was really reinstalled, clear the saved key in the connection settings.", conf.host),
                _ => format!("SSH connection to {}:{port} failed: {e}", conf.host),
            });
        }
        Ok(Ok(h)) => h,
    };
    authenticate(&mut h, conf, secrets).await?;
    let fingerprint = seen.lock().unwrap().clone().unwrap_or_default();
    let session = Arc::new(h);
    // Without `to` the local end is a SOCKS5 gateway and each client names its own destination.
    let fixed = to.is_some();
    serve(fixed, want_socket, fingerprint, move |s| {
        let (session, to) = (session.clone(), to.clone());
        async move {
            let _ = match to {
                Some((host, port)) => forward(s, &session, host, port, &[], &[]).await,
                None => socks5(s, session).await,
            };
        }
    })
    .await
}

/// Opens a forwarder to `to` through a user-run SOCKS5 proxy, shaped like `open`'s fixed forward so a client that cannot
/// speak SOCKS5 can use it. One dial is made first so a wrong proxy or a refused destination fails here, with the
/// proxy's reason, and not as a closed connection on the first query.
pub async fn socks_forward(p: &crate::db::Proxy, to: (String, u16), want_socket: bool, timeout: std::time::Duration) -> Result<Tunnel, String> {
    let port = if p.port == 0 { 1080 } else { p.port };
    let name = format!("{}:{port}", p.host);
    // Resolved once here, so the blocking lookup of a host name never runs inside the accept loop's tasks.
    let addr = tokio::net::lookup_host((p.host.as_str(), port)).await.map_err(|e| format!("Could not find the SOCKS5 proxy {name}: {e}"))?.next().ok_or_else(|| format!("Could not find the SOCKS5 proxy {name}"))?;
    let (user, password) = (p.user.clone(), p.password.clone().unwrap_or_default());
    let dial = move |to: (String, u16)| {
        let (user, password) = (user.clone(), password.clone());
        async move {
            let mut conf = fast_socks5::client::Config::default();
            conf.set_connect_timeout(timeout.as_secs().max(1));
            let r = if user.is_empty() {
                fast_socks5::client::Socks5Stream::connect(addr, to.0, to.1, conf).await
            } else {
                fast_socks5::client::Socks5Stream::connect_with_password(addr, to.0, to.1, user, password, conf).await
            };
            r.map_err(|e| e.to_string())
        }
    };
    match tokio::time::timeout(timeout, dial(to.clone())).await {
        Err(_) => return Err(format!("The SOCKS5 proxy {name} did not answer in time.")),
        Ok(Err(e)) => return Err(format!("The SOCKS5 proxy {name} refused the connection to {}:{}: {e}", to.0, to.1)),
        Ok(Ok(first)) => drop(first),
    }
    serve(true, want_socket, String::new(), move |mut s| {
        let (dial, to) = (dial.clone(), to.clone());
        async move {
            if let Ok(mut up) = dial(to).await {
                let _ = tokio::io::copy_bidirectional(&mut s, &mut up).await;
            }
        }
    })
    .await
}

/// The local end that SSH and SOCKS5 forwarding share: a socket for a fixed destination where the platform has one,
/// and always a localhost port. `dial` is the only part that differs; it gets each accepted connection and ends it.
async fn serve<D, F>(fixed: bool, want_socket: bool, fingerprint: String, dial: D) -> Result<Tunnel, String>
where
    D: Fn(Box<dyn Local>) -> F + Send + Sync + 'static,
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|e| e.to_string())?;
    let local = listener.local_addr().map_err(|e| e.to_string())?.port();
    // Without the socket the caller dials the port as before, so a long or unwritable TMPDIR costs only the name check.
    #[cfg(unix)]
    let unix = if want_socket && fixed { private_socket().ok() } else { None };
    #[cfg(unix)]
    let socket = unix.as_ref().map(|(_, p)| p.clone());
    #[cfg(not(unix))]
    let socket = {
        let _ = (want_socket, fixed);
        None
    };
    let dial = Arc::new(dial);
    // Both accept loops run in this one task, so aborting it on drop stops both and lets the upstream go.
    let task = tokio::spawn(async move {
        #[cfg(unix)]
        let by_socket = {
            let dial = dial.clone();
            async move {
                let Some((l, _)) = unix else { return };
                while let Ok((stream, _)) = l.accept().await {
                    let dial = dial.clone();
                    tokio::spawn(async move { dial(Box::new(stream)).await });
                }
            }
        };
        #[cfg(not(unix))]
        let by_socket = async {};
        let by_port = async move {
            while let Ok((stream, _)) = listener.accept().await {
                let dial = dial.clone();
                tokio::spawn(async move { dial(Box::new(stream)).await });
            }
        };
        tokio::join!(by_socket, by_port);
    });
    Ok(Tunnel { port: local, socket, fingerprint, task })
}

/// What a client of the local end looks like, so one accept loop can serve a socket and a port.
trait Local: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Local for T {}

/// A socket in a fresh folder only this user may open, so no other local user can ride the tunnel through it.
#[cfg(unix)]
fn private_socket() -> Result<(tokio::net::UnixListener, std::path::PathBuf), String> {
    use std::os::unix::fs::PermissionsExt;
    // A short prefix: the whole socket path must fit in sun_path.
    let dir = crate::update::fresh_temp_dir("qssh")?;
    let path = dir.join("s");
    let bound = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string()).and_then(|_| tokio::net::UnixListener::bind(&path).map_err(|e| format!("Could not open the tunnel's local socket: {e}")));
    match bound {
        Ok(l) => Ok((l, path)),
        Err(e) => {
            crate::update::remove(&dir);
            Err(e)
        }
    }
}

/// A minimal SOCKS5 server (CONNECT only, no authentication, local use only).
async fn socks5<S: AsyncRead + AsyncWrite + Unpin>(mut s: S, ssh: Arc<Session>) -> std::io::Result<()> {
    let mut head = [0u8; 2];
    s.read_exact(&mut head).await?;
    if head[0] != 5 {
        return Ok(());
    }
    let mut methods = vec![0u8; head[1] as usize];
    s.read_exact(&mut methods).await?;
    s.write_all(&[5, 0]).await?;
    let mut req = [0u8; 4];
    s.read_exact(&mut req).await?;
    let host = match req[3] {
        1 => {
            let mut a = [0u8; 4];
            s.read_exact(&mut a).await?;
            std::net::Ipv4Addr::from(a).to_string()
        }
        3 => {
            let mut len = [0u8; 1];
            s.read_exact(&mut len).await?;
            let mut name = vec![0u8; len[0] as usize];
            s.read_exact(&mut name).await?;
            String::from_utf8_lossy(&name).into_owned()
        }
        4 => {
            let mut a = [0u8; 16];
            s.read_exact(&mut a).await?;
            std::net::Ipv6Addr::from(a).to_string()
        }
        _ => return Ok(()),
    };
    let mut port = [0u8; 2];
    s.read_exact(&mut port).await?;
    let port = u16::from_be_bytes(port);
    if req[1] != 1 {
        // Only CONNECT is supported.
        return s.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await;
    }
    forward(s, &ssh, host, port, &[5, 0, 0, 1, 0, 0, 0, 0, 0, 0], &[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]).await
}

/// Opens a direct-tcpip channel to `host:port` and copies bytes both ways. `opened` and `failed` are
/// written to the client first, which lets SOCKS5 answer its handshake while a plain forward sends nothing.
async fn forward<S: AsyncRead + AsyncWrite + Unpin>(mut s: S, ssh: &Session, host: String, port: u16, opened: &[u8], failed: &[u8]) -> std::io::Result<()> {
    match ssh.channel_open_direct_tcpip(host, port as u32, "127.0.0.1", 0).await {
        Ok(ch) => {
            s.write_all(opened).await?;
            let mut stream = ch.into_stream();
            tokio::io::copy_bidirectional(&mut s, &mut stream).await?;
            Ok(())
        }
        Err(_) => s.write_all(failed).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_key_order() {
        assert!(!host_key_types(false).contains(&Algorithm::Rsa { hash: None }));
        assert!(host_key_types(true).contains(&Algorithm::Rsa { hash: None }));
        assert_eq!(host_key_types(false)[0], Algorithm::Rsa { hash: Some(HashAlg::Sha512) });
    }

    #[test]
    fn home_paths() {
        assert!(!expand_home("~/.ssh/id_rsa").starts_with('~'));
        assert_eq!(expand_home("/k"), "/k");
    }

    /// A SOCKS5 server in this process that connects to whatever it is asked for, with or without a password.
    async fn socks_server(login: Option<(&str, &str)>) -> u16 {
        use fast_socks5::server::{Authentication, Config, SimpleUserPassword, Socks5Socket};
        async fn run<A: Authentication + 'static>(l: TcpListener, conf: Config<A>)
        where
            A::Item: Send,
        {
            let conf = Arc::new(conf);
            while let Ok((s, _)) = l.accept().await {
                let conf = conf.clone();
                tokio::spawn(async move {
                    let _ = Socks5Socket::new(s, conf).upgrade_to_socks5().await;
                });
            }
        }
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        match login {
            Some((username, password)) => {
                let conf = Config::<fast_socks5::server::DenyAuthentication>::default().with_authentication(SimpleUserPassword { username: username.into(), password: password.into() });
                tokio::spawn(run(l, conf));
            }
            None => {
                tokio::spawn(run(l, Config::<fast_socks5::server::DenyAuthentication>::default()));
            }
        }
        port
    }

    async fn echo_server() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 64];
                    while let Ok(n) = s.read(&mut buf).await {
                        if n == 0 || s.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        port
    }

    fn proxy(port: u16, user: &str, password: Option<&str>) -> crate::db::Proxy {
        crate::db::Proxy { host: "127.0.0.1".into(), port, user: user.into(), password: password.map(Into::into) }
    }

    async fn round_trip<S: AsyncRead + AsyncWrite + Unpin>(mut s: S) {
        s.write_all(b"ping").await.unwrap();
        let mut got = [0u8; 4];
        s.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"ping");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn socks_forward_carries_bytes_without_a_password() {
        let (server, echo) = (socks_server(None).await, echo_server().await);
        let t = socks_forward(&proxy(server, "", None), ("127.0.0.1".into(), echo), true, std::time::Duration::from_secs(5)).await.unwrap();
        assert!(t.fingerprint.is_empty());
        round_trip(tokio::net::TcpStream::connect(("127.0.0.1", t.port)).await.unwrap()).await;
        #[cfg(unix)]
        round_trip(tokio::net::UnixStream::connect(t.socket.as_ref().expect("a socket was asked for")).await.unwrap()).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn socks_forward_sends_the_password_and_reports_a_refusal() {
        let (server, echo) = (socks_server(Some(("app", "s3cret"))).await, echo_server().await);
        let wait = std::time::Duration::from_secs(5);
        let to = ("127.0.0.1".to_string(), echo);
        let t = socks_forward(&proxy(server, "app", Some("s3cret")), to.clone(), false, wait).await.unwrap();
        assert!(t.socket.is_none());
        round_trip(tokio::net::TcpStream::connect(("127.0.0.1", t.port)).await.unwrap()).await;
        let wrong = socks_forward(&proxy(server, "app", Some("nope")), to.clone(), false, wait).await.err().expect("a wrong password is refused");
        assert!(wrong.contains("SOCKS5 proxy"), "{wrong}");
        assert!(socks_forward(&proxy(server, "", None), to, false, wait).await.is_err(), "the server wants a login");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn socks_forward_fails_at_open_when_the_destination_is_unreachable() {
        let server = socks_server(None).await;
        // A port nothing listens on, found by binding and dropping.
        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().port();
        let r = socks_forward(&proxy(server, "", None), ("127.0.0.1".into(), closed), false, std::time::Duration::from_secs(5)).await;
        assert!(r.err().expect("refused").contains("refused the connection"));
    }
}
