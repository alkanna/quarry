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

/// Longer statements are pasted dumps, not typing, and are never sent on a pause.
pub const LINT_MAX: usize = 64 * 1024;

/// PostgreSQL's unknown table, column, type and schema. An unknown function is 42883 too, but so is an operator that does
/// not exist for its types, which no earlier statement creates; that one stays an error.
const PG_UNKNOWN: [&str; 4] = ["42P01", "42703", "42704", "3F000"];
/// MySQL's and MariaDB's unknown table, column, function, database and table of a multi-table statement.
const MY_UNKNOWN: [u16; 5] = [1146, 1054, 1305, 1049, 1109];

/// Parses `stmt` in `schema` the way a run would read it. `Ok(None)` when the server takes it, or what it said is neither
/// a syntax error nor an unknown name; `Err` when the check could not run (no connection, a lock, a timeout), so the
/// statement is not taken as clean.
pub(super) async fn check(db: &Db, schema: &str, stmt: &str) -> Result<Option<Lint>, ()> {
    if stmt.trim().is_empty() || stmt.len() > LINT_MAX {
        return Ok(None);
    }
    // The server's own lock timeout ends a parse that waits behind an ALTER first; this only covers a server that hangs.
    let r = tokio::time::timeout(Duration::from_secs(5), async {
        match db.dialect {
            Dialect::Postgres => pg(db, schema, stmt).await,
            _ => my(db, schema, stmt).await,
        }
    });
    r.await.map_err(|_| ())?
}

/// Parse and Describe inside a read-only transaction that is rolled back, on a pooled connection: the schema is a SET
/// LOCAL, and the named statement sqlx makes is closed before the transaction ends, so the connection goes back as it
/// came. Behind a pooler the transaction keeps every message on one backend, as `Conn::describe` relies on.
async fn pg(db: &Db, schema: &str, stmt: &str) -> Result<Option<Lint>, ()> {
    let mut locals = if schema.is_empty() { String::new() } else { format!("{}; ", db.pg_set_path(schema)) };
    locals.push_str("SET LOCAL lock_timeout = '1s'");
    let mut c = db.pg_pool().map_err(|_| ())?.acquire().await.map_err(|_| ())?;
    let r = super::pg::read::in_read_only(&mut c, db, &locals, async |c| {
        (&mut *c).prepare(AssertSqlSafe(stmt.to_string()).into_sql_str()).await?;
        c.clear_cached_statements().await
    })
    .await;
    let Err(err) = r else { return Ok(None) };
    let d = err.as_database_error().and_then(|d| d.try_downcast_ref::<sqlx::postgres::PgDatabaseError>()).ok_or(())?;
    let warn = match d.code() {
        "42601" => false,
        "42883" => d.message().starts_with("function"),
        c if PG_UNKNOWN.contains(&c) => true,
        // A lock held elsewhere, or a statement name another client of a pooler left on this backend.
        "55P03" | "42P05" => return Err(()),
        _ => return Ok(None),
    };
    let at = match d.position() {
        // A character position from 1.
        Some(sqlx::postgres::PgErrorPosition::Original(p)) => stmt.char_indices().map(|(i, _)| i).chain([stmt.len()]).nth(p.saturating_sub(1)).unwrap_or(stmt.len()),
        _ => name_at(stmt, d.message(), Dialect::Postgres),
    };
    Ok(Some(Lint { at, msg: crate::db::lint_wording(d.message(), warn), warn }))
}

/// COM_STMT_PREPARE on a connection of its own, closed afterwards so the prepared statement and the USE go with it.
/// ponytail: one connection per check, which through a tunnel is a full sign-in per pause; keep a checker connection
/// per Db if that shows.
async fn my(db: &Db, schema: &str, stmt: &str) -> Result<Option<Lint>, ()> {
    let mut conn = db.acquire_conn().await.map_err(|_| ())?;
    let r = async {
        let c = conn.my().map_err(|_| ())?;
        // In seconds, and 1 is the least: a parse queued behind an ALTER's metadata lock gives up quickly.
        c.execute("SET SESSION lock_wait_timeout = 1").await.map_err(|_| ())?;
        if !schema.is_empty() {
            let q = format!("USE {}", db.dialect.quote_ident(schema));
            c.execute(AssertSqlSafe(q.as_str())).await.map_err(|_| ())?;
        }
        Ok(c.prepare(AssertSqlSafe(stmt.to_string()).into_sql_str()).await.err())
    }
    .await;
    let _ = conn.close().await;
    let Some(err) = r? else { return Ok(None) };
    let d = err.as_database_error().and_then(|d| d.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>()).ok_or(())?;
    let (n, msg) = (d.number(), d.message());
    let at = match n {
        1064 => mysql_near(stmt, msg),
        n if MY_UNKNOWN.contains(&n) => name_at(stmt, msg, db.dialect),
        1205 => return Err(()),
        _ => return Ok(None),
    };
    let warn = n != 1064;
    Ok(Some(Lint { at, msg: crate::db::lint_wording(msg, warn), warn }))
}

/// Where a MySQL syntax error points. The server gives no offset, only the text from the failing token on (`near '...'`,
/// cut at 80 characters, empty at the end) and its line. An uncut text is the tail of the statement, so it is matched
/// there and never at an earlier copy of the same words. A sentence in another language marks the start.
fn mysql_near(stmt: &str, msg: &str) -> usize {
    let found = || {
        let i = msg.rfind("' at line ")?;
        let line: usize = msg[i + 10..].trim().parse().ok()?;
        let near = &msg[msg[..i].find(" near '")? + 7..i];
        if stmt.ends_with(near) {
            return Some(stmt.len() - near.len());
        }
        let start: usize = stmt.split_inclusive('\n').take(line.saturating_sub(1)).map(str::len).sum();
        Some(stmt.get(start..)?.find(near)? + start)
    };
    found().unwrap_or(0)
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
    lex::tokens(stmt, d).into_iter().find(|(t, r)| matches!(t, Tok::Word | Tok::Quoted) && lex::unquote_ident(&stmt[r.clone()]).eq_ignore_ascii_case(name)).map_or(0, |(_, r)| r.start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mysql_near_finds_the_failing_token_on_its_line() {
        let msg = "You have an error in your SQL syntax; check the manual that corresponds to your MySQL server version for the right syntax to use near 'FRO A' at line 1";
        assert_eq!(mysql_near("SELCT ( FRO A", msg), 8);
        assert_eq!(crate::db::lint_wording(msg, false), "Syntax error near 'FRO A'");
        // A cut text is looked for from the start of its line.
        let stmt = format!("SELECT 1\nFROM t\nWHER x = 1 {}", "y ".repeat(60));
        let msg = format!("... MariaDB server version for the right syntax to use near '{}' at line 3", &stmt[stmt.find("x = 1").unwrap()..][..80]);
        assert_eq!(mysql_near(&stmt, &msg), stmt.find("x = 1").unwrap());
        // An uncut one is the tail, not an earlier copy.
        assert_eq!(mysql_near("SELECT 1 1", "... to use near '1' at line 1"), 9);
        assert_eq!(mysql_near("SELECT (", "... to use near '' at line 1"), 8);
        assert_eq!(mysql_near("SELECT (", "Erreur de syntaxe"), 0);
    }

    #[test]
    fn unknown_names_point_at_their_token() {
        assert_eq!(name_at("SELECT * FROM shop.nope", "Table 'shop.nope' doesn't exist", Dialect::MySql), 19);
        assert_eq!(name_at("SELECT `Qty` FROM t", "Unknown column 'qty' in 'field list'", Dialect::MySql), 7);
        assert_eq!(name_at("SELECT 1 FROM \"Nope\"", "relation \"Nope\" does not exist", Dialect::Postgres), 14);
        assert_eq!(name_at("SELECT f()", "function f() does not exist", Dialect::Postgres), 0);
    }

    use crate::db::sql::testing::{every_pg, every_server, live_pgbouncer, unique_name, LivePg, MyFixture, PgFixture};

    /// The checks every PostgreSQL connection must pass, through a pooler or not.
    fn pg_cases(l: &LivePg, schema: &str, var: &str) {
        let f = PgFixture::new(l, schema, "CREATE TABLE things (id int PRIMARY KEY);");
        let go = |s: &str| l.w.rt.block_on(check(&f.db, schema, s)).unwrap_or_else(|_| panic!("{var}: the check did not run for {s}"));
        assert_eq!(go("SELCT ( FRO things").map(|e| (e.at, e.warn)), Some((0, false)), "{var}");
        assert_eq!(go("SELECT id FROM things WHERE").map(|e| (e.at, e.warn)), Some((27, false)), "{var}: the end of input");
        assert_eq!(go("CREATE TABLE (id int)").map(|e| (e.at, e.warn)), Some((13, false)), "{var}: DDL");
        assert_eq!(go("SELECT nope FROM things").map(|e| (e.at, e.warn)), Some((7, true)), "{var}");
        assert_eq!(go("SELECT * FROM nothings").map(|e| (e.at, e.warn)), Some((14, true)), "{var}");
        assert_eq!(go("SELECT 'a'::text + 1").map(|e| e.warn), Some(false), "{var}: a missing operator is an error");
        for ok in ["SELECT id FROM things", "CREATE TABLE made (id int)", "INSERT INTO things VALUES (1)", "BEGIN", "ANALYZE things"] {
            assert_eq!(go(ok), None, "{var}: {ok}");
        }
        assert_eq!(f.one("SELECT count(*)::text FROM things"), "0", "{var}: the INSERT ran");
        assert_eq!(f.one(&format!("SELECT count(*)::text FROM pg_tables WHERE schemaname = '{schema}' AND tablename = 'made'")), "0", "{var}: the CREATE ran");
    }

    #[test]
    fn live_postgres_marks_syntax_and_unknown_names_and_runs_nothing() {
        every_pg(|l, var| {
            pg_cases(l, "lint_pg", var);
            let left = l.w.rt.block_on(async {
                let mut c = l.db.at("quarry_test").pg_pool().unwrap().acquire().await.unwrap();
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM pg_prepared_statements").persistent(false).fetch_one(&mut *c).await.unwrap()
            });
            assert_eq!(left, 0, "{var}: a checked statement stayed prepared on a pooled connection");
        });
    }

    #[test]
    fn live_pgbouncer_checks_every_statement_and_runs_nothing() {
        let Some(l) = live_pgbouncer() else { return };
        pg_cases(&l, "lint_pgb", "pgbouncer");
    }

    #[test]
    fn live_mysql_marks_syntax_and_unknown_names_and_runs_nothing() {
        every_server(|l, var| {
            let f = MyFixture::new(l, &unique_name("lint"), "");
            l.sql(&format!("CREATE TABLE `{}`.things (id int PRIMARY KEY)", f.name));
            let go = |s: &str| l.w.rt.block_on(check(&l.db, &f.name, s)).unwrap_or_else(|_| panic!("{var}: the check did not run for {s}"));
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
