//! The activity rail: a thin column of icons left of the sidebar for the app's panels.

use super::*;

#[derive(Clone, Copy)]
pub(super) enum RailIcon {
    Explorer,
    Connections,
    Library,
    Search,
    Log,
    Server,
    Settings,
}

impl App {
    pub(super) fn rail(&mut self, ui: &mut egui::Ui) {
        let frame = egui::Frame::new().fill(CHROME()).inner_margin(egui::Margin { left: 8, right: 8, top: 10, bottom: 10 });
        egui::Panel::left("rail").exact_size(RAIL_W).resizable(false).show_separator_line(false).frame(frame).show(ui, |ui| {
            let full = ui.max_rect().expand2(egui::vec2(8.0, 10.0));
            ui.painter().vline(full.right() - 0.5, full.y_range(), Stroke::new(1.0, LINE()));
            ui.spacing_mut().item_spacing.y = 8.0;
            let connected = self.active.and_then(|id| self.conn(id)).is_some_and(|c| c.client.is_some() || c.sql.is_some());
            let palette = matches!(self.modal, Some(Modal::Palette { .. }));
            let mode = self.prefs.side_mode();
            let shown = !self.prefs.sidebar_hidden;
            if rail_button(ui, RailIcon::Explorer, shown && mode == SideMode::Explorer, true, "Database explorer").clicked() {
                self.pick_side_mode(SideMode::Explorer);
            }
            if conns_panel::READY && rail_button(ui, RailIcon::Connections, shown && mode == SideMode::Connections, true, "Saved connections").clicked() {
                self.pick_side_mode(SideMode::Connections);
            }
            if library::panel::READY && rail_button(ui, RailIcon::Library, shown && mode == SideMode::Library, true, "Script library").clicked() {
                self.pick_side_mode(SideMode::Library);
            }
            if rail_button(ui, RailIcon::Search, palette, true, "Jump to a collection (Cmd+K)").clicked() {
                self.open_palette();
            }
            if rail_button(ui, RailIcon::Log, self.log.open, true, "Query log (Cmd+L)").clicked() {
                self.log.open = !self.log.open;
            }
            let tip = if connected { "Server status" } else { "Server status (connect first)" };
            if rail_button(ui, RailIcon::Server, self.server.is_some() || self.sql_server.is_some() || self.pg_server.is_some(), connected, tip).clicked()
                && let Some(conn) = self.active
            {
                self.open_server(conn);
            }
            self.conn_shortcuts(ui);
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                if rail_button(ui, RailIcon::Settings, matches!(self.modal, Some(Modal::Settings)), true, "Settings (Cmd+,)").clicked() {
                    self.modal = Some(Modal::Settings);
                }
            });
        });
    }
}

impl App {
    /// A click on a panel button of the rail: the panel that is showing folds the sidebar away, any other shows in it.
    pub(super) fn pick_side_mode(&mut self, m: SideMode) {
        if !self.prefs.sidebar_hidden && self.prefs.side_mode() == m {
            self.prefs.sidebar_hidden = true;
        } else {
            self.prefs.set_side_mode(m);
            self.prefs.sidebar_hidden = false;
        }
        self.save_prefs();
    }

    /// One square per open connection under the panel buttons, with a folded group as one tile, scrolling when they outgrow the rail.
    fn conn_shortcuts(&mut self, ui: &mut egui::Ui) {
        let open: Vec<u64> = self.conns.iter().filter(|c| matches!(c.status, Status::Ready | Status::Connecting)).map(|c| c.meta.id).collect();
        sync_order(&mut self.rail_order, &open);
        if self.rail_order.is_empty() {
            return;
        }
        let (line, _) = ui.allocate_exact_size(egui::vec2(28.0, 1.0), Sense::hover());
        ui.painter().hline(line.shrink2(egui::vec2(6.0, 0.0)).x_range(), line.center().y, Stroke::new(1.0, LINE()));
        // Leaves room for the Settings button, which sits at the bottom of the rail.
        let room = (ui.available_height() - 36.0).max(28.0);
        let items = rail_items(&self.rail_order, &self.groups);
        let mut pick = None;
        let mut act = None;
        let mut toggle = None;
        let mut drops: Vec<RailDrop> = Vec::new();
        egui::ScrollArea::vertical().max_height(room).auto_shrink([false, true]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.spacing_mut().scroll.bar_width = 3.0;
            // A capsule is wider than the 28 px column, so the scroll area's clip must not cut its sides.
            ui.set_clip_rect(ui.clip_rect().expand2(egui::vec2(6.0, 0.0)));
            for item in &items {
                match item {
                    RailItem::Conn(id) => self.conn_tile(ui, *id, DropOn::Loose(*id), &mut pick, &mut act, &mut drops),
                    RailItem::Folder { gid, members } => {
                        let Some(g) = self.groups.group(*gid) else { continue };
                        let color = group_color(&g.color);
                        let open_now = self.rail_open.contains(gid);
                        // The tile sits at the first open member in rail order, which is not always the group's first.
                        let anchor = folder_anchor(&self.rail_order, members);
                        let marks: Vec<&str> = members.iter().filter_map(|m| self.conn(*m)).map(|c| engine_mark(c.meta.engine, &c.server.product).0).collect();
                        let tip = format!("{} \u{b7} {} connection{}, {} open", g.name, g.members.len(), if g.members.len() == 1 { "" } else { "s" }, members.len());
                        // Reserved before the tile so the capsule tint lies under it and under its drop mark.
                        let bg = open_now.then(|| ui.painter().add(egui::Shape::Noop));
                        let (r, rect) = folder_tile(ui, &g.name, color, open_now, &marks, &tip);
                        if r.clicked() {
                            toggle = Some(*gid);
                        }
                        let top = if open_now { DropOn::FolderTop { gid: *gid, anchor } } else { DropOn::Folder { gid: *gid, anchor } };
                        tile_drop(ui, &r, rect, top, &self.groups, &mut drops);
                        if let Some(bg) = bg {
                            let mut span = rect;
                            ui.scope(|ui| {
                                ui.spacing_mut().item_spacing.y = 4.0;
                                for m in members {
                                    self.conn_tile(ui, *m, DropOn::Member { id: *m, gid: *gid }, &mut pick, &mut act, &mut drops);
                                    span = span.union(ui.min_rect());
                                }
                            });
                            ui.painter().set(bg, egui::Shape::rect_filled(span.expand2(egui::vec2(3.0, 3.0)), R_LG, tint(color, 36)));
                        }
                    }
                }
            }
        });
        if let Some(id) = pick {
            self.active = Some(id);
        }
        if let Some(gid) = toggle
            && !self.rail_open.remove(&gid)
        {
            self.rail_open.insert(gid);
        }
        // Both change state after the frame was drawn.
        if toggle.is_some() || !drops.is_empty() {
            ui.ctx().request_repaint();
        }
        for d in drops {
            self.apply_rail_drop(d);
        }
        match act {
            Some((id, 0)) => {
                self.active = Some(id);
                self.connect(id);
            }
            Some((id, 1)) => self.edit_conn(id),
            Some((id, _)) => self.disconnect(id),
            None => {}
        }
    }

    /// One connection's tile with its context menu, drag source and drop target.
    fn conn_tile(&self, ui: &mut egui::Ui, id: u64, target: DropOn, pick: &mut Option<u64>, act: &mut Option<(u64, u8)>, drops: &mut Vec<RailDrop>) {
        let Some(c) = self.conns.iter().find(|c| c.meta.id == id) else { return };
        let r = conn_shortcut(ui, c, self.active == Some(id));
        if r.clicked() {
            *pick = Some(id);
        }
        crate::ui::dnd::drag_source(ui, &r, DragItem::Conn(id), &c.meta.name);
        tile_drop(ui, &r, r.rect, target, &self.groups, drops);
        r.context_menu(|ui| {
            menu_look(ui);
            if ui.button("Reconnect").clicked() {
                *act = Some((id, 0));
            }
            if ui.button("Edit connection\u{2026}").clicked() {
                *act = Some((id, 1));
            }
            if danger_item(ui, "Disconnect").clicked() {
                *act = Some((id, 2));
            }
        });
    }

    /// Applies one drop on the rail: the groups are saved, and the toast offers Undo.
    fn apply_rail_drop(&mut self, d: RailDrop) {
        let before = self.groups.clone();
        let all = self.conn_ids();
        let name = |a: &App, id: u64| a.conn(id).map(|c| c.meta.name.clone()).unwrap_or_default();
        match d {
            RailDrop::Reorder { conn, anchor, after } => {
                if !self.rail_order.contains(&conn) {
                    return;
                }
                // A member of a folded group dropped between tiles leaves its group. An unfolded one is loose in the rail already.
                let left = self.groups.group_of(conn).filter(|g| g.fold_rail).map(|g| g.name.clone());
                if let Some(g) = left {
                    self.groups.place(conn, Place::Loose(None), &all);
                    self.toast_restore(format!("{} left {g}.", name(self, conn)), Restore::Groups(Box::new(before)));
                }
                move_next_to(&mut self.rail_order, conn, anchor, after);
            }
            RailDrop::Into { conn, gid, at } => {
                let joined = self.groups.group_of(conn).map(|g| g.id) != Some(gid);
                if self.groups.place(conn, Place::Into(gid, at), &all) && joined {
                    let g = self.groups.group(gid).map(|g| g.name.clone()).unwrap_or_default();
                    // The folder sits at its first open member, so a tile from above it must follow the group or the folder jumps.
                    let members = self.groups.group(gid).map(|g| g.members.clone()).unwrap_or_default();
                    if let Some(last) = self.rail_order.iter().copied().filter(|c| *c != conn && members.contains(c)).last() {
                        move_next_to(&mut self.rail_order, conn, last, true);
                    }
                    self.toast_restore(format!("{} added to {g}.", name(self, conn)), Restore::Groups(Box::new(before)));
                }
            }
            RailDrop::Pair { conn, target } => {
                let gid = self.groups.new_group("New group");
                self.groups.place(target, Place::Into(gid, None), &all);
                self.groups.place(conn, Place::Into(gid, None), &all);
                self.rail_open.insert(gid);
                move_next_to(&mut self.rail_order, conn, target, true);
                self.toast_restore_edit(format!("Grouped {} and {}.", name(self, target), name(self, conn)), Restore::Groups(Box::new(before)), Some(gid));
            }
        }
        self.save_groups();
        self.save_prefs();
    }

    pub(super) fn open_server(&mut self, conn: u64) {
        self.req += 1;
        match self.conn(conn).map(|c| (c.meta.engine, c.sql.as_ref().map(|d| d.dialect), c.meta.advanced.clone())) {
            Some((db::sql::Engine::MySql, dialect, adv)) => self.sql_server = Some(SqlServerWin::new(conn, dialect.map(|d| db::sql::server::set_by_quarry(&adv, d, db::sql::advanced::local_offset_secs())).unwrap_or_default())),
            Some((db::sql::Engine::Postgres, _, adv)) => self.pg_server = Some(PgServerWin::new(conn, db::sql::pg::server::set_by_quarry(&adv))),
            _ => self.server = Some(ServerWin::new(conn, self.req)),
        }
    }

    /// Drops the session but keeps the saved connection, which then leaves the rail.
    pub(super) fn disconnect(&mut self, id: u64) {
        let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == id) else { return };
        (c.client, c.sql, c.tunnel, c.status, c.writable) = (None, None, None, Status::Idle, false);
        c.forget_lists();
        self.pg_tables_asked.forget(id, None, None);
        let name = c.meta.name.clone();
        self.ok(format!("Disconnected from {name}"));
    }
}

/// Keeps the ids still open in their old order and appends the newly open ones, so a shortcut
/// never jumps when another connection opens or closes.
fn sync_order(order: &mut Vec<u64>, open: &[u64]) {
    order.retain(|id| open.contains(id));
    for id in open {
        if !order.contains(id) {
            order.push(*id);
        }
    }
}

/// What the rail draws, top to bottom: a connection tile or a folded group's tile.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum RailItem {
    Conn(u64),
    /// A group with `fold_rail` that has an open member. `members` are its open ones, in the group's own order.
    Folder { gid: u64, members: Vec<u64> },
}

/// Walks `order` (the open connections in rail order) and puts one `Folder` where the first open member of a folded
/// group sits. A group that does not fold leaves its members loose.
pub(super) fn rail_items(order: &[u64], groups: &Groups) -> Vec<RailItem> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for &id in order {
        match groups.group_of(id).filter(|g| g.fold_rail) {
            Some(g) if seen.insert(g.id) => out.push(RailItem::Folder { gid: g.id, members: g.members.iter().copied().filter(|m| order.contains(m)).collect() }),
            Some(_) => {}
            None => out.push(RailItem::Conn(id)),
        }
    }
    out
}

/// The tile a dragged connection is over, which decides what dropping there does.
#[derive(Clone, Copy, Debug)]
enum DropOn {
    /// A tile in no folded group: the middle makes a group, the edges reorder.
    Loose(u64),
    /// A member tile: the halves are slots among the group's members.
    Member { id: u64, gid: u64 },
    /// A closed folder: the middle adds to the group, the edges reorder around its first open member.
    Folder { gid: u64, anchor: u64 },
    /// The top tile of an open capsule.
    FolderTop { gid: u64, anchor: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RailDrop {
    Reorder { conn: u64, anchor: u64, after: bool },
    Into { conn: u64, gid: u64, at: Option<usize> },
    Pair { conn: u64, target: u64 },
}

/// How a drop is marked on the tile: an insertion line at its top or bottom, or a fill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mark {
    Line { top: bool },
    Fill,
}

/// Top third, middle and bottom third of a tile, by the pointer's height.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Zone {
    Before,
    Onto,
    After,
}

fn zone_of(rect: egui::Rect, y: f32) -> Zone {
    let f = (y - rect.top()) / rect.height().max(1.0);
    if f < 1.0 / 3.0 {
        Zone::Before
    } else if f > 2.0 / 3.0 {
        Zone::After
    } else {
        Zone::Onto
    }
}

/// The index `Groups::place` wants to put `dragged` next to `target` in group `gid`: it counts the members after the dragged one leaves.
fn slot_in_group(groups: &Groups, gid: u64, target: u64, after: bool, dragged: u64) -> Option<usize> {
    let rest: Vec<u64> = groups.group(gid)?.members.iter().copied().filter(|m| *m != dragged).collect();
    rest.iter().position(|m| *m == target).map(|i| i + after as usize)
}

/// What dropping `dragged` at `zone` of `on` does, or None when nothing sensible. A member tile has no middle: Onto counts as After there.
fn resolve_drop(on: DropOn, zone: Zone, dragged: u64, groups: &Groups) -> Option<(RailDrop, Mark)> {
    let reorder = |anchor: u64, after: bool| (anchor != dragged).then_some((RailDrop::Reorder { conn: dragged, anchor, after }, Mark::Line { top: !after }));
    let in_group = |gid: u64| groups.group(gid).is_some_and(|g| g.members.contains(&dragged));
    match on {
        DropOn::Loose(id) if id == dragged => None,
        DropOn::Loose(id) => match zone {
            Zone::Before => reorder(id, false),
            Zone::After => reorder(id, true),
            Zone::Onto => Some((RailDrop::Pair { conn: dragged, target: id }, Mark::Fill)),
        },
        DropOn::Member { id, .. } if id == dragged => None,
        DropOn::Member { id, gid } => {
            let after = zone != Zone::Before;
            let at = slot_in_group(groups, gid, id, after, dragged)?;
            Some((RailDrop::Into { conn: dragged, gid, at: Some(at) }, Mark::Line { top: !after }))
        }
        DropOn::Folder { gid, .. } if in_group(gid) => None,
        DropOn::Folder { gid, anchor } => match zone {
            Zone::Before => reorder(anchor, false),
            Zone::After => reorder(anchor, true),
            Zone::Onto => Some((RailDrop::Into { conn: dragged, gid, at: None }, Mark::Fill)),
        },
        DropOn::FolderTop { gid, anchor } => match zone {
            Zone::Before => reorder(anchor, false),
            Zone::Onto if in_group(gid) => None,
            Zone::Onto => Some((RailDrop::Into { conn: dragged, gid, at: None }, Mark::Fill)),
            Zone::After => Some((RailDrop::Into { conn: dragged, gid, at: Some(0) }, Mark::Line { top: false })),
        },
    }
}

/// Marks `rect` while a connection is dragged over it and, on release, queues what the drop does.
fn tile_drop(ui: &egui::Ui, r: &egui::Response, rect: egui::Rect, on: DropOn, groups: &Groups, out: &mut Vec<RailDrop>) {
    let Some(item) = r.dnd_hover_payload::<DragItem>() else { return };
    let DragItem::Conn(dragged) = *item else { return };
    let Some(pos) = ui.ctx().pointer_interact_pos() else { return };
    let Some((drop, mark)) = resolve_drop(on, zone_of(rect, pos.y), dragged, groups) else { return };
    let p = ui.painter();
    match mark {
        Mark::Line { top } => crate::ui::dnd::drop_line(p, rect.x_range().expand(2.0), if top { rect.top() - 4.0 } else { rect.bottom() + 4.0 }),
        Mark::Fill => crate::ui::dnd::drop_fill(p, rect),
    }
    if r.dnd_release_payload::<DragItem>().is_some() {
        out.push(drop);
    }
}

/// A 28 px tile for a folded group: tinted in the group color with a bar on its left edge, holding a 2x2 grid of its
/// members' engine logos while closed and an open folder while open. Returns the click response and the tile's rect.
fn folder_tile(ui: &mut egui::Ui, name: &str, color: Color32, open: bool, marks: &[&str], tip: &str) -> (egui::Response, egui::Rect) {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), Sense::click());
    let state = state_of(&r);
    let alpha = [if open { 72 } else { 56 }, 84, 104][state];
    let p = ui.painter();
    p.rect_filled(rect, R_MD, tint(color, alpha));
    p.rect_filled(egui::Rect::from_min_size(rect.left_top() + egui::vec2(0.0, 6.0), egui::vec2(2.0, 16.0)), 1, color);
    if open {
        paint_open_folder(p, rect.center(), color);
    } else {
        let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
        for (i, mark) in marks.iter().take(4).enumerate() {
            let Some(tex) = engine_logo(ui.ctx(), mark) else { continue };
            let at = rect.center() + egui::vec2(if i % 2 == 0 { -5.5 } else { 5.5 }, if i < 2 { -5.5 } else { 5.5 });
            ui.painter().image(tex.id(), egui::Rect::from_center_size(at, egui::vec2(9.0, 9.0)), uv, if state > 0 { BRIGHT() } else { mark_color(mark) });
        }
    }
    focus_ring(ui, &r, rect, R_MD as f32);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{name} folder")));
    (r.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand), rect)
}

/// An open folder drawn as shapes, since glyphs from fallback fonts sit off center: the back with its tab and a slanted front.
fn paint_open_folder(p: &egui::Painter, c: egui::Pos2, color: Color32) {
    let v = egui::vec2;
    let s = Stroke::new(1.3, color);
    p.line(vec![c + v(-6.5, 4.5), c + v(-6.5, -5.0), c + v(-2.5, -5.0), c + v(-1.0, -3.0), c + v(5.0, -3.0), c + v(5.0, -1.0)], s);
    p.add(egui::Shape::closed_line(vec![c + v(-6.5, 4.5), c + v(-4.5, -1.0), c + v(7.5, -1.0), c + v(5.5, 4.5)], s));
}

/// The connection a folder's tile sits at: its first open member in rail order, which is not always the group's first.
fn folder_anchor(order: &[u64], members: &[u64]) -> u64 {
    order.iter().copied().find(|id| members.contains(id)).unwrap_or(members[0])
}

/// Moves `conn` to just before or after `anchor` in the rail order. An anchor that is gone, or is `conn` itself, puts it last.
fn move_next_to(order: &mut Vec<u64>, conn: u64, anchor: u64, after: bool) {
    // A connection dragged in from the sidebar may be closed, and a closed one has no tile to place.
    if !order.contains(&conn) {
        return;
    }
    order.retain(|c| *c != conn);
    let at = order.iter().position(|c| *c == anchor).map_or(order.len(), |i| i + after as usize);
    order.insert(at, conn);
}

/// Two-letter mark and product name. MariaDB shares the MySQL engine, so only the server
/// reply, once connected, tells it apart.
pub(super) fn engine_mark(engine: db::sql::Engine, product: &str) -> (&'static str, &'static str) {
    match (engine, product) {
        (db::sql::Engine::Mongo, _) => ("MO", "MongoDB"),
        (db::sql::Engine::Postgres, _) => ("PG", "PostgreSQL"),
        (_, "MariaDB") => ("MA", "MariaDB"),
        _ => ("MY", "MySQL"),
    }
}

/// The colors of the connection form's engine cards, so an engine reads the same everywhere.
pub(super) fn mark_color(mark: &str) -> Color32 {
    match mark {
        "MO" => C_STRING(),
        "MA" => C_KEYWORD(),
        "PG" => ENGINE_PG(),
        _ => C_LINK(),
    }
}

/// The engine's logo as a white mask, tinted when painted. Decoded once and kept in egui's memory.
/// The shapes are Simple Icons' (CC0), baked to 64 px PNGs so the binary needs no SVG renderer.
pub(super) fn engine_logo(ctx: &egui::Context, mark: &str) -> Option<egui::TextureHandle> {
    if mark == "PG" {
        // No permissively licensed PostgreSQL mark was available offline, so the "PG" monogram is drawn here.
        let id = egui::Id::unique("engine-postgres");
        return ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)).or_else(|| {
            let tex = ctx.load_texture("engine-postgres", pg_monogram(), egui::TextureOptions::LINEAR);
            ctx.data_mut(|d| d.insert_temp(id, tex.clone()));
            Some(tex)
        });
    }
    let (name, png): (&str, &[u8]) = match mark {
        "MO" => ("engine-mongodb", include_bytes!("../../assets/engines/mongodb.png")),
        "MA" => ("engine-mariadb", include_bytes!("../../assets/engines/mariadb.png")),
        _ => ("engine-mysql", include_bytes!("../../assets/engines/mysql.png")),
    };
    let id = egui::Id::unique(name);
    ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)).or_else(|| {
        let icon = eframe::icon_data::from_png_bytes(png).ok()?;
        let img = egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
        let tex = ctx.load_texture(name, img, egui::TextureOptions::LINEAR);
        ctx.data_mut(|d| d.insert_temp(id, tex.clone()));
        Some(tex)
    })
}

/// "PG" as a white 64 px mask, rasterized from two stroked outlines by distance to the segments.
fn pg_monogram() -> egui::ColorImage {
    const P: &[(f32, f32)] = &[(14.0, 52.0), (14.0, 12.0), (26.0, 12.0), (32.0, 16.0), (34.0, 23.0), (32.0, 30.0), (26.0, 34.0), (14.0, 34.0)];
    const G: &[(f32, f32)] = &[(58.0, 21.0), (53.0, 14.0), (47.0, 12.0), (41.0, 15.0), (38.0, 22.0), (38.0, 42.0), (41.0, 49.0), (47.0, 52.0), (54.0, 50.0), (58.0, 44.0), (58.0, 36.0), (48.0, 36.0)];
    const HALF: f32 = 3.5;
    fn dist(p: egui::Vec2, a: (f32, f32), b: (f32, f32)) -> f32 {
        let (a, b) = (egui::vec2(a.0, a.1), egui::vec2(b.0, b.1));
        let ab = b - a;
        let t = ((p - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0);
        (p - (a + ab * t)).length()
    }
    let mut img = egui::ColorImage::filled([64, 64], Color32::TRANSPARENT);
    for y in 0..64 {
        for x in 0..64 {
            let p = egui::vec2(x as f32 + 0.5, y as f32 + 0.5);
            let d = [P, G].iter().flat_map(|line| line.windows(2).map(|w| dist(p, w[0], w[1]))).fold(f32::MAX, f32::min);
            let a = ((HALF + 0.5 - d).clamp(0.0, 1.0) * 255.0) as u8;
            img.pixels[y * 64 + x] = Color32::from_rgba_unmultiplied(255, 255, 255, a);
        }
    }
    img
}

/// A 28 px square with the engine logo, a status dot at the bottom right and, for a tagged
/// connection, a bar in the tag color on the left edge.
fn conn_shortcut(ui: &mut egui::Ui, c: &Conn, active: bool) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), Sense::click_and_drag());
    let state = state_of(&r);
    let (mark, product) = engine_mark(c.meta.engine, &c.server.product);
    let color = mark_color(mark);
    let fill = if active { tint(PRIMARY(), if state == 2 { 90 } else { 64 }) } else { hover_fill(state) };
    let p = ui.painter();
    p.rect_filled(rect, R_MD, fill);
    if !active && state == 0 {
        p.rect_filled(rect.shrink(3.0), R_SM, tint(color, 40));
    }
    let fg = if active || state > 0 { BRIGHT() } else { color };
    if let Some(tex) = engine_logo(ui.ctx(), mark) {
        let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
        p.image(tex.id(), egui::Rect::from_center_size(rect.center(), egui::vec2(16.0, 16.0)), uv, fg);
    }
    if !c.meta.tag.is_empty() {
        p.rect_filled(egui::Rect::from_min_size(rect.left_top() + egui::vec2(0.0, 6.0), egui::vec2(2.0, 16.0)), 1, tag_color(&c.meta.tag));
    }
    // A ring in the rail's own color keeps the dot readable over the tinted square.
    let dot = rect.right_bottom() - egui::vec2(4.5, 4.5);
    p.circle_filled(dot, 4.5, CHROME());
    p.circle_filled(dot, 3.0, status_color(&c.status));
    focus_ring(ui, &r, rect, R_MD as f32);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Switch to {}", c.meta.name)));
    let mut tip = c.meta.name.clone();
    if !c.meta.tag.is_empty() {
        tip += &format!("  ({})", c.meta.tag);
    }
    tip += &format!("\n{product}");
    if !c.server.version.is_empty() {
        tip += &format!(" {}", c.server.version);
    }
    tip += if c.status == Status::Connecting { "\nConnecting" } else if c.writable { "\nWrites on" } else { "\nRead-only" };
    r.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A 28 px square icon button: a `HOVER` fill on hover, `PRESSED` while held, and an accent
/// tint with a bright icon while its panel is open.
fn rail_button(ui: &mut egui::Ui, icon: RailIcon, active: bool, enabled: bool, tip: &str) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), if enabled { Sense::click() } else { Sense::hover() });
    // A disabled button only senses hover, so it never reports a press.
    let state = state_of(&r);
    let fill = if active { tint(PRIMARY(), if state == 2 { 90 } else { 64 }) } else if enabled { hover_fill(state) } else { Color32::TRANSPARENT };
    let p = ui.painter();
    p.rect_filled(rect, R_MD, fill);
    let color = if !enabled { FAINT() } else if active || state > 0 { BRIGHT() } else { MUTED() };
    paint_icon(p, rect.center(), icon, Stroke::new(1.3, color));
    focus_ring(ui, &r, rect, R_MD as f32);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, tip));
    let r = r.on_hover_text(tip);
    if enabled { r.on_hover_cursor(egui::CursorIcon::PointingHand) } else { r }
}

/// Line icons drawn in a 16 px box, so they need no icon font.
pub(super) fn paint_icon(p: &egui::Painter, c: egui::Pos2, icon: RailIcon, s: Stroke) {
    let v = egui::vec2;
    match icon {
        // A window with a left panel.
        RailIcon::Explorer => {
            let r = egui::Rect::from_center_size(c, v(15.0, 13.0));
            p.rect_stroke(r, 2.5, s, egui::StrokeKind::Middle);
            p.vline(r.left() + 5.0, r.y_range(), s);
        }
        // A database cylinder: the top ellipse, the sides and two curves below.
        RailIcon::Connections => {
            let arc = |dy: f32, from: f32, to: f32| -> Vec<egui::Pos2> { (0..=16).map(|i| from + (to - from) * i as f32 / 16.0).map(|a| c + v(6.0 * a.cos(), dy + 2.4 * a.sin())).collect() };
            p.add(egui::Shape::closed_line(arc(-4.8, 0.0, std::f32::consts::TAU), s));
            p.line_segment([c + v(-6.0, -4.8), c + v(-6.0, 4.8)], s);
            p.line_segment([c + v(6.0, -4.8), c + v(6.0, 4.8)], s);
            p.line(arc(0.0, 0.0, std::f32::consts::PI), s);
            p.line(arc(4.8, 0.0, std::f32::consts::PI), s);
        }
        // A book: the cover with its spine and two lines of text.
        RailIcon::Library => {
            let r = egui::Rect::from_center_size(c, v(11.5, 14.0));
            p.rect_stroke(r, 2.0, s, egui::StrokeKind::Middle);
            p.vline(r.left() + 3.0, r.y_range(), s);
            for dy in [-2.5, 1.0] {
                p.line_segment([c + v(-1.0, dy), c + v(3.0, dy)], s);
            }
        }
        RailIcon::Search => {
            p.circle_stroke(c + v(-1.5, -1.5), 5.0, s);
            p.line_segment([c + v(2.2, 2.2), c + v(6.0, 6.0)], s);
        }
        // Rows of a log, with a bullet each.
        RailIcon::Log => {
            for dy in [-4.5, 0.0, 4.5] {
                p.circle_filled(c + v(-5.5, dy), 1.1, s.color);
                p.line_segment([c + v(-2.5, dy), c + v(6.5, dy)], s);
            }
        }
        // A pulse line.
        RailIcon::Server => {
            let pts = [v(-7.5, 0.5), v(-4.0, 0.5), v(-2.0, -5.0), v(1.0, 5.5), v(3.0, 0.5), v(7.5, 0.5)];
            p.line(pts.iter().map(|d| c + *d).collect(), s);
        }
        // A gear: ring, hub and eight teeth.
        RailIcon::Settings => {
            p.circle_stroke(c, 4.6, s);
            p.circle_stroke(c, 1.8, s);
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::FRAC_PI_4;
                let d = v(a.cos(), a.sin());
                p.line_segment([c + d * 4.6, c + d * 7.0], s);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_keep_the_order_they_opened_in() {
        let mut order = vec![];
        sync_order(&mut order, &[3, 1]);
        assert_eq!(order, [3, 1]);
        sync_order(&mut order, &[1, 2, 3]);
        assert_eq!(order, [3, 1, 2]);
        sync_order(&mut order, &[2, 3]);
        assert_eq!(order, [3, 2]);
    }

    fn groups_of(sets: &[(&str, bool, &[u64])]) -> Groups {
        let mut g = Groups::default();
        for (name, fold, members) in sets {
            let id = g.new_group(name);
            let gr = g.group_mut(id).unwrap();
            (gr.fold_rail, gr.members) = (*fold, members.to_vec());
        }
        g
    }

    #[test]
    fn a_folded_group_is_one_item_where_its_first_open_member_sits() {
        let g = groups_of(&[("Prod", true, &[2, 4, 9])]);
        // 9 is not open, so it is not listed; 4 comes before 2 in the rail but the group's order wins for members.
        assert_eq!(rail_items(&[1, 4, 3, 2], &g), [RailItem::Conn(1), RailItem::Folder { gid: 1, members: vec![2, 4] }, RailItem::Conn(3)]);
        assert_eq!(rail_items(&[], &g), []);
    }

    #[test]
    fn a_group_that_does_not_fold_leaves_its_members_loose() {
        let g = groups_of(&[("Prod", false, &[1, 2])]);
        assert_eq!(rail_items(&[1, 3, 2], &g), [RailItem::Conn(1), RailItem::Conn(3), RailItem::Conn(2)]);
    }

    #[test]
    fn a_folder_follows_sync_order_as_connections_open_and_close() {
        let g = groups_of(&[("Prod", true, &[5, 6])]);
        let mut order = vec![];
        sync_order(&mut order, &[1, 6]);
        assert_eq!(rail_items(&order, &g), [RailItem::Conn(1), RailItem::Folder { gid: 1, members: vec![6] }]);
        sync_order(&mut order, &[1, 6, 5]);
        assert_eq!(rail_items(&order, &g), [RailItem::Conn(1), RailItem::Folder { gid: 1, members: vec![5, 6] }]);
        // The first open member closing moves the tile to the next one, and the last closing removes it.
        sync_order(&mut order, &[1, 5]);
        assert_eq!(rail_items(&order, &g), [RailItem::Conn(1), RailItem::Folder { gid: 1, members: vec![5] }]);
        sync_order(&mut order, &[1]);
        assert_eq!(rail_items(&order, &g), [RailItem::Conn(1)]);
    }

    #[test]
    fn the_folder_anchor_is_the_first_open_member_in_rail_order() {
        let g = groups_of(&[("Prod", true, &[2, 4])]);
        let order = [1, 4, 3, 2];
        let RailItem::Folder { members, .. } = &rail_items(&order, &g)[1] else { panic!("no folder") };
        // Group order says 2, but the tile sits at 4, so a drop before the folder must anchor on 4.
        assert_eq!(members[0], 2);
        assert_eq!(folder_anchor(&order, members), 4);
    }

    #[test]
    fn a_moved_tile_lands_next_to_its_anchor() {
        let mut o = vec![1, 5, 6, 7];
        move_next_to(&mut o, 1, 7, true);
        assert_eq!(o, [5, 6, 7, 1]);
        move_next_to(&mut o, 1, 5, false);
        assert_eq!(o, [1, 5, 6, 7]);
        move_next_to(&mut o, 7, 99, true);
        assert_eq!(o, [1, 5, 6, 7]);
        // A connection with no tile, such as a closed one dragged in from the sidebar, is not added.
        move_next_to(&mut o, 42, 5, true);
        assert_eq!(o, [1, 5, 6, 7]);
    }

    #[test]
    fn zones_are_thirds_of_the_tile() {
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 100.0), egui::vec2(28.0, 30.0));
        assert_eq!((zone_of(r, 105.0), zone_of(r, 115.0), zone_of(r, 125.0)), (Zone::Before, Zone::Onto, Zone::After));
    }

    #[test]
    fn drops_resolve_by_tile_and_zone() {
        let g = groups_of(&[("Prod", true, &[1, 2, 3])]);
        let r = |on, z, d| resolve_drop(on, z, d, &g).map(|x| x.0);
        assert_eq!(r(DropOn::Loose(8), Zone::Onto, 9), Some(RailDrop::Pair { conn: 9, target: 8 }));
        assert_eq!(r(DropOn::Loose(8), Zone::After, 9), Some(RailDrop::Reorder { conn: 9, anchor: 8, after: true }));
        assert_eq!(r(DropOn::Loose(8), Zone::Onto, 8), None);
        // Closed folder: the middle adds, a member of it does nothing, the edges reorder.
        assert_eq!(r(DropOn::Folder { gid: 1, anchor: 1 }, Zone::Onto, 9), Some(RailDrop::Into { conn: 9, gid: 1, at: None }));
        assert_eq!(r(DropOn::Folder { gid: 1, anchor: 1 }, Zone::Onto, 2), None);
        assert_eq!(r(DropOn::Folder { gid: 1, anchor: 1 }, Zone::Before, 9), Some(RailDrop::Reorder { conn: 9, anchor: 1, after: false }));
        // Inside the capsule the index counts the members after the dragged one leaves: 1 below 3 lands at 2.
        assert_eq!(r(DropOn::Member { id: 3, gid: 1 }, Zone::After, 1), Some(RailDrop::Into { conn: 1, gid: 1, at: Some(2) }));
        assert_eq!(r(DropOn::Member { id: 2, gid: 1 }, Zone::Before, 9), Some(RailDrop::Into { conn: 9, gid: 1, at: Some(1) }));
        assert_eq!(r(DropOn::FolderTop { gid: 1, anchor: 1 }, Zone::After, 9), Some(RailDrop::Into { conn: 9, gid: 1, at: Some(0) }));
    }

    #[test]
    fn engine_marks() {
        use db::sql::Engine::{Mongo, MySql};
        assert_eq!(engine_mark(Mongo, "").0, "MO");
        assert_eq!(engine_mark(MySql, "MySQL").0, "MY");
        assert_eq!(engine_mark(MySql, "").0, "MY");
        assert_eq!(engine_mark(MySql, "MariaDB"), ("MA", "MariaDB"));
    }
}
