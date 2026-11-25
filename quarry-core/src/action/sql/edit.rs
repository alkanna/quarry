//! Editing SQL rows from the Content grid: the cell editor's opening, cell edits staged as drafts, Save and Discard of the
//! staged edits, row inserts and deletes. Drawn by src/app/sql/edit.rs.

use crate::action::{Dialog, Refusal, Reply, UiFx};
use crate::core::drafts::{is_long, stage_draft};
use crate::core::Core;
use crate::db;
use crate::db::sql::value::Kind;
use crate::db::sql::write::{NewRow, RowEdit};
use crate::db::sql::Col;
use crate::state::sql::edit::*;
use crate::state::table::Rows;
use crate::state::{Draft, Sel};
use mongodb::bson::{Bson, Document};
use std::sync::Arc;

pub fn has_key(d: &Document) -> bool {
    d.get_document("_id").is_ok_and(|k| !k.is_empty())
}

impl Core {
    /// The table, columns and key of the open SQL tab.
    pub fn sql_ctx(&self) -> Option<(Sel, Arc<Vec<Col>>, Vec<String>)> {
        let t = self.t()?;
        let s = t.sql.as_ref()?;
        Some((t.sel.clone(), s.cols.clone(), s.pk.clone()))
    }

    /// Why the open table cannot be edited at all, if so.
    pub fn sql_edit_block(&self) -> Option<&'static str> {
        let s = self.t()?.sql.as_ref()?;
        if s.matched.is_some() {
            Some("Grouped rows are summaries, not table rows. Ungroup to edit.")
        } else if self.sel_is_view() {
            Some("Views cannot be edited.")
        } else if s.pk.is_empty() {
            Some("This table has no primary key or unique key, so its rows cannot be edited.")
        } else {
            None
        }
    }

    /// Why row `i` of the open table cannot be edited, if so. A unique key lets rows hold NULL, and such a row has no `_id`:
    /// its draft would be shared with every other keyless row and could never be saved.
    pub fn sql_row_block(&self, i: usize) -> Option<&'static str> {
        let t = self.t()?;
        Rows::Raw(&t.docs).id(i).is_none().then_some("This row has no key value, so it cannot be edited here.")
    }

    /// A column of the open SQL table, by its key in the row documents.
    pub fn sql_col(&self, name: &str) -> Option<&Col> {
        self.t()?.sql.as_ref()?.cols.iter().find(|c| c.name == name)
    }

    /// Why one column cannot be edited in place, if so.
    pub fn sql_col_block(&self, col: &str) -> Option<&'static str> {
        let s = self.t()?.sql.as_ref()?;
        if s.pk.iter().any(|p| p == col) {
            return Some("Key columns can't be edited inline, because changing the key would break undo for this row. Use a query instead.");
        }
        let c = self.sql_col(col)?;
        if c.generated {
            return Some("Generated columns are computed by the server and cannot be written.");
        }
        (c.kind == Kind::Geometry).then_some("Geometry values cannot be edited as text yet.")
    }

    /// Everything a write needs, or None with the reason shown. The one gate for SQL writes.
    fn writable_sql(&mut self) -> Option<(u64, db::sql::Db, Sel, Arc<Vec<Col>>, Vec<String>)> {
        let (sel, cols, pk) = self.sql_ctx()?;
        if self.demo {
            self.ok("Offline demo: no database is connected.");
            return None;
        }
        if !self.sel_writable() {
            self.warn_ro();
            return None;
        }
        let Some((db, _)) = self.sql_target(&sel) else {
            self.warn_offline();
            return None;
        };
        Some((sel.conn, db, sel, cols, pk))
    }

    /// The open tab's connection, for a refusal that asks for writes on it.
    fn sql_conn(&self) -> u64 {
        self.t().map_or(0, |t| t.sel.conn)
    }

    /// Double-click on a cell: opens the editor, after asking for write access when it is locked.
    pub fn sql_open_cell(&mut self, i: usize, col: String) -> Result<Reply, Refusal> {
        if let Some(why) = self.sql_edit_block().or_else(|| self.sql_col_block(&col)) {
            return Err(self.refuse(why));
        }
        if !self.sel_writable() {
            return Err(self.refuse_ro_quiet(self.sql_conn()));
        }
        if self.panel_dirty() {
            return Err(self.refuse("Apply or cancel the value in the side panel first."));
        }
        let Some(t) = self.t() else { return Ok(Reply::Done) };
        let rows = Rows::Raw(&t.docs);
        if let Some(why) = self.sql_row_block(i) {
            return Err(self.refuse(why));
        }
        let v = self.shown_row(rows, i, false).and_then(|d| d.get(&col).cloned()).or_else(|| rows.value(i, &col).map(|v| v.bson().into_owned())).unwrap_or(Bson::Null);
        let kind = self.sql_col(&col).map_or(Kind::Text, |c| c.kind);
        let text = edit_text(&v, kind);
        // With the panel open, a double-click loads the new cell into it, so the panel follows the selection.
        let big = is_long(&text) || self.value_in_panel();
        self.sql_cell = Some(CellEdit { json_err: None, id: rows.id(i), col, text, dirty: false, err: None, focus: true, done: None, big, ..Default::default() });
        Ok(Reply::Done)
    }

    /// A cell menu's Set to NULL, after asking for write access when it is locked.
    pub fn sql_set_null(&mut self, i: usize, col: String) -> Result<Reply, Refusal> {
        if let Some(why) = self.sql_edit_block().or_else(|| self.sql_col_block(&col)) {
            return Err(self.refuse(why));
        }
        if let Some(why) = self.sql_row_block(i) {
            return Err(self.refuse(why));
        }
        if !self.sel_writable() {
            return Err(self.refuse_ro_quiet(self.sql_conn()));
        }
        self.sql_set_cell(i, &col, Bson::Null);
        Ok(Reply::Done)
    }

    /// Stages `v` as the value of a cell of the open page.
    pub fn sql_set_cell(&mut self, i: usize, col: &str, v: Bson) {
        let Some(t) = self.t() else { return };
        let rows = Rows::Raw(&t.docs);
        let Some(orig) = rows.owned(i) else { return };
        if let Some(why) = self.sql_row_block(i) {
            return self.warn(why);
        }
        let cur = self.shown_row(rows, i, false).unwrap_or(orig);
        let mut new = cur.clone();
        new.insert(col, v);
        self.sql_stage(&cur, new);
    }

    fn sql_stage(&mut self, d: &Document, new: Document) {
        let key = self.draft_key(d);
        let Some(sel) = self.t().map(|t| t.sel.clone()) else { return };
        let n = stage_draft(&mut self.drafts, &key, &sel, d, new);
        if n == 0 {
            self.ok("No changes left on this row");
        } else if self.prefs.auto_save && self.sel_writable() {
            self.write_sql_drafts(vec![key]);
        } else {
            self.ok(format!("{n} cell{} staged. Save writes them, Discard drops them.", if n == 1 { "" } else { "s" }));
        }
    }

    /// One transaction for the given drafts. A failure keeps every one of them staged.
    pub fn write_sql_drafts(&mut self, keys: Vec<String>) {
        let drafts: Vec<(String, Draft)> = keys.into_iter().filter_map(|k| self.drafts.remove(&k).map(|d| (k, d))).collect();
        let Some(first) = drafts.first().map(|d| Sel { conn: d.1.conn, db: d.1.db.clone(), schema: d.1.schema.clone(), coll: d.1.coll.clone() }) else { return };
        let put_back = |a: &mut Core, drafts: Vec<(String, Draft)>, why: &str| {
            a.drafts.extend(drafts);
            a.err(why.to_string());
        };
        let Some((db, space)) = self.sql_target(&first) else { return put_back(self, drafts, "Not connected.") };
        let read_only = !self.conn_writable(first.conn);
        let Some((cols, pk)) = self.tabs.iter().find(|t| t.sel == first).and_then(|t| t.sql.as_ref()).map(|s| (s.cols.to_vec(), s.pk.clone())) else { return put_back(self, drafts, "Open the table to save its edits.") };
        // A row without its key could address any row, so it is never sent; its edits stay staged.
        let (keyed, keyless): (Vec<_>, Vec<_>) = drafts.into_iter().partition(|(_, d)| has_key(&d.current));
        let skipped = keyless.len();
        self.drafts.extend(keyless);
        if skipped > 0 {
            self.err(format!("{skipped} row{} without a key value {} not saved.", if skipped == 1 { "" } else { "s" }, if skipped == 1 { "was" } else { "were" }));
        }
        let drafts = keyed;
        if drafts.is_empty() {
            return;
        }
        let edits: Vec<RowEdit> = drafts
            .iter()
            .map(|(_, d)| RowEdit {
                pk: d.current.get_document("_id").cloned().unwrap_or_default(),
                changes: db::diff(&d.original, &d.current).into_iter().filter(|c| c.0 != "_id").map(|(p, a, b)| (p, a.unwrap_or(Bson::Null), b.unwrap_or(Bson::Null))).collect(),
            })
            .collect();
        let tags: Vec<String> = drafts.iter().map(|d| d.0.clone()).collect();
        self.saving.extend(drafts);
        self.worker.sql_save(tags, first.conn, db, space, first.coll, cols, pk, edits, read_only);
    }

    /// Cmd+S, the staged bar's Save: asks for write access first, then confirms like Mongo's Save all when the setting says so.
    pub fn sql_save_all(&mut self, keys: Vec<String>) -> Result<Reply, Refusal> {
        if !self.sel_writable() {
            return Err(self.refuse_ro_quiet(self.sql_conn()));
        }
        if self.prefs.confirm_save && !self.prefs.always_writable {
            self.ui_fx.push(UiFx::Open(Dialog::SaveAll(keys)));
        } else {
            self.write_sql_drafts(keys);
        }
        Ok(Reply::Done)
    }

    /// The staged bar's Discard.
    pub(super) fn sql_discard(&mut self) {
        for k in self.tab_drafts() {
            self.drafts.remove(&k);
        }
        self.ok("Discarded the staged edits");
    }

    /// Opens the insert form, filled from `from` for a duplicate.
    pub fn sql_new_row(&mut self, from: Option<Document>) -> Result<Reply, Refusal> {
        if !self.sel_writable() {
            return Err(self.refuse_ro_quiet(self.sql_conn()));
        }
        let Some((_, cols, pk)) = self.sql_ctx() else { return Ok(Reply::Done) };
        // A single integer key is almost always generated, so a copy leaves it to the server.
        let auto = pk.len() == 1 && cols.iter().any(|c| c.src == pk[0] && matches!(c.kind, Kind::Int | Kind::UInt));
        let fields = cols
            .iter()
            .map(|col| {
                let v = from.as_ref().and_then(|d| d.get(&col.name)).filter(|_| !(auto && col.src == pk[0]));
                let (mode, text) = match v {
                    None => (Mode::Default, String::new()),
                    Some(Bson::Null) => (Mode::Null, String::new()),
                    Some(v) => (Mode::Value, edit_text(v, col.kind)),
                };
                RowField { col: col.clone(), mode, text }
            })
            .collect();
        let title = if from.is_some() { "Duplicate row" } else { "Insert a row" };
        self.ui_fx.push(UiFx::Open(Dialog::SqlRow(Box::new(RowForm { title: title.into(), fields, err: None }))));
        Ok(Reply::Done)
    }

    pub fn sql_insert_rows(&mut self, rows: Vec<NewRow>) {
        let Some((conn, db, sel, cols, pk)) = self.writable_sql() else { return };
        let read_only = !self.conn_writable(conn);
        self.worker.sql_insert(conn, db, sel.space().to_string(), sel.coll, cols.to_vec(), pk, rows, read_only);
    }

    /// Deletes rows by key once confirmed. Their staged edits go with them.
    pub fn sql_delete(&mut self, rows: Vec<Document>) {
        let Some((conn, db, sel, cols, pk)) = self.writable_sql() else { return };
        for r in &rows {
            let key = self.draft_key(r);
            self.drafts.remove(&key);
        }
        if let Some(t) = self.tm() {
            t.selected = None;
        }
        let read_only = !self.conn_writable(conn);
        self.worker.sql_delete_rows(conn, db, sel.space().to_string(), sel.coll, cols.to_vec(), pk, rows, read_only);
    }
}
