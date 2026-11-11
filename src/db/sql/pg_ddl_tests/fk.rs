//! Live tests of editing a PostgreSQL table's foreign keys, on the same fixtures as the structure tests above. `p` is the
//! parent (a primary key, a unique column and a unique pair), `t` the child that gets the keys.

use super::*;
use crate::db::sql::structure::ForeignKey;

const DDL: &str = "CREATE TABLE p (id integer PRIMARY KEY, code text UNIQUE, name text); CREATE UNIQUE INDEX p_id_code ON p (id, code); \
                   INSERT INTO p VALUES (1, 'a', 'A'), (2, 'b', 'B'), (3, 'c', 'C'); \
                   CREATE TABLE t (id integer PRIMARY KEY, pid integer, pcode text, note text); \
                   INSERT INTO t VALUES (1, 1, 'a', 'x'), (2, 2, 'b', 'y'), (3, NULL, NULL, 'z')";

fn structure_of(t: &T, schema: &str, table: &str) -> Structure {
    t.l.w.sql_structure(9, t.db.clone(), schema.into(), table.into());
    let Msg::SqlStructure { res, .. } = t.l.next() else { panic!("expected SqlStructure") };
    res.unwrap()
}

fn key(name: &str, cols: &[&str], ref_schema: &str, ref_table: &str, ref_cols: &[&str]) -> ForeignKey {
    let own = |v: &[&str]| v.iter().map(|c| (*c).to_string()).collect::<Vec<_>>();
    ForeignKey { name: name.into(), cols: own(cols), ref_db: ref_schema.into(), ref_table: ref_table.into(), ref_cols: own(ref_cols), on_update: "NO ACTION".into(), on_delete: "NO ACTION".into(), ..Default::default() }
}

/// The key with the parent as the dialog would have read it, which is what makes a change of the parent noticed.
fn seen(t: &T, mut k: ForeignKey) -> ForeignKey {
    let schema = if k.ref_db.is_empty() { t.schema.clone() } else { k.ref_db.clone() };
    k.ref_seen = ddl::ref_state(&structure_of(t, &schema, &k.ref_table), &k.ref_cols);
    k
}

/// Every foreign key of `t` as the catalog holds it, with what a key can carry besides its columns.
fn fks(t: &T) -> String {
    t.one("SELECT COALESCE(string_agg(conname || '|' || pg_get_constraintdef(oid) || '|' || condeferrable || '|' || condeferred || '|' || convalidated || '|' || COALESCE(obj_description(oid, 'pg_constraint'), ''), ';' ORDER BY conname), '') FROM pg_constraint WHERE contype = 'f' AND conrelid = 't'::regclass")
}

fn add_fk(k: ForeignKey) -> Change {
    Change::AddFk(k)
}

fn drop_fk(n: &str) -> Change {
    Change::DropFk(n.into())
}

/// Holds a lock on `table` from another session until the returned transaction is rolled back.
fn hold(t: &T, table: &str, mode: &str) -> sqlx::Transaction<'static, sqlx::Postgres> {
    t.l.w.rt.block_on(async {
        let mut tx = t.pool.begin().await.unwrap();
        sqlx::raw_sql(&format!("LOCK TABLE {}.{table} IN {mode} MODE", t.schema)).execute(&mut *tx).await.unwrap();
        tx
    })
}

fn release(t: &T, tx: sqlx::Transaction<'static, sqlx::Postgres>) {
    t.l.w.rt.block_on(async { tx.rollback().await.ok() });
}

#[test]
fn a_foreign_key_is_added_with_its_options_and_the_undo_is_exact() {
    every_pg(|l, var| {
        let t = fixture(l, var, "fkadd", DDL);
        assert_eq!(fks(&t), "", "{var}");
        // the plain key: the statement is the one the preview shows, the parent is qualified, the actions are spelled out
        let k = seen(&t, ForeignKey { on_delete: "CASCADE".into(), ..key("t_pid_fk", &["pid"], "", "p", &["id"]) });
        let plan = t.plan(&[add_fk(k.clone())]).unwrap();
        let s = &t.schema;
        assert_eq!(plan.steps, vec![format!("ALTER TABLE \"{s}\".\"t\" ADD CONSTRAINT \"t_pid_fk\" FOREIGN KEY (\"pid\") REFERENCES \"{s}\".\"p\" (\"id\") ON DELETE CASCADE ON UPDATE NO ACTION")], "{var}");
        // what the transaction relies on: the parent shared-row-exclusive locked and compared, the rows checked
        let e = plan.expect.as_ref().unwrap();
        assert_eq!(e.refs.len(), 1, "{var}");
        assert!((e.refs[0].schema.as_str(), e.refs[0].table.as_str(), e.refs[0].exclusive) == (s.as_str(), "p", false) && e.refs[0].seen.is_some(), "{var}: {:?}", e.refs);
        assert_eq!(e.orphans.len(), 1, "{var}");
        let a = t.apply(&[add_fk(k)]);
        let now = fks(&t);
        assert!(now.starts_with("t_pid_fk|FOREIGN KEY (pid) REFERENCES p(id) ON DELETE CASCADE|false|false|true|"), "{var}: {now}");
        t.undo(a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(fks(&t), "", "{var}");
        // every option at once: a pair, MATCH FULL, deferrable and initially deferred, with a comment
        let mut pair = seen(&t, ForeignKey { match_full: true, deferrable: true, deferred: true, comment: "the owner's pair".into(), on_delete: "SET NULL".into(), on_update: "CASCADE".into(), ..key("t_pair_fk", &["pid", "pcode"], "", "p", &["id", "code"]) });
        let plan = t.plan(&[add_fk(pair.clone())]).unwrap();
        assert_eq!(plan.steps.len(), 2, "{var}");
        assert!(plan.steps[0].ends_with("MATCH FULL ON DELETE SET NULL ON UPDATE CASCADE DEFERRABLE INITIALLY DEFERRED"), "{var}: {}", plan.steps[0]);
        let a = t.apply(&[add_fk(pair.clone())]);
        let now = fks(&t);
        assert!(now.contains("MATCH FULL") && now.contains("|true|true|true|the owner's pair"), "{var}: {now}");
        t.undo(a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(fks(&t), "", "{var}");
        // INITIALLY DEFERRED without DEFERRABLE is refused before anything runs
        pair.deferrable = false;
        assert!(t.plan(&[add_fk(pair)]).unwrap_err().contains("DEFERRABLE"), "{var}");
    });
}

#[test]
fn a_foreign_key_is_dropped_renamed_and_changed_and_each_undo_is_exact() {
    every_pg(|l, var| {
        let t = fixture(l, var, "fkdrop", DDL);
        t.sql("ALTER TABLE t ADD CONSTRAINT t_pid_fk FOREIGN KEY (pid) REFERENCES p (id) ON DELETE SET NULL ON UPDATE CASCADE DEFERRABLE INITIALLY DEFERRED; COMMENT ON CONSTRAINT t_pid_fk ON t IS 'who owns it'");
        let before = fks(&t);
        assert!(before.contains("|true|true|true|who owns it"), "{var}: {before}");
        // drop: match, deferrable flags and comment come back
        let a = t.apply(&[drop_fk("t_pid_fk")]);
        assert_eq!(fks(&t), "", "{var}");
        t.undo(a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(fks(&t), before, "{var}");
        // rename keeps everything else, and undo renames back
        let a = t.apply(&[Change::RenameFk { from: "t_pid_fk".into(), to: "t_owner_fk".into() }]);
        assert_eq!(fks(&t), before.replace("t_pid_fk", "t_owner_fk"), "{var}");
        t.undo(a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(fks(&t), before, "{var}");
        // a change of the actions is a drop and an add in one transaction, and its undo is a drop and an add as well
        let st = structure_of(&t, &t.schema, "t");
        let old = st.foreign_keys[0].clone();
        let changed = seen(&t, ForeignKey { on_delete: "CASCADE".into(), on_update: "NO ACTION".into(), deferrable: false, deferred: false, ..old });
        let plan = t.plan(&[drop_fk("t_pid_fk"), add_fk(changed.clone())]).unwrap();
        assert_eq!(plan.steps.len(), 3, "{var}: {:?}", plan.steps);
        let a = t.apply(&[drop_fk("t_pid_fk"), add_fk(changed)]);
        assert!(fks(&t).starts_with("t_pid_fk|FOREIGN KEY (pid) REFERENCES p(id) ON DELETE CASCADE|false|false|true|"), "{var}: {}", fks(&t));
        t.undo(a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(fks(&t), before, "{var}");
        // a key that was added NOT VALID stays NOT VALID through a drop and its undo
        t.sql("ALTER TABLE t DROP CONSTRAINT t_pid_fk; INSERT INTO t VALUES (4, 77, 'a', 'orphan'); ALTER TABLE t ADD CONSTRAINT t_nv_fk FOREIGN KEY (pid) REFERENCES p (id) NOT VALID");
        let nv = fks(&t);
        assert!(nv.contains("NOT VALID") && nv.contains("|false|false|false|"), "{var}: {nv}");
        let a = t.apply(&[drop_fk("t_nv_fk")]);
        t.undo(a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(fks(&t), nv, "{var}");
        assert_eq!(t.one("SELECT count(*)::text FROM t WHERE pid = 77"), "1", "{var}: the row that points at nothing was not touched");
    });
}

#[test]
fn a_key_can_point_at_a_table_of_another_schema() {
    every_pg(|l, var| {
        let t = fixture(l, var, "fkcross", DDL);
        let other = format!("{}_o", t.schema);
        t.sql(&format!("DROP SCHEMA IF EXISTS {other} CASCADE; CREATE SCHEMA {other}; CREATE TABLE {other}.q (id integer PRIMARY KEY, code text UNIQUE); INSERT INTO {other}.q VALUES (1, 'a'), (2, 'b')"));
        let k = seen(&t, key("t_q_fk", &["pid"], &other, "q", &["id"]));
        let plan = t.plan(&[add_fk(k.clone())]).unwrap();
        assert!(plan.steps[0].ends_with(&format!("REFERENCES \"{other}\".\"q\" (\"id\") ON DELETE NO ACTION ON UPDATE NO ACTION")), "{var}: {}", plan.steps[0]);
        assert_eq!(plan.expect.as_ref().unwrap().refs[0].schema, other, "{var}");
        let a = t.apply(&[add_fk(k)]);
        assert_eq!(t.one("SELECT confrelid::regclass::text FROM pg_constraint WHERE conname = 't_q_fk'"), format!("{other}.q"), "{var}");
        // the key is read back with the parent's schema, and dropping it names only the key
        let read = structure_of(&t, &t.schema, "t").foreign_keys;
        assert_eq!((read[0].ref_db.as_str(), read[0].ref_table.as_str()), (other.as_str(), "q"), "{var}");
        t.undo(a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(fks(&t), "", "{var}");
        let k = seen(&t, key("t_q_fk", &["pid"], &other, "q", &["id"]));
        t.apply(&[add_fk(k)]);
        let gone = t.apply(&[drop_fk("t_q_fk")]);
        t.undo(gone).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert!(fks(&t).starts_with("t_q_fk|"), "{var}");
        t.sql(&format!("DROP SCHEMA {other} CASCADE"));
    });
}

#[test]
fn rows_that_point_at_nothing_refuse_the_key_and_are_shown() {
    every_pg(|l, var| {
        let t = fixture(l, var, "fkorph", DDL);
        t.sql("INSERT INTO t VALUES (4, 77, 'a', 'o1'), (5, 78, 'b', 'o2')");
        let k = seen(&t, key("t_pid_fk", &["pid"], "", "p", &["id"]));
        let before = t.shape();
        let (res, undo) = t.run(t.plan(&[add_fk(k.clone())]).unwrap());
        let err = res.unwrap_err();
        assert!(err.starts_with("2 rows of") && err.contains("have a value in (\"pid\")") && err.contains("(77)") && err.contains("(78)") && err.contains("NOT VALID") && err.contains("Nothing was changed"), "{var}: {err}");
        assert!(undo.is_none() && fks(&t) == "" && t.shape() == before, "{var}");
        // one row is said in the singular
        t.sql("DELETE FROM t WHERE pid = 78");
        let err = t.run(t.plan(&[add_fk(k.clone())]).unwrap()).0.unwrap_err();
        assert!(err.starts_with("1 row of") && err.contains("has a value in") && err.contains("The row is (77)"), "{var}: {err}");
        // the dialog's own count asks the same question and gets the rows too
        t.l.w.sql_orphans(5, t.db.clone(), t.schema.clone(), "t".into(), k.clone());
        let Msg::SqlOrphans { res, sample, .. } = t.l.next() else { panic!("expected SqlOrphans") };
        assert_eq!((res.unwrap(), sample), (1, vec!["(77)".to_string()]), "{var}");
        // a pair is shown as a pair
        t.sql("UPDATE t SET pcode = 'zzz' WHERE pid = 77");
        let pair = seen(&t, key("t_pair_fk", &["pid", "pcode"], "", "p", &["id", "code"]));
        let err = t.run(t.plan(&[add_fk(pair)]).unwrap()).0.unwrap_err();
        assert!(err.contains("(77, zzz)"), "{var}: {err}");
        // NOT VALID skips the check, and the key still guards new rows
        let nv = ForeignKey { not_valid: true, ..k };
        assert!(t.plan(&[add_fk(nv.clone())]).unwrap().expect.unwrap().orphans.is_empty(), "{var}");
        let a = t.apply(&[add_fk(nv)]);
        assert!(fks(&t).contains("NOT VALID"), "{var}");
        l.w.rt.block_on(async { sqlx::raw_sql(&format!("SET search_path = {}; INSERT INTO t VALUES (6, 99, 'a', 'new')", t.schema)).execute(&t.pool).await }).expect_err("a new row that points at nothing");
        t.undo(a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(fks(&t), "", "{var}");
    });
}

#[test]
fn a_parent_changed_after_the_plan_was_made_refuses_the_plan_and_nothing_runs() {
    every_pg(|l, var| {
        let t = fixture(l, var, "fkstale", DDL);
        let k = seen(&t, key("t_code_fk", &["pcode"], "", "p", &["code"]));
        let before = t.shape();
        // the unique constraint the key needs goes after the plan was made
        let plan = t.plan(&[add_fk(k.clone())]).unwrap();
        t.sql("ALTER TABLE p DROP CONSTRAINT p_code_key");
        let (res, undo) = t.run(plan);
        let err = res.unwrap_err();
        assert!(err.contains("changed since this plan was made") && err.contains("referenced table") && err.contains("\"p\""), "{var}: {err}");
        assert!(undo.is_none() && fks(&t) == "" && t.shape() == before, "{var}");
        // a plan made without having seen the parent runs and the server refuses it, which rolls everything back
        let blind = ForeignKey { ref_seen: Vec::new(), ..k.clone() };
        let (res, _) = t.run(t.plan(&[add_fk(blind)]).unwrap());
        let err = res.unwrap_err();
        assert!(err.contains("unique constraint") && err.contains("statement 1 of 1") && err.contains("rolled back"), "{var}: {err}");
        // the referenced column changes type
        t.sql("ALTER TABLE p ADD CONSTRAINT p_code_key UNIQUE (code)");
        let k = seen(&t, key("t_code_fk", &["pcode"], "", "p", &["code"]));
        let plan = t.plan(&[add_fk(k.clone())]).unwrap();
        t.sql("ALTER TABLE p ALTER COLUMN code TYPE varchar(30)");
        let err = t.run(plan).0.unwrap_err();
        assert!(err.contains("referenced table"), "{var}: {err}");
        assert_eq!(fks(&t), "", "{var}");
        // a unique index is added, which a key may now pick instead: still a change of the parent
        let plan = t.plan(&[add_fk(seen(&t, key("t_code_fk", &["pcode"], "", "p", &["code"])))]).unwrap();
        t.sql("CREATE UNIQUE INDEX p_code_again ON p (code)");
        assert!(t.run(plan).0.unwrap_err().contains("referenced table"), "{var}");
        // the parent is gone
        let plan = t.plan(&[add_fk(seen(&t, key("t_code_fk", &["pcode"], "", "p", &["code"])))]).unwrap();
        t.sql("DROP TABLE p");
        let err = t.run(plan).0.unwrap_err();
        assert!(err.contains("referenced table") && err.contains("it is gone"), "{var}: {err}");
        assert_eq!(fks(&t), "", "{var}");
    });
}

#[test]
fn a_parent_that_another_session_holds_fails_the_plan_on_the_lock_timeout() {
    every_pg(|l, var| {
        let t = fixture(l, var, "fklock", DDL);
        let k = seen(&t, key("t_pid_fk", &["pid"], "", "p", &["id"]));
        let before = t.shape();
        // a session that has a row of the parent open for writing: it blocks the SHARE ROW EXCLUSIVE a new key takes there
        let tx = hold(&t, "p", "ROW EXCLUSIVE");
        let started = std::time::Instant::now();
        let (res, undo) = t.run(t.plan(&[add_fk(k.clone())]).unwrap());
        let err = res.unwrap_err();
        assert!(err.contains("locked by another session") && err.contains(&format!("\"{}\".\"p\"", t.schema)) && err.contains("Nothing was changed"), "{var}: {err}");
        assert!(undo.is_none() && started.elapsed() < std::time::Duration::from_secs(8), "{var}: {:?}", started.elapsed());
        release(&t, tx);
        assert_eq!((fks(&t), t.shape()), (String::new(), before), "{var}");
        // readers of the parent do not stop a new key, which only needs SHARE ROW EXCLUSIVE there
        let tx = hold(&t, "p", "ACCESS SHARE");
        let a = t.apply(&[add_fk(k)]);
        // dropping a key takes ACCESS EXCLUSIVE on the parent, so the same reader stops that
        let (res, _) = t.run(t.plan(&[drop_fk("t_pid_fk")]).unwrap());
        assert!(res.unwrap_err().contains("locked by another session"), "{var}");
        assert!(fks(&t).starts_with("t_pid_fk|"), "{var}");
        release(&t, tx);
        t.undo(a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(fks(&t), "", "{var}");
    });
}

#[test]
fn a_parent_that_changed_after_a_drop_refuses_the_undo() {
    every_pg(|l, var| {
        let t = fixture(l, var, "fkundo", DDL);
        t.sql("ALTER TABLE t ADD CONSTRAINT t_code_fk FOREIGN KEY (pcode) REFERENCES p (code)");
        let before = fks(&t);
        let a = t.apply(&[drop_fk("t_code_fk")]);
        t.sql("ALTER TABLE p ALTER COLUMN code TYPE varchar(30)");
        let (res, kept) = t.undo(a);
        let err = res.unwrap_err();
        assert!(err.contains("Undo refused") && err.contains("referenced table"), "{var}: {err}");
        assert!(kept.is_some() && fks(&t) == "", "{var}");
        // the parent as it was: the same undo now runs
        t.sql("ALTER TABLE p ALTER COLUMN code TYPE text");
        let (res, _) = t.undo(kept.unwrap());
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(fks(&t), before, "{var}");
    });
}

#[test]
fn a_dropped_column_takes_its_key_and_the_undo_puts_it_back_whole() {
    every_pg(|l, var| {
        let t = fixture(l, var, "fkcol", DDL);
        t.sql("ALTER TABLE t ADD CONSTRAINT t_pid_fk FOREIGN KEY (pid) REFERENCES p (id) ON DELETE SET NULL ON UPDATE CASCADE DEFERRABLE INITIALLY DEFERRED; COMMENT ON CONSTRAINT t_pid_fk ON t IS 'who owns it'");
        let before = fks(&t);
        let plan = t.plan(&[Change::Drop("pid".into())]).unwrap();
        // the parent is locked for the drop of the key and again for the key the undo adds
        assert!(plan.expect.as_ref().unwrap().refs.iter().any(|r| r.table == "p" && r.exclusive), "{var}");
        assert!(plan.undo.as_ref().unwrap().refs.iter().any(|r| r.table == "p" && !r.exclusive), "{var}");
        let a = t.apply(&[Change::Drop("pid".into())]);
        assert_eq!(fks(&t), "", "{var}");
        t.undo(a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(fks(&t), before, "{var}");
        assert!(t.col("pid").is_some(), "{var}");
        // a key that is NOT VALID comes back NOT VALID
        t.sql("ALTER TABLE t DROP CONSTRAINT t_pid_fk; UPDATE t SET pid = 77 WHERE id = 1; ALTER TABLE t ADD CONSTRAINT t_nv_fk FOREIGN KEY (pid) REFERENCES p (id) NOT VALID");
        let nv = fks(&t);
        let a = t.apply(&[Change::Drop("pid".into())]);
        t.undo(a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(fks(&t), nv, "{var}");
        // a pair under MATCH FULL could not come back with one column empty: the plan says there is no undo
        t.sql("ALTER TABLE t DROP CONSTRAINT t_nv_fk; ALTER TABLE t ADD CONSTRAINT t_full_fk FOREIGN KEY (pid, pcode) REFERENCES p (id, code) MATCH FULL NOT VALID");
        let why = t.plan(&[Change::Drop("pid".into())]).unwrap().undo.unwrap_err();
        assert!(why.contains("MATCH FULL") && why.contains("t_full_fk"), "{var}: {why}");
        // a plain pair is fine: the empty column passes MATCH SIMPLE
        t.sql("ALTER TABLE t DROP CONSTRAINT t_full_fk; ALTER TABLE t ADD CONSTRAINT t_pair_fk FOREIGN KEY (pid, pcode) REFERENCES p (id, code) NOT VALID");
        let pair = fks(&t);
        let a = t.apply(&[Change::Drop("pcode".into())]);
        t.undo(a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(fks(&t), pair, "{var}");
    });
}

#[test]
fn a_key_of_another_table_that_points_at_a_dropped_column_is_refused_and_says_so() {
    every_pg(|l, var| {
        let t = fixture(l, var, "fkin", DDL);
        t.sql("CREATE TABLE c (tid integer REFERENCES t (id))");
        let before = t.shape();
        let plan = t.plan(&[Change::Drop("id".into())]).unwrap();
        let why = plan.undo.as_ref().unwrap_err();
        assert!(why.contains("another table"), "{var}: {why}");
        // the server refuses the drop instead of cascading, so nothing is lost and nothing needs an undo
        let (res, undo) = t.run(plan);
        assert!(res.unwrap_err().contains("depend"), "{var}");
        assert!(undo.is_none() && t.shape() == before && t.one("SELECT count(*)::text FROM c") == "0", "{var}");
        assert!(t.col("id").is_some(), "{var}");
    });
}

#[test]
fn a_key_of_a_table_on_itself_comes_back_with_its_column() {
    every_pg(|l, var| {
        let t = fixture(l, var, "fkself", "CREATE TABLE t (id integer PRIMARY KEY, parent integer, name text, CONSTRAINT t_parent_fk FOREIGN KEY (parent) REFERENCES t (id) ON DELETE CASCADE); INSERT INTO t VALUES (1, NULL, 'a'), (2, 1, 'b')");
        let before = fks(&t);
        let plan = t.plan(&[Change::Drop("parent".into())]).unwrap();
        // the table is its own parent: it is locked once, and the key it refers back to is part of what is compared
        let locks = plan.expect.as_ref().unwrap();
        assert!(locks.refs.iter().all(|r| r.table == "t"), "{var}");
        let a = t.apply(&[Change::Drop("parent".into())]);
        assert_eq!(fks(&t), "", "{var}");
        t.undo(a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(fks(&t), before, "{var}");
        // a self key is added and dropped like any other
        let k = seen(&t, ForeignKey { name: "t_again_fk".into(), ..key("t_again_fk", &["id"], "", "t", &["id"]) });
        let a = t.apply(&[add_fk(k)]);
        t.undo(a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(fks(&t), before, "{var}");
    });
}

#[test]
fn a_key_the_editor_cannot_write_back_is_refused_instead_of_rewritten() {
    every_pg(|l, var| {
        let t = fixture(l, var, "fkopaque", "CREATE TABLE p2 (a integer, b integer, PRIMARY KEY (a, b)); CREATE TABLE t (id integer PRIMARY KEY, a integer, b integer)");
        if t.one("SHOW server_version_num").parse::<u32>().unwrap() < 150000 {
            // PostgreSQL 13 has no ON DELETE column list, so there is nothing it could not write back
            assert!(structure_of(&t, &t.schema, "t").foreign_keys.iter().all(|k| k.opaque.is_empty()), "{var}");
            return;
        }
        t.sql("ALTER TABLE t ADD CONSTRAINT t_set_fk FOREIGN KEY (id, a) REFERENCES p2 (a, b) ON DELETE SET NULL (a)");
        let read = structure_of(&t, &t.schema, "t").foreign_keys;
        assert!(read[0].opaque.contains("names columns"), "{var}: {:?}", read[0].opaque);
        let before = fks(&t);
        let why = t.plan(&[drop_fk("t_set_fk")]).unwrap().undo.unwrap_err();
        assert!(why.contains("cannot be written back") && why.contains("names columns"), "{var}: {why}");
        let why = t.plan(&[Change::Drop("a".into())]).unwrap().undo.unwrap_err();
        assert!(why.contains("cannot put back"), "{var}: {why}");
        assert_eq!(fks(&t), before, "{var}");
    });
}

#[test]
fn a_foreign_key_applies_and_undoes_through_a_pooler() {
    let Some(l) = live_pgbouncer() else { return };
    let t = fixture(&l, "QUARRY_TEST_PGBOUNCER", "fkpgb", DDL);
    assert!(t.db.pooler());
    let k = seen(&t, ForeignKey { on_delete: "CASCADE".into(), deferrable: true, ..key("t_pid_fk", &["pid"], "", "p", &["id"]) });
    let a = t.apply(&[add_fk(k)]);
    assert!(fks(&t).contains("ON DELETE CASCADE") && fks(&t).contains("|true|false|true|"), "applied: {}", fks(&t));
    let gone = t.apply(&[drop_fk("t_pid_fk")]);
    assert_eq!(fks(&t), "");
    t.undo(gone).0.unwrap_or_else(|x| panic!("undo of the drop: {x}"));
    assert!(fks(&t).starts_with("t_pid_fk|"));
    t.undo(a).0.unwrap_or_else(|x| panic!("undo of the add: {x}"));
    assert_eq!(fks(&t), "");
    // an orphan is refused with its rows here too, and the pooled session keeps none of the transaction's settings
    t.sql("INSERT INTO t VALUES (4, 77, 'a', 'o')");
    let err = t.run(t.plan(&[add_fk(seen(&t, key("t_again_fk", &["pid"], "", "p", &["id"])))]).unwrap()).0.unwrap_err();
    assert!(err.contains("(77)") && err.contains("Nothing was changed"), "{err}");
    assert_eq!(fks(&t), "");
    assert_eq!(t.one("SHOW lock_timeout"), "0");
}
