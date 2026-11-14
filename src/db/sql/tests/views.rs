//! Live tests of creating, editing and undoing SQL views, on every MySQL and MariaDB server named by QUARRY_TEST_MYSQL and
//! QUARRY_TEST_MARIADB and every PostgreSQL server named by QUARRY_TEST_POSTGRES and QUARRY_TEST_POSTGRES13. Each test works in a
//! database or schema of its own.

use crate::db::sql::testing::{every_pg, every_server, unique_name, Live, LivePg};
use crate::db::sql::view::{ViewDef, ViewDone, ViewSpec};
use crate::db::sql::write::SqlUndo;
use crate::db::{Msg, Res, UndoAction, UndoKind};

struct Saved {
    kind: ViewDone,
    res: Res<String>,
    undo: Option<UndoAction>,
    need_drop: bool,
    name: String,
}

fn saved(m: Msg) -> Saved {
    let Msg::SqlViewSaved { kind, res, undo, need_drop, name, .. } = m else { panic!("expected SqlViewSaved") };
    Saved { kind, res, undo, need_drop, name }
}

fn spec(schema: &str, name: &str, body: &str, replace: bool) -> ViewSpec {
    ViewSpec { def: ViewDef { schema: schema.into(), name: name.into(), body: body.into(), ..Default::default() }, replace, drop_first: false }
}

fn is_view_undo(u: &UndoAction, created: bool) -> bool {
    matches!(&u.kind, UndoKind::Sql { undo: SqlUndo::View(v), .. } if v.created == created)
}

impl Live {
    fn view_def(&self, schema: &str, name: &str) -> Res<ViewDef> {
        self.w.sql_view_def(1, self.db.clone(), schema.into(), name.into());
        let Msg::SqlViewDef { res, .. } = self.rx.recv_timeout(std::time::Duration::from_secs(20)).expect("worker answer") else { panic!("expected SqlViewDef") };
        res
    }

    fn save_view(&self, s: ViewSpec) -> Saved {
        self.w.sql_save_view(1, self.db.clone(), s, false);
        saved(self.rx.recv_timeout(std::time::Duration::from_secs(20)).expect("worker answer"))
    }

    fn undo_view(&self, a: UndoAction) -> Saved {
        self.w.sql_undo_view(self.db.clone(), a, false);
        saved(self.rx.recv_timeout(std::time::Duration::from_secs(20)).expect("worker answer"))
    }

    fn view_has_column(&self, schema: &str, view: &str, col: &str) -> bool {
        self.one(&format!("SELECT COUNT(*) FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = '{schema}' AND TABLE_NAME = '{view}' AND COLUMN_NAME = '{col}'")) == "1"
    }

    fn view_exists(&self, schema: &str, view: &str) -> bool {
        self.one(&format!("SELECT COUNT(*) FROM information_schema.VIEWS WHERE TABLE_SCHEMA = '{schema}' AND TABLE_NAME = '{view}'")) == "1"
    }
}

#[test]
fn live_a_mysql_view_is_made_read_back_edited_and_undone() {
    every_server(|l, var| {
        let db = unique_name("qvw");
        l.sql(&format!("CREATE DATABASE `{db}`; CREATE TABLE `{db}`.t (id INT PRIMARY KEY, n INT); INSERT INTO `{db}`.t VALUES (1, 3), (2, 9)"));
        let made = l.save_view(spec(&db, "big", &format!("SELECT id FROM `{db}`.t WHERE n > 5;"), false));
        assert!(made.res.is_ok() && made.kind == ViewDone::Created, "{var}: {:?}", made.res);
        let create_undo = made.undo.expect("a create keeps an undo");
        assert!(is_view_undo(&create_undo, true) && l.view_exists(&db, "big"), "{var}");
        assert_eq!(l.one(&format!("SELECT COUNT(*) FROM `{db}`.big")), "1", "{var}");

        let def = l.view_def(&db, "big").unwrap_or_else(|e| panic!("{var}: {e}"));
        assert_eq!((def.algorithm.as_str(), def.security.as_str()), ("UNDEFINED", "DEFINER"), "{var}");
        assert!(def.definer.is_some() && def.body.to_lowercase().starts_with("select"), "{var}: {def:?}");

        // An edit keeps the definer and the options it read, and adds a column.
        let mut edit = ViewSpec { def: def.clone(), replace: true, drop_first: false };
        edit.def.body = format!("SELECT id, n FROM `{db}`.t WHERE n > 5");
        edit.def.algorithm = "MERGE".into();
        let edited = l.save_view(edit);
        assert!(edited.res.is_ok() && edited.kind == ViewDone::Replaced, "{var}: {:?}", edited.res);
        let edit_undo = edited.undo.expect("an edit keeps an undo");
        assert!(is_view_undo(&edit_undo, false) && l.view_has_column(&db, "big", "n"), "{var}");
        let now = l.view_def(&db, "big").unwrap();
        assert_eq!((now.algorithm.as_str(), now.definer.clone()), ("MERGE", def.definer.clone()), "{var}");

        let back = l.undo_view(edit_undo);
        assert!(back.res.is_ok() && back.kind == ViewDone::Restored, "{var}: {:?}", back.res);
        assert!(!l.view_has_column(&db, "big", "n") && l.view_exists(&db, "big"), "{var}");

        // Someone else changed the view after the create, so the undo of the create refuses and hands the entry back.
        l.sql(&format!("CREATE OR REPLACE VIEW `{db}`.big AS SELECT id, n FROM `{db}`.t"));
        let refused = l.undo_view(create_undo);
        assert!(refused.res.unwrap_err().contains("changed after this") && refused.undo.is_some(), "{var}");
        assert!(l.view_exists(&db, "big"), "{var}");

        // A body that is not one read never reaches the server.
        let bad = l.save_view(spec(&db, "bad", "SELECT 1; DROP TABLE t", false));
        assert!(bad.res.is_err() && !l.view_exists(&db, "bad") && l.one(&format!("SELECT COUNT(*) FROM `{db}`.t")) == "2", "{var}");
        let gone = l.view_def(&db, "t");
        assert!(gone.is_err(), "{var}: a table is no view");
        l.sql(&format!("DROP DATABASE `{db}`"));
    });
}

#[test]
fn live_the_undo_of_a_mysql_create_drops_the_view_it_made() {
    every_server(|l, var| {
        let db = unique_name("qvw");
        l.sql(&format!("CREATE DATABASE `{db}`; CREATE TABLE `{db}`.t (id INT PRIMARY KEY)"));
        let made = l.save_view(spec(&db, "v", &format!("SELECT * FROM `{db}`.t"), false));
        let gone = l.undo_view(made.undo.unwrap());
        assert!(gone.res.is_ok() && gone.kind == ViewDone::Removed && gone.undo.is_none() && !l.view_exists(&db, "v"), "{var}: {:?}", gone.res);
        l.sql(&format!("DROP DATABASE `{db}`"));
    });
}

impl LivePg {
    fn view_save(&self, db: &crate::db::sql::Db, s: ViewSpec) -> Saved {
        self.w.sql_save_view(1, db.clone(), s, false);
        saved(self.next())
    }

    fn view_undo(&self, db: &crate::db::sql::Db, a: UndoAction) -> Saved {
        self.w.sql_undo_view(db.clone(), a, false);
        saved(self.next())
    }

    fn view_read(&self, db: &crate::db::sql::Db, schema: &str, name: &str) -> Res<ViewDef> {
        self.w.sql_view_def(1, db.clone(), schema.into(), name.into());
        let Msg::SqlViewDef { res, .. } = self.next() else { panic!("expected SqlViewDef") };
        res
    }
}

#[test]
fn live_a_postgresql_view_is_edited_with_replace_and_with_drop_when_the_columns_change() {
    every_pg(|l, var| {
        let schema = unique_name("qvw");
        let (db, pool) = l.fresh_schema(&schema, "CREATE TABLE t (id integer PRIMARY KEY, n integer); INSERT INTO t VALUES (1, 3), (2, 9); CREATE MATERIALIZED VIEW mv AS SELECT id FROM t");
        let exists = |name: &str| l.qone(&pool, &format!("SELECT count(*) FROM pg_class c JOIN pg_namespace s ON s.oid = c.relnamespace WHERE s.nspname = '{schema}' AND c.relname = '{name}' AND c.relkind = 'v'")) == "1";
        let cols = |name: &str| l.qone(&pool, &format!("SELECT string_agg(attname, ',' ORDER BY attnum) FROM pg_attribute WHERE attrelid = '{schema}.{name}'::regclass AND attnum > 0"));

        let mut base = spec(&schema, "big", "SELECT id FROM t WHERE n > 5;", false);
        base.def.options = vec!["security_barrier=true".into()];
        let made = l.view_save(&db, base);
        assert!(made.res.is_ok() && made.kind == ViewDone::Created && made.name == "big", "{var}: {:?}", made.res);
        let create_undo = made.undo.unwrap();
        assert!(exists("big") && cols("big") == "id", "{var}");

        let def = l.view_read(&db, &schema, "big").unwrap_or_else(|e| panic!("{var}: {e}"));
        assert!(!def.materialized && def.options == ["security_barrier=true"] && def.body.contains("n > 5"), "{var}: {def:?}");

        // A column added at the end goes through CREATE OR REPLACE, which would reset the options if they were not written back.
        let mut add = ViewSpec { def: def.clone(), replace: true, drop_first: false };
        add.def.body = "SELECT id, n FROM t WHERE n > 5".into();
        let added = l.view_save(&db, add);
        assert!(added.res.is_ok() && cols("big") == "id,n", "{var}: {:?}", added.res);
        assert_eq!(l.view_read(&db, &schema, "big").unwrap().options, ["security_barrier=true"], "{var}");

        // A column taken away is refused, and the answer says so, with nothing changed.
        let mut cut = ViewSpec { def: def.clone(), replace: true, drop_first: false };
        cut.def.body = "SELECT n FROM t".into();
        let refused = l.view_save(&db, cut.clone());
        assert!(refused.need_drop && refused.res.is_err() && cols("big") == "id,n", "{var}");
        cut.drop_first = true;
        let dropped = l.view_save(&db, cut);
        assert!(dropped.res.is_ok() && cols("big") == "n", "{var}: {:?}", dropped.res);

        // Undoing it puts the old columns back by dropping and making the view again.
        let back = l.view_undo(&db, dropped.undo.unwrap());
        assert!(back.res.is_ok() && cols("big") == "id,n", "{var}: {:?}", back.res);

        // The undo of the add cannot use CREATE OR REPLACE either, and falls back the same way.
        let back = l.view_undo(&db, added.undo.unwrap());
        assert!(back.res.is_ok() && cols("big") == "id", "{var}: {:?}", back.res);
        // The view reads as the create left it, so the undo of the create drops it.
        let gone = l.view_undo(&db, create_undo);
        assert!(gone.res.is_ok() && gone.kind == ViewDone::Removed && !exists("big"), "{var}: {:?}", gone.res);

        // A materialized view cannot be replaced, and its definition reads as such.
        let mv = l.view_read(&db, &schema, "mv").unwrap();
        assert!(mv.materialized, "{var}");
        let mut edit = ViewSpec { def: mv, replace: true, drop_first: false };
        edit.def.body = "SELECT id FROM t".into();
        assert!(l.view_save(&db, edit).res.unwrap_err().contains("materialized"), "{var}");

        // Not one read: refused before the server sees it.
        assert!(l.view_save(&db, spec(&schema, "bad", "SELECT 1; DROP TABLE t", false)).res.is_err() && l.qone(&pool, &format!("SELECT count(*) FROM {schema}.t")) == "2", "{var}");
        l.qsql(&pool, &format!("DROP SCHEMA {schema} CASCADE"));
    });
}
