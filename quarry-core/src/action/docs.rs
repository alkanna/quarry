//! The Documents view of a MongoDB collection: the query bar, the read of a page and its answer, row selection, field
//! edits staged as drafts, saving them, insert, duplicate and delete, and the write answers. src/app/documents.rs,
//! tree.rs, value_edit.rs and modals.rs draw it.

use super::{Dialog, Refusal, Reply, UiFx};
use crate::core::Core;
use crate::db::{self, UndoAction};
use crate::core::drafts::{is_long, stage_draft};
use crate::state::config::push_history;
use crate::state::complete::Fields;
use crate::state::run::{bar_filled, find_query};
use crate::state::*;
use mongodb::bson::{doc, Bson, Document};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DocsA {
    /// The query bar's Run: Enter, the button or the stale note.
    Run { tab: u64 },
    /// A page of the results bar.
    Page { tab: u64, page: u64 },
    /// A pick from the query history menu fills one box of the bar.
    History { tab: u64, field: BarField, text: String },
    /// A shell query pasted into the bar, or any pipeline the app parsed, run on the tab.
    RunShell { tab: u64, text: String },
    RunPipeline { tab: u64, pipeline: Vec<Document> },
    /// Table or JSON.
    Layout { table: bool },
    /// A click on row `row` of the Documents rows (`agg` false) or of another view's results: Cmd checks it, Shift
    /// checks a range, a plain click opens it.
    Click { tab: u64, agg: bool, row: usize, cmd: bool, shift: bool },
    /// An arrow key on the grid: the open card moves to row `row` with the selection instead of staying on the one left.
    Step { tab: u64, agg: bool, row: usize },
    /// The query bar's autocomplete asks the server for the values of `path` once per session, in collection `key`: a
    /// sample, or with `prefix` the values starting with the text being typed.
    WantValues { key: String, path: String, prefix: Option<String> },
    /// Esc on the page: closes the innermost thing open, from a calendar or an editor to the row, the record and the
    /// explorer's filter.
    Escape,
    /// Opens or closes row `row` without the modifiers of a click, as the keyboard does.
    Select { tab: u64, agg: bool, row: usize, toggle: bool },
    CloseRow { tab: u64, agg: bool, row: usize },
    /// A row's check box: on its own, or with Shift the range from the last one.
    Check { tab: u64, row: usize, range: bool },
    /// The header's check box over the `rows` shown.
    CheckAll { tab: u64, rows: usize, on: bool },
    ClearChecks { tab: u64 },
    /// A field edit on document `doc` as shown in the tree.
    Edit { tab: u64, doc: Document, act: EditA },
    /// The large value editor's Apply, Set null or Close.
    Value { tab: u64, act: ValueA },
    /// An edit built by a dialog, such as Rename field, kept as unsaved; `new` is why it could not be built.
    Stage { tab: u64, doc: Document, new: Result<Document, String> },
    /// A field menu's Filter by this value, added to the filter or in place of it.
    FilterField { tab: u64, path: String, value: Bson, add: bool },
    /// A draft's Save: asks first when Ask before saving is on.
    Save { key: String },
    /// Writes drafts without asking: the Save and Save all confirmations.
    Write { keys: Vec<String> },
    Discard { key: String },
    /// The draft bar's Ask before saving.
    AskBeforeSave(bool),
    /// The results bar's Save all.
    SaveAll { tab: u64 },
    /// Opens the insert editor.
    Insert { tab: u64 },
    /// The insert editor's Insert, of the text it holds.
    InsertRun { tab: u64 },
    InsertClose,
    /// A row menu's or the selection bar's Duplicate, Delete and Filter.
    Duplicate { tab: u64, docs: Vec<Document> },
    Delete { tab: u64, docs: Vec<Document> },
    Filter { tab: u64, docs: Vec<Document> },
    /// The delete confirmation's Delete, on the tab showing `at`.
    DeleteRun { at: Sel, docs: Vec<Document> },
    /// A box of the query bar after a frame of typing.
    Bar { tab: u64, field: BarField, text: String },
    /// The bar's autocomplete saw a string being typed in `field`: once typing pauses it asks for the values starting with `prefix`.
    TypedValues { key: String, field: Option<String>, prefix: Option<String> },
}

/// The box of the query bar a history pick fills.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub enum BarField {
    Filter,
    Sort,
    Project,
    Limit,
    Skip,
    Hint,
    Collation,
}

/// A field edit in the document tree.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum EditA {
    /// Starts editing one value: small ones in place, objects and arrays in the large editor.
    Start { path: Vec<String>, value: Bson },
    Commit,
    /// Moves the inline edit into the large editor.
    Expand,
    Cancel,
    Remove { path: Vec<String> },
    AddField { parent: Vec<String> },
    CommitNew,
    /// The whole document in the large editor.
    EditDoc,
}

/// The large value editor's buttons.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub enum ValueA {
    Apply,
    Null,
    Close,
}

/// Positions in `new` of the documents at `rows` in `old`, matched by _id.
pub fn follow_ids(old: &[db::Row], new: &[db::Row], rows: impl Iterator<Item = usize>) -> BTreeSet<usize> {
    let ids: HashSet<String> = rows.filter_map(|i| old.get(i)?.id().map(|v| db::compact(&v))).collect();
    if ids.is_empty() {
        return BTreeSet::new();
    }
    new.iter().enumerate().filter(|(_, d)| d.id().is_some_and(|v| ids.contains(&db::compact(&v)))).map(|(i, _)| i).collect()
}

/// The large editor's text read back into the document, or why it does not read.
fn json_edit_doc(je: &JsonEdit, act: ValueA) -> db::Res<Document> {
    match act {
        ValueA::Close => unreachable!("closing the editor stages nothing"),
        ValueA::Null => db::edit_path(&je.base, &je.path, Some(Bson::Null)),
        ValueA::Apply if je.path.is_empty() => db::parse_doc(&je.text).and_then(|mut new| {
            match (je.base.get("_id"), new.get("_id")) {
                (Some(a), Some(b)) if a != b => return Err("The _id of a document cannot change. Use Duplicate to make a copy.".to_string()),
                (Some(a), None) => {
                    // Keep _id first, as MongoDB stores it.
                    let mut d = doc! { "_id": a.clone() };
                    d.extend(new);
                    new = d;
                }
                _ => {}
            }
            Ok(new)
        }),
        ValueA::Apply if je.kind == "binary" && !je.enc.is_empty() => db::decode_bytes(&je.text, je.enc).and_then(|bytes| {
            let subtype = je.subtype;
            db::edit_path(&je.base, &je.path, Some(Bson::Binary(mongodb::bson::Binary { subtype, bytes })))
        }),
        ValueA::Apply => db::make_value(je.kind, &je.text).and_then(|v| db::edit_path(&je.base, &je.path, Some(v))),
    }
}

impl Core {
    pub(super) fn docs(&mut self, a: DocsA) -> Result<Reply, Refusal> {
        match a {
            DocsA::Run { tab } => {
                self.cur_tab(tab)?;
                self.run_query();
            }
            DocsA::Page { tab, page } => {
                self.cur_tab(tab)?;
                if let Some(t) = self.tm() {
                    t.page = page;
                }
                self.reload();
            }
            DocsA::History { tab, field, text } => {
                self.cur_tab(tab)?;
                let t = &mut self.tabs[self.cur];
                set_bar(t, field, text);
            }
            DocsA::Bar { tab, field, text } => {
                self.cur_tab(tab)?;
                let t = &mut self.tabs[self.cur];
                // A filter, sort or projection the Cmd+K form filled reads its braces as placeholders again once edited.
                t.snip.off = t.snip.off && !matches!(field, BarField::Filter | BarField::Sort | BarField::Project);
                set_bar(t, field, text);
            }
            DocsA::TypedValues { key, field, prefix } => {
                let due = std::time::Instant::now() + std::time::Duration::from_millis(400);
                self.values_typed = field.zip(prefix).filter(|(_, p)| p.chars().count() >= 2).map(|(f, p)| (due, key, f, p));
            }
            DocsA::Layout { table } => {
                if table != self.prefs.table {
                    self.prefs.table = table;
                    self.save_prefs();
                }
            }
            DocsA::Click { tab, agg, row, cmd, shift } => {
                self.cur_tab(tab)?;
                self.row_click(agg, row, cmd, shift);
            }
            DocsA::Select { tab, agg, row, toggle } => {
                self.cur_tab(tab)?;
                self.select(agg, row, toggle);
            }
            DocsA::Step { tab, agg, row } => {
                self.cur_tab(tab)?;
                if let Some(prev) = self.t().and_then(|t| t.selected) {
                    self.tm().unwrap().expanded.remove(&prev);
                }
                self.select(agg, row, false);
            }
            DocsA::RunShell { tab, text } => {
                self.cur_tab(tab)?;
                self.run_shell(&text);
            }
            DocsA::RunPipeline { tab, pipeline } => {
                self.cur_tab(tab)?;
                self.run_pipeline(pipeline);
            }
            DocsA::Escape => self.escape(),
            DocsA::WantValues { key, path, prefix } => self.want_values(key, path, prefix),
            DocsA::CloseRow { tab, agg, row } => {
                self.cur_tab(tab)?;
                self.close_row((agg, row));
            }
            DocsA::Check { tab, row, range } => {
                self.cur_tab(tab)?;
                self.toggle_check(row, range);
            }
            DocsA::CheckAll { tab, rows, on } => {
                self.cur_tab(tab)?;
                *self.checks_mut().0 = if on { (0..rows).collect() } else { BTreeSet::new() };
            }
            DocsA::ClearChecks { tab } => {
                self.cur_tab(tab)?;
                self.checks_mut().0.clear();
            }
            DocsA::Stage { tab, doc, new } => {
                self.cur_tab(tab)?;
                self.stage(&doc, new);
            }
            DocsA::FilterField { tab, path, value, add } => {
                self.cur_tab(tab)?;
                let path = db::strip_indices(&path);
                let t = self.tm().unwrap();
                let mut f = if add { t.query.filter.clone() } else { Document::new() };
                f.insert(path, value);
                t.filter_txt = db::shell_text(&Bson::Document(f));
                t.view = View::Documents;
                t.selected = None;
                self.reset_doc_state();
                self.run_query();
            }
            DocsA::Edit { tab, doc, act } => {
                self.cur_tab(tab)?;
                return self.edit(&doc, act);
            }
            DocsA::Value { tab, act } => {
                self.cur_tab(tab)?;
                self.json_edit_act(act);
            }
            DocsA::Save { key } => return self.save_draft(&key),
            DocsA::Write { keys } => {
                for k in &keys {
                    self.write_draft(k);
                }
            }
            DocsA::Discard { key } => {
                self.drafts.remove(&key);
                self.ok("Edits discarded");
            }
            DocsA::AskBeforeSave(on) => {
                if on != self.prefs.confirm_save {
                    self.prefs.confirm_save = on;
                    self.save_prefs();
                }
            }
            DocsA::SaveAll { tab } => {
                self.cur_tab(tab)?;
                let pending = self.tab_drafts();
                if pending.is_empty() {
                    return Ok(Reply::Done);
                }
                if !self.sel_writable() {
                    let conn = self.tabs[self.cur].sel.conn;
                    return Err(self.refuse_ro_quiet(conn));
                }
                if self.prefs.confirm_save && !self.prefs.always_writable {
                    self.ui_fx.push(UiFx::Open(Dialog::SaveAll(pending)));
                } else {
                    for k in &pending {
                        self.write_draft(k);
                    }
                }
            }
            DocsA::Insert { tab } => {
                self.cur_tab(tab)?;
                if !self.sel_writable() {
                    let conn = self.tabs[self.cur].sel.conn;
                    return Err(self.refuse_ro_quiet(conn));
                }
                self.insert = Some("{\n  \n}".into());
            }
            DocsA::InsertRun { tab } => {
                self.cur_tab(tab)?;
                let Some(text) = &self.insert else { return Ok(Reply::Done) };
                match db::parse_doc(text) {
                    Ok(d) => {
                        let (conn, target) = self.write_target()?;
                        let text = self.insert.take();
                        self.worker.insert(conn, target, d, text);
                    }
                    Err(e) => self.err(e),
                }
            }
            DocsA::InsertClose => self.insert = None,
            DocsA::Duplicate { tab, docs } => {
                self.cur_tab(tab)?;
                self.writable_docs_act(&docs)?;
                let copies: Vec<Document> = docs.into_iter().map(|mut d| {
                    d.remove("_id");
                    d
                }).collect();
                // One copy opens the insert editor to tweak it first; many are written straight away.
                if let [c] = copies.as_slice() {
                    self.insert = Some(db::to_edit_json(c));
                } else {
                    let (conn, target) = self.write_target()?;
                    self.worker.import(conn, target, copies, None);
                }
            }
            DocsA::Delete { tab, docs } => {
                self.cur_tab(tab)?;
                self.writable_docs_act(&docs)?;
                if let Some(sel) = self.t().map(|t| t.sel.clone()) {
                    self.ui_fx.push(UiFx::Open(Dialog::Delete(sel, docs)));
                }
            }
            DocsA::Filter { tab, docs } => {
                self.cur_tab(tab)?;
                if docs.is_empty() {
                    return Ok(Reply::Done);
                }
                let mut ids: Vec<Bson> = docs.iter().filter_map(|d| d.get("_id").cloned()).collect();
                let by_id = Bson::Document(if ids.len() == 1 { doc! { "_id": ids.remove(0) } } else { doc! { "_id": { "$in": ids } } });
                self.tabs[self.cur].filter_txt = db::shell_text(&by_id);
                self.run_query();
            }
            DocsA::DeleteRun { at, docs } => {
                // Run on the tab the rows were picked in: tabs can close or move while the dialog is open.
                let Some(i) = super::tabs::tab_for(self.tabs.iter().map(|t| &t.sel), self.cur, &at) else {
                    return Err(self.refuse(format!("Nothing ran: {} is no longer open in a tab.", at.coll)));
                };
                let back = self.cur;
                self.cur = i;
                let sent = self.write_target();
                if let Ok((conn, target)) = &sent {
                    match docs.as_slice() {
                        [d] => self.worker.delete(*conn, target.clone(), d.clone(), true),
                        _ => self.worker.delete_docs(*conn, target.clone(), docs.clone()),
                    }
                    // The reload drops deleted rows from the checks; others stay checked.
                    if let Some(t) = self.tm() {
                        t.selected = None;
                    }
                }
                if back < self.tabs.len() {
                    self.cur = back;
                }
                sent?;
            }
        }
        Ok(Reply::Done)
    }

    /// `writable_target` as a refusal, with the same toasts: on a read-only connection the app offers Unlock and runs
    /// the action again after it.
    pub fn write_target(&mut self) -> Result<(u64, db::Target), Refusal> {
        let Some(conn) = self.t().map(|t| t.sel.conn) else { return Err(Refusal { kind: super::RefusalKind::Invalid, msg: String::new() }) };
        if !self.sel_writable() {
            return Err(self.refuse_ro(conn, None));
        }
        match self.target() {
            Some(t) => Ok((conn, t)),
            None => Err(self.refuse_offline(conn)),
        }
    }

    /// Duplicate and Delete are asked before their dialog opens, like an inline edit, rather than refused after the
    /// user confirmed them. Rows read with a projection are refused first.
    fn writable_docs_act(&mut self, docs: &[Document]) -> Result<(), Refusal> {
        if docs.is_empty() {
            return Err(Refusal { kind: super::RefusalKind::Invalid, msg: String::new() });
        }
        if self.projected() {
            return Err(Refusal { kind: super::RefusalKind::Invalid, msg: self.toasts.last().map(|t| t.title.clone()).unwrap_or_default() });
        }
        if !self.sel_writable() {
            let conn = self.tabs[self.cur].sel.conn;
            return Err(self.refuse_ro_quiet(conn));
        }
        Ok(())
    }

    pub fn run_query(&mut self) {
        let Some(t) = self.t() else { return };
        if db::is_shell(&t.filter_txt) {
            return match bar_filled(t, &t.filter_txt, "Filter") {
                Ok(text) => self.run_shell(&text),
                Err(e) => self.tm().unwrap().bar_err = Some(e),
            };
        }
        let q = find_query(t);
        match q {
            Ok(q) => {
                let (f, so, p) = (t.filter_txt.clone(), t.sort_txt.clone(), t.proj_txt.clone());
                let key = self.col_key(false);
                let h = self.prefs.history.entry(key).or_default();
                push_history(&mut h.selector, &f);
                push_history(&mut h.sort, &so);
                push_history(&mut h.project, &p);
                self.save_prefs();
                self.tm().unwrap().apply_find(q);
                self.reload();
            }
            // Nothing ran, so the message goes under the bar and the rows stay.
            Err(e) => self.tm().unwrap().bar_err = Some(e),
        }
    }

    /// Runs a pasted `db.<coll>.find(...)` or `db.<coll>.aggregate([...])`, switching collection if needed.
    pub fn run_shell(&mut self, text: &str) {
        let sh = match db::parse_shell(text) {
            Ok(s) => s,
            Err(e) => {
                match self.tm().filter(|t| t.view == View::Documents) {
                    Some(t) => t.bar_err = Some(e),
                    None => self.err(e),
                }
                return;
            }
        };
        let Some(cur) = self.t().map(|t| t.sel.clone()) else { return self.warn("Open a collection first.") };
        if let Some(t) = self.tm() {
            t.bar_err = None;
        }
        if sh.coll != cur.coll {
            // The pasted text belongs to the other collection, so this tab's bar goes back to what it last ran.
            if let Some(t) = self.tm() {
                t.filter_txt = t.ran_txt[0].clone();
            }
            // Open::Link never takes a tab that holds work, so another collection's own query is not overwritten.
            self.open_collection(Sel { coll: sh.coll.clone(), ..cur }, Open::Link);
        }
        let t = self.tm().unwrap();
        t.selected = None;
        match sh.kind {
            db::ShellKind::Find(q) => {
                t.view = View::Documents;
                t.filter_txt = text.trim().to_string();
                t.clear_options();
                t.apply_find(q);
                self.reload();
            }
            db::ShellKind::Aggregate(p) => {
                t.view = View::Aggregate;
                t.agg_txt = text.trim().to_string();
                self.run_pipeline(p);
            }
        }
    }

    pub fn run_pipeline(&mut self, p: Vec<Document>) {
        self.tm().unwrap().agg_stage = None;
        if db::has_write_stage(&p) {
            if self.writable_target().is_some() {
                self.ui_fx.push(UiFx::Open(Dialog::AggWrite(p)));
            }
        } else if self.demo {
            self.ok("Offline demo: nothing to run.");
        } else {
            match self.target() {
                Some(target) => {
                    self.req += 1;
                    let req = self.req;
                    self.tm().unwrap().agg_req = req;
                    self.worker.aggregate(req, target, p);
                }
                None => self.warn_offline(),
            }
        }
    }

    /// Asks for the preview of every relation of the tab that read request `req`.
    /// Typing in a string asks for the values starting with it, once typing pauses. Run each frame.
    pub(crate) fn values_typed_tick(&mut self) {
        let now = std::time::Instant::now();
        if let Some((_, k, f, p)) = self.values_typed.take_if(|(due, ..)| *due <= now) {
            self.want_values(k, f, Some(p));
            self.ui_fx.push(super::UiFx::RepaintAfter(0.0));
        } else if let Some((due, ..)) = &self.values_typed {
            self.ui_fx.push(super::UiFx::RepaintAfter((*due - now).as_secs_f64()));
        }
    }

    /// Closed rows of the Documents view drop their decoded copy, so only the ones that are open keep it (see `db::Row`).
    pub(crate) fn forget_closed_rows(&mut self) {
        let Some(t) = self.tabs.get_mut(self.cur) else { return };
        let (sel, open) = (t.selected, &t.expanded);
        for (i, r) in t.docs.iter_mut().enumerate() {
            if sel != Some((false, i)) && !open.contains(&(false, i)) {
                r.forget();
            }
        }
    }

    pub fn load_related(&mut self, req: u64) {
        let Some(t) = self.tabs.iter().find(|t| t.req == req) else { return };
        let Some(client) = self.conn(t.sel.conn).and_then(|c| c.client.clone()) else { return };
        let rels = self.prefs.relations.get(&format!("{}/{}", t.sel.conn, t.sel.db)).into_iter().flatten().filter(|r| r.coll == t.sel.coll && !r.preview.is_empty());
        for r in rels {
            let ids = db::values_at(t.docs.iter().map(db::Row::raw), &r.path);
            if !ids.is_empty() {
                self.worker.related(req, db::Target { client: client.clone(), db: t.sel.db.clone(), coll: r.target.clone(), timeout: self.timeout() }, ids, r.preview.clone());
            }
        }
    }

    /// The answer to a page read. `info` builds the field paths and suggestions of a page, which the app's autocomplete
    /// owns: the rows, then the values fetched for the collection before.
    pub fn docs_loaded(&mut self, req: u64, res: db::Res<(Vec<db::Row>, u64, u128)>, approx: bool, info: &dyn Fn(&[db::Row], Option<&HashMap<String, Vec<Bson>>>) -> (Vec<String>, Fields)) {
        let Some(i) = self.tabs.iter().position(|t| t.req == req) else { return };
        let t = &mut self.tabs[i];
        t.rows_replaced();
        t.loading = false;
        t.count_approx = approx;
        match res {
            Ok((docs, count, ms)) => {
                // Deleting the last page's rows leaves the tab past the end; step back to the new last page.
                let last = count.saturating_sub(1) / self.page_size();
                if docs.is_empty() && self.tabs[i].page > last {
                    self.tabs[i].page = last;
                    return self.reload_of(i);
                }
                let Core { tabs, values, .. } = self;
                let t = &mut tabs[i];
                // A query that works again clears an earlier error.
                t.clear_failed(View::Documents);
                // SQL columns keep the table's order and never include the hidden _id.
                t.fields = match &t.sql {
                    Some(s) => s.cols.iter().map(|c| c.name.clone()).collect(),
                    None => auto_fields(&docs),
                };
                let (paths, f) = info(&docs, values.get(&t.coll_key()));
                t.paths = paths;
                t.field_info = Arc::new(f);
                // Checks and open rows follow their documents by _id, so actions that reload
                // (duplicate, filter, edits) keep them; rows that left the results drop out.
                if !t.checked.is_empty() {
                    t.checked = follow_ids(&t.docs, &docs, t.checked.iter().copied());
                    t.check_anchor = None;
                }
                // The open document follows its _id too; rows without one keep their position.
                if let Some((false, i)) = t.selected && t.docs.get(i).is_some_and(|d| d.id().is_some()) {
                    t.selected = follow_ids(&t.docs, &docs, std::iter::once(i)).first().map(|&j| (false, j));
                }
                let open = follow_ids(&t.docs, &docs, t.expanded.iter().filter(|e| !e.0).map(|e| e.1));
                t.expanded.retain(|e| e.0);
                t.expanded.extend(open.into_iter().map(|i| (false, i)));
                t.docs = docs;
                t.count = count;
                t.ms = ms;
                if matches!(t.selected, Some((false, i)) if i >= t.docs.len()) {
                    t.selected = None;
                }
                t.related.clear();
                let ask = (!t.stats_asked && t.sql.is_none()).then(|| t.sel.clone());
                t.stats_asked = true;
                self.sql_remember_filter(i);
                self.load_related(req);
                if let Some(s) = ask && let Some(client) = self.conn(s.conn).and_then(|c| c.client.clone()) {
                    let tag = format!("tabstats\u{1f}{}\u{1f}{}\u{1f}{}", s.conn, s.db, s.coll);
                    self.worker.coll_stats(client, s.db, s.coll, tag);
                }
            }
            Err(e) => {
                // Old rows under the new query would read as its results. Selection and checks are indexes
                // into those rows, so they go too, or the next read would open whatever lands at that index.
                t.docs.clear();
                t.selected = t.selected.filter(|s| s.0);
                t.checked.clear();
                t.expanded.retain(|e| e.0);
                let what = if t.sql.is_some() { "load the rows" } else { "load the documents" };
                self.fail_view(i, View::Documents, e, what);
            }
        }
    }

    /// The answer to a write that is not a draft save: insert, duplicate, delete, import, and the like.
    pub fn written(&mut self, at: (String, String), res: db::Res<String>, undo: Option<UndoAction>, reload: bool, retry: Option<(String, String, String)>) {
        match res {
            Ok(s) => {
                // A write that ran for another tab must not close an editor being typed in this one.
                if self.t().is_some_and(|t| t.sel.space() == at.0 && t.sel.coll == at.1) {
                    self.reset_doc_state();
                }
                self.ok(s);
            }
            Err(e) => {
                // Reopen the refused insert so the typed document is not lost, but
                // only over the collection it was meant for.
                if let Some((db, coll, text)) = retry && self.t().is_some_and(|t| t.sel.space() == db && t.sel.coll == coll) {
                    self.insert = Some(text);
                }
                self.err(e);
            }
        }
        // An agent's change never joins the user's list; its undo goes back to its Activity entry.
        match undo {
            Some(u) if u.agent.is_some() => {
                let id = u.agent.unwrap_or_default();
                self.agent_keep_undo(id, db::AgentUndo::Sql(u));
            }
            Some(u) => self.push_undo(u),
            None => {}
        }
        // The worker only knows the database and collection, so a same-named
        // collection on another connection may reload too, which is harmless.
        let tabs: Vec<usize> = (0..self.tabs.len()).filter(|&i| self.tabs[i].sel.space() == at.0 && self.tabs[i].sel.coll == at.1).collect();
        for i in tabs {
            if reload {
                self.reload_of(i);
            } else {
                self.load_indexes_of(i);
            }
        }
    }

    /// The answer to a draft save, named by its draft key.
    pub fn saved(&mut self, tag: String, res: db::Res<String>, undo: Option<UndoAction>) {
        let dr = self.saving.remove(&tag);
        let sel = dr.as_ref().map(|d| Sel { conn: d.conn, db: d.db.clone(), schema: d.schema.clone(), coll: d.coll.clone() });
        match res {
            Ok(s) => {
                self.toast_ok(s.clone(), undo.as_ref().map(|u| u.at));
                self.ok(s);
            }
            Err(e) => {
                // Keep the edits so nothing typed is lost; the person can reload and redo. An edit staged on the
                // same row while this save was in flight is newer, so it wins over the failed one.
                if let Some(dr) = dr {
                    self.drafts.entry(tag).or_insert(dr);
                }
                self.err(e);
            }
        }
        if let Some(u) = undo {
            self.push_undo(u);
        }
        // Waits for the collection's other saves so the reload shows all of them.
        if let Some(sel) = sel.filter(|s| !self.saving.values().any(|d| d.conn == s.conn && d.db == s.db && d.schema == s.schema && d.coll == s.coll)) {
            let tabs: Vec<usize> = (0..self.tabs.len()).filter(|&i| self.tabs[i].sel == sel).collect();
            for i in tabs {
                self.reload_of(i);
            }
        }
    }

    /// An agent's undo joins its Activity entry rather than Recent changes; older ones drop off past the size limit.
    pub fn agent_keep_undo(&mut self, id: u64, u: db::AgentUndo) {
        let bytes = u.bytes();
        let gone = self.agents.undo.push(id, bytes, u, self.agents.cfg.limits.undo_total_bytes);
        if let Some(a) = self.agents.activity.iter_mut().find(|a| a.id == id) {
            a.undo = Some(id);
        }
        for g in gone {
            if let Some(a) = self.agents.activity.iter_mut().find(|a| a.id == g) {
                a.undo = None;
                a.summary = format!("{} · undo expired", a.summary);
            }
        }
    }

    /// Row click: Cmd toggles the row's check, Shift checks a range, a plain click opens it.
    pub fn row_click(&mut self, agg: bool, i: usize, cmd: bool, shift: bool) {
        if (!agg || self.t().is_some_and(|t| t.view == View::Shell)) && (cmd || shift) {
            self.toggle_check(i, shift);
        } else if self.prefs.open_inline {
            // Rows open and close on their own; the last one opened takes edits and keys.
            let t = self.tm().unwrap();
            let key = (agg, i);
            if t.expanded.remove(&key) || t.selected == Some(key) {
                self.close_row(key);
            } else {
                t.expanded.insert(key);
                self.select(agg, i, false);
            }
        } else {
            self.select(agg, i, true);
        }
    }

    /// Esc closes the innermost thing that is open. Dialogs and menus handle their own Esc.
    fn escape(&mut self) {
        // The calendar of a date closes before the editor under it.
        if let Some(i) = &mut self.inline && let Some(o) = i.pick.take() {
            i.text = o;
            return;
        }
        if self.inline.take().is_some() || self.new_field.take().is_some() {
            return;
        }
        if let Some(t) = self.tm() {
            // A SQL row's record closes before the row is let go.
            if t.record && t.record_row() {
                t.record = false;
                return;
            }
            if t.view == View::Sql && let Some(s) = &mut t.sql && s.qsel.take().is_some() {
                return;
            }
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

    pub fn close_row(&mut self, key: (bool, usize)) {
        let t = self.tm().unwrap();
        t.expanded.remove(&key);
        if t.selected == Some(key) {
            t.selected = None;
            self.reset_doc_state();
        }
    }

    /// The checked rows of the grid the current view shows: Content, a Query result or the Shell's result.
    pub fn checks(&self) -> &BTreeSet<usize> {
        let t = self.t().unwrap();
        match (t.view, t.sql.as_ref()) {
            (View::Shell, _) => &t.shell_checked,
            (View::Sql, Some(s)) => &s.qchecked,
            _ => &t.checked,
        }
    }

    pub fn checks_mut(&mut self) -> (&mut BTreeSet<usize>, &mut Option<usize>) {
        let t = self.tm().unwrap();
        match (t.view, t.sql.as_mut()) {
            (View::Shell, _) => (&mut t.shell_checked, &mut t.check_anchor),
            (View::Sql, Some(s)) => (&mut s.qchecked, &mut s.qanchor),
            _ => (&mut t.checked, &mut t.check_anchor),
        }
    }

    pub fn toggle_check(&mut self, i: usize, range: bool) {
        let (checked, anchor) = self.checks_mut();
        match anchor.filter(|_| range) {
            Some(a) => checked.extend(a.min(i)..=a.max(i)),
            None => {
                if !checked.insert(i) {
                    checked.remove(&i);
                }
            }
        }
        *anchor = Some(i);
    }

    pub fn select(&mut self, agg: bool, i: usize, toggle: bool) {
        let t = self.tm().unwrap();
        let same = t.selected == Some((agg, i));
        t.selected = if same && toggle { None } else { Some((agg, i)) };
        if !same || toggle {
            self.reset_doc_state();
        }
    }

    /// Keeps an edit as unsaved instead of writing it, so several edits go out in one save.
    pub fn stage(&mut self, d: &Document, new: db::Res<Document>) {
        // Refused before the editors close, so the typed edit is still there once Project is cleared.
        if self.projected() {
            return;
        }
        let new = match new {
            Ok(n) => n,
            Err(e) => return self.err(e),
        };
        let key = self.draft_key(d);
        let Some(sel) = self.t().map(|t| t.sel.clone()) else { return };
        self.inline = None;
        self.new_field = None;
        self.json_edit = None;
        let n = stage_draft(&mut self.drafts, &key, &sel, d, new);
        if n == 0 {
            return self.ok("No changes left on this document");
        }
        if self.prefs.auto_save && self.sel_writable() {
            self.write_draft(&key);
        } else {
            self.ok(format!("{n} unsaved change(s). Press Save on the document when ready."));
        }
    }

    /// Rows read with a projection hold only some fields: saving one fails its unchanged check, a copy holds only those
    /// fields, and rows the projection left without _id would share one draft.
    pub fn projected(&mut self) -> bool {
        let p = self.t().is_some_and(|t| t.sql.is_none() && t.query.projection.as_ref().is_some_and(|p| !p.is_empty()));
        if p {
            self.warn("These rows hold only the projected fields. Clear Project and run the query again to change documents.");
        }
        p
    }

    pub fn save_draft(&mut self, key: &str) -> Result<Reply, Refusal> {
        let Some(dr) = self.drafts.get(key) else { return Ok(Reply::Done) };
        if !self.conn(dr.conn).is_some_and(|c| c.writable) {
            let conn = dr.conn;
            return Err(self.refuse_ro_quiet(conn));
        }
        if self.prefs.confirm_save && !self.prefs.always_writable {
            let changes = db::diff(&dr.original, &dr.current);
            self.ui_fx.push(UiFx::Open(Dialog::Save { tag: key.to_string(), changes }));
        } else {
            self.write_draft(key);
        }
        Ok(Reply::Done)
    }

    pub fn write_draft(&mut self, key: &str) {
        if self.drafts.get(key).is_some_and(|d| self.conn(d.conn).is_some_and(|c| c.sql.is_some())) {
            return self.write_sql_drafts(vec![key.to_string()]);
        }
        let Some(dr) = self.drafts.remove(key) else { return };
        let Some(target) = self.target_for(&Sel { conn: dr.conn, db: dr.db.clone(), schema: dr.schema.clone(), coll: dr.coll.clone() }) else {
            self.drafts.insert(key.to_string(), dr);
            return self.warn_offline();
        };
        self.worker.save(key.to_string(), dr.conn, target, dr.original.clone(), dr.current.clone());
        self.saving.insert(key.to_string(), dr);
    }

    /// The large editor's Apply, Set null or Close, on the editor the app put back after drawing it.
    fn json_edit_act(&mut self, act: ValueA) {
        let Some(je) = self.json_edit.take() else { return };
        if act == ValueA::Close {
            return;
        }
        match json_edit_doc(&je, act) {
            Ok(n) => {
                let base = je.base.clone();
                self.stage(&base, Ok(n));
            }
            Err(e) => {
                self.err(e);
                self.json_edit = Some(je);
            }
        }
    }

    /// A field edit in the tree of document `d`. Edits that open an editor ask to unlock first, with no toast.
    fn edit(&mut self, d: &Document, act: EditA) -> Result<Reply, Refusal> {
        let locked = matches!(act, EditA::Start { .. } | EditA::Remove { .. } | EditA::AddField { .. } | EditA::EditDoc) && !self.sel_writable();
        if locked {
            let conn = self.tabs[self.cur].sel.conn;
            return Err(self.refuse_ro_quiet(conn));
        }
        match act {
            EditA::Start { path, value } => self.start_inline(d, path, value),
            EditA::Expand => {
                if let Some(i) = self.inline.take() {
                    // Text typed inline counts as an edit the side panel must keep.
                    let dirty = db::get_path(d, &i.path.join(".")).is_none_or(|v| db::inline_text(v) != i.text);
                    self.json_edit = Some(JsonEdit { dirty, ..JsonEdit::new(d.clone(), i.path, i.text, i.kind) });
                }
            }
            EditA::Cancel => {
                self.inline = None;
                self.new_field = None;
            }
            EditA::Commit => {
                if let Some(i) = &self.inline {
                    let new = db::make_value(i.kind, &i.text).and_then(|v| db::edit_path(d, &i.path, Some(v)));
                    self.stage(d, new);
                }
            }
            EditA::Remove { path } => self.stage(d, db::edit_path(d, &path, None)),
            EditA::AddField { parent } => {
                self.inline = None;
                self.new_field = Some(NewField { parent, name: String::new(), text: String::new(), kind: "string", err: None });
            }
            EditA::CommitNew => {
                if let Some(n) = &mut self.new_field {
                    let is_array = !n.parent.is_empty() && matches!(db::get_path(d, &n.parent.join(".")), Some(Bson::Array(_)));
                    let name = n.name.trim().to_string();
                    if name.is_empty() && !is_array {
                        n.err = Some("Give the new field a name.".into());
                        return Ok(Reply::Done);
                    }
                    let mut path = n.parent.clone();
                    path.push(if is_array { usize::MAX.to_string() } else { name.clone() });
                    if !is_array && db::get_path(d, &path.join(".")).is_some() {
                        n.err = Some(format!("{name} already exists. Double-click it in the tree to change it."));
                        return Ok(Reply::Done);
                    }
                    let new = db::make_value(n.kind, &n.text).and_then(|v| db::edit_path(d, &path, Some(v)));
                    self.stage(d, new);
                }
            }
            EditA::EditDoc => {
                self.inline = None;
                self.json_edit = Some(JsonEdit::new(d.clone(), Vec::new(), db::to_edit_json(d), "object"));
            }
        }
        Ok(Reply::Done)
    }

    /// Starts editing one value: small values in place, objects and arrays in the large JSON editor.
    fn start_inline(&mut self, d: &Document, path: Vec<String>, v: Bson) {
        if self.panel_dirty() {
            return self.warn("Apply or cancel the value in the side panel first.");
        }
        self.new_field = None;
        let kind = db::type_name_kind(&v);
        if let ("binary", Bson::Binary(bin)) = (kind, &v) {
            let enc = db::default_encoding(&bin.bytes);
            self.inline = None;
            let text = db::encode_bytes(bin, enc).unwrap_or_default();
            self.json_edit = Some(JsonEdit { enc, subtype: bin.subtype, ..JsonEdit::new(d.clone(), path, text, kind) });
            return;
        }
        let container = matches!(v, Bson::Document(_) | Bson::Array(_));
        let text = if container { db::to_edit_value(&v) } else { db::inline_text(&v) };
        // With the panel open, a double-click loads the new field into it, so the panel follows the selection.
        if container || is_long(&text) || self.value_in_panel() {
            self.inline = None;
            self.json_edit = Some(JsonEdit::new(d.clone(), path, text, kind));
        } else {
            self.inline = Some(Inline { text, kind, path, focus: true, pick: None, local: false });
        }
    }
}

fn set_bar(t: &mut Tab, field: BarField, text: String) {
    match field {
        BarField::Filter => t.filter_txt = text,
        BarField::Sort => t.sort_txt = text,
        BarField::Project => t.proj_txt = text,
        BarField::Limit => t.limit_txt = text,
        BarField::Skip => t.skip_txt = text,
        BarField::Hint => t.hint_txt = text,
        BarField::Collation => t.collation_txt = text,
    }
}
