//! The audit log: one JSON line per agent call in `agents/audit.jsonl`, rotated into `audit.1.jsonl` to
//! `audit.4.jsonl`. Each line carries the SHA-256 of the line before it, across rotation, so an edited or removed
//! line shows. Arguments arrive already scrubbed; result data is never written, only counts and timings.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

const FILES: usize = 5;
pub const ARGS_MAX: usize = 4096;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Summary {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    /// Seconds since the Unix epoch; chrono is not a direct dependency.
    pub time: u64,
    pub session: u64,
    /// As the agent reported it, so untrusted.
    pub client: String,
    pub tool: String,
    pub conn_id: Option<u64>,
    pub conn_name: Option<String>,
    pub ns: Option<String>,
    /// Scrubbed by the caller, cut to 4 KB here.
    pub args: String,
    /// allowed, asked, approved, denied, refused, timed out.
    pub decision: String,
    /// The setting or rule behind the decision.
    pub reason: Option<String>,
    pub result: Summary,
    pub undo: Option<u64>,
}

#[derive(Serialize)]
struct LineOut<'a> {
    prev: &'a str,
    #[serde(flatten)]
    entry: &'a Entry,
}

#[derive(Deserialize)]
struct LineIn {
    // A line without it does not parse, which is what drops damaged lines; only the chain check reads the value.
    #[cfg_attr(not(test), allow(dead_code))]
    prev: String,
    #[serde(flatten)]
    entry: Entry,
}

/// Where a chain check found the first line whose `prev` does not match the line before it (or that does not
/// parse). Either that line or the one before it was changed.
#[cfg(test)]
#[derive(Debug, PartialEq)]
pub struct Broken {
    pub file: PathBuf,
    /// 1-based.
    pub line: usize,
}

pub struct Audit {
    dir: PathBuf,
    file_cap: u64,
    last: String,
    size: u64,
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn sha(line: &[u8]) -> String {
    super::link::hex(&Sha256::digest(line))
}

fn cut(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max - "...".len();
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &s[..end])
}

impl Audit {
    fn path(&self, n: usize) -> PathBuf {
        path_in(&self.dir, n)
    }

    /// Opens the log in `dir` (created if missing) with a total cap of `max_bytes` across the five files. A line
    /// cut short by a crash is dropped here, so the next line starts clean and the chain still verifies.
    pub fn open(dir: &Path, max_bytes: u64) -> io::Result<Audit> {
        fs::create_dir_all(dir)?;
        let mut a = Audit { dir: dir.to_path_buf(), file_cap: (max_bytes / FILES as u64).max(4096), last: String::new(), size: 0 };
        let cur = a.path(0);
        if let Ok(mut f) = OpenOptions::new().read(true).write(true).open(&cur) {
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            let keep = buf.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
            if keep < buf.len() {
                f.set_len(keep as u64)?;
                buf.truncate(keep);
            }
            a.size = keep as u64;
            if let Some(l) = buf.rsplit(|&b| b == b'\n').find(|l| !l.is_empty()) {
                a.last = sha(l);
            }
        }
        if a.last.is_empty() {
            for n in 1..FILES {
                if let Ok(buf) = fs::read(a.path(n)) {
                    if let Some(l) = buf.rsplit(|&b| b == b'\n').find(|l| !l.is_empty()) {
                        a.last = sha(l);
                    }
                    break;
                }
            }
        }
        Ok(a)
    }

    pub fn append(&mut self, e: &Entry) -> io::Result<()> {
        let mut e = e.clone();
        e.args = cut(&e.args, ARGS_MAX);
        if let Some(err) = &e.result.error {
            e.result.error = Some(cut(err, ARGS_MAX));
        }
        let mut line = serde_json::to_vec(&LineOut { prev: &self.last, entry: &e }).map_err(io::Error::other)?;
        if self.size > 0 && self.size + line.len() as u64 + 1 > self.file_cap {
            self.rotate()?;
        }
        let hash = sha(&line);
        line.push(b'\n');
        let mut f = OpenOptions::new().create(true).append(true).open(self.path(0))?;
        f.write_all(&line)?;
        f.flush()?;
        self.size += line.len() as u64;
        self.last = hash;
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        let _ = fs::remove_file(self.path(FILES - 1));
        for n in (0..FILES - 1).rev() {
            if self.path(n).exists() {
                fs::rename(self.path(n), self.path(n + 1))?;
            }
        }
        self.size = 0;
        Ok(())
    }

    /// The last `n` entries, oldest first, from as many files as needed. Lines that do not parse are skipped, which
    /// covers a line still being written.
    pub fn read_tail(&self, n: usize) -> Vec<Entry> {
        let mut out: Vec<Entry> = Vec::new();
        for f in 0..FILES {
            let Ok(text) = fs::read_to_string(self.path(f)) else { continue };
            let complete = text.rfind('\n').map_or("", |i| &text[..i]);
            let mut lines: Vec<Entry> = complete.lines().filter_map(|l| serde_json::from_str::<LineIn>(l).ok()).map(|l| l.entry).collect();
            lines.append(&mut out);
            out = lines;
            if out.len() >= n {
                break;
            }
        }
        let skip = out.len().saturating_sub(n);
        out.split_off(skip)
    }

    /// Walks every file from oldest to newest. The oldest line's `prev` points at a file already rotated away, so
    /// it cannot be checked; every other line must name the hash of the line before it.
    #[cfg(test)]
    pub fn verify_chain(&self) -> Result<(), Broken> {
        let mut prev: Option<String> = None;
        for f in (0..FILES).rev() {
            let path = self.path(f);
            let Ok(buf) = fs::read(&path) else { continue };
            for (i, l) in buf.split(|&b| b == b'\n').enumerate().filter(|(_, l)| !l.is_empty()) {
                let broken = || Broken { file: path.clone(), line: i + 1 };
                let Ok(parsed) = serde_json::from_slice::<LineIn>(l) else { return Err(broken()) };
                if prev.as_ref().is_some_and(|p| *p != parsed.prev) {
                    return Err(broken());
                }
                prev = Some(sha(l));
            }
        }
        Ok(())
    }
}

fn path_in(dir: &Path, n: usize) -> PathBuf {
    dir.join(if n == 0 { "audit.jsonl".to_string() } else { format!("audit.{n}.jsonl") })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("quarry-audit-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn entry(i: u64) -> Entry {
        Entry { time: 1_700_000_000 + i, session: 1, client: "claude-code".into(), tool: "find".into(), conn_id: Some(7), conn_name: Some("prod".into()), ns: Some("shop.users".into()), args: format!("{{\"n\":{i}}}"), decision: "allowed".into(), reason: None, result: Summary { count: Some(i), bytes: Some(10), ms: Some(3), error: None }, undo: None }
    }

    #[test]
    fn appends_and_reads_back() {
        let d = dir("basic");
        let mut a = Audit::open(&d, 1 << 20).unwrap();
        for i in 0..5 {
            a.append(&entry(i)).unwrap();
        }
        let tail = a.read_tail(3);
        assert_eq!(tail.iter().map(|e| e.time - 1_700_000_000).collect::<Vec<_>>(), [2, 3, 4]);
        assert_eq!(a.verify_chain(), Ok(()));
        let text = fs::read_to_string(d.join("audit.jsonl")).unwrap();
        assert!(text.lines().next().unwrap().starts_with(r#"{"prev":"","#));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn args_are_truncated() {
        let d = dir("trunc");
        let mut a = Audit::open(&d, 1 << 20).unwrap();
        a.append(&Entry { args: "é".repeat(5000), ..entry(0) }).unwrap();
        let e = &a.read_tail(1)[0];
        assert!(e.args.len() <= ARGS_MAX && e.args.ends_with("..."));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn rotates_keeps_five_files_and_chains_across_them() {
        let d = dir("rotate");
        let cap = 5 * 4096;
        let mut a = Audit::open(&d, cap).unwrap();
        for i in 0..400 {
            a.append(&entry(i)).unwrap();
        }
        let files: Vec<_> = (0..6).map(|n| path_in(&d, n).exists()).collect();
        assert_eq!(files, [true, true, true, true, true, false]);
        let total: u64 = (0..5).map(|n| fs::metadata(path_in(&d, n)).unwrap().len()).sum();
        assert!(total <= cap, "{total}");
        assert_eq!(a.verify_chain(), Ok(()));
        let tail = a.read_tail(20);
        assert_eq!(tail.len(), 20, "read across files");
        assert_eq!(tail.last().unwrap().time, 1_700_000_399);
        let reopened = Audit::open(&d, cap).unwrap();
        assert_eq!(reopened.last, a.last, "the chain continues after a restart");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn tampering_is_found() {
        let d = dir("tamper");
        let mut a = Audit::open(&d, 1 << 20).unwrap();
        for i in 0..4 {
            a.append(&entry(i)).unwrap();
        }
        let p = d.join("audit.jsonl");
        let text = fs::read_to_string(&p).unwrap();
        fs::write(&p, text.replacen("\"decision\":\"allowed\"", "\"decision\":\"denied\"", 2).replacen("\"decision\":\"denied\"", "\"decision\":\"allowed\"", 1)).unwrap();
        assert_eq!(a.verify_chain(), Err(Broken { file: p.clone(), line: 3 }));
        let mut lines: Vec<&str> = text.lines().collect();
        lines.remove(1);
        fs::write(&p, lines.join("\n") + "\n").unwrap();
        assert_eq!(a.verify_chain(), Err(Broken { file: p, line: 2 }));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn partial_last_line_is_skipped_and_dropped_on_open() {
        let d = dir("partial");
        let mut a = Audit::open(&d, 1 << 20).unwrap();
        for i in 0..3 {
            a.append(&entry(i)).unwrap();
        }
        let p = d.join("audit.jsonl");
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(br#"{"prev":"abc","time":17"#).unwrap();
        drop(f);
        assert_eq!(a.read_tail(10).len(), 3);
        let mut b = Audit::open(&d, 1 << 20).unwrap();
        b.append(&entry(3)).unwrap();
        assert_eq!(b.read_tail(10).len(), 4);
        assert_eq!(b.verify_chain(), Ok(()));
        let _ = fs::remove_dir_all(&d);
    }
}
