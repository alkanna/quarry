//! Live tests of editing PostgreSQL triggers and their functions, on the same fixtures as the structure tests above. `t` is the
//! table that gets the triggers. `world` is the exactness check: every trigger of `t` as `pg_get_triggerdef` prints it, its
//! enabled state and comment, and every function of the schema as `pg_get_functiondef` prints it with its comment, so two
//! equal worlds are the same triggers and functions.

use super::fk::hold;
use super::*;
use crate::db::sql::ddl::{PgDef, TrigDef, TrigJob, TrigSave};
use crate::db::sql::ddl_pg_trig;
use crate::db::sql::structure::Trigger;

const DDL: &str = "CREATE TABLE t (id integer PRIMARY KEY, name text, qty integer); INSERT INTO t VALUES (1, 'a', 1); \
                   CREATE FUNCTION f0() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$";

fn structure_of(t: &T, table: &str) -> Structure {
    t.l.w.sql_structure(9, t.db.clone(), t.schema.clone(), table.into());
    let Msg::SqlStructure { res, .. } = t.l.next() else { panic!("expected SqlStructure") };
    res.unwrap()
}

fn trigger_on(t: &T, table: &str, name: &str) -> Trigger {
    structure_of(t, table).triggers.into_iter().find(|x| x.name == name).unwrap_or_else(|| panic!("no trigger {name} on {table}"))
}

fn draft(name: &str, timing: &str, event: &str, body: &str) -> TrigDef {
    TrigDef { name: name.into(), timing: timing.into(), event: event.into(), body: body.into(), pg: PgDef::default() }
}

fn create(d: TrigDef) -> TrigJob {
    TrigJob::Create(TrigSave { def: d, definer: None, order: None })
}

/// The trigger as the editor holds it after the person changed it.
fn edit_on(t: &T, table: &str, name: &str, f: impl FnOnce(&mut TrigDef)) -> TrigJob {
    let was = trigger_on(t, table, name);
    let mut d = ddl_pg_trig::def_of(&was);
    f(&mut d);
    TrigJob::Edit { was, save: TrigSave { def: d, definer: None, order: None } }
}

fn edit(t: &T, name: &str, f: impl FnOnce(&mut TrigDef)) -> TrigJob {
    edit_on(t, "t", name, f)
}

fn drop_job(t: &T, name: &str) -> TrigJob {
    TrigJob::Drop(trigger_on(t, "t", name))
}

fn run_on(l: &LivePg, db: &Db, schema: &str, table: &str, job: TrigJob) -> (Res<String>, Option<UndoAction>, bool) {
    l.w.sql_trigger(1, db.clone(), schema.into(), table.into(), job, Mode::DEFAULT, false, "done".into());
    let Msg::SqlAltered { res, undo, unsure, alter, database, .. } = l.next() else { panic!("expected SqlAltered") };
    assert!(!alter && database == db.pg_database(), "a trigger answer is not an ALTER, and names its database");
    (res, undo, unsure)
}

fn run(t: &T, job: TrigJob) -> (Res<String>, Option<UndoAction>) {
    let (res, undo, _) = run_on(t.l, &t.db, &t.schema, "t", job);
    (res, undo)
}

fn apply(t: &T, job: TrigJob) -> UndoAction {
    let (res, undo) = run(t, job);
    res.unwrap_or_else(|x| panic!("apply: {x}"));
    undo.expect("an undo")
}

fn undo(t: &T, a: UndoAction) -> (Res<String>, Option<UndoAction>) {
    t.undo(a)
}

fn world(t: &T) -> String {
    t.one(&format!(
        "SELECT COALESCE(string_agg(x, E'\\n' ORDER BY x), '') FROM (\
           SELECT 'T ' || pg_get_triggerdef(g.oid, false) || '|' || g.tgenabled::text || '|' || COALESCE(obj_description(g.oid, 'pg_trigger'), '') AS x FROM pg_trigger g WHERE g.tgrelid = 't'::regclass AND NOT g.tgisinternal \
           UNION ALL SELECT 'F ' || pg_get_functiondef(p.oid) || '|' || COALESCE(obj_description(p.oid, 'pg_proc'), '') FROM pg_proc p WHERE p.pronamespace = '{}'::regnamespace AND p.proname <> 'f0') s",
        t.schema
    ))
}

#[test]
fn a_trigger_is_created_with_its_function_and_the_undo_removes_both() {
    every_pg(|l, var| {
        let t = fixture(l, var, "trgnew", DDL);
        let empty = world(&t);
        let a = apply(&t, create(draft("audit", "BEFORE", "INSERT", "BEGIN\n  NEW.name := upper(NEW.name);\n  RETURN NEW;\nEND")));
        let made = world(&t);
        assert!(made.contains("T CREATE TRIGGER audit BEFORE INSERT ON ") && made.contains("t_audit_fn()") && made.contains("RETURNS trigger"), "{var}: {made}");
        t.sql("INSERT INTO t VALUES (2, 'x', 2)");
        assert_eq!(t.one("SELECT name FROM t WHERE id = 2"), "X", "{var}: the trigger fires");
        t.sql("DELETE FROM t WHERE id = 2");
        let (res, left) = undo(&t, a);
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert!(left.is_none(), "{var}");
        assert_eq!(world(&t), empty, "{var}");
        // nothing else of the table changed
        assert_eq!(t.one("SELECT count(*)::text FROM t"), "1", "{var}");
        // a function name that is taken is refused, and the trigger is not made either
        t.sql("CREATE FUNCTION t_taken_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$");
        let before = world(&t);
        let err = run(&t, create(draft("taken", "BEFORE", "INSERT", "BEGIN\n  RETURN NEW;\nEND"))).0.unwrap_err();
        assert!(err.contains("already exists") && err.contains("t_taken_fn"), "{var}: {err}");
        assert_eq!(world(&t), before, "{var}");
        // so is a trigger name that is taken
        t.sql("CREATE TRIGGER twice BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION f0()");
        let before = world(&t);
        let err = run(&t, create(draft("twice", "BEFORE", "INSERT", "BEGIN\n  RETURN NEW;\nEND"))).0.unwrap_err();
        assert!(err.contains("already has a trigger called twice"), "{var}: {err}");
        assert_eq!(world(&t), before, "{var}");
    });
}

#[test]
fn a_body_is_edited_and_the_undo_puts_the_old_function_back_exactly() {
    every_pg(|l, var| {
        let t = fixture(l, var, "trgbody", DDL);
        t.sql("CREATE FUNCTION audit_fn() RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp AS $$\nBEGIN\n  NEW.name := upper(NEW.name);\n  RETURN NEW;\nEND\n$$; \
               CREATE TRIGGER audit BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION audit_fn(); COMMENT ON TRIGGER audit ON t IS 'who watches'; COMMENT ON FUNCTION audit_fn() IS 'the audit'");
        let before = world(&t);
        let job = edit(&t, "audit", |d| d.body = "BEGIN\n  NEW.name := lower(NEW.name) || '!';\n  RETURN NEW;\nEND".into());
        let (steps, warnings) = ddl_pg_trig::preview(&t.schema, "t", &job).unwrap();
        assert_eq!(steps.len(), 1, "{var}: the trigger part is the same, so only the function is replaced");
        assert!(warnings.iter().any(|w| w.contains("SECURITY DEFINER")), "{var}: {warnings:?}");
        let a = apply(&t, job);
        let after = world(&t);
        assert_ne!(after, before, "{var}");
        // SECURITY DEFINER, the SET clause and the comments are carried through
        assert!(after.contains("SECURITY DEFINER") && after.contains("SET search_path TO 'pg_catalog', 'pg_temp'") && after.contains("|the audit") && after.contains("|who watches"), "{var}: {after}");
        t.sql("INSERT INTO t VALUES (2, 'X', 2)");
        assert_eq!(t.one("SELECT name FROM t WHERE id = 2"), "x!", "{var}");
        t.sql("DELETE FROM t WHERE id = 2");
        undo(&t, a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(world(&t), before, "{var}");
    });
}

#[test]
fn timing_events_when_columns_and_row_or_statement_are_changed_and_the_undo_is_exact() {
    every_pg(|l, var| {
        let t = fixture(l, var, "trgparts", DDL);
        t.sql("CREATE FUNCTION audit_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; CREATE TRIGGER audit BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION audit_fn(); COMMENT ON TRIGGER audit ON t IS 'who watches'");
        let before = world(&t);
        let job = edit(&t, "audit", |d| {
            d.timing = "AFTER".into();
            d.event = "INSERT OR UPDATE".into();
            d.pg.of_cols = "name, qty".into();
            d.pg.when = "NEW.qty > 0".into();
        });
        let (steps, _) = ddl_pg_trig::preview(&t.schema, "t", &job).unwrap();
        assert!(steps[0].starts_with("DROP TRIGGER IF EXISTS \"audit\" ON ") && steps[1].starts_with("CREATE TRIGGER \"audit\" AFTER INSERT OR UPDATE OF \"name\", \"qty\" ON "), "{var}: {steps:?}");
        let a = apply(&t, job);
        let now = world(&t);
        assert!(now.contains("AFTER INSERT OR UPDATE OF name, qty ON") && now.contains("WHEN ((new.qty > 0))") && now.contains("|O|who watches"), "{var}: {now}");
        // the structure reads the parts back the way the editor holds them
        let read = trigger_on(&t, "t", "audit");
        let d = ddl_pg_trig::def_of(&read);
        assert_eq!((d.timing.as_str(), d.event.as_str(), d.pg.of_cols.as_str(), d.pg.when.as_str(), d.pg.per_row), ("AFTER", "INSERT OR UPDATE", "name, qty", "new.qty > 0", true), "{var}");
        undo(&t, a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(world(&t), before, "{var}");
        // statement level with TRUNCATE, no WHEN
        let a = apply(&t, edit(&t, "audit", |d| {
            d.event = "INSERT OR DELETE OR TRUNCATE".into();
            d.pg.per_row = false;
        }));
        assert!(world(&t).contains("AFTER INSERT OR DELETE OR TRUNCATE ON") || world(&t).contains("BEFORE INSERT OR DELETE OR TRUNCATE ON"), "{var}: {}", world(&t));
        assert!(world(&t).contains("FOR EACH STATEMENT"), "{var}");
        undo(&t, a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(world(&t), before, "{var}");
        // a WHEN condition that names a column the table lacks is refused by the server and nothing changes
        let err = run(&t, edit(&t, "audit", |d| d.pg.when = "NEW.nope > 0".into())).0.unwrap_err();
        assert!(err.contains("nope") && err.contains("rolled back"), "{var}: {err}");
        assert_eq!(world(&t), before, "{var}");
    });
}

#[test]
fn a_trigger_is_disabled_and_enabled_with_the_weak_lock_and_the_undo_is_exact() {
    every_pg(|l, var| {
        let t = fixture(l, var, "trgonoff", DDL);
        t.sql("CREATE FUNCTION audit_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.name := 'T'; RETURN NEW; END $$; CREATE TRIGGER audit BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION audit_fn()");
        let before = world(&t);
        let job = edit(&t, "audit", |d| d.pg.mode = "D".into());
        assert_eq!(ddl_pg_trig::preview(&t.schema, "t", &job).unwrap().0.len(), 1, "{var}: only ALTER TABLE .. DISABLE TRIGGER");
        // ALTER TABLE .. DISABLE TRIGGER takes SHARE ROW EXCLUSIVE, so a reader of the table does not stop it
        let held = hold(&t, "t", "ACCESS SHARE");
        let a = apply(&t, job);
        drop(held);
        assert_eq!(t.one("SELECT tgenabled::text FROM pg_trigger WHERE tgname = 'audit' AND tgrelid = 't'::regclass"), "D", "{var}");
        t.sql("INSERT INTO t VALUES (2, 'x', 2)");
        assert_eq!(t.one("SELECT name FROM t WHERE id = 2"), "x", "{var}: a disabled trigger does not fire");
        t.sql("DELETE FROM t WHERE id = 2");
        undo(&t, a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(world(&t), before, "{var}");
        // replica and always are kept through an edit of the rest
        t.sql("ALTER TABLE t ENABLE ALWAYS TRIGGER audit");
        let always = world(&t);
        let a = apply(&t, edit(&t, "audit", |d| d.timing = "AFTER".into()));
        assert!(world(&t).contains("|A|"), "{var}: {}", world(&t));
        undo(&t, a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(world(&t), always, "{var}");
    });
}

#[test]
fn a_dropped_trigger_takes_its_function_and_the_undo_brings_both_back() {
    every_pg(|l, var| {
        let t = fixture(l, var, "trgdrop", DDL);
        t.sql("CREATE FUNCTION audit_fn() RETURNS trigger LANGUAGE plpgsql AS $$\nBEGIN\n  RETURN NEW;\nEND\n$$; CREATE TRIGGER audit AFTER INSERT OR UPDATE ON t FOR EACH ROW WHEN (NEW.qty > 0) EXECUTE FUNCTION audit_fn(); \
               ALTER TABLE t DISABLE TRIGGER audit; COMMENT ON TRIGGER audit ON t IS 'who watches'; COMMENT ON FUNCTION audit_fn() IS 'the audit'");
        let before = world(&t);
        let (steps, _) = ddl_pg_trig::preview(&t.schema, "t", &drop_job(&t, "audit")).unwrap();
        assert_eq!(steps.len(), 2, "{var}: {steps:?}");
        let a = apply(&t, drop_job(&t, "audit"));
        assert_eq!(world(&t), "", "{var}: the function is gone with its trigger");
        undo(&t, a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(world(&t), before, "{var}: trigger, state, comments and function are back");
        // a function with privileges granted on it is kept, since the undo could not give them back
        t.sql("REVOKE EXECUTE ON FUNCTION audit_fn() FROM PUBLIC");
        let kept = ddl_pg_trig::preview(&t.schema, "t", &drop_job(&t, "audit"));
        let (steps, warnings) = kept.unwrap();
        assert_eq!(steps.len(), 1, "{var}: {steps:?}");
        assert!(warnings[0].contains("privileges"), "{var}: {warnings:?}");
    });
}

#[test]
fn a_function_other_triggers_run_is_warned_about_and_kept() {
    every_pg(|l, var| {
        let t = fixture(l, var, "trgshared", DDL);
        t.sql("CREATE FUNCTION shared_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; \
               CREATE TRIGGER s1 BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION shared_fn(); CREATE TRIGGER s2 BEFORE UPDATE ON t FOR EACH ROW EXECUTE FUNCTION shared_fn()");
        let before = world(&t);
        let job = edit(&t, "s1", |d| d.body = "BEGIN\n  NEW.qty := 99;\n  RETURN NEW;\nEND".into());
        let (_, warnings) = ddl_pg_trig::preview(&t.schema, "t", &job).unwrap();
        assert!(warnings.iter().any(|w| w.contains("also runs 1 other trigger")), "{var}: {warnings:?}");
        let a = apply(&t, job);
        t.sql("UPDATE t SET name = 'q' WHERE id = 1");
        assert_eq!(t.one("SELECT qty::text FROM t WHERE id = 1"), "99", "{var}: the other trigger runs the changed function");
        t.sql("UPDATE t SET qty = 1 WHERE id = 1");
        // the function is shared, so the undo puts it back for both
        undo(&t, a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(world(&t), before, "{var}");
        // dropping one trigger keeps the function the other runs
        let (steps, warnings) = ddl_pg_trig::preview(&t.schema, "t", &drop_job(&t, "s1")).unwrap();
        assert_eq!(steps.len(), 1, "{var}: {steps:?}");
        assert!(warnings[0].contains("stays") && warnings[0].contains("1 other trigger"), "{var}: {warnings:?}");
        let a = apply(&t, drop_job(&t, "s1"));
        assert_eq!(t.one("SELECT count(*)::text FROM pg_proc WHERE proname = 'shared_fn'"), "1", "{var}");
        t.sql("UPDATE t SET name = 'z' WHERE id = 1");
        undo(&t, a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(world(&t), before, "{var}");
    });
}

#[test]
fn a_body_with_dollar_quotes_and_the_generated_tag_is_stored_whole_and_cannot_break_out() {
    every_pg(|l, var| {
        let t = fixture(l, var, "trgdollar", DDL);
        let body = "DECLARE a text := $$x$$;\nBEGIN\n  a := $function$y$function$ || $functionx$z$functionx$;\n  NEW.name := a;\n  RETURN NEW;\nEND";
        let a = apply(&t, create(draft("dq", "BEFORE", "INSERT", body)));
        let read = trigger_on(&t, "t", "dq");
        assert_eq!(read.pg.as_ref().unwrap().src.trim(), body, "{var}");
        assert!(read.pg.as_ref().unwrap().func_def.contains("AS $functionxx$"), "{var}: {}", read.pg.as_ref().unwrap().func_def);
        t.sql("INSERT INTO t VALUES (2, 'x', 2)");
        assert_eq!(t.one("SELECT name FROM t WHERE id = 2"), "yz", "{var}");
        t.sql("DELETE FROM t WHERE id = 2");
        // editing a body whose tag the server grew exactly like ours
        let before = world(&t);
        let b = apply(&t, edit(&t, "dq", |d| d.body = d.body.replace("NEW.name := a;", "NEW.name := a || '!';")));
        t.sql("INSERT INTO t VALUES (3, 'x', 2)");
        assert_eq!(t.one("SELECT name FROM t WHERE id = 3"), "yz!", "{var}");
        t.sql("DELETE FROM t WHERE id = 3");
        undo(&t, b).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(world(&t), before, "{var}");
        undo(&t, a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(world(&t), "", "{var}");
        // a body that closes its own quote and starts a statement is text to the server, which refuses it as plpgsql
        let evil = "BEGIN\n  RETURN NEW;\nEND\n$function$; DROP TABLE t; --";
        let err = run(&t, create(draft("evil", "BEFORE", "INSERT", evil))).0.unwrap_err();
        assert!(err.contains("rolled back"), "{var}: {err}");
        assert_eq!(t.one("SELECT count(*)::text FROM t"), "1", "{var}: the table is there");
        assert_eq!(world(&t), "", "{var}: neither trigger nor function was made");
        // a name that tries to leave its quotes is only a name
        let name = "x\" BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION f0(); --";
        let a = apply(&t, create(draft(name, "BEFORE", "INSERT", "BEGIN\n  RETURN NEW;\nEND")));
        assert_eq!(t.one("SELECT string_agg(tgname::text, '|') FROM pg_trigger WHERE tgrelid = 't'::regclass AND NOT tgisinternal"), name, "{var}");
        undo(&t, a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(world(&t), "", "{var}");
        // a WHEN that carries a second statement never reaches the server
        let mut d = draft("w", "BEFORE", "INSERT", "BEGIN\n  RETURN NEW;\nEND");
        d.pg.when = "true) EXECUTE FUNCTION f0(); --".into();
        assert!(run(&t, create(d)).0.is_err(), "{var}");
        assert_eq!(world(&t), "", "{var}");
    });
}

#[test]
fn what_cannot_be_written_back_exactly_is_refused_with_its_reason() {
    every_pg(|l, var| {
        let t = fixture(l, var, "trgrefuse", DDL);
        t.sql("CREATE FUNCTION f_args() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.name := TG_ARGV[0]; RETURN NEW; END $$; \
               CREATE CONSTRAINT TRIGGER ct AFTER INSERT ON t DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION f0(); \
               CREATE TRIGGER withargs BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION f_args('one'); \
               CREATE TRIGGER tt AFTER UPDATE ON t REFERENCING NEW TABLE AS nt FOR EACH STATEMENT EXECUTE FUNCTION f0()");
        let before = world(&t);
        // a constraint trigger: no change, no drop
        let err = run(&t, edit(&t, "ct", |d| d.timing = "BEFORE".into())).0.unwrap_err();
        assert!(err.contains("constraint trigger"), "{var}: {err}");
        let err = run(&t, drop_job(&t, "ct")).0.unwrap_err();
        assert!(err.contains("constraint trigger"), "{var}: {err}");
        // arguments: the trigger part is refused, the function body is not, and the drop replays the arguments
        let err = run(&t, edit(&t, "withargs", |d| d.timing = "AFTER".into())).0.unwrap_err();
        assert!(err.contains("arguments"), "{var}: {err}");
        let a = apply(&t, edit(&t, "withargs", |d| d.body = d.body.replace("TG_ARGV[0]", "TG_ARGV[0] || '!'")));
        undo(&t, a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(world(&t), before, "{var}");
        let a = apply(&t, drop_job(&t, "withargs"));
        assert!(!world(&t).contains("withargs"), "{var}");
        undo(&t, a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(world(&t), before, "{var}: the arguments came back");
        // transition tables
        let err = run(&t, edit(&t, "tt", |d| d.timing = "BEFORE".into())).0.unwrap_err();
        assert!(err.contains("transition"), "{var}: {err}");
        assert_eq!(world(&t), before, "{var}");
        // a built in function is shown, not rewritten, and survives the drop of its trigger
        t.sql("CREATE TRIGGER sup BEFORE UPDATE ON t FOR EACH ROW EXECUTE FUNCTION suppress_redundant_updates_trigger()");
        let before = world(&t);
        let err = run(&t, edit(&t, "sup", |d| d.body = "x".into())).0.unwrap_err();
        assert!(err.contains("internal"), "{var}: {err}");
        let (steps, _) = ddl_pg_trig::preview(&t.schema, "t", &drop_job(&t, "sup")).unwrap();
        assert_eq!(steps.len(), 1, "{var}");
        let a = apply(&t, drop_job(&t, "sup"));
        undo(&t, a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(world(&t), before, "{var}");
        // a partitioned table and its partitions: the clones are not ours to rewrite
        t.sql("CREATE TABLE pt (id integer, v integer) PARTITION BY RANGE (id); CREATE TABLE pt1 PARTITION OF pt FOR VALUES FROM (0) TO (100); CREATE TRIGGER ptrg AFTER INSERT ON pt FOR EACH ROW EXECUTE FUNCTION f0()");
        let err = run_on(t.l, &t.db, &t.schema, "pt", edit_on(&t, "pt", "ptrg", |d| d.timing = "BEFORE".into())).0.unwrap_err();
        assert!(err.contains("partitioned"), "{var}: {err}");
        // PostgreSQL before 14 marks the clone on a partition as internal, so it is not listed at all; later ones list it, and it is
        // refused as inherited
        if t.one("SELECT tgisinternal::text FROM pg_trigger WHERE tgname = 'ptrg' AND tgrelid = 'pt1'::regclass") == "false" {
            let err = run_on(t.l, &t.db, &t.schema, "pt1", edit_on(&t, "pt1", "ptrg", |d| d.timing = "BEFORE".into())).0.unwrap_err();
            assert!(err.contains("inherited"), "{var}: {err}");
            let err = run_on(t.l, &t.db, &t.schema, "pt1", TrigJob::Drop(trigger_on(&t, "pt1", "ptrg"))).0.unwrap_err();
            assert!(err.contains("inherited"), "{var}: {err}");
        } else {
            assert!(structure_of(&t, "pt1").triggers.is_empty(), "{var}: a hidden clone is not listed");
        }
        assert_eq!(t.one("SELECT count(*)::text FROM pg_trigger WHERE tgname = 'ptrg' AND tgrelid IN ('pt'::regclass, 'pt1'::regclass)"), "2", "{var}: the clone is still there");
    });
}

#[test]
fn a_change_made_by_another_session_after_the_read_is_found_under_the_lock_and_stops_the_change() {
    every_pg(|l, var| {
        let t = fixture(l, var, "trgstale", DDL);
        t.sql("CREATE FUNCTION audit_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; CREATE TRIGGER audit BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION audit_fn()");
        // the function is replaced by someone else after the editor read it
        let job = edit(&t, "audit", |d| d.timing = "AFTER".into());
        t.sql("CREATE OR REPLACE FUNCTION audit_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.qty := 5; RETURN NEW; END $$");
        let theirs = world(&t);
        let err = run(&t, job).0.unwrap_err();
        assert!(err.contains("changed since it was read") && err.contains("audit_fn"), "{var}: {err}");
        assert_eq!(world(&t), theirs, "{var}");
        // so is a trigger that was disabled, and a function another trigger started to run
        let job = edit(&t, "audit", |d| d.timing = "AFTER".into());
        t.sql("ALTER TABLE t DISABLE TRIGGER audit");
        let err = run(&t, job).0.unwrap_err();
        assert!(err.contains("changed since it was read") && err.contains("audit"), "{var}: {err}");
        t.sql("ALTER TABLE t ENABLE TRIGGER audit");
        let job = edit(&t, "audit", |d| d.body = "BEGIN\n  RETURN NEW;\nEND".into());
        t.sql("CREATE TRIGGER other AFTER UPDATE ON t FOR EACH ROW EXECUTE FUNCTION audit_fn()");
        let theirs = world(&t);
        assert!(run(&t, job).0.unwrap_err().contains("changed since it was read"), "{var}");
        assert_eq!(world(&t), theirs, "{var}");
        t.sql("DROP TRIGGER other ON t");
        // the undo of a change is refused when the function was changed again, and stays in the list
        let a = apply(&t, edit(&t, "audit", |d| d.body = "BEGIN\n  NEW.name := 'mine';\n  RETURN NEW;\nEND".into()));
        t.sql("CREATE OR REPLACE FUNCTION audit_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.name := 'theirs'; RETURN NEW; END $$");
        let theirs = world(&t);
        let (res, kept) = undo(&t, a);
        assert!(res.unwrap_err().contains("Undo refused") && kept.is_some(), "{var}");
        assert_eq!(world(&t), theirs, "{var}");
        // the undo of a drop is refused when a function of that name exists again
        t.sql("CREATE FUNCTION gone_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; CREATE TRIGGER gone BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION gone_fn()");
        let a = apply(&t, drop_job(&t, "gone"));
        t.sql("CREATE FUNCTION gone_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$");
        let theirs = world(&t);
        let (res, kept) = undo(&t, a);
        assert!(res.unwrap_err().contains("exists again") && kept.is_some(), "{var}");
        assert_eq!(world(&t), theirs, "{var}");
    });
}

#[test]
fn a_lock_another_session_holds_times_out_and_nothing_is_applied() {
    every_pg(|l, var| {
        let t = fixture(l, var, "trglock", DDL);
        t.sql("CREATE FUNCTION audit_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; CREATE TRIGGER audit BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION audit_fn()");
        let before = world(&t);
        // DROP TRIGGER takes ACCESS EXCLUSIVE, which a plain reader of the table stands in the way of
        let job = edit(&t, "audit", |d| d.timing = "AFTER".into());
        let held = hold(&t, "t", "ACCESS SHARE");
        let started = std::time::Instant::now();
        let err = run(&t, job).0.unwrap_err();
        assert!(err.contains("locked by another session") && err.contains("Nothing was changed"), "{var}: {err}");
        assert!(started.elapsed() < std::time::Duration::from_secs(10), "{var}: the wait is bounded");
        drop(held);
        assert_eq!(world(&t), before, "{var}");
        // a change that takes only SHARE ROW EXCLUSIVE waits on a session holding that, and applies when it lets go
        let held = hold(&t, "t", "SHARE ROW EXCLUSIVE");
        let err = run(&t, edit(&t, "audit", |d| d.pg.mode = "D".into())).0.unwrap_err();
        assert!(err.contains("locked by another session"), "{var}: {err}");
        drop(held);
        assert_eq!(world(&t), before, "{var}");
        // and a creation, which takes the same weak lock, is not stopped by a plain reader
        let held = hold(&t, "t", "ACCESS SHARE");
        let a = apply(&t, create(draft("late", "BEFORE", "INSERT", "BEGIN\n  RETURN NEW;\nEND")));
        drop(held);
        undo(&t, a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(world(&t), before, "{var}");
        // the transaction's settings never reached the pooled session
        assert_eq!(t.one("SHOW lock_timeout"), "0", "{var}");
    });
}

/// A login with the TRIGGER privilege on `t` and CREATE on the schema, but no UPDATE, DELETE or TRUNCATE, which a LOCK TABLE needs.
struct Role<'a> {
    t: &'a T<'a>,
    name: String,
}

impl<'a> Role<'a> {
    fn new(t: &'a T<'a>) -> Role<'a> {
        let name = format!("qtrg_{}", std::process::id());
        let r = Role { t, name };
        r.t.sql(&r.cleanup());
        let n = &r.name;
        r.t.sql(&format!("CREATE ROLE {n} LOGIN PASSWORD 'quarry'; GRANT USAGE, CREATE ON SCHEMA {} TO {n}; GRANT SELECT, TRIGGER ON t TO {n}", r.t.schema));
        r
    }

    fn cleanup(&self) -> String {
        let n = &self.name;
        format!("DO $$ BEGIN IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{n}') THEN EXECUTE 'DROP OWNED BY {n}'; EXECUTE 'DROP ROLE {n}'; END IF; END $$")
    }
}

impl Drop for Role<'_> {
    fn drop(&mut self) {
        let _ = self.t.l.w.rt.block_on(async { sqlx::raw_sql(&self.cleanup()).execute(&self.t.pool).await });
    }
}

#[test]
fn a_role_without_the_privileges_a_lock_needs_still_creates_a_trigger() {
    every_pg(|l, var| {
        let t = fixture(l, var, "trgrole", DDL);
        let role = Role::new(&t);
        let (scheme, rest) = l.uri.split_once("://").unwrap();
        let (_, host) = rest.split_once('@').unwrap();
        let r = crate::db::sql::testing::live_pg_at(format!("{scheme}://{}:quarry@{host}", role.name));
        let db = r.quarry_test();
        assert_eq!(t.one(&format!("SELECT has_table_privilege('{}', '{}.t', 'UPDATE')::text", role.name, t.schema)), "false", "{var}");
        // CREATE TRIGGER needs TRIGGER on the table and CREATE FUNCTION needs CREATE on the schema; the explicit lock is skipped
        let (res, undo, _) = run_on(&r, &db, &t.schema, "t", create(draft("mine", "BEFORE", "INSERT", "BEGIN\n  RETURN NEW;\nEND")));
        res.unwrap_or_else(|x| panic!("{var}: create as the role: {x}"));
        assert_eq!(t.one("SELECT count(*)::text FROM pg_trigger WHERE tgname = 'mine' AND tgrelid = 't'::regclass"), "1", "{var}");
        // dropping needs the table's owner: the undo fails with the server's reason, ran nothing and stays in the list
        let before = world(&t);
        r.w.sql_undo(db.clone(), undo.unwrap(), Vec::new(), false);
        let Msg::SqlAltered { res, undo: kept, .. } = r.next() else { panic!("expected SqlAltered") };
        let err = res.unwrap_err();
        assert!(err.contains("owner") && kept.is_some(), "{var}: {err}");
        assert_eq!(world(&t), before, "{var}");
        // as the owner it goes
        t.l.w.sql_undo(t.db.clone(), kept.unwrap(), Vec::new(), false);
        let Msg::SqlAltered { res, .. } = t.l.next() else { panic!("expected SqlAltered") };
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(t.one("SELECT count(*)::text FROM pg_trigger WHERE tgname = 'mine' AND tgrelid = 't'::regclass"), "0", "{var}");
    });
}

#[test]
fn a_trigger_is_created_and_undone_through_a_pooler() {
    let Some(l) = live_pgbouncer() else { return };
    let t = fixture(&l, "QUARRY_TEST_PGBOUNCER", "trgpgb", DDL);
    assert!(t.db.pooler());
    let empty = world(&t);
    let a = apply(&t, create(draft("audit", "BEFORE", "INSERT", "BEGIN\n  NEW.name := upper(NEW.name);\n  RETURN NEW;\nEND")));
    t.sql("INSERT INTO t VALUES (2, 'x', 2)");
    assert_eq!(t.one("SELECT name FROM t WHERE id = 2"), "X");
    t.sql("DELETE FROM t WHERE id = 2");
    let made = world(&t);
    let b = apply(&t, edit(&t, "audit", |d| {
        d.timing = "AFTER".into();
        d.body = "BEGIN\n  RETURN NEW;\nEND".into();
    }));
    undo(&t, b).0.unwrap_or_else(|x| panic!("undo of the edit: {x}"));
    assert_eq!(world(&t), made);
    undo(&t, a).0.unwrap_or_else(|x| panic!("undo of the create: {x}"));
    assert_eq!(world(&t), empty);
    // the settings of the transaction never reached the session of another client of the pooler
    assert_eq!(t.one("SHOW lock_timeout"), "0");
    assert_eq!(t.one("SHOW check_function_bodies"), "on");
}

#[test]
fn a_when_with_spaces_line_breaks_and_a_backslash_survives_edits_of_the_rest() {
    every_pg(|l, var| {
        let t = fixture(l, var, "trgwhen", DDL);
        t.sql("CREATE FUNCTION w_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; \
               CREATE TRIGGER sp BEFORE INSERT ON t FOR EACH ROW WHEN (NEW.name = 'x  y') EXECUTE FUNCTION w_fn(); \
               CREATE TRIGGER nl AFTER INSERT ON t FOR EACH ROW WHEN (NEW.name = E'p\\nq') EXECUTE FUNCTION w_fn(); \
               CREATE TRIGGER bs AFTER DELETE ON t FOR EACH ROW WHEN (OLD.name = 'a\\') EXECUTE FUNCTION w_fn()");
        let def = |n: &str| t.one(&format!("SELECT pg_get_triggerdef(oid) FROM pg_trigger WHERE tgname = '{n}' AND tgrelid = 't'::regclass"));
        let before = world(&t);
        // the double space stays when the timing changes
        let a = apply(&t, edit(&t, "sp", |d| d.timing = "AFTER".into()));
        assert!(def("sp").contains("AFTER INSERT") && def("sp").contains("'x  y'::text"), "{var}: {}", def("sp"));
        undo(&t, a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(world(&t), before, "{var}");
        // a line break cannot be shown in the field: the trigger part is refused, the body and the switch still work
        let err = run(&t, edit(&t, "nl", |d| d.timing = "BEFORE".into())).0.unwrap_err();
        assert!(err.contains("several lines"), "{var}: {err}");
        let a = apply(&t, edit(&t, "nl", |d| d.body = "BEGIN\n  RETURN NEW;\nEND -- x".into()));
        undo(&t, a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        let a = apply(&t, edit(&t, "nl", |d| d.pg.mode = "D".into()));
        undo(&t, a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        // a backslash in a literal blocks nothing: the server prints it as 'a\'::text
        let a = apply(&t, edit(&t, "bs", |d| d.pg.mode = "D".into()));
        undo(&t, a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        let a = apply(&t, edit(&t, "bs", |d| d.body = "BEGIN\n  RETURN NEW;\nEND -- y".into()));
        undo(&t, a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        let a = apply(&t, edit(&t, "bs", |d| d.timing = "BEFORE".into()));
        assert!(def("bs").contains("BEFORE DELETE") && def("bs").contains("'a\\'::text"), "{var}: {}", def("bs"));
        undo(&t, a).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(world(&t), before, "{var}");
    });
}

#[test]
fn a_column_with_a_comma_in_its_name_keeps_a_body_only_save_from_rewriting_the_trigger() {
    every_pg(|l, var| {
        let t = fixture(l, var, "trgcomma", DDL);
        t.sql("CREATE TABLE c (\"a,b\" integer, a integer, b integer); \
               CREATE FUNCTION c_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; \
               CREATE TRIGGER ct BEFORE UPDATE OF \"a,b\" ON c FOR EACH ROW EXECUTE FUNCTION c_fn()");
        let trig = || t.one("SELECT pg_get_triggerdef(oid) FROM pg_trigger WHERE tgname = 'ct' AND tgrelid = 'c'::regclass");
        let was = trig();
        assert!(was.contains("UPDATE OF \"a,b\" ON"), "{var}: {was}");
        let job = edit_on(&t, "c", "ct", |d| d.body = "BEGIN\n  RETURN NEW;\nEND -- x".into());
        let (steps, _) = ddl_pg_trig::preview(&t.schema, "c", &job).unwrap();
        assert_eq!(steps.len(), 1, "{var}: the trigger part is untouched, so only the function is replaced: {steps:?}");
        let (res, undo_a, _) = run_on(t.l, &t.db, &t.schema, "c", job);
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(trig(), was, "{var}");
        t.undo(undo_a.unwrap()).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        // a change of the timing writes the name back whole
        let (res, undo_a, _) = run_on(t.l, &t.db, &t.schema, "c", edit_on(&t, "c", "ct", |d| d.timing = "AFTER".into()));
        res.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(trig(), was.replace("BEFORE", "AFTER"), "{var}");
        t.undo(undo_a.unwrap()).0.unwrap_or_else(|x| panic!("{var}: {x}"));
        assert_eq!(trig(), was, "{var}");
    });
}

/// A schema made for one test, dropped with it.
struct Extra<'a> {
    t: &'a T<'a>,
    name: String,
}

impl<'a> Extra<'a> {
    fn new(t: &'a T<'a>, tag: &str) -> Extra<'a> {
        let e = Extra { t, name: format!("{}_{tag}", t.schema) };
        e.t.sql(&format!("DROP SCHEMA IF EXISTS {} CASCADE; CREATE SCHEMA {}", e.name, e.name));
        e
    }
}

impl Drop for Extra<'_> {
    fn drop(&mut self) {
        let _ = self.t.l.w.rt.block_on(async { sqlx::raw_sql(&format!("DROP SCHEMA IF EXISTS {} CASCADE", self.name)).execute(&self.t.pool).await });
    }
}

#[test]
fn a_function_in_another_schema_is_edited_dropped_and_put_back_where_it_was() {
    every_pg(|l, var| {
        let t = fixture(l, var, "trgother", DDL);
        let x = Extra::new(&t, "fn");
        t.sql(&format!("CREATE FUNCTION {s}.o_fn() RETURNS trigger LANGUAGE plpgsql AS $$\nBEGIN\n  RETURN NEW;\nEND\n$$; \
                        CREATE TRIGGER ot BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION {s}.o_fn(); COMMENT ON FUNCTION {s}.o_fn() IS 'elsewhere'", s = x.name));
        let funcs = || t.one(&format!("SELECT COALESCE(string_agg(pg_get_functiondef(p.oid) || '|' || COALESCE(obj_description(p.oid, 'pg_proc'), ''), E'\\n'), '') FROM pg_proc p WHERE p.pronamespace = '{}'::regnamespace", x.name));
        let before = funcs();
        let trig_before = world(&t);
        assert!(before.contains(&format!("{}.o_fn()", x.name)), "{var}: {before}");
        let a = apply(&t, edit(&t, "ot", |d| d.body = "BEGIN\n  NEW.qty := 5;\n  RETURN NEW;\nEND".into()));
        assert_ne!(funcs(), before, "{var}");
        undo(&t, a).0.unwrap_or_else(|e| panic!("{var}: {e}"));
        assert_eq!(funcs(), before, "{var}");
        // a changed trigger still runs the function in its own schema
        let a = apply(&t, edit(&t, "ot", |d| d.timing = "AFTER".into()));
        assert!(t.one("SELECT pg_get_triggerdef(oid) FROM pg_trigger WHERE tgname = 'ot' AND tgrelid = 't'::regclass").contains(&format!("{}.o_fn()", x.name)), "{var}: {}", t.one("SELECT pg_get_triggerdef(oid) FROM pg_trigger WHERE tgname = 'ot' AND tgrelid = 't'::regclass"));
        undo(&t, a).0.unwrap_or_else(|e| panic!("{var}: {e}"));
        let a = apply(&t, drop_job(&t, "ot"));
        assert_eq!(funcs(), "", "{var}: the function went with its trigger");
        undo(&t, a).0.unwrap_or_else(|e| panic!("{var}: {e}"));
        assert_eq!(funcs(), before, "{var}");
        assert_eq!(world(&t), trig_before, "{var}");
    });
}

#[test]
fn a_function_that_would_come_back_with_other_privileges_is_kept() {
    every_pg(|l, var| {
        let t = fixture(l, var, "trgdefacl", DDL);
        t.sql("CREATE FUNCTION d_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; CREATE TRIGGER dt BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION d_fn()");
        let (steps, _) = ddl_pg_trig::preview(&t.schema, "t", &drop_job(&t, "dt")).unwrap();
        assert_eq!(steps.len(), 2, "{var}: a function with no ACL and no default privileges comes back as it was");
        // a function made now would get this ACL, while the one dropped has none
        t.sql(&format!("ALTER DEFAULT PRIVILEGES IN SCHEMA {} GRANT EXECUTE ON FUNCTIONS TO PUBLIC", t.schema));
        let (steps, warnings) = ddl_pg_trig::preview(&t.schema, "t", &drop_job(&t, "dt")).unwrap();
        assert_eq!(steps.len(), 1, "{var}: {steps:?}");
        assert!(warnings[0].contains("default privileges"), "{var}: {warnings:?}");
    });
}

/// A login that owns `t` and the function in the extra schema but has no CREATE on either schema, which putting the function back needs.
struct Owner<'a> {
    t: &'a T<'a>,
    name: String,
}

impl<'a> Owner<'a> {
    fn new(t: &'a T<'a>, other: &str) -> Owner<'a> {
        let o = Owner { t, name: format!("qtrgo_{}", std::process::id()) };
        o.t.sql(&o.cleanup());
        let (n, s) = (&o.name, &t.schema);
        // a new owner has to hold CREATE on the schema while it is handed the object, and loses it after
        o.t.sql(&format!("CREATE ROLE {n} LOGIN PASSWORD 'quarry'; GRANT USAGE, CREATE ON SCHEMA {s}, {other} TO {n}; \
                          ALTER TABLE t OWNER TO {n}; ALTER FUNCTION {other}.o_fn() OWNER TO {n}; REVOKE CREATE ON SCHEMA {s}, {other} FROM {n}"));
        o
    }

    fn cleanup(&self) -> String {
        let n = &self.name;
        format!("DO $$ BEGIN IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{n}') THEN EXECUTE 'DROP OWNED BY {n}'; EXECUTE 'DROP ROLE {n}'; END IF; END $$")
    }
}

impl Drop for Owner<'_> {
    fn drop(&mut self) {
        let _ = self.t.l.w.rt.block_on(async { sqlx::raw_sql(&self.cleanup()).execute(&self.t.pool).await });
    }
}

#[test]
fn a_function_the_role_cannot_create_again_is_kept_so_the_undo_of_a_drop_works() {
    every_pg(|l, var| {
        let t = fixture(l, var, "trgnocreate", DDL);
        let x = Extra::new(&t, "fn");
        t.sql(&format!("CREATE FUNCTION {s}.o_fn() RETURNS trigger LANGUAGE plpgsql AS $$\nBEGIN\n  RETURN NEW;\nEND\n$$; CREATE TRIGGER ot BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION {s}.o_fn()", s = x.name));
        let owner = Owner::new(&t, &x.name);
        let (scheme, rest) = l.uri.split_once("://").unwrap();
        let (_, host) = rest.split_once('@').unwrap();
        let r = crate::db::sql::testing::live_pg_at(format!("{scheme}://{}:quarry@{host}", owner.name));
        let db = r.quarry_test();
        let as_owner = |job: &dyn Fn(&Trigger) -> TrigJob| {
            r.w.sql_structure(9, db.clone(), t.schema.clone(), "t".into());
            let Msg::SqlStructure { res, .. } = r.next() else { panic!("expected SqlStructure") };
            job(&res.unwrap().triggers.into_iter().find(|x| x.name == "ot").unwrap())
        };
        assert_eq!(t.one(&format!("SELECT has_schema_privilege('{}', '{}', 'CREATE')::text", owner.name, x.name)), "false", "{var}");
        let drop = as_owner(&|tr| TrigJob::Drop(tr.clone()));
        let (steps, warnings) = ddl_pg_trig::preview(&t.schema, "t", &drop).unwrap();
        assert_eq!(steps.len(), 1, "{var}: the function stays: {steps:?}");
        assert!(warnings[0].contains("CREATE"), "{var}: {warnings:?}");
        // a new body is a CREATE OR REPLACE, which needs the same privilege: it is refused with the reason
        let body = as_owner(&|tr| {
            let mut d = ddl_pg_trig::def_of(tr);
            d.body = "BEGIN\n  RETURN NEW;\nEND -- x".into();
            TrigJob::Edit { was: tr.clone(), save: TrigSave { def: d, definer: None, order: None } }
        });
        let err = ddl_pg_trig::preview(&t.schema, "t", &body).unwrap_err();
        assert!(err.contains("CREATE privilege"), "{var}: {err}");
        let (res, undo_a, _) = run_on(&r, &db, &t.schema, "t", drop);
        res.unwrap_or_else(|e| panic!("{var}: {e}"));
        assert_eq!(t.one(&format!("SELECT count(*)::text FROM pg_proc WHERE proname = 'o_fn' AND pronamespace = '{}'::regnamespace", x.name)), "1", "{var}");
        r.w.sql_undo(db.clone(), undo_a.unwrap(), Vec::new(), false);
        let Msg::SqlAltered { res, .. } = r.next() else { panic!("expected SqlAltered") };
        res.unwrap_or_else(|e| panic!("{var}: the undo: {e}"));
        assert_eq!(t.one("SELECT count(*)::text FROM pg_trigger WHERE tgname = 'ot' AND tgrelid = 't'::regclass"), "1", "{var}");
    });
}

#[test]
fn a_trigger_on_a_view_cannot_be_switched_off() {
    every_pg(|l, var| {
        let t = fixture(l, var, "trgview", DDL);
        t.sql("CREATE VIEW v AS SELECT * FROM t; CREATE FUNCTION v_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; \
               CREATE TRIGGER vt INSTEAD OF INSERT ON v FOR EACH ROW EXECUTE FUNCTION v_fn()");
        let job = edit_on(&t, "v", "vt", |d| d.pg.mode = "D".into());
        let err = ddl_pg_trig::preview(&t.schema, "v", &job).unwrap_err();
        assert!(err.contains("view"), "{var}: {err}");
        assert_eq!(t.one("SELECT tgenabled::text FROM pg_trigger WHERE tgname = 'vt' AND tgrelid = 'v'::regclass"), "O", "{var}");
    });
}
