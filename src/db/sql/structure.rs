//! The structure of one SQL table for the Structure, Relations, Triggers and Info views: columns,
//! indexes, foreign keys in both directions, triggers, the table status and the CREATE statement.
//! Everything is read-only.

use super::worker::{raw, text};
use super::{Db, Dialect, Pool};
use crate::db::{e, Msg, Res, Worker};
use sqlx::mysql::MySqlRow;
use sqlx::{raw_sql, Row as _};

#[derive(Clone, Debug, Default)]
pub struct Column {
    pub name: String,
    /// As the server writes it, such as "int unsigned" or "enum('a','b')".
    pub full_type: String,
    pub nullable: bool,
    /// "PRI", "UNI", "MUL" or empty.
    pub key: String,
    /// The default as MySQL reports it: no SQL quotes around strings. None is a real NULL.
    pub default: Option<String>,
    /// The default is an expression such as CURRENT_TIMESTAMP, not a literal.
    pub default_expr: bool,
    pub extra: String,
    pub collation: Option<String>,
    pub comment: String,
}

impl Column {
    /// "INT" for "int unsigned", "DECIMAL" for "decimal(10,2)".
    pub fn base_type(&self) -> String {
        split_type(&self.full_type).0
    }

    /// The length or the list of values: "10,2" or "'a','b'". Empty when the type has none.
    pub fn args(&self) -> String {
        split_type(&self.full_type).1
    }

    pub fn unsigned(&self) -> bool {
        split_type(&self.full_type).2
    }
}

#[derive(Clone, Debug, Default)]
pub struct IndexCol {
    pub name: String,
    pub desc: bool,
    /// How many leading characters or bytes are indexed.
    pub prefix: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct Index {
    pub name: String,
    pub unique: bool,
    pub cols: Vec<IndexCol>,
    pub kind: String,
    pub cardinality: Option<u64>,
    pub comment: String,
}

/// One foreign key constraint, its columns in order. In `Structure::foreign_keys` the key belongs
/// to the table itself; in `referenced_by` it belongs to `table`, which points here.
#[derive(Clone, Debug, Default)]
pub struct ForeignKey {
    pub name: String,
    pub db: String,
    pub table: String,
    pub cols: Vec<String>,
    pub ref_db: String,
    pub ref_table: String,
    pub ref_cols: Vec<String>,
    pub on_update: String,
    pub on_delete: String,
}

#[derive(Clone, Debug, Default)]
pub struct Trigger {
    pub name: String,
    pub timing: String,
    pub event: String,
    pub body: String,
    pub definer: String,
}

impl Trigger {
    /// The statement as the Triggers view shows it. The server stores only the body.
    pub fn create_sql(&self, table: &str) -> String {
        format!("CREATE TRIGGER {} {} {} ON {}\nFOR EACH ROW\n{}", self.name, self.timing, self.event, table, self.body)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub engine: String,
    pub row_format: String,
    pub collation: String,
    pub rows: Option<u64>,
    pub avg_row: Option<u64>,
    pub data: Option<u64>,
    pub index: Option<u64>,
    pub free: Option<u64>,
    pub auto_increment: Option<u64>,
    pub created: String,
    pub updated: String,
    pub comment: String,
}

#[derive(Clone, Debug, Default)]
pub struct Structure {
    pub columns: Vec<Column>,
    pub indexes: Vec<Index>,
    pub foreign_keys: Vec<ForeignKey>,
    pub referenced_by: Vec<ForeignKey>,
    pub triggers: Vec<Trigger>,
    pub status: Status,
    pub create: String,
}

impl Structure {
    /// The index that serves a foreign key: the first one that starts with the key's columns.
    pub fn index_for(&self, fk: &ForeignKey) -> Option<&Index> {
        self.indexes.iter().find(|ix| ix.cols.len() >= fk.cols.len() && ix.cols.iter().zip(&fk.cols).all(|(a, b)| &a.name == b))
    }
}

/// ("INT", "10,2", true) for "int(10,2) unsigned zerofill"-like text. The last closing parenthesis
/// ends the arguments, because enum values may hold parentheses but nothing follows them except words.
pub fn split_type(full: &str) -> (String, String, bool) {
    let (head, args, tail) = match (full.find('('), full.rfind(')')) {
        (Some(a), Some(b)) if b > a => (&full[..a], full[a + 1..b].to_string(), &full[b + 1..]),
        _ => (full, String::new(), ""),
    };
    let rest = format!("{head} {tail}").to_ascii_uppercase();
    let unsigned = rest.split_whitespace().any(|w| w == "UNSIGNED");
    let base = head.split_whitespace().next().unwrap_or_default().to_ascii_uppercase();
    (base, args, unsigned)
}

/// Makes MariaDB's COLUMN_DEFAULT read like MySQL's. MariaDB keeps the SQL quotes around string
/// defaults, writes the text NULL for a nullable column without default and names expressions such
/// as current_timestamp(). Returns the default and whether it is an expression.
pub fn normalize_default(d: Dialect, raw: Option<String>, nullable: bool, extra: &str) -> (Option<String>, bool) {
    let Some(v) = raw else { return (None, false) };
    if d == Dialect::MySql {
        return (Some(v), extra.to_ascii_uppercase().contains("DEFAULT_GENERATED"));
    }
    if v == "NULL" && nullable {
        return (None, false);
    }
    if v.len() >= 2 && v.starts_with('\'') && v.ends_with('\'') {
        return (Some(v[1..v.len() - 1].replace("''", "'")), false);
    }
    let literal = v == "NULL" || v.parse::<f64>().is_ok();
    (Some(v), !literal)
}

/// An unsigned column value, whichever width and signedness the server reports.
fn num(r: &MySqlRow, name: &str) -> Option<u64> {
    if let Ok(v) = r.try_get::<Option<u64>, _>(name) {
        return v;
    }
    r.try_get::<Option<i64>, _>(name).ok().flatten().and_then(|n| u64::try_from(n).ok())
}

fn s(r: &MySqlRow, name: &str) -> String {
    text(r, name).unwrap_or_default()
}

fn columns(d: Dialect, rows: &[MySqlRow]) -> Vec<Column> {
    rows.iter()
        .map(|r| {
            let nullable = r.try_get::<i64, _>("nullable").map(|n| n != 0).unwrap_or(true);
            let extra = s(r, "extra");
            let (default, default_expr) = normalize_default(d, text(r, "col_default"), nullable, &extra);
            Column { name: s(r, "name"), full_type: s(r, "full_type"), nullable, key: s(r, "col_key"), default, default_expr, extra, collation: text(r, "collation"), comment: s(r, "comment") }
        })
        .collect()
}

/// The rows come ordered by index name and then position, so one index is a run of rows.
fn indexes(rows: &[MySqlRow]) -> Vec<Index> {
    let mut out: Vec<Index> = Vec::new();
    for r in rows {
        let name = s(r, "name");
        let col = IndexCol { name: s(r, "col"), desc: text(r, "direction").as_deref() == Some("D"), prefix: num(r, "sub_part") };
        if out.last().is_some_and(|i| i.name == name) {
            out.last_mut().unwrap().cols.push(col);
        } else {
            let unique = r.try_get::<i64, _>("non_unique").map(|n| n == 0).unwrap_or(false);
            out.push(Index { name, unique, cols: vec![col], kind: s(r, "index_type"), cardinality: num(r, "cardinality"), comment: s(r, "comment") });
        }
    }
    out
}

/// One row per key column, ordered by constraint and then position.
fn foreign_keys(rows: &[MySqlRow]) -> Vec<ForeignKey> {
    let mut out: Vec<ForeignKey> = Vec::new();
    for r in rows {
        let (name, db, table) = (s(r, "name"), s(r, "db"), s(r, "tbl"));
        if let Some(k) = out.last_mut().filter(|k| k.name == name && k.db == db && k.table == table) {
            k.cols.push(s(r, "col"));
            k.ref_cols.push(s(r, "ref_col"));
        } else {
            out.push(ForeignKey { name, db, table, cols: vec![s(r, "col")], ref_db: s(r, "ref_db"), ref_table: s(r, "ref_tbl"), ref_cols: vec![s(r, "ref_col")], on_update: s(r, "on_update"), on_delete: s(r, "on_delete") });
        }
    }
    out
}

fn triggers(rows: &[MySqlRow]) -> Vec<Trigger> {
    rows.iter().map(|r| Trigger { name: s(r, "name"), timing: s(r, "timing"), event: s(r, "event"), body: s(r, "body"), definer: s(r, "definer") }).collect()
}

fn status(rows: &[MySqlRow]) -> Status {
    let Some(r) = rows.first() else { return Status::default() };
    Status {
        engine: s(r, "engine"),
        row_format: s(r, "row_format"),
        collation: s(r, "collation"),
        rows: num(r, "row_estimate"),
        avg_row: num(r, "avg_row_bytes"),
        data: num(r, "data_bytes"),
        index: num(r, "index_bytes"),
        free: num(r, "free_bytes"),
        auto_increment: num(r, "auto_increment"),
        created: s(r, "created"),
        updated: s(r, "updated"),
        comment: s(r, "comment"),
    }
}

async fn query(pool: &sqlx::MySqlPool, sql: &str, schema: &str, table: &str) -> Res<Vec<MySqlRow>> {
    sqlx::query(sql).bind(schema).bind(table).fetch_all(pool).await.map_err(e)
}

async fn load(db: &Db, schema: &str, table: &str) -> Res<Structure> {
    let Pool::My(pool) = &db.pool;
    let d = db.dialect;
    // SHOW CREATE TABLE also answers for a view.
    let create = async { raw_sql(&d.create_stmt(false, schema, table)).fetch_one(pool).await.map_err(e) };
    let (cols, idx, fks, refs, trig, stat, create) = tokio::try_join!(
        query(pool, d.columns(), schema, table),
        query(pool, d.indexes(), schema, table),
        query(pool, d.foreign_keys(), schema, table),
        query(pool, d.referenced_by(), schema, table),
        query(pool, d.triggers(), schema, table),
        query(pool, d.table_status(), schema, table),
        create,
    )?;
    Ok(Structure {
        columns: columns(d, &cols),
        indexes: indexes(&idx),
        foreign_keys: foreign_keys(&fks),
        referenced_by: foreign_keys(&refs),
        triggers: triggers(&trig),
        status: status(&stat),
        create: raw(&create, 1).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default(),
    })
}

impl Worker {
    /// Everything the structure views show, in one answer. Uses the tab's `idx_req`, so it never
    /// competes with a page load.
    pub fn sql_structure(&self, req: u64, db: Db, schema: String, table: String) {
        self.spawn(async move { Msg::SqlStructure { req, res: load(&db, &schema, &table).await } });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_split_into_name_values_and_unsigned() {
        assert_eq!(split_type("int unsigned"), ("INT".into(), String::new(), true));
        assert_eq!(split_type("decimal(10,2) unsigned"), ("DECIMAL".into(), "10,2".into(), true));
        assert_eq!(split_type("varchar(255)"), ("VARCHAR".into(), "255".into(), false));
        assert_eq!(split_type("enum('a','b)c','d')"), ("ENUM".into(), "'a','b)c','d'".into(), false));
        assert_eq!(split_type("text"), ("TEXT".into(), String::new(), false));
        // An enum value that says unsigned is not the flag.
        assert!(!split_type("enum('unsigned')").2);
    }

    #[test]
    fn mariadb_defaults_read_like_mysql() {
        let m = Dialect::MariaDb;
        let n = |v: &str, nullable| normalize_default(m, Some(v.into()), nullable, "");
        assert_eq!(n("'abc'", false), (Some("abc".into()), false));
        assert_eq!(n("'it''s'", false), (Some("it's".into()), false));
        assert_eq!(n("''", false), (Some(String::new()), false));
        assert_eq!(n("NULL", true), (None, false));
        // A quoted string that says NULL is a string.
        assert_eq!(n("'NULL'", true), (Some("NULL".into()), false));
        assert_eq!(n("0.00", false), (Some("0.00".into()), false));
        assert_eq!(n("-1", false), (Some("-1".into()), false));
        assert_eq!(n("current_timestamp()", false), (Some("current_timestamp()".into()), true));
        assert_eq!(normalize_default(m, None, true, ""), (None, false));
    }

    #[test]
    fn mysql_defaults_pass_through() {
        let d = Dialect::MySql;
        assert_eq!(normalize_default(d, Some("abc".into()), false, ""), (Some("abc".into()), false));
        assert_eq!(normalize_default(d, Some("CURRENT_TIMESTAMP".into()), false, "DEFAULT_GENERATED on update CURRENT_TIMESTAMP"), (Some("CURRENT_TIMESTAMP".into()), true));
        assert_eq!(normalize_default(d, None, true, ""), (None, false));
    }

    #[test]
    fn trigger_statement_is_composed() {
        let t = Trigger { name: "t1".into(), timing: "BEFORE".into(), event: "UPDATE".into(), body: "BEGIN\nEND".into(), definer: String::new() };
        assert_eq!(t.create_sql("orders"), "CREATE TRIGGER t1 BEFORE UPDATE ON orders\nFOR EACH ROW\nBEGIN\nEND");
    }

    #[test]
    fn fk_index_is_the_one_that_leads_with_its_columns() {
        let ix = |name: &str, cols: &[&str]| Index { name: name.into(), cols: cols.iter().map(|c| IndexCol { name: (*c).into(), ..Default::default() }).collect(), ..Default::default() };
        let s = Structure { indexes: vec![ix("PRIMARY", &["id"]), ix("idx_a", &["x", "customer_id"]), ix("idx_customer", &["customer_id", "created_at"])], ..Default::default() };
        let fk = ForeignKey { cols: vec!["customer_id".into()], ..Default::default() };
        assert_eq!(s.index_for(&fk).map(|i| i.name.as_str()), Some("idx_customer"));
    }

    /// Needs servers: QUARRY_TEST_MYSQL (MySQL) and QUARRY_TEST_MARIADB (MariaDB), both like
    /// mysql://root:quarry@127.0.0.1:3308. Each is skipped when unset.
    fn live(var: &str) {
        let Ok(uri) = std::env::var(var) else { return };
        let (tx, rx) = std::sync::mpsc::channel();
        let w = Worker::new(tx, eframe::egui::Context::default());
        let pool = w.rt.block_on(async {
            let p = sqlx::MySqlPool::connect(&uri).await.unwrap();
            for sql in [
                "DROP DATABASE IF EXISTS quarry_struct",
                "CREATE DATABASE quarry_struct",
                "CREATE TABLE quarry_struct.customers (id INT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY, email VARCHAR(100) NOT NULL, UNIQUE KEY uq_email (email))",
                "CREATE TABLE quarry_struct.orders (
                    a INT NOT NULL, b INT NOT NULL,
                    customer_id INT UNSIGNED NOT NULL,
                    status ENUM('pending','paid','it''s') NOT NULL DEFAULT 'pending',
                    total DECIMAL(10,2) UNSIGNED NOT NULL DEFAULT 0.00,
                    note TEXT NULL,
                    tag VARCHAR(20) NULL DEFAULT NULL,
                    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
                    PRIMARY KEY (a, b),
                    UNIQUE KEY uq_ab_cust (b, customer_id),
                    KEY idx_status_created (status, created_at DESC),
                    KEY idx_note (tag(5)),
                    CONSTRAINT fk_orders_customer FOREIGN KEY (customer_id) REFERENCES customers (id) ON UPDATE CASCADE ON DELETE RESTRICT
                ) COMMENT='Customer orders'",
                "CREATE TABLE quarry_struct.items (id INT PRIMARY KEY, oa INT NOT NULL, ob INT NOT NULL, CONSTRAINT fk_items_order FOREIGN KEY (oa, ob) REFERENCES orders (a, b) ON DELETE CASCADE)",
                "CREATE TRIGGER quarry_struct.orders_bu BEFORE UPDATE ON quarry_struct.orders FOR EACH ROW SET NEW.tag = 'x'",
            ] {
                sqlx::raw_sql(sql).execute(&p).await.unwrap_or_else(|err| panic!("{sql}: {err}"));
            }
            p
        });
        w.sql_connect(1, uri, std::time::Duration::from_secs(30), crate::db::Net::default());
        let Msg::SqlConnected { res: Ok((db, ..)), .. } = rx.recv_timeout(std::time::Duration::from_secs(15)).unwrap() else { panic!("expected SqlConnected") };
        w.sql_structure(7, db.clone(), "quarry_struct".into(), "orders".into());
        let Msg::SqlStructure { req: 7, res } = rx.recv_timeout(std::time::Duration::from_secs(15)).unwrap() else { panic!("expected SqlStructure") };
        let st = res.unwrap_or_else(|err| panic!("{var}: {err}"));

        let names: Vec<_> = st.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "customer_id", "status", "total", "note", "tag", "created_at"]);
        let col = |n: &str| st.columns.iter().find(|c| c.name == n).unwrap();
        assert_eq!(col("a").key, "PRI");
        assert_eq!(col("customer_id").key, "MUL");
        assert!(col("customer_id").unsigned() && col("customer_id").base_type() == "INT");
        assert_eq!((col("status").base_type().as_str(), col("status").args().as_str()), ("ENUM", "'pending','paid','it''s'"));
        assert_eq!(col("status").default, Some("pending".into()), "{var} enum default");
        assert_eq!(col("total").args(), "10,2");
        assert!(col("total").unsigned());
        assert_eq!(col("total").default, Some("0.00".into()));
        assert!(col("note").nullable && col("note").default.is_none(), "{var} text NULL default");
        assert!(col("tag").nullable && col("tag").default.is_none(), "{var} explicit NULL default");
        assert!(col("created_at").default_expr && col("created_at").default.as_deref().is_some_and(|d| d.to_ascii_lowercase().starts_with("current_timestamp")), "{:?}", col("created_at"));
        assert!(!col("status").default_expr);

        let ix: Vec<_> = st.indexes.iter().map(|i| (i.name.as_str(), i.unique, i.cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>())).collect();
        assert_eq!(ix[0], ("PRIMARY", true, vec!["a", "b"]), "{ix:?}");
        let by = |n: &str| st.indexes.iter().find(|i| i.name == n).unwrap();
        assert!(by("uq_ab_cust").unique);
        assert!(!by("idx_status_created").unique);
        assert_eq!(by("idx_status_created").cols.iter().map(|c| c.desc).collect::<Vec<_>>(), [false, true]);
        assert_eq!(by("idx_note").cols[0].prefix, Some(5));
        assert_eq!(by("PRIMARY").kind, "BTREE");

        assert_eq!(st.foreign_keys.len(), 1);
        let fk = &st.foreign_keys[0];
        assert_eq!((fk.name.as_str(), fk.cols.as_slice(), fk.ref_table.as_str(), fk.ref_cols.as_slice()), ("fk_orders_customer", &["customer_id".to_string()][..], "customers", &["id".to_string()][..]));
        assert_eq!((fk.on_update.as_str(), fk.on_delete.as_str()), ("CASCADE", "RESTRICT"));
        assert!(st.index_for(fk).is_some());
        // A composite key from another table is one entry with both columns.
        assert_eq!(st.referenced_by.len(), 1);
        let r = &st.referenced_by[0];
        assert_eq!((r.table.as_str(), r.cols.as_slice(), r.ref_cols.as_slice()), ("items", &["oa".to_string(), "ob".to_string()][..], &["a".to_string(), "b".to_string()][..]));
        assert_eq!(r.on_delete, "CASCADE");

        assert_eq!(st.triggers.len(), 1);
        let t = &st.triggers[0];
        assert_eq!((t.name.as_str(), t.timing.as_str(), t.event.as_str()), ("orders_bu", "BEFORE", "UPDATE"));
        assert!(t.body.contains("NEW.tag"), "{}", t.body);

        assert_eq!(st.status.engine, "InnoDB");
        assert_eq!(st.status.comment, "Customer orders");
        assert!(st.status.created.starts_with("20"), "{}", st.status.created);
        assert!(st.create.starts_with("CREATE TABLE `orders`"), "{}", st.create);

        // The customers table's keys point the other way.
        w.sql_structure(8, db, "quarry_struct".into(), "customers".into());
        let Msg::SqlStructure { res, .. } = rx.recv_timeout(std::time::Duration::from_secs(15)).unwrap() else { panic!() };
        let c = res.unwrap();
        assert!(c.foreign_keys.is_empty());
        assert_eq!(c.referenced_by.len(), 1);
        assert!(c.indexes.iter().any(|i| i.name == "uq_email" && i.unique));
        assert!(c.columns[0].extra.contains("auto_increment"));

        w.rt.block_on(async { sqlx::raw_sql("DROP DATABASE quarry_struct").execute(&pool).await.unwrap() });
    }

    #[test]
    fn live_mysql() {
        live("QUARRY_TEST_MYSQL");
    }

    #[test]
    fn live_mariadb() {
        live("QUARRY_TEST_MARIADB");
    }
}
