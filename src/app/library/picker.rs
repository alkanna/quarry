//! The quick picker that replaces the old library menu in the Query, Documents, Aggregate and Shell views: search, the
//! recent scripts that fit the view, and the folders. It only picks: saving is the Save dialog, and deleting is the
//! sidebar's. The popover draws from a snapshot (`PickData`) and returns what was asked (`PickAct`), so the views that
//! hold borrows of the tab can still show it.
#![allow(dead_code)]

use super::open::How;
use super::save_dialog::folder_rows;
use super::*;
use crate::library::ScriptRef;
use crate::library::tree::live_parent;
use std::collections::HashSet;

/// What a view lists: the scripts of its own kind that fit it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::app) enum PickKind {
    /// The Query view of a SQL connection: the connection's SQL scripts.
    Sql,
    /// The Documents view: the filters of this collection.
    Query,
    /// The Aggregate view: the pipelines of this collection.
    Pipeline,
    /// The Shell view: the connection's Shell scripts.
    Shell,
}

impl PickKind {
    fn kind(self) -> &'static str {
        match self {
            PickKind::Sql => "sql",
            PickKind::Query => "filter",
            PickKind::Pipeline => "pipeline",
            PickKind::Shell => "shell",
        }
    }
}

/// One script of the list: what the row shows and what the search looks at.
pub(in crate::app) struct PickRow {
    pub r: ScriptRef,
    pub name: String,
    pub first: String,
    pub hay: String,
}

pub(in crate::app) struct PickFolder {
    pub id: u64,
    pub depth: usize,
    pub name: String,
    /// The scripts in it and in its subfolders that fit the view.
    pub count: usize,
    /// Its own scripts, as indexes into `PickData::rows`.
    pub own: Vec<usize>,
}

pub(in crate::app) struct PickData {
    pub kind: PickKind,
    /// The scripts that fit the view, for the button's label. Known even when the lists below are not built.
    pub count: usize,
    /// The lists below are built, which happens only while the popover is open.
    pub full: bool,
    pub conn_name: String,
    pub dialect: db::sql::Dialect,
    pub rows: Vec<PickRow>,
    /// Indexes into `rows`, most recently opened first.
    pub recent: Vec<usize>,
    pub folders: Vec<PickFolder>,
    /// Scripts in no folder, after the folders.
    pub loose: Vec<usize>,
    /// The files of linked folders that fit the view, after the recent ones.
    pub linked: Vec<usize>,
    /// Whether the sidebar has a Library panel to open.
    pub sidebar: bool,
    /// Cmd+O asked for the popover.
    pub open: bool,
    /// The script the tab is bound to, for the chip beside the button: what it shows and whether the tab changed it.
    pub bound: Option<(String, bool)>,
}

pub(in crate::app) enum PickAct {
    Open(ScriptRef, How),
    Save,
    SaveAs,
    Show,
    Unbind,
    Sidebar,
}

/// The start of a script on one line, skipping the comments a person starts with, for the row under its name.
fn first_line(kind: &str, body: &str) -> String {
    let skip = |l: &str| match kind {
        "sql" => l.starts_with("--") || l.starts_with('#') || l.starts_with("/*"),
        "shell" => l.starts_with("//") || l.starts_with("async function"),
        _ => false,
    };
    let lines: Vec<&str> = body.lines().map(str::trim).filter(|l| !l.is_empty() && !skip(l)).collect();
    let text = if lines.is_empty() { body.split_whitespace().collect::<Vec<_>>().join(" ") } else { lines.join(" ") };
    text.chars().take(200).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The frame a picker's popover last drew in, kept in egui's memory: the views build the full lists only while it is open.
fn seen_id() -> egui::Id {
    egui::Id::unique("quarry-lib-picker-seen")
}

/// Whether a picker popover drew last frame or this one, so it is open.
fn picker_open(ctx: &egui::Context) -> bool {
    let now = ctx.cumulative_frame_nr();
    ctx.data(|d| d.get_temp::<u64>(seen_id())).is_some_and(|f| f + 1 >= now)
}

impl App {
    /// The scripts the current tab's view lists, grouped for the picker. While no picker is open only the count is worked
    /// out, since the views ask for this every frame and the lists cost an allocation per script.
    pub(in crate::app) fn pick_data(&self, ctx: &egui::Context, kind: PickKind) -> PickData {
        let lib = &self.library;
        let (conn, db, coll) = self.t().map(|t| (t.sel.conn, t.sel.db.clone(), t.sel.coll.clone())).unwrap_or_default();
        let fits = |s: &crate::library::Script| s.kind == kind.kind() && s.conn == conn && (matches!(kind, PickKind::Sql | PickKind::Shell) || (s.db == db && s.coll == coll));
        // Files of the linked folders used on this connection, of this view's kind; a filter or pipeline file that names a
        // collection shows only on that collection.
        let engine = self.conn(conn).map(|c| c.meta.engine);
        let files: Vec<(u64, &crate::library::disk::Entry)> = lib
            .links
            .iter()
            .filter(|l| engine.is_some_and(|e| crate::library::format::fits(l, kind.kind(), conn, e)))
            .filter_map(|l| self.lib_ui.scans.get(&l.id).map(|s| (l.id, s)))
            .flat_map(|(id, s)| s.entries.iter().map(move |e| (id, e)))
            .filter(|(_, e)| e.kind == kind.kind() && (matches!(kind, PickKind::Sql | PickKind::Shell) || ((e.target.coll.is_empty() || e.target.coll == coll) && (e.target.db.is_empty() || e.target.db == db))))
            .collect();
        let count = lib.scripts.iter().filter(|s| fits(s)).count() + files.len();
        let bound = self.t().and_then(|t| t.script.as_ref().filter(|b| b.view == t.view).map(|b| (b.r.clone(), super::linked::bound_text(t).is_some_and(|x| x != b.base))));
        let bound = bound.map(|(r, edited)| (self.chip_text(&r), edited));
        if !picker_open(ctx) {
            return PickData { kind, count, full: false, conn_name: String::new(), dialect: self.dialect_of(conn), rows: Vec::new(), recent: Vec::new(), folders: Vec::new(), loose: Vec::new(), linked: Vec::new(), sidebar: panel::READY, open: self.lib_ui.open_picker, bound };
        }
        let mut rows = Vec::new();
        let mut index = std::collections::HashMap::new();
        for s in lib.scripts.iter().filter(|s| fits(s)) {
            let first = first_line(&s.kind, &s.body);
            let hay = format!("{} {}", s.name, s.body.chars().take(200).collect::<String>()).to_lowercase();
            index.insert(s.id, rows.len());
            rows.push(PickRow { r: ScriptRef::Saved(s.id), name: s.name.clone(), first, hay });
        }
        let mut linked = Vec::new();
        for (link, e) in &files {
            let name = e.rel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let hay = format!("{} {}", e.rel.to_string_lossy(), e.head.chars().take(200).collect::<String>()).to_lowercase();
            linked.push(rows.len());
            rows.push(PickRow { r: ScriptRef::File { link: *link, rel: e.rel.clone() }, name, first: first_line(kind.kind(), &e.head), hay });
        }
        let recent: Vec<usize> = lib.recent.iter().filter_map(|r| match r {
            ScriptRef::Saved(id) => index.get(id).copied(),
            file => rows.iter().position(|x| x.r == *file),
        }).take(6).collect();
        let folder_of = |id: u64| lib.scripts.iter().find(|s| s.id == id).and_then(|s| s.folder);
        let mut folders = Vec::new();
        for (id, depth) in folder_rows(lib) {
            let own: Vec<usize> = lib.scripts.iter().filter(|s| live_parent(lib, s.folder) == Some(id)).filter_map(|s| index.get(&s.id).copied()).collect();
            // The scripts below it count too, so a closed parent still says what it holds.
            let below = rows.iter().enumerate().filter(|(_, r)| match r.r {
                ScriptRef::Saved(sid) => {
                    let mut at = folder_of(sid);
                    let mut hops = 0;
                    while let Some(f) = at {
                        if f == id {
                            return true;
                        }
                        at = lib.folders.iter().find(|x| x.id == f).and_then(|x| x.parent);
                        hops += 1;
                        if hops > 32 {
                            break;
                        }
                    }
                    false
                }
                _ => false,
            }).count();
            if below > 0 {
                folders.push(PickFolder { id, depth, name: lib.folders.iter().find(|f| f.id == id).map(|f| f.name.clone()).unwrap_or_default(), count: below, own });
            }
        }
        // A script whose folder is gone shows with the loose ones, as in the panel.
        let loose: Vec<usize> = lib.scripts.iter().filter(|s| live_parent(lib, s.folder).is_none()).filter_map(|s| index.get(&s.id).copied()).collect();
        PickData { kind, count, full: true, conn_name: self.conn_name(conn), dialect: self.dialect_of(conn), rows, recent, folders, loose, linked, sidebar: panel::READY, open: self.lib_ui.open_picker, bound }
    }

    /// The chip of a bound tab: "monthly-revenue.sql  ·  shop-queries / reports", the script and where it is kept.
    pub(in crate::app) fn chip_text(&self, r: &ScriptRef) -> String {
        let place = match r {
            ScriptRef::Saved(id) => {
                let folder = self.library.scripts.iter().find(|s| s.id == *id).map(|s| crate::library::ops::folder_path(&self.library, s.folder)).unwrap_or_default();
                if folder.is_empty() { "Library".to_string() } else { folder }
            }
            ScriptRef::File { link, rel } => {
                let l = self.library.links.iter().find(|l| l.id == *link).map(|l| if l.name.is_empty() { l.path.display().to_string() } else { l.name.clone() }).unwrap_or_default();
                match rel.parent().filter(|p| !p.as_os_str().is_empty()) {
                    Some(p) => format!("{l} / {}", p.display()),
                    None => l,
                }
            }
        };
        format!("{}  \u{b7}  {place}", self.script_name(r))
    }

    /// Does what the picker asked for.
    pub(in crate::app) fn pick_acts(&mut self, acts: Vec<PickAct>) {
        // The view drew its picker this frame, so a Cmd+O for it has been seen.
        self.lib_ui.open_picker = false;
        for a in acts {
            match a {
                PickAct::Open(r, how) => self.open_script(&r, how),
                PickAct::Save => self.save_script(),
                PickAct::SaveAs => self.save_script_as(),
                PickAct::Show => {
                    if let Some(r) = self.t().and_then(|t| t.script.as_ref()).map(|b| b.r.clone()) {
                        self.show_in_library(&r);
                    }
                }
                PickAct::Unbind => self.unbind_script(),
                PickAct::Sidebar => {
                    if self.prefs.sidebar_hidden || self.prefs.side_mode() != SideMode::Library {
                        self.prefs.set_side_mode(SideMode::Library);
                        self.prefs.sidebar_hidden = false;
                        self.save_prefs();
                    }
                }
            }
        }
    }

    /// In the offline demo with QUARRY_TEST_LIBRARY=`which`, holds the picker open for the picture.
    pub(in crate::app) fn demo_open_picker(&self, ctx: &egui::Context, button: &egui::Response, which: &str) {
        if self.demo && std::env::var("QUARRY_TEST_LIBRARY").as_deref() == Ok(which) {
            egui::Popup::open_id(ctx, egui::Popup::default_response_id(button));
        }
    }
}

/// What the keys do on a row, spelled for this system.
fn hint() -> String {
    format!("{} opens  \u{b7}  {} opens in a new tab  \u{b7}  {} runs all", super::keys_text(false, false, false, "Enter"), super::keys_text(false, false, true, "Enter"), super::keys_text(true, false, false, "Enter"))
}

/// One two-line row: the name over the start of the script in mono with its syntax colors.
fn script_row(ui: &mut egui::Ui, d: &PickData, row: &PickRow, selected: bool) -> egui::Response {
    let (resp, hovered) = pop_row(ui, POP_ROW2_H);
    let rect = resp.rect;
    let p = ui.painter().clone();
    if selected && !hovered {
        p.rect_filled(rect.expand2(egui::vec2(6.0, 0.0)), R_SM, MENU_HOVER());
    }
    let (name_y, code_y) = (rect.top() + 15.0, rect.top() + 33.0);
    let mut name = egui::text::LayoutJob::single_section(row.name.clone(), ui_fmt(TXT_BODY, BRIGHT(), W_MEDIUM, 0.0));
    name.wrap = egui::text::TextWrapping::truncate_at_width(rect.width() - 8.0);
    galley_at(&p, rect.left(), name_y, p.layout_job(name), BRIGHT());
    let mut job = match d.kind {
        PickKind::Sql => sql_job_in(&row.first, TXT_BODY, d.dialect),
        PickKind::Shell => js_job(&row.first),
        PickKind::Query | PickKind::Pipeline => json_job(&row.first),
    };
    job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width() - 2.0);
    galley_at(&p, rect.left(), code_y, p.layout_job(job), TEXT());
    let label = format!("Script {}", row.name);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label.clone()));
    resp.on_hover_text(hint())
}

/// The popover. `icon` shows its button as the book icon, as the find bar does; otherwise it is a text button with the
/// number of scripts. Returns the button, which the demo uses to hold the popover open.
pub(in crate::app) fn picker_button(ui: &mut egui::Ui, d: &PickData, icon: bool, acts: &mut Vec<PickAct>) -> egui::Response {
    let body = |ui: &mut egui::Ui| picker_body(ui, d, acts);
    let button = if icon { icon_menu(ui, paint_book, true, body).on_hover_text(format!("Library ({})", super::keys_text(true, false, false, "O"))) } else { sticky_menu(ui, format!("Library {}", d.count), body) };
    if d.open {
        egui::Popup::open_id(ui.ctx(), egui::Popup::default_response_id(&button));
    }
    // Drawn after the button, so in the views' right-to-left headers it sits to its left.
    if let Some((text, edited)) = &d.bound {
        bound_chip(ui, text, *edited, acts);
    }
    button
}

/// The chip of a tab bound to a script: its name and place, a dot while the tab holds changes not saved to it, and a menu
/// with what can be done with the binding.
fn bound_chip(ui: &mut egui::Ui, text: &str, edited: bool, acts: &mut Vec<PickAct>) {
    use egui::containers::menu::MenuButton;
    ui.scope(|ui| {
        quiet_look(ui);
        ui.set_max_width(280.0);
        if edited {
            let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, BUTTON_H), Sense::hover());
            ui.painter().circle_filled(dot.center(), 3.0, WARNING());
        }
        let (r, _) = MenuButton::from_button(egui::Button::new(RichText::new(text).size(TXT_SMALL)).truncate()).ui(ui, |ui| {
            menu_look(ui);
            ui.spacing_mut().button_padding = egui::vec2(10.0, 5.0);
            for (label, key, act) in [("Save", super::keys_text(true, false, false, "S"), PickAct::Save), ("Save as new\u{2026}", super::keys_text(true, true, false, "S"), PickAct::SaveAs)] {
                if ui.add(egui::Button::new(label).shortcut_text(RichText::new(key).color(FAINT()))).clicked() {
                    acts.push(act);
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("Show in library").clicked() {
                acts.push(PickAct::Show);
                ui.close();
            }
            if ui.button("Unbind").on_hover_text("Keep the text, and stop saving it to this script").clicked() {
                acts.push(PickAct::Unbind);
                ui.close();
            }
        });
        let label = format!("Bound to {text}{}", if edited { ", edited" } else { "" });
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label.clone()));
    });
}

fn picker_body(ui: &mut egui::Ui, d: &PickData, acts: &mut Vec<PickAct>) {
    let ctx = ui.ctx().clone();
    // The frame number is read first: asking the context for it inside data_mut would lock it twice.
    let frame = ctx.cumulative_frame_nr();
    ctx.data_mut(|x| x.insert_temp(seen_id(), frame));
    // The frame the popover opens in had only the count. It is egui's invisible sizing pass, and the next frame has the lists.
    if !d.full {
        // The width is taken all the same: egui places the popover from this pass, and a narrow one ran off the window.
        popover_body(ui, 360.0, |_| {});
        ctx.request_repaint();
        return;
    }
    popover_body(ui, 360.0, |ui| {
        ui.horizontal(|ui| {
            ui.label(semibold(RichText::new("Library").size(TXT_TITLE).color(BRIGHT())));
            ui.label(RichText::new(&d.conn_name).monospace().size(TXT_CAPTION).color(FAINT()));
            if d.sidebar {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let r = quiet(ui, "Open in sidebar");
                    if r.clicked() {
                        acts.push(PickAct::Sidebar);
                        ui.close();
                    }
                });
            }
        });
        ui.add_space(10.0);
        // The search text, the open folders and the highlighted row outlive the popup, so it reopens where it was left.
        let (fid, open_id, sel_id, seen_id) = (ui.make_persistent_id("pick-search"), ui.make_persistent_id("pick-open"), ui.make_persistent_id("pick-sel"), ui.make_persistent_id("pick-seen"));
        let mut needle: String = ui.data(|x| x.get_temp(fid)).unwrap_or_default();
        let mut open: HashSet<u64> = ui.data(|x| x.get_temp::<Vec<u64>>(open_id)).unwrap_or_default().into_iter().collect();
        let mut sel: usize = ui.data(|x| x.get_temp(sel_id)).unwrap_or(0);
        // A popup that was closed last frame is a new one: it starts with an empty search that has the focus.
        let now = ui.ctx().cumulative_frame_nr();
        if ui.data(|x| x.get_temp::<u64>(seen_id)).is_none_or(|l| now > l + 2) {
            needle.clear();
            sel = 0;
            ui.memory_mut(|m| m.request_focus(fid));
        }
        ui.data_mut(|x| x.insert_temp(seen_id, now));
        let before = needle.clone();
        well_field(ui, fid, FieldIcon::Search, None, |ui| {
            let r = ui.add(field_edit(&mut needle, fid, "Search scripts", ui.available_width(), egui::TextStyle::Body));
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Search library"));
        });
        if needle != before {
            sel = 0;
        }
        ui.add_space(8.0);
        let q = needle.trim().to_lowercase();
        // The scripts shown, in the order they are drawn, for the arrow keys and Return.
        let mut order: Vec<usize> = Vec::new();
        egui::ScrollArea::vertical().id_salt("pick-list").max_height(340.0).auto_shrink([false, true]).show(ui, |ui| {
            let mut draw = |ui: &mut egui::Ui, i: usize, order: &mut Vec<usize>| {
                let selected = order.len() == sel;
                order.push(i);
                let r = script_row(ui, d, &d.rows[i], selected);
                if r.clicked() {
                    acts.push(PickAct::Open(d.rows[i].r.clone(), How::Here));
                    ui.close();
                }
            };
            if !q.is_empty() {
                let hits: Vec<usize> = (0..d.rows.len()).filter(|&i| d.rows[i].hay.contains(&q)).collect();
                if hits.is_empty() {
                    empty_state(ui, paint_search, "Nothing matches", "", |_| {});
                } else {
                    section_head(ui, &format!("RESULTS  {}", hits.len()), None);
                    for i in hits {
                        draw(ui, i, &mut order);
                    }
                }
                return;
            }
            if d.rows.is_empty() {
                empty_state(ui, paint_book, "No saved scripts yet", &format!("{} saves the one you are writing.", super::keys_text(true, false, false, "S")), |_| {});
                return;
            }
            if !d.recent.is_empty() {
                section_head(ui, "RECENT", None);
                for &i in &d.recent {
                    draw(ui, i, &mut order);
                }
                ui.add_space(8.0);
            }
            if !d.linked.is_empty() {
                section_head(ui, "LINKED", None);
                for &i in &d.linked {
                    draw(ui, i, &mut order);
                }
                ui.add_space(8.0);
            }
            if !d.folders.is_empty() {
                section_head(ui, "FOLDERS", None);
            }
            // A closed folder hides what is below it: the rows after it that are deeper.
            let mut hidden: Option<usize> = None;
            for f in &d.folders {
                if hidden.is_some_and(|h| f.depth > h) {
                    continue;
                }
                hidden = None;
                let (row, _) = pop_row(ui, POP_ROW_H);
                let (p, rect) = (ui.painter().clone(), row.rect);
                let x = rect.left() + f.depth as f32 * 14.0;
                let is_open = open.contains(&f.id);
                chevron(&p, egui::pos2(x + 6.0, rect.center().y), is_open, MUTED());
                galley_at(&p, x + 20.0, rect.center().y, p.layout_no_wrap(f.name.clone(), FontId::proportional(TXT_BODY), BRIGHT()), BRIGHT());
                let count = p.layout_no_wrap(f.count.to_string(), FontId::monospace(TXT_CAPTION), FAINT());
                galley_at(&p, rect.right() - count.size().x, rect.center().y, count, FAINT());
                let label = format!("Folder {}", f.name);
                row.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label.clone()));
                if row.clicked() {
                    if !open.remove(&f.id) {
                        open.insert(f.id);
                    }
                }
                if is_open {
                    for &i in &f.own {
                        draw(ui, i, &mut order);
                    }
                } else {
                    hidden = Some(f.depth);
                }
            }
            if !d.loose.is_empty() {
                ui.add_space(8.0);
                section_head(ui, "NOT IN A FOLDER", None);
            }
            for &i in &d.loose {
                draw(ui, i, &mut order);
            }
        });
        // Arrows move the highlight; Return opens, Alt+Return opens in a new tab, Cmd+Enter runs. The longer combinations
        // are asked first, since extra Alt and Shift still match a shorter one.
        if !order.is_empty() {
            sel = sel.min(order.len() - 1);
            let (none, cmd, alt) = (egui::Modifiers::NONE, egui::Modifiers::COMMAND, egui::Modifiers::ALT);
            let (down, up, run, tab, plain) = ui.input_mut(|i| (i.consume_key(none, egui::Key::ArrowDown), i.consume_key(none, egui::Key::ArrowUp), i.consume_key(cmd, egui::Key::Enter), i.consume_key(alt, egui::Key::Enter), i.consume_key(none, egui::Key::Enter)));
            if down {
                sel = (sel + 1).min(order.len() - 1);
            }
            if up {
                sel = sel.saturating_sub(1);
            }
            let how = if run { Some(How::Run) } else if tab { Some(How::NewTab) } else if plain { Some(How::Here) } else { None };
            if let Some(how) = how {
                acts.push(PickAct::Open(d.rows[order[sel]].r.clone(), how));
                ui.close();
            }
        }
        ui.data_mut(|x| {
            x.insert_temp(fid, needle.clone());
            x.insert_temp(open_id, open.iter().copied().collect::<Vec<u64>>());
            x.insert_temp(sel_id, sel);
        });
        popover_divider(ui);
        let (row, _) = pop_row(ui, POP_ROW_H);
        let (p, rect) = (ui.painter().clone(), row.rect);
        paint_plus(&p, egui::pos2(rect.left() + 6.0, rect.center().y), 4.5, PRIMARY());
        galley_at(&p, rect.left() + 20.0, rect.center().y, p.layout_no_wrap("Save this script…".into(), FontId::proportional(TXT_BODY), TEXT()), TEXT());
        let key = p.layout_no_wrap(super::keys_text(true, false, false, "S"), FontId::proportional(TXT_CAPTION), FAINT());
        galley_at(&p, rect.right() - key.size().x, rect.center().y, key, FAINT());
        row.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Save this script"));
        if row.clicked() {
            acts.push(PickAct::Save);
            ui.close();
        }
        ui.add_space(4.0);
        let (n, m) = (d.rows.len() - d.linked.len(), d.folders.len());
        ui.label(RichText::new(format!("{n} script{}, {m} folder{}", if n == 1 { "" } else { "s" }, if m == 1 { "" } else { "s" })).size(TXT_CAPTION).color(FAINT()));
    });
}

impl App {
    /// QUARRY_TEST_LIBRARY=save, picker, picker-docs or picker-shell: the Save dialog and the quick pickers over a made-up
    /// library. Returns whether `state` was one of these.
    pub(in crate::app) fn demo_library_quick(&mut self, state: &str) -> bool {
        if !matches!(state, "save" | "picker" | "picker-docs" | "picker-shell") {
            return false;
        }
        // The SQL states show the MySQL demo's Query tab; the others stay on the Mongo demo.
        if matches!(state, "save" | "picker") {
            (self.conns, self.tabs) = (Vec::new(), Vec::new());
            self.open_dbs.clear();
            self.load_sql_demo();
            self.sql_query_demo();
        }
        self.demo_scripts();
        match state {
            "save" => self.save_script(),
            "picker-shell" => {
                if let Some(t) = self.tm() {
                    t.view = View::Shell;
                }
            }
            _ => {}
        }
        true
    }

    /// Three folders and a dozen scripts on the two demo connections, with some of them opened recently.
    pub(in crate::app) fn demo_scripts(&mut self) {
        use crate::library::{Folder, Library, Script};
        let lib = &mut self.library;
        let folder = |lib: &mut Library, name: &str, parent: Option<u64>| {
            let id = lib.next_id();
            lib.folders.push(Folder { id, name: name.into(), parent });
            id
        };
        let (reports, maint) = (folder(lib, "Reports", None), folder(lib, "Maintenance", None));
        let monthly = folder(lib, "Monthly", Some(reports));
        let add = |lib: &mut Library, kind: &str, conn: u64, name: &str, body: &str, folder: Option<u64>| {
            let id = lib.next_id();
            let (db, coll) = if matches!(kind, "filter" | "pipeline") { ("shop", "orders") } else { ("", "") };
            lib.scripts.push(Script { id, name: name.into(), kind: kind.into(), body: body.into(), conn, db: db.into(), coll: coll.into(), folder, updated: 1_731_400_000 - id * 3600, ..Default::default() });
            id
        };
        let late = add(lib, "sql", 3, "Late shipments", "-- Paid orders that never shipped\nSELECT o.id, o.created_at, s.carrier\nFROM orders o\nLEFT JOIN shipments s ON s.order_id = o.id\nWHERE o.status = 'paid' AND s.id IS NULL;", Some(reports));
        let tier = add(lib, "sql", 3, "Revenue by tier", "SELECT tier, SUM(total) AS revenue FROM orders GROUP BY tier ORDER BY revenue DESC;", Some(reports));
        add(lib, "sql", 3, "Top customers", "SELECT c.name, SUM(o.total) AS spent FROM customers c JOIN orders o ON o.customer_id = c.id GROUP BY c.id ORDER BY spent DESC LIMIT 20;", Some(monthly));
        let orphans = add(lib, "sql", 3, "Orphaned payments", "SELECT p.* FROM payments p LEFT JOIN orders o ON o.id = p.order_id WHERE o.id IS NULL;", Some(maint));
        add(lib, "sql", 3, "Scratch", "SELECT NOW();", None);
        let paid = add(lib, "filter", 1, "Paid over 100", "{ status: 'paid', total: { $gt: 100 } }", Some(reports));
        add(lib, "filter", 1, "Pending orders", "{ status: 'pending' }", None);
        let grouped = add(lib, "pipeline", 1, "Revenue by tier", "[\n  { $group: { _id: '$customer.tier', revenue: { $sum: '$total' } } },\n  { $sort: { revenue: -1 } }\n]", Some(reports));
        let backfill = add(lib, "shell", 1, "Backfill note field", "// Fill the empty notes\nasync function run() {\n  return await orders.find({ note: null }, { limit: 20 });\n}", Some(maint));
        add(lib, "shell", 3, "Count orders by status", "async function run() {\n  return await sql`SELECT status, COUNT(*) AS n FROM orders GROUP BY status`;\n}", None);
        lib.recent = [late, tier, orphans, paid, grouped, backfill].map(ScriptRef::Saved).to_vec();
        self.lib_ui.last_folder = Some(reports);
    }
}
