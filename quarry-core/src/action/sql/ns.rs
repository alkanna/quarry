//! The namespace of SQL tabs. MySQL has two levels (database, table) and PostgreSQL three (database, schema, table), but a
//! PostgreSQL connection is bound to one database, so the database only picks the pool (`Db::at`) and what the worker
//! calls take as their "schema" argument is the schema there and the database on MySQL. This file turns a `Sel` into that
//! pair, and loads the schemas the sidebar switches between.

use crate::core::Core;
use crate::db;
use crate::state::{Conn, Sel, PG_PUBLIC};

/// The namespace a worker call takes for `sel`. A PostgreSQL selection that names no schema (a Query tab opened before the
/// database's schemas were listed) uses `picked`, so no call ever sends the database name as a schema.
pub fn namespace(dialect: db::sql::Dialect, sel: &Sel, picked: &str) -> String {
    match dialect {
        db::sql::Dialect::Postgres if sel.schema.is_empty() => picked.to_string(),
        db::sql::Dialect::Postgres => sel.schema.clone(),
        db::sql::Dialect::MySql | db::sql::Dialect::MariaDb => sel.db.clone(),
    }
}

/// Where namespace `ns` of a foreign key lives next to `base`: on PostgreSQL a key points at a table of another schema
/// of the same database, on MySQL at a table of another database.
pub fn with_ns(dialect: db::sql::Dialect, base: &Sel, ns: &str, coll: String) -> Sel {
    let (db, schema) = split_ns(dialect, &base.db, ns);
    Sel { conn: base.conn, db, schema, coll }
}

/// The (database, schema) a foreign key's namespace `ns` stands for, as `Sel` and the caches take them.
pub fn split_ns(dialect: db::sql::Dialect, base_db: &str, ns: &str) -> (String, String) {
    if dialect == db::sql::Dialect::Postgres { (base_db.to_string(), ns.to_string()) } else { (ns.to_string(), String::new()) }
}

impl Core {
    /// The connection and namespace a worker call about `sel` takes: on PostgreSQL the connection pointing at the
    /// selection's database and its schema, on MySQL the connection and the database name. None while not connected.
    pub fn sql_target(&self, sel: &Sel) -> Option<(db::sql::Db, String)> {
        let sql = self.conn(sel.conn)?.sql.as_ref()?;
        let picked = if sel.schema.is_empty() { self.ns_of(sel.conn, &sel.db) } else { "" };
        Some((sql.at(&sel.db), namespace(sql.dialect, sel, picked)))
    }

    /// The schema of a selection on PostgreSQL: its own, else the one the sidebar shows, else `public`. Empty on MySQL,
    /// where the database is the whole namespace.
    pub fn sel_schema(&self, sel: &Sel) -> String {
        if !self.is_pg(sel.conn) || !sel.schema.is_empty() {
            return sel.schema.clone();
        }
        Some(self.ns_of(sel.conn, &sel.db)).filter(|s| !s.is_empty()).unwrap_or(PG_PUBLIC).to_string()
    }

    /// Whether the engine of `conn` has `f`. Mongo and unknown connections answer yes, so their paths stay as they are.
    pub fn supports(&self, conn: u64, f: db::sql::Feature) -> bool {
        self.conn(conn).is_none_or(|c| c.meta.engine.supports(f))
    }

    /// Whether `conn` is a MySQL, MariaDB or PostgreSQL connection.
    pub fn is_sql(&self, conn: u64) -> bool {
        self.conn(conn).is_some_and(|c| c.meta.engine.is_sql())
    }

    /// Whether `conn` is a PostgreSQL connection.
    pub fn is_pg(&self, conn: u64) -> bool {
        self.conn(conn).is_some_and(|c| c.meta.engine == db::sql::Engine::Postgres)
    }

    /// The dialect of `conn`. A connection without a session (the offline demo) still has the dialect of its engine.
    pub fn dialect_of(&self, conn: u64) -> db::sql::Dialect {
        match self.conn(conn) {
            Some(c) if c.meta.engine == db::sql::Engine::Postgres => db::sql::Dialect::Postgres,
            Some(Conn { sql: Some(s), .. }) => s.dialect,
            _ => db::sql::Dialect::default(),
        }
    }

    /// The schema the sidebar shows for a database of `conn`: on PostgreSQL the one picked, else `public`, else the first
    /// user schema; empty on MySQL and Mongo, where the database is the whole namespace.
    pub fn ns_of(&self, conn: u64, db: &str) -> &str {
        self.conn(conn).filter(|c| c.meta.engine == db::sql::Engine::Postgres).map_or("", |c| c.pg_picked(db))
    }

    /// The tables and views of the schema a selection points at, once loaded.
    pub fn tables_of(&self, sel: &Sel) -> Vec<String> {
        self.tables_ref(sel).to_vec()
    }

    /// Whether the schema a selection points at lists `name`, without copying the list for a check drawn every frame.
    pub fn table_listed(&self, sel: &Sel, name: &str) -> bool {
        self.tables_ref(sel).iter().any(|t| t == name)
    }

    fn tables_ref(&self, sel: &Sel) -> &[String] {
        let Some(c) = self.conn(sel.conn) else { return &[] };
        if c.meta.engine == db::sql::Engine::Postgres {
            let ns = if sel.schema.is_empty() { c.pg_picked(&sel.db) } else { &sel.schema };
            return c.tables_in(&sel.db, ns).map_or(&[], |v| v.as_slice());
        }
        c.dbs.iter().find(|d| d.0 == sel.db).and_then(|d| d.1.as_deref()).unwrap_or(&[])
    }

    /// The (database, schema) a form takes for the sidebar database `db`: on PostgreSQL the database and the schema the sidebar shows,
    /// on MySQL no database and `db` as the schema.
    pub fn form_ns(&self, conn: u64, db: String) -> (String, String) {
        if self.is_pg(conn) {
            let schema = self.sel_schema(&Sel { conn, db: db.clone(), ..Default::default() });
            (db, schema)
        } else {
            (String::new(), db)
        }
    }

    /// A selection of `db` in the schema the sidebar shows, for a table or a new Query tab opened from there.
    pub fn sel_in(&self, conn: u64, db: &str, coll: String) -> Sel {
        Sel { conn, db: db.to_string(), schema: self.ns_of(conn, db).to_string(), coll }
    }

    /// The session of `conn` for a read the demo never sends, or None while not connected.
    pub fn live_sql(&self, conn: u64) -> Option<db::sql::Db> {
        if self.demo {
            return None;
        }
        self.conn(conn)?.sql.clone()
    }

    /// Asks for the schemas of an opened PostgreSQL database. The tables of the picked one follow when they arrive.
    pub fn pg_ask_schemas(&mut self, conn: u64, db: &str) {
        let Some(sql) = self.live_sql(conn) else { return };
        self.sql_asked.insert((conn, db.to_string()));
        self.worker.sql_schemas(conn, sql, db.to_string());
    }

    /// Loads the tables of one schema unless they are held or on their way.
    pub fn pg_load_tables(&mut self, conn: u64, db: &str, schema: &str) {
        // Without a session nothing is sent and nothing is marked asked, so the load happens once it exists.
        let Some(sql) = self.live_sql(conn) else { return };
        if schema.is_empty() || !self.pg_tables_asked.insert(conn, db, schema) {
            return;
        }
        self.worker.sql_tables(conn, sql.at(db), schema.to_string());
    }

    /// The switcher's pick: the sidebar lists that schema from now on, and its tables load if they are not held.
    pub fn pg_set_schema(&mut self, conn: u64, db: &str, schema: String) {
        if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == conn) {
            c.pg.entry(db.to_string()).or_default().picked = schema.clone();
        }
        self.pg_load_tables(conn, db, &schema);
    }

    /// How the connection's last reported sql_mode reads quotes and backslashes. A connection without a server, as in
    /// the offline demo, reads like a stock one.
    pub fn sql_mode(&self, conn: u64) -> db::sql::ddl::Mode {
        self.conn(conn).and_then(|c| c.sql.as_ref()).map_or(db::sql::ddl::Mode::DEFAULT, |d| db::sql::ddl::Mode::of(&d.sql_mode))
    }

    /// The server refused or reported a newer sql_mode than the one the previews follow; they follow the new one now.
    pub fn adopt_sql_mode(&mut self, conn: u64, mode: Option<String>) {
        if let Some(m) = mode && let Some(d) = self.conns.iter_mut().find(|c| c.meta.id == conn).and_then(|c| c.sql.as_mut()) {
            d.sql_mode = m;
        }
    }

    /// The open tab's dialect.
    pub fn sql_dialect(&self) -> db::sql::Dialect {
        self.dialect_of(self.t().map_or(0, |t| t.sel.conn))
    }

    /// Whether the open connection's server reads backslash escapes in strings.
    pub fn sql_bs(&self) -> bool {
        self.t().is_none_or(|t| self.sql_bs_of(t.sel.conn))
    }

    /// Whether connection `conn` reads backslashes in string literals as escapes.
    pub fn sql_bs_of(&self, conn: u64) -> bool {
        self.conn(conn).and_then(|c| c.sql.as_ref()).is_none_or(|d| d.backslash_escapes())
    }
}

impl Core {
    /// Reads a schema's tables and columns for completion and shell types, once until a Refresh. `key` is the cache key
    /// (`ns_key`), `space` the schema name the server reads, and `sql` a session on the right database.
    pub fn ask_sql_schema(&mut self, conn: u64, sql: db::sql::Db, key: String, space: String) {
        if self.sql_schema_asked.insert((conn, key)) {
            self.worker.sql_schema(conn, sql, space);
        }
    }
}
