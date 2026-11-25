//! The script library: what the Library panel's clicks, menus and drops do to library.json and to linked folders, the
//! scans and file jobs of linked folders and their answers (`Msg::LibScan`, `Msg::LibFile`), the conflict choice above a
//! tab whose file changed on disk, and the script waiting for its connection. src/app/library/ draws it. Opening a script
//! into a tab is still the app's (`UiFx::OpenScript`, `UiFx::OpenResolved`) until the tab actions take it.

use super::{Dialog, Reply, Refusal, UiFx};
use crate::core::Core;
use crate::db;
use crate::library::disk::{self, FileOut, Loaded, SaveOut, read_script};
use crate::library::ops::{self, DropAt};
use crate::library::tree::Node;
use crate::library::{format, Script, ScriptRef};
use crate::state::library::linked::*;
use crate::state::library::open::{bind, uri_db, How};
use crate::state::restore::Restore;
use crate::state::{Bound, Status, View};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// How often a followed link, or one with an open tab, is scanned again while the window has focus. Polling replaces a
/// file watcher, which would add a dependency and miss changes on network drives.
const POLL_SECS: f64 = 2.0;

pub const MISSING: &str = "That folder is not there. Reload it once it is back, or link the folder where it moved.";

type Meta = Option<(SystemTime, u64)>;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum LibA {
    /// The toast's Cancel on a script waiting for its connection.
    CancelPending,
    /// The conflict banner of the tab with this uid: take the file's text, keep the tab's, or compare them.
    ConflictReload { tab: u64 },
    ConflictKeep { tab: u64 },
    ConflictCompare { tab: u64 },
    /// A rename typed in the panel: a Quarry script or folder, a link's name, or a file or folder on disk.
    Commit { node: Node, text: String },
    Duplicate(u64),
    /// A drop or menu move within the library.
    Move { node: Node, at: DropAt },
    /// Move to a new folder, which the panel then lets the person name.
    MoveToNew(Node),
    /// Delete a Quarry script or folder (with an Undo), or move a linked file to the Trash.
    Delete(Node),
    NewFolder(Option<u64>),
    /// The panel's scope: every connection, or the current one.
    Scope(bool),
    /// A new file or folder in a linked folder, at `dir` below it.
    NewScript { link: u64, dir: PathBuf },
    NewDir { link: u64, dir: PathBuf },
    /// Read a linked folder again.
    Reload(u64),
    Unlink(u64),
    /// Drops that touch the disk: a script written into a linked folder, a file moved between the folders of its link,
    /// and a file copied into the library.
    ScriptToLink { id: u64, link: u64, dir: PathBuf },
    MoveFile { link: u64, rel: PathBuf, dir: PathBuf },
    FileToQuarry { link: u64, rel: PathBuf, at: DropAt },
}

impl Core {
    pub(super) fn library_act(&mut self, a: LibA) -> Result<Reply, Refusal> {
        match a {
            LibA::CancelPending => self.lib_ui.pending = None,
            LibA::ConflictReload { tab } => self.conflict_reload(tab),
            LibA::ConflictKeep { tab } => self.conflict_keep(tab),
            LibA::ConflictCompare { tab } => self.conflict_compare(tab),
            LibA::Commit { node, text } => self.lib_commit(node, text),
            LibA::Duplicate(id) => {
                if let Some(new) = ops::duplicate(&mut self.library, id, crate::library::now_secs()) {
                    self.save_library();
                    self.lib_ui.panel.sel = Some(Node::Script(new));
                }
            }
            LibA::Move { node, at } => match ops::move_to(&mut self.library, &node, at.clone()) {
                Ok(()) => {
                    if let DropAt::Into(f) = at {
                        self.lib_ui.panel.open.insert(Node::Folder(f));
                    }
                    self.save_library();
                }
                Err(e) => return Err(self.refuse(e)),
            },
            LibA::MoveToNew(n) => {
                let id = ops::new_folder(&mut self.library, None, "");
                if ops::move_to(&mut self.library, &n, DropAt::Into(id)).is_ok() {
                    let name = self.library.folders.iter().find(|f| f.id == id).map(|f| f.name.clone()).unwrap_or_default();
                    let st = &mut self.lib_ui.panel;
                    st.open.insert(Node::Folder(id));
                    st.query.clear();
                    st.sel = Some(Node::Folder(id));
                    st.rename = Some((Node::Folder(id), name, true));
                }
                self.save_library();
            }
            LibA::Delete(Node::File { link, rel }) => {
                if self.delete_linked_file(link, &rel) {
                    self.lib_ui.panel.sel = None;
                }
            }
            LibA::Delete(n) => self.lib_delete(n),
            LibA::NewFolder(parent) => {
                let id = ops::new_folder(&mut self.library, parent, "");
                if let Some(p) = parent {
                    self.lib_ui.panel.open.insert(Node::Folder(p));
                }
                self.lib_ui.panel.query.clear();
                self.save_library();
                let name = self.library.folders.iter().find(|f| f.id == id).map(|f| f.name.clone()).unwrap_or_default();
                self.lib_ui.panel.sel = Some(Node::Folder(id));
                self.lib_ui.panel.rename = Some((Node::Folder(id), name, true));
            }
            LibA::Scope(all) => {
                self.prefs.lib_scope_all = all;
                self.save_prefs();
            }
            LibA::NewScript { link, dir } => {
                if let Some(rel) = self.new_linked_script(link, &dir) {
                    self.show_linked(link, &dir);
                    let node = Node::File { link, rel: rel.clone() };
                    let stem = rel.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let st = &mut self.lib_ui.panel;
                    st.query.clear();
                    st.sel = Some(node.clone());
                    st.rename = Some((node, stem, true));
                }
            }
            LibA::NewDir { link, dir } => {
                if let Some(rel) = self.new_linked_dir(link, &dir) {
                    self.show_linked(link, &dir);
                    let node = Node::Dir { link, rel: rel.clone() };
                    let name = rel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let st = &mut self.lib_ui.panel;
                    st.query.clear();
                    st.sel = Some(node.clone());
                    st.rename = Some((node, name, true));
                }
            }
            LibA::Reload(link) => {
                self.rescan_link(link);
                self.ok("Reading the folder again.");
            }
            LibA::Unlink(link) => {
                self.unlink(link);
                let st = &mut self.lib_ui.panel;
                st.open.retain(|n| !matches!(n, Node::Link(l) | Node::Dir { link: l, .. } | Node::File { link: l, .. } if *l == link));
                if st.sel.as_ref().is_some_and(|n| matches!(n, Node::Link(l) | Node::Dir { link: l, .. } | Node::File { link: l, .. } if *l == link)) {
                    st.sel = None;
                }
            }
            LibA::ScriptToLink { id, link, dir } => {
                if let Some(rel) = self.script_to_link(id, link, &dir) {
                    self.show_linked(link, &dir);
                    self.lib_ui.panel.sel = Some(Node::File { link, rel });
                }
            }
            LibA::MoveFile { link, rel, dir } => {
                if let Some(new) = self.move_linked_file(link, &rel, &dir) {
                    self.show_linked(link, &dir);
                    self.lib_ui.panel.sel = Some(Node::File { link, rel: new });
                }
            }
            // The copy is selected, and its folder opened, once the file is read.
            LibA::FileToQuarry { link, rel, at } => self.file_to_quarry(link, &rel, at),
        }
        Ok(Reply::Done)
    }

    fn lib_commit(&mut self, n: Node, text: String) {
        match &n {
            Node::Link(id) => {
                let name = text.trim().to_string();
                if !name.is_empty() && let Some(l) = self.library.links.iter_mut().find(|l| l.id == *id) {
                    l.name = name;
                    self.save_library();
                }
            }
            Node::Dir { link, rel } | Node::File { link, rel } => {
                let is_dir = matches!(n, Node::Dir { .. });
                if let Some(new) = self.rename_linked(*link, rel, is_dir, &text) {
                    let to = if is_dir { Node::Dir { link: *link, rel: new } } else { Node::File { link: *link, rel: new } };
                    let st = &mut self.lib_ui.panel;
                    if st.open.remove(&n) {
                        st.open.insert(to.clone());
                    }
                    st.sel = Some(to);
                }
            }
            _ => {
                if ops::rename(&mut self.library, &n, &text) {
                    self.save_library();
                }
            }
        }
    }

    fn lib_delete(&mut self, n: Node) {
        let label = match &n {
            Node::Script(id) => self.library.scripts.iter().find(|s| s.id == *id).map(|s| format!("\"{}\"", s.name)),
            Node::Folder(id) => self.library.folders.iter().find(|f| f.id == *id).map(|f| format!("folder \"{}\"", f.name)),
            _ => None,
        };
        if let Some((folders, scripts)) = ops::delete(&mut self.library, &n) {
            self.forget_scripts(&scripts.iter().map(|s| s.id).collect::<Vec<_>>());
            self.save_library();
            let msg = match (&n, scripts.len()) {
                (Node::Folder(_), 0) => format!("Deleted {}.", label.unwrap_or_default()),
                (Node::Folder(_), k) => format!("Deleted {} and {k} script{}.", label.unwrap_or_default(), if k == 1 { "" } else { "s" }),
                _ => format!("Deleted {}.", label.unwrap_or_default()),
            };
            let st = &mut self.lib_ui.panel;
            if st.sel.as_ref().is_some_and(|s| *s == n || matches!(s, Node::Script(id) if scripts.iter().any(|x| x.id == *id)) || matches!(s, Node::Folder(id) if folders.iter().any(|f| f.id == *id))) {
                st.sel = None;
            }
            self.toast_restore(msg, Restore::Saved { scripts, folders, written: None });
        }
    }

    /// Opens a linked folder and the folders above `dir` in it, so a file just made or moved there shows.
    fn show_linked(&mut self, link: u64, dir: &Path) {
        let st = &mut self.lib_ui.panel;
        st.open.insert(Node::Link(link));
        let mut at = PathBuf::new();
        for c in dir.components() {
            at.push(c);
            st.open.insert(Node::Dir { link, rel: at.clone() });
        }
    }

    pub fn save_library(&mut self) {
        // The demo's made-up scripts must never reach the real library file.
        if self.demo {
            return;
        }
        self.library.v = 1;
        let b = serde_json::to_vec_pretty(&self.library).unwrap_or_default();
        self.write_config("library.json", b);
    }

    /// Called every frame: scheduled library work, such as opening a script once its connection is ready and rescanning
    /// linked folders. `now` is the UI clock in seconds and `focused` whether the window has focus; answers when the UI
    /// should look again.
    pub fn library_tick(&mut self, now: f64, focused: bool) -> Option<f64> {
        let a = self.library_pending(now);
        let b = self.linked_tick(now, focused);
        match (a, b) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Opens the script that waited for its connection, once the connection is ready. A connection that fails on a new
    /// attempt drops it with one warning; one that never starts connecting (its address could not be read, say) drops it after
    /// `WAIT_SECS`, so it never opens later out of the blue. A Run becomes a plain open if another tab is current by then.
    fn library_pending(&mut self, now: f64) -> Option<f64> {
        const WAIT_SECS: f64 = 15.0;
        let p = self.lib_ui.pending.clone()?;
        let Some(c) = self.conn(p.conn) else {
            self.lib_ui.pending = None;
            self.warn(format!("{} was not opened: its connection was removed.", p.name));
            return None;
        };
        let (ready, connecting, failed, attempt, cname) = (c.status == Status::Ready, c.status == Status::Connecting, matches!(c.status, Status::Failed(_)), c.attempt, c.meta.name.clone());
        if ready {
            self.lib_ui.pending = None;
            let moved = self.t().map(|t| t.uid) != p.uid;
            let how = if p.how == How::Run && moved { How::Here } else { p.how };
            // Opening a tab is the app's; M4b's tab actions take this over.
            self.ui_fx.push(UiFx::OpenScript { r: p.r, how, conn: p.conn });
            return None;
        }
        if connecting {
            if let Some(q) = self.lib_ui.pending.as_mut() {
                (q.since, q.seen_connecting) = (None, true);
            }
            return None;
        }
        // The failure the connection had when the script was asked for is not news: only a new attempt that fails is, or the
        // one that was under way.
        if failed && (attempt != p.attempt || p.seen_connecting) {
            self.lib_ui.pending = None;
            self.warn(format!("{} was not opened: {cname} failed to connect.", p.name));
            return None;
        }
        let since = p.since.unwrap_or(now);
        if let Some(q) = self.lib_ui.pending.as_mut() {
            q.since = Some(since);
        }
        if now - since >= WAIT_SECS {
            self.lib_ui.pending = None;
            self.warn(format!("{} was not opened: {cname} did not connect.", p.name));
            None
        } else {
            Some(WAIT_SECS - (now - since))
        }
    }

    /// Fills an empty library from the saved queries of ui.json, once: library.json is absent and the old map is not.
    /// The old map stays in ui.json, frozen, for an older build.
    pub fn migrate_library(&mut self) {
        if self.prefs.library.is_empty() {
            return;
        }
        let old: Vec<(String, Vec<crate::library::migrate::OldEntry>)> = self
            .prefs
            .library
            .iter()
            .map(|(k, v)| (k.clone(), v.iter().map(|q| crate::library::migrate::OldEntry { name: q.name.clone(), filter: q.filter.clone(), sort: q.sort.clone(), project: q.project.clone(), pipeline: q.pipeline.clone(), sql: q.sql.clone() }).collect()))
            .collect();
        let now = crate::library::now_secs();
        // Written even when nothing came over (every key was bad), so library.json exists and this never runs again.
        crate::library::migrate::migrate_into(&mut self.library, &old, now);
        self.save_library();
    }

    /// The database a connection opens in when nothing names one: the one in its address, else the first that is not the
    /// server's own (information_schema, mysql, admin and the like), never simply the first in the list.
    pub fn default_db(&self, conn: u64) -> String {
        let Some(c) = self.conn(conn) else { return String::new() };
        let dbs: Vec<&str> = c.dbs.iter().map(|d| d.0.as_str()).collect();
        let named = uri_db(&c.masked_uri);
        if !named.is_empty() && (dbs.is_empty() || dbs.contains(&named.as_str())) {
            return named;
        }
        let system = |d: &str| ["admin", "local", "config"].contains(&d) || db::sql::create::SYSTEM_SCHEMAS.contains(&d);
        dbs.iter().find(|d| !system(d)).or(dbs.first()).map(|d| d.to_string()).unwrap_or_default()
    }

    /// Scripts that left the library (deleted, or moved into a linked folder): tabs bound to them keep their text but are
    /// unbound, so their next Cmd+S opens the Save dialog, and they leave the recent list and a pending open.
    pub fn forget_scripts(&mut self, ids: &[u64]) {
        let gone = |r: &ScriptRef| matches!(r, ScriptRef::Saved(id) if ids.contains(id));
        for t in &mut self.tabs {
            if t.script.as_ref().is_some_and(|b| gone(&b.r)) {
                t.script = None;
            }
        }
        self.library.recent.retain(|r| !gone(r));
        if self.lib_ui.pending.as_ref().is_some_and(|p| gone(&p.r)) {
            self.lib_ui.pending = None;
        }
    }

    /// Called each frame from `library_tick`: starts the scans that are due. A link is scanned when first seen, when asked
    /// to (Reload, a saved file, a changed link), when the window gets focus back, and every 2 s while focused if it
    /// follows changes or one of its files is open. One scan per link runs at a time. `now` is the UI clock in seconds;
    /// answers how long until the next poll is due.
    fn linked_tick(&mut self, now: f64, focused: bool) -> Option<f64> {
        let regained = focused && !self.lib_ui.was_focused;
        self.lib_ui.was_focused = focused;
        // Links whose scans are set by hand, as the demo does, never touch the disk.
        if self.lib_ui.frozen {
            return None;
        }
        let mut next: Option<f64> = None;
        for (id, follow) in self.library.links.iter().map(|l| (l.id, l.follow)).collect::<Vec<_>>() {
            if self.lib_ui.scanning.contains(&id) {
                continue;
            }
            let open = self.tabs.iter().any(|t| matches!(t.script.as_ref().map(|b| &b.r), Some(ScriptRef::File { link, .. }) if *link == id));
            let last = self.lib_ui.scan_at.get(&id).copied();
            let asked = self.lib_ui.rescan.remove(&id);
            let polled = focused && (follow || open);
            let due = last.is_none() || asked || regained || (polled && now - last.unwrap_or(0.0) >= POLL_SECS);
            if due {
                self.start_scan(id, now);
            } else if polled {
                let wait = POLL_SECS - (now - last.unwrap_or(now));
                next = Some(next.map_or(wait, |n: f64| n.min(wait)));
            }
        }
        next.map(|wait| wait.max(0.05))
    }

    fn start_scan(&mut self, id: u64, now: f64) {
        let Some(l) = self.library.links.iter().find(|l| l.id == id) else { return };
        let (path, exts) = (l.path.clone(), l.exts.clone());
        let prev = self.lib_ui.scans.get(&id).cloned();
        // The files open in tabs are read in full by the scan when they changed, so `apply_scan` never reads on this thread.
        let open: Vec<(PathBuf, disk::Meta)> = self
            .tabs
            .iter()
            .filter_map(|t| match t.script.as_ref()? {
                Bound { r: ScriptRef::File { link, rel }, disk, .. } if *link == id && crate::library::safe_rel(rel) => Some((rel.clone(), *disk)),
                _ => None,
            })
            .collect();
        self.lib_ui.scanning.insert(id);
        self.lib_ui.scan_at.insert(id, now);
        self.worker.blocking(move || db::Msg::LibScan { link: id, scan: disk::scan_reading(&path, &exts, prev.as_ref(), &open) });
    }

    /// Runs `work` on the worker for the job `job`; its answer comes back to `file_done`.
    pub fn run_job(&mut self, job: Job, work: impl FnOnce() -> FileOut + Send + 'static) {
        self.lib_ui.job_seq += 1;
        let id = self.lib_ui.job_seq;
        self.lib_ui.jobs.insert(id, job);
        self.worker.blocking(move || db::Msg::LibFile { job: id, out: work() });
    }

    pub fn tab_by_uid(&self, uid: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.uid == uid)
    }

    /// The binding of tab `uid` when it is still to the file `rel` of `link`.
    fn bound_to(&mut self, uid: u64, link: u64, rel: &std::path::Path) -> Option<&mut Bound> {
        let i = self.tab_by_uid(uid)?;
        self.tabs[i].script.as_mut().filter(|b| matches!(&b.r, ScriptRef::File { link: l, rel: r } if *l == link && r == rel))
    }

    /// A linked-file job finished on the worker.
    pub fn file_done(&mut self, job: u64, out: FileOut) {
        let Some(job) = self.lib_ui.jobs.remove(&job) else { return };
        match (job, out) {
            (Job::Open { r, how, conn }, FileOut::Read(res)) => {
                let ScriptRef::File { link, rel } = &r else { return };
                match res.map_err(|e| format!("Could not open {}: {e}", file_label(rel))).and_then(|l| self.file_script_from(*link, rel, l, Some(conn))) {
                    // Opening a tab is the app's; M4b's tab actions take this over.
                    Ok((s, meta)) => self.ui_fx.push(UiFx::OpenResolved { r: r.clone(), how, script: Box::new(s), meta }),
                    Err(e) => self.warn(e),
                }
            }
            (Job::Copy { link, rel, at }, FileOut::Read(res)) => match res.map_err(|e| format!("Could not open {}: {e}", file_label(&rel))).and_then(|l| self.file_script_from(link, &rel, l, None)) {
                Ok((s, _)) => self.copy_into_library(s, &rel, at),
                Err(e) => self.warn(e),
            },
            (Job::Reload { uid, link, rel }, FileOut::Read(res)) => match res {
                Ok(now) => {
                    // Only a tab still bound to that file takes its text.
                    if self.bound_to(uid, link, &rel).is_none() {
                        return;
                    }
                    let Some(t) = self.tab_by_uid(uid).map(|i| &mut self.tabs[i]) else { return };
                    // Reload throws the tab's edits away, so the toast keeps them for Undo.
                    let mine = bound_text(t).filter(|m| *m != now.text).map(String::from);
                    set_bound_text(t, &now.text, now.parsed.as_ref());
                    if let Some(b) = t.script.as_mut() {
                        (b.base, b.disk, b.conflict) = (now.text, now.meta, None);
                    }
                    self.mark_seen(link, &rel, now.meta);
                    match mine {
                        Some(text) => self.toast_restore(format!("{} reloaded from disk.", file_label(&rel)), Restore::TabText { uid, text }),
                        None => self.ok(format!("{} reloaded from disk.", file_label(&rel))),
                    }
                }
                Err(e) => self.err(format!("Could not read {}: {e}", file_label(&rel))),
            },
            (Job::Save { uid, link, rel, text }, FileOut::Saved(res)) => {
                match res {
                    SaveOut::Written(meta) => {
                        if let Some(b) = self.bound_to(uid, link, &rel) {
                            (b.base, b.disk, b.conflict) = (text, meta, None);
                        }
                        self.mark_seen(link, &rel, meta);
                        self.rescan_link(link);
                        self.ok(format!("Saved {}", file_label(&rel)));
                    }
                    SaveOut::Changed(now) => {
                        if let Some(b) = self.bound_to(uid, link, &rel) {
                            (b.disk, b.conflict) = (now.meta, Some(now.text));
                        }
                        self.warn(format!("{} changed on disk. Nothing was saved.", file_label(&rel)));
                    }
                    SaveOut::Gone => {
                        // The next Cmd+S writes it back, now that the person has been told.
                        if let Some(b) = self.bound_to(uid, link, &rel) {
                            b.disk = None;
                        }
                        self.drop_entry(link, &rel);
                        self.warn(format!("{} was deleted on disk. Nothing was saved; save again to write it back.", file_label(&rel)));
                    }
                    SaveOut::Refused(e) => self.warn(e),
                    SaveOut::Failed(e) => self.err(e),
                }
            }
            (Job::SaveNew { uid, link, rel, view, body, place, password }, FileOut::Made(e, replaced)) => {
                let meta = Some((e.mtime, e.len));
                self.insert_entry(link, e);
                let r = ScriptRef::File { link, rel: rel.clone() };
                // Other tabs open on the file it replaced see the change now, as a scan would show it: a clean one that
                // follows takes the new text, an edited one gets the conflict banner.
                let others: Vec<usize> = (0..self.tabs.len()).filter(|i| self.tabs[*i].uid != uid && self.tabs[*i].script.as_ref().is_some_and(|b| b.r == r)).collect();
                for i in others {
                    self.file_changed(i, link, &rel, Loaded { text: body.clone(), parsed: None, meta });
                }
                match (view, self.tab_by_uid(uid)) {
                    (Some(v), Some(i)) if self.tabs[i].view == v => {
                        bind(&mut self.tabs[i], Bound { r: r.clone(), view: v, base: body, disk: meta, conflict: None });
                    }
                    _ => {}
                }
                self.library.push_recent(r);
                self.save_library();
                self.mark_seen(link, &rel, meta);
                match (replaced, self.library.link_join(link, &rel)) {
                    (Some((old, new)), Some(path)) => self.toast_restore(format!("Replaced {} in {place}.", file_label(&rel)), Restore::Replaced { link, path, old, new }),
                    _ => self.ok(format!("Saved {} to {place}", file_label(&rel))),
                }
                if password {
                    self.warn("Saved, but the script holds a password in plain text in the file.");
                }
            }
            (Job::SaveNew { .. }, FileOut::Saved(SaveOut::Refused(e))) => self.warn(e),
            (Job::SaveNew { .. }, FileOut::Saved(SaveOut::Failed(e))) => self.err(e),
            (Job::Trash { link, rel, path }, FileOut::Trashed(res)) => match res {
                Ok(bytes) => {
                    self.retarget(link, &rel, None);
                    self.drop_entry(link, &rel);
                    self.save_library();
                    self.toast_restore(format!("Moved {} to the Trash.", file_label(&rel)), Restore::File { path, bytes });
                }
                Err(e) => self.err(format!("Could not move {} to the Trash: {e}", file_label(&rel))),
            },
            _ => {}
        }
    }

    /// Whether the last scan of a link found its folder gone. Nothing is written there until a scan finds it again.
    pub fn link_missing(&self, link: u64) -> bool {
        self.lib_ui.scans.get(&link).is_some_and(|s| s.missing)
    }

    /// Asks for a new scan of a link at the next frame, such as after Reload or a file was written.
    pub fn rescan_link(&mut self, id: u64) {
        self.lib_ui.rescan.insert(id);
    }

    /// A scan arrived. It is stored, and any open tab whose file changed on disk is looked at. Link 0, the Link a folder
    /// dialog's preview, fills the app's dialog and never comes here.
    pub fn apply_scan(&mut self, link: u64, scan: disk::Scan) {
        self.lib_ui.scanning.remove(&link);
        if !self.library.links.iter().any(|l| l.id == link) {
            self.lib_ui.scans.remove(&link);
            return;
        }
        let changed: Vec<(usize, PathBuf)> = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(i, t)| match t.script.as_ref()? {
                Bound { r: ScriptRef::File { link: l, rel }, disk, .. } if *l == link => {
                    let e = scan.entries.iter().find(|e| e.rel == *rel)?;
                    (*disk != Some((e.mtime, e.len))).then(|| (i, rel.clone()))
                }
                _ => None,
            })
            .collect();
        // A file that differs from what Quarry last read or wrote of it gets a dot in the panel until it is opened or saved.
        // The first scan of a file is its baseline, so nothing shows changed at startup.
        let base = self.lib_ui.baseline.entry(link).or_default();
        for e in &scan.entries {
            match base.get(&e.rel) {
                None => {
                    base.insert(e.rel.clone(), (e.mtime, e.len));
                }
                Some(m) if *m != (e.mtime, e.len) => {
                    self.lib_ui.changed.insert((link, e.rel.clone()));
                }
                _ => {}
            }
        }
        let mut scan = scan;
        let texts = std::mem::take(&mut scan.texts);
        self.lib_ui.scans.insert(link, scan);
        for (i, rel) in changed {
            if let Some((_, now)) = texts.iter().find(|(r, _)| *r == rel) {
                self.file_changed(i, link, &rel, now.clone());
            }
        }
    }

    /// The file under tab `i` changed on disk. The rule, in order:
    /// 1. the text is the one the tab was opened or saved with: only the remembered time changes;
    /// 2. the tab holds no edits and the link follows changes: the tab takes the new text. A tab restored at launch has no
    ///    time on its binding and its base is only the text it held, which may be edits, so it never reloads by itself;
    /// 3. otherwise the tab keeps its text and shows the conflict banner.
    /// `now` is the file as the scan read it on the worker.
    fn file_changed(&mut self, i: usize, link: u64, rel: &std::path::Path, now: Loaded) {
        let Some(t) = self.tabs.get_mut(i) else { return };
        let (Some(b), Some(mine)) = (t.script.as_ref(), bound_text(t).map(String::from)) else { return };
        let follow = match &b.r {
            ScriptRef::File { link, .. } => self.library.links.iter().find(|l| l.id == *link).is_some_and(|l| l.follow),
            _ => false,
        };
        let meta = now.meta;
        let seen = if now.text == b.base {
            let b = t.script.as_mut().unwrap();
            (b.disk, b.conflict) = (now.meta, None);
            true
        } else if now.text == mine {
            // The file now holds what the tab holds, as when the same edit was made elsewhere: nothing to choose between.
            let b = t.script.as_mut().unwrap();
            (b.base, b.disk, b.conflict) = (now.text, now.meta, None);
            true
        } else if mine == b.base && follow && b.disk.is_some() {
            set_bound_text(t, &now.text, now.parsed.as_ref());
            let b = t.script.as_mut().unwrap();
            (b.base, b.disk, b.conflict) = (now.text, now.meta, None);
            true
        } else {
            let b = t.script.as_mut().unwrap();
            (b.disk, b.conflict) = (now.meta, Some(now.text));
            false
        };
        if seen {
            self.mark_seen(link, rel, meta);
        }
    }

    /// Cmd+S on a tab bound to a linked file: writes the file, or raises the conflict when it changed on disk since the tab
    /// loaded it. True when the tab is bound to a linked file and this handled the key.
    pub fn save_linked(&mut self) -> bool {
        let Some(t) = self.tabs.get(self.cur) else { return false };
        let Some(b) = t.script.as_ref().filter(|b| b.view == t.view) else { return false };
        let ScriptRef::File { link, rel } = b.r.clone() else { return false };
        let Some(path) = self.library.link_join(link, &rel) else {
            self.warn("The folder this file belongs to is no longer linked.");
            return true;
        };
        if self.demo {
            self.warn("The demo does not write files.");
            return true;
        }
        if b.conflict.is_some() {
            self.warn(format!("{} changed on disk. Use Reload or Keep mine above the editor first.", file_label(&rel)));
            return true;
        }
        if self.link_missing(link) {
            self.warn(MISSING);
            return true;
        }
        // One save of a file at a time: a second one sent before the first is back would carry the same old time and text,
        // and both would write through the same temp file.
        if self.lib_ui.jobs.values().any(|j| matches!(j, Job::Save { link: l, rel: r, .. } if *l == link && *r == rel)) {
            return true;
        }
        let (base, was, uid) = (b.base.clone(), b.disk, t.uid);
        let (view, text) = (b.view, bound_text(t).unwrap_or_default().to_string());
        let json = (format::ext_of(&path) == "json").then(|| disk::JsonParts {
            kind: if view == View::Aggregate { "pipeline" } else { "filter" }.into(),
            sort: t.sort_txt.clone(),
            project: t.proj_txt.clone(),
            target: format::Target { db: t.sel.db.clone(), schema: String::new(), coll: t.sel.coll.clone() },
        });
        let root = self.library.links.iter().find(|l| l.id == link).map(|l| l.path.clone()).unwrap_or_default();
        // The base the tab gets is the text sent, so an edit typed while the write runs still shows as an edit.
        let sent = text.clone();
        self.run_job(Job::Save { uid, link, rel, text }, move || FileOut::Saved(disk::save_file(&root, &path, was, &base, &sent, json)));
        true
    }

    /// Reload: the tab takes the text of the file, read on the worker.
    fn conflict_reload(&mut self, tab: u64) {
        let Some(t) = self.tabs.iter().find(|t| t.uid == tab) else { return };
        let Some(ScriptRef::File { link, rel }) = t.script.as_ref().map(|b| b.r.clone()) else { return };
        let Some(path) = self.library.link_join(link, &rel) else { return };
        let uid = t.uid;
        self.run_job(Job::Reload { uid, link, rel }, move || FileOut::Read(read_script(&path)));
    }

    /// Keep mine: the file's text becomes the base, so the tab still counts as edited and Cmd+S replaces the file.
    fn conflict_keep(&mut self, tab: u64) {
        if let Some(b) = self.tabs.iter_mut().find(|t| t.uid == tab).and_then(|t| t.script.as_mut()) {
            if let Some(text) = b.conflict.take() {
                b.base = text;
            }
        }
    }

    /// Compare: the lines of the tab against those of the file, in a text window.
    fn conflict_compare(&mut self, tab: u64) {
        let Some(t) = self.tabs.iter().find(|t| t.uid == tab) else { return };
        let (Some(b), Some(mine)) = (t.script.as_ref(), bound_text(t)) else { return };
        let (Some(theirs), ScriptRef::File { rel, .. }) = (b.conflict.clone(), &b.r) else { return };
        let body = match disk::diff_text(mine, &theirs) {
            Some(d) => format!("- only in this tab\n+ only in the file\n\n{d}"),
            None => "Too many lines to compare here. Use Reload or Keep mine.".to_string(),
        };
        let title = format!("{} in this tab and on disk", file_label(rel));
        self.ui_fx.push(UiFx::Open(Dialog::Text { title, body, code: true }));
    }
    /// What Quarry last read or wrote of a file, so the panel can tell that it changed since. A change found later by a scan
    /// puts a dot on its row until the file is opened or saved here.
    pub fn mark_seen(&mut self, link: u64, rel: &std::path::Path, meta: Meta) {
        self.lib_ui.changed.remove(&(link, rel.to_path_buf()));
        match meta {
            Some(m) => {
                self.lib_ui.baseline.entry(link).or_default().insert(rel.to_path_buf(), m);
            }
            None => {
                if let Some(b) = self.lib_ui.baseline.get_mut(&link) {
                    b.remove(rel);
                }
            }
        }
    }

    /// Puts the file `rel` into the stored scan of its link at once, so a file made here shows before the next scan.
    pub fn add_entry(&mut self, link: u64, rel: &std::path::Path) {
        let Some(root) = self.library.links.iter().find(|l| l.id == link).map(|l| l.path.clone()) else { return };
        let Some(e) = disk::entry_of(&root, rel) else { return };
        self.insert_entry(link, e);
    }

    /// `add_entry` with the entry already read, as a job on the worker does.
    pub fn insert_entry(&mut self, link: u64, e: disk::Entry) {
        let rel = e.rel.clone();
        let rel = rel.as_path();
        self.mark_seen(link, rel, Some((e.mtime, e.len)));
        let scan = self.lib_ui.scans.entry(link).or_default();
        scan.missing = false;
        scan.entries.retain(|x| x.rel != rel);
        scan.entries.push(e);
        scan.entries.sort_by(|a, b| a.rel.to_string_lossy().to_lowercase().cmp(&b.rel.to_string_lossy().to_lowercase()));
        scan.empty_dirs.retain(|d| !rel.starts_with(d));
        self.lib_ui.rescan.insert(link);
    }

    pub fn drop_entry(&mut self, link: u64, rel: &std::path::Path) {
        if let Some(s) = self.lib_ui.scans.get_mut(&link) {
            s.entries.retain(|x| x.rel != rel);
        }
        self.mark_seen(link, rel, None);
        self.lib_ui.rescan.insert(link);
    }

    /// The kind of a linked file: what its scan found, else its extension tells.
    pub fn file_kind(&self, link: u64, rel: &std::path::Path) -> String {
        kind_in(&self.lib_ui.scans, link, rel)
    }

    /// The connection a copy of a linked file is kept for in the library: the current tab's, then the active one, then any
    /// other the link is used on. Nothing runs on it, so unlike opening a file (`script_target`) it never asks.
    pub fn file_conn(&self, link: u64, rel: &std::path::Path) -> Option<u64> {
        let l = self.library.links.iter().find(|l| l.id == link)?;
        let kind = self.file_kind(link, rel);
        let order = self.t().map(|t| t.sel.conn).into_iter().chain(self.active).chain(self.conns.iter().map(|c| c.meta.id));
        order.into_iter().find(|id| self.conn(*id).is_some_and(|c| format::fits(l, &kind, *id, c.meta.engine)))
    }

    /// Reads the file `rel` on the worker, then `job` goes on with it in `file_done`.
    pub fn read_file_job(&mut self, link: u64, rel: &std::path::Path, job: Job) {
        let Some(path) = self.library.link_join(link, rel) else { return self.warn("That folder is no longer linked.") };
        self.run_job(job, move || FileOut::Read(read_script(&path)));
    }

    /// The file `rel`, as `read_script` read it, as the script a tab opens on `conn` (or the one `file_conn` gives), with the
    /// time and length it had. The target is a header the person wrote or the keys of a .json file; without one it is the
    /// current tab's database on that connection, or the connection's default.
    pub fn file_script_from(&self, link: u64, rel: &std::path::Path, loaded: Loaded, conn: Option<u64>) -> Result<(Script, Meta), String> {
        self.library.link_join(link, rel).ok_or("That folder is no longer linked.")?;
        let (kind, tgt, sort, project) = match &loaded.parsed {
            Some(p) => (p.kind.clone(), p.target.clone(), p.sort.clone(), p.project.clone()),
            None => (format::kind_of_ext(&format::ext_of(rel)).unwrap_or("sql").to_string(), format::header(&loaded.text), String::new(), String::new()),
        };
        let conn = conn.or_else(|| self.file_conn(link, rel)).ok_or("No connection this folder is used on can run that file.")?;
        let own = self.t().filter(|t| t.sel.conn == conn);
        let mut db = tgt.db.clone();
        if db.is_empty() {
            db = own.map(|t| t.sel.db.clone()).filter(|d| !d.is_empty()).unwrap_or_else(|| self.default_db(conn));
        }
        let mut coll = tgt.coll.clone();
        if matches!(kind.as_str(), "filter" | "pipeline") && coll.is_empty() {
            coll = own.map(|t| t.sel.coll.clone()).unwrap_or_default();
            if coll.is_empty() {
                return Err(format!("{} names no collection. Add \"collection\" to the file, or open it from a tab on one.", file_label(rel)));
            }
        }
        let script = Script { name: file_label(rel), kind, body: loaded.text, sort, project, conn, db, schema: tgt.schema, coll, ..Default::default() };
        Ok((script, loaded.meta))
    }

    /// Re-points the tabs bound to a file that moved, or unbinds them when it is gone (`to` None). Their text stays.
    fn retarget(&mut self, link: u64, from: &std::path::Path, to: Option<PathBuf>) {
        for t in &mut self.tabs {
            if let Some(b) = t.script.as_mut()
                && matches!(&b.r, ScriptRef::File { link: l, rel } if *l == link && rel == from)
            {
                match &to {
                    Some(n) => b.r = ScriptRef::File { link, rel: n.clone() },
                    None => t.script = None,
                }
            }
        }
        match &to {
            Some(n) => {
                for r in &mut self.library.recent {
                    if matches!(r, ScriptRef::File { link: l, rel } if *l == link && rel == from) {
                        *r = ScriptRef::File { link, rel: n.clone() };
                    }
                }
            }
            None => self.library.recent.retain(|r| !matches!(r, ScriptRef::File { link: l, rel } if *l == link && rel == from)),
        }
    }

    /// New script in a linked folder: an empty file of the first kind the link shows, named "New script", which the panel
    /// then lets the person name. Returns its path relative to the link.
    pub fn new_linked_script(&mut self, link: u64, dir: &std::path::Path) -> Option<PathBuf> {
        let l = self.library.links.iter().find(|l| l.id == link)?;
        let ext = ["sql", "js", "json"].into_iter().find(|e| l.exts.iter().any(|x| x == e)).unwrap_or("sql");
        let root = l.path.clone();
        let body: &[u8] = if ext == "json" { b"{\n  \"filter\": {}\n}\n" } else { b"" };
        match disk::create_file(&root, dir, "New script", ext, body) {
            Ok(p) => {
                let rel = p.strip_prefix(&root).ok()?.to_path_buf();
                self.add_entry(link, &rel);
                Some(rel)
            }
            Err(e) => {
                self.err(format!("Could not create the file: {e}"));
                None
            }
        }
    }

    /// New folder in a linked folder. It is empty, so the scan lists it among its empty folders.
    pub fn new_linked_dir(&mut self, link: u64, dir: &std::path::Path) -> Option<PathBuf> {
        let at = self.library.link_join(link, dir)?;
        let name = disk::unique_name(&at, "New folder");
        match std::fs::create_dir(at.join(&name)) {
            Ok(()) => {
                let rel = dir.join(&name);
                self.lib_ui.scans.entry(link).or_default().empty_dirs.push(rel.clone());
                self.lib_ui.rescan.insert(link);
                Some(rel)
            }
            Err(e) => {
                self.err(format!("Could not create the folder: {e}"));
                None
            }
        }
    }

    /// Renames a file or a folder below a link on disk, and returns its new path relative to the link. The files in a
    /// renamed folder keep their names.
    pub fn rename_linked(&mut self, link: u64, rel: &std::path::Path, is_dir: bool, text: &str) -> Option<PathBuf> {
        let from = self.library.link_join(link, rel)?;
        let new = if is_dir {
            let name = text.trim();
            if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
                self.warn("Use a name without slashes.");
                return None;
            }
            let to = from.with_file_name(name);
            if to != from && std::fs::symlink_metadata(&to).is_ok() && !disk::same_file_other_case(&from, &to) {
                self.warn(format!("{name} already exists here."));
                return None;
            }
            std::fs::rename(&from, &to).map(|()| to)
        } else {
            disk::rename_file(&from, text)
        };
        match new {
            Ok(to) => {
                let root = self.library.links.iter().find(|l| l.id == link)?.path.clone();
                let new_rel = to.strip_prefix(&root).ok()?.to_path_buf();
                if new_rel != rel {
                    if is_dir {
                        self.dir_moved(link, rel, &new_rel);
                    } else {
                        self.retarget(link, rel, Some(new_rel.clone()));
                        self.drop_entry(link, rel);
                        self.add_entry(link, &new_rel);
                    }
                    self.save_library();
                }
                Some(new_rel)
            }
            Err(e) => {
                self.warn(format!("Could not rename: {e}"));
                None
            }
        }
    }

    /// A folder moved: tabs bound to files in it follow, and the link is read again.
    fn dir_moved(&mut self, link: u64, old: &std::path::Path, new: &std::path::Path) {
        let moved: Vec<PathBuf> = self.lib_ui.scans.get(&link).map(|s| s.entries.iter().filter(|e| e.rel.starts_with(old)).map(|e| e.rel.clone()).collect()).unwrap_or_default();
        for f in moved {
            if let Ok(rest) = f.strip_prefix(old) {
                self.retarget(link, &f, Some(new.join(rest)));
            }
        }
        self.lib_ui.scans.remove(&link);
        self.lib_ui.rescan.insert(link);
    }

    /// Moves a file to the Trash on the worker, keeping its bytes for the Undo toast. A tab that holds it keeps its text but
    /// is unbound. True when the move started.
    pub fn delete_linked_file(&mut self, link: u64, rel: &std::path::Path) -> bool {
        let Some(path) = self.library.link_join(link, rel) else { return false };
        if self.demo {
            self.warn("The demo does not delete files.");
            return false;
        }
        let p = path.clone();
        self.run_job(Job::Trash { link, rel: rel.to_path_buf(), path }, move || FileOut::Trashed(disk::trash_file(&p)));
        true
    }

    /// Moves a file into another folder of the same link, never over a file that is there.
    pub fn move_linked_file(&mut self, link: u64, rel: &std::path::Path, dir: &std::path::Path) -> Option<PathBuf> {
        let root = self.library.links.iter().find(|l| l.id == link)?.path.clone();
        let (from, at) = (self.library.link_join(link, rel)?, self.library.link_join(link, dir)?);
        let name = rel.file_name()?.to_string_lossy().into_owned();
        let to = at.join(disk::unique_name(&at, &name));
        if let Err(e) = std::fs::rename(&from, &to) {
            self.warn(format!("Could not move {name}: {e}"));
            return None;
        }
        let new_rel = to.strip_prefix(&root).ok()?.to_path_buf();
        self.retarget(link, rel, Some(new_rel.clone()));
        self.drop_entry(link, rel);
        self.add_entry(link, &new_rel);
        self.save_library();
        if let Some(l) = self.library.links.iter().find(|l| l.id == link).cloned() {
            self.ok(format!("Moved {name} to {}.", link_place(&l, &new_rel)));
        }
        Some(new_rel)
    }

    /// A script kept by Quarry written into a linked folder as a file. The Quarry copy is removed and Undo puts it back (the
    /// file goes again, unless it was edited since). Refused when the folder does not show files of that kind.
    pub fn script_to_link(&mut self, id: u64, link: u64, dir: &std::path::Path) -> Option<PathBuf> {
        let s = self.library.scripts.iter().find(|s| s.id == id)?.clone();
        let l = self.library.links.iter().find(|l| l.id == link)?.clone();
        // A SQL Content filter has no file form: as .json it would read back as a Mongo filter, as .sql as a script.
        if s.kind == "where" {
            self.warn(format!("\"{}\" is a table filter, which is only kept in Quarry. It stays where it is.", s.name));
            return None;
        }
        let ext = kind_ext(&s.kind);
        if !l.exts.iter().any(|e| e == ext) {
            self.warn(format!("{} does not show .{ext} files. Add the type in Use on connections first.", l.name));
            return None;
        }
        if self.demo {
            self.warn("The demo does not write files.");
            return None;
        }
        if self.link_missing(link) {
            self.warn(MISSING);
            return None;
        }
        let target = format::Target { db: s.db.clone(), schema: s.schema.clone(), coll: s.coll.clone() };
        let bytes = match file_bytes(&s.kind, &s.body, &s.sort, &s.project, target, None) {
            Ok(b) => b,
            Err(e) => {
                self.warn(format!("Not moved: {e}"));
                return None;
            }
        };
        let path = match disk::create_file(&l.path, dir, &s.name, ext, &bytes) {
            Ok(p) => p,
            Err(e) => {
                self.err(format!("Could not write the file: {e}"));
                return None;
            }
        };
        let rel = path.strip_prefix(&l.path).ok()?.to_path_buf();
        self.add_entry(link, &rel);
        if let Some((folders, scripts)) = ops::delete(&mut self.library, &Node::Script(id)) {
            self.forget_scripts(&[id]);
            self.save_library();
            self.toast_restore(format!("Moved \"{}\" to disk as {}. It left the library.", s.name, link_place(&l, &rel)), Restore::Saved { scripts, folders, written: Some((link, rel.clone(), bytes.clone())) });
        }
        Some(rel)
    }

    /// A linked file copied into the library as a script. The file stays where it is. `at` places the copy in a folder. The
    /// file is read on the worker, and the copy is made and selected in the panel when it comes back.
    pub fn file_to_quarry(&mut self, link: u64, rel: &std::path::Path, at: DropAt) {
        self.read_file_job(link, rel, Job::Copy { link, rel: rel.to_path_buf(), at });
    }

    fn copy_into_library(&mut self, mut s: Script, rel: &std::path::Path, at: DropAt) {
        s.id = self.library.next_id();
        s.updated = crate::library::now_secs();
        if let Some(n) = rel.file_stem() {
            s.name = n.to_string_lossy().into_owned();
        }
        let (id, name) = (s.id, s.name.clone());
        self.library.scripts.push(s);
        if let DropAt::Into(f) = at {
            self.lib_ui.panel.open.insert(Node::Folder(f));
        }
        if let Err(e) = ops::move_to(&mut self.library, &Node::Script(id), at) {
            self.warn(e);
        }
        self.lib_ui.panel.sel = Some(Node::Script(id));
        self.save_library();
        let folder = self.library.scripts.iter().find(|s| s.id == id).and_then(|s| crate::library::tree::live_parent(&self.library, s.folder));
        let place = ops::folder_path(&self.library, folder);
        let place = if place.is_empty() { "the top of the library".to_string() } else { place };
        self.ok(format!("Copied {name} to {place}. The file stays in the folder."));
    }

    /// Unlink: the link and what Quarry remembers of it go, the files stay on disk. Tabs bound to its files keep their text
    /// and are unbound, and the toast says how many, with an Undo that links the folder again and binds them back.
    pub fn unlink(&mut self, link: u64) {
        let Some(at) = self.library.links.iter().position(|l| l.id == link) else { return };
        self.library.reserve_ids();
        let l = self.library.links.remove(at);
        let recent: Vec<(usize, ScriptRef)> = self.library.recent.iter().enumerate().filter(|(_, r)| matches!(r, ScriptRef::File { link: x, .. } if *x == link)).map(|(i, r)| (i, r.clone())).collect();
        self.library.recent.retain(|r| !matches!(r, ScriptRef::File { link: x, .. } if *x == link));
        let mut tabs = Vec::new();
        for t in &mut self.tabs {
            if matches!(t.script.as_ref().map(|b| &b.r), Some(ScriptRef::File { link: x, .. }) if *x == link)
                && let Some(b) = t.script.take()
            {
                tabs.push((t.uid, b));
            }
        }
        self.lib_ui.scans.remove(&link);
        self.lib_ui.changed.retain(|(x, _)| *x != link);
        self.lib_ui.baseline.remove(&link);
        self.save_library();
        let name = if l.name.is_empty() { l.path.display().to_string() } else { l.name.clone() };
        let unbound = match tabs.len() {
            0 => String::new(),
            1 => " 1 tab was unbound.".to_string(),
            n => format!(" {n} tabs were unbound."),
        };
        self.toast_restore(format!("Unlinked {name}. Its files stay on disk.{unbound}"), Restore::Link { link: l, at, tabs, recent });
    }

    /// Undo of Unlink: the link goes back where it was, with its recent files, and the tabs that still show no other script
    /// are bound to their files again. Their disk state is checked at the scan that follows.
    pub fn relink(&mut self, l: crate::library::Link, at: usize, tabs: Vec<(u64, Bound)>, recent: Vec<(usize, ScriptRef)>) {
        let (id, name) = (l.id, if l.name.is_empty() { l.path.display().to_string() } else { l.name.clone() });
        if self.library.links.iter().any(|x| x.id == id) {
            return;
        }
        self.library.links.insert(at.min(self.library.links.len()), l);
        for (i, r) in recent {
            if !self.library.recent.contains(&r) {
                let i = i.min(self.library.recent.len());
                self.library.recent.insert(i, r);
            }
        }
        self.library.recent.truncate(crate::library::RECENT_MAX);
        for (uid, b) in tabs {
            if let Some(t) = self.tabs.iter_mut().find(|t| t.uid == uid && t.script.is_none() && t.view == b.view) {
                bind(t, b);
            }
        }
        self.save_library();
        self.rescan_link(id);
        self.ok(format!("{name} is linked again."));
    }}
