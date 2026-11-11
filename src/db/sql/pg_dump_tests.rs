//! Live tests of the PostgreSQL SQL dump and the script run, on every server named by QUARRY_TEST_POSTGRES and
//! QUARRY_TEST_POSTGRES13, and once through the PgBouncer. Each test makes databases of its own with a unique name and
//! drops them again, so tests of one server run side by side and never touch a database another test uses.

use super::csv::Source;
use super::dump::{dump_to, run_file, DumpPlan, DumpTable, Ran};
use super::import::Progress;
use super::testing::{every_pg, live_pg_at, live_pgbouncer, LivePg};
use super::Db;
use crate::db::{Msg, Res};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

fn tag(var: &str) -> &'static str {
    if var.ends_with("13") { "13" } else { "18" }
}

/// Databases a test made, dropped when it ends, failed or not.
struct Dbs<'a> {
    l: &'a LivePg,
    names: Vec<String>,
}

impl<'a> Dbs<'a> {
    fn new(l: &'a LivePg) -> Dbs<'a> {
        Dbs { l, names: Vec::new() }
    }

    fn drop_one(l: &LivePg, name: &str) {
        let _ = l.w.rt.block_on(async { sqlx::raw_sql(&format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)")).execute(&l.pool).await });
    }

    /// A new empty database `name`, and a plain pool on it outside the session guard, for fixtures.
    fn make(&mut self, name: &str) -> (Db, PgPool) {
        Dbs::drop_one(self.l, name);
        // template0 is never connected to, so two tests creating a database at once do not meet "template1 is being accessed".
        self.l.w.rt.block_on(async { sqlx::raw_sql(&format!("CREATE DATABASE \"{name}\" TEMPLATE template0")).execute(&self.l.pool).await }).unwrap_or_else(|x| panic!("create {name}: {x}"));
        self.names.push(name.to_string());
        (self.l.db.at(name), pool_of(self.l, name))
    }
}

impl Drop for Dbs<'_> {
    fn drop(&mut self) {
        for n in &self.names {
            Dbs::drop_one(self.l, n);
        }
    }
}

fn pool_of(l: &LivePg, name: &str) -> PgPool {
    let o = PgConnectOptions::from_str(&l.uri).unwrap().database(name);
    l.w.rt.block_on(async { PgPoolOptions::new().max_connections(3).connect_with(o).await.unwrap() })
}

fn sql(l: &LivePg, pool: &PgPool, s: &str) {
    l.qsql(pool, s);
}

fn one(l: &LivePg, pool: &PgPool, s: &str) -> String {
    l.qone(pool, s)
}

fn file(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("quarry_pgdump_{name}.sql"))
}

fn plan(schema: &str, whole: bool, tables: Vec<DumpTable>) -> DumpPlan {
    DumpPlan { schema: schema.into(), tables, create_db: whole, extras: true, rows_per_insert: 1000, snapshot: true, filter: None }
}

fn table(name: &str, view: bool, structure: bool, content: bool, drop: bool) -> DumpTable {
    DumpTable { name: name.into(), view, structure, content, drop }
}

fn dump(l: &LivePg, db: &Db, p: &DumpPlan, path: &Path) -> Res<u64> {
    let (count, cancel, id) = (AtomicU64::new(0), AtomicBool::new(false), AtomicU64::new(0));
    l.w.rt.block_on(dump_to(db, p, path.to_path_buf(), &count, &cancel, &id)).map(|d| d.rows)
}

fn run(l: &LivePg, db: &Db, schema: &str, path: &Path) -> Res<Ran> {
    l.w.rt.block_on(run_file(db.clone(), schema.into(), Source::File(path.to_path_buf()), false, Arc::new(Progress::default()), Arc::new(AtomicBool::new(false)), Arc::new(AtomicU64::new(0))))
}

fn run_text(l: &LivePg, db: &Db, schema: &str, text: &str) -> Res<Ran> {
    l.w.rt.block_on(run_file(db.clone(), schema.into(), Source::Mem(Arc::new(text.as_bytes().to_vec())), false, Arc::new(Progress::default()), Arc::new(AtomicBool::new(false)), Arc::new(AtomicU64::new(0))))
}

/// The first column of every row of a catalog query, read under an empty search_path like the dump does, so every name
/// prints qualified.
fn lines(l: &LivePg, pool: &PgPool, q: &str) -> Vec<String> {
    l.w.rt.block_on(async {
        let mut c = pool.acquire().await.unwrap();
        sqlx::raw_sql("SET search_path = ''; SET standard_conforming_strings = on").execute(&mut *c).await.unwrap();
        let rows = sqlx::raw_sql(q).fetch_all(&mut *c).await.unwrap_or_else(|x| panic!("{q}: {x}"));
        rows.iter().map(|r| super::worker::text_at(r, 0).unwrap_or_default()).collect()
    })
}

/// What psql does with a script: each statement on its own through the simple protocol, reading the server's own
/// standard_conforming_strings after every statement. The session starts with it off, as one of a database set so would.
fn psql_like(l: &LivePg, pool: &PgPool, text: &str) {
    use super::pg_script::PgSplitter;
    l.w.rt.block_on(async {
        let mut c = pool.acquire().await.unwrap();
        let mut sp = PgSplitter::new(true);
        sp.feed(text.as_bytes());
        sp.finish();
        while let Some(p) = sp.next_piece().unwrap() {
            sqlx::raw_sql(&p.text).execute(&mut *c).await.unwrap_or_else(|x| panic!("statement at line {} failed: {x}\n{}", p.line, p.text));
            let r = sqlx::query("SHOW standard_conforming_strings").persistent(false).fetch_one(&mut *c).await.unwrap();
            sp.set_plain_backslash(super::worker::text_at(&r, 0).as_deref() != Some("on"));
        }
    });
}

const USER_NS: &str = "n.nspname <> 'information_schema' AND n.nspname NOT LIKE 'pg\\_%'";

/// What the catalogs say of every user object, one text per object, in an order that does not depend on oids, and a count and
/// a checksum of every table's rows. Two databases with the same list hold the same objects and data.
fn shape(l: &LivePg, pool: &PgPool) -> Vec<String> {
    let mut out = Vec::new();
    let ver: i64 = one(l, pool, "SELECT current_setting('server_version_num')").parse().unwrap();
    let mut queries: Vec<(&str, String)> = vec![
        ("schema", format!("SELECT n.nspname || ' ' || COALESCE(obj_description(n.oid, 'pg_namespace'), '') FROM pg_namespace n WHERE {USER_NS} ORDER BY 1")),
        ("extension", "SELECT e.extname || ' ' || e.extnamespace::regnamespace FROM pg_extension e WHERE e.extname <> 'plpgsql' ORDER BY 1".into()),
        ("relation", format!("SELECT c.oid::regclass || ' ' || c.relkind::text || c.relpersistence::text || ' ' || c.relispopulated || ' ' || COALESCE(c.reloptions::text, '') FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.relkind IN ('r', 'v', 'm', 'S') AND {USER_NS} ORDER BY 1")),
        ("column", format!("SELECT c.oid::regclass || ' ' || a.attnum || ' ' || a.attname || ' ' || format_type(a.atttypid, a.atttypmod) || ' nn=' || a.attnotnull || ' id=' || a.attidentity::text || ' gen=' || a.attgenerated::text || ' def=' || COALESCE(pg_get_expr(d.adbin, d.adrelid), '') || ' coll=' || a.attcollation::regcollation FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum WHERE a.attnum > 0 AND NOT a.attisdropped AND c.relkind IN ('r', 'v', 'm') AND {USER_NS} ORDER BY 1")),
        ("constraint", format!("SELECT CASE WHEN k.conrelid <> 0 THEN k.conrelid::regclass::text ELSE k.contypid::regtype::text END || ' ' || k.conname || ' ' || k.contype::text || ' ' || pg_get_constraintdef(k.oid) FROM pg_constraint k JOIN pg_namespace n ON n.oid = k.connamespace WHERE {USER_NS} ORDER BY 1")),
        ("index", format!("SELECT pg_get_indexdef(i.indexrelid) || ' cl=' || i.indisclustered || ' valid=' || i.indisvalid FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid JOIN pg_namespace n ON n.oid = c.relnamespace WHERE {USER_NS} ORDER BY 1")),
        ("view", format!("SELECT c.oid::regclass || ' ' || pg_get_viewdef(c.oid, true) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.relkind IN ('v', 'm') AND {USER_NS} ORDER BY 1")),
        ("function", format!("SELECT pg_get_functiondef(p.oid) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE p.prokind IN ('f', 'p') AND {USER_NS} AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_proc'::regclass AND d.objid = p.oid AND d.deptype = 'e') ORDER BY 1")),
        ("trigger", "SELECT pg_get_triggerdef(t.oid) || ' en=' || t.tgenabled::text FROM pg_trigger t WHERE NOT t.tgisinternal ORDER BY 1".into()),
        ("type", format!("SELECT t.oid::regtype || ' ' || t.typtype::text || ' ' || COALESCE((SELECT string_agg(e.enumlabel, '|' ORDER BY e.enumsortorder) FROM pg_enum e WHERE e.enumtypid = t.oid), '') || ' ' || COALESCE(format_type(NULLIF(t.typbasetype, 0), t.typtypmod), '') || ' nn=' || t.typnotnull || ' def=' || COALESCE(pg_get_expr(t.typdefaultbin, 0), '') || ' ' || COALESCE((SELECT string_agg(a.attname || ':' || format_type(a.atttypid, a.atttypmod) || '/' || a.attcollation::regcollation, ',' ORDER BY a.attnum) FROM pg_attribute a WHERE a.attrelid = t.typrelid AND a.attnum > 0 AND t.typtype = 'c'), '') || ' ' || COALESCE((SELECT r.rngsubtype::regtype::text FROM pg_range r WHERE r.rngtypid = t.oid), '') FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace WHERE t.typtype IN ('e', 'd', 'r', 'c') AND t.typcategory <> 'A' AND (t.typrelid = 0 OR EXISTS (SELECT 1 FROM pg_class k WHERE k.oid = t.typrelid AND k.relkind = 'c')) AND {USER_NS} AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_type'::regclass AND d.objid = t.oid AND d.deptype = 'e') ORDER BY 1")),
        ("sequence", "SELECT schemaname || '.' || sequencename || ' ' || data_type || ' ' || start_value || ' ' || increment_by || ' ' || min_value || ' ' || max_value || ' ' || cache_size || ' ' || cycle || ' last=' || COALESCE(last_value::text, 'null') || ' owner=' || COALESCE((SELECT d.refobjid::regclass::text || '.' || d.refobjsubid || ' ' || d.deptype::text FROM pg_depend d WHERE d.classid = 'pg_class'::regclass AND d.objid = format('%I.%I', schemaname, sequencename)::regclass AND d.refclassid = 'pg_class'::regclass AND d.deptype IN ('a', 'i') LIMIT 1), '') FROM pg_sequences WHERE schemaname NOT LIKE 'pg\\_%' AND schemaname <> 'information_schema' ORDER BY 1".into()),
        ("comment", "SELECT pg_describe_object(d.classoid, d.objoid, d.objsubid) || ' = ' || d.description FROM pg_description d WHERE d.objoid >= 16384 ORDER BY 1".into()),
    ];
    // The options of a range type: subtype, operator class, collation, difference function and, from 14, the multirange type.
    queries.push(("range", format!("SELECT r.rngtypid::regtype || ' sub=' || r.rngsubtype::regtype || ' opc=' || oc.opcname || ' coll=' || r.rngcollation::regcollation || ' diff=' || r.rngsubdiff::regproc{} FROM pg_range r JOIN pg_opclass oc ON oc.oid = r.rngsubopc JOIN pg_type t ON t.oid = r.rngtypid JOIN pg_namespace n ON n.oid = t.typnamespace WHERE {USER_NS} ORDER BY 1", if ver >= 140000 { " || ' multi=' || (to_jsonb(r) ->> 'rngmultitypid')::oid::regtype" } else { "" })));
    for (what, q) in queries {
        for line in lines(l, pool, &q) {
            out.push(format!("{what}: {line}"));
        }
    }
    // Rows: a count and one checksum over the text of every row, whatever its order.
    let tables = lines(l, pool, &format!("SELECT format('%I.%I', n.nspname, c.relname) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.relkind IN ('r', 'm') AND {USER_NS} ORDER BY 1"));
    for t in tables {
        let r = lines(l, pool, &format!("SELECT count(*) || ' ' || COALESCE(md5(string_agg(x::text, E'\\n' ORDER BY x::text)), '') FROM {t} x"));
        out.push(format!("rows of {t}: {}", r[0]));
    }
    out
}

fn same(what: &str, a: &[String], b: &[String]) {
    let (only_a, only_b): (Vec<&String>, Vec<&String>) = (a.iter().filter(|x| !b.contains(x)).collect(), b.iter().filter(|x| !a.contains(x)).collect());
    assert!(only_a.is_empty() && only_b.is_empty(), "{what}: the restored database differs.\nonly in the original:\n{}\nonly in the restored one:\n{}", only_a.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n"), only_b.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n"));
    assert_eq!(a.len(), b.len(), "{what}");
}

const FIXTURE: &str = r#"
CREATE EXTENSION citext;
CREATE EXTENSION pg_trgm;
CREATE SCHEMA app;
COMMENT ON SCHEMA app IS 'the app''s schema \ with a backslash';
CREATE SCHEMA "Odd ""Schema";
CREATE SCHEMA empty_schema;
CREATE TYPE app.mood AS ENUM ('sad', 'ok', 'it''s a \ fine', 'ünï');
COMMENT ON TYPE app.mood IS 'moods';
CREATE DOMAIN app.posint AS integer DEFAULT 1 NOT NULL CONSTRAINT posint_pos CHECK (VALUE > 0);
CREATE DOMAIN "Odd ""Schema".txt AS text COLLATE "C" CHECK (length(VALUE) < 100);
CREATE TYPE app.pair AS (a integer, b text);
CREATE TYPE app.fl AS RANGE (subtype = float8);
CREATE TYPE app.fl2 AS RANGE (subtype = float8, subtype_diff = float8mi);
CREATE TYPE app.tr AS RANGE (subtype = text, subtype_opclass = text_pattern_ops, collation = "C");
CREATE TYPE app.cm AS (t text COLLATE "C", u text);
CREATE SEQUENCE app.standalone START 100 INCREMENT 5 MINVALUE 100 MAXVALUE 100000 CACHE 3 CYCLE;
COMMENT ON SEQUENCE app.standalone IS 'standalone';
CREATE FUNCTION app.double(i integer) RETURNS integer LANGUAGE sql IMMUTABLE AS $$ SELECT i * 2 $$;
CREATE FUNCTION "Odd ""Schema"."fn;weird"(a text) RETURNS text LANGUAGE plpgsql AS $body$ BEGIN /* a; /* nested */ b; */ RETURN a || E'\';'; END $body$;
COMMENT ON FUNCTION app.double(integer) IS 'twice';
CREATE TABLE app."Items" (
  id integer GENERATED ALWAYS AS IDENTITY (START WITH 10 INCREMENT BY 2) PRIMARY KEY,
  "Mixed Case ""Name""" text NOT NULL DEFAULT 'it''s \ default',
  m app.mood, p app.posint, pr app.pair, r app.fl,
  ts timestamptz, tsn timestamp, d date, t time, iv interval,
  ba bytea, js jsonb, jn json, arr integer[], tarr text[], f4 real, f8 double precision, nm numeric(12,3), b boolean, u uuid, ip inet,
  cit citext, tsv tsvector, bits bit(5),
  doubled integer GENERATED ALWAYS AS (id * 2) STORED,
  s2 integer DEFAULT nextval('app.standalone'),
  ser serial,
  odd "Odd ""Schema".txt,
  UNIQUE (ser),
  CONSTRAINT "chk ""q""" CHECK (nm IS NULL OR nm > -1000000)
) WITH (fillfactor = 70);
COMMENT ON TABLE app."Items" IS 'the items, with a ''quote''';
COMMENT ON COLUMN app."Items"."Mixed Case ""Name""" IS 'name \ column';
COMMENT ON CONSTRAINT "chk ""q""" ON app."Items" IS 'a check';
CREATE TABLE app.child (
  id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
  item integer NOT NULL REFERENCES app."Items" (id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
  note text,
  CONSTRAINT excl EXCLUDE USING btree (item WITH =)
);
CREATE INDEX child_note_trgm ON app.child USING gin (note public.gin_trgm_ops) WHERE note IS NOT NULL;
CREATE INDEX "Items expr" ON app."Items" (lower("Mixed Case ""Name""")) INCLUDE (id);
COMMENT ON INDEX app."Items expr" IS 'an index';
ALTER TABLE app.child CLUSTER ON child_pkey;
CREATE TABLE app.empty_t (id int);
ALTER TABLE app.empty_t ADD CONSTRAINT bs_chk CHECK (id IS NULL OR 'x\y' <> '');
CREATE SEQUENCE app.owned_seq OWNED BY app.empty_t.id;
CREATE VIEW app.bs_view AS SELECT id, 'a\b'::text AS lit FROM app.empty_t WHERE 'c\d' <> '';
CREATE TABLE app."two
lines" (x int);
CREATE TABLE app."a;--b" ("c;d" text PRIMARY KEY);
CREATE TABLE "Odd ""Schema"."T ""x""" ("a b" int PRIMARY KEY, "c;d" text);
CREATE UNLOGGED TABLE app.unlogged_t (id int);
CREATE FUNCTION app.trg() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.note := coalesce(NEW.note, 'n') || ':' || NEW.item; RETURN NEW; END $$;
CREATE TRIGGER trg_bi BEFORE INSERT ON app.child FOR EACH ROW EXECUTE FUNCTION app.trg();
COMMENT ON TRIGGER trg_bi ON app.child IS 'fills the note';
CREATE TRIGGER trg_off AFTER UPDATE ON app.child FOR EACH ROW EXECUTE FUNCTION app.trg();
ALTER TABLE app.child DISABLE TRIGGER trg_off;
INSERT INTO app."Items" ("Mixed Case ""Name""", m, p, pr, r, ts, tsn, d, t, iv, ba, js, jn, arr, tarr, f4, f8, nm, b, u, ip, cit, tsv, bits, odd)
SELECT CASE WHEN g % 7 = 0 THEN 'it''s \ back\\slash; DROP TABLE x; -- ' || E'line\nbreak\ttab ' || chr(128512) ELSE 'name ' || g END,
  (ARRAY['sad', 'ok', 'it''s a \ fine', 'ünï']::app.mood[])[1 + g % 4],
  g, ROW(g, 'x''y\')::app.pair, CASE WHEN g % 3 = 0 THEN NULL ELSE '[1,2)'::app.fl END,
  CASE WHEN g % 101 = 0 THEN 'infinity'::timestamptz WHEN g % 103 = 0 THEN '-infinity'::timestamptz ELSE '2026-01-02 03:04:05.123456+05:30'::timestamptz + g * interval '1 hour' END,
  '2026-01-02 03:04:05.5'::timestamp + g * interval '1 minute', '2026-01-02'::date + g, '03:04:05.25'::time, make_interval(years => g % 3, days => g, secs => 1.5),
  decode('00ff10' || lpad(to_hex(g), 4, '0'), 'hex'), jsonb_build_object('k', g, 's', 'a"b\c'), json_build_array(g, 'x'),
  ARRAY[g, NULL, 3], ARRAY['a b', 'c"d', NULL, 'e\f', '', 'it''s'],
  CASE g % 5 WHEN 0 THEN 'NaN'::real WHEN 1 THEN 'Infinity'::real WHEN 2 THEN '-0'::real ELSE (g / 7.0)::real END,
  CASE g % 5 WHEN 0 THEN 'NaN'::float8 WHEN 1 THEN '-Infinity'::float8 WHEN 2 THEN '-0'::float8 ELSE 0.1 + g / 3.0 END,
  CASE WHEN g % 97 = 0 THEN 'NaN'::numeric ELSE g * 1.001 END, g % 2 = 0, md5(g::text)::uuid, ('10.0.' || g % 256 || '.1')::inet,
  ('CaSe' || g)::citext, to_tsvector('simple', 'a b ' || g), (g % 32)::bit(5), 'odd ' || g
FROM generate_series(1, 2500) g;
INSERT INTO app."Items" ("Mixed Case ""Name""") VALUES ('nulls');
INSERT INTO app.child (item, note) SELECT id, CASE WHEN id % 3 = 0 THEN NULL ELSE 'c' || id END FROM app."Items" ORDER BY id LIMIT 1200;
INSERT INTO app."a;--b" VALUES ('x'), ('y;z');
INSERT INTO "Odd ""Schema"."T ""x""" VALUES (1, 'a\'), (2, NULL);
INSERT INTO app."two
lines" VALUES (1);
INSERT INTO app.unlogged_t VALUES (1);
CREATE VIEW app.b_base AS SELECT i.id, i."Mixed Case ""Name""" AS n FROM app."Items" i;
CREATE VIEW app.a_dependent WITH (security_barrier = true) AS SELECT id, upper(n) AS un FROM app.b_base;
CREATE VIEW app.c_check AS SELECT * FROM app.child WHERE id > 0 WITH LOCAL CHECK OPTION;
COMMENT ON VIEW app.a_dependent IS 'depends on b_base';
COMMENT ON COLUMN app.a_dependent.un IS 'upper';
CREATE MATERIALIZED VIEW app.m_sum AS SELECT item, count(*) AS c FROM app.child GROUP BY item;
CREATE UNIQUE INDEX m_sum_item ON app.m_sum (item);
COMMENT ON MATERIALIZED VIEW app.m_sum IS 'sums';
CREATE FUNCTION app.rowf(t app."Items") RETURNS integer LANGUAGE sql AS $$ SELECT $1.id $$;
SELECT setval('app.standalone', 4000);
"#;

/// A function body of the SQL standard form, which only PostgreSQL 14 and later knows.
const ATOMIC_FN: &str = "CREATE FUNCTION app.viewf() RETURNS bigint LANGUAGE sql BEGIN ATOMIC SELECT count(*) FROM app.b_base; END;";

/// From 14: a range type with a multirange name of its own.
const MULTIRANGE: &str = "CREATE TYPE app.mr AS RANGE (subtype = int8, multirange_type_name = app.my_multi_name);";

/// From 18 a NOT NULL is a constraint: one that is NOT VALID has NULL rows behind it, one is named, one is NO INHERIT.
const NOT_NULLS: &str = "CREATE TABLE app.nnv (id int PRIMARY KEY, a int, b int, c int, d int);
INSERT INTO app.nnv VALUES (1, NULL, 1, 1, 1);
ALTER TABLE app.nnv ADD CONSTRAINT a_must NOT NULL a NOT VALID;
ALTER TABLE app.nnv ADD CONSTRAINT named_b NOT NULL b;
ALTER TABLE app.nnv ALTER COLUMN c SET NOT NULL;
ALTER TABLE app.nnv ADD CONSTRAINT d_noinherit NOT NULL d NO INHERIT;";

fn version(l: &LivePg, pool: &PgPool) -> i64 {
    one(l, pool, "SELECT current_setting('server_version_num')").parse().unwrap()
}

fn fixture(l: &LivePg, pool: &PgPool) {
    sql(l, pool, FIXTURE);
    let v = version(l, pool);
    if v >= 140000 {
        sql(l, pool, ATOMIC_FN);
        sql(l, pool, MULTIRANGE);
    }
    if v >= 180000 {
        sql(l, pool, NOT_NULLS);
    }
}

#[test]
fn live_a_database_dumped_and_run_back_into_an_empty_one_is_the_same() {
    every_pg(|l, var| {
        let t = tag(var);
        let mut dbs = Dbs::new(l);
        let (src, src_pool) = dbs.make(&format!("qdump_whole_src_{t}"));
        let (dst, dst_pool) = dbs.make(&format!("qdump_whole_dst_{t}"));
        fixture(l, &src_pool);
        let before = shape(l, &src_pool);
        // The session of the connection is read-only, which is what a locked connection is, and the dump still works.
        let ro = l.w.rt.block_on(async { sqlx::raw_sql("SHOW default_transaction_read_only").fetch_all(&src.pg_pool().unwrap()).await }).unwrap();
        assert_eq!(super::worker::text_at(&ro[0], 0).as_deref(), Some("on"), "{var}");
        let path = file(&format!("whole_{t}"));
        let rows = dump(l, &src, &plan("", true, Vec::new()), &path).unwrap_or_else(|x| panic!("{var}: {x}"));
        // Items (2501) and child (1200), a;--b (2), T"x" (2), two lines (1), unlogged (1), and from 18 the nnv row.
        let v18 = version(l, &src_pool) >= 180000;
        assert_eq!(rows, 2501 + 1200 + 2 + 2 + 1 + 1 + v18 as u64, "{var}");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("SET search_path = '';") && text.contains("CREATE EXTENSION IF NOT EXISTS \"citext\""), "{var}");
        if v18 {
            // The NOT VALID one is added after the rows with its mark, and the named one keeps its name.
            assert!(text.contains("ADD CONSTRAINT \"a_must\" NOT NULL a NOT VALID") && text.contains("ADD CONSTRAINT \"named_b\" NOT NULL b;"), "{var}");
            assert!(!text.contains("\"a\" integer NOT NULL"), "{var}: a column whose NOT NULL is NOT VALID is written without it");
        }
        // The order a restore needs: types, sequences, tables, rows, keys, foreign keys, views, triggers.
        let at = |s: &str| text.find(s).unwrap_or_else(|| panic!("{var}: {s} is not in the dump"));
        assert!(at("CREATE TYPE \"app\".\"mood\"") < at("CREATE SEQUENCE IF NOT EXISTS \"app\".\"standalone\""), "{var}");
        assert!(at("CREATE SEQUENCE IF NOT EXISTS \"app\".\"standalone\"") < at("CREATE TABLE \"app\".\"Items\""), "{var}");
        assert!(at("CREATE TABLE \"app\".\"Items\"") < at("INSERT INTO \"app\".\"Items\""), "{var}");
        assert!(at("INSERT INTO \"app\".\"child\"") < at("ADD CONSTRAINT \"child_pkey\""), "{var}");
        assert!(at("ADD CONSTRAINT \"child_pkey\"") < at("FOREIGN KEY"), "{var}");
        assert!(at("FOREIGN KEY") < at("CREATE VIEW \"app\".\"b_base\""), "{var}");
        assert!(at("CREATE VIEW \"app\".\"b_base\"") < at("CREATE VIEW \"app\".\"a_dependent\""), "{var}: a view comes after the view it reads from, not in name order");
        assert!(at("CREATE VIEW \"app\".\"a_dependent\"") < at("CREATE TRIGGER trg_bi"), "{var}");
        assert!(at("INSERT INTO \"app\".\"child\"") < at("CREATE TRIGGER trg_bi"), "{var}: the load does not fire the trigger");
        // A name with a line break cannot leave its comment line.
        assert!(text.contains("-- Table app.two lines\n"), "{var}");
        // The whole file is one reading under both standard_conforming_strings: no plain literal holds a backslash.
        assert!(!text.contains(" '\\"), "{var}");
        let ran = run(l, &dst, "", &path).unwrap_or_else(|x| panic!("{var}: {x}"));
        assert!(ran.statements > 60, "{var}: {}", ran.statements);
        let after = shape(l, &dst_pool);
        same(var, &before, &after);
        // The sequences carry on from where they were.
        sql(l, &dst_pool, "INSERT INTO app.child (item) VALUES ((SELECT id FROM app.\"Items\" ORDER BY id OFFSET 1200 LIMIT 1))");
        assert_eq!(one(l, &dst_pool, "SELECT max(id) FROM app.child"), "1201", "{var}: the identity sequence continues after the last row");
        assert_eq!(one(l, &dst_pool, "SELECT nextval('app.standalone')"), one(l, &src_pool, "SELECT nextval('app.standalone')"), "{var}");
        // The trigger works again, and the function that needs a table's row type and the SQL body ones too.
        assert_eq!(one(l, &dst_pool, "SELECT note FROM app.child WHERE id = 1201"), "n:2410", "{var}");
        assert_eq!(one(l, &dst_pool, "SELECT app.double(21)"), "42", "{var}");
        assert_eq!(one(l, &dst_pool, "SELECT \"Odd \"\"Schema\".\"fn;weird\"('a')"), "a';", "{var}");
        // A gzip name writes the same script compressed.
        let gz = file(&format!("whole_{t}.sql.gz")).with_extension("gz");
        dump(l, &src, &plan("", true, Vec::new()), &gz).unwrap();
        assert!(std::fs::metadata(&gz).unwrap().len() < text.len() as u64 / 2, "{var}");
        for p in [path, gz] {
            let _ = std::fs::remove_file(p);
        }
    });
}

/// A subset: the tables of one schema with their types, sequences, functions and triggers, which then run a second time into
/// the same database (DROP first) without a clash.
#[test]
fn live_some_tables_of_a_schema_dump_and_replace_what_is_there() {
    every_pg(|l, var| {
        let t = tag(var);
        let mut dbs = Dbs::new(l);
        let (src, src_pool) = dbs.make(&format!("qdump_sub_src_{t}"));
        let (dst, dst_pool) = dbs.make(&format!("qdump_sub_dst_{t}"));
        fixture(l, &src_pool);
        let tables = vec![
            table("Items", false, true, true, true),
            table("child", false, true, true, true),
            table("empty_t", false, true, false, true),
            table("b_base", true, true, false, true),
            table("a_dependent", true, true, false, true),
            table("m_sum", true, true, false, true),
        ];
        let path = file(&format!("sub_{t}"));
        let rows = dump(l, &src, &plan("app", false, tables.clone()), &path).unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(rows, 2501 + 1200, "{var}");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("DROP VIEW IF EXISTS") && text.contains("DROP TABLE IF EXISTS \"app\".\"Items\", \"app\".\"child\""), "{var}");
        // On a subset the types may exist already, so each is made inside a block that ignores that.
        assert!(text.contains("EXCEPTION WHEN duplicate_object"), "{var}");
        assert!(!text.contains("\"Odd \"\"Schema\".\"T \"\"x\"\"\""), "{var}: a table that was not asked for is not written");
        for round in 0..2 {
            run(l, &dst, "", &path).unwrap_or_else(|x| panic!("{var} round {round}: {x}"));
            for q in ["SELECT count(*) FROM app.child", "SELECT md5(string_agg(x::text, E'\\n' ORDER BY x::text)) FROM app.\"Items\" x", "SELECT md5(string_agg(x::text, E'\\n' ORDER BY x::text)) FROM app.child x", "SELECT pg_get_viewdef('app.a_dependent'::regclass)", "SELECT count(*) FROM app.m_sum"] {
                assert_eq!(one(l, &dst_pool, q), one(l, &src_pool, q), "{var} round {round}: {q}");
            }
        }
        // The structure the tables came with: columns, keys and indexes of the dumped tables.
        let keep = |v: &[String]| {
            v.iter()
                .filter(|x| x.starts_with("constraint: app.\"Items\"") || x.starts_with("constraint: app.child") || x.starts_with("column: app.\"Items\"") || x.starts_with("column: app.child") || x.starts_with("column: app.m_sum") || x.starts_with("column: app.b_base")
                    || (x.starts_with("index: ") && [" ON app.\"Items\" ", " ON app.child ", " ON app.m_sum "].iter().any(|t| x.contains(t))))
                .cloned()
                .collect::<Vec<_>>()
        };
        let (a, b) = (keep(&shape(l, &src_pool)), keep(&shape(l, &dst_pool)));
        assert!(a.len() >= 40, "{var}: {}", a.len());
        same(var, &a, &b);
        let _ = std::fs::remove_file(path);

        // The rows a filter keeps, with no structure, into a table that is there.
        let mut filtered = plan("app", false, vec![table("Items", false, false, true, false)]);
        filtered.filter = Some("id <= 20".into());
        let fp = file(&format!("sub_filter_{t}"));
        assert_eq!(dump(l, &src, &filtered, &fp).unwrap(), 6, "{var}");
        sql(l, &dst_pool, "TRUNCATE app.\"Items\" CASCADE");
        run(l, &dst, "app", &fp).unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(one(l, &dst_pool, "SELECT count(*) || ' ' || max(id) FROM app.\"Items\""), "6 20", "{var}");
        // The filter is one statement that only reads.
        filtered.filter = Some("true); DROP TABLE app.child; --".into());
        assert!(dump(l, &src, &filtered, &fp).is_err(), "{var}");
        filtered.filter = Some("id < 0 OR (SELECT set_config('default_transaction_read_only', 'off', false)) = 'x'".into());
        assert!(dump(l, &src, &filtered, &fp).is_err(), "{var}");
        assert_eq!(one(l, &src_pool, "SELECT count(*) FROM app.child"), "1200", "{var}");
        let _ = std::fs::remove_file(fp);
        // A table that is gone says so.
        let gone = plan("app", false, vec![table("nope", false, true, true, false)]);
        assert!(dump(l, &src, &gone, &file(&format!("gone_{t}"))).unwrap_err().contains("no longer exists"), "{var}");
        assert!(!file(&format!("gone_{t}")).exists(), "{var}: a failed dump leaves no file");
    });
}

/// Values that are hard to write: each must read back to the same value under both settings of standard_conforming_strings.
#[test]
fn live_awkward_values_read_back_under_both_string_settings() {
    every_pg(|l, var| {
        let t = tag(var);
        let mut dbs = Dbs::new(l);
        let (src, src_pool) = dbs.make(&format!("qdump_val_src_{t}"));
        let (dst, dst_pool) = dbs.make(&format!("qdump_val_dst_{t}"));
        sql(l, &src_pool, "CREATE TABLE v (id int PRIMARY KEY, s text, b bytea, f4 real, f8 float8, n numeric, ts timestamptz, d date, iv interval, a text[], j jsonb, r int4range, tz timetz, m money, e text)");
        sql(l, &src_pool, r#"INSERT INTO v VALUES
            (1, E'back\\slash \\\\ two\\n', '\x00ff5c27', 'NaN', 'Infinity', 'NaN', '2026-03-29 02:30:00+02', '0044-03-15 BC', '1 year 2 mons 3 days 04:05:06.789', ARRAY['a\b', 'c"d', NULL], '{"k": "a\\u0000b\\\\"}', '[1,10)', '10:00:00+05:30', '$1.50', ''),
            (2, 'quote '' and "double"', '\x', '-0', '-Infinity', '-12345678901234567890.123456789', 'infinity', '5874897-12-31', '-3 days', '{}', 'null', 'empty', '23:59:59.999999-00:30', '-$0.05', NULL),
            (3, E'tab\there\nnewline\r\ncrlf; -- not a comment /* nor this */ $$ not a quote $$', E'\\x', '1.17549435e-38', '5e-324', '0.000000000000000000001', '2026-01-02 03:04:05.123456-12', '2026-02-28', '00:00:00', ARRAY[]::text[], '[1, 2, {"a": [true, null]}]', '(,5]', '00:00:00+14', '$0.00', 'ünï ☃ 😀')"#);
        let before = shape(l, &src_pool);
        let path = file(&format!("values_{t}"));
        dump(l, &src, &plan("public", false, vec![table("v", false, true, true, false)]), &path).unwrap_or_else(|x| panic!("{var}: {x}"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("E'") && text.contains("\\\\x00ff5c27"), "{var}: a value with a backslash is an E'' string with the backslash doubled");
        // Run it with the setting off: the file must not depend on it.
        sql(l, &dst_pool, &format!("ALTER DATABASE \"qdump_val_dst_{t}\" SET standard_conforming_strings = off"));
        // A new session of the database starts with it off; Quarry's own sessions refuse that, so the file is run by a plain pool.
        let pool_off = pool_of(l, &format!("qdump_val_dst_{t}"));
        assert_eq!(one(l, &pool_off, "SHOW standard_conforming_strings"), "off", "{var}");
        l.w.rt.block_on(async {
            let mut c = pool_off.acquire().await.unwrap();
            sqlx::raw_sql(&text).execute(&mut *c).await.unwrap_or_else(|x| panic!("{var}: the dump under standard_conforming_strings = off: {x}"));
        });
        let off = shape(l, &pool_off);
        same(&format!("{var} scs off"), &before, &off);
        sql(l, &dst_pool, "TRUNCATE v");
        sql(l, &dst_pool, &format!("ALTER DATABASE \"qdump_val_dst_{t}\" RESET standard_conforming_strings"));
        // And through Quarry's own runner, which pins it on.
        sql(l, &dst_pool, "DROP TABLE v");
        run(l, &dst, "", &path).unwrap_or_else(|x| panic!("{var}: {x}"));
        same(&format!("{var} scs on"), &before, &shape(l, &dst_pool));
        let _ = std::fs::remove_file(path);
    });
}

#[test]
fn live_a_script_with_bodies_comments_and_strings_runs_statement_by_statement() {
    every_pg(|l, var| {
        let t = tag(var);
        let mut dbs = Dbs::new(l);
        let (db, pool) = dbs.make(&format!("qdump_script_{t}"));
        let script = r#"
-- header comment; with a semicolon
CREATE TABLE s (id int PRIMARY KEY, t text);
/* a /* nested ; */ comment; */
CREATE FUNCTION f1() RETURNS int LANGUAGE plpgsql AS $$ BEGIN PERFORM 1; RETURN 1; END $$;
CREATE FUNCTION f2() RETURNS text LANGUAGE plpgsql AS $tag$ BEGIN RETURN $$ ; $$ || E'\';' || 'it''s'; END $tag$;
INSERT INTO s VALUES (1, E'it\'s; one'), (2, 'a;b'), (3, $q$ it's ; $$ $q$);
CREATE RULE r AS ON INSERT TO s DO ALSO (NOTIFY a; NOTIFY b);
SELECT 'x'
;
CREATE TABLE "weird;--name" ("a;b" int); COMMENT ON TABLE "weird;--name" IS 'c;d'
"#;
        let ran = run_text(l, &db, "public", script).unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(ran.statements, 8, "{var}");
        assert_eq!(one(l, &pool, "SELECT count(*) FROM s"), "3", "{var}");
        assert_eq!(one(l, &pool, "SELECT t FROM s WHERE id = 1"), "it's; one", "{var}");
        assert_eq!(one(l, &pool, "SELECT t FROM s WHERE id = 3"), " it's ; $$ ", "{var}");
        assert_eq!(one(l, &pool, "SELECT f2()"), " ; ';it's", "{var}");
        assert_eq!(one(l, &pool, "SELECT obj_description('\"weird;--name\"'::regclass)"), "c;d", "{var}");
        // The schema the dialog names is where unqualified names land.
        sql(l, &pool, "CREATE SCHEMA other");
        run_text(l, &db, "other", "CREATE TABLE in_other (x int)").unwrap();
        assert_eq!(one(l, &pool, "SELECT to_regclass('other.in_other')::text"), "other.in_other", "{var}");
        // A file may set its own search_path.
        run_text(l, &db, "other", "SET search_path = ''; CREATE TABLE public.by_name (x int)").unwrap();
        assert_eq!(one(l, &pool, "SELECT to_regclass('public.by_name')::text"), "by_name", "{var}");

        // A COPY FROM stdin is refused at its line and nothing after it runs.
        let e = run_text(l, &db, "public", "CREATE TABLE before_copy (a int);\n\nCOPY s (id, t) FROM stdin;\n9\tx\n\\.\nCREATE TABLE after_copy (a int);\n").err().unwrap_or_else(|| panic!("{var}: COPY FROM stdin ran"));
        assert!(e.contains("Line 3") && e.contains("COPY ... FROM stdin") && e.contains("stay applied"), "{var}: {e}");
        assert_eq!(one(l, &pool, "SELECT to_regclass('before_copy') IS NOT NULL"), "t", "{var}");
        assert_eq!(one(l, &pool, "SELECT to_regclass('after_copy') IS NULL"), "t", "{var}");
        assert_eq!(one(l, &pool, "SELECT count(*) FROM s"), "3", "{var}");
        // A psql command is refused with its line.
        let e = run_text(l, &db, "public", "SELECT 1;\n\\connect other\nCREATE TABLE after_meta (a int);").unwrap_err();
        assert!(e.contains("Line 2") && e.contains("\\connect") && e.contains("psql"), "{var}: {e}");
        assert_eq!(one(l, &pool, "SELECT to_regclass('after_meta') IS NULL"), "t", "{var}");

        // A failure names the statement and its line, shows it, and says what ran.
        let e = run_text(l, &db, "public", "CREATE TABLE ok_one (a int);\nCREATE TABLE ok_two (a int);\n\nINSERT INTO nope VALUES (1);\nCREATE TABLE never (a int);").unwrap_err();
        assert!(e.contains("Statement 3 (line 4) failed") && e.contains("nope") && e.contains("INSERT INTO nope") && e.contains("stay applied"), "{var}: {e}");
        assert_eq!(one(l, &pool, "SELECT count(*) FROM pg_class WHERE relname IN ('ok_one', 'ok_two')"), "2", "{var}: the statements before it stay");
        assert_eq!(one(l, &pool, "SELECT to_regclass('never') IS NULL"), "t", "{var}");
        // A transaction the file opens keeps its place, and one it never ends is reported.
        run_text(l, &db, "public", "BEGIN; CREATE TABLE tx_ok (a int); INSERT INTO tx_ok VALUES (1); COMMIT;").unwrap();
        assert_eq!(one(l, &pool, "SELECT count(*) FROM tx_ok"), "1", "{var}");
        let e = run_text(l, &db, "public", "BEGIN; CREATE TABLE tx_lost (a int);").unwrap_err();
        assert!(e.contains("never ended it") && e.contains("rolled back"), "{var}: {e}");
        assert_eq!(one(l, &pool, "SELECT to_regclass('tx_lost') IS NULL"), "t", "{var}");
        let e = run_text(l, &db, "public", "BEGIN; CREATE TABLE tx_half (a int); SELECT 1/0; COMMIT;").unwrap_err();
        assert!(e.contains("transaction the file opened is rolled back"), "{var}: {e}");
        assert_eq!(one(l, &pool, "SELECT to_regclass('tx_half') IS NULL"), "t", "{var}");
        // Statements that cannot share a transaction run, because each is its own.
        Dbs::drop_one(l, "qdump_script_made");
        dbs.names.push("qdump_script_made".into());
        run_text(l, &db, "public", "CREATE INDEX CONCURRENTLY s_t ON s (t); VACUUM s; CREATE DATABASE \"qdump_script_made\" TEMPLATE template0;").unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(one(l, &pool, "SELECT count(*) FROM pg_indexes WHERE indexname = 's_t'"), "1", "{var}");
        // A locked connection refuses to run a file at all.
        let locked = l.w.rt.block_on(run_file(db.clone(), "public".into(), Source::Mem(Arc::new(b"SELECT 1".to_vec())), true, Arc::new(Progress::default()), Arc::new(AtomicBool::new(false)), Arc::new(AtomicU64::new(0))));
        assert!(locked.err().is_some_and(|e| e.contains("locked")), "{var}");
        // Text that is not UTF-8 is refused, not mangled.
        let bad = l.w.rt.block_on(run_file(db.clone(), "public".into(), Source::Mem(Arc::new(b"SELECT '\xff'".to_vec())), false, Arc::new(Progress::default()), Arc::new(AtomicBool::new(false)), Arc::new(AtomicU64::new(0))));
        assert!(bad.err().is_some_and(|e| e.contains("not valid UTF-8")), "{var}");
        // A script's own SET of standard_conforming_strings changes how the rest reads.
        run_text(l, &db, "public", "SET standard_conforming_strings = off; CREATE TABLE scs_off (t text); INSERT INTO scs_off VALUES ('it\\'s; x'); INSERT INTO scs_off VALUES ('b')").unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(one(l, &pool, "SELECT string_agg(t, ',' ORDER BY t) FROM scs_off"), "b,it's; x", "{var}");
    });
}

/// A piece the splitter and the server read differently must never run as several statements the progress does not count.
#[test]
fn live_a_piece_the_server_reads_as_several_statements_never_runs_unseen() {
    every_pg(|l, var| {
        let t = tag(var);
        let mut dbs = Dbs::new(l);
        let (db, pool) = dbs.make(&format!("qdump_multi_{t}"));
        sql(l, &pool, "CREATE TABLE v1 (a int); CREATE TABLE v2 (a int); CREATE TABLE v3 (a int); CREATE TABLE mt (a int)");
        // A function called begin with an argument called atomic is no SQL body, so the DROP and the END are statements of their own.
        let ran = run_text(l, &db, "public", "CREATE FUNCTION begin(atomic int) RETURNS int LANGUAGE sql AS 'select 1'; DROP TABLE v1; END;").unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(ran.statements, 3, "{var}: the DROP ran inside the CREATE's piece and the run never counted it");
        assert_eq!(one(l, &pool, "SELECT to_regclass('v1') IS NULL"), "t", "{var}");
        // A SET LOCAL that the COMMIT undoes: the string after it is a plain one again, so the DROP is a statement of its own.
        let ran = run_text(l, &db, "public", "BEGIN; SET LOCAL standard_conforming_strings = off; COMMIT; SELECT 'a\\'; DROP TABLE v2; --';").unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(ran.statements, 5, "{var}: the splitter went on reading with backslashes after the COMMIT");
        assert_eq!(one(l, &pool, "SELECT to_regclass('v2') IS NULL"), "t", "{var}");
        // A client_encoding that reads the backslash as part of a character ends the run before anything it could hide.
        let e = run_text(l, &db, "public", "SET client_encoding = 'SJIS'; SELECT E'Á\\'; DROP TABLE v3; --';").unwrap_err();
        assert!(e.contains("client_encoding") && e.contains("SJIS") && e.contains("stay applied"), "{var}: {e}");
        assert_eq!(one(l, &pool, "SELECT to_regclass('v3') IS NOT NULL"), "t", "{var}: the DROP behind the encoding ran");
        let e = run_text(l, &db, "public", "SET client_encoding = 'LATIN1'; CREATE TABLE enc (t text); INSERT INTO enc VALUES ('é')").unwrap_err();
        assert!(e.contains("client_encoding"), "{var}: {e}");
        assert_eq!(one(l, &pool, "SELECT to_regclass('enc') IS NULL"), "t", "{var}: a UTF-8 file went in as Latin-1");
        // Whatever the cause, the server refuses a piece of several statements when it is sent on its own.
        let e = l.w.rt.block_on(async {
            let mut c = pool.acquire().await.unwrap();
            super::pg_dump::run_text(&mut c, "INSERT INTO mt VALUES (1); INSERT INTO mt VALUES (2)").await.err()
        });
        assert!(e.is_some_and(|e| e.to_string().contains("multiple commands")), "{var}");
        assert_eq!(one(l, &pool, "SELECT count(*) FROM mt"), "0", "{var}");
        // A statement the server runs outside a transaction block runs through an unnamed statement just the same.
        run_text(l, &db, "public", "VACUUM mt; CREATE INDEX CONCURRENTLY mt_a ON mt (a); REINDEX TABLE CONCURRENTLY mt; DROP INDEX CONCURRENTLY mt_a").unwrap_or_else(|x| panic!("{var}: {x}"));
    });
}

/// Two-phase commit hands the transaction to the server, which the run could not follow.
#[test]
fn live_two_phase_commit_statements_are_refused_in_a_file() {
    every_pg(|l, var| {
        let t = tag(var);
        let mut dbs = Dbs::new(l);
        let (db, pool) = dbs.make(&format!("qdump_2pc_{t}"));
        for s in ["BEGIN; CREATE TABLE tp (a int); PREPARE TRANSACTION 'qdump_tp'", "COMMIT PREPARED 'qdump_none'", "ROLLBACK PREPARED 'qdump_none'"] {
            let e = run_text(l, &db, "public", s).unwrap_err();
            assert!(e.contains("hand the transaction to the server") && e.contains("line 1"), "{var}: {s}: {e}");
        }
        let _ = l.w.rt.block_on(async { sqlx::raw_sql("ROLLBACK PREPARED 'qdump_tp'").execute(&pool).await });
        assert_eq!(one(l, &pool, "SELECT to_regclass('tp') IS NULL"), "t", "{var}");
    });
}

/// A catalog text prints `'a\b'::text` plain, which a client with standard_conforming_strings off reads as another value.
/// The file pins the setting itself, so a script runner that works like psql restores the same database.
#[test]
fn live_a_dump_read_by_a_client_with_standard_conforming_strings_off_restores_the_same() {
    every_pg(|l, var| {
        let t = tag(var);
        let mut dbs = Dbs::new(l);
        let (src, src_pool) = dbs.make(&format!("qdump_bs_src_{t}"));
        let (_, dst_pool) = dbs.make(&format!("qdump_bs_dst_{t}"));
        sql(l, &src_pool, r#"CREATE TABLE d (id int PRIMARY KEY, t text NOT NULL DEFAULT 'a\b', u text, CONSTRAINT chk CHECK (t <> 'x\y'));
CREATE INDEX d_part ON d (id) WHERE t <> 'p\q';
CREATE VIEW dv AS SELECT id, 'v\w'::text AS lit FROM d WHERE t <> 'c\d';
CREATE FUNCTION dtrg() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$;
CREATE TRIGGER dt BEFORE UPDATE ON d FOR EACH ROW WHEN (OLD.t <> 'w\z') EXECUTE FUNCTION dtrg();
INSERT INTO d (id, u) VALUES (1, 'q\r')"#);
        let before = shape(l, &src_pool);
        let path = file(&format!("bs_{t}"));
        dump(l, &src, &plan("public", false, vec![table("d", false, true, true, false), table("dv", true, true, false, false)]), &path).unwrap_or_else(|x| panic!("{var}: {x}"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("SET standard_conforming_strings = on;\n"), "{var}");
        sql(l, &dst_pool, &format!("ALTER DATABASE \"qdump_bs_dst_{t}\" SET standard_conforming_strings = off"));
        let off = pool_of(l, &format!("qdump_bs_dst_{t}"));
        assert_eq!(one(l, &off, "SHOW standard_conforming_strings"), "off", "{var}");
        psql_like(l, &off, &text);
        same(var, &before, &shape(l, &off));
        assert_eq!(one(l, &off, "SELECT t FROM public.d"), "a\\b", "{var}");
        let _ = std::fs::remove_file(path);
    });
}

/// A default or generated column that calls a function naming a table, and a column typed as another table's row type, need
/// that function or table written first, whatever the names sort like.
#[test]
fn live_functions_and_tables_that_need_each_other_are_written_in_order() {
    every_pg(|l, var| {
        let t = tag(var);
        let mut dbs = Dbs::new(l);
        let (src, src_pool) = dbs.make(&format!("qdump_order_src_{t}"));
        let (dst, dst_pool) = dbs.make(&format!("qdump_order_dst_{t}"));
        sql(l, &src_pool, "CREATE TABLE zt (id int PRIMARY KEY); CREATE FUNCTION zrow(t zt) RETURNS int LANGUAGE sql AS 'select $1.id'; CREATE TABLE zu (a int DEFAULT zrow(ROW(5)::zt)); CREATE TABLE za (r zt, rs zt[])");
        let mut names = vec![table("za", false, true, false, false), table("zt", false, true, false, false), table("zu", false, true, false, false)];
        if version(l, &src_pool) >= 140000 {
            sql(l, &src_pool, "CREATE FUNCTION zcount() RETURNS bigint LANGUAGE sql BEGIN ATOMIC SELECT count(*) FROM zt; END; CREATE TABLE zv (n bigint DEFAULT zcount())");
            names.push(table("zv", false, true, false, false));
        }
        let before = shape(l, &src_pool);
        let path = file(&format!("order_{t}"));
        dump(l, &src, &plan("public", false, names), &path).unwrap_or_else(|x| panic!("{var}: {x}"));
        run(l, &dst, "", &path).unwrap_or_else(|x| panic!("{var}: {x}"));
        same(var, &before, &shape(l, &dst_pool));
        sql(l, &dst_pool, "INSERT INTO public.zu DEFAULT VALUES");
        assert_eq!(one(l, &dst_pool, "SELECT a FROM public.zu"), "5", "{var}");
        let _ = std::fs::remove_file(path);
    });
}

/// Starts `text` as a run, presses Stop once it sleeps and returns what the run said.
fn stopped_run(l: &LivePg, db: &Db, text: &str, var: &str) -> String {
    let (prog, id, cancel) = (Arc::new(Progress::default()), Arc::new(AtomicU64::new(0)), Arc::new(AtomicBool::new(false)));
    let src = Source::Mem(Arc::new(text.as_bytes().to_vec()));
    l.w.sql_run_file(7, db.clone(), "public".into(), src, false, prog.clone(), id.clone(), cancel.clone());
    let t0 = std::time::Instant::now();
    while prog.rows.load(Ordering::Relaxed) < 1 || id.load(Ordering::SeqCst) == 0 {
        assert!(t0.elapsed().as_secs() < 20, "{var}: the script did not start");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    l.w.sql_kill(db.clone(), id.load(Ordering::SeqCst), cancel.clone());
    let t0 = std::time::Instant::now();
    loop {
        assert!(t0.elapsed().as_secs() < 20, "{var}: Stop did not end the script");
        if let Ok(Msg::SqlJob { res, .. }) = l.rx.recv_timeout(std::time::Duration::from_millis(200)) {
            return res.err().unwrap_or_else(|| panic!("{var}: a stopped script reported success"));
        }
    }
}

#[test]
fn live_stop_in_a_transaction_the_file_opened_says_it_was_rolled_back() {
    every_pg(|l, var| {
        let t = tag(var);
        let mut dbs = Dbs::new(l);
        let (db, pool) = dbs.make(&format!("qdump_stop_tx_{t}"));
        let e = stopped_run(l, &db, "CREATE TABLE tx_before (a int); BEGIN; CREATE TABLE tx_cancel (a int); SELECT pg_sleep(60); COMMIT;", var);
        assert!(e.contains("stopped") && e.contains("transaction the file opened is rolled back"), "{var}: {e}");
        assert!(!e.contains("Statements that already ran stay applied"), "{var}: {e}");
        assert_eq!(one(l, &pool, "SELECT to_regclass('tx_cancel') IS NULL AND to_regclass('tx_before') IS NOT NULL"), "t", "{var}");
    });
}

#[test]
fn live_stop_cancels_the_running_statement_and_the_rest_does_not_run() {
    every_pg(|l, var| {
        let t = tag(var);
        let mut dbs = Dbs::new(l);
        let (db, pool) = dbs.make(&format!("qdump_stop_{t}"));
        let (prog, id, cancel) = (Arc::new(Progress::default()), Arc::new(AtomicU64::new(0)), Arc::new(AtomicBool::new(false)));
        let src = Source::Mem(Arc::new(b"CREATE TABLE before_stop (a int); SELECT pg_sleep(60); CREATE TABLE after_stop (a int);".to_vec()));
        l.w.sql_run_file(7, db.clone(), "public".into(), src, false, prog.clone(), id.clone(), cancel.clone());
        let t0 = std::time::Instant::now();
        while prog.rows.load(Ordering::Relaxed) < 1 || id.load(Ordering::SeqCst) == 0 {
            assert!(t0.elapsed().as_secs() < 20, "{var}: the script did not start");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        l.w.sql_kill(db.clone(), id.load(Ordering::SeqCst), cancel.clone());
        let mut stopped = None;
        let t0 = std::time::Instant::now();
        while stopped.is_none() {
            assert!(t0.elapsed().as_secs() < 20, "{var}: Stop did not end the script");
            if let Ok(Msg::SqlJob { res, .. }) = l.rx.recv_timeout(std::time::Duration::from_millis(200)) {
                stopped = Some(res);
            }
        }
        let e = stopped.unwrap().err().unwrap_or_else(|| panic!("{var}: a stopped script reported success"));
        assert!(e.contains("stopped") && e.contains("stay applied"), "{var}: {e}");
        assert_eq!(one(l, &pool, "SELECT to_regclass('before_stop') IS NOT NULL"), "t", "{var}");
        assert_eq!(one(l, &pool, "SELECT to_regclass('after_stop') IS NULL"), "t", "{var}");
    });
}

/// A dump written while another session keeps inserting into two tables in one transaction each must hold the same ids in both:
/// it reads one snapshot.
#[test]
fn live_a_dump_taken_while_another_session_writes_is_one_snapshot() {
    every_pg(|l, var| {
        let t = tag(var);
        let mut dbs = Dbs::new(l);
        let (src, src_pool) = dbs.make(&format!("qdump_snap_{t}"));
        sql(l, &src_pool, "CREATE TABLE a (id bigint PRIMARY KEY, pad text); CREATE TABLE b (id bigint PRIMARY KEY, pad text); INSERT INTO a SELECT g, repeat('x', 200) FROM generate_series(1, 150000) g; INSERT INTO b SELECT g, repeat('y', 200) FROM generate_series(1, 150000) g");
        let stop = Arc::new(AtomicBool::new(false));
        let writer_pool = src_pool.clone();
        let flag = stop.clone();
        let writer = l.w.rt.spawn(async move {
            let mut n = 150_000i64;
            while !flag.load(Ordering::SeqCst) {
                n += 1;
                let _ = sqlx::raw_sql(&format!("BEGIN; INSERT INTO a VALUES ({n}, 'w'); INSERT INTO b VALUES ({n}, 'w'); COMMIT")).execute(&writer_pool).await;
            }
            n
        });
        let path = file(&format!("snap_{t}"));
        let p = plan("public", false, vec![table("a", false, true, true, false), table("b", false, true, true, false)]);
        dump(l, &src, &p, &path).unwrap_or_else(|x| panic!("{var}: {x}"));
        stop.store(true, Ordering::SeqCst);
        let last = l.w.rt.block_on(writer).unwrap();
        assert!(last > 150_000, "{var}: the writer never ran");
        let text = std::fs::read_to_string(&path).unwrap();
        let ids = |table: &str| -> Vec<i64> {
            let marker = format!("INSERT INTO \"public\".\"{table}\"");
            // The rows of one INSERT end at its ";", and every value is a quoted literal.
            text.split(&marker).skip(1).flat_map(|chunk| chunk.split(";\n").next().unwrap_or_default().split("\n(").skip(1).filter_map(|r| r.split(',').next().and_then(|n| n.trim().trim_matches('\'').parse::<i64>().ok())).collect::<Vec<_>>()).collect()
        };
        let (ia, ib) = (ids("a"), ids("b"));
        // The chunk of one INSERT ends at its ";", so rows of the next statement are not mixed in: both lists are in id order.
        assert!(ia.len() >= 150_000 && ia.len() == ib.len(), "{var}: {} rows of a against {} of b", ia.len(), ib.len());
        assert_eq!(ia, ib, "{var}: the two tables were not read at one point in time");
        let _ = std::fs::remove_file(path);
    });
}

/// A login that may read only some tables: the dump says which one it may not, writes what it may, and the run of a file as
/// that login is refused before any statement since writes stay locked.
#[test]
fn live_privileges_are_checked_before_anything_is_dumped() {
    every_pg(|l, var| {
        let t = tag(var);
        let mut dbs = Dbs::new(l);
        let (_, pool) = dbs.make(&format!("qdump_priv_{t}"));
        let role = format!("qdump_lim_{t}");
        sql(l, &pool, "CREATE TABLE allowed (id int PRIMARY KEY, v text); CREATE TABLE secret (id int); INSERT INTO allowed VALUES (1, 'a'); INSERT INTO secret VALUES (1); CREATE SEQUENCE hidden_seq; CREATE TABLE uses_hidden (id int DEFAULT nextval('hidden_seq'))");
        let _ = l.w.rt.block_on(async { sqlx::raw_sql(&format!("DROP ROLE IF EXISTS {role}")).execute(&l.pool).await });
        l.w.rt.block_on(async { sqlx::raw_sql(&format!("CREATE ROLE {role} LOGIN PASSWORD 'pw'")).execute(&l.pool).await }).unwrap();
        sql(l, &pool, &format!("GRANT USAGE ON SCHEMA public TO {role}; GRANT SELECT ON allowed, uses_hidden TO {role}"));
        let uri = l.uri.replacen("postgres:quarry@", &format!("{role}:pw@"), 1).rsplit_once('/').map(|(a, _)| format!("{a}/qdump_priv_{t}")).unwrap();
        let lim = live_pg_at(uri);
        let db = lim.db.at(&format!("qdump_priv_{t}"));
        let path = file(&format!("priv_{t}"));
        let both = plan("public", false, vec![table("allowed", false, true, true, false), table("secret", false, true, true, false)]);
        let e = dump(&lim, &db, &both, &path).unwrap_err();
        assert!(e.contains("no SELECT privilege on public.secret"), "{var}: {e}");
        assert!(!path.exists(), "{var}");
        let ok = plan("public", false, vec![table("allowed", false, true, true, false), table("secret", false, true, false, false), table("uses_hidden", false, true, false, false)]);
        assert_eq!(dump(&lim, &db, &ok, &path).unwrap_or_else(|x| panic!("{var}: {x}")), 1, "{var}");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("CREATE TABLE \"public\".\"secret\"") && !text.contains("INSERT INTO \"public\".\"secret\""), "{var}");
        assert!(text.contains("The position of sequence public.hidden_seq was left out"), "{var}: {text}");
        let _ = std::fs::remove_file(path);
        drop(lim);
        sql(l, &pool, &format!("DROP OWNED BY {role}"));
        let _ = l.w.rt.block_on(async { sqlx::raw_sql(&format!("DROP ROLE IF EXISTS {role}")).execute(&l.pool).await });
    });
}

/// Through a real PgBouncer in transaction mode: nothing is set on the session, so the whole dump runs in one transaction that
/// carries its settings, and the file's own SETs become the settings of each statement of its run.
#[test]
fn live_a_dump_and_its_run_work_through_a_transaction_pooler() {
    let Some(l) = live_pgbouncer() else { return };
    let db = l.quarry_test();
    let pool = l.fixture_pool();
    let schema = "pgdump_bouncer";
    l.reset_schemas(&pool, &[schema], &format!("CREATE TYPE {schema}.mood AS ENUM ('a', 'b\\c'); CREATE TABLE {schema}.t (id serial PRIMARY KEY, m {schema}.mood, s text, b bytea); INSERT INTO {schema}.t (m, s, b) SELECT 'a', 'it''s \\ ' || g, '\\x00ff'::bytea FROM generate_series(1, 3000) g; CREATE VIEW {schema}.v AS SELECT id FROM {schema}.t; CREATE FUNCTION {schema}.f() RETURNS int LANGUAGE sql AS $$ SELECT 1; $$"));
    let before = one(&l, &pool, &format!("SELECT md5(string_agg(x::text, E'\\n' ORDER BY x::text)) FROM {schema}.t x"));
    let path = file("bouncer");
    let p = plan(schema, false, vec![table("t", false, true, true, false), table("v", true, true, false, false)]);
    assert_eq!(dump(&l, &db, &p, &path).unwrap_or_else(|x| panic!("{x}")), 3000);
    sql(&l, &pool, &format!("DROP SCHEMA {schema} CASCADE"));
    // The header's SETs are carried by every statement's transaction; a COPY, a plain BEGIN and a session SET of the user's are not.
    run(&l, &db, "", &path).unwrap_or_else(|x| panic!("{x}"));
    assert_eq!(one(&l, &pool, &format!("SELECT md5(string_agg(x::text, E'\\n' ORDER BY x::text)) FROM {schema}.t x")), before);
    assert_eq!(one(&l, &pool, &format!("SELECT {schema}.f()")), "1");
    let e = run_text(&l, &db, "", "SELECT 1; BEGIN; SELECT 2").unwrap_err();
    assert!(e.contains("Statement 2") && e.contains("pooler"), "{e}");
    let e = run_text(&l, &db, "", "CREATE INDEX CONCURRENTLY nope ON pgdump_bouncer.t (s)").unwrap_err();
    assert!(e.contains("cannot run") && e.contains("pooler"), "{e}");
    // A file's DO block, a pg_temp table and a search_path with a statement hidden in it never reach a shared backend.
    for s in ["DO $$BEGIN PERFORM pg_advisory_lock(424244); END$$", "CREATE TABLE pg_temp.pgdump_tmp (a int)"] {
        let got = run_text(&l, &db, "", s);
        sql(&l, &pool, "SELECT pg_terminate_backend(pid) FROM pg_locks WHERE locktype = 'advisory' AND objid = 424244");
        let e = got.unwrap_err();
        assert!(e.contains("cannot run") && e.contains("pooler"), "{s}: {e}");
    }
    let e = run_text(&l, &db, "", "SET search_path = 'a', (; DROP TABLE t; ); SELECT 1").unwrap_err();
    assert!(e.contains("Statement 1") && e.contains("cannot run"), "{e}");
    sql(&l, &pool, &format!("DROP SCHEMA {schema} CASCADE"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn live_cancel_leaves_no_file_and_wide_rows_go_in_batches() {
    every_pg(|l, var| {
        let t = tag(var);
        let mut dbs = Dbs::new(l);
        let (src, src_pool) = dbs.make(&format!("qdump_cancel_{t}"));
        sql(l, &src_pool, "CREATE TABLE big AS SELECT g AS id, repeat('x', 100) AS pad FROM generate_series(1, 200000) g");
        let path = file(&format!("cancel_{t}"));
        let (count, cancel, id) = (AtomicU64::new(0), AtomicBool::new(true), AtomicU64::new(0));
        let r = l.w.rt.block_on(dump_to(&src, &plan("public", false, vec![table("big", false, true, true, false)]), path.clone(), &count, &cancel, &id));
        assert_eq!(r.err().as_deref(), Some("Export cancelled"), "{var}");
        assert!(!path.exists(), "{var}");
        // A very large row goes through in batches of at most 1 MiB of text, never one string of the whole table.
        sql(l, &src_pool, "CREATE TABLE wide (id int, big text); INSERT INTO wide SELECT g, repeat('z', 3000000) FROM generate_series(1, 4) g");
        let wp = file(&format!("wide_{t}"));
        dump(l, &src, &plan("public", false, vec![table("wide", false, true, true, false)]), &wp).unwrap_or_else(|x| panic!("{var}: {x}"));
        let text = std::fs::read_to_string(&wp).unwrap();
        assert_eq!(text.matches("INSERT INTO \"public\".\"wide\"").count(), 4, "{var}: one INSERT per large row");
        let _ = std::fs::remove_file(wp);
    });
}
