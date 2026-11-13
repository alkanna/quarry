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

fn find_install_target() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    if cfg!(target_os = "macos") {
        let app = exe.parent()?.parent()?.parent()?;
        (app.extension()? == "app").then(|| app.to_path_buf())
    } else if exe.ancestors().skip(2).take(2).any(|p| p.file_name().is_some_and(|n| n == "target")) {
        // A cargo build sits in target/<profile> or target/<triple>/<profile>; a folder named target higher up is a real install.
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

pub(crate) fn remove(p: &Path) {
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

pub(crate) fn run(cmd: &mut Command) -> Result<String, String> {
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

/// Ok(None) when up to date, Ok(Some(version)) once a newer version is installed and a restart
/// will start it.
pub fn check_and_install() -> Result<Option<String>, String> {
    let Some(target) = install_target() else { return Ok(None) };
    let Some(name) = asset_name() else { return Ok(None) };
    let api = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let json = run(Command::new("curl").args(["-fsSL", "-H", "Accept: application/vnd.github+json", "-H", "User-Agent: quarry-updater", &api]))?;
    let v: serde_json::Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    let tag = v["tag_name"].as_str().ok_or("The latest release has no tag.")?;
    if !newer(env!("CARGO_PKG_VERSION"), tag) {
        return Ok(None);
    }
    // The release is published minutes before the build jobs attach their files: until then it
    // counts as up to date and the next check picks it up.
    let asset_url = |n: &str| v["assets"].as_array().and_then(|a| a.iter().find(|x| x["name"] == n)).and_then(|x| x["browser_download_url"].as_str());
    let Some(url) = asset_url(&name) else { return Ok(None) };
    // The build jobs upload the checksum before the archive, so an archive without one is from a
    // release older than checksums or a broken build, never one still being attached.
    let sum_url = asset_url(&format!("{name}.sha256")).ok_or_else(|| format!("Quarry {tag} publishes no checksum for {name}, so it was not installed."))?;
    let sum = run(Command::new("curl").args(["-fsSL", sum_url]))?;
    let sha = parse_checksum(&sum).ok_or_else(|| format!("The checksum of {name} is unreadable, so Quarry {tag} was not installed."))?;
    let dir = fresh_temp_dir("quarry-update")?;
    let installed = download_and_install(&dir, &name, url, &sha, &target);
    remove(&dir);
    installed?;
    Ok(Some(tag.trim_start_matches('v').to_string()))
}

fn download_and_install(dir: &Path, name: &str, url: &str, sha: &str, target: &Path) -> Result<(), String> {
    let out = dir.join("x");
    std::fs::create_dir(&out).map_err(|e| e.to_string())?;
    let archive = dir.join(name);
    run(Command::new("curl").args(["-fsSL", "-o"]).arg(&archive).arg(url))?;
    if !sha256_matches(&archive, sha)? {
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

pub(crate) fn sha256_matches(file: &Path, sha: &str) -> Result<bool, String> {
    use sha2::Digest;
    let bytes = std::fs::read(file).map_err(|e| e.to_string())?;
    let got: String = sha2::Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    Ok(got.eq_ignore_ascii_case(sha))
}

/// A new, empty folder in the temp folder with a name nobody can guess, so another user cannot
/// plant files or links where the download lands.
pub(crate) fn fresh_temp_dir(prefix: &str) -> Result<PathBuf, String> {
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
    let out = tmp.join("x");
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let archive = tmp.join(url.rsplit('/').next().unwrap_or("tools"));
    run(Command::new("curl").args(["-fsSL", "-o"]).arg(&archive).arg(&url))?;
    if !sha256_matches(&archive, &sha)? {
        remove(&tmp);
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
    std::fs::write(dir.join(TOOLS_VERSION_FILE), &version).map_err(|e| e.to_string())?;
    remove(&tmp);
    Ok(version)
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

/// The notes of every release after `since` up to `version`, newest first, from CHANGELOG.md
/// (written by release-please), as plain text. An update can skip releases, for example when
/// Quarry was not opened for a while, so the notes cover all of them. An unknown `since`, as in
/// settings from 0.2.0 and earlier, gives the notes of `version` alone.
pub fn changelog(since: &str, version: &str) -> Option<String> {
    changelog_in(include_str!("../CHANGELOG.md"), since, version)
}

fn changelog_in(text: &str, since: &str, version: &str) -> Option<String> {
    // Release headings are "## [1.2.0](compare link) (date)", section headings "### Features".
    fn release(l: &str) -> Option<&str> {
        l.strip_prefix('#')?.trim_start_matches('#').trim_start().strip_prefix('[')?.split(']').next()
    }
    let (top, from) = (parse(version)?, parse(since));
    let wanted = |v: &str| parse(v).is_some_and(|v| v <= top && from.map_or(v == top, |f| v > f));
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut cur: Option<(String, String)> = None;
    for l in text.lines() {
        if let Some(v) = release(l) {
            sections.extend(cur.take());
            cur = wanted(v).then(|| (v.to_string(), String::new()));
            continue;
        }
        let Some((_, out)) = cur.as_mut() else { continue };
        // Drops the trailing commit link, " ([abc1234](https://...))", and bold scopes.
        let l = l.find(" ([").map_or(l, |i| &l[..i]).replace("**", "");
        let l = l.trim_start_matches('#').trim();
        let l = l.strip_prefix("* ").map_or(l.to_string(), |b| format!("- {b}"));
        if !l.is_empty() || !out.ends_with("\n\n") {
            out.push_str(&l);
            out.push('\n');
        }
    }
    sections.extend(cur);
    let sections: Vec<(String, String)> = sections.into_iter().map(|(v, b)| (v, b.trim().to_string())).filter(|(_, b)| !b.is_empty()).collect();
    match sections.as_slice() {
        [] => None,
        [(_, body)] => Some(body.clone()),
        many => Some(many.iter().map(|(v, b)| format!("Quarry {v}\n\n{b}")).collect::<Vec<_>>().join("\n\n")),
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
        assert_eq!(changelog("", "0.2.0").as_deref(), Some("Features\n\n- initial release of Quarry, a native MongoDB workbench"));
        assert_eq!(changelog("", "9.9.9"), None);
        let text = "# Changelog\n\n## [0.3.0](x) (d)\n\n### Features\n\n* c ([1](y))\n\n## [0.2.1](x) (d)\n\n### Bug Fixes\n\n* b\n\n## [0.2.0](x) (d)\n\n* a\n";
        // Skipped releases are included, newest first, each under its version.
        assert_eq!(changelog_in(text, "0.2.0", "0.3.0").as_deref(), Some("Quarry 0.3.0\n\nFeatures\n\n- c\n\nQuarry 0.2.1\n\nBug Fixes\n\n- b"));
        assert_eq!(changelog_in(text, "0.2.1", "0.3.0").as_deref(), Some("Features\n\n- c"));
        assert_eq!(changelog_in(text, "", "0.2.1").as_deref(), Some("Bug Fixes\n\n- b"));
    }
}
