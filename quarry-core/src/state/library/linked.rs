//! State behind src/app/library/linked.rs, which draws it.

use crate::library::ops::DropAt;
use crate::library::{format, ScriptRef};
use crate::state::{Tab, View};
use std::path::PathBuf;

/// A linked-file job running on the worker, by id in `LibUi::jobs`: what to do with its answer. Tabs are named by their
/// `uid`, since the tab list can change before the answer comes back.
pub enum Job {
    /// Open a file into a tab on the connection `conn`, picked when it was asked for.
    Open { r: ScriptRef, how: super::open::How, conn: u64 },
    /// Copy a file into the library, at `at`.
    Copy { link: u64, rel: PathBuf, at: DropAt },
    /// Reload: the tab takes the file's text.
    Reload { uid: u64, link: u64, rel: PathBuf },
    /// Cmd+S on a tab bound to a file, which wrote `text`.
    Save { uid: u64, link: u64, rel: PathBuf, text: String },
    /// The Save dialog wrote a new file; the tab is bound to it in `view` with `body` as its base.
    SaveNew { uid: u64, link: u64, rel: PathBuf, view: Option<View>, body: String, place: String, password: bool },
    /// Move to Trash.
    Trash { link: u64, rel: PathBuf, path: PathBuf },
}

/// The kind of the linked file `rel`: what the scan found, else what its extension says.
pub fn kind_in(scans: &crate::library::tree::Scans, link: u64, rel: &std::path::Path) -> String {
    let found = scans.get(&link).and_then(|s| s.entries.iter().find(|e| e.rel == rel)).map(|e| e.kind.clone());
    found.or_else(|| format::kind_of_ext(&format::ext_of(rel)).map(String::from)).unwrap_or_default()
}

/// The text of the view a tab's script is bound to. Borrowed, since the tab strip asks every frame whether each tab
/// holds edits.
pub fn bound_text(t: &Tab) -> Option<&str> {
    let b = t.script.as_ref()?;
    Some(match b.view {
        View::Sql => &t.sql.as_ref()?.query,
        View::Shell => &t.shell.code,
        View::Aggregate => &t.agg_txt,
        View::Documents => &t.filter_txt,
        _ => return None,
    })
}

/// Puts `text` in the view a tab's script is bound to. A filter file also sets the sort and projection.
pub fn set_bound_text(t: &mut Tab, text: &str, parsed: Option<&format::Parsed>) {
    let Some(view) = t.script.as_ref().map(|b| b.view) else { return };
    match view {
        View::Sql => {
            if let Some(s) = t.sql.as_mut() {
                s.query = text.to_string();
            }
        }
        View::Shell => t.shell.code = text.to_string(),
        View::Aggregate => t.agg_txt = text.to_string(),
        View::Documents => {
            t.filter_txt = text.to_string();
            if let Some(p) = parsed {
                (t.sort_txt, t.proj_txt) = (p.sort.clone(), p.project.clone());
            }
        }
        _ => {}
    }
}

/// Where a file below a link is, for a message: the link's name and the folders inside it, as "queries / reports / a.sql".
pub fn link_place(l: &crate::library::Link, rel: &std::path::Path) -> String {
    let name = if l.name.is_empty() { l.path.display().to_string() } else { l.name.clone() };
    std::iter::once(name).chain(rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned())).collect::<Vec<_>>().join(" / ")
}

pub fn file_label(rel: &std::path::Path) -> String {
    rel.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string()
}

/// The extension a script of `kind` is kept under in a linked folder.
pub fn kind_ext(kind: &str) -> &'static str {
    match kind {
        "sql" => "sql",
        "shell" => "js",
        _ => "json",
    }
}

/// The bytes of a linked file for a script. SQL and Shell files are the text as it is, with no header added. A filter or a
/// pipeline is the small JSON shape, which needs its collection; `old` is the file's text now, so its other keys stay.
pub fn file_bytes(kind: &str, body: &str, sort: &str, project: &str, target: format::Target, old: Option<&str>) -> Result<Vec<u8>, String> {
    if kind_ext(kind) != "json" {
        return Ok(body.as_bytes().to_vec());
    }
    let p = format::Parsed { kind: kind.into(), body: body.into(), sort: sort.into(), project: project.into(), target };
    format::to_json(&p, old).map(String::into_bytes)
}
