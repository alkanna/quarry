//! Drawing for SQL tabs: the Content view, and a note for the views that are not built yet.

use crate::app::results_bar::BarInfo;
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
        if let Some(s) = t.sql.as_mut() {
            s.ran = q.clone();
            s.bar.dirty = false;
        }
        self.worker.sql_page(req, db, space, t.sel.coll.clone(), q, size, t.page * size);
    }

    /// Puts the WHERE and ORDER BY of tab `i`'s page in the table's history once the page loaded, so a filter the server
    /// refused is not kept. They are saved in ui.json and listed in the menu, so a password in them is masked.
    pub(in crate::app) fn sql_remember_filter(&mut self, i: usize) {
        let Some(t) = self.tabs.get(i).filter(|t| !t.sel.coll.is_empty()) else { return };
        let Some(s) = t.sql.as_ref() else { return };
        let (key, filter, order) = (t.coll_key(), db::redact_sql(&s.ran.filter).0, db::redact_sql(&s.ran.order).0);
        let h = self.prefs.history.entry(key).or_default();
        let before = (h.selector.first().cloned(), h.sort.first().cloned());
        push_history(&mut h.selector, &filter);
        push_history(&mut h.sort, &order);
        // Paging and reloads repeat the same texts, which leave the lists as they were and need no write.
        if (h.selector.first().cloned(), h.sort.first().cloned()) != before {
            self.save_prefs();
        }
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

    /// Asks for the filter panel's counts while it is open on a live connection: the whole WHERE once it has rested,
    /// and the hovered row alone. Each slot has one request in flight; an answer to an older one is dropped.
    fn sql_filter_tick(&mut self, ctx: &egui::Context, conn: u64, db: &str, schema: &str, table: &str) {
        use crate::app::sql::filter_panel::Tick;
        // The demo has no session, and a PostgreSQL tab without its schema has nothing to count in.
        let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) else { return };
        if schema.is_empty() && self.is_pg(conn) {
            return;
        }
        let (d, bs) = (self.sql_dialect(), self.sql_bs());
        let now = ctx.input(|i| i.time);
        let space = if schema.is_empty() { db.to_string() } else { schema.to_string() };
        let t = &mut self.tabs[self.cur];
        let text = t.filter_txt.clone();
        let Some(s) = t.sql.as_mut().filter(|s| s.bar.panel.open && !s.bar.c.sql) else { return };
        let p = &mut s.bar.panel;
        // The same text on another table, schema, database or connection is another count.
        let place = format!("{conn}\u{1f}{db}\u{1f}{schema}\u{1f}{table}");
        if p.table != place {
            p.forget_counts();
            p.table = place;
        }
        match p.count.tick(&text, now) {
            Tick::Ask => {
                self.req += 1;
                p.count.ask(self.req, &text);
                self.worker.sql_filter_count(self.req, sql.at(db), space.clone(), table.to_string(), text);
            }
            Tick::Wait(left) => ctx.request_repaint_after(std::time::Duration::from_secs_f32(left)),
            Tick::Idle => {}
        }
        let hovered = p.hover.and_then(|id| s.bar.tree.find(id)).and_then(|n| match &n.item {
            db::sql::filter::Item::Cond(c) if c.on && c.rule.complete() => Some(db::sql::rules::rule_sql(&c.rule, d, bs)),
            _ => None,
        });
        if let Some(one) = hovered.filter(|h| !p.hover_cache.contains_key(h)) {
            match p.hover_count.tick(&one, now) {
                Tick::Ask => {
                    self.req += 1;
                    p.hover_count.ask(self.req, &one);
                    self.worker.sql_filter_count(self.req, sql.at(db), space, table.to_string(), one);
                }
                Tick::Wait(left) => ctx.request_repaint_after(std::time::Duration::from_secs_f32(left)),
                Tick::Idle => {}
            }
        }
    }

    /// An answer for a filter panel's count, given to the tab that asked.
    pub(in crate::app) fn sql_filter_counted(&mut self, req: u64, res: crate::app::sql::filter_panel::Counted) {
        for t in &mut self.tabs {
            if let Some(s) = t.sql.as_mut()
                && s.bar.panel.counted(req, &res)
            {
                return;
            }
        }
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
        let hist = self.prefs.history.get(&self.tabs[self.cur].coll_key()).cloned().unwrap_or_default();
        let pick = self.pick_data(ui.ctx(), PickKind::Where);
        let covered = self.window_open();
        let table = self.tabs[self.cur].sel.coll.clone();
        let facts = self.sql_facts.get(&(conn, ns_key(&db, &schema), table.clone()));
        let mut side = crate::app::sql::bar::Side { hist: &hist, pick: &pick, qacts: Vec::new(), pick_acts: Vec::new(), help: false, covered, facts };
        let t = &mut self.tabs[self.cur];
        let run = crate::app::sql::bar::bar(ui, t, dialect, bs, scope, self.sql_values.get(&(conn, ns_key(&db, &schema))).unwrap_or(&none), &mut side);
        let wants = t.sql.as_mut().map(|s| std::mem::take(&mut s.bar.wants)).unwrap_or_default();
        let want_facts = t.sql.as_mut().and_then(|s| s.bar.want_facts.take());
        // Picking from the history fills the box, as in the Documents view; Run or Enter applies it.
        for a in std::mem::take(&mut side.qacts) {
            match a {
                QAct::Filter(v) => t.filter_txt = v,
                QAct::Sort(v) => t.sort_txt = v,
                QAct::Project(_) => {}
            }
        }
        let pick_acts = std::mem::take(&mut side.pick_acts);
        if side.help {
            self.modal = Some(Modal::Text { title: "Query help".into(), body: crate::db::sql::rules::filter_help(dialect), code: false });
        }
        self.sql_serve(conn, &db, &schema, wants);
        if let Some((column, kind)) = want_facts {
            self.sql_serve_facts(conn, &db, &schema, &table, column, kind);
        }
        self.sql_filter_tick(ui.ctx(), conn, &db, &schema, &table);
        self.pick_acts(pick_acts);
        if self.tabs[self.cur].sql.as_mut().is_some_and(|s| std::mem::take(&mut s.bar.explain)) {
            return self.explain();
        }
        if self.tabs[self.cur].sql.as_mut().is_some_and(|s| std::mem::take(&mut s.bar.save_filter)) {
            self.save_script_as();
        }
        if run {
            self.tm().unwrap().page = 0;
            self.reload();
        }
        self.agent_notice(ui);
        ui.add_space(10.0);
        let t = self.t().unwrap();
        let grouped = t.sql.as_ref().and_then(|s| s.matched);
        let info = BarInfo {
            page: t.page,
            size: self.page_size(),
            total: t.count,
            unit: if grouped.is_some() { "groups" } else { "rows" },
            approx: t.sql.as_ref().is_some_and(|s| s.estimated),
            ms: t.ms,
            loading: t.loading,
            stats: &[],
            grouped,
        };
        let out = self.results_bar(ui, &info, |app, ui| app.sql_row_tools(ui));
        if out.ungroup {
            crate::app::sql::bar::ungroup(self.tm().unwrap());
            self.reload();
        } else if let Some(p) = out.go {
            self.tm().unwrap().page = p;
            self.reload();
        }
        ui.add_space(4.0);
        self.sql_results(ui);
    }
}
