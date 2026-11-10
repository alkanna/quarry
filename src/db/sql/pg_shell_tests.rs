//! Live tests of the shell on PostgreSQL: scripts run by Bun, whose queries come back to the relay. They run on every server
//! named by QUARRY_TEST_POSTGRES and QUARRY_TEST_POSTGRES13 and skip when Bun is not installed. Each test makes its own
//! schemas in the shared `quarry_test` database.

use super::testing::{every_pg, live, LivePg};
use super::Db;
use crate::db::{Msg, Res, ShellJob, ShellQuery, SqlRelay, Worker};
use std::sync::mpsc::Receiver;
use std::time::Duration;

fn bun_missing() -> bool {
    let missing = std::process::Command::new("bun").arg("--version").output().is_err();
    if missing {
        println!("skipped: bun is not installed");
    }
    missing
}

/// Fresh schemas of the shared database with `ddl` run in the first, and the `Db` pointing at that database.
fn fixture(l: &LivePg, schemas: &[&str], ddl: &str) -> Db {
    let db = l.quarry_test();
    let pool = l.fixture_pool();
    let drops: String = schemas.iter().map(|s| format!("DROP SCHEMA IF EXISTS {s} CASCADE; CREATE SCHEMA {s};")).collect();
    l.qsql(&pool, &format!("{drops} SET search_path = {}; {ddl}", schemas[0]));
    db
}

fn relay(db: &Db, schema: &str, read_only: bool) -> SqlRelay {
    SqlRelay { db: db.clone(), schema: schema.into(), read_only, conn_id: Default::default() }
}

fn entry(name: &str, tables: &[&str]) -> serde_json::Value {
    serde_json::json!({ "name": name, "db": "quarry_test", "tables": tables, "dialect": "postgres" })
}

fn input(code: &str, main: &str, mongo: serde_json::Value, sql: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({ "code": code, "packages": [], "timeoutMs": 30000, "main": main, "mongo": mongo, "sql": sql })
}

fn job(input: serde_json::Value, sql: Vec<(String, SqlRelay)>) -> ShellJob {
    ShellJob { runtime: "bun".into(), dir: std::env::temp_dir().join("quarry-shell-live-test-pg"), input: input.to_string(), packages: Vec::new(), sql }
}

/// Runs the runner with this input line and these relays, returning the queries it reported and its result.
fn run(w: &Worker, rx: &Receiver<Msg>, input: serde_json::Value, sql: Vec<(String, SqlRelay)>) -> (Vec<ShellQuery>, Res<String>) {
    let (_cancel, cancel_rx) = tokio::sync::oneshot::channel();
    w.shell(5, job(input, sql), cancel_rx);
    let mut queries = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_secs(180)).expect("shell answer") {
            Msg::ShellQuery { q, .. } => queries.push(q),
            Msg::ShellDone { res, .. } => return (queries, res),
            _ => {}
        }
    }
}

/// A script on the one PostgreSQL connection "pg", and the value it returned.
fn script(l: &LivePg, db: &Db, schema: &str, code: &str, tables: &[&str], read_only: bool) -> (Vec<ShellQuery>, Res<serde_json::Value>) {
    let (queries, res) = run(&l.w, &l.rx, input(code, "pg", serde_json::json!([]), vec![entry("pg", tables)]), vec![("pg".into(), relay(db, schema, read_only))]);
    (queries, res.map(|r| serde_json::from_str::<serde_json::Value>(&r).unwrap()["v"].clone()))
}

#[test]
fn values_of_every_kind_reach_the_server_as_themselves() {
    if bun_missing() {
        return;
    }
    every_pg(|l, var| {
        let db = fixture(l, &["t7_values"], "");
        let code = r#"async function run() {
  const text = "it's a \\ back\\\\slash \"quoted\" $1 ?";
  const [a] = await sql`SELECT ${text}::text AS t, ${42} AS i, ${1.5} AS f, ${true} AS b, ${false} AS nb, ${null}::text AS n`;
  const [b] = await sql`SELECT encode(${Buffer.from([0, 255, 16])}, 'hex') AS h, ${Buffer.from([1, 2])} AS raw`;
  const [c] = await sql`SELECT ARRAY[${[1, 2, 3]}] AS arr, ${5} IN (${[]}) AS empty, ${7} IN (${[6, 7]}) AS hit`;
  const [d] = await sql`SELECT ${new Date(0)}::timestamptz AT TIME ZONE 'UTC' AS ts, (${{ k: [1, "x"] }}::jsonb)->'k'->>1 AS j, ${9007199254740993n}::int8 AS big`;
  return { a, b: { h: b.h, raw: Buffer.from(b.raw).toString("hex") }, c, d };
}"#;
        let (_, res) = script(l, &db, "t7_values", code, &[], true);
        let v = res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(v["a"]["t"], "it's a \\ back\\\\slash \"quoted\" $1 ?", "{var}: {v}");
        assert_eq!(v["a"]["i"], 42);
        assert_eq!(v["a"]["f"], "1.5");
        assert_eq!((v["a"]["b"].clone(), v["a"]["nb"].clone(), v["a"]["n"].clone()), (true.into(), false.into(), serde_json::Value::Null), "{var}: {v}");
        assert_eq!(v["b"]["h"], "00ff10");
        assert_eq!(v["b"]["raw"], "0102");
        assert_eq!(v["c"]["arr"], "{1,2,3}");
        assert!(v["c"]["empty"].is_null() && v["c"]["hit"] == true, "{var}: {v}");
        assert_eq!(v["d"]["ts"], "1970-01-01 00:00:00");
        assert_eq!(v["d"]["j"], "x");
        assert_eq!(v["d"]["big"]["$numberLong"], "9007199254740993", "{var}: {v}");
    });
}

#[test]
fn a_jsonb_question_mark_operator_survives() {
    if bun_missing() {
        return;
    }
    every_pg(|l, var| {
        let db = fixture(l, &["t7_jsonb"], "");
        let code = r#"async function run() {
  const [a] = await sql.unsafe(`SELECT '{"a":1}'::jsonb ? 'a' AS yes, '{"a":1}'::jsonb ? 'b' AS no, '{"a":1}'::jsonb ?| array['b', 'a'] AS any`);
  const [b] = await sql`SELECT '{"a":1}'::jsonb ? ${"a"} AS yes, ${"b"} AS b`;
  return { a, b };
}"#;
        let (queries, res) = script(l, &db, "t7_jsonb", code, &[], true);
        let v = res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!((v["a"]["yes"].clone(), v["a"]["no"].clone(), v["a"]["any"].clone()), (true.into(), false.into(), true.into()), "{var}: {v}");
        assert_eq!((v["b"]["yes"].clone(), v["b"]["b"].clone()), (true.into(), "b".into()));
        // The text the runner sent for the template has a $1 and its question mark untouched.
        assert!(queries.iter().any(|q| q.text.contains("? $1")), "{var}: {:?}", queries.iter().map(|q| &q.text).collect::<Vec<_>>());
    });
}

#[test]
fn table_helpers_build_postgres_sql() {
    if bun_missing() {
        return;
    }
    every_pg(|l, var| {
        let db = fixture(l, &["t7_tab_a", "t7_tab_b"], "CREATE TABLE t (id int PRIMARY KEY, name text, n int); INSERT INTO t VALUES (1, 'alice', 10), (2, 'ALBERT', 20), (3, 'bob', 30);");
        let code = r#"async function run() {
  const t = sql.table('t', 't7_tab_a');
  const ids = (rows) => rows.map((r) => r.id);
  const sel = await t.find({ n: { gte: 20 } }, { orderBy: '-id', select: ['id', 'n'] });
  const like = await t.find({ name: { like: 'al%' } }, { orderBy: 'id' });
  const offset = await t.find({}, { orderBy: 'id', offset: 1 });
  const some = await t.find({ id: [1, 3], n: { ne: 99 }, name: { ne: null } }, { orderBy: 'id', limit: 5 });
  const one = await t.findOne({ id: 99 });
  const count = await t.count({ n: { gt: 10 } });
  const qualified = await sql.table('t', 't7_tab_a').count();
  let unqualified;
  try { await sql.table('t').count(); unqualified = 'ran' } catch (e) { unqualified = e.message }
  return { sel, like: ids(like), offset: ids(offset), some: ids(some), one, count, qualified, unqualified };
}"#;
        // The relay runs in the second schema, which has no t, so only the qualified helper finds the table.
        let (queries, res) = script(l, &db, "t7_tab_b", code, &[], true);
        let v = res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(v["sel"], serde_json::json!([{ "id": 3, "n": 30 }, { "id": 2, "n": 20 }]), "{var}");
        assert_eq!(v["like"], serde_json::json!([1, 2]), "LIKE reads as ILIKE: {var}");
        assert_eq!(v["offset"], serde_json::json!([2, 3]));
        assert_eq!(v["some"], serde_json::json!([1, 3]));
        assert!(v["one"].is_null());
        assert_eq!((v["count"].clone(), v["qualified"].clone()), (2.into(), 3.into()));
        assert!(v["unqualified"].as_str().unwrap().contains("does not exist"), "{var}: {v}");
        let text = |n: &str| queries.iter().find(|q| q.text.contains(n)).map(|q| q.text.clone()).unwrap_or_default();
        assert!(text("ILIKE").contains("\"name\" ILIKE $1") && text("OFFSET 1").contains("LIMIT ALL OFFSET 1"), "{var}: {:?}", queries.iter().map(|q| &q.text).collect::<Vec<_>>());
        assert!(text("\"t7_tab_a\"").contains("\"t7_tab_a\".\"t\""));
    });
}

#[test]
fn a_locked_relay_refuses_writes_and_runs_each_statement_in_a_read_only_transaction() {
    if bun_missing() {
        return;
    }
    every_pg(|l, var| {
        let db = fixture(l, &["t7_locked"], "CREATE TABLE w (id int PRIMARY KEY, note text); INSERT INTO w VALUES (1, 'keep');");
        let code = r#"async function run() {
  const refused = [];
  for (const f of [() => w.delete({ id: 1 }), () => w.insert({ id: 9 }), () => sql`UPDATE w SET note = ${'x'}`, () => sql.unsafe('DROP TABLE w'),
    () => sql.unsafe("SELECT set_config('default_transaction_read_only', 'off', false)"), () => sql.unsafe('BEGIN'), () => sql.unsafe('COMMIT'),
    () => sql.unsafe("SELECT 1; DELETE FROM w"), () => sql.unsafe("WITH d AS (DELETE FROM w RETURNING *) SELECT * FROM d"), () => sql.unsafe("PREPARE p AS SELECT 1"), () => sql.unsafe("SET client_encoding = 'SJIS'")]) {
    try { await f(); refused.push(null) } catch (e) { refused.push(e.message) }
  }
  const [s] = await sql.unsafe("SELECT pg_catalog.current_setting('transaction_read_only') AS ro, (SELECT xact_start < query_start FROM pg_stat_activity WHERE pid = pg_backend_pid()) AS own_tx, pg_catalog.current_setting('search_path') AS sp");
  return { refused, after: await w.count(), note: (await w.findOne({ id: 1 })).note, s };
}"#;
        let (_, res) = script(l, &db, "t7_locked", code, &["w"], true);
        let v = res.unwrap_or_else(|x| panic!("{var}: {x}"));
        let refused = v["refused"].as_array().unwrap();
        assert!(refused.iter().all(|m| m.is_string()), "{var}: {v}");
        assert_eq!((v["after"].clone(), v["note"].clone()), (1.into(), "keep".into()), "{var}");
        // Each statement ran alone after its own BEGIN READ ONLY, and the session is pointed at the schema.
        assert_eq!((v["s"]["ro"].clone(), v["s"]["own_tx"].clone()), ("on".into(), true.into()), "{var}: {v}");
        assert!(v["s"]["sp"].as_str().unwrap().contains("t7_locked"), "{var}: {v}");
    });
}

#[test]
fn an_unlocked_relay_writes_in_autocommit_and_insert_returns_a_row_only_when_asked() {
    if bun_missing() {
        return;
    }
    every_pg(|l, var| {
        let db = fixture(l, &["t7_unlocked"], "CREATE TABLE ins (id serial PRIMARY KEY, note text);");
        let code = r#"async function run() {
  const a = await ins.insert({ note: 'a' }, { returning: true });
  const plain = await ins.insert({ note: 'p' });
  const cols = await ins.insert({ note: 'k' }, { returning: ['id'] });
  const b = await ins.insert([{ note: 'b' }, { note: 'c' }]);
  const c = await sql`INSERT INTO ins (note) VALUES (${'d'}) RETURNING id, note`;
  const d = await sql`UPDATE ins SET note = ${'z'} WHERE note = ANY(ARRAY[${['a', 'b']}])`;
  const none = await ins.update({ id: 99 }, { note: 'q' });
  const del = await ins.delete({ note: 'nope' });
  const [s] = await sql.unsafe("SELECT pg_catalog.current_setting('transaction_read_only') AS ro, (SELECT xact_start < query_start FROM pg_stat_activity WHERE pid = pg_backend_pid()) AS own_tx");
  return { a: { id: a.insertId, affected: a.affected, row: a[0] }, plain: { id: typeof plain.insertId, affected: plain.affected, rows: plain.length }, cols: { id: cols.insertId, row: cols[0] }, b: { affected: b.affected, id: typeof b.insertId }, c, d: { affected: d.affected, id: typeof d.insertId }, none: none.affected, del: del.affected, n: await ins.count(), s };
}"#;
        let (_, res) = script(l, &db, "t7_unlocked", code, &["ins"], false);
        let v = res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!((v["a"]["id"].clone(), v["a"]["affected"].clone()), (1.into(), 1.into()), "{var}: {v}");
        assert_eq!(v["a"]["row"]["note"], "a");
        assert_eq!(v["plain"], serde_json::json!({ "id": "undefined", "affected": 1, "rows": 0 }), "{var}: {v}");
        assert_eq!((v["cols"]["id"].clone(), v["cols"]["row"].clone()), (3.into(), serde_json::json!({ "id": 3 })), "{var}: {v}");
        assert_eq!((v["b"]["affected"].clone(), v["b"]["id"].clone()), (2.into(), "undefined".into()), "{var}: {v}");
        assert_eq!(v["c"], serde_json::json!([{ "id": 6, "note": "d" }]));
        assert_eq!((v["d"]["affected"].clone(), v["d"]["id"].clone()), (2.into(), "undefined".into()), "{var}: {v}");
        assert_eq!((v["none"].clone(), v["del"].clone(), v["n"].clone()), (0.into(), 0.into(), 6.into()), "{var}: {v}");
        assert_eq!((v["s"]["ro"].clone(), v["s"]["own_tx"].clone()), ("off".into(), false.into()), "{var}: {v}");
        assert_eq!(l.qone(&l.fixture_pool(), "SELECT count(*)::text FROM t7_unlocked.ins WHERE note = 'z'"), "2");
    });
}

/// Makes (or keeps) a login-less role the insert tests switch to inside their own session, so no global setting changes.
fn insert_role(l: &LivePg, pool: &sqlx::PgPool, schema: &str, grants: &str) {
    l.qsql(pool, "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 't7_ins_role') THEN CREATE ROLE t7_ins_role LOGIN PASSWORD 'pw'; END IF; END $$");
    l.qsql(pool, "ALTER ROLE t7_ins_role LOGIN PASSWORD 'pw'");
    l.qsql(pool, &format!("GRANT USAGE ON SCHEMA {schema} TO t7_ins_role; {grants}"));
}

fn drop_insert_role(l: &LivePg, pool: &sqlx::PgPool) {
    l.qsql(pool, "DROP OWNED BY t7_ins_role; DROP ROLE t7_ins_role");
}

#[test]
fn a_plain_insert_works_with_insert_only_privilege_row_level_security_and_a_do_instead_view() {
    if bun_missing() {
        return;
    }
    every_pg(|l, var| {
        let _ = fixture(
            l,
            &["t7_plain_ins"],
            "CREATE TABLE only_ins (id int PRIMARY KEY, note text); CREATE TABLE rls (id int PRIMARY KEY, note text); ALTER TABLE rls ENABLE ROW LEVEL SECURITY;
             CREATE TABLE base (id int PRIMARY KEY, note text); CREATE VIEW vw AS SELECT * FROM base; CREATE RULE vw_ins AS ON INSERT TO vw DO INSTEAD INSERT INTO base VALUES (NEW.id, NEW.note);",
        );
        let pool = l.fixture_pool();
        insert_role(l, &pool, "t7_plain_ins", "GRANT INSERT ON t7_plain_ins.only_ins TO t7_ins_role; GRANT SELECT, INSERT ON t7_plain_ins.rls TO t7_ins_role; CREATE POLICY p_ins ON t7_plain_ins.rls FOR INSERT TO t7_ins_role WITH CHECK (true); CREATE POLICY p_sel ON t7_plain_ins.rls FOR SELECT TO t7_ins_role USING (false); GRANT SELECT, INSERT ON t7_plain_ins.base, t7_plain_ins.vw TO t7_ins_role;");
        let code = r#"async function run() {
  const out = {};
  for (const [k, f] of [['only', () => only_ins.insert({ id: 1, note: 'a' })], ['rls', () => rls.insert({ id: 1, note: 'b' })], ['view', () => vw.insert({ id: 1, note: 'c' })]]) {
    try { const r = await f(); out[k] = { affected: r.affected, id: typeof r.insertId } } catch (e) { out[k] = e.message }
  }
  return out;
}"#;
        // A second login that holds only these privileges, so no session setting of the test login changes.
        let (tx, rx) = std::sync::mpsc::channel();
        let w2 = Worker::new(tx, eframe::egui::Context::default());
        w2.sql_connect(1, l.uri.replacen("postgres:quarry@", "t7_ins_role:pw@", 1), Duration::from_secs(30), crate::db::Net::default());
        let Msg::SqlConnected { res: Ok((role_db, _, _)), .. } = rx.recv_timeout(Duration::from_secs(20)).unwrap() else { panic!("{var}: the role could not connect") };
        let (_, res) = run(&w2, &rx, input(code, "pg", serde_json::json!([]), vec![entry("pg", &["only_ins", "rls", "vw"])]), vec![("pg".into(), relay(&role_db.at("quarry_test"), "t7_plain_ins", false))]);
        let res = res.map(|r| serde_json::from_str::<serde_json::Value>(&r).unwrap()["v"].clone());
        let v = res.unwrap_or_else(|x| panic!("{var}: {x}"));
        for k in ["only", "rls", "view"] {
            assert_eq!(v[k], serde_json::json!({ "affected": 1, "id": "undefined" }), "{var} {k}: {v}");
        }
        assert_eq!(l.qone(&pool, "SELECT (SELECT count(*) FROM t7_plain_ins.only_ins) || '/' || (SELECT count(*) FROM t7_plain_ins.rls) || '/' || (SELECT count(*) FROM t7_plain_ins.base)"), "1/1/1");
        drop_insert_role(l, &pool);
    });
}

#[test]
fn an_unlocked_relay_recovers_from_a_failed_transaction_with_rollback() {
    if bun_missing() {
        return;
    }
    every_pg(|l, var| {
        let db = fixture(l, &["t7_abort"], "CREATE TABLE a (id int PRIMARY KEY); INSERT INTO a VALUES (1);");
        let code = r#"async function run() {
  await sql.unsafe('BEGIN');
  let failed;
  try { await sql`INSERT INTO a VALUES (${1})` } catch (e) { failed = e.message }
  await sql.unsafe('ROLLBACK');
  const after = await sql`SELECT count(*)::int AS n FROM a WHERE id = ${1}`;
  return { failed, n: after[0].n };
}"#;
        let (_, res) = script(l, &db, "t7_abort", code, &[], false);
        let v = res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert!(v["failed"].as_str().unwrap().contains("duplicate key"), "{var}: {v}");
        assert_eq!(v["n"], 1, "{var}: {v}");
    });
}

#[test]
fn a_server_error_that_says_more_than_is_an_ordinary_error() {
    if bun_missing() {
        return;
    }
    every_pg(|l, var| {
        let db = fixture(l, &["t7_more"], "");
        let code = r#"async function run() {
  let first;
  try { await sql.unsafe('SELECT (SELECT i FROM generate_series(1, 2) i)') } catch (e) { first = e.message }
  const [again] = await sql`SELECT ${7}::int AS n`;
  return { first, again };
}"#;
        for locked in [true, false] {
            let (_, res) = script(l, &db, "t7_more", code, &[], locked);
            let v = res.unwrap_or_else(|x| panic!("{var} locked {locked}: {x}"));
            assert!(v["first"].as_str().unwrap().contains("more than one row"), "{var}: {v}");
            assert_eq!(v["again"]["n"], 7, "{var} locked {locked}: {v}");
        }
    });
}

#[test]
fn a_cut_result_cancels_its_backend_and_releases_its_locks() {
    if bun_missing() {
        return;
    }
    every_pg(|l, var| {
        let db = fixture(l, &["t7_cut"], "CREATE TABLE lk (id int PRIMARY KEY); INSERT INTO lk VALUES (1);");
        let code = r#"async function run() {
  const [{ pid }] = await sql`SELECT pg_backend_pid()::int AS pid`;
  let cut;
  try { await sql`SELECT lk.id FROM lk, generate_series(1, 100001) WHERE ${1} = 1` } catch (e) { cut = e.message }
  let later;
  try { await sql`SELECT ${1}` } catch (e) { later = e.message }
  let locks = -1, alive = -1;
  for (let i = 0; i < 40; i++) {
    [{ alive }] = await obs`SELECT count(*)::int AS alive FROM pg_stat_activity WHERE pid = ${pid}`;
    [{ locks }] = await obs`SELECT count(*)::int AS locks FROM pg_locks WHERE pid = ${pid}`;
    if (alive === 0) break;
    await new Promise((r) => setTimeout(r, 250));
  }
  return { cut, later, locks, alive };
}"#;
        let input = input(code, "pg", serde_json::json!([]), vec![entry("pg", &[]), entry("obs", &[])]);
        let (_, res) = run(&l.w, &l.rx, input, vec![("pg".into(), relay(&db, "t7_cut", true)), ("obs".into(), relay(&db, "t7_cut", true))]);
        let v: serde_json::Value = serde_json::from_str(&res.unwrap_or_else(|x| panic!("{var}: {x}"))).unwrap();
        let v = &v["v"];
        assert!(v["cut"].as_str().unwrap().contains("more than 100000 rows"), "{var}: {v}");
        assert!(v["later"].as_str().unwrap().contains("closed after a result was cut"), "{var}: {v}");
        assert_eq!((v["alive"].clone(), v["locks"].clone()), (0.into(), 0.into()), "{var}: {v}");
    });
}

#[test]
fn a_digit_after_a_value_does_not_join_the_placeholder_number() {
    if bun_missing() {
        return;
    }
    every_pg(|l, var| {
        let db = fixture(l, &["t7_digit"], "");
        let code = r#"async function run() {
  const v = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
  try { await sql`SELECT ${v[0]}1, ${v[1]}, ${v[2]}, ${v[3]}, ${v[4]}, ${v[5]}, ${v[6]}, ${v[7]}, ${v[8]}, ${v[9]}, ${v[10]}` } catch (e) { return e.message }
  return 'ran';
}"#;
        let (queries, _) = script(l, &db, "t7_digit", code, &[], true);
        let text = queries.first().map(|q| q.text.clone()).unwrap_or_default();
        assert!(text.contains("$1 1,") && !text.contains("$11,"), "{var}: {text}");
    });
}

#[test]
fn behind_a_pooler_each_statement_has_its_own_transaction_and_the_session_stays_untouched() {
    if bun_missing() {
        return;
    }
    every_pg(|l, var| {
        let db = fixture(l, &["t7_pool"], "CREATE TABLE t (id int PRIMARY KEY, note text);");
        let pooled = super::pg::pooled_for_test(&db, &l.uri);
        let want_path = l.one("SHOW search_path");
        let want_ro = l.one("SHOW default_transaction_read_only");
        // Locked: the unqualified table is found through the search_path of the statement's own transaction.
        let code = r#"async function run() {
  const [a] = await sql`SELECT current_setting('transaction_read_only') AS ro, current_schema() AS s`;
  let write;
  try { await sql`INSERT INTO t VALUES (${1}, 'x')` } catch (e) { write = e.message }
  return { a, write, n: (await t.find()).length };
}"#;
        let (_, res) = script(l, &pooled, "t7_pool", code, &["t"], true);
        let v = res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!((v["a"]["ro"].clone(), v["a"]["s"].clone(), v["n"].clone()), ("on".into(), "t7_pool".into(), 0.into()), "{var}: {v}");
        assert!(v["write"].as_str().is_some(), "{var}: {v}");
        // Unlocked: each statement is begun READ WRITE and committed, the script's own transaction control is refused,
        // and a failed statement is rolled back.
        let code = r#"async function run() {
  const refused = [];
  for (const w of ['BEGIN', 'START TRANSACTION', 'COMMIT', 'END', 'ROLLBACK', 'SAVEPOINT a', 'RELEASE a', 'PREPARE TRANSACTION \'x\'', '/* c */ begin']) {
    try { await sql.unsafe(w); refused.push(null) } catch (e) { refused.push(e.message) }
  }
  await t.insert({ id: 1, note: 'a' });
  let dup;
  try { await sql`INSERT INTO t VALUES (${1}, 'b')` } catch (e) { dup = e.message }
  const [s] = await sql`SELECT current_setting('transaction_read_only') AS ro, current_schema() AS s, ${1}::int AS one`;
  return { refused, dup, s, n: await t.count() };
}"#;
        let (_, res) = script(l, &pooled, "t7_pool", code, &["t"], false);
        let v = res.unwrap_or_else(|x| panic!("{var}: {x}"));
        let msg = "not available";
        assert!(v["refused"].as_array().unwrap().iter().all(|m| m.as_str().is_some_and(|m| m.contains(msg))), "{var}: {v}");
        assert!(v["dup"].as_str().unwrap().contains("duplicate key"), "{var}: {v}");
        assert_eq!((v["s"]["ro"].clone(), v["s"]["s"].clone(), v["n"].clone()), ("off".into(), "t7_pool".into(), 1.into()), "{var}: {v}");
        assert_eq!(l.qone(&l.fixture_pool(), "SELECT count(*)::text FROM t7_pool.t"), "1", "{var}: committed");
        // Nothing reached a session another client could get.
        assert_eq!((l.one("SHOW search_path"), l.one("SHOW default_transaction_read_only")), (want_path, want_ro), "{var}");
    });
}

#[test]
fn the_columns_of_an_empty_locked_answer_are_planned_inside_the_read_only_transaction() {
    if bun_missing() {
        return;
    }
    every_pg(|l, var| {
        // Behind a pooler the session itself is writable, so only the statement's own transaction protects describe.
        let db = fixture(
            l,
            &["t7_desc"],
            "CREATE TABLE log (n int); CREATE FUNCTION g(x int) RETURNS int LANGUAGE plpgsql VOLATILE AS $f$ BEGIN IF current_setting('transaction_read_only') = 'off' THEN INSERT INTO t7_desc.log VALUES (x); END IF; RETURN x; END $f$;
             CREATE FUNCTION f(x int) RETURNS int LANGUAGE plpgsql IMMUTABLE AS 'BEGIN RETURN t7_desc.g(x); END';",
        );
        let pooled = super::pg::pooled_for_test(&db, &l.uri);
        let (_, res) = script(l, &pooled, "t7_desc", "async function run() { return await sql`SELECT t7_desc.f(${1}) AS x WHERE false` }", &[], true);
        let v = res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(v, serde_json::json!([]), "{var}");
        let pool = l.fixture_pool();
        assert_eq!(l.qone(&pool, "SELECT count(*)::text FROM t7_desc.log"), "0", "{var}: planning wrote outside the read-only transaction");
        // The control: the same call from a writable session does write, so the zero above means something.
        l.qsql(&pool, "SELECT t7_desc.f(1)");
        assert_eq!(l.qone(&pool, "SELECT count(*)::text FROM t7_desc.log"), "1", "{var}");
    });
}

#[test]
fn an_unlocked_relay_refuses_when_something_moved_how_text_is_read() {
    if bun_missing() {
        return;
    }
    every_pg(|l, var| {
        let db = fixture(l, &["t7_enc"], "CREATE VIEW v_set AS SELECT * FROM pg_settings;");
        // The lexer sees an UPDATE of a view, not of pg_settings, so only the readback before the next fill can catch it.
        for (name, value, what) in [("client_encoding", "SJIS", "client_encoding"), ("standard_conforming_strings", "off", "standard_conforming_strings"), ("backslash_quote", "on", "backslash_quote")] {
            let code = format!(
                r#"async function run() {{
  const first = (await sql`SELECT ${{'a\\b'}}::text AS t`)[0].t;
  let direct;
  try {{ await sql.unsafe("UPDATE pg_settings SET setting = 'SJIS' WHERE name = 'client_encoding'"); direct = 'ran' }} catch (e) {{ direct = e.message }}
  await sql.unsafe("UPDATE v_set SET setting = '{value}' WHERE name = '{name}'");
  let after;
  try {{ await sql`SELECT ${{'x'}}::text AS t`; after = 'ran' }} catch (e) {{ after = e.message }}
  let again;
  try {{ await sql.unsafe('SELECT 1'); again = 'ran' }} catch (e) {{ again = e.message }}
  return {{ first, after, again, direct }};
}}"#
            );
            let (_, res) = script(l, &db, "t7_enc", &code, &[], false);
            let v = res.unwrap_or_else(|x| panic!("{var} {name}: {x}"));
            assert_eq!(v["first"], "a\\b", "{var} {name}: {v}");
            assert!(v["after"].as_str().unwrap().contains(what) && v["after"].as_str().unwrap().contains("changed how text is read"), "{var} {name}: {v}");
            assert!(v["again"].as_str().unwrap().contains("lost") || v["again"].as_str().unwrap().contains("changed"), "{var} {name}: {v}");
            // Written out, an UPDATE of pg_settings is refused before it runs.
            assert!(v["direct"].as_str().unwrap().contains("not allowed in the shell"), "{var} {name}: {v}");
        }
    });
}

#[test]
fn stop_cancels_a_running_pg_sleep() {
    if bun_missing() {
        return;
    }
    every_pg(|l, var| {
        let db = fixture(l, &["t7_stop"], "");
        let (cancel, cancel_rx) = tokio::sync::oneshot::channel();
        let started = std::time::Instant::now();
        let input = input("async function run() { await sql`SELECT pg_sleep(31)`; return 1 }", "pg", serde_json::json!([]), vec![entry("pg", &[])]);
        l.w.shell(5, job(input, vec![("pg".into(), relay(&db, "t7_stop", true))]), cancel_rx);
        let active = || l.one("SELECT count(*)::text FROM pg_stat_activity WHERE state = 'active' AND query LIKE '%pg_sleep(31)%' AND pid <> pg_backend_pid()");
        // Bun may have to install first, so wait for the statement itself.
        let begun = std::time::Instant::now();
        while active() == "0" {
            assert!(begun.elapsed() < Duration::from_secs(150), "{var}: the statement never started");
            std::thread::sleep(Duration::from_millis(200));
        }
        cancel.send(()).unwrap();
        let res = loop {
            match l.rx.recv_timeout(Duration::from_secs(30)).expect("shell answer") {
                Msg::ShellDone { res, .. } => break res,
                _ => {}
            }
        };
        assert_eq!(res, Err("Stopped.".to_string()), "{var}");
        let gone = std::time::Instant::now();
        while active() != "0" {
            assert!(gone.elapsed() < Duration::from_secs(10), "{var}: pg_sleep still running after Stop");
            std::thread::sleep(Duration::from_millis(200));
        }
        assert!(started.elapsed() < Duration::from_secs(170));
    });
}

#[test]
fn a_postgres_attachment_runs_from_a_mysql_shell_and_from_a_mongo_shell() {
    if bun_missing() {
        return;
    }
    let my = live("QUARRY_TEST_MYSQL");
    every_pg(|l, var| {
        let db = fixture(l, &["t7_attach"], "CREATE TABLE a (id int PRIMARY KEY, name text); INSERT INTO a VALUES (1, 'pg row');");
        if let Some(my) = &my {
            let code = "async function run() {
  const [m] = await sql`SELECT ${'one'} AS one, 40 + 2 AS n`;
  const [p] = await other.sql`SELECT ${\"it's\"}::text AS t, ${true} AS b`;
  const rows = await other.a.find({ name: { like: 'PG%' } });
  let refused;
  try { await other.a.delete({ id: 1 }) } catch (e) { refused = e.message }
  return { m, p, rows, refused };
}";
            let input = input(code, "shop", serde_json::json!([]), vec![serde_json::json!({ "name": "shop", "db": "", "tables": [], "dialect": "mysql" }), entry("other", &["a"])]);
            let (queries, res) = run(&my.w, &my.rx, input, vec![("shop".into(), relay(&my.db, "", true)), ("other".into(), relay(&db, "t7_attach", true))]);
            let v: serde_json::Value = serde_json::from_str(&res.unwrap_or_else(|x| panic!("{var}: {x}"))).unwrap();
            let v = &v["v"];
            assert_eq!((v["m"]["one"].clone(), v["m"]["n"].clone()), ("one".into(), 42.into()), "{var}: {v}");
            assert_eq!((v["p"]["t"].clone(), v["p"]["b"].clone()), ("it's".into(), true.into()), "{var}: {v}");
            assert_eq!(v["rows"], serde_json::json!([{ "id": 1, "name": "pg row" }]));
            assert!(v["refused"].as_str().is_some_and(|m| !m.is_empty()), "{var}: {v}");
            assert!(queries.iter().any(|q| q.conn == "other" && q.text.contains("$1")) && queries.iter().any(|q| q.conn == "shop" && q.text.contains('?')), "{var}");
        }
        // A Mongo shell with the same attachment, when a Mongo server is at hand.
        if let Ok(uri) = std::env::var("QUARRY_TEST_MONGO") {
            let code = "async function run() {
  const coll = db.collection('pg7_attach');
  await coll.deleteMany({});
  await coll.insertOne({ k: 'mongo' });
  const [p] = await other.sql`SELECT name FROM a WHERE id = ${1}`;
  const docs = await coll.find({}).toArray();
  await coll.drop();
  return { name: p.name, k: docs[0].k };
}";
            let input = input(code, "docs", serde_json::json!([{ "name": "docs", "uri": uri, "db": "quarry_pg7", "proxy": null, "collections": [], "readOnly": false }]), vec![entry("other", &["a"])]);
            let (_, res) = run(&l.w, &l.rx, input, vec![("other".into(), relay(&db, "t7_attach", true))]);
            let v: serde_json::Value = serde_json::from_str(&res.unwrap_or_else(|x| panic!("{var}: {x}"))).unwrap();
            assert!(v.to_string().contains("pg row") && v.to_string().contains("mongo"), "{var}: {v}");
        }
    });
}
