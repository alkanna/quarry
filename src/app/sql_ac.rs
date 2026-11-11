//! The app side of SQL autocomplete: the schema and value caches, and the requests that fill them.

use super::*;
use crate::ui::sql_complete::{value_key, Need, Scope, Wants};

/// Typed prefixes asked per column, and values kept per column.
const MAX_ASKED: usize = 64;
const MAX_VALUES: usize = 500;

impl App {
    /// What completion knows about schema `schema` of database `db` of connection `conn`; MySQL has no schema and `db` is
    /// the whole namespace. `table` is the implicit table of the Content bar. It borrows the connection list, so it takes
    /// the fields rather than `&self` and a tab can be borrowed mutably beside it.
    pub(super) fn sql_scope<'a>(conns: &'a [Conn], schemas: &HashMap<(u64, String), Arc<db::sql::schema::Schema>>, views: &Views, conn: u64, db: &str, schema: &str, table: Option<&str>) -> Scope<'a> {
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
    pub(super) fn put_sql_schema(&mut self, conn: u64, key: String, schema: Arc<db::sql::schema::Schema>) {
        self.sql_rows.insert((conn, key.clone()), schema.tables.iter().filter_map(|t| Some((t.name.clone(), t.rows?))).collect());
        self.sql_schema.insert((conn, key), schema);
    }

    /// Sends what an input asked for while it was drawn: the schema's tables and columns the first time one of its inputs
    /// has focus, and a column's values once per column and typed text. Demo mode has no server.
    pub(super) fn sql_serve(&mut self, conn: u64, db: &str, schema: &str, wants: Wants) {
        if !wants.schema && wants.values.is_none() {
            return;
        }
        let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) else { return };
        // A PostgreSQL tab whose schemas have not arrived has nothing to ask for, and the database name alone is not a schema.
        if schema.is_empty() && self.is_pg(conn) {
            return;
        }
        let key = ns_key(db, schema);
        let space = if schema.is_empty() { db.to_string() } else { schema.to_string() };
        if wants.schema && self.sql_schema_asked.insert((conn, key.clone())) {
            self.worker.sql_schema(conn, sql.at(db), space.clone());
        }
        if let Some(Need { table, column, prefix }) = wants.values {
            // A column asks once per typed prefix, but only so many, so typing through a long list cannot grow the set forever.
            let asked = self.sql_values_asked.entry((conn, key, table.clone(), column.clone())).or_default();
            if (asked.len() < MAX_ASKED || asked.contains(&prefix)) && asked.insert(prefix.clone()) {
                self.worker.sql_field_values(conn, sql.at(db), space, table, column, prefix);
            }
        }
    }

    /// A dropped database: its cached schema and the mark that it was asked for go with it, or a database made again
    /// under that name would never get its autocomplete and shell types back. A PostgreSQL database holds one cache per
    /// schema, all of them under keys that start with its name.
    pub(super) fn forget_schema(&mut self, conn: u64, db: &str) {
        let mine = |k: &str| k == db || k.starts_with(&format!("{db}\u{1f}"));
        self.sql_schema.retain(|k, _| !(k.0 == conn && mine(&k.1)));
        self.sql_rows.retain(|k, _| !(k.0 == conn && mine(&k.1)));
        self.sql_schema_asked.retain(|k| !(k.0 == conn && mine(&k.1)));
        self.sql_values.retain(|k, _| !(k.0 == conn && mine(&k.1)));
        self.sql_values_asked.retain(|k, _| !(k.0 == conn && mine(&k.1)));
    }

    /// The sidebar's Refresh: the schema is read again and looked up values are forgotten.
    pub(super) fn sql_refresh_schema(&mut self, conn: u64, db: &str, schema: &str) {
        let key = ns_key(db, schema);
        self.sql_values.remove(&(conn, key.clone()));
        self.sql_values_asked.retain(|k, _| !(k.0 == conn && k.1 == key));
        if self.sql_schema_asked.contains(&(conn, key)) {
            if let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) {
                self.worker.sql_schema(conn, sql.at(db), if schema.is_empty() { db.to_string() } else { schema.to_string() });
            }
        }
    }

    pub(super) fn sql_values_done(&mut self, conn: u64, database: String, schema: String, table: String, column: String, prefix: Option<String>, res: db::Res<Option<Vec<(String, u64)>>>) {
        let key = if database.is_empty() { schema } else { ns_key(&database, &schema) };
        let entry = self.sql_values.entry((conn, key)).or_default().entry(value_key(&table, &column));
        // An answer of any kind creates the entry, so a list waiting for it stops waiting.
        let info = entry.or_insert_with(|| FieldInfo { kind: "string", ..Default::default() });
        let Ok(Some(rows)) = res else { return };
        if prefix.is_none() {
            (info.values, info.freq) = rows.into_iter().map(|(v, n)| (Bson::String(v), n)).unzip();
            return;
        }
        // Typed lookups add what the frequent list did not hold, up to a cap so a long session cannot grow the list forever.
        let mut known: HashSet<String> = info.values.iter().filter_map(|x| x.as_str().map(String::from)).collect();
        for (v, n) in rows {
            if info.values.len() >= MAX_VALUES {
                break;
            }
            if known.insert(v.clone()) {
                info.values.push(Bson::String(v));
                info.freq.push(n);
            }
        }
    }
}
