//! Drawing for SQL tabs: the Content view, and a note for the views that are not built yet.

use crate::app::*;

impl App {
    /// Asks for the tab's current page. The answer is matched back to the tab by `req`.
    pub(in crate::app) fn reload_sql(&mut self, i: usize) {
        // A Query tab that belongs to no table has no page to load.
        if self.tabs.get(i).is_some_and(|t| t.sel.coll.is_empty()) {
            return;
        }
        let Some((db, space)) = self.tabs.get(i).and_then(|t| self.sql_target(&t.sel)) else { return };
        self.req += 1;
        let (req, size) = (self.req, self.page_size());
        let t = &mut self.tabs[i];
        (t.req, t.loading, t.size) = (req, true, size);
        let q = crate::app::sql::bar::page_query(t);
        self.worker.sql_page(req, db, space, t.sel.coll.clone(), q, size, t.page * size);
    }

    /// How the connection's last reported sql_mode reads quotes and backslashes. A connection without a server, as in
    /// the offline demo, reads like a stock one.
    pub(in crate::app) fn sql_mode(&self, conn: u64) -> db::sql::ddl::Mode {
        self.conn(conn).and_then(|c| c.sql.as_ref()).map_or(db::sql::ddl::Mode::DEFAULT, |d| db::sql::ddl::Mode::of(&d.sql_mode))
    }

    /// The server refused or reported a newer sql_mode than the one the previews follow; they follow the new one now.
    pub(in crate::app) fn adopt_sql_mode(&mut self, conn: u64, mode: Option<String>) {
        if let Some(m) = mode && let Some(d) = self.conns.iter_mut().find(|c| c.meta.id == conn).and_then(|c| c.sql.as_mut()) {
            d.sql_mode = m;
        }
    }

    /// The SQL session of a connection for a statement about to run. The offline demo has no server, so it says so
    /// and gives none. `missing` is what to say when the connection has no session, or nothing when None.
    pub(in crate::app) fn sql_db_for(&mut self, conn: u64, missing: Option<&str>) -> Option<db::sql::Db> {
        if self.demo {
            self.ok("Offline demo: no database is connected.");
            return None;
        }
        let db = self.conn(conn).and_then(|c| c.sql.clone());
        if let (None, Some(m)) = (&db, missing) {
            self.warn(m);
        }
        db
    }

    /// The open tab's dialect.
    pub(in crate::app) fn sql_dialect(&self) -> db::sql::Dialect {
        self.dialect_of(self.t().map_or(0, |t| t.sel.conn))
    }

    /// Double click on a grouped row: its rows replace the grouped result.
    pub(in crate::app) fn sql_drill(&mut self, row: usize) {
        let (dialect, bs) = (self.sql_dialect(), self.sql_bs());
        crate::app::sql::bar::drill(self.tm().unwrap(), row, dialect, bs);
        self.reload();
    }

    /// Content: the query line, the results line and the table.
    pub(in crate::app) fn sql_content(&mut self, ui: &mut egui::Ui) {
        // Loaded with another page size, set from another tab: its page numbers no longer line up.
        let size = self.page_size();
        if let Some(t) = self.tm().filter(|t| t.size != 0 && t.size != size && !t.loading) {
            t.page = 0;
            self.reload();
        }
        let (dialect, bs) = (self.sql_dialect(), self.sql_bs());
        let (conn, db) = (self.tabs[self.cur].sel.conn, self.tabs[self.cur].sel.db.clone());
        let schema = self.sel_schema(&self.tabs[self.cur].sel);
        let scope = Self::sql_scope(&self.conns, &self.sql_schema, &self.views, conn, &db, &schema, None);
        let none = Fields::new();
        let t = &mut self.tabs[self.cur];
        let run = crate::app::sql::bar::bar(ui, t, dialect, bs, scope, self.sql_values.get(&(conn, ns_key(&db, &schema))).unwrap_or(&none));
        let wants = t.sql.as_mut().map(|s| std::mem::take(&mut s.bar.wants)).unwrap_or_default();
        self.sql_serve(conn, &db, &schema, wants);
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
            if crate::app::sql::bar::grouped_note(ui, self.t().map_or(0, |t| t.count), rows) {
                crate::app::sql::bar::ungroup(self.tm().unwrap());
                self.reload();
            }
            ui.add_space(6.0);
        }
        self.sql_results(ui);
    }
}
