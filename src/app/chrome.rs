//! The window frame around the views: top bar, tab strip, status bar, toasts and keyboard shortcuts.

use super::*;


impl App {
    /// Keeps the open tabs and their inputs in the settings file, so the next launch reopens them.
    pub(super) fn remember_tabs(&mut self) {
        if self.ephemeral || self.shot.is_some() {
            return;
        }
        let tabs: Vec<SavedTab> = self
            .tabs
            .iter()
            .map(|t| SavedTab {
                conn: t.sel.conn,
                db: t.sel.db.clone(),
                schema: t.sel.schema.clone(),
                coll: t.sel.coll.clone(),
                view: match t.view {
                    View::Documents => "Documents",
                    View::Aggregate => "Aggregate",
                    View::Indexes => "Indexes",
                    View::Shell => "Shell",
                    View::Schema => "Schema",
                    View::Import => "Import",
                    View::Compare => "Compare",
                    View::Watch => "Watch",
                    View::Structure => "Structure",
                    View::Relations => "Relations",
                    View::Triggers => "Triggers",
                    View::Info => "Info",
                    View::Sql => "Sql",
                }
                .into(),
                filter: t.filter_txt.clone(),
                sort: t.sort_txt.clone(),
                project: t.proj_txt.clone(),
                agg: t.agg_txt.clone(),
                shell: t.shell.code.clone(),
                attached: t.shell.attached.clone(),
                clauses: t.sql.as_ref().and_then(|s| serde_json::to_string(&s.bar.c).ok()).unwrap_or_default(),
                query: t.sql.as_ref().map(|s| s.query.clone()).unwrap_or_default(),
                run_db: t.sql.as_ref().map(|s| s.run_db.clone()).unwrap_or_default(),
                run_schema: t.sql.as_ref().map(|s| s.run_schema.trim().to_string()).unwrap_or_default(),
                stages: if t.stages_src == t.agg_txt { t.stages.iter().map(|s| SavedStage { op: s.op.clone(), text: s.text.clone(), on: s.on }).collect() } else { Vec::new() },
                agg_text_mode: t.agg_text_mode,
            })
            .collect();
        let open: Vec<u64> = self.rail_order.iter().copied().filter(|id| self.conn(*id).is_some_and(|c| matches!(c.status, Status::Ready | Status::Connecting))).collect();
        if tabs != self.prefs.open_tabs || self.cur != self.prefs.cur_tab || self.active != self.prefs.last_conn || open != self.prefs.open_conns {
            self.prefs.open_tabs = tabs;
            self.prefs.cur_tab = self.cur;
            self.prefs.last_conn = self.active;
            self.prefs.open_conns = open;
            self.save_prefs();
        }
    }

    /// A dialog or window over the page. It takes its own keys, Esc included, so page shortcuts
    /// stay off while it is open; a new kind of window belongs in this list.
    pub(super) fn window_open(&self) -> bool {
        self.modal.is_some() || self.cloud_browser.is_some() || self.insert.is_some() || self.value_dialog_open() || self.users.is_some() || self.server.is_some() || self.sql_users.is_some() || self.pg_roles.is_some() || self.sql_server.is_some() || self.pg_server.is_some() || self.tool.is_some() || !self.agents.asks.is_empty() || self.agents.activity_open || self.agents.rules.is_some()
    }

    pub(super) fn keys(&mut self, ctx: &egui::Context) {
        if self.window_open() {
            return;
        }
        let k = |m: egui::Modifiers, key: Key| ctx.input_mut(|i| i.consume_key(m, key));
        let (none, cmd, shift, ctrl, alt) = (egui::Modifiers::NONE, egui::Modifiers::COMMAND, egui::Modifiers::SHIFT, egui::Modifiers::CTRL, egui::Modifiers::ALT);
        // Extra Shift and Alt still match a plain Cmd shortcut, so the longer combos go first.
        if k(cmd | shift, Key::T) {
            self.reopen_tab();
        }
        if k(cmd, Key::W) {
            self.close_tab(self.cur);
        }
        if k(cmd, Key::K) {
            self.open_palette();
        }
        if k(cmd, Key::T) {
            self.open_new_tab();
        }
        let query_view = self.t().is_some_and(|t| t.view == View::Sql && t.sql.is_some());
        if query_view && k(cmd | shift, Key::Enter) {
            self.sql_run(true, false);
        } else if query_view && k(cmd, Key::Enter) {
            self.sql_run(false, false);
        }
        if k(cmd, Key::E) {
            if query_view {
                self.sql_run(false, true);
            } else {
                self.explain();
            }
        }
        if k(cmd | shift, Key::R) && let Some(id) = self.active.filter(|_| !self.demo) {
            self.connect(id);
        }
        if k(cmd, Key::R) && !self.tabs.is_empty() {
            self.reload();
        }
        // "/" only while nothing is being typed into, so it never steals a slash from a field.
        let typing = ctx.memory(|m| m.focused().is_some());
        if k(cmd | shift, Key::F) || (!typing && k(none, Key::Slash)) {
            self.show_sidebar();
            ctx.memory_mut(|m| m.request_focus(egui::Id::unique("tree-filter")));
        }
        if k(cmd, Key::F) && !self.tabs.is_empty() {
            self.set_view(View::Documents);
            ctx.memory_mut(|m| m.request_focus(query_id("QUERY")));
        }
        if k(cmd, Key::Comma) {
            self.modal = Some(Modal::Settings);
        }
        if k(cmd, Key::N) {
            self.new_conn_modal();
        }
        if k(cmd, Key::L) {
            self.log.open = !self.log.open;
        }
        if k(cmd, Key::Slash) {
            self.shortcuts_modal();
        }
        let n = self.tabs.len();
        if n > 0 {
            if k(ctrl | shift, Key::Tab) || k(cmd | shift, Key::OpenBracket) || k(cmd | shift, Key::OpenCurlyBracket) {
                self.go_tab((self.cur + n - 1) % n);
            }
            if k(ctrl, Key::Tab) || k(cmd | shift, Key::CloseBracket) || k(cmd | shift, Key::CloseCurlyBracket) {
                self.go_tab((self.cur + 1) % n);
            }
            const DIGITS: [Key; 9] = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
            // The palette shows these as each view's position in MONGO_VIEWS.
            let views: &[(View, &str)] = if self.t().is_some_and(|t| t.sql.is_some()) { &SQL_VIEWS } else { &MONGO_VIEWS };
            for (d, (v, _)) in DIGITS.iter().zip(views) {
                if k(cmd | alt, *d) {
                    self.set_view(*v);
                }
            }
            // After the Cmd+Shift brackets above, which would also match these.
            if k(cmd, Key::OpenBracket) {
                self.nav(true);
            }
            if k(cmd, Key::CloseBracket) {
                self.nav(false);
            }
            for (i, d) in DIGITS.iter().enumerate() {
                if k(cmd, *d) {
                    self.go_tab(if i == 8 { n - 1 } else { i.min(n - 1) });
                }
            }
        }
        // Plain keys only act when no text field, menu or cell editor wants them. The boolean switch and the calendar
        // leave no text field focused but still read the arrows and letters.
        if ctx.memory(|m| m.focused().is_none()) && !ctx.any_popup_open() && !self.sql_cell.as_ref().is_some_and(|e| e.grabs_keys) && !self.inline.as_ref().is_some_and(|i| i.pick.is_some()) {
            let (up, down) = (k(none, Key::ArrowUp), k(none, Key::ArrowDown));
            let (prev, next) = (k(none, Key::PageUp), k(none, Key::PageDown));
            let nav = self.t().filter(|t| matches!(t.view, View::Documents | View::Aggregate)).map(|t| {
                let agg = t.view == View::Aggregate;
                let len = if agg { t.agg_docs.len() } else { t.docs.len() };
                let row = ((up || down) && len > 0).then(|| match t.selected.filter(|s| s.0 == agg) {
                    None => 0,
                    Some((_, i)) if down => (i + 1).min(len - 1),
                    Some((_, i)) => i.saturating_sub(1),
                });
                let (page, pages) = (t.page, t.count.div_ceil(self.page_size()).max(1));
                let go = if agg { None } else if prev && page > 0 { Some(page - 1) } else if next && page + 1 < pages { Some(page + 1) } else { None };
                (agg, row, go)
            });
            if let Some((agg, row, go)) = nav {
                if let Some(i) = row {
                    // The open card moves with the keys instead of leaving the previous one open.
                    if let Some(prev) = self.t().and_then(|t| t.selected) {
                        self.tm().unwrap().expanded.remove(&prev);
                    }
                    self.select(agg, i, false);
                    self.scroll_sel = true;
                }
                if let Some(p) = go {
                    self.tm().unwrap().page = p;
                    self.reload();
                }
            }
        }
        // Esc closes the innermost thing that is open. Dialogs and menus handle their own Esc.
        if ctx.any_popup_open() || !ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
            return;
        }
        // The calendar of a date closes before the editor under it.
        if let Some(i) = &mut self.inline && let Some(o) = i.pick.take() {
            i.text = o;
            return;
        }
        if self.inline.take().is_some() || self.new_field.take().is_some() {
            return;
        }
        if let Some(t) = self.tm() {
            if let Some(key) = t.selected {
                return self.close_row(key);
            }
            if !t.expanded.is_empty() {
                t.expanded.clear();
                return;
            }
        }
        self.tree_filter.clear();
    }

    pub(super) fn shortcuts_modal(&mut self) {
        let body = SHORTCUTS.iter().map(|(keys, what)| format!("{keys:<30}{what}")).collect::<Vec<_>>().join("\n");
        self.modal = Some(Modal::Text { title: "Keyboard shortcuts".into(), body, code: false });
    }

    pub(super) fn go_tab(&mut self, i: usize) {
        if i != self.cur && i < self.tabs.len() {
            self.set_cur(i);
            self.reset_doc_state();
        }
    }

    pub(super) fn set_view(&mut self, v: View) {
        let Some(t) = self.tm().filter(|t| t.view != v) else { return };
        t.view = v;
        t.selected = None;
        // The editor belongs to the Content grid; another view must never inherit it.
        self.sql_cell = None;
        self.reset_doc_state();
        if v == View::Indexes {
            self.load_indexes();
        }
    }

    pub(super) fn screenshot(&mut self, ctx: &egui::Context) {
        let Some((path, frames, slot)) = &mut self.shot else { return };
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
            ui.painter().hline(full.x_range(), full.bottom() - 0.5, Stroke::new(1.0, LINE()));
            // Registered before the widgets so they stay clickable; the empty header moves the window.
            let bar = ui.interact(full, egui::Id::unique("titlebar"), Sense::click_and_drag());
            if bar.drag_started_by(egui::PointerButton::Primary) {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            if bar.double_clicked() {
                let max = ui.input(|i| i.viewport().maximized.unwrap_or(false));
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
            }
            ui.horizontal_centered(|ui| {
                app_mark(ui);
                ui.add_space(-2.0);
                ui.label(semibold(RichText::new("quarry").size(TXT_TITLE).color(BRIGHT())));
                ui.add_space(22.0);
                if let Some(s) = self.t().map(|t| t.sel.clone()) {
                    let (name, writable) = self.conn(s.conn).map(|c| (c.meta.name.clone(), c.writable)).unwrap_or_default();
                    let mut job = LayoutJob::default();
                    // A query tab has no table, so its own name ends the path.
                    let last = self.tab_name(self.cur);
                    // A PostgreSQL path has a schema level between the database and the table.
                    let place = if s.schema.is_empty() || s.coll.is_empty() { s.db.clone() } else { format!("{} / {}", s.db, s.schema) };
                    job.append(&format!("{name} / {place} / "), 0.0, mono_fmt(12.0, MUTED(), 400.0));
                    job.append(&last, 0.0, mono_fmt(12.0, BRIGHT(), W_SEMIBOLD));
                    ui.label(job);
                    ui.add_space(4.0);
                    if self.prefs.always_writable {
                        // No lock to toggle: writes are always on.
                    } else if writable {
                        if badge(ui, "WRITES ON", ALERT()).on_hover_text("Click to lock this connection again").clicked() {
                            if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == s.conn) {
                                c.writable = false;
                            }
                            self.ok("Writes locked");
                        }
                    } else if badge(ui, "READ-ONLY", PRIMARY()).on_hover_text("Click to allow edits on this connection for this session").clicked() {
                        self.modal = Some(Modal::Unlock(s.conn, None));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.undo.retain(|a| a.at.elapsed().as_secs() <= db::UNDO_MAX_AGE_S);
                    let n = self.undo.len();
                    let undo = if n > 0 { format!("Undo ({n})") } else { "Undo".into() };
                    if quiet_enabled(ui, n > 0, undo).on_hover_text("Recent writes you can take back").clicked() {
                        self.modal = Some(Modal::Undo);
                    }
                    if quiet(ui, "Settings").on_hover_text("Cmd+,").clicked() {
                        self.modal = Some(Modal::Settings);
                    }
                    ui.add_space(12.0);
                    if search_pill(ui).clicked() {
                        self.open_palette();
                    }
                    if let Some(v) = &self.updated {
                        let r = ui.add(primary_button(ui, format!("Restart for {v}"), SUCCESS())).on_hover_text("The update is installed. Quarry restarts by itself once you have no unsaved edits and have been idle for a minute.");
                        if r.clicked() {
                            if self.busy() {
                                self.warn("Save or discard your edits and wait for running scripts first.");
                            } else {
                                self.restart();
                            }
                        }
                    }
                });
            });
        });
    }

    /// Refreshes the memory and CPU readouts once a second, and asks for the repaint that shows them.
    fn sample_usage(&mut self, ctx: &egui::Context) {
        let (at, cpu_then, pct, _) = self.usage;
        let wall = at.elapsed();
        if wall >= Duration::from_secs(1) || cpu_then.is_zero() {
            let (mem, cpu_now) = process_usage();
            // A sample older than 2 s is from before the readouts were hidden, so it only starts a new second.
            let pct = if cpu_then.is_zero() || wall > Duration::from_secs(2) { pct } else { cpu_now.saturating_sub(cpu_then).as_secs_f32() / wall.as_secs_f32() * 100.0 };
            self.usage = (std::time::Instant::now(), cpu_now, pct, mem);
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
                if let Some(c) = self.active.and_then(|id| self.conn(id)).filter(|_| self.prefs.show_server) {
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
                if self.prefs.show_fps {
                    // Averaged over half a second of frames: one frame's dt swings above the refresh
                    // rate when an input event lands just after a vsync. Idle gaps are left out, but
                    // not the Shell's 100 ms repaints while a script runs.
                    let dt = ui.input(|i| i.unstable_dt);
                    let (sum, n, shown) = &mut self.fps;
                    if dt < 0.25 {
                        *sum += dt;
                        *n += 1;
                    }
                    if *sum >= 0.5 || *shown == 0.0 && *n > 0 {
                        *shown = *n as f32 / sum.max(0.000_1);
                        (*sum, *n) = (0.0, 0);
                    }
                    let rate = self.fps.2.max(1.0);
                    let color = if self.frame_ms > 1000.0 / rate * 0.8 { WARNING() } else { SUCCESS() };
                    fixed_readout(ui, format!("{:>3.0} fps · {:>4.1} ms", rate, self.frame_ms), "000 fps · 00.0 ms", color).on_hover_text("Frames per second while something moves, and the time Quarry needs to build one frame. At 120 Hz each frame has 8.3 ms.");
                }
                if self.prefs.show_mem || self.prefs.show_cpu {
                    self.sample_usage(ui.ctx());
                    let (_, _, cpu, mem) = self.usage;
                    if self.prefs.show_mem {
                        let text = mem.map_or("mem n/a".into(), |b| format!("mem {:>9}", human_bytes(b as usize)));
                        fixed_readout(ui, text, "mem 0000.0 MB", MUTED()).on_hover_text("Memory Quarry uses now, updated every second.");
                    }
                    if self.prefs.show_cpu {
                        let color = if cpu >= 80.0 { WARNING() } else { MUTED() };
                        fixed_readout(ui, format!("cpu {:>3.0}%", cpu), "cpu 000%", color).on_hover_text("Quarry's CPU use over the last second, in percent of one core.");
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.agents_pill(ui);
                    let log = RichText::new("Query log").size(TXT_CAPTION);
                    if quiet(ui, if self.log.open { log.color(BRIGHT()) } else { log }).on_hover_text("Every command sent to the servers (Cmd+L)").clicked() {
                        self.log.open = !self.log.open;
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
                        let first = self.status.lines().next().unwrap_or_default();
                        ui.add(egui::Label::new(RichText::new(first).size(TXT_CAPTION).color(FAINT())).truncate()).on_hover_text(self.status.as_str());
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
        let has_closed = !self.closed.is_empty();
        const TAB_MIN_W: f32 = 120.0;
        const PLUS_W: f32 = 32.0;
        const LIST_W: f32 = 84.0;
        let n = self.tabs.len();
        // The name is left as a placeholder so its color can follow hover, which is known only after layout.
        let titles: Vec<_> = self.tabs.iter().enumerate().map(|(i, t)| {
            let mut job = LayoutJob::default();
            job.append(&self.tab_name(i), 0.0, ui_fmt(TXT_BODY, Color32::PLACEHOLDER, if i == self.cur { W_MEDIUM } else { 400.0 }, 0.0));
            job.append(t.sel.scope_label(), 6.0, mono_fmt(10.0, FAINT(), 400.0));
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
                job.append(v, 6.0, mono_fmt(10.0, C_LINK(), 400.0));
            }
            ui.fonts_mut(|f| f.layout_job(job))
        }).collect();
        let frame = egui::Frame::new().fill(PANEL()).inner_margin(egui::Margin { left: 0, right: 8, top: 0, bottom: 0 });
        egui::Panel::top("tabs").exact_size(TAB_STRIP_H).show_separator_line(false).frame(frame).show(ui, |ui| {
        let panel = ui.max_rect();
        // Under the tabs, so the active one covers it and joins the page below.
        ui.painter().hline(panel.left()..=panel.right() + 8.0, panel.bottom() - 0.5, Stroke::new(1.0, LINE()));
        // Like a browser, tabs narrow down to TAB_MIN_W before the strip has to scroll.
        let full = ui.available_width();
        let cap = ((full - PLUS_W) / n.max(1) as f32).max(TAB_MIN_W);
        let widths: Vec<f32> = titles.iter().map(|g| (g.size().x + 48.0).clamp(TAB_MIN_W, 260.0).min(cap)).collect();
        let overflow = widths.iter().sum::<f32>() + PLUS_W > full;
        let view_w = full - PLUS_W - if overflow { LIST_W } else { 0.0 };
        // Scroll to the active tab whenever it changes, however it was picked, shortcuts included.
        let moved = self.tab_seen != Some(self.cur);
        self.tab_seen = Some(self.cur);
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
                    ui.horizontal(|ui| {
                        for (i, (t, title)) in self.tabs.iter().zip(titles).enumerate() {
                            let active = i == self.cur;
                            let (rect, resp) = ui.allocate_exact_size(egui::vec2(widths[i], TAB_STRIP_H), Sense::click());
                            let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                            if active && moved {
                                resp.scroll_to_me(None);
                            }
                            let p = ui.painter();
                            let hovered = resp.hovered();
                            if active {
                                // The page's fill and an accent underline, covering the strip's bottom line
                                // so the tab joins what it shows.
                                p.rect_filled(rect, 0, BG());
                                p.rect_filled(egui::Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - 2.0), rect.max), 0, PRIMARY());
                            } else {
                                p.rect_filled(rect.with_max_y(rect.bottom() - 1.0), 0, hover_fill(state_of(&resp)));
                            }
                            p.vline(rect.right() - 0.5, rect.shrink2(egui::vec2(0.0, 7.0)).y_range(), Stroke::new(1.0, LINE()));
                            let clip = egui::Rect::from_min_max(rect.min, egui::pos2(rect.right() - 28.0, rect.bottom()));
                            let name = if active { BRIGHT() } else if hovered { TEXT() } else { MUTED() };
                            p.with_clip_rect(clip.intersect(p.clip_rect())).galley(egui::pos2(rect.left() + 14.0, rect.center().y - title.size().y / 2.0), title, name);
                            let xr = egui::Rect::from_center_size(egui::pos2(rect.right() - 14.0, rect.center().y), egui::vec2(16.0, 16.0));
                            let xresp = ui.interact(xr, ui.scope_id().with(("close", i)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
                            if active || hovered || xresp.hovered() {
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
                            let conn_name = self.conn(t.sel.conn).map_or("", |c| c.meta.name.as_str());
                            let place = if t.sel.schema.is_empty() || t.sel.coll.is_empty() { t.sel.db.clone() } else { format!("{} / {}", t.sel.db, t.sel.schema) };
                            let resp = resp.on_hover_text(format!("{conn_name} / {place} / {}\nMiddle-click closes, right-click for more", self.tab_name(i)));
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
                            });
                            if xresp.clicked() || resp.middle_clicked() {
                                close = Some(i);
                            } else if resp.clicked() {
                                pick = Some(i);
                            }
                        }
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
        if pick.is_some() {
            // Scroll even when the picked tab was already active but scrolled out of view.
            self.tab_seen = None;
        }
        if reopen {
            return self.reopen_tab();
        }
        if let Some(query) = search {
            return self.open_palette_with(query);
        }
        if let Some((i, what)) = many {
            let keep = |j: usize| match what {
                TabClose::This => j != i,
                TabClose::Others => j == i,
                TabClose::Left => j >= i,
                TabClose::Right => j <= i,
                TabClose::All => false,
            };
            let cur = self.cur;
            let all = std::mem::take(&mut self.tabs);
            for (j, t) in all.into_iter().enumerate() {
                if keep(j) {
                    self.tabs.push(t);
                } else {
                    self.remember_closed(t);
                }
            }
            // Stay on the same tab when it survives, else on the clicked one, else the nearest.
            self.cur = if keep(cur) { (0..cur).filter(|&k| keep(k)).count() } else if keep(i) { (0..i).filter(|&k| keep(k)).count() } else { i.saturating_sub(1) }.min(self.tabs.len().saturating_sub(1));
            self.set_cur(self.cur);
            self.reset_doc_state();
        } else if let Some(i) = close {
            self.close_tab(i);
        } else if let Some(i) = pick {
            self.go_tab(i);
        }
        if dup {
            let t = &self.tabs[self.cur];
            let mut n = Tab::new(t.sel.clone());
            (n.filter_txt, n.sort_txt, n.proj_txt, n.limit_txt, n.skip_txt, n.hint_txt, n.collation_txt, n.query, n.view, n.agg_txt) = (t.filter_txt.clone(), t.sort_txt.clone(), t.proj_txt.clone(), t.limit_txt.clone(), t.skip_txt.clone(), t.hint_txt.clone(), t.collation_txt.clone(), t.query.clone(), t.view, t.agg_txt.clone());
            self.tabs.insert(self.cur + 1, n);
            self.set_cur(self.cur + 1);
            self.reset_doc_state();
            self.reload();
        }
    }

    pub(super) fn welcome(&mut self, ui: &mut egui::Ui) {
        if let Some(id) = self.active.filter(|id| self.conn(*id).is_some_and(|c| matches!(c.status, Status::Failed(_)))) {
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
        self.conn_test = None;
        self.test_info = None;
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
    let k = p.layout_job(LayoutJob::single_section(keys(false, false, "K"), mono_fmt(10.0, MUTED(), 400.0)));
    let kr = egui::Rect::from_min_size(egui::pos2(rect.right() - 6.0 - (k.size().x + 10.0), rect.center().y - 8.0), egui::vec2(k.size().x + 10.0, 16.0));
    paint_kbd(p, kr, k);
    r.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Jump to a connection, database or collection")
}
