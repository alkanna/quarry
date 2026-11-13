//! The PostgreSQL Server window against PostgreSQL 18 and 13 and the PgBouncer in transaction mode (every reader skips a server
//! whose variable is unset). Sessions to look at, cancel and end are opened by the tests themselves, each tagged with a unique
//! comment so a parallel test's sessions are never touched.

use super::*;
use crate::db::sql::testing::{live_pg, live_pg_at, unique_name, LivePg, PGBOUNCER_ENV, PG_ENVS};
use sqlx::Connection;
use std::sync::mpsc;
use std::time::Duration;

/// Every server of the live suite, with whether it is the PgBouncer.
fn every(f: impl Fn(&LivePg, bool)) {
    for var in PG_ENVS {
        if let Some(l) = live_pg(var) {
            f(&l, false);
        }
    }
    if let Some(l) = live_pg(PGBOUNCER_ENV) {
        assert!(l.db.pooler());
        f(&l, true);
    }
}

/// `uri` pointing at `database`.
fn at(uri: &str, database: &str) -> sqlx::postgres::PgConnectOptions {
    use std::str::FromStr;
    sqlx::postgres::PgConnectOptions::from_str(uri).unwrap().database(database)
}

/// A plain function, because inside a spawned block the executor lifetimes of a borrowed connection do not infer.
async fn exec(c: &mut sqlx::PgConnection, sql: &str) -> Result<(), sqlx::Error> {
    sqlx::Executor::execute(c, sql).await.map(|_| ())
}

/// Runs `sql` on a plain connection of its own and sends back how it ended and whether the connection still answered after.
fn session(l: &LivePg, database: &str, sql: String) -> mpsc::Receiver<(Result<(), String>, bool)> {
    let o = at(&l.uri, database);
    let (tx, rx) = mpsc::channel();
    l.w.rt.spawn(async move {
        let mut c = sqlx::PgConnection::connect_with(&o).await.unwrap();
        let r = exec(&mut c, &sql).await.map_err(se);
        let alive = exec(&mut c, "SELECT 1").await.is_ok();
        let _ = tx.send((r, alive));
    });
    rx
}

fn sleeper(l: &LivePg, tag: &str) -> mpsc::Receiver<(Result<(), String>, bool)> {
    session(l, "postgres", format!("SELECT pg_sleep(30) /* {tag} */"))
}

/// The session whose query carries `tag` once `ok` holds for it.
fn find(l: &LivePg, db: &Db, tag: &str, ok: impl Fn(&Session) -> bool) -> Session {
    for _ in 0..200 {
        let a = l.w.rt.block_on(activity(db, true)).unwrap();
        if let Some(s) = a.rows.into_iter().find(|s| s.query.contains(tag) && ok(s)) {
            return s;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("no session with {tag}");
}

fn running(l: &LivePg, tag: &str) -> Session {
    find(l, &l.db, tag, |s| s.state == "active")
}

/// What the window would send for `s`, with the server of the list it read.
fn tgt(l: &LivePg, s: &Session) -> Target {
    let server = l.w.rt.block_on(activity(&l.db, false)).unwrap().server;
    Target::of(s, &server)
}

#[test]
fn live_every_read_answers() {
    every(|l, pooled| {
        let o = l.w.rt.block_on(overview(&l.db)).unwrap();
        assert!(o.version.starts_with("PostgreSQL") && !o.short_version.is_empty(), "{o:?}");
        assert!(o.uptime.is_some() && o.max_connections.is_some_and(|m| m > 0.0) && o.connections.is_some_and(|c| c >= 1.0));
        assert!(o.commits.is_some() && o.hits.is_some() && o.shared_buffers.is_some_and(|b| b > 0.0) && o.started.ends_with("UTC"));
        assert!(o.read_all && o.user == "postgres" && !o.data_dir.is_empty() && !o.encoding.is_empty());
        assert!(matches!(o.role, Some(Role::Primary { ref standbys, visible: true }) if standbys.is_empty()), "{:?}", o.role);
        let sz = l.w.rt.block_on(sizes(&l.db)).unwrap();
        assert!(sz.iter().any(|(n, b)| n == "postgres" && b.is_some_and(|b| b > 0)), "{sz:?}");
        // The second sample gives a rate.
        std::thread::sleep(Duration::from_millis(300));
        let o2 = l.w.rt.block_on(overview(&l.db)).unwrap();
        let m = metrics(&o2, Some(&(0.3, o)));
        assert!(m.tps.is_some_and(|t| t >= 0.0) && m.commit.is_some() && m.hit.is_some_and(|h| h > 0.0 && h <= 1.0), "{m:?}");
        let st = l.w.rt.block_on(settings(&l.db)).unwrap();
        assert!(st.rows.len() > 200 && st.user == "postgres", "{}", st.rows.len());
        let max = st.rows.iter().find(|r| r.name == "max_connections").unwrap();
        assert!(max.value.parse::<u32>().is_ok() && max.context == "postmaster" && !max.desc.is_empty() && !max.boot.is_empty());
        let sb = st.rows.iter().find(|r| r.name == "shared_buffers").unwrap();
        assert!(sb.value.ends_with("MB") || sb.value.ends_with("GB") || sb.value.ends_with("kB"), "{sb:?}");
        // Quarry's own: the read-only guard is a startup option on a direct connection, and nothing is set on the session behind a pooler.
        let ro = st.rows.iter().find(|r| r.name == "default_transaction_read_only").unwrap();
        assert_eq!(ro.source == "client", !pooled, "{ro:?}");
        let q = set_by_quarry(&Advanced::default());
        assert!(q.contains(&"statement_timeout".to_string()) && q.contains(&"application_name".to_string()));
        // pg_file_settings is asked only when a setting waits for a restart; check that its statement reads as the superuser.
        let pending = l.w.rt.block_on(read(&l.db, async |c| Ok((rows(c, FILE_READABLE).await?, rows(c, FILE_PENDING).await?)))).unwrap();
        assert_eq!(text_at(&pending.0[0], 0).as_deref(), Some("1"));
        let a = l.w.rt.block_on(activity(&l.db, true)).unwrap();
        assert!(a.read_all && a.user == "postgres");
        // The reading connection is in the list and marked as Quarry's.
        // Active only: an idle pooled session keeps the text of its last statement, and a parallel test's poll shows up the same way.
        let me = a.rows.iter().find(|r| r.state == "active" && r.query.contains("pg_blocking_pids")).expect("the reader");
        assert!(me.ours && me.state == "active" && !me.backend_start.is_empty() && !a.server.is_empty(), "{me:?}");
        assert_eq!(a.own_app.is_some(), pooled);
        // A poll without blockers does not call pg_blocking_pids. The list is the whole server's, where parallel tests poll with blockers
        // on, so what is checked is the statement this poll sends, not the rows it reads back.
        let b = l.w.rt.block_on(activity(&l.db, false)).unwrap();
        assert!(!b.blockers && !activity_sql(false).contains("pg_blocking_pids") && activity_sql(true).contains("pg_blocking_pids"));
    });
}

#[test]
fn live_a_long_query_is_cut_and_copy_reads_the_whole() {
    every(|l, _| {
        let tag = unique_name("q_srv_long");
        // The tag goes first: the server keeps only track_activity_query_size bytes of the text.
        let pad = "x".repeat(QUERY_CUT + 500);
        let rx = session(l, "postgres", format!("SELECT /* {tag} */ pg_sleep(30), '{pad}'"));
        let s = running(l, &tag);
        assert!(s.query.chars().count() <= QUERY_CUT, "{}", s.query.len());
        // The window cuts only what the server kept past QUERY_CUT, which needs a raised track_activity_query_size (1 kB by default).
        assert!(!s.cut || s.query.chars().count() == QUERY_CUT);
        let full = l.w.rt.block_on(full_query(&l.db, s.pid, s.backend_start.clone(), s.query.clone())).unwrap();
        assert!(full.starts_with(&s.query) && full.len() >= s.query.len());
        // Another statement than the one shown copies nothing.
        let other = l.w.rt.block_on(full_query(&l.db, s.pid, s.backend_start.clone(), "SELECT other".into())).unwrap_err();
        assert!(other.contains("another statement"), "{other}");
        l.w.rt.block_on(signal(&l.db, &tgt(l, &s), false, false)).unwrap();
        let _ = rx.recv_timeout(Duration::from_secs(10));
    });
}

#[test]
fn live_cancel_stops_the_statement_and_keeps_the_session() {
    every(|l, _| {
        let tag = unique_name("q_srv_cancel");
        let rx = sleeper(l, &tag);
        let s = running(l, &tag);
        assert!(s.signal && s.signalable() && s.active() && !s.ours);
        let r = l.w.rt.block_on(signal(&l.db, &tgt(l, &s), false, false)).unwrap();
        assert!(r.contains("Cancelled"), "{r}");
        let (out, alive) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let err = out.unwrap_err();
        assert!(err.contains("57014"), "{err}");
        assert!(alive, "the session answers after a cancel");
    });
}

#[test]
fn live_terminate_ends_the_session() {
    every(|l, pooled| {
        let tag = unique_name("q_srv_term");
        let rx = sleeper(l, &tag);
        let s = running(l, &tag);
        let r = l.w.rt.block_on(signal(&l.db, &tgt(l, &s), true, false)).unwrap();
        assert!(r.contains("Ended session"), "{r}");
        let (out, alive) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(out.is_err());
        // PgBouncer opens a new server connection for the client's next transaction, so only a direct session stays dead.
        if !pooled {
            assert!(!alive, "the session is gone after a terminate");
        }
        std::thread::sleep(Duration::from_millis(300));
        let a = l.w.rt.block_on(activity(&l.db, true)).unwrap();
        assert!(!a.rows.iter().any(|r| r.pid == s.pid && r.backend_start == s.backend_start), "the backend is gone");
        // A second try finds the session gone and sends nothing.
        let again = l.w.rt.block_on(signal(&l.db, &tgt(l, &s), true, false)).unwrap_err();
        assert!(again.contains("already gone") || again.contains("another session"), "{again}");
    });
}

#[test]
fn live_a_locked_connection_a_reused_pid_and_a_changed_statement_send_nothing() {
    every(|l, _| {
        let tag = unique_name("q_srv_refuse");
        let rx = sleeper(l, &tag);
        let s = running(l, &tag);
        let t = tgt(l, &s);
        for terminate in [false, true] {
            let err = l.w.rt.block_on(signal(&l.db, &t, terminate, true)).unwrap_err();
            assert!(err.contains("locked"), "{err}");
        }
        // The pid now belongs to a backend that started at another time: the server reused it.
        let reused = Target { backend_start: "2001-01-01 00:00:00+00".into(), ..t.clone() };
        for terminate in [false, true] {
            let err = l.w.rt.block_on(signal(&l.db, &reused, terminate, false)).unwrap_err();
            assert!(err.contains("reused"), "{err}");
        }
        let other_user = Target { user: "someone".into(), ..t.clone() };
        assert!(l.w.rt.block_on(signal(&l.db, &other_user, true, false)).unwrap_err().contains("reused"));
        // A cancel is for the statement the list showed.
        let later = Target { query_start: "2001-01-01 00:00:00+00".into(), ..t.clone() };
        assert!(l.w.rt.block_on(signal(&l.db, &later, false, false)).unwrap_err().contains("another statement"));
        // A row the role could not see cannot be checked.
        let blind = Target { backend_start: String::new(), ..t.clone() };
        assert!(l.w.rt.block_on(signal(&l.db, &blind, true, false)).unwrap_err().contains("pg_read_all_stats"));
        assert!(rx.recv_timeout(Duration::from_millis(500)).is_err(), "the statement still runs");
        l.w.rt.block_on(signal(&l.db, &t, false, false)).unwrap();
        let _ = rx.recv_timeout(Duration::from_secs(10));
    });
}

#[test]
fn live_quarrys_own_sessions_are_refused() {
    for var in PG_ENVS {
        let Some(l) = live_pg(var) else { continue };
        // A pooled connection of Quarry's: the one that read the list.
        let a = l.w.rt.block_on(activity(&l.db, true)).unwrap();
        let me = a.rows.iter().find(|r| r.ours && r.state == "active" && r.query.contains("pg_blocking_pids")).expect("the reader");
        // The list also holds the sessions of parallel tests in this process, which may close before the signal is checked, so a
        // session that is already gone proves nothing and the next one is tried. A reader that is still there is always refused.
        let mut refused = 0;
        for r in a.rows.iter().filter(|r| r.ours && r.state == "active" && r.query.contains("pg_blocking_pids")) {
            for terminate in [false, true] {
                let err = l.w.rt.block_on(signal(&l.db, &tgt(&l, r), terminate, false)).unwrap_err();
                assert!(err.contains("Quarry's own") || err.contains("already gone"), "{err}");
                refused += err.contains("Quarry's own") as u32;
            }
        }
        assert!(refused > 0, "no live reader was refused");
        // A connection of Quarry's own outside the pools (Stop and the script runs open these) is marked too.
        let c = l.w.rt.block_on(l.db.detached()).unwrap();
        let a = l.w.rt.block_on(activity(&l.db, true)).unwrap();
        let ours: Vec<&Session> = a.rows.iter().filter(|r| r.ours).collect();
        assert!(ours.len() >= 2 && ours.iter().all(|r| r.pid == me.pid || is_own(&a.server, r.pid, &r.backend_start)), "{ours:?}");
        // Another saved connection to the same server, here under another login, sees the first one's sessions as Quarry's.
        let role = unique_name("q_srv_other");
        l.w.rt.block_on(sqlx::raw_sql(&format!("CREATE ROLE {role} LOGIN PASSWORD 'other-pw'; GRANT pg_read_all_stats TO {role}")).execute(&l.pool)).unwrap();
        let other = live_pg_at(l.uri.replacen("postgres:quarry@", &format!("{role}:other-pw@"), 1));
        let seen = l.w.rt.block_on(activity(&other.db, true)).unwrap();
        assert_eq!(seen.server, a.server);
        // A pooled backend may have closed in between, but the detached one is held, so at least one is still there.
        let still: Vec<&Session> = seen.rows.iter().filter(|r| ours.iter().any(|o| o.pid == r.pid && o.backend_start == r.backend_start)).collect();
        assert!(!still.is_empty() && still.iter().all(|r| r.ours), "{still:?}");
        let reader = still[0];
        let err = l.w.rt.block_on(signal(&other.db, &Target::of(reader, &seen.server), true, false)).unwrap_err();
        assert!(err.contains("Quarry's own"), "{err}");
        drop(c);
        drop(other);
        std::thread::sleep(Duration::from_millis(200));
        let _ = l.w.rt.block_on(sqlx::raw_sql(&format!("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE usename = '{role}'; DROP ROLE {role}")).execute(&l.pool));
    }
}

#[test]
fn live_behind_a_pooler_sessions_named_like_quarry_are_its_own() {
    every(|l, pooled| {
        let tag = unique_name("q_srv_named");
        let o = at(&l.uri, "postgres").application_name("Quarry");
        let (tx, rx) = mpsc::channel();
        let sql = format!("SELECT pg_sleep(30) /* {tag} */");
        l.w.rt.spawn(async move {
            let mut c = sqlx::PgConnection::connect_with(&o).await.unwrap();
            let r = exec(&mut c, &sql).await.map_err(se);
            let _ = tx.send(r);
        });
        let s = running(l, &tag);
        // A direct connection knows its own backends exactly, so another client's "Quarry" session is foreign there.
        assert_eq!(s.ours, pooled, "{s:?}");
        let r = l.w.rt.block_on(signal(&l.db, &tgt(l, &s), false, false));
        if pooled {
            assert!(r.unwrap_err().contains("Quarry's own"));
            let _ = l.w.rt.block_on(sqlx::raw_sql(&format!("SELECT pg_cancel_backend(pid) FROM pg_stat_activity WHERE query LIKE '%{tag}%' AND pid <> pg_backend_pid()")).execute(&l.pool));
        } else {
            r.unwrap();
        }
        let _ = rx.recv_timeout(Duration::from_secs(10));
    });
}

#[test]
fn live_the_signal_runs_in_a_read_only_transaction() {
    // A hot standby refuses BEGIN READ WRITE (25006), which the test servers cannot be put in; the check and the signal must
    // then work inside the READ ONLY transaction Quarry opens for them, on a held connection.
    every(|l, _| {
        let tag = unique_name("q_srv_ro");
        let rx = sleeper(l, &tag);
        let s = running(l, &tag);
        let t = tgt(l, &s);
        let begin = signal_begin(&l.db);
        assert!(begin.starts_with("BEGIN READ ONLY"), "{begin}");
        let sql = signal_sql(false, true);
        let params = [Param::Int(t.pid as i64), Param::Text(t.backend_start.clone()), Param::Text(t.user.clone()), Param::Text(t.query_start.clone())];
        let rows = l.w.rt.block_on(async {
            let Conn::Pg(mut c) = l.db.acquire_conn().await.unwrap() else { panic!() };
            // The transaction is read-only for real: a write in it is refused.
            let mut probe = c.begin_with(begin.clone()).await.unwrap();
            let w = sqlx::Executor::execute(&mut *probe, "CREATE TEMP TABLE q_srv_ro_probe (a int)").await.unwrap_err();
            assert_eq!(crate::db::sql::exec::sql_state(&w).as_deref(), Some("25006"), "{w}");
            probe.rollback().await.unwrap();
            let rows = send_signal(&mut c, begin, &sql, &params).await.unwrap();
            let _ = c.close().await;
            rows
        });
        assert_eq!(text_at(&rows[0], 4).as_deref(), Some("true"));
        let (out, _) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(out.unwrap_err().contains("57014"));
    });
}

#[test]
fn the_signal_opens_a_read_only_transaction_direct_and_behind_a_pooler() {
    use crate::db::sql::pg::advanced::PgSession;
    for pooled in [false, true] {
        let db = Db::offline_pg(pooled, PgSession::default());
        let b = signal_begin(&db);
        assert!(b.starts_with("BEGIN READ ONLY") && !b.contains("READ WRITE"), "{b}");
    }
}

#[test]
fn the_own_registry_keeps_a_backend_noted_during_a_read() {
    use crate::db::sql::pg::note_own;
    let server = unique_name("srv");
    let other = unique_name("srv");
    note_own(&server, 10, "2026-11-12 10:00:00.000001");
    note_own(&server, 11, "2026-11-12 10:00:00.000002");
    note_own(&other, 10, "2026-11-12 10:00:00.000001");
    let mark = own_mark();
    // Opened after the snapshot began: the list below cannot show it yet.
    note_own(&server, 12, "2026-11-12 10:00:00.000003");
    let s = |pid: u64, start: &str| Session { pid, backend_start: start.into(), ..Default::default() };
    // 10 closed, 11 is listed under a role that may not read its start.
    let listed = vec![s(11, ""), s(99, "x")];
    keep_own(&server, mark, |pid, start| gone(&listed, pid, start));
    assert!(!is_own(&server, 10, "2026-11-12 10:00:00.000001"));
    assert!(is_own(&server, 11, "2026-11-12 10:00:00.000002"));
    assert!(is_own(&server, 12, "2026-11-12 10:00:00.000003"));
    // Another server's entries are left alone, and a pid that now has another start is gone.
    assert!(is_own(&other, 10, "2026-11-12 10:00:00.000001"));
    keep_own(&server, own_mark(), |pid, start| gone(&[s(11, "2026-11-12 11:00:00.000000"), s(12, "2026-11-12 10:00:00.000003")], pid, start));
    assert!(!is_own(&server, 11, "2026-11-12 10:00:00.000002") && is_own(&server, 12, "2026-11-12 10:00:00.000003"));
    assert!(!is_own(&server, 12, ""));
}

#[test]
fn a_poll_without_blockers_keeps_the_last_ones_for_sessions_that_still_wait() {
    let s = |pid: u64, start: &str, lock: &str, blockers: &[u64]| Session { pid, backend_start: start.into(), lock_mode: lock.into(), blockers: blockers.to_vec(), ..Default::default() };
    let old = vec![s(1, "a", "", &[]), s(2, "b", "ShareLock", &[1, 7]), s(3, "c", "ShareLock", &[1]), s(4, "d", "ShareLock", &[1])];
    // 2 still waits (7 has gone), 3 got its lock, 4 is a new backend on the same pid, 5 started waiting since.
    let mut rows = vec![s(1, "a", "", &[]), s(2, "b", "ShareLock", &[]), s(3, "c", "", &[]), s(4, "e", "ShareLock", &[]), s(5, "f", "ShareLock", &[])];
    carry_blockers(&mut rows, &old);
    let got: Vec<Vec<u64>> = rows.iter().map(|r| r.blockers.clone()).collect();
    assert_eq!(got, [vec![], vec![1], vec![], vec![], vec![]]);
}

#[test]
fn the_query_log_says_what_a_signal_did() {
    assert_eq!(signal_log(true, 41877, &Ok("Ended session 41877".into())), ("SELECT pg_terminate_backend(41877) -- Ended session 41877".to_string(), None));
    let (line, err) = signal_log(false, 41877, &Err("Session 41877 is running another statement now, so nothing was sent. Refresh and try again.".into()));
    assert_eq!(line, "SELECT pg_cancel_backend(41877)");
    assert!(err.unwrap().contains("nothing was sent"));
    // The guarded statement with its placeholders is not what the log shows.
    assert!(!line.contains('$'));
}

#[test]
fn live_locks_show_the_chain_and_terminate_frees_it() {
    every(|l, _| {
        let schema = unique_name("q_srv_lock");
        let (db, _pool) = l.fresh_schema(&schema, "CREATE TABLE t (id int PRIMARY KEY, v int); INSERT INTO t VALUES (1, 1)");
        let tag = unique_name("lk");
        // A holds a row lock and sits idle in its transaction; B waits for the row, C for the whole table, and D queues behind C.
        let hold = format!("BEGIN; UPDATE {schema}.t SET v = 2 WHERE id = 1 /* {tag} a */");
        let a_rx = session(l, "quarry_test", format!("{hold}; SELECT pg_sleep(30)"));
        let a = find(l, &db, &format!("{tag} a"), |s| s.state == "active");
        let b_rx = session(l, "quarry_test", format!("UPDATE {schema}.t SET v = 3 WHERE id = 1 /* {tag} b */"));
        let b = find(l, &db, &format!("{tag} b"), |s| !s.blockers.is_empty());
        let c_rx = session(l, "quarry_test", format!("ALTER TABLE {schema}.t ADD COLUMN note text /* {tag} c */"));
        let c = find(l, &db, &format!("{tag} c"), |s| !s.blockers.is_empty());
        let d_rx = session(l, "quarry_test", format!("SELECT count(*) FROM {schema}.t /* {tag} d */"));
        let d = find(l, &db, &format!("{tag} d"), |s| !s.blockers.is_empty());
        assert_eq!(b.blockers, [a.pid]);
        assert_eq!(b.lock_mode, "ShareLock");
        assert!(b.lock_on.starts_with("transaction "), "{b:?}");
        assert_eq!(c.lock_mode, "AccessExclusiveLock");
        assert_eq!(c.lock_on, format!("{schema}.t"));
        assert!(d.blockers.contains(&c.pid) && d.lock_mode == "AccessShareLock", "{d:?}");
        let rows = l.w.rt.block_on(activity(&db, true)).unwrap().rows;
        let chain = chains(&rows).into_iter().find(|ch| ch.head == a.pid).expect("a's chain");
        // C waits for A and for B, which both hold a lock on the table, so it leads under A; D only queues behind C.
        assert!(chain.waiting.contains(&(b.pid, 1)) && chain.waiting.contains(&(c.pid, 1)) && chain.waiting.contains(&(d.pid, 2)), "{chain:?}");
        // Ending the head rolls its transaction back and lets every waiter through.
        let head = rows.iter().find(|r| r.pid == a.pid).unwrap();
        l.w.rt.block_on(signal(&l.db, &tgt(l, head), true, false)).unwrap();
        assert!(a_rx.recv_timeout(Duration::from_secs(10)).unwrap().0.is_err());
        for rx in [b_rx, c_rx, d_rx] {
            assert!(rx.recv_timeout(Duration::from_secs(10)).unwrap().0.is_ok());
        }
        let _ = l.w.rt.block_on(sqlx::raw_sql(&format!("DROP SCHEMA {schema} CASCADE")).execute(&_pool));
    });
}

#[test]
fn live_a_role_without_pg_read_all_stats_sees_others_as_insufficient_privilege() {
    for var in PG_ENVS {
        let Some(l) = live_pg(var) else { continue };
        let tag = unique_name("q_srv_priv");
        let role = unique_name("q_srv_low");
        l.w.rt.block_on(sqlx::raw_sql(&format!("CREATE ROLE {role} LOGIN PASSWORD 'low-pw'")).execute(&l.pool)).unwrap();
        let uri = l.uri.replacen("postgres:quarry@", &format!("{role}:low-pw@"), 1);
        let low = live_pg_at(uri);
        let rx = sleeper(&l, &tag);
        let theirs = running(&l, &tag);
        let a = l.w.rt.block_on(activity(&low.db, true)).unwrap();
        assert!(!a.read_all && a.user == role);
        let seen = a.rows.iter().find(|r| r.pid == theirs.pid).expect("the superuser's session is listed");
        assert!(seen.hidden && seen.line == "insufficient privilege" && seen.backend_start.is_empty() && seen.state.is_empty() && !seen.signal, "{seen:?}");
        assert_eq!((seen.user.as_str(), seen.db.as_str()), ("postgres", "postgres"));
        // Its own session is fully visible.
        assert!(a.rows.iter().any(|r| r.user == role && !r.hidden && !r.backend_start.is_empty()));
        let o = l.w.rt.block_on(overview(&low.db)).unwrap();
        assert!(!o.read_all && o.data_dir.is_empty());
        // Nothing is sent for a row the role could not check, even with what another role saw of it.
        for t in [tgt(&low, seen), tgt(&l, &theirs)] {
            let err = l.w.rt.block_on(signal(&low.db, &t, false, false)).unwrap_err();
            assert!(err.contains("pg_read_all_stats"), "{err}");
        }
        // With pg_read_all_stats it sees the row, and the server refuses to signal a superuser's session.
        l.w.rt.block_on(sqlx::raw_sql(&format!("GRANT pg_read_all_stats TO {role}")).execute(&l.pool)).unwrap();
        let a = l.w.rt.block_on(activity(&low.db, true)).unwrap();
        let seen = a.rows.iter().find(|r| r.pid == theirs.pid).unwrap();
        assert!(a.read_all && !seen.hidden && !seen.signal, "{seen:?}");
        let err = l.w.rt.block_on(signal(&low.db, &tgt(&low, seen), false, false)).unwrap_err();
        assert!(err.contains("Your role cannot"), "{err}");
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "the statement still runs");
        l.w.rt.block_on(signal(&l.db, &tgt(&l, &theirs), false, false)).unwrap();
        let _ = rx.recv_timeout(Duration::from_secs(10));
        drop(low);
        std::thread::sleep(Duration::from_millis(200));
        let _ = l.w.rt.block_on(sqlx::raw_sql(&format!("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE usename = '{role}'; DROP ROLE {role}")).execute(&l.pool));
    }
}

#[test]
fn chains_nest_a_waiter_under_the_waiter_it_queues_behind() {
    let s = |pid: u64, blockers: &[u64]| Session { pid, blockers: blockers.to_vec(), ..Default::default() };
    let rows = vec![s(1, &[]), s(2, &[1]), s(3, &[2, 1]), s(4, &[3]), s(10, &[]), s(11, &[10]), s(20, &[21]), s(21, &[20])];
    let c = chains(&rows);
    // 3 is held up by 1 itself, so it is not nested under 2 even though 2 is listed first; 4 only queues behind 3.
    assert_eq!(c[0], Chain { head: 1, waiting: vec![(2, 1), (3, 1), (4, 2)] });
    assert_eq!(c[1], Chain { head: 10, waiting: vec![(11, 1)] });
    // A cycle with no head still shows, led by its lowest pid.
    assert_eq!(c[2], Chain { head: 20, waiting: vec![(21, 1)] });
    assert_eq!(c.len(), 3);
}

#[test]
fn metrics_need_two_samples_for_a_rate() {
    let o = |c: f64, r: f64| Overview { commits: Some(c), rollbacks: Some(r), hits: Some(990.0), reads: Some(10.0), ..Default::default() };
    let m = metrics(&o(1200.0, 10.0), Some(&(2.0, o(1000.0, 10.0))));
    assert_eq!((m.tps, m.commit), (Some(100.0), Some(1.0)));
    assert_eq!(m.hit, Some(0.99));
    let first = metrics(&o(990.0, 10.0), None);
    assert_eq!((first.tps, first.commit), (None, Some(0.99)));
    // A statistics reset makes the counters go down: no rate across it.
    assert_eq!(metrics(&o(5.0, 0.0), Some(&(2.0, o(1000.0, 10.0)))).tps, None);
}

#[test]
fn the_signal_check_words_every_outcome() {
    let t = Target { pid: 41877, backend_start: "s".into(), user: "app".into(), query_start: "q".into(), app: "psql".into(), server: String::new() };
    assert!(signal_outcome(&t, true, false, None).unwrap_err().contains("already gone"));
    assert!(signal_outcome(&t, true, false, Some(("s", "app", "q", true, None))).unwrap_err().contains("Quarry sends this on"));
    assert!(signal_outcome(&t, true, false, Some(("s2", "app", "q", false, None))).unwrap_err().contains("reused"));
    assert!(signal_outcome(&t, false, true, Some(("s", "app", "q2", false, None))).unwrap_err().contains("another statement"));
    // A terminate on a direct connection does not care which statement runs.
    assert_eq!(signal_outcome(&t, true, false, Some(("s", "app", "q2", false, Some("true")))).unwrap(), "Ended session 41877");
    assert!(signal_outcome(&t, false, true, Some(("s", "app", "q", false, Some("false")))).unwrap_err().contains("could not signal"));
    assert!(signal_sql(false, true).contains("pg_cancel_backend") && signal_sql(false, true).contains("$4"));
    assert!(signal_sql(true, false).contains("pg_terminate_backend") && !signal_sql(true, false).contains("$4"));
}
