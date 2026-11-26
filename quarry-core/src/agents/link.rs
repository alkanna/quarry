//! The local link between `quarry mcp` and the running app: a Unix socket or a per-user named pipe, a handshake in
//! which each end proves it holds the token in `agents/token`, then one JSON object per line.
//!
//! Uses outside crates only, so `tests/mcp_stdio.rs` can build this file on its own and play the app.
//!
//! Handshake, one line each:
//! 1. bridge: `{link_version, nonce}`
//! 2. app: `{link_version, nonce, proof}` with proof = HMAC(token, "quarry-app" + bridge nonce), or `{link_version, error}`
//!    where error is "version" or "off"
//! 3. bridge: `{proof}` with proof = HMAC(token, "quarry-bridge" + app nonce)
//! 4. app: `{ok: true}` or `{error: "auth"}`
//!
//! Then the bridge sends `{id, tool, args, client, progress?}` and `{cancel: id}`, the app answers `{id, ok}` or
//! `{id, err: {code, message}}`.

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt};

/// Raised whenever the messages between bridge and app change shape. A self-update replaces the binary under
/// running bridges, so the two can differ until the agent restarts its bridge.
pub const LINK_VERSION: u32 = 1;
/// Longest line either side accepts, on stdio and on the link.
pub const MAX_LINE: usize = 8 << 20;
/// The handshake is a few small lines; anything slower is not a Quarry on the other end.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const APP_LABEL: &[u8] = b"quarry-app";
const BRIDGE_LABEL: &[u8] = b"quarry-bridge";

pub type Token = [u8; 32];

/// What the agent said it is, passed with each call. Never trusted for decisions.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

/// A refused or failed call, sent back to the agent as a tool error.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolErr {
    pub code: String,
    pub message: String,
}

impl ToolErr {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into() }
    }
}

/// `config_dir()/quarry/agents`, or QUARRY_AGENTS_DIR: tests point it at a temp dir, since Windows finds the
/// config dir without looking at HOME or APPDATA.
pub fn agents_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("QUARRY_AGENTS_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    default_agents_dir()
}

fn default_agents_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("quarry").join("agents")
}

/// Creates `dir` if needed and keeps it to this user.
pub fn private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Written beside the file with mode 0600, then renamed over it, so the file is never readable by others nor
/// left half written.
pub fn write_private(dir: &Path, file: &str, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    private_dir(dir)?;
    let tmp = dir.join(format!(".{file}.tmp"));
    let _ = std::fs::remove_file(&tmp);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let res = o.open(&tmp).and_then(|mut f| f.write_all(bytes)).and_then(|()| std::fs::rename(&tmp, dir.join(file)));
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

pub fn random() -> [u8; 32] {
    use chacha20poly1305::aead::Generate;
    chacha20poly1305::Key::generate().into()
}

pub fn read_token(dir: &Path) -> io::Result<Token> {
    let hex = std::fs::read_to_string(dir.join("token"))?;
    unhex(hex.trim()).and_then(|b| b.try_into().ok()).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "the token file is damaged"))
}

/// The app's token, created the first time agents are turned on.
pub fn ensure_token(dir: &Path) -> io::Result<Token> {
    if let Ok(t) = read_token(dir) {
        return Ok(t);
    }
    let t = random();
    write_private(dir, "token", hex(&t).as_bytes())?;
    Ok(t)
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

fn mac(token: &Token, label: &[u8], nonce: &[u8]) -> Hmac<Sha256> {
    let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(token).expect("HMAC takes any key length");
    m.update(label);
    m.update(nonce);
    m
}

/// The label keeps a proof one side made from being replayed as the other side's.
pub fn proof(token: &Token, label: &[u8], nonce: &[u8]) -> String {
    hex(&mac(token, label, nonce).finalize().into_bytes())
}

/// Compared in constant time.
pub fn verify(token: &Token, label: &[u8], nonce: &[u8], proof_hex: &str) -> bool {
    unhex(proof_hex).is_some_and(|p| mac(token, label, nonce).verify_slice(&p).is_ok())
}

pub enum Line {
    Text(String),
    /// Longer than the cap. The rest of it was read and dropped, so the next line starts clean.
    TooLong,
    Eof,
}

fn finish_line(mut buf: Vec<u8>, over: bool) -> Line {
    if over {
        return Line::TooLong;
    }
    while buf.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
        buf.pop();
    }
    Line::Text(String::from_utf8(buf).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()))
}

/// One line, without ever holding more than `max` bytes of it.
pub async fn read_line<R: AsyncBufRead + Unpin>(r: &mut R, max: usize) -> io::Result<Line> {
    let (mut buf, mut over) = (Vec::new(), false);
    loop {
        let chunk = r.fill_buf().await?;
        if chunk.is_empty() {
            return Ok(if buf.is_empty() && !over { Line::Eof } else { finish_line(buf, over) });
        }
        let (take, done) = match chunk.iter().position(|b| *b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        if !over && buf.len() + take > max + 1 {
            (over, buf) = (true, Vec::new());
        }
        if !over {
            buf.extend_from_slice(&chunk[..take]);
        }
        r.consume(take);
        if done {
            return Ok(finish_line(buf, over));
        }
    }
}

/// `read_line` for stdin, which the bridge reads on a plain thread.
pub fn read_line_sync<R: std::io::BufRead>(r: &mut R, max: usize) -> io::Result<Line> {
    let (mut buf, mut over) = (Vec::new(), false);
    loop {
        let chunk = r.fill_buf()?;
        if chunk.is_empty() {
            return Ok(if buf.is_empty() && !over { Line::Eof } else { finish_line(buf, over) });
        }
        let (take, done) = match chunk.iter().position(|b| *b == b'\n') {
            Some(i) => (i + 1, true),
            None => (chunk.len(), false),
        };
        if !over && buf.len() + take > max + 1 {
            (over, buf) = (true, Vec::new());
        }
        if !over {
            buf.extend_from_slice(&chunk[..take]);
        }
        r.consume(take);
        if done {
            return Ok(finish_line(buf, over));
        }
    }
}

pub async fn write_msg<W: AsyncWrite + Unpin>(w: &mut W, v: &Value) -> io::Result<()> {
    let mut b = serde_json::to_vec(v)?;
    b.push(b'\n');
    w.write_all(&b).await?;
    w.flush().await
}

/// The next line as a JSON object; None at the end of the stream.
pub async fn read_msg<R: AsyncBufRead + Unpin>(r: &mut R) -> io::Result<Option<Value>> {
    match read_line(r, MAX_LINE).await? {
        Line::Eof => Ok(None),
        Line::TooLong => Err(io::Error::new(io::ErrorKind::InvalidData, "message too large")),
        Line::Text(t) => serde_json::from_str::<Value>(&t).ok().filter(Value::is_object).map(Some).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not a JSON object")),
    }
}

async fn hello_msg<R: AsyncBufRead + Unpin>(r: &mut R) -> io::Result<Value> {
    match tokio::time::timeout(HELLO_TIMEOUT, read_msg(r)).await {
        Ok(Ok(Some(v))) => Ok(v),
        Ok(Ok(None)) => Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the other end closed the link")),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "no answer during the handshake")),
    }
}

fn nonce_of(v: &Value) -> Option<Vec<u8>> {
    v["nonce"].as_str().and_then(unhex).filter(|n| n.len() == 32)
}

/// The app's side of the handshake. `token` is None while agents are off, which the bridge is told. True once
/// the bridge proved it holds the token; anything else has already been answered and the link should close.
pub async fn accept_hello<R: AsyncBufRead + Unpin, W: AsyncWrite + Unpin>(r: &mut R, w: &mut W, token: Option<Token>) -> io::Result<bool> {
    let hi = hello_msg(r).await?;
    if hi["link_version"].as_u64() != Some(LINK_VERSION as u64) {
        write_msg(w, &json!({"link_version": LINK_VERSION, "error": "version"})).await?;
        return Ok(false);
    }
    let Some(token) = token else {
        write_msg(w, &json!({"link_version": LINK_VERSION, "error": "off"})).await?;
        return Ok(false);
    };
    let Some(theirs) = nonce_of(&hi) else {
        write_msg(w, &json!({"link_version": LINK_VERSION, "error": "auth"})).await?;
        return Ok(false);
    };
    let ours = random();
    write_msg(w, &json!({"link_version": LINK_VERSION, "nonce": hex(&ours), "proof": proof(&token, APP_LABEL, &theirs)})).await?;
    let ans = hello_msg(r).await?;
    if !ans["proof"].as_str().is_some_and(|p| verify(&token, BRIDGE_LABEL, &ours, p)) {
        write_msg(w, &json!({"error": "auth"})).await?;
        return Ok(false);
    }
    write_msg(w, &json!({"ok": true})).await?;
    Ok(true)
}

#[derive(Debug)]
pub enum Refused {
    /// The app speaks another link version.
    Version { app: u64 },
    Off,
    /// One side could not prove it holds the token.
    Auth,
    Io(io::Error),
}

/// The bridge's side. `token` is read only once the app answered, since the app creates it when agents are turned on.
pub async fn hello<R: AsyncBufRead + Unpin, W: AsyncWrite + Unpin>(r: &mut R, w: &mut W, token: impl FnOnce() -> Option<Token>) -> Result<(), Refused> {
    let ours = random();
    write_msg(w, &json!({"link_version": LINK_VERSION, "nonce": hex(&ours)})).await.map_err(Refused::Io)?;
    let ans = hello_msg(r).await.map_err(Refused::Io)?;
    match ans["error"].as_str() {
        Some("version") => return Err(Refused::Version { app: ans["link_version"].as_u64().unwrap_or(0) }),
        Some("off") => return Err(Refused::Off),
        Some(_) => return Err(Refused::Auth),
        None => {}
    }
    if ans["link_version"].as_u64() != Some(LINK_VERSION as u64) {
        return Err(Refused::Version { app: ans["link_version"].as_u64().unwrap_or(0) });
    }
    let (Some(token), Some(theirs)) = (token(), nonce_of(&ans)) else { return Err(Refused::Auth) };
    if !ans["proof"].as_str().is_some_and(|p| verify(&token, APP_LABEL, &ours, p)) {
        return Err(Refused::Auth);
    }
    write_msg(w, &json!({"proof": proof(&token, BRIDGE_LABEL, &theirs)})).await.map_err(Refused::Io)?;
    match hello_msg(r).await.map_err(Refused::Io)? {
        v if v["ok"] == true => Ok(()),
        _ => Err(Refused::Auth),
    }
}

pub trait Duplex: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Duplex for T {}
pub type Stream = Box<dyn Duplex>;

pub enum BindErr {
    /// Another Quarry answers on the endpoint.
    Busy,
    Io(io::Error),
}

impl From<io::Error> for BindErr {
    fn from(e: io::Error) -> Self {
        BindErr::Io(e)
    }
}

pub enum ConnectErr {
    /// Nothing listens: Quarry is closed, or it never ran with this config dir.
    NotOpen,
    Io(io::Error),
}

#[cfg(unix)]
fn uid() -> u32 {
    // SAFETY: getuid cannot fail and touches no memory.
    unsafe { libc::getuid() }
}

fn short_hash(s: &[u8]) -> String {
    hex(&Sha256::digest(s)[..8])
}

/// Where the socket goes. sun_path holds 104 bytes on macOS and 108 on Linux, and a macOS config dir with a long
/// user name can pass that, hence the fallback under /tmp.
#[cfg(unix)]
pub fn socket_path(dir: &Path) -> io::Result<PathBuf> {
    let fits = |p: &Path| p.as_os_str().len() < 100;
    // XDG_RUNTIME_DIR is on tmpfs and already private; a config dir on NFS may not take sockets at all. Only for
    // the real config dir, so tests with their own dir never meet a running Quarry there.
    #[cfg(target_os = "linux")]
    if dir == default_agents_dir()
        && let Some(run) = dirs::runtime_dir()
    {
        let d = run.join("quarry");
        if fits(&d.join("mcp.sock")) {
            private_dir(&d)?;
            return Ok(d.join("mcp.sock"));
        }
    }
    if fits(&dir.join("mcp.sock")) {
        private_dir(dir)?;
        return Ok(dir.join("mcp.sock"));
    }
    // Named per config dir, so a test's temp dir never meets the real one.
    let d = PathBuf::from(format!("/tmp/quarry-{}", uid()));
    tmp_dir(&d)?;
    Ok(d.join(format!("{}.sock", short_hash(dir.as_os_str().as_encoded_bytes()))))
}

/// /tmp is shared, so a directory someone else made there first, or a link, is refused rather than used.
#[cfg(unix)]
fn tmp_dir(d: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    match std::fs::DirBuilder::new().mode(0o700).create(d) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let m = std::fs::symlink_metadata(d)?;
    if !m.is_dir() || m.uid() != uid() || m.mode() & 0o077 != 0 {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("{} is not a private folder of this user", d.display())));
    }
    Ok(())
}

/// The pipe name holds a hash of this user's SID, so two users of one machine never meet, and of the config dir,
/// so a test never meets the real app.
#[cfg(windows)]
pub fn pipe_name(dir: &Path) -> io::Result<String> {
    let sid = win::user_sid()?;
    let mut key = sid.into_bytes();
    key.extend_from_slice(dir.as_os_str().as_encoded_bytes());
    Ok(format!(r"\\.\pipe\quarry-mcp-{}", short_hash(&key)))
}

pub struct Listener {
    #[cfg(unix)]
    inner: tokio::net::UnixListener,
    #[cfg(unix)]
    path: PathBuf,
    #[cfg(windows)]
    name: String,
    #[cfg(windows)]
    next: tokio::net::windows::named_pipe::NamedPipeServer,
}

impl Listener {
    /// Takes the endpoint unless another Quarry answers there, and writes where it is to `dir/endpoint`.
    #[cfg(unix)]
    pub async fn bind(dir: &Path) -> Result<Self, BindErr> {
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};
        let path = socket_path(dir)?;
        if tokio::net::UnixStream::connect(&path).await.is_ok() {
            return Err(BindErr::Busy);
        }
        // Nobody answers, so it is left from a Quarry that quit without cleaning up. Only a socket is removed.
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_socket() => std::fs::remove_file(&path)?,
            Ok(_) => return Err(BindErr::Io(io::Error::new(io::ErrorKind::AlreadyExists, format!("{} exists and is not a socket", path.display())))),
            Err(_) => {}
        }
        let inner = tokio::net::UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        write_private(dir, "endpoint", path.as_os_str().as_encoded_bytes())?;
        Ok(Self { inner, path })
    }

    #[cfg(windows)]
    pub async fn bind(dir: &Path) -> Result<Self, BindErr> {
        let name = pipe_name(dir)?;
        let next = match win::create(&name, true) {
            Ok(s) => s,
            // ERROR_ACCESS_DENIED: first_pipe_instance refuses because the pipe already exists.
            Err(e) if e.raw_os_error() == Some(5) => return Err(BindErr::Busy),
            Err(e) => return Err(e.into()),
        };
        write_private(dir, "endpoint", name.as_bytes())?;
        Ok(Self { name, next })
    }

    /// The next client. On Unix, a process of another user is turned away before a byte is read.
    #[cfg(unix)]
    pub async fn accept(&mut self) -> io::Result<Stream> {
        loop {
            let (s, _) = self.inner.accept().await?;
            if s.peer_cred().is_ok_and(|c| c.uid() == uid()) {
                return Ok(Box::new(s));
            }
        }
    }

    /// The pipe's security descriptor already keeps other users out, and remote clients are rejected.
    #[cfg(windows)]
    pub async fn accept(&mut self) -> io::Result<Stream> {
        // A client that gave up mid-connect leaves the instance unusable, so a fresh one replaces it either way.
        let res = self.next.connect().await;
        let fresh = win::create(&self.name, false)?;
        let cur = std::mem::replace(&mut self.next, fresh);
        res?;
        Ok(Box::new(cur))
    }
}

#[cfg(unix)]
impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Connects to the app named in `dir/endpoint` and checks it runs as this user.
pub async fn connect(dir: &Path) -> Result<Stream, ConnectErr> {
    let Ok(ep) = std::fs::read_to_string(dir.join("endpoint")) else { return Err(ConnectErr::NotOpen) };
    let ep = ep.trim();
    let gone = |e: io::Error| match e.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => ConnectErr::NotOpen,
        _ => ConnectErr::Io(e),
    };
    #[cfg(unix)]
    {
        let s = tokio::net::UnixStream::connect(ep).await.map_err(gone)?;
        if !s.peer_cred().is_ok_and(|c| c.uid() == uid()) {
            return Err(ConnectErr::Io(io::Error::new(io::ErrorKind::PermissionDenied, "the socket belongs to another user")));
        }
        Ok(Box::new(s))
    }
    #[cfg(windows)]
    {
        // ERROR_PIPE_BUSY: every instance is taken for a moment, between a client connecting and the next instance.
        for _ in 0..40 {
            match tokio::net::windows::named_pipe::ClientOptions::new().open(ep) {
                Ok(c) => return Ok(Box::new(c)),
                Err(e) if e.raw_os_error() == Some(231) => tokio::time::sleep(Duration::from_millis(25)).await,
                Err(e) => return Err(gone(e)),
            }
        }
        Err(ConnectErr::Io(io::Error::new(io::ErrorKind::TimedOut, "Quarry's pipe stayed busy")))
    }
}

#[cfg(windows)]
mod win {
    use std::io;
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
    use windows_sys::Win32::Security::{GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// This process's user as an "S-1-5-..." string.
    pub fn user_sid() -> io::Result<String> {
        // SAFETY: every out pointer points at a live local of the right type and size, the token handle is
        // closed on every path, and the string the system allocates is freed with LocalFree as documented.
        unsafe {
            let mut tok: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut buf = vec![0u64; 64];
            let mut len = 0u32;
            let ok = GetTokenInformation(tok, TokenUser, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32, &mut len);
            CloseHandle(tok);
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            let user = &*(buf.as_ptr() as *const TOKEN_USER);
            let mut s: *mut u16 = std::ptr::null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut s) == 0 {
                return Err(io::Error::last_os_error());
            }
            let n = (0..).take_while(|&i| *s.add(i) != 0).count();
            let out = String::from_utf16_lossy(std::slice::from_raw_parts(s, n));
            LocalFree(s.cast());
            Ok(out)
        }
    }

    /// A pipe instance only this user can open: a protected DACL granting all access to the user's SID alone.
    pub fn create(name: &str, first: bool) -> io::Result<NamedPipeServer> {
        let sddl = wide(&format!("D:P(A;;GA;;;{})", user_sid()?));
        // SAFETY: the descriptor the system allocates lives until LocalFree below, after the pipe is created;
        // SECURITY_ATTRIBUTES is a plain struct read only during the create call.
        unsafe {
            let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut sa = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd, bInheritHandle: 0 };
            let res = ServerOptions::new().first_pipe_instance(first).reject_remote_clients(true).create_with_security_attributes_raw(name, (&mut sa as *mut SECURITY_ATTRIBUTES).cast());
            LocalFree(sd);
            res
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
    }

    #[test]
    fn lines_are_capped_and_resync() {
        rt().block_on(async {
            let data = format!("{}\nshort\r\n{}", "x".repeat(40), "tail");
            let mut r = BufReader::with_capacity(8, data.as_bytes());
            assert!(matches!(read_line(&mut r, 16).await.unwrap(), Line::TooLong));
            assert!(matches!(read_line(&mut r, 16).await.unwrap(), Line::Text(t) if t == "short"));
            assert!(matches!(read_line(&mut r, 16).await.unwrap(), Line::Text(t) if t == "tail"));
            assert!(matches!(read_line(&mut r, 16).await.unwrap(), Line::Eof));
        });
        let data = format!("{}\nok\n", "y".repeat(100));
        let mut r = std::io::BufReader::with_capacity(8, data.as_bytes());
        assert!(matches!(read_line_sync(&mut r, 99).unwrap(), Line::TooLong));
        assert!(matches!(read_line_sync(&mut r, 99).unwrap(), Line::Text(t) if t == "ok"));
        assert!(matches!(read_line_sync(&mut r, 99).unwrap(), Line::Eof));
        // Exactly at the cap still fits.
        let data = format!("{}\n", "z".repeat(16));
        let mut r = std::io::BufReader::new(data.as_bytes());
        assert!(matches!(read_line_sync(&mut r, 16).unwrap(), Line::Text(t) if t.len() == 16));
    }

    #[test]
    fn proofs_are_bound_to_label_and_token() {
        let (t, other, n) = ([1u8; 32], [2u8; 32], [9u8; 32]);
        let p = proof(&t, APP_LABEL, &n);
        assert!(verify(&t, APP_LABEL, &n, &p));
        assert!(!verify(&t, BRIDGE_LABEL, &n, &p));
        assert!(!verify(&other, APP_LABEL, &n, &p));
        assert!(!verify(&t, APP_LABEL, &[8u8; 32], &p));
        assert!(!verify(&t, APP_LABEL, &n, "zz"));
        assert_eq!(unhex(&hex(&[0, 255, 16])), Some(vec![0, 255, 16]));
    }

    /// Runs both ends of the handshake over an in-memory pipe.
    fn handshake(app_token: Option<Token>, bridge_token: Option<Token>) -> (io::Result<bool>, Result<(), Refused>) {
        rt().block_on(async {
            let (a, b) = tokio::io::duplex(4096);
            let (ar, mut aw) = tokio::io::split(a);
            let (br, mut bw) = tokio::io::split(b);
            let app = async move { accept_hello(&mut BufReader::new(ar), &mut aw, app_token).await };
            let bridge = async move { hello(&mut BufReader::new(br), &mut bw, || bridge_token).await };
            tokio::join!(app, bridge)
        })
    }

    #[test]
    fn handshake_succeeds_with_the_same_token() {
        let (app, bridge) = handshake(Some([7; 32]), Some([7; 32]));
        assert!(app.unwrap());
        assert!(bridge.is_ok());
    }

    #[test]
    fn handshake_fails_with_another_token() {
        let (app, bridge) = handshake(Some([7; 32]), Some([8; 32]));
        // The bridge sees the app's proof does not match and stops before proving anything.
        assert!(matches!(bridge, Err(Refused::Auth)));
        assert!(app.is_err() || !app.unwrap());
        let (_, bridge) = handshake(Some([7; 32]), None);
        assert!(matches!(bridge, Err(Refused::Auth)));
    }

    #[test]
    fn handshake_says_off() {
        let (app, bridge) = handshake(None, Some([7; 32]));
        assert!(!app.unwrap());
        assert!(matches!(bridge, Err(Refused::Off)));
    }

    #[test]
    fn app_refuses_a_bridge_that_cannot_prove() {
        rt().block_on(async {
            let (a, b) = tokio::io::duplex(4096);
            let (ar, mut aw) = tokio::io::split(a);
            let (br, mut bw) = tokio::io::split(b);
            let app = async move { accept_hello(&mut BufReader::new(ar), &mut aw, Some([3; 32])).await };
            let bridge = async move {
                let mut br = BufReader::new(br);
                write_msg(&mut bw, &json!({"link_version": LINK_VERSION, "nonce": hex(&[1; 32])})).await.unwrap();
                read_msg(&mut br).await.unwrap().unwrap();
                write_msg(&mut bw, &json!({"proof": hex(&[0; 32])})).await.unwrap();
                read_msg(&mut br).await.unwrap().unwrap()
            };
            let (ok, last) = tokio::join!(app, bridge);
            assert!(!ok.unwrap());
            assert_eq!(last["error"], "auth");
        });
    }

    #[test]
    fn version_mismatch_is_reported_both_ways() {
        rt().block_on(async {
            let (a, b) = tokio::io::duplex(4096);
            let (ar, mut aw) = tokio::io::split(a);
            let (br, mut bw) = tokio::io::split(b);
            let app = async move { accept_hello(&mut BufReader::new(ar), &mut aw, Some([3; 32])).await };
            let bridge = async move {
                let mut br = BufReader::new(br);
                write_msg(&mut bw, &json!({"link_version": LINK_VERSION + 1, "nonce": hex(&[1; 32])})).await.unwrap();
                read_msg(&mut br).await.unwrap().unwrap()
            };
            let (ok, ans) = tokio::join!(app, bridge);
            assert!(!ok.unwrap());
            assert_eq!(ans["error"], "version");
            assert_eq!(ans["link_version"], LINK_VERSION);
        });
        // An app that answers with another version.
        rt().block_on(async {
            let (a, b) = tokio::io::duplex(4096);
            let (ar, mut aw) = tokio::io::split(a);
            let (br, mut bw) = tokio::io::split(b);
            let app = async move {
                let mut ar = BufReader::new(ar);
                read_msg(&mut ar).await.unwrap();
                write_msg(&mut aw, &json!({"link_version": 99, "error": "version"})).await.unwrap();
            };
            let bridge = async move { hello(&mut BufReader::new(br), &mut bw, || Some([3; 32])).await };
            let ((), res) = tokio::join!(app, bridge);
            assert!(matches!(res, Err(Refused::Version { app: 99 })));
        });
    }

    #[cfg(unix)]
    #[test]
    fn bind_refuses_when_served_and_replaces_a_stale_socket() {
        let dir = std::env::temp_dir().join(format!("quarry-link-{}", hex(&random()[..6])));
        rt().block_on(async {
            let first = Listener::bind(&dir).await.ok().expect("first bind");
            assert!(matches!(Listener::bind(&dir).await, Err(BindErr::Busy)));
            let path = first.path.clone();
            drop(first);
            assert!(!path.exists());
            // A socket file nobody listens on, as a crash leaves it: std does not unlink on drop.
            drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
            assert!(path.exists());
            // Another test spawning a process can fork while the std listener is open, and the child holds the
            // socket until it execs, so it may answer for a moment.
            let mut again = None;
            for _ in 0..40 {
                match Listener::bind(&dir).await {
                    Ok(l) => {
                        again = Some(l);
                        break;
                    }
                    Err(BindErr::Busy) => tokio::time::sleep(Duration::from_millis(25)).await,
                    Err(BindErr::Io(e)) => panic!("bind over a stale socket: {e}"),
                }
            }
            let again = again.expect("a stale socket was taken for a running Quarry");
            assert_eq!(std::fs::read_to_string(dir.join("endpoint")).unwrap(), path.display().to_string());
            drop(again);
            assert!(!path.exists());
            assert!(matches!(connect(&dir).await, Err(ConnectErr::NotOpen)));
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn private_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("quarry-priv-{}", hex(&random()[..6])));
        let t = ensure_token(&dir).unwrap();
        assert_eq!(ensure_token(&dir).unwrap(), t);
        assert_eq!(std::fs::metadata(dir.join("token")).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
