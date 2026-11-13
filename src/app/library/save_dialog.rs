//! The Save to library dialog: a name, a folder and a preview of what is saved. Opened by Cmd+S on a script that is not
//! bound to the library yet (see `save_script` in open.rs).
#![allow(dead_code)]

use super::linked::{FileOut, Job, SaveOut, file_bytes, kind_ext};
use super::open::{TabText, now_secs};
use super::*;
use crate::library::{Library, Script, ScriptRef, disk, format};

/// The fields of the Save to library dialog. The text and target are taken from the tab when it opens, so the dialog saves
/// what was on screen even though nothing else can change while it is up.
#[derive(Default)]
pub(in crate::app) struct SaveForm {
    pub name: String,
    /// The Quarry folder it goes to; None is the top level.
    pub folder: Option<u64>,
    /// Set while a new folder is being named in the dialog; it is made on Save and takes the script.
    pub new_folder: Option<String>,
    /// The linked folder chosen instead of a Quarry one, the folder inside it (empty for its top), and the file name, which
    /// stays empty until typed and then follows the script's name.
    pub link: Option<u64>,
    pub dir: PathBuf,
    pub file: String,
    pub kind: String,
    pub body: String,
    pub sort: String,
    pub project: String,
    pub conn: u64,
    pub db: String,
    pub schema: String,
    pub coll: String,
    /// The view the text came from, which the tab is bound in after the save.
    pub view: Option<View>,
    /// The context line under the title.
    pub head: String,
    /// The dialect that colors a SQL preview.
    pub dialect: db::sql::Dialect,
    pub focused: bool,
    /// The linked file the save would write and whether it was there when the path last changed.
    pub exists: Option<(PathBuf, bool)>,
}

/// Every Quarry folder in display order with its depth, as the panel shows them: a folder whose parent is gone sits at the
/// top level instead of vanishing.
pub(in crate::app) fn folder_rows(lib: &Library) -> Vec<(u64, usize)> {
    crate::library::ops::folder_list(lib, None).into_iter().map(|(id, depth, _)| (id, depth)).collect()
}

/// The folders of a scan below its top, parents before children, empty ones too.
fn link_dirs(scan: &disk::Scan) -> Vec<PathBuf> {
    let mut all = std::collections::BTreeSet::new();
    // An empty folder is walked up from itself, a file from the folder that holds it.
    let starts: Vec<&std::path::Path> = scan.entries.iter().filter_map(|e| e.rel.parent()).chain(scan.empty_dirs.iter().map(|d| d.as_path())).collect();
    for start in starts {
        let mut cur = Some(start);
        while let Some(d) = cur.filter(|d| !d.as_os_str().is_empty()) {
            all.insert(d.to_path_buf());
            cur = d.parent();
        }
    }
    all.into_iter().collect()
}

/// `name` as a file name in the style of the files beside it: kebab-case among kebab-case names, snake_case among snake_case
/// ones, as typed otherwise, so a team's folder keeps one style.
pub(in crate::app) fn styled_name(name: &str, ext: &str, siblings: &[String]) -> String {
    let stems: Vec<&str> = siblings.iter().map(|s| s.rsplit_once('.').map_or(s.as_str(), |(a, _)| a)).collect();
    let style = |sep: char, other: char| stems.iter().filter(|s| s.contains(sep) && !s.contains(' ') && !s.contains(other) && **s == s.to_lowercase()).count();
    let (kebab, snake) = (style('-', '_'), style('_', '-'));
    let words = || name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect::<Vec<_>>();
    let styled = if kebab * 2 > stems.len() {
        words().join("-")
    } else if snake * 2 > stems.len() {
        words().join("_")
    } else {
        name.to_string()
    };
    disk::file_name(if styled.is_empty() { name } else { &styled }, ext)
}

/// A name for a new script from the comment it starts with, such as "revenue per month" from "-- revenue per month".
pub(in crate::app) fn comment_name(body: &str) -> Option<String> {
    let first = body.lines().map(str::trim).find(|l| !l.is_empty())?;
    let text = ["--", "//", "/*", "#"].iter().find_map(|m| first.strip_prefix(m))?;
    let text = text.trim().trim_end_matches("*/").trim().trim_start_matches('*').trim();
    (!text.is_empty()).then(|| text.chars().take(60).collect::<String>().trim().to_string())
}

/// The script a save would replace: same name, folder, kind and target. A filter or pipeline belongs to a collection, a SQL
/// or Shell script to its connection only, since it runs on whichever database the tab has.
pub(in crate::app) fn same_place(lib: &Library, f: &SaveForm, name: &str, folder: Option<u64>) -> Option<usize> {
    lib.scripts.iter().position(|s| {
        s.name == name && s.folder == folder && s.kind == f.kind && s.conn == f.conn && (matches!(f.kind.as_str(), "sql" | "shell") || (s.db == f.db && s.coll == f.coll))
    })
}

impl App {
    /// Opens the dialog for the text `tt`, with the name and folder of the script the tab came from when it has one.
    pub(in crate::app) fn open_save_dialog(&mut self, tt: TabText) {
        let Some(t) = self.t() else { return };
        let conn = t.sel.conn;
        let bound = t.script.as_ref().and_then(|b| match &b.r {
            ScriptRef::Saved(id) => self.library.scripts.iter().find(|s| s.id == *id),
            _ => None,
        });
        let name = bound.map_or_else(|| comment_name(&tt.body).unwrap_or_else(|| self.tab_name(self.cur)), |s| s.name.clone());
        let last = self.lib_ui.last_folder.filter(|id| self.library.folders.iter().any(|f| f.id == *id));
        let folder = bound.map_or(last, |s| s.folder);
        let (cname, product, version) = self.conn(conn).map(|c| (c.meta.name.clone(), c.server.product.clone(), c.server.version.clone())).unwrap_or_default();
        let engine = if product.is_empty() { "mongod".to_string() } else { product };
        let head = [cname, tt.db.clone(), format!("{engine} {version}").trim().to_string()].into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join("  ·  ");
        let form = SaveForm {
            name,
            folder,
            kind: tt.kind.into(),
            body: tt.body,
            sort: tt.sort,
            project: tt.project,
            conn,
            db: tt.db,
            schema: tt.schema,
            coll: tt.coll,
            view: Some(tt.view),
            head,
            dialect: self.sql_dialect(),
            ..Default::default()
        };
        self.modal = Some(Modal::SaveScript(Box::new(form)));
    }

    /// Writes the script the dialog describes: a new one, or the one of the same name in the same folder, replaced. The tab
    /// is bound to it, so the next Cmd+S writes back without asking.
    fn commit_save(&mut self, f: &SaveForm) {
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
                (s.body, s.sort, s.project, s.db, s.schema, s.updated) = (f.body.clone(), f.sort.clone(), f.project.clone(), f.db.clone(), f.schema.clone(), now_secs());
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
                    updated: now_secs(),
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
            Some(old) => self.toast_restore(format!("Replaced \"{name}\" in the library"), crate::app::restore::Restore::Script(old)),
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
            return self.warn(super::panel::MISSING);
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
    fn file_for(&self, f: &SaveForm, name: &str) -> String {
        let typed = f.file.trim();
        if typed.is_empty() { self.styled_file(f, name, kind_ext(&f.kind)) } else { disk::file_name(typed, kind_ext(&f.kind)) }
    }

    fn styled_file(&self, f: &SaveForm, name: &str, ext: &str) -> String {
        let siblings: Vec<String> = f.link.and_then(|l| self.lib_ui.scans.get(&l)).map(|s| s.entries.iter().filter(|e| e.rel.parent().unwrap_or(std::path::Path::new("")) == f.dir).filter_map(|e| e.rel.file_name().map(|n| n.to_string_lossy().into_owned())).collect()).unwrap_or_default();
        styled_name(name, ext, &siblings)
    }

    pub(in crate::app) fn save_dialog(&mut self, ui: &mut egui::Ui, f: &mut SaveForm, keep: &mut bool) {
        ui.set_width(520.0);
        dialog_head(ui, &Head::new("Save to library").sub(f.head.clone()).glyph(paint_book));
        let name = f.name.trim().to_string();
        let new_folder_empty = f.new_folder.as_deref().is_some_and(|n| n.trim().is_empty());
        let mut enter = false;
        dialog_body(ui, |ui| {
            form_card(ui, "", "", |rows| {
                rows.label_width(110.0);
                rows.row("Name", "", |ui| {
                    let r = ui.add(egui::TextEdit::singleline(&mut f.name).desired_width(ui.available_width()));
                    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Script name"));
                    if !f.focused {
                        r.request_focus();
                        // The name is a suggestion: selected, so typing replaces it instead of adding to it.
                        // The field keeps no state before its first frame, so this waits for one.
                        let mut s = egui::TextEdit::load_state(ui.ctx(), r.id).unwrap_or_default();
                        s.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(f.name.chars().count()))));
                        s.store(ui.ctx(), r.id);
                        f.focused = true;
                    }
                    enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                });
                rows.row("Folder", "", |ui| self.folder_combo(ui, f));
                if f.new_folder.is_some() {
                    rows.row("New folder", "", |ui| {
                        let n = f.new_folder.get_or_insert_with(String::new);
                        let r = ui.add(egui::TextEdit::singleline(n).desired_width(ui.available_width()).hint_text("Name of the folder"));
                        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "New folder name"));
                    });
                }
                if let Some(l) = f.link.and_then(|id| self.library.links.iter().find(|l| l.id == id)) {
                    let ext = kind_ext(&f.kind);
                    let default = self.styled_file(f, f.name.trim(), ext);
                    rows.row("File", "", |ui| {
                        let r = ui.add(egui::TextEdit::singleline(&mut f.file).font(egui::TextStyle::Monospace).hint_text(default).desired_width(ui.available_width()));
                        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "File name"));
                    });
                    if !l.exts.iter().any(|e| e == ext) {
                        rows.full(|ui| field_note(ui, &format!("{} does not show .{ext} files.", l.name), ALERT()));
                    }
                }
            });
            let (caption, note) = match f.kind.as_str() {
                "sql" => ("Statement", format!("the whole script  ·  {} lines", f.body.lines().count().max(1))),
                "shell" => ("Script", format!("{} lines", f.body.lines().count().max(1))),
                "pipeline" => ("Pipeline", format!("{} lines", f.body.lines().count().max(1))),
                _ => ("Query", "sort and project are saved with it".to_string()),
            };
            card_caption(ui, caption, &note);
            well_frame().inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical().id_salt("save-preview").max_height(180.0).auto_shrink([false, true]).show(ui, |ui| {
                    let job = match f.kind.as_str() {
                        "sql" => sql_job_in(&f.body, TXT_SMALL, f.dialect),
                        "shell" => js_job(&f.body),
                        _ => json_job(&f.body),
                    };
                    ui.add(egui::Label::new(job).selectable(false));
                });
            });
            let folder = if f.new_folder.is_some() { None } else { f.folder };
            if let Some(l) = f.link.and_then(|id| self.library.links.iter().find(|l| l.id == id)) {
                let file = self.file_for(f, &name);
                // Looked up once per path, not every frame: on a network drive each look is a round trip.
                let path = l.path.join(&f.dir).join(&file);
                if f.exists.as_ref().is_none_or(|(p, _)| *p != path) {
                    let there = path.exists();
                    f.exists = Some((path, there));
                }
                if !name.is_empty() && f.exists.as_ref().is_some_and(|(_, there)| *there) {
                    field_note(ui, &format!("Replaces {file} in this folder."), WARNING());
                }
            } else if f.new_folder.is_none() && !name.is_empty() && same_place(&self.library, f, &name, folder).is_some() {
                field_note(ui, &format!("Replaces the saved \"{name}\" in this folder."), WARNING());
            }
        });
        let linked_ok = f.link.is_none_or(|id| self.library.links.iter().find(|l| l.id == id).is_some_and(|l| l.exts.iter().any(|e| e == kind_ext(&f.kind))));
        let ok = !name.is_empty() && !new_folder_empty && linked_ok;
        let mut save = false;
        dialog_foot(ui, |ui| {
            // Saving over a script of the same name says so on the button, and the toast after it offers Undo.
            let replaces = f.link.is_none() && f.new_folder.is_none() && !name.is_empty() && same_place(&self.library, f, &name, f.folder).is_some();
            save = ui.add_enabled(ok, primary_button(ui, if replaces { "Replace" } else { "Save" }, PRIMARY())).clicked() || (enter && ok);
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
            foot_note(ui, &format!("Then {} saves straight back to it", super::keys_text(true, false, false, "S")));
        });
        if save {
            self.commit_save(f);
            *keep = false;
        }
    }

    /// The Folder dropdown: linked folders first (the linked folders' code fills that section), then the folders Quarry
    /// keeps with their subfolders indented and a check on the current one, No folder, and New folder.
    fn folder_combo(&mut self, ui: &mut egui::Ui, f: &mut SaveForm) {
        let rows = folder_rows(&self.library);
        let name_of = |id: u64| self.library.folders.iter().find(|x| x.id == id).map(|x| x.name.clone()).unwrap_or_default();
        let link_name = |id: u64| self.library.links.iter().find(|l| l.id == id).map(|l| if l.name.is_empty() { l.path.display().to_string() } else { l.name.clone() }).unwrap_or_default();
        let shown = match (&f.new_folder, f.folder, f.link) {
            (_, _, Some(id)) if f.dir.as_os_str().is_empty() => link_name(id),
            (_, _, Some(id)) => format!("{} / {}", link_name(id), f.dir.display()),
            (Some(_), _, _) => "New folder".to_string(),
            (None, Some(id), _) => name_of(id),
            (None, None, _) => "No folder".to_string(),
        };
        // The linked folders the script can be written to: used on its connection and showing files of its kind.
        let engine = self.conn(f.conn).map(|c| c.meta.engine);
        let linked: Vec<(u64, String, Vec<PathBuf>)> = self
            .library
            .links
            .iter()
            .filter(|l| engine.is_some_and(|e| format::fits(l, &f.kind, f.conn, e)) && !self.link_missing(l.id))
            .map(|l| (l.id, link_name(l.id), self.lib_ui.scans.get(&l.id).map(link_dirs).unwrap_or_default()))
            .collect();
        egui::ComboBox::from_id_salt("save-folder").width(ui.available_width()).selected_text(shown).show_ui(ui, |ui| {
            menu_look(ui);
            ui.set_min_width(260.0);
            if !linked.is_empty() {
                section_head(ui, "LINKED FOLDERS", None);
            }
            let pick = |ui: &mut egui::Ui, label: &str, depth: usize, on: bool| -> bool {
                let (row, _) = pop_row(ui, 28.0);
                let (p, rect) = (ui.painter(), row.rect);
                let x = rect.left() + 6.0 + depth as f32 * 16.0;
                galley_at(p, x, rect.center().y, p.layout_no_wrap(label.to_string(), FontId::proportional(TXT_BODY), TEXT()), TEXT());
                if on {
                    glyphs::check(p, egui::pos2(rect.right() - 12.0, rect.center().y), PRIMARY());
                }
                let label = label.to_string();
                row.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Folder {label}")));
                row.clicked()
            };
            for (id, name, dirs) in &linked {
                if pick(ui, name, 0, f.link == Some(*id) && f.dir.as_os_str().is_empty()) {
                    (f.link, f.dir, f.folder, f.new_folder) = (Some(*id), PathBuf::new(), None, None);
                    ui.close();
                }
                for d in dirs {
                    let label = d.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    if pick(ui, &label, d.components().count(), f.link == Some(*id) && f.dir == *d) {
                        (f.link, f.dir, f.folder, f.new_folder) = (Some(*id), d.clone(), None, None);
                        ui.close();
                    }
                }
            }
            if !linked.is_empty() {
                section_head(ui, "IN QUARRY", None);
            }
            for (id, depth) in &rows {
                if pick(ui, &name_of(*id), *depth, f.link.is_none() && f.new_folder.is_none() && f.folder == Some(*id)) {
                    (f.folder, f.new_folder, f.link) = (Some(*id), None, None);
                    ui.close();
                }
            }
            if pick(ui, "No folder", 0, f.link.is_none() && f.new_folder.is_none() && f.folder.is_none()) {
                (f.folder, f.new_folder, f.link) = (None, None, None);
                ui.close();
            }
            if pick(ui, "New folder…", 0, false) {
                (f.folder, f.new_folder, f.link) = (None, Some(String::new()), None);
                ui.close();
            }
        });
    }
}
