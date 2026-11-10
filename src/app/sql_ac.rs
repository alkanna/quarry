//! The app side of SQL autocomplete: the schema and value caches, and the requests that fill them.

use super::*;
use crate::ui::sql_complete::{value_key, Need, Scope, Wants};

impl App {
    /// What completion knows about database `db` of connection `conn`. `table` is the implicit table of
    /// the Content bar. It borrows the connection list, so it takes the fields rather than `&self` and a
    /// tab can be borrowed mutably beside it.
    pub(super) fn sql_scope<'a>(conns: &'a [Conn], schemas: &HashMap<(u64, String), Arc<db::sql::schema::Schema>>, views: &HashSet<(u64, String, String)>, conn: u64, db: &str, table: Option<&str>) -> Scope<'a> {
        let dbs = conns.iter().find(|c| c.meta.id == conn).map_or(&[][..], |c| c.dbs.as_slice());
        Scope { schema: schemas.get(&(conn, db.to_string())).cloned().unwrap_or_default(), db: db.to_string(), dbs, dialect: conns.iter().find(|c| c.meta.id == conn).and_then(|c| c.sql.as_ref()).map(|s| s.dialect).unwrap_or_default(), table: table.filter(|t| !t.is_empty()).map(str::to_string), fallback: Default::default(), views: views.iter().filter(|v| v.0 == conn && v.1 == db).map(|v| v.2.clone()).collect() }
    }

    /// Sends what an input asked for while it was drawn: the database's schema the first time one of its
    /// inputs has focus, and a column's values once per column and typed text. Demo mode has no server.
    pub(super) fn sql_serve(&mut self, conn: u64, db: &str, wants: Wants) {
        let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) else { return };
        if wants.schema && self.sql_schema_asked.insert((conn, db.to_string())) {
            self.worker.sql_schema(conn, sql.clone(), db.to_string());
        }
        if let Some(Need { table, column, prefix }) = wants.values {
            if self.sql_values_asked.insert((conn, db.to_string(), table.clone(), column.clone(), prefix.clone())) {
                self.worker.sql_field_values(conn, sql, db.to_string(), table, column, prefix);
            }
        }
    }

    /// A dropped database: its cached schema and the mark that it was asked for go with it, or a database made again
    /// under that name would never get its autocomplete and shell types back.
    pub(super) fn forget_schema(&mut self, conn: u64, db: &str) {
        let key = (conn, db.to_string());
        self.sql_schema.remove(&key);
        self.sql_schema_asked.remove(&key);
        self.sql_values.remove(&key);
        self.sql_values_asked.retain(|k| !(k.0 == conn && k.1 == db));
    }

    /// The sidebar's Refresh: the schema is read again and looked up values are forgotten.
    pub(super) fn sql_refresh_schema(&mut self, conn: u64, db: &str) {
        self.sql_values.remove(&(conn, db.to_string()));
        self.sql_values_asked.retain(|k| !(k.0 == conn && k.1 == db));
        if self.sql_schema_asked.contains(&(conn, db.to_string())) {
            if let Some(sql) = self.conn(conn).and_then(|c| c.sql.clone()) {
                self.worker.sql_schema(conn, sql, db.to_string());
            }
        }
    }

    pub(super) fn sql_values_done(&mut self, conn: u64, schema: String, table: String, column: String, prefix: Option<String>, res: db::Res<Option<Vec<(String, u64)>>>) {
        let entry = self.sql_values.entry((conn, schema)).or_default().entry(value_key(&table, &column));
        // An answer of any kind creates the entry, so a list waiting for it stops waiting.
        let info = entry.or_insert_with(|| FieldInfo { kind: "string", ..Default::default() });
        let Ok(Some(rows)) = res else { return };
        if prefix.is_none() {
            (info.values, info.freq) = rows.into_iter().map(|(v, n)| (Bson::String(v), n)).unzip();
            return;
        }
        // Typed lookups add what the frequent list did not hold.
        for (v, n) in rows {
            if !info.values.iter().any(|x| x.as_str() == Some(&v)) {
                info.values.push(Bson::String(v));
                info.freq.push(n);
            }
        }
    }
}
