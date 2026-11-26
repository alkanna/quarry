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

    pub(in crate::app) fn sql_serve(&mut self, conn: u64, db: &str, schema: &str, wants: Wants) {
        self.exec(Action::SqlAsk(SqlAskA::Serve { conn, db: db.to_string(), schema: schema.to_string(), wants }))
    }

    pub(in crate::app) fn sql_serve_facts(&mut self, conn: u64, db: &str, schema: &str, table: &str, column: String, kind: db::sql::value::Kind) {
        self.exec(Action::SqlAsk(SqlAskA::ServeFacts { conn, db: db.to_string(), schema: schema.to_string(), table: table.to_string(), column, kind }))
    }

}
