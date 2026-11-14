use super::*;
use crate::db::sql::structure::Structure;
use std::sync::mpsc::Receiver;
use std::time::Duration;

fn recv(rx: &Receiver<Msg>) -> Msg {
    rx.recv_timeout(Duration::from_secs(30)).expect("no answer from the worker")
}

/// Everything about the structure that an ALTER and its undo must leave as it was.
pub(super) fn shape(st: &Structure) -> Vec<String> {
    let cols = st.columns.iter().map(|c| format!("col {}|{}|{}|{:?}|{}|{}|{:?}|{}|{:?}|{:?}", c.name, c.full_type, c.nullable, c.default, c.default_expr, c.extra, c.collation, c.comment, c.srid, c.check));
    let idx = st.indexes.iter().map(|i| format!("idx {}|{}|{}|{}|{}|{}", i.name, i.unique, i.cols.iter().map(|c| format!("{}:{:?}:{}:{:?}", c.name, c.prefix, c.desc, c.expr)).collect::<Vec<_>>().join(","), i.kind, i.comment, i.invisible));
    let checks = st.checks.iter().map(super::sim::check_line);
    cols.chain(idx).chain(checks).collect()
}

struct Env {
    w: Worker,
    rx: Receiver<Msg>,
    db: Db,
    pool: sqlx::MySqlPool,
    n: u64,
}

impl Env {
    fn exec(&self, sql: &str) {
        self.w.rt.block_on(async { sqlx::raw_sql(sql).execute(&self.pool).await.unwrap_or_else(|err| panic!("{sql}: {err}")) });
    }

    fn structure(&mut self) -> Structure {
        self.n += 1;
        self.w.sql_structure(self.n, self.db.clone(), "quarry_ddl".into(), "t".into());
        match recv(&self.rx) {
            Msg::SqlStructure { res, .. } => res.unwrap(),
            _ => panic!("expected SqlStructure"),
        }
    }

    fn fresh(&self) {
        self.exec("DROP TABLE IF EXISTS quarry_ddl.t");
        self.exec("CREATE TABLE quarry_ddl.t (
                id INT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
                name VARCHAR(50) NOT NULL DEFAULT 'x',
                note TEXT NULL,
                price DECIMAL(10,2) NOT NULL DEFAULT 0.00 COMMENT 'p',
                created DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
                kind ENUM('a','b') NULL,
                KEY idx_name (name),
                UNIQUE KEY uq_price (price, name)
            ) COMMENT='ddl test'");
        self.exec("INSERT INTO quarry_ddl.t (name, note, price) VALUES ('one', 'n1', 1.50), ('two', NULL, 2.00)");
    }

    /// Applies the changes, checks `after` on the new structure, undoes and checks the old shape is back.
    fn round_trip(&mut self, label: &str, changes: Vec<Change>, after: impl FnOnce(&Structure)) {
        self.fresh();
        self.round_trip_here(label, changes, after);
    }

    /// `round_trip` on the table as it is, for a test that sets it up first.
    fn round_trip_here(&mut self, label: &str, changes: Vec<Change>, after: impl FnOnce(&Structure)) {
        let before = self.structure();
        let (d, v) = (self.db.dialect, self.db.version.clone());
        let m = Mode::of(&self.db.sql_mode);
        let p = plan(d, &v, m, "t", &before, &changes).unwrap_or_else(|e| panic!("{label}: {e}"));
        let sql = p.sql();
        let mut back = p.undo.unwrap_or_else(|e| panic!("{label}: {e}"));
        back.label = label.into();
        self.w.sql_alter(1, self.db.clone(), "quarry_ddl".into(), "t".into(), sql.clone(), m, Some(back), false, "done".into());
        let Msg::SqlAltered { res, undo, .. } = recv(&self.rx) else { panic!("expected SqlAltered") };
        res.unwrap_or_else(|e| panic!("{label}: {e}\n{sql}"));
        let changed = self.structure();
        assert_ne!(shape(&before), shape(&changed), "{label}: the change did nothing\n{sql}");
        after(&changed);
        let undo = undo.expect("an undo");
        self.w.sql_undo(self.db.clone(), undo, Vec::new(), false);
        let Msg::SqlAltered { res, .. } = recv(&self.rx) else { panic!("expected SqlAltered") };
        res.unwrap_or_else(|e| panic!("{label} undo: {e}"));
        assert_eq!(shape(&self.structure()), shape(&before), "{label}: undo did not restore the table\n{sql}");
    }
}

fn start(var: &str) -> Option<Env> {
    let uri = std::env::var(var).ok()?;
    let (tx, rx) = std::sync::mpsc::channel();
    let w = Worker::new(tx, eframe::egui::Context::default());
    let pool = w.rt.block_on(async {
        let p = sqlx::MySqlPool::connect(&uri).await.unwrap();
        for sql in ["DROP DATABASE IF EXISTS quarry_ddl", "CREATE DATABASE quarry_ddl"] {
            sqlx::raw_sql(sql).execute(&p).await.unwrap();
        }
        p
    });
    w.sql_connect(1, uri, Duration::from_secs(30), crate::db::Net::default());
    let Msg::SqlConnected { res: Ok((db, ..)), .. } = recv(&rx) else { panic!("expected SqlConnected") };
    Some(Env { w, rx, db, pool, n: 100 })
}

fn live(var: &str) {
    let Some(mut e) = start(var) else { return };
    let col = |st: &Structure, n: &str| st.columns.iter().find(|c| c.name == n).unwrap_or_else(|| panic!("no column {n}")).clone();
    let names = |st: &Structure| st.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
    let cdef = |e: &mut Env, n: &str| ColDef::from_column(&col(&e.structure(), n)).unwrap();

    e.fresh();
    let mut qty = ColDef::new("qty", "INT");
    qty.nullable = false;
    qty.default = Dv::Value("1".into());
    qty.comment = "how many".into();
    e.round_trip("add column", vec![Change::Add { def: qty.clone(), pos: Pos::After("name".into()) }], |st| {
        assert_eq!(names(st), ["id", "name", "qty", "note", "price", "created", "kind"]);
        let q = col(st, "qty");
        assert!(q.full_type.starts_with("int"));
        assert_eq!((q.nullable, q.default.as_deref(), q.comment.as_str()), (false, Some("1"), "how many"));
    });
    e.round_trip("add column first", vec![Change::Add { def: ColDef { args: "20".into(), ..ColDef::new("first_col", "VARCHAR") }, pos: Pos::First }], |st| assert_eq!(st.columns[0].name, "first_col"));

    e.fresh();
    let mut price = cdef(&mut e, "price");
    price.args = "12,3".into();
    price.comment = "net".into();
    price.default = Dv::Value("0.500".into());
    e.round_trip("modify column", vec![Change::Modify { from: "price".into(), def: price, pos: Pos::Keep }], |st| {
        let p = col(st, "price");
        assert_eq!((p.full_type.as_str(), p.comment.as_str(), p.default.as_deref()), ("decimal(12,3)", "net", Some("0.500")));
    });

    let mut title = cdef(&mut e, "name");
    title.name = "title".into();
    e.round_trip("rename column", vec![Change::Modify { from: "name".into(), def: title, pos: Pos::Keep }], |st| {
        assert!(names(st).contains(&"title".to_string()));
        // The indexes follow the new name.
        assert!(st.indexes.iter().any(|i| i.name == "idx_name" && i.cols[0].name == "title"));
    });

    let kind = cdef(&mut e, "kind");
    e.round_trip("change enum values, default and collation", vec![Change::Modify { from: "kind".into(), def: ColDef { args: values_text(&["a".into(), "b".into(), "it's".into(), "back\\slash".into()]), default: Dv::Value("it's".into()), collation: Some("utf8mb4_bin".into()), ..kind }, pos: Pos::Keep }], |st| {
        let k = col(st, "kind");
        assert_eq!(k.default.as_deref(), Some("it's"));
        assert!(k.full_type.contains("back") && k.full_type.contains("slash") && !k.full_type.contains("backslash"), "{}", k.full_type);
        assert_eq!(k.collation.as_deref(), Some("utf8mb4_bin"));
    });

    let ts = cdef(&mut e, "created");
    e.round_trip("on update", vec![Change::Modify { from: "created".into(), def: ColDef { on_update: Some("CURRENT_TIMESTAMP".into()), ..ts }, pos: Pos::Keep }], |st| assert!(col(st, "created").extra.to_lowercase().contains("on update")));

    e.round_trip("drop column", vec![Change::Drop("note".into())], |st| assert!(!names(st).contains(&"note".to_string())));
    // Dropping a column that sits in a one-column index and in a composite one: undo brings both indexes back.
    e.round_trip("drop indexed column", vec![Change::Drop("name".into())], |st| {
        assert!(!st.indexes.iter().any(|i| i.name == "idx_name"));
        assert_eq!(st.indexes.iter().find(|i| i.name == "uq_price").map(|i| i.cols.len()), Some(1));
    });

    let price_def = cdef(&mut e, "price");
    e.round_trip("reorder first", vec![Change::Modify { from: "price".into(), def: price_def.clone(), pos: Pos::First }], |st| assert_eq!(names(st)[0], "price"));
    e.round_trip("reorder after", vec![Change::Modify { from: "price".into(), def: price_def, pos: Pos::After("kind".into()) }], |st| assert_eq!(names(st).last().unwrap(), "price"));

    let ix = |name: &str, kind: IndexKind, cols: &[(&str, Option<u64>)]| IndexDef { name: name.into(), kind, cols: cols.iter().map(|(n, p)| IdxCol { name: (*n).into(), prefix: *p, desc: false }).collect(), comment: String::new(), ..Default::default() };
    e.round_trip("add index", vec![Change::AddIndex(ix("idx_note", IndexKind::Index, &[("note", Some(10)), ("kind", None)]))], |st| {
        let i = st.indexes.iter().find(|i| i.name == "idx_note").unwrap();
        assert_eq!((i.cols.len(), i.cols[0].prefix), (2, Some(10)));
    });
    e.round_trip("add unique index", vec![Change::AddIndex(ix("uq_name", IndexKind::Unique, &[("name", None)]))], |st| assert!(st.indexes.iter().any(|i| i.name == "uq_name" && i.unique)));
    e.round_trip("add fulltext index", vec![Change::AddIndex(ix("ft_note", IndexKind::Fulltext, &[("note", None)]))], |st| assert!(st.indexes.iter().any(|i| i.name == "ft_note" && i.kind == "FULLTEXT")));
    e.round_trip("drop index", vec![Change::DropIndex("idx_name".into())], |st| assert!(!st.indexes.iter().any(|i| i.name == "idx_name")));
    e.fresh();
    let old = IndexDef::from_index(e.structure().indexes.iter().find(|i| i.name == "uq_price").unwrap());
    e.round_trip("rename index", vec![Change::RenameIndex { from: "uq_price".into(), to: "uq_cost".into(), def: old.clone() }], |st| assert!(st.indexes.iter().any(|i| i.name == "uq_cost" && i.unique && i.cols.len() == 2)));
    // The fallback for servers without RENAME INDEX is these two clauses.
    e.round_trip("rename index by drop and add", vec![Change::DropIndex("uq_price".into()), Change::AddIndex(IndexDef { name: "uq_cost".into(), ..old })], |st| assert!(st.indexes.iter().any(|i| i.name == "uq_cost")));
    let id = ColDef { auto_increment: false, ..cdef(&mut e, "id") };
    e.round_trip("drop primary key", vec![Change::Modify { from: "id".into(), def: id, pos: Pos::Keep }, Change::DropIndex("PRIMARY".into())], |st| assert!(!st.indexes.iter().any(|i| i.name == "PRIMARY")));

    e.fresh();
    let t = e.structure();
    let combined = vec![
        Change::Add { def: ColDef { nullable: false, default: Dv::Value("0".into()), ..ColDef::new("qty", "INT") }, pos: Pos::After("id".into()) },
        Change::Modify { from: "note".into(), def: ColDef { name: "memo".into(), ..ColDef::from_column(&t.columns[2]).unwrap() }, pos: Pos::First },
        Change::Drop("kind".into()),
        Change::AddIndex(ix("idx_qty", IndexKind::Index, &[("qty", None)])),
        Change::DropIndex("idx_name".into()),
    ];
    e.round_trip("combined", combined, |st| {
        assert_eq!(names(st), ["memo", "id", "qty", "name", "price", "created"]);
        assert!(st.indexes.iter().any(|i| i.name == "idx_qty") && !st.indexes.iter().any(|i| i.name == "idx_name"));
    });

    let hostile = "a`b";
    let junk = "'; DROP TABLE t; -- \\ \\' \" `";
    e.round_trip("hostile names and values", vec![Change::Add { def: ColDef { default: Dv::Value(junk.into()), comment: junk.into(), nullable: false, ..ColDef { args: "30".into(), ..ColDef::new(hostile, "VARCHAR") } }, pos: Pos::Keep }, Change::AddIndex(ix(hostile, IndexKind::Index, &[(hostile, Some(5))]))], |st| {
        let c = col(st, hostile);
        assert_eq!(c.default.as_deref(), Some(junk));
        assert_eq!(c.comment, junk);
        assert!(st.indexes.iter().any(|i| i.name == hostile));
    });
    // The table is still there and still holds its rows.
    let rows: i64 = e.w.rt.block_on(async { sqlx::query_scalar("SELECT COUNT(*) FROM quarry_ddl.t").fetch_one(&e.pool).await.unwrap() });
    assert_eq!(rows, 2);

    // CHECK constraints: added, dropped, and edited as a drop and an add of the same name in one ALTER, each undone.
    let chk = |name: &str, expr: &str| CheckConstraint { name: name.into(), expr: expr.into(), ..Default::default() };
    let find = |st: &Structure, n: &str| st.checks.iter().find(|k| k.name == n).cloned();
    if e.db.dialect.enforces_checks(&e.db.version) {
        e.round_trip("add check", vec![Change::AddCheck(chk("chk_price", "price >= 0"))], |st| assert!(find(st, "chk_price").is_some_and(|k| k.expr.contains("price") && !k.not_enforced), "{:?}", st.checks));
        e.fresh();
        e.exec("ALTER TABLE quarry_ddl.t ADD CONSTRAINT chk_name CHECK (CHAR_LENGTH(name) > 0)");
        e.round_trip_here("drop check", vec![Change::DropCheck("chk_name".into())], |st| assert!(find(st, "chk_name").is_none(), "{:?}", st.checks));
        e.round_trip_here("edit check", vec![Change::DropCheck("chk_name".into()), Change::AddCheck(chk("chk_name", "CHAR_LENGTH(name) > 1"))], |st| assert!(find(st, "chk_name").is_some_and(|k| k.expr.contains("> 1")), "{:?}", st.checks));
        // The server refuses an add that existing rows break, and the table is left as it was.
        let (d, v) = (e.db.dialect, e.db.version.clone());
        let st = e.structure();
        let p = plan(d, &v, Mode::of(&e.db.sql_mode), "t", &st, &[Change::AddCheck(chk("chk_never", "price > 100"))]).unwrap();
        e.w.sql_alter(1, e.db.clone(), "quarry_ddl".into(), "t".into(), p.sql(), Mode::of(&e.db.sql_mode), None, false, "done".into());
        let Msg::SqlAltered { res, .. } = recv(&e.rx) else { panic!("expected SqlAltered") };
        assert!(res.is_err());
        assert!(find(&e.structure(), "chk_never").is_none());
        if d == Dialect::MySql {
            e.round_trip("add a check that is not enforced", vec![Change::AddCheck(CheckConstraint { not_enforced: true, ..chk("chk_off", "price > 100") })], |st| assert!(find(st, "chk_off").is_some_and(|k| k.not_enforced), "{:?}", st.checks));
        }
    } else {
        // A server that parses a CHECK and ignores it is refused before anything runs.
        let st = e.structure();
        assert!(plan(e.db.dialect, &e.db.version, Mode::of(&e.db.sql_mode), "t", &st, &[Change::AddCheck(chk("chk_price", "price >= 0"))]).unwrap_err().contains("ignores"));
    }

    // Writes stay locked until the caller unlocks them.
    e.w.sql_alter(1, e.db.clone(), "quarry_ddl".into(), "t".into(), "ALTER TABLE `t` DROP COLUMN `note`".into(), Mode::of(&e.db.sql_mode), None, true, "done".into());
    let Msg::SqlAltered { res, .. } = recv(&e.rx) else { panic!() };
    assert!(res.is_err());
    assert!(col(&e.structure(), "note").name == "note");

    e.exec("DROP DATABASE quarry_ddl");
}

#[test]
fn live_mysql() {
    live("QUARRY_TEST_MYSQL");
}

#[test]
fn live_mariadb() {
    live("QUARRY_TEST_MARIADB");
}
