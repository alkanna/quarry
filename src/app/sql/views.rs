//! Drawing for SQL tabs: the Content view, and a note for the views that are not built yet.

use crate::app::results_bar::BarInfo;
use crate::app::*;
use quarry_core::action::{Action, SqlA};
use quarry_core::state::sql::bar::BarEdit;

impl App {
    pub(in crate::app) fn sql_mode(&self, conn: u64) -> db::sql::ddl::Mode {
        self.core.sql_mode(conn)
    }

    pub(in crate::app) fn sql_dialect(&self) -> db::sql::Dialect {
        self.core.sql_dialect()
    }

    /// Double click on a grouped row: its rows replace the grouped result.
    pub(in crate::app) fn sql_drill(&mut self, row: usize) {
        let tab = self.core.tabs[self.core.cur].uid;
        self.exec(Action::Sql(SqlA::Drill { tab, row }));
    }

    /// Asks for the filter panel's counts while it is open on a live connection; Core decides, the app only waits out the pause.
    fn sql_filter_tick(&mut self, ctx: &egui::Context) {
        self.exec(Action::SqlAsk(SqlAskA::FilterTick { now: ctx.input(|i| i.time) }));
        self.take_repaint(ctx);
    }

    /// Content: the query line, the results line and the table.
    pub(in crate::app) fn sql_content(&mut self, ui: &mut egui::Ui) {
        // Loaded with another page size, set from another tab: its page numbers no longer line up.
        let (size, tab) = (self.page_size(), self.core.tabs[self.core.cur].uid);
        if self.t().is_some_and(|t| t.size != 0 && t.size != size && !t.loading) {
            self.exec(Action::Sql(SqlA::PageSize { tab }));
        }
        // The bar reads the WHERE text as rules first, so it draws from the tree Core holds.
        self.exec(Action::Sql(SqlA::BarSync { tab }));
        let (dialect, bs) = (self.sql_dialect(), self.sql_bs());
        let (conn, db) = (self.core.tabs[self.core.cur].sel.conn, self.core.tabs[self.core.cur].sel.db.clone());
        let schema = self.sel_schema(&self.core.tabs[self.core.cur].sel);
        let scope = Self::sql_scope(&self.core.conns, &self.core.sql_schema, &self.core.views, conn, &db, &schema, None);
        let none = Fields::new();
        let hist = self.core.prefs.history.get(&self.core.tabs[self.core.cur].coll_key()).cloned().unwrap_or_default();
        let pick = self.pick_data(ui.ctx(), PickKind::Where);
        let covered = self.window_open();
        let table = self.core.tabs[self.core.cur].sel.coll.clone();
        let facts = self.core.sql_facts.get(&(conn, ns_key(&db, &schema), table.clone()));
        let mut side = crate::app::sql::bar::Side { hist: &hist, pick: &pick, qacts: Vec::new(), pick_acts: Vec::new(), help: false, covered, facts, acts: Vec::new(), save_view: false, explain: false, save_filter: false, edit: None };
        let u = self.ui.bar_ui.entry(tab).or_default();
        let run = crate::app::sql::bar::bar(ui, &self.core.tabs[self.core.cur], u, dialect, bs, scope, self.core.sql_values.get(&(conn, ns_key(&db, &schema))).unwrap_or(&none), &mut side);
        let wants = std::mem::take(&mut u.wants);
        let want_facts = u.want_facts.take();
        // Everything the bar left in `side` is taken before the first action runs, since `side` still borrows the core.
        let (edit, qacts, pick_acts) = (side.edit.take(), std::mem::take(&mut side.qacts), std::mem::take(&mut side.pick_acts));
        let (acts, help, save_view, explain, save_filter) = (std::mem::take(&mut side.acts), side.help, side.save_view, side.explain, side.save_filter);
        if let Some(edit) = edit {
            self.exec(Action::Sql(SqlA::BarEdit { tab, edit: Box::new(edit) }));
        }
        // Picking from the history fills the box, as in the Documents view; Run or Enter applies it.
        for a in qacts {
            let edit = match a {
                QAct::Filter(v) => BarEdit { filter: Some(v), ..Default::default() },
                QAct::Sort(v) => BarEdit { sort: Some(v), ..Default::default() },
                QAct::Project(_) => continue,
            };
            self.exec(Action::Sql(SqlA::BarEdit { tab, edit: Box::new(edit) }));
        }
        for a in acts {
            self.exec(Action::Sql(a));
        }
        if save_view {
            self.save_content_as_view(self.core.cur);
        }
        if help {
            self.modal = Some(Modal::Text { title: "Query help".into(), body: crate::db::sql::rules::filter_help(dialect), code: false });
        }
        self.sql_serve(conn, &db, &schema, wants);
        if let Some((column, kind)) = want_facts {
            self.sql_serve_facts(conn, &db, &schema, &table, column, kind);
        }
        self.sql_filter_tick(ui.ctx());
        self.pick_acts(pick_acts);
        if explain {
            return self.explain();
        }
        if save_filter {
            self.save_script_as();
        }
        if run {
            self.exec(Action::Sql(SqlA::Apply { tab }));
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
            about: if t.sql.as_ref().is_some_and(|s| s.estimated) { "~" } else { "" },
            ms: t.ms,
            loading: t.loading,
            stats: &[],
            grouped,
        };
        let out = self.results_bar(ui, &info, |app, ui| app.sql_row_tools(ui));
        if out.ungroup {
            self.exec(Action::Sql(SqlA::Ungroup { tab }));
        } else if let Some(page) = out.go {
            self.exec(Action::Sql(SqlA::Page { tab, page }));
        }
        ui.add_space(4.0);
        self.sql_results(ui);
    }
}
