use super::live::shape;
use super::*;
use crate::db::sql::structure::Structure;
use crate::db::sql::write::SqlUndo;
use crate::db::sql::Pool;
use sqlx::Executor;
use std::sync::mpsc::Receiver;
use std::time::Duration;

const S: &str = "quarry_sf";

struct R {
    w: Worker,
    rx: Receiver<Msg>,
    db: Db,
    pool: sqlx::MySqlPool,
    n: u64,
}

fn recv(rx: &Receiver<Msg>) -> Msg {
    rx.recv_timeout(Duration::from_secs(60)).expect("no answer from the worker")
}

fn col(st: &Structure, n: &str) -> Column {
    st.columns.iter().find(|c| c.name == n).unwrap_or_else(|| panic!("no column {n}")).clone()
}

fn has_col(st: &Structure, n: &str) -> bool {
    st.columns.iter().any(|c| c.name == n)
}

fn moved(st: &Structure, n: &str) -> Change {
    Change::Modify { from: n.into(), def: ColDef::in_table(st, &col(st, n)).unwrap(), pos: Pos::First }
}

fn renamed(st: &Structure, from: &str, to: &str) -> Change {
    Change::Modify { from: from.into(), def: ColDef { name: to.into(), ..ColDef::in_table(st, &col(st, from)).unwrap() }, pos: Pos::Keep }
}

fn alter_undo(a: &UndoAction) -> &AlterUndo {
    match &a.kind {
        UndoKind::Sql { undo: SqlUndo::Alter(u), .. } => u,
        _ => panic!("not a structure undo"),
    }
}

impl R {
    fn exec(&self, sql: &str) {
        self.w.rt.block_on(async { sqlx::raw_sql(sql).execute(&self.pool).await.unwrap_or_else(|err| panic!("{sql}: {err}")) });
    }

    fn rows(&self, sql: &str) -> Vec<String> {
        self.w.rt.block_on(async { sqlx::query_scalar::<_, String>(sql).fetch_all(&self.pool).await.unwrap_or_else(|err| panic!("{sql}: {err}")) })
    }

    fn exists(&self, table: &str) -> bool {
        let q = format!("SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = '{S}' AND TABLE_NAME = '{table}'");
        self.w.rt.block_on(async { sqlx::query_scalar::<_, i64>(&q).fetch_one(&self.pool).await.unwrap() }) == 1
    }

    fn st(&mut self, db: &Db, table: &str) -> Structure {
        self.n += 1;
        self.w.sql_structure(self.n, db.clone(), S.into(), table.into());
        match recv(&self.rx) {
            Msg::SqlStructure { res, .. } => res.unwrap(),
            _ => panic!("expected SqlStructure"),
        }
    }

    /// The server as it is when its global sql_mode also has `extra`: every session of this pool starts with it.
    /// A pool of its own rather than SET GLOBAL, so the other live tests running at the same time are not affected.
    fn moded(&self, extra: &str) -> Db {
        let mut db = self.db.clone();
        let q = format!("SET SESSION sql_mode = CONCAT(@@SESSION.sql_mode, ',{extra}')");
        let opts = db.opts.my().unwrap().clone();
        let pool = self.w.rt.block_on(async move {
            sqlx::mysql::MySqlPoolOptions::new()
                .max_connections(2)
                .after_connect(move |c, _| {
                    let q = q.clone();
                    Box::pin(async move { c.execute(q.as_str()).await.map(|_| ()) })
                })
                .connect_with(opts)
                .await
                .unwrap()
        });
        db.sql_mode = self.w.rt.block_on(async { sqlx::query_scalar::<_, String>("SELECT @@SESSION.sql_mode").fetch_one(&pool).await.unwrap() });
        db.pool = Pool::My(pool);
        db
    }

    fn fresh(&self, table: &str, create: &str) {
        self.exec(&format!("DROP TABLE IF EXISTS {S}.{table}"));
        self.exec(create);
    }

    /// Plans the changes the way Apply does and runs them. Returns the answer, the undo and the statement.
    fn apply(&mut self, db: &Db, table: &str, changes: &[Change]) -> (Res<String>, Option<UndoAction>, Option<String>, String) {
        let st = self.st(db, table);
        let m = Mode::of(&db.sql_mode);
        let p = plan(db.dialect, &db.version, m, table, &st, changes).unwrap_or_else(|e| panic!("{table}: {e}"));
        let sql = p.sql();
        let undo = p.undo.ok().map(|mut u| {
            u.label = "test".into();
            u
        });
        self.w.sql_alter(1, db.clone(), S.into(), table.into(), sql.clone(), m, undo, false, "done".into());
        let Msg::SqlAltered { res, undo, sql_mode, .. } = recv(&self.rx) else { panic!("expected SqlAltered") };
        (res, undo, sql_mode, sql)
    }

    fn undo(&mut self, db: &Db, a: UndoAction) -> (Res<String>, Option<UndoAction>) {
        self.w.sql_undo(db.clone(), a, Vec::new(), false);
        let Msg::SqlAltered { res, undo, .. } = recv(&self.rx) else { panic!("expected SqlAltered") };
        (res, undo)
    }

    /// Creates the table, applies what `changes` builds from its structure, checks `after` with the structure
    /// before and after, undoes, and checks the table is back as it was.
    fn trip(&mut self, db: &Db, table: &str, create: &str, label: &str, changes: impl FnOnce(&Structure) -> Vec<Change>, after: impl FnOnce(&Structure, &Structure)) {
        self.fresh(table, create);
        let before = self.st(db, table);
        let c = changes(&before);
        let (res, undo, _, sql) = self.apply(db, table, &c);
        res.unwrap_or_else(|e| panic!("{label}: {e}\n{sql}"));
        after(&before, &self.st(db, table));
        let (res, _) = self.undo(db, undo.unwrap_or_else(|| panic!("{label}: no undo")));
        res.unwrap_or_else(|e| panic!("{label} undo: {e}\n{sql}"));
        assert_eq!(shape(&self.st(db, table)), shape(&before), "{label}: undo did not restore the table\n{sql}");
    }
}

fn start(var: &str) -> Option<R> {
    let uri = std::env::var(var).ok()?;
    let (tx, rx) = std::sync::mpsc::channel();
    let w = Worker::new(tx, eframe::egui::Context::default());
    let pool = w.rt.block_on(async {
        let p = sqlx::MySqlPool::connect(&uri).await.unwrap();
        for sql in [format!("DROP DATABASE IF EXISTS {S}"), format!("CREATE DATABASE {S}")] {
            sqlx::raw_sql(&sql).execute(&p).await.unwrap();
        }
        p
    });
    w.sql_connect(1, uri, Duration::from_secs(30), crate::db::Net::default());
    let Msg::SqlConnected { res: Ok((db, ..)), .. } = recv(&rx) else { panic!("expected SqlConnected") };
    Some(R { w, rx, db, pool, n: 100 })
}

fn review(var: &str) {
    let Some(mut r) = start(var) else { return };
    let db = r.db.clone();
    let maria = db.dialect == Dialect::MariaDb;
    finding_1_undo_confirms_loss_and_checks_the_table(&mut r, &db);
    finding_2_enum_values_survive_a_move(&mut r, &db);
    finding_3_and_13_one_statement_in_the_session_mode(&mut r, &db);
    finding_4_binary_and_bit_defaults(&mut r, &db);
    finding_5_attributes(&mut r, &db, maria);
    finding_6_and_8_undo_steps(&mut r, &db);
    final_6_undo_after_the_sql_mode_changed(&mut r, &db);
    if !maria {
        finding_7_functional_index(&mut r, &db);
    }
    r.exec(&format!("DROP DATABASE {S}"));
}

fn finding_1_undo_confirms_loss_and_checks_the_table(r: &mut R, db: &Db) {
    let t = "sf_undo";
    r.fresh(t, &format!("CREATE TABLE {S}.{t} (id INT PRIMARY KEY, note VARCHAR(50) NULL)"));
    r.exec(&format!("INSERT INTO {S}.{t} VALUES (1, 'kept')"));
    let (res, undo, ..) = r.apply(db, t, &[Change::Add { def: ColDef::new("qty", "INT"), pos: Pos::Keep }]);
    res.unwrap();
    let undo = undo.expect("an undo");
    assert_eq!(alter_undo(&undo).loss, ["Undo drops column qty and every value it holds."]);
    // Someone changes the column after the change ran: the undo would not put the table back, so it is refused
    // and nothing runs.
    r.exec(&format!("ALTER TABLE {S}.{t} MODIFY qty BIGINT NULL"));
    let (res, kept) = r.undo(db, undo);
    assert!(res.as_ref().unwrap_err().contains("Undo refused: column qty changed"), "{res:?}");
    assert!(has_col(&r.st(db, t), "qty"));
    // Back as the change left it, the same undo runs.
    r.exec(&format!("ALTER TABLE {S}.{t} MODIFY qty INT NULL DEFAULT NULL"));
    let (res, _) = r.undo(db, kept.expect("the refused undo stays in the list"));
    res.unwrap();
    assert!(!has_col(&r.st(db, t), "qty"));
    // Widening a column: its undo narrows it again, which is named.
    let st = r.st(db, t);
    let wide = Change::Modify { from: "note".into(), def: ColDef { args: "255".into(), ..ColDef::in_table(&st, &col(&st, "note")).unwrap() }, pos: Pos::Keep };
    let (res, undo, ..) = r.apply(db, t, &[wide]);
    res.unwrap();
    assert!(alter_undo(undo.as_ref().unwrap()).loss[0].starts_with("Undo converts note back to VARCHAR(50)"));
    assert_eq!(r.rows(&format!("SELECT note FROM {S}.{t}")), ["kept"]);
}

fn finding_2_enum_values_survive_a_move(r: &mut R, db: &Db) {
    let t = "sf_enum";
    for extra in ["", "NO_BACKSLASH_ESCAPES"] {
        let mdb = if extra.is_empty() { db.clone() } else { r.moded(extra) };
        assert_eq!(mdb.backslash_escapes(), extra.is_empty(), "{}", mdb.sql_mode);
        // Created over the default-mode pool, so '\n' is a newline and '\\' one backslash.
        r.fresh(t, &format!(r"CREATE TABLE {S}.{t} (id INT PRIMARY KEY, x INT, e ENUM('a\nb','c\\d') NULL)"));
        r.exec(&format!(r"INSERT INTO {S}.{t} VALUES (1, 0, 'a\nb'), (2, 0, 'c\\d')"));
        let q = format!("SELECT CONCAT(id, ':', HEX(e)) FROM {S}.{t} ORDER BY id");
        assert_eq!(r.rows(&q), ["1:610A62", "2:635C64"]);
        let before = r.st(&mdb, t);
        let (res, undo, _, sql) = r.apply(&mdb, t, &[moved(&before, "e")]);
        res.unwrap_or_else(|e| panic!("mode {extra}: {e}\n{sql}"));
        let after = r.st(&mdb, t);
        assert_eq!(after.columns[0].name, "e");
        assert_eq!(col(&after, "e").full_type, col(&before, "e").full_type, "mode {extra}: {sql}");
        assert_eq!(r.rows(&q), ["1:610A62", "2:635C64"], "mode {extra}: {sql}");
        let (res, _) = r.undo(&mdb, undo.unwrap());
        res.unwrap();
        assert_eq!(shape(&r.st(&mdb, t)), shape(&before));
        assert_eq!(r.rows(&q), ["1:610A62", "2:635C64"]);
    }
}

fn finding_3_and_13_one_statement_in_the_session_mode(r: &mut R, db: &Db) {
    let (t, victim) = ("sf_ansi", "sf_victim");
    r.fresh(victim, &format!("CREATE TABLE {S}.{victim} (id INT)"));
    r.fresh(t, &format!("CREATE TABLE {S}.{t} (id INT PRIMARY KEY, c VARCHAR(20) NULL)"));
    let ansi = r.moded("ANSI_QUOTES");
    let am = Mode::of(&ansi.sql_mode);
    assert!(am.ansi, "{}", ansi.sql_mode);
    let st = r.st(&ansi, t);
    let x = format!("\"\\\"); DROP TABLE {victim}; SELECT (\"");
    let hostile = [Change::Modify { from: "c".into(), def: ColDef { default: Dv::Expr(x), ..ColDef::in_table(&st, &col(&st, "c")).unwrap() }, pos: Pos::Keep }];
    // The check reads quotes as the session does and refuses it.
    assert!(plan(ansi.dialect, &ansi.version, am, t, &st, &hostile).is_err());
    // Past the check (the text a default-mode check lets through), sent to the ANSI_QUOTES session: the
    // prepared statement holds one statement, so the server rejects it and the victim stays.
    let smuggled = alter(ansi.dialect, &ansi.version, Mode::DEFAULT, t, &hostile).unwrap();
    r.w.sql_alter(1, ansi.clone(), S.into(), t.into(), smuggled.clone(), am, None, false, "done".into());
    let Msg::SqlAltered { res, .. } = recv(&r.rx) else { panic!() };
    assert!(res.is_err(), "{smuggled}");
    assert!(r.exists(victim), "{smuggled}");
    // Plain text with a second statement is refused in the default mode too.
    r.w.sql_alter(1, db.clone(), S.into(), t.into(), format!("ALTER TABLE {t} ADD COLUMN z INT; DROP TABLE {victim}"), Mode::of(&db.sql_mode), None, false, "done".into());
    let Msg::SqlAltered { res, .. } = recv(&r.rx) else { panic!() };
    assert!(res.is_err());
    assert!(r.exists(victim) && !has_col(&r.st(db, t), "z"));
    // Finding 13: a statement written for another sql_mode than the session's does not run, and the answer
    // carries the mode in force so the preview can follow it.
    let wrong = Mode { bs: !Mode::of(&db.sql_mode).bs, ansi: false };
    r.w.sql_alter(1, db.clone(), S.into(), t.into(), format!("ALTER TABLE {t} ADD COLUMN z INT"), wrong, None, false, "done".into());
    let Msg::SqlAltered { res, sql_mode, .. } = recv(&r.rx) else { panic!() };
    assert!(res.unwrap_err().contains("sql_mode"));
    assert_eq!(sql_mode.map(|m| Mode::of(&m)), Some(Mode::of(&db.sql_mode)));
    assert!(!has_col(&r.st(db, t), "z"));
    // And through Apply: the ANSI_QUOTES session runs what was planned for it.
    let (res, undo, ..) = r.apply(&ansi, t, &[Change::Add { def: ColDef { default: Dv::Value("say \"hi\"".into()), args: "20".into(), ..ColDef::new("z", "VARCHAR") }, pos: Pos::Keep }]);
    res.unwrap();
    assert_eq!(col(&r.st(&ansi, t), "z").default.as_deref(), Some("say \"hi\""));
    r.undo(&ansi, undo.unwrap()).0.unwrap();
}

fn finding_4_binary_and_bit_defaults(r: &mut R, db: &Db) {
    let t = "sf_bin";
    let create = format!("CREATE TABLE {S}.{t} (id INT PRIMARY KEY, vb VARBINARY(10) DEFAULT 'ab', b4 BINARY(4) DEFAULT 'ab', bt BIT(1) DEFAULT b'1', n INT)");
    for c in ["vb", "b4", "bt"] {
        r.trip(db, t, &create, &format!("move {c}"), |st| vec![moved(st, c)], |before, after| {
            assert_eq!(after.columns[0].name, c);
            let (a, b) = (col(after, c), col(before, c));
            assert_eq!((a.default, a.default_expr, a.full_type), (b.default, b.default_expr, b.full_type), "{c}");
        });
        r.trip(db, t, &create, &format!("drop {c}"), |_| vec![Change::Drop(c.into())], |_, after| assert!(!has_col(after, c)));
    }
    // A row that takes the defaults after a move still gets 'ab' and 1.
    r.fresh(t, &create);
    let st = r.st(db, t);
    let (res, ..) = r.apply(db, t, &[moved(&st, "vb"), moved(&st, "b4"), moved(&st, "bt")]);
    res.unwrap();
    r.exec(&format!("INSERT INTO {S}.{t} (id) VALUES (1)"));
    assert_eq!(r.rows(&format!("SELECT CONCAT(HEX(vb), ',', HEX(b4), ',', bt + 0) FROM {S}.{t}")), ["6162,61620000,1"]);
}

fn finding_5_attributes(r: &mut R, db: &Db, maria: bool) {
    let hide = if maria { "IGNORED" } else { "INVISIBLE" };
    let t = "sf_inv";
    let create = format!("CREATE TABLE {S}.{t} (id INT PRIMARY KEY, a INT, b INT, KEY iv (a) {hide}, KEY ivb (b, a) {hide})");
    r.trip(db, t, &create, "drop invisible index", |_| vec![Change::DropIndex("iv".into())], |before, after| {
        assert!(before.indexes.iter().any(|i| i.name == "iv" && i.invisible));
        assert!(!after.indexes.iter().any(|i| i.name == "iv"));
    });
    r.trip(db, t, &create, "drop column of invisible indexes", |_| vec![Change::Drop("a".into())], |_, after| assert!(after.indexes.iter().all(|i| i.name != "iv")));
    if maria {
        let t = "sf_json";
        let create = format!("CREATE TABLE {S}.{t} (id INT PRIMARY KEY, j JSON, k INT CHECK (k > 0), z BLOB COMPRESSED, n INT)");
        r.trip(db, t, &create, "move json", |st| vec![moved(st, "j")], |_, after| {
            assert_eq!(after.columns[0].name, "j");
            assert_eq!(col(after, "j").check.as_deref(), Some("json_valid(`j`)"));
        });
        r.trip(db, t, &create, "drop json", |_| vec![Change::Drop("j".into())], |_, after| assert!(!has_col(after, "j")));
        let st = r.st(db, t);
        assert!(ColDef::in_table(&st, &col(&st, "k")).unwrap_err().contains("CHECK"));
        assert!(ColDef::in_table(&st, &col(&st, "z")).unwrap_err().contains("COMPRESSED"));
        // Dropping either has no undo, and the plan says so before anything runs.
        for c in ["k", "z"] {
            let p = plan(db.dialect, &db.version, Mode::of(&db.sql_mode), t, &st, &[Change::Drop(c.into())]).unwrap();
            assert!(p.undo.is_err(), "{c}");
        }
    } else {
        let t = "sf_geo";
        let create = format!("CREATE TABLE {S}.{t} (id INT PRIMARY KEY, p POINT NOT NULL SRID 4326, n INT)");
        r.trip(db, t, &create, "move srid column", |st| vec![moved(st, "p")], |_, after| {
            assert_eq!(after.columns[0].name, "p");
            assert_eq!(col(after, "p").srid, Some(4326));
        });
        r.trip(db, t, &create, "drop srid column", |_| vec![Change::Drop("p".into())], |_, after| assert!(!has_col(after, "p")));
    }
}

/// Final review finding 6: the change ran in the default mode, the undo in a session with NO_BACKSLASH_ESCAPES. It
/// used to be refused with "Nothing ran" for good; now its steps are written again for that session.
fn final_6_undo_after_the_sql_mode_changed(r: &mut R, db: &Db) {
    let t = "sf_mode_undo";
    r.fresh(t, &format!(r"CREATE TABLE {S}.{t} (id INT PRIMARY KEY, note VARCHAR(20) NULL DEFAULT 'C:\\temp' COMMENT 'it''s \\ here')"));
    let before = r.st(db, t);
    assert_eq!(col(&before, "note").default.as_deref(), Some(r"C:\temp"));
    let (res, undo, ..) = r.apply(db, t, &[Change::Drop("note".into())]);
    res.unwrap();
    let nb = r.moded("NO_BACKSLASH_ESCAPES");
    r.w.sql_undo(nb.clone(), undo.expect("an undo"), Vec::new(), false);
    let Msg::SqlAltered { res, undo, sql_mode, .. } = recv(&r.rx) else { panic!("expected SqlAltered") };
    res.unwrap_or_else(|e| panic!("{e}"));
    assert!(undo.is_none());
    assert!(sql_mode.is_some_and(|m| !Mode::of(&m).bs), "the answer carries the mode the undo was written for");
    let after = r.st(db, t);
    assert_eq!(col(&after, "note").default.as_deref(), Some(r"C:\temp"));
    assert_eq!(col(&after, "note").comment, r"it's \ here");
    assert_eq!(shape(&after), shape(&before));
}

fn finding_6_and_8_undo_steps(r: &mut R, db: &Db) {
    let t = "sf_seq";
    let create = format!("CREATE TABLE {S}.{t} (id INT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY, name VARCHAR(50) NOT NULL DEFAULT 'x', note TEXT NULL, price DECIMAL(10,2) NOT NULL DEFAULT 0.00, KEY idx_name (name), UNIQUE KEY uq_price (price, name))");
    // Moving name up past the new x places it after id, as the Move up button does.
    r.trip(db, t, &create, "add x after id then move name past it", |st| vec![Change::Add { def: ColDef::new("x", "INT"), pos: Pos::After("id".into()) }, Change::Modify { from: "name".into(), def: ColDef::in_table(st, &col(st, "name")).unwrap(), pos: Pos::After("id".into()) }], |_, after| assert!(has_col(after, "x")));
    r.trip(db, t, &create, "rename name to title then drop idx_name", |st| vec![renamed(st, "name", "title"), Change::DropIndex("idx_name".into())], |_, after| assert!(has_col(after, "title")));
    r.trip(db, t, &create, "rename name to title then drop note", |st| vec![renamed(st, "name", "title"), Change::Drop("note".into())], |_, after| assert!(!has_col(after, "note")));
    r.trip(db, t, &create, "rename name to title then drop price", |st| vec![renamed(st, "name", "title"), Change::Drop("price".into())], |_, after| assert!(!has_col(after, "price")));
    // Finding 8: on MariaDB the index added back without price names the column by its new name.
    r.trip(db, t, &create, "drop price then rename name to title", |st| vec![Change::Drop("price".into()), renamed(st, "name", "title")], |_, after| {
        assert!(after.indexes.iter().any(|i| i.name == "uq_price" && i.cols.len() == 1 && i.cols[0].name == "title"), "{:?}", after.indexes);
    });
}

fn finding_7_functional_index(r: &mut R, db: &Db) {
    let t = "sf_fn";
    r.fresh(t, &format!("CREATE TABLE {S}.{t} (id INT PRIMARY KEY, a INT, KEY f ((a + 1)))"));
    let st = r.st(db, t);
    assert!(st.indexes.iter().any(|i| i.name == "f" && i.cols[0].expr.is_some()), "{:?}", st.indexes);
    let p = plan(db.dialect, &db.version, Mode::of(&db.sql_mode), t, &st, &[Change::DropIndex("f".into())]).unwrap();
    assert!(p.undo.unwrap_err().contains("expression"));
}

#[test]
fn review_live_mysql() {
    review("QUARRY_TEST_MYSQL");
}

#[test]
fn review_live_mariadb() {
    review("QUARRY_TEST_MARIADB");
}
