//! Cloud SQL for PostgreSQL: starts Google's cloud-sql-proxy for the life of a connection, on a private unix socket (a
//! localhost port on Windows), finds instances through the gcloud CLI, and finds or downloads the proxy program.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncBufReadExt;
use tokio::net::TcpStream;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Debug)]
pub enum IpKind {
    #[default]
    Public,
    Private,
    Psc,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Default, Debug)]
#[serde(default)]
pub struct CloudSql {
    /// `project:region:instance`, or `domain.com:project:region:instance`.
    pub instance: String,
    /// Sign in with the Google identity instead of a saved password.
    pub iam: bool,
    pub ip: IpKind,
}

/// Where the proxy program and gcloud were found, from Settings or the app's own search. None means the worker searches.
#[derive(Clone, Default)]
pub struct Tools {
    pub proxy: Option<PathBuf>,
    pub gcloud: Option<PathBuf>,
}

/// The start of the error a connection gets when no proxy program is found, which the app answers with Download and Install.
pub const PROXY_MISSING: &str = "Cloud SQL proxy not found";

const PROXY_VERSION: &str = "v2.27.0";
const PROXY_URL: &str = "https://storage.googleapis.com/cloud-sql-connectors/cloud-sql-proxy";
const READY_LINES: usize = 20;

/// Instance connection names are passed as an argument, so anything that could be read as a flag or hold a space is refused.
pub fn valid_instance(s: &str) -> bool {
    let parts: Vec<&str> = s.split(':').collect();
    let word = |p: &str, dot: bool| !p.is_empty() && p.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit()) && p.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || (dot && c == '.'));
    match parts.as_slice() {
        [p, r, i] => word(p, true) && word(r, false) && word(i, false),
        [d, p, r, i] => word(d, true) && word(p, true) && word(r, false) && word(i, false),
        _ => false,
    }
}

/// Project ids are passed to gcloud, which on Windows runs through a batch file, so only plain characters are allowed.
fn valid_project(s: &str) -> bool {
    !s.is_empty() && s.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit()) && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | ':' | '.'))
}

/// Where the proxy listens for Quarry.
#[derive(Clone, Debug, PartialEq)]
pub enum Listen {
    /// macOS and Linux: this socket file, in a folder only this user may open, so no other local user can reach the proxy
    /// or stand in for it.
    #[cfg_attr(not(unix), allow(dead_code))]
    Socket(PathBuf),
    /// Windows: a localhost port, because sqlx has no other way to connect there.
    Tcp(u16),
}

/// The socket file name sqlx looks for in a socket folder, for port 5432.
#[cfg_attr(not(unix), allow(dead_code))]
const SOCKET_FILE: &str = ".s.PGSQL.5432";
/// sun_path holds 104 bytes on macOS and 108 on Linux, the closing NUL included.
#[cfg_attr(not(unix), allow(dead_code))]
const MAX_SOCKET_PATH: usize = if cfg!(target_os = "macos") { 103 } else { 107 };
/// What the proxy prints on stdout once every listener is open.
const READY_LINE: &str = "ready for new connections";

/// Windows has no socket the driver can use, so it listens on a port there.
fn native_tcp() -> bool {
    !cfg!(unix)
}

/// A path as a query value: the proxy splits its instance argument at `?` and decodes the rest like a URL query.
fn query_value(p: &Path) -> String {
    p.as_os_str().as_encoded_bytes().iter().map(|&b| if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// The proxy's command line. Nothing secret is on it: the proxy reads the Google credentials itself.
pub fn proxy_args(conf: &CloudSql, at: &Listen, gcloud_account: bool) -> Vec<String> {
    let mut a = match at {
        Listen::Socket(p) => vec![format!("{}?unix-socket-path={}", conf.instance, query_value(p))],
        Listen::Tcp(port) => vec![conf.instance.clone(), "--port".into(), port.to_string(), "--address".into(), "127.0.0.1".into()],
    };
    // The proxy refuses --gcloud-auth with --auto-iam-authn, so IAM always signs in with Application Default Credentials.
    // A password sign-in uses the account that listed the instances when gcloud has one.
    if gcloud_account && !conf.iam {
        a.push("--gcloud-auth".into());
    }
    if conf.iam {
        a.push("--auto-iam-authn".into());
    }
    match conf.ip {
        IpKind::Public => {}
        IpKind::Private => a.push("--private-ip".into()),
        IpKind::Psc => a.push("--psc".into()),
    }
    a
}

/// The start of the error an IAM connection gets when this machine has no Application Default Credentials.
pub const ADC_MISSING: &str = "Cloud SQL: Application Default Credentials not found";

/// The Application Default Credentials file the proxy reads, found the way Google's Go auth library looks: the file
/// GOOGLE_APPLICATION_CREDENTIALS names (only that one when it is set), else the one `gcloud auth application-default
/// login` writes into gcloud's config folder.
fn adc_path(env: impl Fn(&str) -> Option<std::ffi::OsString>, home: Option<PathBuf>, windows: bool) -> Option<PathBuf> {
    let set = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(f) = set("GOOGLE_APPLICATION_CREDENTIALS") {
        return Some(f);
    }
    let dir = match set("CLOUDSDK_CONFIG") {
        Some(d) => d,
        None if windows => set("APPDATA")?.join("gcloud"),
        None => set("HOME").or(home)?.join(".config").join("gcloud"),
    };
    Some(dir.join("application_default_credentials.json"))
}

/// Whether Application Default Credentials exist. Only a file counts: the metadata server of a Google Cloud VM is not seen.
pub fn adc_exists() -> bool {
    adc_path(|k| std::env::var_os(k), dirs::home_dir(), cfg!(windows)).is_some_and(|p| p.is_file())
}

/// The database user for an IAM account: a service account drops its `.gserviceaccount.com`, a user stays as is.
pub fn iam_user(account: &str) -> String {
    let a = account.trim();
    match a.strip_suffix(".gserviceaccount.com") {
        Some(s) => s.to_string(),
        None => a.to_string(),
    }
}

/// A folder only this user may open, removed with everything in it when dropped.
#[cfg_attr(not(unix), allow(dead_code))]
struct SockDir(PathBuf);

impl Drop for SockDir {
    fn drop(&mut self) {
        crate::update::remove(&self.0);
    }
}

/// A running proxy. Dropping it kills the process, then removes its socket folder.
pub struct CloudProxy {
    /// The localhost port on Windows; 5432 with a socket, since sqlx names the socket file after the port.
    pub port: u16,
    /// Whether the proxy signs in with the gcloud account (else Application Default Credentials).
    pub gcloud_account: bool,
    /// Held only so the process dies with the handle (kill_on_drop). Declared before `dir`, so it is dropped first.
    _child: tokio::process::Child,
    #[cfg(windows)]
    _job: Option<Job>,
    dir: Option<SockDir>,
}

impl CloudProxy {
    /// The folder of the proxy's socket, which sqlx takes in place of a host; None when it listens on a port.
    pub fn socket_dir(&self) -> Option<&Path> {
        self.dir.as_ref().map(|d| d.0.as_path())
    }
}

/// The variables that change what the proxy does (socket folder, credentials file, API endpoint and so on). Another
/// program's settings must not redirect Quarry's proxy, so they are removed from its environment.
fn proxy_vars(vars: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>) -> Vec<std::ffi::OsString> {
    // Windows names are case-insensitive.
    vars.map(|(k, _)| k).filter(|k| k.to_string_lossy().to_ascii_uppercase().starts_with("CSQL_PROXY_")).collect()
}

/// Starts the proxy for `conf` and waits until it accepts connections.
pub async fn open(conf: &CloudSql, exe: &Path, gcloud: Option<PathBuf>, timeout: Duration) -> Result<CloudProxy, String> {
    if !valid_instance(&conf.instance) {
        return Err(format!("Cloud SQL proxy not started: \"{}\" is not an instance connection name (project:region:instance).", conf.instance));
    }
    // The test binary stands in for the proxy, and libtest would refuse the proxy's flags.
    #[cfg(test)]
    if std::env::current_exe().ok().as_deref() == Some(exe) {
        let target = tests::STUB_TARGET.lock().unwrap().clone();
        return open_with(timeout, native_tcp(), false, move |at| {
            let mut c = tests::stub(if target.is_some() { "forward" } else { "listen" })(at);
            if let Some(t) = &target {
                c.env("QUARRY_STUB_TARGET", t);
            }
            c
        })
        .await;
    }
    // Refused here: the proxy itself only says so after it gave up on the API.
    if conf.iam && !adc_exists() {
        return Err(format!("{ADC_MISSING}. Run `gcloud auth application-default login`, then Retry."));
    }
    let acct = match gcloud.clone().filter(|_| !conf.iam) {
        Some(g) => tokio::task::spawn_blocking(move || matches!(account(&g), Ok(Some(_)))).await.unwrap_or(false),
        None => false,
    };
    // The proxy runs gcloud itself with --gcloud-auth, and an app opened from Finder has no gcloud on its PATH.
    let path = gcloud.as_deref().and_then(path_with_parent);
    open_with(timeout, native_tcp(), acct, |at| {
        let mut c = tokio::process::Command::new(exe);
        c.args(proxy_args(conf, at, acct));
        if let Some(p) = &path {
            c.env("PATH", p);
        }
        c
    })
    .await
}

/// A fresh socket folder, or a free port when `tcp`.
fn local(tcp: bool) -> Result<(Listen, Option<SockDir>), String> {
    #[cfg(unix)]
    if !tcp {
        use std::os::unix::fs::PermissionsExt;
        // A short prefix: the whole socket path must fit in sun_path.
        let dir = SockDir(crate::update::fresh_temp_dir("qcsql").map_err(|e| format!("Cloud SQL proxy not started: {e}"))?);
        std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o700)).map_err(|e| format!("Cloud SQL proxy not started: {e}"))?;
        let sock = dir.0.join(SOCKET_FILE);
        socket_fits(&sock)?;
        return Ok((Listen::Socket(sock), Some(dir)));
    }
    let _ = tcp;
    Ok((Listen::Tcp(free_port()?), None))
}

#[cfg_attr(not(unix), allow(dead_code))]
fn socket_fits(sock: &Path) -> Result<(), String> {
    let n = sock.as_os_str().len();
    if n > MAX_SOCKET_PATH {
        return Err(format!("Cloud SQL proxy not started: the local socket path {} is {n} bytes, more than the {MAX_SOCKET_PATH} this system allows. Set TMPDIR to a shorter folder.", sock.display()));
    }
    Ok(())
}

/// Whether the proxy itself, and not some other program, accepts connections at `at`.
async fn accepts(at: &Listen, ready: &AtomicBool) -> bool {
    match at {
        // Nobody else can create a file in the 0700 folder, so a socket that answers is the proxy's.
        #[cfg(unix)]
        Listen::Socket(p) => tokio::net::UnixStream::connect(p).await.is_ok(),
        #[cfg(not(unix))]
        Listen::Socket(_) => false,
        // Any local program may listen on a port, so the proxy must first say it is ready.
        // ponytail: the port is free when picked and only bound when the proxy starts, and once it is up any local user
        // can connect to it, which with IAM sign-in lends them the IAM identity. That is the ceiling on Windows, where
        // sqlx has no unix socket or named pipe; move to one if sqlx gains it there.
        Listen::Tcp(port) => ready.load(Ordering::Acquire) && TcpStream::connect(("127.0.0.1", *port)).await.is_ok(),
    }
}

async fn open_with(timeout: Duration, tcp: bool, gcloud_account: bool, build: impl Fn(&Listen) -> tokio::process::Command) -> Result<CloudProxy, String> {
    let mut last = String::new();
    for _ in 0..2 {
        let (at, dir) = local(tcp)?;
        let mut cmd = build(&at);
        for k in proxy_vars(std::env::vars_os()) {
            cmd.env_remove(k);
        }
        cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(crate::update::CREATE_NO_WINDOW);
        // The proxy dies with Quarry even when Quarry is killed. The flag follows the spawning thread, and this runs on a
        // runtime worker thread, which lives as long as the runtime.
        #[cfg(target_os = "linux")]
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
        // ponytail: macOS has no parent-death signal, so only kill_on_drop covers it. A crash of Quarry leaves the proxy
        // running on its socket in the temp folder until it is killed; add a pid file sweep at startup if that shows up.
        let mut child = cmd.spawn().map_err(|e| format!("Cloud SQL proxy could not start: {e}"))?;
        #[cfg(windows)]
        let job = child.raw_handle().and_then(job_for);
        let tail: Arc<Mutex<VecDeque<String>>> = Arc::default();
        let ready = Arc::new(AtomicBool::new(false));
        // Errors come on stderr. stdout carries the ready line and is read to the end, so its pipe never fills.
        if let Some(out) = child.stdout.take() {
            let ready = ready.clone();
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(out).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    if l.contains(READY_LINE) {
                        ready.store(true, Ordering::Release);
                    }
                }
            });
        }
        let drain = child.stderr.take().map(|err| {
            let tail = tail.clone();
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(err).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    let mut t = tail.lock().unwrap();
                    if t.len() == READY_LINES {
                        t.pop_front();
                    }
                    t.push_back(l);
                }
            })
        });
        let text = |tail: &Arc<Mutex<VecDeque<String>>>| tail.lock().unwrap().iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
        let start = Instant::now();
        let err = loop {
            // Alive before and after, so a listener left by a proxy that just died is not taken for it.
            if matches!(child.try_wait(), Ok(None)) && accepts(&at, &ready).await && matches!(child.try_wait(), Ok(None)) {
                return Ok(CloudProxy {
                    port: if let Listen::Tcp(p) = at { p } else { 5432 },
                    gcloud_account,
                    _child: child,
                    #[cfg(windows)]
                    _job: job,
                    dir,
                });
            }
            if let Ok(Some(status)) = child.try_wait() {
                // The last lines may still be in the pipe.
                if let Some(d) = drain {
                    let _ = tokio::time::timeout(Duration::from_millis(500), d).await;
                }
                let said = text(&tail);
                break if said.is_empty() { format!("Cloud SQL proxy failed: it exited with {status}") } else { format!("Cloud SQL proxy failed: {said}") };
            }
            if start.elapsed() >= timeout {
                let _ = child.kill().await;
                let said = text(&tail);
                break format!("Cloud SQL proxy did not accept connections within {} s{}", timeout.as_secs(), if said.is_empty() { String::new() } else { format!(": {said}") });
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        // Another program may take the port between picking it and the proxy binding it.
        let busy = matches!(at, Listen::Tcp(_)) && (err.to_lowercase().contains("address already in use") || err.contains("Only one usage of each socket address"));
        last = err;
        if !busy {
            break;
        }
    }
    Err(last)
}

fn free_port() -> Result<u16, String> {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("Cloud SQL proxy could not find a free port: {e}"))?;
    l.local_addr().map(|a| a.port()).map_err(|e| e.to_string())
}

/// PATH with the folder of `gcloud` in front.
fn path_with_parent(gcloud: &Path) -> Option<std::ffi::OsString> {
    let dir = gcloud.parent().filter(|d| !d.as_os_str().is_empty())?.to_path_buf();
    let mut all = vec![dir];
    all.extend(std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default());
    std::env::join_paths(all).ok()
}

/// A Job object that kills what is in it when its last handle closes, so the proxy cannot outlive Quarry.
#[cfg(windows)]
struct Job(windows_sys::Win32::Foundation::HANDLE);

// The handle is only closed on drop.
#[cfg(windows)]
unsafe impl Send for Job {}
#[cfg(windows)]
unsafe impl Sync for Job {}

#[cfg(windows)]
impl Drop for Job {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

#[cfg(windows)]
const CREATE_SUSPENDED: u32 = 0x0000_0004;

#[cfg(windows)]
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtResumeProcess(process: std::os::windows::io::RawHandle) -> i32;
}

#[cfg(windows)]
fn job_for(process: std::os::windows::io::RawHandle) -> Option<Job> {
    use windows_sys::Win32::System::JobObjects::*;
    unsafe {
        let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if h.is_null() {
            return None;
        }
        let job = Job(h);
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let set = SetInformationJobObject(h, JobObjectExtendedLimitInformation, &info as *const _ as *const _, std::mem::size_of_val(&info) as u32);
        (set != 0 && AssignProcessToJobObject(h, process as _) != 0).then_some(job)
    }
}

// Discovery through gcloud

fn gcloud_file() -> &'static str {
    // A batch file on Windows: Command only adds .exe, so the name carries the extension.
    if cfg!(windows) { "gcloud.cmd" } else { "gcloud" }
}

fn proxy_file() -> String {
    format!("cloud-sql-proxy{}", std::env::consts::EXE_SUFFIX)
}

/// A program from its Settings path, else PATH, the folder Quarry installs tools in, then `extra`.
pub fn find_program(file: &str, setting: &str, extra: &[PathBuf]) -> Option<PathBuf> {
    if !setting.trim().is_empty() {
        return Some(PathBuf::from(setting.trim()));
    }
    search(file, std::env::var_os("PATH"), &crate::app::config::tools_dir(), extra)
}

/// Quarry's tools folder holds only verified downloads, so it comes before the guessed folders. A relative PATH entry
/// (empty or `.`) would run a program from whatever folder Quarry was started in, so it is skipped.
fn search(file: &str, path: Option<std::ffi::OsString>, tools: &Path, extra: &[PathBuf]) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = path.map(|p| std::env::split_paths(&p).filter(|d| d.is_absolute()).collect()).unwrap_or_default();
    dirs.push(tools.to_path_buf());
    dirs.extend(extra.iter().cloned());
    dirs.into_iter().map(|d| d.join(file)).find(|p| p.is_file())
}

/// Homebrew's folders, which a Finder-opened app does not have on PATH. Only on macOS: elsewhere the same paths are
/// ordinary folders, and on Windows `C:\opt\homebrew\bin` is one any user may create.
pub fn brew_dirs() -> Vec<PathBuf> {
    brew_dirs_on(std::env::consts::OS)
}

fn brew_dirs_on(os: &str) -> Vec<PathBuf> {
    if os == "macos" { ["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from).to_vec() } else { Vec::new() }
}

/// Where the Google Cloud SDK usually puts `gcloud`. An app opened from Finder has no shell PATH, so these are checked directly.
fn gcloud_dirs() -> Vec<PathBuf> {
    let home = dirs::home_dir().map(|h| h.join("google-cloud-sdk").join("bin"));
    let env = |k: &str, rest: &str| std::env::var_os(k).map(|v| PathBuf::from(v).join(rest));
    let list: Vec<Option<PathBuf>> = if cfg!(target_os = "macos") {
        vec![home, Some("/opt/homebrew/share/google-cloud-sdk/bin".into()), Some("/usr/local/share/google-cloud-sdk/bin".into()), Some("/opt/homebrew/bin".into()), Some("/usr/local/bin".into())]
    } else if cfg!(windows) {
        let sdk = "Google\\Cloud SDK\\google-cloud-sdk\\bin";
        vec![env("LOCALAPPDATA", sdk), env("ProgramFiles(x86)", sdk), env("ProgramFiles", sdk)]
    } else {
        vec![Some("/usr/bin".into()), Some("/snap/bin".into()), home, Some("/usr/lib/google-cloud-sdk/bin".into())]
    };
    list.into_iter().flatten().collect()
}

pub fn find_gcloud(setting: &str) -> Option<PathBuf> {
    find_program(gcloud_file(), setting, &gcloud_dirs())
}

/// The proxy: Settings, PATH, Quarry's tools folder, the gcloud SDK's own `bin` (where `gcloud components install` puts
/// it), Homebrew. May run gcloud, so call it off the UI thread.
pub fn find_proxy(setting: &str, gcloud: Option<&Path>) -> Option<PathBuf> {
    let file = proxy_file();
    let mut extra = Vec::new();
    // gcloud is often a symlink into the SDK, so the real location is looked at first and it costs nothing.
    if let Some(d) = gcloud.and_then(|g| std::fs::canonicalize(g).ok()).and_then(|g| g.parent().map(Path::to_path_buf)) {
        extra.push(d);
    }
    extra.extend(brew_dirs());
    if let Some(p) = find_program(&file, setting, &extra) {
        return Some(p);
    }
    let root = run_gcloud(gcloud?, &["info", "--format=value(installation.sdk_root)"]).ok()?;
    let p = PathBuf::from(root.trim()).join("bin").join(&file);
    p.is_file().then_some(p)
}

/// How long one gcloud call may take before it is stopped, so a hung gcloud cannot keep a connection or the browser
/// waiting forever.
const GCLOUD_LIMIT: Duration = Duration::from_secs(30);
/// `components install` downloads, so it gets longer.
const INSTALL_LIMIT: Duration = Duration::from_secs(300);

fn run_gcloud(gcloud: &Path, args: &[&str]) -> Result<String, String> {
    run_gcloud_for(gcloud, args, GCLOUD_LIMIT)
}

fn run_gcloud_for(gcloud: &Path, args: &[&str], limit: Duration) -> Result<String, String> {
    let mut cmd = std::process::Command::new(gcloud);
    cmd.arg("--quiet").args(args);
    cmd.env("CLOUDSDK_CORE_DISABLE_PROMPTS", "1");
    // gcloud is Python, which on Windows would otherwise write in the console's code page.
    cmd.env("PYTHONUTF8", "1");
    if let Some(p) = path_with_parent(gcloud) {
        cmd.env("PATH", p);
    }
    let out = match run_limited(cmd, limit) {
        Ok(o) => o,
        Err(Limited::Start(e)) => return Err(format!("Cloud SQL: gcloud could not start ({e})")),
        Err(Limited::Late) => return Err(format!("Cloud SQL: gcloud did not answer within {} s and was stopped.", limit.as_secs())),
    };
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(gcloud_error(&String::from_utf8_lossy(&out.stderr)))
    }
}

enum Limited {
    Start(std::io::Error),
    Late,
}

/// Runs `cmd` with its output captured, and stops it and what it started once `limit` has passed.
fn run_limited(mut cmd: std::process::Command, limit: Duration) -> Result<std::process::Output, Limited> {
    use std::io::Read;
    use std::sync::mpsc;
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    // Suspended, so gcloud.cmd cannot start python.exe before the job holds it; children of a process in a job join it.
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, crate::update::CREATE_NO_WINDOW | CREATE_SUSPENDED);
    // A group of its own, so a timeout also stops what gcloud started.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd.spawn().map_err(Limited::Start)?;
    // On Windows gcloud is a batch file that starts Python; the job stops both, and whatever is left when it closes.
    #[cfg(windows)]
    let job = {
        let h = std::os::windows::io::AsRawHandle::as_raw_handle(&child);
        let job = job_for(h);
        // std keeps no thread handle, so the whole process is resumed. Also when the job could not be made: the run goes on
        // with the plain kill, as before.
        unsafe { NtResumeProcess(h) };
        job
    };
    fn read(r: Option<impl Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
        let (tx, rx) = mpsc::channel();
        if let Some(mut r) = r {
            std::thread::spawn(move || {
                let mut b = Vec::new();
                let _ = r.read_to_end(&mut b);
                let _ = tx.send(b);
            });
        }
        rx
    }
    let (out, err) = (read(child.stdout.take()), read(child.stderr.take()));
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Duration::from_millis(50)),
            _ => break None,
        }
    };
    let Some(status) = status else {
        // Not yet waited for, so the id is still this process group's.
        #[cfg(unix)]
        unsafe {
            libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
        }
        #[cfg(windows)]
        drop(job);
        let _ = child.kill();
        let _ = child.wait();
        return Err(Limited::Late);
    };
    // Something gcloud left running may still hold the pipes, so the output is not waited for long.
    let take = |rx: mpsc::Receiver<Vec<u8>>| rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
    Ok(std::process::Output { status, stdout: take(out), stderr: take(err) })
}

/// gcloud's own wording is long and full of advice for its terminal, so the known cases get a short one.
fn gcloud_error(stderr: &str) -> String {
    let low = stderr.to_lowercase();
    if low.contains("reauthentication") || low.contains("do not currently have an active account") || low.contains("gcloud auth login") || low.contains("does not have any valid credentials") {
        return "Cloud SQL: gcloud is signed out. Run `gcloud auth login` and try again.".into();
    }
    let last = stderr.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("no output");
    let last: String = last.chars().take(300).collect();
    if low.contains("not found") {
        format!("Cloud SQL: gcloud did not find it: {last}")
    } else {
        format!("Cloud SQL: gcloud failed: {last}")
    }
}

/// The active gcloud account, if there is one.
pub fn account(gcloud: &Path) -> Result<Option<String>, String> {
    let out = run_gcloud(gcloud, &["config", "get-value", "account"])?;
    let a = out.trim();
    Ok((!a.is_empty() && a != "(unset)").then(|| a.to_string()))
}

#[derive(Clone, PartialEq, Debug)]
pub struct Project {
    pub id: String,
    pub name: String,
}

pub fn projects(gcloud: &Path) -> Result<Vec<Project>, String> {
    parse_projects(&run_gcloud(gcloud, &["--format=json", "projects", "list"])?)
}

pub fn parse_projects(json: &str) -> Result<Vec<Project>, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct P {
        project_id: String,
        #[serde(default)]
        name: String,
    }
    let v: Vec<P> = serde_json::from_str(json).map_err(|e| format!("Cloud SQL: gcloud returned an unexpected project list ({e})"))?;
    Ok(v.into_iter().map(|p| Project { id: p.project_id, name: p.name }).collect())
}

#[derive(Clone, PartialEq, Debug)]
pub struct Instance {
    pub connection_name: String,
    pub name: String,
    pub region: String,
    pub version: String,
    pub state: String,
    pub public_ip: bool,
    pub private_ip: bool,
    pub psc: bool,
    pub iam: bool,
}

pub fn instances(gcloud: &Path, project: &str) -> Result<Vec<Instance>, String> {
    if !valid_project(project) {
        return Err(format!("Cloud SQL: \"{project}\" is not a project id."));
    }
    parse_instances(&run_gcloud(gcloud, &["--format=json", "sql", "instances", "list", &format!("--project={project}"), "--filter=databaseVersion:POSTGRES*"])?)
}

/// Only PostgreSQL instances are kept, whatever the filter let through.
pub fn parse_instances(json: &str) -> Result<Vec<Instance>, String> {
    #[derive(Deserialize, Default)]
    #[serde(default, rename_all = "camelCase")]
    struct Raw {
        connection_name: String,
        name: String,
        region: String,
        database_version: String,
        state: String,
        ip_addresses: Vec<Ip>,
        settings: Settings,
    }
    #[derive(Deserialize, Default)]
    #[serde(default, rename_all = "camelCase")]
    struct Ip {
        r#type: String,
    }
    #[derive(Deserialize, Default)]
    #[serde(default, rename_all = "camelCase")]
    struct Settings {
        database_flags: Vec<Flag>,
        ip_configuration: IpConf,
        activation_policy: String,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Flag {
        name: String,
        value: String,
    }
    #[derive(Deserialize, Default)]
    #[serde(default, rename_all = "camelCase")]
    struct IpConf {
        psc_config: Psc,
    }
    #[derive(Deserialize, Default)]
    #[serde(default, rename_all = "camelCase")]
    struct Psc {
        psc_enabled: bool,
    }
    let v: Vec<Raw> = serde_json::from_str(json).map_err(|e| format!("Cloud SQL: gcloud returned an unexpected instance list ({e})"))?;
    Ok(v.into_iter()
        .filter(|r| r.database_version.starts_with("POSTGRES_") && !r.connection_name.is_empty())
        .map(|r| Instance {
            public_ip: r.ip_addresses.iter().any(|i| i.r#type == "PRIMARY"),
            private_ip: r.ip_addresses.iter().any(|i| i.r#type == "PRIVATE"),
            psc: r.settings.ip_configuration.psc_config.psc_enabled,
            iam: r.settings.database_flags.iter().any(|f| f.name == "cloudsql.iam_authentication" && f.value == "on"),
            connection_name: r.connection_name,
            name: r.name,
            region: r.region,
            version: r.database_version,
            // gcloud's list already says STOPPED, but the API itself keeps RUNNABLE with the activation policy NEVER.
            state: if r.settings.activation_policy == "NEVER" { "STOPPED".into() } else { r.state },
        })
        .collect())
}

// Getting the proxy

/// The release file for a system and its SHA-256, copied from the release notes (Google publishes no checksum file).
/// Windows on ARM has no build of its own and runs the x64 one under emulation.
pub fn asset(os: &str, arch: &str) -> Option<(&'static str, &'static str)> {
    Some(match (os, arch) {
        ("macos", "x86_64") => ("darwin.amd64", "4bfa8a0a20ff513063182ecb36b07347a7d0f07debc15e4ce115c4931802de28"),
        ("macos", "aarch64") => ("darwin.arm64", "ad99aa34bd783ad5955f60380cd030b7d54a7626fa31111557ce1bbd5629e554"),
        ("linux", "x86_64") => ("linux.amd64", "39ed2abcaa073c3b4714585e36a904e2b1d92218c5e07c18224fb73ef4bedde5"),
        ("linux", "aarch64") => ("linux.arm64", "8ba6ebca26cb2249b8d623fd3736d72e4b57e52bb19738515e936a01201833e5"),
        ("windows", "x86_64" | "aarch64") => ("x64.exe", "b8ac981379283baaadb420cdf187d832eb488b6db76dd108f833f673f4bf80a9"),
        _ => return None,
    })
}

/// Deletes `file` and refuses when it does not match `sha`, so a wrong download is never left where it could be run.
fn verify_or_remove(file: &Path, sha: &str) -> Result<(), String> {
    if crate::update::sha256_matches(file, sha)? {
        return Ok(());
    }
    let _ = std::fs::remove_file(file);
    Err("The downloaded Cloud SQL proxy does not match Google's checksum; nothing was installed.".into())
}

/// Downloads the pinned proxy into `dir`, verified against its published SHA-256 before it is moved into place.
pub fn download_proxy(dir: &Path) -> Result<PathBuf, String> {
    let (name, sha) = asset(std::env::consts::OS, std::env::consts::ARCH).ok_or("Google publishes no Cloud SQL proxy for this system.")?;
    let tmp = crate::update::fresh_temp_dir("quarry-cloud-sql-proxy")?;
    let res = (|| {
        let file = tmp.join(name);
        crate::update::run(std::process::Command::new("curl").args(["-fsSL", "-o"]).arg(&file).arg(format!("{PROXY_URL}/{PROXY_VERSION}/cloud-sql-proxy.{name}")))?;
        verify_or_remove(&file, sha)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        }
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let target = dir.join(proxy_file());
        // Copied beside the target, then renamed over it, so a copy that is running stays intact.
        let part = dir.join(format!(".{}.part", proxy_file()));
        std::fs::copy(&file, &part).map_err(|e| e.to_string())?;
        std::fs::rename(&part, &target).map_err(|e| e.to_string())?;
        Ok(target)
    })();
    crate::update::remove(&tmp);
    res
}

/// `gcloud components install cloud-sql-proxy`. Package-manager installs of gcloud refuse it.
pub fn install_component(gcloud: &Path) -> Result<String, String> {
    run_gcloud_for(gcloud, &["components", "install", "cloud-sql-proxy"], INSTALL_LIMIT).map_err(|m| {
        let low = m.to_lowercase();
        if low.contains("component manager") && low.contains("disabled") {
            "Cloud SQL: this gcloud was installed with a package manager, so it cannot install components. Use Download instead.".into()
        } else if low.contains("permission") || low.contains("access is denied") || low.contains("administrator") || low.contains("elevat") {
            "Cloud SQL: gcloud may not write to its install folder. Use Download instead.".into()
        } else {
            m
        }
    })
}

/// Whether `gcloud components install` can work here. Package-manager installs say their component manager is disabled.
fn components_manageable(gcloud: &Path) -> bool {
    match run_gcloud(gcloud, &["components", "list", "--only-local-state", "--format=value(id)"]) {
        Ok(_) => true,
        Err(m) => !(m.to_lowercase().contains("component manager") && m.to_lowercase().contains("disabled")),
    }
}

/// What the Cloud SQL browser learns when it opens: where the programs are, the gcloud account and its projects.
#[derive(Clone, Debug)]
pub struct Start {
    pub gcloud: Option<PathBuf>,
    pub proxy: Option<PathBuf>,
    /// `gcloud components install cloud-sql-proxy` can work; only asked when the proxy is missing.
    pub components: bool,
    pub account: Option<String>,
    pub projects: Result<Vec<Project>, String>,
    /// Application Default Credentials exist, which IAM sign-in needs.
    pub adc: bool,
}

/// An answer to the Cloud SQL browser.
#[derive(Debug)]
pub enum Found {
    Start(Start),
    /// The PostgreSQL instances of one project, or why they could not be listed.
    Instances { project: String, res: Result<Vec<Instance>, String> },
    /// The proxy was downloaded or installed at this path.
    Proxy(Result<PathBuf, String>),
    /// Where gcloud is without a Settings path, for the Settings dialog.
    Gcloud(Option<PathBuf>),
}

/// Finds gcloud and the proxy and lists the projects. Runs gcloud, so call it off the UI thread.
pub fn start(gcloud_setting: &str, proxy_setting: &str) -> Start {
    let Some(g) = find_gcloud(gcloud_setting) else {
        return Start { gcloud: None, proxy: find_proxy(proxy_setting, None), components: false, account: None, projects: Ok(Vec::new()), adc: adc_exists() };
    };
    let proxy = find_proxy(proxy_setting, Some(&g));
    let components = proxy.is_none() && components_manageable(&g);
    let (account, projects) = match account(&g) {
        Ok(a) => (a, projects(&g)),
        Err(e) => (None, Err(e)),
    };
    Start { gcloud: Some(g), proxy, components, account, projects, adc: adc_exists() }
}

/// Installs the proxy as a gcloud component, then finds where gcloud put it.
pub fn install_and_find(gcloud: &Path) -> Result<PathBuf, String> {
    install_component(gcloud)?;
    find_proxy("", Some(gcloud)).ok_or_else(|| "Cloud SQL: gcloud installed cloud-sql-proxy, but it is not in the SDK's bin folder.".to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn conf(iam: bool, ip: IpKind) -> CloudSql {
        CloudSql { instance: "proj-1:europe-west1:db-main".into(), iam, ip }
    }

    #[test]
    fn args_follow_the_credential_source_iam_and_ip_kind() {
        let tcp = Listen::Tcp(5500);
        let a = proxy_args(&conf(false, IpKind::Public), &tcp, false);
        assert_eq!(a, ["proj-1:europe-west1:db-main", "--port", "5500", "--address", "127.0.0.1"]);
        // A password sign-in uses the gcloud account when there is one.
        assert_eq!(&proxy_args(&conf(false, IpKind::Private), &tcp, true)[5..], ["--gcloud-auth", "--private-ip"]);
        // The proxy refuses --gcloud-auth with --auto-iam-authn, so IAM always goes through Application Default Credentials.
        assert_eq!(&proxy_args(&conf(true, IpKind::Private), &tcp, true)[5..], ["--auto-iam-authn", "--private-ip"]);
        assert_eq!(proxy_args(&conf(false, IpKind::Psc), &tcp, false).last().unwrap(), "--psc");
        // The socket is given to the instance alone, so no port is opened, and its path is query-encoded.
        let a = proxy_args(&conf(true, IpKind::Public), &Listen::Socket("/tmp/q a+b/.s.PGSQL.5432".into()), false);
        assert_eq!(a, ["proj-1:europe-west1:db-main?unix-socket-path=/tmp/q%20a%2Bb/.s.PGSQL.5432", "--auto-iam-authn"]);
    }

    #[test]
    fn adc_is_looked_for_where_google_auth_looks() {
        let env = |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|p| p.0 == k).map(|p| std::ffi::OsString::from(p.1));
        let home = Some(PathBuf::from("/home/u"));
        assert_eq!(adc_path(env(&[("GOOGLE_APPLICATION_CREDENTIALS", "/k.json"), ("CLOUDSDK_CONFIG", "/c")]), home.clone(), false), Some("/k.json".into()));
        assert_eq!(adc_path(env(&[("CLOUDSDK_CONFIG", "/c")]), home.clone(), true), Some(PathBuf::from("/c").join("application_default_credentials.json")));
        assert_eq!(adc_path(env(&[("HOME", "/h")]), home.clone(), false), Some(PathBuf::from("/h/.config/gcloud").join("application_default_credentials.json")));
        assert_eq!(adc_path(env(&[("GOOGLE_APPLICATION_CREDENTIALS", "")]), home.clone(), false), Some(PathBuf::from("/home/u/.config/gcloud").join("application_default_credentials.json")));
        assert_eq!(adc_path(env(&[("APPDATA", "C:\\Users\\u\\AppData\\Roaming")]), home.clone(), true), Some(PathBuf::from("C:\\Users\\u\\AppData\\Roaming").join("gcloud").join("application_default_credentials.json")));
        assert_eq!(adc_path(env(&[]), None, true), None);
    }

    #[test]
    fn proxy_settings_from_the_environment_are_removed() {
        let vars = [("CSQL_PROXY_UNIX_SOCKET", "/x"), ("csql_proxy_credentials_file", "/k"), ("PATH", "/bin"), ("GOOGLE_APPLICATION_CREDENTIALS", "/a"), ("MY_CSQL_PROXY_X", "1")];
        let gone = proxy_vars(vars.iter().map(|(k, v)| (k.into(), v.into())));
        assert_eq!(gone, ["CSQL_PROXY_UNIX_SOCKET", "csql_proxy_credentials_file"]);
    }

    #[test]
    fn programs_are_never_found_in_relative_or_homebrew_folders_off_macos() {
        assert!(brew_dirs_on("windows").is_empty() && brew_dirs_on("linux").is_empty());
        assert_eq!(brew_dirs_on("macos").len(), 2);
        let file = format!("quarry-search-test{}", std::env::consts::EXE_SUFFIX);
        // A relative folder under the working directory, which a PATH entry like `.` resolves into.
        let rel = PathBuf::from("target").join(format!("search-test-{}", std::process::id()));
        std::fs::create_dir_all(&rel).unwrap();
        std::fs::write(rel.join(&file), b"").unwrap();
        let (tools, extra) = (crate::update::fresh_temp_dir("quarry-search-tools").unwrap(), crate::update::fresh_temp_dir("quarry-search-extra").unwrap());
        let path = std::env::join_paths([rel.clone(), PathBuf::new()]).unwrap();
        assert_eq!(search(&file, Some(path.clone()), &tools, &[]), None, "a relative PATH entry is skipped");
        std::fs::write(extra.join(&file), b"").unwrap();
        std::fs::write(tools.join(&file), b"").unwrap();
        assert_eq!(search(&file, Some(path), &tools, std::slice::from_ref(&extra)), Some(tools.join(&file)), "Quarry's tools folder comes before the guessed folders");
        for d in [&rel, &tools, &extra] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn a_socket_path_too_long_for_the_system_is_refused() {
        assert!(socket_fits(Path::new("/tmp/qcsql-0123456789abcdef/.s.PGSQL.5432")).is_ok());
        let long = PathBuf::from(format!("/{}", "d".repeat(100))).join(SOCKET_FILE);
        assert!(socket_fits(&long).unwrap_err().contains("TMPDIR"));
    }

    #[test]
    fn a_hung_gcloud_is_stopped_and_output_is_captured() {
        let stub = |mode: &str| {
            let mut c = std::process::Command::new(std::env::current_exe().unwrap());
            c.args(["--exact", "cloudsql::tests::stub_proxy", "--ignored", "--nocapture", "--test-threads=1"]).env("QUARRY_STUB_PROXY", mode);
            c
        };
        let t = Instant::now();
        assert!(matches!(run_limited(stub("hang"), Duration::from_millis(800)), Err(Limited::Late)));
        assert!(t.elapsed() < Duration::from_secs(10), "stopped at the limit, not when it ended");
        let out = run_limited(stub("fail"), Duration::from_secs(20)).ok().unwrap();
        assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("instance not found"));
    }

    #[test]
    fn instance_names_are_checked() {
        for ok in ["p:r:i", "proj-1:europe-west1:db-main", "example.com:proj:us-central1:db"] {
            assert!(valid_instance(ok), "{ok}");
        }
        for bad in ["", "p:r", "p:r:i:j:k", "--port", "-p:r:i", "p:r:i --x", "P:r:i", "p:r.x:i", "p::i", "a b:r:i", "p:r:i\n", ":r:i"] {
            assert!(!valid_instance(bad), "{bad}");
        }
    }

    #[test]
    fn iam_user_drops_the_service_account_domain() {
        assert_eq!(iam_user("app@proj.iam.gserviceaccount.com"), "app@proj.iam");
        assert_eq!(iam_user("me@example.com\n"), "me@example.com");
    }

    const INSTANCES: &str = r#"[
      {"connectionName":"proj-1:europe-west1:main","name":"main","region":"europe-west1","databaseVersion":"POSTGRES_16","state":"RUNNABLE",
       "ipAddresses":[{"type":"PRIMARY","ipAddress":"203.0.113.5"},{"type":"OUTGOING","ipAddress":"203.0.113.6"}],
       "settings":{"databaseFlags":[{"name":"cloudsql.iam_authentication","value":"on"}],"ipConfiguration":{"ipv4Enabled":true}},"kind":"sql#instance"},
      {"connectionName":"proj-1:us-central1:internal","name":"internal","region":"us-central1","databaseVersion":"POSTGRES_15","state":"RUNNABLE",
       "ipAddresses":[{"type":"PRIVATE","ipAddress":"10.0.0.3"}],"settings":{"ipConfiguration":{"ipv4Enabled":false}}},
      {"connectionName":"proj-1:us-east1:psc","name":"psc","region":"us-east1","databaseVersion":"POSTGRES_17","state":"STOPPED",
       "settings":{"ipConfiguration":{"pscConfig":{"pscEnabled":true}}}},
      {"connectionName":"proj-1:us-east1:idle","name":"idle","region":"us-east1","databaseVersion":"POSTGRES_14","state":"RUNNABLE","settings":{"activationPolicy":"NEVER"}},
      {"connectionName":"proj-1:us-east1:shop","name":"shop","region":"us-east1","databaseVersion":"MYSQL_8_0","state":"RUNNABLE"}
    ]"#;

    #[test]
    fn instances_parse_and_keep_only_postgres() {
        let v = parse_instances(INSTANCES).unwrap();
        assert_eq!(v.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["main", "internal", "psc", "idle"]);
        assert_eq!(v[3].state, "STOPPED", "an instance set to never run is stopped, as gcloud's list says");
        assert!(v[0].public_ip && v[0].iam && !v[0].private_ip && !v[0].psc);
        assert!(v[1].private_ip && !v[1].public_ip && !v[1].iam);
        assert!(v[2].psc && v[2].state == "STOPPED");
        assert!(parse_instances("[]").unwrap().is_empty());
        assert!(parse_instances("not json").is_err());
    }

    #[test]
    fn projects_parse() {
        let v = parse_projects(r#"[{"projectId":"proj-1","name":"Main","projectNumber":"1"},{"projectId":"example.com:old"}]"#).unwrap();
        assert_eq!(v, [Project { id: "proj-1".into(), name: "Main".into() }, Project { id: "example.com:old".into(), name: String::new() }]);
        assert!(valid_project("example.com:old") && !valid_project("--x") && !valid_project("a b"));
    }

    #[test]
    fn gcloud_errors_get_short_wording() {
        assert!(gcloud_error("ERROR: (gcloud.sql.instances.list) Reauthentication failed. cannot prompt").contains("gcloud auth login"));
        assert!(gcloud_error("You do not currently have an active account selected.").contains("signed out"));
        assert!(gcloud_error("ERROR: project not found").starts_with("Cloud SQL: gcloud did not find it"));
        assert!(gcloud_error("boom").starts_with("Cloud SQL: gcloud failed"));
    }

    #[test]
    fn every_target_has_an_asset() {
        assert_eq!(asset("macos", "aarch64").unwrap().0, "darwin.arm64");
        assert_eq!(asset("macos", "x86_64").unwrap().0, "darwin.amd64");
        assert_eq!(asset("linux", "x86_64").unwrap().0, "linux.amd64");
        assert_eq!(asset("linux", "aarch64").unwrap().0, "linux.arm64");
        assert_eq!(asset("windows", "x86_64").unwrap().0, "x64.exe");
        assert_eq!(asset("windows", "aarch64").unwrap().0, "x64.exe");
        assert!(asset("freebsd", "x86_64").is_none());
    }

    #[test]
    fn a_checksum_mismatch_deletes_the_file() {
        let dir = crate::update::fresh_temp_dir("quarry-cloudsql-test").unwrap();
        let f = dir.join("proxy");
        std::fs::write(&f, b"hello").unwrap();
        // sha256("hello")
        let good = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        assert!(verify_or_remove(&f, good).is_ok() && f.exists());
        assert!(verify_or_remove(&f, &"0".repeat(64)).is_err());
        assert!(!f.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Where the stub forwards connections, for a test that connects through it to a real server; None makes it only listen.
    pub(crate) static STUB_TARGET: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

    /// The test binary run again as a stand-in for the proxy, so no script or extra program is needed on any system.
    /// Does nothing unless QUARRY_STUB_PROXY is set.
    /// One accepted connection: a handle to read from it and one to write to it.
    type Halves = (Box<dyn std::io::Read + Send>, Box<dyn std::io::Write + Send>);

    /// Listens where the stub was told, on the socket path or the port, prints the proxy's ready line when `ready`, and
    /// hands each connection to `each`.
    fn serve(ready: bool, each: impl Fn(Halves) + Send + Sync + 'static) {
        let each = Arc::new(each);
        let say = || {
            if ready {
                println!("The proxy has started successfully and is {READY_LINE}!");
            }
        };
        #[cfg(unix)]
        if let Ok(p) = std::env::var("QUARRY_STUB_SOCKET") {
            let l = std::os::unix::net::UnixListener::bind(p).unwrap();
            say();
            for s in l.incoming().flatten() {
                let each = each.clone();
                std::thread::spawn(move || each((Box::new(s.try_clone().unwrap()), Box::new(s))));
            }
            return;
        }
        let l = std::net::TcpListener::bind(("127.0.0.1", std::env::var("QUARRY_STUB_PORT").unwrap().parse::<u16>().unwrap())).unwrap();
        say();
        for s in l.incoming().flatten() {
            let each = each.clone();
            std::thread::spawn(move || each((Box::new(s.try_clone().unwrap()), Box::new(s))));
        }
    }

    /// The test binary run again as a stand-in for the proxy, so no script or extra program is needed on any system.
    /// Does nothing unless QUARRY_STUB_PROXY is set.
    #[test]
    #[ignore]
    fn stub_proxy() {
        let Ok(mode) = std::env::var("QUARRY_STUB_PROXY") else { return };
        match mode.as_str() {
            "listen" => serve(true, drop),
            // Another program on the port, which never says it is the proxy.
            "squat" => serve(false, drop),
            "forward" => {
                let target = std::env::var("QUARRY_STUB_TARGET").unwrap();
                serve(true, move |(mut r, mut w)| {
                    let Ok(t) = std::net::TcpStream::connect(&target) else { return };
                    let (mut up_w, mut down_r) = (t.try_clone().unwrap(), t);
                    std::thread::spawn(move || {
                        let _ = std::io::copy(&mut r, &mut up_w);
                        let _ = up_w.shutdown(std::net::Shutdown::Write);
                    });
                    let _ = std::io::copy(&mut down_r, &mut w);
                });
            }
            "fail" => {
                eprintln!("first line");
                eprintln!("failed to connect: instance not found");
                std::process::exit(3);
            }
            _ => std::thread::sleep(Duration::from_secs(600)),
        }
    }

    pub(super) fn stub(mode: &'static str) -> impl Fn(&Listen) -> tokio::process::Command {
        move |at| {
            let mut c = tokio::process::Command::new(std::env::current_exe().unwrap());
            c.args(["--exact", "cloudsql::tests::stub_proxy", "--ignored", "--nocapture", "--test-threads=1"]).env("QUARRY_STUB_PROXY", mode);
            match at {
                Listen::Socket(p) => c.env("QUARRY_STUB_SOCKET", p),
                Listen::Tcp(port) => c.env("QUARRY_STUB_PORT", port.to_string()),
            };
            c
        }
    }

    async fn refused(port: u16) -> bool {
        for _ in 0..40 {
            if TcpStream::connect(("127.0.0.1", port)).await.is_err() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_ready_proxy_on_a_port_accepts_and_dropping_it_stops_it() {
        let p = open_with(Duration::from_secs(20), true, false, stub("listen")).await.unwrap();
        let port = p.port;
        assert!(p.socket_dir().is_none());
        assert!(TcpStream::connect(("127.0.0.1", port)).await.is_ok());
        drop(p);
        assert!(refused(port).await, "the child should be killed with the handle");
    }

    /// The socket sits in a folder only this user may open, and the folder goes with the handle.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_ready_proxy_on_a_private_socket_accepts_and_dropping_it_removes_it() {
        use std::os::unix::fs::PermissionsExt;
        let p = open_with(Duration::from_secs(20), false, false, stub("listen")).await.unwrap();
        let dir = p.socket_dir().unwrap().to_path_buf();
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(p.port, 5432);
        assert!(tokio::net::UnixStream::connect(dir.join(SOCKET_FILE)).await.is_ok());
        drop(p);
        assert!(!dir.exists(), "the socket folder is removed with the handle");
    }

    /// A program that took the port first passes a connect check, but it never prints the proxy's ready line.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_listener_that_is_not_the_proxy_is_not_taken_for_it() {
        let e = open_with(Duration::from_millis(1500), true, false, stub("squat")).await.err().unwrap();
        assert!(e.starts_with("Cloud SQL proxy did not accept connections"), "{e}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failing_proxy_reports_its_stderr() {
        let e = open_with(Duration::from_secs(20), native_tcp(), false, stub("fail")).await.err().unwrap();
        assert!(e.starts_with("Cloud SQL proxy failed:") && e.contains("instance not found"), "{e}");
        assert_eq!(crate::db::classify(&e), crate::db::ErrKind::CloudSql);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_proxy_that_never_listens_times_out() {
        let e = open_with(Duration::from_millis(800), native_tcp(), false, stub("hang")).await.err().unwrap();
        assert!(e.starts_with("Cloud SQL proxy did not accept connections"), "{e}");
    }
}
