use super::*;
use crate::db::sql::structure::Structure;
use std::sync::mpsc::Receiver;
use std::time::Duration;

static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn recv(rx: &Receiver<Msg>) -> Msg {
    rx.recv_timeout(Duration::from_secs(30)).expect("no answer from the worker")
}

/// Keys and indexes as a table holds them, for comparing before and after.
fn shape(st: &Structure) -> Vec<String> {
    let mut v: Vec<String> = st.indexes.iter().map(|i| format!("idx {}|{}|{}", i.name, i.unique, i.cols.iter().map(|c| c.name.clone()).collect::<Vec<_>>().join(","))).collect();
    v.extend(st.foreign_keys.iter().map(|k| format!("fk {}|{}|{}.{}|{}|{}|{}", k.name, k.cols.join(","), k.ref_db, k.ref_table, k.ref_cols.join(","), k.on_update, k.on_delete)));
    v.sort();
    v
}

struct Env {
    w: Worker,
    rx: Receiver<Msg>,
    db: Db,
    pool: sqlx::MySqlPool,
    n: u64,
    /// The sql_mode every connection of these tests starts with.
    mode: String,
}

impl Env {
    fn try_exec(&self, sql: &str) -> Result<(), String> {
        self.w.rt.block_on(async { sqlx::raw_sql(sql).execute(&self.pool).await.map(|_| ()).map_err(|e| e.to_string()) })
    }

    fn exec(&self, sql: &str) {
        self.try_exec(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
    }

    fn scalar(&self, sql: &str) -> Option<String> {
        self.w.rt.block_on(async { sqlx::raw_sql(sql).fetch_one(&self.pool).await.ok().and_then(|r| super::super::worker::text_at(&r, 0)) })
    }

    fn structure_in(&mut self, schema: &str, table: &str) -> Structure {
        self.n += 1;
        self.w.sql_structure(self.n, self.db.clone(), schema.into(), table.into());
        match recv(&self.rx) {
            Msg::SqlStructure { res, .. } => res.unwrap(),
            _ => panic!("expected SqlStructure"),
        }
    }

    fn structure(&mut self, table: &str) -> Structure {
        self.structure_in("quarry_rt", table)
    }

    /// What Apply does for staged changes with foreign keys in them: the same plan and checked ALTER as any other.
    fn apply(&mut self, table: &str, changes: &[Change]) -> Result<UndoAction, String> {
        let before = self.structure(table);
        let m = Mode::of(&self.db.sql_mode);
        let p = plan(self.db.dialect, &self.db.version, m, table, &before, changes)?;
        let sql = p.sql();
        let mut u = p.undo?;
        u.label = "label".into();
        self.w.sql_alter(1, self.db.clone(), "quarry_rt".into(), table.into(), sql.clone(), m, Some(u), false, "done".into());
        let Msg::SqlAltered { res, undo, .. } = recv(&self.rx) else { panic!("expected SqlAltered") };
        res.map_err(|e| format!("{e}\n{sql}"))?;
        Ok(undo.expect("an undo"))
    }

    fn undo(&mut self, a: UndoAction) -> Result<(), String> {
        self.w.sql_undo(self.db.clone(), a, Vec::new(), false);
        let Msg::SqlAltered { res, .. } = recv(&self.rx) else { panic!("expected SqlAltered") };
        res.map(|_| ())
    }

    /// Sends a trigger job as the app does, written for the mode the app last saw.
    fn job_on(&mut self, db: &Db, table: &str, job: TrigJob) -> (Res<String>, Option<UndoAction>, Option<String>) {
        let m = Mode::of(&self.db.sql_mode);
        self.w.sql_trigger(1, db.clone(), "quarry_rt".into(), table.into(), job, m, false, "done".into());
        let Msg::SqlAltered { res, undo, sql_mode, alter, .. } = recv(&self.rx) else { panic!("expected SqlAltered") };
        assert!(!alter, "a trigger answer is not an ALTER");
        (res, undo, sql_mode)
    }

    fn job(&mut self, table: &str, job: TrigJob) -> Result<UndoAction, String> {
        let db = self.db.clone();
        let (res, undo, _) = self.job_on(&db, table, job);
        res?;
        Ok(undo.expect("an undo"))
    }

    fn live_trigger(&mut self, table: &str, name: &str) -> Option<crate::db::sql::structure::Trigger> {
        self.structure(table).triggers.into_iter().find(|t| t.name == name)
    }

    /// Creates a new trigger, or replaces the one of that name as the editor does (keeping its definer and place).
    fn trigger(&mut self, table: &str, def: &TrigDef, exists: bool) -> Result<UndoAction, String> {
        let all = self.structure(table).triggers;
        let was = all.iter().find(|t| t.name == def.name).cloned();
        assert_eq!(was.is_some(), exists, "{}", def.name);
        self.job(table, save_job(&all, was.as_ref(), def))
    }

    fn drop_trigger(&mut self, table: &str, name: &str) -> Result<UndoAction, String> {
        let t = self.live_trigger(table, name).expect("a trigger to drop");
        self.job(table, TrigJob::Drop(t))
    }

    /// The tag a fresh row gets from the BEFORE INSERT triggers.
    fn tag(&self) -> Option<String> {
        self.try_exec("INSERT INTO quarry_rt.rt_trig (tag) VALUES (NULL)").map_err(|e| format!("INSERT FAILS: {e}")).err().or_else(|| self.scalar("SELECT tag FROM quarry_rt.rt_trig ORDER BY id DESC LIMIT 1"))
    }

    /// Runs `sql` on one connection of the test pool with `mode` added to its session, then puts the mode back.
    fn exec_in_mode(&self, mode: &str, sql: &str) {
        self.exec(&format!("SET SESSION sql_mode = CONCAT(@@SESSION.sql_mode, ',{mode}'); {sql}; SET SESSION sql_mode = '{}'", self.mode));
    }
}

/// A pool whose every connection starts in `mode`, whatever the server's global mode is at that moment.
fn pinned(w: &Worker, opts: sqlx::mysql::MySqlConnectOptions, mode: &str) -> sqlx::MySqlPool {
    let set = format!("SET SESSION sql_mode = '{mode}'");
    w.rt.block_on(async move {
        sqlx::mysql::MySqlPoolOptions::new()
            .max_connections(4)
            .after_connect(move |c, _| {
                let set = set.clone();
                Box::pin(async move { sqlx::Executor::execute(c, set.as_str()).await.map(|_| ()) })
            })
            .connect_with(opts)
            .await
            .unwrap()
    })
}

/// Connects the worker and a plain pool for the checks, both pinned to the server's mode without
/// NO_BACKSLASH_ESCAPES: another live test sets the global mode for a moment, and a connection opened then would
/// read text differently from the one the worker was connected with.
fn start(var: &str) -> Option<Env> {
    let uri = std::env::var(var).ok()?;
    let (tx, rx) = std::sync::mpsc::channel();
    let w = Worker::new(tx, eframe::egui::Context::default());
    w.sql_connect(1, uri.clone(), Duration::from_secs(30), crate::db::Net::default());
    let Msg::SqlConnected { res: Ok((mut db, ..)), .. } = recv(&rx) else { panic!("expected SqlConnected") };
    let mode = db.sql_mode.split(',').filter(|m| !m.trim().eq_ignore_ascii_case("NO_BACKSLASH_ESCAPES")).collect::<Vec<_>>().join(",");
    db.pool = Pool::My(pinned(&w, db.opts.my().unwrap().clone(), &mode));
    db.sql_mode = mode.clone();
    let p = pinned(&w, db.opts.my().unwrap().clone(), &mode);
    let pool = w.rt.block_on(async {
        for sql in [
            "DROP DATABASE IF EXISTS quarry_rt",
            "DROP DATABASE IF EXISTS quarry_rt_b",
            "CREATE DATABASE quarry_rt",
            "CREATE DATABASE quarry_rt_b",
            "CREATE TABLE quarry_rt_b.rt_currencies (code CHAR(3) NOT NULL PRIMARY KEY, label VARCHAR(20))",
            "INSERT INTO quarry_rt_b.rt_currencies VALUES ('EUR', 'Euro')",
            "CREATE TABLE quarry_rt.rt_customers (id INT UNSIGNED NOT NULL PRIMARY KEY, email VARCHAR(40) NOT NULL)",
            "INSERT INTO quarry_rt.rt_customers VALUES (1, 'a@x'), (2, 'b@x')",
            "CREATE TABLE quarry_rt.rt_orders (id INT NOT NULL PRIMARY KEY, customer_id INT UNSIGNED NULL, cur CHAR(3) NULL, note TEXT, wrong INT NULL)",
            "CREATE TABLE quarry_rt.rt_regions (a INT NOT NULL, b INT NOT NULL, PRIMARY KEY (a, b))",
            "INSERT INTO quarry_rt.rt_regions VALUES (1, 2)",
            "CREATE TABLE quarry_rt.rt_lines (id INT NOT NULL PRIMARY KEY, ra INT NULL, rb INT NULL)",
            "CREATE TABLE quarry_rt.rt_trig (id INT NOT NULL AUTO_INCREMENT PRIMARY KEY, tag VARCHAR(20) NULL)",
        ] {
            sqlx::raw_sql(sql).execute(&p).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
        }
        p
    });
    Some(Env { w, rx, db, pool, n: 100, mode })
}

fn key(name: &str, table: &str, cols: &[&str], ref_db: &str, ref_table: &str, ref_cols: &[&str]) -> ForeignKey {
    ForeignKey { name: name.into(), db: "quarry_rt".into(), table: table.into(), cols: cols.iter().map(|c| (*c).into()).collect(), ref_db: ref_db.into(), ref_table: ref_table.into(), ref_cols: ref_cols.iter().map(|c| (*c).into()).collect(), on_update: "CASCADE".into(), on_delete: "RESTRICT".into(), ..Default::default() }
}

fn foreign_keys(var: &str) {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut e) = start(var) else { return };
    let before = shape(&e.structure("rt_orders"));
    let k = key("fk_rt_cust", "rt_orders", &["customer_id"], "quarry_rt", "rt_customers", &["id"]);
    let (own, parent) = (e.structure("rt_orders"), e.structure("rt_customers"));
    assert_eq!(fk_problem(&own, "rt_orders", &parent, &k, false), None, "{var}");

    // The orphan count says what would make the ALTER fail, and the ALTER does fail on it.
    e.exec("INSERT INTO quarry_rt.rt_orders (id, customer_id) VALUES (90, 777), (91, NULL), (92, 1)");
    e.w.sql_orphans(5, e.db.clone(), "quarry_rt".into(), "rt_orders".into(), k.clone());
    let Msg::SqlOrphans { res, .. } = recv(&e.rx) else { panic!("expected SqlOrphans") };
    assert_eq!(res.unwrap(), 1, "{var}: only the row pointing at 777 is an orphan");
    assert!(e.apply("rt_orders", &[Change::AddFk(k.clone())]).is_err(), "{var}: ADD with an orphan row must fail");
    e.exec("DELETE FROM quarry_rt.rt_orders");

    // Add: the index is made, a bad row is refused, a good one is taken.
    let undo = e.apply("rt_orders", &[Change::AddFk(k.clone())]).unwrap_or_else(|er| panic!("{var}: {er}"));
    let after = e.structure("rt_orders");
    assert_eq!(after.foreign_keys.len(), 1);
    assert!(after.indexes.iter().any(|i| i.name == "fk_rt_cust"), "{var}: {:?}", shape(&after));
    assert!(e.try_exec("INSERT INTO quarry_rt.rt_orders (id, customer_id) VALUES (1, 4242)").is_err(), "{var}: the key must refuse a missing customer");
    e.exec("INSERT INTO quarry_rt.rt_orders (id, customer_id) VALUES (2, 1)");
    e.exec("DELETE FROM quarry_rt.rt_orders");
    // Undo is refused when someone changed the key meanwhile.
    e.exec("ALTER TABLE quarry_rt.rt_orders DROP FOREIGN KEY fk_rt_cust");
    let refused = e.undo(undo).unwrap_err();
    assert!(refused.contains("foreign key fk_rt_cust changed"), "{var}: {refused}");
    e.exec("ALTER TABLE quarry_rt.rt_orders DROP INDEX fk_rt_cust");
    assert_eq!(shape(&e.structure("rt_orders")), before, "{var}");

    // Undo of an add restores the table.
    let undo = e.apply("rt_orders", &[Change::AddFk(k.clone())]).unwrap();
    let with_key = shape(&e.structure("rt_orders"));
    e.undo(undo).unwrap_or_else(|er| panic!("{var}: {er}"));
    assert_eq!(shape(&e.structure("rt_orders")), before, "{var}: undo of add");

    // Drop takes the index made for the key, and undo brings both back as they were.
    e.apply("rt_orders", &[Change::AddFk(k.clone())]).unwrap();
    let undo = e.apply("rt_orders", &[Change::DropFk("fk_rt_cust".into())]).unwrap_or_else(|er| panic!("{var}: {er}"));
    assert_eq!(shape(&e.structure("rt_orders")), before, "{var}: drop took key and index");
    e.undo(undo).unwrap_or_else(|er| panic!("{var}: {er}"));
    assert_eq!(shape(&e.structure("rt_orders")), with_key, "{var}: undo of drop");
    e.exec("ALTER TABLE quarry_rt.rt_orders DROP FOREIGN KEY fk_rt_cust, DROP INDEX fk_rt_cust");

    // An index somebody made stays when the key goes.
    e.exec("ALTER TABLE quarry_rt.rt_orders ADD INDEX mine (customer_id)");
    let mine = shape(&e.structure("rt_orders"));
    e.apply("rt_orders", &[Change::AddFk(k.clone())]).unwrap();
    assert!(!e.structure("rt_orders").indexes.iter().any(|i| i.name == "fk_rt_cust"), "{var}: the key had an index to use");
    e.apply("rt_orders", &[Change::DropFk("fk_rt_cust".into())]).unwrap();
    assert_eq!(shape(&e.structure("rt_orders")), mine, "{var}: their index stays");
    e.exec("ALTER TABLE quarry_rt.rt_orders DROP INDEX mine");

    // SET NULL acts as asked.
    let sn = ForeignKey { on_delete: "SET NULL".into(), ..k.clone() };
    let undo = e.apply("rt_orders", &[Change::AddFk(sn)]).unwrap();
    e.exec("INSERT INTO quarry_rt.rt_orders (id, customer_id) VALUES (3, 2)");
    e.exec("DELETE FROM quarry_rt.rt_customers WHERE id = 2");
    assert_eq!(e.scalar("SELECT customer_id IS NULL FROM quarry_rt.rt_orders WHERE id = 3").as_deref(), Some("1"), "{var}");
    e.exec("INSERT INTO quarry_rt.rt_customers VALUES (2, 'b@x')");
    e.undo(undo).unwrap();
    e.exec("DELETE FROM quarry_rt.rt_orders");
    assert_eq!(shape(&e.structure("rt_orders")), before);

    // A composite key checks both columns together.
    let before_l = shape(&e.structure("rt_lines"));
    let c = key("fk_rt_region", "rt_lines", &["ra", "rb"], "quarry_rt", "rt_regions", &["a", "b"]);
    let undo = e.apply("rt_lines", &[Change::AddFk(c)]).unwrap_or_else(|er| panic!("{var}: {er}"));
    let st = e.structure("rt_lines");
    assert_eq!((st.foreign_keys[0].cols.clone(), st.foreign_keys[0].ref_cols.clone()), (vec!["ra".to_string(), "rb".to_string()], vec!["a".to_string(), "b".to_string()]));
    assert!(e.try_exec("INSERT INTO quarry_rt.rt_lines VALUES (1, 1, 9)").is_err(), "{var}: (1, 9) is not a region");
    e.exec("INSERT INTO quarry_rt.rt_lines VALUES (2, 1, 2)");
    e.exec("DELETE FROM quarry_rt.rt_lines");
    e.undo(undo).unwrap();
    assert_eq!(shape(&e.structure("rt_lines")), before_l, "{var}: composite undo");

    // A key into another schema.
    let x = key("fk_rt_cur", "rt_orders", &["cur"], "quarry_rt_b", "rt_currencies", &["code"]);
    let own = e.structure("rt_orders");
    let parent = e.structure_in("quarry_rt_b", "rt_currencies");
    assert_eq!(fk_problem(&own, "rt_orders", &parent, &x, false), None, "{var}");
    let undo = e.apply("rt_orders", &[Change::AddFk(x)]).unwrap_or_else(|er| panic!("{var}: {er}"));
    let st = e.structure("rt_orders");
    assert_eq!(st.foreign_keys[0].ref_db, "quarry_rt_b", "{var}");
    assert!(e.try_exec("INSERT INTO quarry_rt.rt_orders (id, cur) VALUES (5, 'XXX')").is_err());
    e.exec("INSERT INTO quarry_rt.rt_orders (id, cur) VALUES (6, 'EUR')");
    e.exec("DELETE FROM quarry_rt.rt_orders");
    e.undo(undo).unwrap();
    assert_eq!(shape(&e.structure("rt_orders")), before, "{var}: cross schema undo");

    // The check agrees with the server about a pair that does not fit.
    let bad = key("fk_rt_bad", "rt_orders", &["wrong"], "quarry_rt", "rt_customers", &["id"]);
    let (own, parent) = (e.structure("rt_orders"), e.structure("rt_customers"));
    assert!(fk_problem(&own, "rt_orders", &parent, &bad, false).is_some());
    assert!(e.apply("rt_orders", &[Change::AddFk(bad)]).is_err(), "{var}: the server refuses INT against INT UNSIGNED too");

    e.exec("DROP DATABASE quarry_rt");
    e.exec("DROP DATABASE quarry_rt_b");
}

fn triggers(var: &str) {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut e) = start(var) else { return };
    let def = |name: &str, body: &str| TrigDef { name: name.into(), timing: "BEFORE".into(), event: "INSERT".into(), body: body.into(), pg: PgDef::default() };
    let body_of = |e: &mut Env, name: &str| e.structure("rt_trig").triggers.iter().find(|t| t.name == name).map(|t| t.body.clone());

    let a = def("rt_bi", "SET NEW.tag = 'a'");
    let create_undo = e.trigger("rt_trig", &a, false).unwrap_or_else(|er| panic!("{var}: {er}"));
    assert_eq!(e.tag().as_deref(), Some("a"), "{var}: the trigger fires");

    // Edit: the new body fires.
    let b = def("rt_bi", "SET NEW.tag = 'b'");
    let edit_undo = e.trigger("rt_trig", &b, true).unwrap_or_else(|er| panic!("{var}: {er}"));
    assert_eq!(e.tag().as_deref(), Some("b"), "{var}");

    // A refused edit leaves the old trigger in place (MySQL tries it under a scratch name first).
    let bad = def("rt_bi", "SET NEW.nope_column = 1");
    if let Err(err) = e.trigger("rt_trig", &bad, true) {
        assert!(!err.is_empty());
        assert_eq!(e.tag().as_deref(), Some("b"), "{var}: the old trigger survived {err}");
        assert_eq!(body_of(&mut e, "rt_bi").as_deref(), Some("SET NEW.tag = 'b'"));
        assert!(!e.structure("rt_trig").triggers.iter().any(|t| t.name.starts_with("quarry_check_")), "{var}: no scratch trigger is left");
    } else {
        // MariaDB checks NEW columns when the trigger runs, not when it is made.
        assert_eq!(e.db.dialect, Dialect::MariaDb, "{var}: MySQL must refuse a missing column");
        e.trigger("rt_trig", &b, true).unwrap();
    }

    // Undo restores the old body.
    e.undo(edit_undo).unwrap_or_else(|er| panic!("{var}: {er}"));
    assert_eq!(e.tag().as_deref(), Some("a"), "{var}: undo restored the previous body");
    // Undo is refused when the trigger was changed by someone else.
    let again = e.trigger("rt_trig", &b, true).unwrap();
    e.exec("DROP TRIGGER quarry_rt.rt_bi");
    e.exec("CREATE TRIGGER quarry_rt.rt_bi BEFORE INSERT ON quarry_rt.rt_trig FOR EACH ROW SET NEW.tag = 'z'");
    let refused = e.undo(again).unwrap_err();
    assert!(refused.contains("changed after this change ran"), "{var}: {refused}");
    assert_eq!(e.tag().as_deref(), Some("z"), "{var}: a refused undo changes nothing");
    e.exec("DROP TRIGGER quarry_rt.rt_bi");
    // Undo of a create whose trigger is already gone changes nothing and says so.
    assert!(e.undo(create_undo).is_ok());

    // Drop, and undo of a drop.
    e.trigger("rt_trig", &a, false).unwrap();
    let drop_undo = e.drop_trigger("rt_trig", "rt_bi").unwrap_or_else(|er| panic!("{var}: {er}"));
    assert!(body_of(&mut e, "rt_bi").is_none());
    e.undo(drop_undo).unwrap_or_else(|er| panic!("{var}: {er}"));
    assert_eq!(e.tag().as_deref(), Some("a"), "{var}: undo of drop");
    e.drop_trigger("rt_trig", "rt_bi").unwrap();

    // A compound body with a SIGNAL, a hostile name and a quote in a string.
    let guard = TrigDef { name: "rt`;x".into(), timing: "BEFORE".into(), event: "UPDATE".into(), body: "BEGIN\n  IF NEW.tag = 'it''s bad' THEN\n    SIGNAL SQLSTATE '45000' SET MESSAGE_TEXT = 'no';\n  END IF;\nEND".into(), pg: PgDef::default() };
    let undo = e.trigger("rt_trig", &guard, false).unwrap_or_else(|er| panic!("{var}: {er}"));
    e.exec("INSERT INTO quarry_rt.rt_trig (id, tag) VALUES (500, 'fine')");
    assert!(e.try_exec("UPDATE quarry_rt.rt_trig SET tag = 'it''s bad' WHERE id = 500").is_err(), "{var}: the body ran");
    assert!(e.structure("rt_trig").triggers.iter().any(|t| t.name == "rt`;x"));
    e.undo(undo).unwrap_or_else(|er| panic!("{var}: {er}"));
    assert!(e.structure("rt_trig").triggers.is_empty(), "{var}: undo of create");
    e.exec("UPDATE quarry_rt.rt_trig SET tag = 'it''s bad' WHERE id = 500");

    // Writes stay locked until the caller unlocks them.
    e.w.sql_trigger(1, e.db.clone(), "quarry_rt".into(), "rt_trig".into(), TrigJob::Create(TrigSave { def: a.clone(), definer: None, order: None }), Mode::of(&e.db.sql_mode), true, "done".into());
    let Msg::SqlAltered { res, .. } = recv(&e.rx) else { panic!() };
    assert!(res.is_err());
    assert!(e.structure("rt_trig").triggers.is_empty());

    e.exec("DROP DATABASE quarry_rt");
    e.exec("DROP DATABASE quarry_rt_b");
}

/// The review of 49a4432 and c5c77f1, each finding with the input that proved it.
fn review(var: &str) {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut e) = start(var) else { return };
    let mysql = e.db.dialect == Dialect::MySql;
    let def = |name: &str, body: &str| TrigDef { name: name.into(), timing: "BEFORE".into(), event: "INSERT".into(), body: body.into(), pg: PgDef::default() };
    let exists = |e: &mut Env, name: &str| e.live_trigger("rt_trig", name).is_some();
    let clear = |e: &Env| e.exec("DELETE FROM quarry_rt.rt_trig");

    // Finding 1: a body that smuggles a statement after its END is refused before anything runs, also by the
    // worker when it is handed one.
    e.exec("CREATE TABLE quarry_rt.rt_victim (x INT)");
    for body in ["BEGIN END; DROP TABLE quarry_rt.rt_victim", "BEGIN /*!99999 BEGIN */ END; DROP TABLE quarry_rt.rt_victim; END"] {
        let (res, undo, _) = { let db = e.db.clone(); e.job_on(&db, "rt_trig", TrigJob::Create(TrigSave { def: def("rt_s", body), definer: None, order: None })) };
        assert!(res.is_err() && undo.is_none(), "{var}: {body}");
        assert!(!exists(&mut e, "rt_s"), "{var}: {body}");
    }
    assert_eq!(e.scalar("SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = 'quarry_rt' AND TABLE_NAME = 'rt_victim'").as_deref(), Some("1"), "{var}");
    e.exec("CREATE TRIGGER quarry_rt.rt_n BEFORE INSERT ON quarry_rt.rt_trig FOR EACH ROW SET NEW.tag = 'old'");
    let r = e.trigger("rt_trig", &def("rt_n", "BEGIN SET NEW.tag='new'; END; DELIMITER ;"), true);
    assert!(r.is_err(), "{var}");
    assert_eq!(e.tag().as_deref(), Some("old"), "{var}: nothing changed on the server");
    e.exec("DROP TRIGGER quarry_rt.rt_n");
    // Finding 11: bodies the old check refused are saved and run.
    let u = e.trigger("rt_trig", &def("rt_if", "IF NEW.tag IS NULL THEN SET NEW.tag = 'n'; END IF"), false).unwrap_or_else(|x| panic!("{var}: {x}"));
    assert_eq!(e.tag().as_deref(), Some("n"));
    e.undo(u).unwrap();
    let u = e.trigger("rt_trig", &def("rt_lbl", "lbl: BEGIN SET NEW.tag = 'l'; END lbl"), false).unwrap_or_else(|x| panic!("{var}: {x}"));
    assert_eq!(e.tag().as_deref(), Some("l"));
    e.undo(u).unwrap();
    clear(&e);

    // Finding 2: a trigger made under PIPES_AS_CONCAT, edited and undone, still concatenates.
    e.exec_in_mode("PIPES_AS_CONCAT", "CREATE TRIGGER quarry_rt.rt_p BEFORE INSERT ON quarry_rt.rt_trig FOR EACH ROW SET NEW.tag = 'a' || 'b'");
    assert_eq!(e.tag().as_deref(), Some("ab"), "{var}");
    let u = e.trigger("rt_trig", &def("rt_p", "SET NEW.tag = 'x'"), true).unwrap_or_else(|x| panic!("{var}: {x}"));
    assert_eq!(e.tag().as_deref(), Some("x"));
    e.undo(u).unwrap_or_else(|x| panic!("{var}: {x}"));
    assert_eq!(e.tag().as_deref(), Some("ab"), "{var}: the undone trigger runs in its own mode");
    assert!(e.live_trigger("rt_trig", "rt_p").unwrap().sql_mode.contains("PIPES_AS_CONCAT"), "{var}");
    // And a dropped one comes back in it too.
    let u = e.drop_trigger("rt_trig", "rt_p").unwrap();
    e.undo(u).unwrap();
    assert_eq!(e.tag().as_deref(), Some("ab"), "{var}");
    e.exec("DROP TRIGGER quarry_rt.rt_p");

    // Finding 3: a trigger made under ANSI_QUOTES survives a refused edit and an edit and its undo.
    e.exec_in_mode("ANSI_QUOTES", "CREATE TRIGGER quarry_rt.rt_q BEFORE INSERT ON quarry_rt.rt_trig FOR EACH ROW SET NEW.\"tag\" = 'q'");
    assert_eq!(e.tag().as_deref(), Some("q"));
    let refused = e.trigger("rt_trig", &def("rt_q", "SET NEW.nope = 1"), true);
    if mysql {
        assert!(refused.is_err(), "{var}");
    }
    assert!(exists(&mut e, "rt_q"), "{var}: the old trigger stays after a refused edit");
    if refused.is_ok() {
        e.exec("DROP TRIGGER quarry_rt.rt_q");
        e.exec_in_mode("ANSI_QUOTES", "CREATE TRIGGER quarry_rt.rt_q BEFORE INSERT ON quarry_rt.rt_trig FOR EACH ROW SET NEW.\"tag\" = 'q'");
    }
    let u = e.trigger("rt_trig", &def("rt_q", "SET NEW.tag = 'y'"), true).unwrap();
    assert_eq!(e.tag().as_deref(), Some("y"));
    e.undo(u).unwrap_or_else(|x| panic!("{var}: {x}"));
    assert_eq!(e.tag().as_deref(), Some("q"), "{var}: the ANSI_QUOTES body is back");
    e.exec("DROP TRIGGER quarry_rt.rt_q");
    // An undo whose old trigger the server now refuses keeps a trigger: the edited one is put back.
    e.exec("ALTER TABLE quarry_rt.rt_trig ADD COLUMN extra INT NULL");
    e.exec("CREATE TRIGGER quarry_rt.rt_x BEFORE INSERT ON quarry_rt.rt_trig FOR EACH ROW SET NEW.extra = 1, NEW.tag = 'old'");
    let u = e.trigger("rt_trig", &def("rt_x", "SET NEW.tag = 'new'"), true).unwrap();
    e.exec("ALTER TABLE quarry_rt.rt_trig DROP COLUMN extra");
    let r = e.undo(u);
    assert!(exists(&mut e, "rt_x"), "{var}: a trigger is left after the undo ({r:?})");
    if mysql {
        assert!(r.unwrap_err().contains("put back"), "{var}");
        assert_eq!(e.tag().as_deref(), Some("new"), "{var}: the edited trigger is the one put back");
    }
    e.exec("DROP TRIGGER quarry_rt.rt_x");
    // An undo that finds no trigger, as a run that stopped halfway leaves, makes the old one again.
    e.exec("CREATE TRIGGER quarry_rt.rt_h BEFORE INSERT ON quarry_rt.rt_trig FOR EACH ROW SET NEW.tag = 'h1'");
    let u = e.trigger("rt_trig", &def("rt_h", "SET NEW.tag = 'h2'"), true).unwrap();
    e.exec("DROP TRIGGER quarry_rt.rt_h");
    e.undo(u).unwrap_or_else(|x| panic!("{var}: {x}"));
    assert_eq!(e.tag().as_deref(), Some("h1"), "{var}");
    e.exec("DROP TRIGGER quarry_rt.rt_h");
    clear(&e);

    // Finding 5: two triggers that append keep their order through an edit, its undo, and a drop and its undo.
    e.exec("CREATE TRIGGER quarry_rt.rt_1 BEFORE INSERT ON quarry_rt.rt_trig FOR EACH ROW SET NEW.tag = CONCAT(IFNULL(NEW.tag, ''), '1')");
    e.exec("CREATE TRIGGER quarry_rt.rt_2 BEFORE INSERT ON quarry_rt.rt_trig FOR EACH ROW SET NEW.tag = CONCAT(IFNULL(NEW.tag, ''), '2')");
    assert_eq!(e.tag().as_deref(), Some("12"));
    let u = e.trigger("rt_trig", &def("rt_1", "SET NEW.tag = CONCAT(IFNULL(NEW.tag, ''), '1') /* same */"), true).unwrap();
    assert_eq!(e.tag().as_deref(), Some("12"), "{var}: after the edit");
    e.undo(u).unwrap();
    assert_eq!(e.tag().as_deref(), Some("12"), "{var}: after its undo");
    let u = e.drop_trigger("rt_trig", "rt_1").unwrap();
    assert_eq!(e.tag().as_deref(), Some("2"));
    e.undo(u).unwrap();
    assert_eq!(e.tag().as_deref(), Some("12"), "{var}: after the drop and its undo");
    let u = e.trigger("rt_trig", &def("rt_2", "SET NEW.tag = CONCAT(IFNULL(NEW.tag, ''), '2') /* same */"), true).unwrap();
    assert_eq!(e.tag().as_deref(), Some("12"), "{var}: after editing the last one");
    e.undo(u).unwrap();
    e.exec("DROP TRIGGER quarry_rt.rt_1");
    e.exec("DROP TRIGGER quarry_rt.rt_2");
    clear(&e);

    // Finding 6: an edited trigger keeps its definer, and a login that may not create one for another user is told
    // so and changes nothing.
    let _ = e.try_exec("DROP USER IF EXISTS 'qrf_owner'@'%'");
    let _ = e.try_exec("DROP USER IF EXISTS 'qrf_low'@'%'");
    e.exec("CREATE USER 'qrf_owner'@'%' IDENTIFIED BY 'x'");
    e.exec("CREATE USER 'qrf_low'@'%' IDENTIFIED BY 'qrf_low_pw'");
    e.exec("GRANT ALL ON quarry_rt.* TO 'qrf_owner'@'%'");
    e.exec("GRANT SELECT, INSERT, TRIGGER, LOCK TABLES ON quarry_rt.* TO 'qrf_low'@'%'");
    e.exec("CREATE DEFINER='qrf_owner'@'%' TRIGGER quarry_rt.rt_d BEFORE INSERT ON quarry_rt.rt_trig FOR EACH ROW SET NEW.tag = 'd'");
    let definer = |e: &Env| e.scalar("SELECT DEFINER FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = 'quarry_rt' AND TRIGGER_NAME = 'rt_d'");
    let u = e.trigger("rt_trig", &def("rt_d", "SET NEW.tag = 'e'"), true).unwrap_or_else(|x| panic!("{var}: {x}"));
    assert_eq!(definer(&e).as_deref(), Some("qrf_owner@%"), "{var}: the definer is kept");
    e.undo(u).unwrap();
    assert_eq!(definer(&e).as_deref(), Some("qrf_owner@%"));
    let low = {
        let pool = pinned(&e.w, e.db.opts.my().unwrap().clone().username("qrf_low").password("qrf_low_pw"), &e.mode);
        Db { pool: Pool::My(pool), ..e.db.clone() }
    };
    let all = e.structure("rt_trig").triggers;
    let was = all.iter().find(|t| t.name == "rt_d").cloned();
    let (res, _, _) = e.job_on(&low, "rt_trig", save_job(&all, was.as_ref(), &def("rt_d", "SET NEW.tag = 'f'")));
    let err = res.unwrap_err();
    assert!(err.contains("SET_USER_ID") && err.contains("Nothing was changed"), "{var}: {err}");
    assert_eq!(e.tag().as_deref(), Some("d"), "{var}: the old trigger stays");
    assert_eq!(definer(&e).as_deref(), Some("qrf_owner@%"));
    e.exec("DROP TRIGGER quarry_rt.rt_d");
    e.exec("DROP USER 'qrf_owner'@'%'");
    e.exec("DROP USER 'qrf_low'@'%'");
    clear(&e);

    // Finding 12: the worker reads the writer's own sql_mode, so a session in ANSI_QUOTES the app did not know of
    // runs nothing and hands the mode back.
    let ansi = Db { pool: Pool::My(pinned(&e.w, e.db.opts.my().unwrap().clone(), &format!("{},ANSI_QUOTES", e.mode))), ..e.db.clone() };
    let (res, _, seen) = e.job_on(&ansi, "rt_trig", TrigJob::Create(TrigSave { def: def("rt_m", r#"SET NEW.tag = "m""#), definer: None, order: None }));
    assert!(res.unwrap_err().contains("Nothing ran"), "{var}");
    assert!(seen.is_some_and(|m| Mode::of(&m).ansi), "{var}");
    assert!(!exists(&mut e, "rt_m"));

    // Finding 4: staged [Drop column wrong, AddFk fk_mix] takes the checked ALTER path: its undo is one ALTER per
    // step, refused when the key changed since, and run when it did not.
    let k = key("fk_mix", "rt_orders", &["customer_id"], "quarry_rt", "rt_customers", &["id"]);
    let before = shape(&e.structure("rt_orders"));
    let cols = e.structure("rt_orders").columns.len();
    let undo = e.apply("rt_orders", &[Change::Drop("wrong".into()), Change::AddFk(k)]).unwrap_or_else(|x| panic!("{var}: {x}"));
    let UndoKind::Sql { undo: crate::db::sql::write::SqlUndo::Alter(au), .. } = &undo.kind else { panic!("{var}: not the checked ALTER undo") };
    assert!(au.steps.len() >= 2 && au.fks == ["fk_mix"] && au.expect.iter().any(|(w, v)| w == "foreign key fk_mix" && v.is_some()), "{var}: {au:?}");
    e.exec("ALTER TABLE quarry_rt.rt_orders DROP FOREIGN KEY fk_mix");
    e.w.sql_undo(e.db.clone(), undo, Vec::new(), false);
    let Msg::SqlAltered { res, undo: kept, .. } = recv(&e.rx) else { panic!("expected SqlAltered") };
    let refused = res.unwrap_err();
    assert!(refused.contains("foreign key fk_mix changed"), "{var}: {refused}");
    let undo = kept.expect("a refused undo stays in the list");
    e.exec("ALTER TABLE quarry_rt.rt_orders ADD CONSTRAINT fk_mix FOREIGN KEY (customer_id) REFERENCES rt_customers (id) ON UPDATE CASCADE ON DELETE RESTRICT");
    e.undo(undo).unwrap_or_else(|x| panic!("{var}: {x}"));
    assert_eq!(shape(&e.structure("rt_orders")), before, "{var}");
    assert_eq!(e.structure("rt_orders").columns.len(), cols);

    // Finding 10: on MySQL 8.4 a key on part of a key is refused by the server (6125), and the pre-check says so first.
    e.n += 1;
    e.w.sql_fk_rules(e.n, e.db.clone());
    let Msg::SqlFkRules { strict, .. } = recv(&e.rx) else { panic!("expected SqlFkRules") };
    assert_eq!(strict, mysql && fk_whole_key_default(e.db.dialect, &e.db.version), "{var}");
    e.exec("CREATE TABLE quarry_rt.rt_loose (x INT NOT NULL, y INT NOT NULL, KEY idx_x (x))");
    e.exec("CREATE TABLE quarry_rt.rt_child (id INT PRIMARY KEY, x INT NULL)");
    let k = key("fk_loose", "rt_child", &["x"], "quarry_rt", "rt_loose", &["x"]);
    let (own, parent) = (e.structure("rt_child"), e.structure("rt_loose"));
    let pre = fk_problem(&own, "rt_child", &parent, &k, strict);
    let ran = e.apply("rt_child", &[Change::AddFk(k)]).map(|_| ());
    assert_eq!(pre.is_some(), ran.is_err(), "{var}: the pre-check agrees with the server ({pre:?}, {ran:?})");
    if strict {
        assert!(ran.unwrap_err().contains("6125"), "{var}");
    }

    e.exec("DROP DATABASE quarry_rt");
    e.exec("DROP DATABASE quarry_rt_b");
}

#[test]
fn live_foreign_keys_mysql() {
    foreign_keys("QUARRY_TEST_MYSQL");
}

#[test]
fn live_foreign_keys_mariadb() {
    foreign_keys("QUARRY_TEST_MARIADB");
}

#[test]
fn live_triggers_mysql() {
    triggers("QUARRY_TEST_MYSQL");
}

#[test]
fn live_triggers_mariadb() {
    triggers("QUARRY_TEST_MARIADB");
}

#[test]
fn live_review_mysql() {
    review("QUARRY_TEST_MYSQL");
}

#[test]
fn live_review_mariadb() {
    review("QUARRY_TEST_MARIADB");
}
