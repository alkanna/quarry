//! Updates from the GitHub releases of the repo, using the system's curl and archive tools
//! (curl, ditto or tar ship with macOS, Windows 10+ and common Linux distributions), so no HTTP
//! or archive library is bundled.

use std::path::{Path, PathBuf};
use std::process::Command;

pub const REPO: &str = "alkanna/quarry";

fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut it = core.split('.').map(|n| n.parse::<u64>().ok());
    Some((it.next()??, it.next()??, it.next()??))
}

/// Whether the release tag is a higher semver than the running version.
pub fn newer(current: &str, tag: &str) -> bool {
    matches!((parse(current), parse(tag)), (Some(a), Some(b)) if b > a)
}

/// The release file for this platform, as named by the release workflow.
pub fn asset_name() -> Option<String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("Quarry-macos-arm64.zip".into()),
        ("macos", _) => Some("Quarry-macos-x86_64.zip".into()),
        ("linux", a @ ("x86_64" | "aarch64")) => Some(format!("quarry-linux-{a}.tar.gz")),
        ("windows", a @ ("x86_64" | "aarch64")) => Some(format!("quarry-windows-{a}.zip")),
        _ => None,
    }
}

/// What gets replaced: the .app bundle on macOS, else the executable. None when running from
/// somewhere that is not an installed app (a cargo build), which is never updated.
pub fn install_target() -> Option<PathBuf> {
    // Read once: on Linux the running executable's path follows it when an update renames it to
    // .old, so asking again afterwards would restart and clean up the old copy.
    static TARGET: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    TARGET.get_or_init(find_install_target).clone()
}

/// A cargo build sits in target/debug or target/release, or under a target triple between them. Any other folder
/// named target, like /opt/target/bin, is a real install.
fn cargo_build(exe: &Path) -> bool {
    let profile = exe.parent().and_then(|p| p.file_name()).is_some_and(|n| n == "debug" || n == "release");
    profile && exe.ancestors().skip(2).take(2).any(|p| p.file_name().is_some_and(|n| n == "target"))
}

fn find_install_target() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    if cfg!(target_os = "macos") {
        let app = exe.parent()?.parent()?.parent()?;
        (app.extension()? == "app").then(|| app.to_path_buf())
    } else if cargo_build(&exe) {
        None
    } else {
        Some(exe)
    }
}

fn old_path(target: &Path) -> PathBuf {
    let mut s = target.as_os_str().to_owned();
    s.push(".old");
    PathBuf::from(s)
}

pub fn remove(p: &Path) {
    let _ = if p.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
}

/// Deletes what the last update moved aside (Windows cannot delete a running executable).
pub fn cleanup() {
    if let Some(t) = install_target() {
        remove(&old_path(&t));
    }
}

/// The app runs without a console on Windows, so each console tool it starts would otherwise
/// flash a console window of its own.
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn run(cmd: &mut Command) -> Result<String, String> {
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(cmd, CREATE_NO_WINDOW);
    let name = cmd.get_program().to_string_lossy().to_string();
    let out = cmd.output().map_err(|e| format!("{name}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("{name}: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// A newer release with a build for this platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Available {
    pub version: String,
    /// The archive's file name, as `asset_name` gives it.
    pub name: String,
    pub url: String,
    /// In bytes, as GitHub lists it; 0 when unknown.
    pub size: u64,
    pub sum_url: String,
    /// The release body, parsed like CHANGELOG.md.
    pub notes: Vec<Section>,
}

/// Ok(None) when up to date, or when Quarry runs from somewhere it never updates (a cargo build).
pub fn check() -> Result<Option<Available>, String> {
    if install_target().is_none() {
        return Ok(None);
    }
    let Some(name) = asset_name() else { return Ok(None) };
    let api = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let json = run(Command::new("curl").args(["-fsSL", "-H", "Accept: application/vnd.github+json", "-H", "User-Agent: quarry-updater", &api]))?;
    let v: serde_json::Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    available(&v, &name, crate::VERSION)
}

fn available(v: &serde_json::Value, name: &str, current: &str) -> Result<Option<Available>, String> {
    let tag = v["tag_name"].as_str().ok_or("The latest release has no tag.")?;
    if !newer(current, tag) {
        return Ok(None);
    }
    let asset = |n: &str| v["assets"].as_array().and_then(|a| a.iter().find(|x| x["name"] == n));
    // The release is published minutes before the build jobs attach their files: until then it
    // counts as up to date and the next check picks it up.
    let Some(a) = asset(name) else { return Ok(None) };
    let url = a["browser_download_url"].as_str().ok_or_else(|| format!("{name} has no download address."))?;
    // The build jobs upload the checksum before the archive, so an archive without one is from a
    // release older than checksums or a broken build, never one still being attached.
    let sum_url = asset(&format!("{name}.sha256")).and_then(|x| x["browser_download_url"].as_str()).ok_or_else(|| format!("Quarry {tag} publishes no checksum for {name}, so it was not installed."))?;
    Ok(Some(Available { version: tag.trim_start_matches('v').to_string(), name: name.to_string(), url: url.to_string(), size: a["size"].as_u64().unwrap_or(0), sum_url: sum_url.to_string(), notes: parse_body(v["body"].as_str().unwrap_or("")) }))
}

/// Shared with the UI while an update downloads.
#[derive(Default, Debug)]
pub struct Progress {
    pub bytes: std::sync::atomic::AtomicU64,
    pub verifying: std::sync::atomic::AtomicBool,
    pub cancel: std::sync::atomic::AtomicBool,
}

impl Progress {
    /// How far the download is, when its size is known; full once it is being checked.
    pub fn percent(&self, size: u64) -> Option<u8> {
        use std::sync::atomic::Ordering::Relaxed;
        if self.verifying.load(Relaxed) { Some(100) } else { percent(self.bytes.load(Relaxed), size) }
    }
}

/// The error of a download stopped through `Progress::cancel`.
const CANCELLED: &str = "The download was cancelled.";

pub fn percent(bytes: u64, size: u64) -> Option<u8> {
    (size > 0).then(|| (bytes.saturating_mul(100) / size).min(100) as u8)
}

/// Downloads, checks and installs the release; returns its version, which starts on restart.
pub fn download(a: &Available, prog: &Progress) -> Result<String, String> {
    let target = install_target().ok_or("Quarry is not running from an installed copy, so it cannot update itself.")?;
    let sum = run(Command::new("curl").args(["-fsSL", &a.sum_url]))?;
    let sha = parse_checksum(&sum).ok_or_else(|| format!("The checksum of {} is unreadable, so Quarry {} was not installed.", a.name, a.version))?;
    let dir = fresh_temp_dir("quarry-update")?;
    let installed = fetch(&dir.join(&a.name), &a.url, prog).and_then(|archive| {
        prog.verifying.store(true, std::sync::atomic::Ordering::Relaxed);
        unpack_and_install(&dir, &a.name, &archive, &sha, &target)
    });
    remove(&dir);
    installed.map(|_| a.version.clone())
}

/// curl into `to`, reporting the bytes written every 100 ms and stopping on cancel.
fn fetch(to: &Path, url: &str, prog: &Progress) -> Result<PathBuf, String> {
    use std::sync::atomic::Ordering::Relaxed;
    let mut cmd = Command::new("curl");
    cmd.args(["-fsSL", "-o"]).arg(to).arg(url).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, CREATE_NO_WINDOW);
    let mut child = cmd.spawn().map_err(|e| format!("curl: {e}"))?;
    let size = || std::fs::metadata(to).map_or(0, |m| m.len());
    loop {
        if let Some(st) = child.try_wait().map_err(|e| format!("curl: {e}"))? {
            prog.bytes.store(size(), Relaxed);
            if st.success() {
                return Ok(to.to_path_buf());
            }
            let mut err = String::new();
            if let Some(mut s) = child.stderr.take() {
                let _ = std::io::Read::read_to_string(&mut s, &mut err);
            }
            return Err(format!("curl: {}", err.trim()));
        }
        if prog.cancel.load(Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CANCELLED.into());
        }
        prog.bytes.store(size(), Relaxed);
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn unpack_and_install(dir: &Path, name: &str, archive: &Path, sha: &str, target: &Path) -> Result<(), String> {
    let out = dir.join("x");
    std::fs::create_dir(&out).map_err(|e| e.to_string())?;
    if !sha256_matches(archive, sha)? {
        return Err(format!("The downloaded {name} does not match its published checksum; nothing was installed."));
    }
    if cfg!(target_os = "macos") {
        run(Command::new("ditto").args(["-x", "-k"]).arg(&archive).arg(&out))?;
    } else {
        // GNU tar detects gzip itself; Windows' bsdtar also reads zip files.
        run(Command::new("tar").arg("-xf").arg(&archive).arg("-C").arg(&out))?;
    }
    let new = if cfg!(target_os = "macos") { out.join("Quarry.app") } else { out.join(format!("quarry{}", std::env::consts::EXE_SUFFIX)) };
    if !new.exists() {
        return Err(format!("{name} does not contain {}.", new.file_name().unwrap_or_default().to_string_lossy()));
    }
    install(&new, target)
}

/// The digest in a `.sha256` file as written by sha256sum, shasum or Get-FileHash: the first
/// word, which must be 64 hex digits.
fn parse_checksum(s: &str) -> Option<String> {
    let h = s.split_whitespace().next()?;
    (h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())).then(|| h.to_ascii_lowercase())
}

pub fn sha256_matches(file: &Path, sha: &str) -> Result<bool, String> {
    use sha2::Digest;
    let bytes = std::fs::read(file).map_err(|e| e.to_string())?;
    let got: String = sha2::Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    Ok(got.eq_ignore_ascii_case(sha))
}

/// A new, empty folder in the temp folder with a name nobody can guess, so another user cannot
/// plant files or links where the download lands.
pub fn fresh_temp_dir(prefix: &str) -> Result<PathBuf, String> {
    use std::hash::{BuildHasher, Hasher};
    // RandomState is keyed from the system's random source, which saves a dependency.
    let n = std::collections::hash_map::RandomState::new().build_hasher().finish();
    let dir = std::env::temp_dir().join(format!("{prefix}-{n:016x}"));
    // create_dir, not create_dir_all: it fails when the path already exists, links included.
    std::fs::create_dir(&dir).map_err(|e| format!("Cannot create {} ({e}).", dir.display()))?;
    Ok(dir)
}

/// The MongoDB Database Tools build for this system in the official feed's latest release, as
/// (version, url, sha256). Linux builds are per distribution, so /etc/os-release picks the newest
/// build of its family not newer than the system; unknown distributions get Ubuntu 22.04's.
pub fn pick_tools(feed: &serde_json::Value, os: &str, arch: &str, os_release: &str) -> Option<(String, String, String)> {
    let latest = &feed["versions"][0];
    // The feed calls 64-bit ARM "arm64" on macOS and Ubuntu and "aarch64" elsewhere. Windows on
    // ARM has no build and runs the x86_64 one under emulation.
    let arches: &[&str] = match (os, arch) {
        ("windows", _) => &["x86_64"],
        (_, "aarch64") => &["arm64", "aarch64"],
        _ => &["x86_64"],
    };
    let downloads: Vec<&serde_json::Value> = latest["downloads"].as_array()?.iter().filter(|d| arches.contains(&d["arch"].as_str().unwrap_or(""))).collect();
    let name = match os {
        "macos" | "windows" => os.to_string(),
        "linux" => {
            let (family, ours) = linux_family(os_release);
            downloads
                .iter()
                .filter_map(|d| d["name"].as_str())
                .filter_map(|n| n.strip_prefix(family).and_then(|v| family_version(family, v)).map(|v| (v, n)))
                .filter(|(v, _)| *v <= ours)
                .max()?
                .1
                .to_string()
        }
        _ => return None,
    };
    let d = downloads.iter().find(|d| d["name"] == name.as_str())?;
    Some((latest["version"].as_str()?.into(), d["archive"]["url"].as_str()?.into(), d["archive"]["sha256"].as_str()?.into()))
}

/// The feed family of a Linux system and its version on that family's scale.
fn linux_family(os_release: &str) -> (&'static str, u32) {
    let field = |k: &str| os_release.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix('=')).map(|v| v.trim().trim_matches('"').to_lowercase()).unwrap_or_default();
    let (id, like, ver) = (field("ID"), field("ID_LIKE"), field("VERSION_ID"));
    let is = |f: &str| id == f || like.split_whitespace().any(|x| x == f);
    let num = |family| family_version(family, &ver.replace('.', ""));
    if id == "ubuntu" {
        ("ubuntu", num("ubuntu").unwrap_or(2204))
    } else if id == "debian" {
        ("debian", ver.split('.').next().and_then(|v| v.parse().ok()).unwrap_or(12))
    } else if id == "amzn" {
        ("amazon", ver.parse().unwrap_or(2023))
    } else if is("rhel") || is("fedora") || is("centos") {
        // Fedora is ahead of every RHEL, so it takes the newest RHEL build.
        ("rhel", if id == "fedora" { u32::MAX } else { num("rhel").unwrap_or(9) })
    } else if is("suse") || id.starts_with("opensuse") || id == "sles" {
        ("suse", ver.split('.').next().and_then(|v| v.parse().ok()).unwrap_or(15))
    } else {
        // Ubuntu derivatives (Mint, Pop!_OS) number their releases differently, so they take an LTS.
        ("ubuntu", 2204)
    }
}

/// A feed name's version within its family, comparable with `linux_family`'s. RHEL names pack
/// major and minor ("rhel88", "rhel93") except from 10 on ("rhel10"), so only the major counts.
fn family_version(family: &str, v: &str) -> Option<u32> {
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if family == "rhel" {
        return if v.starts_with("1") && v.len() >= 2 { v[..2].parse().ok() } else { v[..1].parse().ok() };
    }
    v.parse().ok()
}

/// The newest Database Tools build for this system, as (version, url, sha256).
fn latest_tools() -> Result<(String, String, String), String> {
    let json = run(Command::new("curl").args(["-fsSL", "https://downloads.mongodb.org/tools/db/release.json"]))?;
    let feed: serde_json::Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    let os_release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    pick_tools(&feed, std::env::consts::OS, std::env::consts::ARCH, &os_release).ok_or_else(|| "MongoDB publishes no Database Tools build for this system.".into())
}

/// Records which version Quarry installed, which also marks the tools as Quarry's to update.
const TOOLS_VERSION_FILE: &str = "VERSION";

/// Downloads the MongoDB Database Tools into `dir` and returns the installed version.
pub fn install_db_tools(dir: &Path) -> Result<String, String> {
    install_tools_build(dir, latest_tools()?)
}

/// Updates tools that Quarry installed itself to the newest build. Ok(None) when there is
/// nothing to do, including tools installed some other way, which are never touched.
pub fn update_db_tools(dir: &Path) -> Result<Option<String>, String> {
    let Ok(installed) = std::fs::read_to_string(dir.join(TOOLS_VERSION_FILE)) else { return Ok(None) };
    let latest = latest_tools()?;
    if !newer(installed.trim(), &latest.0) {
        return Ok(None);
    }
    install_tools_build(dir, latest).map(Some)
}

/// Downloads one build, checks it against the feed's SHA-256 and puts its programs in `dir`.
fn install_tools_build(dir: &Path, (version, url, sha): (String, String, String)) -> Result<String, String> {
    // A fresh folder of our own: a predictable one in a shared /tmp could be made by another user, who could then
    // swap the archive between the checksum and the unpacking.
    let tmp = fresh_temp_dir("quarry-tools")?;
    // Its name is random now, so a failed attempt that left it behind would never be cleared by the next one.
    let res = unpack_tools(dir, &tmp, &version, &url, &sha);
    remove(&tmp);
    res.map(|()| version)
}

fn unpack_tools(dir: &Path, tmp: &Path, version: &str, url: &str, sha: &str) -> Result<(), String> {
    let out = tmp.join("x");
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let archive = tmp.join(url.rsplit('/').next().unwrap_or("tools"));
    run(Command::new("curl").args(["-fsSL", "-o"]).arg(&archive).arg(url))?;
    if !sha256_matches(&archive, sha)? {
        return Err("The downloaded Database Tools do not match MongoDB's checksum; nothing was installed.".into());
    }
    if cfg!(target_os = "macos") {
        run(Command::new("ditto").args(["-x", "-k"]).arg(&archive).arg(&out))?;
    } else {
        run(Command::new("tar").arg("-xf").arg(&archive).arg("-C").arg(&out))?;
    }
    // The archive holds one folder, mongodb-database-tools-<platform>-<version>, with bin/ inside.
    let bin = std::fs::read_dir(&out).map_err(|e| e.to_string())?.flatten().map(|e| e.path().join("bin")).find(|p| p.is_dir()).ok_or("The Database Tools archive has no bin folder.")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    for f in std::fs::read_dir(&bin).map_err(|e| e.to_string())?.flatten() {
        // Copied beside the target, then renamed over it: a rename leaves a copy that is running
        // intact, where writing into the file in place would corrupt it.
        let part = dir.join(format!(".{}.part", f.file_name().to_string_lossy()));
        std::fs::copy(f.path(), &part).map_err(|e| e.to_string())?;
        std::fs::rename(&part, dir.join(f.file_name())).map_err(|e| e.to_string())?;
    }
    std::fs::write(dir.join(TOOLS_VERSION_FILE), version).map_err(|e| e.to_string())
}

/// Moves the running copy aside (allowed even while it runs), then puts the new one in its place;
/// on failure the old copy is put back.
fn install(new: &Path, target: &Path) -> Result<(), String> {
    let old = old_path(target);
    remove(&old);
    std::fs::rename(target, &old).map_err(|e| {
        // macOS runs a downloaded app that was never moved from a read-only random copy.
        if target.to_string_lossy().contains("/AppTranslocation/") {
            "Move Quarry to the Applications folder to get updates.".to_string()
        } else {
            format!("Cannot replace {} ({e}).", target.display())
        }
    })?;
    // A copy, not a rename: the temp folder may be on another disk.
    let copied = if new.is_dir() { run(Command::new("ditto").arg(new).arg(target)).map(drop) } else { std::fs::copy(new, target).map(drop).map_err(|e| e.to_string()) };
    if let Err(e) = copied {
        remove(target);
        if let Err(back) = std::fs::rename(&old, target) {
            return Err(format!("{e}\nQuarry could not be put back ({back}); the previous version is at {}.", old.display()));
        }
        return Err(e);
    }
    if cfg!(target_os = "macos") {
        remove(&old);
    }
    Ok(())
}

/// What a section of release notes is about, from its release-please heading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    New,
    Fixed,
    Faster,
    Other,
}

impl Kind {
    fn of(heading: &str) -> Kind {
        match heading {
            "Features" => Kind::New,
            "Bug Fixes" => Kind::Fixed,
            "Performance Improvements" => Kind::Faster,
            _ => Kind::Other,
        }
    }
}

/// One line of release notes: "agents: fenced $objectToArray ..." from PR #279.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub scope: Option<String>,
    pub text: String,
    pub pr: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub kind: Kind,
    /// The heading as written, kept for copied text.
    pub title: String,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    /// As in the heading, 2026-11-17, or empty.
    pub date: String,
    pub sections: Vec<Section>,
}

impl Release {
    pub fn count(&self, kind: Kind) -> usize {
        self.sections.iter().filter(|s| s.kind == kind).map(|s| s.entries.len()).sum()
    }
}

/// The notes of every release after `since` up to `version`, newest first, from CHANGELOG.md
/// (written by release-please). An update can skip releases, for example when Quarry was not
/// opened for a while, so the notes cover all of them. An unknown `since`, as in settings from
/// 0.2.0 and earlier, gives the notes of `version` alone.
pub fn changelog(since: &str, version: &str) -> Vec<Release> {
    releases(include_str!("../../CHANGELOG.md"), since, version)
}

fn releases(text: &str, since: &str, version: &str) -> Vec<Release> {
    // Release headings are "## [1.2.0](compare link) (2026-11-17)".
    fn heading(l: &str) -> Option<(&str, &str)> {
        let rest = l.strip_prefix('#')?.trim_start_matches('#').trim_start().strip_prefix('[')?;
        let (v, after) = rest.split_once(']')?;
        let date = after.rsplit_once('(').map_or("", |(_, d)| d.trim_end_matches(')').trim());
        Some((v, date))
    }
    let Some(top) = parse(version) else { return Vec::new() };
    let from = parse(since);
    let wanted = |v: &str| parse(v).is_some_and(|v| v <= top && from.map_or(v == top, |f| v > f));
    fn close(cur: Option<(Release, String)>, out: &mut Vec<Release>) {
        if let Some((mut r, body)) = cur {
            r.sections = parse_body(&body);
            if !r.sections.is_empty() {
                out.push(r);
            }
        }
    }
    let mut out = Vec::new();
    let mut cur: Option<(Release, String)> = None;
    for l in text.lines() {
        if let Some((v, date)) = heading(l) {
            close(cur.take(), &mut out);
            cur = wanted(v).then(|| (Release { version: v.to_string(), date: date.to_string(), sections: Vec::new() }, String::new()));
        } else if let Some((_, body)) = cur.as_mut() {
            body.push_str(l);
            body.push('\n');
        }
    }
    close(cur, &mut out);
    out
}

/// The sections of one release's notes, as release-please writes them in CHANGELOG.md and in
/// the GitHub release body: "### Features" headings over "* **scope:** text ([#1](..)) ([abc](..))".
pub fn parse_body(text: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    for l in text.lines() {
        let t = l.trim();
        if let Some(h) = t.strip_prefix("###") {
            let title = h.trim().to_string();
            out.push(Section { kind: Kind::of(&title), title, entries: Vec::new() });
        } else if let Some(b) = t.strip_prefix("* ").or_else(|| t.strip_prefix("- ")) {
            if out.is_empty() {
                out.push(Section { kind: Kind::Other, title: String::new(), entries: Vec::new() });
            }
            out.last_mut().unwrap().entries.push(entry(b));
        }
    }
    out.retain(|s| !s.entries.is_empty());
    out
}

fn entry(line: &str) -> Entry {
    let pr = line.find("([#").and_then(|i| line[i + 3..].split(']').next()?.parse().ok());
    // The links go: " ([#279](https://...)) ([8a8be5a](https://...))".
    let line = line.find(" ([").map_or(line, |i| &line[..i]).trim();
    let (scope, text) = match line.strip_prefix("**").and_then(|r| r.split_once(":**")) {
        Some((s, t)) => (Some(unescape(s.trim())), t.trim()),
        None => (None, line),
    };
    let text = unescape(text);
    let mut c = text.chars();
    let text = c.next().map_or(String::new(), |f| f.to_uppercase().chain(c).collect());
    Entry { scope, text, pr }
}

fn unescape(s: &str) -> String {
    s.replace("&gt;", ">").replace("&lt;", "<").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&")
}

/// The notes as plain text for the clipboard. Several releases each get a "Quarry x.y.z" line.
pub fn notes_text(rs: &[Release]) -> String {
    let body = |r: &Release| {
        r.sections
            .iter()
            .map(|s| {
                let items = s.entries.iter().map(|e| match &e.scope {
                    Some(sc) => format!("- {sc}: {}", e.text),
                    None => format!("- {}", e.text),
                });
                let items = items.collect::<Vec<_>>().join("\n");
                if s.title.is_empty() { items } else { format!("{}\n\n{items}", s.title) }
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    match rs {
        [one] => body(one),
        many => many.iter().map(|r| format!("Quarry {}\n\n{}", r.version, body(r))).collect::<Vec<_>>().join("\n\n"),
    }
}

/// Starts the installed copy and ends this one.
pub fn restart() -> ! {
    if let Some(t) = install_target() {
        if cfg!(target_os = "macos") {
            // `open` refuses a second copy of a running app, so wait for this one to quit first.
            let _ = Command::new("/bin/sh").args(["-c", "sleep 1; open \"$0\""]).arg(&t).spawn();
        } else {
            let _ = Command::new(&t).spawn();
        }
    }
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_cargo_profile_folders_count_as_a_build() {
        assert!(cargo_build(Path::new("/src/quarry/target/release/quarry")));
        assert!(cargo_build(Path::new("/src/quarry/target/x86_64-unknown-linux-gnu/debug/quarry")));
        assert!(!cargo_build(Path::new("/opt/target/bin/quarry")));
        assert!(!cargo_build(Path::new("/home/a/target/quarry")));
    }

    #[test]
    fn versions() {
        assert!(newer("0.1.0", "v0.2.0"));
        assert!(newer("0.9.9", "1.0.0"));
        assert!(!newer("1.2.0", "v1.2.0"));
        assert!(!newer("1.10.0", "v1.9.9"));
        assert!(!newer("1.0.0", "nightly"));
        assert!(newer("1.0.0", "v1.0.1-beta"));
        assert_eq!(old_path(Path::new("/A/Quarry.app")), PathBuf::from("/A/Quarry.app.old"));
    }

    #[test]
    fn update_checksum() {
        let hex = "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855";
        assert_eq!(parse_checksum(&format!("{hex}  quarry-linux-x86_64.tar.gz\n")), Some(hex.to_lowercase()));
        assert_eq!(parse_checksum(hex), Some(hex.to_lowercase()));
        assert_eq!(parse_checksum(""), None);
        assert_eq!(parse_checksum("abc  file"), None);
        let dir = fresh_temp_dir("quarry-test").unwrap();
        let f = dir.join("empty");
        std::fs::write(&f, "").unwrap();
        assert_eq!(sha256_matches(&f, hex), Ok(true));
        let other = fresh_temp_dir("quarry-test").unwrap();
        assert_ne!(other, dir);
        remove(&dir);
        remove(&other);
    }

    #[test]
    fn db_tools_platform() {
        let builds = [("macos", "arm64"), ("macos", "x86_64"), ("windows", "x86_64"), ("ubuntu2004", "x86_64"), ("ubuntu2204", "x86_64"), ("ubuntu2404", "x86_64"), ("ubuntu2204", "arm64"), ("ubuntu2404", "arm64"), ("debian11", "x86_64"), ("debian12", "x86_64"), ("rhel70", "x86_64"), ("rhel88", "x86_64"), ("rhel93", "x86_64"), ("rhel10", "x86_64"), ("rhel88", "aarch64"), ("rhel93", "aarch64"), ("amazon2", "aarch64"), ("amazon2023", "aarch64"), ("suse15", "x86_64")];
        let downloads: Vec<_> = builds.iter().map(|(n, a)| serde_json::json!({ "name": n, "arch": a, "archive": { "url": format!("https://x/{n}-{a}"), "sha256": "ab" } })).collect();
        let feed = serde_json::json!({ "versions": [{ "version": "100.20.1", "downloads": downloads }] });
        let url = |os: &str, arch: &str, rel: &str| pick_tools(&feed, os, arch, rel).map(|(_, u, _)| u.trim_start_matches("https://x/").to_string());
        assert_eq!(pick_tools(&feed, "macos", "aarch64", "").unwrap(), ("100.20.1".into(), "https://x/macos-arm64".into(), "ab".into()));
        assert_eq!(url("macos", "x86_64", "").unwrap(), "macos-x86_64");
        // Windows on ARM runs the x86_64 build under emulation.
        assert_eq!(url("windows", "aarch64", "").unwrap(), "windows-x86_64");
        assert_eq!(url("linux", "x86_64", "ID=ubuntu\nVERSION_ID=\"24.04\"").unwrap(), "ubuntu2404-x86_64");
        // A release between builds takes the older one, whose libraries it has.
        assert_eq!(url("linux", "x86_64", "ID=ubuntu\nVERSION_ID=\"23.10\"").unwrap(), "ubuntu2204-x86_64");
        assert_eq!(url("linux", "aarch64", "ID=ubuntu\nVERSION_ID=\"24.04\"").unwrap(), "ubuntu2404-arm64");
        assert_eq!(url("linux", "x86_64", "ID=debian\nVERSION_ID=\"12\"").unwrap(), "debian12-x86_64");
        assert_eq!(url("linux", "aarch64", "ID=\"rocky\"\nID_LIKE=\"rhel centos fedora\"\nVERSION_ID=\"9.4\"").unwrap(), "rhel93-aarch64");
        assert_eq!(url("linux", "x86_64", "ID=\"rhel\"\nVERSION_ID=\"8.10\"").unwrap(), "rhel88-x86_64");
        assert_eq!(url("linux", "x86_64", "ID=\"rhel\"\nVERSION_ID=\"10.0\"").unwrap(), "rhel10-x86_64");
        assert_eq!(url("linux", "x86_64", "ID=fedora\nVERSION_ID=41").unwrap(), "rhel10-x86_64");
        assert_eq!(url("linux", "aarch64", "ID=\"amzn\"\nVERSION_ID=\"2023\"").unwrap(), "amazon2023-aarch64");
        assert_eq!(url("linux", "x86_64", "ID=\"opensuse-leap\"\nID_LIKE=\"suse opensuse\"\nVERSION_ID=\"15.6\"").unwrap(), "suse15-x86_64");
        assert_eq!(url("linux", "x86_64", "ID=linuxmint\nID_LIKE=\"ubuntu debian\"\nVERSION_ID=\"22\"").unwrap(), "ubuntu2204-x86_64");
        assert_eq!(url("linux", "x86_64", "ID=arch").unwrap(), "ubuntu2204-x86_64");
        assert_eq!(url("freebsd", "x86_64", ""), None);
    }

    #[test]
    fn db_tools_update_skips_foreign_installs() {
        // Without Quarry's version file nothing is checked or replaced, so this needs no network.
        let dir = std::env::temp_dir().join("quarry-test-no-tools");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(update_db_tools(&dir), Ok(None));
    }

    #[test]
    fn changelog_section() {
        assert_eq!(notes_text(&changelog("", "0.2.0")), "Features\n\n- Initial release of Quarry, a native MongoDB workbench");
        assert!(changelog("", "9.9.9").is_empty());
        let text = "# Changelog\n\n## [0.3.0](x) (2026-11-17)\n\n### Features\n\n* c ([1](y))\n\n## [0.2.1](x) (d)\n\n### Bug Fixes\n\n* b\n\n## [0.2.0](x) (d)\n\n* a\n";
        // Skipped releases are included, newest first, each under its version.
        assert_eq!(notes_text(&releases(text, "0.2.0", "0.3.0")), "Quarry 0.3.0\n\nFeatures\n\n- C\n\nQuarry 0.2.1\n\nBug Fixes\n\n- B");
        assert_eq!(notes_text(&releases(text, "0.2.1", "0.3.0")), "Features\n\n- C");
        assert_eq!(notes_text(&releases(text, "", "0.2.1")), "Bug Fixes\n\n- B");
        assert_eq!(releases(text, "", "0.3.0")[0].date, "2026-11-17");
        // A release whose notes have no section heading still lists its entries.
        assert_eq!(notes_text(&releases(text, "", "0.2.0")), "- A");
    }

    #[test]
    fn latest_release() {
        let asset = |n: &str, size: u64| serde_json::json!({ "name": n, "size": size, "browser_download_url": format!("https://x/{n}") });
        let v = serde_json::json!({ "tag_name": "v0.55.0", "body": "### Bug Fixes\n\n* b ([#9](u))\n", "assets": [asset("a.zip.sha256", 64), asset("a.zip", 1000)] });
        let a = available(&v, "a.zip", "0.54.0").unwrap().unwrap();
        assert_eq!((a.version.as_str(), a.size, a.url.as_str(), a.sum_url.as_str()), ("0.55.0", 1000, "https://x/a.zip", "https://x/a.zip.sha256"));
        assert_eq!(a.notes[0].entries[0].pr, Some(9));
        assert_eq!(available(&v, "a.zip", "0.55.0"), Ok(None));
        // Files still being attached count as up to date; an archive without a checksum is refused.
        assert_eq!(available(&v, "b.zip", "0.54.0"), Ok(None));
        let bare = serde_json::json!({ "tag_name": "v0.55.0", "assets": [asset("a.zip", 1)] });
        assert!(available(&bare, "a.zip", "0.54.0").unwrap_err().contains("no checksum"));
        assert_eq!((percent(50, 200), percent(300, 200), percent(5, 0)), (Some(25), Some(100), None));
    }

    #[test]
    fn fetch_reports_the_bytes_written() {
        let dir = fresh_temp_dir("quarry-test").unwrap();
        let src = dir.join("src.bin");
        std::fs::write(&src, vec![7u8; 4096]).unwrap();
        let url = format!("file://{}", src.display().to_string().replace('\\', "/"));
        let url = if url.starts_with("file:///") { url } else { url.replacen("file://", "file:///", 1) };
        let prog = Progress::default();
        let got = fetch(&dir.join("out.bin"), &url, &prog).unwrap();
        assert_eq!(std::fs::read(got).unwrap().len(), 4096);
        assert_eq!(prog.bytes.load(std::sync::atomic::Ordering::Relaxed), 4096);
        assert!(fetch(&dir.join("missing"), "file:///nowhere/at/all", &Progress::default()).is_err());
        remove(&dir);
    }

    #[test]
    fn release_entries() {
        let body = "### Features\n\n* **agents:** fenced $objectToArray in pipelines ([#279](https://x/279)) ([8a8be5a](https://x/c))\n* Linux header, switched in Settings &gt; Appearance ([#263](u)) ([18aa1c7](u))\n\n### Performance Improvements\n\n* faster grid\n";
        let s = parse_body(body);
        assert_eq!(s.iter().map(|s| s.kind).collect::<Vec<_>>(), [Kind::New, Kind::Faster]);
        assert_eq!(s[0].entries[0], Entry { scope: Some("agents".into()), text: "Fenced $objectToArray in pipelines".into(), pr: Some(279) });
        assert_eq!(s[0].entries[1], Entry { scope: None, text: "Linux header, switched in Settings > Appearance".into(), pr: Some(263) });
        assert_eq!(s[1].entries[0].pr, None);
        let r = Release { version: "1.0.0".into(), date: String::new(), sections: s };
        assert_eq!((r.count(Kind::New), r.count(Kind::Fixed), r.count(Kind::Faster)), (2, 0, 1));
    }
}
