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
            const VIEWS: [View; 7] = [View::Documents, View::Aggregate, View::Indexes, View::Schema, View::Import, View::Compare, View::Shell];
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

    pub(super) fn status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("status").frame(egui::Frame::new().fill(PANEL()).inner_margin(egui::Margin::symmetric(12, 5))).show(ui, |ui| {
            ui.horizontal(|ui| {
                if self.prefs.show_fps {
                    let dt = ui.input(|i| i.stable_dt).max(0.000_1);
                    let budget = 1000.0 * dt;
                    let color = if self.frame_ms > budget * 0.8 { WARNING() } else { SUCCESS() };
                    ui.label(RichText::new(format!("{:.0} fps · {:.1} ms", 1.0 / dt, self.frame_ms)).monospace().size(TXT_CAPTION).color(color)).on_hover_text("Frames per second while something moves, and the time Quarry needs to build one frame. At 120 Hz each frame has 8.3 ms.");
                    ui.add_space(8.0);
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
        egui::ScrollArea::horizontal().id_salt("tabs").max_height(38.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let n = self.tabs.len();
                for (i, t) in self.tabs.iter().enumerate() {
                    let active = i == self.cur;
                    let mut job = LayoutJob::default();
                    job.append(&t.sel.coll, 0.0, egui::TextFormat::simple(FontId::proportional(TXT_TITLE), if active { BRIGHT() } else { TEXT() }));
                    job.append(&t.sel.db, 6.0, egui::TextFormat::simple(FontId::proportional(TXT_CAPTION), FAINT()));
                    if t.view != View::Documents {
                        let v = match t.view {
                            View::Aggregate => "aggregate",
                            View::Indexes => "indexes",
                            View::Shell => "shell",
                            View::Schema => "schema",
                            View::Import => "import",
                            View::Compare => "compare",
                            View::Documents => "",
                        };
                        job.append(v, 6.0, egui::TextFormat::simple(FontId::proportional(TXT_CAPTION), C_LINK().gamma_multiply(0.8)));
                    }
                    let title = ui.fonts_mut(|f| f.layout_job(job));
                    let w = (title.size().x + 50.0).clamp(96.0, 300.0);
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 30.0), Sense::click());
                    let rect = rect.shrink2(egui::vec2(0.0, 1.0));
                    let p = ui.painter();
                    let hovered = resp.hovered();
                    if active {
                        p.rect(rect, R_MD, RAISED(), Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
                        // The original marks the active tab with a yellow underline.
                        p.line_segment([rect.left_bottom() + egui::vec2(8.0, -1.0), rect.right_bottom() + egui::vec2(-8.0, -1.0)], Stroke::new(2.0, WARNING()));
                    } else if hovered {
                        p.rect_filled(rect, R_MD, PANEL());
                    }
                    let is_view = self.views.contains(&(t.sel.conn, t.sel.db.clone(), t.sel.coll.clone()));
                    let dot = egui::pos2(rect.left() + 13.0, rect.center().y);
                    let dc = if active { WARNING() } else { FAINT() };
                    if is_view {
                        p.circle_stroke(dot, 3.0, Stroke::new(1.3, dc));
                    } else {
                        p.circle_filled(dot, 3.0, dc);
                    }
                    let clip = egui::Rect::from_min_max(rect.min, egui::pos2(rect.right() - 24.0, rect.bottom()));
                    p.with_clip_rect(clip).galley(egui::pos2(rect.left() + 24.0, rect.center().y - title.size().y / 2.0), title, TEXT());
                    let xr = egui::Rect::from_center_size(egui::pos2(rect.right() - 13.0, rect.center().y), egui::vec2(16.0, 16.0));
                    let xresp = ui.interact(xr, ui.scope_id().with(("close", i)), Sense::click());
                    if active || hovered || xresp.hovered() {
                        if xresp.hovered() {
                            ui.painter().rect_filled(xr, R_SM, HOVER());
                        }
                        let c = xr.center();
                        let s = Stroke::new(1.4, if xresp.hovered() { BRIGHT() } else { MUTED() });
                        ui.painter().line_segment([c + egui::vec2(-3.5, -3.5), c + egui::vec2(3.5, 3.5)], s);
                        ui.painter().line_segment([c + egui::vec2(3.5, -3.5), c + egui::vec2(-3.5, 3.5)], s);
                    }
                    let conn_name = self.conns.iter().find(|c| c.meta.id == t.sel.conn).map(|c| c.meta.name.clone()).unwrap_or_default();
                    let resp = resp.on_hover_text(format!("{conn_name} / {} / {}\nMiddle-click closes, right-click for more", t.sel.db, t.sel.coll));
                    resp.context_menu(|ui| {
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
                if !self.tabs.is_empty() && ui.add(quiet_button(RichText::new("+").size(TXT_HEADING).color(MUTED()))).on_hover_text("Duplicate this tab").clicked() {
                    dup = true;
                }
            });
        });
        ui.painter().hline(ui.max_rect().x_range(), ui.min_rect().bottom(), Stroke::new(1.0, BORDER()));
        ui.add_space(8.0);
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
