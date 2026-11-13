//! The script library: folders and scripts kept by Quarry, and folders on disk linked to it. Nothing here touches egui,
//! so all of it is tested without a window. The file is library.json, written apart from ui.json and connections.json
//! because an older Quarry rewrites those and drops the fields it does not know.
//!
//! Values that name a choice (`Script::kind`, `Link::on`) are strings read leniently, so a build that knows more of them
//! does not make an older one skip the entry.
#![allow(dead_code)]

pub mod disk;
pub mod format;
pub mod migrate;
pub mod ops;
pub mod tree;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Scripts opened recently, newest first, never more than this many.
pub const RECENT_MAX: usize = 20;

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Library {
    pub v: u32,
    pub folders: Vec<Folder>,
    pub scripts: Vec<Script>,
    pub links: Vec<Link>,
    pub recent: Vec<ScriptRef>,
    /// The next id to hand out. Ids are never reused: a tab or a recent entry that still names a deleted script must not
    /// find a new one under its id. A file without it starts after the highest id in use.
    pub next: u64,
}

/// A folder kept by Quarry. Siblings show in the order of the list.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Folder {
    pub id: u64,
    pub name: String,
    pub parent: Option<u64>,
}

/// A saved script. `kind` is "sql", "shell", "filter" or "pipeline". `conn` is the saved connection it was written on;
/// the rest of the target (`db`, `schema`, `coll`) is empty where the kind has none. A script whose connection was
/// removed stays, shown as a removed connection.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Script {
    pub id: u64,
    pub name: String,
    pub kind: String,
    pub body: String,
    pub sort: String,
    pub project: String,
    pub conn: u64,
    pub db: String,
    pub schema: String,
    pub coll: String,
    pub folder: Option<u64>,
    pub note: String,
    /// Seconds since the Unix epoch.
    pub updated: u64,
}

/// A folder on disk shown in the library. `on` is "conns" (the connections in `conns`), "engine" (every connection of
/// `engine`) or "all". `exts` are the file extensions it shows, without the dot.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Link {
    pub id: u64,
    pub path: PathBuf,
    pub name: String,
    pub on: String,
    pub conns: Vec<u64>,
    pub engine: String,
    pub exts: Vec<String>,
    /// Reload a tab when its file changes on disk and the tab holds no edits.
    pub follow: bool,
}

/// What a tab was opened from: a script of the library, or a file below a linked folder (`rel` is relative to its path).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ScriptRef {
    Saved(u64),
    File { link: u64, rel: PathBuf },
}

impl Library {
    /// An id no folder, script or link uses or ever used, so the three share one id space and a deleted item's id is not
    /// handed out again.
    pub fn next_id(&mut self) -> u64 {
        self.reserve_ids();
        let id = self.next;
        self.next = id + 1;
        id
    }

    /// Moves `next` past every id in use. Called before anything is removed, so an id handed out by a library that had no
    /// counter yet (an older file, the demo) is not handed out again once its item is gone.
    pub fn reserve_ids(&mut self) {
        let ids = self.folders.iter().map(|f| f.id).chain(self.scripts.iter().map(|s| s.id)).chain(self.links.iter().map(|l| l.id));
        self.next = self.next.max(ids.max().unwrap_or(0) + 1);
    }

    /// Puts deleted folders and scripts back for an Undo. An item whose id was taken since gets a new one, and what pointed
    /// at it follows; a folder whose parent is gone lands at the top level.
    pub fn restore(&mut self, folders: Vec<Folder>, scripts: Vec<Script>) {
        let mut moved: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
        let start = self.folders.len();
        for mut f in folders {
            let old = f.id;
            if self.folders.iter().any(|x| x.id == old) || self.scripts.iter().any(|x| x.id == old) || self.links.iter().any(|x| x.id == old) {
                f.id = self.next_id();
            }
            moved.insert(old, f.id);
            self.folders.push(f);
        }
        let exists = |lib: &Library, id: u64| lib.folders[..start].iter().any(|x| x.id == id);
        for i in start..self.folders.len() {
            let parent = self.folders[i].parent.and_then(|p| moved.get(&p).copied().or_else(|| exists(self, p).then_some(p)));
            self.folders[i].parent = parent;
        }
        for mut s in scripts {
            if self.folders.iter().any(|x| x.id == s.id) || self.scripts.iter().any(|x| x.id == s.id) || self.links.iter().any(|x| x.id == s.id) {
                s.id = self.next_id();
            }
            s.folder = s.folder.and_then(|p| moved.get(&p).copied().or_else(|| exists(self, p).then_some(p)));
            self.scripts.push(s);
        }
    }

    /// Puts `r` first in `recent`, moving it up when it is already there.
    pub fn push_recent(&mut self, r: ScriptRef) {
        self.recent.retain(|x| *x != r);
        self.recent.insert(0, r);
        self.recent.truncate(RECENT_MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_shared_and_recent_is_capped() {
        let mut l = Library { folders: vec![Folder { id: 4, ..Default::default() }], scripts: vec![Script { id: 9, ..Default::default() }], ..Default::default() };
        assert_eq!(l.next_id(), 10);
        for i in 0..30 {
            l.push_recent(ScriptRef::Saved(i));
        }
        l.push_recent(ScriptRef::Saved(25));
        assert_eq!((l.recent.len(), l.recent[0].clone(), l.recent[1].clone()), (RECENT_MAX, ScriptRef::Saved(25), ScriptRef::Saved(29)));
    }

    #[test]
    fn a_deleted_id_is_never_handed_out_again() {
        let mut l = Library::default();
        let (a, b) = (l.next_id(), l.next_id());
        l.scripts.push(Script { id: a, ..Default::default() });
        l.scripts.push(Script { id: b, ..Default::default() });
        // The highest id goes: max + 1 would give it again.
        l.scripts.retain(|s| s.id != b);
        assert_eq!(l.next_id(), b + 1);
        // A library made without the counter (the demo, an older file) keeps its ids too once one is deleted.
        let mut old = Library { scripts: vec![Script { id: 4, ..Default::default() }, Script { id: 9, ..Default::default() }], ..Default::default() };
        ops::delete(&mut old, &tree::Node::Script(9));
        assert_eq!(old.next_id(), 10);
        // The counter survives the file.
        let back: Library = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
        assert_eq!(back.next, b + 2);
    }

    #[test]
    fn restored_items_keep_their_ids_unless_taken_and_their_links_follow() {
        let folder = |id, parent| Folder { id, name: format!("f{id}"), parent };
        let script = |id, folder| Script { id, name: format!("s{id}"), folder, ..Default::default() };
        // Folders 5 (top) and 6 (inside 5) and script 20 in 6 were deleted, and a new script took id 5 since.
        let mut l = Library { folders: vec![folder(1, None)], scripts: vec![script(5, None)], ..Default::default() };
        l.restore(vec![folder(5, None), folder(6, Some(5))], vec![script(20, Some(6)), script(21, Some(99))]);
        let moved = l.folders[1].id;
        assert_ne!(moved, 5);
        assert_eq!((l.folders[2].parent, l.scripts[1].folder), (Some(moved), Some(l.folders[2].id)));
        assert_eq!((l.scripts[1].id, l.scripts[2].folder), (20, None));
        let ids: std::collections::HashSet<u64> = l.folders.iter().map(|f| f.id).chain(l.scripts.iter().map(|s| s.id)).collect();
        assert_eq!(ids.len(), 6);
    }

    #[test]
    fn a_library_written_by_a_newer_build_still_loads() {
        let l: Library = serde_json::from_str(r#"{"v":2,"scripts":[{"id":1,"name":"a","kind":"notebook","extra":true}],"future":[]}"#).unwrap();
        assert_eq!((l.scripts[0].kind.as_str(), l.scripts[0].conn), ("notebook", 0));
    }
}
