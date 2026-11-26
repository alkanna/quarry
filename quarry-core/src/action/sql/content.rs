//! The Content grid of a SQL tab: its page loads, the bar's filter, sort and paging, and the answers that fill them.
//! Drawn by src/app/sql/views.rs, bar.rs and edit.rs.

use crate::core::Core;
use crate::db;
use crate::state::config::push_history;

impl Core {
    /// Asks for the tab's current page. The answer is matched back to the tab by `req`.
    pub fn reload_sql(&mut self, i: usize) {
        // A Query tab that belongs to no table has no page to load.
        if self.tabs.get(i).is_some_and(|t| t.sel.coll.is_empty()) {
            return;
        }
        let Some((db, space)) = self.tabs.get(i).and_then(|t| self.sql_target(&t.sel)) else { return };
        self.req += 1;
        let (req, size) = (self.req, self.page_size());
        let t = &mut self.tabs[i];
        (t.req, t.loading, t.size) = (req, true, size);
        let q = crate::state::sql::bar::page_query(t);
        if let Some(s) = t.sql.as_mut() {
            s.ran = q.clone();
            s.bar.dirty = false;
        }
        self.worker.sql_page(req, db, space, t.sel.coll.clone(), q, size, t.page * size);
    }

    /// Puts the WHERE and ORDER BY of tab `i`'s page in the table's history once the page loaded, so a filter the server
    /// refused is not kept. They are saved in ui.json and listed in the menu, so a password in them is masked.
    pub fn sql_remember_filter(&mut self, i: usize) {
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

    /// Reads the open tab's page again, as the app's reload does for a SQL tab; the demo has no server to ask.
    pub(super) fn sql_reload(&mut self) {
        self.sql_reload_of(self.cur);
    }

    /// Reads tab `i`'s page again, as the app's reload_of does for a SQL tab.
    pub(crate) fn sql_reload_of(&mut self, i: usize) {
        if self.demo {
            return;
        }
        if let Some(t) = self.tabs.get_mut(i) {
            t.agent_changed = false;
        }
        self.reload_sql(i);
    }

    /// The bar's Run, Enter or the panel's Apply: the first page of what the bar reads now.
    pub(super) fn sql_apply(&mut self) {
        self.tabs[self.cur].page = 0;
        self.sql_reload();
    }

    /// The results bar's pager.
    pub(super) fn sql_page(&mut self, page: u64) {
        self.tabs[self.cur].page = page;
        self.sql_reload();
    }

    /// Ungroup: back to the table's rows, keeping a sort that names real columns.
    pub(super) fn sql_ungroup(&mut self) {
        crate::state::sql::bar::ungroup(&mut self.tabs[self.cur]);
        self.sql_reload();
    }

    /// Double click on a grouped row: its rows replace the grouped result.
    pub(super) fn sql_drill(&mut self, row: usize) {
        let (dialect, bs) = (self.sql_dialect(), self.sql_bs());
        crate::state::sql::bar::drill(&mut self.tabs[self.cur], row, dialect, bs);
        self.sql_reload();
    }

    /// The Rules or SQL switch of the bar. Rules mode reads the WHERE text again into the tree first.
    pub(super) fn sql_where_mode(&mut self, sql: bool) {
        let (d, bs) = (self.sql_dialect(), self.sql_bs());
        let t = &mut self.tabs[self.cur];
        let Some(s) = t.sql.as_mut() else { return };
        let b = &mut s.bar;
        if sql == b.c.sql {
            return;
        }
        b.c.sql = sql;
        if sql {
            b.panel.open = false;
        } else {
            let cols = if b.table_cols.is_empty() { s.cols.clone() } else { b.table_cols.clone() };
            let pairs: Vec<_> = cols.iter().map(|c| (c.name.clone(), c.kind)).collect();
            // The tree is from before SQL mode, so it is read again now, before the next frame writes it out.
            b.src.clear();
            crate::state::sql::bar::read_tree(b, &t.filter_txt, &pairs, d, bs, true);
        }
    }

    /// The columns of a page answer, given to the tab that asked.
    pub fn sql_cols(&mut self, req: u64, cols: Vec<db::sql::Col>, pk: Vec<String>, estimated: bool, matched: Option<u64>) {
        if let Some(s) = self.tabs.iter_mut().find(|t| t.req == req).and_then(|t| t.sql.as_mut()) {
            // The rules pick from table columns, which a grouped answer does not have.
            let cols = std::sync::Arc::new(cols);
            if matched.is_none() {
                s.bar.table_cols = cols.clone();
            }
            (s.cols, s.pk, s.estimated, s.matched) = (cols, pk, estimated, matched);
        }
    }

    /// An answer for a filter panel's count, given to the tab that asked.
    pub fn sql_filter_counted(&mut self, req: u64, res: crate::state::sql::filter_panel::Counted) {
        for t in &mut self.tabs {
            if let Some(s) = t.sql.as_mut()
                && s.bar.panel.counted(req, &res)
            {
                return;
            }
        }
    }

    /// The rows a foreign key cell editor looked up, for the lookup still awaited.
    pub fn sql_keys_done(&mut self, req: u64, res: db::Res<(db::sql::KeyTable, Vec<(String, String)>)>) {
        let Some(k) = self.sql_cell.as_mut().and_then(|e| e.keys.as_mut()).filter(|k| k.req == req) else { return };
        k.busy = false;
        // A lookup the server refuses leaves the list empty; the key can still be typed and saved.
        match res {
            Ok((t, rows)) => (k.table, k.rows) = (Some(t), rows),
            // Treated as a big table with no name column, so later lookups skip the catalog and only match exact keys.
            Err(_) => (k.table, k.rows) = (Some(db::sql::KeyTable { label: None, big: true }), Vec::new()),
        }
    }
}

impl Core {
    /// Before the Content bar draws: reads the WHERE text as a tree and works out whether the bar holds another query than the one that ran.
    pub(super) fn sql_bar_sync(&mut self) {
        let (d, bs) = (self.sql_dialect(), self.sql_bs());
        crate::state::sql::bar::sync(&mut self.tabs[self.cur], d, bs);
    }

    /// What a frame of the Content bar changed: its texts, clauses, tree and panel. A row edited in the panel also writes the WHERE text.
    pub(super) fn sql_bar_edit(&mut self, e: crate::state::sql::bar::BarEdit) {
        let (d, bs) = (self.sql_dialect(), self.sql_bs());
        let t = &mut self.tabs[self.cur];
        let Some(s) = t.sql.as_mut() else { return };
        if let Some(f) = e.filter {
            t.filter_txt = f;
        }
        if let Some(v) = e.sort {
            t.sort_txt = v;
        }
        let b = &mut s.bar;
        if let Some(c) = e.clauses {
            // The Rules or SQL switch is its own action and may have run since the bar copied the clauses.
            let sql = b.c.sql;
            b.c = c;
            b.c.sql = sql;
        }
        if let Some(v) = e.panel {
            // Opening shows counts for rows that may have changed while it was closed, so they are asked again.
            if v.open && !b.panel.open {
                b.panel.forget_counts();
            }
            (b.panel.open, b.panel.sel, b.panel.pick, b.panel.ops, b.panel.hover) = (v.open, v.sel, v.pick, v.ops, v.hover);
        }
        if let Some(tree) = e.tree {
            b.tree = tree;
        }
        if e.written {
            let cols = if b.table_cols.is_empty() { s.cols.clone() } else { b.table_cols.clone() };
            let pairs: Vec<_> = cols.iter().map(|c| (c.name.clone(), c.kind)).collect();
            crate::state::sql::bar::write_tree(b, &mut t.filter_txt, &pairs, d, bs);
            b.tree_json = crate::db::sql::filter::to_json(&b.tree);
        }
    }
}
