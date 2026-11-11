//! Live tests of the PostgreSQL read path, run on every server named by QUARRY_TEST_POSTGRES and
//! QUARRY_TEST_POSTGRES13. Each test makes its own schema in the shared `quarry_test` database.

use super::pg_read::read_only;
use super::testing::{every_pg, LivePg};
use super::value::Kind;
use super::{Db, PageQuery};
use crate::db::Msg;
use mongodb::bson::{Bson, Document};
use sqlx::Executor;
use std::time::Duration;

/// A fresh schema of the shared database with `ddl` run in it, and the `Db` pointing at that database.
fn fixture(l: &LivePg, schema: &str, ddl: &str) -> (Db, sqlx::PgPool) {
    let db = l.quarry_test();
    let pool = l.fixture_pool();
    l.qsql(&pool, &format!("DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema}; SET search_path = {schema}; {ddl}"));
    (db, pool)
}

fn page(l: &LivePg, db: &Db, schema: &str, table: &str, q: PageQuery, limit: u64, offset: u64) -> (Vec<super::Col>, Vec<String>, Vec<Document>, u64) {
    l.w.sql_page(7, db.clone(), schema.into(), table.into(), q, limit, offset);
    let Msg::SqlCols { cols, pk, .. } = l.next() else { panic!("expected SqlCols") };
    let Msg::Docs { res: Ok((rows, count, _)), .. } = l.next() else { panic!("expected Docs") };
    (cols, pk, rows.iter().map(|r| r.decode().unwrap()).collect(), count)
}

fn structure(l: &LivePg, db: &Db, schema: &str, table: &str) -> super::Structure {
    l.w.sql_structure(3, db.clone(), schema.into(), table.into());
    let Msg::SqlStructure { res, .. } = l.next() else { panic!("expected SqlStructure") };
    res.unwrap_or_else(|x| panic!("{schema}.{table}: {x}"))
}

fn code(x: &sqlx::Error) -> Option<String> {
    x.as_database_error().and_then(|d| d.code().map(|c| c.to_string()))
}

#[test]
fn schemas_hide_the_system_ones() {
    every_pg(|l, var| {
        let (db, _p) = fixture(l, "t4_schemas", "");
        l.w.sql_schemas(1, db, "quarry_test".into());
        let Msg::SqlSchemas { res: Ok(all), database, .. } = l.next() else { panic!("expected SqlSchemas") };
        assert_eq!(database, "quarry_test");
        let names: Vec<String> = all.iter().filter(|s| !s.system).map(|s| s.name.clone()).collect();
        assert!(names.iter().any(|n| n == "public") && names.iter().any(|n| n == "t4_schemas"), "{var}: {names:?}");
        for hidden in ["pg_catalog", "information_schema"] {
            assert!(!names.iter().any(|n| n == hidden), "{var}: {names:?}");
            assert!(all.iter().any(|s| s.name == hidden && s.system), "{var}: the system schema {hidden} is listed and flagged");
        }
        assert!(!names.iter().any(|n| n.starts_with("pg_toast") || n.starts_with("pg_temp")), "{var}: {names:?}");
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        // The fixture schema holds no table until the next test makes one, so its count is a number, not a guess.
        assert_eq!(all.iter().find(|s| s.name == "t4_schemas").map(|s| s.tables), Some(0), "{var}: an empty schema counts no tables");
    });
}

#[test]
fn tables_list_views_and_hide_partitions() {
    every_pg(|l, var| {
        let (db, _p) = fixture(l, "t4_list", "CREATE TABLE t (id int PRIMARY KEY); CREATE VIEW v AS SELECT id FROM t; CREATE MATERIALIZED VIEW m AS SELECT id FROM t; CREATE TABLE p (id int, d date) PARTITION BY RANGE (d); CREATE TABLE p_2024 PARTITION OF p FOR VALUES FROM ('2024-01-01') TO ('2025-01-01');");
        l.w.sql_tables(1, db.clone(), "t4_list".into());
        let Msg::SqlTables { res: Ok((names, views)), database, schema, .. } = l.next() else { panic!("expected SqlTables") };
        assert_eq!((database.as_str(), schema.as_str()), ("quarry_test", "t4_list"), "{var}");
        assert_eq!(names, ["m", "p", "t", "v"], "{var}");
        assert!(views.contains(&"v".to_string()) && views.contains(&"m".to_string()) && views.len() == 2, "{var}: {views:?}");
        // The switcher's count is the sidebar's list: partitions are left out.
        l.w.sql_schemas(1, db, "quarry_test".into());
        let Msg::SqlSchemas { res: Ok(all), .. } = l.next() else { panic!("expected SqlSchemas") };
        assert_eq!(all.iter().find(|s| s.name == "t4_list").map(|s| s.tables), Some(4), "{var}");
    });
}

#[test]
fn page_and_count_with_a_filter() {
    every_pg(|l, var| {
        let (db, _p) = fixture(l, "t4_page", "CREATE TABLE t (a int, b varchar(20), n int, PRIMARY KEY (b, a)); INSERT INTO t SELECT i, 'k' || i, i % 3 FROM generate_series(0, 29) i;");
        let (cols, pk, rows, count) = page(l, &db, "t4_page", "t", PageQuery { filter: "n = 1".into(), order: "a DESC -- trailing comment".into(), ..Default::default() }, 4, 2);
        assert_eq!(count, 10, "{var}");
        assert_eq!(pk, ["b", "a"]);
        assert_eq!(cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["a", "b", "n"]);
        assert_eq!(rows.iter().map(|d| d.get_i64("a").unwrap()).collect::<Vec<_>>(), [22, 19, 16, 13], "{var}");
        assert_eq!(rows[0].get_document("_id").unwrap(), &mongodb::bson::doc! { "b": "k22", "a": 22_i64 });
        // The whole table; the index hint chip does nothing on this engine; an empty page still has its headers.
        let (_, _, rows, count) = page(l, &db, "t4_page", "t", PageQuery { index_hint: "whatever".into(), ..Default::default() }, 100, 0);
        assert_eq!((rows.len(), count), (30, 30));
        let (cols, _, rows, count) = page(l, &db, "t4_page", "t", PageQuery { filter: "n = 99".into(), ..Default::default() }, 10, 0);
        assert_eq!((cols.len(), rows.len(), count), (3, 0, 0), "{var}");
        // A grouped page counts groups.
        let g = PageQuery { select: vec!["n".into(), "count(*) AS c".into()], group: vec!["n".into()], order: "n".into(), ..Default::default() };
        let (_, pk, rows, count) = page(l, &db, "t4_page", "t", g, 10, 0);
        assert!(pk.is_empty());
        assert_eq!((rows.len(), count), (3, 3), "{var}");
        assert_eq!(rows[0].get_i64("c"), Ok(10));
    });
}

#[test]
fn user_text_cannot_leave_the_session_writable_or_change_how_the_next_statement_is_read() {
    every_pg(|l, var| {
        let (db, _p) = fixture(l, "t4_ro", "CREATE TABLE t (id int PRIMARY KEY); INSERT INTO t VALUES (1);");
        let _rt = l.w.rt.enter();
        let pool = db.pg_pool().unwrap();
        let show = |what: &str| {
            l.w.rt.block_on(async {
                let mut c = pool.acquire().await.unwrap();
                let r = c.fetch_all(format!("SHOW {what}").as_str()).await.unwrap();
                crate::db::sql::worker::text_at(&r[0], 0).unwrap()
            })
        };
        // Statement A turns the guard off for the session and changes the client encoding. The pooled connection
        // goes back (user_text false) and its next statement still sees both as they were.
        let a = "SELECT set_config('default_transaction_read_only', 'off', false), set_config('client_encoding', 'SJIS', false)";
        let rows = l.w.rt.block_on(read_only(&pool, false, "", a)).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(show("default_transaction_read_only"), "on", "{var}");
        assert_eq!(show("client_encoding"), "UTF8", "{var}");
        let err = l.w.rt.block_on(async {
            let mut c = pool.acquire().await.unwrap();
            c.execute("CREATE TABLE t4_ro.nope (id int)").await.unwrap_err()
        });
        assert_eq!(code(&err).as_deref(), Some("25006"), "{var}");
        // The same text as a Content filter: the lexer of this branch may refuse it, and either way the next
        // statement on the pool is unharmed.
        l.w.sql_page(8, db.clone(), "t4_ro".into(), "t".into(), PageQuery { filter: "set_config('default_transaction_read_only', 'off', false) = 'off'".into(), ..Default::default() }, 10, 0);
        // A refused filter answers once, a page that ran answers twice (rows and count), so the second is optional.
        let _ = l.next();
        let _ = l.rx.recv_timeout(std::time::Duration::from_secs(3));
        assert_eq!(show("default_transaction_read_only"), "on", "{var}");
        // A write is refused inside the transaction as well.
        let err = l.w.rt.block_on(read_only(&pool, false, "", "DELETE FROM t4_ro.t")).unwrap_err();
        assert_eq!(code(&err).as_deref(), Some("25006"), "{var}");
    });
}

#[test]
fn a_connection_that_ran_user_text_does_not_go_back_to_the_pool() {
    every_pg(|l, var| {
        let (db, p) = fixture(l, "t4_lock", "CREATE TABLE t (id int PRIMARY KEY); INSERT INTO t VALUES (1); CREATE FUNCTION grab() RETURNS boolean LANGUAGE sql AS $$ SELECT pg_advisory_lock(424242) IS NULL $$;");
        let _rt = l.w.rt.enter();
        let pool = db.pg_pool().unwrap();
        let locks = || l.qone(&p, "SELECT count(*)::text FROM pg_locks WHERE locktype = 'advisory' AND objid = 424242 AND classid = 0");
        let settle = |want: &dyn Fn() -> bool| {
            let t = std::time::Instant::now();
            while !want() && t.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        // A filter that calls a wrapper around a session advisory lock, through the worker as the app does.
        l.w.sql_page(9, db.clone(), "t4_lock".into(), "t".into(), PageQuery { filter: "t4_lock.grab() IS NOT NULL".into(), ..Default::default() }, 10, 0);
        let _ = l.next();
        let _ = l.next();
        settle(&|| locks() == "0");
        assert_eq!(locks(), "0", "{var}: the lock outlived the connection");
        // Closed, not returned: the backend that ran user text is gone, and one that ran Quarry's own SQL stays.
        let pid_of = |user_text: bool| {
            let rows = l.w.rt.block_on(read_only(&pool, user_text, "", "SELECT pg_backend_pid()::text, t4_lock.grab()::text")).unwrap();
            crate::db::sql::worker::text_at(&rows[0], 0).unwrap()
        };
        let alive = |pid: &str| l.qone(&p, &format!("SELECT count(*)::text FROM pg_stat_activity WHERE pid = {pid}"));
        let user = pid_of(true);
        settle(&|| alive(&user) == "0");
        assert_eq!(alive(&user), "0", "{var}: the connection went back to the pool");
        settle(&|| locks() == "0");
        assert_eq!(locks(), "0", "{var}");
        let own = pid_of(false);
        assert_eq!(alive(&own), "1", "{var}");
        assert_eq!(pid_of(false), own, "{var}: the pool should hand the same connection back");
        // That one holds the lock, as the contrast to the closed ones. Closing it releases it.
        assert_eq!(locks(), "1", "{var}");
        l.w.rt.block_on(pool.close());
    });
}

#[test]
fn every_kind_decodes() {
    every_pg(|l, var| {
        let ddl = "CREATE TYPE mood AS ENUM ('sad', 'ok'); CREATE DOMAIN posint AS int CHECK (VALUE > 0); CREATE DOMAIN posint2 AS posint;
            CREATE TABLE t (id int PRIMARY KEY, b bool, y bytea, u uuid, ia int[], iv interval, ip inet, r int4range, n numeric(10,2), tz timestamptz, j jsonb, e mood, d posint, d2 posint2, f float8, tx text, dt date, tm time);
            INSERT INTO t VALUES (1, true, '\\x00ff10', '123e4567-e89b-12d3-a456-426614174000', '{1,2,3}', '1 day 02:00:00', '10.0.0.1/24', '[1,5)', 12.50, '2024-02-29 13:14:15+00', '{\"a\": [1, 2]}', 'ok', 7, 8, 1.5, 'héllo', '2024-02-29', '13:14:15');
            INSERT INTO t (id) VALUES (2);";
        let (db, _p) = fixture(l, "t4_kinds", ddl);
        let (cols, _, rows, _) = page(l, &db, "t4_kinds", "t", PageQuery { order: "id".into(), ..Default::default() }, 10, 0);
        let kinds: Vec<Kind> = cols.iter().map(|c| c.kind).collect();
        assert_eq!(kinds, [Kind::Int, Kind::Bool, Kind::Binary, Kind::Uuid, Kind::Array, Kind::Interval, Kind::Inet, Kind::Range, Kind::Decimal, Kind::DateTime, Kind::Json, Kind::Enum, Kind::Int, Kind::Int, Kind::Float, Kind::Text, Kind::Date, Kind::Time], "{var}");
        let r = &rows[0];
        assert_eq!(r.get("b"), Some(&Bson::Boolean(true)));
        assert_eq!(r.get("y"), Some(&Bson::Binary(mongodb::bson::Binary { subtype: mongodb::bson::spec::BinarySubtype::Generic, bytes: vec![0, 255, 16] })), "{var}");
        assert_eq!(r.get_str("u"), Ok("123e4567-e89b-12d3-a456-426614174000"));
        assert_eq!(r.get_str("ia"), Ok("{1,2,3}"));
        assert_eq!(r.get_str("iv"), Ok("1 day 02:00:00"), "{var}");
        assert_eq!(r.get_str("ip"), Ok("10.0.0.1/24"));
        assert_eq!(r.get_str("r"), Ok("[1,5)"));
        assert_eq!(r.get_str("n"), Ok("12.50"));
        assert!(r.get_str("tz").unwrap().starts_with("2024-02-29 13:14:15"), "{var}: {:?}", r.get("tz"));
        assert!(r.get_str("j").unwrap().contains("\"a\""));
        assert_eq!(r.get_str("e"), Ok("ok"));
        assert_eq!((r.get("d"), r.get("d2")), (Some(&Bson::Int64(7)), Some(&Bson::Int64(8))));
        assert_eq!(r.get("f"), Some(&Bson::Double(1.5)));
        assert_eq!(r.get_str("tx"), Ok("héllo"));
        assert_eq!((r.get_str("dt"), r.get_str("tm")), (Ok("2024-02-29"), Ok("13:14:15")));
        for c in cols.iter().filter(|c| c.name != "id") {
            assert_eq!(rows[1].get(&c.name), Some(&Bson::Null), "{}", c.name);
            assert!(c.nullable, "{}", c.name);
        }
        // The same kinds reach autocomplete.
        l.w.sql_schema(1, db.clone(), "t4_kinds".into());
        let Msg::SqlSchema { res: Ok(s), .. } = l.next() else { panic!("expected SqlSchema") };
        let t = s.table("t").unwrap();
        assert_eq!(t.cols.iter().map(|c| c.kind).collect::<Vec<_>>(), kinds);
        assert_eq!(t.col("n").unwrap().ty, "numeric(10,2)");
        assert_eq!(t.col("id").unwrap().key, "PRI");
    });
}

#[test]
fn structure_info_relations_and_triggers() {
    every_pg(|l, var| {
        let ddl = "CREATE TABLE parent (id int PRIMARY KEY);
            CREATE FUNCTION tf() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.status := 'x'; RETURN NEW; END $$;
            CREATE TABLE orders (a int, b int, customer_id int NOT NULL REFERENCES parent (id) ON UPDATE CASCADE ON DELETE RESTRICT,
                qty int NOT NULL DEFAULT 1 CHECK (qty > 0), status text DEFAULT 'new', seq int GENERATED BY DEFAULT AS IDENTITY, dbl int GENERATED ALWAYS AS (qty * 2) STORED,
                note varchar(40) COLLATE \"C\", serial_id serial, PRIMARY KEY (a, b), CONSTRAINT uq_ab_cust UNIQUE (b, customer_id));
            CREATE INDEX idx_status ON orders (status, qty DESC); CREATE INDEX idx_lower ON orders (lower(status));
            CREATE TRIGGER orders_bu BEFORE UPDATE OR INSERT ON orders FOR EACH ROW EXECUTE FUNCTION tf();
            CREATE TABLE items (id int PRIMARY KEY, oa int NOT NULL, ob int NOT NULL, FOREIGN KEY (oa, ob) REFERENCES orders (a, b) ON DELETE CASCADE);
            COMMENT ON TABLE orders IS 'Customer orders'; COMMENT ON COLUMN orders.qty IS 'How many';
            INSERT INTO parent VALUES (1);";
        let (db, p) = fixture(l, "t4_struct", ddl);
        let st = structure(l, &db, "t4_struct", "orders");
        let col = |n: &str| st.columns.iter().find(|c| c.name == n).unwrap();
        assert_eq!(st.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["a", "b", "customer_id", "qty", "status", "seq", "dbl", "note", "serial_id"]);
        assert_eq!((col("a").key.as_str(), col("a").nullable, col("a").full_type.as_str()), ("PRI", false, "integer"), "{var}");
        assert_eq!(col("note").full_type, "character varying(40)");
        assert_eq!(col("note").collation.as_deref(), Some("C"));
        assert_eq!(col("status").key, "MUL");
        assert_eq!((col("qty").default.as_deref(), col("qty").default_expr, col("qty").comment.as_str()), (Some("1"), false, "How many"));
        assert_eq!((col("status").default.as_deref(), col("status").default_expr), (Some("new"), false), "{:?}", col("status").default_raw);
        assert!(col("status").nullable);
        assert!(col("seq").extra.contains("auto_increment") && col("seq").default.is_none(), "{}", col("seq").extra);
        assert!(col("dbl").extra.contains("GENERATED") && col("dbl").default.is_none(), "{}", col("dbl").extra);
        assert!(col("serial_id").extra.contains("auto_increment") && col("serial_id").default_expr, "{:?}", col("serial_id"));
        assert_eq!(col("a").kind, Some(Kind::Int));
        let ix = |n: &str| st.indexes.iter().find(|i| i.name == n).unwrap();
        assert_eq!(st.indexes[0].name, "orders_pkey");
        assert!(st.indexes[0].unique && st.indexes[0].constraint && st.indexes[0].cols.iter().map(|c| c.name.as_str()).eq(["a", "b"]));
        assert!(ix("uq_ab_cust").unique && ix("uq_ab_cust").constraint);
        assert!(!ix("idx_status").unique && !ix("idx_status").constraint && ix("idx_status").kind == "BTREE");
        assert_eq!(ix("idx_status").cols.iter().map(|c| c.desc).collect::<Vec<_>>(), [false, true]);
        assert!(ix("idx_lower").cols[0].name.is_empty() && ix("idx_lower").cols[0].expr.as_deref().is_some_and(|e| e.contains("lower")), "{:?}", ix("idx_lower").cols);
        assert!(ix("idx_status").def.starts_with("CREATE INDEX idx_status ON t4_struct.orders"), "{}", ix("idx_status").def);
        let kinds: Vec<char> = st.constraints.iter().map(|c| c.kind).collect();
        assert!(kinds.contains(&'p') && kinds.contains(&'u') && kinds.contains(&'c'), "{:?}", st.constraints);
        assert!(st.constraints.iter().any(|c| c.kind == 'c' && c.def.contains("qty > 0")));
        assert_eq!(st.foreign_keys.len(), 1);
        let fk = &st.foreign_keys[0];
        assert_eq!((fk.cols.as_slice(), fk.ref_db.as_str(), fk.ref_table.as_str(), fk.ref_cols.as_slice(), fk.on_update.as_str(), fk.on_delete.as_str()), (&["customer_id".to_string()][..], "t4_struct", "parent", &["id".to_string()][..], "CASCADE", "RESTRICT"));
        assert_eq!(st.referenced_by.len(), 1);
        let r = &st.referenced_by[0];
        assert_eq!((r.table.as_str(), r.cols.as_slice(), r.ref_cols.as_slice(), r.on_delete.as_str()), ("items", &["oa".to_string(), "ob".to_string()][..], &["a".to_string(), "b".to_string()][..], "CASCADE"));
        assert_eq!(st.triggers.len(), 1);
        let t = &st.triggers[0];
        assert_eq!((t.name.as_str(), t.timing.as_str(), t.event.as_str()), ("orders_bu", "BEFORE", "INSERT OR UPDATE"));
        assert!(t.body.starts_with("CREATE TRIGGER orders_bu") && t.body.contains("EXECUTE FUNCTION"), "{}", t.body);
        assert_eq!(t.create_sql("orders"), t.body);
        assert_eq!(st.status.comment, "Customer orders");
        assert!(st.status.engine.is_empty() && st.status.auto_increment.is_none());
        assert!(st.status.data.is_some() && st.status.index.is_some() && st.status.total.is_some_and(|t| t > 0), "{:?}", st.status);
        assert!(st.status.rows.is_some(), "{var}");
        // The other direction.
        let pst = structure(l, &db, "t4_struct", "parent");
        assert!(pst.foreign_keys.is_empty() && pst.referenced_by.len() == 1 && pst.referenced_by[0].table == "orders");
        // The CREATE TABLE text runs back and gives the same table.
        assert!(st.create.starts_with("CREATE TABLE \"t4_struct\".\"orders\" ("), "{}", st.create);
        let names = |s: &super::Structure| (s.columns.iter().map(|c| (c.name.clone(), c.full_type.clone(), c.nullable, c.default.clone())).collect::<Vec<_>>(), s.indexes.iter().map(|i| i.name.clone()).collect::<Vec<_>>(), s.constraints.len(), s.foreign_keys.len());
        let before = names(&st);
        l.qsql(&p, "DROP TABLE t4_struct.items; DROP TABLE t4_struct.orders");
        l.qsql(&p, &st.create);
        let again = structure(l, &db, "t4_struct", "orders");
        assert_eq!(names(&again), before, "{var}\n{}", st.create);
        assert_eq!(again.status.comment, "Customer orders");
    });
}

#[test]
fn views_and_unknown_row_estimates() {
    every_pg(|l, var| {
        let (db, p) = fixture(l, "t4_view", "CREATE TABLE t (id int PRIMARY KEY, status text); INSERT INTO t SELECT i, CASE WHEN i % 4 = 0 THEN 'open' ELSE 'paid' END FROM generate_series(1, 120) i; CREATE VIEW v AS SELECT id, status FROM t WHERE id > 5; CREATE MATERIALIZED VIEW m AS SELECT id FROM t; CREATE INDEX m_id ON m (id);");
        // Never analyzed: the planner has no count (-1 from 14, 0 on 13), so the reader counts.
        let est: f64 = l.qone(&p, "SELECT reltuples::text FROM pg_class WHERE oid = 't4_view.t'::regclass").parse().unwrap();
        let st = structure(l, &db, "t4_view", "t");
        if est <= 0.0 {
            assert_eq!(st.status.rows, Some(120), "{var}");
        } else {
            assert!(st.status.rows.is_some());
        }
        let v = structure(l, &db, "t4_view", "v");
        assert!(v.create.starts_with("CREATE VIEW \"t4_view\".\"v\" AS\n") && v.create.contains("SELECT") && v.create.ends_with(';'), "{var}: {}", v.create);
        assert_eq!(v.columns.len(), 2);
        let m = structure(l, &db, "t4_view", "m");
        assert!(m.create.starts_with("CREATE MATERIALIZED VIEW \"t4_view\".\"m\" AS\n") && m.create.contains("CREATE INDEX m_id ON t4_view.m"), "{}", m.create);
        // The count of a plain page falls back to a real count as well.
        let (_, _, rows, count) = page(l, &db, "t4_view", "t", PageQuery::default(), 50, 0);
        assert_eq!((rows.len(), count), (50, 120), "{var}");
        // Values for autocomplete: grouped by their text, most frequent first, a prefix narrows, a wildcard is plain.
        let ask = |col: &str, prefix: Option<&str>| {
            l.w.sql_field_values(1, db.clone(), "t4_view".into(), "t".into(), col.into(), prefix.map(str::to_string));
            let Msg::SqlValues { res, .. } = l.next() else { panic!("expected SqlValues") };
            res
        };
        assert_eq!(ask("status", None).unwrap().unwrap(), [("paid".to_string(), 90), ("open".to_string(), 30)], "{var}");
        assert_eq!(ask("status", Some("o")).unwrap().unwrap(), [("open".to_string(), 30)]);
        assert!(ask("status", Some("%")).unwrap().unwrap().is_empty());
        assert_eq!(ask("id", Some("11")).unwrap().unwrap().len(), 11);
        assert!(ask("x\" FROM t; --", None).is_err());
    });
}
