//! Live tests of the CSV import and export on PostgreSQL, run on every server named by QUARRY_TEST_POSTGRES and
//! QUARRY_TEST_POSTGRES13, and through the PgBouncer of QUARRY_TEST_PGBOUNCER. Each test makes its own schema in the
//! shared `quarry_test` database.

use super::csv::{auto_map, DateFmt, Enc, Mapping, Opts, Source};
use super::export::{export_to, Plan, Sink};
use super::import::{run, undo_plan, Done, Dup, ImportPlan, Progress};
use super::testing::{every_pg, live_pgbouncer, LivePg};
use super::write::{SqlUndo, CONFLICT};
use super::Db;
use crate::db::{Msg, Res, UndoAction, UndoKind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

struct F<'a> {
    l: &'a LivePg,
    db: Db,
    pool: sqlx::PgPool,
    schema: String,
}

fn fixture<'a>(l: &'a LivePg, schema: &str, ddl: &str) -> F<'a> {
    let db = l.quarry_test();
    let pool = l.fixture_pool();
    l.qsql(&pool, &format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE; CREATE SCHEMA \"{schema}\"; SET search_path = \"{schema}\"; {ddl}"));
    F { l, db, pool, schema: schema.into() }
}

impl Drop for F<'_> {
    fn drop(&mut self) {
        self.l.qsql(&self.pool, &format!("DROP SCHEMA IF EXISTS \"{}\" CASCADE", self.schema));
    }
}

fn opts() -> Opts {
    Opts { delim: ',', quote: Some('"'), enc: Enc::Utf8 }
}

impl F<'_> {
    fn sql(&self, s: &str) {
        self.l.qsql(&self.pool, &format!("SET search_path = \"{}\"; {s}", self.schema));
    }

    fn one(&self, s: &str) -> String {
        self.l.qone(&self.pool, &format!("SET search_path = \"{}\"; {s}", self.schema))
    }

    fn count(&self, table: &str) -> i64 {
        self.one(&format!("SELECT count(*) FROM \"{table}\"")).parse().unwrap()
    }

    fn plan(&self, table: &str, csv: &str, dup: Dup) -> ImportPlan {
        let info = self.l.w.rt.block_on(async { super::csv::TableInfo::from_structure(&super::structure::load(&self.db, &self.schema, table).await.unwrap()) });
        assert!(info.pg, "the structure of a PostgreSQL table says so");
        let names: Vec<String> = csv.lines().next().unwrap().split(',').map(|s| s.to_string()).collect();
        let map = Mapping { map: auto_map(&names, &info.cols), fmts: vec![DateFmt::DayFirst; names.len()], empty_null: true };
        ImportPlan { source: Source::Mem(Arc::new(csv.as_bytes().to_vec())), opts: opts(), header: true, info, map, dup, want_undo: true }
    }

    fn run(&self, table: &str, plan: &ImportPlan) -> (Res<Done>, Arc<Progress>) {
        let prog = Arc::new(Progress::default());
        let r = self.l.w.rt.block_on(run(&self.db, &self.schema, table, plan, 1, false, &prog, &AtomicBool::new(false), &AtomicU64::new(0)));
        (r, prog)
    }

    fn import(&self, table: &str, csv: &str, dup: Dup) -> Done {
        let plan = self.plan(table, csv, dup);
        self.run(table, &plan).0.unwrap_or_else(|x| panic!("import into {table}: {x}"))
    }

    fn undo(&self, a: UndoAction, pk: &[String]) -> (Res<String>, Option<UndoAction>) {
        self.l.w.sql_undo(self.db.clone(), a, pk.to_vec(), false);
        let Msg::Written { res, undo, .. } = self.l.next() else { panic!("expected Written") };
        (res, undo)
    }
}

fn kind(a: &UndoAction) -> &SqlUndo {
    let UndoKind::Sql { undo, .. } = &a.kind else { panic!("not a SQL undo") };
    undo
}

fn pk_of(plan: &ImportPlan) -> Vec<String> {
    plan.info.pk.clone()
}

#[test]
fn live_new_rows_get_their_serial_keys_and_undo_removes_exactly_them() {
    every_pg(|l, var| {
        let f = fixture(l, "pgcsv_new", "CREATE TABLE t (id serial PRIMARY KEY, name text NOT NULL, qty int); INSERT INTO t (name, qty) VALUES ('old1', 1), ('old2', 2)");
        let plan = f.plan("t", "name,qty\nann,5\nbob,\n\"c,\"\"d\",7\n", Dup::Stop);
        // The key is not in the file and counts up by itself: both the statement and the dialog know the new rows can be found.
        assert!(undo_plan(&plan.info, &plan.map, plan.dup, 3).is_ok(), "{var}");
        let done = f.run("t", &plan).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!((done.rows, done.inserted), (3, 3), "{var}");
        assert_eq!(f.one("SELECT string_agg(id || ':' || name || ':' || coalesce(qty::text, '~'), ',' ORDER BY id) FROM t"), "1:old1:1,2:old2:2,3:ann:5,4:bob:~,5:c,\"d:7", "{var}");
        let undo = done.undo.unwrap_or_else(|| panic!("{var}: no undo, {:?}", done.why_not));
        assert!(matches!(kind(&undo), SqlUndo::Import(_)), "{var}");
        let (res, left) = f.undo(undo, &pk_of(&plan));
        assert_eq!(res.unwrap_or_else(|x| panic!("{var}: {x}")), "Removed 3 imported rows", "{var}");
        assert!(left.is_none());
        assert_eq!(f.one("SELECT string_agg(id || ':' || name, ',' ORDER BY id) FROM t"), "1:old1,2:old2", "{var}");
    });
}

#[test]
fn live_duplicate_keys_stop_skip_or_update_and_each_undo_is_exact() {
    every_pg(|l, var| {
        let f = fixture(l, "pgcsv_dups", "CREATE TABLE d (code text PRIMARY KEY, label text, n int); INSERT INTO d VALUES ('a', 'old a', 1), ('b', 'old b', 2)");
        let state = || f.one("SELECT string_agg(code || ':' || coalesce(label, '~') || ':' || coalesce(n::text, '~'), ',' ORDER BY code) FROM d");
        let before = state();
        let file = "code,label,n\na,new a,10\nc,new c,30\nd,,40\n";
        let plan = f.plan("d", file, Dup::Stop);
        let e = f.run("d", &plan).0.err().unwrap_or_else(|| panic!("{var}: must stop"));
        assert!(e.contains("duplicate key") && e.contains("Rows 2 to 4") && e.contains("Nothing was imported"), "{var}: {e}");
        assert_eq!(state(), before, "{var}: nothing was kept");

        // Skip: the existing row stays, the new ones go in, and undo removes only those.
        let skipped = f.import("d", file, Dup::Skip);
        assert_eq!((skipped.rows, skipped.inserted), (3, 2), "{var}");
        assert_eq!(state(), "a:old a:1,b:old b:2,c:new c:30,d:~:40", "{var}");
        let (r, _) = f.undo(skipped.undo.unwrap_or_else(|| panic!("{var}: skip has an undo")), &pk_of(&plan));
        r.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(state(), before, "{var}: undo of skip");

        // Update: the old values of the overwritten row come back, and the new rows go.
        let updated = f.import("d", file, Dup::Update);
        assert_eq!((updated.rows, updated.inserted), (3, 2), "{var}");
        assert_eq!(state(), "a:new a:10,b:old b:2,c:new c:30,d:~:40", "{var}");
        let (r, _) = f.undo(updated.undo.unwrap_or_else(|| panic!("{var}: update has an undo")), &pk_of(&plan));
        assert_eq!(r.unwrap_or_else(|x| panic!("{var}: {x}")), "Removed 2 imported rows and restored 1 overwritten row", "{var}");
        assert_eq!(state(), before, "{var}: undo of update");

        // The same key on two lines: the last one wins, in one batch or two, and undo still reaches the state before.
        let twice = "code,label,n\nx,first,1\nx,second,2\na,again,3\na,last,4\n";
        let done = f.import("d", twice, Dup::Update);
        assert_eq!(state(), "a:last:4,b:old b:2,x:second:2", "{var}");
        assert_eq!(done.inserted, 1, "{var}");
        let (r, _) = f.undo(done.undo.unwrap(), &pk_of(&plan));
        r.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(state(), before, "{var}: undo with a key on two lines");
        // Skip with the same key twice keeps the first.
        let done = f.import("d", twice, Dup::Skip);
        assert_eq!(state(), "a:old a:1,b:old b:2,x:first:1", "{var}");
        f.undo(done.undo.unwrap(), &pk_of(&plan)).0.unwrap();
        assert_eq!(state(), before, "{var}");
    });
}

#[test]
fn live_every_type_goes_in_as_text_through_its_cast_and_comes_back_out_on_undo() {
    every_pg(|l, var| {
        let f = fixture(
            l,
            "pgcsv_types",
            "CREATE TYPE mood AS ENUM ('happy', 'sad'); CREATE TABLE t (id int PRIMARY KEY, i int, n numeric(10,2), b bool, d date, tz timestamptz, u uuid, j jsonb, jj json, by bytea, ia int[], ta text[], m mood, f float8, iv interval, tx text, bits bit(4))",
        );
        let csv = "id,i,n,b,d,tz,u,j,jj,by,ia,ta,m,f,iv,tx,bits\n\
            1,5,1.50,true,2026-11-09,2026-11-09 12:41:00+02,123e4567-e89b-12d3-a456-426614174000,\"{\"\"a\"\": 1}\",\"{\"\"b\"\":  [1, 2]}\",0xCAFE,\"{1,2,NULL}\",\"{\"\"a b\"\",c}\",happy,1.5,1 day,hello,0101\n\
            2,,,,,,,,,,,,,,,,\n\
            3,7,-0.5,f,09/11/2026,2026-11-09 00:00:00+00,{123E4567-E89B-12D3-A456-426614174000},[1],null,\\xdead,{},{},sad,NaN,2 hours,\"it's \\ \"\"x\"\"\",1111\n\
            4,\\N,\\N,yes,2026-01-31,infinity,,,,,,,,-Infinity,,\"\",\n";
        let plan = f.plan("t", csv, Dup::Stop);
        let done = f.run("t", &plan).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(done.inserted, 4, "{var}");
        let cell = |col: &str, id: i64| f.one(&format!("SELECT coalesce(({col})::text, '~') FROM t WHERE id = {id}"));
        assert_eq!(cell("i", 1), "5");
        assert_eq!(cell("n", 1), "1.50");
        assert_eq!(cell("b", 1), "true");
        assert_eq!(cell("d", 1), "2026-11-09");
        assert_eq!(cell("to_char(tz at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS')", 1), "2026-11-09 10:41:00");
        assert_eq!(cell("u", 1), "123e4567-e89b-12d3-a456-426614174000");
        assert_eq!(cell("j->>'a'", 1), "1");
        assert_eq!(cell("jj::text", 1), "{\"b\":  [1, 2]}", "json keeps its own text");
        assert_eq!(cell("encode(by, 'hex')", 1), "cafe");
        assert_eq!(cell("ia", 1), "{1,2,NULL}");
        assert_eq!(cell("ta", 1), "{\"a b\",c}");
        assert_eq!(cell("m", 1), "happy");
        assert_eq!(cell("f", 1), "1.5");
        assert_eq!(cell("iv", 1), "1 day");
        assert_eq!(cell("bits", 1), "0101");
        // A row of empty cells is a row of NULLs.
        assert_eq!(f.one("SELECT count(*) FROM t WHERE id = 2 AND i IS NULL AND n IS NULL AND b IS NULL AND d IS NULL AND tz IS NULL AND u IS NULL AND j IS NULL AND jj IS NULL AND by IS NULL AND ia IS NULL AND m IS NULL AND f IS NULL AND iv IS NULL AND bits IS NULL AND tx IS NULL"), "1");
        assert_eq!(cell("d", 3), "2026-11-09", "day first, as the dialog reads it");
        assert_eq!(cell("u", 3), "123e4567-e89b-12d3-a456-426614174000", "braces and capitals are read");
        assert_eq!(cell("jj::text", 3), "null", "the JSON value null is not an SQL NULL");
        assert_eq!(cell("encode(by, 'hex')", 3), "dead");
        assert_eq!(cell("ia", 3), "{}");
        assert_eq!(cell("f", 3), "NaN");
        assert_eq!(cell("tx", 3), "it's \\ \"x\"", "a backslash and quotes are text");
        assert_eq!(cell("b", 4), "true");
        assert_eq!(cell("tz", 4), "infinity");
        assert_eq!(cell("f", 4), "-Infinity");
        assert_eq!(cell("i", 4), "~", "\\N is NULL");
        assert_eq!(cell("tx", 4), "~", "an empty text cell is NULL by the dialog's choice");
        // Every kind of value is compared on undo too: json as text, bytea, arrays, NaN and infinity.
        let undo = done.undo.unwrap_or_else(|| panic!("{var}: no undo, {:?}", done.why_not));
        let (r, _) = f.undo(undo, &pk_of(&plan));
        r.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(f.count("t"), 0, "{var}");
    });
}

#[test]
fn live_a_bad_row_fails_the_whole_file_and_nothing_is_applied() {
    every_pg(|l, var| {
        let f = fixture(l, "pgcsv_bad", "CREATE TABLE s (id serial PRIMARY KEY, small smallint NOT NULL, u text UNIQUE)");
        // The server refuses the last value, after two batches were sent.
        let mut csv = String::from("small,u\n");
        for i in 0..1500 {
            csv.push_str(&format!("{},u{i}\n", i % 100));
        }
        csv.push_str("99999,x\n");
        let plan = f.plan("s", &csv, Dup::Stop);
        let (r, prog) = f.run("s", &plan);
        let e = r.err().unwrap_or_else(|| panic!("{var}: must fail"));
        assert!(e.contains("out of range") && e.contains("Nothing was imported") && e.contains("Rows 1502 to 1502"), "{var}: {e}");
        assert_eq!(f.count("s"), 0, "{var}");
        assert!(prog.rows.load(Ordering::Relaxed) >= 1000, "{var}: batches had been sent before the failure");
        // A unique value twice inside the file.
        let e = f.run("s", &f.plan("s", "small,u\n1,a\n2,b\n3,a\n", Dup::Stop)).0.err().unwrap();
        assert!(e.contains("duplicate key") && e.contains("(u)=(a)"), "{var}: {e}");
        assert_eq!(f.count("s"), 0, "{var}");
        // The client checks a value the type refuses before the first row is sent, and names the row.
        let e = f.run("s", &f.plan("s", "small,u\n1,a\nabc,b\n", Dup::Stop)).0.err().unwrap();
        assert!(e.contains("Row 3") && e.contains("Nothing was imported"), "{var}: {e}");
        assert_eq!(f.count("s"), 0, "{var}");
        // A short row, and a mapped NOT NULL column left empty.
        assert!(f.run("s", &f.plan("s", "small,u\n1,a\n2\n", Dup::Stop)).0.err().unwrap().contains("Row 3 has 1 values"), "{var}");
        assert!(f.run("s", &f.plan("s", "small,u\n,a\n", Dup::Stop)).0.err().unwrap().contains("does not allow NULL"), "{var}");
        // The sequence moved, but no row is left and a later import still works.
        let done = f.import("s", "small,u\n1,a\n", Dup::Stop);
        assert_eq!(done.inserted, 1, "{var}");
    });
}

/// Starts `run` with a watcher that stops it once `after` rows are sent, and returns the answer.
fn stopped(f: &F, table: &str, plan: &ImportPlan, after: u64) -> Res<Done> {
    let prog = Arc::new(Progress::default());
    let cancel = Arc::new(AtomicBool::new(false));
    let watcher = {
        let (prog, cancel) = (prog.clone(), cancel.clone());
        std::thread::spawn(move || {
            let t = std::time::Instant::now();
            while prog.rows.load(Ordering::Relaxed) < after && t.elapsed().as_secs() < 60 {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            cancel.store(true, Ordering::SeqCst);
        })
    };
    let r = f.l.w.rt.block_on(run(&f.db, &f.schema, table, plan, 1, false, &prog, &cancel, &AtomicU64::new(0)));
    watcher.join().unwrap();
    assert!(prog.rows.load(Ordering::Relaxed) < 400_000, "it stopped early");
    r
}

#[test]
fn live_stop_in_the_middle_rolls_everything_back() {
    every_pg(|l, var| {
        let f = fixture(l, "pgcsv_stop", "CREATE TABLE slow (id serial PRIMARY KEY, a text, b text); INSERT INTO slow (a, b) VALUES ('keep', 'me')");
        let mut csv = String::from("a,b\n");
        for i in 0..400_000 {
            csv.push_str(&format!("alpha {i},beta {i}\n"));
        }
        let plan = f.plan("slow", &csv, Dup::Stop);
        assert_eq!(stopped(&f, "slow", &plan, 5_000).err().as_deref(), Some("Import cancelled"), "{var}");
        assert_eq!(f.count("slow"), 1, "{var}: a stopped import leaves no rows");
        assert_eq!(f.one("SELECT count(*) FROM pg_stat_activity WHERE datname = 'quarry_test' AND state = 'idle in transaction' AND query LIKE '%pgcsv_stop%'"), "0", "{var}: no transaction is left open");
        // The same for an update that is stopped: the old values are still there.
        let plan = f.plan("slow", &csv.replacen("alpha 0,beta 0", "keep,changed", 1), Dup::Update);
        assert_eq!(stopped(&f, "slow", &plan, 5_000).err().as_deref(), Some("Import cancelled"), "{var}");
        assert_eq!(f.one("SELECT a || b FROM slow WHERE id = 1"), "keepme", "{var}");
    });
}

#[test]
fn live_undo_is_refused_after_an_outside_change_and_runs_once_it_is_put_back() {
    every_pg(|l, var| {
        let f = fixture(l, "pgcsv_outside", "CREATE TABLE o (id int PRIMARY KEY, v text, w int); INSERT INTO o VALUES (1, 'old', 1)");
        let state = || f.one("SELECT string_agg(id || ':' || v || ':' || coalesce(w::text, '~'), ',' ORDER BY id) FROM o");
        let csv = "id,v,w\n1,new,10\n2,two,20\n3,three,30\n";
        let plan = f.plan("o", csv, Dup::Update);
        let done = f.run("o", &plan).0.unwrap();
        let after = state();
        assert_eq!(after, "1:new:10,2:two:20,3:three:30");
        let mut undo = done.undo.unwrap();
        // An inserted row was edited since.
        f.sql("UPDATE o SET v = 'edited' WHERE id = 2");
        let (res, back) = f.undo(undo, &pk_of(&plan));
        assert_eq!(res.err().as_deref(), Some(CONFLICT), "{var}");
        assert_eq!(f.one("SELECT string_agg(id || ':' || v || ':' || coalesce(w::text, '~'), ',' ORDER BY id) FROM o"), "1:new:10,2:edited:20,3:three:30", "{var}: nothing was changed, not even the rows that still matched");
        undo = back.unwrap_or_else(|| panic!("{var}: the entry stays in the list"));
        // An overwritten row was edited since.
        f.sql("UPDATE o SET v = 'two' WHERE id = 2; UPDATE o SET w = 99 WHERE id = 1");
        let (res, back) = f.undo(undo, &pk_of(&plan));
        assert_eq!(res.err().as_deref(), Some(CONFLICT), "{var}");
        undo = back.unwrap();
        // An inserted row is gone, and one is replaced under the same key by a row with other values.
        f.sql("UPDATE o SET w = 10 WHERE id = 1; DELETE FROM o WHERE id = 3");
        let (res, back) = f.undo(undo, &pk_of(&plan));
        assert!(res.is_err(), "{var}");
        undo = back.unwrap();
        f.sql("INSERT INTO o VALUES (3, 'three', 30)");
        assert_eq!(state(), after);
        // The primary key changed: the rows could not be told apart.
        f.sql("ALTER TABLE o DROP CONSTRAINT o_pkey, ADD PRIMARY KEY (id, v)");
        let (res, back) = f.undo(undo, &pk_of(&plan));
        assert!(res.err().unwrap().contains("Undo refused: the primary key of o"), "{var}");
        undo = back.unwrap();
        f.sql("ALTER TABLE o DROP CONSTRAINT o_pkey, ADD PRIMARY KEY (id)");
        let (res, _) = f.undo(undo, &pk_of(&plan));
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(state(), "1:old:1", "{var}: exactly what was there before");
    });
}

#[test]
fn live_a_big_file_goes_in_batches_and_undo_takes_it_out() {
    every_pg(|l, var| {
        let f = fixture(l, "pgcsv_big", "CREATE TABLE big (id serial PRIMARY KEY, name text NOT NULL, qty int, price numeric(10,2), note text, seen timestamptz); INSERT INTO big (name) VALUES ('old1'), ('old2'), ('old3')");
        let mut csv = String::from("name,qty,price,note,seen\n");
        for i in 0..50_000usize {
            let note = if i % 997 == 0 { format!("\"it's \"\"quoted\"\", ok\\; DROP TABLE big; -- {i}\nsecond line \\' \\N $1 {{}}\"") } else { format!("note {i}") };
            let seen = if i % 2 == 0 { format!("2026-03-{:02} 10:{:02}:00+00", 1 + i % 28, i % 60) } else { String::new() };
            csv.push_str(&format!("row{i},{},{}.{:02},{note},{seen}\n", i % 1000, i % 500, i % 100));
        }
        let plan = f.plan("big", &csv, Dup::Stop);
        let (r, prog) = f.run("big", &plan);
        let done = r.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!((done.rows, done.inserted), (50_000, 50_000), "{var}");
        assert_eq!(prog.rows.load(Ordering::Relaxed), 50_000, "{var}");
        assert_eq!(f.count("big"), 50_003, "{var}");
        assert_eq!(f.one("SELECT note FROM big WHERE name = 'row997'"), "it's \"quoted\", ok\\; DROP TABLE big; -- 997\nsecond line \\' \\N $1 {}", "{var}");
        assert_eq!(f.one("SELECT price FROM big WHERE name = 'row123'"), "123.23", "{var}");
        assert_eq!(f.one("SELECT count(*) FROM big WHERE seen IS NULL AND name LIKE 'row%'"), "25000", "{var}");
        assert_eq!(f.one("SELECT to_regclass('big')::text"), "big", "{var}: the table is still there");
        let started = std::time::Instant::now();
        let (r, _) = f.undo(done.undo.unwrap_or_else(|| panic!("{var}: no undo, {:?}", done.why_not)), &pk_of(&plan));
        r.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert!(started.elapsed().as_secs() < 60, "{var}: undo of 50 000 rows took {:?}", started.elapsed());
        assert_eq!(f.one("SELECT string_agg(name, ',' ORDER BY id) FROM big"), "old1,old2,old3", "{var}");
        // Wider than a batch of 500 rows can bind: the batch shrinks to fit 65 535 values.
        let cols: Vec<String> = (0..140).map(|i| format!("c{i} int")).collect();
        f.sql(&format!("CREATE TABLE wide (id serial PRIMARY KEY, {})", cols.join(", ")));
        let head: Vec<String> = (0..140).map(|i| format!("c{i}")).collect();
        let mut csv = format!("{}\n", head.join(","));
        for r in 0..1200 {
            csv.push_str(&(0..140).map(|c| ((r + c) % 100).to_string()).collect::<Vec<_>>().join(","));
            csv.push('\n');
        }
        let plan = f.plan("wide", &csv, Dup::Stop);
        assert_eq!(super::import_pg::batch_rows(140), 468);
        let done = f.run("wide", &plan).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(done.inserted, 1200, "{var}");
        assert_eq!(f.one("SELECT sum(c139) FROM wide"), (0..1200).map(|r| (r + 139) % 100).sum::<i32>().to_string(), "{var}");
        f.undo(done.undo.unwrap(), &pk_of(&plan)).0.unwrap();
        assert_eq!(f.count("wide"), 0, "{var}");
    });
}

#[test]
fn live_names_and_types_outside_public_and_in_other_cases_resolve_like_the_read() {
    every_pg(|l, var| {
        // A schema with capitals and a space, an enum and a domain of its own that public does not have, columns whose names differ
        // only in case, and a same-named table in public that must stay untouched.
        l.quarry_test();
        l.qsql(&l.fixture_pool(), "CREATE TABLE IF NOT EXISTS public.\"Mixed\" (id int PRIMARY KEY, \"Name\" text)");
        let f = fixture(
            l,
            "PgCsv Case",
            "CREATE TYPE feeling AS ENUM ('glad', 'sad'); CREATE DOMAIN pos AS int CHECK (VALUE > 0); CREATE TABLE \"Mixed\" (id int PRIMARY KEY, \"Name\" text, name text, f feeling, p pos, \"ARR\" feeling[])",
        );
        let csv = "id,Name,name,f,p,ARR\n1,upper,lower,glad,5,\"{glad,sad}\"\n";
        let plan = f.plan("Mixed", csv, Dup::Stop);
        // Case matters: the file's `Name` goes to "Name", and `name` to name. No column is found by another case.
        assert_eq!(plan.map.map, vec![Some(0), Some(1), Some(2), Some(3), Some(4), Some(5)], "{var}");
        let done = f.run("Mixed", &plan).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(f.one("SELECT \"Name\" || '/' || name || '/' || f || '/' || p || '/' || \"ARR\"::text FROM \"Mixed\""), "upper/lower/glad/5/{glad,sad}", "{var}");
        assert_eq!(l.qone(&f.pool, "SELECT count(*) FROM public.\"Mixed\""), "0", "{var}: the table of another schema is untouched");
        // A value the domain refuses is a server error naming the rows, with nothing applied.
        let e = f.run("Mixed", &f.plan("Mixed", "id,p\n2,-1\n", Dup::Stop)).0.err().unwrap();
        assert!(e.contains("Rows 2 to 2") && e.contains("Nothing was imported"), "{var}: {e}");
        f.undo(done.undo.unwrap(), &pk_of(&plan)).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(f.count("Mixed"), 0, "{var}");
        l.qsql(&f.pool, "DROP TABLE public.\"Mixed\"");
    });
}

#[test]
fn live_a_table_without_a_key_imports_and_skips_but_cannot_update_or_undo() {
    every_pg(|l, var| {
        let f = fixture(l, "pgcsv_nokey", "CREATE TABLE n (a text UNIQUE, b int); INSERT INTO n VALUES ('x', 1)");
        let plan = f.plan("n", "a,b\ny,2\n", Dup::Stop);
        let done = f.run("n", &plan).0.unwrap();
        assert!(done.undo.is_none() && done.why_not.as_deref().is_some_and(|w| w.contains("no primary key")), "{var}");
        assert!(undo_plan(&plan.info, &plan.map, Dup::Stop, 1).unwrap_err().contains("no primary key"));
        let skipped = f.import("n", "a,b\nx,5\nz,3\n", Dup::Skip);
        assert_eq!((skipped.rows, skipped.inserted), (2, 1), "{var}");
        assert_eq!(f.one("SELECT string_agg(a || b, ',' ORDER BY a) FROM n"), "x1,y2,z3", "{var}");
        let e = f.run("n", &f.plan("n", "a,b\nx,5\n", Dup::Update)).0.err().unwrap();
        assert!(e.starts_with("Update needs a primary key"), "{var}: {e}");
    });
}

#[test]
fn live_a_null_in_a_counting_key_takes_the_next_number_and_a_locked_connection_writes_nothing() {
    every_pg(|l, var| {
        let f = fixture(l, "pgcsv_default", "CREATE TABLE s (id serial PRIMARY KEY, v text); CREATE TABLE i (id int GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, v text); CREATE TABLE a (id int GENERATED ALWAYS AS IDENTITY PRIMARY KEY, g int GENERATED ALWAYS AS (1 + 1) STORED, v text)");
        for t in ["s", "i"] {
            let done = f.import(t, "id,v\n,one\n10,ten\n\\N,three\n", Dup::Stop);
            assert_eq!(f.one(&format!("SELECT string_agg(id || v, ',' ORDER BY v) FROM {t}")), "1one,10ten,2three", "{t}");
            f.undo(done.undo.unwrap(), &["id".to_string()]).0.unwrap();
            assert_eq!(f.count(t), 0);
        }
        // An identity that is always generated and a computed column are not offered, and a file that names them is not mapped onto them.
        let plan = f.plan("a", "id,g,v\n1,2,x\n", Dup::Stop);
        assert_eq!(plan.map.map, vec![None, None, Some(2)], "{var}");
        f.run("a", &plan).0.unwrap();
        assert_eq!(f.one("SELECT id || ':' || g || ':' || v FROM a"), "1:2:x", "{var}");
        // Locked: nothing runs.
        let prog = Progress::default();
        let r = l.w.rt.block_on(run(&f.db, &f.schema, "s", &f.plan("s", "v\nx\n", Dup::Stop), 1, true, &prog, &AtomicBool::new(false), &AtomicU64::new(0)));
        assert!(r.err().unwrap().starts_with("Writes are locked"), "{var}");
        assert_eq!(f.count("s"), 0);
    });
}

#[test]
fn live_export_streams_the_types_of_a_table_and_the_file_imports_back_the_same() {
    every_pg(|l, var| {
        let f = fixture(
            l,
            "pgcsv_export",
            "CREATE TABLE src (id int PRIMARY KEY, n numeric(10,2), b bool, d date, tz timestamptz, u uuid, j jsonb, jj json, by bytea, ia int[], ta text[], f float8, iv interval, tx text); CREATE TABLE copy (LIKE src INCLUDING ALL); \
             INSERT INTO src VALUES (1, 1.50, true, '2026-11-09', '2026-11-09 12:41:00+02', '123e4567-e89b-12d3-a456-426614174000', '{\"a\": [1, 2]}', '{\"b\":  1}', '\\xcafe00', '{1,NULL,3}', '{\"a b\",\"c,d\"}', 0.1, '1 day 02:03:04', E'line\\none, \"q\" \\\\ it''s'), \
             (2, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL), (3, -0.5, false, '2026-01-31', 'infinity', NULL, 'null', 'null', '\\x', '{}', '{}', 'NaN', '-1 mons', '')",
        );
        let path = std::env::temp_dir().join(format!("pgcsv_export_{}_{}.csv", std::process::id(), var));
        let plan = Plan { fmt: crate::db::copy::Fmt::Csv, header: true, table: "src".into(), bs: false };
        let out = l.w.rt.block_on(export_to(&f.db, &f.schema, "SELECT * FROM src ORDER BY id", &plan, Sink::File(path.clone()), &AtomicU64::new(0), &AtomicBool::new(false), &AtomicU64::new(0))).unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(out.rows, 3, "{var}");
        let text = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(text.starts_with("id,n,b,d,tz,u,j,jj,by,ia,ta,f,iv,tx\n"), "{var}: {text}");
        // The text of each type as the server prints it, bytea as 0x hex, NULL as an empty cell.
        assert!(text.contains("1,1.50,true,2026-11-09,2026-11-09 10:41:00+00,123e4567-e89b-12d3-a456-426614174000,"), "{var}: {text}");
        assert!(text.contains(",0xCAFE00,\"{1,NULL,3}\",\"{\"\"a b\"\",\"\"c,d\"\"}\",0.1,1 day 02:03:04,"), "{var}: {text}");
        assert!(text.contains("\n2,,,,,,,,,,,,,\n"), "{var}: {text}");
        assert!(text.contains(",infinity,"), "{var}: {text}");
        // The file goes back in through the import, and every column reads the same.
        let plan = f.plan("copy", &text, Dup::Stop);
        assert_eq!(plan.map.map.iter().filter(|m| m.is_some()).count(), 14, "{var}");
        let done = f.run("copy", &plan).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(done.inserted, 3, "{var}");
        // json has no equality, so the two sides are compared through their text.
        let same = f.one("SELECT count(*) FROM src s JOIN copy c USING (id) WHERE s.n IS NOT DISTINCT FROM c.n AND s.b IS NOT DISTINCT FROM c.b AND s.d IS NOT DISTINCT FROM c.d AND s.tz IS NOT DISTINCT FROM c.tz AND s.u IS NOT DISTINCT FROM c.u AND s.j IS NOT DISTINCT FROM c.j AND s.jj::text IS NOT DISTINCT FROM c.jj::text AND s.by IS NOT DISTINCT FROM c.by AND s.ia IS NOT DISTINCT FROM c.ia AND s.ta IS NOT DISTINCT FROM c.ta AND s.f IS NOT DISTINCT FROM c.f AND s.iv IS NOT DISTINCT FROM c.iv AND s.tx IS NOT DISTINCT FROM c.tx");
        // Row 3's empty text cell is NULL after the import (the choice the dialog makes), so only rows 1 and 2 compare equal there.
        assert_eq!(same, "2", "{var}");
        assert_eq!(f.one("SELECT count(*) FROM copy WHERE id = 3 AND tx IS NULL AND f = 'NaN' AND tz = 'infinity' AND j = 'null'::jsonb AND by = ''::bytea"), "1", "{var}");
    });
}

#[test]
fn live_the_import_and_its_undo_work_behind_a_transaction_pooler() {
    let Some(l) = live_pgbouncer() else { return };
    let f = fixture(&l, "pgcsv_bouncer", "CREATE TABLE p (id int PRIMARY KEY, v text, w int); INSERT INTO p VALUES (1, 'old', 1)");
    let state = || f.one("SELECT string_agg(id || ':' || v || ':' || coalesce(w::text, '~'), ',' ORDER BY id) FROM p");
    // More than one batch, so statements of one transaction meet other clients' statements on the pooled backends.
    let mut csv = String::from("id,v,w\n1,new,10\n");
    for i in 2..1800 {
        csv.push_str(&format!("{i},v{i},{i}\n"));
    }
    let plan = f.plan("p", &csv, Dup::Update);
    let done = f.run("p", &plan).0.unwrap_or_else(|x| panic!("bouncer: {x}"));
    assert_eq!((done.rows, done.inserted), (1799, 1798));
    assert_eq!(f.count("p"), 1799);
    assert_eq!(f.one("SELECT v FROM p WHERE id = 1"), "new");
    f.undo(done.undo.unwrap(), &pk_of(&plan)).0.unwrap_or_else(|x| panic!("bouncer undo: {x}"));
    assert_eq!(state(), "1:old:1");
    let done = f.import("p", "id,v,w\n1,again,5\n7,seven,7\n", Dup::Skip);
    assert_eq!(done.inserted, 1);
    f.undo(done.undo.unwrap(), &pk_of(&plan)).0.unwrap();
    assert_eq!(state(), "1:old:1");
    // A failure rolls back through the pooler too, and Stop acts between batches.
    let mut big = String::from("id,v,w\n");
    for i in 100..300_000 {
        big.push_str(&format!("{i},x{i},1\n"));
    }
    let plan = f.plan("p", &big, Dup::Stop);
    assert_eq!(stopped(&f, "p", &plan, 3_000).err().as_deref(), Some("Import cancelled"));
    assert_eq!(state(), "1:old:1", "nothing stayed");
    let e = f.run("p", &f.plan("p", "id,v,w\n5,a,1\n1,dup,1\n", Dup::Stop)).0.err().unwrap();
    assert!(e.contains("duplicate key") && e.contains("Nothing was imported"), "{e}");
    assert_eq!(state(), "1:old:1");
}
