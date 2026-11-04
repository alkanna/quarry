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
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    if cfg!(target_os = "macos") {
        let app = exe.parent()?.parent()?.parent()?;
        (app.extension()? == "app").then(|| app.to_path_buf())
    } else if exe.components().any(|c| c.as_os_str() == "target") {
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

fn remove(p: &Path) {
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

fn run(cmd: &mut Command) -> Result<String, String> {
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
    let Some(url) = v["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["name"] == name.as_str()))
        .and_then(|x| x["browser_download_url"].as_str())
    else {
        return Ok(None);
    };
    let dir = std::env::temp_dir().join(format!("quarry-update-{tag}"));
    remove(&dir);
    let out = dir.join("x");
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let archive = dir.join(&name);
    run(Command::new("curl").args(["-fsSL", "-o"]).arg(&archive).arg(url))?;
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
    install(&new, &target)?;
    remove(&dir);
    Ok(Some(tag.trim_start_matches('v').to_string()))
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
        let _ = std::fs::rename(&old, target);
        return Err(e);
    }
    if cfg!(target_os = "macos") {
        remove(&old);
    }
    Ok(())
}

/// The notes of one version from CHANGELOG.md (written by release-please), as plain text.
pub fn changelog(version: &str) -> Option<String> {
    // Release headings are "## [1.2.0](compare link) (date)", section headings "### Features".
    let is_release = |l: &str| l.starts_with('#') && l.trim_start_matches('#').trim_start().starts_with('[');
    let head = format!("[{version}]");
    let mut lines = include_str!("../CHANGELOG.md").lines().skip_while(|l| !(is_release(l) && l.contains(&head)));
    lines.next()?;
    let mut out = String::new();
    for l in lines.take_while(|l| !is_release(l)) {
        // Drops the trailing commit link, " ([abc1234](https://...))", and bold scopes.
        let l = l.find(" ([").map_or(l, |i| &l[..i]).replace("**", "");
        let l = l.trim_start_matches('#').trim();
        let l = l.strip_prefix("* ").map_or(l.to_string(), |b| format!("- {b}"));
        if !l.is_empty() || !out.ends_with("\n\n") {
            out.push_str(&l);
            out.push('\n');
        }
    }
    let out = out.trim().to_string();
    (!out.is_empty()).then_some(out)
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
    fn changelog_section() {
        assert_eq!(changelog("0.2.0").as_deref(), Some("Features\n\n- initial release of Quarry, a native MongoDB workbench"));
        assert_eq!(changelog("9.9.9"), None);
    }
}
