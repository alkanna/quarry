use super::*;
use crate::db::bun_missing;
use std::sync::atomic::AtomicU64;
use super::super::relay::fill;
use super::super::{ResultSet, RUN_ROWS_MAX};
use std::sync::mpsc::Receiver;
use std::sync::Mutex;

/// Tests share one database and run in parallel, so the last fixture to go drops it again.
static OPEN: Mutex<usize> = Mutex::new(0);

/// The writable fixture pool. Keep it bound for the whole test: dropping it counts the test as finished.
struct Fixture {
    pool: sqlx::MySqlPool,
    rt: tokio::runtime::Handle,
}

impl std::ops::Deref for Fixture {
    type Target = sqlx::MySqlPool;
    fn deref(&self) -> &sqlx::MySqlPool {
        &self.pool
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let mut n = OPEN.lock().unwrap_or_else(|e| e.into_inner());
        *n -= 1;
        if *n == 0 {
            // Dropping the database takes its views with it.
            let _ = self.rt.block_on(raw_sql("DROP DATABASE IF EXISTS quarry_test").execute(&self.pool));
        }
    }
}

/// Needs a server: QUARRY_TEST_MYSQL, like mysql://root:quarry@127.0.0.1:3306. Skipped when unset.
/// Returns a worker, its channel, a connected Db whose queries time out after 30 s, and a plain
/// writable pool for fixtures (the Db's own session is read-only).
fn live(table: &str, ddl: &str) -> Option<(Worker, Receiver<Msg>, Db, Fixture)> {
    let uri = std::env::var("QUARRY_TEST_MYSQL").ok()?;
    *OPEN.lock().unwrap_or_else(|e| e.into_inner()) += 1;
    let (tx, rx) = std::sync::mpsc::channel();
    let w = Worker::new(tx, eframe::egui::Context::default());
    let setup = w.rt.block_on(async {
        let p = sqlx::MySqlPool::connect(&uri).await.unwrap();
        raw_sql("CREATE DATABASE IF NOT EXISTS quarry_test").execute(&p).await.unwrap();
        if !table.is_empty() {
            raw_sql(sqlx::AssertSqlSafe(format!("DROP TABLE IF EXISTS quarry_test.{table}"))).execute(&p).await.unwrap();
            raw_sql(sqlx::AssertSqlSafe(format!("CREATE TABLE quarry_test.{table} ({ddl})"))).execute(&p).await.unwrap();
        }
        p
    });
    w.sql_connect(1, uri, Duration::from_secs(30), Net::default());
    let Msg::SqlConnected { res: Ok((db, dbs, _)), .. } = next(&rx) else { panic!("expected SqlConnected") };
    assert!(dbs.iter().any(|d| d == "quarry_test"), "{dbs:?}");
    let rt = w.rt.handle().clone();
    Some((w, rx, db, Fixture { pool: setup, rt }))
}

fn next(rx: &Receiver<Msg>) -> Msg {
    rx.recv_timeout(Duration::from_secs(15)).expect("worker answer")
}

fn exec(w: &Worker, p: &sqlx::MySqlPool, sql: &str) {
    w.rt.block_on(async { raw_sql(sqlx::AssertSqlSafe(sql)).execute(p).await.unwrap() });
}

fn page_of(w: &Worker, rx: &Receiver<Msg>, db: &Db, table: &str, filter: &str, order: &str, limit: u64, offset: u64) -> (Vec<Col>, Vec<String>, Vec<Document>, u64) {
    page_q(w, rx, db, table, PageQuery { filter: filter.into(), order: order.into(), ..Default::default() }, limit, offset)
}

fn page_q(w: &Worker, rx: &Receiver<Msg>, db: &Db, table: &str, q: PageQuery, limit: u64, offset: u64) -> (Vec<Col>, Vec<String>, Vec<Document>, u64) {
    w.sql_page(7, db.clone(), "quarry_test".into(), table.into(), q, limit, offset);
    let Msg::SqlCols { cols, pk, .. } = next(rx) else { panic!("expected SqlCols") };
    let Msg::Docs { res: Ok((rows, count, _)), .. } = next(rx) else { panic!("expected Docs") };
    (cols, pk, rows.iter().map(|r| r.decode().unwrap()).collect(), count)
}

#[test]
fn live_page_clauses_select_distinct_group_having_and_index() {
    let Some((w, rx, db, p)) = live("t_clause", "id INT PRIMARY KEY, status VARCHAR(10), total INT, at DATETIME, KEY k_status (status)") else { return };
    let vals: Vec<String> = (0..20).map(|i| format!("({i}, '{}', {}, '2026-11-{:02} 10:00:00')", ["paid", "shipped", "open"][i % 3], i * 10, 1 + i % 4)).collect();
    exec(&w, &p, &format!("INSERT INTO quarry_test.t_clause VALUES {}", vals.join(",")));
    let base = PageQuery::default();
    // A select list: only those columns, and the key is gone with its column.
    let (cols, pk, rows, count) = page_q(&w, &rx, &db, "t_clause", PageQuery { select: vec!["status".into(), "total * 2 AS dbl".into()], order: "total".into(), ..base.clone() }, 5, 0);
    assert_eq!(cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["status", "dbl"]);
    assert!(pk.is_empty());
    assert_eq!((rows.len(), count), (5, 20));
    assert_eq!(rows[1].get_str("status"), Ok("shipped"));
    // DISTINCT counts distinct rows.
    let (_, _, rows, count) = page_q(&w, &rx, &db, "t_clause", PageQuery { select: vec!["status".into()], distinct: true, order: "status".into(), ..base.clone() }, 10, 0);
    assert_eq!((rows.len(), count), (3, 3));
    // LIMIT caps the whole result and OFFSET skips: 7 rows in 5 per page leave 2 on page 2.
    let (_, _, rows, count) = page_q(&w, &rx, &db, "t_clause", PageQuery { order: "id".into(), limit: Some(7), skip: 2, ..base.clone() }, 5, 5);
    assert_eq!((rows.len(), count), (2, 7));
    assert_eq!(rows[0].get_i64("id"), Ok(7));
    // GROUP BY with an aggregate and HAVING: counts are groups, rows have no key.
    let g = PageQuery { select: vec!["status".into(), "COUNT(*) AS n".into(), "SUM(total) AS sum_total".into()], group: vec!["status".into()], having: "n > 6".into(), order: "n DESC".into(), ..base.clone() };
    let (cols, pk, rows, count) = page_q(&w, &rx, &db, "t_clause", g.clone(), 10, 0);
    assert_eq!(cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["status", "n", "sum_total"]);
    assert!(pk.is_empty() && rows[0].get("_id").is_none());
    assert_eq!((rows.len(), count), (2, 2));
    assert_eq!(rows[0].get_str("status"), Ok("paid"));
    w.sql_page(8, db.clone(), "quarry_test".into(), "t_clause".into(), g, 10, 0);
    let Msg::SqlCols { matched, .. } = next(&rx) else { panic!("expected SqlCols") };
    assert_eq!(matched, Some(20));
    let _ = next(&rx);
    // A date grain groups by the day.
    let day = crate::db::sql::rules::Grain::Day.expr("at", db.dialect);
    let (_, _, rows, count) = page_q(&w, &rx, &db, "t_clause", PageQuery { select: vec![format!("{day} AS day"), "COUNT(*) AS n".into()], group: vec!["day".into()], order: "day".into(), ..base.clone() }, 10, 0);
    assert_eq!((rows.len(), count), (4, 4));
    // The index hint is quoted, and a bad name is the server's error, not an injection.
    let (_, _, rows, _) = page_q(&w, &rx, &db, "t_clause", PageQuery { filter: "status = 'paid'".into(), index_hint: "k_status".into(), ..base.clone() }, 100, 0);
    assert_eq!(rows.len(), 7);
    w.sql_page(9, db.clone(), "quarry_test".into(), "t_clause".into(), PageQuery { index_hint: "k) ; DROP TABLE t_clause; --".into(), ..base }, 10, 0);
    let Msg::Docs { res, .. } = next(&rx) else { panic!("expected Docs") };
    assert!(res.is_err());
    assert_eq!(page_of(&w, &rx, &db, "t_clause", "", "", 100, 0).3, 20);
}

fn run_all(w: &Worker, rx: &Receiver<Msg>, db: &Db, text: &str, read_only: bool) -> Vec<ResultSet> {
    w.sql_run(9, db.clone(), "quarry_test".into(), text.into(), read_only, 0, Default::default());
    let Msg::SqlRan { sets, .. } = next(rx) else { panic!("expected SqlRan") };
    sets
}

#[test]
fn live_connect_reports_version_dialect_and_tls() {
    let Some((w, rx, db, _p)) = live("", "") else { return };
    println!("{} tls={} mode={}", db.label(), db.tls, db.sql_mode);
    assert!(db.version.chars().next().unwrap().is_ascii_digit());
    assert_eq!(db.dialect, Dialect::from_version(&db.version));
    assert!(db.label().starts_with("MySQL ") || db.label().starts_with("MariaDB "));
    let uri = std::env::var("QUARRY_TEST_MYSQL").unwrap();
    w.sql_test(uri, Duration::from_secs(10), Net::default());
    let Msg::Tested { res: Ok(m), .. } = next(&rx) else { panic!("expected Tested") };
    assert!(m.starts_with("Connected to ") && m.contains(&db.label()) && m.ends_with(" ms"), "{m}");
    w.sql_test("mysql://root:wrong@127.0.0.1:3306".into(), Duration::from_secs(10), Net::default());
    let Msg::Tested { res, .. } = next(&rx) else { panic!("expected Tested") };
    assert!(res.is_err());
}

#[test]
fn live_tables_lists_views_apart() {
    let Some((w, rx, db, p)) = live("t_list", "id INT PRIMARY KEY") else { return };
    exec(&w, &p, "DROP VIEW IF EXISTS quarry_test.v_list");
    exec(&w, &p, "CREATE VIEW quarry_test.v_list AS SELECT id FROM quarry_test.t_list");
    w.sql_tables(1, db, "quarry_test".into());
    let Msg::Collections { res: Ok((names, views)), .. } = next(&rx) else { panic!("expected Collections") };
    assert!(names.contains(&"t_list".to_string()) && names.contains(&"v_list".to_string()));
    assert!(views.contains(&"v_list".to_string()) && !views.contains(&"t_list".to_string()));
}

#[test]
fn live_schema_lists_columns_keys_and_estimates() {
    let Some((w, rx, db, p)) = live("t_sch_parent", "id INT UNSIGNED PRIMARY KEY") else { return };
    exec(&w, &p, "DROP TABLE IF EXISTS quarry_test.t_sch_child");
    exec(&w, &p, "CREATE TABLE quarry_test.t_sch_child (id INT PRIMARY KEY, parent_id INT UNSIGNED NOT NULL, status ENUM('a','b'), note TEXT, KEY k_parent (parent_id), CONSTRAINT fk_parent FOREIGN KEY (parent_id) REFERENCES t_sch_parent (id))");
    exec(&w, &p, "DROP VIEW IF EXISTS quarry_test.v_sch");
    exec(&w, &p, "CREATE VIEW quarry_test.v_sch AS SELECT id FROM quarry_test.t_sch_parent");
    w.sql_schema(1, db, "quarry_test".into());
    let Msg::SqlSchema { schema, res: Ok(s), .. } = next(&rx) else { panic!("expected SqlSchema") };
    assert_eq!(schema, "quarry_test");
    let child = s.table("t_sch_child").unwrap();
    assert!(!child.view && child.rows.is_some());
    let col = |n: &str| child.col(n).unwrap();
    assert_eq!((col("id").key.as_str(), col("id").nullable, col("id").fk), ("PRI", false, false));
    assert_eq!((col("parent_id").ty.as_str(), col("parent_id").kind, col("parent_id").fk), ("int unsigned", Kind::UInt, true));
    assert_eq!((col("status").kind, col("status").nullable), (Kind::Enum, true));
    assert_eq!(crate::db::sql::schema::enum_values(&col("status").ty), ["a", "b"]);
    let view = s.table("v_sch").unwrap();
    assert!(view.view && view.rows.is_none() && view.cols.len() == 1);
    assert_eq!(child.cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "parent_id", "status", "note"]);
}

#[test]
fn live_column_values_read_the_whole_query() {
    let Some((w, rx, db, p)) = live("t_colvals", "id INT PRIMARY KEY, `a.b` DECIMAL(6,2), at DATETIME") else { return };
    exec(&w, &p, "INSERT INTO quarry_test.t_colvals VALUES (1, 1.50, '2026-01-01 00:00:00'), (2, 2.50, '2026-01-03 06:00:00'), (3, NULL, NULL), (4, 2.50, '2026-01-02 00:00:00')");
    let run = |op, col: &str, q: PageQuery| {
        w.sql_column_values(1, db.clone(), "quarry_test".into(), "quarry_test".into(), "t_colvals".into(), q, op, col.into(), col.into());
        next(&rx)
    };
    // The LIMIT chip is ignored, the dotted name is one column, and DECIMAL text counts as numbers.
    let Msg::ColumnValues { res: Ok((ColOut::Notice(s), false)), .. } = run(ColOp::Sum, "a.b", PageQuery { limit: Some(1), ..Default::default() }) else { panic!("expected a sum") };
    assert!(s.starts_with("6.5\n") && s.contains("3 of 4 values are numbers"), "{s}");
    let Msg::ColumnValues { res: Ok((ColOut::Notice(s), _)), .. } = run(ColOp::Range, "at", PageQuery { filter: "id < 3".into(), ..Default::default() }) else { panic!("expected a range") };
    assert!(s.contains("2026-01-01T00:00:00") && s.contains("Span 2d 6h"), "{s}");
    // Groups keep the exact text, and a SQL table has no pipeline to open.
    let Msg::Grouped { res: Ok(rows), pipeline, .. } = run(ColOp::Group, "a.b", PageQuery::default()) else { panic!("expected groups") };
    assert!(pipeline.is_empty());
    assert_eq!(rows[0], mongodb::bson::doc! { "_id": "2.50", "count": 2i64 });
    let Msg::ColumnValues { res: Ok((ColOut::Copy(text, 3), _)), .. } = run(ColOp::CopyDistinct, "a.b", PageQuery { order: "id".into(), ..Default::default() }) else { panic!("expected a copy") };
    assert_eq!(text, "1.50\n2.50\n");
}

#[test]
fn live_field_values_count_and_respect_the_gate() {
    let Some((w, rx, db, p)) = live("t_vals", "id INT PRIMARY KEY, status VARCHAR(10), n INT, note VARCHAR(10), KEY k_status (status)") else { return };
    let vals: Vec<String> = (0..10).map(|i| format!("({i}, '{}', {}, NULL)", if i < 6 { "paid" } else if i < 9 { "open" } else { "it''s" }, i % 2)).collect();
    exec(&w, &p, &format!("INSERT INTO quarry_test.t_vals VALUES {}", vals.join(",")));
    let ask = |col: &str, prefix: Option<&str>| {
        w.sql_field_values(1, db.clone(), "quarry_test".into(), "t_vals".into(), col.into(), prefix.map(str::to_string));
        let Msg::SqlValues { res, column, .. } = next(&rx) else { panic!("expected SqlValues") };
        assert_eq!(column, col);
        res
    };
    // Most frequent first, an integer column comes back as text, NULLs are left out.
    assert_eq!(ask("status", None).unwrap().unwrap(), [("paid".to_string(), 6), ("open".to_string(), 3), ("it's".to_string(), 1)]);
    assert_eq!(ask("n", None).unwrap().unwrap().len(), 2);
    assert!(ask("note", None).unwrap().unwrap().is_empty());
    assert_eq!(ask("status", Some("p")).unwrap().unwrap(), [("paid".to_string(), 6)]);
    // A wildcard typed by the user is a plain character.
    assert!(ask("status", Some("%")).unwrap().unwrap().is_empty());
    // A name that is not a column is the server's error, not an injection.
    assert!(ask("x` FROM t_vals; --", None).is_err());
    // Big estimate and no leading index: not looked up. The estimate is faked with a stats-free table of many rows.
    exec(&w, &p, "DROP TABLE IF EXISTS quarry_test.t_big");
    exec(&w, &p, "CREATE TABLE quarry_test.t_big (id INT PRIMARY KEY, a INT, b INT, KEY k_ab (a, b))");
    exec(&w, &p, "SET SESSION cte_max_recursion_depth = 200000; INSERT INTO quarry_test.t_big WITH RECURSIVE s(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM s WHERE i < 150000) SELECT i, i % 7, i % 5 FROM s");
    exec(&w, &p, "ANALYZE TABLE quarry_test.t_big");
    let big = |col: &str| {
        w.sql_field_values(1, db.clone(), "quarry_test".into(), "t_big".into(), col.into(), None);
        let Msg::SqlValues { res, .. } = next(&rx) else { panic!("expected SqlValues") };
        res
    };
    assert_eq!(big("b").unwrap(), None);
    assert_eq!(big("a").unwrap().unwrap().len(), 7);
}

#[test]
fn live_page_filters_orders_and_carries_the_primary_key() {
    let Some((w, rx, db, p)) = live("t_page", "a INT, b VARCHAR(20) COLLATE utf8mb4_bin, n INT, PRIMARY KEY (b, a)") else { return };
    let vals: Vec<String> = (0..30).map(|i| format!("({i}, 'k{i}', {})", i % 3)).collect();
    exec(&w, &p, &format!("INSERT INTO quarry_test.t_page VALUES {}", vals.join(",")));
    let (cols, pk, rows, count) = page_of(&w, &rx, &db, "t_page", "n = 1", "a DESC -- trailing comment", 4, 2);
    assert_eq!(count, 10);
    assert_eq!(pk, ["b", "a"]);
    assert_eq!(cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["a", "b", "n"]);
    // A _bin text column is reported as binary by sqlx, the column's declared type keeps it text.
    assert_eq!(cols[1].kind, Kind::Text);
    assert_eq!(rows.iter().map(|d| d.get_i64("a").unwrap()).collect::<Vec<_>>(), [22, 19, 16, 13]);
    assert_eq!(rows[0].get_str("b"), Ok("k22"));
    assert_eq!(rows[0].get_document("_id").unwrap(), &mongodb::bson::doc! { "b": "k22", "a": 22_i64 });
    // An empty page still has its columns, and injection through the filter is refused.
    let (cols, _, rows, count) = page_of(&w, &rx, &db, "t_page", "n = 99", "", 10, 0);
    assert_eq!((cols.len(), rows.len(), count), (3, 0, 0));
    w.sql_page(8, db.clone(), "quarry_test".into(), "t_page".into(), PageQuery { filter: "1=1; DELETE FROM t_page".into(), ..Default::default() }, 10, 0);
    let Msg::Docs { res, .. } = next(&rx) else { panic!("expected Docs") };
    assert!(res.is_err());
    // The rows are all still there.
    assert_eq!(page_of(&w, &rx, &db, "t_page", "", "", 100, 0).3, 30);
}

#[test]
fn live_every_column_type_converts() {
    let ddl = "id INT PRIMARY KEY, i INT, bu BIGINT UNSIGNED, bi BIGINT, d DECIMAL(10,2), f DOUBLE, v VARCHAR(20), t TEXT, b BLOB, dt DATE, dtt DATETIME, tm TIME, y YEAR, j JSON, e ENUM('a','b'), s SET('a','b','c'), bt BIT(8)";
    let Some((w, rx, db, p)) = live("t_types", ddl) else { return };
    exec(&w, &p, "INSERT INTO quarry_test.t_types VALUES (1, -5, 18446744073709551615, 9007199254740993, 12.50, 1.5, 'héllo', 'long text', 0x0001FF, '2024-02-29', '2024-02-29 13:14:15', '-12:34:56', 2024, '{\"a\": [1, 2]}', 'b', 'a,c', b'101')");
    exec(&w, &p, "INSERT INTO quarry_test.t_types (id) VALUES (2)");
    let (cols, _, rows, _) = page_of(&w, &rx, &db, "t_types", "", "id", 10, 0);
    let bin = |b: &[u8]| Bson::Binary(mongodb::bson::Binary { subtype: mongodb::bson::spec::BinarySubtype::Generic, bytes: b.to_vec() });
    let r = &rows[0];
    assert_eq!(r.get("i"), Some(&Bson::Int64(-5)));
    // Above i64::MAX stays exact as text.
    assert_eq!(r.get("bu"), Some(&Bson::String("18446744073709551615".into())));
    assert_eq!(r.get("bi"), Some(&Bson::Int64(9007199254740993)));
    assert_eq!(r.get("d"), Some(&Bson::String("12.50".into())));
    assert_eq!(r.get("f"), Some(&Bson::Double(1.5)));
    assert_eq!(r.get_str("v"), Ok("héllo"));
    assert_eq!(r.get_str("t"), Ok("long text"));
    assert_eq!(r.get("b"), Some(&bin(&[0, 1, 255])));
    assert_eq!(r.get_str("dt"), Ok("2024-02-29"));
    assert_eq!(r.get_str("dtt"), Ok("2024-02-29 13:14:15"));
    assert_eq!(r.get_str("tm"), Ok("-12:34:56"));
    assert_eq!(r.get("y"), Some(&Bson::Int64(2024)));
    assert!(r.get_str("j").unwrap().contains("\"a\""));
    assert_eq!((r.get_str("e"), r.get_str("s")), (Ok("b"), Ok("a,c")));
    assert_eq!(r.get("bt"), Some(&bin(&[5])));
    let n = &rows[1];
    for c in cols.iter().filter(|c| c.name != "id") {
        assert_eq!(n.get(&c.name), Some(&Bson::Null), "{}", c.name);
        assert!(c.nullable, "{}", c.name);
    }
    let kinds: Vec<Kind> = cols.iter().map(|c| c.kind).collect();
    // MariaDB stores JSON as LONGTEXT with a json_valid CHECK, which is read back as JSON.
    assert_eq!(kinds, [Kind::Int, Kind::Int, Kind::UInt, Kind::Int, Kind::Decimal, Kind::Float, Kind::Text, Kind::Text, Kind::Binary, Kind::Date, Kind::DateTime, Kind::Time, Kind::Year, Kind::Json, Kind::Enum, Kind::Set, Kind::Bit]);
    assert!(!cols[0].nullable);
}

#[test]
fn fill_binds_values_as_escaped_literals() {
    use value::Param;
    let f = |t: &str, p: &[Param], nb: bool| fill(t, p, !nb, false);
    assert_eq!(f("SELECT ? , '?', `?` -- ?\n, ?", &[Param::Int(1), Param::Null], false).unwrap(), "SELECT  1  , '?', `?` -- ?\n,  NULL ");
    assert_eq!(f("WHERE a = ?", &[Param::Text("it's \\ x".into())], false).unwrap(), "WHERE a =  'it''s \\\\ x' ");
    assert_eq!(f("WHERE a = ?", &[Param::Text("a\\".into())], true).unwrap(), "WHERE a =  'a\\' ");
    assert_eq!(f("?, ?", &[Param::Bytes(vec![0, 255]), Param::Float(1.5)], false).unwrap(), " X'00ff' ,  1.5e0 ");
    // A literal never fuses with the word next to it.
    assert_eq!(f("SELECT X?, a?, @?", &[Param::Int(1), Param::Int(2), Param::Int(3)], false).unwrap(), "SELECT X 1 , a 2 , @ 3 ");
    // A value that tries to end the string stays inside it, so the text is still one statement.
    let evil = f("SELECT ?", &[Param::Text("'; DROP TABLE t; --".into())], false).unwrap();
    assert_eq!(lex::split_statements(&evil, Dialect::MySql).len(), 1);
    assert!(f("?", &[], false).is_err() && f("1", &[Param::Null], false).is_err());
    assert!(f("?", &[Param::Float(f64::NAN)], false).is_err() && f("?", &[Param::Text("a\0".into())], false).is_err());
}

/// Runs the runner with this input line and these SQL relays, returning the queries it reported and its result.
fn shell_run(w: &Worker, rx: &Receiver<Msg>, input: serde_json::Value, sql: Vec<(String, crate::db::SqlRelay)>) -> (Vec<crate::db::ShellQuery>, Res<String>) {
    let (queries, res, _) = shell_run_timed(w, rx, input, sql);
    (queries, res)
}

/// Calls run_shell itself rather than Worker::shell, whose ShellDone carries the parsed value: these tests read the runner's text.
fn shell_run_timed(w: &Worker, _rx: &Receiver<Msg>, input: serde_json::Value, sql: Vec<(String, crate::db::SqlRelay)>) -> (Vec<crate::db::ShellQuery>, Res<String>, (u128, Option<u64>)) {
    let job = crate::db::ShellJob { runtime: "bun".into(), dir: std::env::temp_dir().join("quarry-shell-live-test"), input: input.to_string(), packages: Vec::new(), sql };
    let start = std::time::Instant::now();
    let (queries, res) = crate::db::shell::run_shell_blocking(w, &job);
    let ms = start.elapsed().as_millis();
    let query_ms = res.as_ref().ok().and_then(|r| r.1);
    (queries, res.and_then(|r| r.0.text()), (ms, query_ms))
}

fn relay(db: &Db, read_only: bool) -> crate::db::SqlRelay {
    crate::db::SqlRelay { db: db.clone(), schema: "quarry_test".into(), read_only, conn_id: Default::default() }
}

/// Runs a shell script on a SQL relay, returning the queries it sent and its result.
fn shell_script(w: &Worker, rx: &Receiver<Msg>, db: &Db, code: &str, tables: &[&str], read_only: bool) -> (Vec<crate::db::ShellQuery>, Res<String>) {
    let input = serde_json::json!({ "code": code, "packages": [], "timeoutMs": 30000, "main": "shop", "mongo": [], "sql": [{ "name": "shop", "db": "quarry_test", "tables": tables }] });
    shell_run(w, rx, input, vec![("shop".into(), relay(db, read_only))])
}

fn query_names(queries: &[crate::db::ShellQuery]) -> String {
    queries.iter().map(|q| format!("{} {}", q.conn, q.text)).collect::<Vec<_>>().join(" | ")
}

#[test]
fn live_shell_second_sql_connection_keeps_its_own_read_only_and_names_its_queries() {
    let Some((w, rx, db, p)) = live("t_att", "id INT PRIMARY KEY, note VARCHAR(20)") else { return };
    if bun_missing() {
        return;
    }
    exec(&w, &p, "INSERT INTO quarry_test.t_att VALUES (1, 'a')");
    let script = "async function run() {
  const [{ one }] = await other.sql`SELECT 1 AS one`;
  let refused = null;
  try { await other.sql`UPDATE t_att SET note = 'x'` } catch (e) { refused = e.message }
  let viaTable = null;
  try { await other.t_att.update({ id: 1 }, { note: 'y' }) } catch (e) { viaTable = e.message }
  const seen = await other.t_att.find({});
  await t_att.update({ id: 1 }, { note: 'main' });
  return { one, refused, viaTable, seen: seen.length, now: (await other.sql.unsafe('SELECT note FROM t_att'))[0].note, hasTable: typeof other.table };
}";
    // Tables named like function properties must not replace them.
    let input = serde_json::json!({ "code": script, "packages": [], "timeoutMs": 30000, "main": "shop", "mongo": [], "sql": [
        { "name": "shop", "db": "quarry_test", "tables": ["t_att"] }, { "name": "other", "db": "quarry_test", "tables": ["t_att", "call", "bind"] }] });
    let (queries, res) = shell_run(&w, &rx, input, vec![("shop".into(), relay(&db, false)), ("other".into(), relay(&db, true))]);
    let v: serde_json::Value = serde_json::from_str(&res.expect("script result")).unwrap();
    let v = &v["v"];
    assert_eq!(v["one"], 1, "{v}");
    assert!(v["refused"].as_str().is_some_and(|m| !m.is_empty()) && v["viaTable"].as_str().is_some_and(|m| !m.is_empty()), "{v}");
    assert_eq!(v["seen"], 1);
    assert_eq!(v["hasTable"], "function");
    // The unlocked main connection wrote, the locked one did not.
    assert_eq!(v["now"], "main");
    assert!(queries.iter().any(|q| q.conn == "other" && q.error.is_some()) && queries.iter().any(|q| q.conn == "shop" && q.error.is_none() && q.affected == Some(1)), "{}", query_names(&queries));
    assert!(queries.iter().all(|q| q.conn == "shop" || q.conn == "other"));
}

fn sql_entry(name: &str, tables: &[&str]) -> serde_json::Value {
    serde_json::json!({ "name": name, "db": "quarry_test", "tables": tables })
}

#[test]
fn live_shell_refuses_repeated_connection_names() {
    let Some((w, rx, db, _p)) = live("t_dup", "id INT PRIMARY KEY") else { return };
    if bun_missing() {
        return;
    }
    let code = "async function run() { return await shop.sql`SELECT 1` }";
    // The runner refuses the repeat when Rust did not catch it.
    let input = serde_json::json!({ "code": code, "packages": [], "timeoutMs": 30000, "main": "shop", "mongo": [], "sql": [sql_entry("shop", &[]), sql_entry("shop", &[])] });
    let (_, res) = shell_run(&w, &rx, input.clone(), vec![("shop".into(), relay(&db, true))]);
    assert!(res.unwrap_err().contains("more than once"));
    // run_shell refuses before Bun starts.
    let (queries, res) = shell_run(&w, &rx, input, vec![("shop".into(), relay(&db, true)), ("shop".into(), relay(&db, true))]);
    assert!(res.unwrap_err().contains("more than once") && queries.is_empty());
    // A Mongo entry and an SQL entry share a name; no server is reached because the check comes first.
    let input = serde_json::json!({ "code": code, "packages": [], "timeoutMs": 30000, "main": "x",
        "mongo": [{ "name": "x", "uri": "mongodb://127.0.0.1:1", "db": "d", "proxy": null, "collections": [], "readOnly": true }], "sql": [sql_entry("x", &[])] });
    let (_, res) = shell_run(&w, &rx, input, vec![("x".into(), relay(&db, true))]);
    assert!(res.unwrap_err().contains("more than once"));
}

#[test]
fn live_shell_results_keep_invalid_dates_and_proto_keys() {
    let Some((w, rx, db, _p)) = live("t_dates", "id INT PRIMARY KEY") else { return };
    if bun_missing() {
        return;
    }
    let script = "async function run() { return { bad: new Date(NaN), old: new Date(-62198755200000 - 86400000*400), far: new Date(8.64e15), ok: new Date(0), p: JSON.parse('{\"__proto__\":{\"a\":1},\"b\":2}') } }";
    let check = |res: Res<String>| {
        let v = decode(&res.expect("script result"));
        assert_eq!(v.get("bad"), Some(&Bson::Null), "{v}");
        assert_eq!(v.get("old"), Some(&Bson::Null));
        assert_eq!(v.get("far"), Some(&Bson::Null));
        assert!(matches!(v.get("ok"), Some(Bson::DateTime(_))));
        let p = v.get_document("p").unwrap();
        assert_eq!(p.get_i32("b").unwrap(), 2);
        assert_eq!(p.get_document("__proto__").unwrap().get_i32("a").unwrap(), 1, "{p}");
    };
    let input = serde_json::json!({ "code": script, "packages": [], "timeoutMs": 30000, "main": "shop", "mongo": [], "sql": [sql_entry("shop", &[])] });
    check(shell_run(&w, &rx, input, vec![("shop".into(), relay(&db, true))]).1);
    // With the driver loaded the dates are spelled differently.
    let Some(uri) = mongo_user(&w, "quarry_attach_c") else { return };
    let input = serde_json::json!({ "code": script, "packages": [], "timeoutMs": 30000, "main": "shop",
        "mongo": [{ "name": "travels", "uri": uri, "db": "quarry_attach_c", "proxy": null, "collections": [], "readOnly": true }], "sql": [sql_entry("shop", &[])] });
    check(shell_run(&w, &rx, input, vec![("shop".into(), relay(&db, true))]).1);
}

#[test]
fn live_shell_lists_mongo_queries_only_with_attachments_and_words_them_right() {
    let Some((w, rx, db, _p)) = live("t_mq", "id INT PRIMARY KEY") else { return };
    let Some(uri) = mongo_user(&w, "quarry_attach_d") else { return };
    if bun_missing() {
        return;
    }
    let mongo = serde_json::json!([{ "name": "docs", "uri": uri, "db": "quarry_attach_d", "proxy": null, "collections": ["users"], "readOnly": false }]);
    // One connection: no Mongo query lines.
    let input = serde_json::json!({ "code": "async function run() { return { n: (await db.collection('users').find({}).toArray()).length } }", "packages": [], "timeoutMs": 30000, "main": "docs", "mongo": mongo, "sql": [] });
    let (queries, res, (ms, query_ms)) = shell_run_timed(&w, &rx, input, vec![]);
    assert_eq!(decode(&res.expect("script result")).get_i32("n").unwrap(), 1);
    assert!(queries.is_empty(), "{}", query_names(&queries));
    // The query time leaves out starting Bun and connecting, even without command monitoring.
    assert!(query_ms.is_some_and(|q| u128::from(q) < ms), "{query_ms:?} of {ms} ms");
    // The two long filters put an emoji across the 2000 character cut in one of the two parities.
    let script = "async function run() {
  const u = db.collection('users');
  await u.insertMany([{ k: 1 }, { k: 2 }, { k: 3 }]);
  await db.command({ count: 'users' });
  await u.find({}).batchSize(1).toArray();
  await u.find({}).toArray();
  try { await db.command({ dropUser: 'nobody' }) } catch (e) {}
  for (const n of [0, 1]) await u.find({ t: 'a'.repeat(1980 + n) + '😀'.repeat(300) }).toArray();
  return { ok: true };
}";
    let input = serde_json::json!({ "code": script, "packages": [], "timeoutMs": 30000, "main": "docs", "mongo": mongo, "sql": [sql_entry("shop", &[])] });
    let (queries, res) = shell_run(&w, &rx, input, vec![("shop".into(), relay(&db, false))]);
    res.expect("script result");
    let ours: Vec<_> = queries.iter().filter(|q| q.conn == "docs").collect();
    let names = query_names(&queries);
    // count reports rows, never affected.
    assert!(ours.iter().any(|q| q.text.starts_with("users.count(") && q.rows == Some(4) && q.affected.is_none()), "{names}");
    assert!(ours.iter().any(|q| q.text.starts_with("users.insert(") && q.affected == Some(3)), "{names}");
    // A multi-batch find omits rows; a one-batch find has them.
    assert!(ours.iter().any(|q| q.text == "users.find({})" && q.rows.is_none() && q.error.is_none()), "{names}");
    assert!(ours.iter().any(|q| q.text == "users.find({})" && q.rows == Some(4)), "{names}");
    // A string argument is not a collection, and the error is text.
    let drop = ours.iter().find(|q| q.text.starts_with("db.dropUser(")).unwrap_or_else(|| panic!("{names}"));
    assert!(drop.error.as_deref().is_some_and(|e| !e.is_empty() && !e.contains("[object Object]")), "{:?}", drop.error);
    // Both cuts arrive as queries, so neither held a lone surrogate that serde_json refuses.
    assert_eq!(ours.iter().filter(|q| q.text.starts_with("users.find({\"t\"") && q.text.len() > 1900).count(), 2, "{names}");
}

#[test]
fn live_shell_attachment_names_cannot_replace_globals() {
    let Some((w, rx, db, _p)) = live("t_glob", "id INT PRIMARY KEY") else { return };
    if bun_missing() {
        return;
    }
    let code = "async function run() { return 1 }";
    for (clash, attach_first) in [("_", false), ("dayjs", false), ("t_glob", false), ("t_glob", true)] {
        let (main, other) = (sql_entry("shop", &["t_glob"]), sql_entry(clash, &[]));
        let list = if attach_first { vec![other, main] } else { vec![main, other] };
        let input = serde_json::json!({ "code": code, "packages": [], "timeoutMs": 30000, "main": "shop", "mongo": [], "sql": list });
        let (_, res) = shell_run(&w, &rx, input, vec![("shop".into(), relay(&db, true)), (clash.into(), relay(&db, true))]);
        let err = res.unwrap_err();
        assert!(err.contains(&format!("\"{clash}\"")) && err.contains("already defines"), "{clash}: {err}");
    }
}

#[test]
fn live_shell_unknown_sql_connection_name_answers_an_error() {
    let Some((w, rx, db, _p)) = live("t_att2", "id INT PRIMARY KEY") else { return };
    if bun_missing() {
        return;
    }
    // The input lists a connection the job has no relay for, as a stale attachment would.
    let input = serde_json::json!({ "code": "async function run() { return await ghost.sql`SELECT 1` }", "packages": [], "timeoutMs": 30000, "main": "shop", "mongo": [], "sql": [
        { "name": "shop", "db": "quarry_test", "tables": [] }, { "name": "ghost", "db": "quarry_test", "tables": [] }] });
    let (_, res) = shell_run(&w, &rx, input, vec![("shop".into(), relay(&db, true))]);
    assert!(res.unwrap_err().contains("ghost"));
}

/// Needs QUARRY_TEST_MONGO too. Seeds one document and returns the server's uri.
fn mongo_user(w: &Worker, db: &str) -> Option<String> {
    let uri = std::env::var("QUARRY_TEST_MONGO").ok()?;
    w.rt.block_on(async {
        let c = mongodb::Client::with_uri_str(&uri).await.unwrap().database(db).collection::<Document>("users");
        c.drop().await.unwrap();
        c.insert_one(mongodb::bson::doc! { "legacyId": 7, "name": "Ada" }).await.unwrap();
    });
    Some(uri)
}

/// What poll.rs does with a result line.
fn decode(res: &str) -> Document {
    let v: serde_json::Value = serde_json::from_str(res).unwrap();
    let Bson::Document(mut d) = Bson::try_from(v).unwrap() else { panic!("not a document") };
    let Some(Bson::Document(v)) = d.remove("v") else { panic!("v is not a document") };
    v
}

#[test]
fn live_shell_sql_main_reads_an_attached_mongo_and_mixes_types_in_one_result() {
    let Some((w, rx, db, _p)) = live("t_att3", "id INT PRIMARY KEY") else { return };
    let Some(uri) = mongo_user(&w, "quarry_attach_a") else { return };
    if bun_missing() {
        return;
    }
    let script = "async function run() {
  const found = await travels.users.find({ legacyId: 7 }).toArray();
  const [row] = await sql`SELECT CAST(9007199254740993 AS SIGNED) AS big, X'0aff' AS bytes`;
  return { oid: ObjectId('507f1f77bcf86cd799439011'), big: row.big, bytes: row.bytes, name: found[0].name };
}";
    // A collection named like a Db method keeps the method.
    let input = serde_json::json!({ "code": script, "packages": [], "timeoutMs": 30000, "main": "shop",
        "mongo": [{ "name": "travels", "uri": uri, "db": "quarry_attach_a", "proxy": null, "collections": ["users", "command"], "readOnly": false }],
        "sql": [{ "name": "shop", "db": "quarry_test", "tables": [] }] });
    let (queries, res) = shell_run(&w, &rx, input, vec![("shop".into(), relay(&db, true))]);
    let v = decode(&res.expect("script result"));
    assert_eq!(v.get_object_id("oid").unwrap().to_hex(), "507f1f77bcf86cd799439011");
    assert_eq!(v.get_i64("big").unwrap(), 9007199254740993);
    assert_eq!(v.get_binary_generic("bytes").unwrap(), &vec![0x0a, 0xff]);
    assert_eq!(v.get_str("name").unwrap(), "Ada");
    assert!(queries.iter().any(|q| q.conn == "travels" && q.text.starts_with("users.find(") && q.text.contains("legacyId") && q.rows == Some(1)), "{}", query_names(&queries));
    assert!(queries.iter().any(|q| q.conn == "shop"));
}

/// The Shell tab sends documents as BSON frames; agents and older runs read one Extended JSON line. A script's
/// result must read the same both ways, whatever mix of SQL rows and Mongo documents it returns.
#[test]
fn live_shell_sql_rows_and_mongo_documents_read_the_same_as_bson_frames() {
    let Some((w, _rx, db, _p)) = live("t_frames", "id INT PRIMARY KEY") else { return };
    let Some(uri) = mongo_user(&w, "quarry_attach_frames") else { return };
    if bun_missing() {
        return;
    }
    let rows = "SELECT 1 AS i, CAST(3000000000 AS SIGNED) AS big, CAST(9007199254740993 AS SIGNED) AS huge, 1.5 AS dbl, CAST('12.34' AS DECIMAL(10,2)) AS price, X'0aff' AS bytes, DATE('2024-01-02') AS d, TIMESTAMP('2024-01-02 03:04:05') AS ts, NULL AS n, 'é😀' AS s, JSON_OBJECT('a', 1) AS j";
    let both = |code: String, mongo: serde_json::Value| {
        let input = |frames: bool| {
            let mut i = serde_json::json!({ "code": code, "packages": [], "timeoutMs": 30000, "main": "shop", "mongo": mongo, "sql": [{ "name": "shop", "db": "quarry_test", "tables": [] }] });
            if frames {
                i["maxBytes"] = crate::db::SHELL_MAX.into();
            }
            crate::db::ShellJob { runtime: "bun".into(), dir: std::env::temp_dir().join("quarry-shell-live-test"), input: i.to_string(), packages: Vec::new(), sql: vec![("shop".into(), relay(&db, true))] }
        };
        let text = crate::db::shell::run_shell_blocking(&w, &input(false)).1.expect("text run").0.text().expect("one line");
        let out = crate::db::shell::run_shell_blocking(&w, &input(true)).1.expect("frames run").0;
        assert!(matches!(out, crate::db::ShellOut::Docs { cut: false, .. }), "the documents came as frames");
        let frames = match out {
            crate::db::ShellOut::Docs { rows, single, .. } if !single => Bson::Array(rows.iter().map(|r| Bson::Document(r.owned())).collect()),
            crate::db::ShellOut::Docs { rows, .. } => Bson::Document(rows[0].owned()),
            crate::db::ShellOut::Value(_) => unreachable!(),
        };
        assert_eq!(frames, crate::db::shell::shell_value(&text).unwrap(), "{code}");
    };
    both(format!("async function run() {{ return await sql`{rows}`; }}"), serde_json::json!([]));
    let mongo = serde_json::json!([{ "name": "travels", "uri": uri, "db": "quarry_attach_frames", "proxy": null, "collections": ["users"], "readOnly": false }]);
    both(format!("async function run() {{ const found = await travels.users.find({{}}).toArray(); const r = await sql`{rows}`; return [...r, ...found, {{ oid: ObjectId('507f1f77bcf86cd799439011'), at: new Date(0), big: r[0].huge }}]; }}"), mongo.clone());
    // One document, not a list.
    both(format!("async function run() {{ const [r] = await sql`{rows}`; return {{ ...r, name: (await travels.users.findOne({{}}))?.name ?? null }}; }}"), mongo);
}

#[test]
fn live_shell_mongo_main_uses_an_attached_sql_connection_and_the_read_only_guard() {
    let Some((w, rx, db, _p)) = live("t_att4", "id INT PRIMARY KEY") else { return };
    let Some(uri) = mongo_user(&w, "quarry_attach_b") else { return };
    if bun_missing() {
        return;
    }
    let script = "async function run() {
  const users = await db.collection('users').find({}).toArray();
  const [{ n }] = await shop.sql`SELECT 41 + 1 AS n`;
  let refused = null;
  try { await db.collection('users').insertOne({ x: 1 }) } catch (e) { refused = e.message }
  return { users: users.length, n, refused };
}";
    let input = serde_json::json!({ "code": script, "packages": [], "timeoutMs": 30000, "main": "docs",
        "mongo": [{ "name": "docs", "uri": uri, "db": "quarry_attach_b", "proxy": null, "collections": ["users"], "readOnly": true }],
        "sql": [{ "name": "shop", "db": "quarry_test", "tables": [] }] });
    let (queries, res) = shell_run(&w, &rx, input, vec![("shop".into(), relay(&db, true))]);
    let v = decode(&res.expect("script result"));
    assert_eq!(v.get_i32("users").unwrap(), 1);
    assert_eq!(v.get_i32("n").unwrap(), 42);
    assert!(v.get_str("refused").unwrap().contains("read-only"));
    assert!(queries.iter().any(|q| q.conn == "docs" && q.text.starts_with("users.find(")) && queries.iter().any(|q| q.conn == "shop"), "{}", query_names(&queries));
}

#[test]
fn live_shell_runs_sql_tables_and_refuses_writes_when_locked() {
    let Some((w, rx, db, p)) = live("t_shell", "id INT PRIMARY KEY, status ENUM('paid','open') NOT NULL, note VARCHAR(20), total DECIMAL(8,2)") else { return };
    if std::process::Command::new("bun").arg("--version").output().is_err() {
        println!("skipped: bun is not installed");
        return;
    }
    exec(&w, &p, "INSERT INTO quarry_test.t_shell VALUES (1, 'paid', 'it''s', 9.50), (2, 'open', NULL, 3.25), (3, 'paid', NULL, 1.00)");
    let script = "async function run() {
  await sql`SET @x = 41`;
  const [{ y }] = await sql`SELECT @x + 1 AS y`;
  const rows = await sql`SELECT status, COUNT(*) AS n FROM t_shell WHERE id IN (${[1, 2, 3]}) AND (note = ${\"it's\"} OR note IS NULL) GROUP BY status ORDER BY status`;
  const found = await t_shell.find({ status: 'paid' }, { orderBy: '-id' });
  const missing = await t_shell.findOne({ id: 99 });
  const count = await t_shell.count({ total: { gt: 2 } });
  console.log('count', count);
  const refused = [];
  for (const f of [() => t_shell.delete({ id: 1 }), () => t_shell.insert({ id: 9, status: 'open' }), () => sql`UPDATE t_shell SET note = 'x'`, () => sql.unsafe('DROP TABLE t_shell')]) {
    try { await f(); refused.push(null) } catch (e) { refused.push(e.message) }
  }
  return { y, rows, found, missing, count, refused, after: await t_shell.count() };
}";
    let (queries, res) = shell_script(&w, &rx, &db, script, &["t_shell"], true);
    let v: serde_json::Value = serde_json::from_str(&res.expect("script result")).unwrap();
    let v = &v["v"];
    assert_eq!(v["y"], 42, "{v}");
    assert_eq!(v["rows"].as_array().unwrap().len(), 2);
    assert_eq!(v["found"].as_array().unwrap().iter().map(|r| r["id"].as_i64().unwrap()).collect::<Vec<_>>(), [3, 1]);
    // DECIMAL stays exact text.
    assert_eq!(v["found"][1]["total"], "9.50");
    assert!(v["missing"].is_null());
    assert_eq!(v["count"], 2);
    for r in v["refused"].as_array().unwrap() {
        assert!(r.as_str().is_some_and(|m| m.contains("read-only") || m.contains("not allowed") || m.contains("Unlock")), "{v}");
    }
    // Nothing was written, and the refused statements never reached the server.
    assert_eq!(v["after"], 3);
    assert!(queries.iter().any(|q| q.error.is_some()) && queries.iter().any(|q| q.rows == Some(2)));
    // Unlocked, the same table methods write.
    let (_, res) = shell_script(&w, &rx, &db, "async function run() { await t_shell.update({ id: 2 }, { note: 'new' }); await t_shell.insert([{ id: 4, status: 'open' }, { id: 5, status: 'open' }]); return await t_shell.count() }", &["t_shell"], false);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&res.unwrap()).unwrap()["v"], 5);
}

#[test]
fn live_run_several_statements_with_an_empty_result() {
    let Some((w, rx, db, p)) = live("t_run", "id INT PRIMARY KEY, name VARCHAR(10)") else { return };
    exec(&w, &p, "INSERT INTO quarry_test.t_run VALUES (1, 'a'), (2, 'b')");
    let sets = run_all(&w, &rx, &db, "SELECT * FROM t_run ORDER BY id; SELECT id, name FROM t_run WHERE id > 99; SELECT 1 + 1 AS two; SET @x = 5; SELECT @x AS x", true);
    assert_eq!(sets.len(), 5);
    assert_eq!((sets[0].rows.len(), sets[0].cols.len()), (2, 2));
    assert!(sets[1].rows.is_empty() && sets[1].affected.is_none() && sets[1].error.is_none());
    assert_eq!(sets[1].cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "name"]);
    assert_eq!(sets[2].rows[0].decode().unwrap().get_i64("two"), Ok(2));
    assert!(sets[3].cols.is_empty() && sets[3].affected == Some(0));
    // The user variable lives on the script's own connection.
    assert_eq!(sets[4].rows[0].decode().unwrap().get_i64("x"), Ok(5));
    // A failing statement stops the script and says why.
    let sets = run_all(&w, &rx, &db, "SELECT 1; SELECT * FROM missing_table; SELECT 3", true);
    assert_eq!(sets.len(), 2);
    assert!(sets[1].error.as_deref().is_some_and(|m| m.contains("missing_table")));
    // A long SELECT is read a page at a time, with a row more asked for to know another page follows.
    let sets = run_all(&w, &rx, &db, "SELECT 1 AS n FROM information_schema.columns a, information_schema.columns b LIMIT 1500", true);
    if sets[0].error.is_none() {
        assert_eq!((sets[0].rows.len(), sets[0].capped), (RUN_ROWS_MAX, false));
        assert_eq!(sets[0].page.map(|p| (p.n, p.next)), Some((0, true)));
    }
}

#[test]
fn live_read_only_refuses_writes_but_runs_selects() {
    let Some((w, rx, db, p)) = live("t_ro", "id INT PRIMARY KEY") else { return };
    exec(&w, &p, "INSERT INTO quarry_test.t_ro VALUES (1)");
    let text = "SELECT COUNT(*) AS n FROM t_ro; DELETE FROM t_ro; SET SESSION transaction_read_only=0; /*!DELETE FROM t_ro*/; SELECT id FROM t_ro";
    let sets = run_all(&w, &rx, &db, text, true);
    assert_eq!(sets.len(), 5);
    assert!(sets[0].error.is_none() && sets[4].error.is_none());
    for i in [1, 2, 3] {
        assert!(sets[i].error.is_some(), "{}", sets[i].stmt);
    }
    assert_eq!(sets[4].rows.len(), 1);
    // Even a statement that slipped past the check would hit the read-only session.
    let sets = run_all(&w, &rx, &db, "SELECT 1", false);
    assert!(sets[0].error.is_none());
    let pool = db.my_pool().unwrap();
    let n: i64 = w.rt.block_on(async { sqlx::query_scalar("SELECT COUNT(*) FROM quarry_test.t_ro").fetch_one(pool).await.unwrap() });
    assert_eq!(n, 1);
    // The unlocked run must not leave READ WRITE on a connection that went back to the pool. 1792 is
    // ER_CANT_EXECUTE_IN_READ_ONLY_TRANSACTION on MySQL and MariaDB alike.
    let err = w.rt.block_on(async { raw_sql("DELETE FROM quarry_test.t_ro").execute(pool).await.unwrap_err() });
    let code = err.as_database_error().and_then(|d| d.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>()).map(|d| d.number());
    assert_eq!(code, Some(1792), "{err}");
    // Unlocked, the same write runs.
    let sets = run_all(&w, &rx, &db, "DELETE FROM t_ro", false);
    assert_eq!(sets[0].affected, Some(1), "{:?}", sets[0].error);
}

#[test]
fn live_kill_stops_a_running_statement() {
    let Some((w, rx, db, _p)) = live("", "") else { return };
    let h = RunHandle::default();
    let (id, cancel) = (h.conn.clone(), h.cancel.clone());
    w.sql_run(3, db.clone(), "quarry_test".into(), "SELECT SLEEP(10)".into(), true, 0, h);
    let start = Instant::now();
    while id.load(Ordering::SeqCst) == 0 {
        assert!(start.elapsed() < Duration::from_secs(5), "no connection id");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(300));
    w.sql_kill(db, id.load(Ordering::SeqCst), cancel);
    let (mut ran, mut info) = (None, None);
    while ran.is_none() || info.is_none() {
        match next(&rx) {
            Msg::SqlRan { sets, .. } => ran = Some(sets),
            Msg::Info(r) => info = Some(r),
            _ => {}
        }
    }
    assert!(info.unwrap().is_ok());
    assert!(start.elapsed() < Duration::from_secs(6), "the sleep was not interrupted");
    assert!(ran.is_some());
    // A finished run resets the id so a late Stop cannot hit another session.
    assert_eq!(id.load(Ordering::SeqCst), 0);
}

#[test]
fn live_stop_keeps_later_statements_from_running() {
    let Some((w, rx, db, p)) = live("t_stop", "id INT PRIMARY KEY") else { return };
    exec(&w, &p, "INSERT INTO quarry_test.t_stop VALUES (1)");
    let h = RunHandle::default();
    let (id, cancel) = (h.conn.clone(), h.cancel.clone());
    // Unlocked, so the DELETE would really run if Stop only interrupted the sleep.
    w.sql_run(3, db.clone(), "quarry_test".into(), "SELECT SLEEP(5); DELETE FROM t_stop".into(), false, 0, h);
    let start = Instant::now();
    while id.load(Ordering::SeqCst) == 0 {
        assert!(start.elapsed() < Duration::from_secs(5), "no connection id");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(300));
    w.sql_kill(db, id.load(Ordering::SeqCst), cancel);
    let sets = loop {
        if let Msg::SqlRan { sets, .. } = next(&rx) {
            break sets;
        }
    };
    // MySQL answers an interrupted SLEEP with 1, so the flag marks the DELETE; MariaDB fails the SLEEP itself.
    if sets.len() == 2 {
        assert!(sets[1].error.as_deref().is_some_and(|m| m.contains("Stopped before running")), "{:?}", sets[1].error);
    } else {
        assert!(sets[0].error.is_some());
    }
    let n: i64 = w.rt.block_on(async { sqlx::query_scalar("SELECT COUNT(*) FROM quarry_test.t_stop").fetch_one(&*p).await.unwrap() });
    assert_eq!(n, 1);
}

#[test]
fn unique_key_prefers_not_null_plain_columns() {
    use super::super::structure::{Index, IndexCol};
    let ix = |name: &str, cols: &[&str]| Index { name: name.into(), unique: true, cols: cols.iter().map(|c| IndexCol { name: c.to_string(), ..Default::default() }).collect(), ..Default::default() };
    let pick = |all: &[Index], nullable: &[&str]| unique_key(all.iter(), &nullable.iter().map(|c| c.to_string()).collect());
    // NOT NULL wins over fewer columns, since rows with a NULL key cannot be addressed.
    assert_eq!(pick(&[ix("a_u", &["x"]), ix("b_u", &["y", "z"])], &["x"]), ["y", "z"]);
    // A plain index, or a unique one with an expression, a prefix, a predicate or another comparison, is never a key.
    let expr = Index { cols: vec![IndexCol { expr: Some("lower(x)".into()), ..Default::default() }], ..ix("e", &[]) };
    let prefix = Index { cols: vec![IndexCol { name: "x".into(), prefix: Some(3), ..Default::default() }], ..ix("f", &[]) };
    let skipped = [Index { unique: false, ..ix("k", &["x"]) }, expr, prefix, Index { predicate: "b > 0".into(), ..ix("p", &["x"]) }, Index { complex: true, ..ix("c", &["x"]) }];
    assert!(pick(&skipped, &[]).is_empty());
    // Nullable is still a key when nothing better exists, and ties go to the fewest columns, then the name.
    assert_eq!(pick(&[ix("a", &["s"]), ix("b", &["r"]), ix("c", &["p", "q"])], &["p", "q", "r", "s"]), ["s"]);
}

#[test]
fn live_a_unique_key_stands_in_for_a_missing_primary_key() {
    let Some((w, rx, db, p)) = live("t_uniq", "u VARCHAR(10), v INT, UNIQUE KEY uq_pre (u(3)), UNIQUE KEY uq_v (v)") else { return };
    exec(&w, &p, "INSERT INTO quarry_test.t_uniq VALUES ('a', 1), ('b', NULL)");
    let (_, pk, rows, _) = page_of(&w, &rx, &db, "t_uniq", "", "u", 10, 0);
    // The prefix index is skipped: its key is only part of the value.
    assert_eq!(pk, ["v"]);
    assert!(rows[0].get_document("_id").is_ok_and(|k| k.contains_key("v")), "{:?}", rows[0]);
    assert!(!rows[1].contains_key("_id"), "a NULL key value addressed a row: {:?}", rows[1]);
}

#[test]
fn live_invisible_primary_key_is_not_a_usable_key() {
    let Some((w, rx, db, p)) = live("", "") else { return };
    let made = w.rt.block_on(async {
        let mut c = p.acquire().await.unwrap();
        raw_sql("DROP TABLE IF EXISTS quarry_test.t_inv").execute(&mut *c).await.unwrap();
        // Only MySQL 8.0.30 and later know this variable.
        if raw_sql("SET SESSION sql_generate_invisible_primary_key = ON").execute(&mut *c).await.is_err() {
            return false;
        }
        raw_sql("CREATE TABLE quarry_test.t_inv (a INT)").execute(&mut *c).await.unwrap();
        raw_sql("INSERT INTO quarry_test.t_inv VALUES (1), (2)").execute(&mut *c).await.unwrap();
        true
    });
    if !made {
        return;
    }
    let (cols, pk, rows, _) = page_of(&w, &rx, &db, "t_inv", "", "", 10, 0);
    assert!(pk.is_empty(), "{pk:?}");
    assert_eq!(cols.len(), 1);
    assert!(rows.iter().all(|d| !d.contains_key("_id")));
}

#[test]
fn live_pages_are_stable_and_a_real_id_column_survives() {
    let Some((w, rx, db, p)) = live("t_stable", "_id INT, k INT, g INT, PRIMARY KEY (k)") else { return };
    let vals: Vec<String> = (0..20).map(|i| format!("({}, {i}, {})", 100 + i, i % 2)).collect();
    exec(&w, &p, &format!("INSERT INTO quarry_test.t_stable VALUES {}", vals.join(",")));
    // No order: the key orders it. A user order that ties gets the key as tie-breaker, so pages never overlap.
    let (_, _, first, _) = page_of(&w, &rx, &db, "t_stable", "", "g", 10, 0);
    let (cols, pk, second, _) = page_of(&w, &rx, &db, "t_stable", "", "g", 10, 10);
    let mut ks: Vec<i64> = first.iter().chain(&second).map(|d| d.get_i64("k").unwrap()).collect();
    ks.sort();
    assert_eq!(ks, (0..20).collect::<Vec<_>>());
    let (_, _, plain, _) = page_of(&w, &rx, &db, "t_stable", "", "", 3, 0);
    assert_eq!(plain.iter().map(|d| d.get_i64("k").unwrap()).collect::<Vec<_>>(), [0, 1, 2]);
    // The real _id column is renamed, the key keeps the _id slot.
    assert_eq!(pk, ["k"]);
    assert_eq!(cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["_id_2", "k", "g"]);
    assert_eq!(second[0].get_document("_id").unwrap(), &mongodb::bson::doc! { "k": second[0].get_i64("k").unwrap() });
    assert!(second[0].get_i64("_id_2").is_ok());
}

#[test]
fn live_call_returns_each_result_set_apart() {
    let Some((w, rx, db, p)) = live("", "") else { return };
    exec(&w, &p, "DROP PROCEDURE IF EXISTS quarry_test.p_two");
    exec(&w, &p, "CREATE PROCEDURE quarry_test.p_two() BEGIN SELECT 1 AS a; SELECT 'x' AS b, 2 AS c; END");
    let sets = run_all(&w, &rx, &db, "CALL p_two(); SELECT 3 AS z", false);
    assert_eq!(sets.len(), 3, "{:?}", sets.iter().map(|s| (s.cols.len(), s.rows.len(), s.affected, s.error.clone())).collect::<Vec<_>>());
    assert_eq!(sets[0].cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["a"]);
    assert_eq!(sets[1].cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["b", "c"]);
    assert_eq!(sets[2].rows[0].decode().unwrap().get_i64("z"), Ok(3));
}

#[test]
fn live_pages_follow_each_other_and_a_count_comes_apart() {
    let Some((w, rx, db, p)) = live("t_pages", "id INT PRIMARY KEY, v VARCHAR(5)") else { return };
    let vals: Vec<String> = (1..=2500).map(|i| format!("({i}, 'x')")).collect();
    exec(&w, &p, &format!("INSERT INTO quarry_test.t_pages VALUES {}", vals.join(",")));
    let stmt = "SELECT id -- the key\nFROM t_pages ORDER BY id DESC";
    let page = |n: usize| {
        w.sql_run(9, db.clone(), "quarry_test".into(), stmt.into(), true, n, Default::default());
        let Msg::SqlRan { mut sets, .. } = next(&rx) else { panic!("expected SqlRan") };
        sets.remove(0)
    };
    let (a, b, c) = (page(0), page(1), page(2));
    assert_eq!((a.rows.len(), b.rows.len(), c.rows.len()), (1000, 1000, 500));
    assert_eq!(a.stmt, stmt);
    assert_eq!((a.page.unwrap().next, b.page.unwrap().next, c.page.unwrap().next), (true, true, false));
    assert_eq!(a.rows[0].decode().unwrap().get_i64("id"), Ok(2500));
    assert_eq!(c.rows[0].decode().unwrap().get_i64("id"), Ok(500));
    assert!(a.page.unwrap().ordered);
    w.sql_count(4, 0, db.clone(), "quarry_test".into(), stmt.into());
    let Msg::SqlCount { req, idx, res } = next(&rx) else { panic!("expected SqlCount") };
    assert_eq!((req, idx, res), (4, 0, Ok(2500)));
    // A repeated column name cannot be wrapped, so it falls back to a plain run without pages.
    let dup = page_of_text(&w, &rx, &db, "SELECT 1 AS a, 2 AS a");
    assert!(dup.error.is_none() && dup.page.is_none() && dup.rows.len() == 1);
}

fn page_of_text(w: &Worker, rx: &Receiver<Msg>, db: &Db, text: &str) -> ResultSet {
    run_all(w, rx, db, text, true).remove(0)
}

#[test]
fn live_export_streams_200k_rows_to_csv_and_json_one_batch_at_a_time() {
    use super::super::export::{export_to, Plan, Sink, BATCH};
    use crate::db::copy::Fmt;
    let Some((w, _rx, db, p)) = live("t_export_big", "id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(20)") else { return };
    exec(&w, &p, "INSERT INTO quarry_test.t_export_big (name) SELECT 'x' FROM information_schema.columns a, information_schema.columns b LIMIT 200000");
    let dir = std::env::temp_dir().join(format!("quarry-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let stmt = "SELECT id, name FROM t_export_big ORDER BY id";
    let run = |fmt: Fmt, file: &str| {
        let plan = Plan { target: crate::db::sql::export::Target::Text(fmt), header: true, table: "t_export_big".into(), bs: true };
        let path = dir.join(file);
        let count = AtomicU64::new(0);
        let done = w.rt.block_on(export_to(&db, "quarry_test", stmt, &plan, Sink::File(path.clone()), &count, &AtomicBool::new(false), &AtomicU64::new(0))).unwrap();
        // The worker never held more than one batch, and the count the status bar reads is the whole result.
        assert!(done.peak_batch <= BATCH, "{}", done.peak_batch);
        assert_eq!((done.rows, count.load(Ordering::Relaxed)), (200_000, 200_000));
        // Written beside and renamed: no half file stays.
        assert!(!dir.join(format!(".{file}.part")).exists());
        path
    };
    let csv = std::fs::read_to_string(run(Fmt::Csv, "big.csv")).unwrap();
    assert_eq!((csv.lines().count(), csv.lines().next(), csv.lines().last()), (200_001, Some("id,name"), Some("200000,x")));
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(run(Fmt::Json, "big.json")).unwrap()).unwrap();
    assert_eq!((json.as_array().map(|a| a.len()), json[199_999]["id"].as_i64()), (Some(200_000), Some(200_000)));
    // A workbook streams the same way and is a whole zip with the last row as a number.
    let plan = Plan { target: crate::db::sql::export::Target::Xlsx(crate::db::xlsx::Opts { sheet: "big".into(), about: None }), header: true, table: String::new(), bs: true };
    let path = dir.join("big.xlsx");
    let done = w.rt.block_on(export_to(&db, "quarry_test", stmt, &plan, Sink::File(path.clone()), &AtomicU64::new(0), &AtomicBool::new(false), &AtomicU64::new(0))).unwrap();
    assert!(done.peak_batch <= BATCH && done.rows == 200_000);
    let mut z = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let mut sheet = String::new();
    std::io::Read::read_to_string(&mut z.by_name("xl/worksheets/sheet1.xml").unwrap(), &mut sheet).unwrap();
    assert!(sheet.contains("<v>200000</v>") && sheet.contains("ref=\"A1:B200001\""), "{}", &sheet[..sheet.len().min(400)]);
    // A statement that writes is refused before it reaches the server, and leaves no file.
    let plan = Plan { target: crate::db::sql::export::Target::Text(Fmt::Csv), header: true, table: String::new(), bs: true };
    let refused = dir.join("never.csv");
    let err = w.rt.block_on(export_to(&db, "quarry_test", "DELETE FROM t_export_big", &plan, Sink::File(refused.clone()), &AtomicU64::new(0), &AtomicBool::new(false), &AtomicU64::new(0))).err().unwrap();
    assert!(err.contains("read"), "{err}");
    assert!(!refused.exists());
    let n: i64 = w.rt.block_on(async { sqlx::query_scalar("SELECT COUNT(*) FROM quarry_test.t_export_big").fetch_one(&p.pool).await.unwrap() });
    assert_eq!(n, 200_000);
    // An empty result still writes its header and footer.
    let empty = w.rt.block_on(export_to(&db, "quarry_test", "SELECT id FROM t_export_big WHERE id < 0", &Plan { target: crate::db::sql::export::Target::Text(Fmt::Json), header: true, table: String::new(), bs: true }, Sink::Clipboard, &AtomicU64::new(0), &AtomicBool::new(false), &AtomicU64::new(0))).unwrap();
    assert_eq!((empty.rows, empty.text.as_deref()), (0, Some("[]\n")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn live_export_all_statements_runs_one_session_and_writes_json_or_csv_files() {
    use super::super::export::{check_script, export_all, AllPlan, Plan};
    use crate::db::copy::Fmt;
    let Some((w, _rx, db, p)) = live("t_exp", "id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(20)") else { return };
    exec(&w, &p, "INSERT INTO quarry_test.t_exp (name) VALUES ('a'), ('b'), ('c')");
    let dir = std::env::temp_dir().join(format!("quarry-export-all-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let names = |d: &std::path::Path| {
        let mut v: Vec<String> = std::fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    };
    // USE and @variables carry over: the table has no schema in front of it and the variable is still set.
    let script = "USE quarry_test; SET @lim = 2; SELECT id, name FROM t_exp ORDER BY id; SELECT @lim AS lim, COUNT(*) AS n FROM t_exp";
    let stmts = check_script(script, Dialect::MySql).unwrap();
    let run = |stmts: &[String], fmt: Fmt, single: bool, file: &str, cancel: bool| {
        let ap = AllPlan { plan: Plan { target: crate::db::sql::export::Target::Text(fmt), header: true, table: "t_exp".into(), bs: true }, single };
        let (count, step) = (AtomicU64::new(0), AtomicU64::new(0));
        let r = w.rt.block_on(export_all(&db, "", stmts, &ap, &dir.join(file), &count, &step, &AtomicBool::new(cancel), &AtomicU64::new(0)));
        (r, count.load(Ordering::Relaxed), step.load(Ordering::Relaxed))
    };

    let (r, count, step) = run(&stmts, Fmt::Json, true, "all.json", false);
    let done = r.unwrap();
    // The count the status bar reads is every row of every result, and the step is the last statement.
    assert_eq!((done.rows, done.files, count, step), (4, 1, 4, 4));
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("all.json")).unwrap()).unwrap();
    assert_eq!(json.as_array().map(|a| a.len()), Some(2));
    assert_eq!(json[0]["statement"], "SELECT id, name FROM t_exp ORDER BY id");
    assert_eq!(json[0]["rows"].as_array().map(|a| a.len()), Some(3));
    assert_eq!(json[1]["rows"][0]["lim"], 2);
    assert_eq!(json[1]["rows"][0]["n"], 3);

    let done = run(&stmts, Fmt::Csv, false, "res.csv", false).0.unwrap();
    assert_eq!((done.rows, done.files), (4, 2));
    assert_eq!(std::fs::read_to_string(dir.join("res-1.csv")).unwrap(), "id,name\n1,a\n2,b\n3,c\n");
    assert_eq!(std::fs::read_to_string(dir.join("res-2.csv")).unwrap(), "lim,n\n2,3\n");
    // No temporary file is left, and USE and SET made no file of their own.
    assert_eq!(names(&dir), ["all.json", "res-1.csv", "res-2.csv"]);

    // A statement that fails midway leaves nothing, not even the first result's file.
    let bad = check_script("SELECT 1 AS a; SELECT * FROM quarry_test.no_such_table_here", Dialect::MySql).unwrap();
    let err = run(&bad, Fmt::Csv, false, "bad.csv", false).0.err().unwrap();
    assert!(err.contains("no_such_table_here"), "{err}");
    let err = run(&bad, Fmt::Json, true, "bad.json", false).0.err().unwrap();
    assert!(err.contains("no_such_table_here"), "{err}");
    assert_eq!(names(&dir), ["all.json", "res-1.csv", "res-2.csv"]);

    // Cancel before the first statement writes nothing either.
    let err = run(&stmts, Fmt::Csv, false, "stop.csv", true).0.err().unwrap();
    assert_eq!(err, "Export cancelled");
    assert_eq!(names(&dir).len(), 3);

    // A script with a DELETE is refused up front and the table stays whole.
    let refused = check_script("SELECT 1; DELETE FROM t_exp", Dialect::MySql);
    assert!(refused.is_err());
    let n = |rt: &tokio::runtime::Handle, pool: &sqlx::MySqlPool| -> i64 { rt.block_on(async { sqlx::query_scalar("SELECT COUNT(*) FROM quarry_test.t_exp").fetch_one(pool).await.unwrap() }) };
    assert_eq!(n(&p.rt, &p.pool), 3);
    // The worker itself refuses it too, when a caller skips check_script.
    let sneaky = vec!["SELECT 1".to_string(), "DELETE FROM t_exp".to_string()];
    let err = run(&sneaky, Fmt::Csv, false, "sneaky.csv", false).0.err().unwrap();
    assert!(err.contains("read"), "{err}");
    assert_eq!(n(&p.rt, &p.pool), 3);
    assert_eq!(names(&dir).len(), 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn live_a_capped_result_ends_the_script() {
    let Some((w, rx, db, _p)) = live("", "") else { return };
    exec(&w, &_p, "DROP PROCEDURE IF EXISTS quarry_test.p_big");
    exec(&w, &_p, "CREATE PROCEDURE quarry_test.p_big() BEGIN SELECT 1 AS n FROM information_schema.columns a, information_schema.columns b LIMIT 1500; END");
    // A CALL may write after it selects, so it is read to its end: the grid keeps the cap and the script goes on.
    let sets = run_all(&w, &rx, &db, "CALL p_big(); SELECT 2", false);
    assert_eq!(sets[0].rows.len(), RUN_ROWS_MAX);
    let last = sets.last().unwrap();
    assert!(last.error.is_none() && last.rows.len() == 1, "{:?}", last.error);
}

#[test]
fn fill_refuses_text_that_reads_two_ways() {
    use value::Param;
    let evil = [Param::Text("x; DELETE FROM t; #".into())];
    let text = "SELECT '\\'' AS a, ? AS b -- '";
    // With backslash escapes the ? is inside a comment-free tail; without them the quote closes early.
    assert!(fill(text, &evil, true, false).is_err());
    assert!(fill(text, &evil, false, false).is_err());
    // A text both readings agree on still fills, under either mode.
    assert!(fill("SELECT ? AS b", &evil, true, false).is_ok() && fill("SELECT ? AS b", &evil, false, true).is_ok());
    // ANSI_QUOTES makes "?" an identifier, not a placeholder, which both backslash readings share.
    assert!(fill("SELECT \"a\" AS \"?\", ?", &evil, true, true).is_ok());
}

#[test]
fn int_params_keep_every_digit() {
    use value::Param;
    let p = |j: serde_json::Value| crate::db::shell::sql_params_for_test(&j);
    assert_eq!(p(serde_json::json!([{ "$int": "9007199254740993" }, { "$int": "-9223372036854775808" }, { "$int": "18446744073709551615" }])).unwrap(), [Param::Int(9007199254740993), Param::Int(i64::MIN), Param::UInt(u64::MAX)]);
    assert!(p(serde_json::json!([{ "$int": "18446744073709551616" }])).is_err());
}

/// Needs Bun and a server. Shared by the live tests that run scripts the person could not trust.
fn bun_ok() -> bool {
    std::process::Command::new("bun").arg("--version").output().is_ok()
}

#[test]
fn live_shell_where_never_means_every_row_by_accident() {
    let Some((w, rx, db, p)) = live("t_where", "id BIGINT PRIMARY KEY, note VARCHAR(20)") else { return };
    if !bun_ok() {
        return;
    }
    exec(&w, &p, "INSERT INTO quarry_test.t_where VALUES (1, 'a'), (2, NULL), (9007199254740993, 'big')");
    let script = "async function run() {
  const bad = [5, null, [], false, 'x', { id: {} }, { id: undefined }, { id: { gt: undefined } }, { id: new Map() }];
  const out = [];
  for (const w of bad) {
    for (const f of [() => t_where.delete(w), () => t_where.update(w, { note: 'z' }), () => t_where.find(w), () => t_where.count(w)]) {
      try { await f(); out.push('ran') } catch (e) { out.push('refused') }
    }
  }
  const big = await t_where.findOne({ id: 9007199254740993n });
  let unsafe;
  try { await sql`SELECT ${2 ** 60}`; unsafe = 'ran' } catch (e) { unsafe = e.message }
  return { out, count: await t_where.count({}), big, unsafe, sum: (await sql`SELECT ${9223372036854775807n} AS v, ${18446744073709551615n} AS u`)[0] };
}";
    let (_, res) = shell_script(&w, &rx, &db, script, &["t_where"], false);
    let v: serde_json::Value = serde_json::from_str(&res.expect("script result")).unwrap();
    let v = &v["v"];
    assert!(v["out"].as_array().unwrap().iter().all(|x| x == "refused"), "{v}");
    assert_eq!(v["count"], 3, "{v}");
    assert_eq!(v["big"]["note"], "big");
    // The row id came back as an exact value, not a rounded number.
    assert_eq!(v["big"]["id"]["$numberLong"], "9007199254740993", "{v}");
    assert!(v["unsafe"].as_str().unwrap().contains("BigInt"), "{v}");
    assert_eq!(v["sum"]["v"]["$numberLong"], "9223372036854775807", "{v}");
    assert_eq!(v["sum"]["u"].to_string().trim_matches('"'), "18446744073709551615", "{v}");
}

#[test]
fn live_shell_refuses_sets_that_move_the_escaping_even_when_unlocked() {
    let Some((w, rx, db, _p)) = live("", "") else { return };
    if !bun_ok() {
        return;
    }
    let script = "async function run() {
  const out = [];
  for (const t of ['SET NAMES utf8mb4', 'SET CHARACTER SET utf8mb4', 'SET CHARSET utf8mb4', 'SET SESSION character_set_client = 1', 'set @@character_set_connection = 1', \"SET sql_mode = ''\", \"SET @@session.sql_mode = ''\", \"SET @a = 1, @@sql_mode = ''\"]) {
    try { await sql.unsafe(t); out.push('ran') } catch (e) { out.push(e.message) }
  }
  await sql`SET @a = 1`;
  return out;
}";
    let (_, res) = shell_script(&w, &rx, &db, script, &[], false);
    let v: serde_json::Value = serde_json::from_str(&res.expect("script result")).unwrap();
    assert!(v["v"].as_array().unwrap().iter().all(|m| m.as_str().unwrap().contains("not allowed in the shell")), "{v}");
}

#[test]
fn live_no_backslash_escapes_cannot_smuggle_a_statement() {
    let Some((w, rx, db, p)) = live("t_nb", "id INT PRIMARY KEY") else { return };
    if !bun_ok() {
        return;
    }
    exec(&w, &p, "INSERT INTO quarry_test.t_nb VALUES (1)");
    // Every session of this pool starts in NO_BACKSLASH_ESCAPES, as on a server set that way. A pool of its own
    // rather than SET GLOBAL, which raced the other live tests: their new sessions changed mode under them.
    let mut db = db;
    let opts = db.opts.my().unwrap().clone();
    let pool = w.rt.block_on(async move {
        sqlx::mysql::MySqlPoolOptions::new()
            .max_connections(2)
            .after_connect(|c, _| Box::pin(async move { sqlx::Executor::execute(c, "SET SESSION sql_mode = CONCAT(@@SESSION.sql_mode, ',NO_BACKSLASH_ESCAPES')").await.map(|_| ()) }))
            .connect_with(opts)
            .await
            .unwrap()
    });
    db.sql_mode = w.rt.block_on(async { sqlx::query_scalar::<_, String>("SELECT @@SESSION.sql_mode").fetch_one(&pool).await.unwrap() });
    assert!(db.sql_mode.contains("NO_BACKSLASH_ESCAPES"), "{}", db.sql_mode);
    db.pool = Pool::My(pool);
    let script = "async function run() {
  let r;
  try { r = await sql.unsafe(\"SELECT '\\\\'' AS a, ? AS b -- '\", ['x; DELETE FROM t_nb; #']); r = 'ran' } catch (e) { r = e.message }
  return { r, left: await t_nb.count() };
}";
    let (_, res) = shell_script(&w, &rx, &db, script, &["t_nb"], false);
    let v: serde_json::Value = serde_json::from_str(&res.expect("script result")).unwrap();
    assert_eq!(v["v"]["left"], 1, "{v}");
    assert!(v["v"]["r"].as_str().unwrap().contains("differently"), "{v}");
}
