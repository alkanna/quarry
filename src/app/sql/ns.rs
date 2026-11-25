//! The namespace of SQL tabs. MySQL has two levels (database, table) and PostgreSQL three (database, schema, table), but a
//! PostgreSQL connection is bound to one database, so the database only picks the pool (`Db::at`) and what the worker
//! calls take as their "schema" argument is the schema there and the database on MySQL. This file turns a `Sel` into that
//! pair, and loads and remembers the schemas the sidebar switches between.

use crate::app::*;

pub(in crate::app) use quarry_core::action::sql::ns::{split_ns, with_ns};
#[cfg(test)]
use quarry_core::action::sql::ns::namespace;

/// The selection of a (database, schema) pair as the forms hold them: `database` is empty on MySQL, where `schema` is the database.
pub(in crate::app) fn sel_of(conn: u64, database: &str, schema: &str, coll: String) -> Sel {
    if database.is_empty() { Sel { conn, db: schema.to_string(), schema: String::new(), coll } } else { Sel { conn, db: database.to_string(), schema: schema.to_string(), coll } }
}

/// Whether a namespace belongs to the server: `schema` on PostgreSQL, `db` on MySQL, where the database is the namespace.
pub(in crate::app) fn system_ns(pg: bool, schema: &str, db: &str) -> bool {
    if pg { db::sql::pg::create::system_schema(schema) } else { db::sql::create::SYSTEM_SCHEMAS.contains(&db) }
}

/// The schemas the switcher lists: the user ones, and the system ones when asked for.
pub(in crate::app) fn listed_schemas(all: &[db::sql::PgSchema], system: bool) -> Vec<&db::sql::PgSchema> {
    all.iter().filter(|s| system || !s.system).collect()
}

impl App {
    pub(in crate::app) fn sql_target(&self, sel: &Sel) -> Option<(db::sql::Db, String)> {
        self.core.sql_target(sel)
    }

    pub(in crate::app) fn sel_schema(&self, sel: &Sel) -> String {
        self.core.sel_schema(sel)
    }

    pub(in crate::app) fn supports(&self, conn: u64, f: db::sql::Feature) -> bool {
        self.core.supports(conn, f)
    }

    pub(in crate::app) fn is_sql(&self, conn: u64) -> bool {
        self.core.is_sql(conn)
    }

    pub(in crate::app) fn is_pg(&self, conn: u64) -> bool {
        self.core.is_pg(conn)
    }

    pub(in crate::app) fn dialect_of(&self, conn: u64) -> db::sql::Dialect {
        self.core.dialect_of(conn)
    }

    pub(in crate::app) fn ns_of(&self, conn: u64, db: &str) -> &str {
        self.core.ns_of(conn, db)
    }

    pub(in crate::app) fn tables_of(&self, sel: &Sel) -> Vec<String> {
        self.core.tables_of(sel)
    }

    pub(in crate::app) fn table_listed(&self, sel: &Sel, name: &str) -> bool {
        self.core.table_listed(sel, name)
    }

    pub(in crate::app) fn form_ns(&self, conn: u64, db: String) -> (String, String) {
        self.core.form_ns(conn, db)
    }

    pub(in crate::app) fn live_sql(&self, conn: u64) -> Option<db::sql::Db> {
        self.core.live_sql(conn)
    }

    pub(in crate::app) fn pg_ask_schemas(&mut self, conn: u64, db: &str) {
        self.core.pg_ask_schemas(conn, db)
    }

    pub(in crate::app) fn pg_load_tables(&mut self, conn: u64, db: &str, schema: &str) {
        self.core.pg_load_tables(conn, db, schema)
    }

    /// Refresh: the schema list, the tables of every schema already loaded and the autocomplete cache are read again.
    /// Every loaded schema is reloaded, not only the picked one, because the command palette lists the tables of all of
    /// them and a schema left alone would keep offering dropped tables. Schemas never opened stay unloaded.
    pub(in crate::app) fn pg_refresh(&mut self, conn: u64, db: &str) {
        let held = self.core.pg_tables_asked.held(conn, db);
        self.core.pg_tables_asked.forget(conn, Some(db), None);
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
