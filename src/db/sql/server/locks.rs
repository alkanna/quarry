//! The Locks tab of the MySQL and MariaDB Server window: who waits on whom, from the server's own lock tables. MySQL 8.0
//! has them in performance_schema (row locks in data_lock_waits, metadata locks in metadata_locks); MySQL 5.7 and MariaDB
//! keep them in information_schema (INNODB_LOCK_WAITS). Each wait is an edge from a waiting thread to the thread it waits
//! behind, and `chains` turns the edges into the blocker trees the tab draws, the way the PostgreSQL tab does.

use super::{me_and_identity, proc_list, rows, Proc};
use crate::db::sql::worker::text;
use crate::db::sql::{Db, Dialect};
use crate::db::{e, Res};
use std::collections::{HashMap, HashSet};

/// One thread waiting for a lock another thread holds.
#[derive(Clone, Debug, PartialEq)]
pub struct LockWait {
    pub waiter: u64,
    pub blocker: u64,
    /// "Row", "Table" or "Metadata".
    pub kind: String,
    /// What is locked, such as `shop.orders` or `shop.orders (PRIMARY)`.
    pub on: String,
    /// What the waiter asked for, in the server's words ("X,REC_NOT_GAP", "EXCLUSIVE").
    pub mode: String,
}

/// What an open InnoDB transaction says about its thread.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Trx {
    pub id: u64,
    /// Seconds since the transaction began.
    pub open_secs: Option<f64>,
    /// Seconds it has waited for the lock it is stuck on.
    pub wait_secs: Option<f64>,
    pub query: String,
}

/// Everything the Locks tab shows.
#[derive(Clone, Debug, Default)]
pub struct Locks {
    pub waits: Vec<LockWait>,
    /// The threads of SHOW FULL PROCESSLIST by id, for the statements and for Kill.
    pub procs: HashMap<u64, Proc>,
    pub trx: HashMap<u64, Trx>,
    /// The connection that ran the reads, so the window can mark it as Quarry's own.
    pub me: u64,
    pub identity: String,
    /// What the server could not tell, such as a metadata lock instrument that is off. Shown under the list.
    pub notes: Vec<String>,
    /// Where the waits were read from, for the line over the list.
    pub source: &'static str,
}

const TRX: &str = "SELECT trx_mysql_thread_id AS id, TIMESTAMPDIFF(SECOND, trx_started, NOW()) AS open_secs, TIMESTAMPDIFF(SECOND, trx_wait_started, NOW()) AS wait_secs, trx_query AS query FROM information_schema.INNODB_TRX";

/// MySQL 8.0 row locks. The thread ids are processlist ids; a transaction whose thread performance_schema no longer
/// knows still names its connection in INNODB_TRX.
const PS_ROWS: &str = "SELECT COALESCE(rt.PROCESSLIST_ID, rx.trx_mysql_thread_id) AS req_id, COALESCE(bt.PROCESSLIST_ID, bx.trx_mysql_thread_id) AS blk_id, l.OBJECT_SCHEMA AS obj_schema, l.OBJECT_NAME AS obj_name, l.INDEX_NAME AS index_name, l.LOCK_TYPE AS lock_type, l.LOCK_MODE AS lock_mode \
    FROM performance_schema.data_lock_waits w \
    JOIN performance_schema.data_locks l ON l.ENGINE_LOCK_ID = w.REQUESTING_ENGINE_LOCK_ID AND l.ENGINE = w.ENGINE \
    LEFT JOIN performance_schema.threads rt ON rt.THREAD_ID = w.REQUESTING_THREAD_ID \
    LEFT JOIN performance_schema.threads bt ON bt.THREAD_ID = w.BLOCKING_THREAD_ID \
    LEFT JOIN information_schema.INNODB_TRX rx ON rx.trx_id = w.REQUESTING_ENGINE_TRANSACTION_ID \
    LEFT JOIN information_schema.INNODB_TRX bx ON bx.trx_id = w.BLOCKING_ENGINE_TRANSACTION_ID";

/// MySQL 8.0 metadata lock waits: a pending request against the granted locks on the same object that it cannot share
/// with. The mode test is an approximation, since the server's own conflict matrix is not exposed.
const PS_MDL: &str = "SELECT pt.PROCESSLIST_ID AS req_id, gt.PROCESSLIST_ID AS blk_id, p.OBJECT_TYPE AS obj_type, p.OBJECT_SCHEMA AS obj_schema, p.OBJECT_NAME AS obj_name, p.LOCK_TYPE AS lock_mode \
    FROM performance_schema.metadata_locks p \
    JOIN performance_schema.metadata_locks g ON g.OBJECT_TYPE = p.OBJECT_TYPE AND g.OBJECT_SCHEMA <=> p.OBJECT_SCHEMA AND g.OBJECT_NAME <=> p.OBJECT_NAME AND g.LOCK_STATUS = 'GRANTED' AND g.OWNER_THREAD_ID <> p.OWNER_THREAD_ID \
    JOIN performance_schema.threads pt ON pt.THREAD_ID = p.OWNER_THREAD_ID \
    JOIN performance_schema.threads gt ON gt.THREAD_ID = g.OWNER_THREAD_ID \
    WHERE p.LOCK_STATUS = 'PENDING' AND pt.PROCESSLIST_ID IS NOT NULL AND gt.PROCESSLIST_ID IS NOT NULL AND (p.LOCK_TYPE = 'EXCLUSIVE' OR g.LOCK_TYPE IN ('EXCLUSIVE', 'SHARED_NO_WRITE', 'SHARED_NO_READ_WRITE'))";

const MDL_INSTRUMENT: &str = "SELECT ENABLED AS enabled FROM performance_schema.setup_instruments WHERE NAME = 'wait/lock/metadata/sql/mdl'";

/// MySQL 5.7 and MariaDB row locks, with what was asked for from INNODB_LOCKS.
const IDB_ROWS: &str = "SELECT r.trx_mysql_thread_id AS req_id, b.trx_mysql_thread_id AS blk_id, l.lock_table AS obj_name, l.lock_index AS index_name, l.lock_type AS lock_type, l.lock_mode AS lock_mode \
    FROM information_schema.INNODB_LOCK_WAITS w \
    JOIN information_schema.INNODB_TRX r ON r.trx_id = w.requesting_trx_id \
    JOIN information_schema.INNODB_TRX b ON b.trx_id = w.blocking_trx_id \
    LEFT JOIN information_schema.INNODB_LOCKS l ON l.lock_id = w.requested_lock_id";

/// The same without INNODB_LOCKS, for a MariaDB that no longer has the table: the waits stay, the object and mode go.
const IDB_ROWS_BARE: &str = "SELECT r.trx_mysql_thread_id AS req_id, b.trx_mysql_thread_id AS blk_id \
    FROM information_schema.INNODB_LOCK_WAITS w \
    JOIN information_schema.INNODB_TRX r ON r.trx_id = w.requesting_trx_id \
    JOIN information_schema.INNODB_TRX b ON b.trx_id = w.blocking_trx_id";

fn number(x: &sqlx::Error) -> Option<u16> {
    match x {
        sqlx::Error::Database(d) => d.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>().map(|m| m.number()),
        _ => None,
    }
}

/// Whether the error says the table is not there (1146, 1109) or its engine is off (1286): MySQL 5.7 has no
/// performance_schema.data_lock_waits, and a server started without performance_schema has none of its tables.
fn table_missing(n: Option<u16>) -> bool {
    matches!(n, Some(1146 | 1109 | 1286))
}

/// What a refused read of the lock tables means to the person who opened the tab, from the server's error number.
/// `ps` is the performance_schema read; the information_schema ones need the PROCESS privilege.
pub fn lock_denied(n: Option<u16>, ps: bool) -> Option<String> {
    match n {
        Some(1227) => Some("Your account cannot read the locks: it needs the PROCESS privilege.".into()),
        Some(1142 | 1143 | 1044) if ps => Some("Your account cannot read the locks: it needs SELECT on performance_schema (data_locks, data_lock_waits and threads) and the PROCESS privilege.".into()),
        Some(1142 | 1143 | 1044) => Some("Your account cannot read the locks: it needs the PROCESS privilege.".into()),
        _ => None,
    }
}

const PS_OFF: &str = "MySQL 8 reports lock waits through performance_schema, which is off on this server. Set performance_schema=ON in the option file and restart the server.";

const MDL_OFF: &str = "Metadata lock waits are not recorded: the wait/lock/metadata/sql/mdl instrument is off. Turn it on with UPDATE performance_schema.setup_instruments SET ENABLED = 'YES' WHERE NAME = 'wait/lock/metadata/sql/mdl'; only locks taken afterwards are seen.";

fn num(v: Option<String>) -> Option<u64> {
    v.and_then(|s| s.trim().parse().ok()).filter(|n| *n != 0)
}

fn secs(v: Option<String>) -> Option<f64> {
    v.and_then(|s| s.trim().parse::<f64>().ok()).map(|s| s.max(0.0))
}

/// `db.table`, or what the server calls the object when it has no table.
fn object(schema: &str, name: &str, index: &str) -> String {
    let dotted: Vec<&str> = [schema, name].into_iter().filter(|s| !s.is_empty()).collect();
    let on = dotted.join(".");
    if index.is_empty() || on.is_empty() { on } else { format!("{on} ({index})") }
}

/// A row-lock wait from either source's columns, or None when a thread id is missing: the server has already forgotten
/// one of the two sessions then, and there is nothing to name.
pub fn row_wait(get: &dyn Fn(&str) -> Option<String>) -> Option<LockWait> {
    let (waiter, blocker) = (num(get("req_id"))?, num(get("blk_id"))?);
    let s = |n: &str| get(n).unwrap_or_default();
    // 5.7 and MariaDB print the table as `shop`.`orders`.
    let table = s("obj_name").replace('`', "");
    let on = object(&s("obj_schema"), &table, &s("index_name"));
    let kind = if s("lock_type").eq_ignore_ascii_case("TABLE") { "Table" } else { "Row" };
    Some(LockWait { waiter, blocker, kind: kind.into(), on, mode: s("lock_mode") })
}

/// A metadata lock wait from `PS_MDL`'s columns.
pub fn mdl_wait(get: &dyn Fn(&str) -> Option<String>) -> Option<LockWait> {
    let (waiter, blocker) = (num(get("req_id"))?, num(get("blk_id"))?);
    let s = |n: &str| get(n).unwrap_or_default();
    let on = match object(&s("obj_schema"), &s("obj_name"), "") {
        o if o.is_empty() => s("obj_type").to_lowercase(),
        o => o,
    };
    Some(LockWait { waiter, blocker, kind: "Metadata".into(), on, mode: s("lock_mode") })
}

pub fn trx_from(get: &dyn Fn(&str) -> Option<String>) -> Option<Trx> {
    Some(Trx { id: num(get("id"))?, open_secs: secs(get("open_secs")), wait_secs: secs(get("wait_secs")), query: get("query").unwrap_or_default() })
}

/// One edge per waiter and blocker pair of a kind: a lock wait is listed once per lock it needs, and the tab wants the
/// threads, not every record lock.
pub fn dedupe(waits: Vec<LockWait>) -> Vec<LockWait> {
    let mut seen = HashSet::new();
    waits.into_iter().filter(|w| w.waiter != w.blocker && seen.insert((w.waiter, w.blocker, w.kind.clone()))).collect()
}

/// A blocking chain: the thread at its head and those queued behind it, each with its depth.
#[derive(Debug, PartialEq)]
pub struct Chain {
    pub head: u64,
    pub waiting: Vec<(u64, usize)>,
}

/// The blocking chains of a list of waits, heads in id order. A waiter goes under a blocker that does not wait itself when
/// it has one, so a thread that holds the lock it needs leads; one blocked only by other waiters queues behind them and goes
/// a level deeper. Waiters in a cycle with no head (a deadlock the server has not broken yet) lead a chain of their own.
pub fn chains(waits: &[LockWait]) -> Vec<Chain> {
    let mut by_waiter: HashMap<u64, Vec<u64>> = HashMap::new();
    for w in waits {
        let b = by_waiter.entry(w.waiter).or_default();
        if !b.contains(&w.blocker) {
            b.push(w.blocker);
        }
    }
    let parent = |waiter: u64| {
        let bs = &by_waiter[&waiter];
        bs.iter().copied().find(|b| !by_waiter.contains_key(b)).unwrap_or(bs[0])
    };
    let mut order: Vec<u64> = by_waiter.keys().copied().collect();
    order.sort();
    let mut kids: HashMap<u64, Vec<u64>> = HashMap::new();
    for w in &order {
        kids.entry(parent(*w)).or_default().push(*w);
    }
    let mut heads: Vec<u64> = kids.keys().copied().filter(|p| !by_waiter.contains_key(p)).collect();
    heads.sort();
    fn walk(p: u64, depth: usize, kids: &HashMap<u64, Vec<u64>>, placed: &mut HashSet<u64>, into: &mut Vec<(u64, usize)>) {
        for k in kids.get(&p).into_iter().flatten() {
            if placed.insert(*k) {
                into.push((*k, depth));
                walk(*k, depth + 1, kids, placed, into);
            }
        }
    }
    let mut placed = HashSet::new();
    let mut out = Vec::new();
    for h in heads {
        let mut waiting = Vec::new();
        placed.insert(h);
        walk(h, 1, &kids, &mut placed, &mut waiting);
        out.push(Chain { head: h, waiting });
    }
    for w in order {
        if placed.insert(w) {
            let mut waiting = Vec::new();
            walk(w, 1, &kids, &mut placed, &mut waiting);
            out.push(Chain { head: w, waiting });
        }
    }
    out
}

fn parse<T>(list: &[sqlx::mysql::MySqlRow], f: fn(&dyn Fn(&str) -> Option<String>) -> Option<T>) -> Vec<T> {
    list.iter().filter_map(|r| f(&|n| text(r, n))).collect()
}

/// MySQL 5.7 and MariaDB row locks. INNODB_LOCKS is gone from newer MariaDB, so a read without it is the second try.
async fn innodb_waits(c: &mut sqlx::MySqlConnection) -> Res<Vec<LockWait>> {
    let list = match rows(c, IDB_ROWS).await {
        Ok(l) => l,
        Err(x) if table_missing(number(&x)) => rows(c, IDB_ROWS_BARE).await.map_err(|x| lock_denied(number(&x), false).unwrap_or_else(|| e(x)))?,
        Err(x) => return Err(lock_denied(number(&x), false).unwrap_or_else(|| e(x))),
    };
    Ok(parse(&list, row_wait))
}

pub(super) async fn read(db: &Db) -> Res<Locks> {
    db.mysql_only()?;
    let d = db.dialect;
    let mut c = db.acquire().await?;
    let (me, identity) = me_and_identity(&mut c, d).await?;
    let procs = proc_list(&mut c).await?;
    // Needs PROCESS on every server, so a missing privilege is told here and not as an empty list.
    let trx = rows(&mut c, TRX).await.map_err(|x| lock_denied(number(&x), false).unwrap_or_else(|| e(x)))?;
    let trx: HashMap<u64, Trx> = parse(&trx, trx_from).into_iter().map(|t| (t.id, t)).collect();
    let mut notes = Vec::new();
    let (waits, source) = if d == Dialect::MariaDb {
        notes.push("Waits for metadata locks are not shown on MariaDB.".to_string());
        (innodb_waits(&mut c).await?, "information_schema.INNODB_LOCK_WAITS")
    } else {
        match rows(&mut c, PS_ROWS).await {
            Ok(list) => {
                let mut waits = parse(&list, row_wait);
                match rows(&mut c, PS_MDL).await {
                    Ok(list) => {
                        let mdl: Vec<LockWait> = parse(&list, mdl_wait);
                        if mdl.is_empty() && rows(&mut c, MDL_INSTRUMENT).await.ok().and_then(|r| r.first().and_then(|r| text(r, "enabled"))).is_some_and(|v| v.eq_ignore_ascii_case("NO")) {
                            notes.push(MDL_OFF.to_string());
                        }
                        waits.extend(mdl);
                    }
                    Err(x) if table_missing(number(&x)) => notes.push(MDL_OFF.to_string()),
                    Err(x) => notes.push(format!("Metadata lock waits could not be read: {}", lock_denied(number(&x), true).unwrap_or_else(|| e(x)))),
                }
                (waits, "performance_schema.data_lock_waits")
            }
            // MySQL 5.7 has no data_lock_waits, and a server without performance_schema has none of its tables.
            Err(x) if table_missing(number(&x)) => match innodb_waits(&mut c).await {
                Ok(w) => (w, "information_schema.INNODB_LOCK_WAITS"),
                Err(_) => return Err(PS_OFF.into()),
            },
            Err(x) => return Err(lock_denied(number(&x), true).unwrap_or_else(|| e(x))),
        }
    };
    Ok(Locks { waits: dedupe(waits), procs: procs.into_iter().map(|p| (p.id, p)).collect(), trx, me, identity, notes, source })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sql::testing::every_server;

    fn row(cols: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = cols.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |n| m.get(n).cloned()
    }

    fn wait(waiter: u64, blocker: u64) -> LockWait {
        LockWait { waiter, blocker, kind: "Row".into(), on: "shop.orders".into(), mode: "X".into() }
    }

    #[test]
    fn a_mysql_8_row_wait_names_the_table_and_index() {
        let w = row_wait(&row(&[("req_id", "41"), ("blk_id", "40"), ("obj_schema", "shop"), ("obj_name", "orders"), ("index_name", "PRIMARY"), ("lock_type", "RECORD"), ("lock_mode", "X,REC_NOT_GAP")])).unwrap();
        assert_eq!((w.waiter, w.blocker, w.kind.as_str(), w.on.as_str(), w.mode.as_str()), (41, 40, "Row", "shop.orders (PRIMARY)", "X,REC_NOT_GAP"));
    }

    #[test]
    fn a_5_7_or_mariadb_wait_strips_the_quotes_of_lock_table() {
        let w = row_wait(&row(&[("req_id", "7"), ("blk_id", "6"), ("obj_name", "`shop`.`orders`"), ("index_name", "PRIMARY"), ("lock_type", "RECORD"), ("lock_mode", "X")])).unwrap();
        assert_eq!((w.on.as_str(), w.kind.as_str()), ("shop.orders (PRIMARY)", "Row"));
        let t = row_wait(&row(&[("req_id", "7"), ("blk_id", "6"), ("obj_name", "`shop`.`orders`"), ("lock_type", "TABLE"), ("lock_mode", "IX")])).unwrap();
        assert_eq!((t.on.as_str(), t.kind.as_str()), ("shop.orders", "Table"));
        // Without INNODB_LOCKS the wait stays and the object is unknown.
        let bare = row_wait(&row(&[("req_id", "7"), ("blk_id", "6")])).unwrap();
        assert_eq!((bare.on.as_str(), bare.mode.as_str()), ("", ""));
    }

    #[test]
    fn a_wait_with_a_forgotten_session_is_dropped() {
        assert!(row_wait(&row(&[("req_id", "7")])).is_none());
        assert!(row_wait(&row(&[("req_id", "7"), ("blk_id", "0")])).is_none());
        assert!(row_wait(&row(&[("req_id", ""), ("blk_id", "6")])).is_none());
    }

    #[test]
    fn a_metadata_wait_names_its_object_or_its_type() {
        let w = mdl_wait(&row(&[("req_id", "9"), ("blk_id", "8"), ("obj_type", "TABLE"), ("obj_schema", "shop"), ("obj_name", "orders"), ("lock_mode", "EXCLUSIVE")])).unwrap();
        assert_eq!((w.kind.as_str(), w.on.as_str(), w.mode.as_str()), ("Metadata", "shop.orders", "EXCLUSIVE"));
        let g = mdl_wait(&row(&[("req_id", "9"), ("blk_id", "8"), ("obj_type", "GLOBAL"), ("lock_mode", "SHARED")])).unwrap();
        assert_eq!(g.on, "global");
    }

    #[test]
    fn a_transaction_row_gives_the_times_and_the_statement() {
        let t = trx_from(&row(&[("id", "41"), ("open_secs", "73"), ("wait_secs", "12"), ("query", "UPDATE orders SET a = 1")])).unwrap();
        assert_eq!((t.id, t.open_secs, t.wait_secs, t.query.as_str()), (41, Some(73.0), Some(12.0), "UPDATE orders SET a = 1"));
        // A transaction that waits for nothing has a NULL wait time.
        let t = trx_from(&row(&[("id", "40"), ("open_secs", "90")])).unwrap();
        assert_eq!((t.wait_secs, t.query.as_str()), (None, ""));
    }

    #[test]
    fn one_edge_per_thread_pair_however_many_locks() {
        let mut m = wait(2, 1);
        m.kind = "Metadata".into();
        let out = dedupe(vec![wait(2, 1), wait(2, 1), wait(3, 1), wait(4, 4), m]);
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn a_chain_queues_waiters_under_the_thread_that_holds_the_lock() {
        // 1 holds, 2 and 3 wait for 1, 4 waits for 3 only, and 3 is itself waiting: 4 goes a level deeper.
        let ch = chains(&[wait(2, 1), wait(3, 1), wait(4, 3), wait(6, 5)]);
        assert_eq!(ch, vec![Chain { head: 1, waiting: vec![(2, 1), (3, 1), (4, 2)] }, Chain { head: 5, waiting: vec![(6, 1)] }]);
        // A waiter blocked by a waiter and a holder sits under the holder.
        assert_eq!(chains(&[wait(2, 3), wait(2, 1), wait(3, 1)]), vec![Chain { head: 1, waiting: vec![(2, 1), (3, 1)] }]);
        assert!(chains(&[]).is_empty());
    }

    #[test]
    fn a_deadlock_cycle_still_gets_a_chain() {
        let ch = chains(&[wait(1, 2), wait(2, 1)]);
        assert_eq!(ch.len(), 1);
        assert_eq!(ch[0].head, 1);
        assert_eq!(ch[0].waiting, vec![(2, 1)]);
    }

    #[test]
    fn refused_reads_say_which_privilege_is_missing() {
        assert!(lock_denied(Some(1227), false).unwrap().contains("PROCESS"));
        assert!(lock_denied(Some(1142), true).unwrap().contains("performance_schema"));
        assert!(!lock_denied(Some(1142), false).unwrap().contains("performance_schema"));
        assert!(lock_denied(Some(1064), true).is_none() && lock_denied(None, true).is_none());
        assert!(table_missing(Some(1146)) && table_missing(Some(1286)) && !table_missing(Some(1227)));
    }

    #[test]
    fn live_a_blocked_update_shows_who_blocks_it() {
        every_server(|l, var| {
            let t = format!("t_locks_{}", var.to_lowercase());
            let waits = l.w.rt.block_on(async {
                let pool = l.pool.clone();
                sqlx::raw_sql(&format!("DROP TABLE IF EXISTS quarry_test.{t}")).execute(&pool).await.unwrap();
                sqlx::raw_sql(&format!("CREATE TABLE quarry_test.{t} (id INT PRIMARY KEY, v INT) ENGINE=InnoDB")).execute(&pool).await.unwrap();
                sqlx::raw_sql(&format!("INSERT INTO quarry_test.{t} VALUES (1, 0)")).execute(&pool).await.unwrap();
                let mut holder = pool.acquire().await.unwrap();
                sqlx::raw_sql("SET SESSION innodb_lock_wait_timeout = 30").execute(&mut *holder).await.unwrap();
                sqlx::raw_sql("BEGIN").execute(&mut *holder).await.unwrap();
                sqlx::raw_sql(&format!("UPDATE quarry_test.{t} SET v = 1 WHERE id = 1")).execute(&mut *holder).await.unwrap();
                let hid: u64 = sqlx::query_scalar("SELECT CONNECTION_ID()").fetch_one(&mut *holder).await.unwrap();
                let mut waiter = pool.acquire().await.unwrap();
                sqlx::raw_sql("SET SESSION innodb_lock_wait_timeout = 30").execute(&mut *waiter).await.unwrap();
                let wid: u64 = sqlx::query_scalar("SELECT CONNECTION_ID()").fetch_one(&mut *waiter).await.unwrap();
                let q = format!("UPDATE quarry_test.{t} SET v = 2 WHERE id = 1");
                let blocked = tokio::spawn(async move {
                    let _ = sqlx::raw_sql(&q).execute(&mut *waiter).await;
                    waiter
                });
                let mut found = None;
                for _ in 0..40 {
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    if let Ok(k) = read(&l.db).await && k.waits.iter().any(|w| w.waiter == wid && w.blocker == hid) {
                        found = Some((k, wid, hid));
                        break;
                    }
                }
                let _ = sqlx::raw_sql("ROLLBACK").execute(&mut *holder).await;
                let _ = blocked.await;
                drop(holder);
                sqlx::raw_sql(&format!("DROP TABLE IF EXISTS quarry_test.{t}")).execute(&pool).await.unwrap();
                found
            });
            let (k, wid, hid) = waits.expect("the wait is listed");
            assert!(k.procs.contains_key(&wid) && k.procs.contains_key(&hid) && !k.source.is_empty());
            assert!(k.waits.iter().any(|w| w.waiter == wid && w.blocker == hid && w.kind == "Row" && w.on.contains(&t)), "{:?}", k.waits);
            assert_eq!(chains(&k.waits).iter().find(|c| c.head == hid).map(|c| c.waiting.iter().any(|(p, _)| *p == wid)), Some(true));
        });
    }
}
