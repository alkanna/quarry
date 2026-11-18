//! Trying a candidate index before creating it, with HypoPG: the extension adds a hypothetical index to the planner of
//! one backend, and EXPLAIN on that backend plans as if the index existed. Nothing is written. The index lives in the
//! backend's memory and is not transactional, so a ROLLBACK keeps it: the trial resets it and then closes the
//! connection, which takes the index with the backend even when the reset never ran.
//!
//! The whole trial runs in one `BEGIN READ ONLY` that is rolled back, locked or not: HypoPG 1.4 accepts
//! hypopg_create_index there (checked on PostgreSQL 18), so no write access is ever needed.

use super::super::exec::Conn;
use super::super::plan::findings::Fix;
use super::super::plan::{self, FixKind, Plan};
use super::super::{lex, Db, Dialect, Pool, ResultSet};
use std::sync::atomic::AtomicU64;
use std::time::Duration;

/// The statement timeout of the trial in ms, or the session's when that is shorter. Creating a hypothetical index and
/// planning take milliseconds, so a trial that runs longer waits on something else.
const TRIAL_MS: u64 = 10_000;

/// The schema HypoPG is installed in, which the functions are called through so a search_path never picks another.
const EXT_SCHEMA: &str = "SELECT n.nspname::text AS name FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace WHERE e.extname = 'hypopg'";

/// What a trial gave.
#[derive(Clone, Debug, PartialEq)]
pub struct Trial {
    /// The statement's plan as EXPLAIN (FORMAT JSON) wrote it with the index in place.
    pub plan: String,
    /// The name HypoPG gave the index, which the plan names where it reads through it: "<13640>btree_bookings_provider_status".
    pub index: String,
}

/// The candidate's CREATE INDEX as HypoPG takes it, as the index builder wrote it for a trial. None when `fix` is not
/// one index HypoPG can simulate: a GIN index, the several indexes of an OR, a setting.
pub fn create_sql(fix: &Fix) -> Option<&str> {
    fix.trial.as_deref().filter(|s| fix.kind == FixKind::Index && lex::split_statements(s, Dialect::Postgres).len() == 1)
}

/// The three statements of a trial, in the order they run: create the hypothetical index, explain the statement, reset.
/// `ext` is the schema HypoPG is installed in.
pub fn script(ext: &str, create: &str, stmt: &str) -> [String; 3] {
    let f = Dialect::Postgres.quote_ident(ext);
    [format!("SELECT indexname::text AS name FROM {f}.hypopg_create_index({})", super::advanced::literal(create)), plan::explain_script(Dialect::Postgres, stmt, None), format!("SELECT {f}.hypopg_reset()")]
}

/// Whether `name` is one HypoPG made up: its object id in angle brackets, then the access method and the columns.
pub fn is_hypothetical(name: &str) -> bool {
    name.strip_prefix('<').and_then(|r| r.split_once('>')).is_some_and(|(oid, rest)| !oid.is_empty() && oid.bytes().all(|b| b.is_ascii_digit()) && !rest.is_empty())
}

/// Whether the trial's plan reads through the hypothetical index `index`. A name HypoPG did not make up is a real index,
/// which says nothing about the candidate.
pub fn uses(p: &Plan, index: &str) -> bool {
    is_hypothetical(index) && p.nodes.iter().any(|n| n.index == index)
}

/// Why `stmt` cannot be tried, before anything is sent. On a locked connection the explain goes through the same
/// read-only check as the dialog's own, so a statement refused there is refused here too.
pub fn refused(stmt: &str, read_only: bool) -> Option<String> {
    let explain = plan::explain_script(Dialect::Postgres, stmt, None);
    if lex::split_statements(&explain, Dialect::Postgres).len() != 1 {
        return Some("Only one statement can be tried at a time.".into());
    }
    if read_only {
        return lex::read_only_check(&explain, Dialect::Postgres).err();
    }
    None
}

/// Runs the trial of the index `create` (a CREATE INDEX of `create_sql`) for `stmt`, with `schema` as the dialog's
/// run had it. Each statement is logged like any other run's.
pub(in crate::db::sql) async fn trial(db: &Db, schema: &str, create: &str, stmt: &str, read_only: bool) -> Result<Trial, String> {
    if db.dialect != Dialect::Postgres {
        return Err("HypoPG is a PostgreSQL extension.".into());
    }
    // A pooler can hand the explain to another backend than the one that holds the hypothetical index.
    if db.pooler() {
        return Err("Behind a connection pooler the hypothetical index and the plan could land on different server connections.".into());
    }
    if let Some(why) = refused(stmt, read_only) {
        return Err(why);
    }
    // Planning never waits for long, so the outer limit only covers a connection that does not answer.
    match tokio::time::timeout(Duration::from_millis(TRIAL_MS * 3), on_own_conn(db, schema, create, stmt)).await {
        Ok(r) => r,
        Err(_) => Err("The trial took too long and was dropped. Nothing was created.".into()),
    }
}

async fn on_own_conn(db: &Db, schema: &str, create: &str, stmt: &str) -> Result<Trial, String> {
    // Detached, so the backend that held the hypothetical index never goes back to a pool.
    let mut conn = db.acquire_conn().await?;
    super::super::run::prepare(&mut conn, db, schema, true, &AtomicU64::new(0)).await?;
    let Conn::Pg(c) = &mut conn else { return Err("Not a PostgreSQL connection".into()) };
    let session = match &db.pool {
        Pool::Pg(p) => p.timeout().parse::<u64>().ok().filter(|&ms| ms > 0),
        Pool::My(_) => None,
    };
    let limit = session.map_or(TRIAL_MS, |ms| ms.min(TRIAL_MS));
    let extra = [db.pg_run_locals(schema), super::set_timeout(&limit.to_string())].into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join("; ");
    if let Err(x) = super::read::begin_read_only(c, db, &extra).await {
        return Err(super::super::se(x));
    }
    let res = steps(&mut conn, db, schema, create, stmt).await;
    // The rollback ends the transaction; closing the connection is what drops the index, reset or not.
    let _ = conn.execute("ROLLBACK").await;
    let _ = tokio::time::timeout(Duration::from_secs(2), conn.close()).await;
    res
}

async fn steps(conn: &mut Conn, db: &Db, schema: &str, create: &str, stmt: &str) -> Result<Trial, String> {
    let ext = first_cell(&step(conn, db, schema, EXT_SCHEMA, false).await?).ok_or("HypoPG is not installed in this database.")?;
    let [make, explain, reset] = script(&ext, create, stmt);
    let index = first_cell(&step(conn, db, schema, &make, true).await?).ok_or("HypoPG did not create the index.")?;
    // The plan is taken from the explain's own result, never from whichever result came first.
    let plan = plan::plan_text(&step(conn, db, schema, &explain, true).await?)?;
    step(conn, db, schema, &reset, true).await?;
    Ok(Trial { plan, index })
}

/// One statement of the trial, logged when `log` (the catalog read of where HypoPG lives is Quarry's own).
async fn step(conn: &mut Conn, db: &Db, schema: &str, sql: &str, log: bool) -> Result<Vec<ResultSet>, String> {
    let sets = super::super::run::run_one(conn, sql, 10).await;
    let failed = sets.iter().find_map(|s| s.error.clone());
    if log {
        crate::db::log_sql(db.conn, schema, sql, sets.first().map_or(0, |s| s.ms), failed.as_deref());
    }
    match failed {
        Some(e) => Err(e),
        None => Ok(sets),
    }
}

/// The first cell of the first row, as text.
fn first_cell(sets: &[ResultSet]) -> Option<String> {
    let s = sets.first()?;
    match s.rows.first()?.doc().get(&s.cols.first()?.name) {
        Some(mongodb::bson::Bson::String(t)) => Some(t.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRIAL: &str = include_str!("../plan/fixtures/pg18-hypo-trial.json");
    const BEFORE: &str = include_str!("../plan/fixtures/pg18-hypo-before.json");

    #[test]
    fn the_trial_takes_the_bare_create_index_of_an_index_fix() {
        let fix = |kind, trial: Option<&str>| Fix { kind, label: String::new(), sql: String::new(), note: String::new(), trial: trial.map(str::to_string) };
        let bare = r#"CREATE INDEX "bookings_provider_status_idx" ON "public"."bookings" ("provider", "status")"#;
        assert_eq!(create_sql(&fix(FixKind::Index, Some(bare))), Some(bare));
        // A GIN or an OR's indexes come without one, and a setting or an ANALYZE is no index.
        assert_eq!(create_sql(&fix(FixKind::Index, None)), None);
        assert_eq!(create_sql(&fix(FixKind::Analyze, Some(bare))), None);
        assert_eq!(create_sql(&fix(FixKind::Index, Some("CREATE INDEX i ON t (a); DROP TABLE t"))), None);
    }

    #[test]
    fn the_script_calls_hypopg_through_its_schema_with_the_index_as_a_literal() {
        let [make, explain, reset] = script("public", r#"CREATE INDEX "it's_idx" ON "public"."t" ("a")"#, "SELECT * FROM t WHERE a = 1;");
        assert_eq!(make, r#"SELECT indexname::text AS name FROM "public".hypopg_create_index('CREATE INDEX "it''s_idx" ON "public"."t" ("a")')"#);
        assert_eq!(explain, "EXPLAIN (FORMAT JSON) SELECT * FROM t WHERE a = 1");
        assert_eq!(reset, r#"SELECT "public".hypopg_reset()"#);
        // A schema that needs quoting is quoted.
        assert!(script("My Ext", "CREATE INDEX i ON t (a)", "SELECT 1")[0].contains(r#"FROM "My Ext".hypopg_create_index("#));
        // A backslash reads the same whatever standard_conforming_strings says.
        let [make, ..] = script("public", r"CREATE INDEX i ON t ((a || '\'))", "SELECT 1");
        assert!(make.contains(r"hypopg_create_index(E'CREATE INDEX i ON t ((a || ''\\''))')"), "{make}");
    }

    #[test]
    fn a_locked_trial_refuses_what_the_read_only_check_refuses() {
        assert_eq!(refused("SELECT * FROM t WHERE a = 1", true), None);
        assert!(refused("DELETE FROM t WHERE a = 1", true).is_some());
        assert_eq!(refused("DELETE FROM t WHERE a = 1", false), None);
        assert!(refused("SELECT 1; DROP TABLE t", false).is_some());
    }

    #[test]
    fn the_captured_trial_reads_through_the_hypothetical_index() {
        let p = plan::parse(Dialect::Postgres, TRIAL).unwrap();
        let ix = "<13640>btree_bookings_provider_status";
        assert!(uses(&p, ix));
        assert!(!uses(&p, "<13641>btree_bookings_provider"));
        assert!(!uses(&p, "bookings_pkey"));
        assert!(p.nodes.iter().any(|n| is_hypothetical(&n.index)));
        // The plan without it reads bookings in full and costs about twice as much.
        let before = plan::parse(Dialect::Postgres, BEFORE).unwrap();
        assert!(!before.nodes.iter().any(|n| is_hypothetical(&n.index)));
        assert!(before.cost.unwrap() > p.cost.unwrap() * 1.5, "{:?} {:?}", before.cost, p.cost);
    }

    #[test]
    fn only_hypopg_names_look_hypothetical() {
        for n in ["<13640>btree_bookings_provider_status", "<1>hash_t_a"] {
            assert!(is_hypothetical(n), "{n}");
        }
        for n in ["bookings_provider_status_idx", "", "<>btree_t", "<12a>btree_t", "<13640>", "13640>btree_t"] {
            assert!(!is_hypothetical(n), "{n}");
        }
    }
}
