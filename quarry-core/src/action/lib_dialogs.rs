//! What the library's dialogs commit: Save to library, Link a folder, the snippet form's values and Make default, and
//! Make it a value. src/app/library/ draws them and sends `LibA::Save`, `SaveLink`, `SnippetValues`, `SnippetDefault`
//! and `MakeValue`.

use super::library::MISSING;
use crate::db;
use crate::core::Core;
use crate::library::disk::{self, FileOut, SaveOut};
use crate::library::{format, Link, Script, ScriptRef};
use crate::state::library::linked::{file_bytes, kind_ext, Job};
use crate::state::library::open::{bind, TabText};
use crate::state::library::save_dialog::{same_place, styled_name, SaveForm};
use crate::state::library::snippet_strip::{text_mut, Src};
use crate::state::restore::Restore;
use crate::state::{Bound, View};
use std::ops::Range;

impl Core {
    /// Ties the current tab to a script, so Cmd+S writes it back, and puts it first in the recent list.
    pub fn bind_script(&mut self, r: ScriptRef, view: View, base: String) {
        if let Some(t) = self.tm() {
            bind(t, Bound { r: r.clone(), view, base, disk: None, conflict: None });
        }
        self.library.push_recent(r);
        self.save_library();
    }

    /// A SQL script with a password in it is saved as typed, since one with the password cut out would not run, and the
    /// person is told where it now sits.
    pub fn warn_plain_password(&mut self, tt: &TabText) {
        if tt.kind == "sql" && !db::redact_sql(&tt.body).1.is_empty() {
            self.warn("Saved, but the script holds a password in plain text in the library file.");
        }
    }

    /// Writes the script the dialog describes: a new one, or the one of the same name in the same folder, replaced. The tab
    /// is bound to it, so the next Cmd+S writes back without asking.
    pub(super) fn commit_save(&mut self, f: &SaveForm) {
        if let Some(link) = f.link {
            return self.commit_save_linked(f, link);
        }
        let name = f.name.trim().to_string();
        let mut folder = f.folder;
        if let Some(n) = f.new_folder.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            folder = Some(match self.library.folders.iter().find(|x| crate::library::tree::live_parent(&self.library, x.parent).is_none() && x.name == n) {
                Some(x) => x.id,
                None => {
                    let id = self.library.next_id();
                    self.library.folders.push(crate::library::Folder { id, name: n.to_string(), parent: None });
                    id
                }
            });
        }
        let mut replaced = None;
        let id = match same_place(&self.library, f, &name, folder) {
            Some(i) => {
                // Kept for the toast's Undo, since the old text is otherwise gone.
                replaced = Some(self.library.scripts[i].clone());
                let s = &mut self.library.scripts[i];
                (s.body, s.sort, s.project, s.db, s.schema, s.updated) = (f.body.clone(), f.sort.clone(), f.project.clone(), f.db.clone(), f.schema.clone(), crate::library::now_secs());
                s.id
            }
            None => {
                let id = self.library.next_id();
                self.library.scripts.push(Script {
                    id,
                    name: name.clone(),
                    kind: f.kind.clone(),
                    body: f.body.clone(),
                    sort: f.sort.clone(),
                    project: f.project.clone(),
                    conn: f.conn,
                    db: f.db.clone(),
                    schema: f.schema.clone(),
                    coll: f.coll.clone(),
                    folder,
                    updated: crate::library::now_secs(),
                    ..Default::default()
                });
                id
            }
        };
        self.lib_ui.last_folder = folder;
        match f.view.filter(|v| self.t().is_some_and(|t| t.view == *v)) {
            Some(v) => self.bind_script(ScriptRef::Saved(id), v, f.body.clone()),
            None => {
                self.library.push_recent(ScriptRef::Saved(id));
                self.save_library();
            }
        }
        match replaced {
            Some(old) => self.toast_restore(format!("Replaced \"{name}\" in the library"), Restore::Script(old)),
            None => self.ok(format!("Saved \"{name}\" to the library")),
        }
        let tt = TabText { kind: if f.kind == "sql" { "sql" } else { "" }, body: f.body.clone(), sort: String::new(), project: String::new(), view: f.view.unwrap_or(View::Sql), db: String::new(), schema: String::new(), coll: String::new() };
        self.warn_plain_password(&tt);
    }

    /// Writes the script as a file in a linked folder: the text as it is, never with a header added, or the JSON shape for a
    /// filter or pipeline. A file of that name is replaced. The tab is bound to it, so the next Cmd+S writes it again.
    fn commit_save_linked(&mut self, f: &SaveForm, link: u64) {
        let Some(l) = self.library.links.iter().find(|l| l.id == link).cloned() else { return self.warn("That folder is no longer linked.") };
        if self.demo {
            return self.warn("The demo does not write files.");
        }
        if self.link_missing(link) {
            return self.warn(MISSING);
        }
        let ext = kind_ext(&f.kind);
        if !l.exts.iter().any(|e| e == ext) {
            return self.warn(format!("{} does not show .{ext} files. Add the type in Use on connections first.", l.name));
        }
        let file = self.file_for(f, f.name.trim());
        let rel = f.dir.join(&file);
        let Some(path) = self.library.link_join(link, &rel) else { return self.warn("That folder is outside the linked folder.") };
        let (root, sub) = (l.path.clone(), f.dir.clone());
        let target = format::Target { db: f.db.clone(), schema: f.schema.clone(), coll: f.coll.clone() };
        let (kind, body, sort, project) = (f.kind.clone(), f.body.clone(), f.sort.clone(), f.project.clone());
        let place = if l.name.is_empty() { l.path.display().to_string() } else { l.name.clone() };
        let password = f.kind == "sql" && !db::redact_sql(&f.body).1.is_empty();
        let uid = self.t().map_or(0, |t| t.uid);
        let job = Job::SaveNew { uid, link, rel: rel.clone(), view: f.view, body: f.body.clone(), place, password };
        // The old file is read, the new one written and listed on the worker; the tab is bound when it is back.
        self.run_job(job, move || {
            let was = std::fs::read(&path).ok();
            let old = was.as_deref().map(|b| String::from_utf8_lossy(b).into_owned());
            let bytes = match file_bytes(&kind, &body, &sort, &project, target, old.as_deref()) {
                Ok(b) => b,
                Err(e) => return FileOut::Saved(SaveOut::Refused(format!("Not saved: {e}"))),
            };
            if let Err(e) = disk::dir_below(&root, &sub).and_then(|_| disk::write_file(&path, &bytes)) {
                return FileOut::Saved(SaveOut::Failed(format!("Could not save {}: {e}", path.display())));
            }
            match disk::entry_of(&root, &rel) {
                Some(e) => FileOut::Made(e, was.map(|w| (w, bytes))),
                None => FileOut::Saved(SaveOut::Failed(format!("{} was written but cannot be read back", path.display()))),
            }
        });
    }

    /// The file a linked save writes: the name typed in the File row, else the script's name in the style of its folder.
    pub fn file_for(&self, f: &SaveForm, name: &str) -> String {
        let typed = f.file.trim();
        if typed.is_empty() { self.styled_file(f, name, kind_ext(&f.kind)) } else { disk::file_name(typed, kind_ext(&f.kind)) }
    }

    pub fn styled_file(&self, f: &SaveForm, name: &str, ext: &str) -> String {
        let siblings: Vec<String> = f.link.and_then(|l| self.lib_ui.scans.get(&l)).map(|s| s.entries.iter().filter(|e| e.rel.parent().unwrap_or(std::path::Path::new("")) == f.dir).filter_map(|e| e.rel.file_name().map(|n| n.to_string_lossy().into_owned())).collect()).unwrap_or_default();
        styled_name(name, ext, &siblings)
    }

    /// The Link a folder dialog's Save: a new link, or the one edited or located, which is read again.
    pub(super) fn save_link(&mut self, edit: Option<u64>, locate: bool, mut link: Link) {
        let id = edit.unwrap_or_else(|| self.library.next_id());
        link.id = id;
        let name = link.name.clone();
        match self.library.links.iter_mut().find(|l| l.id == id) {
            Some(l) => *l = link,
            None => self.library.links.push(link),
        }
        self.save_library();
        // A folder that moved is read from scratch: the old scan said it was missing.
        if locate {
            self.lib_ui.scans.remove(&id);
        }
        self.lib_ui.rescan.insert(id);
        self.ok(if locate { format!("{name} found.") } else if edit.is_some() { format!("{name} updated.") } else { format!("Linked {name}.") });
    }

    /// Make default, from the form or a snippet's tab: the script runs on `conn` and `db` from Cmd+K from now on.
    pub(super) fn set_snippet_default(&mut self, id: u64, conn: u64, db: &str, schema: &str) {
        crate::library::ops::set_target(&mut self.library, id, conn, db, schema);
        self.save_library();
        let at = if db.is_empty() { self.conn_name(conn) } else { format!("{} \u{b7} {db}", self.conn_name(conn)) };
        self.ok(format!("Runs on {at} by default"));
    }

    /// Make it a value's apply on tab `uid`.
    pub(super) fn make_value(&mut self, uid: u64, src: Src, range: Range<usize>, literal: &str, ph: &str) {
        let Some(t) = self.tabs.iter_mut().find(|t| t.uid == uid) else { return };
        t.snip.filled = false;
        let Some(text) = text_mut(t, src) else { return };
        // The text may have changed under the dialog; only the literal it was opened on is replaced.
        if text.get(range.clone()) == Some(literal) {
            text.replace_range(range, ph);
        } else {
            self.warn("The statement changed, so the literal was not replaced.");
        }
    }
}
