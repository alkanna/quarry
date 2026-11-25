//! Live tests of the explain dialog's index suggestions: a plan read through the worker as the dialog reads it, the
//! catalog's evidence for its scans, and the findings worked out with both. Each test makes its own schema or database
//! with tables of its own, and runs the explain the way a locked connection does unless ANALYZE needs more.

use crate::db::sql::plan::{self, FixKind, Plan};
use crate::db::sql::testing::{every_pg, every_server, schema_ident, unique_name, MyFixture, PgFixture};
use crate::db::sql::{Db, Dialect};
use crate::db::{Msg, Worker};
use std::sync::mpsc::Receiver;
use std::time::Duration;

/// The plan of `stmt` run in `schema`, with the findings the dialog shows once the catalog answered.
fn explained(w: &Worker, rx: &Receiver<Msg>, db: &Db, schema: &str, stmt: &str, analyze: bool, read_only: bool) -> Plan {
    let d = db.dialect;
    let script = plan::explain_script(d, stmt, analyze.then_some(plan::Analyze::Direct));
    w.sql_explain(1, db.clone(), schema.into(), script, read_only, Default::default());
    let Ok(Msg::SqlExplained { sets, .. }) = rx.recv_timeout(Duration::from_secs(60)) else { panic!("no plan for {stmt}") };
    let text = plan::plan_text(&sets).unwrap_or_else(|e| panic!("{stmt}: {e}"));
    let mut p = plan::parse(d, &text).unwrap_or_else(|e| panic!("{stmt}: {e}\n{text}"));
    w.sql_explain_evidence(2, db.clone(), schema.into(), plan::evidence::wants(&p, stmt, d));
    let Ok(Msg::SqlExplainEvidence { ev, .. }) = rx.recv_timeout(Duration::from_secs(30)) else { panic!("no evidence for {stmt}") };
    assert!(!ev.failed, "{stmt}: the catalog could not be read");
    p.refind(d, Some(&ev), db.pooler());
    p
}

/// The SQL of the candidate index, when the plan has one.
fn candidate(p: &Plan) -> Option<String> {
    p.findings.iter().find_map(|f| f.fix.as_ref().filter(|x| x.kind == FixKind::Index).map(|x| x.sql.clone()))
}

/// Every finding with the reason it gave for no index, for the assertion messages.
fn reasons(p: &Plan) -> Vec<String> {
    p.findings.iter().map(|f| format!("{}: {}", f.title, f.no_fix.clone().unwrap_or_default())).collect()
}

/// ANALYZE until the statistics count no write since it. Write counts reach the statistics a little after the statement
/// that made them, so an ANALYZE run at once can see the INSERT before it land afterwards and look stale.
fn settle_pg(fx: &PgFixture, table: &str) {
    for _ in 0..20 {
        fx.sql(&format!("ANALYZE {table}"));
        std::thread::sleep(Duration::from_millis(600));
        if fx.one(&format!("SELECT pg_stat_get_mod_since_analyze('{table}'::regclass)::text")) == "0" {
            return;
        }
    }
    panic!("{table} still counts writes after ANALYZE");
}

#[test]
fn live_postgres_suggests_an_index_for_a_big_filtered_scan_and_none_once_it_exists() {
    every_pg(|l, var| {
        let s = unique_name("qexp");
        let fx = PgFixture::new(
            l,
            &s,
            "CREATE TABLE big (id int PRIMARY KEY, status text NOT NULL, kind int NOT NULL, note text); \
             INSERT INTO big SELECT g, 's' || (g % 50), g % 7, repeat('x', 40) FROM generate_series(1, 50000) g; \
             CREATE TABLE small (id int PRIMARY KEY, status text NOT NULL); \
             INSERT INTO small SELECT g, 's' || (g % 50) FROM generate_series(1, 100) g;",
        );
        settle_pg(&fx, "big");
        settle_pg(&fx, "small");
        let stmt = "SELECT * FROM big WHERE status = 's7'";
        // A locked connection: the plan and the catalog are both read-only reads.
        let p = explained(&l.w, &l.rx, &fx.db, &s, stmt, false, true);
        let sql = candidate(&p).unwrap_or_else(|| panic!("{var}: no candidate: {:?}", reasons(&p)));
        assert_eq!(sql, format!("CREATE INDEX CONCURRENTLY \"big_status_idx\" ON \"{s}\".\"big\" (\"status\");"), "{var}");
        let scan = p.findings.iter().find(|f| f.fix.is_some()).unwrap();
        assert!(scan.evidence.iter().any(|l| l.contains("about 50,000 rows")), "{var}: {:?}", scan.evidence);
        // Once it exists the planner uses it, and nothing is suggested.
        fx.sql("CREATE INDEX big_status_idx ON big (status)");
        settle_pg(&fx, "big");
        let p = explained(&l.w, &l.rx, &fx.db, &s, stmt, false, true);
        assert_eq!(candidate(&p), None, "{var}");
        // A table of 100 rows is read whole.
        let p = explained(&l.w, &l.rx, &fx.db, &s, "SELECT * FROM small WHERE status = 's7'", false, true);
        assert_eq!(candidate(&p), None, "{var}");
    });
}

#[test]
fn live_postgres_reads_the_table_the_statement_means_beside_a_public_one_of_the_same_name() {
    every_pg(|l, var| {
        let s = unique_name("qexp");
        // A mixed-case name, which only resolves when it is quoted.
        let t = format!("Twin_{}", unique_name("t"));
        let qt = schema_ident(&t);
        // Wide enough rows that the scan costs more than a full scan finding needs, whatever the estimate of the filter.
        let fx = PgFixture::new(l, &s, &format!("CREATE TABLE {qt} (id int PRIMARY KEY, status text NOT NULL, note text); INSERT INTO {qt} SELECT g, 's' || (g % 50), repeat('x', 40) FROM generate_series(1, 50000) g;"));
        let public = format!("public.{qt}");
        l.qsql(&fx.pool, &format!("DROP TABLE IF EXISTS {public}; CREATE TABLE {public} (id int PRIMARY KEY, status text NOT NULL); INSERT INTO {public} SELECT g, 's' || (g % 50) FROM generate_series(1, 100) g;"));
        settle_pg(&fx, &format!("{}.{qt}", schema_ident(&s)));
        settle_pg(&fx, &public);
        let checks = || {
            // Unqualified under the fixture's schema: its search_path finds the big one.
            let p = explained(&l.w, &l.rx, &fx.db, &s, &format!("SELECT * FROM {qt} WHERE status = 's7'"), false, true);
            let sql = candidate(&p).unwrap_or_else(|| panic!("{var}: no candidate: {:?}", reasons(&p)));
            assert!(sql.contains(&format!("ON \"{s}\".\"{t}\"")), "{var}: {sql}");
            // Qualified while the run is in public: the plan drops the schema, the statement keeps it.
            let p = explained(&l.w, &l.rx, &fx.db, "public", &format!("SELECT * FROM {}.{qt} WHERE status = 's7'", schema_ident(&s)), false, true);
            assert!(candidate(&p).is_some_and(|sql| sql.contains(&format!("ON \"{s}\".\"{t}\""))), "{var}: {:?}", reasons(&p));
            // The public one, named so from the fixture's schema, is the small table: nothing to suggest.
            let p = explained(&l.w, &l.rx, &fx.db, &s, &format!("SELECT * FROM {public} WHERE status = 's7'"), false, true);
            assert_eq!(candidate(&p), None, "{var}");
        };
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(checks));
        l.qsql(&fx.pool, &format!("DROP TABLE IF EXISTS {public}"));
        if let Err(e) = res {
            std::panic::resume_unwind(e);
        }
    });
}

#[test]
fn live_mysql_and_mariadb_suggest_an_index_once_analyze_measured_the_filter() {
    every_server(|l, var| {
        let s = unique_name("qexp");
        let _fx = MyFixture::new(
            l,
            &s,
            &format!(
                "CREATE TABLE `{s}`.digits (n int); INSERT INTO `{s}`.digits VALUES (0),(1),(2),(3),(4),(5),(6),(7),(8),(9); \
                 CREATE TABLE `{s}`.big (id int PRIMARY KEY, status varchar(16) NOT NULL, kind int NOT NULL, note varchar(60)); \
                 INSERT INTO `{s}`.big SELECT 1 + a.n + 10 * b.n + 100 * c.n + 1000 * d.n + 10000 * e.n, CONCAT('s', (a.n + 10 * b.n) % 50), a.n % 7, REPEAT('x', 40) \
                   FROM `{s}`.digits a, `{s}`.digits b, `{s}`.digits c, `{s}`.digits d, `{s}`.digits e WHERE e.n < 5; \
                 CREATE TABLE `{s}`.small (id int PRIMARY KEY, status varchar(16) NOT NULL); \
                 INSERT INTO `{s}`.small SELECT 1 + a.n + 10 * b.n, CONCAT('s', a.n) FROM `{s}`.digits a, `{s}`.digits b; \
                 ANALYZE TABLE `{s}`.big, `{s}`.small"
            ),
        );
        let d = l.db.dialect;
        let stmt = "SELECT * FROM big WHERE status = 's7'";
        // Locked, without ANALYZE: the server's filtered share is a guess, so no index, and the reason says how to measure.
        let p = explained(&l.w, &l.rx, &l.db, &s, stmt, false, true);
        assert_eq!(candidate(&p), None, "{var}");
        assert!(reasons(&p).iter().any(|r| r.contains("Run with ANALYZE")), "{var}: {:?}", reasons(&p));
        // MariaDB's guard refuses its ANALYZE statement on a locked connection, as the dialog does.
        let ro = d == Dialect::MySql;
        let p = explained(&l.w, &l.rx, &l.db, &s, stmt, true, ro);
        let sql = candidate(&p).unwrap_or_else(|| panic!("{var}: no candidate: {:?}", reasons(&p)));
        assert_eq!(sql, format!("ALTER TABLE `{s}`.`big` ADD INDEX `big_status_idx` (`status`), ALGORITHM=INPLACE, LOCK=NONE;"), "{var}");
        l.sql(&format!("CREATE INDEX big_status_idx ON `{s}`.big (status); ANALYZE TABLE `{s}`.big"));
        let p = explained(&l.w, &l.rx, &l.db, &s, stmt, true, ro);
        assert_eq!(candidate(&p), None, "{var}");
        let p = explained(&l.w, &l.rx, &l.db, &s, "SELECT * FROM small WHERE status = 's7'", true, ro);
        assert_eq!(candidate(&p), None, "{var}");
    });
}

#[test]
fn live_hypopg_trial_uses_the_hypothetical_index_and_leaves_nothing_behind() {
    use crate::db::sql::pg::hypo;
    let Some(l) = crate::db::sql::testing::live_pg("QUARRY_TEST_POSTGRES_HYPOPG") else { return };
    if l.one("SELECT count(*)::text FROM pg_available_extensions WHERE name = 'hypopg'") == "0" {
        return;
    }
    let s = unique_name("qhypo");
    let fx = PgFixture::new(
        &l,
        &s,
        "CREATE TABLE big (id int PRIMARY KEY, status text NOT NULL, kind int NOT NULL, note text); \
         INSERT INTO big SELECT g, 's' || (g % 50), g % 7, repeat('x', 40) FROM generate_series(1, 50000) g;",
    );
    // In public, so dropping the fixture's schema does not take the extension with it.
    l.qsql(&fx.pool, "CREATE EXTENSION IF NOT EXISTS hypopg SCHEMA public");
    settle_pg(&fx, "big");
    let stmt = "SELECT * FROM big WHERE status = 's7'";
    let p = explained(&l.w, &l.rx, &fx.db, &s, stmt, false, true);
    let fix = p.findings.iter().find_map(|f| f.fix.clone().filter(|x| x.kind == FixKind::Index)).unwrap_or_else(|| panic!("no candidate: {:?}", reasons(&p)));
    l.w.sql_explain_evidence(3, fx.db.clone(), s.clone(), plan::evidence::wants(&p, stmt, Dialect::Postgres));
    let Ok(Msg::SqlExplainEvidence { ev, .. }) = l.rx.recv_timeout(Duration::from_secs(30)) else { panic!("no evidence") };
    assert!(ev.hypopg, "the catalog does not see HypoPG");
    let create = hypo::create_sql(&fix).unwrap_or_else(|| panic!("not a CREATE INDEX: {}", fix.sql)).to_string();
    // Locked, then unlocked: both run in a read-only transaction.
    for read_only in [true, false] {
        l.w.sql_explain_hypo(4, fx.db.clone(), s.clone(), create.clone(), stmt.into(), read_only);
        let Ok(Msg::SqlExplainHypo { res, .. }) = l.rx.recv_timeout(Duration::from_secs(60)) else { panic!("no trial") };
        let t = res.unwrap_or_else(|e| panic!("read_only {read_only}: {e}"));
        assert!(hypo::is_hypothetical(&t.index), "{}", t.index);
        let trial = plan::parse(Dialect::Postgres, &t.plan).unwrap();
        assert!(hypo::uses(&trial, &t.index), "read_only {read_only}: the trial does not use {}:\n{}", t.index, t.plan);
        assert!(trial.cost.unwrap() < p.cost.unwrap(), "the trial costs {:?}, the plan {:?}", trial.cost, p.cost);
    }
    // Unlocked, a write is only planned: the read-only transaction lets EXPLAIN plan it, and nothing is deleted.
    l.w.sql_explain_hypo(5, fx.db.clone(), s.clone(), create.clone(), "DELETE FROM big WHERE status = 's7'".into(), false);
    let Ok(Msg::SqlExplainHypo { res, .. }) = l.rx.recv_timeout(Duration::from_secs(60)) else { panic!("no trial") };
    let t = res.unwrap_or_else(|e| panic!("DELETE: {e}"));
    assert!(hypo::uses(&plan::parse(Dialect::Postgres, &t.plan).unwrap(), &t.index), "{}", t.plan);
    assert_eq!(fx.one("SELECT count(*)::text FROM big"), "50000");
    // Nothing was created: the table has its primary key alone, and a new session has no hypothetical index.
    assert_eq!(fx.one("SELECT count(*)::text FROM pg_indexes WHERE schemaname = current_schema() AND tablename = 'big'"), "1");
    assert_eq!(fx.one("SELECT count(*)::text FROM public.hypopg_list_indexes"), "0");
}

/// The statement with the condition the rewrite finding offers in place of the one it rewrites.
fn rewritten(p: &Plan, stmt: &str, d: Dialect) -> Option<String> {
    let f = p.findings.iter().find(|f| f.fix.as_ref().is_some_and(|x| x.kind == FixKind::Rewrite))?;
    plan::rewrite::statement(stmt, d, &p.nodes[f.node?], f.fix.as_ref()?)
}

fn titles(p: &Plan) -> Vec<String> {
    p.findings.iter().map(|f| f.title.clone()).collect()
}

fn labels(p: &Plan) -> Vec<String> {
    p.nodes.iter().map(|n| format!("{} {}", n.label, n.index)).collect()
}

#[test]
fn live_postgres_rewrites_a_date_of_an_indexed_column_as_a_range_the_index_serves() {
    every_pg(|l, var| {
        let s = unique_name("qexp");
        // A hundred rows a day over 500 days, so one day is a small share of the table.
        let fx = PgFixture::new(
            l,
            &s,
            "CREATE TABLE ev (id int PRIMARY KEY, created_at timestamptz NOT NULL, note text); \
             INSERT INTO ev SELECT g, timestamptz '2026-01-01' + (g % 500) * interval '1 day' + (g % 1440) * interval '1 minute', repeat('x', 40) FROM generate_series(1, 50000) g; \
             CREATE INDEX ev_created_at_idx ON ev (created_at);",
        );
        settle_pg(&fx, "ev");
        let stmt = "SELECT * FROM ev WHERE date(created_at) = '2026-03-01'";
        let p = explained(&l.w, &l.rx, &fx.db, &s, stmt, false, true);
        let sql = rewritten(&p, stmt, Dialect::Postgres).unwrap_or_else(|| panic!("{var}: no rewrite: {:?}", titles(&p)));
        assert_eq!(sql, "SELECT * FROM ev WHERE created_at >= '2026-03-01' AND created_at < '2026-03-02'", "{var}");
        // The rewritten statement reads through the index, and nothing is left to rewrite.
        let p = explained(&l.w, &l.rx, &fx.db, &s, &sql, false, true);
        assert!(p.nodes.iter().any(|n| n.index == "ev_created_at_idx"), "{var}: {:?}", labels(&p));
        assert_eq!(rewritten(&p, &sql, Dialect::Postgres), None, "{var}");
    });
}

#[test]
fn live_mysql_rewrites_a_date_of_an_indexed_column_and_mariadb_needs_none() {
    every_server(|l, var| {
        let s = unique_name("qexp");
        let _fx = MyFixture::new(
            l,
            &s,
            &format!(
                "CREATE TABLE `{s}`.digits (n int); INSERT INTO `{s}`.digits VALUES (0),(1),(2),(3),(4),(5),(6),(7),(8),(9); \
                 CREATE TABLE `{s}`.ev (id int PRIMARY KEY, created_at datetime NOT NULL, note varchar(60), KEY ev_created_at_idx (created_at)); \
                 INSERT INTO `{s}`.ev SELECT g.n, TIMESTAMP '2026-01-01 00:00:00' + INTERVAL (g.n % 500) DAY + INTERVAL (g.n % 1440) MINUTE, REPEAT('x', 40) \
                   FROM (SELECT 1 + a.n + 10 * b.n + 100 * c.n + 1000 * d.n + 10000 * e.n AS n FROM `{s}`.digits a, `{s}`.digits b, `{s}`.digits c, `{s}`.digits d, `{s}`.digits e WHERE e.n < 5) g; \
                 ANALYZE TABLE `{s}`.ev"
            ),
        );
        let d = l.db.dialect;
        let stmt = "SELECT * FROM ev WHERE DATE(created_at) = '2026-03-01'";
        let p = explained(&l.w, &l.rx, &l.db, &s, stmt, false, true);
        if d == Dialect::MariaDb {
            // MariaDB turns DATE() of a column into a range itself: the index is used and nothing is rewritten.
            assert_eq!(rewritten(&p, stmt, d), None, "{var}");
            assert!(p.nodes.iter().any(|n| n.index == "ev_created_at_idx"), "{var}: {:?}", labels(&p));
            return;
        }
        let sql = rewritten(&p, stmt, d).unwrap_or_else(|| panic!("{var}: no rewrite: {:?}", titles(&p)));
        assert_eq!(sql, "SELECT * FROM ev WHERE created_at >= '2026-03-01' AND created_at < '2026-03-02'", "{var}");
        let p = explained(&l.w, &l.rx, &l.db, &s, &sql, false, true);
        assert!(p.nodes.iter().any(|n| n.index == "ev_created_at_idx"), "{var}: {:?}", labels(&p));
        assert_eq!(rewritten(&p, &sql, d), None, "{var}");
    });
}
