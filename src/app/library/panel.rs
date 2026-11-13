//! The Library panel of the sidebar: folders and scripts kept by Quarry (and, once B2 lands, linked folders), a scope switch,
//! a search, a preview card, and the menus and drags that organize them. The rows come from `library::tree::flatten`, the
//! changes go through `library::ops`, and a script leaves for a tab through `open_script`.
//!
//! Rows are drawn with `show_rows`, so nothing here holds a reference into `self` across a frame: the drawing collects
//! `Act`s and `apply` runs them once the panel is done.
#![allow(dead_code)]

use super::*;
use crate::app::restore::Restore;
use crate::library::ops::{self, DropAt};
use crate::library::tree::{self, Node, Pos, Row, Scans};
use crate::library::{Library, ScriptRef};
use crate::ui::dnd::{drag_source, drop_fill, drop_line, hover_open, side_row_drag};
use super::open::How;

/// Whether the rail shows the Library button.
pub(in crate::app) const READY: bool = true;

const ROW_H: f32 = 26.0;
const INDENT: f32 = 16.0;
/// Preview card height when the person never dragged the splitter, and the room the tree and the card keep.
const PREVIEW_DEFAULT: f32 = 196.0;
const PREVIEW_MIN: f32 = 130.0;
const TREE_MIN: f32 = 120.0;

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
    /// The row whose dots menu is open.
    pub menu: Option<Node>,
    pub scroll_y: f32,
    pub view_h: f32,
    pub scroll_set: Option<f32>,
    /// What B2's scans found below each link. Empty until a folder is linked.
    pub scans: Scans,
    /// Demo only: a menu to open on this node, and a drag held from the first node over the second.
    pub demo_menu: Option<Node>,
    pub demo_drag: Option<(Node, Node)>,
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
    Refuse(&'static str),
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

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// "edited 5m ago", or "" for a script that never recorded a time.
fn edited(updated: u64) -> String {
    if updated == 0 {
        return String::new();
    }
    let s = now_secs().saturating_sub(updated);
    if s >= 2 * 86400 {
        return format!("edited {}d ago", s / 86400);
    }
    format!("edited {} ago", crate::app::log::ago(std::time::UNIX_EPOCH + Duration::from_secs(updated)))
}

fn item_node(d: &DragItem) -> Option<Node> {
    match d {
        DragItem::Script(ScriptRef::Saved(id)) => Some(Node::Script(*id)),
        DragItem::Folder(id) => Some(Node::Folder(*id)),
        _ => None,
    }
}

fn drag_item(node: &Node) -> Option<DragItem> {
    match node {
        Node::Script(id) => Some(DragItem::Script(ScriptRef::Saved(*id))),
        Node::Folder(id) => Some(DragItem::Folder(*id)),
        _ => None,
    }
}

/// What a row shows and needs, looked up once per frame from the library.
struct RowCx<'a> {
    lib: &'a Library,
    conns: &'a [Conn],
    scans: &'a Scans,
    active: Option<u64>,
    /// The item being dragged, if a drag of a script or folder is on.
    drag: Option<Node>,
    demo_drag: Option<&'a (Node, Node)>,
}

struct Look {
    name: String,
    kind: String,
    right: String,
}

impl RowCx<'_> {
    fn conn_name(&self, id: u64) -> String {
        self.conns.iter().find(|c| c.meta.id == id).map_or_else(|| "removed connection".to_string(), |c| c.meta.name.clone())
    }

    fn look(&self, row: &Row) -> Look {
        let file_name = |rel: &std::path::Path| rel.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        match &row.node {
            Node::Folder(id) => Look { name: self.lib.folders.iter().find(|f| f.id == *id).map(|f| f.name.clone()).unwrap_or_default(), kind: String::new(), right: row.count.to_string() },
            Node::Script(id) => {
                let Some(s) = self.lib.scripts.iter().find(|s| s.id == *id) else { return Look { name: String::new(), kind: String::new(), right: String::new() } };
                let right = if self.active.is_some_and(|a| a != s.conn) || (self.active.is_none() && !self.conns.iter().any(|c| c.meta.id == s.conn)) {
                    self.conn_name(s.conn)
                } else {
                    match s.kind.as_str() {
                        "shell" => "shell".to_string(),
                        "filter" | "pipeline" => [s.db.as_str(), s.coll.as_str()].iter().filter(|x| !x.is_empty()).copied().collect::<Vec<_>>().join("/"),
                        _ => s.db.clone(),
                    }
                };
                Look { name: s.name.clone(), kind: s.kind.clone(), right }
            }
            Node::Link(id) => {
                let l = self.lib.links.iter().find(|l| l.id == *id);
                let name = l.map(|l| if l.name.is_empty() { file_name(&l.path) } else { l.name.clone() }).unwrap_or_default();
                Look { name, kind: String::new(), right: row.count.to_string() }
            }
            Node::Dir { rel, .. } => Look { name: file_name(rel), kind: String::new(), right: row.count.to_string() },
            Node::File { link, rel } => {
                let kind = self.scans.get(link).and_then(|s| s.files.iter().find(|f| f.rel == *rel)).map(|f| f.kind.clone()).unwrap_or_default();
                Look { name: file_name(rel), kind, right: String::new() }
            }
        }
    }
}

/// A 28 px toolbar button with a painted glyph, muted at rest and bright on hover.
fn tool_button(ui: &mut egui::Ui, glyph: glyphs::Glyph, name: &str, tip: &str) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(28.0, 24.0), Sense::click());
    ui.painter().rect_filled(rect, R_SM, hover_fill(state_of(&r)));
    glyph(ui.painter(), rect.center(), if r.hovered() { BRIGHT() } else { MUTED() });
    focus_ring(ui, &r, rect, R_SM as f32);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, name));
    r.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The small square a hovered row shows at its right end.
fn row_tool(ui: &mut egui::Ui, id: egui::Id, at: egui::Rect, glyph: glyphs::Glyph, name: &str, tip: &str) -> egui::Response {
    let r = ui.interact(at, id, Sense::click());
    let state = state_of(&r);
    ui.painter().rect_filled(at, R_SM, [tint(TEXT(), 14), tint(TEXT(), 34), tint(TEXT(), 46)][state]);
    glyph(ui.painter(), at.center(), if state > 0 { BRIGHT() } else { MUTED() });
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, name));
    r.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A bordered button with the play mark, for Run in the preview card.
fn run_chip(ui: &mut egui::Ui) -> egui::Response {
    let g = ui.painter().layout_job(LayoutJob::single_section("Run".into(), ui_fmt(TXT_BODY, Color32::PLACEHOLDER, W_MEDIUM, 0.0)));
    let (rect, r) = ui.allocate_exact_size(egui::vec2(g.size().x + 38.0, BUTTON_H), Sense::click());
    let state = state_of(&r);
    ui.painter().rect(rect, R_MD, [RAISED(), shade(RAISED(), 0.12), shade(RAISED(), 0.24)][state], Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
    glyphs::play(ui.painter(), egui::pos2(rect.left() + 17.0, rect.center().y), if state > 0 { BRIGHT() } else { MUTED() });
    let dy = optical_dy(&g);
    ui.painter().galley(egui::pos2(rect.left() + 28.0, rect.center().y - g.size().y / 2.0 + dy), g, TEXT());
    focus_ring(ui, &r, rect, R_MD as f32);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Run"));
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn menu_item(ui: &mut egui::Ui, acts: &mut Vec<Act>, label: &str, key: &str, act: Act) {
    if menu_row(ui, label, TEXT(), false, key).clicked() {
        acts.push(act);
        ui.close();
    }
}

/// The menu of a row: what Return, Alt+Return, Cmd+Enter and F2 do, plus the rest. A linked file offers what opens it; the
/// edits are for folders and scripts kept by Quarry. B2 adds the menus of linked folders here.
fn node_menu(ui: &mut egui::Ui, lib: &Library, node: &Node, kind: &str, acts: &mut Vec<Act>) {
    menu_look(ui);
    let run = kind != "filter";
    match node {
        Node::Script(_) | Node::File { .. } => {
            menu_item(ui, acts, "Open", "Return", Act::Open(node.clone(), How::Here));
            menu_item(ui, acts, "Open in a new tab", "Alt+Return", Act::Open(node.clone(), How::NewTab));
            if run {
                menu_item(ui, acts, "Run", "Cmd+Enter", Act::Open(node.clone(), How::Run));
            }
        }
        _ => {}
    }
    if let Node::Folder(id) = node {
        menu_item(ui, acts, "New folder", "", Act::NewFolder(Some(*id)));
    }
    if node.is_saved() {
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
        ui.menu_button("Move to folder", |ui| {
            menu_look(ui);
            for (id, depth, name) in ops::folder_list(lib, skip) {
                let label = format!("{}{}", "  ".repeat(depth), name);
                if menu_row(ui, &label, TEXT(), cur == Some(id), "").clicked() {
                    acts.push(Act::Move(node.clone(), DropAt::Into(id)));
                    ui.close();
                }
            }
            ui.separator();
            menu_item(ui, acts, "New folder…", "", Act::MoveToNew(node.clone()));
            if cur.is_some() {
                menu_item(ui, acts, "Move out of folders", "", Act::Move(node.clone(), DropAt::Root));
            }
        });
        if let Node::Script(id) = node {
            menu_item(ui, acts, "Copy text", "", Act::Copy(*id));
        }
        ui.separator();
        if danger_item(ui, "Delete…").clicked() {
            acts.push(Act::Delete(node.clone()));
            ui.close();
        }
    }
}

/// Whether a row has a menu: folders and scripts kept by Quarry, and linked files.
fn has_menu(node: &Node) -> bool {
    node.is_saved() || matches!(node, Node::File { .. })
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
        let active = self.active.filter(|id| self.conn(*id).is_some());
        let all = self.prefs.lib_scope_all || active.is_none();
        let active_name = active.and_then(|id| self.conn(id)).map(|c| c.meta.name.clone());
        let scope_conn = if all { None } else { active };

        // A press outside the panel hands the keys back to the page.
        if ctx.input(|i| i.pointer.any_pressed()) && ctx.pointer_interact_pos().is_some_and(|p| !ui.max_rect().contains(p)) {
            st.focus = false;
        }

        // Header
        let new_parent = match &st.sel {
            Some(Node::Folder(id)) => Some(*id),
            Some(Node::Script(id)) => self.library.scripts.iter().find(|s| s.id == *id).and_then(|s| tree::live_parent(&self.library, s.folder)),
            _ => None,
        };
        ui.horizontal(|ui| {
            ui.label(LayoutJob::single_section("LIBRARY".into(), ui_fmt(10.5, MUTED(), W_SEMIBOLD, 0.6)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let r = dots_menu(ui, |ui| {
                    if menu_row(ui, "Link a folder…", TEXT(), false, "").clicked() {
                        acts.push(Act::Link);
                        ui.close();
                    }
                    if menu_row(ui, "Fold all", TEXT(), false, "").clicked() {
                        acts.push(Act::FoldAll);
                        ui.close();
                    }
                    if menu_row(ui, "Show all connections", TEXT(), all, "").clicked() {
                        acts.push(Act::Scope(!all));
                        ui.close();
                    }
                });
                r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Library menu"));
                if tool_button(ui, glyphs::folder_plus, "New folder", "New folder").clicked() {
                    acts.push(Act::NewFolder(new_parent));
                }
            });
        });
        ui.add_space(6.0);

        // Scope
        let mut scope_all = all;
        let label = active_name.as_deref().map(|n| if n.chars().count() > 16 { format!("{}…", n.chars().take(15).collect::<String>()) } else { n.to_string() }).unwrap_or_else(|| "No connection".into());
        segmented_on(ui, &mut scope_all, &[(false, label.as_str(), active.is_some()), (true, "All connections", true)]);
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

        let rows = tree::flatten(&self.library, &st.scans, scope_conn, &st.query, &st.open);
        if rows.is_empty() && st.query.trim().is_empty() {
            self.library_empty(ui, active, active_name.as_deref(), all, acts);
            return;
        }
        if rows.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new(format!("No scripts match \"{}\".", st.query.trim())).size(TXT_SMALL).color(MUTED()));
            return;
        }

        if st.sel.as_ref().is_some_and(|n| !rows.iter().any(|r| r.node == *n)) && st.rename.is_none() {
            st.sel = None;
        }
        self.library_keys(&ctx, st, &rows, acts);

        // Tree, splitter and preview share what is left of the panel's height.
        let avail = ui.available_height();
        let show_preview = st.sel.as_ref().is_some_and(|n| n.script_ref().is_some()) && avail > PREVIEW_MIN + TREE_MIN + 12.0;
        let want = if self.prefs.lib_preview_h > 0.0 { self.prefs.lib_preview_h } else { PREVIEW_DEFAULT };
        let preview_h = want.clamp(PREVIEW_MIN, (avail - TREE_MIN - 6.0).max(PREVIEW_MIN));
        let tree_h = if show_preview { avail - preview_h - 6.0 } else { avail };

        // The scans and the demo drag move out of `st` for the frame, so the rows can read them while `st` is borrowed for state.
        let scans = std::mem::take(&mut st.scans);
        let demo_drag = st.demo_drag.clone();
        let cx = RowCx {
            lib: &self.library,
            conns: &self.conns,
            scans: &scans,
            active,
            drag: egui::DragAndDrop::payload::<DragItem>(&ctx).and_then(|d| item_node(&d)),
            demo_drag: demo_drag.as_ref(),
        };
        let total = rows.len();
        let mut area = egui::ScrollArea::vertical().id_salt("library-tree").auto_shrink([false, false]).max_height(tree_h);
        if let Some(y) = st.scroll_set.take() {
            area = area.vertical_scroll_offset(y);
        }
        let out = ui
            .scope(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                area.show_rows(ui, ROW_H, total, |ui, range| {
                    for i in range {
                        draw_row(ui, &cx, st, &rows[i], acts);
                    }
                })
            })
            .inner;
        st.scroll_y = out.state.clamped_offset().y;
        st.view_h = out.inner_rect.height();

        // The space below the last row takes drops that move an item out of its folder.
        let used = (total as f32 * ROW_H - st.scroll_y).max(0.0);
        let zone = egui::Rect::from_min_max(egui::pos2(out.inner_rect.left(), out.inner_rect.top() + used), out.inner_rect.right_bottom());
        if zone.height() > 6.0 && cx.drag.is_some() {
            let r = ui.interact(zone, egui::Id::unique("library-root-drop"), Sense::hover());
            if let (Some(p), Some(item)) = (r.dnd_hover_payload::<DragItem>(), cx.drag.as_ref()) {
                let _ = p;
                if ops::can_move(&self.library, item, &DropAt::Root).is_ok() {
                    drop_line(ui.painter(), zone.x_range(), zone.top() + 1.0);
                }
            }
            if let (Some(_), Some(item)) = (r.dnd_release_payload::<DragItem>(), cx.drag.clone()) {
                acts.push(Act::Move(item, DropAt::Root));
            }
        }
        st.scans = scans;

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
        empty_state(ui, paint_book, &title, "Press Cmd+S in a query to keep it here,\nor link a folder of scripts from a repo.", |ui| {
            ui.add_space(10.0);
            if primary(ui, "Link a folder…", PRIMARY()).clicked() {
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
    fn library_keys(&mut self, ctx: &egui::Context, st: &mut PanelUi, rows: &[Row], acts: &mut Vec<Act>) {
        if !st.focus || self.window_open() || st.rename.is_some() || ctx.egui_wants_keyboard_input() {
            return;
        }
        let none = egui::Modifiers::NONE;
        let k = |m: egui::Modifiers, key: Key| ctx.input_mut(|i| i.consume_key(m, key));
        let at = st.sel.as_ref().and_then(|n| rows.iter().position(|r| r.node == *n));
        let kind = |n: &Node| match n {
            Node::Script(id) => self.library.scripts.iter().find(|s| s.id == *id).map(|s| s.kind.clone()).unwrap_or_default(),
            Node::File { link, rel } => st.scans.get(link).and_then(|s| s.files.iter().find(|f| f.rel == *rel)).map(|f| f.kind.clone()).unwrap_or_default(),
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
            if k(none, Key::F2) && n.is_saved() {
                acts.push(Act::Rename(n.clone()));
            }
            if (k(none, Key::Delete) || k(none, Key::Backspace)) && n.is_saved() {
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
        let ScriptRef::Saved(id) = node.script_ref().unwrap_or(ScriptRef::Saved(0)) else {
            // A linked file shows its text once B2 reads it.
            return;
        };
        let Some(s) = self.library.scripts.iter().find(|s| s.id == id).cloned() else { return };
        let place = {
            let p = ops::folder_path(&self.library, s.folder);
            let p = if p.is_empty() { "Library".to_string() } else { p };
            if all { format!("{} · {p}", self.conns.iter().find(|c| c.meta.id == s.conn).map_or("removed connection", |c| c.meta.name.as_str())) } else { p }
        };
        let when = edited(s.updated);
        let avail = ui.available_height();
        egui::Frame::new().fill(RAISED()).stroke(Stroke::new(1.0, LINE())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height((avail - 20.0).max(0.0));
            let w = ui.available_width();
            let mut head = LayoutJob::single_section(s.name.clone(), ui_fmt(TXT_TITLE, BRIGHT(), W_MEDIUM, 0.0));
            head.wrap = egui::text::TextWrapping::truncate_at_width(w);
            ui.label(head);
            let mut sub = LayoutJob::single_section(if when.is_empty() { place.clone() } else { format!("{place}  ·  {when}") }, ui_fmt(TXT_CAPTION, FAINT(), 400.0, 0.0));
            sub.wrap = egui::text::TextWrapping::truncate_at_width(w);
            ui.label(sub);
            ui.add_space(6.0);
            // As many lines as the card has room for between the header and the buttons.
            let lines = (((avail - 112.0) / 15.0) as usize).clamp(2, 8);
            let text: String = s.body.chars().take(1500).collect();
            let job = match s.kind.as_str() {
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
                if s.kind != "filter" && run_chip(ui).clicked() {
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
            Act::Toggle(n) => {
                if !st.open.remove(&n) {
                    st.open.insert(n);
                }
            }
            Act::Rename(n) => {
                let name = match &n {
                    Node::Folder(id) => self.library.folders.iter().find(|f| f.id == *id).map(|f| f.name.clone()),
                    Node::Script(id) => self.library.scripts.iter().find(|s| s.id == *id).map(|s| s.name.clone()),
                    _ => None,
                };
                if let Some(name) = name {
                    st.sel = Some(n.clone());
                    st.rename = Some((n, name, true));
                }
            }
            Act::Commit(n, text) => {
                if ops::rename(&mut self.library, &n, &text) {
                    self.save_library();
                }
            }
            Act::Duplicate(id) => {
                if let Some(new) = ops::duplicate(&mut self.library, id, now_secs()) {
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
            Act::Delete(n) => {
                let label = match &n {
                    Node::Script(id) => self.library.scripts.iter().find(|s| s.id == *id).map(|s| format!("\"{}\"", s.name)),
                    Node::Folder(id) => self.library.folders.iter().find(|f| f.id == *id).map(|f| format!("folder \"{}\"", f.name)),
                    _ => None,
                };
                if let Some((folders, scripts)) = ops::delete(&mut self.library, &n) {
                    self.save_library();
                    let msg = match (&n, scripts.len()) {
                        (Node::Folder(_), 0) => format!("Deleted {}.", label.unwrap_or_default()),
                        (Node::Folder(_), k) => format!("Deleted {} and {k} script{}.", label.unwrap_or_default(), if k == 1 { "" } else { "s" }),
                        _ => format!("Deleted {}.", label.unwrap_or_default()),
                    };
                    if st.sel.as_ref().is_some_and(|s| *s == n || matches!(s, Node::Script(id) if scripts.iter().any(|x| x.id == *id)) || matches!(s, Node::Folder(id) if folders.iter().any(|f| f.id == *id))) {
                        st.sel = None;
                    }
                    self.toast_restore(msg, Restore::Saved { scripts, folders });
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
            Act::FoldAll => st.open.clear(),
            Act::Scope(all) => {
                self.prefs.lib_scope_all = all;
                self.save_prefs();
            }
            Act::Link => self.open_link_dialog(None),
            Act::Refuse(why) => self.warn(why),
        }
    }

    /// QUARRY_TEST_LIBRARY=<state>: save, picker, picker-docs, picker-shell (B1); panel, all, search, empty, menu,
    /// folder-menu, drag (B3); linked, missing, link, save-linked, conflict (B2).
    pub(in crate::app) fn demo_library(&mut self, state: &str) {
        // B3
        if matches!(state, "panel" | "all" | "search" | "empty" | "menu" | "folder-menu" | "drag") {
            self.demo_library_panel(state);
        }
        // B1: save, picker, picker-docs, picker-shell (picker.rs).
        if self.demo_library_quick(state) {
            return;
        }
        // B2: its states go in a block of their own below B1's.
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
        let now = now_secs();
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
    let (glyph, gcol): (glyphs::Glyph, Color32) = match &row.node {
        Node::Folder(_) | Node::Dir { .. } => (if row.open { glyphs::folder_open } else { glyphs::folder }, MUTED()),
        Node::Link(_) => (glyphs::link, C_LINK()),
        Node::Script(_) | Node::File { .. } => kind_glyph(&look.kind),
    };

    if let Some((n, buf, fresh)) = st.rename.as_mut().filter(|(n, _, _)| *n == row.node) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::hover());
        let x = rect.left() + 8.0 + indent;
        if n.is_container() {
            chevron(ui.painter(), egui::pos2(x + 7.0, rect.center().y), row.open, FAINT());
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
    let rect_est = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), ROW_H));
    let hovered = cx.drag.is_none() && ui.rect_contains_pointer(rect_est);
    let selected = st.sel.as_ref() == Some(&row.node);
    let dots_id = egui::Id::unique(("lib-dots", &row.node));
    let menu_open = egui::Popup::is_id_open(&ctx, dots_id.with("popup")) || st.demo_menu.as_ref() == Some(&row.node);
    let show_tools = (hovered || menu_open) && has_menu(&row.node);
    let (is_container, saved) = (row.node.is_container(), row.node.is_saved());

    let r = side_row_drag(ui, ROW_H, indent, selected, &look.name, |p, inner| {
        let cy = inner.center().y;
        for k in 0..row.depth {
            let gx = inner.left() - indent + INDENT * k as f32 + 7.0;
            p.vline(gx, inner.y_range(), Stroke::new(1.0, LINE()));
        }
        let x = inner.left();
        if is_container {
            chevron(p, egui::pos2(x + 7.0, cy), row.open, if hovered { MUTED() } else { FAINT() });
        }
        glyph(p, egui::pos2(x + 22.0, cy), gcol);
        let tools_w = if show_tools { 52.0 } else { 0.0 };
        let right = if show_tools { String::new() } else { look.right.clone() };
        let rg = (!right.is_empty()).then(|| p.layout_job(LayoutJob::single_section(right, mono_fmt(10.5, FAINT(), 400.0))));
        let rw = rg.as_ref().map_or(0.0, |g| g.size().x + 8.0);
        if let Some(g) = rg {
            let dy = optical_dy(&g);
            p.galley(egui::pos2(inner.right() - g.size().x, cy - g.size().y / 2.0 + dy), g, FAINT());
        }
        let weight = if is_container { W_MEDIUM } else { 400.0 };
        let color = if selected || hovered { BRIGHT() } else { TEXT() };
        let mut job = LayoutJob::single_section(look.name.clone(), ui_fmt(TXT_BODY, color, weight, 0.0));
        job.wrap = egui::text::TextWrapping::truncate_at_width((inner.right() - rw - tools_w - (x + 33.0)).max(20.0));
        let g = p.layout_job(job);
        let dy = optical_dy(&g);
        p.galley(egui::pos2(x + 33.0, cy - g.size().y / 2.0 + dy), g, color);
    });
    let rect = r.rect;
    if dim {
        ui.painter().rect_filled(rect, R_SM, tint(PANEL(), 150));
    }

    // Hover tools: Run and the dots menu for a script, new folder and the dots menu for a folder.
    if show_tools {
        let at = |k: f32| egui::Rect::from_center_size(egui::pos2(rect.right() - 18.0 - 24.0 * k, rect.center().y), egui::vec2(20.0, 20.0));
        let dots = row_tool(ui, dots_id, at(0.0), paint_dots, &format!("More for {}", look.name), "More");
        if matches!(row.node, Node::Script(_) | Node::File { .. }) {
            if look.kind != "filter" && row_tool(ui, dots_id.with("run"), at(1.0), glyphs::play, &format!("Run {}", look.name), "Run (Cmd+Enter)").clicked() {
                acts.push(Act::Open(row.node.clone(), How::Run));
            }
        } else if let Node::Folder(id) = row.node
            && row_tool(ui, dots_id.with("plus"), at(1.0), glyphs::plus, &format!("New folder in {}", look.name), "New folder inside").clicked()
        {
            acts.push(Act::NewFolder(Some(id)));
        }
        if st.demo_menu.as_ref() == Some(&row.node) {
            egui::Popup::open_id(&ctx, dots_id.with("popup"));
            st.demo_menu = None;
        }
        egui::Popup::menu(&dots).width(210.0).show(|ui| node_menu(ui, cx.lib, &row.node, &look.kind, acts));
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
        r.context_menu(|ui| node_menu(ui, cx.lib, &row.node, &look.kind, acts));
    }

    // Dragging a folder or script, and what a drag over this row would do.
    if saved && let Some(item) = drag_item(&row.node) {
        drag_source(ui, &r, item, &look.name);
    }
    let target_is_folder = matches!(row.node, Node::Folder(_));
    let over = r.dnd_hover_payload::<DragItem>().and_then(|p| item_node(&p));
    let frac = ctx.pointer_hover_pos().map_or(0.5, |p| ((p.y - rect.top()) / rect.height()).clamp(0.0, 1.0));
    let pos = tree::drop_at(target_is_folder, row.open, frac);
    let at = match (&row.node, pos) {
        (Node::Folder(id), Pos::Into) => Some(DropAt::Into(*id)),
        (n, Pos::Before) if n.is_saved() => Some(DropAt::Before(n.clone())),
        (n, Pos::After) if n.is_saved() => Some(DropAt::After(n.clone())),
        _ => None,
    };
    let into_closed = target_is_folder && !row.open;
    if let Some(item) = over.as_ref().filter(|i| **i != row.node) {
        let verdict = match &at {
            Some(a) => ops::can_move(cx.lib, item, a),
            None => ops::can_move(cx.lib, item, &DropAt::Before(row.node.clone())),
        };
        if verdict.is_ok() {
            match pos {
                Pos::Into if target_is_folder => drop_fill(ui.painter(), rect),
                Pos::Before => drop_line(ui.painter(), egui::Rangef::new(rect.left() + 8.0 + indent, rect.right()), rect.top()),
                _ => drop_line(ui.painter(), egui::Rangef::new(rect.left() + 8.0 + indent, rect.right()), rect.bottom()),
            }
        } else {
            ctx.set_cursor_icon(egui::CursorIcon::NotAllowed);
        }
        if let Some(DragItem::Script(_) | DragItem::Folder(_)) = r.dnd_release_payload::<DragItem>().as_deref() {
            match (verdict, at) {
                (Ok(()), Some(a)) => acts.push(Act::Move(item.clone(), a)),
                (Err(e), _) => acts.push(Act::Refuse(e)),
                (Ok(()), None) => acts.push(Act::Refuse("Linked folders are kept on disk, so nothing moves into or out of them here.")),
            }
        }
    }
    // A closed folder under a drag opens after a short rest, so the item can go inside it.
    let resting = over.as_ref().is_some_and(|i| *i != row.node) && into_closed && pos == Pos::Into;
    if hover_open(&ctx, r.id.with("dnd-open"), resting) {
        st.open.insert(row.node.clone());
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
