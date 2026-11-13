//! Linked folders on disk: scanning them, reading the git branch, and the writes the library makes there. Plain std::fs,
//! run off the UI thread by the caller, so a slow network drive never stalls a frame.
use super::format::{self, Target};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Folders deeper than this are not entered, and a scan stops after this many files, so a link to a home folder ends.
pub const MAX_DEPTH: usize = 12;
pub const MAX_FILES: usize = 5000;
/// Folders opened, so a link to the root of a drive, which holds few scripts among millions of folders, still ends.
pub const MAX_DIRS: usize = 20_000;
/// A .json file larger than this is not read: it is data, not a filter.
pub const JSON_MAX: u64 = 256 * 1024;
/// How much of a script's start is kept for search and the preview.
pub const HEAD_CHARS: usize = 400;
/// How much of a file the scan reads. The header is on the first line and the head fits well inside this.
const HEAD_BYTES: usize = 2048;
/// The diff gives up past this many lines on either side.
pub const DIFF_MAX: usize = 2000;

/// One script file found below a linked folder. The scan reads only the start of a file, so it stays cheap on a big tree.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// Relative to the link's folder.
    pub rel: PathBuf,
    pub mtime: SystemTime,
    pub len: u64,
    /// "sql", "shell", "filter" or "pipeline".
    pub kind: String,
    /// The first characters of the script, for search and the preview. For .json it is the filter or pipeline as shell text.
    pub head: String,
    /// A header the person wrote, or the keys of a .json file.
    pub target: Target,
}

#[derive(Clone, Debug, Default)]
pub struct Scan {
    /// The folder is not there (moved, deleted, a drive that is not mounted).
    pub missing: bool,
    /// "main", or "detached 1a2b3c4" when HEAD holds a commit. None outside a git checkout.
    pub branch: Option<String>,
    /// The walk stopped at the file cap.
    pub truncated: bool,
    /// Sorted by path, so a folder's files sit together.
    pub entries: Vec<Entry>,
    /// Folders with nothing in them at all, which no file would otherwise show, such as one just made.
    pub empty_dirs: Vec<PathBuf>,
}

fn skipped_dir(name: &str) -> bool {
    name.starts_with('.') || name == "node_modules"
}

fn head_of(text: &str) -> String {
    text.chars().take(HEAD_CHARS).collect()
}

fn read_head(path: &Path) -> Option<String> {
    let mut buf = Vec::with_capacity(HEAD_BYTES);
    std::fs::File::open(path).ok()?.take(HEAD_BYTES as u64).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Walks `root` for files with one of `exts` (without the dot), skipping hidden folders and node_modules. An entry whose
/// modified time and length match one in `prev` is reused without opening the file.
pub fn scan(root: &Path, exts: &[String], prev: Option<&Scan>) -> Scan {
    let mut out = Scan::default();
    if !root.is_dir() {
        out.missing = true;
        return out;
    }
    out.branch = git_branch(root);
    let old: HashMap<&Path, &Entry> = prev.map(|p| p.entries.iter().map(|e| (e.rel.as_path(), e)).collect()).unwrap_or_default();
    let mut stack = vec![(PathBuf::new(), 0usize)];
    let mut opened = 0;
    'walk: while let Some((rel, depth)) = stack.pop() {
        opened += 1;
        if opened > MAX_DIRS {
            out.truncated = true;
            break;
        }
        let Ok(rd) = std::fs::read_dir(root.join(&rel)) else { continue };
        let mut any = false;
        for de in rd.flatten() {
            any = true;
            let name = de.file_name().to_string_lossy().into_owned();
            let Ok(ft) = de.file_type() else { continue };
            let path = de.path();
            // A link to a folder is not followed, since it can point back up and loop; a link to a file is read.
            let is_dir = ft.is_dir();
            if is_dir {
                if depth < MAX_DEPTH && !skipped_dir(&name) {
                    stack.push((rel.join(&name), depth + 1));
                }
                continue;
            }
            if name.starts_with('.') {
                continue;
            }
            let ext = format::ext_of(&path);
            if !exts.iter().any(|e| e.eq_ignore_ascii_case(&ext)) || format::kind_of_ext(&ext).is_none() {
                continue;
            }
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            if !meta.is_file() {
                continue;
            }
            let (mtime, len) = (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len());
            let file_rel = rel.join(&name);
            if let Some(e) = old.get(file_rel.as_path()).filter(|e| e.mtime == mtime && e.len == len) {
                out.entries.push((*e).clone());
            } else if let Some(e) = read_entry(&path, file_rel, &ext, mtime, len) {
                out.entries.push(e);
            } else {
                continue;
            }
            if out.entries.len() >= MAX_FILES {
                out.truncated = true;
                break 'walk;
            }
        }
        if !any && !rel.as_os_str().is_empty() {
            out.empty_dirs.push(rel);
        }
    }
    out.empty_dirs.sort();
    out.entries.sort_by(|a, b| a.rel.to_string_lossy().to_lowercase().cmp(&b.rel.to_string_lossy().to_lowercase()).then_with(|| a.rel.cmp(&b.rel)));
    out
}

/// The entry for the file `rel` below `root` as it is now, for a file the app just wrote.
pub fn entry_of(root: &Path, rel: &Path) -> Option<Entry> {
    let path = root.join(rel);
    let meta = std::fs::metadata(&path).ok()?;
    read_entry(&path, rel.to_path_buf(), &format::ext_of(&path), meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len())
}

fn read_entry(path: &Path, rel: PathBuf, ext: &str, mtime: SystemTime, len: u64) -> Option<Entry> {
    if ext == "json" {
        if len > JSON_MAX {
            return None;
        }
        let p = format::parse_json(&std::fs::read_to_string(path).ok()?)?;
        return Some(Entry { rel, mtime, len, kind: p.kind, head: head_of(&p.body), target: p.target });
    }
    let text = read_head(path)?;
    Some(Entry { rel, mtime, len, kind: format::kind_of_ext(ext)?.into(), head: head_of(&text), target: format::header(&text) })
}

/// The branch of the git checkout that holds `dir`: the nearest `.git` above it, a folder or a file saying `gitdir: path`
/// (worktrees and submodules), then HEAD in it.
pub fn git_branch(dir: &Path) -> Option<String> {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        let dot = d.join(".git");
        if let Ok(meta) = std::fs::metadata(&dot) {
            let gitdir = if meta.is_dir() {
                dot
            } else {
                let text = std::fs::read_to_string(&dot).ok()?;
                let target = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
                d.join(target)
            };
            let head = std::fs::read_to_string(gitdir.join("HEAD")).ok()?;
            let head = head.trim();
            return Some(match head.strip_prefix("ref:") {
                Some(r) => {
                    let r = r.trim();
                    r.strip_prefix("refs/heads/").unwrap_or(r).to_string()
                }
                None => format!("detached {}", head.chars().take(7).collect::<String>()),
            });
        }
        cur = d.parent();
    }
    None
}

/// A file name for `name` that every system accepts: path separators and the characters Windows reserves become `-`, and
/// dots and spaces at the ends go. `ext` is added unless the name already ends with it.
pub fn file_name(name: &str, ext: &str) -> String {
    let mut s: String = name.chars().map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '-' } else { c }).collect();
    s = s.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_string();
    if s.is_empty() {
        s = "script".into();
    }
    let dotted = format!(".{ext}");
    // Both sides lowercased, or "Q.SQL" with ext "SQL" would gain a second ".SQL".
    if !ext.is_empty() && !s.to_lowercase().ends_with(&dotted.to_lowercase()) {
        s.push_str(&dotted);
    }
    s
}

/// `file` when nothing in `dir` has that name, else "stem 2.ext", "stem 3.ext" and so on.
pub fn unique_name(dir: &Path, file: &str) -> String {
    if std::fs::symlink_metadata(dir.join(file)).is_err() {
        return file.to_string();
    }
    let p = Path::new(file);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or(file);
    let ext = p.extension().and_then(|e| e.to_str()).map(|e| format!(".{e}")).unwrap_or_default();
    (2..).map(|n| format!("{stem} {n}{ext}")).find(|f| std::fs::symlink_metadata(dir.join(f)).is_err()).unwrap_or_else(|| file.to_string())
}

/// Replaces the file at `path` with `bytes`. A temp file in the same folder is renamed over it, so a reader never sees half
/// a script and a crash loses nothing. A symlink is written through instead, since a rename would replace the link.
pub fn write_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(path).ok();
    if meta.as_ref().is_some_and(|m| m.file_type().is_symlink()) {
        return std::fs::write(path, bytes);
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    let file = path.file_name().and_then(|f| f.to_str()).unwrap_or("script");
    let tmp = dir.join(format!(".{file}.quarry-tmp"));
    let res = (|| {
        std::fs::write(&tmp, bytes)?;
        if let Some(m) = &meta {
            let _ = std::fs::set_permissions(&tmp, m.permissions());
        }
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// The folder `dir` below the linked folder `root`, made when it is missing. A missing `root` is refused, never made: it is
/// a moved folder or a drive that is not mounted, and writing there would build the dead path again on the local disk.
pub fn dir_below(root: &Path, dir: &Path) -> std::io::Result<PathBuf> {
    if !root.is_dir() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("{} is not there", root.display())));
    }
    let at = root.join(dir);
    std::fs::create_dir_all(&at)?;
    Ok(at)
}

/// Creates a new file `name` in the folder `dir` of the linked folder `root` and returns its path. A name that is taken
/// gets " 2", " 3" and so on; nothing is overwritten.
pub fn create_file(root: &Path, dir: &Path, name: &str, ext: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    let dir = dir_below(root, dir)?;
    let dir = dir.as_path();
    loop {
        let path = dir.join(unique_name(dir, &file_name(name, ext)));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                f.write_all(bytes)?;
                return Ok(path);
            }
            // Someone made the file between the check and the open.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Whether `to` names the same file as `from` under another case, as it does on a case-insensitive filesystem (macOS,
/// Windows). There a case-only rename finds its own file at the target and must still go ahead.
pub fn same_file_other_case(from: &Path, to: &Path) -> bool {
    let (Some(a), Some(b)) = (from.file_name().and_then(|f| f.to_str()), to.file_name().and_then(|f| f.to_str())) else { return false };
    from.parent() == to.parent() && a != b && a.to_lowercase() == b.to_lowercase() && matches!((std::fs::canonicalize(from), std::fs::canonicalize(to)), (Ok(x), Ok(y)) if x == y)
}

/// Renames the file at `path` to `name` in the same folder, and returns the new path. Refuses a name that is taken by
/// another file; a change of case only is allowed.
pub fn rename_file(path: &Path, name: &str) -> std::io::Result<PathBuf> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let to = path.with_file_name(file_name(name, ext));
    if to == path {
        return Ok(to);
    }
    if std::fs::symlink_metadata(&to).is_ok() && !same_file_other_case(path, &to) {
        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{} already exists", to.file_name().and_then(|f| f.to_str()).unwrap_or(""))));
    }
    std::fs::rename(path, &to)?;
    Ok(to)
}

/// Moves a file to the Trash and returns what it held, so an Undo can put it back.
pub fn trash_file(path: &Path) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        // The default method asks Finder, which makes macOS prompt for Automation access the first time.
        let mut ctx = trash::TrashContext::default();
        ctx.set_delete_method(DeleteMethod::NsFileManager);
        ctx.delete(path).map_err(|e| e.to_string())?;
    }
    #[cfg(not(target_os = "macos"))]
    trash::delete(path).map_err(|e| e.to_string())?;
    Ok(bytes)
}

/// One line of a diff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Same,
    Del,
    Add,
}

/// The lines of `a` turned into those of `b`, by longest common subsequence. None when either has more than `DIFF_MAX`
/// lines, where the table would cost too much for a dialog.
pub fn diff<'a>(a: &'a str, b: &'a str) -> Option<Vec<(Op, &'a str)>> {
    let (x, y): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
    if x.len() > DIFF_MAX || y.len() > DIFF_MAX {
        return None;
    }
    let pre = x.iter().zip(&y).take_while(|(p, q)| p == q).count();
    let suf = x[pre..].iter().rev().zip(y[pre..].iter().rev()).take_while(|(p, q)| p == q).count();
    let (xm, ym) = (&x[pre..x.len() - suf], &y[pre..y.len() - suf]);
    let w = ym.len() + 1;
    let mut t = vec![0u32; (xm.len() + 1) * w];
    for i in (0..xm.len()).rev() {
        for j in (0..ym.len()).rev() {
            t[i * w + j] = if xm[i] == ym[j] { t[(i + 1) * w + j + 1] + 1 } else { t[(i + 1) * w + j].max(t[i * w + j + 1]) };
        }
    }
    let mut out: Vec<(Op, &str)> = x[..pre].iter().map(|l| (Op::Same, *l)).collect();
    let (mut i, mut j) = (0, 0);
    while i < xm.len() || j < ym.len() {
        if i < xm.len() && j < ym.len() && xm[i] == ym[j] {
            out.push((Op::Same, xm[i]));
            i += 1;
            j += 1;
        } else if j >= ym.len() || (i < xm.len() && t[(i + 1) * w + j] >= t[i * w + j + 1]) {
            out.push((Op::Del, xm[i]));
            i += 1;
        } else {
            out.push((Op::Add, ym[j]));
            j += 1;
        }
    }
    out.extend(x[x.len() - suf..].iter().map(|l| (Op::Same, *l)));
    Some(out)
}

/// The diff as text for a dialog: "- " for a line only in `a`, "+ " for one only in `b`, two spaces for the rest. Runs of
/// unchanged lines far from a change are folded into one "..." line.
pub fn diff_text(a: &str, b: &str) -> Option<String> {
    let ops = diff(a, b)?;
    let near = |i: usize| (i.saturating_sub(2)..(i + 3).min(ops.len())).any(|k| ops[k].0 != Op::Same);
    let mut out = String::new();
    let mut folded = false;
    for (i, (op, line)) in ops.iter().enumerate() {
        if *op == Op::Same && !near(i) {
            if !folded {
                out.push_str("  ...\n");
                folded = true;
            }
            continue;
        }
        folded = false;
        out.push_str(match op {
            Op::Same => "  ",
            Op::Del => "- ",
            Op::Add => "+ ",
        });
        out.push_str(line);
        out.push('\n');
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A fresh empty folder for one test, removed by the caller.
    pub fn temp_dir(tag: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!("quarry-disk-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn exts(e: &[&str]) -> Vec<String> {
        e.iter().map(|s| s.to_string()).collect()
    }

    fn rels(s: &Scan) -> Vec<String> {
        s.entries.iter().map(|e| e.rel.to_string_lossy().replace('\\', "/")).collect()
    }

    #[test]
    fn a_scan_keeps_the_chosen_extensions_and_skips_hidden_and_vendored_folders() {
        let d = temp_dir("walk");
        put(&d, "a.sql", "-- quarry: db=shop schema=x\nselect 1");
        put(&d, "sub/b.SQL", "select 2");
        put(&d, "sub/c.js", "db.a.find()");
        put(&d, "sub/notes.txt", "x");
        put(&d, ".hidden/h.sql", "x");
        put(&d, "node_modules/m.sql", "x");
        put(&d, ".dot.sql", "x");
        put(&d, "f.json", r#"{"collection":"orders","db":"shop","filter":{"a":1}}"#);
        put(&d, "package.json", r#"{"name":"pkg"}"#);
        let s = scan(&d, &exts(&["sql", "json"]), None);
        assert_eq!(rels(&s), ["a.sql", "f.json", "sub/b.SQL"]);
        assert!(!s.missing && !s.truncated && s.branch.is_none());
        assert_eq!((s.entries[0].kind.as_str(), s.entries[0].target.clone()), ("sql", Target { db: "shop".into(), schema: "x".into(), coll: String::new() }));
        assert_eq!((s.entries[1].kind.as_str(), s.entries[1].head.as_str(), s.entries[1].target.coll.as_str()), ("filter", "{ a: 1 }", "orders"));
        assert_eq!(rels(&scan(&d, &exts(&["js"]), None)), ["sub/c.js"]);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_big_json_file_and_a_big_head_are_not_read_whole() {
        let d = temp_dir("big");
        put(&d, "big.json", &format!(r#"{{"collection":"c","filter":{{"s":"{}"}}}}"#, "x".repeat(JSON_MAX as usize)));
        put(&d, "long.sql", &format!("select '{}'", "y".repeat(100_000)));
        let s = scan(&d, &exts(&["sql", "json"]), None);
        assert_eq!(rels(&s), ["long.sql"]);
        assert_eq!(s.entries[0].head.chars().count(), HEAD_CHARS);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_depth_and_file_caps_end_the_walk() {
        let d = temp_dir("caps");
        let deep = (0..=MAX_DEPTH + 1).map(|i| format!("d{i}")).collect::<Vec<_>>().join("/");
        put(&d, &format!("{deep}/deep.sql"), "x");
        put(&d, "top.sql", "x");
        let s = scan(&d, &exts(&["sql"]), None);
        assert_eq!(rels(&s), ["top.sql"]);
        let many = temp_dir("many");
        std::fs::create_dir_all(&many).unwrap();
        for i in 0..MAX_FILES + 3 {
            std::fs::write(many.join(format!("{i:05}.sql")), "x").unwrap();
        }
        let s = scan(&many, &exts(&["sql"]), None);
        assert!(s.truncated && s.entries.len() == MAX_FILES);
        std::fs::remove_dir_all(&d).unwrap();
        std::fs::remove_dir_all(&many).unwrap();
    }

    #[test]
    fn an_unchanged_file_is_reused_and_a_changed_one_is_read_again() {
        let d = temp_dir("reuse");
        put(&d, "a.sql", "select 1");
        put(&d, "b.sql", "select 2");
        let first = scan(&d, &exts(&["sql"]), None);
        // A previous entry the file still matches in time and length is taken as it is, even with a head the file lacks.
        let mut prev = first.clone();
        prev.entries[0].head = "from the previous scan".into();
        std::fs::write(d.join("b.sql"), "select 22").unwrap();
        let again = scan(&d, &exts(&["sql"]), Some(&prev));
        assert_eq!((again.entries[0].head.as_str(), again.entries[1].head.as_str()), ("from the previous scan", "select 22"));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_missing_folder_is_reported() {
        let s = scan(&std::env::temp_dir().join("quarry-no-such-folder-0xdead"), &exts(&["sql"]), None);
        assert!(s.missing && s.entries.is_empty() && s.branch.is_none());
    }

    #[test]
    fn the_branch_comes_from_head_in_every_checkout_shape() {
        let d = temp_dir("git");
        // A plain checkout, with the linked folder one level below the repository root.
        put(&d, "repo/.git/HEAD", "ref: refs/heads/feature/x\n");
        put(&d, "repo/queries/a.sql", "x");
        assert_eq!(git_branch(&d.join("repo/queries")).as_deref(), Some("feature/x"));
        assert_eq!(scan(&d.join("repo/queries"), &exts(&["sql"]), None).branch.as_deref(), Some("feature/x"));
        // A detached HEAD shows the start of the commit.
        put(&d, "det/.git/HEAD", "1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d\n");
        assert_eq!(git_branch(&d.join("det")).as_deref(), Some("detached 1a2b3c4"));
        // A worktree or submodule: .git is a file naming the real directory, relative to it.
        put(&d, "real/HEAD", "ref: refs/heads/main\n");
        put(&d, "wt/.git", "gitdir: ../real\n");
        assert_eq!(git_branch(&d.join("wt")).as_deref(), Some("main"));
        // No repository at all.
        let none = temp_dir("nogit");
        assert_eq!(git_branch(&none), None);
        std::fs::remove_dir_all(&d).unwrap();
        std::fs::remove_dir_all(&none).unwrap();
    }

    #[test]
    fn names_are_made_safe_and_never_overwrite() {
        assert_eq!(file_name("a/b: c?", "sql"), "a-b- c-.sql");
        assert_eq!(file_name("  ..  ", "sql"), "script.sql");
        assert_eq!(file_name("Report.SQL", "sql"), "Report.SQL");
        let d = temp_dir("names");
        let a = create_file(&d, Path::new(""), "q", "sql", b"1").unwrap();
        let b = create_file(&d, Path::new(""), "q", "sql", b"2").unwrap();
        let c = create_file(&d, Path::new(""), "q", "sql", b"3").unwrap();
        let names: Vec<_> = [&a, &b, &c].iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["q.sql", "q 2.sql", "q 3.sql"]);
        assert_eq!(std::fs::read(&a).unwrap(), b"1");
        assert!(rename_file(&b, "q").is_err());
        assert_eq!(rename_file(&b, "other").unwrap().file_name().unwrap(), "other.sql");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn nothing_is_written_below_a_linked_folder_that_is_gone() {
        let d = temp_dir("gone");
        let root = d.join("moved-repo");
        assert!(create_file(&root, Path::new("reports"), "q", "sql", b"1").is_err());
        assert!(dir_below(&root, Path::new("")).is_err());
        assert!(!root.exists(), "the dead path was made again");
        // Below a folder that is there, a missing subfolder is still made.
        std::fs::create_dir_all(&root).unwrap();
        assert!(create_file(&root, Path::new("reports"), "q", "sql", b"1").unwrap().ends_with("reports/q.sql"));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn an_uppercase_extension_is_not_added_twice() {
        assert_eq!(file_name("Q2.SQL", "SQL"), "Q2.SQL");
        assert_eq!(file_name("Q2", "SQL"), "Q2.SQL");
        assert_eq!(file_name("q2.sql", "SQL"), "q2.sql");
        let d = temp_dir("upper");
        put(&d, "Q.SQL", "select 1");
        let to = rename_file(&d.join("Q.SQL"), "Q2").unwrap();
        assert_eq!(to.file_name().unwrap(), "Q2.SQL");
        let again = rename_file(&to, "Q2.SQL").unwrap();
        assert_eq!(again.file_name().unwrap(), "Q2.SQL");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_rename_that_changes_only_the_case_goes_ahead() {
        // On a case-insensitive filesystem the target "exists" because it is the same file; on a case-sensitive one it
        // does not exist at all. Either way the rename must work, and a real other file must still be refused.
        let d = temp_dir("case");
        put(&d, "report.sql", "select 1");
        let to = rename_file(&d.join("report.sql"), "Report").unwrap();
        assert_eq!(to.file_name().unwrap(), "Report.sql");
        let names: Vec<String> = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["Report.sql"]);
        assert_eq!(std::fs::read_to_string(&to).unwrap(), "select 1");
        put(&d, "other.sql", "select 2");
        assert_eq!(rename_file(&to, "other").unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_write_replaces_the_file_in_place_and_leaves_no_temp_file() {
        let d = temp_dir("write");
        put(&d, "a.sql", "old");
        write_file(&d.join("a.sql"), b"new").unwrap();
        assert_eq!(std::fs::read(d.join("a.sql")).unwrap(), b"new");
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1);
        #[cfg(unix)]
        {
            // A symlink is written through: the link stays a link and its target changes.
            std::os::unix::fs::symlink(d.join("a.sql"), d.join("link.sql")).unwrap();
            write_file(&d.join("link.sql"), b"through").unwrap();
            assert!(std::fs::symlink_metadata(d.join("link.sql")).unwrap().file_type().is_symlink());
            assert_eq!(std::fs::read(d.join("a.sql")).unwrap(), b"through");
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_diff_marks_added_and_removed_lines() {
        let ops = diff("a\nb\nc\nd", "a\nc\nd\ne").unwrap();
        assert_eq!(ops, [(Op::Same, "a"), (Op::Del, "b"), (Op::Same, "c"), (Op::Same, "d"), (Op::Add, "e")]);
        assert_eq!(diff("x", "x").unwrap(), [(Op::Same, "x")]);
        let ops = diff("one\ntwo", "uno\ndos").unwrap();
        assert_eq!(ops.iter().filter(|o| o.0 == Op::Del).count() + ops.iter().filter(|o| o.0 == Op::Add).count(), 4);
        let text = diff_text(&(0..30).map(|i| i.to_string()).collect::<Vec<_>>().join("\n"), &(0..30).map(|i| if i == 15 { "x".into() } else { i.to_string() }).collect::<Vec<_>>().join("\n")).unwrap();
        assert!(text.starts_with("  ...\n") && text.contains("- 15\n+ x\n") && text.lines().count() < 14, "{text}");
    }

    #[test]
    fn a_huge_diff_is_refused() {
        let big = "x\n".repeat(DIFF_MAX + 1);
        assert!(diff(&big, "x").is_none() && diff("x", &big).is_none());
        assert!(diff(&"x\n".repeat(DIFF_MAX), "y").is_some());
    }
}
