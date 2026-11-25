//! Functions and procedures of a SQL database: the sidebar's ROUTINES group and the window that reads, edits, runs or drops
//! one. Drawn by src/app/sql/routines.rs; the statements and their checks are in `db::sql::routines`.

use super::{Refusal, Reply};
use crate::core::Core;
use crate::db::sql::routines::{self as rt, Arg, Kind, Routine};
use crate::state::sql::routines::*;
use crate::state::{ns_key, Tab, View};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum RoutineA {
    /// Reads the list of a (database, schema).
    Load { conn: u64, db: String, schema: String },
    /// Reads it again after a failure.
    Reload { conn: u64, db: String, schema: String },
    /// Folds or unfolds the group of namespace `ns`.
    Fold { conn: u64, ns: String },
    /// Opens the window on a routine of the list, on one of its panes.
    Open { conn: u64, db: String, schema: String, routine: Routine, pane: Pane },
    /// Opens the window on a new routine, from a template.
    New { conn: u64, db: String, schema: String, kind: Kind },
    /// The window's tabs: view, run or drop.
    Pane(Pane),
    /// The window's Save; `restore` is Undo, which writes back the text from before the last save.
    Save { restore: bool },
    Drop,
    /// Runs the call in a new Query tab.
    Run,
}

impl Core {
    pub(super) fn routine_act(&mut self, a: RoutineA) -> Result<Reply, Refusal> {
        match a {
            RoutineA::Load { conn, db, schema } => self.routines_load(conn, db, schema),
            RoutineA::Reload { conn, db, schema } => {
                let key = self.routines_key(conn, &db, &schema);
                self.routines.lists.remove(&key);
                self.routines_load(conn, db, schema);
            }
            RoutineA::Fold { conn, ns } => {
                let key = (conn, ns);
                if !self.routines.folded.remove(&key) {
                    self.routines.folded.insert(key);
                }
            }
            RoutineA::Open { conn, db, schema, routine, pane } => self.routine_open(conn, db, schema, Some(routine), pane),
            RoutineA::New { conn, db, schema, kind } => self.routine_new(conn, db, schema, kind),
            RoutineA::Pane(p) => {
                if let Some(w) = self.routines.win.as_mut() {
                    w.pane = p;
                    w.typed.clear();
                }
            }
            RoutineA::Save { restore } => return self.routine_save(restore),
            RoutineA::Drop => return self.routine_drop(),
            RoutineA::Run => return self.routine_run(),
        }
        Ok(Reply::Done)
    }

    pub fn routines_key(&self, conn: u64, db: &str, schema: &str) -> (u64, String) {
        (conn, ns_key(db, if self.is_pg(conn) { schema } else { "" }))
    }

    fn routines_load(&mut self, conn: u64, db: String, schema: String) {
        let key = self.routines_key(conn, &db, &schema);
        // A connection without a session (the offline demo) has no routines to read; an empty list ends the asking.
        let Some(sql) = self.live_sql(conn) else {
            self.routines.lists.entry(key).or_insert(Loaded::Ok(Vec::new()));
            return;
        };
        self.routines.lists.insert(key, Loaded::Wait);
        self.worker.sql_routines(conn, sql.at(&db), db, schema);
    }

    pub fn routine_window_for(&self, conn: u64, db: String, schema: String, kind: Kind, routine: Option<Routine>) -> RoutineWin {
        let dialect = self.dialect_of(conn);
        // A namespace that belongs to the server: the schema on PostgreSQL, the database on MySQL.
        let system = if dialect == crate::db::sql::Dialect::Postgres { crate::db::sql::pg::create::system_schema(&schema) } else { crate::db::sql::create::SYSTEM_SCHEMAS.contains(&db.as_str()) };
        RoutineWin { conn, db, schema, dialect, system, kind, routine, detail: None, req: 0, busy: false, error: None, text: String::new(), base: String::new(), editing: false, recreate: false, offer_recreate: false, undo: None, pane: Pane::View, typed: String::new(), args: Vec::new(), memo: None }
    }

    fn routine_open(&mut self, conn: u64, db: String, schema: String, r: Option<Routine>, pane: Pane) {
        let Some(r) = r else { return };
        let mut w = self.routine_window_for(conn, db.clone(), schema.clone(), r.kind, Some(r.clone()));
        w.pane = pane;
        if let Some(sql) = self.live_sql(conn) {
            self.req += 1;
            w.req = self.req;
            self.worker.sql_routine_detail(conn, self.req, sql.at(&db), schema, r);
        }
        self.routines.win = Some(w);
    }

    fn routine_new(&mut self, conn: u64, db: String, schema: String, kind: Kind) {
        let mut w = self.routine_window_for(conn, db, schema.clone(), kind, None);
        w.text = rt::template(w.dialect, kind, &schema);
        w.editing = true;
        self.routines.win = Some(w);
    }

    /// An answer of the worker. A read or a change answers the window that asked, by its request.
    pub fn routine_done(&mut self, conn: u64, req: u64, ans: rt::Answer) {
        match ans {
            rt::Answer::List { database, schema, res } => {
                let key = self.routines_key(conn, &database, &schema);
                // A list asked for before a Refresh forgot it is not put back.
                if !matches!(self.routines.lists.get(&key), Some(Loaded::Wait)) {
                    return;
                }
                self.routines.lists.insert(key, match res {
                    Ok(l) => Loaded::Ok(l),
                    Err(e) => Loaded::Err(e),
                });
            }
            rt::Answer::Detail { res } => {
                let Some(w) = self.routines.win.as_mut().filter(|w| w.req == req) else { return };
                w.req = 0;
                match res {
                    Ok(d) => {
                        // Typing in the editor is kept when the read arrives after it started.
                        let typing = w.editing && w.dirty();
                        w.base = d.source.clone();
                        if !typing {
                            w.text = d.source.clone();
                        }
                        w.args = rt::inputs(&d).iter().map(|_| Arg::default()).collect();
                        w.detail = Some(*d);
                        w.error = None;
                    }
                    Err(e) => w.error = Some(e),
                }
            }
            rt::Answer::Changed { res } => {
                let Some(w) = self.routines.win.as_mut().filter(|w| w.req == req) else { return };
                w.req = 0;
                w.busy = false;
                match res {
                    Ok(r) => self.routine_changed(r),
                    Err(f) => {
                        w.offer_recreate = f.recreate;
                        w.error = Some(f.why);
                    }
                }
            }
        }
    }

    /// A save or a drop went through. `r` is the routine as it is now, None after a drop.
    fn routine_changed(&mut self, r: Option<Routine>) {
        let Some(mut w) = self.routines.win.take() else { return };
        let (conn, db, schema) = (w.conn, w.db.clone(), w.schema.clone());
        let list_key = self.routines_key(conn, &db, &schema);
        self.routines.lists.remove(&list_key);
        match r {
            None => self.ok(format!("Dropped the {}", w.kind.noun())),
            Some(r) => {
                let was = w.routine.is_some();
                if was && let Some(old) = &w.detail {
                    w.undo = Some((old.source.clone(), w.recreate));
                } else {
                    w.undo = None;
                }
                self.ok(if was { format!("Saved the {}", w.kind.noun()) } else { format!("Created the {}", w.kind.noun()) });
                w.routine = Some(r.clone());
                w.kind = r.kind;
                w.editing = false;
                w.recreate = false;
                w.offer_recreate = false;
                w.error = None;
                w.memo = None;
                // Read again, so the view shows what the server holds now and Undo has the current routine to work on.
                w.base = String::new();
                w.detail = None;
                if let Some(sql) = self.live_sql(conn) {
                    self.req += 1;
                    w.req = self.req;
                    self.worker.sql_routine_detail(conn, self.req, sql.at(&db), schema, r);
                }
                self.routines.win = Some(w);
            }
        }
    }

    /// Starts a save; a locked connection asks to unlock first and saves again after it.
    fn routine_save(&mut self, restore: bool) -> Result<Reply, Refusal> {
        let Some(w) = self.routines.win.as_ref() else { return Ok(Reply::Done) };
        if self.demo {
            self.ok("Offline demo: no database is connected.");
            return Ok(Reply::Done);
        }
        if !self.conn_writable(w.conn) {
            return Err(self.refuse_ro_quiet(w.conn));
        }
        let Some(w) = self.routines.win.as_mut() else { return Ok(Reply::Done) };
        // Undo writes the text from before the last save, on the routine as it is now, the way the save went.
        let (text, recreate) = match (restore, w.undo.clone()) {
            (true, Some((old, recreate))) => (old, recreate),
            _ => (w.text.clone(), w.recreate),
        };
        let (conn, db, schema, old) = (w.conn, w.db.clone(), w.schema.clone(), w.detail.clone());
        // An edit waits for the read of the routine it changes.
        if w.routine.is_some() && old.is_none() {
            return Ok(Reply::Done);
        }
        w.recreate = recreate;
        w.busy = true;
        w.error = None;
        self.req += 1;
        w.req = self.req;
        let (req, read_only) = (self.req, !self.conn_writable(conn));
        let Some(sql) = self.live_sql(conn) else { return Err(self.refuse_offline(conn)) };
        self.worker.sql_routine_save(conn, req, sql.at(&db), schema, old, text, recreate, read_only);
        Ok(Reply::Pending(req))
    }

    fn routine_drop(&mut self) -> Result<Reply, Refusal> {
        let Some(w) = self.routines.win.as_ref() else { return Ok(Reply::Done) };
        let (conn, db, schema, Some(r)) = (w.conn, w.db.clone(), w.schema.clone(), w.routine.clone()) else { return Ok(Reply::Done) };
        if self.demo {
            self.ok("Offline demo: no database is connected.");
            return Ok(Reply::Done);
        }
        if !self.conn_writable(conn) {
            return Err(self.refuse_ro_quiet(conn));
        }
        self.req += 1;
        let (req, read_only) = (self.req, !self.conn_writable(conn));
        let Some(sql) = self.live_sql(conn) else { return Err(self.refuse_offline(conn)) };
        if let Some(w) = self.routines.win.as_mut() {
            (w.req, w.busy, w.error) = (req, true, None);
        }
        self.worker.sql_routine_drop(conn, req, sql.at(&db), schema, r, read_only);
        Ok(Reply::Pending(req))
    }

    /// Runs the call in a new Query tab, where its results show. A routine that may write needs the connection unlocked; a PostgreSQL
    /// function declared STABLE or IMMUTABLE runs on a locked one, on the session's read-only guard, which refuses it if it writes anyway.
    fn routine_run(&mut self) -> Result<Reply, Refusal> {
        let Some(w) = self.routines.win.as_ref() else { return Ok(Reply::Done) };
        let Some(detail) = w.detail.as_ref() else { return Ok(Reply::Done) };
        let (conn, db, schema) = (w.conn, w.db.clone(), w.schema.clone());
        let bs = self.sql_bs_of(conn);
        let Some(script) = rt::run_script(w.dialect, &schema, detail, &w.args, bs) else { return Ok(Reply::Done) };
        if !detail.read_only && !self.conn_writable(conn) {
            return Err(self.refuse_ro_quiet(conn));
        }
        self.sql_new_query(conn, db);
        if let Some(s) = self.tabs.get_mut(self.cur).and_then(|t| t.sql.as_mut()) {
            s.query = script;
        }
        self.sql_run(true, false);
        self.routines.win = None;
        Ok(Reply::Done)
    }

    /// Opens a Query tab for a database. It has no table, so its collection name stays empty.
    pub fn sql_new_query(&mut self, conn: u64, db: String) {
        let engine = self.conn(conn).map(|c| c.meta.engine).unwrap_or_default();
        // A PostgreSQL Query tab starts in the schema the sidebar shows for its database.
        let mut t = Tab::new(self.sel_in(conn, &db, String::new())).for_engine(engine);
        // A database never expanded has no schema list yet; the tab fills its schema when the list arrives.
        if self.is_pg(conn) && !self.sql_asked.contains(&(conn, db.clone())) {
            self.pg_ask_schemas(conn, &db);
        }
        t.view = View::Sql;
        self.tabs.push(t);
        self.set_cur(self.tabs.len() - 1);
        self.reset_doc_state();
    }
}
