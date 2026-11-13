//! Linked folders in the app: scanning them off the UI thread, noticing that a file under an open tab changed on disk, the
//! conflict banner, and writing a bound file back. The files themselves are read and written through `crate::library::disk`.

use super::*;
use crate::library::{Library, ScriptRef, disk, format};
use std::time::{Duration, SystemTime};

/// How often a followed link, or one with an open tab, is scanned again while the window has focus. Polling replaces a
/// file watcher, which would add a dependency and miss changes on network drives.
const POLL_SECS: f64 = 2.0;

/// A script file read in full.
pub(in crate::app) struct Loaded {
    /// What the editor holds: the file as it is for .sql and .js, the filter or pipeline as shell text for .json.
    pub text: String,
    /// A .json file's parts, which the Documents view also needs for its sort and projection.
    pub parsed: Option<format::Parsed>,
    /// Modified time and length when it was read, which the tab keeps to notice a later change.
    pub meta: Option<(SystemTime, u64)>,
}

/// Reads a script file in full. Err names why it cannot be a script, such as a .json file of another shape.
pub(in crate::app) fn read_script(path: &std::path::Path) -> Result<Loaded, String> {
    let meta = std::fs::metadata(path).ok().and_then(|m| Some((m.modified().ok()?, m.len())));
    let raw = std::fs::read(path).map_err(|e| e.to_string())?;
    let raw = String::from_utf8_lossy(&raw).into_owned();
    if format::ext_of(path) == "json" {
        let p = format::parse_json(&raw).ok_or("It is not a filter or a pipeline")?;
        return Ok(Loaded { text: p.body.clone(), parsed: Some(p), meta });
    }
    Ok(Loaded { text: raw, parsed: None, meta })
}

/// The text of the view a tab's script is bound to.
fn bound_text(t: &Tab) -> Option<String> {
    let b = t.script.as_ref()?;
    Some(match b.view {
        View::Sql => t.sql.as_ref()?.query.clone(),
        View::Shell => t.shell.code.clone(),
        View::Aggregate => t.agg_txt.clone(),
        View::Documents => t.filter_txt.clone(),
        _ => return None,
    })
}

/// Puts `text` in the view a tab's script is bound to. A filter file also sets the sort and projection.
fn set_bound_text(t: &mut Tab, text: &str, parsed: Option<&format::Parsed>) {
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

fn file_label(rel: &std::path::Path) -> String {
    rel.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string()
}

impl Library {
    /// The folder of a link and the file `rel` below it.
    pub(in crate::app) fn file_path(&self, link: u64, rel: &std::path::Path) -> Option<PathBuf> {
        self.links.iter().find(|l| l.id == link).map(|l| l.path.join(rel))
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
        self.lib_ui.scanning.insert(id);
        self.lib_ui.scan_at.insert(id, now);
        self.worker.blocking(move || db::Msg::LibScan { link: id, scan: disk::scan(&path, &exts, prev.as_ref()) });
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
        self.lib_ui.scans.insert(link, scan);
        for (i, rel) in changed {
            if let Some(path) = self.library.file_path(link, &rel) {
                self.file_changed(i, &path);
            }
        }
    }

    /// The file under tab `i` changed on disk. The rule, in order:
    /// 1. the text is the one the tab was opened or saved with: only the remembered time changes;
    /// 2. the tab holds no edits and the link follows changes: the tab takes the new text;
    /// 3. otherwise the tab keeps its text and shows the conflict banner.
    fn file_changed(&mut self, i: usize, path: &std::path::Path) {
        // A file caught half written, or one that stopped being a script, waits for the next scan.
        let Ok(now) = read_script(path) else { return };
        let Some(t) = self.tabs.get_mut(i) else { return };
        let (Some(b), Some(mine)) = (t.script.as_ref(), bound_text(t)) else { return };
        let follow = match &b.r {
            ScriptRef::File { link, .. } => self.library.links.iter().find(|l| l.id == *link).is_some_and(|l| l.follow),
            _ => false,
        };
        if now.text == b.base {
            let b = t.script.as_mut().unwrap();
            (b.disk, b.conflict) = (now.meta, None);
        } else if mine == b.base && follow {
            set_bound_text(t, &now.text, now.parsed.as_ref());
            let b = t.script.as_mut().unwrap();
            (b.base, b.disk, b.conflict) = (now.text, now.meta, None);
        } else {
            let b = t.script.as_mut().unwrap();
            (b.disk, b.conflict) = (now.meta, Some(now.text));
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
        let (base, was) = (b.base.clone(), b.disk);
        let (view, text) = (b.view, bound_text(t).unwrap_or_default());
        let (db, coll, sort, project) = (t.sel.db.clone(), t.sel.coll.clone(), t.sort_txt.clone(), t.proj_txt.clone());
        let ext = format::ext_of(&path);
        // The file changed since the tab read it: with the same text as the tab's base it is no conflict, otherwise it is one.
        let meta = std::fs::metadata(&path).ok().and_then(|m| Some((m.modified().ok()?, m.len())));
        if was.is_some() && meta != was {
            match read_script(&path) {
                Ok(now) if now.text != base => {
                    if let Some(b) = self.tabs.get_mut(self.cur).and_then(|t| t.script.as_mut()) {
                        (b.disk, b.conflict) = (now.meta, Some(now.text));
                    }
                    self.warn(format!("{} changed on disk. Nothing was saved.", file_label(&rel)));
                    return true;
                }
                _ => {}
            }
        }
        let bytes = if ext == "json" {
            let old = std::fs::read_to_string(&path).ok();
            let mut p = old.as_deref().and_then(format::parse_json).unwrap_or_else(|| format::Parsed { target: format::Target { db, schema: String::new(), coll }, ..Default::default() });
            p.kind = if view == View::Aggregate { "pipeline" } else { "filter" }.into();
            p.body = text.clone();
            (p.sort, p.project) = (sort, project);
            match format::to_json(&p, old.as_deref()) {
                Ok(s) => s.into_bytes(),
                Err(e) => {
                    self.warn(format!("Not saved: {e}"));
                    return true;
                }
            }
        } else {
            text.clone().into_bytes()
        };
        if let Err(e) = disk::write_file(&path, &bytes) {
            self.err(format!("Could not save {}: {e}", path.display()));
            return true;
        }
        let meta = std::fs::metadata(&path).ok().and_then(|m| Some((m.modified().ok()?, m.len())));
        if let Some(b) = self.tabs.get_mut(self.cur).and_then(|t| t.script.as_mut()) {
            (b.base, b.disk, b.conflict) = (text, meta, None);
        }
        self.rescan_link(link);
        self.ok(format!("Saved {}", file_label(&rel)));
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
                    for (n, label, tip) in [(3, "Compare", "See what differs"), (2, "Keep mine", "Keep your edits; saving then replaces the file"), (1, "Reload", "Replace your edits with the file")] {
                        if ui.button(label).on_hover_text(tip).clicked() {
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

    /// Reload: the tab takes the text of the file.
    fn conflict_reload(&mut self) {
        let Some(t) = self.tabs.get_mut(self.cur) else { return };
        let Some(ScriptRef::File { link, rel }) = t.script.as_ref().map(|b| b.r.clone()) else { return };
        let Some(path) = self.library.file_path(link, &rel) else { return };
        match read_script(&path) {
            Ok(now) => {
                let t = &mut self.tabs[self.cur];
                set_bound_text(t, &now.text, now.parsed.as_ref());
                if let Some(b) = t.script.as_mut() {
                    (b.base, b.disk, b.conflict) = (now.text, now.meta, None);
                }
            }
            Err(e) => self.err(format!("Could not read {}: {e}", path.display())),
        }
    }

    /// Keep mine: the file's text becomes the base, so the tab still counts as edited and Cmd+S replaces the file.
    fn conflict_keep(&mut self) {
        if let Some(b) = self.tabs.get_mut(self.cur).and_then(|t| t.script.as_mut()) {
            if let Some(text) = b.conflict.take() {
                b.base = text;
            }
        }
    }

    /// Compare: the lines of the tab against those of the file, in a text window.
    fn conflict_compare(&mut self) {
        let Some(t) = self.tabs.get(self.cur) else { return };
        let (Some(b), Some(mine)) = (t.script.as_ref(), bound_text(t)) else { return };
        let (Some(theirs), ScriptRef::File { rel, .. }) = (b.conflict.clone(), &b.r) else { return };
        let body = match disk::diff_text(&mine, &theirs) {
            Some(d) => format!("- only in this tab\n+ only in the file\n\n{d}"),
            None => "Too many lines to compare here. Use Reload or Keep mine.".to_string(),
        };
        self.modal = Some(Modal::Text { title: format!("{} in this tab and on disk", file_label(rel)), body, code: true });
    }
}
