//! The live check of the Query editor: the server parses the statement under the caret without running it, and what
//! it refuses is marked in the editor. Only syntax errors and unknown names are kept: anything else depends on what
//! the script did before (a SET, a lock), so it would read as a false alarm.

use super::lex::{self, Tok};
use super::{Db, Dialect};
use sqlx::{AssertSqlSafe, Connection, Executor, SqlSafeStr};
use std::time::Duration;

/// What the server refused in a statement. `at` is a byte offset into it, at the token to mark. `warn` is a name the
/// server does not know, which an earlier statement of the script may still create, so it is not called an error.
#[derive(Clone, Debug, PartialEq)]
pub struct Lint {
    pub at: usize,
    pub msg: String,
    pub warn: bool,
}

/// Longer statements are pasted dumps, not typing, and are not sent on every pause.
const MAX: usize = 64 * 1024;

/// What a pooled PostgreSQL check may put EXPLAIN in front of. Any other first word could be one of EXPLAIN's own
/// options (EXPLAIN ANALYZE runs the statement) or a statement EXPLAIN refuses, which would read as a false error.
/// ponytail: a typo in the first word goes unchecked behind a pooler; a full keyword list could let it through.
const EXPLAINABLE: [&str; 8] = ["select", "insert", "update", "delete", "merge", "values", "with", "table"];

/// PostgreSQL's unknown table, column, function, type and schema.
const PG_UNKNOWN: [&str; 5] = ["42P01", "42703", "42883", "42704", "3F000"];
/// MySQL's and MariaDB's unknown table, column, function, database and table of a multi-table statement.
const MY_UNKNOWN: [u16; 5] = [1146, 1054, 1305, 1049, 1109];

/// Parses `stmt` in `schema` the way a run would read it, or None when the server takes it, the check could not run,
/// or what it said is neither a syntax error nor an unknown name.
pub(super) async fn check(db: &Db, schema: &str, stmt: &str) -> Option<Lint> {
    if stmt.trim().is_empty() || stmt.len() > MAX {
        return None;
    }
    // The server's own lock timeout ends a parse that waits behind an ALTER first; this only covers a server that hangs.
    let r = tokio::time::timeout(Duration::from_secs(5), async {
        match db.dialect {
            Dialect::Postgres => pg(db, schema, stmt).await,
            _ => my(db, schema, stmt).await,
        }
    });
    r.await.ok().flatten()
}

/// Parse and Describe inside a read-only transaction that is rolled back, on a pooled connection: the schema is a SET
/// LOCAL and the named statement sqlx makes is closed before the end, so the connection goes back as it came. Behind a
/// pooler a named statement could land on another client's backend, so the statement goes unnamed under EXPLAIN,
/// which plans it and never runs it.
async fn pg(db: &Db, schema: &str, stmt: &str) -> Option<Lint> {
    let pooled = db.pooler();
    if pooled && !explainable(stmt) {
        return None;
    }
    let sent = if pooled { format!("EXPLAIN {stmt}") } else { stmt.to_string() };
    let mut locals = if schema.is_empty() { String::new() } else { format!("{}; ", db.pg_set_path(schema)) };
    locals.push_str("SET LOCAL lock_timeout = '1s'");
    let mut c = db.pg_pool().ok()?.acquire().await.ok()?;
    let r = super::pg::read::in_read_only(&mut c, db, &locals, async |c| {
        if pooled {
            return c.fetch_all(AssertSqlSafe(sent.as_str())).await.map(|_| ());
        }
        (&mut *c).prepare(AssertSqlSafe(sent.clone()).into_sql_str()).await?;
        c.clear_cached_statements().await
    })
    .await;
    let err = r.err()?;
    let d = err.as_database_error()?.try_downcast_ref::<sqlx::postgres::PgDatabaseError>()?;
    let warn = match d.code() {
        "42601" => false,
        c if PG_UNKNOWN.contains(&c) => true,
        _ => return None,
    };
    let shift = sent.len() - stmt.len();
    let at = match d.position() {
        // A character position from 1 into the text sent, EXPLAIN included.
        Some(sqlx::postgres::PgErrorPosition::Original(p)) => stmt.char_indices().map(|(i, _)| i).chain([stmt.len()]).nth(p.saturating_sub(1).saturating_sub(shift)).unwrap_or(stmt.len()),
        _ => name_at(stmt, d.message(), Dialect::Postgres),
    };
    Some(Lint { at, msg: d.message().to_string(), warn })
}

/// COM_STMT_PREPARE on a connection of its own, closed afterwards so the prepared statement and the USE go with it.
async fn my(db: &Db, schema: &str, stmt: &str) -> Option<Lint> {
    let mut conn = db.acquire_conn().await.ok()?;
    let r = async {
        let c = conn.my().ok()?;
        // In seconds, and 1 is the least: a parse queued behind an ALTER's metadata lock gives up quickly.
        c.execute("SET SESSION lock_wait_timeout = 1").await.ok()?;
        if !schema.is_empty() {
            let q = format!("USE {}", db.dialect.quote_ident(schema));
            c.execute(AssertSqlSafe(q.as_str())).await.ok()?;
        }
        c.prepare(AssertSqlSafe(stmt.to_string()).into_sql_str()).await.err()
    }
    .await;
    let _ = conn.close().await;
    let err = r?;
    let d = err.as_database_error()?.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>()?;
    let (n, msg) = (d.number(), d.message());
    if n == 1064 {
        let (at, msg) = mysql_near(stmt, msg);
        return Some(Lint { at, msg, warn: false });
    }
    MY_UNKNOWN.contains(&n).then(|| Lint { at: name_at(stmt, msg, db.dialect), msg: msg.to_string(), warn: true })
}

fn explainable(stmt: &str) -> bool {
    let first = lex::tokens(stmt, Dialect::Postgres).into_iter().find(|(t, _)| !matches!(t, Tok::Space | Tok::Comment));
    first.is_some_and(|(t, r)| t == Tok::Word && EXPLAINABLE.iter().any(|w| w.eq_ignore_ascii_case(&stmt[r.clone()])))
}

/// Where a MySQL syntax error points, and its sentence without the manual advice. The server gives no offset, only the
/// text from the failing token on (`near '...'`, cut at 80 characters, empty at the end) and its line. A sentence in
/// another language does not match and marks the start.
fn mysql_near(stmt: &str, msg: &str) -> (usize, String) {
    let found = (|| {
        let i = msg.rfind("' at line ")?;
        let line: usize = msg[i + 10..].trim().parse().ok()?;
        let near = &msg[msg[..i].find(" near '")? + 7..i];
        if near.is_empty() {
            return Some((stmt.len(), "Syntax error at the end of the statement".to_string()));
        }
        let start: usize = stmt.split_inclusive('\n').take(line.saturating_sub(1)).map(str::len).sum();
        let at = stmt.get(start..)?.find(near)? + start;
        let shown: String = near.chars().take(40).collect();
        Some((at, format!("Syntax error near '{shown}{}'", if shown.len() < near.len() { "\u{2026}" } else { "" })))
    })();
    found.unwrap_or_else(|| (0, msg.to_string()))
}

/// The token of `stmt` that the first quoted name of `msg` points at (`relation "nope"`, `Table 'shop.nope'`), by its
/// last dotted part, or the start when there is none.
fn name_at(stmt: &str, msg: &str, d: Dialect) -> usize {
    let quoted = msg.find(['"', '\'']).and_then(|i| {
        let q = msg[i..].chars().next()?;
        let rest = &msg[i + 1..];
        Some(&rest[..rest.find(q)?])
    });
    let Some(name) = quoted.map(|n| n.rsplit('.').next().unwrap_or(n)) else { return 0 };
    let unquote = |s: &str| if s.len() >= 2 && s.starts_with(['"', '`']) { s[1..s.len() - 1].to_string() } else { s.to_string() };
    lex::tokens(stmt, d).into_iter().find(|(t, r)| matches!(t, Tok::Word | Tok::Quoted) && unquote(&stmt[r.clone()]).eq_ignore_ascii_case(name)).map_or(0, |(_, r)| r.start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mysql_near_finds_the_failing_token_on_its_line() {
        let msg = "You have an error in your SQL syntax; check the manual that corresponds to your MySQL server version for the right syntax to use near 'FRO A' at line 1";
        assert_eq!(mysql_near("SELCT ( FRO A", msg), (8, "Syntax error near 'FRO A'".into()));
        let stmt = "SELECT 1\nFROM t\nWHER x = 1";
        let msg = "You have an error in your SQL syntax; check the manual that corresponds to your MariaDB server version for the right syntax to use near 'x = 1' at line 3";
        assert_eq!(mysql_near(stmt, msg).0, stmt.find("x = 1").unwrap());
        let end = "... for the right syntax to use near '' at line 1";
        assert_eq!(mysql_near("SELECT (", end).0, 8);
        assert_eq!(mysql_near("SELECT (", "Erreur de syntaxe").0, 0);
    }

    #[test]
    fn unknown_names_point_at_their_token() {
        assert_eq!(name_at("SELECT * FROM shop.nope", "Table 'shop.nope' doesn't exist", Dialect::MySql), 19);
        assert_eq!(name_at("SELECT `Qty` FROM t", "Unknown column 'qty' in 'field list'", Dialect::MySql), 7);
        assert_eq!(name_at("SELECT 1 FROM \"Nope\"", "relation \"Nope\" does not exist", Dialect::Postgres), 14);
        assert_eq!(name_at("SELECT f()", "function f() does not exist", Dialect::Postgres), 0);
    }

    #[test]
    fn a_pooled_check_explains_only_what_explain_never_runs() {
        assert!(explainable("select 1"));
        assert!(explainable("-- hi\nWITH x AS (SELECT 1) SELECT * FROM x"));
        assert!(!explainable("ANALYZE orders"));
        assert!(!explainable("CREATE TABLE t (id int)"));
        assert!(!explainable("(SELECT 1)"));
    }

    use crate::db::sql::testing::{every_pg, every_server, live_pgbouncer, unique_name, MyFixture, PgFixture};

    #[test]
    fn live_postgres_marks_syntax_and_unknown_names_and_runs_nothing() {
        every_pg(|l, var| {
            let f = PgFixture::new(l, "lint_pg", "CREATE TABLE things (id int PRIMARY KEY);");
            let go = |s: &str| l.w.rt.block_on(check(&f.db, "lint_pg", s));
            assert_eq!(go("SELCT ( FRO things").map(|e| (e.at, e.warn)), Some((0, false)), "{var}");
            assert_eq!(go("SELECT id FROM things WHERE").map(|e| (e.at, e.warn)), Some((27, false)), "{var}: the end of input");
            assert_eq!(go("SELECT nope FROM things").map(|e| (e.at, e.warn)), Some((7, true)), "{var}");
            assert_eq!(go("SELECT * FROM nothings").map(|e| (e.at, e.warn)), Some((14, true)), "{var}");
            for ok in ["SELECT id FROM things", "CREATE TABLE made (id int)", "INSERT INTO things VALUES (1)", "BEGIN", "ANALYZE things"] {
                assert_eq!(go(ok), None, "{var}: {ok}");
            }
            assert_eq!(f.one("SELECT count(*)::text FROM things"), "0", "{var}: the INSERT ran");
            assert_eq!(f.one("SELECT count(*)::text FROM pg_tables WHERE schemaname = 'lint_pg' AND tablename = 'made'"), "0", "{var}: the CREATE ran");
            let left = l.w.rt.block_on(async {
                let mut c = f.db.pg_pool().unwrap().acquire().await.unwrap();
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM pg_prepared_statements").persistent(false).fetch_one(&mut *c).await.unwrap()
            });
            assert_eq!(left, 0, "{var}: a checked statement stayed prepared on a pooled connection");
        });
    }

    #[test]
    fn live_pgbouncer_explains_reads_and_leaves_the_rest() {
        let Some(l) = live_pgbouncer() else { return };
        let f = PgFixture::new(&l, "lint_pgb", "CREATE TABLE things (id int);");
        assert!(f.db.pooler());
        let go = |s: &str| l.w.rt.block_on(check(&f.db, "lint_pgb", s));
        assert_eq!(go("SELECT id FROM things WHERE").map(|e| (e.at, e.warn)), Some((27, false)));
        assert_eq!(go("SELECT nope FROM things").map(|e| (e.at, e.warn)), Some((7, true)));
        assert_eq!(go("INSERT INTO nothings VALUES (1)").map(|e| (e.at, e.warn)), Some((12, true)));
        for skipped in ["SELECT id FROM things", "INSERT INTO things VALUES (1)", "CREATE TABLE (", "ANALYZE things"] {
            assert_eq!(go(skipped), None, "{skipped}");
        }
        assert_eq!(f.one("SELECT count(*)::text FROM things"), "0", "the INSERT ran");
    }

    #[test]
    fn live_mysql_marks_syntax_and_unknown_names_and_runs_nothing() {
        every_server(|l, var| {
            let f = MyFixture::new(l, &unique_name("lint"), "");
            l.sql(&format!("CREATE TABLE `{}`.things (id int PRIMARY KEY)", f.name));
            let go = |s: &str| l.w.rt.block_on(check(&l.db, &f.name, s));
            let e = go("SELCT ( FRO things").unwrap_or_else(|| panic!("{var}: no syntax error"));
            assert_eq!((e.at, e.warn, e.msg.as_str()), (0, false, "Syntax error near 'SELCT ( FRO things'"), "{var}");
            assert_eq!(go("SELECT id FROM things WHERE").map(|e| (e.at, e.warn)), Some((27, false)), "{var}: the end of input");
            assert_eq!(go("SELECT id\nFROM things\nWHER id = 1").map(|e| e.at), Some(27), "{var}: a later line");
            assert_eq!(go("SELECT nope FROM things").map(|e| (e.at, e.warn)), Some((7, true)), "{var}");
            assert_eq!(go("SELECT * FROM nothings").map(|e| (e.at, e.warn)), Some((14, true)), "{var}");
            for ok in ["SELECT id FROM things", "CREATE TABLE made (id int)", "INSERT INTO things VALUES (1)", "USE mysql"] {
                assert_eq!(go(ok), None, "{var}: {ok}");
            }
            assert_eq!(l.count(&format!("`{}`.things", f.name)), 0, "{var}: the INSERT ran");
            assert_eq!(l.one(&format!("SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = '{}' AND TABLE_NAME = 'made'", f.name)), "0", "{var}: the CREATE ran");
        });
    }
}
