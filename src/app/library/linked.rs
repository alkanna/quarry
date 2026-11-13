//! Linked folders in the app: scanning them off the UI thread, noticing that a file under an open tab changed on disk, the
//! conflict banner, and writing a bound file back. The files themselves are read and written through `crate::library::disk`.

use super::*;
use crate::app::restore::Restore;
use crate::library::ops::{self, DropAt};
use crate::library::tree::Node;
use crate::library::{Library, Script, ScriptRef, disk, format};
use std::time::{Duration, SystemTime};

/// How often a followed link, or one with an open tab, is scanned again while the window has focus. Polling replaces a
/// file watcher, which would add a dependency and miss changes on network drives.
const POLL_SECS: f64 = 2.0;

pub(in crate::app) use crate::library::disk::{FileOut, Loaded, SaveOut, read_script};

/// A linked-file job running on the worker, by id in `LibUi::jobs`: what to do with its answer. Tabs are named by their
/// `uid`, since the tab list can change before the answer comes back.
pub(in crate::app) enum Job {
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

/// The text of the view a tab's script is bound to. Borrowed, since the tab strip asks every frame whether each tab
/// holds edits.
pub(in crate::app) fn bound_text(t: &Tab) -> Option<&str> {
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
pub(in crate::app) fn set_bound_text(t: &mut Tab, text: &str, parsed: Option<&format::Parsed>) {
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
fn link_place(l: &crate::library::Link, rel: &std::path::Path) -> String {
    let name = if l.name.is_empty() { l.path.display().to_string() } else { l.name.clone() };
    std::iter::once(name).chain(rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned())).collect::<Vec<_>>().join(" / ")
}

fn file_label(rel: &std::path::Path) -> String {
    rel.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string()
}

impl Library {
    /// The folder of a link and the file `rel` below it. None when the link is gone or `rel` would leave its folder.
    pub(in crate::app) fn file_path(&self, link: u64, rel: &std::path::Path) -> Option<PathBuf> {
        self.link_join(link, rel)
    }
}

impl App {
    /// Called each frame from `library_tick`: starts the scans that are due. A link is scanned when first seen, when asked
    /// to (Reload, a saved file, a changed link), when the window gets focus back, and every 2 s while focused if it
    /// follows changes or one of its files is open. One scan per link runs at a time.
    pub(super) fn linked_tick(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        let regained = focused && !self.lib_ui.was_focused;
        self.lib_ui.was_focused = focused;
        // Links whose scans are set by hand, as the demo does, never touch the disk.
        if self.lib_ui.frozen {
            return;
        }
        let now = ctx.input(|i| i.time);
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
        if let Some(wait) = next {
            ctx.request_repaint_after(Duration::from_secs_f64(wait.max(0.05)));
        }
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
    pub(in crate::app) fn run_job(&mut self, job: Job, work: impl FnOnce() -> FileOut + Send + 'static) {
        self.lib_ui.job_seq += 1;
        let id = self.lib_ui.job_seq;
        self.lib_ui.jobs.insert(id, job);
        self.worker.blocking(move || db::Msg::LibFile { job: id, out: work() });
    }

    fn tab_by_uid(&self, uid: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.uid == uid)
    }

    /// The binding of tab `uid` when it is still to the file `rel` of `link`.
    fn bound_to(&mut self, uid: u64, link: u64, rel: &std::path::Path) -> Option<&mut Bound> {
        let i = self.tab_by_uid(uid)?;
        self.tabs[i].script.as_mut().filter(|b| matches!(&b.r, ScriptRef::File { link: l, rel: r } if *l == link && r == rel))
    }

    /// A linked-file job finished on the worker.
    pub(in crate::app) fn file_done(&mut self, job: u64, out: FileOut) {
        let Some(job) = self.lib_ui.jobs.remove(&job) else { return };
        match (job, out) {
            (Job::Open { r, how, conn }, FileOut::Read(res)) => {
                let ScriptRef::File { link, rel } = &r else { return };
                match res.map_err(|e| format!("Could not open {}: {e}", file_label(rel))).and_then(|l| self.file_script_from(*link, rel, l, Some(conn))) {
                    Ok((s, meta)) => self.open_resolved(&r, how, s, Some(meta)),
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
                    (Some(v), Some(i)) if self.tabs[i].view == v => self.tabs[i].script = Some(Bound { r: r.clone(), view: v, base: body, disk: meta, conflict: None }),
                    _ => {}
                }
                self.library.push_recent(r);
                self.save_library();
                self.mark_seen(link, &rel, meta);
                match (replaced, self.library.file_path(link, &rel)) {
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
    pub(in crate::app) fn link_missing(&self, link: u64) -> bool {
        self.lib_ui.scans.get(&link).is_some_and(|s| s.missing)
    }

    /// Asks for a new scan of a link at the next frame, such as after Reload or a file was written.
    pub(in crate::app) fn rescan_link(&mut self, id: u64) {
        self.lib_ui.rescan.insert(id);
    }

    /// A scan arrived. Link 0 is the Link a folder dialog's preview. Otherwise it is stored, and any open tab whose file
    /// changed on disk is looked at.
    pub(in crate::app) fn apply_scan(&mut self, link: u64, scan: disk::Scan) {
        if link == 0 {
            if let Some(Modal::LinkFolder(f)) = &mut self.modal {
                f.got_preview(scan);
            }
            return;
        }
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
        for e in &scan.entries {
            let k = (link, e.rel.clone());
            match self.lib_ui.baseline.get(&k) {
                None => {
                    self.lib_ui.baseline.insert(k, (e.mtime, e.len));
                }
                Some(m) if *m != (e.mtime, e.len) => {
                    self.lib_ui.changed.insert(k);
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
    pub(in crate::app) fn save_linked(&mut self) -> bool {
        let Some(t) = self.tabs.get(self.cur) else { return false };
        let Some(b) = t.script.as_ref().filter(|b| b.view == t.view) else { return false };
        let ScriptRef::File { link, rel } = b.r.clone() else { return false };
        let Some(path) = self.library.file_path(link, &rel) else {
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
            self.warn(super::panel::MISSING);
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

    /// The conflict banner above the editor of a tab whose file changed on disk under unsaved edits.
    pub(in crate::app) fn conflict_banner(&mut self, ui: &mut egui::Ui) {
        let Some(t) = self.tabs.get(self.cur) else { return };
        let Some(b) = t.script.as_ref().filter(|b| b.conflict.is_some() && b.view == t.view) else { return };
        let ScriptRef::File { rel, .. } = &b.r else { return };
        let name = file_label(rel);
        let mut act = 0;
        banner_frame(WARNING()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.set_min_height(BUTTON_H);
                glyph_icon(ui, glyphs::warn, WARNING(), 16.0);
                ui.add_space(4.0);
                ui.label(semibold(RichText::new(format!("{name} changed on disk")).size(TXT_BODY).color(BRIGHT())));
                ui.label(RichText::new("This tab has unsaved edits").size(TXT_BODY).color(TEXT()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    for (n, label, tip) in [(3, "Compare", "See what differs"), (2, "Keep mine", "Keep your edits; saving then replaces the file"), (1, "Reload", "Replace your edits with the file. Undo brings them back")] {
                        if quiet(ui, label).on_hover_text(tip).clicked() {
                            act = n;
                        }
                    }
                });
            });
        });
        ui.add_space(10.0);
        match act {
            1 => self.conflict_reload(),
            2 => self.conflict_keep(),
            3 => self.conflict_compare(),
            _ => {}
        }
    }

    /// Reload: the tab takes the text of the file, read on the worker.
    pub(in crate::app) fn conflict_reload(&mut self) {
        let Some(t) = self.tabs.get(self.cur) else { return };
        let Some(ScriptRef::File { link, rel }) = t.script.as_ref().map(|b| b.r.clone()) else { return };
        let Some(path) = self.library.file_path(link, &rel) else { return };
        let uid = t.uid;
        self.run_job(Job::Reload { uid, link, rel }, move || FileOut::Read(read_script(&path)));
    }

    /// Keep mine: the file's text becomes the base, so the tab still counts as edited and Cmd+S replaces the file.
    pub(in crate::app) fn conflict_keep(&mut self) {
        if let Some(b) = self.tabs.get_mut(self.cur).and_then(|t| t.script.as_mut()) {
            if let Some(text) = b.conflict.take() {
                b.base = text;
            }
        }
    }

    /// Compare: the lines of the tab against those of the file, in a text window.
    pub(in crate::app) fn conflict_compare(&mut self) {
        let Some(t) = self.tabs.get(self.cur) else { return };
        let (Some(b), Some(mine)) = (t.script.as_ref(), bound_text(t)) else { return };
        let (Some(theirs), ScriptRef::File { rel, .. }) = (b.conflict.clone(), &b.r) else { return };
        let body = match disk::diff_text(mine, &theirs) {
            Some(d) => format!("- only in this tab\n+ only in the file\n\n{d}"),
            None => "Too many lines to compare here. Use Reload or Keep mine.".to_string(),
        };
        self.modal = Some(Modal::Text { title: format!("{} in this tab and on disk", file_label(rel)), body, code: true });
    }
}

/// Opens `path` in the file manager, selecting it where the system can. The one place that starts the file manager, so
/// the audit log and the linked folders do it the same way.
pub(in crate::app) fn reveal(path: &std::path::Path, select: bool) -> std::io::Result<()> {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        if select {
            c.arg("-R");
        }
        c.arg(path);
        c
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("explorer");
        if select {
            c.arg(format!("/select,{}", path.display()));
        } else {
            c.arg(path);
        }
        c
    } else {
        // xdg-open has no select; the folder that holds the file is the closest.
        let mut c = std::process::Command::new("xdg-open");
        c.arg(if select { path.parent().unwrap_or(path) } else { path });
        c
    };
    cmd.spawn().map(|_| ())
}

/// The extension a script of `kind` is kept under in a linked folder.
pub(in crate::app) fn kind_ext(kind: &str) -> &'static str {
    match kind {
        "sql" => "sql",
        "shell" => "js",
        _ => "json",
    }
}

/// The bytes of a linked file for a script. SQL and Shell files are the text as it is, with no header added. A filter or a
/// pipeline is the small JSON shape, which needs its collection; `old` is the file's text now, so its other keys stay.
pub(in crate::app) fn file_bytes(kind: &str, body: &str, sort: &str, project: &str, target: format::Target, old: Option<&str>) -> Result<Vec<u8>, String> {
    if kind_ext(kind) != "json" {
        return Ok(body.as_bytes().to_vec());
    }
    let p = format::Parsed { kind: kind.into(), body: body.into(), sort: sort.into(), project: project.into(), target };
    format::to_json(&p, old).map(String::into_bytes)
}

type Meta = Option<(SystemTime, u64)>;

impl App {
    /// What Quarry last read or wrote of a file, so the panel can tell that it changed since. A change found later by a scan
    /// puts a dot on its row until the file is opened or saved here.
    pub(in crate::app) fn mark_seen(&mut self, link: u64, rel: &std::path::Path, meta: Meta) {
        let k = (link, rel.to_path_buf());
        self.lib_ui.changed.remove(&k);
        match meta {
            Some(m) => self.lib_ui.baseline.insert(k, m),
            None => self.lib_ui.baseline.remove(&k),
        };
    }

    /// Puts the file `rel` into the stored scan of its link at once, so a file made here shows before the next scan.
    pub(in crate::app) fn add_entry(&mut self, link: u64, rel: &std::path::Path) {
        let Some(root) = self.library.links.iter().find(|l| l.id == link).map(|l| l.path.clone()) else { return };
        let Some(e) = disk::entry_of(&root, rel) else { return };
        self.insert_entry(link, e);
    }

    /// `add_entry` with the entry already read, as a job on the worker does.
    pub(in crate::app) fn insert_entry(&mut self, link: u64, e: disk::Entry) {
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

    pub(in crate::app) fn drop_entry(&mut self, link: u64, rel: &std::path::Path) {
        if let Some(s) = self.lib_ui.scans.get_mut(&link) {
            s.entries.retain(|x| x.rel != rel);
        }
        self.mark_seen(link, rel, None);
        self.lib_ui.rescan.insert(link);
    }

    /// The kind of a linked file: what its scan found, else its extension tells.
    pub(in crate::app) fn file_kind(&self, link: u64, rel: &std::path::Path) -> String {
        let found = self.lib_ui.scans.get(&link).and_then(|s| s.entries.iter().find(|e| e.rel == rel)).map(|e| e.kind.clone());
        found.or_else(|| format::kind_of_ext(&format::ext_of(rel)).map(String::from)).unwrap_or_default()
    }

    /// The connection a copy of a linked file is kept for in the library: the current tab's, then the active one, then any
    /// other the link is used on. Nothing runs on it, so unlike opening a file (`script_target`) it never asks.
    pub(in crate::app) fn file_conn(&self, link: u64, rel: &std::path::Path) -> Option<u64> {
        let l = self.library.links.iter().find(|l| l.id == link)?;
        let kind = self.file_kind(link, rel);
        let order = self.t().map(|t| t.sel.conn).into_iter().chain(self.active).chain(self.conns.iter().map(|c| c.meta.id));
        order.into_iter().find(|id| self.conn(*id).is_some_and(|c| format::fits(l, &kind, *id, c.meta.engine)))
    }

    /// Reads the file `rel` on the worker, then `job` goes on with it in `file_done`.
    pub(in crate::app) fn read_file_job(&mut self, link: u64, rel: &std::path::Path, job: Job) {
        let Some(path) = self.library.file_path(link, rel) else { return self.warn("That folder is no longer linked.") };
        self.run_job(job, move || FileOut::Read(read_script(&path)));
    }

    /// The file `rel`, as `read_script` read it, as the script a tab opens on `conn` (or the one `file_conn` gives), with the
    /// time and length it had. The target is a header the person wrote or the keys of a .json file; without one it is the
    /// current tab's database on that connection, or the connection's default.
    pub(in crate::app) fn file_script_from(&self, link: u64, rel: &std::path::Path, loaded: Loaded, conn: Option<u64>) -> Result<(Script, Meta), String> {
        self.library.file_path(link, rel).ok_or("That folder is no longer linked.")?;
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
    pub(in crate::app) fn new_linked_script(&mut self, link: u64, dir: &std::path::Path) -> Option<PathBuf> {
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
    pub(in crate::app) fn new_linked_dir(&mut self, link: u64, dir: &std::path::Path) -> Option<PathBuf> {
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
    pub(in crate::app) fn rename_linked(&mut self, link: u64, rel: &std::path::Path, is_dir: bool, text: &str) -> Option<PathBuf> {
        let from = self.library.file_path(link, rel)?;
        let new = if is_dir {
            let name = text.trim();
            if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
                self.warn("Use a name without slashes.");
                return None;
            }
            let to = from.with_file_name(name);
            if to != from && std::fs::symlink_metadata(&to).is_ok() {
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
    pub(in crate::app) fn delete_linked_file(&mut self, link: u64, rel: &std::path::Path) -> bool {
        let Some(path) = self.library.file_path(link, rel) else { return false };
        if self.demo {
            self.warn("The demo does not delete files.");
            return false;
        }
        let p = path.clone();
        self.run_job(Job::Trash { link, rel: rel.to_path_buf(), path }, move || FileOut::Trashed(disk::trash_file(&p)));
        true
    }

    /// Moves a file into another folder of the same link, never over a file that is there.
    pub(in crate::app) fn move_linked_file(&mut self, link: u64, rel: &std::path::Path, dir: &std::path::Path) -> Option<PathBuf> {
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
    pub(in crate::app) fn script_to_link(&mut self, id: u64, link: u64, dir: &std::path::Path) -> Option<PathBuf> {
        let s = self.library.scripts.iter().find(|s| s.id == id)?.clone();
        let l = self.library.links.iter().find(|l| l.id == link)?.clone();
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
            self.warn(super::panel::MISSING);
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
    pub(in crate::app) fn file_to_quarry(&mut self, link: u64, rel: &std::path::Path, at: DropAt) {
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
    pub(in crate::app) fn unlink(&mut self, link: u64) {
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
        self.lib_ui.baseline.retain(|(x, _), _| *x != link);
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
    pub(in crate::app) fn relink(&mut self, l: crate::library::Link, at: usize, tabs: Vec<(u64, Bound)>, recent: Vec<(usize, ScriptRef)>) {
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
                t.script = Some(b);
            }
        }
        self.save_library();
        self.rescan_link(id);
        self.ok(format!("{name} is linked again."));
    }
}

/// A made-up scan of the demo folder: `files` are (path below the folder, kind, first text).
fn demo_scan(files: &[(&str, &str, &str)]) -> disk::Scan {
    let now = SystemTime::now();
    let entries = files
        .iter()
        .enumerate()
        .map(|(i, (rel, kind, head))| disk::Entry { rel: PathBuf::from(rel), mtime: now - Duration::from_secs(600 + 7200 * i as u64), len: head.len() as u64, kind: kind.to_string(), head: head.to_string(), target: format::Target::default() })
        .collect();
    disk::Scan { branch: Some("main".into()), entries, ..Default::default() }
}

const DEMO_FILES: [(&str, &str, &str); 14] = [
    ("monthly-revenue.sql", "sql", "SELECT DATE_FORMAT(created_at, '%Y-%m') AS month,\n       SUM(total) AS revenue\nFROM orders\nWHERE status = 'paid'\nGROUP BY month\nORDER BY month DESC;"),
    ("top-customers.sql", "sql", "SELECT customer_id, SUM(total) AS spent\nFROM orders\nGROUP BY customer_id\nORDER BY spent DESC\nLIMIT 20;"),
    ("slow-lookup.sql", "sql", "EXPLAIN SELECT * FROM orders WHERE customer_id = 42;"),
    ("scratch.sql", "sql", "SELECT 1;"),
    ("reports/orders-per-day.sql", "sql", "SELECT DATE(created_at) AS day, COUNT(*) FROM orders GROUP BY day;"),
    ("reports/refunds.sql", "sql", "SELECT * FROM refunds WHERE created_at > NOW() - INTERVAL 7 DAY;"),
    ("reports/stock-low.sql", "sql", "SELECT sku, qty FROM stock WHERE qty < 5;"),
    ("reports/by-region.sql", "sql", "SELECT region, SUM(total) FROM orders GROUP BY region;"),
    ("reports/cohorts.sql", "sql", "SELECT 1;"),
    ("reports/shipping.sql", "sql", "SELECT 1;"),
    ("maintenance/stale-carts.sql", "sql", "DELETE FROM carts WHERE updated_at < NOW() - INTERVAL 30 DAY;"),
    ("maintenance/vacuum.sql", "sql", "OPTIMIZE TABLE orders;"),
    ("maintenance/backfill.js", "shell", "const n = await orders.countDocuments({ total: null });"),
    ("maintenance/indexes.sql", "sql", "SHOW INDEX FROM orders;"),
];

impl App {
    /// QUARRY_TEST_LIBRARY=linked, missing, link, save-linked or conflict: linked folders over made-up scans, so nothing
    /// reads the disk. True when `state` is one of them.
    pub(in crate::app) fn demo_library_linked(&mut self, state: &str) -> bool {
        if !matches!(state, "linked" | "missing" | "link" | "save-linked" | "conflict") {
            return false;
        }
        self.demo_library("panel");
        self.lib_ui.panel.sel = None;
        self.lib_ui.frozen = true;
        let link = |id, name: &str, path: &str| crate::library::Link { id, name: name.into(), path: path.into(), on: "engine".into(), engine: "mysql".into(), exts: ["sql", "js", "json"].map(String::from).to_vec(), follow: true, ..Default::default() };
        if state != "link" {
            self.library.links.push(link(100, "shop-queries", "/Users/demo/work/shop-queries"));
            self.lib_ui.scans.insert(100, demo_scan(&DEMO_FILES));
            let st = &mut self.lib_ui.panel;
            st.open.insert(Node::Link(100));
            st.open.insert(Node::Dir { link: 100, rel: "reports".into() });
            self.lib_ui.changed.insert((100, "reports/refunds.sql".into()));
        }
        match state {
            "linked" => self.lib_ui.panel.sel = Some(Node::File { link: 100, rel: "monthly-revenue.sql".into() }),
            "missing" => {
                self.library.links.push(link(101, "legacy-exports", "/Volumes/archive/exports"));
                self.lib_ui.scans.insert(101, disk::Scan { missing: true, ..Default::default() });
            }
            "link" => {
                self.open_link_dialog(None);
                let scan = demo_scan(&DEMO_FILES);
                if let Some(Modal::LinkFolder(f)) = &mut self.modal {
                    f.fill_demo("/Users/demo/work/shop-queries", scan);
                }
            }
            "save-linked" => {
                if self.demo_library_quick("save")
                    && let Some(Modal::SaveScript(f)) = &mut self.modal
                {
                    (f.link, f.dir, f.name) = (Some(100), "reports".into(), "Monthly revenue".into());
                }
            }
            _ => {
                // A tab open on a file whose text was edited here and changed on disk since.
                let rel = PathBuf::from("monthly-revenue.sql");
                let base = DEMO_FILES[0].2.to_string();
                let mine = base.replace("ORDER BY month DESC", "ORDER BY month ASC");
                let theirs = base.replace("status = 'paid'", "status IN ('paid', 'shipped')");
                let mut t = Tab::new(Sel { conn: 3, db: "shop".into(), schema: String::new(), coll: String::new() }).for_engine(db::sql::Engine::MySql);
                t.view = View::Sql;
                if let Some(q) = t.sql.as_mut() {
                    q.query = mine;
                }
                t.script = Some(Bound { r: ScriptRef::File { link: 100, rel }, view: View::Sql, base, disk: None, conflict: Some(theirs) });
                self.tabs = vec![t];
                self.cur = 0;
            }
        }
        true
    }
}
