//! Drawing for SQL tabs: the Content view, and a note for the views that are not built yet.

use super::*;

impl App {
    /// Asks for the tab's current page. The answer is matched back to the tab by `req`.
    pub(super) fn reload_sql(&mut self, i: usize) {
        // A Query tab that belongs to no table has no page to load.
        if self.tabs.get(i).is_some_and(|t| t.sel.coll.is_empty()) {
            return;
        }
        let Some(db) = self.tabs.get(i).and_then(|t| self.conn(t.sel.conn)).and_then(|c| c.sql.clone()) else { return };
        self.req += 1;
        let (req, size) = (self.req, self.page_size());
        let t = &mut self.tabs[i];
        (t.req, t.loading, t.size) = (req, true, size);
        let q = super::sql_bar::page_query(t);
        self.worker.sql_page(req, db, t.sel.db.clone(), t.sel.coll.clone(), q, size, t.page * size);
    }

    /// The open tab's dialect, and whether its server reads backslash escapes in strings.
    fn sql_dialect(&self) -> (db::sql::Dialect, bool) {
        let d = self.conn(self.t().map(|t| t.sel.conn).unwrap_or(0)).and_then(|c| c.sql.as_ref());
        (d.map(|s| s.dialect).unwrap_or_default(), d.is_none_or(|s| s.backslash_escapes()))
    }

    /// Double click on a grouped row: its rows replace the grouped result.
    pub(super) fn sql_drill(&mut self, row: usize) {
        let (dialect, bs) = self.sql_dialect();
        super::sql_bar::drill(self.tm().unwrap(), row, dialect, bs);
        self.reload();
    }

    /// Content: the query line, the results line and the table.
    pub(super) fn sql_content(&mut self, ui: &mut egui::Ui) {
        // Loaded with another page size, set from another tab: its page numbers no longer line up.
        let size = self.page_size();
        if let Some(t) = self.tm().filter(|t| t.size != 0 && t.size != size && !t.loading) {
            t.page = 0;
            self.reload();
        }
        let (dialect, bs) = self.sql_dialect();
        let (conn, db) = (self.tabs[self.cur].sel.conn, self.tabs[self.cur].sel.db.clone());
        let scope = Self::sql_scope(&self.conns, &self.sql_schema, conn, &db, None);
        let none = Fields::new();
        let t = &mut self.tabs[self.cur];
        let run = super::sql_bar::bar(ui, t, dialect, bs, scope, self.sql_values.get(&(conn, db.clone())).unwrap_or(&none));
        let wants = t.sql.as_mut().map(|s| std::mem::take(&mut s.bar.wants)).unwrap_or_default();
        self.sql_serve(conn, &db, wants);
        if run {
            self.tm().unwrap().page = 0;
            self.reload();
        }
        ui.add_space(10.0);
        let t = self.t().unwrap();
        let grouped = t.sql.as_ref().and_then(|s| s.matched);
        let (count, ms, loading, page) = (t.count, t.ms, t.loading, t.page);
        let approx = if t.sql.as_ref().is_some_and(|s| s.estimated) { "~" } else { "" };
        let pages = count.div_ceil(self.page_size()).max(1);
        let mut go: Option<u64> = None;
        ui.horizontal(|ui| {
            let unit = if grouped.is_some() { if count == 1 { "group" } else { "groups" } } else { "rows" };
            ui.label(semibold(RichText::new(format!("{approx}{} {unit}", group(&count.to_string()))).font(FontId::monospace(TXT_SMALL)).color(BRIGHT())));
            ui.label(RichText::new(format!("{ms} ms")).font(FontId::monospace(TXT_SMALL)).color(SUCCESS()));
            if loading {
                spin(ui);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                self.sql_row_tools(ui);
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
        if let Some(rows) = grouped {
            if super::sql_bar::grouped_note(ui, self.t().map_or(0, |t| t.count), rows) {
                super::sql_bar::ungroup(self.tm().unwrap());
                self.reload();
            }
            ui.add_space(6.0);
        }
        self.sql_results(ui);
    }
}
