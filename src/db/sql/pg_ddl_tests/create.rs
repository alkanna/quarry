//! Live tests of creating and dropping PostgreSQL databases, schemas and tables, on every server named by QUARRY_TEST_POSTGRES and
//! QUARRY_TEST_POSTGRES13 and once through the PgBouncer. Every database and schema a test makes has a name of its own (process id,
//! counter, tag) and is dropped by `Tidy` when the test ends, also when it fails, so a test never drops what another one uses.

use super::*;
use crate::db::UndoKind;
use crate::db::sql::create::{Ddl as What, DropInfo};
use crate::db::sql::create_pg::{self, TableSpec};
use crate::db::sql::ddl::{ColDef, Dv};
use crate::db::sql::testing::{db_turn, live_pg_at};
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

fn uniq(tag: &str) -> String {
    static N: AtomicUsize = AtomicUsize::new(0);
    format!("qcr_{}_{}_{tag}", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

/// A SQL string literal, for fixtures that name things with quotes and spaces in them.
fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Drops what a test made, however it ended.
struct Tidy<'a> {
    l: &'a LivePg,
    dbs: std::cell::RefCell<Vec<String>>,
    schemas: std::cell::RefCell<Vec<String>>,
}

impl<'a> Tidy<'a> {
    fn new(l: &'a LivePg) -> Tidy<'a> {
        Tidy { l, dbs: Default::default(), schemas: Default::default() }
    }

    fn db(&self, tag: &str) -> String {
        let n = uniq(tag);
        self.dbs.borrow_mut().push(n.clone());
        n
    }

    fn schema(&self, name: &str) -> String {
        self.schemas.borrow_mut().push(name.to_string());
        name.to_string()
    }
}

impl Drop for Tidy<'_> {
    fn drop(&mut self) {
        for d in self.dbs.borrow().iter() {
            // A template database is refused by DROP until it is a plain one again, and a database whose undo was refused may have closed to connections.
            let _ = self.l.w.rt.block_on(async { sqlx::raw_sql(&format!("ALTER DATABASE {} IS_TEMPLATE false", ident(d))).execute(&self.l.pool).await });
            let _ = self.l.w.rt.block_on(async { sqlx::raw_sql(&format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", ident(d))).execute(&self.l.pool).await });
        }
        if !self.schemas.borrow().is_empty() {
            self.l.quarry_test();
            let pool = self.l.fixture_pool();
            for s in self.schemas.borrow().iter() {
                let _ = self.l.w.rt.block_on(async { sqlx::raw_sql(&format!("DROP SCHEMA IF EXISTS {} CASCADE", ident(s))).execute(&pool).await });
            }
        }
    }
}

struct Ans {
    res: Res<String>,
    undo: Option<UndoAction>,
    dbs: Option<Vec<String>>,
    database: String,
    what: What,
}

fn ans(l: &LivePg) -> Ans {
    let Msg::SqlDdl { res, undo, dbs, database, what, .. } = l.next() else { panic!("expected SqlDdl") };
    Ans { res, undo, dbs, database, what }
}

fn make_db(l: &LivePg, name: &str, template: Option<&str>) -> Ans {
    // Two CREATE DATABASE at once both want template1 to themselves, so the tests of one run take turns.
    let _turn = db_turn();
    l.w.pg_create_db(1, l.db.clone(), name.into(), template.map(String::from), false);
    ans(l)
}

fn drop_db(l: &LivePg, name: &str, force: bool) -> Ans {
    l.w.pg_drop_db(1, l.db.clone(), name.into(), force, false);
    ans(l)
}

fn exists(l: &LivePg, name: &str) -> bool {
    l.one(&format!("SELECT count(*)::text FROM pg_database WHERE datname = {}", lit(name))) == "1"
}

fn sessions(l: &LivePg, name: &str) -> i64 {
    l.one(&format!("SELECT count(*)::text FROM pg_stat_activity WHERE datname = {}", lit(name))).parse().unwrap()
}

/// A session of someone else on a database, outside Quarry's pools.
fn foreign(l: &LivePg, name: &str) -> sqlx::PgConnection {
    use sqlx::Connection;
    let o = sqlx::postgres::PgConnectOptions::from_str(&l.uri).unwrap().database(name);
    l.w.rt.block_on(async { sqlx::PgConnection::connect_with(&o).await.unwrap() })
}

/// The tables, views, sequences, functions and types of users whose description names one of `schemas`, as the server describes them.
/// What a drop took is the difference of two of these, read from the server and not rebuilt from dependency rules.
fn objects(l: &LivePg, pool: &sqlx::PgPool, schemas: &[&str]) -> std::collections::BTreeSet<String> {
    let sql = "SELECT COALESCE(string_agg(d, E'\\n' ORDER BY d), '') FROM (SELECT pg_describe_object('pg_class'::regclass, oid, 0) AS d FROM pg_class WHERE oid >= 16384 AND relkind IN ('r', 'p', 'v', 'm', 'f', 'S', 'c') \
               UNION ALL SELECT pg_describe_object('pg_proc'::regclass, oid, 0) FROM pg_proc WHERE oid >= 16384 \
               UNION ALL SELECT pg_describe_object('pg_type'::regclass, oid, 0) FROM pg_type WHERE oid >= 16384 AND typtype IN ('d', 'e')) x";
    l.qone(pool, sql).lines().filter(|d| schemas.iter().any(|s| d.contains(s))).map(String::from).collect()
}

/// The lines of a drop list that name a table, view, sequence, function or type, without the notes in brackets after them.
fn kinds(lines: &[String]) -> std::collections::BTreeSet<String> {
    lines
        .iter()
        .map(|x| match x.rfind(" (") {
            Some(i) if x.ends_with(')') => x[..i].to_string(),
            _ => x.clone(),
        })
        .filter(|x| ["table ", "view ", "materialized view ", "foreign table ", "sequence ", "function ", "type ", "composite type "].iter().any(|k| x.starts_with(k)))
        .collect()
}

/// Runs statements inside another database of the server, outside Quarry's pools.
fn in_db(l: &LivePg, name: &str, sql: &str) {
    use sqlx::Connection;
    let o = sqlx::postgres::PgConnectOptions::from_str(&l.uri).unwrap().database(name);
    l.w.rt.block_on(async {
        let mut c = sqlx::PgConnection::connect_with(&o).await.unwrap();
        sqlx::raw_sql(sql).execute(&mut c).await.unwrap_or_else(|x| panic!("{sql}: {x}"));
        c.close().await.unwrap();
    });
}

fn undo_of(l: &LivePg, a: UndoAction) -> Ans {
    let UndoKind::Sql { database, .. } = &a.kind else { panic!("not a SQL undo") };
    // Run on the pool of the database the create ran in, as the app does.
    l.w.sql_undo(l.db.at(database), a, Vec::new(), false);
    ans(l)
}

fn info(l: &LivePg, database: &str, schema: &str, table: &str) -> DropInfo {
    l.w.pg_drop_info(1, l.db.clone(), database.into(), schema.into(), table.into(), false);
    let Msg::SqlDropInfo { res, .. } = l.next() else { panic!("expected SqlDropInfo") };
    res.unwrap_or_else(|x| panic!("drop info: {x}"))
}

fn err(a: &Ans) -> String {
    a.res.clone().err().unwrap_or_else(|| panic!("expected a refusal, got {:?}", a.res))
}

#[test]
fn a_database_is_made_and_dropped_with_and_without_force() {
    every_pg(|l, var| {
        let tidy = Tidy::new(l);
        let name = tidy.db("plain");
        let a = make_db(l, &name, None);
        a.res.clone().unwrap_or_else(|x| panic!("{var}: create: {x}"));
        assert_eq!(a.what, What::CreatedDb);
        assert!(a.undo.is_some() && a.dbs.as_ref().is_some_and(|d| d.contains(&name)), "{var}: the answer carries the undo and the new list");
        assert_eq!(l.one(&format!("SELECT (pg_get_userbyid(datdba) = current_user)::text FROM pg_database WHERE datname = {}", lit(&name))), "true", "{var}: the owner is the connected role");
        // Quarry reads from it, so its own pool holds a session on it: the drop has to close that first.
        l.w.sql_schemas(1, l.db.clone(), name.clone());
        let Msg::SqlSchemas { res, .. } = l.next() else { panic!("expected SqlSchemas") };
        res.unwrap();
        assert!(sessions(l, &name) > 0, "{var}: the read left a pooled session to close");
        let d = drop_db(l, &name, false);
        d.res.clone().unwrap_or_else(|x| panic!("{var}: Quarry's own session must not stop the drop: {x}"));
        assert_eq!(d.what, What::DroppedDb);
        assert!(!exists(l, &name) && d.dbs.is_some_and(|d| !d.contains(&name)), "{var}");
        // A second Quarry read makes a fresh pool for the name, which does not exist any more.
        l.w.sql_schemas(1, l.db.clone(), name.clone());
        let Msg::SqlSchemas { res, .. } = l.next() else { panic!("expected SqlSchemas") };
        assert!(res.is_err(), "{var}: the closed pool is not reused for a database that is gone");

        // Someone else is connected: refused, and the message says what to do; with FORCE the session goes.
        let name = tidy.db("busy");
        make_db(l, &name, None).res.unwrap();
        let mut other = foreign(l, &name);
        let d = drop_db(l, &name, false);
        let why = err(&d);
        assert!(why.contains("55006") && why.contains("FORCE"), "{var}: {why}");
        assert!(exists(l, &name), "{var}: a refused drop drops nothing");
        let d = drop_db(l, &name, true);
        d.res.clone().unwrap_or_else(|x| panic!("{var}: FORCE: {x}"));
        assert!(!exists(l, &name), "{var}");
        let gone = l.w.rt.block_on(async { sqlx::query("SELECT 1").execute(&mut other).await });
        assert!(gone.is_err(), "{var}: the other session was ended");
    });
}

#[test]
fn a_database_comes_from_a_listed_template_only() {
    every_pg(|l, var| {
        let tidy = Tidy::new(l);
        l.w.pg_templates(1, l.db.clone());
        let Msg::PgTemplates { res, .. } = l.next() else { panic!("expected PgTemplates") };
        let templates = res.unwrap();
        assert!(templates.contains(&"template0".to_string()) && templates.contains(&"template1".to_string()), "{var}: {templates:?}");
        let name = tidy.db("tpl");
        make_db(l, &name, Some("template0")).res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert!(exists(l, &name));
        // A database that is not a template is not offered, and one typed by hand is refused before a statement exists.
        let why = err(&make_db(l, &tidy.db("nope"), Some("quarry_test")));
        assert!(why.contains("not a template"), "{var}: {why}");
        assert!(create_pg::create_db_sql("x", Some("t\"; DROP DATABASE y; --"), &templates).is_err());
        assert_eq!(create_pg::create_db_sql("x", Some("template0"), &templates).unwrap(), "CREATE DATABASE \"x\" TEMPLATE \"template0\"");
    });
}

#[test]
fn the_undo_of_a_database_create_needs_it_empty_and_alone() {
    every_pg(|l, var| {
        let tidy = Tidy::new(l);
        let name = tidy.db("undo");
        let a = make_db(l, &name, None);
        let u = undo_of(l, a.undo.unwrap());
        u.res.clone().unwrap_or_else(|x| panic!("{var}: {x}"));
        assert!(!exists(l, &name) && u.undo.is_none(), "{var}: an empty new database goes, and its entry is used up");

        // A table made in it since: refused, still there, and the entry is handed back so it stays in the list.
        let name = tidy.db("undo2");
        let a = make_db(l, &name, None);
        let o = sqlx::postgres::PgConnectOptions::from_str(&l.uri).unwrap().database(&name);
        l.w.rt.block_on(async {
            use sqlx::Connection;
            let mut c = sqlx::PgConnection::connect_with(&o).await.unwrap();
            sqlx::raw_sql("CREATE TABLE kept (id int)").execute(&mut c).await.unwrap();
            c.close().await.unwrap();
        });
        let u = undo_of(l, a.undo.unwrap());
        let why = err(&u);
        assert!(why.starts_with("Not undone") && why.contains("has tables"), "{var}: {why}");
        assert!(exists(l, &name) && u.undo.is_some(), "{var}: refused truthfully, nothing dropped");
        // Another session connected: refused too, even though it is empty.
        l.w.rt.block_on(async {
            use sqlx::Connection;
            let mut c = sqlx::PgConnection::connect_with(&o).await.unwrap();
            sqlx::raw_sql("DROP TABLE kept").execute(&mut c).await.unwrap();
            c.close().await.unwrap();
        });
        let other = foreign(l, &name);
        let u = undo_of(l, u.undo.unwrap());
        let why = err(&u);
        assert!(why.contains("another session is connected") && exists(l, &name), "{var}: {why}");
        l.w.rt.block_on(async { use sqlx::Connection; other.close().await.unwrap() });
        // Alone and empty now.
        let u = undo_of(l, u.undo.unwrap());
        u.res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert!(!exists(l, &name), "{var}");
    });
}

#[test]
fn the_undo_of_a_database_create_sees_what_a_table_count_misses_and_the_database_it_made() {
    every_pg(|l, var| {
        let tidy = Tidy::new(l);
        // Things a user puts in a database that are no table, function or schema: each one makes the undo refuse, and once it is gone the undo works.
        let cases = [
            ("a large object", "SELECT lo_create(0)", "SELECT lo_unlink(oid) FROM pg_largeobject_metadata"),
            ("a collation", "CREATE COLLATION qcoll FROM \"C\"", "DROP COLLATION qcoll"),
            ("a publication", "CREATE PUBLICATION qpub", "DROP PUBLICATION qpub"),
            ("a text search configuration", "CREATE TEXT SEARCH CONFIGURATION qcfg (COPY = simple)", "DROP TEXT SEARCH CONFIGURATION qcfg"),
            ("a foreign data wrapper", "CREATE FOREIGN DATA WRAPPER qfdw", "DROP FOREIGN DATA WRAPPER qfdw"),
            ("an operator", "CREATE FUNCTION qeq(int, int) RETURNS bool LANGUAGE sql AS 'select true'; CREATE OPERATOR === (LEFTARG = int, RIGHTARG = int, FUNCTION = qeq)", "DROP OPERATOR === (int, int); DROP FUNCTION qeq(int, int)"),
        ];
        let name = tidy.db("miss");
        let mut entry = make_db(l, &name, None).undo.unwrap();
        for (what, make, clean) in cases {
            in_db(l, &name, make);
            let u = undo_of(l, entry);
            let why = err(&u);
            assert!(why.starts_with("Not undone") && why.contains("has tables or other objects"), "{var}: {what}: {why}");
            assert!(exists(l, &name), "{var}: {what}: a refused undo drops nothing");
            in_db(l, &name, clean);
            entry = u.undo.unwrap_or_else(|| panic!("{var}: {what}: the entry stays"));
        }

        // Another session connected: refused, and the database takes connections again afterwards, as before the undo.
        let other = foreign(l, &name);
        let u = undo_of(l, entry);
        assert!(err(&u).contains("another session is connected"), "{var}: {:?}", u.res);
        assert_eq!(l.one(&format!("SELECT datallowconn::text FROM pg_database WHERE datname = {}", lit(&name))), "true", "{var}: a refused undo leaves the database open to connections");
        l.w.rt.block_on(async { use sqlx::Connection; other.close().await.unwrap() });
        let u = undo_of(l, u.undo.unwrap());
        u.res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert!(!exists(l, &name), "{var}");

        // Dropped and made again under the same name: another database, which this did not create.
        let name = tidy.db("again");
        let a = make_db(l, &name, None);
        l.w.rt.block_on(async { sqlx::raw_sql(&format!("DROP DATABASE {} WITH (FORCE)", ident(&name))).execute(&l.pool).await.unwrap() });
        assert!(make_db_plain(l, &name));
        let u = undo_of(l, a.undo.unwrap());
        assert!(err(&u).contains("made again"), "{var}: {:?}", u.res);
        assert!(exists(l, &name) && u.undo.is_some(), "{var}");

        // What the template carried in is not the person's work: a table in the template does not stop the undo, one added later does.
        let tpl = tidy.db("tpl");
        make_db(l, &tpl, None).res.unwrap();
        in_db(l, &tpl, "CREATE TABLE inherited (id int)");
        l.w.rt.block_on(async { sqlx::raw_sql(&format!("ALTER DATABASE {} IS_TEMPLATE true", ident(&tpl))).execute(&l.pool).await.unwrap() });
        let (kept, mine) = (tidy.db("kid"), tidy.db("kid2"));
        let a = make_db(l, &kept, Some(&tpl));
        a.res.clone().unwrap_or_else(|x| panic!("{var}: {x}"));
        let u = undo_of(l, a.undo.unwrap());
        u.res.unwrap_or_else(|x| panic!("{var}: a database copied from a template with a table is empty for the undo: {x}"));
        let a = make_db(l, &mine, Some(&tpl));
        in_db(l, &mine, "CREATE TABLE mine (id int)");
        let u = undo_of(l, a.undo.unwrap());
        assert!(err(&u).contains("has tables"), "{var}: {:?}", u.res);
    });
}

/// CREATE DATABASE by hand for a test that needs a second database of a name, taking the same turn as `make_db`.
fn make_db_plain(l: &LivePg, name: &str) -> bool {
    let _turn = db_turn();
    for attempt in 0..8 {
        match l.w.rt.block_on(async { sqlx::raw_sql(&format!("CREATE DATABASE {}", ident(name))).execute(&l.pool).await }) {
            Ok(_) => return true,
            Err(x) if x.as_database_error().and_then(|d| d.code().map(|c| c.to_string())).as_deref() == Some("55006") && attempt < 7 => std::thread::sleep(std::time::Duration::from_millis(300 * (attempt + 1))),
            Err(x) => panic!("{x}"),
        }
    }
    false
}

#[test]
fn the_connections_own_database_and_system_names_are_refused() {
    every_pg(|l, var| {
        let tidy = Tidy::new(l);
        for n in ["postgres", "template0", "template1"] {
            assert!(create_pg::create_db_sql(n, None, &[]).is_err() && create_pg::drop_db_sql(n, false).is_err(), "{var}: {n}");
            let why = err(&drop_db(l, n, false));
            assert!(why.contains("belongs to the server"), "{var}: {why}");
        }
        // Names are exact on PostgreSQL: Postgres is a database of its own and is made like any other.
        assert!(create_pg::create_db_sql("Postgres", None, &[]).is_ok() && create_pg::create_db_sql("POSTGRES", None, &[]).is_ok());
        assert!(create_pg::create_db_sql(&"x".repeat(64), None, &[]).is_err());
        assert!(create_pg::create_db_sql("a\nb", None, &[]).is_err() && create_pg::create_db_sql("", None, &[]).is_err());
        for s in ["pg_catalog", "information_schema", "pg_toast", "pg_toast_temp_1", "pg_temp_3", "pg_mine"] {
            assert!(create_pg::create_schema_sql(s).is_err(), "{var}: create {s}");
        }
        for s in ["pg_catalog", "information_schema", "pg_toast", "pg_temp_3"] {
            assert!(create_pg::drop_schema_sql(s, true).is_err() && create_pg::drop_table_sql(s, "t", false, false).is_err(), "{var}: drop {s}");
        }
        assert!(create_pg::drop_schema_sql("Public", false).is_ok() && create_pg::drop_schema_sql("pgx", false).is_ok());
        let why = err(&{
            let spec = TableSpec { schema: "pg_catalog".into(), name: "t".into(), cols: vec![ColDef::new("a", "int")], ..Default::default() };
            l.w.pg_create_table(1, l.db.at("quarry_test"), spec, false);
            ans(l)
        });
        assert!(why.contains("belongs to the server"), "{var}: {why}");

        // The database the connection opens on cannot be dropped from it: Quarry keeps its maintenance pool there.
        let name = tidy.db("own");
        make_db(l, &name, None).res.unwrap();
        let (prefix, _) = l.uri.rsplit_once('/').unwrap();
        let at = live_pg_at(format!("{prefix}/{name}"));
        at.w.pg_drop_db(1, at.db.clone(), name.clone(), false, false);
        let why = err(&ans(&at));
        assert!(why.contains("opens on") && exists(l, &name), "{var}: {why}");
        assert!(info(&at, &name, "", "").own_database, "{var}: the dialog is told");
        assert!(!info(l, &name, "", "").own_database);
    });
}

#[test]
fn a_database_named_with_quotes_capitals_and_spaces() {
    every_pg(|l, var| {
        let tidy = Tidy::new(l);
        let name = format!("{} \"Odd\" DB 'x'", tidy.db("awk"));
        tidy.dbs.borrow_mut().push(name.clone());
        let a = make_db(l, &name, None);
        a.res.clone().unwrap_or_else(|x| panic!("{var}: {x}"));
        assert!(exists(l, &name) && a.dbs.is_some_and(|d| d.contains(&name)), "{var}");
        // The name written differently is another database, which does not exist.
        assert!(!exists(l, &name.to_lowercase()));
        let i = info(l, &name, "", "");
        assert!(i.exists && !i.is_template && i.size.unwrap_or(0) > 0, "{var}: {i:?}");
        drop_db(l, &name, false).res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert!(!exists(l, &name), "{var}");
        assert!(!info(l, &name, "", "").exists, "{var}: a database that is gone says so");
    });
}

fn schema_with(l: &LivePg, pool: &sqlx::PgPool, name: &str, ddl: &str) {
    l.qsql(pool, &format!("DROP SCHEMA IF EXISTS {n} CASCADE; CREATE SCHEMA {n}; {ddl}", n = ident(name)));
}

#[test]
fn a_schema_is_made_and_its_undo_needs_it_empty_and_the_same() {
    every_pg(|l, var| {
        let tidy = Tidy::new(l);
        let db = l.quarry_test();
        let pool = l.fixture_pool();
        let name = tidy.schema(&format!("{} \"Odd\" S", uniq("sch")));
        l.w.pg_create_schema(1, db.clone(), name.clone(), false);
        let a = ans(l);
        a.res.clone().unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!((a.what, a.database.as_str()), (What::CreatedSchema, "quarry_test"), "{var}: the answer names the database");
        let has = || l.qone(&pool, &format!("SELECT count(*)::text FROM pg_namespace WHERE nspname = {}", lit(&name))) == "1";
        assert!(has());
        let u = undo_of(l, a.undo.unwrap());
        u.res.clone().unwrap_or_else(|x| panic!("{var}: {x}"));
        assert!(!has() && u.undo.is_none(), "{var}: an empty new schema goes");

        // Something in it: refused by the server inside the transaction, nothing dropped, entry handed back.
        l.w.pg_create_schema(1, db.clone(), name.clone(), false);
        let a = ans(l);
        l.qsql(&pool, &format!("CREATE TABLE {}.t (id int)", ident(&name)));
        let u = undo_of(l, a.undo.unwrap());
        let why = err(&u);
        assert!(why.starts_with("Not undone") && why.contains("has objects"), "{var}: {why}");
        assert!(has() && u.undo.is_some(), "{var}");
        // Dropped and made again by someone else: not the schema this made.
        l.qsql(&pool, &format!("DROP SCHEMA {} CASCADE; CREATE SCHEMA {}", ident(&name), ident(&name)));
        let u = undo_of(l, u.undo.unwrap());
        assert!(err(&u).contains("made again"), "{var}");
        assert!(has(), "{var}");
        // A name the server already has is the server's own refusal.
        l.w.pg_create_schema(1, db.clone(), name.clone(), false);
        let why = err(&ans(l));
        assert!(why.contains("42P06"), "{var}: {why}");
    });
}

#[test]
fn dropping_a_schema_lists_what_cascade_would_take_and_needs_it() {
    every_pg(|l, var| {
        let tidy = Tidy::new(l);
        let db = l.quarry_test();
        let pool = l.fixture_pool();
        let (a, b) = (tidy.schema(&uniq("a")), tidy.schema(&uniq("b")));
        schema_with(l, &pool, &b, "");
        schema_with(l, &pool, &a, &format!(
            "CREATE TABLE {a}.t (id int PRIMARY KEY, n text); INSERT INTO {a}.t VALUES (1, 'x'); CREATE FUNCTION {a}.f() RETURNS int LANGUAGE sql AS 'select 1'; \
             CREATE VIEW {b}.v AS SELECT id FROM {a}.t; CREATE TABLE {b}.child (id int REFERENCES {a}.t (id)); CREATE SEQUENCE {a}.s",
            a = ident(&a), b = ident(&b)));
        let i = info(l, "quarry_test", &a, "");
        let before = objects(l, &pool, &[&a, &b]);
        assert!(i.exists, "{var}: {i:?}");
        let all = i.dependents.join("\n");
        assert!(all.contains("constraint"), "{var}: the key of the other schema's table is listed\n{all}");
        assert!(!all.contains("index"), "{var}: an index is not listed, it goes with its table\n{all}");

        let drop = |cascade: bool| {
            l.w.pg_drop_schema(1, db.clone(), a.clone(), cascade, false);
            ans(l)
        };
        let why = err(&drop(false));
        assert!(why.contains("2BP01") && why.contains("CASCADE"), "{var}: {why}");
        assert_eq!(l.qone(&pool, &format!("SELECT count(*)::text FROM {}.t", ident(&a))), "1", "{var}: a refused drop dropped nothing");
        drop(true).res.unwrap_or_else(|x| panic!("{var}: {x}"));
        let took: std::collections::BTreeSet<String> = before.difference(&objects(l, &pool, &[&a, &b])).cloned().collect();
        assert_eq!(kinds(&i.dependents), took, "{var}: the list is exactly what the server dropped");
        assert_eq!(i.dependents_total as usize, i.dependents.len(), "{var}: nothing is cut off here");
        assert_eq!(l.qone(&pool, &format!("SELECT count(*)::text FROM pg_namespace WHERE nspname = {}", lit(&a))), "0", "{var}");
        assert_eq!(l.qone(&pool, &format!("SELECT count(*)::text FROM pg_class WHERE relnamespace = {}::regnamespace AND relname IN ('child')", lit(&ident(&b)))), "1", "{var}: the other schema's table stays, only its key went");
        assert_eq!(l.qone(&pool, &format!("SELECT count(*)::text FROM pg_views WHERE schemaname = {} AND viewname = 'v'", lit(&b))), "0", "{var}: a view over the schema's table went with it");
        // An empty schema lists nothing, and plain DROP takes it.
        l.w.pg_create_schema(1, db.clone(), a.clone(), false);
        ans(l).res.unwrap();
        let i = info(l, "quarry_test", &a, "");
        assert_eq!(i.dependents_total, 0);
        drop(false).res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert!(!info(l, "quarry_test", &a, "").exists);
    });
}

#[test]
fn what_a_cascade_drops_is_read_off_the_server_with_partitions_defaults_and_columns() {
    every_pg(|l, var| {
        let tidy = Tidy::new(l);
        let db = l.quarry_test();
        let pool = l.fixture_pool();
        let drop_schema = |name: &str, cascade: bool| {
            l.w.pg_drop_schema(1, db.clone(), name.to_string(), cascade, false);
            ans(l)
        };
        // A partitioned table whose partition lives in another schema, a default that calls a sequence, a generated column and an index
        // that call a function: everything the server drops with the schema `s`, none of it a plain table of `s`.
        let (s, o) = (tidy.schema(&uniq("ps")), tidy.schema(&uniq("po")));
        schema_with(l, &pool, &o, "");
        let (qs, qo) = (ident(&s), ident(&o));
        schema_with(
            l,
            &pool,
            &s,
            &format!(
                "CREATE SEQUENCE {qs}.seq; CREATE FUNCTION {qs}.f(int) RETURNS int IMMUTABLE LANGUAGE sql AS 'select $1'; CREATE TYPE {qs}.pair AS (a int, b int); \
                 CREATE TABLE {qs}.parent (id int, k int) PARTITION BY LIST (k); CREATE TABLE {qo}.p1 PARTITION OF {qs}.parent FOR VALUES IN (1); INSERT INTO {qs}.parent VALUES (1, 1); \
                 CREATE TABLE {qo}.t (x int DEFAULT nextval({}), y int GENERATED ALWAYS AS ({qs}.f(x)) STORED); CREATE INDEX t_fx ON {qo}.t ({qs}.f(x))",
                lit(&format!("{qs}.seq"))
            ),
        );
        let i = info(l, "quarry_test", &s, "");
        let before = objects(l, &pool, &[&s, &o]);
        let all = i.dependents.join("\n");
        assert!(i.dependents_total >= 8, "{var}: the partition, the default, the generated column's expression and the index are all dropped with it\n{all}");
        assert!(all.contains("t_fx") && all.contains("default value for column x") && all.contains("default value for column y"), "{var}: {all}");
        assert!(i.dependents.iter().any(|x| x.contains("p1") && x.contains("1 row") && x.contains("outside this schema")), "{var}: the partition is named with its rows and as outside the schema\n{all}");
        assert!(i.dependents[0].contains("outside this schema"), "{var}: what is outside the schema comes first\n{all}");
        assert!(err(&drop_schema(&s, false)).contains("2BP01"), "{var}");
        drop_schema(&s, true).res.unwrap_or_else(|x| panic!("{var}: {x}"));
        let took: std::collections::BTreeSet<String> = before.difference(&objects(l, &pool, &[&s, &o])).cloned().collect();
        assert_eq!(kinds(&i.dependents), took, "{var}: the list is exactly what the server dropped\n{all}");
        assert_eq!(l.qone(&pool, &format!("SELECT count(*)::text FROM pg_class WHERE relname = 'p1' AND relnamespace = {}::regnamespace", lit(&qo))), "0", "{var}: the partition went with its row");

        // A schema that holds only a partitioned table is not empty, so the confirmation asks for CASCADE.
        let only = tidy.schema(&uniq("pt"));
        schema_with(l, &pool, &only, &format!("CREATE TABLE {}.parent (id int) PARTITION BY RANGE (id)", ident(&only)));
        let i = info(l, "quarry_test", &only, "");
        assert_eq!(i.dependents_total, 1, "{var}: {:?}", i.dependents);
        assert!(err(&drop_schema(&only, false)).contains("2BP01"), "{var}");
        drop_schema(&only, true).res.unwrap_or_else(|x| panic!("{var}: {x}"));

        // An enum used by a column: the server drops the column and keeps the table and the view over its other columns.
        let (e, t) = (tidy.schema(&uniq("ce")), tidy.schema(&uniq("ct")));
        schema_with(l, &pool, &t, "");
        schema_with(l, &pool, &e, &format!("CREATE TYPE {qe}.mood AS ENUM ('a', 'b'); CREATE TABLE {qt}.t2 (id int, m {qe}.mood); CREATE VIEW {qt}.v2 AS SELECT id FROM {qt}.t2", qe = ident(&e), qt = ident(&t)));
        let i = info(l, "quarry_test", &e, "");
        let all = i.dependents.join("\n");
        assert!(all.contains("column m of table") && !kinds(&i.dependents).iter().any(|x| x.starts_with("table ") || x.starts_with("view ")), "{var}: only the column is listed, not its table or the view\n{all}");
        let before = objects(l, &pool, &[&e, &t]);
        drop_schema(&e, true).res.unwrap_or_else(|x| panic!("{var}: {x}"));
        let took: std::collections::BTreeSet<String> = before.difference(&objects(l, &pool, &[&e, &t])).cloned().collect();
        assert_eq!(kinds(&i.dependents), took, "{var}: {all}");
        assert_eq!(l.qone(&pool, &format!("SELECT count(*)::text FROM pg_views WHERE schemaname = {} AND viewname = 'v2'", lit(&t))), "1", "{var}: the view stays");
        assert_eq!(l.qone(&pool, &format!("SELECT count(*)::text FROM pg_attribute WHERE attrelid = {}::regclass AND attname = 'm' AND NOT attisdropped", lit(&format!("{}.t2", ident(&t))))), "0", "{var}: the column went");

        // A partitioned table dropped on its own takes its partitions, which hold rows.
        let (a, b) = (tidy.schema(&uniq("pa")), tidy.schema(&uniq("pb")));
        schema_with(l, &pool, &b, "");
        schema_with(l, &pool, &a, &format!("CREATE TABLE {qa}.pp (id int) PARTITION BY RANGE (id); CREATE TABLE {qb}.pp1 PARTITION OF {qa}.pp FOR VALUES FROM (0) TO (10); INSERT INTO {qa}.pp VALUES (1), (2)", qa = ident(&a), qb = ident(&b)));
        let i = info(l, "quarry_test", &a, "pp");
        let before = objects(l, &pool, &[&a, &b]);
        assert!(i.dependents.iter().any(|x| x.contains("pp1") && x.contains("2 rows")), "{var}: {:?}", i.dependents);
        l.w.pg_drop_table(1, db.clone(), a.clone(), "pp".into(), false, true, false);
        ans(l).res.unwrap_or_else(|x| panic!("{var}: {x}"));
        let mut took: std::collections::BTreeSet<String> = before.difference(&objects(l, &pool, &[&a, &b])).cloned().collect();
        assert!(took.remove(&format!("table {a}.pp")), "{var}: {took:?}");
        assert_eq!(kinds(&i.dependents), took, "{var}");
    });
}

fn col(name: &str, ty: &str) -> ColDef {
    ColDef::new(name, ty)
}

fn made(l: &LivePg, db: &Db, spec: TableSpec) -> Ans {
    l.w.pg_create_table(1, db.clone(), spec, false);
    ans(l)
}

fn count(l: &LivePg, pool: &sqlx::PgPool, sql: &str) -> String {
    l.qone(pool, sql)
}

#[test]
fn a_table_is_built_with_identity_defaults_keys_and_comments() {
    every_pg(|l, var| {
        let tidy = Tidy::new(l);
        let db = l.quarry_test();
        let pool = l.fixture_pool();
        let s = tidy.schema(&uniq("tbl"));
        // A type of the schema's own: the statements run under the schema's search_path, so `mood` is this one and not a stranger's.
        schema_with(l, &pool, &s, &format!("CREATE TYPE {}.mood AS ENUM ('ok', 'sad')", ident(&s)));
        let table = "Line \"Items\"";
        let spec = TableSpec {
            schema: s.clone(),
            name: table.into(),
            cols: vec![
                ColDef { auto_increment: true, nullable: false, ..col("id", "integer") },
                ColDef { nullable: false, default: Dv::Value("it's \\ ok".into()), ..col("Order Code", "varchar(12)") },
                ColDef { nullable: false, default: Dv::Expr("now()".into()), comment: "when it was made".into(), ..col("created", "timestamptz") },
                ColDef { default: Dv::Value("ok".into()), ..col("Mood", "mood") },
                ColDef { comment: "free 'text'".into(), ..col("note", "text") },
                ColDef { auto_increment: true, identity_always: true, nullable: false, ..col("ticket", "bigint") },
            ],
            pk: vec!["id".into()],
            uniques: vec![vec!["Order Code".into()], vec!["note".into(), "Mood".into()]],
            comment: "demo 'table'".into(),
        };
        let stmts = create_pg::create_table_stmts(&spec).unwrap();
        assert!(stmts[0].starts_with(&format!("CREATE TABLE {}.{} (", ident(&s), ident(table))), "{}", stmts[0]);
        let a = made(l, &db, spec.clone());
        a.res.clone().unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!((a.what, a.database.as_str()), (What::CreatedTable, "quarry_test"), "{var}");
        let q = format!("{}.{}", ident(&s), ident(table));
        let one = |sql: &str| count(l, &pool, &sql.replace("@T", &lit(&q)));
        assert_eq!(one("SELECT string_agg(attname, ',' ORDER BY attnum) FROM pg_attribute WHERE attrelid = @T::regclass AND attnum > 0 AND NOT attisdropped"), "id,Order Code,created,Mood,note,ticket", "{var}");
        assert_eq!(one("SELECT string_agg(attname || ':' || attidentity::text, ',' ORDER BY attnum) FROM pg_attribute WHERE attrelid = @T::regclass AND attnum > 0 AND attidentity <> ''"), "id:d,ticket:a", "{var}: by default and always");
        assert_eq!(one("SELECT string_agg(attname, ',' ORDER BY attnum) FROM pg_attribute WHERE attrelid = @T::regclass AND attnum > 0 AND attnotnull"), "id,Order Code,created,ticket", "{var}");
        let def = |c: &str| one(&format!("SELECT pg_get_expr(adbin, adrelid) FROM pg_attrdef WHERE adrelid = @T::regclass AND adnum = (SELECT attnum FROM pg_attribute WHERE attrelid = @T::regclass AND attname = {})", lit(c)));
        assert_eq!(def("Order Code"), "'it''s \\ ok'::character varying", "{var}: a quote doubled, a backslash left alone");
        assert_eq!(def("created"), "now()", "{var}");
        assert!(def("Mood").starts_with("'ok'::"), "{var}: {}", def("Mood"));
        assert_eq!(one("SELECT format_type(atttypid, atttypmod) FROM pg_attribute WHERE attrelid = @T::regclass AND attname = 'Mood'"), format!("{s}.mood"), "{var}: the schema's own type");
        let cons = one("SELECT string_agg(contype::text || ':' || pg_get_constraintdef(oid), ' | ' ORDER BY contype, conname) FROM pg_constraint WHERE conrelid = @T::regclass");
        assert!(cons.contains("p:PRIMARY KEY (id)") && cons.contains("u:UNIQUE (\"Order Code\")") && cons.contains("UNIQUE (note, \"Mood\")"), "{var}: {cons}");
        assert_eq!(one("SELECT obj_description(@T::regclass, 'pg_class')"), "demo 'table'", "{var}");
        assert_eq!(one("SELECT col_description(@T::regclass, 3)"), "when it was made", "{var}");
        assert_eq!(one("SELECT col_description(@T::regclass, 5)"), "free 'text'", "{var}");
        // The structure editor reads it back, which is what the app opens next.
        l.w.sql_structure(9, db.clone(), s.clone(), table.into());
        let Msg::SqlStructure { res, .. } = l.next() else { panic!("expected SqlStructure") };
        let st = res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(st.columns.len(), 6, "{var}");

        // A row made from the defaults carries the odd literal as written.
        l.qsql(&pool, &format!("INSERT INTO {q} (note) VALUES (DEFAULT)"));
        assert_eq!(one(&format!("SELECT \"Order Code\" FROM {q}")), "it's \\ ok", "{var}");
        let u = undo_of(l, a.undo.unwrap());
        let why = err(&u);
        assert!(why.starts_with("Not undone") && why.contains("has rows"), "{var}: {why}");
        assert_eq!(one(&format!("SELECT count(*)::text FROM {q}")), "1", "{var}: the row is still there");
        // Empty again and unchanged: it goes.
        l.qsql(&pool, &format!("DELETE FROM {q}"));
        let u = undo_of(l, u.undo.unwrap());
        u.res.clone().unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(one("SELECT count(*)::text FROM pg_class WHERE oid = to_regclass(@T)"), "0", "{var}");
        assert!(u.undo.is_none());
    });
}

#[test]
fn the_undo_of_a_table_create_refuses_a_table_that_is_not_the_one_it_made() {
    every_pg(|l, var| {
        let tidy = Tidy::new(l);
        let db = l.quarry_test();
        let pool = l.fixture_pool();
        let s = tidy.schema(&uniq("tundo"));
        schema_with(l, &pool, &s, "");
        let spec = |name: &str| TableSpec { schema: s.clone(), name: name.into(), cols: vec![ColDef { auto_increment: true, nullable: false, ..col("id", "integer") }, col("v", "text")], pk: vec!["id".into()], ..Default::default() };
        let q = |t: &str| format!("{}.{}", ident(&s), ident(t));
        let has = |t: &str| l.qone(&pool, &format!("SELECT (to_regclass({}) IS NOT NULL)::text", lit(&q(t)))) == "true";
        // changed since: a column added
        let a = made(l, &db, spec("changed"));
        a.res.clone().unwrap();
        l.qsql(&pool, &format!("ALTER TABLE {} ADD COLUMN extra int", q("changed")));
        let u = undo_of(l, a.undo.unwrap());
        let why = err(&u);
        assert!(why.contains("structure of changed changed"), "{var}: {why}");
        assert!(has("changed") && u.undo.is_some());
        // an index added
        l.qsql(&pool, &format!("ALTER TABLE {t} DROP COLUMN extra; CREATE INDEX ON {t} (v)", t = q("changed")));
        let u = undo_of(l, u.undo.unwrap());
        assert!(err(&u).contains("changed"), "{var}");
        // dropped and made again with the same structure: another table
        let a = made(l, &db, spec("again"));
        let first = l.qone(&pool, &format!("SELECT to_regclass({})::oid::text", lit(&q("again"))));
        l.qsql(&pool, &format!("DROP TABLE {t}; CREATE TABLE {t} (id integer GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, v text)", t = q("again")));
        assert_ne!(first, l.qone(&pool, &format!("SELECT to_regclass({})::oid::text", lit(&q("again")))));
        let u = undo_of(l, a.undo.unwrap());
        assert!(err(&u).contains("made again"), "{var}: {:?}", u.res);
        assert!(has("again"));
        // gone altogether
        let a = made(l, &db, spec("vanished"));
        l.qsql(&pool, &format!("DROP TABLE {}", q("vanished")));
        let u = undo_of(l, a.undo.unwrap());
        assert!(err(&u).contains("already gone"), "{var}: {:?}", u.res);
        // another table's key on it since: the structure read already sees what points at it, and the server would refuse anyway
        let a = made(l, &db, spec("pointed"));
        l.qsql(&pool, &format!("CREATE TABLE {c} (pid int REFERENCES {t} (id))", c = q("pointer"), t = q("pointed")));
        let u = undo_of(l, a.undo.unwrap());
        assert!(err(&u).starts_with("Not undone"), "{var}: {:?}", u.res);
        assert!(has("pointed"));
        // a view over it since
        let a = made(l, &db, spec("viewed"));
        l.qsql(&pool, &format!("CREATE VIEW {v} AS SELECT * FROM {t}", v = q("viewer"), t = q("viewed")));
        let u = undo_of(l, a.undo.unwrap());
        let why = err(&u);
        assert!(why.starts_with("Not undone") && (why.contains("depends on") || why.contains("changed")), "{var}: {why}");
        assert!(has("viewed"));
        // a lock held by another session: the undo waits its 3 s and says whose lock it is, nothing changed
        let a = made(l, &db, spec("locked"));
        let mut holder = {
            use sqlx::Connection;
            let o = sqlx::postgres::PgConnectOptions::from_str(&l.uri).unwrap().database("quarry_test");
            l.w.rt.block_on(async { sqlx::PgConnection::connect_with(&o).await.unwrap() })
        };
        l.w.rt.block_on(async { sqlx::raw_sql(&format!("BEGIN; LOCK TABLE {} IN ACCESS SHARE MODE", q("locked"))).execute(&mut holder).await.unwrap() });
        let u = undo_of(l, a.undo.unwrap());
        assert!(err(&u).contains("locked by another session"), "{var}: {:?}", u.res);
        l.w.rt.block_on(async { sqlx::raw_sql("ROLLBACK").execute(&mut holder).await.unwrap() });
        undo_of(l, u.undo.unwrap()).res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert!(!has("locked"));
    });
}

#[test]
fn the_undo_of_a_table_create_refuses_row_security_triggers_and_comments_added_since() {
    every_pg(|l, var| {
        let tidy = Tidy::new(l);
        let db = l.quarry_test();
        let pool = l.fixture_pool();
        let s = tidy.schema(&uniq("tx"));
        schema_with(l, &pool, &s, "");
        let spec = TableSpec { schema: s.clone(), name: "t".into(), cols: vec![col("v", "text")], ..Default::default() };
        let t = format!("{}.t", ident(&s));
        let mut entry = made(l, &db, spec).undo.unwrap();
        l.qsql(&pool, &format!("CREATE FUNCTION {}.tf() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$", ident(&s)));
        // What the structure editor does not show, each one added and taken away again: the undo refuses while it is there, and drops the
        // table once it is gone, so the check reads these and a table with them is not taken for the one that was created.
        let cases = [
            ("row security", format!("ALTER TABLE {t} ENABLE ROW LEVEL SECURITY"), format!("ALTER TABLE {t} DISABLE ROW LEVEL SECURITY")),
            ("forced row security", format!("ALTER TABLE {t} ENABLE ROW LEVEL SECURITY; ALTER TABLE {t} FORCE ROW LEVEL SECURITY"), format!("ALTER TABLE {t} NO FORCE ROW LEVEL SECURITY; ALTER TABLE {t} DISABLE ROW LEVEL SECURITY")),
            ("a policy", format!("CREATE POLICY p ON {t} USING (false)"), format!("DROP POLICY p ON {t}")),
            ("a trigger", format!("CREATE TRIGGER trg BEFORE INSERT ON {t} FOR EACH ROW EXECUTE FUNCTION {}.tf()", ident(&s)), format!("DROP TRIGGER trg ON {t}")),
            ("a comment", format!("COMMENT ON TABLE {t} IS 'mine'"), format!("COMMENT ON TABLE {t} IS NULL")),
        ];
        for (what, make, clean) in cases {
            l.qsql(&pool, &make);
            let u = undo_of(l, entry);
            let why = err(&u);
            assert!(why.starts_with("Not undone") && why.contains("changed"), "{var}: {what}: {why}");
            assert_eq!(l.qone(&pool, &format!("SELECT (to_regclass({}) IS NOT NULL)::text", lit(&t))), "true", "{var}: {what}: a refused undo drops nothing");
            l.qsql(&pool, &clean);
            entry = u.undo.unwrap_or_else(|| panic!("{var}: {what}: the entry stays"));
        }
        undo_of(l, entry).res.unwrap_or_else(|x| panic!("{var}: {x}"));
    });
}

/// Drops a role and what it owns, also when the test fails on the way.
struct RoleGuard<'a> {
    l: &'a LivePg,
    pool: sqlx::PgPool,
    role: String,
}

impl Drop for RoleGuard<'_> {
    fn drop(&mut self) {
        let _ = self.l.w.rt.block_on(async { sqlx::raw_sql(&format!("DROP OWNED BY {}", ident(&self.role))).execute(&self.pool).await });
        let _ = self.l.w.rt.block_on(async { sqlx::raw_sql(&format!("DROP ROLE IF EXISTS {}", ident(&self.role))).execute(&self.l.pool).await });
    }
}

#[test]
fn the_undo_of_a_table_create_is_refused_when_the_role_cannot_lock_the_table() {
    every_pg(|l, var| {
        let tidy = Tidy::new(l);
        let pool = l.fixture_pool();
        let role = uniq("role");
        let s = tidy.schema(&uniq("tl"));
        l.w.rt.block_on(async { sqlx::raw_sql(&format!("CREATE ROLE {} LOGIN PASSWORD 'pw'", ident(&role))).execute(&l.pool).await.unwrap() });
        let _guard = RoleGuard { l, pool: pool.clone(), role: role.clone() };
        l.qsql(&pool, &format!("CREATE SCHEMA {} AUTHORIZATION {}", ident(&s), ident(&role)));
        let o = sqlx::postgres::PgConnectOptions::from_str(&l.uri).unwrap();
        let rl = crate::db::sql::testing::live_pg_at(format!("postgres://{role}:pw@{}:{}/quarry_test", o.get_host(), o.get_port()));
        let rdb = rl.quarry_test();
        let spec = TableSpec { schema: s.clone(), name: "t".into(), cols: vec![col("v", "text")], ..Default::default() };
        let a = made(&rl, &rdb, spec);
        a.res.clone().unwrap_or_else(|x| panic!("{var}: {x}"));
        let t = format!("{}.t", ident(&s));
        // The owner revokes its own privileges: it can still drop the table, but cannot take the lock the empty check relies on.
        l.qsql(&pool, &format!("REVOKE ALL ON TABLE {t} FROM {}", ident(&role)));
        let u = undo_of(&rl, a.undo.unwrap());
        let why = err(&u);
        assert!(why.starts_with("Not undone") && why.contains("cannot be locked"), "{var}: {why}");
        assert_eq!(l.qone(&pool, &format!("SELECT (to_regclass({}) IS NOT NULL)::text", lit(&t))), "true", "{var}: the table is still there");
        l.qsql(&pool, &format!("GRANT ALL ON TABLE {t} TO {}", ident(&role)));
        undo_of(&rl, u.undo.unwrap()).res.unwrap_or_else(|x| panic!("{var}: {x}"));
    });
}

#[test]
fn a_table_that_cannot_be_built_is_refused_before_or_by_the_server_and_nothing_is_left() {
    every_pg(|l, var| {
        let tidy = Tidy::new(l);
        let db = l.quarry_test();
        let pool = l.fixture_pool();
        let s = tidy.schema(&uniq("bad"));
        schema_with(l, &pool, &s, "");
        let base = |cols: Vec<ColDef>| TableSpec { schema: s.clone(), name: "t".into(), cols, ..Default::default() };
        let no_table = || l.qone(&pool, &format!("SELECT (to_regclass({}) IS NULL)::text", lit(&format!("{}.t", ident(&s))))) == "true";
        // refused by the builder
        for (what, spec) in [
            ("no columns", base(vec![])),
            ("a column twice in other case", base(vec![col("A", "int"), col("a", "int")])),
            ("a type with a second clause", base(vec![col("a", "integer, b text")])),
            ("a type with a comment", base(vec![col("a", "int -- x")])),
            ("an identity on text", base(vec![ColDef { auto_increment: true, nullable: false, ..col("a", "text") }])),
            ("an identity with a default", base(vec![ColDef { auto_increment: true, nullable: false, default: Dv::Value("1".into()), ..col("a", "int") }])),
            ("a default that is two statements", base(vec![ColDef { default: Dv::Expr("1); DROP TABLE x; (2".into()), ..col("a", "int") }])),
            ("a key on a missing column", TableSpec { pk: vec!["zz".into()], ..base(vec![col("a", "int")]) }),
            ("a unique constraint with a column twice", TableSpec { uniques: vec![vec!["a".into(), "a".into()]], ..base(vec![col("a", "int")]) }),
            ("a name of 64 bytes", TableSpec { name: "x".repeat(64), ..base(vec![col("a", "int")]) }),
            ("a name with a line break", TableSpec { name: "x\ny".into(), ..base(vec![col("a", "int")]) }),
            ("a NUL in a comment", TableSpec { comment: "a\0b".into(), ..base(vec![col("a", "int")]) }),
        ] {
            assert!(create_pg::create_table_stmts(&spec).is_err(), "{var}: {what} was not refused");
        }
        // refused by the server's own check of the type, inside the transaction, so nothing is created
        let a = made(l, &db, base(vec![col("a", "int"), col("b", "nosuchtype")]));
        assert!(err(&a).contains("not a type the server knows"), "{var}: {:?}", a.res);
        assert!(no_table(), "{var}");
        // a default the server cannot read, and a key it cannot build, fail inside the transaction and leave nothing behind
        let a = made(l, &db, TableSpec { comment: "ok".into(), ..base(vec![ColDef { default: Dv::Expr("no_such_function_x(1)".into()), ..col("a", "int") }, col("b", "int")]) });
        let why = err(&a);
        assert!(why.contains("42883") && no_table(), "{var}: {why}");
        let a = made(l, &db, TableSpec { pk: vec!["p".into()], ..base(vec![col("p", "point")]) });
        let why = err(&a);
        assert!(why.contains("42704") && no_table(), "{var}: {why}");
        // a table that exists is the server's refusal
        made(l, &db, base(vec![col("a", "int")])).res.unwrap();
        let why = err(&made(l, &db, base(vec![col("a", "int")])));
        assert!(why.contains("42P07"), "{var}: {why}");
        // a schema that does not exist
        let why = err(&made(l, &db, TableSpec { schema: "no_such_schema_x".into(), ..base(vec![col("a", "int")]) }));
        assert!(why.contains("3F000"), "{var}: {why}");
        // locked
        l.w.pg_create_table(1, db.clone(), base(vec![col("a", "int")]), true);
        assert!(err(&ans(l)).contains("locked"), "{var}");
    });
}

#[test]
fn dropping_a_table_lists_what_depends_on_it_and_cascade_is_opt_in() {
    every_pg(|l, var| {
        let tidy = Tidy::new(l);
        let db = l.quarry_test();
        let pool = l.fixture_pool();
        let (a, b) = (tidy.schema(&uniq("da")), tidy.schema(&uniq("db")));
        schema_with(l, &pool, &b, "");
        schema_with(l, &pool, &a, &format!(
            "CREATE TABLE {a}.t (id serial PRIMARY KEY, n text); INSERT INTO {a}.t (n) VALUES ('x'), ('y'), ('z'); CREATE VIEW {b}.v AS SELECT * FROM {a}.t; CREATE VIEW {b}.v2 AS SELECT * FROM {b}.v; \
             CREATE TABLE {b}.child (id int REFERENCES {a}.t (id)); CREATE TABLE {a}.plain (id int)",
            a = ident(&a), b = ident(&b)));
        let i = info(l, "quarry_test", &a, "t");
        let before = objects(l, &pool, &[&a, &b]);
        assert_eq!((i.exists, i.rows, i.rows_estimated), (true, Some(3), false), "{var}: {i:?}");
        let all = i.dependents.join("\n");
        assert!(all.contains("view") && all.contains("v2") && all.contains("constraint"), "{var}: a view of a view and a key of another table are listed\n{all}");
        assert!(!all.contains("sequence") && !all.contains("index"), "{var}: what goes with a plain drop is not listed\n{all}");
        assert_eq!(info(l, "quarry_test", &a, "plain").dependents_total, 0);
        assert!(!info(l, "quarry_test", &a, "nope").exists);

        let drop = |t: &str, view: bool, cascade: bool| {
            l.w.pg_drop_table(1, db.clone(), a.clone(), t.into(), view, cascade, false);
            ans(l)
        };
        let why = err(&drop("t", false, false));
        assert!(why.contains("2BP01") && why.contains("CASCADE"), "{var}: {why}");
        assert_eq!(l.qone(&pool, &format!("SELECT count(*)::text FROM {}.t", ident(&a))), "3", "{var}");
        let d = drop("t", false, true);
        d.res.clone().unwrap_or_else(|x| panic!("{var}: {x}"));
        let mut took: std::collections::BTreeSet<String> = before.difference(&objects(l, &pool, &[&a, &b])).cloned().collect();
        assert!(took.remove(&format!("table {}.t", a)), "{var}: the table itself went: {took:?}");
        // The sequence of a serial column goes with a plain DROP too, so it is part of the table and not listed.
        assert!(took.remove(&format!("sequence {a}.t_id_seq")), "{var}: {took:?}");
        assert_eq!(kinds(&i.dependents), took, "{var}: the list is exactly what the server dropped besides the table");
        assert_eq!((d.what, d.undo.is_none()), (What::DroppedTable, true), "{var}: a drop has no undo");
        assert_eq!(l.qone(&pool, &format!("SELECT count(*)::text FROM pg_views WHERE schemaname = {} AND viewname IN ('v', 'v2')", lit(&b))), "0", "{var}");
        assert_eq!(l.qone(&pool, &format!("SELECT count(*)::text FROM {}.child", ident(&b))), "0", "{var}: the table that pointed at it stays, only its key went");
        drop("plain", false, false).res.unwrap_or_else(|x| panic!("{var}: {x}"));
        // a view is dropped as a view, and DROP TABLE on one is the server's refusal
        l.qsql(&pool, &format!("CREATE VIEW {a}.w AS SELECT 1 AS one", a = ident(&a)));
        assert!(err(&drop("w", false, false)).contains("42809"), "{var}");
        drop("w", true, false).res.unwrap_or_else(|x| panic!("{var}: {x}"));
        // awkward names and a table that is gone
        l.qsql(&pool, &format!("CREATE TABLE {a}.{n} (id int)", a = ident(&a), n = ident("Odd \"Name\" 1")));
        drop("Odd \"Name\" 1", false, false).res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert!(err(&drop("Odd \"Name\" 1", false, false)).contains("42P01"), "{var}");
        l.w.pg_drop_table(1, db.clone(), a.clone(), "x".into(), false, false, true);
        assert!(err(&ans(l)).contains("locked"), "{var}");
        assert!(create_pg::drop_table_sql(&a, "", false, false).is_err());
    });
}

/// What a drop dialog reads must never write: counting the rows of a view runs the functions it calls, and one of them can insert.
#[test]
fn reading_what_a_view_drop_takes_writes_nothing() {
    every_pg(|l, var| {
        let tidy = Tidy::new(l);
        let pool = l.fixture_pool();
        let s = tidy.schema(&uniq("rw"));
        let q = ident(&s);
        schema_with(l, &pool, &s, &format!("CREATE TABLE {q}.lg (n int); CREATE FUNCTION {q}.w() RETURNS int LANGUAGE sql AS 'INSERT INTO {q}.lg VALUES (1) RETURNING 1'; CREATE VIEW {q}.v AS SELECT {q}.w() AS one"));
        let i = info(l, "quarry_test", &s, "v");
        assert!(i.exists, "{var}: {i:?}");
        // The exact count runs the view, whose function writes, so a read-only transaction refuses it and the estimate stays. A transaction that
        // allowed writes would count (and roll back) instead.
        assert!(i.rows_estimated, "{var}: the count of a view that writes was not refused: {i:?}");
        assert_eq!(l.qone(&pool, &format!("SELECT count(*)::text FROM {q}.lg")), "0", "{var}: looking at the drop wrote a row");
    });
}

#[test]
fn a_drop_waits_for_the_lock_and_says_whose_it_is() {
    every_pg(|l, var| {
        let tidy = Tidy::new(l);
        let db = l.quarry_test();
        let pool = l.fixture_pool();
        let s = tidy.schema(&uniq("lk"));
        schema_with(l, &pool, &s, &format!("CREATE TABLE {}.t (id int)", ident(&s)));
        let mut holder = {
            use sqlx::Connection;
            let o = sqlx::postgres::PgConnectOptions::from_str(&l.uri).unwrap().database("quarry_test");
            l.w.rt.block_on(async { sqlx::PgConnection::connect_with(&o).await.unwrap() })
        };
        l.w.rt.block_on(async { sqlx::raw_sql(&format!("BEGIN; LOCK TABLE {}.t IN ACCESS SHARE MODE", ident(&s))).execute(&mut holder).await.unwrap() });
        l.w.pg_drop_table(1, db.clone(), s.clone(), "t".into(), false, false, false);
        let why = err(&ans(l));
        assert!(why.contains("locked by another session"), "{var}: {why}");
        l.w.rt.block_on(async { sqlx::raw_sql("ROLLBACK").execute(&mut holder).await.unwrap() });
        l.w.pg_drop_table(1, db.clone(), s.clone(), "t".into(), false, false, false);
        ans(l).res.unwrap_or_else(|x| panic!("{var}: {x}"));
    });
}

/// Behind PgBouncer in transaction mode: a schema and a table are made and undone like anywhere, because each runs in one transaction. A
/// database is made and dropped with single statements, and what happens there is checked here, not assumed.
#[test]
fn behind_a_pooler_a_table_is_made_and_undone_and_a_database_is_made_and_dropped() {
    let Some(l) = live_pgbouncer() else { return };
    let tidy = Tidy::new(&l);
    let db = l.quarry_test();
    let pool = l.fixture_pool();
    let s = tidy.schema(&uniq("pgb"));
    l.w.pg_create_schema(1, db.clone(), s.clone(), false);
    let a = ans(&l);
    a.res.clone().unwrap_or_else(|x| panic!("schema: {x}"));
    let spec = TableSpec { schema: s.clone(), name: "t".into(), cols: vec![ColDef { auto_increment: true, nullable: false, ..col("id", "integer") }, ColDef { default: Dv::Value("a'b".into()), ..col("v", "text") }], pk: vec!["id".into()], comment: "c".into(), ..Default::default() };
    for round in 0..3 {
        let t = made(&l, &db, spec.clone());
        t.res.clone().unwrap_or_else(|x| panic!("round {round}: {x}"));
        let q = format!("{}.t", ident(&s));
        assert_eq!(l.qone(&pool, &format!("SELECT count(*)::text FROM {q}")), "0");
        let u = undo_of(&l, t.undo.unwrap());
        u.res.clone().unwrap_or_else(|x| panic!("round {round} undo: {x}"));
        assert_eq!(l.qone(&pool, &format!("SELECT (to_regclass({}) IS NULL)::text", lit(&q))), "true", "round {round}");
    }
    let t = made(&l, &db, spec);
    t.res.unwrap();
    l.qsql(&pool, &format!("INSERT INTO {}.t (v) VALUES (DEFAULT)", ident(&s)));
    assert!(err(&undo_of(&l, t.undo.unwrap())).contains("has rows"));
    let undo = undo_of(&l, a.undo.unwrap());
    assert!(err(&undo).contains("has objects"), "{:?}", undo.res);
    l.w.pg_drop_schema(1, db.clone(), s.clone(), true, false);
    ans(&l).res.unwrap_or_else(|x| panic!("drop schema: {x}"));

    // Databases: each is one statement, which a pooler passes to a backend as it is. Whatever the pooler does with the connection it
    // holds to the new database, a drop that Quarry cannot make succeed is refused with the server's own reason and nothing is claimed.
    let name = tidy.db("pgb");
    let c = make_db(&l, &name, None);
    c.res.clone().unwrap_or_else(|x| panic!("create through the pooler: {x}"));
    assert!(exists(&l, &name));
    // Quarry reads from it through the pooler, so the pooler holds a server connection on it that Quarry cannot close.
    l.w.sql_schemas(1, l.db.clone(), name.clone());
    let Msg::SqlSchemas { res, .. } = l.next() else { panic!("expected SqlSchemas") };
    res.unwrap();
    // Quarry closes its own connections first, but the pooler keeps a server connection open on the database for its idle timeout, so the
    // plain drop is the server's refusal, which says so and names the way out. Nothing is claimed that did not happen.
    let d = drop_db(&l, &name, false);
    let why = err(&d);
    assert!(exists(&l, &name), "a refused drop dropped nothing: {why}");
    assert!(why.contains("55006") && why.contains("pooler") && why.contains("FORCE"), "the refusal is the server's and names the pooler and the way out: {why}");
    // WITH (FORCE) ends the pooler's own server connection, so it works where the plain drop cannot.
    let d = drop_db(&l, &name, true);
    d.res.clone().unwrap_or_else(|x| panic!("FORCE through the pooler: {x}"));
    assert!(!exists(&l, &name));
}

/// A connection taken out of a pool for good (a query's, a read's) is not tracked by the pool's close, so closing a database has to
/// wait for it by itself: the drop that follows is refused while any session of Quarry's is still on the database.
#[test]
fn closing_a_database_waits_for_a_connection_that_a_query_still_holds() {
    every_pg(|l, var| {
        let tidy = Tidy::new(l);
        let name = tidy.db("held");
        make_db(l, &name, None).res.unwrap();
        let at = l.db.at(&name);
        let crate::db::sql::Pool::Pg(pools) = &l.db.pool else { panic!("not PostgreSQL") };
        let held = l.w.rt.block_on(async { at.acquire_conn().await.unwrap() });
        assert!(sessions(l, &name) > 0, "{var}: the held connection is a session on the database");
        let waited = l.w.rt.block_on(async {
            let release = tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(700)).await;
                let _ = held.close().await;
            });
            let t = Instant::now();
            pools.close_database(&name).await;
            let waited = t.elapsed();
            release.await.unwrap();
            waited
        });
        assert!(waited >= std::time::Duration::from_millis(600), "{var}: closing the database waited for the held connection, took {waited:?}");
        // With the connection gone the plain drop works at the first try.
        let d = drop_db(l, &name, false);
        d.res.unwrap_or_else(|x| panic!("{var}: {x}"));
    });
}

/// One value from another database of the server, outside Quarry's pools.
fn one_in(l: &LivePg, name: &str, sql: &str) -> String {
    use sqlx::Connection;
    let o = sqlx::postgres::PgConnectOptions::from_str(&l.uri).unwrap().database(name);
    l.w.rt.block_on(async {
        let mut c = sqlx::PgConnection::connect_with(&o).await.unwrap();
        let rows = sqlx::raw_sql(sql).fetch_all(&mut c).await.unwrap_or_else(|x| panic!("{sql}: {x}"));
        let v = crate::db::sql::worker::text_at(&rows[0], 0).unwrap_or_default();
        c.close().await.unwrap();
        v
    })
}

#[test]
fn an_undo_does_not_reach_a_database_dropped_and_made_again_under_its_name() {
    use crate::db::sql::value::Kind;
    use crate::db::sql::Col;
    use mongodb::bson::Bson;
    every_pg(|l, var| {
        let tidy = Tidy::new(l);
        let name = tidy.db("again");
        make_db(l, &name, None).res.unwrap();
        let shape = "CREATE TABLE public.orders (id int PRIMARY KEY, note text)";
        in_db(l, &name, shape);
        let at = l.db.at(&name);
        let cols = vec![Col::plain("id", "integer", Kind::Int, false), Col::plain("note", "text", Kind::Text, true)];
        let row = vec![("id".to_string(), Bson::Int64(100)), ("note".to_string(), Bson::String("x".into()))];
        l.w.sql_insert(1, at.clone(), "public".into(), "orders".into(), cols, vec!["id".into()], vec![row], false);
        let Msg::Written { res, undo, .. } = l.next() else { panic!("expected Written") };
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        let undo = undo.expect("an undo");
        // The database goes and comes back with the same table and the same row, which is what a DROP-first dump does.
        drop_db(l, &name, true).res.unwrap_or_else(|x| panic!("{var}: {x}"));
        make_db(l, &name, None).res.unwrap();
        in_db(l, &name, &format!("{shape}; INSERT INTO public.orders VALUES (100, 'x')"));
        l.w.sql_undo(at, undo, vec!["id".into()], false);
        let Msg::Written { res, undo, .. } = l.next() else { panic!("expected Written") };
        let why = res.unwrap_err();
        assert!(why.contains("dropped and made again") && undo.is_none(), "{var}: {why}");
        assert_eq!(one_in(l, &name, "SELECT count(*)::text FROM public.orders"), "1", "{var}: the row of the new database is still there");
    });
}

#[test]
fn a_drop_does_not_go_on_while_quarrys_own_connection_holds_the_database() {
    every_pg(|l, var| {
        let tidy = Tidy::new(l);
        let name = tidy.db("held");
        make_db(l, &name, None).res.unwrap();
        // What a running import, script or shell holds: a connection taken out of its pool for good.
        let held = l.w.rt.block_on(l.db.at(&name).acquire_conn()).unwrap();
        for force in [false, true] {
            let why = err(&drop_db(l, &name, force));
            assert!(why.contains("Quarry itself") && why.contains("Nothing was dropped") && !why.contains("Another session"), "{var}: force {force}: {why}");
            assert!(exists(l, &name), "{var}: force {force}: its own job was not killed");
        }
        drop(held);
        drop_db(l, &name, false).res.unwrap_or_else(|x| panic!("{var}: {x}"));
    });
}

#[test]
fn what_a_drop_takes_is_not_read_through_a_locked_connection() {
    every_pg(|l, var| {
        let tidy = Tidy::new(l);
        let name = tidy.db("locked");
        make_db(l, &name, None).res.unwrap();
        in_db(l, &name, "CREATE TABLE public.t (id int)");
        l.w.pg_drop_info(1, l.db.clone(), name.clone(), "public".into(), "t".into(), true);
        let Msg::SqlDropInfo { res, .. } = l.next() else { panic!("expected SqlDropInfo") };
        let why = res.unwrap_err();
        assert!(why.contains("locked"), "{var}: a rolled back DROP is still a write: {why}");
    });
}
