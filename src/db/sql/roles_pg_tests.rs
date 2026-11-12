//! Live tests of PostgreSQL roles and privileges on PostgreSQL 18 and 13 (QUARRY_TEST_POSTGRES, QUARRY_TEST_POSTGRES13) and
//! behind PgBouncer (QUARRY_TEST_PGBOUNCER): the reads, each write and its undo, a change made meanwhile stopping an apply or
//! an undo, CREATE ROLE and its undo, DROP ROLE across two databases, passwords that never reach the log, a login without
//! CREATEROLE, and what each version offers. Every role a test makes has a name of its own and is dropped at the end.

use super::roles_pg::*;
use super::testing::{db_turn, live_pg, live_pg_at, unique_name, LivePg, PgFixture, PGBOUNCER_ENV, PG_ENVS};
use super::Db;
use crate::db::{Msg, Res, UndoAction};

/// Each PostgreSQL, and the PgBouncer when `pooler` is set.
fn each(pooler: bool, f: impl Fn(&LivePg, &str)) {
    for var in PG_ENVS {
        if let Some(l) = live_pg(var) {
            f(&l, var);
        }
    }
    if pooler {
        if let Some(l) = live_pg(PGBOUNCER_ENV) {
            f(&l, PGBOUNCER_ENV);
        }
    }
}

/// Drops the roles a test made, with whatever they own or hold in the test database, when the test ends.
struct Roles<'a> {
    l: &'a LivePg,
    names: Vec<String>,
}

impl<'a> Roles<'a> {
    fn new(l: &'a LivePg, tags: &[&str]) -> Self {
        Roles { l, names: tags.iter().map(|t| unique_name(&format!("qr_{t}"))).collect() }
    }

    fn get(&self, i: usize) -> &str {
        &self.names[i]
    }
}

impl Drop for Roles<'_> {
    fn drop(&mut self) {
        for n in &self.names {
            let q = format!("\"{n}\"");
            for pool in [self.l.fixture_pool(), self.l.pool.clone()] {
                let _ = self.l.w.rt.block_on(async { sqlx::raw_sql(&format!("DO $$ BEGIN IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = '{n}') THEN EXECUTE 'DROP OWNED BY {q}'; END IF; END $$")).execute(&pool).await });
            }
            let _ = self.l.w.rt.block_on(async { sqlx::raw_sql(&format!("DROP ROLE IF EXISTS {q}")).execute(&self.l.pool).await });
        }
    }
}

fn answer(l: &LivePg) -> Answer {
    loop {
        if let Msg::PgRoles { ans, .. } = l.next() {
            return ans;
        }
    }
}

fn cluster_of(l: &LivePg, db: &Db) -> Cluster {
    l.w.pg_roles_cluster(1, 1, db.clone());
    match answer(l) {
        Answer::Cluster(r) => r.unwrap(),
        _ => panic!("not a cluster answer"),
    }
}

fn cluster(l: &LivePg) -> Cluster {
    cluster_of(l, &l.db)
}

fn objects(l: &LivePg, database: &str, schema: &str, kind: Kind) -> Objects {
    l.w.pg_roles_objects(1, 1, l.db.clone(), database.into(), schema.into(), kind);
    match answer(l) {
        Answer::Objects(r) => r.unwrap(),
        _ => panic!("not an objects answer"),
    }
}

fn defaults(l: &LivePg, database: &str) -> Defaults {
    l.w.pg_roles_defaults(1, 1, l.db.clone(), database.into());
    match answer(l) {
        Answer::Defaults(r) => r.unwrap(),
        _ => panic!("not a defaults answer"),
    }
}

fn done(l: &LivePg) -> (Did, Res<String>, Option<UndoAction>) {
    match answer(l) {
        Answer::Done { did, res, undo } => (did, res, undo),
        _ => panic!("not a done answer"),
    }
}

fn apply(l: &LivePg, b: Batch) -> (Res<String>, Option<UndoAction>) {
    l.w.pg_roles_apply(1, 1, l.db.clone(), b, false);
    let (_, res, undo) = done(l);
    (res, undo)
}

fn undo(l: &LivePg, a: UndoAction) -> (Res<String>, Option<UndoAction>) {
    let database = match &a.kind {
        crate::db::UndoKind::Sql { database, .. } => database.clone(),
        _ => String::new(),
    };
    l.w.sql_undo(l.db.at(&database), a, Vec::new(), false);
    let (_, res, undo) = done(l);
    (res, undo)
}

/// The oids of every role, for a batch.
fn oids(c: &Cluster) -> Vec<(String, String)> {
    c.roles.iter().map(|r| (r.name.clone(), r.oid.clone())).collect()
}

fn obj<'a>(o: &'a Objects, name: &str) -> &'a Obj {
    o.objs.iter().find(|x| x.name == name).unwrap_or_else(|| panic!("{name} not listed"))
}

fn privs(p: &[(&str, bool)]) -> Privs {
    p.iter().map(|(k, g)| (k.to_string(), *g)).collect()
}

/// The table's ACL as the server's own text writes it, NULL read as the owner's defaults: a grant and its revoke leave an explicit
/// ACL where there was none, which grants the same.
fn acl(f: &PgFixture, table: &str) -> String {
    f.one(&format!("SELECT array_to_string(array(SELECT x::text FROM unnest(COALESCE(relacl, acldefault('r', relowner))) x ORDER BY 1), ',') FROM pg_class WHERE oid = '\"{}\".{table}'::regclass", f.schema))
}

fn batch(database: &str, o: &Objects, c: &Cluster, changes: Vec<Change>) -> Batch {
    Batch { database: database.into(), db_oid: o.db_oid.clone(), changes, roles: oids(c), ..Default::default() }
}

#[test]
fn live_reads_acls_inherited_privileges_defaults_and_memberships() {
    each(true, |l, var| {
        let r = Roles::new(l, &["grp", "mem"]);
        let (g, m) = (r.get(0), r.get(1));
        let f = PgFixture::new(l, &unique_name("qr_read"), "CREATE TABLE t (a int); CREATE SEQUENCE s; CREATE FUNCTION f(x integer) RETURNS int LANGUAGE sql AS 'SELECT 1';");
        let s = f.schema.clone();
        f.sql(&format!(
            "CREATE ROLE \"{g}\"; CREATE ROLE \"{m}\" LOGIN; GRANT \"{g}\" TO \"{m}\"; GRANT SELECT ON t TO \"{g}\"; GRANT INSERT ON t TO \"{m}\" WITH GRANT OPTION; \
             GRANT USAGE ON SEQUENCE s TO \"{m}\"; REVOKE EXECUTE ON FUNCTION f(integer) FROM PUBLIC; GRANT EXECUTE ON FUNCTION f(integer) TO \"{g}\"; GRANT USAGE ON SCHEMA \"{s}\" TO \"{m}\"; \
             ALTER DEFAULT PRIVILEGES IN SCHEMA \"{s}\" GRANT SELECT ON TABLES TO \"{m}\""
        ));
        let c = cluster(l);
        assert!(c.databases.iter().any(|d| d == "quarry_test"), "{var}");
        let mem = c.member_of(m);
        assert_eq!(mem.len(), 1, "{var}");
        assert_eq!(mem[0].group, g, "{var}");
        if c.major >= 16 {
            assert_eq!(mem[0].opts.inherit, Some(true), "{var}");
            assert_eq!(mem[0].opts.set, Some(true), "{var}");
        } else {
            assert_eq!(mem[0].opts.inherit, None, "{var}");
        }
        assert_eq!(c.chain(m), [m.to_string(), g.to_string(), String::new()], "{var}");
        let tables = objects(l, "quarry_test", &s, Kind::Tables);
        let t = obj(&tables, "t");
        assert_eq!(t.direct(m), privs(&[("INSERT", true)]), "{var}");
        let cl = cell(t, &c.chain(m));
        assert_eq!(cl.via.get("SELECT").map(String::as_str), Some(g), "{var}: inherited through the group");
        assert!(cell(t, &c.chain("postgres")).owner || t.owner != "postgres", "{var}");
        let seqs = objects(l, "quarry_test", &s, Kind::Sequences);
        assert_eq!(obj(&seqs, "s").direct(m), privs(&[("USAGE", false)]), "{var}");
        let funcs = objects(l, "quarry_test", &s, Kind::Functions);
        let func = obj(&funcs, "f");
        assert_eq!(func.sig, "x integer", "{var}");
        assert!(func.direct("").is_empty(), "{var}: PUBLIC lost EXECUTE");
        assert_eq!(func.direct(g), privs(&[("EXECUTE", false)]), "{var}");
        let schemas = objects(l, "quarry_test", "", Kind::Schemas);
        assert_eq!(obj(&schemas, &s).direct(m), privs(&[("USAGE", false)]), "{var}");
        let dbs = objects(l, "quarry_test", "", Kind::Databases);
        // A database nobody granted on keeps the built-in CONNECT and TEMPORARY for PUBLIC.
        assert!(obj(&dbs, "quarry_test").direct("").contains_key("CONNECT") || !obj(&dbs, "quarry_test").acl.is_empty(), "{var}");
        let d = defaults(l, "quarry_test");
        let row = d.rows.iter().find(|r| r.schema == s && r.kind == DefKind::Tables).unwrap_or_else(|| panic!("{var}: no default row"));
        assert_eq!(row.granted(m), privs(&[("SELECT", false)]), "{var}");
        if c.major >= 16 {
            // A membership that does not inherit passes nothing on.
            f.sql(&format!("GRANT \"{g}\" TO \"{m}\" WITH INHERIT FALSE"));
            let c = cluster(l);
            assert_eq!(c.member_of(m)[0].opts.inherit, Some(false), "{var}");
            assert_eq!(c.chain(m), [m.to_string(), String::new()], "{var}");
        } else {
            f.sql(&format!("ALTER ROLE \"{m}\" NOINHERIT"));
            assert_eq!(cluster(l).chain(m), [m.to_string(), String::new()], "{var}");
        }
    });
}

#[test]
fn live_each_write_and_its_undo() {
    each(true, |l, var| {
        let r = Roles::new(l, &["grp", "mem"]);
        let (g, m) = (r.get(0), r.get(1));
        let f = PgFixture::new(l, &unique_name("qr_write"), "CREATE TABLE t (a int);");
        let s = f.schema.clone();
        f.sql(&format!("CREATE ROLE \"{g}\"; CREATE ROLE \"{m}\" LOGIN"));
        let c = cluster(l);
        let o = objects(l, "quarry_test", &s, Kind::Tables);
        let before_acl = acl(&f, "t");
        let attrs = c.role(m).unwrap().attrs.clone();
        let to_attrs = Attrs { createdb: true, conn_limit: 5, valid_until: "2030-01-02 03:04:05+00".into(), ..attrs.clone() };
        let changes = vec![
            Change::Acl { target: obj(&o, "t").target(Kind::Tables), grantee: m.into(), from: Privs::new(), to: privs(&[("SELECT", false), ("UPDATE", true)]) },
            Change::Attrs { role: m.into(), from: attrs.clone(), to: to_attrs },
            Change::Member { group: g.into(), member: m.into(), from: None, to: Some(Opts { admin: true, ..Opts::new(c.major, true) }) },
            Change::Default { owner: "postgres".into(), schema: s.clone(), kind: DefKind::Sequences, grantee: m.into(), from: Privs::new(), to: privs(&[("USAGE", false)]) },
        ];
        let (res, u) = apply(l, batch("quarry_test", &o, &c, changes));
        res.unwrap_or_else(|e| panic!("{var}: {e}"));
        assert!(acl(&f, "t").contains(&format!("{m}=r*w/")) || acl(&f, "t").contains(&format!("{m}=w*r/")) || acl(&f, "t").contains(&format!("\"{m}\"=")) || acl(&f, "t").contains(m), "{var}: {}", acl(&f, "t"));
        let c2 = cluster(l);
        let a2 = &c2.role(m).unwrap().attrs;
        assert!(a2.createdb && a2.conn_limit == 5 && a2.valid_until == "2030-01-02 03:04:05+00", "{var}: {a2:?}");
        assert!(c2.membership(g, m).is_some_and(|o| o.admin), "{var}");
        let d = defaults(l, "quarry_test");
        assert!(d.rows.iter().any(|r| r.schema == s && r.kind == DefKind::Sequences && r.granted(m) == privs(&[("USAGE", false)])), "{var}");
        let (res, back) = undo(l, u.expect("an undo entry"));
        res.unwrap_or_else(|e| panic!("{var}: {e}"));
        assert!(back.is_none(), "{var}");
        assert_eq!(acl(&f, "t"), before_acl, "{var}");
        let c3 = cluster(l);
        assert_eq!(c3.role(m).unwrap().attrs, attrs, "{var}");
        assert!(c3.membership(g, m).is_none(), "{var}");
        assert!(!defaults(l, "quarry_test").rows.iter().any(|r| r.schema == s), "{var}");
        // A grant option taken away and given back, and a privilege removed, round trip exactly.
        f.sql(&format!("GRANT SELECT ON t TO \"{m}\" WITH GRANT OPTION; GRANT DELETE ON t TO \"{m}\""));
        let (c, o) = (cluster(l), objects(l, "quarry_test", &s, Kind::Tables));
        let before_acl = acl(&f, "t");
        let ch = Change::Acl { target: obj(&o, "t").target(Kind::Tables), grantee: m.into(), from: privs(&[("SELECT", true), ("DELETE", false)]), to: privs(&[("SELECT", false)]) };
        let (res, u) = apply(l, batch("quarry_test", &o, &c, vec![ch]));
        res.unwrap_or_else(|e| panic!("{var}: {e}"));
        assert_eq!(objects(l, "quarry_test", &s, Kind::Tables).objs[0].direct(m), privs(&[("SELECT", false)]), "{var}");
        undo(l, u.unwrap()).0.unwrap_or_else(|e| panic!("{var}: {e}"));
        assert_eq!(acl(&f, "t"), before_acl, "{var}");
        // The lock refuses every write before anything runs.
        l.w.pg_roles_apply(1, 1, l.db.clone(), batch("quarry_test", &o, &c, vec![Change::Attrs { role: m.into(), from: attrs.clone(), to: Attrs { createdb: true, ..attrs.clone() } }]), true);
        assert!(done(l).1.unwrap_err().contains("locked"), "{var}");
    });
}

#[test]
fn live_a_change_made_meanwhile_stops_the_apply() {
    each(true, |l, var| {
        let r = Roles::new(l, &["mem"]);
        let m = r.get(0);
        let f = PgFixture::new(l, &unique_name("qr_race"), "CREATE TABLE t (a int);");
        f.sql(&format!("CREATE ROLE \"{m}\""));
        let (c, o) = (cluster(l), objects(l, "quarry_test", &f.schema, Kind::Tables));
        let attrs = c.role(m).unwrap().attrs.clone();
        // Someone else grants DELETE after the window read the table.
        f.sql(&format!("GRANT DELETE ON t TO \"{m}\""));
        let changes = vec![
            Change::Attrs { role: m.into(), from: attrs.clone(), to: Attrs { createdb: true, ..attrs.clone() } },
            Change::Acl { target: obj(&o, "t").target(Kind::Tables), grantee: m.into(), from: Privs::new(), to: privs(&[("SELECT", false)]) },
        ];
        let err = apply(l, batch("quarry_test", &o, &c, changes)).0.unwrap_err();
        assert!(err.contains("changed after the window read it") && err.contains("DELETE"), "{var}: {err}");
        assert!(!cluster(l).role(m).unwrap().attrs.createdb, "{var}: the other change of the batch ran");
        // A table dropped and made again under its name is not the one the window read.
        f.sql("DROP TABLE t; CREATE TABLE t (a int)");
        let ch = Change::Acl { target: obj(&o, "t").target(Kind::Tables), grantee: m.into(), from: Privs::new(), to: privs(&[("SELECT", false)]) };
        let err = apply(l, batch("quarry_test", &o, &c, vec![ch])).0.unwrap_err();
        assert!(err.contains("dropped, renamed or made again"), "{var}: {err}");
        // An attribute changed meanwhile stops a change of the same attribute.
        f.sql(&format!("ALTER ROLE \"{m}\" CONNECTION LIMIT 7"));
        let ch = Change::Attrs { role: m.into(), from: attrs.clone(), to: Attrs { conn_limit: 3, ..attrs.clone() } };
        let err = apply(l, batch("quarry_test", &o, &c, vec![ch])).0.unwrap_err();
        assert!(err.contains("connection limit 7"), "{var}: {err}");
    });
}

#[test]
fn live_an_undo_is_refused_after_someone_changed_the_object() {
    each(true, |l, var| {
        let r = Roles::new(l, &["mem", "grp"]);
        let (m, g) = (r.get(0), r.get(1));
        let f = PgFixture::new(l, &unique_name("qr_undo"), "CREATE TABLE t (a int);");
        f.sql(&format!("CREATE ROLE \"{m}\"; CREATE ROLE \"{g}\""));
        let (c, o) = (cluster(l), objects(l, "quarry_test", &f.schema, Kind::Tables));
        let ch = Change::Acl { target: obj(&o, "t").target(Kind::Tables), grantee: m.into(), from: Privs::new(), to: privs(&[("SELECT", false)]) };
        let (res, u) = apply(l, batch("quarry_test", &o, &c, vec![ch]));
        res.unwrap_or_else(|e| panic!("{var}: {e}"));
        f.sql(&format!("GRANT INSERT ON t TO \"{m}\""));
        let (res, back) = undo(l, u.unwrap());
        let err = res.unwrap_err();
        assert!(err.starts_with("Undo refused") && err.contains("after this change ran"), "{var}: {err}");
        assert!(acl(&f, "t").contains(&format!("{m}=ar/")) || acl(&f, "t").contains(&format!("{m}=ra/")) || acl(&f, "t").contains(m), "{var}: nothing was revoked");
        // The refused entry comes back to stay in the list, and works once the state is back.
        f.sql(&format!("REVOKE INSERT ON t FROM \"{m}\""));
        undo(l, back.expect("the entry back")).0.unwrap_or_else(|e| panic!("{var}: {e}"));
        // A membership changed after it was added refuses its undo too.
        let c = cluster(l);
        let ch = Change::Member { group: g.into(), member: m.into(), from: None, to: Some(Opts::new(c.major, true)) };
        let (res, u) = apply(l, batch("quarry_test", &o, &c, vec![ch]));
        res.unwrap_or_else(|e| panic!("{var}: {e}"));
        f.sql(&if c.major >= 16 { format!("GRANT \"{g}\" TO \"{m}\" WITH ADMIN TRUE") } else { format!("GRANT \"{g}\" TO \"{m}\" WITH ADMIN OPTION") });
        let err = undo(l, u.unwrap()).0.unwrap_err();
        assert!(err.starts_with("Undo refused"), "{var}: {err}");
        assert!(cluster(l).membership(g, m).is_some(), "{var}");
    });
}

#[test]
fn live_a_revoke_the_server_skips_rolls_the_batch_back() {
    each(false, |l, var| {
        let r = Roles::new(l, &["own", "mem"]);
        let (own, m) = (r.get(0), r.get(1));
        let f = PgFixture::new(l, &unique_name("qr_grantor"), "CREATE TABLE t (a int);");
        // own holds SELECT with grant option and grants it on; postgres revoking it as the owner does not touch own's grant.
        f.sql(&format!("CREATE ROLE \"{own}\"; CREATE ROLE \"{m}\"; GRANT SELECT ON t TO \"{own}\" WITH GRANT OPTION; GRANT USAGE ON SCHEMA \"{}\" TO \"{own}\"; SET ROLE \"{own}\"; GRANT SELECT ON t TO \"{m}\"; RESET ROLE", f.schema));
        let (c, o) = (cluster(l), objects(l, "quarry_test", &f.schema, Kind::Tables));
        let ch = Change::Acl { target: obj(&o, "t").target(Kind::Tables), grantee: m.into(), from: privs(&[("SELECT", false)]), to: Privs::new() };
        let before = acl(&f, "t");
        let err = apply(l, batch("quarry_test", &o, &c, vec![ch])).0.unwrap_err();
        assert!(err.contains("another role"), "{var}: {err}");
        assert_eq!(acl(&f, "t"), before, "{var}");
    });
}

#[test]
fn live_create_role_and_its_undo_only_while_it_owns_nothing() {
    each(true, |l, var| {
        let r = Roles::new(l, &["new", "grp", "owns"]);
        let (n, g, o) = (r.get(0), r.get(1), r.get(2));
        l.w.rt.block_on(async { sqlx::raw_sql(&format!("CREATE ROLE \"{g}\"")).execute(&l.pool).await.unwrap() });
        let spec = NewRole { name: n.into(), attrs: Attrs { login: true, conn_limit: 3, ..Default::default() }, groups: vec![g.into()] };
        l.w.pg_role_create(1, l.db.clone(), spec, "Zq9'pw".into(), false);
        let (_, res, u) = done(l);
        res.unwrap_or_else(|e| panic!("{var}: {e}"));
        let c = cluster(l);
        let role = c.role(n).unwrap_or_else(|| panic!("{var}: not made"));
        assert!(role.attrs.login && role.attrs.conn_limit == 3 && c.membership(g, n).is_some(), "{var}");
        undo(l, u.unwrap()).0.unwrap_or_else(|e| panic!("{var}: {e}"));
        assert!(cluster(l).role(n).is_none(), "{var}: undo dropped it");
        // Once it owns something the undo leaves it.
        l.w.pg_role_create(1, l.db.clone(), NewRole { name: o.into(), ..Default::default() }, String::new(), false);
        let (_, res, u) = done(l);
        res.unwrap_or_else(|e| panic!("{var}: {e}"));
        let f = PgFixture::new(l, &unique_name("qr_owns"), "CREATE TABLE t (a int);");
        f.sql(&format!("ALTER TABLE t OWNER TO \"{o}\""));
        let (res, back) = undo(l, u.unwrap());
        assert!(res.unwrap_err().contains("owns objects"), "{var}");
        assert!(back.is_some() && cluster(l).role(o).is_some(), "{var}");
        // A name taken is refused by the server with nothing made.
        l.w.pg_role_create(1, l.db.clone(), NewRole { name: g.into(), ..Default::default() }, String::new(), false);
        assert!(done(l).1.unwrap_err().contains("already exists"), "{var}");
    });
}

/// A second database with a table owned by `role`, dropped when the test ends.
struct OtherDb<'a> {
    l: &'a LivePg,
    name: String,
}

impl<'a> OtherDb<'a> {
    fn new(l: &'a LivePg) -> Self {
        let name = unique_name("qr_db");
        let _turn = db_turn();
        l.w.rt.block_on(async { sqlx::raw_sql(&format!("CREATE DATABASE \"{name}\"")).execute(&l.pool).await.unwrap() });
        OtherDb { l, name }
    }

    fn sql(&self, s: &str) {
        use std::str::FromStr;
        let o = sqlx::postgres::PgConnectOptions::from_str(&self.l.uri).unwrap().database(&self.name);
        self.l.w.rt.block_on(async {
            let pool = sqlx::postgres::PgPoolOptions::new().max_connections(1).connect_with(o).await.unwrap();
            sqlx::raw_sql(s).execute(&pool).await.unwrap_or_else(|x| panic!("{s}: {x}"));
            pool.close().await;
        });
    }

    fn one(&self, s: &str) -> String {
        use std::str::FromStr;
        let o = sqlx::postgres::PgConnectOptions::from_str(&self.l.uri).unwrap().database(&self.name);
        self.l.w.rt.block_on(async {
            let pool = sqlx::postgres::PgPoolOptions::new().max_connections(1).connect_with(o).await.unwrap();
            let rows = sqlx::raw_sql(s).fetch_all(&pool).await.unwrap();
            pool.close().await;
            super::worker::text_at(&rows[0], 0).unwrap_or_default()
        })
    }
}

impl Drop for OtherDb<'_> {
    fn drop(&mut self) {
        let _turn = db_turn();
        let _ = self.l.w.rt.block_on(async { sqlx::raw_sql(&format!("DROP DATABASE IF EXISTS \"{}\" WITH (FORCE)", self.name)).execute(&self.l.pool).await });
    }
}

fn drop_info(l: &LivePg, role: &str) -> DropInfo {
    l.w.pg_role_drop_info(1, 1, l.db.clone(), role.into());
    match answer(l) {
        Answer::DropInfo(r) => r.unwrap(),
        _ => panic!("not a drop info answer"),
    }
}

#[test]
fn live_drop_role_with_reassign_owned_and_with_drop_owned_across_two_databases() {
    each(true, |l, var| {
        let r = Roles::new(l, &["heir", "jane", "bob"]);
        let (heir, jane, bob) = (r.get(0), r.get(1), r.get(2));
        let other = OtherDb::new(l);
        let f = PgFixture::new(l, &unique_name("qr_drop"), "CREATE TABLE keep (a int);");
        f.sql(&format!("CREATE ROLE \"{heir}\"; CREATE ROLE \"{jane}\"; CREATE ROLE \"{bob}\"; CREATE TABLE jt (a int); ALTER TABLE jt OWNER TO \"{jane}\"; GRANT SELECT ON keep TO \"{jane}\"; CREATE TABLE bt (a int); ALTER TABLE bt OWNER TO \"{bob}\""));
        other.sql(&format!("CREATE TABLE jt2 (a int); ALTER TABLE jt2 OWNER TO \"{jane}\"; CREATE TABLE bt2 (a int); ALTER TABLE bt2 OWNER TO \"{bob}\"; GRANT SELECT ON bt2 TO \"{jane}\""));
        let info = drop_info(l, jane);
        let names: Vec<&str> = info.dbs.iter().map(|d| d.database.as_str()).collect();
        assert!(names.contains(&"quarry_test") && names.contains(&other.name.as_str()), "{var}: {names:?}");
        assert_eq!(info.owned_count(), 2, "{var}: {info:?}");
        assert!(info.dbs.iter().find(|d| d.database == "quarry_test").unwrap().owned.iter().any(|o| o.contains("jt")), "{var}");
        let plan = drop_plan(&info, &DropMode::Reassign(heir.into()), &l.db.pg_database()).unwrap();
        assert!(drop_preview(&plan).contains("REASSIGN OWNED BY"), "{var}");
        l.w.pg_role_drop(1, l.db.clone(), info.clone(), DropMode::Reassign(heir.into()), false);
        done(l).1.unwrap_or_else(|e| panic!("{var}: {e}"));
        assert!(cluster(l).role(jane).is_none(), "{var}: jane is gone");
        assert_eq!(f.one(&format!("SELECT pg_get_userbyid(relowner) FROM pg_class WHERE oid = '\"{}\".jt'::regclass", f.schema)), *heir, "{var}");
        assert_eq!(other.one("SELECT pg_get_userbyid(relowner) FROM pg_class WHERE relname = 'jt2'"), *heir, "{var}");
        // DROP OWNED takes bob's tables with him, in both databases.
        let info = drop_info(l, bob);
        assert_eq!(info.dbs.len(), 2, "{var}: {info:?}");
        // Something added after the window read it stops the drop where it was found.
        f.sql(&format!("CREATE TABLE bt3 (a int); ALTER TABLE bt3 OWNER TO \"{bob}\""));
        l.w.pg_role_drop(1, l.db.clone(), info, DropMode::DropOwned, false);
        let err = done(l).1.unwrap_err();
        assert!(err.contains("Look again"), "{var}: {err}");
        let info = drop_info(l, bob);
        l.w.pg_role_drop(1, l.db.clone(), info, DropMode::DropOwned, false);
        done(l).1.unwrap_or_else(|e| panic!("{var}: {e}"));
        assert!(cluster(l).role(bob).is_none(), "{var}");
        assert_eq!(f.one(&format!("SELECT count(*)::text FROM pg_class WHERE relnamespace = '\"{}\"'::regnamespace AND relname IN ('bt', 'bt3')", f.schema)), "0", "{var}");
        assert_eq!(other.one("SELECT count(*)::text FROM pg_class WHERE relname = 'bt2'"), "0", "{var}");
    });
}

/// The URI of the server with another login.
fn uri_as(l: &LivePg, user: &str, password: &str) -> String {
    let (scheme, rest) = l.uri.split_once("://").unwrap();
    let host = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    let enc = |s: &str| s.bytes().map(|b| if b.is_ascii_alphanumeric() || b == b'_' { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>();
    format!("{scheme}://{}:{}@{host}", enc(user), enc(password))
}

#[test]
fn live_passwords_sign_in_and_never_reach_the_log_or_an_error() {
    // Through PgBouncer a new login cannot sign in (its user list is fixed), so only the direct servers check the sign in.
    each(false, |l, var| {
        let r = Roles::new(l, &["pw"]);
        let n = r.get(0);
        let (first, second) = ("Zq9'Marker\\x", "Zq9 Märker");
        l.w.pg_role_create(1, l.db.clone(), NewRole { name: n.into(), attrs: Attrs { login: true, ..Default::default() }, groups: vec![] }, first.into(), false);
        done(l).1.unwrap_or_else(|e| panic!("{var}: {e}"));
        let stored = l.one(&format!("SELECT rolpassword FROM pg_authid WHERE rolname = '{n}'"));
        // PostgreSQL 13 keeps MD5 by default, and gets what psql's \password would send it.
        let want = if cluster(l).password_encryption == "md5" { "md5" } else { "SCRAM-SHA-256$4096:" };
        assert!(stored.starts_with(want), "{var}: {stored}");
        let who = live_pg_at(uri_as(l, n, first));
        assert_eq!(who.one("SELECT current_user"), *n, "{var}");
        drop(who);
        let oid = cluster(l).role(n).unwrap().oid.clone();
        l.w.pg_role_password(1, l.db.clone(), n.into(), oid.clone(), second.into(), false);
        let (_, res, u) = done(l);
        res.unwrap_or_else(|e| panic!("{var}: {e}"));
        assert!(u.is_none(), "{var}: a password change has no undo");
        // A non-ASCII password goes through SASLprep the way libpq does. It is already normalized here, because sqlx, which signs in
        // below, leaves the password as it is where psql would normalize it.
        let who = live_pg_at(uri_as(l, n, second));
        assert_eq!(who.one("SELECT current_user"), *n, "{var}");
        drop(who);
        // A failing change keeps the password out of the error.
        l.w.pg_role_password(1, l.db.clone(), "qr_no_such_role".into(), oid, second.into(), false);
        let err = done(l).1.unwrap_err();
        assert!(!err.contains("Marker") && !err.contains("Märker"), "{var}: {err}");
        let log = crate::db::LOG.lock().unwrap();
        let mine: Vec<String> = log.iter().filter(|e| e.conn == l.db.conn && e.cmd.contains(n)).map(|e| format!("{} {}", e.cmd, e.err.clone().unwrap_or_default())).collect();
        assert!(mine.iter().any(|c| c.starts_with("CREATE ROLE") && c.contains("PASSWORD '***'")), "{var}: {mine:?}");
        assert!(mine.iter().any(|c| c.starts_with("ALTER ROLE") && c.contains("PASSWORD '***'")), "{var}: {mine:?}");
        assert!(mine.iter().all(|c| !c.contains("Zq9") && !c.contains("SCRAM-SHA-256$")), "{var}: {mine:?}");
    });
}

#[test]
fn live_a_role_without_createrole_reads_but_cannot_change() {
    each(false, |l, var| {
        let r = Roles::new(l, &["weak", "other"]);
        let (weak, other) = (r.get(0), r.get(1));
        let pw = "weak'pw";
        l.w.rt.block_on(async { sqlx::raw_sql(&format!("CREATE ROLE \"{weak}\" LOGIN PASSWORD 'weak''pw'; CREATE ROLE \"{other}\"")).execute(&l.pool).await.unwrap() });
        let w = live_pg_at(uri_as(l, weak, pw));
        let c = cluster(&w);
        assert!(c.read_only_reason().is_some_and(|why| why.contains(weak)), "{var}");
        assert!(c.role(other).is_some_and(|o| c.may_alter(o).is_err()), "{var}");
        // The server agrees: an ALTER ROLE from this login is refused and leaves the role as it was.
        let a = c.role(other).unwrap().attrs.clone();
        let b = Batch { database: String::new(), db_oid: String::new(), changes: vec![Change::Attrs { role: other.into(), from: a.clone(), to: Attrs { createdb: true, ..a.clone() } }], roles: oids(&c), ..Default::default() };
        w.w.pg_roles_apply(1, 1, w.db.clone(), b, false);
        let err = done(&w).1.unwrap_err();
        assert!(err.to_lowercase().contains("permission denied"), "{var}: {err}");
        assert!(!cluster(l).role(other).unwrap().attrs.createdb, "{var}");
    });
}

#[test]
fn live_version_gating_of_maintain_and_membership_options() {
    each(false, |l, var| {
        let r = Roles::new(l, &["mem", "grp"]);
        let (m, g) = (r.get(0), r.get(1));
        let f = PgFixture::new(l, &unique_name("qr_gate"), "CREATE TABLE t (a int);");
        f.sql(&format!("CREATE ROLE \"{m}\"; CREATE ROLE \"{g}\""));
        let (c, o) = (cluster(l), objects(l, "quarry_test", &f.schema, Kind::Tables));
        assert_eq!(Kind::Tables.privs(c.major).contains(&"MAINTAIN"), c.major >= 17, "{var}");
        let maintain = Change::Acl { target: obj(&o, "t").target(Kind::Tables), grantee: m.into(), from: Privs::new(), to: privs(&[("MAINTAIN", false)]) };
        if c.major >= 17 {
            let (res, u) = apply(l, batch("quarry_test", &o, &c, vec![maintain]));
            res.unwrap_or_else(|e| panic!("{var}: {e}"));
            assert!(acl(&f, "t").contains(&format!("{m}=m/")), "{var}: {}", acl(&f, "t"));
            undo(l, u.unwrap()).0.unwrap_or_else(|e| panic!("{var}: {e}"));
        } else {
            assert!(maintain.sql(c.major).is_err(), "{var}");
        }
        let add = Change::Member { group: g.into(), member: m.into(), from: None, to: Some(Opts::new(c.major, true)) };
        let (res, _) = apply(l, batch("quarry_test", &o, &c, vec![add]));
        res.unwrap_or_else(|e| panic!("{var}: {e}"));
        let c = cluster(l);
        let now = c.membership(g, m).unwrap();
        if c.major >= 16 {
            assert_eq!(now, Opts { admin: false, inherit: Some(true), set: Some(true) }, "{var}");
            let to = Opts { set: Some(false), inherit: Some(false), ..now };
            let (res, u) = apply(l, batch("quarry_test", &o, &c, vec![Change::Member { group: g.into(), member: m.into(), from: Some(now), to: Some(to) }]));
            res.unwrap_or_else(|e| panic!("{var}: {e}"));
            assert_eq!(cluster(l).membership(g, m), Some(to), "{var}");
            undo(l, u.unwrap()).0.unwrap_or_else(|e| panic!("{var}: {e}"));
            assert_eq!(cluster(l).membership(g, m), Some(now), "{var}");
        } else {
            assert_eq!(now, Opts::default(), "{var}: only ADMIN before 16");
        }
    });
}


/// A login of its own, for the tests that need the signed-in role to be another one than postgres.
fn login(l: &LivePg, name: &str, attrs: &str) -> LivePg {
    let pw = "Zq9login";
    l.w.rt.block_on(async { sqlx::raw_sql(&format!("CREATE ROLE \"{name}\" LOGIN {attrs} PASSWORD '{pw}'")).execute(&l.pool).await.unwrap() });
    live_pg_at(uri_as(l, name, pw))
}

#[test]
fn live_the_signed_in_role_cannot_lock_itself_out() {
    each(false, |l, var| {
        let r = Roles::new(l, &["su"]);
        let su = r.get(0);
        let w = login(l, su, "SUPERUSER");
        let c = cluster(&w);
        assert_eq!(c.me, su, "{var}");
        let a = c.role(su).unwrap().attrs.clone();
        for to in [Attrs { login: false, ..a.clone() }, Attrs { superuser: false, ..a.clone() }, Attrs { valid_until: "2000-01-01 00:00:00+00".into(), ..a.clone() }, Attrs { valid_until: "-infinity".into(), ..a.clone() }] {
            let b = Batch { changes: vec![Change::Attrs { role: su.into(), from: a.clone(), to: to.clone() }], roles: oids(&c), ..Default::default() };
            w.w.pg_roles_apply(1, 1, w.db.clone(), b, false);
            let err = done(&w).1.unwrap_err();
            assert!(err.contains("signed in as") && err.contains("Nothing was changed"), "{var}: {to:?}: {err}");
        }
        assert_eq!(cluster(l).role(su).unwrap().attrs, a, "{var}: nothing ran");
        // Another role loses them as before.
        let other = Roles::new(l, &["other"]);
        l.w.rt.block_on(async { sqlx::raw_sql(&format!("CREATE ROLE \"{}\" LOGIN", other.get(0))).execute(&l.pool).await.unwrap() });
        let c = cluster(&w);
        let oa = c.role(other.get(0)).unwrap().attrs.clone();
        let b = Batch { changes: vec![Change::Attrs { role: other.get(0).into(), from: oa.clone(), to: Attrs { login: false, ..oa } }], roles: oids(&c), ..Default::default() };
        w.w.pg_roles_apply(1, 1, w.db.clone(), b, false);
        done(&w).1.unwrap_or_else(|e| panic!("{var}: {e}"));
    });
}

#[test]
fn live_valid_until_minus_infinity_round_trips_and_fractions_are_refused() {
    each(true, |l, var| {
        let r = Roles::new(l, &["exp"]);
        let n = r.get(0);
        let f = PgFixture::new(l, &unique_name("qr_inf"), "");
        f.sql(&format!("CREATE ROLE \"{n}\" LOGIN VALID UNTIL '-infinity'"));
        let c = cluster(l);
        let a = c.role(n).unwrap().attrs.clone();
        assert_eq!(a.valid_until, "-infinity", "{var}: read as expired, not never");
        let until = || l.one(&format!("SELECT rolvaliduntil::text FROM pg_roles WHERE rolname = '{n}'"));
        let to = Attrs { valid_until: "2031-02-03 04:05:06+00".into(), ..a.clone() };
        let (res, u) = apply(l, Batch { changes: vec![Change::Attrs { role: n.into(), from: a.clone(), to }], roles: oids(&c), ..Default::default() });
        res.unwrap_or_else(|e| panic!("{var}: {e}"));
        undo(l, u.unwrap()).0.unwrap_or_else(|e| panic!("{var}: {e}"));
        assert_eq!(until(), "-infinity", "{var}: the undo wrote back -infinity, not infinity");
        // Typed -infinity goes out as itself and reads back the same.
        f.sql(&format!("ALTER ROLE \"{n}\" VALID UNTIL 'infinity'"));
        let c = cluster(l);
        let a = c.role(n).unwrap().attrs.clone();
        let (res, _) = apply(l, Batch { changes: vec![Change::Attrs { role: n.into(), from: a.clone(), to: Attrs { valid_until: "-infinity".into(), ..a.clone() } }], roles: oids(&c), ..Default::default() });
        res.unwrap_or_else(|e| panic!("{var}: {e}"));
        assert_eq!(until(), "-infinity", "{var}");
        // A fraction of a second would be written back without it, so it is refused.
        let c = cluster(l);
        let a = c.role(n).unwrap().attrs.clone();
        let (res, _) = apply(l, Batch { changes: vec![Change::Attrs { role: n.into(), from: a.clone(), to: Attrs { valid_until: "2031-02-03 04:05:06.5+00".into(), ..a.clone() } }], roles: oids(&c), ..Default::default() });
        assert!(res.unwrap_err().contains("not the same moment"), "{var}");
        assert_eq!(until(), "-infinity", "{var}");
    });
}

#[test]
fn live_an_undo_restores_each_grantors_grant_exactly() {
    each(false, |l, var| {
        let r = Roles::new(l, &["grp", "mem", "mgr"]);
        let (g, m, mgr) = (r.get(0), r.get(1), r.get(2));
        let f = PgFixture::new(l, &unique_name("qr_grantor2"), "CREATE TABLE t (a int);");
        let w = login(l, mgr, "CREATEROLE");
        f.sql(&format!(
            "CREATE ROLE \"{g}\"; CREATE ROLE \"{m}\"; GRANT \"{g}\" TO \"{m}\"; GRANT \"{g}\" TO \"{mgr}\" WITH ADMIN OPTION; \
             GRANT SELECT ON t TO \"{m}\"; GRANT SELECT ON t TO \"{mgr}\" WITH GRANT OPTION; GRANT USAGE ON SCHEMA \"{}\" TO \"{mgr}\"",
            f.schema
        ));
        let (c, o) = (cluster(&w), objects(&w, "quarry_test", &f.schema, Kind::Tables));
        assert_eq!(c.me, mgr, "{var}");
        // mgr gives the grant option the owner had not: its own grant, which the undo must take away whole.
        let before = acl(&f, "t");
        let ch = Change::Acl { target: obj(&o, "t").target(Kind::Tables), grantee: m.into(), from: privs(&[("SELECT", false)]), to: privs(&[("SELECT", true)]) };
        let (res, u) = apply(&w, batch("quarry_test", &o, &c, vec![ch]));
        res.unwrap_or_else(|e| panic!("{var}: {e}"));
        assert!(acl(&f, "t").contains(&format!("{m}=r*/{mgr}")), "{var}: {}", acl(&f, "t"));
        undo(&w, u.unwrap()).0.unwrap_or_else(|e| panic!("{var}: {e}"));
        assert_eq!(acl(&f, "t"), before, "{var}: the undo left a grant of mgr behind");
        // The same for a membership: from 16 mgr's GRANT makes a row of its own, which the undo revokes by name.
        let rows = || f.one(&format!("SELECT string_agg(pg_get_userbyid(grantor) || ':' || admin_option, ',' ORDER BY 1) FROM pg_auth_members WHERE roleid = '\"{g}\"'::regrole AND member = '\"{m}\"'::regrole"));
        let was = rows();
        let now = c.membership(g, m).unwrap();
        let ch = Change::Member { group: g.into(), member: m.into(), from: Some(now), to: Some(Opts { admin: true, ..now }) };
        let (res, u) = apply(&w, batch("quarry_test", &o, &c, vec![ch]));
        res.unwrap_or_else(|e| panic!("{var}: {e}"));
        undo(&w, u.unwrap()).0.unwrap_or_else(|e| panic!("{var}: {e}"));
        if c.major >= 16 {
            assert_eq!(rows(), was, "{var}: the undo left a row of mgr behind");
        } else {
            // One row before 16, whose grantor the GRANT may rewrite; its options are back.
            assert!(rows().ends_with(":false"), "{var}: {}", rows());
        }
    });
}

#[test]
fn live_a_role_with_only_shared_privileges_is_dropped() {
    each(true, |l, var| {
        let r = Roles::new(l, &["conn"]);
        let n = r.get(0);
        // The fixture makes quarry_test, whose CONNECT the role gets.
        let _f = PgFixture::new(l, &unique_name("qr_shared"), "");
        l.w.rt.block_on(async { sqlx::raw_sql(&format!("CREATE ROLE \"{n}\"; GRANT CONNECT ON DATABASE quarry_test TO \"{n}\"; GRANT CREATE ON TABLESPACE pg_default TO \"{n}\"")).execute(&l.pool).await.unwrap() });
        let info = drop_info(l, n);
        assert!(info.dbs.is_empty(), "{var}: {info:?}");
        assert!(info.shared_held.iter().any(|s| s.contains("quarry_test")) && info.shared_held.iter().any(|s| s.contains("pg_default")), "{var}: {info:?}");
        l.w.pg_role_drop(1, l.db.clone(), info, DropMode::DropOwned, false);
        done(l).1.unwrap_or_else(|e| panic!("{var}: {e}"));
        assert!(cluster(l).role(n).is_none(), "{var}");
    });
}

#[test]
fn live_drop_refuses_when_an_owned_object_was_swapped_for_another() {
    each(true, |l, var| {
        let r = Roles::new(l, &["heir", "bob"]);
        let (heir, bob) = (r.get(0), r.get(1));
        let f = PgFixture::new(l, &unique_name("qr_swap"), "");
        f.sql(&format!("CREATE ROLE \"{heir}\"; CREATE ROLE \"{bob}\"; CREATE TABLE bt (a int); ALTER TABLE bt OWNER TO \"{bob}\""));
        let info = drop_info(l, bob);
        // As many objects as the window saw, but not the same one.
        f.sql(&format!("ALTER TABLE bt OWNER TO \"{heir}\"; CREATE TABLE bt4 (a int); ALTER TABLE bt4 OWNER TO \"{bob}\""));
        l.w.pg_role_drop(1, l.db.clone(), info, DropMode::DropOwned, false);
        let err = done(l).1.unwrap_err();
        assert!(err.contains("Look again"), "{var}: {err}");
        assert_eq!(f.one(&format!("SELECT count(*)::text FROM pg_class WHERE relnamespace = '\"{}\"'::regnamespace AND relname = 'bt4'", f.schema)), "1", "{var}: bt4 was dropped");
    });
}

#[test]
fn live_a_table_revoke_that_would_take_column_grants_is_refused() {
    each(true, |l, var| {
        let r = Roles::new(l, &["col"]);
        let m = r.get(0);
        let f = PgFixture::new(l, &unique_name("qr_cols"), "CREATE TABLE t (a int, b int);");
        f.sql(&format!("CREATE ROLE \"{m}\"; GRANT SELECT, UPDATE ON t TO \"{m}\" WITH GRANT OPTION; GRANT SELECT (a) ON t TO \"{m}\"; GRANT UPDATE (b) ON t TO \"{m}\" WITH GRANT OPTION"));
        let (c, o) = (cluster(l), objects(l, "quarry_test", &f.schema, Kind::Tables));
        let target = obj(&o, "t").target(Kind::Tables);
        let from = privs(&[("SELECT", true), ("UPDATE", true)]);
        let cols = || f.one(&format!("SELECT string_agg(attname || '=' || attacl::text, ' ' ORDER BY attnum) FROM pg_attribute WHERE attrelid = '\"{}\".t'::regclass AND attacl IS NOT NULL", f.schema));
        let was = cols();
        // Taking SELECT would take the column grant on a; taking UPDATE's grant option would take b's.
        for to in [privs(&[("UPDATE", true)]), privs(&[("SELECT", true), ("UPDATE", false)])] {
            let (res, _) = apply(l, batch("quarry_test", &o, &c, vec![Change::Acl { target: target.clone(), grantee: m.into(), from: from.clone(), to }]));
            let err = res.unwrap_err();
            assert!(err.contains("on columns of"), "{var}: {err}");
        }
        assert_eq!(cols(), was, "{var}");
        // Taking SELECT's grant option alone leaves the plain column grant on a alone, so it runs.
        let (res, _) = apply(l, batch("quarry_test", &o, &c, vec![Change::Acl { target, grantee: m.into(), from, to: privs(&[("SELECT", false), ("UPDATE", true)]) }]));
        res.unwrap_or_else(|e| panic!("{var}: {e}"));
        assert_eq!(cols(), was, "{var}");
    });
}

#[test]
fn live_an_md5_server_gets_the_md5_hash() {
    each(false, |l, var| {
        let r = Roles::new(l, &["pe", "new"]);
        let (pe, n) = (r.get(0), r.get(1));
        l.w.rt.block_on(async { sqlx::raw_sql(&format!("CREATE ROLE \"{pe}\" LOGIN CREATEROLE PASSWORD 'Zq9login'; ALTER ROLE \"{pe}\" SET password_encryption = 'md5'")).execute(&l.pool).await.unwrap() });
        let w = live_pg_at(uri_as(l, pe, "Zq9login"));
        assert_eq!(cluster(&w).password_encryption, "md5", "{var}");
        w.w.pg_role_create(1, w.db.clone(), NewRole { name: n.into(), attrs: Attrs { login: true, ..Default::default() }, groups: vec![] }, "Zq9md5pw".into(), false);
        done(&w).1.unwrap_or_else(|e| panic!("{var}: {e}"));
        assert_eq!(l.one(&format!("SELECT rolpassword FROM pg_authid WHERE rolname = '{n}'")), md5_verifier("Zq9md5pw", n), "{var}");
        // Only an md5 line of pg_hba.conf signs in with an MD5 hash; the images from 14 on have scram-sha-256 lines.
        if cluster(l).password_encryption == "md5" {
            let who = live_pg_at(uri_as(l, n, "Zq9md5pw"));
            assert_eq!(who.one("SELECT current_user"), *n, "{var}");
        }
    });
}

#[test]
fn live_saslprep_matches_what_the_server_computes() {
    each(false, |l, var| {
        let r = Roles::new(l, &["prep"]);
        let n = r.get(0);
        l.w.rt.block_on(async { sqlx::raw_sql(&format!("CREATE ROLE \"{n}\"")).execute(&l.pool).await.unwrap() });
        for pw in ["Zq9\u{a0}x\u{1f600}", "\u{ad}", "\u{200b}\u{ad}", "Zq9\u{a0}\u{fb01}", "\u{627}1", "\u{627}\u{628}", "a\u{301}b", "Zq9 Märker"] {
            // The server hashes a password given in clear with its own SASLprep; the verifier here must match it.
            let lit = pw.replace('\'', "''");
            l.w.rt.block_on(async { sqlx::raw_sql(&format!("SET password_encryption = 'scram-sha-256'; ALTER ROLE \"{n}\" PASSWORD '{lit}'")).execute(&l.pool).await.unwrap() });
            let stored = l.one(&format!("SELECT rolpassword FROM pg_authid WHERE rolname = '{n}'"));
            let (iters, salt) = stored.strip_prefix("SCRAM-SHA-256$").and_then(|s| s.split('$').next()).and_then(|s| s.split_once(':')).unwrap_or_else(|| panic!("{var}: {stored}"));
            let salt = b64_decode(salt);
            assert_eq!(scram_verifier(pw, &salt, iters.parse().unwrap()), stored, "{var}: {pw:?}");
        }
    });
}

fn b64_decode(s: &str) -> Vec<u8> {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let (mut out, mut acc, mut bits) = (Vec::new(), 0u32, 0);
    for c in s.bytes().filter(|c| *c != b'=') {
        acc = acc << 6 | A.iter().position(|a| *a == c).unwrap() as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}
