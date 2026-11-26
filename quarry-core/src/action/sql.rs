//! The views of a SQL tab (MySQL, MariaDB, PostgreSQL): the Query view and the Content grid. Drawn by src/app/sql/; each
//! variant names the tab it was drawn for by uid, and runs only while that tab is the open one.

use super::{Reply, Refusal};
use crate::core::Core;
use serde::{Deserialize, Serialize};

pub use filter::FilterA;

pub mod ac;
pub mod content;
pub mod edit;
pub mod filter;
pub mod ns;
pub mod query;
pub mod ddl;
pub mod explain;
pub mod ops;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum SqlA {
    /// Query view: the statement under the caret, or the whole script with `all`; `explain` opens the plan instead.
    Run { tab: u64, all: bool, explain: bool },
    /// A statement picked from the Run menu or the gutter, or the selected text.
    RunText { tab: u64, text: String },
    /// Interrupts the running script.
    Stop { tab: u64 },
    /// Shows result tab `set`.
    PickSet { tab: u64, set: usize },
    /// The Table or Text switch of the results.
    TextMode { tab: u64, on: bool },
    /// Reads result tab `set` again at page `page`.
    GotoPage { tab: u64, set: usize, page: usize },
    /// Sorts the shown result on the client; no keys is the server's order.
    SortPage { tab: u64, keys: Vec<(String, bool)> },
    /// Checks or unchecks row `row` of the shown result; `range` extends from the last one checked.
    Check { tab: u64, row: usize, range: bool },
    CheckAll { tab: u64, on: bool },
    ClearChecks { tab: u64 },
    /// The selected row of the shown result, whose record Tab opens.
    SelectRow { tab: u64, row: usize },
    /// Content: the bar's Run, Enter or the panel's Apply, on the first page.
    Apply { tab: u64 },
    /// Content: the results bar's pager.
    Page { tab: u64, page: u64 },
    /// Content: back from a grouped result to the table's rows.
    Ungroup { tab: u64 },
    /// Content: a grouped row's own rows, from a double click.
    Drill { tab: u64, row: usize },
    /// Content: the Rules or SQL switch of the bar.
    WhereMode { tab: u64, sql: bool },
    /// Content: a structural edit of the filter panel's conditions.
    Filter { tab: u64, a: FilterA },
    /// Content: a double click on cell `col` of page row `row` opens its editor.
    OpenCell { tab: u64, row: usize, col: String },
    /// Content: a cell menu's Set to NULL.
    SetNull { tab: u64, row: usize, col: String },
    /// Content: the staged bar's Save, or Cmd+S.
    Save { tab: u64 },
    /// Content: the staged bar's Discard.
    Discard { tab: u64 },
    /// Content: the insert form, empty, or filled from a row for a duplicate.
    NewRow { tab: u64, from: Option<mongodb::bson::Document> },
    /// Content: the insert form's Insert, or a duplicate of several rows.
    Insert { tab: u64, rows: Vec<crate::db::sql::write::NewRow> },
    /// Content: deletes these rows of the page, by the key in their `_id`, once confirmed.
    DeleteRows { tab: u64, rows: Vec<mongodb::bson::Document> },
    /// The crumb's database pick on a Query view. A free Query tab moves to it and a table's Query view runs there; `schema`
    /// is the one the sidebar shows for that database.
    CrumbDb { tab: u64, db: String, schema: String },
    /// The crumb's schema pick on a PostgreSQL Query view.
    CrumbSchema { tab: u64, schema: String },
}

impl SqlA {
    /// The uid of the tab the action was drawn for.
    fn tab(&self) -> u64 {
        match self {
            SqlA::Run { tab, .. } | SqlA::RunText { tab, .. } | SqlA::Stop { tab } | SqlA::PickSet { tab, .. } | SqlA::TextMode { tab, .. } | SqlA::GotoPage { tab, .. } | SqlA::SortPage { tab, .. } | SqlA::Check { tab, .. } | SqlA::CheckAll { tab, .. } | SqlA::ClearChecks { tab } | SqlA::SelectRow { tab, .. } | SqlA::Apply { tab } | SqlA::Page { tab, .. } | SqlA::Ungroup { tab } | SqlA::Drill { tab, .. } | SqlA::WhereMode { tab, .. } | SqlA::Filter { tab, .. } | SqlA::OpenCell { tab, .. } | SqlA::SetNull { tab, .. } | SqlA::Save { tab } | SqlA::Discard { tab } | SqlA::NewRow { tab, .. } | SqlA::Insert { tab, .. } | SqlA::DeleteRows { tab, .. } | SqlA::CrumbDb { tab, .. } | SqlA::CrumbSchema { tab, .. } => *tab,
        }
    }
}

impl Core {
    pub(super) fn sql(&mut self, a: SqlA) -> Result<Reply, Refusal> {
        // Drawn and run in the same frame, so another tab means the action is stale or came from elsewhere.
        if self.tabs.get(self.cur).is_none_or(|t| t.uid != a.tab() || t.sql.is_none()) {
            return Err(self.refuse("That tab is no longer open."));
        }
        match a {
            SqlA::Run { all, explain, .. } => self.sql_run(all, explain),
            SqlA::RunText { text, .. } => self.sql_run_text(text, false),
            SqlA::Stop { .. } => self.sql_stop(),
            SqlA::PickSet { set, .. } => {
                let s = self.sql_tab();
                (s.set_sel, s.text_cache) = (set, None);
                s.qchecked.clear();
                (s.qanchor, s.qsel) = (None, None);
            }
            SqlA::TextMode { on, .. } => self.sql_tab().text_mode = on,
            SqlA::GotoPage { set, page, .. } => self.sql_goto_page(set, page),
            SqlA::SortPage { keys, .. } => self.sql_sort_page(keys),
            SqlA::Check { row, range, .. } => {
                let s = self.sql_tab();
                match s.qanchor.filter(|_| range) {
                    Some(a) => s.qchecked.extend(a.min(row)..=a.max(row)),
                    None => {
                        if !s.qchecked.insert(row) {
                            s.qchecked.remove(&row);
                        }
                    }
                }
                s.qanchor = Some(row);
            }
            SqlA::CheckAll { on, .. } => {
                let s = self.sql_tab();
                let n = s.cur_set().map_or(0, |x| x.rows.len());
                s.qchecked = if on { (0..n).collect() } else { Default::default() };
            }
            SqlA::ClearChecks { .. } => self.sql_tab().qchecked.clear(),
            SqlA::SelectRow { row, .. } => self.sql_tab().qsel = Some(row),
            SqlA::Apply { .. } => self.sql_apply(),
            SqlA::Page { page, .. } => self.sql_page(page),
            SqlA::Ungroup { .. } => self.sql_ungroup(),
            SqlA::Drill { row, .. } => self.sql_drill(row),
            SqlA::WhereMode { sql, .. } => self.sql_where_mode(sql),
            SqlA::Filter { a, .. } => self.sql_filter(a),
            SqlA::OpenCell { row, col, .. } => return self.sql_open_cell(row, col),
            SqlA::SetNull { row, col, .. } => return self.sql_set_null(row, col),
            SqlA::Save { .. } => {
                let keys = self.tab_drafts();
                return self.sql_save_all(keys);
            }
            SqlA::Discard { .. } => self.sql_discard(),
            SqlA::NewRow { from, .. } => return self.sql_new_row(from),
            SqlA::Insert { rows, .. } => self.sql_insert_rows(rows),
            SqlA::DeleteRows { rows, .. } => self.sql_delete(rows),
            SqlA::CrumbDb { db, schema, .. } => {
                let t = &mut self.tabs[self.cur];
                let (free, own) = (t.sel.coll.is_empty(), db == t.sel.db);
                if free {
                    (t.sel.db, t.sel.schema) = (db, schema);
                    // The tab's own database and schema are now the whole target, so a run target left by a script goes.
                    if let Some(q) = t.sql.as_mut() {
                        (q.run_db, q.run_schema) = (String::new(), String::new());
                    }
                } else if let Some(q) = t.sql.as_mut() {
                    // A table's Query view keeps its table and only runs elsewhere. Its own database runs in the table's schema.
                    q.run_schema = if own { String::new() } else { schema };
                    q.run_db = db;
                }
            }
            SqlA::CrumbSchema { schema, .. } => {
                let t = &mut self.tabs[self.cur];
                let free = t.sel.coll.is_empty();
                let Some(q) = t.sql.as_mut() else { return Ok(Reply::Done) };
                // A run target in another database is where the statements run, so the tab's own schema would not reach them.
                if free && (q.run_db.is_empty() || q.run_db == t.sel.db) {
                    // A schema left from a script or a saved tab would win over the pick.
                    q.run_schema.clear();
                    t.sel.schema = schema;
                } else {
                    q.run_schema = schema;
                }
            }
        }
        Ok(Reply::Done)
    }

    /// The SQL state of the open tab, which `sql` checked is there.
    fn sql_tab(&mut self) -> &mut crate::state::SqlTab {
        self.tabs[self.cur].sql.as_mut().expect("a SQL tab")
    }
}
