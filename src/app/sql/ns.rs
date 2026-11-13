//! The namespace of SQL tabs. MySQL has two levels (database, table) and PostgreSQL three (database, schema, table), but a
//! PostgreSQL connection is bound to one database, so the database only picks the pool (`Db::at`) and what the worker
//! calls take as their "schema" argument is the schema there and the database on MySQL. This file turns a `Sel` into that
//! pair, and loads and remembers the schemas the sidebar switches between.

use crate::app::*;

/// The namespace a worker call takes for `sel`. A PostgreSQL selection that names no schema (a Query tab opened before the
/// database's schemas were listed) uses `picked`, so no call ever sends the database name as a schema.
pub(in crate::app) fn namespace(dialect: db::sql::Dialect, sel: &Sel, picked: &str) -> String {
    match dialect {
        db::sql::Dialect::Postgres if sel.schema.is_empty() => picked.to_string(),
        db::sql::Dialect::Postgres => sel.schema.clone(),
        db::sql::Dialect::MySql | db::sql::Dialect::MariaDb => sel.db.clone(),
    }
}

/// Where namespace `ns` of a foreign key lives next to `base`: on PostgreSQL a key points at a table of another schema
/// of the same database, on MySQL at a table of another database.
pub(in crate::app) fn with_ns(dialect: db::sql::Dialect, base: &Sel, ns: &str, coll: String) -> Sel {
    let (db, schema) = split_ns(dialect, &base.db, ns);
    Sel { conn: base.conn, db, schema, coll }
}

/// The (database, schema) a foreign key's namespace `ns` stands for, as `Sel` and the caches take them.
pub(in crate::app) fn split_ns(dialect: db::sql::Dialect, base_db: &str, ns: &str) -> (String, String) {
    if dialect == db::sql::Dialect::Postgres { (base_db.to_string(), ns.to_string()) } else { (ns.to_string(), String::new()) }
}

/// The schemas the switcher lists: the user ones, and the system ones when asked for.
pub(in crate::app) fn listed_schemas(all: &[db::sql::PgSchema], system: bool) -> Vec<&db::sql::PgSchema> {
    all.iter().filter(|s| system || !s.system).collect()
}

impl App {
    /// The connection and namespace a worker call about `sel` takes: on PostgreSQL the connection pointing at the
    /// selection's database and its schema, on MySQL the connection and the database name. None while not connected.
    pub(in crate::app) fn sql_target(&self, sel: &Sel) -> Option<(db::sql::Db, String)> {
        let sql = self.conn(sel.conn)?.sql.as_ref()?;
        let picked = if sel.schema.is_empty() { self.ns_of(sel.conn, &sel.db) } else { "" };
        Some((sql.at(&sel.db), namespace(sql.dialect, sel, picked)))
    }

    /// The schema of a selection on PostgreSQL: its own, else the one the sidebar shows, else `public`. Empty on MySQL,
    /// where the database is the whole namespace.
    pub(in crate::app) fn sel_schema(&self, sel: &Sel) -> String {
        if !self.is_pg(sel.conn) || !sel.schema.is_empty() {
            return sel.schema.clone();
        }
        Some(self.ns_of(sel.conn, &sel.db)).filter(|s| !s.is_empty()).unwrap_or(PG_PUBLIC).to_string()
    }

    /// Whether the engine of `conn` has `f`. Mongo and unknown connections answer yes, so their paths stay as they are.
    pub(in crate::app) fn supports(&self, conn: u64, f: db::sql::Feature) -> bool {
        self.conn(conn).is_none_or(|c| c.meta.engine.supports(f))
    }

    /// Whether `conn` is a MySQL, MariaDB or PostgreSQL connection.
    pub(in crate::app) fn is_sql(&self, conn: u64) -> bool {
        self.conn(conn).is_some_and(|c| c.meta.engine.is_sql())
    }

    /// Whether `conn` is a PostgreSQL connection.
    pub(in crate::app) fn is_pg(&self, conn: u64) -> bool {
        self.conn(conn).is_some_and(|c| c.meta.engine == db::sql::Engine::Postgres)
    }

    /// The dialect of `conn`. A connection without a session (the offline demo) still has the dialect of its engine.
    pub(in crate::app) fn dialect_of(&self, conn: u64) -> db::sql::Dialect {
        match self.conn(conn) {
            Some(c) if c.meta.engine == db::sql::Engine::Postgres => db::sql::Dialect::Postgres,
            Some(Conn { sql: Some(s), .. }) => s.dialect,
            _ => db::sql::Dialect::default(),
        }
    }

    /// The schema the sidebar shows for a database of `conn`: on PostgreSQL the one picked, else `public`, else the first
    /// user schema; empty on MySQL and Mongo, where the database is the whole namespace.
    pub(in crate::app) fn ns_of(&self, conn: u64, db: &str) -> &str {
        self.conn(conn).filter(|c| c.meta.engine == db::sql::Engine::Postgres).map_or("", |c| c.pg_picked(db))
    }

    /// The tables and views of the schema a selection points at, once loaded.
    pub(in crate::app) fn tables_of(&self, sel: &Sel) -> Vec<String> {
        let Some(c) = self.conn(sel.conn) else { return Vec::new() };
        if c.meta.engine == db::sql::Engine::Postgres {
            let ns = if sel.schema.is_empty() { c.pg_picked(&sel.db) } else { &sel.schema };
            return c.tables_in(&sel.db, ns).cloned().unwrap_or_default();
        }
        c.dbs.iter().find(|d| d.0 == sel.db).and_then(|d| d.1.clone()).unwrap_or_default()
    }

    /// A selection of `db` in the schema the sidebar shows, for a table or a new Query tab opened from there.
    pub(in crate::app) fn sel_in(&self, conn: u64, db: &str, coll: String) -> Sel {
        Sel { conn, db: db.to_string(), schema: self.ns_of(conn, db).to_string(), coll }
    }

    /// The session of `conn` for a read the demo never sends, or None while not connected.
    fn session(&self, conn: u64) -> Option<db::sql::Db> {
        if self.demo {
            return None;
        }
        self.conn(conn)?.sql.clone()
    }

    /// Asks for the schemas of an opened PostgreSQL database. The tables of the picked one follow when they arrive.
    pub(in crate::app) fn pg_ask_schemas(&mut self, conn: u64, db: &str) {
        let Some(sql) = self.session(conn) else { return };
        self.sql_asked.insert((conn, db.to_string()));
        self.worker.sql_schemas(conn, sql, db.to_string());
    }

    /// Loads the tables of one schema unless they are held or on their way.
    pub(in crate::app) fn pg_load_tables(&mut self, conn: u64, db: &str, schema: &str) {
        // Without a session nothing is sent and nothing is marked asked, so the load happens once it exists.
        let Some(sql) = self.session(conn) else { return };
        if schema.is_empty() || !self.pg_tables_asked.insert(conn, db, schema) {
            return;
        }
        self.worker.sql_tables(conn, sql.at(db), schema.to_string());
    }

    /// The switcher's pick: the sidebar lists that schema from now on, and its tables load if they are not held.
    pub(in crate::app) fn pg_set_schema(&mut self, conn: u64, db: &str, schema: String) {
        if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
            c.pg.entry(db.to_string()).or_default().picked = schema.clone();
        }
        self.pg_load_tables(conn, db, &schema);
    }

    /// Refresh: the schema list, the tables of every schema already loaded and the autocomplete cache are read again.
    /// Every loaded schema is reloaded, not only the picked one, because the command palette lists the tables of all of
    /// them and a schema left alone would keep offering dropped tables. Schemas never opened stay unloaded.
    pub(in crate::app) fn pg_refresh(&mut self, conn: u64, db: &str) {
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

    /// The schemas of a database arrived. The pick is kept while its schema still exists, else it falls back to the
    /// default, and the tables of the pick load.
    pub(in crate::app) fn pg_schemas_done(&mut self, conn: u64, database: String, res: db::Res<Vec<db::sql::PgSchema>>) {
        let list = match res {
            Ok(l) => l,
            Err(e) => {
                self.side_err.insert((conn, database.clone()), e.clone());
                return self.err_quiet(e, Some(conn), Some(Sel { conn, db: database.clone(), ..Default::default() }), format!("List schemas · {database}"));
            }
        };
        self.side_err.remove(&(conn, database.clone()));
        let user: Vec<String> = list.iter().filter(|s| !s.system).map(|s| s.name.clone()).collect();
        let listed = |name: &String| list.iter().any(|s| &s.name == name);
        let keep = self.conn(conn).and_then(|c| c.pg.get(&database)).map(|p| p.picked.clone()).filter(|p| listed(p));
        // A tab restored from the last session names its schema before any pick exists; the tree follows it.
        let of_tab = self.t().filter(|t| t.sel.conn == conn && t.sel.db == database).map(|t| t.sel.schema.clone()).filter(|s| !s.is_empty() && listed(s));
        let picked = keep.or(of_tab).unwrap_or_else(|| default_schema(&user).to_string());
        if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
            c.set_schemas(&database, list, picked.clone());
        }
        // Query tabs opened or switched to this database before its schemas were known take the schema now.
        for t in self.tabs.iter_mut().filter(|t| t.sel.conn == conn) {
            if t.sel.db == database && t.sel.schema.is_empty() && t.sel.coll.is_empty() && t.sql.is_some() {
                t.sel.schema = picked.clone();
            }
            if let Some(s) = t.sql.as_mut().filter(|s| s.run_db == database && s.run_schema.trim().is_empty()) {
                s.run_schema = picked.clone();
            }
        }
        self.pg_load_tables(conn, &database, &picked);
    }

    /// The tables of one schema arrived.
    pub(in crate::app) fn pg_tables_done(&mut self, conn: u64, database: String, schema: String, res: db::Res<(Vec<String>, Vec<String>)>) {
        match res {
            Ok((names, views)) => {
                self.side_err.remove(&(conn, ns_key(&database, &schema)));
                self.views.set(conn, &ns_key(&database, &schema), views);
                if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
                    c.tables.entry(database).or_default().insert(schema, names);
                }
            }
            Err(e) => {
                // A schema that failed to list may be asked again by picking it once more.
                self.pg_tables_asked.forget(conn, Some(&database), Some(&schema));
                self.side_err.insert((conn, ns_key(&database, &schema)), e.clone());
                self.err_quiet(e, Some(conn), Some(Sel { conn, db: database.clone(), schema: schema.clone(), ..Default::default() }), format!("List tables · {database}.{schema}"))
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use db::sql::{Dialect, PgSchema};

    #[test]
    fn a_refresh_reloads_every_loaded_schema_and_the_picked_one_once() {
        let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(schemas_to_reload(&v(&["public", "audit"]), "public"), v(&["audit", "public"]));
        assert_eq!(schemas_to_reload(&v(&["audit"]), "public"), v(&["audit", "public"]));
        assert_eq!(schemas_to_reload(&v(&["audit"]), ""), v(&["audit"]));
        assert!(schemas_to_reload(&[], "").is_empty());
    }

    fn sel(db: &str, schema: &str) -> Sel {
        Sel { conn: 1, db: db.into(), schema: schema.into(), coll: "t".into() }
    }

    #[test]
    fn the_namespace_is_the_schema_on_postgres_and_the_database_on_mysql() {
        assert_eq!(namespace(Dialect::Postgres, &sel("shop", "billing"), "public"), "billing");
        assert_eq!(namespace(Dialect::MySql, &sel("shop", ""), ""), "shop");
        assert_eq!(namespace(Dialect::MariaDb, &sel("shop", ""), "public"), "shop");
        assert_eq!(sel("shop", "billing").space(), "billing");
        assert_eq!(sel("shop", "").space(), "shop");
    }

    #[test]
    fn a_postgres_selection_without_a_schema_uses_the_picked_one() {
        assert_eq!(namespace(Dialect::Postgres, &sel("shop", ""), "audit"), "audit");
    }

    #[test]
    fn the_default_schema_is_public_else_the_first() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(default_schema(&names(&["audit", "public", "zeta"])), "public");
        assert_eq!(default_schema(&names(&["audit", "zeta"])), "audit");
        assert_eq!(default_schema(&[]), "");
    }

    #[test]
    fn caches_are_keyed_by_database_and_schema() {
        assert_eq!(ns_key("shop", ""), "shop");
        assert_ne!(ns_key("a", "public"), ns_key("b", "public"));
        assert_ne!(ns_key("a", "public"), ns_key("a", "audit"));
        // A database called like another database's schema cannot collide with it, because of the separator.
        assert_ne!(ns_key("a_b", ""), ns_key("a", "b"));
        assert_eq!(sel("shop", "billing").nskey(), ns_key("shop", "billing"));
        assert_eq!(sel("shop", "").nskey(), "shop");
    }

    #[test]
    fn a_foreign_key_target_stays_in_the_database_on_postgres() {
        let base = sel("shop", "public");
        let pg = with_ns(Dialect::Postgres, &base, "billing", "invoices".into());
        assert_eq!((pg.db.as_str(), pg.schema.as_str(), pg.coll.as_str()), ("shop", "billing", "invoices"));
        let my = with_ns(Dialect::MySql, &sel("shop", ""), "archive", "orders".into());
        assert_eq!((my.db.as_str(), my.schema.as_str()), ("archive", ""));
        assert_eq!(split_ns(Dialect::Postgres, "shop", "billing"), ("shop".to_string(), "billing".to_string()));
        assert_eq!(split_ns(Dialect::MySql, "shop", "archive"), ("archive".to_string(), String::new()));
    }

    #[test]
    fn titles_show_the_schema_outside_public() {
        assert_eq!(sel("shop", "billing").shown(), "billing.t");
        assert_eq!(sel("shop", "public").shown(), "t");
        assert_eq!(sel("shop", "").shown(), "t");
    }

    #[test]
    fn system_schemas_are_listed_only_when_asked() {
        let s = |name: &str, system| PgSchema { name: name.into(), tables: 1, system };
        let all = [s("information_schema", true), s("pg_catalog", true), s("public", false), s("reporting", false)];
        assert_eq!(listed_schemas(&all, false).iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["public", "reporting"]);
        assert_eq!(listed_schemas(&all, true).len(), 4);
    }
}
