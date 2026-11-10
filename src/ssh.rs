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
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

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
    pub fingerprint: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.task.abort();
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
        if let keys::agent::AgentIdentity::PublicKey { key, .. } = id {
            if let Ok(r) = h.authenticate_publickey_with(user, key, hash, &mut agent).await {
                if r.success() {
                    return Ok(true);
                }
            }
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
/// host and port as is, for clients that cannot speak SOCKS5. Otherwise the gateway is SOCKS5.
pub async fn open(conf: &SshConf, secrets: &SshSecrets, timeout: std::time::Duration, to: Option<(String, u16)>) -> Result<Tunnel, String> {
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
    let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|e| e.to_string())?;
    let local = listener.local_addr().map_err(|e| e.to_string())?.port();
    let session = Arc::new(h);
    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let s = session.clone();
            let to = to.clone();
            tokio::spawn(async move {
                let _ = match to {
                    Some((host, port)) => forward(stream, &s, host, port, &[], &[]).await,
                    None => socks5(stream, s).await,
                };
            });
        }
    });
    Ok(Tunnel { port: local, fingerprint, task })
}

/// A minimal SOCKS5 server (CONNECT only, no authentication, local use only).
async fn socks5(mut s: TcpStream, ssh: Arc<Session>) -> std::io::Result<()> {
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
async fn forward(mut s: TcpStream, ssh: &Session, host: String, port: u16, opened: &[u8], failed: &[u8]) -> std::io::Result<()> {
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
}
