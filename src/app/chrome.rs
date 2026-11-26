//! The window frame around the views: top bar, tab strip, status bar, toasts and keyboard shortcuts.

use super::*;
use crate::ui::dnd;
use quarry_core::action::sql::FilterA;
use quarry_core::action::{Action, ConnA, DocsA, Pref, PrefsA, SqlA, TabA};
use quarry_core::cmd::{KEY_ORDER, Layer, Scope};


impl App {
    /// What is over the page: Core's windows, or the app's modal. Each takes its own keys, Esc included, so page
    /// shortcuts stay off while one is open; a new kind of window belongs in `quarry_core::cmd::WINDOWS`.
    pub(super) fn layer(&self) -> Layer {
        if self.modal.is_some() { Layer::Modal } else { self.core.layer() }
    }

    pub(super) fn window_open(&self) -> bool {
        self.layer() != Layer::Page
    }

    pub(super) fn keys(&mut self, ctx: &egui::Context) {
        let page = self.layer() == Layer::Page;
        // A plain key ("/") only while nothing is being typed into, so it never steals a character from a field.
        let typing = ctx.memory(|m| m.focused().is_some());
        let text = ctx.text_edit_focused();
        // KEY_ORDER tries the longer combos first, since extra Shift and Alt still match a plain Cmd shortcut. A command
        // whose keys_on is false leaves its key alone. Cmd+S reaches the library only when no page handler took it: staged
        // SQL grid edits and document drafts consume it where they draw, before this runs, so they win. A new page handler
        // of Cmd+S must consume the key too, or its press would also open the Save dialog.
        for &(s, ch) in KEY_ORDER.iter() {
            let on = match s.scope {
                Scope::App => true,
                Scope::Page | Scope::View => page,
                Scope::Native => false,
            };
            if on && !(ch.mods() == 0 && typing) && s.cmd.keys_on(&self.core, text) && ctx.input_mut(|i| i.consume_shortcut(&shortcut(&ch))) {
                self.run_cmd(s.cmd, ctx);
            }
        }
        if !page {
            return;
        }
        let k = |m: egui::Modifiers, key: Key| ctx.input_mut(|i| i.consume_key(m, key));
        let none = egui::Modifiers::NONE;
        // Plain keys only act when no text field, menu or cell editor wants them. The boolean switch and the calendar
        // leave no text field focused but still read the arrows and letters.
        if ctx.memory(|m| m.focused().is_none()) && !ctx.any_popup_open() && !self.core.sql_cell.as_ref().is_some_and(|e| e.grabs_keys) && !self.core.inline.as_ref().is_some_and(|i| i.pick.is_some()) {
            let (up, down) = (k(none, Key::ArrowUp), k(none, Key::ArrowDown));
            let (prev, next) = (k(none, Key::PageUp), k(none, Key::PageDown));
            let nav = self.t().filter(|t| matches!(t.view, View::Documents | View::Aggregate)).map(|t| {
                let agg = t.view == View::Aggregate;
                let len = if agg { t.agg_docs.len() } else { t.docs.len() };
                let row = ((up || down) && len > 0).then(|| step(t.selected.filter(|s| s.0 == agg).map(|s| s.1), down, len));
                let (page, pages) = (t.page, t.count.div_ceil(self.page_size()).max(1));
                let go = if agg { None } else if prev && page > 0 { Some(page - 1) } else if next && page + 1 < pages { Some(page + 1) } else { None };
                (agg, row, go)
            });
            let tab = self.cur_uid().unwrap_or_default();
            if let Some((agg, row, go)) = nav {
                if let Some(row) = row {
                    self.exec(Action::Docs(DocsA::Step { tab, agg, row }));
                    self.ui.scroll_sel = true;
                }
                if let Some(page) = go {
                    self.exec(Action::Docs(DocsA::Page { tab, page }));
                }
            }
            // The Query grid keeps its own selection, so the arrows move that one.
            if (up || down) && let Some(s) = self.t().filter(|t| t.view == View::Sql).and_then(|t| t.sql.as_ref()).filter(|s| !s.text_mode) {
                let len = s.cur_set().map_or(0, |x| x.rows.len());
                if len > 0 {
                    let row = step(s.qsel, down, len);
                    self.exec(Action::Sql(SqlA::SelectRow { tab, row }));
                    self.ui.scroll_sel = true;
                }
            }
        }
        // Esc closes the innermost thing that is open. Dialogs and menus handle their own Esc.
        if ctx.any_popup_open() || !ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
            return;
        }
        self.exec(Action::Docs(DocsA::Escape));
    }

    /// The Documents view's query: rules mode opens the filter panel, SQL mode focuses its WHERE field, and Mongo its query.
    pub(super) fn focus_query(&mut self, ctx: &egui::Context) {
        let sql = self.t().and_then(|t| t.sql.as_ref().map(|s| (t.uid, s.bar.c.sql, s.bar.panel.open)));
        match sql {
            Some((tab, false, was_open)) => {
                self.exec(Action::Sql(SqlA::Filter { tab, a: FilterA::Open }));
                // Opening the panel folds it out, which lives in egui's memory.
                if !was_open {
                    sql::filter_panel::start_fold(ctx, true);
                }
            }
            Some(_) => ctx.memory_mut(|m| m.request_focus(query_id("SQL-WHERE"))),
            None => ctx.memory_mut(|m| m.request_focus(query_id("QUERY"))),
        }
    }

    pub(super) fn shortcuts_modal(&mut self) {
        self.modal = Some(Modal::Text { title: "Keyboard shortcuts".into(), body: quarry_core::cmd::sheet(), code: false });
    }

    pub(super) fn screenshot(&mut self, ctx: &egui::Context) {
        let Some((path, frames, slot)) = &mut self.shot else { return };
        // A window in the background is not held to vsync, so the 120 frames can pass in less than the fade-in of a popup
        // or dialog, which then shows half see-through in the picture. Pictures show the settled state.
        if *frames == 0 {
            ctx.all_styles_mut(|s| s.animation_time = 0.0);
        }
        // AppKit draws the window buttons outside our frame, so the capture would lack them.
        // Stand-ins go where set_traffic_lights_position puts the real ones.
        if cfg!(target_os = "macos") {
            let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("shot-window-buttons")));
            for (i, c) in [Color32::from_rgb(0xFF, 0x5F, 0x57), Color32::from_rgb(0xFE, 0xBC, 0x2E), Color32::from_rgb(0x28, 0xC8, 0x40)].into_iter().enumerate() {
                p.circle(egui::pos2(23.0 + 20.0 * i as f32, HEADER_H / 2.0), 6.0, c, Stroke::new(0.5, Color32::from_black_alpha(50)));
            }
        }
        *frames += 1;
        ctx.request_repaint();
        if *frames == 120 {
            let slot = slot.clone();
            ctx.request_screenshot(move |img| *slot.lock().unwrap() = Some((*img).clone()));
        }
        let taken = slot.lock().unwrap().take();
        if let Some(img) = taken {
            let px: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            match image::RgbaImage::from_raw(img.size[0] as u32, img.size[1] as u32, px).map(|i| i.save(&*path)) {
                Some(Ok(())) => eprintln!("saved {}", path.display()),
                other => eprintln!("screenshot failed: {other:?}"),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    pub(super) fn top_bar(&mut self, ui: &mut egui::Ui) {
        // Leaves room for the macOS window buttons, which sit over our header.
        let left = if cfg!(target_os = "macos") { 92 } else { 12 };
        let frame = egui::Frame::new().fill(CHROME()).inner_margin(egui::Margin { left, right: 12, top: 7, bottom: 7 });
        egui::Panel::top("top").exact_size(HEADER_H).show_separator_line(false).frame(frame).show(ui, |ui| {
            let full = ui.max_rect().expand2(egui::vec2(left as f32, 7.0));
            // Not over the rail, which shares the header's color, nor over the page's rounded corner, which draws its own edge.
            ui.painter().hline(egui::Rangef::new(full.left() + RAIL_W + R_LG as f32, full.right()), full.bottom() - 0.5, Stroke::new(1.0, LINE()));
            // Registered before the widgets so they stay clickable; the empty header moves the window.
            let bar = ui.interact(full, egui::Id::unique("titlebar"), Sense::click_and_drag());
            if bar.drag_started_by(egui::PointerButton::Primary) {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            if bar.double_clicked() {
                let max = ui.input(|i| i.viewport().maximized.unwrap_or(false));
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
            }
            let win = self.ui.win_buttons;
            ui.horizontal_centered(|ui| {
                if crate::ui::widgets::glyph_button(ui, egui::vec2(26.0, 26.0), glyphs::sidebar, "Toggle sidebar", "Show or hide the sidebar").clicked() {
                    let hidden = self.core.prefs.sidebar_hidden;
                    self.exec(Action::Prefs(PrefsA::Set(Pref::SidebarHidden(!hidden))));
                }
                ui.add_space(4.0);
                app_mark(ui);
                ui.add_space(-2.0);
                ui.label(semibold(RichText::new("quarry").size(TXT_TITLE).color(BRIGHT())));
                ui.add_space(22.0);
                if let Some(s) = self.t().map(|t| t.sel.clone()) {
                    self.crumbs(ui, &s);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if win {
                        ui.add_space(window_buttons::BUTTONS_W - 4.0);
                    }
                    let n = self.core.undo.len();
                    let undo = if n > 0 { format!("Undo ({n})") } else { "Undo".into() };
                    if quiet_enabled(ui, n > 0, undo).on_hover_text("Recent writes you can take back").clicked() {
                        self.modal = Some(Modal::Undo);
                    }
                    ui.add_space(12.0);
                    if search_pill(ui).clicked() {
                        self.open_palette();
                    }
                    ui.add_space(4.0);
                    self.update_pill(ui);
                });
            });
            if win {
                window_buttons::window_buttons(ui, full);
            }
        });
    }

    /// Refreshes the memory and CPU readouts once a second, and asks for the repaint that shows them.
    fn sample_usage(&mut self, ctx: &egui::Context) {
        let (at, cpu_then, pct, _) = self.ui.usage;
        let wall = at.elapsed();
        if wall >= Duration::from_secs(1) || cpu_then.is_zero() {
            let (mem, cpu_now) = process_usage();
            // A sample older than 2 s is from before the readouts were hidden, so it only starts a new second.
            let pct = if cpu_then.is_zero() || wall > Duration::from_secs(2) { pct } else { cpu_now.saturating_sub(cpu_then).as_secs_f32() / wall.as_secs_f32() * 100.0 };
            self.ui.usage = (std::time::Instant::now(), cpu_now, pct, mem);
        }
        ctx.request_repaint_after(Duration::from_secs(1));
    }

    pub(super) fn status_bar(&mut self, ui: &mut egui::Ui) {
        let frame = egui::Frame::new().fill(CHROME()).inner_margin(egui::Margin::symmetric(12, 0));
        egui::Panel::bottom("status").exact_size(STATUS_H).show_separator_line(false).frame(frame).show(ui, |ui| {
            let full = ui.max_rect().expand2(egui::vec2(12.0, 0.0));
            ui.painter().hline(full.x_range(), full.top() + 0.5, Stroke::new(1.0, LINE()));
            ui.horizontal_centered(|ui| {
                // The active connection: its state, then where it is and what it runs.
                if let Some(c) = self.core.active.and_then(|id| self.conn(id)).filter(|_| self.core.prefs.show_server) {
                    let (dot_r, dot_resp) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), Sense::hover());
                    ui.painter().circle_filled(dot_r.center(), 4.0, status_color(&c.status));
                    let state = match &c.status {
                        Status::Ready => "Connected",
                        Status::Connecting => "Connecting",
                        Status::Failed(_) => "Failed to connect",
                        Status::Idle => "Not connected",
                    };
                    dot_resp.on_hover_text(state);
                    ui.label(RichText::new(c.server.status_line(&c.masked_uri)).font(FontId::monospace(TXT_CAPTION)).color(MUTED()));
                    ui.add_space(10.0);
                }
                if self.core.prefs.show_fps {
                    // Averaged over half a second of frames: one frame's dt swings above the refresh
                    // rate when an input event lands just after a vsync. Idle gaps are left out, but
                    // not the Shell's 100 ms repaints while a script runs.
                    let dt = ui.input(|i| i.unstable_dt);
                    let (sum, n, shown) = &mut self.ui.fps;
                    if dt < 0.25 {
                        *sum += dt;
                        *n += 1;
                    }
                    if *sum >= 0.5 || *shown == 0.0 && *n > 0 {
                        *shown = *n as f32 / sum.max(0.000_1);
                        (*sum, *n) = (0.0, 0);
                    }
                    let rate = self.ui.fps.2.max(1.0);
                    let color = if self.ui.frame_ms > 1000.0 / rate * 0.8 { WARNING() } else { SUCCESS() };
                    fixed_readout(ui, format!("{:>3.0} fps · {:>4.1} ms", rate, self.ui.frame_ms), "000 fps · 00.0 ms", color).on_hover_text("Frames per second while something moves, and the time Quarry needs to build one frame. At 120 Hz each frame has 8.3 ms.");
                }
                if self.core.prefs.show_mem || self.core.prefs.show_cpu {
                    self.sample_usage(ui.ctx());
                    let (_, _, cpu, mem) = self.ui.usage;
                    if self.core.prefs.show_mem {
                        let text = mem.map_or("mem n/a".into(), |b| format!("mem {:>9}", human_bytes(b as usize)));
                        // Built only while hovered: it walks every row of every tab.
                        fixed_readout(ui, text, "mem 0000.0 MB", MUTED()).on_hover_ui(|ui| self.mem_tip(ui, mem));
                    }
                    if self.core.prefs.show_cpu {
                        let color = if cpu >= 80.0 { WARNING() } else { MUTED() };
                        fixed_readout(ui, format!("cpu {:>3.0}%", cpu), "cpu 000%", color).on_hover_text("Quarry's CPU use over the last second, in percent of one core.");
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.agents_pill(ui);
                    let log = RichText::new("Query log").size(TXT_CAPTION);
                    if quiet(ui, if self.ui.log.open { log.color(BRIGHT()) } else { log }).on_hover_text("Every command sent to the servers (Cmd+L)").clicked() {
                        self.ui.log.open = !self.ui.log.open;
                    }
                    let connected = self.core.active.and_then(|id| self.conn(id)).is_some_and(|c| c.client.is_some() || c.sql.is_some());
                    let tip = if connected { "Server status" } else { "Server status (connect first)" };
                    if quiet_enabled(ui, connected, RichText::new("Server").size(TXT_CAPTION)).on_hover_text(tip).clicked()
                        && let Some(conn) = self.core.active
                    {
                        self.open_server(conn);
                    }
                    ui.add_space(8.0);
                    ui.label(RichText::new("⌘K jump   ↵ run   ⌘/ keys").font(FontId::monospace(TXT_CAPTION)).color(FAINT())).on_hover_text("Cmd+K jumps to a collection, Enter runs the query, double-click a value to edit it, Cmd+/ lists every shortcut");
                    ui.add_space(10.0);
                    self.errors_chip(ui);
                    // The message gets only the room left between the connection and the hints, so a
                    // long one is cut with an ellipsis instead of running under them.
                    ui.add_space(12.0);
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        if self.export_progress(ui) || self.sql_job_progress(ui) {
                            return;
                        }
                        let first = self.core.status.lines().next().unwrap_or_default();
                        ui.add(egui::Label::new(RichText::new(first).size(TXT_CAPTION).color(FAINT())).truncate()).on_hover_text(self.core.status.as_str());
                    });
                });
            });
        });
    }

    pub(super) fn tab_strip(&mut self, ui: &mut egui::Ui) {
        let mut pick = None;
        let mut close = None;
        let mut dup = false;
        let mut many: Option<(usize, TabClose)> = None;
        let mut reopen = false;
        let mut search = None;
        let mut tab_move: Option<dnd::Move> = None;
        // Move left or Move right picked in a tab's menu.
        let mut menu_move: Option<dnd::Move> = None;
        let has_closed = !self.core.closed.is_empty();
        const TAB_MIN_W: f32 = 146.0;
        // Left of the name: the engine logo and its gap. Right: the close cross.
        const TAB_PAD_L: f32 = 38.0;
        // Right of the text: the cross, and the gap where the second line is cut, with a pixel to spare for rounding.
        const TAB_PAD_R: f32 = 34.0;
        const PLUS_W: f32 = 32.0;
        const LIST_W: f32 = 84.0;
        let n = self.core.tabs.len();
        // The name is left as a placeholder so its color can follow hover, which is known only after layout.
        let titles: Vec<_> = self.core.tabs.iter().enumerate().map(|(i, t)| {
            let mut job = LayoutJob::default();
            job.append(&self.tab_name(i), 0.0, ui_fmt(TXT_BODY, Color32::PLACEHOLDER, if i == self.core.cur { W_MEDIUM } else { 400.0 }, 0.0));
            let v = match t.view {
                View::Aggregate => Some("aggregate"),
                View::Indexes => Some("indexes"),
                View::Shell => Some("shell"),
                View::Schema => Some("schema"),
                View::Import => Some("import"),
                View::Compare => Some("compare"),
                View::Watch => Some("watch"),
                View::Structure => Some("structure"),
                View::Relations => Some("relations"),
                View::Triggers => Some("triggers"),
                View::Info => Some("info"),
                View::Sql => Some("sql"),
                View::Documents => None,
            };
            if let Some(v) = v {
                job.append(v, 6.0, mono_fmt(TXT_MICRO, C_LINK(), 400.0));
            }
            let line2 = crumbs::tab_second_line(self.conn(t.sel.conn).map_or("", |c| c.meta.name.as_str()), t.sel.scope_label());
            let sub = LayoutJob::single_section(line2, mono_fmt(TXT_MICRO, FAINT(), 400.0));
            ui.fonts_mut(|f| (f.layout_job(job), f.layout_job(sub)))
        }).collect();
        let frame = egui::Frame::new().fill(PANEL()).inner_margin(egui::Margin { left: 0, right: 8, top: 0, bottom: 0 });
        egui::Panel::top("tabs").exact_size(TAB_STRIP_H).show_separator_line(false).frame(frame).show(ui, |ui| {
        let panel = ui.max_rect();
        // Under the tabs, so the active one covers it and joins the page below.
        ui.painter().hline(panel.left()..=panel.right() + 8.0, panel.bottom() - 0.5, Stroke::new(1.0, LINE()));
        // Like a browser, tabs narrow down to TAB_MIN_W before the strip has to scroll.
        let full = ui.available_width();
        let cap = ((full - PLUS_W) / n.max(1) as f32).max(TAB_MIN_W);
        let widths: Vec<f32> = titles.iter().map(|g| (g.0.size().x.max(g.1.size().x) + TAB_PAD_L + TAB_PAD_R).clamp(TAB_MIN_W, 260.0).min(cap)).collect();
        let overflow = widths.iter().sum::<f32>() + PLUS_W > full;
        let view_w = full - PLUS_W - if overflow { LIST_W } else { 0.0 };
        // Scroll to the active tab whenever it changes, however it was picked, shortcuts included.
        let moved = self.ui.tab_seen != Some(self.core.cur);
        self.ui.tab_seen = Some(self.core.cur);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            // egui's own edge fades, opaque, show there are more tabs now that the scrollbar is hidden.
            ui.spacing_mut().scroll.fade = egui::style::ScrollFadeStyle { strength: 1.0, size: 24.0 };
            // The floating scrollbar grew over the labels on hover; the wheel and trackpad still scroll.
            egui::ScrollArea::horizontal()
                .id_salt("tabs")
                .max_width(view_w)
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    // Tabs drag sideways to reorder; the engine's drag sits under each tab's own click, so a click picks,
                    // a middle click closes and the right click opens the menu as before.
                    let keys: Vec<egui::Id> = self.core.tabs.iter().map(|t| egui::Id::unique(("tab", t.uid))).collect();
                    tab_move = dnd::reorder(ui, egui::Id::unique("tab-strip"), dnd::Items::horizontal(&widths, TAB_STRIP_H).keys(&keys), |ui, i, st| {
                        let t = &self.core.tabs[i];
                        let (title, sub) = titles[i].clone();
                        let active = i == self.core.cur;
                        let rect = ui.max_rect();
                        let resp = ui.interact(rect, ui.scope_id().with("tab"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
                        resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, active, format!("Tab {}", self.tab_name(i))));
                        if active && moved && !st.busy {
                            resp.scroll_to_me(None);
                        }
                        let p = ui.painter();
                        let hovered = resp.hovered() && !st.busy;
                        if active {
                            // The page's fill and an accent underline, covering the strip's bottom line
                            // so the tab joins what it shows.
                            p.rect_filled(rect, 0, BG());
                            p.rect_filled(egui::Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - 2.0), rect.max), 0, PRIMARY());
                        } else {
                            p.rect_filled(rect.with_max_y(rect.bottom() - 1.0), 0, if st.lifted { PANEL() } else { hover_fill(if st.busy { 0 } else { state_of(&resp) }) });
                        }
                        if !st.lifted {
                            p.vline(rect.right() - 0.5, rect.shrink2(egui::vec2(0.0, 7.0)).y_range(), Stroke::new(1.0, LINE()));
                        }
                        let conn = self.conn(t.sel.conn);
                        // The tag's edge on top reads before the name does: red for production, amber for staging.
                        if let Some(edge) = conn.and_then(|c| tag_level(&c.meta.tag)) {
                            p.rect_filled(rect.with_max_y(rect.top() + 3.0), 0, if active || hovered { edge } else { tint(edge, 170) });
                        }
                        if let Some(c) = conn {
                            let (mark, _) = rail::engine_mark(c.meta.engine, &c.server.product);
                            rail::logo(p, egui::pos2(rect.left() + 22.0, rect.center().y + 1.0), 16.0, mark, if active || hovered { rail::mark_color(mark) } else { tint(rail::mark_color(mark), 150) });
                        }
                        let clip = egui::Rect::from_min_max(rect.min, egui::pos2(rect.right() - 28.0, rect.bottom()));
                        let name = if active { BRIGHT() } else if hovered || st.lifted { TEXT() } else { MUTED() };
                        let tx = rect.left() + TAB_PAD_L;
                        // Cut with an ellipsis at the cross, so the database goes first and the connection stays.
                        let sub = truncated(p, &sub.job.text, FontId::monospace(TXT_MICRO), FAINT(), clip.right() - tx - 4.0);
                        let top = rect.center().y + 1.0 - (title.size().y + sub.size().y + 1.0) / 2.0;
                        let below = top + title.size().y + 1.0;
                        let pc = p.with_clip_rect(clip.intersect(p.clip_rect()));
                        pc.galley(egui::pos2(tx, top), title, name);
                        pc.galley(egui::pos2(tx, below), sub, FAINT());
                        if st.lifted {
                            return resp;
                        }
                        let xr = egui::Rect::from_center_size(egui::pos2(rect.right() - 14.0, rect.center().y), egui::vec2(16.0, 16.0));
                        let xresp = ui.interact(xr, ui.scope_id().with("close"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
                        xresp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Close tab {}", self.tab_name(i))));
                        // A tab whose library script has unsaved changes shows a dot where the cross is, until the pointer
                        // comes to close it, as editors do.
                        if self.tab_edited(i) && !hovered && !xresp.hovered() {
                            ui.painter().circle_filled(xr.center(), 3.5, if active { TEXT() } else { MUTED() });
                        } else if active || hovered || xresp.hovered() {
                            if xresp.is_pointer_button_down_on() {
                                ui.painter().rect_filled(xr, R_SM, PRESSED());
                            } else if xresp.hovered() {
                                ui.painter().rect_filled(xr, R_SM, if active { HOVER() } else { PRESSED() });
                            }
                            let c = xr.center();
                            let s = Stroke::new(1.4, if xresp.hovered() { BRIGHT() } else { MUTED() });
                            ui.painter().line_segment([c + egui::vec2(-3.5, -3.5), c + egui::vec2(3.5, 3.5)], s);
                            ui.painter().line_segment([c + egui::vec2(3.5, -3.5), c + egui::vec2(-3.5, 3.5)], s);
                        }
                        let place = if t.sel.schema.is_empty() || t.sel.coll.is_empty() { t.sel.db.clone() } else { format!("{} / {}", t.sel.db, t.sel.schema) };
                        let (conn_name, product, host) = conn.map(conn_label).unwrap_or_default();
                        let mut tip = format!("{conn_name} / {place} / {}\n{product} on {host}", self.tab_name(i));
                        if let Some(c) = conn.filter(|c| !c.meta.tag.is_empty()) {
                            tip += &format!("\nTag: {}", c.meta.tag);
                        }
                        let resp = resp.on_hover_text(format!("{tip}\nDrag to reorder, middle-click closes, right-click for more"));
                        resp.context_menu(|ui| {
                            menu_look(ui);
                            ui.set_min_width(190.0);
                            for (label, what, enabled) in [
                                ("Close", TabClose::This, true),
                                ("Close others", TabClose::Others, n > 1),
                                ("Close tabs to the left", TabClose::Left, i > 0),
                                ("Close tabs to the right", TabClose::Right, i + 1 < n),
                                ("Close all", TabClose::All, true),
                            ] {
                                if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                                    many = Some((i, what));
                                }
                            }
                            ui.separator();
                            if ui.button("Duplicate").clicked() {
                                pick = Some(i);
                                dup = true;
                            }
                            if ui.add_enabled(has_closed, egui::Button::new("Reopen closed tab").shortcut_text("Cmd+Shift+T")).clicked() {
                                reopen = true;
                            }
                            ui.separator();
                            if let Some(mv) = dnd::move_items(ui, n, i, &[], true) {
                                menu_move = Some(mv);
                            }
                        });
                        if xresp.clicked() || resp.middle_clicked() {
                            close = Some(i);
                        } else if resp.clicked() {
                            pick = Some(i);
                        }
                        resp
                    });
                });
            // Outside the scroll area so it stays reachable when the strip overflows.
            ui.add_space(4.0);
            if n > 0 {
                // Painted like the close cross: the font's "+" sits on the math axis, below center.
                let (r, resp) = ui.allocate_exact_size(egui::vec2(BUTTON_H, TAB_STRIP_H), Sense::click());
                let b = egui::Rect::from_center_size(r.center(), egui::vec2(BUTTON_H, BUTTON_H));
                ui.painter().rect_filled(b, R_MD, hover_fill(state_of(&resp)));
                let (c, s) = (b.center(), Stroke::new(1.5, if resp.hovered() { BRIGHT() } else { MUTED() }));
                ui.painter().line_segment([c - egui::vec2(5.5, 0.0), c + egui::vec2(5.5, 0.0)], s);
                ui.painter().line_segment([c - egui::vec2(0.0, 5.5), c + egui::vec2(0.0, 5.5)], s);
                // A browser's "+": Duplicate stays in the tab's right-click menu.
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(format!("New tab ({})", keys(false, false, "T"))).clicked() {
                    self.open_new_tab();
                }
            }
            if overflow {
                ui.add_space(6.0);
                self.tab_list(ui, &mut pick, &mut close, &mut search);
            }
        });
        });
        let uid = |app: &App, i: usize| app.core.tabs.get(i).map(|t| t.uid);
        if let Some(m) = tab_move.or(menu_move) {
            // A drop into a tab is not a move; tabs only reorder.
            if let (false, Some(tab)) = (m.into, uid(self, m.from)) {
                self.exec(Action::Tab(TabA::Move { tab, to: m.to }));
            }
            ui.ctx().request_repaint();
            return;
        }
        if pick.is_some() {
            // Scroll even when the picked tab was already active but scrolled out of view.
            self.ui.tab_seen = None;
        }
        if reopen {
            return self.exec(Action::Tab(TabA::Reopen));
        }
        if let Some(query) = search {
            return self.open_palette_with(query);
        }
        if let Some((i, what)) = many {
            if let Some(tab) = uid(self, i) {
                self.exec(Action::Tab(TabA::CloseMany { tab, what }));
            }
        } else if let Some(i) = close {
            self.ask_close_tab(i);
        } else if let Some(tab) = pick.and_then(|i| uid(self, i)) {
            self.exec(Action::Tab(if dup { TabA::Duplicate { tab } } else { TabA::Pick { tab } }));
        }
    }

    pub(super) fn welcome(&mut self, ui: &mut egui::Ui) {
        if let Some(id) = self.core.active.filter(|id| self.conn(*id).is_some_and(|c| matches!(c.status, Status::Failed(_)))) {
            return self.conn_failed_card(ui, id);
        }
        ui.vertical_centered(|ui| {
            ui.add_space(120.0);
            ui.label(RichText::new("Pick a collection on the left").size(TXT_HEADING).color(BRIGHT()));
            ui.add_space(6.0);
            ui.label(RichText::new("A click opens it in this tab; Cmd+click or Cmd+T opens a new one. Every connection starts read-only; edits need an explicit unlock, show a change preview, and can be undone.").color(MUTED()));
            ui.add_space(16.0);
            if ui.button("+ Add a connection").clicked() {
                self.new_conn_modal();
            }
            ui.add_space(4.0);
            if quiet(ui, "Add from Cloud SQL…").clicked() {
                self.open_cloud_browser(false);
            }
        });
    }

    pub(super) fn new_conn_modal(&mut self) {
        self.exec(Action::Conn(ConnA::ForgetTest));
        self.modal = Some(Modal::Conn(Box::new(ConnForm { uri: "mongodb://localhost:27017".into(), ..Default::default() })));
    }
}

/// A status bar readout in a slot as wide as `template`, so the text after it does not shift
/// when the numbers change width.
fn fixed_readout(ui: &mut egui::Ui, text: String, template: &str, color: Color32) -> egui::Response {
    let font = FontId::monospace(TXT_CAPTION);
    let w = ui.fonts_mut(|f| f.layout_no_wrap(template.into(), font.clone(), color).size().x);
    let g = ui.fonts_mut(|f| f.layout_no_wrap(text, font, color));
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w.max(g.size().x) + 8.0, g.size().y), Sense::hover());
    ui.painter().galley(rect.left_top(), g, color);
    resp
}

/// The app's memory in bytes and the CPU time it has used so far.
#[cfg(target_os = "macos")]
fn process_usage() -> (Option<u64>, Duration) {
    // phys_footprint is what Activity Monitor shows as Memory.
    let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::proc_pid_rusage(libc::getpid(), libc::RUSAGE_INFO_V2, &mut info as *mut _ as *mut libc::rusage_info_t) } == 0;
    (ok.then_some(info.ri_phys_footprint), unix_cpu_time())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn process_usage() -> (Option<u64>, Duration) {
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    let rss = std::fs::read_to_string("/proc/self/statm").ok().and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok()).map(|p| p * page);
    (rss, unix_cpu_time())
}

#[cfg(unix)]
fn unix_cpu_time() -> Duration {
    let mut r: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut r) };
    let tv = |t: libc::timeval| Duration::from_secs(t.tv_sec as u64) + Duration::from_micros(t.tv_usec as u64);
    tv(r.ru_utime) + tv(r.ru_stime)
}

#[cfg(windows)]
fn process_usage() -> (Option<u64>, Duration) {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    unsafe {
        let p = GetCurrentProcess();
        let mut m: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
        m.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        // The working set, resident memory like the Linux readout and the Settings text say.
        let mem = (GetProcessMemoryInfo(p, &mut m as *mut _ as *mut _, m.cb) != 0).then_some(m.WorkingSetSize as u64);
        let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut c, mut e, mut k, mut u) = (z, z, z, z);
        let ticks = |t: FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
        let cpu = if GetProcessTimes(p, &mut c, &mut e, &mut k, &mut u) != 0 { Duration::from_nanos((ticks(k) + ticks(u)) * 100) } else { Duration::ZERO };
        (mem, cpu)
    }
}

/// The app icon, decoded once and kept as a texture in egui's memory.
fn app_mark(ui: &mut egui::Ui) {
    let id = egui::Id::unique("quarry-logo");
    let tex = ui.ctx().data(|d| d.get_temp::<egui::TextureHandle>(id)).or_else(|| {
        let icon = eframe::icon_data::from_png_bytes(include_bytes!("../../assets/icon-256.png")).ok()?;
        let img = egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
        let tex = ui.ctx().load_texture("quarry-logo", img, egui::TextureOptions::LINEAR);
        ui.ctx().data_mut(|d| d.insert_temp(id, tex.clone()));
        Some(tex)
    });
    let (rect, _) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), Sense::hover());
    if let Some(tex) = tex {
        // The PNG follows the macOS icon grid: the tile has a 100/1024 margin, cropped here so
        // the mark fills its box like the text beside it.
        const M: f32 = 100.0 / 1024.0;
        let uv = egui::Rect::from_min_max(egui::pos2(M, M), egui::pos2(1.0 - M, 1.0 - M));
        ui.painter().image(tex.id(), rect, uv, Color32::WHITE);
    }
}

/// The title bar's jump field: looks like a search box and opens the Cmd+K palette.
fn search_pill(ui: &mut egui::Ui) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(260.0, 24.0), Sense::click());
    let down = r.is_pointer_button_down_on();
    let (fill, edge) = if down { (RAISED(), MUTED()) } else if r.hovered() { (PANEL(), MUTED()) } else { (PANEL(), BORDER()) };
    let p = ui.painter();
    p.rect(rect, R_MD, fill, Stroke::new(1.0, if r.has_focus() { PRIMARY() } else { edge }), egui::StrokeKind::Inside);
    let hint = p.layout_no_wrap("Search or jump to…".into(), FontId::proportional(TXT_SMALL), if r.hovered() { MUTED() } else { FAINT() });
    p.galley(egui::pos2(rect.left() + 12.0, rect.center().y - hint.size().y / 2.0), hint, Color32::PLACEHOLDER);
    let k = p.layout_job(LayoutJob::single_section(keys(false, false, "K"), mono_fmt(TXT_MICRO, MUTED(), 400.0)));
    let kr = egui::Rect::from_min_size(egui::pos2(rect.right() - 6.0 - (k.size().x + 10.0), rect.center().y - 8.0), egui::vec2(k.size().x + 10.0, 16.0));
    paint_kbd(p, kr, k);
    r.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Jump to a connection, database or collection")
}

/// The row an arrow key moves a grid's selection to: the first one when none is selected.
fn step(cur: Option<usize>, down: bool, len: usize) -> usize {
    match cur {
        None => 0,
        Some(i) if down => (i + 1).min(len - 1),
        Some(i) => i.saturating_sub(1),
    }
}
