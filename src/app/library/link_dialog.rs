//! The Link a folder dialog: pick a folder on disk, say which connections use it and which files it shows. The same form
//! edits a link later, for "Use on connections".

use super::*;
use crate::app::sql::create::plural;
use crate::library::{Link, disk, format};
use db::sql::Engine;

/// The fields of the Link a folder dialog.
#[derive(Default)]
pub(in crate::app) struct LinkForm {
    /// The link being edited, None when a new one is made. An edit keeps the folder.
    edit: Option<u64>,
    path: String,
    name: String,
    /// The name was typed, so a new folder path no longer renames it.
    name_set: bool,
    /// "conns", "engine" or "all", as in `Link::on`.
    on: String,
    conns: Vec<u64>,
    /// The engine "Every ... connection" means: that of the first chosen connection, or of the current tab.
    engine: Engine,
    sql: bool,
    js: bool,
    json: bool,
    follow: bool,
    /// The scan that summarizes the folder, with the path it was made for.
    preview: Option<(String, disk::Scan)>,
    /// The path a scan is out for, or None while none is.
    pub(super) asking: Option<String>,
    /// Egui time of the last edit to the path. The scan waits until typing pauses, since each partial path is a folder.
    typed_at: f64,
}

/// How long the path must rest before it is scanned.
const TYPING_PAUSE: f64 = 0.5;

fn engine_name(e: Engine) -> &'static str {
    match e {
        Engine::Mongo => "MongoDB",
        Engine::MySql => "MySQL or MariaDB",
        Engine::Postgres => "PostgreSQL",
    }
}

fn engine_from(key: &str) -> Engine {
    match key {
        "mysql" => Engine::MySql,
        "postgres" => Engine::Postgres,
        _ => Engine::Mongo,
    }
}

/// The path as typed, with a leading `~` read as the home folder.
fn expand(path: &str) -> PathBuf {
    let p = path.trim();
    match (p.strip_prefix("~/").or(if p == "~" { Some("") } else { None }), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(p),
    }
}

fn folder_name(path: &std::path::Path) -> String {
    path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string()
}

impl LinkForm {
    fn exts(&self) -> Vec<String> {
        [("sql", self.sql), ("js", self.js), ("json", self.json)].iter().filter(|(_, on)| *on).map(|(e, _)| e.to_string()).collect()
    }

    /// The demo's dialog: a folder typed and already looked at, so nothing is scanned.
    pub(super) fn fill_demo(&mut self, path: &str, scan: disk::Scan) {
        self.path = path.into();
        self.name = "shop-queries".into();
        self.name_set = true;
        self.preview = Some((path.into(), scan));
    }

    /// The scan that came back, once the dialog has asked for it. A scan for a path that was changed since is dropped.
    pub(super) fn got_preview(&mut self, scan: disk::Scan) {
        if let Some(p) = self.asking.take() {
            self.preview = Some((p, scan));
        }
    }
}

/// The summary under the folder field: how many scripts, in how many folders, and the git branch.
fn summary(scan: &disk::Scan) -> (Color32, String) {
    if scan.missing {
        return (ALERT(), "That folder does not exist".into());
    }
    if scan.entries.is_empty() {
        return (WARNING(), "No scripts of the chosen file types in it yet".into());
    }
    let folders: std::collections::HashSet<_> = scan.entries.iter().map(|e| e.rel.parent().map(|p| p.to_path_buf()).unwrap_or_default()).collect();
    let mut s = format!("{} in {}", plural(scan.entries.len() as u64, "script"), plural(folders.len() as u64, "folder"));
    match scan.branch.as_deref() {
        Some(b) if b.starts_with("detached") => s.push_str(&format!(" \u{b7} git, {b}")),
        Some(b) => s.push_str(&format!(" \u{b7} git, branch {b}")),
        None => {}
    }
    if scan.truncated {
        s.push_str(" \u{b7} stopped at the limit, the rest is not shown");
    }
    (SUCCESS(), s)
}

/// A chosen connection: a status dot, its name and a painted x. True when the x was clicked.
fn conn_chip(ui: &mut egui::Ui, id: egui::Id, name: &str, dot: Color32) -> bool {
    let g = ui.painter().layout_job(egui::text::LayoutJob::single_section(name.to_string(), ui_fmt(TXT_SMALL, BRIGHT(), 400.0, 0.0)));
    let (pad, x_d) = (8.0, 16.0);
    let w = pad + 8.0 + 6.0 + g.size().x + 6.0 + x_d + 4.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, CHIP_H + 4.0), Sense::hover());
    let x_rect = egui::Rect::from_center_size(egui::pos2(rect.right() - 4.0 - x_d / 2.0, rect.center().y), egui::vec2(x_d, x_d));
    let x = ui.interact(x_rect, id.with("x"), Sense::click());
    let state = state_of(&x);
    let p = ui.painter();
    p.rect(rect, rect.height() / 2.0, PANEL(), Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
    p.circle_filled(egui::pos2(rect.left() + pad + 4.0, rect.center().y), 4.0, dot);
    p.galley(egui::pos2(rect.left() + pad + 14.0, rect.center().y - g.size().y / 2.0 + optical_dy(&g)), g, BRIGHT());
    p.circle_filled(x_rect.center(), x_d / 2.0, hover_fill(state));
    let (c, s) = (x_rect.center(), Stroke::new(1.3, if state > 0 { BRIGHT() } else { MUTED() }));
    p.line_segment([c + egui::vec2(-3.0, -3.0), c + egui::vec2(3.0, 3.0)], s);
    p.line_segment([c + egui::vec2(-3.0, 3.0), c + egui::vec2(3.0, -3.0)], s);
    focus_ring(ui, &x, x_rect, x_d / 2.0);
    x.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Remove {name}")));
    x.on_hover_text("Remove").on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

impl App {
    /// The engine a new link is for: that of the current tab's connection, else of the active one.
    fn link_engine(&self) -> Engine {
        let id = self.t().map(|t| t.sel.conn).or(self.active);
        id.and_then(|id| self.conn(id)).map(|c| c.meta.engine).unwrap_or_default()
    }

    /// Opens the Link a folder dialog, editing the link `edit` when given.
    pub(in crate::app) fn open_link_dialog(&mut self, edit: Option<u64>) {
        let mut f = LinkForm { sql: true, js: true, json: true, follow: true, on: "conns".into(), engine: self.link_engine(), ..Default::default() };
        match edit.and_then(|id| self.library.links.iter().find(|l| l.id == id)) {
            Some(l) => {
                f.edit = Some(l.id);
                f.path = l.path.to_string_lossy().into_owned();
                f.name = l.name.clone();
                f.name_set = true;
                f.on = l.on.clone();
                f.conns = l.conns.clone();
                f.engine = if l.engine.is_empty() { f.engine } else { engine_from(&l.engine) };
                (f.sql, f.js, f.json) = (l.exts.iter().any(|e| e == "sql"), l.exts.iter().any(|e| e == "js"), l.exts.iter().any(|e| e == "json"));
                f.follow = l.follow;
            }
            None => {
                if let Some(id) = self.t().map(|t| t.sel.conn).or(self.active) {
                    f.conns = vec![id];
                }
            }
        }
        self.modal = Some(Modal::LinkFolder(Box::new(f)));
    }

    pub(in crate::app) fn link_dialog(&mut self, ui: &mut egui::Ui, f: &mut LinkForm, keep: &mut bool) {
        ui.set_width(600.0);
        let edit = f.edit.is_some();
        let head = if edit { Head::new("Use on connections").sub(f.name.clone()) } else { Head::new("Link a folder").sub("Scripts stay in the folder as plain files you can commit") };
        dialog_head(ui, &head.glyph(glyphs::link));
        let mut b = body_begin(ui);
        let outer = ui;
        let ui = &mut b.content_ui;
        let now = ui.input(|i| i.time);

        form_card(ui, "", "", |rows| {
            rows.row("Folder", "", |ui| {
                if !edit && ui.button("Choose\u{2026}").clicked() {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                        f.path = p.to_string_lossy().into_owned();
                        f.typed_at = 0.0;
                        f.name_set = false;
                    }
                }
                let field = egui::TextEdit::singleline(&mut f.path).font(egui::TextStyle::Monospace).hint_text("/path/to/queries").desired_width(f32::INFINITY).margin(egui::Margin::symmetric(8, 4));
                let r = ui.add_enabled(!edit, field);
                r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, !edit, "Folder path"));
                if r.changed() {
                    f.typed_at = now;
                    f.name_set = false;
                }
            });
            rows.row("Name", "Shown in the library", |ui| {
                let hint = folder_name(&expand(&f.path));
                if !f.name_set && !edit {
                    f.name = hint.clone();
                }
                let r = ui.add(egui::TextEdit::singleline(&mut f.name).hint_text(hint).desired_width(f32::INFINITY).margin(egui::Margin::symmetric(8, 4)));
                r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Link name"));
                if r.changed() {
                    f.name_set = true;
                }
            });
        });

        // The scan that fills the summary runs off the UI thread, once the path has rested.
        let path = f.path.trim().to_string();
        let exts = f.exts();
        let fresh = f.preview.as_ref().is_some_and(|(p, _)| *p == path);
        if !path.is_empty() && !fresh && f.asking.is_none() && now - f.typed_at >= TYPING_PAUSE {
            f.asking = Some(path.clone());
            let (root, all) = (expand(&path), ["sql", "js", "json"].map(String::from).to_vec());
            self.worker.blocking(move || db::Msg::LibScan { link: 0, scan: disk::scan(&root, &all, None) });
        } else if !path.is_empty() && !fresh {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(TYPING_PAUSE));
        }
        ui.add_space(-6.0);
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            match (&f.preview, path.is_empty()) {
                (_, true) => {
                    ui.label(RichText::new("Choose a folder to see what it holds").size(TXT_SMALL).color(FAINT()));
                }
                (Some((p, scan)), false) if *p == path => {
                    // The scan reads every type, so the count follows the file types ticked below.
                    let mut shown = scan.clone();
                    shown.entries.retain(|e| exts.contains(&format::ext_of(&e.rel)));
                    let (dot, text) = summary(&shown);
                    let (r, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 4.0, dot);
                    ui.label(RichText::new(text).size(TXT_SMALL).color(MUTED()));
                }
                _ => {
                    spin(ui);
                    ui.label(RichText::new("Looking at the folder").size(TXT_SMALL).color(FAINT()));
                }
            }
        });
        ui.add_space(14.0);

        card_caption(ui, "Use on", "");
        let w = ui.available_width();
        let mut on = f.on.clone();
        if radio_card(ui, w, on == "conns", "Chosen connections", "Shows in their library and their Query tabs").clicked() {
            on = "conns".into();
        }
        if on == "conns" {
            ui.add_space(6.0);
            let mut gone = None;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                for id in f.conns.clone() {
                    let (name, dot) = self.conn(id).map_or(("removed connection".to_string(), FAINT()), |c| (c.meta.name.clone(), crate::app::sidebar::status_color(&c.status)));
                    if conn_chip(ui, egui::Id::new(("link-chip", id)), &name, dot) {
                        gone = Some(id);
                    }
                }
                let free: Vec<(u64, String)> = self.conns.iter().filter(|c| !f.conns.contains(&c.meta.id)).map(|c| (c.meta.id, c.meta.name.clone())).collect();
                if !free.is_empty() {
                    ui.menu_button(RichText::new("+ Add").color(PRIMARY()), |ui| {
                        menu_look(ui);
                        for (id, name) in &free {
                            if ui.button(name).clicked() {
                                if f.conns.is_empty() {
                                    f.engine = self.conn(*id).map(|c| c.meta.engine).unwrap_or_default();
                                }
                                f.conns.push(*id);
                                ui.close();
                            }
                        }
                    });
                }
            });
            if let Some(id) = gone {
                f.conns.retain(|c| *c != id);
            }
            ui.add_space(6.0);
        }
        ui.add_space(6.0);
        if radio_card(ui, w, on == "engine", &format!("Every {} connection", engine_name(f.engine)), "Matches the dialect of the .sql files").clicked() {
            on = "engine".into();
        }
        ui.add_space(6.0);
        if radio_card(ui, w, on == "all", "Every connection", "A script opens on the connection of the current tab").clicked() {
            on = "all".into();
        }
        f.on = on;
        ui.add_space(16.0);

        form_card(ui, "Files", "", |rows| {
            for (ext, text, v) in [("sql", "SQL scripts", &mut f.sql), ("js", "Shell scripts", &mut f.js), ("json", "Mongo filters and pipelines", &mut f.json)] {
                rows.row(&format!(".{ext}"), text, |ui| {
                    let r = ui.add(egui::Checkbox::new(v, ""));
                    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, *v, format!(".{ext} files")));
                });
            }
            rows.row("Follow changes", "Reload when a file changes on disk", |ui| {
                let r = toggle(ui, &mut f.follow);
                let on = f.follow;
                r.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, on, "Follow changes"));
            });
        });
        b.end(outer);
        let ui = outer;

        let dir = expand(&f.path);
        let found = f.preview.as_ref().is_some_and(|(p, s)| *p == f.path.trim() && !s.missing);
        let clash = (!edit).then(|| self.library.links.iter().find(|l| l.path == dir).map(|l| l.name.clone())).flatten();
        let problem = if f.path.trim().is_empty() {
            Some("Choose a folder".to_string())
        } else if let Some(n) = clash {
            Some(format!("Already linked as {n}"))
        } else if !edit && !found {
            Some(if f.preview.as_ref().is_some_and(|(p, s)| *p == f.path.trim() && s.missing) { "That folder does not exist".to_string() } else { "Looking at the folder".to_string() })
        } else if f.exts().is_empty() {
            Some("Choose at least one file type".to_string())
        } else if f.on == "conns" && f.conns.is_empty() {
            Some("Choose at least one connection".to_string())
        } else {
            None
        };
        let mut save = false;
        dialog_foot(ui, |ui| {
            save = ui.add_enabled(problem.is_none(), primary_button(ui, if edit { "Save" } else { "Link folder" }, PRIMARY())).clicked();
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
            if let Some(p) = &problem {
                foot_note(ui, p);
            }
        });
        if save {
            let name = if f.name.trim().is_empty() { folder_name(&dir) } else { f.name.trim().to_string() };
            let id = f.edit.unwrap_or_else(|| self.library.next_id());
            let link = Link { id, path: dir, name: name.clone(), on: f.on.clone(), conns: if f.on == "conns" { f.conns.clone() } else { Vec::new() }, engine: if f.on == "engine" { format::engine_key(f.engine).into() } else { String::new() }, exts: f.exts(), follow: f.follow };
            match self.library.links.iter_mut().find(|l| l.id == id) {
                Some(l) => *l = link,
                None => self.library.links.push(link),
            }
            self.save_library();
            self.lib_ui.rescan.insert(id);
            self.ok(if edit { format!("{name} updated.") } else { format!("Linked {name}.") });
            *keep = false;
        }
    }
}
