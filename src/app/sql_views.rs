//! Drawing for SQL tabs: the Content view, and a note for the views that are not built yet.

use super::*;

impl App {
    /// Asks for the tab's current page. The answer is matched back to the tab by `req`.
    pub(super) fn reload_sql(&mut self, i: usize) {
        let Some(db) = self.tabs.get(i).and_then(|t| self.conn(t.sel.conn)).and_then(|c| c.sql.clone()) else { return };
        self.req += 1;
        let (req, size) = (self.req, self.page_size());
        let t = &mut self.tabs[i];
        (t.req, t.loading, t.size) = (req, true, size);
        self.worker.sql_page(req, db, t.sel.db.clone(), t.sel.coll.clone(), t.filter_txt.clone(), t.sort_txt.clone(), size, t.page * size);
    }

    /// The centered note for SQL views that a later step builds.
    pub(super) fn sql_soon(&self, ui: &mut egui::Ui) {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| ui.label(RichText::new("Coming in a later step").size(TXT_SMALL).color(FAINT())));
    }

    /// Content: the query line, the results line and the table.
    pub(super) fn sql_content(&mut self, ui: &mut egui::Ui) {
        // Loaded with another page size, set from another tab: its page numbers no longer line up.
        let size = self.page_size();
        if let Some(t) = self.tm().filter(|t| t.size != 0 && t.size != size && !t.loading) {
            t.page = 0;
            self.reload();
        }
        let mut run = false;
        let mono = egui::TextStyle::Monospace.resolve(ui.style());
        let row_h = ui.fonts_mut(|f| f.row_height(&mono)) + 4.0;
        let t = &mut self.tabs[self.cur];
        // ponytail: step 6 replaces this WHERE and ORDER BY line with the F query builder.
        let fid = query_id("SQL-WHERE");
        let focused = ui.memory(|m| m.has_focus(fid));
        let hovered = ui.ctx().read_response(fid).is_some_and(|r| r.hovered());
        let edge = if focused { PRIMARY() } else if hovered { MUTED() } else { BORDER() };
        let bar = egui::Frame::new().fill(PANEL()).stroke(Stroke::new(1.0, edge)).corner_radius(R_BAR).inner_margin(egui::Margin { left: 12, right: 6, top: 7, bottom: 7 }).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.add_sized([0.0, row_h], egui::Label::new(RichText::new("WHERE").font(mono.clone()).color(C_KEYWORD())));
                let w = (ui.available_width() - 120.0).max(120.0);
                let r = ui.add(egui::TextEdit::singleline(&mut t.filter_txt).id(fid).frame(editor_margin()).font(mono.clone()).hint_text(RichText::new("status = 'paid' AND total > 50").color(FAINT())).desired_width(w));
                run |= r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), row_h), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if run_button(ui, "Run", "↵").on_hover_text("Enter").clicked() {
                        run = true;
                    }
                });
            });
        });
        if focused {
            ui.painter().rect_stroke(bar.response.rect.expand(1.0), R_BAR + 1, Stroke::new(2.0, tint(PRIMARY(), 60)), egui::StrokeKind::Outside);
        }
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            let id = query_id("SQL-ORDER");
            let w = (text_w(ui, if t.sort_txt.is_empty() { "created_at DESC" } else { &t.sort_txt }, mono.clone()) + 8.0).clamp(24.0, 420.0);
            run |= opt_chip(ui, "ORDER BY", id, &mut t.sort_txt, |ui, text| {
                let r = ui.add(egui::TextEdit::singleline(text).id(id).frame(editor_margin()).font(mono.clone()).hint_text("created_at DESC").desired_width(w).layouter(&mut code_layout));
                r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
            });
        });
        if run {
            self.tm().unwrap().page = 0;
            self.reload();
        }
        ui.add_space(10.0);
        let t = self.t().unwrap();
        let (count, ms, loading, page) = (t.count, t.ms, t.loading, t.page);
        let pages = count.div_ceil(self.page_size()).max(1);
        let mut go: Option<u64> = None;
        ui.horizontal(|ui| {
            ui.label(semibold(RichText::new(format!("{} rows", group(&count.to_string()))).font(FontId::monospace(TXT_SMALL)).color(BRIGHT())));
            ui.label(RichText::new(format!("{ms} ms")).font(FontId::monospace(TXT_SMALL)).color(SUCCESS()));
            if loading {
                spin(ui);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if icon_button(ui, "⟳", true).on_hover_text("Reload (Cmd+R)").clicked() {
                    go = Some(page);
                }
                // The pager reads left to right, so it gets its own layout at last frame's width.
                let id = ui.scope_id().with("pager-w");
                let w = ui.data(|d| d.get_temp::<f32>(id)).unwrap_or(150.0);
                let r = ui.allocate_ui_with_layout(egui::vec2(w, BUTTON_H), egui::Layout::left_to_right(egui::Align::Center), |ui| pager(ui, page as usize, pages as usize));
                if r.response.rect.width() != w {
                    ui.data_mut(|d| d.insert_temp(id, r.response.rect.width()));
                    ui.ctx().request_repaint();
                }
                if let Some(p) = r.inner {
                    go = Some(p as u64);
                }
                let mut size = self.page_size();
                ui.scope(|ui| {
                    quiet_look(ui);
                    egui::ComboBox::from_id_salt("page-size").selected_text(format!("{size} / page")).show_ui(ui, |ui| {
                        menu_look(ui);
                        for n in [db::PAGE_SIZE, 100, 200, db::PAGE_SIZE_MAX] {
                            ui.selectable_value(&mut size, n, format!("{n} per page"));
                        }
                    });
                });
                if size != self.page_size() {
                    self.prefs.page_size = size;
                    self.save_prefs();
                    go = Some(0);
                }
            });
        });
        if let Some(p) = go {
            self.tm().unwrap().page = p;
            self.reload();
        }
        ui.add_space(4.0);
        self.results(ui, false);
    }
}
