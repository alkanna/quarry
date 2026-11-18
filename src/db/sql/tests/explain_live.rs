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
