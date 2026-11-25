//! The app side of SQL autocomplete: the schema and value caches, and the requests that fill them.

use crate::app::*;
use crate::ui::sql_complete::{Scope, Wants};


impl App {
    /// What completion knows about schema `schema` of database `db` of connection `conn`; MySQL has no schema and `db` is
    /// the whole namespace. `table` is the implicit table of the Content bar. It borrows the connection list, so it takes
    /// the fields rather than `&self` and a tab can be borrowed mutably beside it.
    pub(in crate::app) fn sql_scope<'a>(conns: &'a [Conn], schemas: &HashMap<(u64, String), Arc<db::sql::schema::Schema>>, views: &Views, conn: u64, db: &str, schema: &str, table: Option<&str>) -> Scope<'a> {
        let c = conns.iter().find(|c| c.meta.id == conn);
        let dialect = c.and_then(|c| c.sql.as_ref()).map(|s| s.dialect).unwrap_or_default();
        let key = ns_key(db, schema);
        // A PostgreSQL connection reads one database, so the "databases" of its scope are the schemas of this one: the
        // current schema stands where MySQL has the current database, and the others complete as `schema.`.
        let (shown, dbs, others) = match (dialect, c) {
            (db::sql::Dialect::Postgres, c) => (schema.to_string(), &[][..], c.and_then(|c| c.dbs.iter().find(|d| d.0 == db)).and_then(|d| d.1.clone()).unwrap_or_default()),
            (_, c) => (db.to_string(), c.map_or(&[][..], |c| c.dbs.as_slice()), Vec::new()),
        };
        Scope { schema: schemas.get(&(conn, key.clone())).cloned().unwrap_or_default(), db: shown, dbs, dialect, table: table.filter(|t| !t.is_empty()).map(str::to_string), fallback: Default::default(), schemas: others, views: views.of(conn, &key).cloned().unwrap_or_default() }
    }

    /// Keeps a loaded schema, with the row estimates the sidebar reads next to each table name.
    pub(in crate::app) fn put_sql_schema(&mut self, conn: u64, key: String, schema: Arc<db::sql::schema::Schema>) {
        self.core.sql_rows.insert((conn, key.clone()), schema.tables.iter().filter_map(|t| Some((t.name.clone(), t.rows?))).collect());
        self.core.sql_schema.insert((conn, key), schema);
        self.lint_schema_changed(conn);
    }

    pub(in crate::app) fn sql_serve(&mut self, conn: u64, db: &str, schema: &str, wants: Wants) {
        self.core.sql_serve(conn, db, schema, wants)
    }

    pub(in crate::app) fn sql_serve_facts(&mut self, conn: u64, db: &str, schema: &str, table: &str, column: String, kind: db::sql::value::Kind) {
        self.core.sql_serve_facts(conn, db, schema, table, column, kind)
    }

    /// A dropped database: its cached schema and the mark that it was asked for go with it, or a database made again
    /// under that name would never get its autocomplete and shell types back. A PostgreSQL database holds one cache per
    /// schema, all of them under keys that start with its name.
    pub(in crate::app) fn forget_schema(&mut self, conn: u64, db: &str) {
        self.core.forget_schema(conn, db)
    }

}
