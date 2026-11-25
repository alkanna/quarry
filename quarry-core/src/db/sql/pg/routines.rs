//! The PostgreSQL side of functions and procedures: reading one, and saving or dropping it in one transaction.
//!
//! A routine is saved as the statement the server printed (`pg_get_functiondef`, which is CREATE OR REPLACE), so every attribute
//! the person did not touch is carried through. CREATE OR REPLACE cannot change the return type or an argument name, and a changed
//! argument list is not a replace at all but a second overload, so the transaction compares the routines of the schema before and
//! after and rolls back when the change is not the one asked for. That is the only guard against a silent second function.
//! `Fail::Changed` is for a routine another session changed since the editor read it.

use crate::db::sql::exec::{sql_state, Tx};
use crate::db::sql::pg::ddl::{exec, fail_text, run_locked, set_path, Fail};
use crate::db::sql::routines::{drop_stmt, param_of, pg_detail, plan, routine_of, Detail, Failure, Param, Routine, PG_DETAIL, PG_PARAMS};
use crate::db::sql::value::Param as Bound;
use crate::db::sql::worker::text;
use crate::db::sql::{se, Db, Dialect};
use crate::db::Res;

const D: Dialect = Dialect::Postgres;

/// What the server prints for the routine with this oid, to compare with what the editor saw. Params: [oid].
const SEEN: &str = "SELECT concat_ws(chr(31), pg_get_functiondef(p.oid), pg_get_userbyid(p.proowner)::text, COALESCE(p.proacl::text, ''), COALESCE(obj_description(p.oid, 'pg_proc'), '')) AS seen FROM pg_proc p WHERE p.oid = $1::oid";

/// The oids of the routines of a schema that the list shows. Params: [schema].
const OIDS: &str = "SELECT p.oid::text AS oid FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = $1 AND p.prokind IN ('f', 'p')";

/// One routine by oid. Params: [oid]. Same columns as the list.
const BY_OID: &str = "SELECT p.proname::text AS name, p.prokind::text AS kind, pg_get_function_identity_arguments(p.oid) AS args, COALESCE(pg_get_function_result(p.oid), '') AS returns, l.lanname::text AS lang FROM pg_proc p JOIN pg_language l ON l.oid = p.prolang WHERE p.oid = $1::oid";

/// Opens a routine: its row and its arguments. The read runs with the schema on the search_path, as the save does, so the
/// types in the statement print the way the save will read them.
pub(in crate::db::sql) async fn detail(db: &Db, schema: &str, r: &Routine) -> Res<Detail> {
    let rows = db.meta_with(schema, PG_DETAIL, [schema, r.name.as_str(), r.kind.letter(), r.args.as_str()], true).await?;
    let row = rows.first().ok_or_else(|| format!("{} is gone. Refresh the list.", r.label(D)))?;
    let oid = text(row, "oid").unwrap_or_default();
    let params: Vec<Param> = db.meta_with(schema, PG_PARAMS, [oid.as_str()], true).await?.iter().filter_map(|p| param_of(&|c| text(p, c))).collect();
    pg_detail(&|c| text(row, c), params).ok_or_else(|| "The routine could not be read.".to_string())
}

fn fail(e: sqlx::Error) -> Fail {
    Fail::Run(se(e))
}

async fn oids(tx: &mut Tx<'_>, schema: &str) -> Result<Vec<String>, Fail> {
    Ok(tx.fetch_bound(OIDS, &[Bound::Text(schema.into())]).await.map_err(fail)?.iter().filter_map(|r| text(r, "oid")).collect())
}

async fn seen(tx: &mut Tx<'_>, oid: &str) -> Result<Option<String>, Fail> {
    Ok(tx.fetch_bound(SEEN, &[Bound::Text(oid.into())]).await.map_err(fail)?.first().and_then(|r| text(r, "seen")))
}

async fn by_oid(tx: &mut Tx<'_>, oid: &str) -> Result<Routine, Fail> {
    let rows = tx.fetch_bound(BY_OID, &[Bound::Text(oid.into())]).await.map_err(fail)?;
    rows.first().and_then(|r| routine_of(&|c| text(r, c))).ok_or_else(|| Fail::Run("The routine could not be read after the change.".into()))
}

/// Whether a refusal means the routine cannot be changed in place: a changed return type or argument name is 42P13, a function
/// made into a procedure or back 42809. The code is read, never the sentence, which follows the server's language.
pub(in crate::db::sql) fn needs_recreate(e: &sqlx::Error) -> bool {
    matches!(sql_state(e).as_deref(), Some("42P13" | "42809"))
}

/// Saves `text` (CREATE OR REPLACE of `old`, a CREATE for a new routine). With `recreate` the old routine is dropped first, in the
/// same transaction, with a plain DROP so a routine something depends on is refused rather than taken along.
pub(in crate::db::sql) async fn save(db: &Db, schema: &str, old: Option<&Detail>, text: &str, recreate: bool, read_only: bool) -> Result<Option<Routine>, Failure> {
    let plan = plan(D, schema, old, text, recreate).map_err(Failure::plain)?;
    let mut ask_recreate = false;
    let res = run_locked(db, schema, read_only, &[], "Refresh the routine list to see what it holds now.", async |tx| {
        set_path(tx, db, schema).await.map_err(fail)?;
        let before = oids(tx, schema).await?;
        if let Some(o) = old {
            let now = seen(tx, &o.oid).await?;
            if now.is_none() {
                return Err(Fail::Changed("it was dropped or replaced by someone else".into()));
            }
            if now.as_deref() != Some(o.seen.as_str()) {
                return Err(Fail::Changed("it was changed by someone else".into()));
            }
        }
        for s in &plan.steps {
            if let Err(e) = exec(tx, db, schema, s).await {
                ask_recreate = old.is_some() && !recreate && needs_recreate(&e);
                return Err(fail(e));
            }
        }
        let after = oids(tx, schema).await?;
        let added: Vec<&String> = after.iter().filter(|o| !before.contains(o)).collect();
        let kept = |o: &str| after.iter().any(|x| x == o);
        match old {
            // A replace keeps the oid. A second routine means the arguments or the name differ from this one.
            Some(o) if !recreate => {
                if !added.is_empty() || !kept(&o.oid) {
                    ask_recreate = true;
                    return Err(Fail::Run("The name or the argument types differ from this routine's, so this would add a second routine and leave this one as it is. Nothing was changed.".into()));
                }
                by_oid(tx, &o.oid).await.map(Some)
            }
            Some(o) => match (added.as_slice(), kept(&o.oid)) {
                ([n], false) => by_oid(tx, n).await.map(Some),
                _ => Err(Fail::Run("The routine was not dropped and created again as expected. Nothing was changed.".into())),
            },
            None => match added.as_slice() {
                [n] => by_oid(tx, n).await.map(Some),
                _ => Err(Fail::Run(format!("Nothing new was created: a routine with that name and arguments may already exist (open it to edit it), or the name is in another schema than {schema}. Nothing was changed."))),
            },
        }
    })
    .await;
    res.map_err(|f| Failure { recreate: ask_recreate, why: fail_text(f, |what| format!("Not saved: {what} since it was opened. Open it again to see the current definition.")) })
}

/// Drops one routine with a plain DROP. A routine that a trigger, a view or another function uses is refused, and the server names
/// what depends on it.
pub(in crate::db::sql) async fn remove(db: &Db, schema: &str, r: &Routine, read_only: bool) -> Res<()> {
    let stmt = drop_stmt(D, schema, r);
    run_locked(db, schema, read_only, &[], "Refresh the routine list to see what is left.", async |tx| {
        set_path(tx, db, schema).await.map_err(fail)?;
        exec(tx, db, schema, &stmt).await.map_err(fail)
    })
    .await
    .map_err(|f| fail_text(f, |what| format!("Not dropped: {what}.")))
}
