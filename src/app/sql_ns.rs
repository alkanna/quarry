//! The namespace of SQL tabs. MySQL has two levels (database, table) and PostgreSQL three (database, schema, table), but a
//! PostgreSQL connection is bound to one database, so the database only picks the pool (`Db::at`) and what the worker
//! calls take as their "schema" argument is the schema there and the database on MySQL. This file turns a `Sel` into that
//! pair, and loads and remembers the schemas the sidebar switches between.

use super::*;

/// The namespace a worker call takes for `sel`. A PostgreSQL selection that names no schema (a Query tab opened before the
/// database's schemas were listed) uses `picked`, so no call ever sends the database name as a schema.
pub(super) fn namespace(dialect: db::sql::Dialect, sel: &Sel, picked: &str) -> String {
    match dialect {
        db::sql::Dialect::Postgres if sel.schema.is_empty() => picked.to_string(),
        db::sql::Dialect::Postgres => sel.schema.clone(),
        db::sql::Dialect::MySql | db::sql::Dialect::MariaDb => sel.db.clone(),
    }
}

/// Where namespace `ns` of a foreign key lives next to `base`: on PostgreSQL a key points at a table of another schema
/// of the same database, on MySQL at a table of another database.
pub(super) fn with_ns(pg: bool, base: &Sel, ns: &str, coll: String) -> Sel {
    if pg { Sel { conn: base.conn, db: base.db.clone(), schema: ns.to_string(), coll } } else { Sel { conn: base.conn, db: ns.to_string(), schema: String::new(), coll } }
}

/// The (database, schema) a foreign key's namespace `ns` stands for, as `Sel` and the caches take them.
pub(super) fn split_ns(pg: bool, base_db: &str, ns: &str) -> (String, String) {
    if pg { (base_db.to_string(), ns.to_string()) } else { (ns.to_string(), String::new()) }
}

/// The schemas the switcher lists: the user ones, and the system ones when asked for.
pub(super) fn listed_schemas(all: &[db::sql::PgSchema], system: bool) -> Vec<&db::sql::PgSchema> {
    all.iter().filter(|s| system || !s.system).collect()
}

impl App {
    /// The connection and namespace a worker call about `sel` takes: on PostgreSQL the connection pointing at the
    /// selection's database and its schema, on MySQL the connection and the database name. None while not connected.
    pub(super) fn sql_target(&self, sel: &Sel) -> Option<(db::sql::Db, String)> {
        let sql = self.conn(sel.conn)?.sql.as_ref()?;
        let picked = if sel.schema.is_empty() { self.pg_picked(sel.conn, &sel.db) } else { String::new() };
        Some((sql.at(&sel.db), namespace(sql.dialect, sel, &picked)))
    }

    /// The schema of a selection on PostgreSQL: its own, else the one the sidebar shows, else `public`. Empty on MySQL,
    /// where the database is the whole namespace.
    pub(super) fn sel_schema(&self, sel: &Sel) -> String {
        if !self.is_pg(sel.conn) || !sel.schema.is_empty() {
            return sel.schema.clone();
        }
        Some(self.pg_picked(sel.conn, &sel.db)).filter(|s| !s.is_empty()).unwrap_or_else(|| PG_PUBLIC.to_string())
    }

    /// Why the open table's structure, relations and triggers cannot be changed, when the engine cannot do it yet. The
    /// editing of PostgreSQL structure is a later phase, so its views are read-only whatever the lock says.
    pub(super) fn struct_block(&self) -> Option<&'static str> {
        self.t().filter(|t| self.is_pg(t.sel.conn)).map(|_| "Changing the structure of a PostgreSQL table is not built yet")
    }

    /// Whether `conn` is a PostgreSQL connection.
    pub(super) fn is_pg(&self, conn: u64) -> bool {
        self.conn(conn).is_some_and(|c| c.meta.engine == db::sql::Engine::Postgres)
    }

    /// The schema the sidebar shows for a PostgreSQL database: the one picked, else `public`, else the first user schema.
    pub(super) fn pg_picked(&self, conn: u64, db: &str) -> String {
        if let Some(p) = self.pg_pick.get(&(conn, db.to_string())) {
            return p.clone();
        }
        let user: Vec<String> = self.conn(conn).and_then(|c| c.dbs.iter().find(|d| d.0 == db)).and_then(|d| d.1.clone()).unwrap_or_default();
        default_schema(&user)
    }

    /// The tables and views of the schema a selection points at, once loaded.
    pub(super) fn tables_of(&self, sel: &Sel) -> Vec<String> {
        let Some(c) = self.conn(sel.conn) else { return Vec::new() };
        if c.meta.engine == db::sql::Engine::Postgres {
            return c.tables.get(&(sel.db.clone(), namespace(db::sql::Dialect::Postgres, sel, &self.pg_picked(sel.conn, &sel.db)))).cloned().unwrap_or_default();
        }
        c.dbs.iter().find(|d| d.0 == sel.db).and_then(|d| d.1.clone()).unwrap_or_default()
    }

    /// A selection of `db` in the schema the sidebar shows, for a table or a new Query tab opened from there.
    pub(super) fn sel_in(&self, conn: u64, db: &str, coll: String) -> Sel {
        let schema = if self.is_pg(conn) { self.pg_picked(conn, db) } else { String::new() };
        Sel { conn, db: db.to_string(), schema, coll }
    }

    /// Asks for the schemas of an opened PostgreSQL database. The tables of the picked one follow when they arrive.
    pub(super) fn pg_ask_schemas(&mut self, conn: u64, db: &str) {
        let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) else { return };
        if self.demo {
            return;
        }
        self.sql_asked.insert((conn, db.to_string()));
        self.worker.sql_schemas(conn, sql, db.to_string());
    }

    /// Loads the tables of one schema unless they are held or on their way.
    pub(super) fn pg_load_tables(&mut self, conn: u64, db: &str, schema: &str) {
        // Without a session nothing is sent and nothing is marked asked, so the load happens once it exists.
        let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) else { return };
        if schema.is_empty() || self.demo || !self.pg_tables_asked.insert((conn, db.to_string(), schema.to_string())) {
            return;
        }
        self.worker.sql_tables(conn, sql.at(db), schema.to_string());
    }

    /// The switcher's pick: the sidebar lists that schema from now on, and its tables load if they are not held.
    pub(super) fn pg_set_schema(&mut self, conn: u64, db: &str, schema: String) {
        self.pg_pick.insert((conn, db.to_string()), schema.clone());
        self.pg_load_tables(conn, db, &schema);
    }

    /// Refresh: the schema list, the tables of every schema already loaded and the autocomplete cache are read again.
    /// Schemas never opened stay unloaded; they load when picked.
    pub(super) fn pg_refresh(&mut self, conn: u64, db: &str) {
        let held: Vec<String> = self.pg_tables_asked.iter().filter(|k| k.0 == conn && k.1 == db).map(|k| k.2.clone()).collect();
        self.pg_tables_asked.retain(|k| !(k.0 == conn && k.1 == db));
        for s in &held {
            self.sql_refresh_schema(conn, db, s);
        }
        self.pg_ask_schemas(conn, db);
        // The schemas answer loads the picked one; the others wait for a pick.
        let picked = self.pg_picked(conn, db);
        if !picked.is_empty() {
            self.pg_load_tables(conn, db, &picked);
        }
    }

    /// The schemas of a database arrived. The pick is kept while its schema still exists, else it falls back to the
    /// default, and the tables of the pick load.
    pub(super) fn pg_schemas_done(&mut self, conn: u64, database: String, res: db::Res<Vec<db::sql::PgSchema>>) {
        let list = match res {
            Ok(l) => l,
            Err(e) => return self.err_quiet(e, Some(conn), Some(Sel { conn, db: database.clone(), ..Default::default() }), format!("List schemas · {database}")),
        };
        let user: Vec<String> = list.iter().filter(|s| !s.system).map(|s| s.name.clone()).collect();
        if let Some(d) = self.conns.iter_mut().find(|c| c.meta.id == conn).and_then(|c| c.dbs.iter_mut().find(|d| d.0 == database)) {
            d.1 = Some(user.clone());
        }
        let key = (conn, database.clone());
        let listed = |name: &String| list.iter().any(|s| &s.name == name);
        let keep = self.pg_pick.get(&key).filter(|p| listed(p)).cloned();
        // A tab restored from the last session names its schema before any pick exists; the tree follows it.
        let of_tab = self.t().filter(|t| t.sel.conn == conn && t.sel.db == database).map(|t| t.sel.schema.clone()).filter(|s| !s.is_empty() && listed(s));
        let picked = keep.or(of_tab).unwrap_or_else(|| default_schema(&user));
        self.pg_schemas.insert(key.clone(), list);
        self.pg_pick.insert(key, picked.clone());
        self.pg_load_tables(conn, &database, &picked);
    }

    /// The tables of one schema arrived.
    pub(super) fn pg_tables_done(&mut self, conn: u64, database: String, schema: String, res: db::Res<(Vec<String>, Vec<String>)>) {
        match res {
            Ok((names, views)) => {
                let key = ns_key(&database, &schema);
                self.views.retain(|(c, d, _)| !(*c == conn && *d == key));
                self.views.extend(views.into_iter().map(|n| (conn, key.clone(), n)));
                if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
                    c.tables.insert((database, schema), names);
                }
            }
            Err(e) => {
                // A schema that failed to list may be asked again by picking it once more.
                self.pg_tables_asked.remove(&(conn, database.clone(), schema.clone()));
                self.err_quiet(e, Some(conn), Some(Sel { conn, db: database.clone(), schema: schema.clone(), ..Default::default() }), format!("List tables · {database}.{schema}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use db::sql::{Dialect, PgSchema};

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
        let pg = with_ns(true, &base, "billing", "invoices".into());
        assert_eq!((pg.db.as_str(), pg.schema.as_str(), pg.coll.as_str()), ("shop", "billing", "invoices"));
        let my = with_ns(false, &sel("shop", ""), "archive", "orders".into());
        assert_eq!((my.db.as_str(), my.schema.as_str()), ("archive", ""));
        assert_eq!(split_ns(true, "shop", "billing"), ("shop".to_string(), "billing".to_string()));
        assert_eq!(split_ns(false, "shop", "archive"), ("archive".to_string(), String::new()));
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
