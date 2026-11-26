//! What the library's views change in the tabs and the panel: a script filling the current tab, its binding, Cmd+S, the
//! undo of a library change, and the Library panel's own state. src/app/library/ draws it and decides where a script
//! opens; these are the writes it makes once it knows.

use super::{Dialog, UiFx};
use crate::core::Core;
use crate::db;
use crate::library::disk;
use crate::library::tree::Node;
use crate::library::{Script, ScriptRef};
use crate::state::library::linked::{bound_text, set_bound_text};
use crate::state::library::open::{text_of, view_of_kind, How, Pending, TabText};
use crate::state::library::panel::PanelUi;
use crate::state::restore::Restore;
use crate::state::{Snip, View};
use std::path::PathBuf;

impl Core {
    /// The text of the current tab's view as a script, or None for a view that holds no script.
    pub fn tab_text(&self) -> Option<TabText> {
        text_of(self.t()?)
    }

    /// A script waits for its connection, to open when it is ready.
    pub(super) fn lib_wait(&mut self, p: Pending) {
        self.lib_ui.pending = Some(p);
    }

    /// Puts script `s` in the current tab, whose view and connection are already the script's, and binds the tab to `r`.
    pub(super) fn fill_script(&mut self, r: ScriptRef, s: Script) {
        let Some(view) = view_of_kind(&s.kind) else { return };
        if let Some(t) = self.tm() {
            match s.kind.as_str() {
                "sql" => {
                    // The values of what the tab held before are not this script's.
                    t.snip = Default::default();
                    if let Some(q) = t.sql.as_mut() {
                        q.query = s.body.clone();
                        q.caret = 0;
                        q.run_schema = s.schema.clone();
                        // A reused tab may have been switched to another database; the script runs in its own.
                        q.run_db = String::new();
                    }
                }
                "shell" => t.snip = Default::default(),
                "filter" => {
                    t.clear_options();
                    (t.filter_txt, t.sort_txt, t.proj_txt) = (s.body.clone(), s.sort.clone(), s.project.clone());
                    t.view = View::Documents;
                    t.snip = Default::default();
                }
                "where" => {
                    (t.filter_txt, t.sort_txt, t.page) = (s.body.clone(), s.sort.clone(), 0);
                    t.view = View::Documents;
                }
                "pipeline" => {
                    t.agg_txt = s.body.clone();
                    t.view = View::Aggregate;
                }
                _ => {}
            }
        }
        self.bind_script(r, view, s.body);
    }

    /// A snippet filled by the Cmd+K form runs in the current tab, which is not bound to the script.
    pub(super) fn fill_snippet(&mut self, kind: &str, body: String, sort: String, project: String, schema: String) {
        let Some(t) = self.tm() else { return };
        t.script = None;
        t.snip = Snip { off: true, ..Default::default() };
        match kind {
            "sql" => {
                if let Some(q) = t.sql.as_mut() {
                    q.query = body;
                    q.caret = 0;
                    q.run_schema = schema;
                    q.run_db = String::new();
                }
            }
            "filter" => {
                t.clear_options();
                (t.filter_txt, t.sort_txt, t.proj_txt, t.view) = (body, sort, project, View::Documents);
            }
            _ => {}
        }
    }

    /// A linked file just opened: the tab keeps what the disk held then, so a later change is told from this text.
    pub(super) fn script_seen(&mut self, link: u64, rel: &std::path::Path, meta: Option<(std::time::SystemTime, u64)>) {
        if let Some(b) = self.tm().and_then(|t| t.script.as_mut()) {
            b.disk = meta;
        }
        self.mark_seen(link, rel, meta);
    }

    /// Opens a linked file on `conn`: it is read on the worker, and the tab opens when it is back.
    pub(super) fn open_file(&mut self, r: ScriptRef, how: How, conn: u64) {
        if let ScriptRef::File { link, rel } = &r {
            let (link, rel) = (*link, rel.clone());
            self.read_file_job(link, &rel, crate::state::library::linked::Job::Open { r, how, conn });
        }
    }

    /// The chip's Unbind: the tab keeps its text but no longer writes back to the script, so Cmd+S asks again.
    pub(super) fn unbind_tab(&mut self, tab: u64) {
        if let Some(t) = self.tabs.iter_mut().find(|t| t.uid == tab) {
            t.script = None;
        }
    }

    /// Run opened the script of tab `tab` instead of running it because it can write (`on`), or its banner was dealt with.
    pub(super) fn tab_writes(&mut self, tab: u64, on: bool) {
        if on {
            self.lib_ui.writes.insert(tab);
        } else {
            self.lib_ui.writes.remove(&tab);
        }
    }

    /// Puts saved script `id` back in tab `tab`, which is bound to it, with a toast whose Undo restores the replaced text.
    pub(super) fn reload_bound(&mut self, tab: u64, id: u64) {
        let Some(s) = self.library.scripts.iter().find(|s| s.id == id).cloned() else { return };
        let Some(t) = self.tabs.iter_mut().find(|t| t.uid == tab) else { return };
        let mine = bound_text(t).map(String::from);
        set_bound_text(t, &s.body, None);
        if t.script.as_ref().is_some_and(|b| b.view == View::Documents) {
            (t.sort_txt, t.proj_txt) = (s.sort.clone(), s.project.clone());
        }
        if let Some(b) = t.script.as_mut() {
            b.base = s.body.clone();
        }
        match mine {
            Some(text) => self.toast_restore(format!("\"{}\" reloaded from the library.", s.name), Restore::TabText { uid: tab, text }),
            None => self.ok(format!("\"{}\" reloaded from the library.", s.name)),
        }
    }

    /// The chip's Show in library: the script selected and the folders above it open.
    pub(super) fn show_in_library(&mut self, r: &ScriptRef) {
        let tab_conn = self.t().map(|t| t.sel.conn).or(self.active);
        let st = &mut self.lib_ui.panel;
        st.query.clear();
        let node = match r {
            ScriptRef::Saved(id) => {
                let lib = &self.library;
                let mut at = lib.scripts.iter().find(|s| s.id == *id).and_then(|s| crate::library::tree::live_parent(lib, s.folder));
                // Bounded, since a hand-edited file could hold a loop of folders.
                for _ in 0..32 {
                    let Some(f) = at else { break };
                    st.open.insert(Node::Folder(f));
                    at = lib.folders.iter().find(|x| x.id == f).and_then(|x| crate::library::tree::live_parent(lib, x.parent));
                }
                // A script of another connection shows only in the All connections scope.
                let conn = lib.scripts.iter().find(|s| s.id == *id).map(|s| s.conn);
                if conn.is_some() && conn != tab_conn {
                    self.prefs.lib_scope_all = true;
                }
                Node::Script(*id)
            }
            ScriptRef::File { link, rel } => {
                st.open.insert(Node::Link(*link));
                let mut at = PathBuf::new();
                for c in rel.parent().into_iter().flat_map(|p| p.components()) {
                    at.push(c);
                    st.open.insert(Node::Dir { link: *link, rel: at.clone() });
                }
                Node::File { link: *link, rel: rel.clone() }
            }
        };
        let st = &mut self.lib_ui.panel;
        st.sel = Some(node);
        st.focus = true;
    }

    /// Cmd+S and the Save button: writes the view's text back to the script the tab is bound to, or opens the Save to
    /// library dialog when there is none. Page handlers consume Cmd+S first (staged grid edits, document drafts), so this
    /// only runs when the person means the script.
    pub(super) fn save_script(&mut self) {
        let Some(tt) = self.tab_text() else { return self.warn("There is no script to save in this view. Open a Query, Documents, Content, Aggregate or Shell view.") };
        if tt.body.trim().is_empty() && tt.sort.trim().is_empty() && tt.project.trim().is_empty() {
            return self.warn("Nothing to save: the script is empty.");
        }
        // A tab bound to a linked file writes that file, or raises the conflict when it changed on disk.
        if self.save_linked() {
            return;
        }
        let bound = self.t().and_then(|t| t.script.as_ref()).filter(|b| b.view == tt.view).map(|b| b.r.clone());
        if let Some(ScriptRef::Saved(id)) = bound
            && let Some(i) = self.library.scripts.iter().position(|s| s.id == id)
        {
            let tab_conn = self.t().map(|t| t.sel.conn);
            let sc = &mut self.library.scripts[i];
            (sc.body, sc.sort, sc.project, sc.updated) = (tt.body.clone(), tt.sort.clone(), tt.project.clone(), crate::library::now_secs());
            // The database and collection the tab runs on now are part of what is saved, as the Save dialog keeps them. A script
            // opened on another connection keeps its own, which name places on its own server.
            let own = tab_conn == Some(sc.conn);
            if own && !tt.db.is_empty() {
                (sc.db, sc.schema) = (tt.db.clone(), tt.schema.clone());
            }
            if own && !tt.coll.is_empty() {
                sc.coll = tt.coll.clone();
            }
            let name = sc.name.clone();
            self.bind_script(ScriptRef::Saved(id), tt.view, tt.body.clone());
            self.ok(format!("Saved \"{name}\""));
            self.warn_plain_password(&tt);
            return;
        }
        self.ui_fx.push(UiFx::Open(Dialog::SaveScript(Box::new(tt))));
    }

    /// Undo of a change that is not a database edit: a deleted group, library script or file, an unlink, a moved script.
    pub(super) fn apply_restore(&mut self, id: u64) {
        let Some(i) = self.restores.iter().position(|(r, _)| *r == id) else { return };
        match self.restores.remove(i).1 {
            Restore::Group { g, at } => self.restore_group(g, at),
            Restore::Placed { moved, created, tile, prev, at } => self.restore_placed(moved, created, tile, prev, at),
            Restore::Saved { scripts, folders, written } => {
                let n = scripts.len();
                self.library.restore(folders, scripts);
                self.save_library();
                // A script moved into a linked folder left a file there. It goes too, unless it was changed since, which
                // would make the undo throw away the person's edits.
                if let Some((link, rel, bytes)) = written
                    && let Some(path) = self.library.link_join(link, &rel)
                {
                    if std::fs::read(&path).is_ok_and(|now| now == bytes) {
                        if let Err(e) = std::fs::remove_file(&path) {
                            self.warn(format!("The script is back, but {} could not be removed: {e}", path.display()));
                            return;
                        }
                        self.drop_entry(link, &rel);
                    } else if path.exists() {
                        self.warn(format!("The script is back. {} was changed since, so it was kept.", path.display()));
                        return;
                    }
                }
                self.ok(if n == 1 { "Script restored.".to_string() } else { format!("{n} scripts restored.") });
            }
            Restore::TabText { uid, text } => {
                let Some(t) = self.tabs.iter_mut().find(|t| t.uid == uid) else { return self.warn("That tab was closed.") };
                set_bound_text(t, &text, None);
                self.ok("Your edits are back.");
            }
            Restore::Script(old) => {
                let name = old.name.clone();
                match self.library.scripts.iter_mut().find(|s| s.id == old.id) {
                    Some(s) => *s = old,
                    None => self.library.restore(Vec::new(), vec![old]),
                }
                self.save_library();
                self.ok(format!("\"{name}\" is back as it was."));
            }
            Restore::File { path, bytes } => {
                // A name the person has taken again since must not be overwritten by an undo.
                if std::fs::symlink_metadata(&path).is_ok() {
                    self.warn(format!("{} exists again, so the old file was not put back.", path.display()));
                    return;
                }
                let (Some(dir), Some(file)) = (path.parent(), path.file_name().and_then(|f| f.to_str())) else {
                    // An Undo that does nothing must say so, or the person believes the file is back.
                    return self.err(format!("Could not restore \"{}\": it names no file.", path.display()));
                };
                match crate::state::config::write_atomic(dir, file, &bytes) {
                    Ok(()) => {
                        // The folder it came from shows the file again at the next scan.
                        let links: Vec<u64> = self.library.links.iter().filter(|l| path.starts_with(&l.path)).map(|l| l.id).collect();
                        self.lib_ui.rescan.extend(links);
                        self.ok(format!("{file} restored."));
                    }
                    Err(e) => self.err(format!("Could not restore {}: {e}", path.display())),
                }
            }
            Restore::Link { link, at, tabs, recent } => self.relink(link, at, tabs, recent),
            Restore::Replaced { link, path, old, new } => {
                let name = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                if !std::fs::read(&path).is_ok_and(|now| now == new) {
                    self.warn(format!("{name} was changed since, so it was kept."));
                    return;
                }
                match disk::write_file(&path, &old) {
                    Ok(()) => {
                        self.rescan_link(link);
                        self.ok(format!("{name} is back as it was."));
                    }
                    Err(e) => self.err(format!("Could not restore {}: {e}", path.display())),
                }
            }
        }
    }

    /// A folder opened or closed in the panel, which keeps it drawn while it moves.
    pub(super) fn panel_toggle(&mut self, n: Node) {
        let st = &mut self.lib_ui.panel;
        if !st.open.remove(&n) {
            st.open.insert(n.clone());
        }
        st.folding.insert(n);
    }

    /// Every folder of the panel closed.
    pub(super) fn panel_fold_all(&mut self) {
        let st = &mut self.lib_ui.panel;
        for n in st.open.drain() {
            st.folding.insert(n);
        }
    }

    /// The row starts being renamed, with its current name in the field.
    pub(super) fn panel_rename(&mut self, n: Node) {
        let name = match &n {
            Node::Folder(id) => self.library.folders.iter().find(|f| f.id == *id).map(|f| f.name.clone()),
            Node::Script(id) => self.library.scripts.iter().find(|s| s.id == *id).map(|s| s.name.clone()),
            Node::Link(id) => self.library.links.iter().find(|l| l.id == *id).map(|l| if l.name.is_empty() { l.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() } else { l.name.clone() }),
            Node::Dir { rel, .. } => rel.file_name().map(|n| n.to_string_lossy().into_owned()),
            Node::File { rel, .. } => rel.file_stem().map(|n| n.to_string_lossy().into_owned()),
        };
        if let Some(name) = name {
            let st = &mut self.lib_ui.panel;
            st.sel = Some(n.clone());
            st.rename = Some((n, name, true));
        }
    }

    /// What the panel holds after a frame of drawing: search text, scroll, selection, the row being renamed.
    pub(super) fn panel_set(&mut self, p: PanelUi) {
        self.lib_ui.panel = p;
    }

    /// The Link a folder dialog's preview: the folder is scanned off the UI thread, and `Msg::LibScan` with link 0 answers.
    pub(super) fn link_preview(&mut self, root: PathBuf) {
        let all = ["sql", "js", "json"].map(String::from).to_vec();
        self.worker.blocking(move || db::Msg::LibScan { link: 0, scan: disk::scan(&root, &all, None) });
    }

    /// The values of a snippet's strip, as typed on tab `tab`.
    pub(super) fn tab_snip(&mut self, tab: u64, vals: std::collections::HashMap<String, String>, filled: bool) {
        if let Some(t) = self.tabs.iter_mut().find(|t| t.uid == tab) {
            (t.snip.vals, t.snip.filled) = (vals, filled);
        }
    }

    /// The base table a SQL script's Copy as SQL INSERT writes to, kept with the script.
    pub(super) fn base_table(&mut self, id: u64, table: &str) {
        if let Some(s) = self.library.scripts.iter_mut().find(|s| s.id == id && s.kind == "sql" && s.coll != table) {
            s.coll = table.to_string();
            self.save_library();
        }
    }
}
