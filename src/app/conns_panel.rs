//! The Connections panel of the sidebar: saved connections in colored groups, with the filter, the group menu, inline
//! rename and new group, and drag and drop between and within groups. The rows are a pure function of the connections,
//! the groups and the filter (`panel_rows`), so the picker in the explorer shows the same list and the rules are tested
//! without a window. Changes are collected while the rows are drawn and applied after, since the rows borrow the lists.

use super::*;
use crate::ui::dnd::{Spot, Zone, auto_scroll_while_dragging, drag_cancelled, drag_source_with, drop_fill, hover_open, paint_spot, y_frac, zone_at};
use crate::ui::fold;

/// Whether the rail shows the Connections button.
pub(super) const READY: bool = true;

/// Row heights of the panel, in the design.
const GROUP_H: f32 = 30.0;
const CAPTION_H: f32 = 24.0;
const HINT_H: f32 = 30.0;
/// Where a member's guide line runs and how far members are indented from the group row.
const GUIDE_X: f32 = 14.0;
const MEMBER_INDENT: f32 = 20.0;
/// One dot per open member on a closed group, at most this many.
const DOTS_MAX: usize = 5;
const CONN_H: f32 = 48.0;

/// The id a group's open and close animation goes by.
fn fold_id(g: u64) -> egui::Id {
    egui::Id::unique(("conn-group-fold", g))
}

/// What the Connections panel remembers while the app runs.
#[derive(Default)]
pub(super) struct ConnsUi {
    pub(super) filter: String,
    /// The group being renamed in place, with the text so far.
    pub(super) rename: Option<(u64, String)>,
    /// The name being typed for a new group, in a field at the end of the groups.
    pub(super) adding: Option<String>,
    /// The field that was just opened asks for focus once.
    pub(super) focus_edit: bool,
    pub(super) picker_filter: String,
    /// Groups the picker shows folded. Apart from `Group.open`, so folding in the picker never changes the panel.
    pub(super) picker_closed: HashSet<u64>,
    /// Demo states: the group menu drawn open, a drop shown in progress, and the picker opened.
    pub(super) demo_menu: Option<u64>,
    pub(super) demo_drop: Option<DemoDrop>,
    pub(super) demo_picker: bool,
}

#[derive(Clone, Copy)]
pub(super) enum DemoDrop {
    /// A connection hovering inside this group.
    Into(u64),
}

/// One row of the panel or of the picker.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum CRow {
    /// `shown` of `total` members pass the filter.
    Group { id: u64, open: bool, shown: usize, total: usize },
    /// `pos` is the place in the group's member list, or in the loose order, whatever the filter hides.
    Conn { id: u64, group: Option<u64>, pos: usize },
    /// The line of an open group with no members.
    Hint(u64),
    Caption { text: &'static str, note: String },
    /// Where the groups end: the field for a new group sits here.
    GroupsEnd,
}

/// What a drop on a row means, by how far down the row the pointer is. The middle half of a group row is "into" the
/// group. Elsewhere the top or bottom half of the row means before or after it.
pub(super) fn drop_slot(row: &CRow, y_frac: f32) -> Zone {
    match row {
        CRow::Hint(_) => Zone::Into,
        _ => zone_at(1.0, y_frac, matches!(row, CRow::Group { .. })),
    }
}

/// Where dropping `moved` on `rows[i]` at `y_frac` puts it, in the terms of `Groups::place`, or None when the drop does
/// nothing (on itself, on the end marker). The edges of a group row act as its middle, except the bottom edge of an open
/// one, which puts the connection first under the header.
pub(super) fn drop_place(groups: &Groups, all: &[u64], moved: u64, rows: &[CRow], i: usize, y_frac: f32) -> Option<Place> {
    let row = rows.get(i)?;
    let slot = drop_slot(row, y_frac);
    match row {
        CRow::Group { id, open, .. } => Some(Place::Into(*id, if slot == Zone::After && *open { Some(0) } else { None })),
        CRow::Hint(g) => Some(Place::Into(*g, None)),
        CRow::Caption { .. } => Some(Place::Loose(Some(0))),
        CRow::GroupsEnd => None,
        CRow::Conn { id, group, pos } => {
            if *id == moved {
                return None;
            }
            let mut at = if slot == Zone::Before { *pos } else { *pos + 1 };
            // The index counts the list after the connection leaves it, so a move down inside one list is one less.
            let from = match group {
                Some(g) => groups.group(*g).and_then(|g| g.members.iter().position(|c| *c == moved)),
                None => groups.loose_order(all).iter().position(|c| *c == moved),
            };
            if from.is_some_and(|s| s < at) {
                at -= 1;
            }
            Some(match group {
                Some(g) => Place::Into(*g, Some(at)),
                None => Place::Loose(Some(at)),
            })
        }
    }
}

/// Whether `c` passes the filter: every token is in its name (loosely), its tag or its address.
fn conn_matches(c: &Conn, toks: &[String]) -> bool {
    let (name, tag, uri) = (c.meta.name.to_lowercase(), c.meta.tag.to_lowercase(), c.masked_uri.to_lowercase());
    toks.iter().all(|t| seg_score(t, &name).is_some() || tag.contains(t.as_str()) || uri.contains(t.as_str()))
}

/// The byte ranges of `name` that the tokens match as plain text, sorted and merged, for the highlight. A loose match
/// has nothing to point at and gets none.
pub(super) fn hl_ranges(name: &str, toks: &[String]) -> Vec<(usize, usize)> {
    // ASCII lowering keeps every byte offset, and a match of an ASCII token never starts inside a multi-byte char.
    let low = name.to_ascii_lowercase();
    let mut v: Vec<(usize, usize)> = toks.iter().filter(|t| t.is_ascii()).filter_map(|t| low.find(t.as_str()).map(|a| (a, a + t.len()))).collect();
    v.sort();
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (a, b) in v {
        match out.last_mut() {
            Some(l) if a <= l.1 => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// The rows of the panel or the picker, top to bottom. `open` says whether a group shows its members; a group with a
/// match is open while a filter is on, whatever `open` says, and a group with none is hidden. A group whose own name
/// matches counts all its members as matches.
pub(super) fn panel_rows(conns: &[Conn], groups: &Groups, filter: &str, open: impl Fn(&Group) -> bool) -> Vec<CRow> {
    let toks = tokens(filter);
    let filtering = !toks.is_empty();
    let all: Vec<u64> = conns.iter().map(|c| c.meta.id).collect();
    let conn = |id: u64| conns.iter().find(|c| c.meta.id == id);
    let mut rows = Vec::new();
    let mut matches = 0;
    for g in &groups.groups {
        let members: Vec<u64> = g.members.iter().copied().filter(|id| conn(*id).is_some()).collect();
        let name_hit = filtering && toks.iter().all(|t| seg_score(t, &g.name.to_lowercase()).is_some());
        let shown: Vec<(usize, u64)> = members.iter().copied().enumerate().filter(|(_, id)| !filtering || name_hit || conn(*id).is_some_and(|c| conn_matches(c, &toks))).collect();
        if filtering && shown.is_empty() {
            continue;
        }
        matches += shown.len();
        let is_open = filtering || open(g);
        rows.push(CRow::Group { id: g.id, open: is_open, shown: shown.len(), total: members.len() });
        if is_open {
            if members.is_empty() {
                rows.push(CRow::Hint(g.id));
            }
            rows.extend(shown.into_iter().map(|(pos, id)| CRow::Conn { id, group: Some(g.id), pos }));
        }
    }
    rows.push(CRow::GroupsEnd);
    let loose: Vec<(usize, u64)> = groups.loose_order(&all).into_iter().enumerate().filter(|(_, id)| !filtering || conn(*id).is_some_and(|c| conn_matches(c, &toks))).collect();
    matches += loose.len();
    let grouped_shown = rows.len() > 1;
    if (!filtering && !groups.groups.is_empty()) || (filtering && grouped_shown && !loose.is_empty()) {
        rows.push(CRow::Caption { text: "NOT IN A GROUP", note: if loose.is_empty() { String::new() } else { loose.len().to_string() } });
    }
    rows.extend(loose.into_iter().map(|(pos, id)| CRow::Conn { id, group: None, pos }));
    if filtering {
        rows.insert(0, CRow::Caption { text: "MATCHES", note: format!("{matches} of {}", conns.len()) });
    }
    rows
}

/// What the rows ask for, applied after they are drawn.
enum PanelAct {
    Activate(u64),
    Toggle(u64),
    Move(u64, Place),
    NewGroup(String),
    Rename(u64, String),
    Color(u64, &'static str),
    ConnectAll(u64),
    DisconnectAll(u64),
    NewConnIn(Option<u64>),
    EditGroup(u64),
    DeleteGroup(u64),
    EditConn(u64),
    Connect(u64),
    Disconnect(u64),
    Ungroup(u64),
    Remove(u64),
    FoldAll,
    /// Show the connection's databases: it becomes the active one and the sidebar shows the explorer.
    Browse(u64),
}

/// A row the arrow keys move between: its widget id and what Left and Right do on it.
#[derive(Clone, Copy)]
enum Nav {
    Group { id: u64, open: bool },
    Conn { group: Option<u64> },
}

/// What the rows read while they are drawn.
struct Cx<'a> {
    conns: &'a [Conn],
    groups: &'a Groups,
    all: &'a [u64],
    active: Option<u64>,
    toks: &'a [String],
}

impl App {
    /// The body of the sidebar in Connections mode.
    pub(super) fn conns_panel(&mut self, ui: &mut egui::Ui) {
        let mut acts: Vec<PanelAct> = Vec::new();
        ui.horizontal(|ui| {
            ui.label(LayoutJob::single_section("CONNECTIONS".into(), ui_fmt(TXT_SECTION, MUTED(), W_SEMIBOLD, 0.6)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if icon_btn(ui, "New connection", paint_plus).clicked() {
                    acts.push(PanelAct::NewConnIn(None));
                }
                if icon_btn(ui, "New group", paint_new_group).clicked() {
                    self.conns_ui.adding = Some(String::new());
                    self.conns_ui.focus_edit = true;
                }
                if icon_btn(ui, "Fold all", paint_fold_all).clicked() {
                    acts.push(PanelAct::FoldAll);
                }
            });
        });
        ui.add_space(6.0);
        let r = ui.add(
            egui::TextEdit::singleline(&mut self.conns_ui.filter)
                .id(egui::Id::unique("conns-filter"))
                .hint_text(RichText::new("Filter connections").color(FAINT()))
                .desired_width(f32::INFINITY)
                .margin(egui::Margin { left: 10, right: 10, top: 5, bottom: 5 }),
        );
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Filter connections"));
        ui.add_space(10.0);

        let toks = tokens(&self.conns_ui.filter);
        let all = self.conn_ids();
        let ctx = ui.ctx().clone();
        // A group still closing keeps its members in the list until they have folded away, while its own row reads closed
        // for the keys, the drops and the dots.
        let mut rows = panel_rows(&self.conns, &self.groups, &self.conns_ui.filter, |g| g.open || fold::openness(&ctx, fold_id(g.id), false) > 0.0);
        for r in &mut rows {
            if let CRow::Group { id, open, .. } = r {
                *open = !toks.is_empty() || self.groups.group(*id).is_some_and(|g| g.open);
            }
        }
        let foot_h = 48.0;
        let room = (ui.available_height() - foot_h).max(80.0);
        egui::ScrollArea::vertical().id_salt("conns-panel").auto_shrink(false).max_height(room).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            auto_scroll_while_dragging(ui);
            let cx = Cx { conns: &self.conns, groups: &self.groups, all: &all, active: self.active, toks: &toks };
            draw_rows(ui, &rows, &cx, &mut self.conns_ui, &mut acts);
        });
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);
        let new = side_row(ui, 30.0, 0.0, false, "New connection…", |p, r| {
            paint_plus(p, egui::pos2(r.left() + 8.0, r.center().y), PRIMARY());
            let g = p.layout_no_wrap("New connection".into(), FontId::proportional(TXT_BODY), TEXT());
            p.galley(egui::pos2(r.left() + 24.0, r.center().y - g.size().y / 2.0), g, TEXT());
        });
        if new.clicked() {
            acts.push(PanelAct::NewConnIn(None));
        }
        for a in acts {
            self.apply_panel_act(&ctx, a);
        }
    }

    fn apply_panel_act(&mut self, ctx: &egui::Context, a: PanelAct) {
        match a {
            // A click picks the connection and shows its databases, like a pick in the explorer's picker; the panel
            // would otherwise stay up with nothing on screen changed but a highlight.
            PanelAct::Activate(id) | PanelAct::Browse(id) => {
                self.activate(id);
                self.show_sidebar();
            }
            PanelAct::Toggle(g) => {
                if let Some(g) = self.groups.group_mut(g) {
                    g.open = !g.open;
                    fold::start(ctx, fold_id(g.id), g.open);
                    self.save_groups();
                }
            }
            PanelAct::FoldAll => {
                // One press folds everything; with all folded already, the next one opens everything.
                let any_open = self.groups.groups.iter().any(|g| g.open);
                for g in &mut self.groups.groups {
                    if g.open == any_open {
                        fold::start(ctx, fold_id(g.id), !any_open);
                    }
                    g.open = !any_open;
                }
                self.save_groups();
            }
            PanelAct::Move(c, to) => {
                let all = self.conn_ids();
                if self.groups.place(c, to, &all) {
                    self.save_groups();
                }
            }
            PanelAct::NewGroup(name) => {
                let name = name.trim();
                if !name.is_empty() {
                    self.groups.new_group(name);
                    self.save_groups();
                }
            }
            PanelAct::Rename(g, name) => {
                let name = name.trim();
                if let Some(g) = self.groups.group_mut(g).filter(|_| !name.is_empty()) {
                    g.name = name.into();
                    self.save_groups();
                }
            }
            PanelAct::Color(g, color) => {
                if let Some(g) = self.groups.group_mut(g) {
                    g.color = color.into();
                    self.save_groups();
                }
            }
            PanelAct::ConnectAll(g) => self.connect_group(g),
            PanelAct::DisconnectAll(g) => self.disconnect_group(g),
            PanelAct::NewConnIn(g) => {
                self.new_conn_modal();
                if let Some(Modal::Conn(f)) = &mut self.modal {
                    f.group = g;
                }
            }
            PanelAct::EditGroup(g) => self.edit_group(g),
            PanelAct::DeleteGroup(g) => self.delete_group(g),
            PanelAct::EditConn(id) => self.edit_conn(id),
            PanelAct::Connect(id) => {
                self.active = Some(id);
                if !self.demo {
                    self.connect(id);
                }
            }
            PanelAct::Disconnect(id) => self.disconnect(id),
            PanelAct::Ungroup(id) => {
                let all = self.conn_ids();
                if self.groups.place(id, Place::Loose(None), &all) {
                    self.save_groups();
                }
            }
            PanelAct::Remove(id) => {
                let name = self.conn(id).map(|c| c.meta.name.clone()).unwrap_or_default();
                self.modal = Some(Modal::Typed {
                    title: format!("Remove the saved connection \"{name}\"? Its address is deleted from the Keychain. No data is touched."),
                    expect: name,
                    typed: String::new(),
                    action: Danger::RemoveConnection(id),
                });
            }
        }
    }

    /// Connects every member of a group that is not open yet. The Connections panel and the rail's folder menu share it.
    pub(super) fn connect_group(&mut self, g: u64) {
        let ids: Vec<u64> = self.groups.group(g).map(|g| g.members.clone()).unwrap_or_default();
        for id in ids {
            if self.conn(id).is_some_and(|c| c.status.can_connect()) && !self.demo {
                self.connect(id);
            }
        }
    }

    /// Closes every open member of a group, with one message for the whole group.
    pub(super) fn disconnect_group(&mut self, g: u64) {
        let Some(group) = self.groups.group(g) else { return };
        let (ids, name) = (group.members.clone(), group.name.clone());
        let mut n = 0;
        for id in ids {
            if self.conn(id).is_some_and(|c| c.status.is_open()) && self.drop_session(id).is_some() {
                n += 1;
            }
        }
        // One message for the whole group, where `disconnect` would raise one per connection.
        self.ok(if n == 1 { format!("Disconnected 1 connection in {name}") } else { format!("Disconnected {n} connections in {name}") });
    }

    /// Deletes a group. Its members are in no group afterwards, and the toast offers Undo.
    pub(super) fn delete_group(&mut self, id: u64) {
        let Some(at) = self.groups.groups.iter().position(|g| g.id == id) else { return };
        let g = self.groups.groups.remove(at);
        let all = self.conn_ids();
        // The members follow the connections that were already loose, in saved order.
        self.groups.loose = self.groups.loose_order(&all);
        self.save_groups();
        let (name, n) = (g.name.clone(), g.members.len());
        let msg = match n {
            0 => format!("Group {name} deleted."),
            1 => format!("Group {name} deleted. Its connection is in no group now."),
            _ => format!("Group {name} deleted. Its {n} connections are in no group now."),
        };
        self.toast_restore(msg, Restore::Group { g, at });
    }

    /// QUARRY_TEST_CONNS=<state>: panel, filter, menu, new, edit, drag or picker.
    pub(super) fn demo_conns(&mut self, state: &str) {
        let db_ = |id: u64, name: &str, tag: &str, uri: &str, engine: db::sql::Engine, status: Status| Conn { masked_uri: uri.into(), ..Conn::new(ConnMeta { id, name: name.into(), tag: tag.into(), engine, ..Default::default() }, status) };
        use db::sql::Engine::{MySql, Postgres};
        let mongo = db::sql::Engine::default();
        self.conns.extend([
            db_(11, "prod-eu", "Production", "mongodb+srv://app:****@prod-eu.example.net", mongo, Status::Ready),
            db_(12, "prod-us", "Production", "mongodb+srv://app:****@prod-us.example.net", mongo, Status::Idle),
            db_(13, "billing", "Production", "postgres://billing:****@pg.internal:5432/billing", Postgres, Status::Ready),
            db_(14, "reports", "Staging", "mysql://app:****@reports.internal:3306", MySql, Status::Idle),
            db_(15, "scratch", "", "mongodb://localhost:27018", mongo, Status::Idle),
            db_(16, "analytics", "", "postgres://ro:****@analytics.internal:5432/analytics", Postgres, Status::Ready),
            db_(17, "sandbox", "Development", "mongodb://sandbox.internal:27017", mongo, Status::Idle),
        ]);
        let all = self.conn_ids();
        let (prod, stage, local) = (self.groups.new_group("Production"), self.groups.new_group("Staging"), self.groups.new_group("Local"));
        for (c, g) in [(11, prod), (12, prod), (13, prod), (2, stage), (14, stage), (1, local), (15, local)] {
            self.groups.place(c, Place::Into(g, None), &all);
        }
        if let Some(g) = self.groups.group_mut(local) {
            g.open = false;
        }
        for (g, c) in [(prod, "red"), (stage, "amber"), (local, "teal")] {
            if let Some(g) = self.groups.group_mut(g) {
                g.color = c.into();
            }
        }
        self.rail_order = vec![1, 11, 13, 16];
        self.active = Some(11);
        self.prefs.set_side_mode(SideMode::Connections);
        self.prefs.sidebar_hidden = false;
        match state {
            "filter" => self.conns_ui.filter = "prod".into(),
            "menu" => self.conns_ui.demo_menu = Some(prod),
            "new" => {
                self.conns_ui.adding = Some("Analytics".into());
                self.conns_ui.focus_edit = true;
            }
            "edit" => self.edit_group(prod),
            "drag" => self.conns_ui.demo_drop = Some(DemoDrop::Into(stage)),
            "picker" => {
                self.prefs.set_side_mode(SideMode::Explorer);
                self.conns_ui.demo_picker = true;
            }
            _ => {}
        }
    }
}

/// The rows, top to bottom, with their menus and drops. A group's members are drawn in its fold, which grows and shrinks
/// as the group opens and closes; the members of a group still closing are only drawn, since for the keys and the drops
/// the group is closed.
fn draw_rows(ui: &mut egui::Ui, rows: &[CRow], cx: &Cx, st: &mut ConnsUi, acts: &mut Vec<PanelAct>) {
    let filtering = !cx.toks.is_empty();
    let ctx = ui.ctx().clone();
    let mut nav: Vec<(egui::Id, Nav)> = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        let CRow::Group { id, open, .. } = rows[i] else {
            draw_row(ui, rows, i, cx, st, acts, &ctx, &mut nav, 1.0);
            i += 1;
            continue;
        };
        let t = if filtering { 1.0 } else { fold::openness(&ctx, fold_id(id), open) };
        draw_row(ui, rows, i, cx, st, acts, &ctx, &mut nav, t);
        i += 1;
        let end = i + rows[i..].iter().take_while(|r| matches!(r, CRow::Conn { group: Some(g), .. } | CRow::Hint(g) if *g == id)).count();
        let gap = ui.spacing().item_spacing.y;
        let guess: f32 = rows[i..end].iter().map(|r| if matches!(r, CRow::Hint(_)) { HINT_H } else { CONN_H } + gap).sum();
        let from = i;
        fold::body(ui, fold_id(id), t, guess, open, |ui| {
            // Rows of a closing group stay out of the arrow keys, which must not land on a row about to vanish.
            let mut gone = Vec::new();
            for k in from..end {
                draw_row(ui, rows, k, cx, st, acts, &ctx, if open { &mut nav } else { &mut gone }, 1.0);
            }
        });
        i = end;
    }
    arrow_keys(ui, &nav, acts);
    // The room under the last row takes a connection out to the end of the loose ones.
    let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ui.available_height().max(40.0)), Sense::hover());
    if !filtering && let Some(m) = held(&ctx) && r.contains_pointer() {
        paint_spot(ui.painter(), rect.with_max_y(rect.top() + 2.0), Spot { zone: Zone::Before, depth: 0 }, rect.left() + 8.0, MEMBER_INDENT);
        if r.dnd_release_payload::<DragItem>().is_some() {
            acts.push(PanelAct::Move(m, Place::Loose(None)));
        }
    }
}

/// One row with its menu and drop. `t` is how far open a group row's fold is, for its chevron.
#[allow(clippy::too_many_arguments)]
fn draw_row(ui: &mut egui::Ui, rows: &[CRow], i: usize, cx: &Cx, st: &mut ConnsUi, acts: &mut Vec<PanelAct>, ctx: &egui::Context, nav: &mut Vec<(egui::Id, Nav)>, t: f32) {
    let filtering = !cx.toks.is_empty();
    let row = &rows[i];
    {
        match row {
            CRow::Caption { text, note } => {
                let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), CAPTION_H), Sense::hover());
                let t = ui.painter().layout_job(LayoutJob::single_section(text.to_string(), ui_fmt(TXT_SECTION, MUTED(), W_SEMIBOLD, 0.6)));
                ui.painter().galley(egui::pos2(rect.left() + 8.0, rect.center().y - t.size().y / 2.0), t, MUTED());
                if !note.is_empty() {
                    ui.painter().text(egui::pos2(rect.right() - 8.0, rect.center().y), egui::Align2::RIGHT_CENTER, note, FontId::proportional(TXT_CAPTION), FAINT());
                }
                r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, *text));
                drop_target(ui, &r, rows, i, cx, acts);
            }
            CRow::GroupsEnd => {
                let done = match st.adding.as_mut() {
                    Some(text) => new_group_field(ui, text, &mut st.focus_edit, cx.groups, acts),
                    None => false,
                };
                if done {
                    st.adding = None;
                }
            }
            CRow::Hint(gid) => {
                let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), HINT_H), Sense::hover());
                let color = cx.groups.group(*gid).map_or(FAINT(), |g| group_color(&g.color));
                ui.painter().vline(rect.left() + GUIDE_X, rect.y_range(), Stroke::new(1.0, tint(color, 128)));
                let g = ui.painter().layout_no_wrap("Drag connections here".into(), FontId::proportional(TXT_SMALL), FAINT());
                ui.painter().galley(egui::pos2(rect.left() + MEMBER_INDENT + 8.0, rect.center().y - g.size().y / 2.0), g, FAINT());
                r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, "Drag connections here"));
                drop_target(ui, &r, rows, i, cx, acts);
            }
            CRow::Group { id, open, shown, total } => {
                let Some(g) = cx.groups.group(*id) else { return };
                if let Some(rid) = group_row(ui, g, (*open, t, *shown, *total), cx, st, acts, ctx, rows, i) {
                    nav.push((rid, Nav::Group { id: *id, open: *open }));
                }
            }
            CRow::Conn { id, group, .. } => {
                let Some(c) = cx.conns.iter().find(|c| c.meta.id == *id) else { return };
                let color = group.and_then(|g| cx.groups.group(g)).map(|g| group_color(&g.color));
                let hl = if filtering { hl_ranges(&c.meta.name, cx.toks) } else { Vec::new() };
                let on = cx.active == Some(c.meta.id);
                // The row's own id, known before it is drawn, so a focused row can make room for its menu button.
                let rid = ui.next_auto_id();
                let dots_id = egui::Id::unique(("conn-dots", c.meta.id));
                let hot = row_hot(ui, ctx, rid, dots_id, CONN_H);
                let r = conn_row_room(ui, c, on, if color.is_some() { MEMBER_INDENT } else { 0.0 }, &hl, !filtering, if hot { DOTS_ROOM } else { 0.0 });
                nav.push((r.id, Nav::Conn { group: *group }));
                if let Some(col) = color {
                    ui.painter().vline(r.rect.left() + GUIDE_X, r.rect.y_range().expand(1.0), Stroke::new(1.0, tint(col, 128)));
                }
                if r.clicked() {
                    acts.push(PanelAct::Activate(c.meta.id));
                }
                // The ghost is the row itself, without its indent, so it reads the same wherever it is carried.
                drag_source_with(ui, &r, DragItem::Conn(c.meta.id), |p, rect| paint_conn_row(p, egui::Rect::from_min_max(rect.min + egui::vec2(12.0, 0.0), rect.max - egui::vec2(8.0, 0.0)), c, false, &[]));
                drop_target(ui, &r, rows, i, cx, acts);
                conn_menu(&r, c, group.is_some(), acts);
                if hot {
                    let dots = row_dots(ui, dots_id, r.rect, &format!("More for {}", c.meta.name));
                    egui::Popup::menu(&dots).width(220.0).show(|ui| conn_menu_items(ui, c, group.is_some(), acts));
                }
            }
        }
    }
}

/// Room a row keeps at its right end for its menu button.
const DOTS_ROOM: f32 = 28.0;

/// Whether the row about to be drawn with id `rid` and height `h` shows its menu button: the pointer is on it, it has the
/// keyboard focus, or its menu is open. Never during a drag, where the button would hide the drop marks.
fn row_hot(ui: &egui::Ui, ctx: &egui::Context, rid: egui::Id, dots: egui::Id, h: f32) -> bool {
    let est = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), h));
    held(ctx).is_none() && (ui.rect_contains_pointer(est) || ui.memory(|m| m.has_focus(rid) || m.has_focus(dots)) || egui::Popup::is_id_open(ctx, dots.with("popup")))
}

/// The quiet dots button at the right end of a row, so its menu is found without a right click.
fn row_dots(ui: &mut egui::Ui, id: egui::Id, row: egui::Rect, name: &str) -> egui::Response {
    let at = egui::Rect::from_center_size(egui::pos2(row.right() - 16.0, row.center().y), egui::vec2(22.0, 22.0));
    let r = ui.interact(at, id, Sense::click());
    let state = state_of(&r);
    ui.painter().rect_filled(at, R_SM, hover_fill(state));
    paint_dots(ui.painter(), at.center(), if state > 0 { BRIGHT() } else { MUTED() });
    focus_ring(ui, &r, at, R_SM as f32);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, name));
    r.on_hover_text("More").on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Up and Down move between the rows, Right opens a folded group, Left folds an open one or goes up to a member's group.
/// Return is the row's own click, which egui gives a focused widget. The focused row keeps the arrows, so egui does not
/// also move the focus to whatever widget lies in that direction.
fn arrow_keys(ui: &mut egui::Ui, nav: &[(egui::Id, Nav)], acts: &mut Vec<PanelAct>) {
    let Some(i) = nav.iter().position(|(id, _)| ui.memory(|m| m.has_focus(*id))) else { return };
    let arrows = egui::EventFilter { horizontal_arrows: true, vertical_arrows: true, ..Default::default() };
    ui.memory_mut(|m| m.set_focus_lock_filter(nav[i].0, arrows));
    let key = |ui: &mut egui::Ui, k: Key| ui.input_mut(|inp| inp.consume_key(egui::Modifiers::NONE, k));
    let focus = |ui: &mut egui::Ui, id: egui::Id| ui.memory_mut(|m| m.request_focus(id));
    if key(ui, Key::ArrowDown) {
        if let Some((id, _)) = nav.get(i + 1) {
            focus(ui, *id);
        }
    } else if key(ui, Key::ArrowUp) {
        if let Some((id, _)) = i.checked_sub(1).and_then(|j| nav.get(j)) {
            focus(ui, *id);
        }
    } else if key(ui, Key::ArrowRight) {
        if let Nav::Group { id, open: false } = nav[i].1 {
            acts.push(PanelAct::Toggle(id));
        }
    } else if key(ui, Key::ArrowLeft) {
        match nav[i].1 {
            Nav::Group { id, open: true } => acts.push(PanelAct::Toggle(id)),
            Nav::Conn { group: Some(g) } => {
                if let Some((id, _)) = nav[..i].iter().rev().find(|(_, n)| matches!(n, Nav::Group { id, .. } if *id == g)) {
                    focus(ui, *id);
                }
            }
            _ => {}
        }
    }
}

/// The connection being dragged, if a drag of one is on and Esc has not cancelled it.
fn held(ctx: &egui::Context) -> Option<u64> {
    if drag_cancelled(ctx) {
        return None;
    }
    match egui::DragAndDrop::payload::<DragItem>(ctx).as_deref() {
        Some(DragItem::Conn(id)) => Some(*id),
        _ => None,
    }
}

/// Marks a row as a drop target while a connection is dragged over it and, on release, asks for the move.
fn drop_target(ui: &egui::Ui, r: &egui::Response, rows: &[CRow], i: usize, cx: &Cx, acts: &mut Vec<PanelAct>) {
    // Indexes of a filtered list are not places in the full one, and a row of a closing group is no longer in the list.
    if !cx.toks.is_empty() || !ui.is_enabled() {
        return;
    }
    let Some(m) = held(ui.ctx()).filter(|_| r.contains_pointer()) else { return };
    let frac = y_frac(r.rect, ui.ctx().pointer_interact_pos());
    let Some(place) = drop_place(cx.groups, cx.all, m, rows, i, frac) else { return };
    // The mark follows what the drop does, so the edges of a group row, which put the connection into it, show the fill.
    // A line's depth is the list it lands in: a group's members are one step in.
    let zone = match (&rows[i], drop_slot(&rows[i], frac)) {
        (CRow::Conn { .. }, Zone::Before) => Zone::Before,
        (CRow::Conn { .. }, _) | (CRow::Caption { .. }, _) | (CRow::Group { open: true, .. }, Zone::After) => Zone::After,
        _ => Zone::Into,
    };
    let depth = match &rows[i] {
        CRow::Conn { group: Some(_), .. } | CRow::Group { .. } => 1,
        _ => 0,
    };
    paint_spot(ui.painter(), r.rect, Spot { zone, depth }, r.rect.left() + 8.0, MEMBER_INDENT);
    if r.dnd_release_payload::<DragItem>().is_some() {
        acts.push(PanelAct::Move(m, place));
    }
}

/// The group row: chevron, folder, name, the dots of its open members when closed, the count. Returns the row's id for the
/// arrow keys, or None while it is renamed; clicks, the menu, F2 and the inline rename are handled here.
#[allow(clippy::too_many_arguments)]
fn group_row(ui: &mut egui::Ui, g: &Group, (open, t, shown, total): (bool, f32, usize, usize), cx: &Cx, st: &mut ConnsUi, acts: &mut Vec<PanelAct>, ctx: &egui::Context, rows: &[CRow], i: usize) -> Option<egui::Id> {
    let col = group_color(&g.color);
    let filtering = !cx.toks.is_empty();
    let dots: Vec<Color32> = if open { Vec::new() } else { g.members.iter().filter_map(|id| cx.conns.iter().find(|c| c.meta.id == *id)).filter(|c| c.status.is_open()).take(DOTS_MAX).map(|c| status_color(&c.status)).collect() };
    let hl = if filtering { hl_ranges(&g.name, cx.toks) } else { Vec::new() };
    let renaming = st.rename.as_ref().is_some_and(|(id, _)| *id == g.id);
    let r = if renaming {
        let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), GROUP_H), Sense::hover());
        fold::chevron(ui.painter(), egui::pos2(rect.left() + 14.0, rect.center().y), t, FAINT());
        paint_folder(ui.painter(), egui::pos2(rect.left() + 32.0, rect.center().y), col, true);
        let field = egui::Rect::from_min_max(egui::pos2(rect.left() + 46.0, rect.top() + 2.0), egui::pos2(rect.right() - 4.0, rect.bottom() - 2.0));
        let focus = std::mem::take(&mut st.focus_edit);
        if let Some((_, text)) = st.rename.as_mut() {
            let e = ui.put(field, egui::TextEdit::singleline(text).id(egui::Id::unique(("group-rename", g.id))).desired_width(field.width()).margin(egui::Margin { left: 8, right: 8, top: 4, bottom: 4 }));
            e.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Group name"));
            if focus {
                e.request_focus();
            }
            // Leaving the field keeps the name, as in the Library; only Esc throws it away.
            let esc = ui.input(|inp| inp.key_pressed(Key::Escape));
            if e.lost_focus() || esc {
                if !esc {
                    acts.push(PanelAct::Rename(g.id, text.clone()));
                }
                st.rename = None;
            }
        }
        r
    } else {
        let label = format!("Group {}", g.name);
        let dots_id = egui::Id::unique(("group-dots", g.id));
        let hot = row_hot(ui, ctx, ui.next_auto_id(), dots_id, GROUP_H);
        let r = side_row(ui, GROUP_H, 0.0, false, &label, |p, rect| {
            let cy = rect.center().y;
            fold::chevron(p, egui::pos2(rect.left() + 6.0, cy), t, FAINT());
            paint_folder(p, egui::pos2(rect.left() + 24.0, cy), col, true);
            let count = if filtering { shown.to_string() } else { total.to_string() };
            let cg = p.layout_no_wrap(count, FontId::proportional(TXT_CAPTION), FAINT());
            let mut right = rect.right() - 2.0 - if hot { DOTS_ROOM } else { 0.0 };
            p.galley(egui::pos2(right - cg.size().x, cy - cg.size().y / 2.0), cg.clone(), FAINT());
            right -= cg.size().x + 8.0;
            for dot in &dots {
                p.circle_filled(egui::pos2(right - 3.0, cy), 3.0, *dot);
                right -= 9.0;
            }
            let mut job = LayoutJob::default();
            let plain = ui_fmt(TXT_BODY, TEXT(), W_SEMIBOLD, 0.0);
            let marked = egui::TextFormat { background: HIGHLIGHT(), ..plain.clone() };
            let mut at = 0;
            for &(a, b) in &hl {
                job.append(&g.name[at..a], 0.0, plain.clone());
                job.append(&g.name[a..b], 0.0, marked.clone());
                at = b;
            }
            job.append(&g.name[at..], 0.0, plain);
            job.wrap = egui::text::TextWrapping::truncate_at_width((right - rect.left() - 40.0).max(0.0));
            let name = p.layout_job(job);
            p.galley(egui::pos2(rect.left() + 40.0, cy - name.size().y / 2.0), name, TEXT());
        });
        if r.clicked() && !filtering {
            acts.push(PanelAct::Toggle(g.id));
        }
        if r.has_focus() && ui.input_mut(|inp| inp.consume_key(egui::Modifiers::NONE, Key::F2)) {
            st.rename = Some((g.id, g.name.clone()));
            st.focus_edit = true;
        }
        r.context_menu(|ui| group_menu(ui, g, cx, st, acts));
        if hot {
            let dots = row_dots(ui, dots_id, r.rect, &format!("More for group {}", g.name));
            egui::Popup::menu(&dots).width(250.0).show(|ui| group_menu(ui, g, cx, st, acts));
        }
        r
    };
    let nav_id = (!renaming).then_some(r.id);
    drop_target(ui, &r, rows, i, cx, acts);
    // A drag that rests on a closed group opens it, so a connection can be dropped among its members.
    let held_over = !open && !filtering && held(ctx).is_some() && r.contains_pointer();
    if hover_open(ctx, r.id, held_over) {
        acts.push(PanelAct::Toggle(g.id));
    }
    match st.demo_drop {
        Some(DemoDrop::Into(id)) if id == g.id => {
            drop_fill(ui.painter(), r.rect);
            // The ghost a real drag draws beside the pointer, for the picture.
            let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("demo-drag-ghost")));
            let galley = p.layout_no_wrap("sandbox".into(), FontId::proportional(TXT_BODY), TEXT());
            let rect = egui::Rect::from_min_size(r.rect.center() + egui::vec2(24.0, 10.0), galley.size() + egui::vec2(24.0, 10.0));
            p.rect(rect, R_SM, RAISED(), Stroke::new(1.0, PRIMARY()), egui::StrokeKind::Inside);
            p.galley(rect.min + egui::vec2(12.0, 5.0), galley, TEXT());
        }
        _ => {}
    }
    if st.demo_menu == Some(g.id) {
        let pos = r.rect.right_top() + egui::vec2(-24.0, 20.0);
        egui::Area::new(egui::Id::unique("demo-group-menu")).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
            egui::Frame::menu(ui.style()).show(ui, |ui| {
                ui.set_width(250.0);
                group_menu(ui, g, cx, st, acts);
            });
        });
    }
    nav_id
}

/// The menu of a group row.
fn group_menu(ui: &mut egui::Ui, g: &Group, cx: &Cx, st: &mut ConnsUi, acts: &mut Vec<PanelAct>) {
    menu_look(ui);
    ui.spacing_mut().item_spacing.y = 2.0;
    ui.spacing_mut().button_padding = egui::vec2(10.0, 5.0);
    if ui.add(egui::Button::new("Rename").shortcut_text("F2")).clicked() {
        st.rename = Some((g.id, g.name.clone()));
        st.focus_edit = true;
        ui.close();
    }
    ui.separator();
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        caption(ui, "COLOR");
    });
    ui.add_space(2.0);
    if let Some(c) = color_swatches(ui, &g.color) {
        acts.push(PanelAct::Color(g.id, c));
        ui.close();
    }
    ui.separator();
    let members: Vec<&Conn> = g.members.iter().filter_map(|id| cx.conns.iter().find(|c| c.meta.id == *id)).collect();
    let can_connect = members.iter().any(|c| c.status.can_connect());
    let can_disconnect = members.iter().any(|c| c.status.is_open());
    if ui.add_enabled(can_connect, egui::Button::new("Connect all")).clicked() {
        acts.push(PanelAct::ConnectAll(g.id));
        ui.close();
    }
    if ui.add_enabled(can_disconnect, egui::Button::new("Disconnect all")).clicked() {
        acts.push(PanelAct::DisconnectAll(g.id));
        ui.close();
    }
    ui.separator();
    if ui.button(format!("New connection in {}…", g.name)).clicked() {
        acts.push(PanelAct::NewConnIn(Some(g.id)));
        ui.close();
    }
    if ui.button("Edit group…").clicked() {
        acts.push(PanelAct::EditGroup(g.id));
        ui.close();
    }
    ui.separator();
    if danger_item(ui, "Delete group").clicked() {
        acts.push(PanelAct::DeleteGroup(g.id));
        ui.close();
    }
}

/// The menu of a connection row, on a right click.
fn conn_menu(r: &egui::Response, c: &Conn, grouped: bool, acts: &mut Vec<PanelAct>) {
    r.context_menu(|ui| conn_menu_items(ui, c, grouped, acts));
}

/// What the right click and the dots button of a connection row offer.
fn conn_menu_items(ui: &mut egui::Ui, c: &Conn, grouped: bool, acts: &mut Vec<PanelAct>) {
    let id = c.meta.id;
    let open = c.status.is_open();
    menu_look(ui);
    if ui.button("Browse databases").clicked() {
        acts.push(PanelAct::Browse(id));
        ui.close();
    }
    if ui.button(if open { "Reconnect" } else { "Connect" }).clicked() {
        acts.push(PanelAct::Connect(id));
        ui.close();
    }
    if open && ui.button("Disconnect").clicked() {
        acts.push(PanelAct::Disconnect(id));
        ui.close();
    }
    if ui.button("Edit connection…").clicked() {
        acts.push(PanelAct::EditConn(id));
        ui.close();
    }
    if grouped && ui.button("Remove from group").clicked() {
        acts.push(PanelAct::Ungroup(id));
        ui.close();
    }
    ui.separator();
    if danger_item(ui, format!("Remove \"{}\"…", c.meta.name)).clicked() {
        acts.push(PanelAct::Remove(id));
        ui.close();
    }
}

/// The field for a new group, with the color it will get. Returns true once it is done, by Enter or by leaving it.
fn new_group_field(ui: &mut egui::Ui, text: &mut String, focus: &mut bool, groups: &Groups, acts: &mut Vec<PanelAct>) -> bool {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), GROUP_H), Sense::hover());
    let next = GROUP_COLORS.iter().map(|c| c.0).find(|c| !groups.groups.iter().any(|g| g.color == *c)).unwrap_or(GROUP_COLORS[groups.groups.len() % GROUP_COLORS.len()].0);
    chevron(ui.painter(), egui::pos2(rect.left() + 6.0, rect.center().y), true, FAINT());
    paint_folder(ui.painter(), egui::pos2(rect.left() + 24.0, rect.center().y), group_color(next), true);
    let field = egui::Rect::from_min_max(egui::pos2(rect.left() + 38.0, rect.top() + 2.0), egui::pos2(rect.right() - 4.0, rect.bottom() - 2.0));
    let e = ui.put(field, egui::TextEdit::singleline(text).id(egui::Id::unique("group-new")).hint_text(RichText::new("Group name").color(FAINT())).desired_width(field.width()).margin(egui::Margin { left: 8, right: 8, top: 4, bottom: 4 }));
    e.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "New group name"));
    if std::mem::take(focus) {
        e.request_focus();
    }
    // Leaving the field makes the group, as Enter does; only Esc throws the name away. An empty name makes nothing.
    let esc = ui.input(|i| i.key_pressed(Key::Escape));
    if e.lost_focus() || esc {
        if !esc {
            acts.push(PanelAct::NewGroup(text.clone()));
        }
        return true;
    }
    false
}

/// A row of the eight group colors. Returns the name clicked.
pub(super) fn color_swatches(ui: &mut egui::Ui, current: &str) -> Option<&'static str> {
    let mut picked = None;
    let size = egui::vec2(ACCENT_CELL * GROUP_COLORS.len() as f32, ACCENT_CELL);
    // A left-to-right box, so Tab moves through the swatches in reading order even inside a right-to-left slot.
    ui.allocate_ui_with_layout(size, egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (name, ..) in GROUP_COLORS {
            let (rect, r) = ui.allocate_exact_size(egui::vec2(ACCENT_CELL, ACCENT_CELL), Sense::click());
            let on = name == current;
            r.widget_info(|| egui::WidgetInfo::selected(egui::Role::RadioButton, true, on, name));
            let r = r.on_hover_text(name);
            if r.clicked() && !on {
                picked = Some(name);
            }
            let color = group_color(name);
            let c = rect.center();
            let radius = ACCENT_DOT_R * 0.7;
            ui.painter().circle_filled(c, radius, color);
            if let Some(s) = pick_ring(on, &r, color) {
                ui.painter().circle_stroke(c, radius + 3.0, s);
            }
            focus_ring(ui, &r, egui::Rect::from_center_size(c, egui::Vec2::splat(2.0 * radius + 6.0)), radius + 3.0);
        }
    });
    picked
}

// The picker in the explorer.

/// The filter field at the top of the picker.
pub(super) fn picker_filter(ui: &mut egui::Ui, text: &mut String, focus: bool) {
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        let r = ui.add(egui::TextEdit::singleline(text).id(egui::Id::unique("picker-filter")).hint_text(RichText::new("Filter connections").color(FAINT())).desired_width(ui.available_width() - 6.0).margin(egui::Margin { left: 10, right: 10, top: 5, bottom: 5 }));
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Filter connections"));
        if focus {
            r.request_focus();
        }
    });
    ui.add_space(2.0);
}

/// The picker's list: the connections under group captions that fold. Returns the connection clicked.
pub(super) fn picker_list(ui: &mut egui::Ui, conns: &[Conn], groups: &Groups, st: &mut ConnsUi, active: Option<u64>, scroll_to_active: bool) -> Option<u64> {
    picker_list_in(ui, conns, groups, st, active, scroll_to_active, &[], &|_| None)
}

/// `picker_list` for a dialog that adds connections: the ones in `skip` are left out (a group with none left goes too), and
/// `note` puts a line at the right of a row, such as where the connection moves from.
#[allow(clippy::too_many_arguments)]
pub(super) fn picker_list_in(ui: &mut egui::Ui, conns: &[Conn], groups: &Groups, st: &mut ConnsUi, active: Option<u64>, scroll_to_active: bool, skip: &[u64], note: &dyn Fn(&Conn) -> Option<String>) -> Option<u64> {
    let toks = tokens(&st.picker_filter);
    let rows = panel_rows(conns, groups, &st.picker_filter, |g| !st.picker_closed.contains(&g.id));
    let left = |g: &Group| g.members.iter().any(|m| !skip.contains(m) && conns.iter().any(|c| c.meta.id == *m));
    let mut pick = None;
    let mut fold = None;
    let mut any = false;
    for row in &rows {
        match row {
            CRow::Group { id, open, shown, total } => {
                let Some(g) = groups.group(*id) else { continue };
                if !left(g) {
                    continue;
                }
                any = true;
                let col = group_color(&g.color);
                let n = if toks.is_empty() { *total } else { *shown };
                let r = side_row(ui, CAPTION_H + 4.0, 0.0, false, &format!("Fold {}", g.name), |p, rect| {
                    let cy = rect.center().y;
                    chevron(p, egui::pos2(rect.left() + 8.0, cy), *open, FAINT());
                    paint_folder(p, egui::pos2(rect.left() + 26.0, cy), col, true);
                    let t = p.layout_job(LayoutJob::single_section(g.name.to_uppercase(), ui_fmt(TXT_SECTION, MUTED(), W_SEMIBOLD, 0.6)));
                    p.galley(egui::pos2(rect.left() + 40.0, cy - t.size().y / 2.0), t, MUTED());
                    let cg = p.layout_no_wrap(n.to_string(), FontId::proportional(TXT_CAPTION), FAINT());
                    p.galley(egui::pos2(rect.right() - 4.0 - cg.size().x, cy - cg.size().y / 2.0), cg, FAINT());
                });
                if r.clicked() && toks.is_empty() {
                    fold = Some(g.id);
                }
            }
            CRow::Conn { id, group, .. } => {
                let Some(c) = conns.iter().find(|c| c.meta.id == *id).filter(|c| !skip.contains(&c.meta.id)) else { continue };
                any = true;
                let on = active == Some(c.meta.id);
                let hl = hl_ranges(&c.meta.name, &toks);
                let line = note(c).map(|t| ui.painter().layout_no_wrap(t, FontId::proportional(TXT_CAPTION), WARNING()));
                let room = line.as_ref().map_or(0.0, |g| g.size().x + 12.0);
                let r = conn_row_room(ui, c, on, if group.is_some() { 10.0 } else { 0.0 }, &hl, false, room);
                if let Some(g) = line {
                    let at = egui::pos2(r.rect.right() - 8.0 - g.size().x, r.rect.center().y - g.size().y / 2.0);
                    ui.painter().galley(at, g, WARNING());
                }
                if on && scroll_to_active {
                    r.scroll_to_me(Some(egui::Align::Center));
                }
                if r.clicked() {
                    pick = Some(c.meta.id);
                }
            }
            CRow::Caption { text, note } => {
                ui.horizontal(|ui| {
                    ui.add_space(10.0);
                    caption(ui, if note.is_empty() { text.to_string() } else { format!("{text}  {note}") });
                });
            }
            CRow::Hint(_) | CRow::GroupsEnd => {}
        }
    }
    if !any && !toks.is_empty() {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            ui.label(RichText::new("No connection matches").color(MUTED()));
        });
    }
    if let Some(g) = fold {
        if !st.picker_closed.remove(&g) {
            st.picker_closed.insert(g);
        }
    }
    pick
}

// Painted icons, since glyphs from fallback fonts sit off center.

/// A folder, filled and outlined in `col` when `filled`.
pub(super) fn paint_folder(p: &egui::Painter, c: egui::Pos2, col: Color32, filled: bool) {
    let v = egui::vec2;
    let tab = egui::Rect::from_min_max(c + v(-6.5, -5.5), c + v(-0.5, -2.0));
    let body = egui::Rect::from_min_max(c + v(-6.5, -3.5), c + v(6.5, 5.5));
    let s = Stroke::new(1.3, col);
    let fill = if filled { tint(col, 90) } else { Color32::TRANSPARENT };
    p.rect(tab, 1.5, fill, s, egui::StrokeKind::Inside);
    p.rect(body, 2.0, fill, s, egui::StrokeKind::Inside);
}

/// The Glyph of the Edit group dialog's tile.
pub(super) fn folder_glyph(p: &egui::Painter, c: egui::Pos2, col: Color32) {
    paint_folder(p, c, col, false);
}

fn paint_plus(p: &egui::Painter, c: egui::Pos2, col: Color32) {
    let s = Stroke::new(1.4, col);
    p.line_segment([c + egui::vec2(-4.5, 0.0), c + egui::vec2(4.5, 0.0)], s);
    p.line_segment([c + egui::vec2(0.0, -4.5), c + egui::vec2(0.0, 4.5)], s);
}

fn paint_new_group(p: &egui::Painter, c: egui::Pos2, col: Color32) {
    paint_folder(p, c + egui::vec2(-1.5, 0.5), col, false);
    let s = Stroke::new(1.3, col);
    let k = c + egui::vec2(5.5, 4.5);
    p.line_segment([k + egui::vec2(-2.5, 0.0), k + egui::vec2(2.5, 0.0)], s);
    p.line_segment([k + egui::vec2(0.0, -2.5), k + egui::vec2(0.0, 2.5)], s);
}

/// Two chevrons pointing at each other, for folding every group.
fn paint_fold_all(p: &egui::Painter, c: egui::Pos2, col: Color32) {
    let s = Stroke::new(1.4, col);
    let v = egui::vec2;
    p.add(egui::Shape::line(vec![c + v(-4.0, -5.0), c + v(0.0, -1.5), c + v(4.0, -5.0)], s));
    p.add(egui::Shape::line(vec![c + v(-4.0, 5.0), c + v(0.0, 1.5), c + v(4.0, 5.0)], s));
}

/// A quiet 24 px square button with a painted icon.
fn icon_btn(ui: &mut egui::Ui, name: &str, paint: fn(&egui::Painter, egui::Pos2, Color32)) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), Sense::click());
    let state = state_of(&r);
    ui.painter().rect_filled(rect, R_SM, hover_fill(state));
    paint(ui.painter(), rect.center(), if state > 0 { BRIGHT() } else { MUTED() });
    focus_ring(ui, &r, rect, R_SM as f32);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, name));
    r.on_hover_text(name).on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(id: u64, name: &str, tag: &str, uri: &str) -> Conn {
        Conn { masked_uri: uri.into(), ..Conn::new(ConnMeta { id, name: name.into(), tag: tag.into(), ..Default::default() }, Status::Idle) }
    }

    /// Conns 1..=6; Prod holds 1 and 2, Dev holds 3; 4, 5 and 6 are loose.
    fn fixture() -> (Vec<Conn>, Groups) {
        let conns = vec![conn(1, "prod-eu", "Production", "mongodb://eu.example.net"), conn(2, "prod-us", "Production", ""), conn(3, "dev-box", "Development", "mongodb://localhost"), conn(4, "scratch", "", ""), conn(5, "analytics", "", "postgres://a.example.net"), conn(6, "sandbox", "", "")];
        let mut g = Groups::default();
        let (a, b) = (g.new_group("Prod"), g.new_group("Dev"));
        let all: Vec<u64> = conns.iter().map(|c| c.meta.id).collect();
        for (c, to) in [(1, a), (2, a), (3, b)] {
            g.place(c, Place::Into(to, None), &all);
        }
        (conns, g)
    }

    fn kinds(rows: &[CRow]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                CRow::Group { id, open, shown, total } => format!("G{id}{}{shown}/{total}", if *open { "+" } else { "-" }),
                CRow::Conn { id, group, pos } => format!("c{id}@{}{pos}", group.map_or("L".to_string(), |g| g.to_string())),
                CRow::Hint(g) => format!("hint{g}"),
                CRow::Caption { text, note } => format!("{text}:{note}"),
                CRow::GroupsEnd => "end".into(),
            })
            .collect()
    }

    #[test]
    fn rows_list_groups_then_the_loose_connections_under_a_caption() {
        let (conns, groups) = fixture();
        let rows = panel_rows(&conns, &groups, "", |g| g.open);
        assert_eq!(kinds(&rows), ["G1+2/2", "c1@10", "c2@11", "G2+1/1", "c3@20", "end", "NOT IN A GROUP:3", "c4@L0", "c5@L1", "c6@L2"]);
        // Without groups the list is plain: no caption, no end of groups other than the marker.
        let rows = panel_rows(&conns, &Groups::default(), "", |g| g.open);
        assert_eq!(kinds(&rows)[0], "end");
        assert!(!kinds(&rows).iter().any(|k| k.starts_with("NOT")));
    }

    #[test]
    fn a_closed_group_shows_no_members_and_an_empty_open_one_a_hint() {
        let (conns, mut groups) = fixture();
        groups.group_mut(1).unwrap().open = false;
        let empty = groups.new_group("Empty");
        let rows = panel_rows(&conns, &groups, "", |g| g.open);
        assert_eq!(&kinds(&rows)[..5], ["G1-2/2", "G2+1/1", "c3@20", "G3+0/0", "hint3"]);
        assert_eq!(empty, 3);
        // The picker folds by its own rule.
        let rows = panel_rows(&conns, &groups, "", |g| g.id != 2);
        assert!(kinds(&rows).contains(&"G2-1/1".to_string()));
    }

    #[test]
    fn a_filter_opens_the_groups_with_a_match_and_hides_the_rest() {
        let (conns, mut groups) = fixture();
        groups.group_mut(1).unwrap().open = false;
        let rows = panel_rows(&conns, &groups, "us", |g| g.open);
        // prod-us is in the folded Prod group, which opens for it; Dev has no match and goes.
        assert_eq!(kinds(&rows), ["MATCHES:1 of 6", "G1+1/2", "c2@11", "end"]);
        // The stored flag is untouched.
        assert!(!groups.group(1).unwrap().open);
        // A token matches the tag and the address too, and loose matches follow the groups under their caption.
        let rows = panel_rows(&conns, &groups, "example", |g| g.open);
        assert_eq!(kinds(&rows), ["MATCHES:2 of 6", "G1+1/2", "c1@10", "end", "NOT IN A GROUP:1", "c5@L1"]);
        // The name of a group brings all its members.
        let rows = panel_rows(&conns, &groups, "dev", |g| g.open);
        assert_eq!(kinds(&rows), ["MATCHES:1 of 6", "G2+1/1", "c3@20", "end"]);
        // Nothing matches: only the caption and the marker.
        assert_eq!(kinds(&panel_rows(&conns, &groups, "zzzz", |g| g.open)), ["MATCHES:0 of 6", "end"]);
    }

    #[test]
    fn the_matched_text_is_found_without_regard_to_case() {
        let toks = tokens("PROD us");
        assert_eq!(hl_ranges("prod-US", &toks), [(0, 4), (5, 7)]);
        assert_eq!(hl_ranges("Prod-prod", &tokens("prod")), [(0, 4)]);
        assert!(hl_ranges("scratch", &tokens("zz")).is_empty());
        // Overlapping matches merge, and a name with multi-byte letters keeps its offsets.
        assert_eq!(hl_ranges("pro-prod", &tokens("pro prod")), [(0, 3), (4, 8)]);
        assert_eq!(hl_ranges("Zürich-prod", &tokens("prod")), [(8, 12)]);
    }

    #[test]
    fn the_middle_of_a_group_row_is_into_and_elsewhere_before_or_after() {
        let g = CRow::Group { id: 1, open: true, shown: 2, total: 2 };
        let c = CRow::Conn { id: 1, group: Some(1), pos: 0 };
        assert_eq!([0.0, 0.2, 0.25, 0.5, 0.75, 0.8, 1.0].map(|f| drop_slot(&g, f)), [Zone::Before, Zone::Before, Zone::Into, Zone::Into, Zone::Into, Zone::After, Zone::After]);
        assert_eq!([0.0, 0.49, 0.5, 1.0].map(|f| drop_slot(&c, f)), [Zone::Before, Zone::Before, Zone::After, Zone::After]);
        assert_eq!(drop_slot(&CRow::Hint(1), 0.0), Zone::Into);
    }

    #[test]
    fn a_drop_resolves_to_a_place_counting_the_list_without_the_moved_connection() {
        let (conns, groups) = fixture();
        let all: Vec<u64> = conns.iter().map(|c| c.meta.id).collect();
        let rows = panel_rows(&conns, &groups, "", |g| g.open);
        let at = |kind: &str| rows.iter().position(|r| kinds(std::slice::from_ref(r))[0] == kind).unwrap();
        // On a group row: into it at the end, or first under an open header from its lower edge.
        assert_eq!(drop_place(&groups, &all, 4, &rows, at("G2+1/1"), 0.5), Some(Place::Into(2, None)));
        assert_eq!(drop_place(&groups, &all, 4, &rows, at("G2+1/1"), 0.9), Some(Place::Into(2, Some(0))));
        // Before a member of another group: that member's place. After it: the next place.
        assert_eq!(drop_place(&groups, &all, 4, &rows, at("c2@11"), 0.1), Some(Place::Into(1, Some(1))));
        assert_eq!(drop_place(&groups, &all, 4, &rows, at("c2@11"), 0.9), Some(Place::Into(1, Some(2))));
        // Inside one list, below a later row: one less, because the connection leaves its old place first.
        assert_eq!(drop_place(&groups, &all, 1, &rows, at("c2@11"), 0.9), Some(Place::Into(1, Some(1))));
        assert_eq!(drop_place(&groups, &all, 2, &rows, at("c1@10"), 0.1), Some(Place::Into(1, Some(0))));
        // Loose rows, the caption and a connection on itself.
        assert_eq!(drop_place(&groups, &all, 6, &rows, at("c4@L0"), 0.1), Some(Place::Loose(Some(0))));
        assert_eq!(drop_place(&groups, &all, 4, &rows, at("c6@L2"), 0.9), Some(Place::Loose(Some(2))));
        assert_eq!(drop_place(&groups, &all, 1, &rows, at("NOT IN A GROUP:3"), 0.5), Some(Place::Loose(Some(0))));
        assert_eq!(drop_place(&groups, &all, 4, &rows, at("c4@L0"), 0.5), None);
        assert_eq!(drop_place(&groups, &all, 4, &rows, at("end"), 0.5), None);
    }

    #[test]
    fn dragging_a_member_below_its_neighbour_reorders_it() {
        let (conns, mut groups) = fixture();
        let all: Vec<u64> = conns.iter().map(|c| c.meta.id).collect();
        let rows = panel_rows(&conns, &groups, "", |g| g.open);
        // 1 dropped on the lower half of 2 goes after it.
        let i = rows.iter().position(|r| matches!(r, CRow::Conn { id: 2, .. })).unwrap();
        let place = drop_place(&groups, &all, 1, &rows, i, 0.9).unwrap();
        groups.place(1, place, &all);
        assert_eq!(groups.group(1).unwrap().members, [2, 1]);
    }
}
