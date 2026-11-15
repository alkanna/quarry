//! The rail: one labelled tile per open connection, folders for grouped ones, a pinned Connect tile and Settings.

use super::*;
use crate::ui::dnd::{self, Zone};
use crate::ui::fold;

/// The id a rail folder's open and close animation goes by.
fn fold_id(gid: u64) -> egui::Id {
    egui::Id::unique(("rail-fold", gid))
}

#[derive(Clone, Copy)]
pub(super) enum RailIcon {
    Search,
    Settings,
}

impl App {
    pub(super) fn rail(&mut self, ui: &mut egui::Ui) {
        let frame = egui::Frame::new().fill(CHROME()).inner_margin(egui::Margin { left: 8, right: 8, top: 10, bottom: 10 });
        egui::Panel::left("rail").exact_size(RAIL_W).resizable(false).show_separator_line(false).frame(frame).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            self.conn_shortcuts(ui);
            self.connect_tile(ui);
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                if rail_button(ui, RailIcon::Settings, matches!(self.modal, Some(Modal::Settings)), true, "Settings (Cmd+,)").clicked() {
                    self.modal = Some(Modal::Settings);
                }
            });
        });
    }

    /// Rounds the top-left corner of the page beside the rail, so the page reads as set into the header and the rail,
    /// which share one color and no longer have a divider. Called after every panel has drawn, on the panels' own layer.
    pub(super) fn page_corner(&self, ctx: &egui::Context) {
        let r = R_LG as f32;
        let o = ctx.content_rect().min + egui::vec2(RAIL_W, HEADER_H);
        // A quarter ring r wide whose inner edge is the corner's curve. Its outer half lands on the rail and the header,
        // which are the same color, and a stroke keeps the curve antialiased where a filled concave path would not.
        let c = o + egui::vec2(r, r);
        let pts: Vec<egui::Pos2> = (0..=12).map(|i| {
            let a = std::f32::consts::PI * (1.0 + i as f32 / 24.0);
            c + egui::vec2(a.cos(), a.sin()) * (r * 1.5)
        }).collect();
        ctx.layer_painter(egui::LayerId::background()).add(egui::Shape::line(pts, Stroke::new(r, CHROME())));
    }

    /// The pinned Connect tile and its popover: every saved connection under its group, open ones marked by their status
    /// dot. It is the only way in for a connection that is not open, so it stays on screen however many tiles scroll above.
    fn connect_tile(&mut self, ui: &mut egui::Ui) {
        let (rect, r) = ui.allocate_exact_size(egui::vec2(TILE_W, CONNECT_H), Sense::click());
        let state = state_of(&r);
        let demo_open = std::mem::take(&mut self.conns_ui.demo_picker);
        if demo_open {
            egui::Popup::open_id(ui.ctx(), egui::Popup::default_response_id(&r));
        }
        let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&r));
        let boxed = egui::Rect::from_center_size(rect.center(), egui::vec2(ICON, ICON));
        let p = ui.painter();
        p.rect_filled(boxed, R_TILE, if open { SEL() } else { [tint(TEXT(), 14), HOVER(), PRESSED()][state] });
        // The plus is painted: the glyph comes from a fallback font and sits off center.
        paint_plus(p, boxed.center(), 6.0, if open || state > 0 { BRIGHT() } else { MUTED() });
        focus_ring(ui, &r, boxed, R_TILE as f32);
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Connect"));
        let r = r.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Connect");
        let mut pick = None;
        let mut new = false;
        let mut cloud = false;
        let mut manage = false;
        // The Cloud SQL browser lists SQL instances only, so a MongoDB connection has no use for it.
        let mongo_active = self.active.and_then(|id| self.conn(id)).is_some_and(|c| !c.meta.engine.is_sql());
        // The filter field and the group captions are clicked inside the popup, so it closes on an outside click or an explicit pick only.
        egui::Popup::menu(&r).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).align(egui::RectAlign::RIGHT_START).width(340.0).show(|ui| {
            menu_look(ui);
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.spacing_mut().button_padding = egui::vec2(10.0, 6.0);
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(semibold(RichText::new("Connect").size(TXT_TITLE).color(BRIGHT())));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(10.0);
                    ui.label(RichText::new(format!("{} saved", self.conns.len())).size(TXT_CAPTION).color(FAINT()));
                });
            });
            ui.add_space(4.0);
            // Only the list scrolls, so the actions below stay reachable however many connections there are.
            let room = (ui.ctx().content_rect().bottom() - ui.cursor().top() - 200.0).max(4.0 * 34.0);
            // The menu was not drawn last frame, so it just opened: clear the filter, focus it and bring the active connection
            // into view once.
            let seen = egui::Id::unique("conn-menu-frame");
            let frame = ui.ctx().cumulative_frame_nr();
            let opened = ui.ctx().data(|d| d.get_temp::<u64>(seen)) != Some(frame.wrapping_sub(1));
            ui.ctx().data_mut(|d| d.insert_temp(seen, frame));
            if opened {
                self.conns_ui.picker_filter.clear();
            }
            conns_panel::picker_filter(ui, &mut self.conns_ui.picker_filter, opened);
            egui::ScrollArea::vertical().id_salt("conn-menu").max_height(room).show(ui, |ui| {
                if let Some(id) = conns_panel::picker_list(ui, &self.conns, &self.groups, &mut self.conns_ui, self.active, opened) {
                    pick = Some(id);
                    ui.close();
                }
            });
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(2.0);
            if ui.button("New connection\u{2026}").clicked() {
                new = true;
                ui.close();
            }
            if !mongo_active && ui.button("Add from Cloud SQL\u{2026}").on_hover_text("PostgreSQL instances your gcloud account can reach").clicked() {
                cloud = true;
                ui.close();
            }
            if ui.button("Manage\u{2026}").on_hover_text("Open the Connections panel to edit groups").clicked() {
                manage = true;
                ui.close();
            }
            ui.add_space(2.0);
        });
        if let Some(id) = pick {
            self.active = Some(id);
            // A closed connection opens, and a failed one retries; an open one is only switched to.
            if self.conn(id).is_some_and(|c| c.status.can_connect()) && !self.demo {
                self.connect(id);
            }
        }
        if manage {
            self.prefs.set_side_mode(SideMode::Connections);
            self.prefs.sidebar_hidden = false;
            self.save_prefs();
        }
        if new {
            self.new_conn_modal();
        }
        if cloud {
            self.open_cloud_browser(false);
        }
    }
}

impl App {
    /// One tile per open connection, with a folded group as one tile, scrolling when they outgrow the rail.
    /// The tiles and folders drag to reorder with the shared engine, and an open folder's members are a list of their own.
    /// A lifted tile also carries a `DragItem` payload, so a tile can leave its folder or land among another folder's
    /// members, and a connection dragged in from the sidebar lands here, through the same `tile_drop` targets. Such a
    /// drop wins over the move the list would make, since it is the more precise of the two.
    fn conn_shortcuts(&mut self, ui: &mut egui::Ui) {
        let open: Vec<u64> = self.conns.iter().filter(|c| c.status.is_open()).map(|c| c.meta.id).collect();
        sync_order(&mut self.rail_order, &open);
        self.prune_rail_open();
        if self.rail_order.is_empty() {
            return;
        }
        // Leaves room for the Connect tile and the Settings button, which stay pinned below the scrolling tiles.
        let room = (ui.available_height() - 36.0 - CONNECT_H - 8.0).max(SLOT);
        let items = rail_items(&self.rail_order, &self.groups);
        let ctx = ui.ctx().clone();
        // How far open each folder is drawn. Its slot grows with it, so the reorder engine moves the tiles below smoothly.
        let opens: Vec<f32> = items
            .iter()
            .map(|it| match it {
                RailItem::Folder { gid, .. } => fold::openness(&ctx, fold_id(*gid), self.rail_open.contains(gid)),
                RailItem::Conn(_) => 0.0,
            })
            .collect();
        let sizes: Vec<f32> = items
            .iter()
            .zip(&opens)
            .map(|(it, e)| match it {
                RailItem::Folder { members, .. } => shelf_slot(1) + (shelf_slot(members.len()) - shelf_slot(1)) * e,
                RailItem::Conn(_) => SLOT,
            })
            .collect();
        let keys: Vec<egui::Id> = items
            .iter()
            .map(|it| match it {
                RailItem::Conn(id) => egui::Id::unique(("rail-conn", *id)),
                RailItem::Folder { gid, .. } => egui::Id::unique(("rail-folder", *gid)),
            })
            .collect();
        let into = vec![true; items.len()];
        let mut pick = None;
        let mut act = None;
        let mut toggle = None;
        let mut folder_acts: Vec<(u64, FolderAct)> = Vec::new();
        let mut drops: Vec<RailDrop> = Vec::new();
        let mut inner: Option<(u64, Vec<u64>, dnd::Move)> = None;
        let mut outer = None;
        egui::ScrollArea::vertical().max_height(room).auto_shrink([false, true]).show(ui, |ui| {
            ui.spacing_mut().scroll.bar_width = 3.0;
            // A capsule is wider than the 28 px column, so the scroll area's clip must not cut its sides.
            ui.set_clip_rect(ui.clip_rect().expand2(egui::vec2(6.0, 0.0)));
            outer = dnd::reorder(ui, egui::Id::unique("rail-tiles"), dnd::Items::vertical(&sizes).keys(&keys).into(&into), |ui, k, st| {
                let slot = ui.max_rect();
                let tile = egui::Rect::from_min_size(slot.min + egui::vec2(0.0, 4.0), egui::vec2(TILE_W, TILE_H));
                match &items[k] {
                    RailItem::Conn(id) => {
                        dnd::carry(ui.ctx(), st, || DragItem::Conn(*id));
                        let target = (!st.busy).then_some(DropOn::Loose(*id));
                        self.conn_tile(ui, tile, *id, st.lifted, target, &mut pick, &mut act, &mut drops).unwrap_or_else(|| ui.response())
                    }
                    RailItem::Folder { gid, members } => {
                        let Some(g) = self.groups.group(*gid) else { return ui.response() };
                        let color = group_color(&g.color);
                        let open_now = self.rail_open.contains(gid);
                        // The tile sits at the first open member in rail order, which is not always the group's first.
                        let anchor = folder_anchor(&self.rail_order, members);
                        let worst = folder_status(g.members.iter().filter_map(|m| self.conn(*m)).map(|c| &c.status));
                        let holds_active = self.active.is_some_and(|a| members.contains(&a));
                        let e = opens[k];
                        let full = members.len() as f32 * MEMBER_SLOT;
                        // The shelf grows from one tile to all the members as the folder opens; the folder's own button
                        // shrinks from the name row and the folder tile to the name row alone.
                        let top = slot.min + egui::vec2(0.0, 2.0);
                        let shelf = egui::Rect::from_min_size(top - egui::vec2(3.0, 0.0), egui::vec2(TILE_W + 6.0, CAP_H + MEMBER_SLOT + (full - MEMBER_SLOT) * e + 4.0));
                        let tile = egui::Rect::from_min_size(top, egui::vec2(TILE_W, CAP_H + MEMBER_SLOT * (1.0 - e)));
                        let marks: Vec<&'static str> = members.iter().filter_map(|m| self.conn(*m)).map(|c| engine_mark(c.meta.engine, &c.server.product).0).collect();
                        let (r, rect) = folder_tile(ui, tile, shelf, &g.name, color, &marks, (!open_now).then_some(worst).flatten(), holds_active && !open_now, e);
                        let r = r.on_hover_ui(|ui| self.folder_tip(ui, g, members));
                        let tile = egui::Rect::from_min_size(top, egui::vec2(TILE_W, CAP_H));
                        if st.lifted {
                            if open_now {
                                for (i, m) in members.iter().enumerate() {
                                    let at = egui::Rect::from_min_size(egui::pos2(tile.left(), tile.bottom() + 2.0 + i as f32 * MEMBER_SLOT), egui::vec2(TILE_W, TILE_H));
                                    self.conn_tile(ui, at, *m, true, None, &mut pick, &mut act, &mut drops);
                                }
                            }
                            return r;
                        }
                        if r.clicked() {
                            toggle = Some(*gid);
                        }
                        r.context_menu(|ui| {
                            menu_look(ui);
                            let gid = *gid;
                            menu_act(ui, &mut folder_acts, (gid, FolderAct::Toggle), |ui| ui.button(if open_now { "Close" } else { "Open" }));
                            match conns_panel::connect_all_items(ui, g, &self.conns) {
                                Some(true) => folder_acts.push((gid, FolderAct::ConnectAll)),
                                Some(false) => folder_acts.push((gid, FolderAct::DisconnectAll)),
                                None => {}
                            }
                            ui.separator();
                            menu_act(ui, &mut folder_acts, (gid, FolderAct::Edit), |ui| ui.button("Edit group\u{2026}"));
                            menu_act(ui, &mut folder_acts, (gid, FolderAct::Unfold), |ui| ui.button("Don't fold in the rail"));
                        });
                        if !st.busy {
                            let top = if open_now { DropOn::FolderTop { gid: *gid, anchor } } else { DropOn::Folder { gid: *gid, anchor } };
                            tile_drop(ui, &r, rect, top, &self.groups, &mut drops);
                        }
                        if e > 0.0 && e < 1.0 {
                            // Opening or closing: the members slide out from under the tile as the capsule grows, and
                            // back under it as it shrinks. They are only drawn until the folder has settled open.
                            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(egui::pos2(tile.left(), tile.bottom()), egui::vec2(TILE_W, full))));
                            let mut clip = ui.clip_rect();
                            clip.min.y = clip.min.y.max(tile.bottom());
                            clip.max.y = clip.max.y.min(tile.bottom() + full * e);
                            child.set_clip_rect(clip);
                            child.disable();
                            child.set_opacity(e);
                            for (i, m) in members.iter().enumerate() {
                                let at = egui::Rect::from_min_size(egui::pos2(tile.left(), tile.bottom() + 2.0 + i as f32 * MEMBER_SLOT - (1.0 - e) * full), egui::vec2(TILE_W, TILE_H));
                                self.conn_tile(&mut child, at, *m, true, None, &mut pick, &mut act, &mut drops);
                            }
                        } else if open_now {
                            let list = egui::Rect::from_min_size(egui::pos2(tile.left(), tile.bottom()), egui::vec2(TILE_W, full));
                            let msizes = vec![MEMBER_SLOT; members.len()];
                            let mkeys: Vec<egui::Id> = members.iter().map(|m| egui::Id::unique(("rail-member", *m))).collect();
                            let mv = put_at(ui, list, |ui| {
                                dnd::reorder(ui, egui::Id::unique(("rail-capsule", *gid)), dnd::Items::vertical(&msizes).keys(&mkeys), |ui, i, mst| {
                                    let m = members[i];
                                    dnd::carry(ui.ctx(), mst, || DragItem::Conn(m));
                                    // The lifted tile stays inside its folder, so once the pointer leaves the folder a
                                    // ghost of the tile follows it to show where it is being carried.
                                    if mst.lifted
                                        && ui.ctx().input(|i| i.pointer.any_down())
                                        && !dnd::drag_cancelled(ui.ctx())
                                        && let Some(p) = ui.ctx().pointer_latest_pos().filter(|p| !list.expand(6.0).contains(*p))
                                        && let Some(c) = self.conn(m)
                                    {
                                        ghost_tile(ui, p, c);
                                    }
                                    let at = egui::Rect::from_min_size(ui.max_rect().min + egui::vec2(0.0, 2.0), egui::vec2(TILE_W, TILE_H));
                                    let target = (!mst.busy).then_some(DropOn::Member { id: m, gid: *gid });
                                    self.conn_tile(ui, at, m, mst.lifted, target, &mut pick, &mut act, &mut drops).unwrap_or_else(|| ui.response())
                                })
                            });
                            if let Some(mv) = mv {
                                inner = Some((*gid, members.clone(), mv));
                            }
                        }
                        r
                    }
                }
            });
        });
        if let Some(id) = pick {
            self.active = Some(id);
        }
        if let Some(gid) = toggle {
            self.toggle_rail_folder(&ctx, gid);
        }
        // All of them change state after the frame was drawn.
        if toggle.is_some() || !drops.is_empty() || outer.is_some() || inner.is_some() {
            ui.ctx().request_repaint();
        }
        if !drops.is_empty() {
            for d in drops {
                self.apply_rail_drop(d);
            }
        } else if let Some(m) = outer {
            self.apply_rail_move(&items, m);
        } else if let Some((gid, members, m)) = inner {
            let full = self.groups.group(gid).map(|g| g.members.clone()).unwrap_or_default();
            if let Some(at) = member_slot(&full, &members, m) {
                let all = self.conn_ids();
                self.groups.place(members[m.from], Place::Into(gid, Some(at)), &all);
                self.save_groups();
            }
        }
        if !folder_acts.is_empty() {
            ui.ctx().request_repaint();
        }
        for (gid, a) in folder_acts {
            match a {
                FolderAct::Toggle => self.toggle_rail_folder(&ctx, gid),
                FolderAct::ConnectAll => self.connect_group(gid),
                FolderAct::DisconnectAll => self.disconnect_group(gid),
                FolderAct::Edit => self.edit_group(gid),
                FolderAct::Unfold => {
                    if let Some(g) = self.groups.group_mut(gid) {
                        g.fold_rail = false;
                        let name = g.name.clone();
                        self.save_groups();
                        self.ok(format!("{name} shows its connections one by one in the rail. Edit group turns folding back on."));
                    }
                }
            }
        }
        match act {
            Some((id, TileAct::Reconnect)) => {
                self.active = Some(id);
                self.connect(id);
            }
            Some((id, TileAct::Server)) => self.open_server(id),
            Some((id, TileAct::Edit)) => self.edit_conn(id),
            Some((id, TileAct::Disconnect)) => self.disconnect(id),
            None => {}
        }
    }

    /// Forgets the open rail folders of deleted groups: a deleted group's id is given to the next new one, which must not
    /// show open for it.
    pub(super) fn prune_rail_open(&mut self) {
        let groups = &self.groups;
        self.rail_open.retain(|g| groups.group(*g).is_some());
    }

    /// A folder's tooltip: the group's name, how many of its connections are open, and one row per open member.
    fn folder_tip(&self, ui: &mut egui::Ui, g: &groups::Group, open: &[u64]) {
        ui.horizontal(|ui| {
            ui.label(semibold(RichText::new(&g.name).color(BRIGHT())));
            ui.label(RichText::new(format!("{} of {} open", open.len(), g.members.len())).size(TXT_CAPTION).color(FAINT()));
        });
        for c in open.iter().filter_map(|m| self.conn(*m)) {
            ui.horizontal(|ui| {
                let mark = engine_mark(c.meta.engine, &c.server.product).0;
                if let Some(tex) = engine_logo(ui.ctx(), mark) {
                    ui.add(egui::Image::new((tex.id(), egui::vec2(14.0, 14.0))).tint(mark_color(mark)));
                }
                let active = self.active == Some(c.meta.id);
                ui.label(RichText::new(&c.meta.name).color(if active { BRIGHT() } else { TEXT() }));
                if !c.meta.tag.is_empty() {
                    ui.label(RichText::new(&c.meta.tag).size(TXT_MICRO).color(tag_color(&c.meta.tag)));
                }
            });
        }
    }

    /// Opens or closes a folder in the rail, with its animation.
    fn toggle_rail_folder(&mut self, ctx: &egui::Context, gid: u64) {
        let open = !self.rail_open.remove(&gid);
        if open {
            self.rail_open.insert(gid);
        }
        fold::start(ctx, fold_id(gid), open);
    }

    /// One connection's tile in `rect` with its context menu and, given a `target`, the drop target it is. None when the
    /// connection is gone. A lifted tile floats with the pointer, so it gets no menu.
    #[allow(clippy::too_many_arguments)]
    fn conn_tile(&self, ui: &mut egui::Ui, rect: egui::Rect, id: u64, lifted: bool, target: Option<DropOn>, pick: &mut Option<u64>, act: &mut Option<(u64, TileAct)>, drops: &mut Vec<RailDrop>) -> Option<egui::Response> {
        let c = self.conns.iter().find(|c| c.meta.id == id)?;
        let r = conn_shortcut(ui, rect, c, self.active == Some(id), || self.tabs.iter().filter(|t| t.sel.conn == id).count());
        if lifted {
            return Some(r);
        }
        if r.clicked() {
            *pick = Some(id);
        }
        if let Some(t) = target {
            tile_drop(ui, &r, r.rect, t, &self.groups, drops);
        }
        r.context_menu(|ui| tile_menu(ui, id, act));
        // The picture of a drag over this tile, since a screenshot cannot hold one.
        if self.conns_ui.demo_rail_drop == Some(id) {
            let p = ui.painter();
            p.rect(r.rect.expand(3.0), R_TILE + 3, tint(PRIMARY(), 36), Stroke::new(1.5, PRIMARY()), egui::StrokeKind::Inside);
            let at = r.rect.center() + egui::vec2(4.0, 2.0);
            if let Some(c) = self.conn(1) {
                ghost_tile(ui, at, c);
            }
            hint_card(ui.ctx(), at, "New group");
        }
        // The picture of the menu, drawn beside its tile since a screenshot has no right click to open it with.
        if self.conns_ui.demo_tile_menu == Some(id) {
            egui::Area::new(egui::Id::unique("demo-tile-menu")).order(egui::Order::Foreground).fixed_pos(r.rect.right_top() + egui::vec2(8.0, 0.0)).show(ui.ctx(), |ui| {
                egui::Frame::menu(ui.style()).show(ui, |ui| tile_menu(ui, id, act));
            });
        }
        Some(r)
    }

    /// Applies a move of the rail's tile list: a reorder of tiles and folders, or a tile let go on the middle of another
    /// tile (a new group) or of a folder (into it). A folder let go on a tile does nothing, since groups do not nest.
    fn apply_rail_move(&mut self, items: &[RailItem], m: dnd::Move) {
        if !m.into {
            self.rail_order = order_after_move(&self.rail_order, items, m);
            return;
        }
        let Some(RailItem::Conn(conn)) = items.get(m.from) else { return };
        match items.get(m.to) {
            Some(RailItem::Conn(target)) => self.apply_rail_drop(RailDrop::Pair { conn: *conn, target: *target }),
            Some(RailItem::Folder { gid, .. }) => self.apply_rail_drop(RailDrop::Into { conn: *conn, gid: *gid, at: None }),
            None => {}
        }
    }

    /// Applies one drop on the rail: the groups are saved, and the toast offers Undo.
    fn apply_rail_drop(&mut self, d: RailDrop) {
        let all = self.conn_ids();
        // Undo puts back only what this drop moves: each connection's spot in the groups, the tile's place in the rail and
        // the group it makes. A whole snapshot would also throw away group edits made while the toast is up.
        let tile = match d {
            RailDrop::Reorder { conn, .. } | RailDrop::Into { conn, .. } | RailDrop::Pair { conn, .. } => conn,
        };
        let at = self.rail_order.iter().position(|c| *c == tile).unwrap_or(0);
        let prev = at.checked_sub(1).and_then(|i| self.rail_order.get(i).copied());
        let spot = |a: &App, c: u64| {
            let (g, i) = a.groups.spot(c, &all);
            (c, g, i)
        };
        let undo = |moved: Vec<(u64, Option<u64>, usize)>, created: Option<u64>| Restore::Placed { moved, created, tile, prev, at };
        let name = |a: &App, id: u64| a.conn(id).map(|c| c.meta.name.clone()).unwrap_or_default();
        match d {
            RailDrop::Reorder { conn, anchor, after } => {
                if !self.rail_order.contains(&conn) {
                    return;
                }
                // A member of a folded group dropped between tiles leaves its group. An unfolded one is loose in the rail already.
                let left = self.groups.group_of(conn).filter(|g| g.fold_rail).map(|g| g.name.clone());
                if let Some(g) = left {
                    let was = vec![spot(self, conn)];
                    self.groups.place(conn, Place::Loose(None), &all);
                    self.toast_restore(format!("{} left {g}.", name(self, conn)), undo(was, None));
                }
                move_next_to(&mut self.rail_order, conn, anchor, after);
            }
            RailDrop::Into { conn, gid, at } => {
                let joined = self.groups.group_of(conn).map(|g| g.id) != Some(gid);
                let was = vec![spot(self, conn)];
                if self.groups.place(conn, Place::Into(gid, at), &all) && joined {
                    let g = self.groups.group(gid).map(|g| g.name.clone()).unwrap_or_default();
                    // The folder sits at its first open member, so a tile from above it must follow the group or the folder jumps.
                    let members = self.groups.group(gid).map(|g| g.members.clone()).unwrap_or_default();
                    if let Some(last) = self.rail_order.iter().copied().filter(|c| *c != conn && members.contains(c)).last() {
                        move_next_to(&mut self.rail_order, conn, last, true);
                    }
                    self.toast_restore(format!("{} added to {g}.", name(self, conn)), undo(was, None));
                }
            }
            RailDrop::Pair { conn, target } => {
                let was = vec![spot(self, conn), spot(self, target)];
                let gid = self.groups.new_group("New group");
                self.groups.place(target, Place::Into(gid, None), &all);
                self.groups.place(conn, Place::Into(gid, None), &all);
                self.rail_open.insert(gid);
                move_next_to(&mut self.rail_order, conn, target, true);
                self.toast_restore_edit(format!("Grouped {} and {}.", name(self, target), name(self, conn)), undo(was, Some(gid)), Some(gid));
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
        if let Some(name) = self.drop_session(id) {
            self.ok(format!("Disconnected from {name}"));
        }
    }

    /// Closes the connection's session and forgets what was listed on it. Returns its name, or None when it is gone.
    pub(super) fn drop_session(&mut self, id: u64) -> Option<String> {
        let c = self.conns.iter_mut().find(|c| c.meta.id == id)?;
        (c.client, c.sql, c.tunnel, c.status, c.writable) = (None, None, None, Status::Idle, false);
        c.forget_lists();
        let name = c.meta.name.clone();
        self.pg_tables_asked.forget(id, None, None);
        Some(name)
    }
}

/// The menu of a connection's tile.
fn tile_menu(ui: &mut egui::Ui, id: u64, act: &mut Option<(u64, TileAct)>) {
    menu_look(ui);
    if ui.button("Reconnect").clicked() {
        *act = Some((id, TileAct::Reconnect));
    }
    if ui.button("Server status").clicked() {
        *act = Some((id, TileAct::Server));
    }
    if ui.button("Edit connection\u{2026}").clicked() {
        *act = Some((id, TileAct::Edit));
    }
    if danger_item(ui, "Disconnect").clicked() {
        *act = Some((id, TileAct::Disconnect));
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

/// What a folder tile's menu asks for.
#[derive(Clone, Copy)]
enum FolderAct {
    Toggle,
    ConnectAll,
    DisconnectAll,
    Edit,
    /// Don't fold in the rail.
    Unfold,
}

/// What a connection tile's menu asks for.
#[derive(Clone, Copy)]
enum TileAct {
    Reconnect,
    Server,
    Edit,
    Disconnect,
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
fn zone_of(rect: egui::Rect, y: f32) -> Zone {
    dnd::zone_edges(rect.height().max(1.0), y - rect.top(), 1.0 / 3.0)
}

/// The index `Groups::place` wants to put `dragged` next to `target` in group `gid`: it counts the members after the dragged one leaves.
fn slot_in_group(groups: &Groups, gid: u64, target: u64, after: bool, dragged: u64) -> Option<usize> {
    let rest: Vec<u64> = groups.group(gid)?.members.iter().copied().filter(|m| *m != dragged).collect();
    rest.iter().position(|m| *m == target).map(|i| i + after as usize)
}

/// What dropping `dragged` at `zone` of `on` does, or None when nothing sensible. A member tile has no middle: Into counts as After there.
fn resolve_drop(on: DropOn, zone: Zone, dragged: u64, groups: &Groups) -> Option<(RailDrop, Mark)> {
    let reorder = |anchor: u64, after: bool| (anchor != dragged).then_some((RailDrop::Reorder { conn: dragged, anchor, after }, Mark::Line { top: !after }));
    let in_group = |gid: u64| groups.group(gid).is_some_and(|g| g.members.contains(&dragged));
    match on {
        DropOn::Loose(id) if id == dragged => None,
        DropOn::Loose(id) => match zone {
            Zone::Before => reorder(id, false),
            Zone::After => reorder(id, true),
            Zone::Into => Some((RailDrop::Pair { conn: dragged, target: id }, Mark::Fill)),
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
            Zone::Into => Some((RailDrop::Into { conn: dragged, gid, at: None }, Mark::Fill)),
        },
        DropOn::FolderTop { gid, anchor } => match zone {
            Zone::Before => reorder(anchor, false),
            Zone::Into if in_group(gid) => None,
            Zone::Into => Some((RailDrop::Into { conn: dragged, gid, at: None }, Mark::Fill)),
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
        Mark::Line { top } => {
            let y = if top { rect.top() - 3.0 } else { rect.bottom() + 3.0 };
            crate::ui::dnd::drop_line(p, rect.x_range().expand(2.0), y);
            p.circle_filled(egui::pos2(rect.left() - 2.0, y), 3.0, PRIMARY());
        }
        // A ring a step outside the tile, the way the tile's own active ring sits.
        Mark::Fill => {
            p.rect(rect.expand(3.0), R_TILE + 3, tint(PRIMARY(), 36), Stroke::new(1.5, PRIMARY()), egui::StrokeKind::Inside);
        }
    }
    if let Some(words) = drop_hint(drop, groups) {
        hint_card(ui.ctx(), pos, &words);
    }
    if r.dnd_release_payload::<DragItem>().is_some() {
        out.push(drop);
    }
}

/// What a drop does, in a few words beside the pointer: None for a plain reorder, which the line already says.
fn drop_hint(drop: RailDrop, groups: &Groups) -> Option<String> {
    match drop {
        RailDrop::Pair { .. } => Some("New group".into()),
        RailDrop::Into { conn, gid, .. } => {
            let g = groups.group(gid)?;
            (!g.members.contains(&conn)).then(|| format!("Add to {}", g.name))
        }
        // Only a folded group's member leaves it by landing between tiles; an unfolded one is loose in the rail already.
        RailDrop::Reorder { conn, .. } => groups.group_of(conn).filter(|g| g.fold_rail).map(|g| format!("Remove from {}", g.name)),
    }
}

/// The hint card to the right of the carried tile. Painted on a tooltip layer without a widget, so it never covers the
/// drop targets under the pointer.
fn hint_card(ctx: &egui::Context, pointer: egui::Pos2, words: &str) {
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("rail-hint")));
    let g = p.layout_job(egui::text::LayoutJob::single_section(words.to_string(), ui_fmt(TXT_CAPTION, BRIGHT(), W_MEDIUM, 0.0)));
    let rect = egui::Rect::from_min_size(pointer + egui::vec2(10.0 + ICON + 8.0, 6.0 + ICON / 2.0 - 12.0), egui::vec2(g.size().x + 20.0, 24.0));
    p.add(ctx.global_style().visuals.popup_shadow.as_shape(rect, R_MD));
    p.rect(rect, R_MD, RAISED(), Stroke::new(1.0, RAISED_LINE()), egui::StrokeKind::Inside);
    let at = egui::pos2(rect.left() + 10.0, rect.center().y - g.size().y / 2.0 + optical_dy(&g));
    p.galley(at, g, BRIGHT());
}

/// The status a closed folder shows for its members: a failed one, else one connecting, else ready. None when none is open
/// or failed.
fn folder_status<'a>(st: impl Iterator<Item = &'a Status>) -> Option<&'a Status> {
    let rank = |s: &Status| match s {
        Status::Failed(_) => 3,
        Status::Connecting => 2,
        Status::Ready => 1,
        _ => 0,
    };
    st.filter(|s| rank(s) > 0).max_by_key(|s| rank(s))
}

/// A folded group: a shelf in the group's color under its name, holding either the members (open) or one app-folder tile
/// that previews up to four of their logos (closed). `rect` is the button, the name row and, while closed, the folder tile;
/// `shelf` is the tinted surface, which grows with `e` as the folder opens. The closed tile shows the worst status of the
/// members and the active ring when it holds the active connection, which is otherwise out of sight. Returns the click
/// response and the button's rect.
#[allow(clippy::too_many_arguments)]
fn folder_tile(ui: &mut egui::Ui, rect: egui::Rect, shelf: egui::Rect, name: &str, color: Color32, marks: &[&'static str], status: Option<&Status>, active: bool, e: f32) -> (egui::Response, egui::Rect) {
    let r = ui.interact(rect, ui.scope_id().with("folder-tile"), Sense::click());
    let state = state_of(&r);
    let p = ui.painter();
    p.rect_filled(shelf, R_SHELF, tint(color, [20, 36, 44][state]));
    let head = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), CAP_H));
    let g = p.layout_job({
        let mut job = egui::text::LayoutJob::single_section(name.to_string(), ui_fmt(TXT_SECTION, color, W_SEMIBOLD, 0.0));
        job.wrap = egui::text::TextWrapping::truncate_at_width(shelf.width() - 6.0);
        job
    });
    p.galley(head.center() - g.size() / 2.0 + egui::vec2(0.0, optical_dy(&g)), g, color);
    // The folder tile fades out as the members slide in under it.
    if e < 1.0 {
        let tile = icon_rect(egui::Rect::from_min_size(egui::pos2(rect.left(), head.bottom() + 2.0), egui::vec2(rect.width(), TILE_H)));
        let fade = |c: Color32| c.gamma_multiply(1.0 - e);
        p.rect(tile, R_TILE, fade(tint(TEXT(), [18, 26, 32][state])), Stroke::new(1.0, fade(tile_edge())), egui::StrokeKind::Inside);
        for (i, mark) in marks.iter().take(4).enumerate() {
            let c = tile.center() + egui::vec2(if i % 2 == 0 { -8.0 } else { 8.0 }, if i < 2 { -8.0 } else { 8.0 });
            let sq = egui::Rect::from_center_size(c, egui::vec2(14.0, 14.0));
            gradient_rect(p, sq, R_SM as f32, fade(tint(mark_color(mark), tile_alpha(state))));
            logo(p, sq.center(), 9.0, mark, fade(Color32::WHITE));
        }
        if e == 0.0 {
            if let Some(s) = status {
                status_mark(ui, tile, s);
            }
            if active {
                active_mark(p, tile);
            }
        }
    }
    focus_ring(ui, &r, rect, R_TILE as f32);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{name} folder")));
    (r.on_hover_cursor(egui::CursorIcon::PointingHand), rect)
}

/// The 40 px square of an app-icon tile, centered in its slot's column.
fn icon_rect(slot: egui::Rect) -> egui::Rect {
    egui::Rect::from_center_size(egui::pos2(slot.center().x, slot.top() + ICON / 2.0), egui::vec2(ICON, ICON))
}

/// How strong a tile's engine color is at its top, by its state. Light themes need more of it to carry a light logo.
fn tile_alpha(state: usize) -> u8 {
    let base = if is_light() { 170 } else { 107 };
    base + [0, 22, 30][state]
}

/// A tile's hairline edge: lighter than the tile on a dark theme, darker on a light one.
fn tile_edge() -> Color32 {
    if is_light() { tint(Color32::BLACK, 20) } else { tint(Color32::WHITE, 26) }
}

/// The engine's logo, `size` points wide, centered on `c` in `color`.
fn logo(p: &egui::Painter, c: egui::Pos2, size: f32, mark: &str, color: Color32) {
    if let Some(tex) = engine_logo(p.ctx(), mark) {
        let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
        p.image(tex.id(), egui::Rect::from_center_size(c, egui::vec2(size, size)), uv, color);
    }
}

/// The active connection's marks: a bright ring a step outside the tile and a bar on the rail's own left edge. The bar is
/// not the accent: a light theme's orange accent would read as the red of production.
fn active_mark(p: &egui::Painter, tile: egui::Rect) {
    p.rect_stroke(tile.expand(2.5), R_TILE + 2, Stroke::new(1.5, BRIGHT()), egui::StrokeKind::Middle);
    let edge = tile.center().x - RAIL_W / 2.0;
    p.rect_filled(egui::Rect::from_min_size(egui::pos2(edge, tile.center().y - 10.0), egui::vec2(3.0, 20.0)), 1, BRIGHT());
}

/// Nothing for a healthy connection. One connecting spins a ring around its logo, and a failed one gets a red badge on
/// its corner with a painted "!", since the glyph from a fallback font sits off center.
fn status_mark(ui: &egui::Ui, tile: egui::Rect, s: &Status) {
    match s {
        Status::Connecting => egui::Spinner::new().size(30.0).color(WARNING()).paint_at(ui, egui::Rect::from_center_size(tile.center(), egui::vec2(30.0, 30.0))),
        Status::Failed(_) => {
            let p = ui.painter();
            let c = tile.right_top() + egui::vec2(-2.0, 2.0);
            p.circle_filled(c, 6.5, CHROME());
            p.circle_filled(c, 5.0, ALERT());
            let s = Stroke::new(1.4, Color32::WHITE);
            p.line_segment([c + egui::vec2(0.0, -2.6), c + egui::vec2(0.0, 0.6)], s);
            p.circle_filled(c + egui::vec2(0.0, 2.4), 0.8, Color32::WHITE);
        }
        _ => {}
    }
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

/// A tile carried beside the pointer at `p`: its engine logo on a raised square, painted without a widget so it never
/// covers the drop targets under the pointer.
fn ghost_tile(ui: &egui::Ui, p: egui::Pos2, c: &Conn) {
    let ctx = ui.ctx();
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("rail-ghost")));
    let rect = egui::Rect::from_min_size(p + egui::vec2(10.0, 6.0), egui::vec2(ICON, ICON));
    painter.add(ui.visuals().popup_shadow.as_shape(rect, R_TILE));
    let mark = engine_mark(c.meta.engine, &c.server.product).0;
    painter.rect_filled(rect, R_TILE, RAISED());
    gradient_rect(&painter, rect, R_TILE as f32, tint(mark_color(mark), tile_alpha(1)));
    painter.rect_stroke(rect, R_TILE, Stroke::new(1.5, PRIMARY()), egui::StrokeKind::Inside);
    logo(&painter, rect.center(), 22.0, mark, Color32::WHITE);
}

/// A connection or folder tile's column: the rail's width less its margins. The tile itself is an `ICON` square in it.
const TILE_W: f32 = RAIL_W - 16.0;
const ICON: f32 = 40.0;
const TILE_H: f32 = ICON;
/// A tile's slot in the rail list: the tile and the 8 px gap, half above and half below it.
const SLOT: f32 = TILE_H + 8.0;
/// A member's slot on an open shelf: its tile and a 6 px gap.
const MEMBER_SLOT: f32 = TILE_H + 6.0;
/// The name row at the top of a group's shelf.
const CAP_H: f32 = 20.0;
const CONNECT_H: f32 = ICON;

/// The slot of a shelf showing `rows` tiles (1 for a closed folder): 2 above it, the name row, the tiles, 4 of padding
/// at its bottom and 6 below it.
fn shelf_slot(rows: usize) -> f32 {
    CAP_H + rows as f32 * MEMBER_SLOT + 12.0
}

/// The rail order once item `m.from` of `items` moves to `m.to`: each folder brings its open members in the order they
/// had, so the folder keeps its anchor.
fn order_after_move(order: &[u64], items: &[RailItem], m: dnd::Move) -> Vec<u64> {
    let mut ids: Vec<Vec<u64>> = items
        .iter()
        .map(|it| match it {
            RailItem::Conn(id) => vec![*id],
            RailItem::Folder { members, .. } => order.iter().copied().filter(|c| members.contains(c)).collect(),
        })
        .collect();
    m.apply(&mut ids);
    ids.concat()
}

/// Where `Groups::place` puts the member moved by `m` inside an open folder that shows `open` of the group's `full`
/// members: next to its new neighbour among the open ones, counted in the full list without it. None for no change.
fn member_slot(full: &[u64], open: &[u64], m: dnd::Move) -> Option<usize> {
    if m.into || m.from == m.to || m.to >= open.len() {
        return None;
    }
    let mut now = open.to_vec();
    m.apply(&mut now);
    let dragged = now[m.to];
    let rest: Vec<u64> = full.iter().copied().filter(|c| *c != dragged).collect();
    match (now.get(m.to + 1), m.to.checked_sub(1).and_then(|i| now.get(i))) {
        (Some(next), _) => rest.iter().position(|c| c == next),
        (None, Some(prev)) => rest.iter().position(|c| c == prev).map(|i| i + 1),
        _ => None,
    }
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

/// The engine's silhouette as a white 64 px mask, tinted when painted. Decoded once and kept in egui's memory.
/// Baked to PNG so the binary needs no SVG renderer. Filled silhouettes, since line art and wordmarks blur away in the
/// rail's 16 px tiles and the 9 px folder previews.
pub(super) fn engine_logo(ctx: &egui::Context, mark: &str) -> Option<egui::TextureHandle> {
    let (name, png): (&str, &[u8]) = match mark {
        "MO" => ("engine-mongodb", include_bytes!("../../assets/engines/mongodb.png")),
        "MA" => ("engine-mariadb", include_bytes!("../../assets/engines/mariadb.png")),
        "PG" => ("engine-postgres", include_bytes!("../../assets/engines/postgres.png")),
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

/// A connection's tile: an app icon in the engine's color with the logo in white, a dash in the tag color at its foot for a
/// staging or production tag, a spinner or a red badge when it is connecting or failed, and the active marks. The name,
/// host and state are in the tooltip.
fn conn_shortcut(ui: &mut egui::Ui, rect: egui::Rect, c: &Conn, active: bool, tabs: impl FnOnce() -> usize) -> egui::Response {
    let tile = icon_rect(rect);
    // Click only: the reorder engine senses the drag under it.
    let r = ui.interact(tile, ui.scope_id().with(("tile", c.meta.id)), Sense::click());
    let state = state_of(&r);
    let (mark, _) = engine_mark(c.meta.engine, &c.server.product);
    let p = ui.painter();
    gradient_rect(p, tile, R_TILE as f32, tint(mark_color(mark), tile_alpha(state)));
    p.rect_stroke(tile, R_TILE, Stroke::new(1.0, tile_edge()), egui::StrokeKind::Inside);
    // A connection that is not ready yet, or failed, dims its logo under the status mark.
    let dim = matches!(c.status, Status::Connecting | Status::Failed(_));
    logo(p, tile.center(), 22.0, mark, if dim { Color32::WHITE.gamma_multiply(0.45) } else { Color32::WHITE });
    if let Some(level) = tag_level(&c.meta.tag) {
        p.rect_filled(egui::Rect::from_center_size(egui::pos2(tile.center().x, tile.bottom() - 4.0), egui::vec2(10.0, 2.0)), 1, level);
    }
    status_mark(ui, tile, &c.status);
    if active {
        active_mark(ui.painter(), tile);
    }
    focus_ring(ui, &r, tile, R_TILE as f32);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Switch to {}", c.meta.name)));
    // The tooltip and the tab count are worked out only when the tooltip shows.
    r.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_ui(|ui| conn_tip(ui, c, tabs()))
}

/// A connection's tooltip: its name and tag, product and host, and what it allows with how many tabs it has open.
fn conn_tip(ui: &mut egui::Ui, c: &Conn, tabs: usize) {
    let (name, product, host) = conn_label(c);
    ui.horizontal(|ui| {
        ui.label(semibold(RichText::new(name).color(BRIGHT())));
        if !c.meta.tag.is_empty() {
            ui.label(medium(RichText::new(&c.meta.tag).size(TXT_MICRO).color(tag_color(&c.meta.tag))));
        }
    });
    ui.label(RichText::new(if host.is_empty() { product } else { format!("{product} \u{b7} {host}") }).color(MUTED()));
    let state = match c.status {
        Status::Connecting => "Connecting",
        Status::Failed(_) => "Failed to connect",
        _ if c.writable => "Writes on",
        _ => "Read-only",
    };
    ui.label(RichText::new(format!("{state} \u{b7} {tabs} tab{}", if tabs == 1 { "" } else { "s" })).color(FAINT()));
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
    p.extend(icon_shapes(c, icon, s));
}

/// The shapes of a rail icon centered on `c`.
fn icon_shapes(c: egui::Pos2, icon: RailIcon, s: Stroke) -> Vec<egui::Shape> {
    use egui::Shape;
    let v = egui::vec2;
    let mut out = Vec::new();
    match icon {
        RailIcon::Search => {
            out.push(Shape::circle_stroke(c + v(-1.5, -1.5), 5.0, s));
            out.push(Shape::line_segment([c + v(2.2, 2.2), c + v(6.0, 6.0)], s));
        }
        // A gear: ring, hub and eight teeth.
        RailIcon::Settings => {
            out.push(Shape::circle_stroke(c, 4.6, s));
            out.push(Shape::circle_stroke(c, 1.8, s));
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::FRAC_PI_4;
                let d = v(a.cos(), a.sin());
                out.push(Shape::line_segment([c + d * 4.6, c + d * 7.0], s));
            }
        }
    }
    out
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
        assert_eq!((zone_of(r, 105.0), zone_of(r, 115.0), zone_of(r, 125.0)), (Zone::Before, Zone::Into, Zone::After));
    }

    #[test]
    fn drops_resolve_by_tile_and_zone() {
        let g = groups_of(&[("Prod", true, &[1, 2, 3])]);
        let r = |on, z, d| resolve_drop(on, z, d, &g).map(|x| x.0);
        assert_eq!(r(DropOn::Loose(8), Zone::Into, 9), Some(RailDrop::Pair { conn: 9, target: 8 }));
        assert_eq!(r(DropOn::Loose(8), Zone::After, 9), Some(RailDrop::Reorder { conn: 9, anchor: 8, after: true }));
        assert_eq!(r(DropOn::Loose(8), Zone::Into, 8), None);
        // Closed folder: the middle adds, a member of it does nothing, the edges reorder.
        assert_eq!(r(DropOn::Folder { gid: 1, anchor: 1 }, Zone::Into, 9), Some(RailDrop::Into { conn: 9, gid: 1, at: None }));
        assert_eq!(r(DropOn::Folder { gid: 1, anchor: 1 }, Zone::Into, 2), None);
        assert_eq!(r(DropOn::Folder { gid: 1, anchor: 1 }, Zone::Before, 9), Some(RailDrop::Reorder { conn: 9, anchor: 1, after: false }));
        // Inside the capsule the index counts the members after the dragged one leaves: 1 below 3 lands at 2.
        assert_eq!(r(DropOn::Member { id: 3, gid: 1 }, Zone::After, 1), Some(RailDrop::Into { conn: 1, gid: 1, at: Some(2) }));
        assert_eq!(r(DropOn::Member { id: 2, gid: 1 }, Zone::Before, 9), Some(RailDrop::Into { conn: 9, gid: 1, at: Some(1) }));
        assert_eq!(r(DropOn::FolderTop { gid: 1, anchor: 1 }, Zone::After, 9), Some(RailDrop::Into { conn: 9, gid: 1, at: Some(0) }));
    }

    #[test]
    fn a_moved_folder_brings_its_open_members_along() {
        let g = groups_of(&[("Prod", true, &[2, 4])]);
        let order = [1, 4, 3, 2];
        let items = rail_items(&order, &g);
        // Items: 1, the folder (4 then 2 in rail order), 3. The folder goes last and 4 stays its anchor.
        let m = dnd::Move { from: 1, to: 2, into: false };
        assert_eq!(order_after_move(&order, &items, m), [1, 3, 4, 2]);
        assert_eq!(order_after_move(&order, &items, dnd::Move { from: 2, to: 0, into: false }), [3, 1, 4, 2]);
    }

    #[test]
    fn a_member_moved_in_its_folder_keeps_the_closed_members_in_place() {
        // 9 is closed, so the folder shows 1, 2 and 3.
        let full = [1, 9, 2, 3];
        let open = [1, 2, 3];
        let mv = |from, to| member_slot(&full, &open, dnd::Move { from, to, into: false });
        // 3 to the top goes before 1; 1 to the end goes after 3; 1 below 2 goes before 3, after 9 and 2.
        assert_eq!(mv(2, 0), Some(0));
        assert_eq!(mv(0, 2), Some(3));
        assert_eq!(mv(0, 1), Some(2));
        assert_eq!(mv(1, 1), None);
    }

    #[test]
    fn a_closed_folder_shows_its_worst_member() {
        fn s(v: &[Status]) -> Option<&Status> {
            folder_status(v.iter())
        }
        assert!(s(&[Status::Ready, Status::Idle]) == Some(&Status::Ready));
        assert!(s(&[Status::Ready, Status::Connecting]) == Some(&Status::Connecting));
        assert!(matches!(s(&[Status::Connecting, Status::Failed("x".into()), Status::Ready]), Some(Status::Failed(_))));
        assert!(s(&[Status::Idle]).is_none());
    }

    #[test]
    fn drop_hints_say_what_the_drop_does() {
        let g = groups_of(&[("Fleet", true, &[1, 2]), ("Loose", false, &[3])]);
        let fleet = g.group_of(1).unwrap().id;
        assert_eq!(drop_hint(RailDrop::Pair { conn: 4, target: 5 }, &g).as_deref(), Some("New group"));
        assert_eq!(drop_hint(RailDrop::Into { conn: 4, gid: fleet, at: None }, &g).as_deref(), Some("Add to Fleet"));
        // A member moved inside its own folder, and a plain reorder, already show by their line.
        assert_eq!(drop_hint(RailDrop::Into { conn: 2, gid: fleet, at: Some(0) }, &g), None);
        assert_eq!(drop_hint(RailDrop::Reorder { conn: 4, anchor: 5, after: true }, &g), None);
        assert_eq!(drop_hint(RailDrop::Reorder { conn: 1, anchor: 5, after: true }, &g).as_deref(), Some("Remove from Fleet"));
        // A group that does not fold leaves its members loose in the rail, so a reorder takes nothing out of it.
        assert_eq!(drop_hint(RailDrop::Reorder { conn: 3, anchor: 5, after: true }, &g), None);
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
