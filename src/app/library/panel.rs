//! The Library panel of the sidebar: folders and scripts kept by Quarry and linked folders, a scope switch,
//! a search, a preview card, and the menus and drags that organize them. The rows come from `library::tree::flatten`, the
//! changes go through `library::ops`, and a script leaves for a tab through `open_script`.
//!
//! Only the rows in view are drawn, so nothing here holds a reference into `self` across a frame: the drawing collects
//! `Act`s and `apply` runs them once the panel is done. Rows are one height, and their places follow from their index
//! except while a folder opens or closes, when `tree::lay_out` places them.

use super::*;
use crate::app::restore::Restore;
use crate::library::ops::{self, DropAt};
use crate::library::tree::{self, Node, Row, Scans};
use crate::library::{Library, ScriptRef};
use std::path::{Path, PathBuf};
use crate::ui::dnd::{Spot, Zone, auto_scroll_while_dragging, drag_cancelled, drag_source_with, drop_fill, hover_open, paint_spot, side_row_drag, y_frac, zone_at};
use crate::ui::fold;
use super::open::How;

/// Whether the rail shows the Library button.
pub(in crate::app) const READY: bool = true;

const ROW_H: f32 = 26.0;
const INDENT: f32 = 16.0;
/// Preview card height when the person never dragged the splitter, and the room the tree and the card keep.
const PREVIEW_DEFAULT: f32 = 196.0;
const PREVIEW_MIN: f32 = 130.0;
const TREE_MIN: f32 = 120.0;

/// The id a folder's open and close animation goes by.
fn fold_id(n: &Node) -> egui::Id {
    egui::Id::unique(("lib-fold", n))
}

/// What the panel remembers while the app runs. Folders open or closed are not saved: a fresh start shows them closed.
#[derive(Default)]
pub(in crate::app) struct PanelUi {
    pub open: HashSet<Node>,
    pub sel: Option<Node>,
    pub query: String,
    /// The row being renamed, its text, and whether the editor still has to take focus.
    pub rename: Option<(Node, String, bool)>,
    /// Keys act on the tree only after a row was clicked and until something else takes the pointer or the keyboard.
    pub focus: bool,
    pub scroll_y: f32,
    pub view_h: f32,
    pub scroll_set: Option<f32>,
    /// Demo only: a menu to open on this node, and a drag held from the first node over the second.
    pub demo_menu: Option<Node>,
    pub demo_drag: Option<(Node, Node)>,
    /// What a drop where the drag rests would do to the dragged item, as the ghost's badge says it: set by the row under
    /// the pointer this frame, read by the ghost the next, since the ghost may be drawn before that row.
    pub badge: Option<&'static str>,
    pub badge_next: Option<&'static str>,
    /// Folders a person opened or closed that may still be moving.
    pub folding: HashSet<Node>,
}

enum Act {
    Open(Node, How),
    Toggle(Node),
    Rename(Node),
    Commit(Node, String),
    Duplicate(u64),
    Move(Node, DropAt),
    MoveToNew(Node),
    Copy(u64),
    Delete(Node),
    NewFolder(Option<u64>),
    FoldAll,
    Scope(bool),
    Link,
    /// Open on another connection than the script's own, from the Open on submenu.
    OpenOn(Node, u64),
    /// The empty state's Save current query: the Save dialog for the current tab.
    SaveCurrent,
    Refuse(&'static str),
    /// A linked folder's own actions: a new file or folder in it (link, folder below it), a read of the disk, the file
    /// manager (path, select it), the path on the clipboard, the text of a file, Use on connections and Unlink.
    NewScript(u64, PathBuf),
    NewDir(u64, PathBuf),
    Reload(u64),
    Reveal(PathBuf, bool),
    CopyPath(PathBuf),
    CopyFileText(u64, PathBuf),
    UseOn(u64),
    Unlink(u64),
    /// A linked folder that is gone: point it at the folder where it is now.
    Locate(u64),
    /// A drop or a menu move that touches the disk: a script into a linked folder, a file to another folder of its link, or
    /// a file copied into the library.
    Drop(Node, Plan),
}

/// What a drop on a row does. `Saved` is a move within the library, `Into` writes into a linked folder (a script) or moves
/// a file between its folders, `Out` copies a linked file into the library and leaves the file.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Plan {
    Saved(DropAt),
    Into { link: u64, dir: PathBuf },
    Out(DropAt),
}

/// The linked folder a drop on this row lands in: the link itself, one of its folders, or the folder of a file.
fn dir_of(n: &Node) -> Option<(u64, PathBuf)> {
    match n {
        Node::Link(l) => Some((*l, PathBuf::new())),
        Node::Dir { link, rel } => Some((*link, rel.clone())),
        Node::File { link, rel } => Some((*link, rel.parent().map(Path::to_path_buf).unwrap_or_default())),
        _ => None,
    }
}

/// Why nothing goes into a linked folder whose scan found it gone.
pub(in crate::app) const MISSING: &str = "That folder is not there. Reload it once it is back, or link the folder where it moved.";

/// What dropping `item` on `target` at `pos` would do, or why it cannot be done. Each link keeps its files to itself: a file
/// moves between the folders of its own link, and leaves it only as a copy in the library.
fn plan_drop(lib: &Library, scans: &Scans, item: &Node, target: &Node, pos: Zone) -> Result<Plan, &'static str> {
    if let Some((l, _)) = dir_of(target)
        && scans.get(&l).is_some_and(|s| s.missing)
    {
        return Err(MISSING);
    }
    let at = match (target, pos) {
        (Node::Folder(id), Zone::Into) => Some(DropAt::Into(*id)),
        (n, Zone::Before) if n.is_saved() => Some(DropAt::Before(n.clone())),
        (n, Zone::After) if n.is_saved() => Some(DropAt::After(n.clone())),
        _ => None,
    };
    let dir = dir_of(target);
    match (item, dir) {
        (Node::File { link, rel }, Some((l, dir))) => {
            if l != *link {
                Err("A file moves only between the folders of its own linked folder.")
            } else if rel.parent().unwrap_or(Path::new("")) == dir {
                Err("It is already in that folder.")
            } else {
                Ok(Plan::Into { link: l, dir })
            }
        }
        (Node::File { .. }, None) => at.map(Plan::Out).ok_or("Drop it on a folder of the library to copy it there."),
        (Node::Script(_), Some((link, dir))) => Ok(Plan::Into { link, dir }),
        (Node::Folder(_), Some(_)) => Err("Folders stay in the library. Drop a script on a linked folder instead."),
        (_, None) => match at {
            Some(a) => ops::can_move(lib, item, &a).map(|()| Plan::Saved(a)),
            None => Err("Drop it on a folder or beside a script."),
        },
        _ => Err("That cannot go there."),
    }
}

/// The word the dragged item's ghost carries for a drop `plan`: a script dropped on a linked folder becomes a file there
/// and leaves the library, a linked file dropped in the library is copied and stays on disk. A move inside the library or
/// inside one linked folder needs no word.
fn badge_of(plan: &Plan, item: &Node) -> Option<&'static str> {
    match (plan, item) {
        (Plan::Into { .. }, Node::Script(_)) => Some("Move to disk"),
        (Plan::Out(_), _) => Some("Copy"),
        _ => None,
    }
}

/// The glyph and color of a script kind.
fn kind_glyph(kind: &str) -> (glyphs::Glyph, egui::Color32) {
    match kind {
        "shell" => (glyphs::file_braces, WARNING()),
        "filter" => (glyphs::funnel, C_LINK()),
        "pipeline" => (glyphs::stack, C_OID()),
        "sql" => (glyphs::file_sql, C_KEYWORD()),
        _ => (glyphs::doc, MUTED()),
    }
}


/// "edited 5m ago", or "" for a script that never recorded a time.
fn edited(updated: u64) -> String {
    if updated == 0 {
        return String::new();
    }
    let s = crate::library::now_secs().saturating_sub(updated);
    if s >= 2 * 86400 {
        return format!("edited {}d ago", s / 86400);
    }
    format!("edited {} ago", crate::app::log::ago(std::time::UNIX_EPOCH + Duration::from_secs(updated)))
}

/// The row a drag carries; `drag_item` is its inverse.
fn item_node(d: &DragItem) -> Option<Node> {
    match d {
        DragItem::Script(r) => Some(Node::of_ref(r)),
        DragItem::Folder(id) => Some(Node::Folder(*id)),
        _ => None,
    }
}

/// Where over a row a drop lands, for a pointer `y_frac` down it. A folder takes the middle as "into" and the edges as
/// before and after; an open folder has its first child right below, so its lower edge also means "into". A script has
/// no inside, so it splits at the middle.
fn drop_zone(folder: bool, open: bool, y_frac: f32) -> Zone {
    match zone_at(1.0, y_frac, folder) {
        Zone::After if folder && open => Zone::Into,
        z => z,
    }
}

/// What a drop in the empty space below the rows does: a script or folder goes to the top level, a file is copied there.
fn root_plan(lib: &Library, item: &Node) -> Result<Plan, &'static str> {
    match item {
        Node::File { .. } => Ok(Plan::Out(DropAt::Root)),
        _ => ops::can_move(lib, item, &DropAt::Root).map(|()| Plan::Saved(DropAt::Root)),
    }
}

fn drag_item(node: &Node) -> Option<DragItem> {
    match node {
        Node::Folder(id) => Some(DragItem::Folder(*id)),
        n => n.script_ref().map(DragItem::Script),
    }
}

/// What a row shows and needs, looked up once per frame from the library.
struct RowCx<'a> {
    lib: &'a Library,
    conns: &'a [Conn],
    scans: &'a Scans,
    /// The linked files that changed on disk since Quarry last opened or saved them.
    changed: &'a HashSet<(u64, PathBuf)>,
    active: Option<u64>,
    groups: &'a crate::app::groups::Groups,
    /// The search, for the matched part of a name and the "in text" of a hit inside the script.
    query: String,
    /// The item being dragged, if a drag of a script or folder is on.
    drag: Option<Node>,
    demo_drag: Option<&'a (Node, Node)>,
    /// How far open each moving folder is drawn, for its chevron.
    folds: &'a HashMap<Node, f32>,
}

struct Look {
    name: String,
    /// What assistive tools read for the row: the name, with the state a missing folder shows.
    label: String,
    kind: String,
    right: String,
    /// The right text is a warning (a folder that is not there), or a branch drawn as a pill.
    warn: bool,
    pill: bool,
    /// A linked file that changed on disk since Quarry last opened or saved it.
    dot: bool,
}

impl Look {
    fn new(name: String, kind: String, right: String) -> Look {
        Look { label: name.clone(), name, kind, right, warn: false, pill: false, dot: false }
    }
}

impl RowCx<'_> {
    fn conn_name(&self, id: u64) -> String {
        self.conns.iter().find(|c| c.meta.id == id).map_or_else(|| "removed connection".to_string(), |c| c.meta.name.clone())
    }

    /// The connections a script or file can open on besides its own, for the Open on submenu: same engine for a saved
    /// script, the connections its link is used on for a file, with the current one first and the rest by group.
    fn open_on(&self, node: &Node) -> Vec<(String, Vec<(u64, String)>)> {
        let fits: Box<dyn Fn(&Conn) -> bool> = match node {
            Node::Script(id) => {
                let Some(s) = self.lib.scripts.iter().find(|s| s.id == *id) else { return Vec::new() };
                let own = self.conns.iter().find(|c| c.meta.id == s.conn).map(|c| c.meta.engine);
                let kind = s.kind.clone();
                Box::new(move |c: &Conn| super::open::kind_fits(&kind, c.meta.engine) && own.is_none_or(|e| e == c.meta.engine))
            }
            Node::File { link, rel } => {
                let Some(l) = self.lib.links.iter().find(|l| l.id == *link).cloned() else { return Vec::new() };
                let kind = super::linked::kind_in(self.scans, *link, rel);
                Box::new(move |c: &Conn| crate::library::format::fits(&l, &kind, c.meta.id, c.meta.engine))
            }
            _ => return Vec::new(),
        };
        let fit: Vec<&Conn> = self.conns.iter().filter(|c| fits(c)).collect();
        let mut out: Vec<(String, Vec<(u64, String)>)> = Vec::new();
        if let Some(c) = self.active.and_then(|a| fit.iter().find(|c| c.meta.id == a)) {
            out.push((String::new(), vec![(c.meta.id, c.meta.name.clone())]));
        }
        let active = self.active;
        for g in &self.groups.groups {
            let ids: Vec<(u64, String)> = g.members.iter().filter_map(|m| fit.iter().find(|c| c.meta.id == *m)).filter(|c| Some(c.meta.id) != active).map(|c| (c.meta.id, c.meta.name.clone())).collect();
            if !ids.is_empty() {
                out.push((g.name.clone(), ids));
            }
        }
        let loose: Vec<(u64, String)> = fit.iter().filter(|c| Some(c.meta.id) != active).filter(|c| self.groups.group_of(c.meta.id).is_none()).map(|c| (c.meta.id, c.meta.name.clone())).collect();
        if !loose.is_empty() {
            out.push((if out.is_empty() { String::new() } else { "Not in a group".into() }, loose));
        }
        out
    }

    fn look(&self, row: &Row) -> Look {
        let file_name = |rel: &std::path::Path| rel.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        match &row.node {
            Node::Folder(id) => Look::new(self.lib.folders.iter().find(|f| f.id == *id).map(|f| f.name.clone()).unwrap_or_default(), String::new(), row.count.to_string()),
            Node::Script(id) => {
                let Some(s) = self.lib.scripts.iter().find(|s| s.id == *id) else { return Look::new(String::new(), String::new(), String::new()) };
                let right = if self.active.is_some_and(|a| a != s.conn) || (self.active.is_none() && !self.conns.iter().any(|c| c.meta.id == s.conn)) {
                    self.conn_name(s.conn)
                } else {
                    match s.kind.as_str() {
                        "shell" => "shell".to_string(),
                        "filter" | "pipeline" => [s.db.as_str(), s.coll.as_str()].iter().filter(|x| !x.is_empty()).copied().collect::<Vec<_>>().join("/"),
                        _ => s.db.clone(),
                    }
                };
                Look::new(s.name.clone(), s.kind.clone(), right)
            }
            Node::Link(id) => {
                let l = self.lib.links.iter().find(|l| l.id == *id);
                let name = l.map(|l| if l.name.is_empty() { file_name(&l.path) } else { l.name.clone() }).unwrap_or_default();
                let scan = self.scans.get(id);
                let mut look = Look::new(name, String::new(), row.count.to_string());
                if scan.is_some_and(|s| s.missing) {
                    (look.right, look.warn) = ("not found".into(), true);
                    look.label = format!("{} (not found)", look.name);
                } else if let Some(b) = scan.and_then(|s| s.branch.clone()) {
                    (look.right, look.pill) = (b, true);
                }
                look
            }
            Node::Dir { rel, .. } => Look::new(file_name(rel), String::new(), row.count.to_string()),
            Node::File { link, rel } => {
                let mut look = Look::new(file_name(rel), row.kind.clone(), String::new());
                look.dot = self.changed.contains(&(*link, rel.clone()));
                look
            }
        }
    }
}

/// The small square a hovered row shows at its right end.
fn row_tool(ui: &mut egui::Ui, id: egui::Id, at: egui::Rect, glyph: glyphs::Glyph, name: &str, tip: &str) -> egui::Response {
    let r = ui.interact(at, id, Sense::click());
    glyph_button_on(ui, r, glyph, row_tool_fill, name, tip)
}

/// A bordered button with the play mark, for Run in the preview card.
fn run_chip(ui: &mut egui::Ui) -> egui::Response {
    let g = ui.painter().layout_job(LayoutJob::single_section("Run all".into(), ui_fmt(TXT_BODY, Color32::PLACEHOLDER, W_MEDIUM, 0.0)));
    let (rect, r) = ui.allocate_exact_size(egui::vec2(g.size().x + 38.0, BUTTON_H), Sense::click());
    let state = state_of(&r);
    ui.painter().rect(rect, R_MD, [RAISED(), shade(RAISED(), 0.12), shade(RAISED(), 0.24)][state], Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
    glyphs::play(ui.painter(), egui::pos2(rect.left() + 17.0, rect.center().y), if state > 0 { BRIGHT() } else { MUTED() });
    let dy = optical_dy(&g);
    ui.painter().galley(egui::pos2(rect.left() + 28.0, rect.center().y - g.size().y / 2.0 + dy), g, TEXT());
    focus_ring(ui, &r, rect, R_MD as f32);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Run all"));
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The longest item label a Library menu shows in full, in characters.
const MENU_LABEL_MAX: usize = 36;

/// One item of a Library menu: the interface font, the shortcut faint on the right, and a check at the right end for an
/// option that is on, as the other menus draw them.
fn lib_button(ui: &mut egui::Ui, label: &str, key: &str, checked: bool) -> egui::Response {
    // The menu sizes to its widest item, so a very long name (a folder, a connection) is cut instead of widening it.
    let shown: String = if label.chars().count() > MENU_LABEL_MAX { label.chars().take(MENU_LABEL_MAX - 1).collect::<String>() + "\u{2026}" } else { label.to_string() };
    let mut b = egui::Button::new(shown);
    if !key.is_empty() {
        b = b.shortcut_text(RichText::new(key).color(FAINT()));
    }
    let r = ui.add(b);
    if checked {
        paint_check(ui.painter(), egui::pos2(r.rect.right() - 14.0, r.rect.center().y), TEXT());
    }
    // egui names a button by all its text, shortcut included; the item is named by its label alone.
    let (label, enabled) = (label.trim().to_string(), ui.is_enabled());
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::MenuItem, enabled, checked, label.clone()));
    r
}

fn menu_item(ui: &mut egui::Ui, acts: &mut Vec<Act>, label: &str, key: &str, act: Act) {
    menu_act(ui, acts, act, |ui| lib_button(ui, label, key, false));
}

/// The key that deletes the selected row: Cmd+Backspace on macOS, where Backspace alone is a text key and would delete by
/// accident, and Delete elsewhere.
fn delete_key() -> &'static str {
    if cfg!(target_os = "macos") { "Cmd+Backspace" } else { "Delete" }
}

fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else if cfg!(windows) {
        "Show in Explorer"
    } else {
        "Show in file manager"
    }
}

/// The menu of a row: what Return, Alt+Return, Cmd+Enter and F2 do, plus the rest. A linked folder has its own: new files
/// and folders, a read of the disk, the file manager, which connections use it, and Unlink. A linked file edits like a script
/// kept by Quarry, but on disk: Rename renames the file and Delete moves it to the Trash.
fn node_menu(ui: &mut egui::Ui, cx: &RowCx, node: &Node, kind: &str, acts: &mut Vec<Act>) {
    let (lib, links) = (cx.lib, &cx.lib.links);
    menu_list(ui);
    // A linked folder that is not there takes no new files and has no folder to show: what is left is finding it again,
    // reading it again once the drive is back, or letting it go.
    if let Node::Link(link) = node
        && cx.scans.get(link).is_some_and(|s| s.missing)
    {
        menu_item(ui, acts, "Locate folder\u{2026}", "", Act::Locate(*link));
        menu_item(ui, acts, "Reload from disk", "", Act::Reload(*link));
        ui.separator();
        menu_act(ui, acts, Act::Unlink(*link), |ui| danger_item(ui, "Unlink folder"));
        return;
    }
    let run = kind != "filter";
    let path_of = |link: u64, rel: &Path| links.iter().find(|l| l.id == link).map(|l| if rel.as_os_str().is_empty() { l.path.clone() } else { l.path.join(rel) }).unwrap_or_default();
    match node {
        Node::Script(_) | Node::File { .. } => {
            menu_item(ui, acts, "Open", &super::keys_text(false, false, false, "Enter"), Act::Open(node.clone(), How::Here));
            menu_item(ui, acts, "Open in a new tab", &super::keys_text(false, false, true, "Enter"), Act::Open(node.clone(), How::NewTab));
            if run {
                menu_item(ui, acts, "Run all", &super::keys_text(true, false, false, "Enter"), Act::Open(node.clone(), How::Run));
            }
            // A script kept by Quarry belongs to one connection; this opens it on another of the same engine.
            let on = cx.open_on(node);
            if !on.is_empty() {
                ui.menu_button("Open on", |ui| {
                    menu_list(ui);
                    for (i, (group, conns)) in on.iter().enumerate() {
                        if !group.is_empty() {
                            if i > 0 {
                                ui.separator();
                            }
                            ui.horizontal(|ui| {
                                ui.add_space(10.0);
                                caption(ui, group.to_uppercase());
                            });
                        }
                        for (id, name) in conns {
                            menu_item(ui, acts, name, "", Act::OpenOn(node.clone(), *id));
                        }
                    }
                });
            }
        }
        _ => {}
    }
    if let Node::Folder(id) = node {
        menu_item(ui, acts, "New folder", "", Act::NewFolder(Some(*id)));
    }
    if let Some((link, dir)) = dir_of(node).filter(|_| !matches!(node, Node::File { .. })) {
        menu_item(ui, acts, "New script", "", Act::NewScript(link, dir.clone()));
        menu_item(ui, acts, "New folder", "", Act::NewDir(link, dir.clone()));
        if matches!(node, Node::Link(_)) {
            menu_item(ui, acts, "Reload from disk", "", Act::Reload(link));
        }
        ui.separator();
        menu_item(ui, acts, reveal_label(), "", Act::Reveal(path_of(link, &dir), false));
        menu_item(ui, acts, "Copy path", "", Act::CopyPath(path_of(link, &dir)));
        if matches!(node, Node::Link(_)) {
            menu_item(ui, acts, "Use on connections\u{2026}", "", Act::UseOn(link));
        }
        ui.separator();
        menu_item(ui, acts, "Rename", "F2", Act::Rename(node.clone()));
        if matches!(node, Node::Link(_)) && danger_item(ui, "Unlink folder").clicked() {
            acts.push(Act::Unlink(link));
            ui.close();
        }
    }
    if node.is_saved() || matches!(node, Node::File { .. }) {
        ui.separator();
        menu_item(ui, acts, "Rename", "F2", Act::Rename(node.clone()));
        if let Node::Script(id) = node {
            menu_item(ui, acts, "Duplicate", "", Act::Duplicate(*id));
        }
        let (skip, cur) = match node {
            Node::Folder(id) => (Some(*id), lib.folders.iter().find(|f| f.id == *id).and_then(|f| tree::live_parent(lib, f.parent))),
            Node::Script(id) => (None, lib.scripts.iter().find(|s| s.id == *id).and_then(|s| tree::live_parent(lib, s.folder))),
            _ => (None, None),
        };
        let (label, linked_file) = if matches!(node, Node::File { .. }) { ("Copy to folder", true) } else { ("Move to folder", false) };
        let go = |to: DropAt| if linked_file { Act::Drop(node.clone(), Plan::Out(to)) } else { Act::Move(node.clone(), to) };
        ui.menu_button(label, |ui| {
            menu_list(ui);
            for (id, depth, name) in ops::folder_list(lib, skip) {
                let label = format!("{}{}", "    ".repeat(depth), name);
                menu_act(ui, acts, go(DropAt::Into(id)), |ui| lib_button(ui, &label, "", cur == Some(id)));
            }
            ui.separator();
            if !linked_file {
                menu_item(ui, acts, "New folder\u{2026}", "", Act::MoveToNew(node.clone()));
            }
            if cur.is_some() || linked_file {
                menu_item(ui, acts, if linked_file { "Copy to the top level" } else { "Move out of folders" }, "", go(DropAt::Root));
            }
        });
        match node {
            Node::Script(id) => menu_item(ui, acts, "Copy text", "", Act::Copy(*id)),
            Node::File { link, rel } => {
                menu_item(ui, acts, "Copy text", "", Act::CopyFileText(*link, rel.clone()));
                menu_item(ui, acts, reveal_label(), "", Act::Reveal(path_of(*link, rel), true));
                menu_item(ui, acts, "Copy path", "", Act::CopyPath(path_of(*link, rel)));
            }
            _ => {}
        }
        ui.separator();
        // No ellipsis: it acts at once and the toast offers Undo.
        menu_act(ui, acts, Act::Delete(node.clone()), |ui| danger_item(ui, if linked_file { "Move to Trash" } else { "Delete" }).on_hover_text(delete_key()));
    }
}

/// Every row has a menu.
fn has_menu(_node: &Node) -> bool {
    true
}

impl App {
    /// The body of the sidebar in Library mode.
    pub(in crate::app) fn library_panel(&mut self, ui: &mut egui::Ui) {
        let mut st = std::mem::take(&mut self.lib_ui.panel);
        let mut acts = Vec::new();
        self.library_body(ui, &mut st, &mut acts);
        for a in acts {
            self.apply_lib_act(ui.ctx(), &mut st, a);
        }
        self.lib_ui.panel = st;
    }

    fn library_body(&mut self, ui: &mut egui::Ui, st: &mut PanelUi, acts: &mut Vec<Act>) {
        let ctx = ui.ctx().clone();
        let active = self.lib_conn();
        let all = self.prefs.lib_scope_all || active.is_none();
        let active_name = active.and_then(|id| self.conn(id)).map(|c| c.meta.name.clone());
        let scope_conn = if all { None } else { active };

        // A press outside the panel hands the keys back to the page, and so does a text field taking the keyboard, such as
        // the editor reached with Tab.
        if ctx.input(|i| i.pointer.any_pressed()) && ctx.pointer_interact_pos().is_some_and(|p| !ui.max_rect().contains(p)) {
            st.focus = false;
        }
        // The tree's own rename field is not the page taking the keys: after Enter the arrows walk the tree again.
        if ctx.text_edit_focused() && st.rename.is_none() {
            st.focus = false;
        }

        // Header
        let new_parent = match &st.sel {
            Some(Node::Folder(id)) => Some(*id),
            Some(Node::Script(id)) => self.library.scripts.iter().find(|s| s.id == *id).and_then(|s| tree::live_parent(&self.library, s.folder)),
            _ => None,
        };
        ui.horizontal(|ui| {
            ui.label(LayoutJob::single_section("LIBRARY".into(), ui_fmt(TXT_SECTION, MUTED(), W_SEMIBOLD, 0.6)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let r = dots_menu(ui, |ui| {
                    menu_list(ui);
                    menu_item(ui, acts, "Link a folder\u{2026}", "", Act::Link);
                    menu_item(ui, acts, "Fold all", "", Act::FoldAll);
                    menu_act(ui, acts, Act::Scope(!all), |ui| lib_button(ui, "Show all connections", "", all));
                });
                r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Library menu"));
                if glyph_button(ui, egui::vec2(28.0, 24.0), glyphs::folder_plus, "New folder", "New folder").clicked() {
                    acts.push(Act::NewFolder(new_parent));
                }
            });
        });
        ui.add_space(6.0);

        // Scope
        let mut scope_all = all;
        // The design spans the panel in two equal halves; a long name is cut to its half by the control.
        let label = active_name.clone().unwrap_or_else(|| "No connection".into());
        segmented_fill(ui, &mut scope_all, &[(false, label.as_str(), active.is_some()), (true, "All connections", true)]);
        if scope_all != all {
            acts.push(Act::Scope(scope_all));
        }
        ui.add_space(8.0);

        // Search
        let r = ui.add(
            egui::TextEdit::singleline(&mut st.query)
                .id(egui::Id::unique("library-search"))
                .hint_text(RichText::new("Search scripts").color(FAINT()))
                .desired_width(f32::INFINITY)
                .margin(egui::Margin { left: 28, right: 10, top: 5, bottom: 5 }),
        );
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Search scripts"));
        paint_search(ui.painter(), egui::pos2(r.rect.left() + 14.0, r.rect.center().y), if r.has_focus() { MUTED() } else { FAINT() });
        if r.has_focus() {
            st.focus = false;
        }
        ui.add_space(10.0);

        let engine = scope_conn.and_then(|c| self.conn(c)).map(|c| c.meta.engine);
        let rows = tree::flatten(&self.library, &self.lib_ui.scans, scope_conn, engine, &st.query, &st.open);
        // The folders still moving and how far open each is. A search shows its folders open at once.
        let searching = !st.query.trim().is_empty();
        let mut folds: HashMap<Node, f32> = HashMap::new();
        st.folding.retain(|n| {
            let open = st.open.contains(n);
            let t = fold::openness(&ctx, fold_id(n), open);
            let moving = !searching && if open { t < 1.0 } else { t > 0.0 };
            if moving {
                folds.insert(n.clone(), t);
            }
            moving
        });
        // A closing folder keeps its children on screen until it has closed. They are drawn from a list of their own, so
        // the keys, the selection and the drops only ever see the folder closed.
        let closing: Vec<Node> = folds.keys().filter(|n| !st.open.contains(*n)).cloned().collect();
        let drawn_closing = (!closing.is_empty()).then(|| {
            let mut open = st.open.clone();
            open.extend(closing.iter().cloned());
            let mut v = tree::flatten(&self.library, &self.lib_ui.scans, scope_conn, engine, &st.query, &open);
            for r in &mut v {
                r.open &= st.open.contains(&r.node);
            }
            v
        });
        let drawn: &[Row] = drawn_closing.as_deref().unwrap_or(&rows);
        let live: Option<HashSet<&Node>> = drawn_closing.as_ref().map(|_| rows.iter().map(|r| &r.node).collect());
        let laid = (!folds.is_empty()).then(|| tree::lay_out(drawn, ROW_H, |n| folds.get(n).copied()));
        if rows.is_empty() && st.query.trim().is_empty() {
            self.library_empty(ui, active, active_name.as_deref(), all, acts);
            return;
        }
        if rows.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new(format!("No scripts match \"{}\".", st.query.trim())).size(TXT_SMALL).color(MUTED()));
            return;
        }

        st.badge = st.badge_next.take();
        if st.sel.as_ref().is_some_and(|n| !rows.iter().any(|r| r.node == *n)) && st.rename.is_none() {
            st.sel = None;
        }
        // Cmd+Shift+L: the tree takes the keys, on its first row when none is selected.
        if std::mem::take(&mut self.lib_ui.focus_tree) {
            ctx.memory_mut(|m| {
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
            st.focus = true;
            if st.sel.is_none() {
                st.sel = rows.first().map(|r| r.node.clone());
            }
        }
        if !st.query.trim().is_empty() {
            let hits = rows.iter().filter(|r| r.hit).count();
            ui.label(RichText::new(format!("{hits} match{}", if hits == 1 { "" } else { "es" })).size(TXT_CAPTION).color(FAINT()));
            ui.add_space(4.0);
        }
        self.library_keys(&ctx, st, &rows, acts);

        // Tree, splitter and preview share what is left of the panel's height.
        let avail = ui.available_height();
        let show_preview = st.sel.as_ref().is_some_and(|n| n.script_ref().is_some()) && avail > PREVIEW_MIN + TREE_MIN + 12.0;
        let want = if self.prefs.lib_preview_h > 0.0 { self.prefs.lib_preview_h } else { PREVIEW_DEFAULT };
        let preview_h = want.clamp(PREVIEW_MIN, (avail - TREE_MIN - 6.0).max(PREVIEW_MIN));
        let tree_h = if show_preview { avail - preview_h - 6.0 } else { avail };

        // The demo drag moves out of `st` for the frame, so the rows can read it while `st` is borrowed for state.
        let demo_drag = st.demo_drag.clone();
        let cx = RowCx {
            lib: &self.library,
            conns: &self.conns,
            scans: &self.lib_ui.scans,
            changed: &self.lib_ui.changed,
            active,
            groups: &self.groups,
            query: st.query.trim().to_string(),
            // Esc drops the payload; drag_cancelled keeps the marks off until the button comes up.
            drag: egui::DragAndDrop::payload::<DragItem>(&ctx).filter(|_| !drag_cancelled(&ctx)).and_then(|d| item_node(&d)),
            demo_drag: demo_drag.as_ref(),
            folds: &folds,
        };
        let total = drawn.len();
        let height = laid.as_ref().map_or(total as f32 * ROW_H, |l| l.height);
        let mut area = egui::ScrollArea::vertical().id_salt("library-tree").auto_shrink([false, false]).max_height(tree_h);
        if let Some(y) = st.scroll_set.take() {
            area = area.vertical_scroll_offset(y);
        }
        let out = ui
            .scope(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                area.show_viewport(ui, |ui, view| {
                    auto_scroll_while_dragging(ui);
                    ui.set_height(height);
                    let origin = ui.max_rect().min;
                    let w = ui.max_rect().width();
                    // Only the rows in view are drawn, which keeps a linked folder of thousands of files cheap. At rest
                    // a row's place is its index; while folders move, each row has its place and the line that hides it.
                    let shown: Vec<usize> = match &laid {
                        None => {
                            let first = ((view.min.y / ROW_H).floor().max(0.0) as usize).min(total);
                            (first..((view.max.y / ROW_H).ceil() as usize + 1).min(total)).collect()
                        }
                        Some(l) => (0..total).filter(|&i| l.top[i] < view.max.y && l.top[i] + ROW_H > view.min.y && l.clip[i] > l.top[i]).collect(),
                    };
                    for i in shown {
                        let (top, cut, fade) = laid.as_ref().map_or((i as f32 * ROW_H, f32::INFINITY, 1.0), |l| (l.top[i], l.clip[i], l.fade[i]));
                        let rect = egui::Rect::from_min_size(origin + egui::vec2(0.0, top), egui::vec2(w, ROW_H));
                        let mut row_ui = ui.new_child(egui::UiBuilder::new().id_salt(&drawn[i].node).max_rect(rect).layout(*ui.layout()));
                        if cut.is_finite() {
                            let mut clip = row_ui.clip_rect();
                            clip.max.y = clip.max.y.min(origin.y + cut);
                            row_ui.set_clip_rect(clip);
                            row_ui.multiply_opacity(fade);
                        }
                        // A child of a closing folder is only drawn; it is gone from the tree already.
                        if live.as_ref().is_some_and(|l| !l.contains(&drawn[i].node)) {
                            let o = row_ui.opacity();
                            row_ui.disable();
                            row_ui.set_opacity(o);
                        }
                        draw_row(&mut row_ui, &cx, st, &drawn[i], acts);
                    }
                })
            })
            .inner;
        st.scroll_y = out.state.clamped_offset().y;
        st.view_h = out.inner_rect.height();

        // The space below the last row takes drops that move an item out of its folder.
        let used = (height - st.scroll_y).max(0.0);
        let zone = egui::Rect::from_min_max(egui::pos2(out.inner_rect.left(), out.inner_rect.top() + used), out.inner_rect.right_bottom());
        if zone.height() > 6.0 && cx.drag.is_some() {
            let r = ui.interact(zone, egui::Id::unique("library-root-drop"), Sense::hover());
            if let (Some(p), Some(item)) = (r.dnd_hover_payload::<DragItem>(), cx.drag.as_ref()) {
                let _ = p;
                if root_plan(&self.library, item).is_ok() {
                    paint_spot(ui.painter(), zone.with_max_y(zone.top() + 2.0), Spot { zone: Zone::Before, depth: 0 }, zone.left() + 8.0, INDENT);
                }
            }
            if let (Some(_), Some(item)) = (r.dnd_release_payload::<DragItem>(), cx.drag.clone()) {
                acts.push(match root_plan(&self.library, &item) {
                    Ok(Plan::Saved(a)) => Act::Move(item, a),
                    Ok(plan) => Act::Drop(item, plan),
                    Err(e) => Act::Refuse(e),
                });
            }
        }

        if show_preview {
            let mut pref = if self.prefs.lib_preview_h > 0.0 { avail - preview_h } else { 0.0 };
            let r = splitter(ui);
            if resize_editor(&r, &mut pref, out.inner_rect.height(), TREE_MIN, avail - PREVIEW_MIN - 6.0) {
                self.prefs.lib_preview_h = if pref > 0.0 { avail - pref } else { 0.0 };
                self.save_prefs();
            }
            // A drag in progress moves the card at once; the preference is saved when it ends.
            if r.dragged() && pref > 0.0 {
                self.prefs.lib_preview_h = avail - pref;
            }
            if let Some(sel) = st.sel.clone() {
                self.library_preview(ui, &sel, st, all, acts);
            }
        }
    }

    fn library_empty(&mut self, ui: &mut egui::Ui, active: Option<u64>, name: Option<&str>, all: bool, acts: &mut Vec<Act>) {
        let others = self.library.scripts.iter().filter(|s| active.is_some_and(|a| a != s.conn)).count();
        let title = match (name, all) {
            (Some(n), false) => format!("No saved scripts on {n}"),
            _ => "No saved scripts".to_string(),
        };
        // Saving what is on screen is the likely first step, so it leads when there is something to save.
        let can_save = self.tab_text().is_some_and(|t| !t.body.trim().is_empty());
        let hint = format!("Press {} in a query to keep it here, or link a folder of scripts from a repo.", super::keys_text(true, false, false, "S"));
        empty_state(ui, paint_book, &title, &hint, |ui| {
            ui.add_space(10.0);
            if ui.add_enabled_ui(can_save, |ui| primary(ui, "Save current query", PRIMARY())).inner.clicked() {
                acts.push(Act::SaveCurrent);
            }
            ui.add_space(2.0);
            if quiet(ui, "Link a folder\u{2026}").clicked() {
                acts.push(Act::Link);
            }
            ui.add_space(2.0);
            if quiet(ui, "New folder").clicked() {
                acts.push(Act::NewFolder(None));
            }
            if others > 0 && !all {
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(6.0);
                ui.label(RichText::new(format!("{others} script{} on other connections.", if others == 1 { "" } else { "s" })).size(TXT_SMALL).color(MUTED()));
                if quiet(ui, "Show all connections").clicked() {
                    acts.push(Act::Scope(true));
                }
            }
        });
    }

    /// Return, Alt+Return, Cmd+Enter, F2, Delete and the arrows, while a row has been clicked and no text box has the keys.
    /// A row that has the keyboard focus (reached with Tab) still lets them through: only a text field takes them.
    fn library_keys(&mut self, ctx: &egui::Context, st: &mut PanelUi, rows: &[Row], acts: &mut Vec<Act>) {
        if !st.focus || self.window_open() || st.rename.is_some() || ctx.text_edit_focused() {
            return;
        }
        let none = egui::Modifiers::NONE;
        let k = |m: egui::Modifiers, key: Key| ctx.input_mut(|i| i.consume_key(m, key));
        let at = st.sel.as_ref().and_then(|n| rows.iter().position(|r| r.node == *n));
        let kind = |n: &Node| match n {
            Node::Script(id) => self.library.scripts.iter().find(|s| s.id == *id).map(|s| s.kind.clone()).unwrap_or_default(),
            Node::File { link, rel } => self.file_kind(*link, rel),
            _ => String::new(),
        };
        let go = |st: &mut PanelUi, i: usize| {
            st.sel = Some(rows[i].node.clone());
            let y = i as f32 * ROW_H;
            if y < st.scroll_y {
                st.scroll_set = Some(y);
            } else if y + ROW_H > st.scroll_y + st.view_h {
                st.scroll_set = Some(y + ROW_H - st.view_h);
            }
        };
        // The longer combinations go first: egui lets a plain key match one with extra modifiers.
        if let Some(n) = st.sel.clone() {
            if k(egui::Modifiers::COMMAND, Key::Enter) && n.script_ref().is_some() && kind(&n) != "filter" {
                acts.push(Act::Open(n.clone(), How::Run));
            } else if k(egui::Modifiers::ALT, Key::Enter) && n.script_ref().is_some() {
                acts.push(Act::Open(n.clone(), How::NewTab));
            } else if k(none, Key::Enter) {
                acts.push(if n.script_ref().is_some() { Act::Open(n.clone(), How::Here) } else { Act::Toggle(n.clone()) });
            }
            if k(none, Key::F2) {
                acts.push(Act::Rename(n.clone()));
            }
            if (k(egui::Modifiers::COMMAND, Key::Backspace) || k(none, Key::Delete)) && (n.is_saved() || matches!(n, Node::File { .. })) {
                acts.push(Act::Delete(n.clone()));
            }
            let open = at.is_some_and(|i| rows[i].open);
            if k(none, Key::ArrowRight) && n.is_container() && !open {
                acts.push(Act::Toggle(n.clone()));
            }
            if k(none, Key::ArrowLeft) {
                if n.is_container() && open && st.query.trim().is_empty() {
                    acts.push(Act::Toggle(n.clone()));
                } else if let Some(i) = at {
                    // Up to the row that holds this one.
                    if let Some(p) = rows[..i].iter().rposition(|r| r.depth < rows[i].depth) {
                        go(st, p);
                    }
                }
            }
        }
        if k(none, Key::ArrowDown) {
            match at {
                Some(i) if i + 1 < rows.len() => go(st, i + 1),
                None if !rows.is_empty() => go(st, 0),
                _ => {}
            }
        }
        if k(none, Key::ArrowUp)
            && let Some(i) = at.filter(|i| *i > 0)
        {
            go(st, i - 1);
        }
    }

    fn library_preview(&mut self, ui: &mut egui::Ui, node: &Node, st: &mut PanelUi, all: bool, acts: &mut Vec<Act>) {
        // What the card shows: a script's name, where it lives and when it was edited, its kind and the start of its text.
        let (name, sub, kind, text) = match node.script_ref() {
            Some(ScriptRef::Saved(id)) => {
                let Some(s) = self.library.scripts.iter().find(|s| s.id == id).cloned() else { return };
                let place = {
                    let p = ops::folder_path(&self.library, s.folder);
                    let p = if p.is_empty() { "Library".to_string() } else { p };
                    if all { format!("{} · {p}", self.conns.iter().find(|c| c.meta.id == s.conn).map_or("removed connection", |c| c.meta.name.as_str())) } else { p }
                };
                let when = edited(s.updated);
                (s.name.clone(), if when.is_empty() { place } else { format!("{place}  ·  {when}") }, s.kind.clone(), s.body.chars().take(1500).collect::<String>())
            }
            // A linked file shows what its scan kept of it, so the card never reads the disk while it is drawn.
            Some(ScriptRef::File { link, rel }) => {
                let Some(e) = self.lib_ui.scans.get(&link).and_then(|s| s.entries.iter().find(|e| e.rel == rel)) else { return };
                let l = self.library.links.iter().find(|l| l.id == link);
                let folder = l.map(|l| if l.name.is_empty() { l.path.display().to_string() } else { l.name.clone() }).unwrap_or_default();
                let place = match rel.parent().filter(|p| !p.as_os_str().is_empty()) {
                    Some(p) => format!("{folder} / {}", p.display()),
                    None => folder,
                };
                let secs = e.mtime.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                let when = edited(secs);
                (rel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), if when.is_empty() { place } else { format!("{place}  ·  {when}") }, e.kind.clone(), e.head.clone())
            }
            None => return,
        };
        let runs_on = node.script_ref().map(|r| self.runs_on(&r)).unwrap_or_default();
        let avail = ui.available_height();
        egui::Frame::new().fill(RAISED()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height((avail - 20.0).max(0.0));
            let w = ui.available_width();
            let mut head = LayoutJob::single_section(name.clone(), ui_fmt(TXT_TITLE, BRIGHT(), W_MEDIUM, 0.0));
            head.wrap = egui::text::TextWrapping::truncate_at_width(w);
            ui.label(head);
            let mut subj = LayoutJob::single_section(sub.clone(), ui_fmt(TXT_CAPTION, FAINT(), 400.0, 0.0));
            subj.wrap = egui::text::TextWrapping::truncate_at_width(w);
            ui.label(subj);
            // Where Open and Run all would land, so a script never runs on a connection the person did not expect.
            let mut on = LayoutJob::single_section(runs_on.clone(), ui_fmt(TXT_CAPTION, MUTED(), 400.0, 0.0));
            on.wrap = egui::text::TextWrapping::truncate_at_width(w);
            ui.label(on);
            ui.add_space(6.0);
            // As many lines as the card has room for between the header and the buttons.
            let lines = (((avail - 112.0) / 15.0) as usize).clamp(2, 8);
            let job = match kind.as_str() {
                "shell" => js_job(&text),
                "filter" | "pipeline" => json_job(&text),
                _ => sql_job(&text, TXT_SMALL),
            };
            for line in split_lines(&job).into_iter().take(lines) {
                ui.add(egui::Label::new(line).truncate());
            }
            ui.add_space((ui.available_height() - BUTTON_H - 4.0).max(0.0));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if primary(ui, "Open", PRIMARY()).clicked() {
                    acts.push(Act::Open(node.clone(), How::Here));
                }
                if kind != "filter" && run_chip(ui).on_hover_text(&runs_on).clicked() {
                    acts.push(Act::Open(node.clone(), How::Run));
                }
                if quiet(ui, "New tab").clicked() {
                    acts.push(Act::Open(node.clone(), How::NewTab));
                }
            });
        });
        let _ = st;
    }

    fn apply_lib_act(&mut self, ctx: &egui::Context, st: &mut PanelUi, act: Act) {
        match act {
            Act::Open(n, how) => {
                if let Some(r) = n.script_ref() {
                    self.open_script(&r, how);
                }
            }
            Act::OpenOn(n, conn) => {
                if let Some(r) = n.script_ref() {
                    self.open_script_on(&r, How::Here, conn);
                }
            }
            Act::SaveCurrent => self.save_script(),
            Act::Toggle(n) => {
                let open = !st.open.remove(&n);
                if open {
                    st.open.insert(n.clone());
                }
                fold::start(ctx, fold_id(&n), open);
                st.folding.insert(n);
            }
            Act::Rename(n) => {
                let name = match &n {
                    Node::Folder(id) => self.library.folders.iter().find(|f| f.id == *id).map(|f| f.name.clone()),
                    Node::Script(id) => self.library.scripts.iter().find(|s| s.id == *id).map(|s| s.name.clone()),
                    Node::Link(id) => self.library.links.iter().find(|l| l.id == *id).map(|l| if l.name.is_empty() { l.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() } else { l.name.clone() }),
                    Node::Dir { rel, .. } => rel.file_name().map(|n| n.to_string_lossy().into_owned()),
                    Node::File { rel, .. } => rel.file_stem().map(|n| n.to_string_lossy().into_owned()),
                };
                if let Some(name) = name {
                    st.sel = Some(n.clone());
                    st.rename = Some((n, name, true));
                }
            }
            Act::Commit(n, text) => match &n {
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
            },
            Act::Duplicate(id) => {
                if let Some(new) = ops::duplicate(&mut self.library, id, crate::library::now_secs()) {
                    self.save_library();
                    st.sel = Some(Node::Script(new));
                }
            }
            Act::Move(n, at) => match ops::move_to(&mut self.library, &n, at.clone()) {
                Ok(()) => {
                    if let DropAt::Into(f) = at {
                        st.open.insert(Node::Folder(f));
                    }
                    self.save_library();
                }
                Err(e) => self.warn(e),
            },
            Act::MoveToNew(n) => {
                let id = ops::new_folder(&mut self.library, None, "");
                if ops::move_to(&mut self.library, &n, DropAt::Into(id)).is_ok() {
                    st.open.insert(Node::Folder(id));
                    st.query.clear();
                    st.sel = Some(Node::Folder(id));
                    let name = self.library.folders.iter().find(|f| f.id == id).map(|f| f.name.clone()).unwrap_or_default();
                    st.rename = Some((Node::Folder(id), name, true));
                }
                self.save_library();
            }
            Act::Copy(id) => {
                if let Some(s) = self.library.scripts.iter().find(|s| s.id == id) {
                    ctx.copy_text(s.body.clone());
                    self.ok("Script text copied.");
                }
            }
            Act::Delete(Node::File { link, rel }) => {
                if self.delete_linked_file(link, &rel) {
                    st.sel = None;
                }
            }
            Act::Delete(n) => {
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
                    if st.sel.as_ref().is_some_and(|s| *s == n || matches!(s, Node::Script(id) if scripts.iter().any(|x| x.id == *id)) || matches!(s, Node::Folder(id) if folders.iter().any(|f| f.id == *id))) {
                        st.sel = None;
                    }
                    self.toast_restore(msg, Restore::Saved { scripts, folders, written: None });
                }
            }
            Act::NewFolder(parent) => {
                let id = ops::new_folder(&mut self.library, parent, "");
                if let Some(p) = parent {
                    st.open.insert(Node::Folder(p));
                }
                st.query.clear();
                self.save_library();
                let name = self.library.folders.iter().find(|f| f.id == id).map(|f| f.name.clone()).unwrap_or_default();
                st.sel = Some(Node::Folder(id));
                st.rename = Some((Node::Folder(id), name, true));
            }
            Act::FoldAll => {
                for n in st.open.drain() {
                    fold::start(ctx, fold_id(&n), false);
                    st.folding.insert(n);
                }
            }
            Act::Scope(all) => {
                self.prefs.lib_scope_all = all;
                self.save_prefs();
            }
            Act::Link => self.open_link_dialog(None),
            Act::Refuse(why) => self.warn(why),
            Act::NewScript(link, dir) => {
                if let Some(rel) = self.new_linked_script(link, &dir) {
                    self.show_linked(st, link, &dir);
                    let node = Node::File { link, rel: rel.clone() };
                    let stem = rel.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    st.query.clear();
                    st.sel = Some(node.clone());
                    st.rename = Some((node, stem, true));
                }
            }
            Act::NewDir(link, dir) => {
                if let Some(rel) = self.new_linked_dir(link, &dir) {
                    self.show_linked(st, link, &dir);
                    let node = Node::Dir { link, rel: rel.clone() };
                    let name = rel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    st.query.clear();
                    st.sel = Some(node.clone());
                    st.rename = Some((node, name, true));
                }
            }
            Act::Reload(link) => {
                self.rescan_link(link);
                self.ok("Reading the folder again.");
            }
            Act::Reveal(path, select) => {
                if let Err(e) = super::linked::reveal(&path, select) {
                    self.err(format!("Could not open {}: {e}", path.display()));
                }
            }
            Act::CopyPath(path) => {
                ctx.copy_text(path.to_string_lossy().into_owned());
                self.ok("Path copied.");
            }
            Act::CopyFileText(link, rel) => match self.library.file_path(link, &rel).map(|p| super::linked::read_script(&p)) {
                Some(Ok(l)) => {
                    ctx.copy_text(l.text);
                    self.ok("Script text copied.");
                }
                Some(Err(e)) => self.warn(format!("Could not read the file: {e}")),
                None => {}
            },
            Act::UseOn(link) => self.open_link_dialog(Some(link)),
            Act::Locate(link) => self.open_locate_dialog(link),
            Act::Unlink(link) => {
                self.unlink(link);
                st.open.retain(|n| !matches!(n, Node::Link(l) | Node::Dir { link: l, .. } | Node::File { link: l, .. } if *l == link));
                if st.sel.as_ref().is_some_and(|n| matches!(n, Node::Link(l) | Node::Dir { link: l, .. } | Node::File { link: l, .. } if *l == link)) {
                    st.sel = None;
                }
            }
            Act::Drop(item, plan) => match (item, plan) {
                (Node::Script(id), Plan::Into { link, dir }) => {
                    if let Some(rel) = self.script_to_link(id, link, &dir) {
                        self.show_linked(st, link, &dir);
                        st.sel = Some(Node::File { link, rel });
                    }
                }
                (Node::File { link, rel }, Plan::Into { dir, .. }) => {
                    if let Some(new) = self.move_linked_file(link, &rel, &dir) {
                        self.show_linked(st, link, &dir);
                        st.sel = Some(Node::File { link, rel: new });
                    }
                }
                // The copy is selected, and its folder opened, once the file is read.
                (Node::File { link, rel }, Plan::Out(at)) => self.file_to_quarry(link, &rel, at),
                _ => {}
            },
        }
    }

    /// Opens a linked folder and the folders above `dir` in it, so a file just made or moved there shows.
    fn show_linked(&mut self, st: &mut PanelUi, link: u64, dir: &Path) {
        st.open.insert(Node::Link(link));
        let mut at = PathBuf::new();
        for c in dir.components() {
            at.push(c);
            st.open.insert(Node::Dir { link, rel: at.clone() });
        }
    }

    /// QUARRY_TEST_LIBRARY=<state>: panel, all, search, empty, menu, folder-menu, drag (here); linked, missing, link,
    /// save-linked, conflict (linked.rs); save, picker, picker-docs, picker-shell (picker.rs).
    pub(in crate::app) fn demo_library(&mut self, state: &str) {
        if matches!(state, "panel" | "all" | "search" | "empty" | "menu" | "folder-menu" | "drag") {
            self.demo_library_panel(state);
        }
        if !self.demo_library_linked(state) {
            self.demo_library_quick(state);
        }
    }

    /// The panel states. The library is made up here: reports kept in folders on the MySQL "shop-prod" connection, and the
    /// Mongo kinds on "Local sample", so the All connections scope shows every glyph and a removed connection.
    fn demo_library_panel(&mut self, state: &str) {
        use crate::library::{Folder, Script};
        if self.conn(3).is_none() {
            self.conns.push(Conn { masked_uri: "mysql://app:****@shop.internal:3306".into(), ..Conn::new(ConnMeta { id: 3, name: "shop-prod".into(), tag: "Production".into(), engine: db::sql::Engine::MySql, ..Default::default() }, Status::Ready) });
        }
        self.active = Some(3);
        self.prefs.set_side_mode(SideMode::Library);
        self.prefs.sidebar_hidden = false;
        self.prefs.lib_scope_all = state == "all";
        let now = crate::library::now_secs();
        let folder = |id, name: &str, parent| Folder { id, name: name.into(), parent };
        let script = |id, name: &str, kind: &str, conn, folder, db: &str, coll: &str, body: &str, age: u64| Script { id, name: name.into(), kind: kind.into(), body: body.into(), conn, db: db.into(), coll: coll.into(), folder, updated: now - age, ..Default::default() };
        let mut lib = Library {
            folders: vec![folder(1, "Reports", None), folder(2, "Weekly", Some(1)), folder(3, "Maintenance", None), folder(4, "Scratch", None)],
            ..Default::default()
        };
        if state != "empty" {
            lib.scripts = vec![
                script(10, "Monthly revenue", "sql", 3, Some(1), "shop", "", "-- revenue per month, paid orders only\nSELECT DATE_FORMAT(created_at, '%Y-%m') AS month,\n       SUM(total) AS revenue,\n       COUNT(*) AS orders\nFROM orders\nWHERE status = 'paid'\nGROUP BY month\nORDER BY month DESC\nLIMIT 12;", 5 * 60),
                script(11, "Top customers", "sql", 3, Some(1), "shop", "", "SELECT customer_id, SUM(total) AS spent\nFROM orders\nGROUP BY customer_id\nORDER BY spent DESC\nLIMIT 20;", 3 * 3600),
                script(12, "Orders per day", "sql", 3, Some(2), "shop", "", "SELECT DATE(created_at) AS day, COUNT(*) FROM orders GROUP BY day;", 86400),
                script(13, "Stale carts", "sql", 3, Some(3), "shop", "", "DELETE FROM carts WHERE updated_at < NOW() - INTERVAL 30 DAY;", 4 * 86400),
                script(14, "Slow orders lookup", "sql", 3, None, "shop", "", "EXPLAIN SELECT * FROM orders WHERE customer_id = 42;", 9 * 3600),
                script(20, "Paid orders", "filter", 1, None, "shop", "orders", "{ status: 'paid', total: { $gt: 100 } }", 2 * 86400),
                script(21, "Revenue by status", "pipeline", 1, Some(1), "shop", "orders", "[\n  { $group: { _id: '$status', total: { $sum: '$total' } } },\n  { $sort: { total: -1 } }\n]", 86400),
                script(22, "Backfill totals", "shell", 1, Some(3), "", "", "const n = await orders.countDocuments({ total: null });\nconsole.log('missing totals:', n);", 7 * 86400),
                script(30, "Legacy export", "sql", 99, None, "old", "", "SELECT * FROM export_log;", 30 * 86400),
            ];
        } else {
            lib.scripts = vec![script(20, "Paid orders", "filter", 1, None, "shop", "orders", "{ status: 'paid' }", 86400), script(21, "Revenue by status", "pipeline", 1, None, "shop", "orders", "[]", 86400), script(22, "Backfill totals", "shell", 1, None, "", "", "console.log(1)", 86400)];
            lib.folders.clear();
        }
        self.library = lib;
        let st = &mut self.lib_ui.panel;
        st.open = [1, 2].map(Node::Folder).into_iter().collect();
        match state {
            "search" => st.query = "orders".into(),
            "empty" => {}
            _ => st.sel = Some(Node::Script(10)),
        }
        if state == "all" {
            st.open.insert(Node::Folder(3));
        }
        match state {
            "menu" => st.demo_menu = Some(Node::Script(10)),
            "folder-menu" => st.demo_menu = Some(Node::Folder(1)),
            "drag" => st.demo_drag = Some((Node::Script(14), Node::Folder(3))),
            _ => {}
        }
    }
}

/// Draws one row and gathers what the person does to it.
fn draw_row(ui: &mut egui::Ui, cx: &RowCx, st: &mut PanelUi, row: &Row, acts: &mut Vec<Act>) {
    ui.push_id(&row.node, |ui| draw_row_in(ui, cx, st, row, acts));
}

fn draw_row_in(ui: &mut egui::Ui, cx: &RowCx, st: &mut PanelUi, row: &Row, acts: &mut Vec<Act>) {
    let ctx = ui.ctx().clone();
    let look = cx.look(row);
    let indent = INDENT * row.depth as f32;
    // How far the chevron has turned: a moving folder's openness, else its state.
    let turn = cx.folds.get(&row.node).copied().unwrap_or(if row.open { 1.0 } else { 0.0 });
    let (glyph, gcol): (glyphs::Glyph, Color32) = match &row.node {
        Node::Folder(_) | Node::Dir { .. } => (if row.open { glyphs::folder_open } else { glyphs::folder }, MUTED()),
        Node::Link(_) => (glyphs::link, if look.warn { ALERT() } else { C_LINK() }),
        Node::Script(_) | Node::File { .. } => kind_glyph(&look.kind),
    };

    if let Some((n, buf, fresh)) = st.rename.as_mut().filter(|(n, _, _)| *n == row.node) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::hover());
        let x = rect.left() + 8.0 + indent;
        if n.is_container() {
            fold::chevron(ui.painter(), egui::pos2(x + 7.0, rect.center().y), turn, FAINT());
        }
        glyph(ui.painter(), egui::pos2(x + 22.0, rect.center().y), gcol);
        let er = egui::Rect::from_min_max(egui::pos2(x + 33.0, rect.top() + 2.0), egui::pos2(rect.right() - 8.0, rect.bottom() - 2.0));
        let id = egui::Id::unique("library-rename");
        let te = put_at(ui, er, |ui| ui.add(egui::TextEdit::singleline(buf).id(id).desired_width(er.width()).margin(egui::Margin::symmetric(6, 3))));
        te.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Rename"));
        if *fresh {
            te.request_focus();
            if let Some(mut s) = egui::TextEdit::load_state(&ctx, id) {
                s.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(buf.chars().count()))));
                s.store(&ctx, id);
            }
            *fresh = false;
        } else if te.lost_focus() {
            // Escape also ends the edit, and it must not keep the typed name.
            let text = std::mem::take(buf);
            let node = n.clone();
            let esc = ctx.input(|i| i.key_pressed(Key::Escape));
            st.rename = None;
            if !esc {
                acts.push(Act::Commit(node, text));
            }
        }
        return;
    }

    let dragging_demo = cx.demo_drag.is_some_and(|(src, _)| *src == row.node);
    let dim = cx.drag.as_ref() == Some(&row.node) || dragging_demo;
    let hovered = cx.drag.is_none() && next_row_hovered(ui, ROW_H);
    let selected = st.sel.as_ref() == Some(&row.node);
    let dots_id = egui::Id::unique(("lib-dots", &row.node));
    let menu_open = egui::Popup::is_id_open(&ctx, dots_id.with("popup")) || st.demo_menu.as_ref() == Some(&row.node);
    let show_tools = (hovered || menu_open) && has_menu(&row.node);
    let is_container = row.node.is_container();
    let dir_like = matches!(row.node, Node::Link(_) | Node::Dir { .. });

    let r = side_row_drag(ui, ROW_H, indent, selected, &look.label, |p, inner| {
        let cy = inner.center().y;
        for k in 0..row.depth {
            let gx = inner.left() - indent + INDENT * k as f32 + 7.0;
            p.vline(gx, inner.y_range(), Stroke::new(1.0, LINE()));
        }
        let x = inner.left();
        if is_container {
            fold::chevron(p, egui::pos2(x + 7.0, cy), turn, if hovered { MUTED() } else { FAINT() });
        }
        glyph(p, egui::pos2(x + 22.0, cy), gcol);
        let tools_w = if show_tools { 52.0 } else { 0.0 };
        // A hit found only in the script's text says so, since nothing in its name shows why it is listed.
        let in_text = row.hit && !cx.query.is_empty() && matches!(row.node, Node::Script(_) | Node::File { .. }) && crate::ui::widgets::find_ci(&look.name, &cx.query).is_none();
        let right = if show_tools { String::new() } else if in_text { "in text".to_string() } else { look.right.clone() };
        let right_col = if look.warn { ALERT() } else if look.pill { C_LINK() } else { FAINT() };
        let rg = (!right.is_empty()).then(|| p.layout_job(LayoutJob::single_section(right, mono_fmt(TXT_SECTION, right_col, 400.0))));
        // The pill and the changed dot take a little more room than the bare text.
        let mut rw = rg.as_ref().map_or(0.0, |g| g.size().x + 8.0 + if look.pill { 10.0 } else { 0.0 });
        if let Some(g) = rg {
            let dy = optical_dy(&g);
            if look.pill {
                let pill = egui::Rect::from_center_size(egui::pos2(inner.right() - g.size().x / 2.0 - 4.0, cy), egui::vec2(g.size().x + 10.0, 16.0));
                p.rect_filled(pill, R_SM, tint(C_LINK(), 30));
            }
            let shift = if look.pill { 4.0 } else { 0.0 };
            p.galley(egui::pos2(inner.right() - g.size().x - shift, cy - g.size().y / 2.0 + dy), g, right_col);
        }
        if look.dot && !show_tools {
            p.circle_filled(egui::pos2(inner.right() - 4.0, cy), 3.0, WARNING());
            rw += 12.0;
        }
        let weight = if is_container { W_MEDIUM } else { 400.0 };
        let color = if selected || hovered { BRIGHT() } else { TEXT() };
        // The part of the name the search matched is marked, as in the Connections filter.
        let mut job = LayoutJob::default();
        let plain = ui_fmt(TXT_BODY, color, weight, 0.0);
        match crate::ui::widgets::find_ci(&look.name, &cx.query).filter(|_| row.hit) {
            Some((a, b)) => {
                job.append(&look.name[..a], 0.0, plain.clone());
                job.append(&look.name[a..b], 0.0, egui::TextFormat { background: HIGHLIGHT(), ..plain.clone() });
                job.append(&look.name[b..], 0.0, plain);
            }
            None => job.append(&look.name, 0.0, plain),
        }
        job.wrap = egui::text::TextWrapping::truncate_at_width((inner.right() - rw - tools_w - (x + 33.0)).max(20.0));
        let g = p.layout_job(job);
        let dy = optical_dy(&g);
        p.galley(egui::pos2(x + 33.0, cy - g.size().y / 2.0 + dy), g, color);
    });
    let rect = r.rect;
    if dim {
        ui.painter().rect_filled(rect, R_SM, tint(PANEL(), 150));
    }
    // A row of a closing folder is only drawn: no tools, menu, drag or drop.
    if !ui.is_enabled() {
        return;
    }

    // Hover tools: Run and the dots menu for a script, new folder and the dots menu for a folder.
    if show_tools {
        let at = |k: f32| egui::Rect::from_center_size(egui::pos2(rect.right() - 18.0 - 24.0 * k, rect.center().y), egui::vec2(20.0, 20.0));
        let dots = row_tool(ui, dots_id, at(0.0), paint_dots, &format!("More for {}", look.name), "More");
        if matches!(row.node, Node::Script(_) | Node::File { .. }) {
            if look.kind != "filter" && row_tool(ui, dots_id.with("run"), at(1.0), glyphs::play, &format!("Run all {}", look.name), &format!("Run all ({})", super::keys_text(true, false, false, "Enter"))).clicked() {
                acts.push(Act::Open(row.node.clone(), How::Run));
            }
        } else if let Node::Folder(id) = row.node {
            if row_tool(ui, dots_id.with("plus"), at(1.0), glyphs::plus, &format!("New folder in {}", look.name), "New folder inside").clicked() {
                acts.push(Act::NewFolder(Some(id)));
            }
        } else if let Some((link, dir)) = dir_of(&row.node).filter(|_| dir_like)
            && row_tool(ui, dots_id.with("plus"), at(1.0), glyphs::plus, &format!("New script in {}", look.name), "New script inside").clicked()
        {
            acts.push(Act::NewScript(link, dir));
        }
        if st.demo_menu.as_ref() == Some(&row.node) {
            egui::Popup::open_id(&ctx, dots_id.with("popup"));
            st.demo_menu = None;
        }
        egui::Popup::menu(&dots).show(|ui| node_menu(ui, cx, &row.node, &look.kind, acts));
    }

    // Tab reaching a row selects it and gives the tree the keys, as a click does.
    if r.gained_focus() {
        st.sel = Some(row.node.clone());
        st.focus = true;
    }
    if r.clicked() || r.secondary_clicked() {
        st.sel = Some(row.node.clone());
        st.focus = true;
        if r.clicked() && is_container {
            acts.push(Act::Toggle(row.node.clone()));
        }
    }
    if r.double_clicked() && row.node.script_ref().is_some() {
        acts.push(Act::Open(row.node.clone(), How::Here));
    }
    if has_menu(&row.node) {
        r.context_menu(|ui| node_menu(ui, cx, &row.node, &look.kind, acts));
    }
    // A folder that is not found says where it was looked for.
    let r = match &row.node {
        Node::Link(id) if look.warn => match cx.lib.links.iter().find(|l| l.id == *id) {
            Some(l) => r.on_hover_text(format!("Not found at {}", l.path.display())),
            None => r,
        },
        _ => r,
    };

    // Dragging a folder, a script or a linked file, and what a drag over this row would do.
    if let Some(item) = drag_item(&row.node) {
        let badge = st.badge;
        // The ghost is the row itself: its glyph and name, without the indent, and a badge when the drop copies the item
        // or writes it to disk, so a drag says what it will do before it is let go.
        drag_source_with(ui, &r, item, |p, g| {
            let cy = g.center().y;
            glyph(p, egui::pos2(g.left() + 30.0, cy), gcol);
            let name = p.layout_job(LayoutJob::single_section(look.name.clone(), ui_fmt(TXT_BODY, BRIGHT(), if is_container { W_MEDIUM } else { 400.0 }, 0.0)));
            let dy = optical_dy(&name);
            let right = g.left() + 41.0 + name.size().x;
            p.galley(egui::pos2(g.left() + 41.0, cy - name.size().y / 2.0 + dy), name, BRIGHT());
            if let Some(b) = badge {
                let bg = p.layout_job(LayoutJob::single_section(b.to_string(), ui_fmt(TXT_CAPTION, PRIMARY(), W_MEDIUM, 0.0)));
                let pill = egui::Rect::from_min_size(egui::pos2(right + 8.0, cy - 8.0), egui::vec2(bg.size().x + 10.0, 16.0));
                p.rect_filled(pill, R_SM, tint(PRIMARY(), 36));
                let bdy = optical_dy(&bg);
                p.galley(egui::pos2(pill.left() + 5.0, cy - bg.size().y / 2.0 + bdy), bg, PRIMARY());
            }
        });
    }
    let target_is_folder = matches!(row.node, Node::Folder(_)) || dir_like;
    let over = r.dnd_hover_payload::<DragItem>().filter(|_| !drag_cancelled(&ctx)).and_then(|p| item_node(&p));
    let frac = y_frac(rect, ctx.pointer_hover_pos());
    // A linked folder has nothing to order, so any part of its row means "into".
    let pos = if dir_like { Zone::Into } else { drop_zone(target_is_folder, row.open, frac) };
    let into_closed = target_is_folder && !row.open;
    if let Some(item) = over.as_ref().filter(|i| **i != row.node) {
        let plan = plan_drop(cx.lib, cx.scans, item, &row.node, pos);
        st.badge_next = plan.as_ref().ok().and_then(|p| badge_of(p, item));
        if plan.is_ok() {
            // The mark follows the plan: a fill for into a folder, else a line at the depth of the row it lands beside.
            let zone = match (pos, &plan) {
                (Zone::Into, Ok(Plan::Saved(DropAt::Into(_)))) if target_is_folder => Zone::Into,
                (_, Ok(Plan::Into { .. })) if dir_like => Zone::Into,
                (Zone::Before, _) => Zone::Before,
                _ => Zone::After,
            };
            let depth = row.depth;
            paint_spot(ui.painter(), rect, Spot { zone, depth }, rect.left() + 8.0, INDENT);
        } else {
            ctx.set_cursor_icon(egui::CursorIcon::NotAllowed);
        }
        if let Some(DragItem::Script(_) | DragItem::Folder(_)) = r.dnd_release_payload::<DragItem>().as_deref() {
            acts.push(match plan {
                Ok(Plan::Saved(a)) => Act::Move(item.clone(), a),
                Ok(plan) => Act::Drop(item.clone(), plan),
                Err(e) => Act::Refuse(e),
            });
        }
    }
    // A closed folder under a drag opens after a short rest, so the item can go inside it.
    let resting = over.as_ref().is_some_and(|i| *i != row.node) && into_closed && pos == Zone::Into;
    if hover_open(&ctx, r.id.with("dnd-open"), resting) {
        st.open.insert(row.node.clone());
        fold::start(&ctx, fold_id(&row.node), true);
        st.folding.insert(row.node.clone());
    }

    // Demo: a drag held over a folder, drawn without a pointer.
    if let Some((_, target)) = cx.demo_drag
        && *target == row.node
    {
        drop_fill(ui.painter(), rect);
        let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("library-demo-ghost")));
        let galley = p.layout_no_wrap("Slow orders lookup".to_string(), FontId::proportional(TXT_BODY), TEXT());
        let g = egui::Rect::from_min_size(rect.center() + egui::vec2(18.0, 10.0), galley.size() + egui::vec2(24.0, 10.0));
        p.rect(g, R_SM, RAISED(), Stroke::new(1.0, PRIMARY()), egui::StrokeKind::Inside);
        p.galley(g.min + egui::vec2(12.0, 5.0), galley, TEXT());
    }
}

#[cfg(test)]
mod linked_tests {
    use super::*;
    use crate::library::Link;

    fn node_file(rel: &str) -> Node {
        Node::File { link: 1, rel: rel.into() }
    }

    #[test]
    fn what_a_drop_on_a_row_does_depends_on_what_is_dragged_and_where() {
        let lib = Library { links: vec![Link { id: 1, ..Default::default() }, Link { id: 2, ..Default::default() }], folders: vec![crate::library::Folder { id: 5, name: "F".into(), parent: None }], ..Default::default() };
        let dir = |rel: &str| Node::Dir { link: 1, rel: rel.into() };
        let mut scans = Scans::default();
        // A file goes to another folder of its own link, not the one it is in, and never to another link.
        assert_eq!(plan_drop(&lib, &scans, &node_file("a.sql"), &dir("sub"), Zone::Into), Ok(Plan::Into { link: 1, dir: "sub".into() }));
        assert!(plan_drop(&lib, &scans, &node_file("sub/a.sql"), &dir("sub"), Zone::Into).is_err());
        assert_eq!(plan_drop(&lib, &scans, &node_file("sub/a.sql"), &Node::Link(1), Zone::Into), Ok(Plan::Into { link: 1, dir: PathBuf::new() }));
        assert!(plan_drop(&lib, &scans, &node_file("a.sql"), &Node::Link(2), Zone::Into).is_err());
        assert!(plan_drop(&lib, &scans, &Node::File { link: 2, rel: "x.sql".into() }, &Node::Link(1), Zone::Into).is_err());
        // Onto a library folder it is copied there; a library script goes into the linked folder; a library folder does not.
        assert_eq!(plan_drop(&lib, &scans, &node_file("a.sql"), &Node::Folder(5), Zone::Into), Ok(Plan::Out(DropAt::Into(5))));
        assert_eq!(plan_drop(&lib, &scans, &Node::Script(9), &dir("sub"), Zone::Before), Ok(Plan::Into { link: 1, dir: "sub".into() }));
        assert!(plan_drop(&lib, &scans, &Node::Folder(5), &Node::Link(1), Zone::Into).is_err());
        // A linked folder that is gone takes nothing, neither at its top nor in one of its folders.
        scans.insert(1, crate::library::disk::Scan { missing: true, ..Default::default() });
        assert_eq!(plan_drop(&lib, &scans, &Node::Script(9), &Node::Link(1), Zone::Into), Err(MISSING));
        assert_eq!(plan_drop(&lib, &scans, &node_file("a.sql"), &dir("sub"), Zone::Into), Err(MISSING));
    }

    #[test]
    fn drop_positions_split_rows_by_height() {
        assert_eq!(drop_zone(true, false, 0.1), Zone::Before);
        assert_eq!(drop_zone(true, false, 0.5), Zone::Into);
        assert_eq!(drop_zone(true, false, 0.9), Zone::After);
        // The first child of an open folder is right below it, so its lower edge is "into".
        assert_eq!(drop_zone(true, true, 0.9), Zone::Into);
        assert_eq!((drop_zone(false, false, 0.2), drop_zone(false, false, 0.8)), (Zone::Before, Zone::After));
    }

    #[test]
    fn a_drag_says_when_it_copies_or_writes_to_disk() {
        let into = Plan::Into { link: 1, dir: PathBuf::new() };
        assert_eq!(badge_of(&into, &Node::Script(3)), Some("Move to disk"));
        assert_eq!(badge_of(&Plan::Out(DropAt::Root), &node_file("a.sql")), Some("Copy"));
        // Moves that stay where they are kept need no word.
        assert_eq!(badge_of(&into, &node_file("a.sql")), None);
        assert_eq!(badge_of(&Plan::Saved(DropAt::Root), &Node::Script(3)), None);
    }
}
