//! Row and column operations of a SQL Content grid: Update matching and Delete all matching rows (the bulk dialog), and
//! the column menu's Data read over every matching row. Drawn by src/app/sql/bulk.rs and src/app/column_menu.rs; the
//! bulk dialog's form stays in the app's Modal, which also takes the count it asks for.

use super::ddl::SqlForm;
use crate::action::{Dialog, Refusal, Reply, UiFx};
use crate::core::Core;
use crate::db::ColOp;
use crate::db::sql::bulk::BulkPlan;
use crate::state::sql::bulk::{BulkForm, Pair};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum SqlOpsA {
    /// Opens Update matching (`update`) or Delete all matching rows on the WHERE of the page on screen; `col` starts the
    /// update with that column, as the column menu's Update values to does.
    Bulk { tab: u64, update: bool, col: Option<String> },
    /// The column menu's Data action `op` over every row the query matches.
    ReadColumn { tab: u64, col: String, op: ColOp },
    /// The bulk dialog's confirmed Update or Delete of `plan` on the table of `sel`, whose namespace is `ns`. It runs on the
    /// table the dialog opened on, wherever the current tab is by now.
    BulkRun { sel: crate::state::Sel, ns: String, plan: BulkPlan },
}

impl SqlOpsA {
    fn tab(&self) -> Option<u64> {
        match self {
            SqlOpsA::Bulk { tab, .. } | SqlOpsA::ReadColumn { tab, .. } => Some(*tab),
            SqlOpsA::BulkRun { .. } => None,
        }
    }
}

impl Core {
    pub(in crate::action) fn sql_ops(&mut self, a: SqlOpsA) -> Result<Reply, Refusal> {
        if let Some(uid) = a.tab() && self.tabs.get(self.cur).is_none_or(|t| t.uid != uid || t.sql.is_none()) {
            return Err(self.refuse("That tab is no longer open."));
        }
        match a {
            SqlOpsA::Bulk { update, col, .. } => self.open_bulk_with(update, col)?,
            SqlOpsA::ReadColumn { col, op, .. } => self.sql_read_column(col, op),
            SqlOpsA::BulkRun { sel, ns, plan } => self.sql_bulk_run(sel, ns, plan),
        }
        Ok(Reply::Done)
    }

    fn open_bulk_with(&mut self, update: bool, col: Option<String>) -> Result<(), Refusal> {
        let Some(t) = self.t() else { return Ok(()) };
        let Some(s) = t.sql.as_ref() else { return Ok(()) };
        if let Some(why) = self.sql_edit_block() {
            return Err(self.refuse(why));
        }
        if s.ran.grouped() {
            return Err(self.refuse("Grouped rows are summaries, not table rows. Ungroup to change rows."));
        }
        if !self.sel_writable() {
            return Err(self.refuse_ro_quiet(t.sel.conn));
        }
        let (sel, ran) = (t.sel.clone(), s.ran.clone());
        if self.sql_db_for(sel.conn, Some("Connect first.")).is_none() {
            return Ok(());
        }
        let Some((db, ns)) = self.sql_target(&sel) else {
            self.warn_offline();
            return Ok(());
        };
        self.req += 1;
        let req = self.req;
        self.worker.sql_bulk_prep(req, db, ns.clone(), sel.coll.clone(), ran.filter.clone());
        let capped = ran.limit.is_some() || ran.skip > 0;
        let form = BulkForm { update, sel, ns, filter: ran.filter, capped, req, prep: None, pairs: col.map(|col| vec![Pair { col, ..Default::default() }]).unwrap_or_default(), typed: String::new() };
        self.ui_fx.push(UiFx::Open(Dialog::SqlForm(SqlForm::Bulk(Box::new(form)))));
        Ok(())
    }

    /// Sends a confirmed plan of the bulk dialog. A connection locked again since the dialog opened is refused here and by the worker.
    pub fn sql_bulk_run(&mut self, sel: crate::state::Sel, ns: String, plan: BulkPlan) {
        let conn = sel.conn;
        if !self.conn_writable(conn) {
            return self.warn_ro();
        }
        if self.sql_db_for(conn, Some("Connect first.")).is_none() {
            return;
        }
        let Some((db, _)) = self.sql_target(&sel) else { return self.warn_offline() };
        self.worker.sql_bulk(conn, db, ns, sel.coll.clone(), plan, !self.conn_writable(conn));
    }

    /// Reads column `c` from every row the grid's query matches; the answer is `Msg::ColumnValues`.
    fn sql_read_column(&mut self, c: String, op: ColOp) {
        let Some(t) = self.t() else { return };
        let sel = t.sel.clone();
        let col = t.sql.as_ref().and_then(|s| s.cols.iter().find(|x| x.name == c).cloned());
        let Some((db, space)) = self.sql_target(&sel) else { return self.warn_offline() };
        // The query the grid shows, not bar text typed since.
        let q = self.t().and_then(|t| t.sql.as_ref()).map(|s| s.ran.clone()).unwrap_or_default();
        self.ok(format!("Reading {c} from every matching row…"));
        let src = col.map_or_else(|| c.clone(), |x| x.src);
        self.worker.sql_column_values(sel.conn, db, sel.db, space, sel.coll, q, op, c, src);
    }
}
