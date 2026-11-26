//! What the SQL views and windows ask of the server or of Core's caches while they are drawn: a schema read for
//! autocomplete, the tables of a schema, the debounced counts and live checks, and the requests whose answer fills a form
//! the app's Modal holds (`Reply::Pending` carries the request id the form waits on). None is tied to the open tab, so
//! none is refused: a request that cannot be made yet returns `Done` and is made again by the next frame.

use crate::action::{Reply, Refusal, UiFx};
use crate::core::Core;
use crate::db;
use crate::state::sql::filter_panel::Tick;
use crate::state::sql::query::{run_db, run_target};
use crate::state::sql_complete::Wants;
use crate::state::Sel;
use serde::{Deserialize, Serialize};
use std::ops::Range;

/// The pause before the live check asks, as long as the filter panel's.
const LINT_PAUSE: f64 = 0.6;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum SqlAskA {
    /// The schemas of an opened PostgreSQL database.
    AskSchemas { conn: u64, db: String },
    /// The tables of one schema, unless held or on their way.
    LoadTables { conn: u64, db: String, schema: String },
    /// The sidebar's Refresh of a PostgreSQL database.
    Refresh { conn: u64, db: String },
    /// What an input asked for while it was drawn: the schema, or a column's values.
    Serve { conn: u64, db: String, schema: String, wants: Wants },
    /// The filter panel's value cells ask once for what a column can suggest.
    ServeFacts { conn: u64, db: String, schema: String, table: String, column: String, kind: crate::db::sql::value::Kind },
    /// A dropped database: its cached schema goes with it.
    ForgetSchema { conn: u64, db: String },
    /// The tables of MySQL database `schema`, once.
    Tables { conn: u64, schema: String },
    /// The sizes the Export dialog shows beside its tables. Pending: the answer is `Msg::SqlTableSizes` on this request.
    TableSizes { conn: u64, db: String, schema: String },
    /// The columns of the table the Import dialog fills. Pending.
    TableInfo { sel: Sel },
    /// The structure of `table` in namespace `ns`, read through the connection of `sel`. Pending.
    Structure { sel: Sel, ns: String, table: String },
    /// PostgreSQL: the structure of a table of `schema` in `database`, for the New table dialog's keys. Pending.
    PgStructure { conn: u64, database: String, schema: String, table: String },
    /// The orphan check of a foreign key the Add relation dialog edits. Pending.
    Orphans { sel: Sel, schema: String, table: String, fk: db::sql::structure::ForeignKey },
    /// The filter panel's debounced counts of the open tab, drawn at `now` seconds.
    FilterTick { now: f64 },
    /// The Query view's live check of the statement under the caret, a byte range of the script, drawn at `now` seconds.
    LintTick { now: f64, stmt: Range<usize> },
    /// The cell editor's lookup of the rows foreign key `fk` can point at that match the typed text `q`; `known` is what the
    /// first answer learned about the table. Pending.
    KeySearch { fk: crate::db::sql::structure::ForeignKey, q: String, known: Option<crate::db::sql::KeyTable> },
    /// A duplicate key error's other row, found through the table's indexes.
    ShowDup { sel: Sel, index: String, value: String },
    /// The Export dialog's Export: writes `plan` to `path`; `total` is the row count the notice names.
    Dump { conn: u64, db: String, plan: crate::db::sql::dump::DumpPlan, path: String, total: u64 },
    /// The status bar's Cancel of the running import or script.
    CancelJob,
}

impl Core {
    pub(in crate::action) fn sql_ask(&mut self, a: SqlAskA) -> Result<Reply, Refusal> {
        match a {
            SqlAskA::AskSchemas { conn, db } => self.pg_ask_schemas(conn, &db),
            SqlAskA::LoadTables { conn, db, schema } => self.pg_load_tables(conn, &db, &schema),
            SqlAskA::Refresh { conn, db } => self.pg_refresh(conn, &db),
            SqlAskA::Serve { conn, db, schema, wants } => self.sql_serve(conn, &db, &schema, wants),
            SqlAskA::ServeFacts { conn, db, schema, table, column, kind } => self.sql_serve_facts(conn, &db, &schema, &table, column, kind),
            SqlAskA::ForgetSchema { conn, db } => self.forget_schema(conn, &db),
            SqlAskA::Tables { conn, schema } => {
                if self.sql_asked.insert((conn, schema.clone())) && let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) {
                    self.worker.sql_tables(conn, sql, schema);
                }
            }
            SqlAskA::TableSizes { conn, db, schema } => {
                self.req += 1;
                if let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) {
                    self.worker.sql_table_sizes(self.req, sql.at(&db), if schema.is_empty() { db.clone() } else { schema });
                }
                return Ok(Reply::Pending(self.req));
            }
            SqlAskA::TableInfo { sel } => {
                let Some((sql, ns)) = self.sql_target(&sel) else { return Ok(Reply::Done) };
                self.req += 1;
                self.worker.sql_table_info(self.req, sql, ns, sel.coll);
                return Ok(Reply::Pending(self.req));
            }
            SqlAskA::Structure { sel, ns, table } => {
                let Some(sql) = self.sql_target(&sel).map(|t| t.0) else { return Ok(Reply::Done) };
                self.req += 1;
                self.worker.sql_structure(self.req, sql, ns, table);
                return Ok(Reply::Pending(self.req));
            }
            SqlAskA::PgStructure { conn, database, schema, table } => {
                let Some(sql) = self.pg_session(conn) else { return Ok(Reply::Done) };
                self.req += 1;
                self.worker.sql_structure(self.req, sql.at(&database), schema, table);
                return Ok(Reply::Pending(self.req));
            }
            SqlAskA::Orphans { sel, schema, table, fk } => {
                let Some(sql) = self.sql_target(&sel).map(|t| t.0) else { return Ok(Reply::Done) };
                self.req += 1;
                self.worker.sql_orphans(self.req, sql, schema, table, fk);
                return Ok(Reply::Pending(self.req));
            }
            SqlAskA::FilterTick { now } => self.sql_filter_tick(now),
            SqlAskA::LintTick { now, stmt } => self.sql_lint_tick(now, stmt),
            SqlAskA::KeySearch { fk, q, known } => {
                let (Some(key), false) = (fk.ref_cols.first().cloned(), self.demo) else { return Ok(Reply::Done) };
                let Some((db, space)) = self.t().map(|t| t.sel.clone()).and_then(|s| self.sql_target(&s)) else { return Ok(Reply::Done) };
                self.req += 1;
                let schema = if fk.ref_db.is_empty() { space } else { fk.ref_db.clone() };
                self.worker.sql_key_search(self.req, db, schema, fk.ref_table.clone(), key, q, known);
                return Ok(Reply::Pending(self.req));
            }
            SqlAskA::ShowDup { sel, index, value } => {
                if self.sql_cell.is_some() {
                    self.warn("Finish or cancel the cell edit first.");
                    return Ok(Reply::Done);
                }
                let Some((db, space)) = self.sql_target(&sel) else {
                    self.warn_offline();
                    return Ok(Reply::Done);
                };
                self.req += 1;
                self.dup_wait = Some((self.req, sel.clone(), index, value));
                self.worker.sql_structure(self.req, db, space, sel.coll.clone());
            }
            SqlAskA::Dump { conn, db, plan, path, total } => {
                let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) else {
                    self.warn_offline();
                    return Ok(Reply::Done);
                };
                // A PostgreSQL connection belongs to one database, so the database picks the pool, and the schema is the namespace.
                let sql = sql.at(&db);
                self.req += 1;
                let job = crate::state::export::ExportJob { req: self.req, count: Default::default(), cancel: Default::default(), id: Default::default(), db: Some(sql.clone()), copy: false, step: Default::default(), of: 0 };
                self.worker.sql_dump(job.req, sql, plan, std::path::PathBuf::from(path.trim()), job.count.clone(), job.id.clone(), job.cancel.clone());
                self.export_job = Some(job);
                self.ok(format!("Exporting {} to {}", if total > 0 { format!("about {} rows", db::group(&total.to_string())) } else { "the structure".to_string() }, path.trim()));
            }
            SqlAskA::CancelJob => {
                if let Some(j) = &self.sql_job {
                    self.worker.sql_kill(j.db.clone(), j.id.load(std::sync::atomic::Ordering::SeqCst), j.cancel.clone());
                }
            }
        }
        Ok(Reply::Done)
    }

    /// Asks for the filter panel's counts while it is open on a live connection: the whole WHERE once it has rested,
    /// and the hovered row alone. Each slot has one request in flight; an answer to an older one is dropped.
    fn sql_filter_tick(&mut self, now: f64) {
        let Some(t) = self.t() else { return };
        if !t.sql.as_ref().is_some_and(|s| s.bar.panel.open && !s.bar.c.sql) {
            return;
        }
        let (conn, db, table) = (t.sel.conn, t.sel.db.clone(), t.sel.coll.clone());
        let schema = self.sel_schema(&t.sel);
        // The demo has no session, and a PostgreSQL tab without its schema has nothing to count in.
        let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) else { return };
        if schema.is_empty() && self.is_pg(conn) {
            return;
        }
        let (d, bs) = (self.sql_dialect(), self.sql_bs());
        let space = if schema.is_empty() { db.clone() } else { schema.clone() };
        let Some(t) = self.tabs.get_mut(self.cur) else { return };
        let text = t.filter_txt.clone();
        let Some(s) = t.sql.as_mut().filter(|s| s.bar.panel.open && !s.bar.c.sql) else { return };
        let p = &mut s.bar.panel;
        // The same text on another table, schema, database or connection is another count.
        let (c, d0, s0, t0) = &p.place;
        if (*c, d0.as_str(), s0.as_str(), t0.as_str()) != (conn, db.as_str(), schema.as_str(), table.as_str()) {
            p.forget_counts();
            p.place = (conn, db.clone(), schema.clone(), table.clone());
        }
        match p.count.tick(&text, now) {
            Tick::Ask => {
                self.req += 1;
                p.count.ask(self.req, &text);
                self.worker.sql_filter_count(self.req, sql.at(&db), space.clone(), table.clone(), text);
            }
            Tick::Wait(left) => self.ui_fx.push(UiFx::RepaintAfter(left as f64)),
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
                    self.worker.sql_filter_count(self.req, sql.at(&db), space, table, one);
                }
                Tick::Wait(left) => self.ui_fx.push(UiFx::RepaintAfter(left as f64)),
                Tick::Idle => {}
            }
        }
    }

    /// Sends the statement under the caret (`r`, a byte range of the script) to be checked once typing has rested, one
    /// request at a time per tab. Only on a live connection: the demo and a closed connection have no server to ask.
    fn sql_lint_tick(&mut self, now: f64, r: Range<usize>) {
        let ti = self.cur;
        let Some(t) = self.tabs.get(ti) else { return };
        let conn = t.sel.conn;
        let Some(s) = t.sql.as_ref() else { return };
        // A statement with {{...}} values is not SQL until Run fills it, so the server is not asked about it.
        if r.end > s.query.len() || self.conn(conn).and_then(|c| c.sql.as_ref()).is_none() || r.len() > db::sql::LINT_MAX || crate::library::snippet::is_snippet(&s.query[r.clone()], self.dialect_of(conn)) {
            return;
        }
        let target = format!("{conn}\u{1f}{}\u{1f}{}\u{1f}{}", run_db(t), s.run_schema, t.sel.schema);
        let Some(db) = self.conn(conn).and_then(|c| c.sql.clone()) else { return };
        let (db, schema) = run_target(&self.tabs[ti], db);
        let Some(s) = self.tabs[ti].sql.as_mut() else { return };
        let l = &mut s.lint;
        // An answer still in flight for the old target is dropped with its request.
        if l.target != target {
            (l.target, l.seen, l.wait, l.req) = (target, Default::default(), None, 0);
        }
        let text = &s.query[r];
        if l.req != 0 || !l.stale(text) {
            return;
        }
        let since = match &l.wait {
            Some((w, at)) if w == text => *at,
            _ => {
                l.wait = Some((text.to_string(), now));
                now
            }
        };
        if now - since < LINT_PAUSE {
            self.ui_fx.push(UiFx::RepaintAfter(LINT_PAUSE - (now - since)));
            return;
        }
        (l.wait, l.asked) = (None, text.to_string());
        self.req += 1;
        l.req = self.req;
        let stmt = l.asked.clone();
        self.worker.sql_lint(self.req, db, schema, stmt);
    }
}
