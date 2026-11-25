//! Creating, dropping, renaming, duplicating and emptying SQL databases, schemas, tables and views, from the sidebar menus
//! and their dialogs, and what the app shows once the server answers. The dialogs and their forms stay in the app's Modal:
//! an action that opens one pushes `Dialog::SqlForm`, and the dialog's confirm runs the matching `DdlA` again. Drawn by
//! src/app/sql/create.rs, create_pg.rs, tableops.rs and viewdef.rs.

use crate::action::{Dialog, Refusal, Reply, UiFx};
use crate::core::Core;
use crate::db::sql::create::{ColumnSpec, Ddl, Meta};
use crate::db::sql::pg::create::DbOpts;
use crate::db::sql::view::ViewDone;
use crate::db::{self, UndoAction};
use crate::state::sql::bulk::BulkForm;
use crate::state::sql::create::{DropForm, DropKind, NewDb, NewTable};
use crate::state::sql::create_pg::{PgNewDb, PgNewSchema, PgNewTable};
use crate::state::sql::dump::JobKind;
use crate::state::sql::tableops::{DupForm, RenameForm, TruncForm};
use crate::state::sql::viewdef::ViewForm;
use crate::state::{ns_key, Modal, Open, Sel, Tab, View, PG_PUBLIC};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DdlA {
    /// New database, on MySQL or PostgreSQL.
    NewDb { conn: u64 },
    /// New table in the sidebar database `db` (in its picked schema on PostgreSQL).
    NewTable { conn: u64, db: String },
    /// PostgreSQL: New schema in database `db`.
    NewSchema { conn: u64, db: String },
    /// New view in `db`, its SELECT filled with `body`.
    NewView { conn: u64, db: String, body: String },
    EditView { conn: u64, db: String, name: String },
    /// A Query tab's Save as view: the statement under the caret.
    QueryAsView { tab: u64 },
    /// Opens the drop confirmation of a database (`name` empty), schema, table or view, or MySQL's Empty table.
    Drop { conn: u64, db: String, name: String, kind: DropKind },
    /// Opens the Rename dialog of a table, view or PostgreSQL schema.
    Rename { conn: u64, db: String, name: String, kind: DropKind },
    Duplicate { conn: u64, db: String, name: String },
    /// PostgreSQL: opens the Empty table confirmation.
    Truncate { conn: u64, db: String, table: String },
    /// The drop confirmation's Drop, once the name was typed.
    DropRun { conn: u64, database: String, schema: String, table: String, kind: DropKind, opt: bool },
    RenameRun { conn: u64, database: String, schema: String, from: String, to: String, kind: DropKind },
    DuplicateRun { conn: u64, database: String, schema: String, from: String, to: String, data: bool },
    TruncateRun { conn: u64, database: String, schema: String, table: String, restart: bool, cascade: bool },
    /// MySQL's New database dialog's Create, with the server's lists it was built from.
    CreateDb { conn: u64, name: String, charset: String, collation: String, meta: Meta },
    /// PostgreSQL's New database dialog's Create.
    CreatePgDb { conn: u64, name: String, opts: DbOpts },
    /// PostgreSQL's New schema dialog's Create schema.
    CreateSchema { conn: u64, database: String, name: String },
}

/// The forms of the dialogs these actions open. They live in the app's Modal; `modal` hands one over.
pub enum SqlForm {
    NewDb(Box<NewDb>),
    NewTable(Box<NewTable>),
    PgNewDb(Box<PgNewDb>),
    PgNewSchema(Box<PgNewSchema>),
    PgNewTable(Box<PgNewTable>),
    Drop(Box<DropForm>),
    Rename(Box<RenameForm>),
    Duplicate(Box<DupForm>),
    Truncate(Box<TruncForm>),
    View(Box<ViewForm>),
    Bulk(Box<BulkForm>),
}

impl SqlForm {
    pub fn modal<R>(self) -> Modal<R> {
        match self {
            SqlForm::NewDb(f) => Modal::SqlNewDb(f),
            SqlForm::NewTable(f) => Modal::SqlNewTable(f),
            SqlForm::PgNewDb(f) => Modal::PgNewDb(f),
            SqlForm::PgNewSchema(f) => Modal::PgNewSchema(f),
            SqlForm::PgNewTable(f) => Modal::PgNewTable(f),
            SqlForm::Drop(f) => Modal::SqlDrop(f),
            SqlForm::Rename(f) => Modal::SqlRename(f),
            SqlForm::Duplicate(f) => Modal::SqlDuplicate(f),
            SqlForm::Truncate(f) => Modal::PgTruncate(f),
            SqlForm::View(f) => Modal::SqlView(f),
            SqlForm::Bulk(f) => Modal::SqlBulk(f),
        }
    }
}

/// The schemas a refresh reads the tables of: every one that was loaded plus the picked one, each once.
fn schemas_to_reload(held: &[String], picked: &str) -> Vec<String> {
    let mut out: Vec<String> = held.to_vec();
    out.sort();
    out.dedup();
    if !picked.is_empty() && !out.iter().any(|s| s == picked) {
        out.push(picked.to_string());
    }
    out
}

impl Core {
    pub(in crate::action) fn ddl(&mut self, a: DdlA) -> Result<Reply, Refusal> {
        match a {
            DdlA::NewDb { conn } if self.is_pg(conn) => self.pg_new_db(conn)?,
            DdlA::NewDb { conn } => self.sql_new_db(conn)?,
            DdlA::NewTable { conn, db } if self.is_pg(conn) => self.pg_new_table(conn, db)?,
            DdlA::NewTable { conn, db } => self.sql_new_table(conn, db)?,
            DdlA::NewSchema { conn, db } => self.pg_new_schema(conn, db)?,
            DdlA::NewView { conn, db, body } => self.sql_new_view(conn, db, body)?,
            DdlA::EditView { conn, db, name } => self.sql_edit_view(conn, db, name)?,
            DdlA::QueryAsView { tab } => {
                if self.tabs.get(self.cur).is_none_or(|t| t.uid != tab || t.sql.is_none()) {
                    return Err(self.refuse("That tab is no longer open."));
                }
                self.save_query_as_view()?
            }
            DdlA::Drop { conn, db, name, kind } if self.is_pg(conn) => self.pg_drop_ask(conn, db, name, kind)?,
            DdlA::Drop { conn, db, name, kind } => self.sql_drop_ask(conn, db, name, kind)?,
            DdlA::Rename { conn, db, name, kind } => self.sql_rename_ask(conn, db, name, kind)?,
            DdlA::Duplicate { conn, db, name } => self.sql_duplicate_ask(conn, db, name)?,
            DdlA::Truncate { conn, db, table } => self.pg_truncate_ask(conn, db, table)?,
            DdlA::DropRun { conn, database, schema, table, kind, opt } => self.sql_drop_run(conn, database, schema, table, kind, opt),
            DdlA::RenameRun { conn, database, schema, from, to, kind } => {
                let Some((sql, ro)) = self.write_session(conn, &database) else { return Ok(Reply::Done) };
                self.worker.sql_rename(conn, sql, schema, from, to, kind == DropKind::Schema, ro);
            }
            DdlA::DuplicateRun { conn, database, schema, from, to, data } => {
                let Some((sql, ro)) = self.write_session(conn, &database) else { return Ok(Reply::Done) };
                self.worker.sql_duplicate(conn, sql, schema, from, to, data, ro);
            }
            DdlA::TruncateRun { conn, database, schema, table, restart, cascade } => {
                let Some((sql, ro)) = self.write_session(conn, &database) else { return Ok(Reply::Done) };
                self.worker.pg_truncate(conn, sql, schema, table, restart, cascade, ro);
            }
            DdlA::CreateDb { conn, name, charset, collation, meta } => {
                let Some(sql) = self.sql_db_for(conn, None) else { return Ok(Reply::Done) };
                self.worker.sql_create_db(conn, sql, name, charset, collation, meta, !self.conn_writable(conn));
            }
            DdlA::CreatePgDb { conn, name, opts } => {
                if let Some(sql) = self.sql_db_for(conn, None) {
                    self.worker.pg_create_db(conn, sql, name, opts, !self.conn_writable(conn));
                }
            }
            DdlA::CreateSchema { conn, database, name } => {
                if let Some(sql) = self.sql_db_for(conn, None) {
                    self.worker.pg_create_schema(conn, sql.at(&database), name, !self.conn_writable(conn));
                }
            }
        }
        Ok(Reply::Done)
    }

    fn open_form(&mut self, f: SqlForm) {
        self.ui_fx.push(UiFx::Open(Dialog::SqlForm(f)));
    }

    /// Asks the server for what the dialogs list. The offline demo has no server, so its dialogs are filled directly.
    pub fn ask_meta(&mut self, conn: u64, schema: &str) {
        if let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) {
            self.worker.sql_create_meta(conn, sql, schema.to_string());
        }
    }

    fn sql_new_db(&mut self, conn: u64) -> Result<(), Refusal> {
        if !self.conn_writable(conn) {
            return Err(self.refuse_ro_quiet(conn));
        }
        self.ask_meta(conn, "");
        self.open_form(SqlForm::NewDb(Box::new(NewDb { conn, name: String::new(), charset: String::new(), collation: String::new(), meta: None, load_err: None })));
        Ok(())
    }

    fn sql_new_table(&mut self, conn: u64, schema: String) -> Result<(), Refusal> {
        if !self.conn_writable(conn) {
            return Err(self.refuse_ro_quiet(conn));
        }
        self.ask_meta(conn, &schema);
        self.open_form(SqlForm::NewTable(Box::new(NewTable { conn, schema, name: String::new(), engine: String::new(), collation: String::new(), cols: vec![ColumnSpec::id()], keys: Default::default(), comment: String::new(), meta: None, load_err: None, show_sql: false, built: None })));
        Ok(())
    }

    fn sql_drop_ask(&mut self, conn: u64, schema: String, table: String, kind: DropKind) -> Result<(), Refusal> {
        if !self.conn_writable(conn) {
            return Err(self.refuse_ro_quiet(conn));
        }
        if let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) {
            self.worker.sql_drop_info(conn, sql, schema.clone(), table.clone());
        }
        self.open_form(SqlForm::Drop(Box::new(DropForm { conn, database: String::new(), schema, table, kind, typed: String::new(), opt: false, info: None })));
        Ok(())
    }

    /// A create or drop finished: refresh what the sidebar and the tabs show of it.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_ddl_done(&mut self, conn: u64, database: String, schema: String, table: String, what: Ddl, res: db::Res<String>, undo: Option<UndoAction>, dbs: Option<Vec<String>>, sql_mode: Option<String>) {
        // The create was built for the session's mode, which changed since connecting; the previews follow it now.
        self.adopt_sql_mode(conn, sql_mode);
        // A refused undo hands its entry back, so it is kept whether or not the write went through.
        if let Some(u) = undo {
            self.push_undo(u);
        }
        if self.is_pg(conn) {
            return self.pg_ddl_done(conn, database, schema, table, what, res, dbs);
        }
        match res {
            Ok(s) => self.ok(s),
            Err(e) => return self.err(e),
        }
        if let (Some(dbs), Some(c)) = (dbs, self.conns.iter_mut().find(|c| c.meta.id == conn)) {
            let old = std::mem::take(&mut c.dbs);
            c.dbs = dbs.into_iter().map(|d| (d.clone(), old.iter().find(|o| o.0 == d).and_then(|o| o.1.clone()))).collect();
            // A database just created is known to be empty, so it shows 0 instead of a spinner.
            if what == Ddl::CreatedDb && let Some(d) = c.dbs.iter_mut().find(|d| d.0 == schema) {
                d.1 = Some(Vec::new());
            }
        }
        let gone = |t: &Tab| t.sel.conn == conn && t.sel.db == schema && (table.is_empty() || t.sel.coll == table);
        match what {
            Ddl::CreatedDb => {
                self.sql_asked.insert((conn, schema.clone()));
                self.open_dbs.insert((conn, schema.clone()));
                self.reveal = Some((conn, schema, None));
            }
            Ddl::CreatedTable => {
                self.side_refresh(conn, &schema);
                self.open_collection(Sel { conn, db: schema, coll: table, ..Default::default() }, Open::Here);
                if let Some(t) = self.tm() {
                    t.view = View::Structure;
                }
            }
            Ddl::DroppedDb => {
                self.purge_undo(conn, "", &schema, "");
                self.remove_tabs(gone);
                self.open_dbs.remove(&(conn, schema.clone()));
                self.sql_asked.remove(&(conn, schema.clone()));
                self.forget_schema(conn, &schema);
            }
            Ddl::DroppedTable => {
                self.purge_undo(conn, "", &schema, &table);
                self.remove_tabs(gone);
                self.side_refresh(conn, &schema);
            }
            Ddl::CreatedSchema | Ddl::DroppedSchema => {}
            Ddl::Emptied => {
                let open: Vec<usize> = (0..self.tabs.len()).filter(|&i| gone(&self.tabs[i])).collect();
                for i in open {
                    self.reload_of(i);
                }
                self.side_refresh(conn, &schema);
            }
        }
    }

    /// Runs a confirmed drop. Tabs close and the sidebar updates when the server says it worked. A connection locked
    /// again since the confirmation opened asks to unlock with nothing to run after it, so the name is typed again.
    fn sql_drop_run(&mut self, conn: u64, database: String, schema: String, table: String, kind: DropKind, opt: bool) {
        if self.demo {
            return self.ok("Offline demo: no database is connected.");
        }
        let Some(c) = self.conn(conn) else { return };
        if !c.writable {
            return self.ui_fx.push(UiFx::Open(Dialog::Unlock(conn)));
        }
        let Some(sql) = c.sql.clone() else { return self.warn_offline() };
        if c.meta.engine == db::sql::Engine::Postgres {
            return self.pg_drop_run(conn, sql, database, schema, table, kind, opt);
        }
        self.worker.sql_drop(conn, sql, schema, table, kind == DropKind::View, kind == DropKind::Truncate, false);
    }

    /// The sidebar menu's Refresh on a database of a SQL connection, which says so when it is not connected.
    fn side_refresh(&mut self, conn: u64, db: &str) {
        if self.conn(conn).is_some_and(|c| c.sql.is_some()) {
            self.refresh_db(conn, db);
        } else if self.is_sql(conn) {
            self.warn("Not connected. Connect to this server first.");
        }
    }

    /// Refresh on a database: its collections or tables listed again, and on SQL what autocomplete knew of it dropped.
    /// `db` is the sidebar's database, on PostgreSQL never a schema. Agents that change structure use it too. False
    /// when the connection is not open.
    pub fn refresh_db(&mut self, conn: u64, db: &str) -> bool {
        let Some(c) = self.conn(conn) else { return false };
        let (sql, client) = (c.sql.clone(), c.client.clone());
        self.routines_forget(conn, db);
        match (sql, client) {
            (Some(sql), _) if sql.dialect == db::sql::Dialect::Postgres => self.pg_refresh(conn, db),
            (Some(sql), _) => {
                self.sql_refresh_schema(conn, db, "");
                self.worker.sql_tables(conn, sql, db.to_string());
            }
            (None, Some(client)) => self.worker.collections(conn, client, db.to_string()),
            (None, None) => return false,
        }
        true
    }

    /// Forgets the routine lists of a database, so the next frame reads them again. Refresh on a database calls it.
    pub fn routines_forget(&mut self, conn: u64, db: &str) {
        let pre = format!("{db}\u{1f}");
        self.routines.lists.retain(|k, _| k.0 != conn || (k.1 != db && !k.1.starts_with(&pre)));
    }

    /// Refresh: the schema list, the tables of every schema already loaded and the autocomplete cache are read again.
    /// Every loaded schema is reloaded, not only the picked one, because the command palette lists the tables of all of
    /// them and a schema left alone would keep offering dropped tables. Schemas never opened stay unloaded.
    pub fn pg_refresh(&mut self, conn: u64, db: &str) {
        let held = self.pg_tables_asked.held(conn, db);
        self.pg_tables_asked.forget(conn, Some(db), None);
        for s in &held {
            self.sql_refresh_schema(conn, db, s);
        }
        self.pg_ask_schemas(conn, db);
        let picked = self.ns_of(conn, db).to_string();
        for s in schemas_to_reload(&held, &picked) {
            self.pg_load_tables(conn, db, &s);
        }
    }
}

// PostgreSQL.
impl Core {
    pub fn pg_session(&self, conn: u64) -> Option<db::sql::Db> {
        if self.demo {
            return None;
        }
        self.conn(conn)?.sql.clone()
    }

    /// The schema the sidebar lists for `db`, which is where a new table or a drop applies.
    fn pg_schema_of(&self, conn: u64, db: &str) -> String {
        Some(self.ns_of(conn, db)).filter(|s| !s.is_empty()).unwrap_or(PG_PUBLIC).to_string()
    }

    fn pg_new_db(&mut self, conn: u64) -> Result<(), Refusal> {
        if !self.conn_writable(conn) {
            return Err(self.refuse_ro_quiet(conn));
        }
        if let Some(sql) = self.pg_session(conn) {
            self.worker.pg_db_choices(conn, sql);
        }
        self.open_form(SqlForm::PgNewDb(Box::new(PgNewDb { conn, name: String::new(), opts: DbOpts::default(), choices: None, load_err: None, focused: false })));
        Ok(())
    }

    fn pg_new_schema(&mut self, conn: u64, database: String) -> Result<(), Refusal> {
        if !self.conn_writable(conn) {
            return Err(self.refuse_ro_quiet(conn));
        }
        self.open_form(SqlForm::PgNewSchema(Box::new(PgNewSchema { conn, database, name: String::new(), focused: false })));
        Ok(())
    }

    fn pg_new_table(&mut self, conn: u64, database: String) -> Result<(), Refusal> {
        if !self.conn_writable(conn) {
            return Err(self.refuse_ro_quiet(conn));
        }
        let schema = self.pg_schema_of(conn, &database);
        self.open_form(SqlForm::PgNewTable(Box::new(PgNewTable::new(conn, database, schema))));
        Ok(())
    }

    /// Asks what a drop takes. `name` is the schema for a schema, the table or view for those, and empty for a database.
    fn pg_drop_ask(&mut self, conn: u64, db: String, name: String, kind: DropKind) -> Result<(), Refusal> {
        if !self.conn_writable(conn) {
            return Err(self.refuse_ro_quiet(conn));
        }
        let (database, schema, table) = match kind {
            DropKind::Database => (db, String::new(), String::new()),
            DropKind::Schema => (db, name, String::new()),
            _ => {
                let s = self.pg_schema_of(conn, &db);
                (db, s, name)
            }
        };
        if let Some(why) = self.pg_job_blocks(conn, &database, &schema, kind) {
            return Err(self.refuse(why));
        }
        if let Some(sql) = self.pg_session(conn) {
            self.worker.pg_drop_info(conn, sql, database.clone(), schema.clone(), table.clone(), !self.conn_writable(conn));
        }
        self.open_form(SqlForm::Drop(Box::new(DropForm { conn, database, schema, table, kind, typed: String::new(), opt: false, info: None })));
        Ok(())
    }

    /// Why a drop cannot start: an import, script file or shell of Quarry's own is running in that database (or schema). The server would
    /// refuse the drop for its connections and blame another session, and WITH (FORCE) would kill the work halfway.
    /// A script can touch anything in its database, and an import is known by its schema only, so a table drop is held for both.
    pub fn pg_job_blocks(&self, conn: u64, database: &str, schema: &str, kind: DropKind) -> Option<String> {
        let job = self.sql_job.as_ref().filter(|j| j.conn == conn && j.db.pg_database() == database).filter(|j| kind == DropKind::Database || j.kind == JobKind::Script || j.schema == schema);
        let what = match job {
            Some(j) if j.kind == JobKind::Import => "an import",
            Some(_) => "a script file",
            None if self.tabs.iter().any(|t| t.sel.conn == conn && t.sel.db == database && t.shell.cancel.is_some()) => "a shell script",
            None => return None,
        };
        Some(format!("Quarry is running {what} in {database}. Stop it first, then drop."))
    }

    /// Runs a confirmed drop. Tabs and sidebar state go when the server says it worked (`pg_ddl_done`), never before: a drop that is
    /// refused (another session, a pooler) must leave the person's query text and staged changes where they were.
    #[allow(clippy::too_many_arguments)]
    pub fn pg_drop_run(&mut self, conn: u64, sql: db::sql::Db, database: String, schema: String, table: String, kind: DropKind, opt: bool) {
        // A job can start while the confirmation is open, so the check is made again here.
        if let Some(why) = self.pg_job_blocks(conn, &database, &schema, kind) {
            return self.warn(why);
        }
        // The worker checks again so a lock taken while the confirmation was open still holds.
        let ro = !self.conn_writable(conn);
        match kind {
            DropKind::Database => self.worker.pg_drop_db(conn, sql, database, opt, ro),
            DropKind::Schema => self.worker.pg_drop_schema(conn, sql.at(&database), schema, opt, ro),
            DropKind::Table | DropKind::View => self.worker.pg_drop_table(conn, sql.at(&database), schema, table, kind == DropKind::View, opt, ro),
            DropKind::Truncate => unreachable!("PostgreSQL empties a table through pg_truncate_ask, not the drop dialog"),
        }
    }

    /// What the open tabs of a database hold that closing them would lose: staged row edits, staged structure changes and Query text.
    pub fn pg_unsaved_in(&self, conn: u64, database: &str) -> (usize, usize, usize) {
        let edits = self.drafts.values().filter(|d| d.conn == conn && d.db == database).count();
        let mut staged = 0;
        let mut queries = 0;
        for t in self.tabs.iter().filter(|t| t.sel.conn == conn && t.sel.db == database) {
            if let Some(s) = &t.sql {
                staged += s.st.staged.len();
                queries += usize::from(!s.query.trim().is_empty());
            }
        }
        (edits, staged, queries)
    }

    /// A create or drop finished: the sidebar lists and the tabs follow what the server has now.
    #[allow(clippy::too_many_arguments)]
    fn pg_ddl_done(&mut self, conn: u64, database: String, schema: String, table: String, what: Ddl, res: db::Res<String>, dbs: Option<Vec<String>>) {
        match res {
            Ok(s) => self.ok(s),
            Err(e) => return self.err(e),
        }
        if let (Some(dbs), Some(c)) = (dbs, self.conns.iter_mut().find(|c| c.meta.id == conn)) {
            let old = std::mem::take(&mut c.dbs);
            c.dbs = dbs.into_iter().map(|d| (d.clone(), old.iter().find(|o| o.0 == d).and_then(|o| o.1.clone()))).collect();
        }
        // The tabs of a selection without a schema belong to the one the sidebar shows.
        let picked = self.pg_schema_of(conn, &database);
        let in_schema = |t: &Tab, s: &str| t.sel.conn == conn && t.sel.db == database && (if t.sel.schema.is_empty() { picked.as_str() } else { t.sel.schema.as_str() }) == s;
        match what {
            Ddl::CreatedDb => {
                // Its schemas load when the row opens, so nothing is marked asked.
                self.open_dbs.insert((conn, schema.clone()));
                self.reveal = Some((conn, schema, None));
            }
            Ddl::DroppedDb => {
                self.purge_undo(conn, &schema, "", "");
                self.remove_tabs(|t| t.sel.conn == conn && t.sel.db == schema);
                self.open_dbs.remove(&(conn, schema.clone()));
                self.sql_asked.remove(&(conn, schema.clone()));
                self.forget_schema(conn, &schema);
                self.pg_tables_asked.forget(conn, Some(&schema), None);
                if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
                    c.pg.remove(&schema);
                    c.tables.remove(&schema);
                }
            }
            Ddl::CreatedSchema => {
                if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
                    c.pg.entry(database.clone()).or_default().picked = schema.clone();
                }
                self.pg_refresh(conn, &database);
            }
            Ddl::DroppedSchema => {
                self.purge_undo(conn, &database, &schema, "");
                self.remove_tabs(|t| in_schema(t, &schema));
                self.forget_schema(conn, &ns_key(&database, &schema));
                if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
                    if let Some(p) = c.pg.get_mut(&database).filter(|p| p.picked == schema) {
                        p.picked.clear();
                    }
                    if let Some(t) = c.tables.get_mut(&database) {
                        t.remove(&schema);
                    }
                }
                self.pg_refresh(conn, &database);
            }
            Ddl::CreatedTable => {
                self.pg_refresh(conn, &database);
                self.open_collection(Sel { conn, db: database, schema, coll: table }, Open::Here);
                if let Some(t) = self.tm() {
                    t.view = View::Structure;
                }
            }
            Ddl::DroppedTable => {
                self.purge_undo(conn, &database, &schema, &table);
                self.remove_tabs(|t| in_schema(t, &schema) && t.sel.coll == table);
                self.pg_refresh(conn, &database);
            }
            Ddl::Emptied => self.pg_emptied(conn, &database),
        }
    }

    /// The New table dialog's Create table.
    pub fn pg_create_table(&mut self, f: &PgNewTable) {
        if let Some(sql) = self.sql_db_for(f.conn, None) {
            self.worker.pg_create_table(f.conn, sql.at(&f.database), f.spec(), !self.conn_writable(f.conn));
        }
    }

    /// MySQL's New table dialog's Create.
    pub fn sql_create_table(&mut self, f: &NewTable) {
        let Some(sql) = self.sql_db_for(f.conn, None) else { return };
        let Some(meta) = f.meta.clone() else { return };
        self.worker.sql_create_table(f.conn, sql, f.spec(), meta, !self.conn_writable(f.conn));
    }
}

// Rename, duplicate and empty.
impl Core {
    /// The tabs open on `table` of `schema` (every table of it when `table` is empty). On PostgreSQL a tab without a schema of its own
    /// belongs to the one the sidebar shows.
    pub fn tabs_on(&self, conn: u64, database: &str, schema: &str, table: &str) -> Vec<usize> {
        let on = |t: &Tab| t.sel.conn == conn && (if database.is_empty() { t.sel.db == schema } else { t.sel.db == database && self.sel_schema(&t.sel) == schema }) && (table.is_empty() || t.sel.coll == table);
        (0..self.tabs.len()).filter(|&i| on(&self.tabs[i])).collect()
    }

    /// Staged edits in a tab on the table are keyed by its name, so a rename under them would lose or misplace them.
    pub fn staged_on(&self, tabs: &[usize], conn: u64, table: &str) -> bool {
        let staged = tabs.iter().any(|&i| self.tabs[i].sql.as_ref().is_some_and(|s| !s.st.staged.is_empty()));
        staged || self.drafts.values().any(|d| d.conn == conn && (table.is_empty() || d.coll == table) && tabs.iter().any(|&i| self.tabs[i].sel.db == d.db && self.tabs[i].sel.schema == d.schema))
    }

    fn session_of(&self, conn: u64, database: &str) -> Option<db::sql::Db> {
        let sql = self.live_sql(conn)?;
        Some(if database.is_empty() { sql } else { sql.at(database) })
    }

    /// What a change needs to run: the session and whether the connection is read-only. Says why and answers None in the demo, on a
    /// locked connection (which asks to unlock) and while not connected.
    fn write_session(&mut self, conn: u64, database: &str) -> Option<(db::sql::Db, bool)> {
        if self.demo {
            self.ok("Offline demo: no database is connected.");
            return None;
        }
        if !self.conn_writable(conn) {
            if self.conn(conn).is_some() {
                self.ui_fx.push(UiFx::Open(Dialog::Unlock(conn)));
            }
            return None;
        }
        let Some(sql) = self.session_of(conn, database) else {
            self.warn_offline();
            return None;
        };
        Some((sql, !self.conn_writable(conn)))
    }

    /// `db` is the database the sidebar menu was opened on and `name` the table, view or schema.
    fn sql_rename_ask(&mut self, conn: u64, db: String, name: String, kind: DropKind) -> Result<(), Refusal> {
        if !self.conn_writable(conn) {
            return Err(self.refuse_ro_quiet(conn));
        }
        let (database, schema) = if self.is_pg(conn) && kind == DropKind::Schema { (db, name.clone()) } else { self.form_ns(conn, db) };
        self.open_form(SqlForm::Rename(Box::new(RenameForm { conn, database, schema, kind, to: name.clone(), from: name, focused: false, preview: None })));
        Ok(())
    }

    fn sql_duplicate_ask(&mut self, conn: u64, db: String, name: String) -> Result<(), Refusal> {
        if !self.conn_writable(conn) {
            return Err(self.refuse_ro_quiet(conn));
        }
        let (database, schema) = self.form_ns(conn, db);
        self.open_form(SqlForm::Duplicate(Box::new(DupForm { conn, database, schema, to: format!("{name}_copy"), from: name, data: true, focused: false, preview: None })));
        Ok(())
    }

    fn pg_truncate_ask(&mut self, conn: u64, db: String, table: String) -> Result<(), Refusal> {
        if !self.conn_writable(conn) {
            return Err(self.refuse_ro_quiet(conn));
        }
        let schema = self.sel_schema(&Sel { conn, db: db.clone(), ..Default::default() });
        if let Some(sql) = self.session_of(conn, &db) {
            self.worker.pg_truncate_info(conn, sql, db.clone(), schema.clone(), table.clone());
        }
        self.open_form(SqlForm::Truncate(Box::new(TruncForm { conn, database: db, schema, table, typed: String::new(), restart: false, cascade: false, info: None })));
        Ok(())
    }

    /// A rename or its undo finished: the tabs on the old name take the new one, what the sidebar lists is read again, and the undo is kept.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_renamed(&mut self, conn: u64, database: String, schema: String, from: String, to: String, whole: bool, res: db::Res<String>, undo: Option<UndoAction>) {
        match res {
            Ok(s) => self.ok(s),
            Err(e) => {
                // A refused undo hands its entry back, so it stays in the list with the reason.
                if let Some(u) = undo {
                    self.push_undo(u);
                }
                return self.err(e);
            }
        }
        // Row edits recorded under the old name would act on whatever is made under it next.
        self.purge_undo(conn, &database, &schema, if whole { "" } else { &from });
        if let Some(u) = undo {
            self.push_undo(u);
        }
        let tabs = self.tabs_on(conn, &database, &schema, if whole { "" } else { &from });
        for &i in &tabs {
            if whole {
                if !self.tabs[i].sel.schema.is_empty() {
                    self.tabs[i].sel.schema = to.clone();
                }
            } else {
                self.tabs[i].sel.coll = to.clone();
            }
        }
        if whole && let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
            if let Some(p) = c.pg.get_mut(&database).filter(|p| p.picked == schema) {
                p.picked = to.clone();
            }
            if let Some(t) = c.tables.get_mut(&database) {
                t.remove(&schema);
            }
        }
        if whole {
            self.forget_schema(conn, &ns_key(&database, &schema));
            self.pg_tables_asked.forget(conn, Some(&database), Some(&schema));
        }
        for i in tabs {
            self.reload_of(i);
        }
        if self.is_pg(conn) {
            self.pg_refresh(conn, &database);
        } else {
            self.side_refresh(conn, &schema);
        }
    }

    /// What an emptied table leaves open: tabs on a table of the database read it again, because CASCADE reaches tables the menu did not name.
    pub fn pg_emptied(&mut self, conn: u64, database: &str) {
        let open: Vec<usize> = (0..self.tabs.len()).filter(|&i| self.tabs[i].sel.conn == conn && self.tabs[i].sel.db == database && self.tabs[i].sql.is_some() && !self.tabs[i].sel.coll.is_empty()).collect();
        for i in open {
            self.reload_of(i);
        }
        self.pg_refresh(conn, database);
    }
}

// Views.
impl Core {
    /// A fresh dialog for a view in `db` (the database on MySQL, the PostgreSQL database whose picked schema gets it). `body` pre-fills the SELECT.
    fn sql_new_view(&mut self, conn: u64, db: String, body: String) -> Result<(), Refusal> {
        if !self.conn_writable(conn) {
            return Err(self.refuse_ro_quiet(conn));
        }
        let (database, schema) = self.form_ns(conn, db);
        self.open_form(SqlForm::View(Box::new(ViewForm::new(conn, database, schema, String::new(), body, false))));
        Ok(())
    }

    /// Reads the definition of a view into the dialog that edits it.
    fn sql_edit_view(&mut self, conn: u64, db: String, name: String) -> Result<(), Refusal> {
        if !self.conn_writable(conn) {
            return Err(self.refuse_ro_quiet(conn));
        }
        let (database, schema) = self.form_ns(conn, db);
        let Some(sql) = self.sql_db_for(conn, None) else { return Ok(()) };
        self.worker.sql_view_def(conn, if database.is_empty() { sql } else { sql.at(&database) }, schema.clone(), name.clone());
        self.open_form(SqlForm::View(Box::new(ViewForm::new(conn, database, schema, name, String::new(), true))));
        Ok(())
    }

    /// Query tab: a new view from the statement under the caret.
    fn save_query_as_view(&mut self) -> Result<(), Refusal> {
        let d = self.sql_dialect();
        let Some(t) = self.t() else { return Ok(()) };
        let Some(s) = t.sql.as_ref() else { return Ok(()) };
        let text = s.caret_info(d).stmt.map(|(r, _, _)| s.query[r].trim().trim_end_matches(';').trim_end().to_string()).unwrap_or_default();
        let (conn, db) = (t.sel.conn, Some(s.run_db.clone()).filter(|d| !d.is_empty()).unwrap_or_else(|| t.sel.db.clone()));
        if text.is_empty() {
            return Err(self.refuse("Put the caret in the SELECT to save as a view."));
        }
        // A snippet's view is the statement with its values, as Run sends it.
        let text = match self.snippet_fill_run(text) {
            Ok(t) => t,
            Err(e) => return Err(self.refuse(e)),
        };
        self.sql_new_view(conn, db, text)
    }

    /// The view dialog's Save or Create, or Drop and create with `drop_first`. The form waits for the answer.
    pub fn view_save(&mut self, f: &mut ViewForm, drop_first: bool) {
        let pg = self.is_pg(f.conn);
        let Some(sql) = self.sql_db_for(f.conn, None) else { return };
        let sql = if pg { sql.at(&f.database) } else { sql };
        self.worker.sql_save_view(f.conn, sql, f.spec(pg, drop_first), !self.conn_writable(f.conn));
        (f.busy, f.error, f.ask_drop) = (true, None, false);
    }

    /// A save or an undo of a view finished. The dialog it came from, when it still waits (`waiting`), takes a failure
    /// itself: it is handed back instead of shown as an error.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_view_saved(&mut self, conn: u64, database: String, schema: String, name: String, kind: ViewDone, res: db::Res<String>, undo: Option<UndoAction>, sql_mode: Option<String>, waiting: bool) -> Option<String> {
        self.adopt_sql_mode(conn, sql_mode);
        // A refused undo hands its entry back, so it is kept whether or not the write went through.
        if let Some(u) = undo {
            self.push_undo(u);
        }
        match res {
            Err(e) if waiting => return Some(e),
            Err(e) => {
                self.err(e);
                return None;
            }
            Ok(s) => self.ok(s),
        }
        let pg = self.is_pg(conn);
        if pg {
            self.pg_refresh(conn, &database);
        } else {
            self.side_refresh(conn, &schema);
        }
        // A selection without a schema belongs to the one the sidebar shows.
        let picked = if pg { self.sel_schema(&Sel { conn, db: database.clone(), ..Default::default() }) } else { String::new() };
        let (c_db, c_schema, c_name) = (database.clone(), schema.clone(), name.clone());
        let hit = move |t: &Tab| t.sel.conn == conn && t.sel.coll == c_name && if pg { t.sel.db == c_db && (if t.sel.schema.is_empty() { picked.as_str() } else { t.sel.schema.as_str() }) == c_schema } else { t.sel.db == c_schema };
        match kind {
            ViewDone::Created => {
                let sel = if pg { Sel { conn, db: database, schema, coll: name } } else { Sel { conn, db: schema, coll: name, ..Default::default() } };
                self.open_collection(sel, Open::Here);
            }
            ViewDone::Replaced | ViewDone::Restored => {
                let open: Vec<usize> = (0..self.tabs.len()).filter(|&i| hit(&self.tabs[i])).collect();
                for i in open {
                    self.reload_of(i);
                }
            }
            ViewDone::Removed => {
                self.purge_undo(conn, &database, &schema, &name);
                self.remove_tabs(&hit);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refresh_reloads_every_loaded_schema_and_the_picked_one_once() {
        let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(schemas_to_reload(&v(&["public", "audit"]), "public"), v(&["audit", "public"]));
        assert_eq!(schemas_to_reload(&v(&["audit"]), "public"), v(&["audit", "public"]));
        assert_eq!(schemas_to_reload(&v(&["audit"]), ""), v(&["audit"]));
        assert!(schemas_to_reload(&[], "").is_empty());
    }
}
