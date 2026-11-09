//! The SQL half of the worker: connecting, listing tables, paging a table and running Custom Query
//! scripts. Rows leave here as BSON like Mongo's, see `Row`.

// The table and query views (docs/sql/PLAN.md steps 5 and 8) call page, run and kill; until then only the tests do.
#![allow(dead_code)]

use super::value::{self, Kind};
use super::{lex, Addr, Col, Db, Dialect, Pool, ResultSet, SslMode, RUN_ROWS_MAX};
use crate::db::{e, Msg, Net, Res, Row, Worker};
use mongodb::bson::{Bson, Document};
use sqlx::mysql::{MySqlColumn, MySqlConnectOptions, MySqlPoolOptions, MySqlRow, MySqlSslMode};
use sqlx::{raw_sql, Column, Either, Executor, Row as _, TypeInfo};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Below this many rows the estimate in information_schema is too loose to show, and counting is cheap.
const ESTIMATE_MIN: u64 = 10_000;

/// A cell of a text-protocol row as the server's own bytes. Metadata queries read strings through this
/// too, because a `_bin` collation makes sqlx report text as VARBINARY.
fn raw(row: &MySqlRow, i: usize) -> Option<&[u8]> {
    row.try_get_unchecked::<Option<&[u8]>, _>(i).ok().flatten()
}

fn text(row: &MySqlRow, name: &str) -> Option<String> {
    row.try_get_unchecked::<Option<&[u8]>, _>(name).ok().flatten().map(|b| String::from_utf8_lossy(b).into_owned())
}

fn ssl_mode(m: SslMode) -> MySqlSslMode {
    match m {
        SslMode::Disabled => MySqlSslMode::Disabled,
        SslMode::Preferred => MySqlSslMode::Preferred,
        SslMode::Required => MySqlSslMode::Required,
        SslMode::VerifyCa => MySqlSslMode::VerifyCa,
        SslMode::VerifyIdentity => MySqlSslMode::VerifyIdentity,
    }
}

/// What every pool connection gets: read-only, the statement timeout, fresh table statistics.
async fn session(c: &mut sqlx::MySqlConnection, ms: u64) -> Result<(), sqlx::Error> {
    // The dialect is not known yet, so each connection asks for it to pick the session statements.
    let v: String = sqlx::query_scalar("SELECT VERSION()").fetch_one(&mut *c).await?;
    let d = Dialect::from_version(&v);
    c.execute(d.session_read_only(true)).await?;
    let t = d.session_timeout(ms);
    c.execute(t.as_str()).await?;
    // MySQL 8 caches TABLE_ROWS and sizes for a day; 5.7 has no such variable, so a refusal is fine.
    if d == Dialect::MySql {
        let _ = c.execute("SET SESSION information_schema_stats_expiry = 0").await;
    }
    Ok(())
}

/// Readies the script's own connection: its id for Stop, the database, and write access when unlocked.
async fn prepare(conn: &mut sqlx::MySqlConnection, d: Dialect, schema: &str, read_only: bool, id: &AtomicU64) -> Result<(), sqlx::Error> {
    if let Ok(r) = conn.fetch_one("SELECT CONNECTION_ID()").await {
        id.store(raw(&r, 0).and_then(|b| std::str::from_utf8(b).ok()?.parse().ok()).unwrap_or(0), Ordering::SeqCst);
    }
    if !schema.is_empty() {
        let q = format!("USE {}", d.quote_ident(schema));
        conn.execute(q.as_str()).await?;
    }
    if !read_only {
        conn.execute(d.session_read_only(false)).await?;
    }
    Ok(())
}

async fn open(uri: &str, timeout: Duration, net: &Net) -> Res<(Db, Vec<String>, Option<crate::ssh::Tunnel>)> {
    let a = Addr::parse(uri).ok_or("Not a MySQL address")?;
    let tunnel = match &net.ssh {
        Some((conf, secrets)) => Some(crate::ssh::open(conf, secrets, timeout, Some((a.host.clone(), a.port))).await?),
        None => None,
    };
    // The certificate names the real host, not the tunnel's 127.0.0.1, so the name check cannot pass.
    let (host, port, mode) = match &tunnel {
        Some(t) => ("127.0.0.1".to_string(), t.port, if a.ssl_mode == SslMode::VerifyIdentity { SslMode::VerifyCa } else { a.ssl_mode }),
        None => (a.host.clone(), a.port, a.ssl_mode),
    };
    // The sqlx defaults change how the server reads user SQL (|| as concat, UTC), so they are turned off.
    let mut o = MySqlConnectOptions::new().host(&host).port(port).username(&a.user).ssl_mode(ssl_mode(mode)).pipes_as_concat(false).no_engine_substitution(false).timezone(None);
    if !a.password.is_empty() {
        o = o.password(&a.password);
    }
    if !a.database.is_empty() {
        o = o.database(&a.database);
    }
    if !a.ssl_ca.is_empty() {
        o = o.ssl_ca(&a.ssl_ca);
    }
    let ms = timeout.as_millis() as u64;
    let pool = MySqlPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(timeout)
        .after_connect(move |c, _| Box::pin(session(c, ms)))
        .connect_with(o)
        .await
        .map_err(e)?;
    let version: String = sqlx::query_scalar("SELECT VERSION()").fetch_one(&pool).await.map_err(e)?;
    let sql_mode: String = sqlx::query_scalar("SELECT @@sql_mode").fetch_one(&pool).await.unwrap_or_default();
    let tls = raw_sql("SHOW SESSION STATUS LIKE 'Ssl_cipher'").fetch_all(&pool).await.map_err(e)?.iter().any(|r| raw(r, 1).is_some_and(|v| !v.is_empty()));
    let mut dbs: Vec<String> = raw_sql("SHOW DATABASES").fetch_all(&pool).await.map_err(e)?.iter().filter_map(|r| raw(r, 0).map(|b| String::from_utf8_lossy(b).into_owned())).collect();
    dbs.sort();
    let db = Db { dialect: Dialect::from_version(&version), pool: Pool::My(pool), version, tls, sql_mode };
    Ok((db, dbs, tunnel))
}

/// A table kind from information_schema's COLUMN_TYPE ("varchar(20)", "bigint unsigned"), which stays
/// right for `_bin` text columns that sqlx reports as binary.
fn kind_of_full(full: &str) -> Kind {
    let l = full.to_ascii_lowercase();
    let base: String = l.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    Kind::from_type_name(&if l.contains(" unsigned") { format!("{base} UNSIGNED") } else { base })
}

/// Names must be unique because a row is a BSON document keyed by them.
fn unique(cols: &mut [Col]) {
    let mut seen = HashSet::new();
    for c in cols {
        let base = c.name.clone();
        let mut n = 1;
        while !seen.insert(c.name.clone()) {
            n += 1;
            c.name = format!("{base}_{n}");
        }
    }
}

/// Columns as the result reports them. `meta` (name to COLUMN_TYPE and nullability) wins when the
/// result column is a table column.
fn make_cols(cols: &[MySqlColumn], meta: &HashMap<String, (String, bool)>) -> Vec<Col> {
    let mut out: Vec<Col> = cols
        .iter()
        .map(|c| {
            let ty = c.type_info().name().to_string();
            match meta.get(c.name()) {
                Some((full, nullable)) => Col { name: c.name().into(), kind: kind_of_full(full), ty: full.clone(), nullable: *nullable },
                None => Col { name: c.name().into(), kind: Kind::from_type_name(&ty), ty, nullable: true },
            }
        })
        .collect();
    unique(&mut out);
    out
}

fn to_row(cols: &[Col], row: &MySqlRow, pk: &[String]) -> Res<Row> {
    let vals: Vec<Bson> = cols.iter().enumerate().map(|(i, c)| value::to_bson(c.kind, raw(row, i))).collect();
    let mut d = Document::new();
    if !pk.is_empty() {
        let mut k = Document::new();
        for p in pk {
            if let Some(i) = cols.iter().position(|c| &c.name == p) {
                k.insert(p.clone(), vals[i].clone());
            }
        }
        d.insert("_id", k);
    }
    for (c, v) in cols.iter().zip(vals) {
        d.insert(c.name.clone(), v);
    }
    Row::from_doc(&d)
}

/// Text the user typed is spliced into a SELECT, so it must stay one read-only statement.
fn one_read(sql: &str) -> Res<()> {
    if lex::split_statements(sql).len() != 1 {
        return Err("The filter and sort must not contain another statement".into());
    }
    lex::read_only_check(sql)
}

/// User text goes on its own lines so a trailing `-- comment` cannot swallow what follows.
fn where_of(filter: &str) -> String {
    if filter.trim().is_empty() { String::new() } else { format!("\nWHERE (\n{}\n)", filter.trim()) }
}

async fn count(pool: &sqlx::MySqlPool, d: Dialect, schema: &str, table: &str, filter: &str) -> Res<u64> {
    if filter.trim().is_empty() {
        let est = sqlx::query(d.table_status()).bind(schema).bind(table).fetch_optional(pool).await.map_err(e)?;
        if let Some(n) = est.and_then(|r| r.try_get::<Option<u64>, _>("row_estimate").ok().flatten()).filter(|n| *n >= ESTIMATE_MIN) {
            return Ok(n);
        }
    }
    let sql = format!("SELECT COUNT(*) FROM {}{}", d.qualified(schema, table), where_of(filter));
    one_read(&sql)?;
    let r = raw_sql(&sql).fetch_one(pool).await.map_err(e)?;
    Ok(raw(&r, 0).and_then(|b| std::str::from_utf8(b).ok()?.parse().ok()).unwrap_or(0))
}

/// Column types and the primary key, from information_schema.
async fn table_meta(pool: &sqlx::MySqlPool, d: Dialect, schema: &str, table: &str) -> Res<(HashMap<String, (String, bool)>, Vec<String>)> {
    let cols = sqlx::query(d.columns()).bind(schema).bind(table).fetch_all(pool).await.map_err(e)?;
    let idx = sqlx::query(d.indexes()).bind(schema).bind(table).fetch_all(pool).await.map_err(e)?;
    let meta = cols
        .iter()
        .filter_map(|r| {
            let nullable = r.try_get::<i64, _>("nullable").map(|n| n != 0).unwrap_or(true);
            Some((text(r, "name")?, (text(r, "full_type")?, nullable)))
        })
        .collect();
    let pk = idx.iter().filter(|r| text(r, "name").as_deref() == Some("PRIMARY")).filter_map(|r| text(r, "col")).collect();
    Ok((meta, pk))
}

async fn page(db: &Db, schema: &str, table: &str, filter: &str, order: &str, limit: u64, offset: u64) -> Res<(Vec<Col>, Vec<String>, Vec<Row>, u64, u128)> {
    let start = Instant::now();
    let Pool::My(pool) = &db.pool;
    let d = db.dialect;
    let mut sql = format!("SELECT * FROM {}{}", d.qualified(schema, table), where_of(filter));
    if !order.trim().is_empty() {
        sql.push_str(&format!("\nORDER BY {}", order.trim()));
    }
    sql.push_str(&format!("\n{}", d.page(limit, offset)));
    one_read(&sql)?;
    let data = async { raw_sql(&sql).fetch_all(pool).await.map_err(e) };
    let ((meta, pk), count, rows) = tokio::try_join!(table_meta(pool, d, schema, table), count(pool, d, schema, table, filter), data)?;
    let cols = match rows.first() {
        Some(r) => make_cols(r.columns(), &meta),
        // An empty page still needs its headers.
        None => make_cols((&*pool).describe(&sql).await.map_err(e)?.columns(), &meta),
    };
    let rows = rows.iter().map(|r| to_row(&cols, r, &pk)).collect::<Res<Vec<_>>>()?;
    Ok((cols, pk, rows, count, start.elapsed().as_millis()))
}

fn failed(stmt: &str, error: String) -> ResultSet {
    ResultSet { stmt: stmt.into(), cols: Vec::new(), rows: Vec::new(), affected: None, error: Some(error), ms: 0, capped: false }
}

/// Runs one statement and keeps at most `RUN_ROWS_MAX` rows. The rest are still read, so the
/// connection is clean for the next statement.
async fn run_one(conn: &mut sqlx::MySqlConnection, stmt: &str) -> ResultSet {
    let t = Instant::now();
    let mut rs = failed(stmt, String::new());
    rs.error = None;
    let mut cols: Vec<Col> = Vec::new();
    let mut affected = None;
    {
        let mut s = raw_sql(stmt).fetch_many(&mut *conn);
        while let Some(item) = futures_util::StreamExt::next(&mut s).await {
            match item {
                Ok(Either::Right(row)) => {
                    if cols.is_empty() {
                        cols = make_cols(row.columns(), &HashMap::new());
                    }
                    if rs.rows.len() >= RUN_ROWS_MAX {
                        rs.capped = true;
                    } else {
                        match to_row(&cols, &row, &[]) {
                            Ok(r) => rs.rows.push(r),
                            Err(err) => {
                                rs.error = Some(err);
                                break;
                            }
                        }
                    }
                }
                Ok(Either::Left(r)) => affected = Some(r.rows_affected()),
                Err(err) => {
                    rs.error = Some(e(err));
                    break;
                }
            }
        }
    }
    if rs.error.is_none() && cols.is_empty() {
        // No rows came back: a SELECT with an empty result still has headers, a write does not.
        match (&mut *conn).describe(stmt).await {
            Ok(desc) if !desc.columns().is_empty() => cols = make_cols(desc.columns(), &HashMap::new()),
            _ => rs.affected = affected,
        }
    }
    rs.cols = cols;
    rs.ms = t.elapsed().as_millis();
    rs
}

async fn run(db: &Db, schema: &str, text: &str, read_only: bool, id: &AtomicU64) -> Vec<ResultSet> {
    let Pool::My(pool) = &db.pool;
    // Detached, so a USE or @variable of this script never leaks into the pool.
    let mut conn = match pool.acquire().await {
        Ok(c) => c.detach(),
        Err(err) => return vec![failed("", e(err))],
    };
    let d = db.dialect;
    if let Err(err) = prepare(&mut conn, d, schema, read_only, id).await {
        return vec![failed("", e(err))];
    }
    let mut sets = Vec::new();
    // Each statement goes alone, so user text is never sent as one multi-statement string.
    for (_, stmt) in lex::split_statements(text) {
        if read_only {
            if let Err(why) = lex::read_only_check(stmt) {
                sets.push(failed(stmt, why));
                continue;
            }
        }
        let rs = run_one(&mut conn, stmt).await;
        let stop = rs.error.is_some();
        sets.push(rs);
        if stop {
            break;
        }
    }
    sets
}

impl Worker {
    pub fn sql_connect(&self, conn: u64, uri: String, timeout: Duration, net: Net) {
        self.spawn(async move { Msg::SqlConnected { conn, res: open(&uri, timeout, &net).await } });
    }

    /// Connects and drops everything again, for the connection form.
    pub fn sql_test(&self, uri: String, timeout: Duration, net: Net) {
        self.spawn(async move {
            let start = Instant::now();
            let res = open(&uri, timeout, &net).await.map(|(db, _, t)| {
                let via = t.map(|t| format!(" through SSH (host key {})", t.fingerprint)).unwrap_or_default();
                format!("Connected{via} to {} in {} ms", db.label(), start.elapsed().as_millis())
            });
            Msg::Tested { res }
        });
    }

    /// Tables sorted, and which of them are views.
    pub fn sql_tables(&self, conn: u64, db: Db, schema: String) {
        self.spawn(async move {
            let res = async {
                let Pool::My(pool) = &db.pool;
                let rows = sqlx::query(db.dialect.tables()).bind(&schema).fetch_all(pool).await.map_err(e)?;
                let mut names = Vec::new();
                let mut views = Vec::new();
                for r in &rows {
                    let Some(n) = text(r, "name") else { continue };
                    if text(r, "kind").is_some_and(|k| k.contains("VIEW")) {
                        views.push(n.clone());
                    }
                    names.push(n);
                }
                names.sort();
                Ok((names, views))
            }
            .await;
            Msg::Collections { conn, db: schema, res }
        });
    }

    /// One page of a table. `filter` is the text of a WHERE clause and `order` of an ORDER BY, both
    /// as the user typed them. Sends `SqlCols` and then `Docs`.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_page(&self, req: u64, db: Db, schema: String, table: String, filter: String, order: String, limit: u64, offset: u64) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let res = match page(&db, &schema, &table, &filter, &order, limit, offset).await {
                Ok((cols, pk, rows, count, ms)) => {
                    let _ = tx.send(Msg::SqlCols { req, cols, pk });
                    Ok((rows, count, ms))
                }
                Err(err) => Err(err),
            };
            let _ = tx.send(Msg::Docs { req, res });
            ctx.request_repaint();
        });
    }

    /// Runs a Custom Query script on its own connection, whose id is stored in `conn_id` as soon as it
    /// is known so `sql_kill` can stop it.
    pub fn sql_run(&self, req: u64, db: Db, schema: String, text: String, read_only: bool, conn_id: Arc<AtomicU64>) {
        self.spawn(async move {
            let start = Instant::now();
            let sets = run(&db, &schema, &text, read_only, &conn_id).await;
            Msg::SqlRan { req, sets, ms: start.elapsed().as_millis() }
        });
    }

    /// Stops the statement running on connection `connection_id`, from another connection.
    pub fn sql_kill(&self, db: Db, connection_id: u64) {
        self.spawn(async move {
            let Pool::My(pool) = &db.pool;
            let res = raw_sql(&db.dialect.cancel(connection_id)).execute(pool).await.map(|_| "Stopped the running statement".to_string()).map_err(e);
            Msg::Info(res)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Receiver;

    /// Needs a server: QUARRY_TEST_MYSQL, like mysql://root:quarry@127.0.0.1:3306. Skipped when unset.
    /// Returns a worker, its channel, a connected Db whose queries time out after 30 s, and a plain
    /// writable pool for fixtures (the Db's own session is read-only).
    fn live(table: &str, ddl: &str) -> Option<(Worker, Receiver<Msg>, Db, sqlx::MySqlPool)> {
        let uri = std::env::var("QUARRY_TEST_MYSQL").ok()?;
        let (tx, rx) = std::sync::mpsc::channel();
        let w = Worker::new(tx, eframe::egui::Context::default());
        let setup = w.rt.block_on(async {
            let p = sqlx::MySqlPool::connect(&uri).await.unwrap();
            raw_sql("CREATE DATABASE IF NOT EXISTS quarry_test").execute(&p).await.unwrap();
            if !table.is_empty() {
                raw_sql(&format!("DROP TABLE IF EXISTS quarry_test.{table}")).execute(&p).await.unwrap();
                raw_sql(&format!("CREATE TABLE quarry_test.{table} ({ddl})")).execute(&p).await.unwrap();
            }
            p
        });
        w.sql_connect(1, uri, Duration::from_secs(30), Net::default());
        let Msg::SqlConnected { res: Ok((db, dbs, _)), .. } = next(&rx) else { panic!("expected SqlConnected") };
        assert!(dbs.iter().any(|d| d == "quarry_test"), "{dbs:?}");
        Some((w, rx, db, setup))
    }

    fn next(rx: &Receiver<Msg>) -> Msg {
        rx.recv_timeout(Duration::from_secs(15)).expect("worker answer")
    }

    fn exec(w: &Worker, p: &sqlx::MySqlPool, sql: &str) {
        w.rt.block_on(async { raw_sql(sql).execute(p).await.unwrap() });
    }

    fn page_of(w: &Worker, rx: &Receiver<Msg>, db: &Db, table: &str, filter: &str, order: &str, limit: u64, offset: u64) -> (Vec<Col>, Vec<String>, Vec<Document>, u64) {
        w.sql_page(7, db.clone(), "quarry_test".into(), table.into(), filter.into(), order.into(), limit, offset);
        let Msg::SqlCols { cols, pk, .. } = next(rx) else { panic!("expected SqlCols") };
        let Msg::Docs { res: Ok((rows, count, _)), .. } = next(rx) else { panic!("expected Docs") };
        (cols, pk, rows.iter().map(|r| r.decode().unwrap()).collect(), count)
    }

    fn run_all(w: &Worker, rx: &Receiver<Msg>, db: &Db, text: &str, read_only: bool) -> Vec<ResultSet> {
        w.sql_run(9, db.clone(), "quarry_test".into(), text.into(), read_only, Default::default());
        let Msg::SqlRan { sets, .. } = next(rx) else { panic!("expected SqlRan") };
        sets
    }

    #[test]
    fn live_connect_reports_version_dialect_and_tls() {
        let Some((w, rx, db, _)) = live("", "") else { return };
        println!("{} tls={} mode={}", db.label(), db.tls, db.sql_mode);
        assert!(db.version.chars().next().unwrap().is_ascii_digit());
        assert_eq!(db.dialect, Dialect::from_version(&db.version));
        assert!(db.label().starts_with("MySQL ") || db.label().starts_with("MariaDB "));
        let uri = std::env::var("QUARRY_TEST_MYSQL").unwrap();
        w.sql_test(uri, Duration::from_secs(10), Net::default());
        let Msg::Tested { res: Ok(m) } = next(&rx) else { panic!("expected Tested") };
        assert!(m.starts_with("Connected to ") && m.contains(&db.label()) && m.ends_with(" ms"), "{m}");
        w.sql_test("mysql://root:wrong@127.0.0.1:3306".into(), Duration::from_secs(10), Net::default());
        let Msg::Tested { res } = next(&rx) else { panic!("expected Tested") };
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
        w.sql_page(8, db.clone(), "quarry_test".into(), "t_page".into(), "1=1; DELETE FROM t_page".into(), "".into(), 10, 0);
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
        // MariaDB stores JSON as LONGTEXT, so it is text there.
        let json = if db.dialect == Dialect::MariaDb { Kind::Text } else { Kind::Json };
        assert_eq!(kinds, [Kind::Int, Kind::Int, Kind::UInt, Kind::Int, Kind::Decimal, Kind::Float, Kind::Text, Kind::Text, Kind::Binary, Kind::Date, Kind::DateTime, Kind::Time, Kind::Year, json, Kind::Enum, Kind::Set, Kind::Bit]);
        assert!(!cols[0].nullable);
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
        // Long results are cut at the cap.
        let sets = run_all(&w, &rx, &db, "SELECT seq FROM (WITH RECURSIVE s(seq) AS (SELECT 1 UNION ALL SELECT seq + 1 FROM s WHERE seq < 1500) SELECT seq FROM s) q", true);
        if sets[0].error.is_none() {
            assert_eq!((sets[0].rows.len(), sets[0].capped), (RUN_ROWS_MAX, true));
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
        let Pool::My(pool) = &db.pool;
        let n: i64 = w.rt.block_on(async { sqlx::query_scalar("SELECT COUNT(*) FROM quarry_test.t_ro").fetch_one(pool).await.unwrap() });
        assert_eq!(n, 1);
        // Unlocked, the same write runs.
        let sets = run_all(&w, &rx, &db, "DELETE FROM t_ro", false);
        assert_eq!(sets[0].affected, Some(1), "{:?}", sets[0].error);
    }

    #[test]
    fn live_kill_stops_a_running_statement() {
        let Some((w, rx, db, _)) = live("", "") else { return };
        let id = Arc::new(AtomicU64::new(0));
        w.sql_run(3, db.clone(), "quarry_test".into(), "SELECT SLEEP(10)".into(), true, id.clone());
        let start = Instant::now();
        while id.load(Ordering::SeqCst) == 0 {
            assert!(start.elapsed() < Duration::from_secs(5), "no connection id");
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(300));
        w.sql_kill(db, id.load(Ordering::SeqCst));
        let (mut ran, mut info) = (None, None);
        while ran.is_none() || info.is_none() {
            match next(&rx) {
                Msg::SqlRan { sets, ms, .. } => ran = Some((sets, ms)),
                Msg::Info(r) => info = Some(r),
                _ => {}
            }
        }
        assert!(info.unwrap().is_ok());
        assert!(start.elapsed() < Duration::from_secs(6), "the sleep was not interrupted");
        assert!(ran.unwrap().1 < 6000);
    }
}
