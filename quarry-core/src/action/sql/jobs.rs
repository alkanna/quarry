//! What a finished import or script file, a script's lists and a read schema do to Core: the status line, the undo list, the
//! lists and the tabs that follow what the server has now. Drawn by src/app/sql/dump.rs and import.rs, which start the jobs.

use crate::core::Core;
use crate::db;
use crate::db::sql::import::JobDone;
use crate::state::sql::dump::JobKind;
use crate::state::Sel;
use std::sync::Arc;

impl Core {
    /// Keeps a loaded schema, with the row estimates the sidebar reads next to each table name.
    pub fn put_sql_schema(&mut self, conn: u64, key: String, schema: Arc<db::sql::schema::Schema>) {
        self.sql_rows.insert((conn, key.clone()), schema.tables.iter().filter_map(|t| Some((t.name.clone(), t.rows?))).collect());
        self.sql_schema.insert((conn, key), schema);
        self.lint_schema_changed(conn);
    }

    /// A failed read leaves the database without a schema until the next Refresh; typing still works.
    pub fn sql_schema_done(&mut self, conn: u64, database: String, schema: String, res: db::Res<db::sql::schema::Schema>) {
        if let Ok(s) = res {
            let key = if database.is_empty() { schema } else { crate::state::ns_key(&database, &schema) };
            self.put_sql_schema(conn, key, Arc::new(s));
        }
    }

    /// Starts the Run SQL file dialog's script on `conn`, which `src` was read from. False, with the offline notice, when
    /// there is no session. The source is a loaded file, which no action can carry, so the dialog hands it in here.
    pub(in crate::action) fn start_run_file(&mut self, conn: u64, db: &str, schema: &str, src: crate::db::sql::csv::Source, name: &str) -> bool {
        let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) else {
            self.warn_offline();
            return false;
        };
        let sql = sql.at(db);
        let space = if schema.is_empty() { db.to_string() } else { schema.to_string() };
        self.req += 1;
        let job = crate::state::sql::dump::SqlJob::new(self.req, conn, sql.clone(), space.clone(), JobKind::Script, 0, src.script_len());
        self.worker.sql_run_file(job.req, sql, space, src, !self.conn_writable(conn), job.prog.clone(), job.id.clone(), job.cancel.clone());
        self.sql_job = Some(job);
        self.ok(format!("Running {name}"));
        true
    }

    /// Starts the Import dialog's import of `plan` into `table`; `rows` and `size` are what the file's report counted.
    pub(in crate::action) fn start_import(&mut self, sel: Sel, plan: crate::db::sql::import::ImportPlan, rows: u64, size: u64) {
        let Some(writable) = self.conn(sel.conn).map(|c| c.writable) else { return self.warn_offline() };
        let Some((sql, ns)) = self.sql_target(&sel) else { return self.warn_offline() };
        self.req += 1;
        let mut job = crate::state::sql::dump::SqlJob::new(self.req, sel.conn, sql.clone(), ns.clone(), JobKind::Import, rows, size);
        job.rolls_back = plan.info.transactional();
        let table = sel.coll;
        self.worker.sql_import(job.req, sel.conn, sql, ns, table.clone(), plan, !writable, job.prog.clone(), job.id.clone(), job.cancel.clone());
        self.sql_job = Some(job);
        self.ok(format!("Importing {} rows into {table}", db::group(&rows.to_string())));
    }

    /// An import or a script finished: report it, keep its undo, and reload what it changed.
    pub fn sql_job_done(&mut self, req: u64, res: db::Res<JobDone>) {
        let Some(j) = self.sql_job.take_if(|j| j.req == req) else { return };
        // A script can create or drop anything anywhere, and one that failed or was stopped kept what ran before.
        if j.kind == JobKind::Script && j.db.dialect == db::sql::Dialect::Postgres {
            // The database list of a PostgreSQL server is not what a script changes; its schemas and tables are, in the database the
            // run was for, and the tabs of that database follow what the server has now.
            let database = j.db.pg_database();
            self.pg_refresh(j.conn, &database);
            let open: Vec<usize> = (0..self.tabs.len()).filter(|&i| self.tabs[i].sel.conn == j.conn && self.tabs[i].sel.db == database && !self.tabs[i].sel.coll.is_empty()).collect();
            for i in open {
                self.reload_of(i);
            }
        } else if j.kind == JobKind::Script {
            self.worker.sql_lists(j.conn, j.db.clone(), j.schema.clone());
        }
        match res {
            Ok(d) => {
                self.ok(d.msg);
                if let Some(u) = d.undo {
                    self.push_undo(u);
                }
                // A script's tabs and lists follow in `sql_lists_done`.
                if j.kind == JobKind::Import {
                    // A tab is the table's when its database and namespace, resolved as a read of it resolves them, are the job's: another
                    // PostgreSQL database or schema can hold a table of the same name.
                    let tabs: Vec<usize> = (0..self.tabs.len()).filter(|&i| d.table.as_ref().is_some_and(|t| crate::state::sql::alter::answers_tab(&self.tabs[i].sel, self.sql_target(&self.tabs[i].sel).map(|(x, ns)| (x.pg_database(), ns)), (j.conn, &d.database, &d.schema, t)))).collect();
                    for i in tabs {
                        self.reload_of(i);
                    }
                    if d.tables_changed {
                        self.side_refresh(j.conn, &j.schema);
                    }
                }
            }
            Err(e) if e == "Import cancelled" => self.ok("Import stopped. Nothing was imported."),
            // A table that cannot roll back: the message counts the rows that stayed.
            Err(e) if e.starts_with("Import stopped.") => self.ok(e),
            Err(e) => self.err(e),
        }
    }

    /// After a script, finished or not: the database list, the tables of its database and the open tabs follow what
    /// the server has now. Tabs of a database or table that is gone close; the others of its database reload.
    pub fn sql_lists_done(&mut self, conn: u64, schema: String, res: db::Res<(Vec<String>, Option<Vec<String>>)>) {
        let Ok((dbs, tables)) = res else { return };
        let mut dropped = Vec::new();
        if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
            let old = std::mem::take(&mut c.dbs);
            dropped = old.iter().filter(|o| !dbs.contains(&o.0)).map(|o| o.0.clone()).collect();
            c.dbs = dbs.iter().map(|d| (d.clone(), old.iter().find(|o| o.0 == *d).and_then(|o| o.1.clone()))).collect();
        }
        for d in &dropped {
            self.open_dbs.remove(&(conn, d.clone()));
            self.sql_asked.remove(&(conn, d.clone()));
            self.forget_schema(conn, d);
        }
        self.remove_tabs(|t| t.sel.conn == conn && tab_gone(&t.sel, &dbs, &schema, tables.as_deref()));
        let open: Vec<usize> = (0..self.tabs.len()).filter(|&i| self.tabs[i].sel.conn == conn && self.tabs[i].sel.db == schema && !self.tabs[i].sel.coll.is_empty()).collect();
        for i in open {
            self.reload_of(i);
        }
        if tables.is_some() {
            self.side_refresh(conn, &schema);
        }
    }
}

/// Whether a tab shows a database the server no longer lists, or a table of `schema` that is not among `tables`.
/// `tables` is None when they could not be read, which closes nothing on its own.
fn tab_gone(sel: &Sel, dbs: &[String], schema: &str, tables: Option<&[String]>) -> bool {
    if sel.db.is_empty() {
        return false;
    }
    if !dbs.contains(&sel.db) {
        return true;
    }
    sel.db == schema && !sel.coll.is_empty() && tables.is_some_and(|t| !t.contains(&sel.coll))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Final review finding 8: after a script the tabs of what it dropped close, wherever it was.
    #[test]
    fn tabs_of_what_a_script_dropped_close() {
        let sel = |db: &str, coll: &str| Sel { conn: 1, db: db.into(), coll: coll.into(), ..Default::default() };
        let dbs = vec!["shop".to_string(), "other".to_string()];
        let tables = vec!["orders".to_string()];
        let t = Some(tables.as_slice());
        assert!(!tab_gone(&sel("shop", "orders"), &dbs, "shop", t));
        assert!(tab_gone(&sel("shop", "items"), &dbs, "shop", t), "a table the script dropped");
        assert!(tab_gone(&sel("gone", "x"), &dbs, "shop", t), "a database the script dropped");
        assert!(tab_gone(&sel("gone", ""), &dbs, "shop", t), "a Query tab of a dropped database");
        assert!(!tab_gone(&sel("shop", ""), &dbs, "shop", t), "a Query tab of the database stays");
        assert!(!tab_gone(&sel("other", "items"), &dbs, "shop", t), "tables of other databases were not read");
        assert!(!tab_gone(&sel("shop", "items"), &dbs, "shop", None));
    }
}
