//! The window frame around the views: top bar, tab strip, status bar, toasts and keyboard shortcuts.

use super::*;


impl App {
    /// Keeps the open tabs and their inputs in the settings file, so the next launch reopens them.
    pub(super) fn remember_tabs(&mut self) {
        if self.demo || self.shot.is_some() {
            return;
        }
        let tabs: Vec<SavedTab> = self
            .tabs
            .iter()
            .map(|t| SavedTab {
                conn: t.sel.conn,
                db: t.sel.db.clone(),
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
                }
                .into(),
                filter: t.filter_txt.clone(),
                sort: t.sort_txt.clone(),
                project: t.proj_txt.clone(),
                agg: t.agg_txt.clone(),
                shell: t.shell.code.clone(),
                stages: if t.stages_src == t.agg_txt { t.stages.iter().map(|s| SavedStage { op: s.op.clone(), text: s.text.clone(), on: s.on }).collect() } else { Vec::new() },
                agg_text_mode: t.agg_text_mode,
            })
            .collect();
        if tabs != self.prefs.open_tabs || self.cur != self.prefs.cur_tab || self.active != self.prefs.last_conn {
            self.prefs.open_tabs = tabs;
            self.prefs.cur_tab = self.cur;
            self.prefs.last_conn = self.active;
            self.save_prefs();
        }
    }

    pub(super) fn keys(&mut self, ctx: &egui::Context) {
        if self.modal.is_some() || self.insert.is_some() || self.json_edit.is_some() || self.users.is_some() {
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
        if k(cmd, Key::K) || k(cmd, Key::T) {
            self.modal = Some(Modal::Palette { query: String::new(), sel: 0 });
        }
        if k(cmd, Key::E) {
            self.explain();
        }
        if k(cmd | shift, Key::R) {
            if let Some(id) = self.active.filter(|_| !self.demo) {
                self.connect(id);
            }
        }
        if k(cmd, Key::R) && !self.tabs.is_empty() {
            self.reload();
        }
        if k(cmd | shift, Key::F) {
            ctx.memory_mut(|m| m.request_focus(egui::Id::unique("tree-filter")));
        }
        if k(cmd, Key::F) && !self.tabs.is_empty() {
            self.set_view(View::Documents);
            ctx.memory_mut(|m| m.request_focus(egui::Id::unique(("query", "QUERY"))));
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
            let body = SHORTCUTS.iter().map(|(keys, what)| format!("{keys:<30}{what}")).collect::<Vec<_>>().join("\n");
            self.modal = Some(Modal::Text { title: "Keyboard shortcuts".into(), body, code: false });
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
            const VIEWS: [View; 8] = [View::Documents, View::Aggregate, View::Indexes, View::Schema, View::Import, View::Compare, View::Shell, View::Watch];
            for (d, v) in DIGITS.iter().zip(VIEWS) {
                if k(cmd | alt, *d) {
                    self.set_view(v);
                }
            }
            for (i, d) in DIGITS.iter().enumerate() {
                if k(cmd, *d) {
                    self.go_tab(if i == 8 { n - 1 } else { i.min(n - 1) });
                }
            }
        }
        // Plain keys only act when no text field or menu wants them.
        if ctx.memory(|m| m.focused().is_none()) && !ctx.any_popup_open() {
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
                let (page, pages) = (t.page, t.count.div_ceil(PAGE_SIZE).max(1));
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

    pub(super) fn go_tab(&mut self, i: usize) {
        if i != self.cur && i < self.tabs.len() {
            self.cur = i;
            self.reset_doc_state();
        }
    }

    pub(super) fn set_view(&mut self, v: View) {
        let Some(t) = self.tm().filter(|t| t.view != v) else { return };
        t.view = v;
        t.selected = None;
        self.reset_doc_state();
        if v == View::Indexes {
            self.load_indexes();
        }
    }

    /// "Saved … Undo" notice after a write, for 8 seconds.
    pub(super) fn toast(&mut self, ctx: &egui::Context) {
        const TOAST_S: u64 = 8;
        let Some((msg, at, undo_at)) = self.toast.clone() else { return };
        // Later writes push newer entries in front, so the toast's own entry is found by its time.
        let entry = undo_at.and_then(|k| self.undo.iter().position(|a| a.at == k));
        if at.elapsed().as_secs() >= TOAST_S {
            self.toast = None;
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(250));
        let mut undo = false;
        let mut close = false;
        egui::Area::new(egui::Id::unique(("quarry-layer", "toast"))).anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -40.0)).show(ctx, |ui| {
            egui::Frame::new().fill(RAISED()).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("✓").color(SUCCESS()));
                    ui.label(RichText::new(&msg).color(TEXT()));
                    if entry.is_some() && ui.button(RichText::new("Undo").color(PRIMARY())).clicked() {
                        undo = true;
                    }
                    close = ui.button("🗙").clicked();
                });
            });
        });
        if let (true, Some(i)) = (undo, entry) {
            self.apply_undo(i);
            self.toast = None;
        } else if close {
            self.toast = None;
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
        let frame = egui::Frame::new().fill(PANEL()).inner_margin(egui::Margin { left, right: 12, top: 7, bottom: 7 });
        egui::Panel::top("top").exact_size(HEADER_H).frame(frame).show(ui, |ui| {
            // Registered before the widgets so they stay clickable; the empty header moves the window.
            let bar = ui.interact(ui.max_rect().expand2(egui::vec2(left as f32, 7.0)), egui::Id::unique("titlebar"), Sense::click_and_drag());
            if bar.drag_started_by(egui::PointerButton::Primary) {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            if bar.double_clicked() {
                let max = ui.input(|i| i.viewport().maximized.unwrap_or(false));
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
            }
            ui.horizontal_centered(|ui| {
                ui.label(RichText::new("Quarry").strong().size(TXT_HEADING).color(BRIGHT()));
                ui.label(RichText::new("MongoDB workbench").color(MUTED()));
                ui.separator();
                if let Some(s) = self.t().map(|t| t.sel.clone()) {
                    let (name, writable) = self.conn(s.conn).map(|c| (c.meta.name.clone(), c.writable)).unwrap_or_default();
                    ui.label(RichText::new(format!("{name}  /  {}  /  {}", s.db, s.coll)).monospace().color(TEXT()));
                    if self.prefs.always_writable {
                        // No lock to toggle: writes are always on.
                    } else if writable {
                        if chip(ui, "WRITES ON", ALERT(), true).on_hover_text("Click to lock this connection again").clicked() {
                            if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == s.conn) {
                                c.writable = false;
                            }
                            self.ok("Writes locked");
                        }
                    } else if chip(ui, "READ-ONLY", SUCCESS(), true).on_hover_text("Click to allow edits on this connection for this session").clicked() {
                        self.modal = Some(Modal::Unlock(s.conn, None));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.undo.retain(|a| a.at.elapsed().as_secs() <= db::UNDO_MAX_AGE_S);
                    let n = self.undo.len();
                    if ui.add_enabled(n > 0, egui::Button::new(format!("⟲ Undo ({n})"))).clicked() {
                        self.modal = Some(Modal::Undo);
                    }
                    if ui.button("⚙ Settings").clicked() {
                        self.modal = Some(Modal::Settings);
                    }
                    if let Some(v) = &self.updated {
                        let r = ui.add(primary_button(ui, format!("Restart for {v}"), SUCCESS())).on_hover_text("The update is installed. Quarry restarts by itself once you have no unsaved edits and have been idle for a minute.");
                        if r.clicked() {
                            if self.busy() {
                                self.err("Save or discard your edits and wait for running scripts first.");
                            } else {
                                crate::update::restart();
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
        egui::Panel::bottom("status").frame(egui::Frame::new().fill(PANEL()).inner_margin(egui::Margin::symmetric(12, 5))).show(ui, |ui| {
            ui.horizontal(|ui| {
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
                // Errors fade after 15 s so an old problem does not look current.
                const ERROR_S: u64 = 15;
                if self.status.1 {
                    if self.status_at.elapsed().as_secs() >= ERROR_S {
                        self.status = ("Ready".into(), false);
                    } else {
                        ui.ctx().request_repaint_after(Duration::from_secs(1));
                        if ui.button("🗙").on_hover_text("Dismiss").clicked() {
                            self.status = ("Ready".into(), false);
                        }
                    }
                }
                let (msg, is_err) = &self.status;
                let first = msg.lines().next().unwrap_or_default();
                let r = ui.add(egui::Label::new(RichText::new(first).size(TXT_SMALL).color(if *is_err { ALERT() } else { MUTED() })).truncate().sense(egui::Sense::click()));
                // Errors often carry advice on a second line or a long server message.
                if !*is_err {
                    r.on_hover_text(msg.as_str());
                } else if r.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(format!("{msg}\n\nClick to open the whole message.")).clicked() {
                    self.modal = Some(Modal::Text { title: "Error".into(), body: msg.clone(), code: false });
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.selectable_label(self.log.open, RichText::new("Query log").size(TXT_SMALL)).on_hover_text("Every command sent to the servers (Cmd+L)").clicked() {
                        self.log.open = !self.log.open;
                    }
                    ui.label(RichText::new("Cmd+K jumps  ·  Enter runs  ·  double-click a value to edit  ·  Cmd+/ shortcuts").size(TXT_SMALL).color(FAINT()));
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
        let has_closed = !self.closed.is_empty();
        const TAB_H: f32 = 34.0;
        const TAB_MIN_W: f32 = 120.0;
        const PLUS_W: f32 = 32.0;
        const LIST_W: f32 = 84.0;
        let n = self.tabs.len();
        // The name is left as a placeholder so its color can follow hover, which is known only after layout.
        let titles: Vec<_> = self.tabs.iter().enumerate().map(|(i, t)| {
            let mut job = LayoutJob::default();
            job.append(&t.sel.coll, 0.0, egui::TextFormat::simple(FontId::proportional(TXT_TITLE), Color32::PLACEHOLDER));
            job.append(&t.sel.db, 6.0, egui::TextFormat::simple(FontId::proportional(TXT_CAPTION), if i == self.cur { MUTED() } else { FAINT() }));
            let v = match t.view {
                View::Aggregate => Some("aggregate"),
                View::Indexes => Some("indexes"),
                View::Shell => Some("shell"),
                View::Schema => Some("schema"),
                View::Import => Some("import"),
                View::Compare => Some("compare"),
                View::Watch => Some("watch"),
                View::Documents => None,
            };
            if let Some(v) = v {
                job.append(v, 6.0, egui::TextFormat::simple(FontId::proportional(TXT_CAPTION), C_LINK().gamma_multiply(0.8)));
            }
            ui.fonts_mut(|f| f.layout_job(job))
        }).collect();
        // Like a browser, tabs narrow down to TAB_MIN_W before the strip has to scroll.
        let full = ui.available_width();
        let cap = ((full - PLUS_W) / n.max(1) as f32).max(TAB_MIN_W);
        let widths: Vec<f32> = titles.iter().map(|g| (g.size().x + 54.0).clamp(TAB_MIN_W, 240.0).min(cap)).collect();
        let overflow = widths.iter().sum::<f32>() + PLUS_W > full;
        let view_w = full - PLUS_W - if overflow { LIST_W } else { 0.0 };
        // Scroll to the active tab whenever it changes, however it was picked, shortcuts included.
        let moved = self.tab_seen != Some(self.cur);
        self.tab_seen = Some(self.cur);
        let top = egui::CornerRadius { nw: R_MD, ne: R_MD, sw: 0, se: 0 };
        // Painted first so the active tab's bottom cover hides it, joining the tab to the page.
        let baseline = ui.painter().add(egui::Shape::Noop);
        let strip = ui.horizontal(|ui| {
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
                        let mut prev_plain = false;
                        for (i, (t, title)) in self.tabs.iter().zip(titles).enumerate() {
                            let active = i == self.cur;
                            let (rect, resp) = ui.allocate_exact_size(egui::vec2(widths[i], TAB_H), Sense::click());
                            let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                            if active && moved {
                                resp.scroll_to_me(None);
                            }
                            let p = ui.painter();
                            let hovered = resp.hovered();
                            if active {
                                // Same fill as the page below and no bottom edge, so the tab joins what it shows.
                                p.rect(rect, top, BG(), Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
                                p.rect_filled(egui::Rect::from_min_max(egui::pos2(rect.left() + 1.0, rect.bottom() - 1.5), egui::pos2(rect.right() - 1.0, rect.bottom() + 1.0)), 0, BG());
                                p.line_segment([rect.left_top() + egui::vec2(10.0, 1.0), rect.right_top() + egui::vec2(-10.0, 1.0)], Stroke::new(2.0, WARNING()));
                            } else if hovered {
                                p.rect_filled(rect, top, PANEL());
                            } else if prev_plain {
                                p.vline(rect.left(), rect.shrink2(egui::vec2(0.0, 10.0)).y_range(), Stroke::new(1.0, HOVER()));
                            }
                            prev_plain = !active && !hovered;
                            let is_view = self.views.contains(&(t.sel.conn, t.sel.db.clone(), t.sel.coll.clone()));
                            let dot = egui::pos2(rect.left() + 14.0, rect.center().y);
                            let dc = if active { WARNING() } else { FAINT() };
                            if is_view {
                                p.circle_stroke(dot, 3.0, Stroke::new(1.3, dc));
                            } else {
                                p.circle_filled(dot, 3.0, dc);
                            }
                            let clip = egui::Rect::from_min_max(rect.min, egui::pos2(rect.right() - 26.0, rect.bottom()));
                            let name = if active { BRIGHT() } else if hovered { TEXT() } else { MUTED() };
                            p.with_clip_rect(clip.intersect(p.clip_rect())).galley(egui::pos2(rect.left() + 26.0, rect.center().y - title.size().y / 2.0), title, name);
                            let xr = egui::Rect::from_center_size(egui::pos2(rect.right() - 14.0, rect.center().y), egui::vec2(16.0, 16.0));
                            let xresp = ui.interact(xr, ui.scope_id().with(("close", i)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
                            if active || hovered || xresp.hovered() {
                                if xresp.hovered() {
                                    ui.painter().rect_filled(xr, R_SM, HOVER());
                                }
                                let c = xr.center();
                                let s = Stroke::new(1.4, if xresp.hovered() { BRIGHT() } else { MUTED() });
                                ui.painter().line_segment([c + egui::vec2(-3.5, -3.5), c + egui::vec2(3.5, 3.5)], s);
                                ui.painter().line_segment([c + egui::vec2(3.5, -3.5), c + egui::vec2(-3.5, 3.5)], s);
                            }
                            let conn_name = self.conn(t.sel.conn).map_or("", |c| c.meta.name.as_str());
                            let resp = resp.on_hover_text(format!("{conn_name} / {} / {}\nMiddle-click closes, right-click for more", t.sel.db, t.sel.coll));
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
                                if ui.button("🗋 Duplicate").clicked() {
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
                let (r, resp) = ui.allocate_exact_size(egui::vec2(BUTTON_H, TAB_H), Sense::click());
                let b = egui::Rect::from_center_size(r.center(), egui::vec2(BUTTON_H, BUTTON_H));
                if resp.hovered() {
                    ui.painter().rect_filled(b, R_MD, HOVER());
                }
                let (c, s) = (b.center(), Stroke::new(1.5, if resp.hovered() { BRIGHT() } else { MUTED() }));
                ui.painter().line_segment([c - egui::vec2(5.5, 0.0), c + egui::vec2(5.5, 0.0)], s);
                ui.painter().line_segment([c - egui::vec2(0.0, 5.5), c + egui::vec2(0.0, 5.5)], s);
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Duplicate this tab").clicked() {
                    dup = true;
                }
            }
            if overflow {
                ui.add_space(6.0);
                self.tab_list(ui, &mut pick);
            }
        });
        let y = strip.response.rect.bottom() - 0.5;
        ui.painter().set(baseline, egui::Shape::hline(ui.max_rect().x_range(), y, Stroke::new(1.0, BORDER())));
        ui.add_space(8.0);
        if pick.is_some() {
            // Scroll even when the picked tab was already active but scrolled out of view.
            self.tab_seen = None;
        }
        if reopen {
            return self.reopen_tab();
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
            self.reset_doc_state();
        } else if let Some(i) = close {
            self.close_tab(i);
        } else if let Some(i) = pick {
            self.go_tab(i);
        }
        if dup {
            let t = &self.tabs[self.cur];
            let mut n = Tab::new(t.sel.clone());
            (n.filter_txt, n.sort_txt, n.proj_txt, n.query, n.view, n.agg_txt) = (t.filter_txt.clone(), t.sort_txt.clone(), t.proj_txt.clone(), t.query.clone(), t.view, t.agg_txt.clone());
            self.tabs.insert(self.cur + 1, n);
            self.cur += 1;
            self.reset_doc_state();
            self.reload();
        }
    }

    /// Every open tab grouped by connection and database, with a filter, for when the strip overflows.
    fn tab_list(&self, ui: &mut egui::Ui, pick: &mut Option<usize>) {
        let id = egui::Id::unique("tab-filter");
        let n = self.tabs.len();
        sticky_menu(ui, format!("{n} tabs"), |ui| {
            ui.set_min_width(260.0);
            // The filter lives only while the menu stays open: a gap of a frame means it was closed.
            let frame = ui.ctx().cumulative_frame_nr();
            let mut q = ui.data(|d| d.get_temp::<(String, u64)>(id)).filter(|(_, f)| *f + 1 >= frame).map(|(q, _)| q).unwrap_or_default();
            let r = ui.add(egui::TextEdit::singleline(&mut q).hint_text("Filter tabs").desired_width(f32::INFINITY));
            if ui.memory(|m| m.focused().is_none()) {
                r.request_focus();
            }
            ui.data_mut(|d| d.insert_temp(id, (q.clone(), frame)));
            let q = q.to_lowercase();
            // Grouped by connection too, so same-named databases on two servers stay apart.
            let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
            for (i, t) in self.tabs.iter().enumerate() {
                let conn = self.conn(t.sel.conn).map_or("", |c| c.meta.name.as_str());
                if !format!("{} {} {conn}", t.sel.coll, t.sel.db).to_lowercase().contains(&q) {
                    continue;
                }
                let key = format!("{conn} / {}", t.sel.db);
                match groups.iter_mut().find(|g| g.0 == key) {
                    Some(g) => g.1.push(i),
                    None => groups.push((key, vec![i])),
                }
            }
            if groups.is_empty() {
                ui.label(RichText::new("No tab matches").color(MUTED()));
            }
            menu_scroll(ui, |ui| {
                for (db, idx) in groups {
                    ui.add_space(4.0);
                    caption(ui, db);
                    for i in idx {
                        let mut b = egui::Button::selectable(i == self.cur, &self.tabs[i].sel.coll);
                        // Cmd+1 to 8 jump to those tabs and Cmd+9 to the last one, as in browsers.
                        let key = if i == n - 1 { Some(9) } else if i < 8 { Some(i + 1) } else { None };
                        if let Some(k) = key {
                            b = b.shortcut_text(format!("Cmd+{k}"));
                        }
                        if ui.add(b).clicked() {
                            *pick = Some(i);
                            ui.close();
                        }
                    }
                }
            });
        });
    }

    pub(super) fn welcome(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(120.0);
            ui.label(RichText::new("Pick a collection on the left").size(TXT_HEADING).color(BRIGHT()));
            ui.add_space(6.0);
            ui.label(RichText::new("Each collection opens in its own tab. Every connection starts read-only; edits need an explicit unlock, show a change preview, and can be undone.").color(MUTED()));
            ui.add_space(16.0);
            if ui.button("+ Add a connection").clicked() {
                self.new_conn_modal();
            }
        });
    }

    pub(super) fn new_conn_modal(&mut self) {
        self.conn_test = None;
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
