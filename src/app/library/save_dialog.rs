//! The Save to library dialog: a name, a folder and a preview of what is saved. Opened by Cmd+S on a script that is not
//! bound to the library yet (see `save_script` in open.rs).
#![allow(dead_code)]

use super::open::{TabText, now_secs};
use super::*;
use crate::library::{Library, Script, ScriptRef};

/// The fields of the Save to library dialog. The text and target are taken from the tab when it opens, so the dialog saves
/// what was on screen even though nothing else can change while it is up.
#[derive(Default)]
pub(in crate::app) struct SaveForm {
    pub name: String,
    /// The Quarry folder it goes to; None is the top level.
    pub folder: Option<u64>,
    /// Set while a new folder is being named in the dialog; it is made on Save and takes the script.
    pub new_folder: Option<String>,
    /// B2: the linked folder chosen instead of a Quarry one, and the file name inside it. Both stay empty here.
    pub link: Option<u64>,
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
}

/// Every Quarry folder in display order with its depth: top level folders in list order, each followed by its own.
pub(in crate::app) fn folder_rows(lib: &Library) -> Vec<(u64, usize)> {
    fn walk(lib: &Library, parent: Option<u64>, depth: usize, out: &mut Vec<(u64, usize)>) {
        // A folder file edited by hand could loop through its parents; the depth stops it.
        if depth > 32 {
            return;
        }
        for f in lib.folders.iter().filter(|f| f.parent == parent) {
            out.push((f.id, depth));
            walk(lib, Some(f.id), depth + 1, out);
        }
    }
    let mut out = Vec::new();
    walk(lib, None, 0, &mut out);
    out
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
        let name = bound.map_or_else(|| "Untitled".to_string(), |s| s.name.clone());
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
        let name = f.name.trim().to_string();
        let mut folder = f.folder;
        if let Some(n) = f.new_folder.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            folder = Some(match self.library.folders.iter().find(|x| x.parent.is_none() && x.name == n) {
                Some(x) => x.id,
                None => {
                    let id = self.library.next_id();
                    self.library.folders.push(crate::library::Folder { id, name: n.to_string(), parent: None });
                    id
                }
            });
        }
        let id = match same_place(&self.library, f, &name, folder) {
            Some(i) => {
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
        self.ok(format!("Saved \"{name}\" to the library"));
        let tt = TabText { kind: if f.kind == "sql" { "sql" } else { "" }, body: f.body.clone(), sort: String::new(), project: String::new(), view: f.view.unwrap_or(View::Sql), db: String::new(), schema: String::new(), coll: String::new() };
        self.warn_plain_password(&tt);
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
                // B2 adds the File row here when the folder chosen is a linked one.
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
            if f.new_folder.is_none() && !name.is_empty() && same_place(&self.library, f, &name, folder).is_some() {
                field_note(ui, &format!("Replaces the saved \"{name}\" in this folder."), WARNING());
            }
        });
        let ok = !name.is_empty() && !new_folder_empty;
        let mut save = false;
        dialog_foot(ui, |ui| {
            save = ui.add_enabled(ok, primary_button(ui, "Save", PRIMARY())).clicked() || (enter && ok);
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
            foot_note(ui, "Then Cmd+S saves straight back to it");
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
        let shown = match (&f.new_folder, f.folder) {
            (Some(_), _) => "New folder".to_string(),
            (None, Some(id)) => name_of(id),
            (None, None) => "No folder".to_string(),
        };
        egui::ComboBox::from_id_salt("save-folder").width(ui.available_width()).selected_text(shown).show_ui(ui, |ui| {
            menu_look(ui);
            ui.set_min_width(260.0);
            // LINKED FOLDERS: filled by the linked folders' stream, which lists them here.
            section_head(ui, "IN QUARRY", None);
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
            for (id, depth) in &rows {
                if pick(ui, &name_of(*id), *depth, f.new_folder.is_none() && f.folder == Some(*id)) {
                    (f.folder, f.new_folder) = (Some(*id), None);
                    ui.close();
                }
            }
            if pick(ui, "No folder", 0, f.new_folder.is_none() && f.folder.is_none()) {
                (f.folder, f.new_folder) = (None, None);
                ui.close();
            }
            if pick(ui, "New folder…", 0, false) {
                (f.folder, f.new_folder) = (None, Some(String::new()));
                ui.close();
            }
        });
    }
}
