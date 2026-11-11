//! Editing the triggers of a PostgreSQL table. A trigger is two objects, the trigger on the table and the function it runs,
//! so a change is a function statement and trigger statements in one transaction. PostgreSQL 13 has no CREATE OR REPLACE
//! TRIGGER, so a changed trigger is dropped and created again, and the function is replaced first.
//!
//! Every identifier is quoted and qualified, so the search_path never decides which object is meant. The transaction takes the
//! table's lock at the weakest mode its statements need, reads the trigger and the function again under it and compares them
//! with what the editor read (`Seen`), so a change made by another session in between stops the change instead of being
//! overwritten. The same check guards the undo against how the change left things.
//!
//! The function body is the person's own code. It is wrapped in a dollar quote whose tag does not occur in it, and the server
//! parses a plpgsql body when the function is created, so a syntax error rolls the whole change back before COMMIT. A function
//! is rewritten by replacing only its body inside the text `pg_get_functiondef` gave, so SECURITY DEFINER, SET clauses and
//! every other attribute are carried through exactly; when that text cannot be taken apart exactly the change is refused.

use super::ddl::{PgDef, TrigDef, TrigJob, TrigUndo};
use super::ddl_pg::{check_cond_pg, comment_stmt, exec, lock_error, may_lock, name_ok, q, sql_state, Fail, LOCK_WAIT};
use super::exec::Tx;
use super::pg_catalog::{SEEN_FUNCTION, SEEN_TRIGGER};
use super::structure::PgTrig;
use super::value::Param;
use super::{se, worker::text, write, Db, Dialect};
use crate::db::{Msg, Res, UndoAction, UndoKind, Worker};

const D: Dialect = Dialect::Postgres;

/// The events in the order the server prints them.
const EVENTS: [&str; 4] = ["INSERT", "DELETE", "UPDATE", "TRUNCATE"];
const TIMINGS: [&str; 3] = ["BEFORE", "AFTER", "INSTEAD OF"];

/// The events of a trigger as the structure holds them, joined with OR.
pub fn events_of(event: &str) -> Vec<String> {
    event.split(" OR ").map(|e| e.trim().to_ascii_uppercase()).filter(|e| !e.is_empty()).collect()
}

/// The events written the way the server prints them, so two drafts with the same events compare equal.
pub fn canonical(events: &[String]) -> String {
    EVENTS.iter().filter(|e| events.iter().any(|x| x == **e)).copied().collect::<Vec<_>>().join(" OR ")
}

/// The UPDATE OF columns as typed: names separated by commas. A name with a comma in it cannot be typed here.
pub fn cols_of(text: &str) -> Vec<String> {
    text.split(',').map(str::trim).filter(|c| !c.is_empty()).map(str::to_string).collect()
}

/// The name a new trigger's function gets when the person gives none: `<table>_<trigger>_fn`, cut so it stays within the 63
/// bytes PostgreSQL keeps (it would otherwise cut the name without a word).
pub fn func_name(table: &str, trigger: &str) -> String {
    let mut base = format!("{table}_{trigger}");
    let mut n = base.len().min(60);
    while !base.is_char_boundary(n) {
        n -= 1;
    }
    base.truncate(n);
    format!("{base}_fn")
}

/// What the person's draft says, checked.
struct Head {
    name: String,
    timing: String,
    events: Vec<String>,
    cols: Vec<String>,
    per_row: bool,
    when: String,
    mode: String,
}

fn head(d: &TrigDef) -> Result<Head, String> {
    name_ok(&d.name, "A trigger")?;
    let timing = d.timing.trim().to_ascii_uppercase();
    if !TIMINGS.contains(&timing.as_str()) {
        return Err("Timing is BEFORE, AFTER or INSTEAD OF.".into());
    }
    let events = events_of(&d.event);
    if events.is_empty() {
        return Err("Pick at least one event.".into());
    }
    if let Some(e) = events.iter().find(|e| !EVENTS.contains(&e.as_str())) {
        return Err(format!("{e} is not an event: use INSERT, UPDATE, DELETE or TRUNCATE."));
    }
    if canonical(&events).split(" OR ").count() != events.len() {
        return Err("An event is listed twice.".into());
    }
    let cols = cols_of(&d.pg.of_cols);
    for c in &cols {
        name_ok(c, "A column")?;
    }
    if !cols.is_empty() && !events.iter().any(|e| e == "UPDATE") {
        return Err("UPDATE OF columns need the UPDATE event.".into());
    }
    let per_row = d.pg.per_row;
    if events.iter().any(|e| e == "TRUNCATE") && per_row {
        return Err("TRUNCATE fires once per statement, so it needs FOR EACH STATEMENT.".into());
    }
    let when = d.pg.when.trim().to_string();
    if timing == "INSTEAD OF" {
        if !per_row {
            return Err("An INSTEAD OF trigger fires for each row.".into());
        }
        if !cols.is_empty() {
            return Err("An INSTEAD OF trigger cannot list UPDATE OF columns.".into());
        }
        if !when.is_empty() {
            return Err("An INSTEAD OF trigger cannot have a WHEN condition.".into());
        }
    }
    if !when.is_empty() {
        check_cond_pg(&when)?;
    }
    if !matches!(d.pg.mode.as_str(), "O" | "D" | "R" | "A") {
        return Err("The enabled state is one of O, D, R and A.".into());
    }
    Ok(Head { name: d.name.clone(), timing, events, cols, per_row, when, mode: d.pg.mode.clone() })
}

fn table_q(schema: &str, table: &str) -> String {
    D.qualified(schema, table)
}

fn func_q(schema: &str, func: &str) -> String {
    format!("{}()", D.qualified(schema, func))
}

fn trigger_stmt(h: &Head, schema: &str, table: &str, fs: &str, f: &str) -> String {
    let events: Vec<String> = EVENTS
        .iter()
        .filter(|e| h.events.iter().any(|x| x == **e))
        .map(|e| if *e == "UPDATE" && !h.cols.is_empty() { format!("UPDATE OF {}", h.cols.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ")) } else { (*e).to_string() })
        .collect();
    let when = if h.when.is_empty() { String::new() } else { format!(" WHEN ({})", h.when) };
    format!("CREATE TRIGGER {} {} {} ON {} FOR EACH {}{when} EXECUTE FUNCTION {}", q(&h.name), h.timing, events.join(" OR "), table_q(schema, table), if h.per_row { "ROW" } else { "STATEMENT" }, func_q(fs, f))
}

/// The ALTER TABLE that puts a trigger in the enabled state `mode` (tgenabled).
fn mode_stmt(schema: &str, table: &str, name: &str, mode: &str) -> String {
    let how = match mode {
        "D" => "DISABLE",
        "R" => "ENABLE REPLICA",
        "A" => "ENABLE ALWAYS",
        _ => "ENABLE",
    };
    format!("ALTER TABLE {} {how} TRIGGER {}", table_q(schema, table), q(name))
}

fn drop_stmt(schema: &str, table: &str, name: &str, if_exists: bool) -> String {
    format!("DROP TRIGGER {}{} ON {}", if if_exists { "IF EXISTS " } else { "" }, q(name), table_q(schema, table))
}

fn trigger_comment(schema: &str, table: &str, name: &str, text: &str) -> Result<String, String> {
    comment_stmt(&format!("TRIGGER {} ON {}", q(name), table_q(schema, table)), text)
}

/// The dollar quote for a function body: the one `pg_get_functiondef` picks, `$function$` with an x added until the body has
/// no `$function` in it, so the body cannot hold its own closing tag.
fn tag_for(body: &str) -> String {
    let mut t = String::from("$function");
    while body.contains(&t) {
        t.push('x');
    }
    t.push('$');
    t
}

fn body_ok(body: &str) -> Result<(), String> {
    if body.trim().is_empty() {
        return Err("The function body is empty.".into());
    }
    if body.contains('\0') {
        return Err("The function body holds a NUL character, which PostgreSQL cannot store.".into());
    }
    Ok(())
}

fn new_function(schema: &str, name: &str, body: &str) -> Result<String, String> {
    body_ok(body)?;
    let body = body.trim();
    let tag = tag_for(body);
    Ok(format!("CREATE FUNCTION {}\n RETURNS trigger\n LANGUAGE plpgsql\nAS {tag}\n{body}\n{tag}\n", func_q(schema, name)))
}

/// `def` (from `pg_get_functiondef`) with the body `src` replaced by `body`. The text before the body is kept as it is, so
/// SECURITY DEFINER, SET clauses and every other attribute stay. None when `def` does not end with exactly the body in the
/// tag the server would pick, which means it cannot be rewritten without guessing.
fn rewrite_function(def: &str, src: &str, body: &str) -> Result<String, String> {
    body_ok(body)?;
    let old = tag_for(src);
    let tail = format!("AS {old}{src}{old}");
    let rest = def.trim_end_matches('\n');
    let cut = rest.strip_suffix(&tail).filter(|_| def.starts_with("CREATE OR REPLACE FUNCTION ")).ok_or("The function's definition cannot be taken apart exactly, so it is not rewritten. Nothing was changed.")?;
    let tag = tag_for(body);
    Ok(format!("{cut}AS {tag}{body}{tag}\n"))
}

/// What the transaction finds again under the lock: the trigger and the function as `pg_catalog::SEEN_TRIGGER` and
/// `SEEN_FUNCTION` print them, or None for an object that must not exist.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Seen {
    pub trig: Option<String>,
    pub func: Option<String>,
}

/// What puts things back, and what it relies on.
#[derive(Clone, Debug, PartialEq)]
pub struct PgUndo {
    pub steps: Vec<String>,
    /// Whether a step drops a trigger, which takes the table's strongest lock.
    pub exclusive: bool,
    pub trig: String,
    pub func_schema: String,
    pub func: String,
    /// How the change left the trigger and the function; the undo runs only when it still finds them so.
    pub expect: Seen,
    /// What the answer says once the undo ran.
    pub said: String,
}

/// A change ready to run: the statements the preview shows, the lock they need, what they rely on and what undoes them.
#[derive(Debug)]
pub struct PgPlan {
    pub steps: Vec<String>,
    /// Things the person should know before saving: a function shared with other triggers, a SECURITY DEFINER function, one
    /// owned by another role, a function that is kept when its trigger is dropped.
    pub warnings: Vec<String>,
    pub exclusive: bool,
    pub before: Seen,
    pub trig: String,
    pub func_schema: String,
    pub func: String,
    pub undo: PgUndo,
    pub label: String,
}

/// A WHEN condition with its whitespace collapsed: the server deparses it over several lines, and a change of layout is not a change.
fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn others(n: u64) -> String {
    if n == 1 { "1 other trigger".into() } else { format!("{n} other triggers") }
}

/// Whether the trigger part of the draft is not what was read: only then is the trigger dropped and created again.
fn trigger_changed(h: &Head, pg: &PgTrig, was: &super::structure::Trigger) -> bool {
    !was.timing.eq_ignore_ascii_case(&h.timing) || canonical(&events_of(&was.event)) != canonical(&h.events) || pg.of_cols != h.cols || pg.per_row != h.per_row || norm(&pg.when) != norm(&h.when)
}

/// The statements for a job, written for the table `schema`.`table`, or why it cannot be written.
pub fn plan(schema: &str, table: &str, job: &TrigJob) -> Result<PgPlan, String> {
    match job {
        TrigJob::Create(s) => plan_create(schema, table, &s.def),
        TrigJob::Edit { was, save } => plan_edit(schema, table, was, &save.def),
        TrigJob::Drop(was) => plan_drop(schema, table, was),
    }
}

fn plan_create(schema: &str, table: &str, d: &TrigDef) -> Result<PgPlan, String> {
    let h = head(d)?;
    let f = if d.pg.func.trim().is_empty() { func_name(table, &h.name) } else { d.pg.func.trim().to_string() };
    name_ok(&f, "The function")?;
    let mut steps = vec![new_function(schema, &f, &d.body)?, trigger_stmt(&h, schema, table, schema, &f)];
    if h.mode != "O" {
        steps.push(mode_stmt(schema, table, &h.name, &h.mode));
    }
    // The change made the trigger and the function, so undoing it drops both. Another trigger that has taken the function
    // since is noticed in the function's text, which counts its users.
    let undo = PgUndo { steps: vec![drop_stmt(schema, table, &h.name, false), format!("DROP FUNCTION {}", func_q(schema, &f))], exclusive: true, trig: h.name.clone(), func_schema: schema.into(), func: f.clone(), expect: Seen::default(), said: "Dropped the trigger and the function the change created".into() };
    Ok(PgPlan { steps, warnings: Vec::new(), exclusive: false, before: Seen::default(), trig: h.name.clone(), func_schema: schema.into(), func: f, undo, label: format!("created trigger {}", h.name) })
}

fn plan_edit(schema: &str, table: &str, was: &super::structure::Trigger, d: &TrigDef) -> Result<PgPlan, String> {
    let pg = was.pg.as_deref().ok_or("This is not a PostgreSQL trigger.")?;
    if !pg.refuse.is_empty() {
        return Err(format!("{} Nothing was changed.", pg.refuse));
    }
    if d.name != was.name {
        return Err("A trigger keeps its name here: create the new one and drop this one.".into());
    }
    let h = head(d)?;
    let fn_changed = d.body.trim() != pg.src.trim();
    if fn_changed && pg.lang != "plpgsql" {
        return Err(format!("The function {} is written in {}, and only plpgsql functions are rewritten here. Its trigger part can still be changed.", func_q(&pg.func_schema, &pg.func), pg.lang));
    }
    if fn_changed && !pg.func_can {
        return Err(format!("The function {} is owned by {}, and only its owner can replace it.", func_q(&pg.func_schema, &pg.func), pg.func_owner));
    }
    let trig_changed = trigger_changed(&h, pg, was);
    let mode_changed = h.mode != pg.mode;
    if !fn_changed && !trig_changed && !mode_changed {
        return Err("Nothing to save.".into());
    }
    if trig_changed && !pg.no_rewrite.is_empty() {
        return Err(format!("{} Only its function body and enabled state can be changed.", pg.no_rewrite));
    }
    let mut steps = Vec::new();
    let mut back = Vec::new();
    if fn_changed {
        steps.push(rewrite_function(&pg.func_def, &pg.src, &d.body)?);
        back.push(pg.func_def.clone());
    }
    if trig_changed {
        steps.push(drop_stmt(schema, table, &was.name, true));
        steps.push(trigger_stmt(&h, schema, table, &pg.func_schema, &pg.func));
        back.push(drop_stmt(schema, table, &was.name, true));
        back.push(pg.trig_def.clone());
        if h.mode != "O" {
            steps.push(mode_stmt(schema, table, &was.name, &h.mode));
        }
        if pg.mode != "O" {
            back.push(mode_stmt(schema, table, &was.name, &pg.mode));
        }
        if !pg.comment.is_empty() {
            steps.push(trigger_comment(schema, table, &was.name, &pg.comment)?);
            back.push(trigger_comment(schema, table, &was.name, &pg.comment)?);
        }
    } else if mode_changed {
        steps.push(mode_stmt(schema, table, &was.name, &h.mode));
        back.push(mode_stmt(schema, table, &was.name, &pg.mode));
    }
    let mut warnings = Vec::new();
    if fn_changed {
        let f = func_q(&pg.func_schema, &pg.func);
        if pg.func_others > 0 {
            warnings.push(format!("The function {f} also runs {}, so changing its body changes them too.", others(pg.func_others)));
        }
        if pg.func_secdef {
            warnings.push(format!("The function {f} is SECURITY DEFINER: it runs with the privileges of {}, and keeps doing so.", pg.func_owner));
        }
        if !pg.func_mine {
            warnings.push(format!("The function {f} is owned by {}, not by the connected role.", pg.func_owner));
        }
    }
    let undo = PgUndo { steps: back, exclusive: trig_changed, trig: was.name.clone(), func_schema: pg.func_schema.clone(), func: pg.func.clone(), expect: Seen::default(), said: if fn_changed { "Put the trigger and its function back as they were".into() } else { "Put the trigger back as it was".into() } };
    Ok(PgPlan {
        steps,
        warnings,
        exclusive: trig_changed,
        before: Seen { trig: Some(pg.seen_trig.clone()), func: Some(pg.seen_func.clone()) },
        trig: was.name.clone(),
        func_schema: pg.func_schema.clone(),
        func: pg.func.clone(),
        undo,
        label: format!("changed trigger {}", was.name),
    })
}

fn plan_drop(schema: &str, table: &str, was: &super::structure::Trigger) -> Result<PgPlan, String> {
    let pg = was.pg.as_deref().ok_or("This is not a PostgreSQL trigger.")?;
    if !pg.refuse.is_empty() {
        return Err(format!("{} Nothing was changed.", pg.refuse));
    }
    let f = func_q(&pg.func_schema, &pg.func);
    // The function goes with its trigger only when it can come back exactly: a plpgsql function nobody else runs, owned by
    // the connected role, with no privileges granted on it and not part of an extension. A comment is carried. Otherwise it
    // stays, and the note says why.
    let keep = if pg.lang != "plpgsql" {
        Some(format!("is written in {}", pg.lang))
    } else if pg.func_others > 0 {
        Some(format!("is also run by {}", others(pg.func_others)))
    } else if !pg.func_mine {
        Some(format!("is owned by {}", pg.func_owner))
    } else if pg.func_acl {
        Some("has privileges granted on it that would be lost".to_string())
    } else if pg.func_ext {
        Some("belongs to an extension".to_string())
    } else {
        None
    };
    let mut steps = vec![drop_stmt(schema, table, &was.name, false)];
    let mut back = Vec::new();
    let mut warnings = Vec::new();
    match &keep {
        None => {
            steps.push(format!("DROP FUNCTION {f}"));
            back.push(pg.func_def.clone());
            if !pg.func_comment.is_empty() {
                back.push(comment_stmt(&format!("FUNCTION {f}"), &pg.func_comment)?);
            }
        }
        Some(why) => warnings.push(format!("The function {f} stays: it {why}.")),
    }
    back.push(pg.trig_def.clone());
    if pg.mode != "O" {
        back.push(mode_stmt(schema, table, &was.name, &pg.mode));
    }
    if !pg.comment.is_empty() {
        back.push(trigger_comment(schema, table, &was.name, &pg.comment)?);
    }
    let undo = PgUndo { steps: back, exclusive: true, trig: was.name.clone(), func_schema: pg.func_schema.clone(), func: pg.func.clone(), expect: Seen::default(), said: if keep.is_none() { "Put the dropped trigger and its function back".into() } else { "Put the dropped trigger back".into() } };
    Ok(PgPlan {
        steps,
        warnings,
        exclusive: true,
        before: Seen { trig: Some(pg.seen_trig.clone()), func: Some(pg.seen_func.clone()) },
        trig: was.name.clone(),
        func_schema: pg.func_schema.clone(),
        func: pg.func.clone(),
        undo,
        label: format!("dropped trigger {}", was.name),
    })
}

/// The statements and the warnings the preview shows for a job, or why it cannot run.
pub fn preview(schema: &str, table: &str, job: &TrigJob) -> Result<(Vec<String>, Vec<String>), String> {
    plan(schema, table, job).map(|p| (p.steps, p.warnings))
}

/// Whether the explicit LOCK TABLE is taken at all: only a plain table can be locked (a view's lock would reach the tables
/// under it, and a foreign table cannot be locked), and only by a role with a privilege the lock needs. A table that is not
/// there answers yes, so the LOCK reports it gone. Without the lock each statement takes its own, under the same lock_timeout.
async fn lockable(tx: &mut Tx<'_>, name: &str) -> bool {
    let sql = "SELECT COALESCE((SELECT c.relkind::text FROM pg_class c WHERE c.oid = to_regclass($1)), '') AS k";
    let kind = match tx.fetch_bound(sql, &[Param::Text(name.to_string())]).await {
        Ok(rows) => rows.first().and_then(|r| text(r, "k")).unwrap_or_default(),
        Err(_) => String::new(),
    };
    match kind.as_str() {
        "" => true,
        "r" => may_lock(tx, name).await,
        _ => false,
    }
}

/// The trigger and the function as the transaction sees them now.
async fn seen(tx: &mut Tx<'_>, schema: &str, table: &str, trig: &str, fs: &str, f: &str) -> Result<Seen, Fail> {
    let fail = |e: sqlx::Error| Fail::Run(se(e));
    let t = tx.fetch_bound(SEEN_TRIGGER, &[Param::Text(schema.into()), Param::Text(table.into()), Param::Text(trig.into())]).await.map_err(fail)?;
    let g = tx.fetch_bound(SEEN_FUNCTION, &[Param::Text(func_q(fs, f)), Param::Text(table_q(schema, table)), Param::Text(trig.into())]).await.map_err(fail)?;
    Ok(Seen { trig: t.first().and_then(|r| text(r, "s")), func: g.first().and_then(|r| text(r, "s")) })
}

/// What a transaction runs: statements, the lock they need, the objects it watches and what they must look like first.
struct Unit<'a> {
    steps: &'a [String],
    exclusive: bool,
    trig: &'a str,
    func_schema: &'a str,
    func: &'a str,
    before: &'a Seen,
    undo: bool,
}

/// Runs a unit in one transaction and returns how it left the trigger and the function, read before COMMIT so nothing can
/// slip in between.
async fn run(db: &Db, schema: &str, table: &str, u: &Unit<'_>, read_only: bool) -> Result<Seen, Fail> {
    let mut conn = write::writer_conn(db, schema, read_only).await.map_err(Fail::Run)?;
    let mut tx = conn.begin_write().await.map_err(|e| Fail::Run(se(e)))?;
    match locked(&mut tx, db, schema, table, u).await {
        // The connection can drop after the server committed, so a failed COMMIT does not say that nothing changed.
        Ok(after) => tx.commit().await.map(|_| after).map_err(|e| Fail::Unknown(format!("{}. The server did not confirm the commit, so the change may or may not have been applied. The structure is being reloaded: check it before running the change again.", se(e)))),
        Err(f) => {
            let _ = tx.rollback().await;
            Err(f)
        }
    }
}

async fn locked(tx: &mut Tx<'_>, db: &Db, schema: &str, table: &str, u: &Unit<'_>) -> Result<Seen, Fail> {
    let fail = |e: sqlx::Error| Fail::Run(se(e));
    exec(tx, db, schema, &format!("SET LOCAL lock_timeout = '{LOCK_WAIT}'")).await.map_err(fail)?;
    let name = table_q(schema, table);
    if lockable(tx, &name).await {
        // DROP TRIGGER takes ACCESS EXCLUSIVE, and CREATE TRIGGER and ALTER TABLE .. ENABLE or DISABLE TRIGGER take SHARE ROW
        // EXCLUSIVE, which also keeps another session from changing the table's triggers while this one reads them. A change of
        // only the function takes no table lock of its own, so the weaker mode is all it holds.
        let mode = if u.exclusive { "ACCESS EXCLUSIVE" } else { "SHARE ROW EXCLUSIVE" };
        if let Err(e) = exec(tx, db, schema, &format!("LOCK TABLE {name} IN {mode} MODE")).await {
            return Err(match (lock_error(&e, &name), sql_state(&e).as_deref()) {
                (Some(why), _) => Fail::Run(why),
                // 42P01 is undefined_table: the table was dropped after the editor read it.
                (None, Some("42P01")) => Fail::Changed(format!("the table {name} (it is gone)")),
                _ => Fail::Run(se(e)),
            });
        }
    }
    // The editor read the catalogs under this path, and the statements resolve every name under it, so what the server prints
    // for a function outside the schema is the same text now and when it is replayed.
    exec(tx, db, schema, &format!("SET LOCAL search_path = {}", q(schema))).await.map_err(fail)?;
    // A role or a database can turn the body check off, and the check is what stops a body that does not parse.
    exec(tx, db, schema, "SET LOCAL check_function_bodies = on").await.map_err(fail)?;
    let now = seen(tx, schema, table, u.trig, u.func_schema, u.func).await?;
    if now.func.is_some() && u.before.func.is_none() {
        let f = func_q(u.func_schema, u.func);
        return Err(Fail::Run(if u.undo {
            format!("Undo refused: a function named {f} exists again, so the one this change dropped cannot be put back. Nothing was changed.")
        } else {
            format!("A function named {f} already exists. Give the new function another name. Nothing was changed.")
        }));
    }
    if now.trig.is_some() && u.before.trig.is_none() && !u.undo {
        return Err(Fail::Run(format!("The table already has a trigger called {}. Nothing was changed.", u.trig)));
    }
    if now.trig != u.before.trig {
        return Err(Fail::Changed(format!("the trigger {}", u.trig)));
    }
    if now.func != u.before.func {
        return Err(Fail::Changed(format!("the function {}", func_q(u.func_schema, u.func))));
    }
    for (i, s) in u.steps.iter().enumerate() {
        exec(tx, db, schema, s).await.map_err(|e| Fail::Run(format!("{} (statement {} of {}). The whole change was rolled back, so nothing was changed.", se(e), i + 1, u.steps.len())))?;
    }
    seen(tx, schema, table, u.trig, u.func_schema, u.func).await
}

fn fail_text(f: Fail, undo: bool) -> String {
    match f {
        Fail::Run(e) | Fail::Unknown(e) => e,
        Fail::Changed(what) if undo => format!("Undo refused: {what} changed after this change ran, so undoing it now would overwrite that."),
        Fail::Changed(what) => format!("Nothing ran: {what} changed since it was read. Reload the structure and try again."),
    }
}

fn undo_action(db: &Db, conn: u64, schema: &str, table: &str, u: TrigUndo) -> UndoAction {
    UndoAction { at: std::time::Instant::now(), conn, db: schema.into(), coll: table.into(), kind: UndoKind::Sql { database: db.pg_database(), cols: Vec::new(), undo: write::SqlUndo::Trigger(Box::new(u)) } }
}

/// Runs a recorded undo in one transaction, after checking the trigger and its function are as the change left them.
pub(super) async fn undo(db: &Db, a: UndoAction, u: TrigUndo, read_only: bool) -> Msg {
    let UndoAction { at, conn, db: schema, coll: table, .. } = a;
    let Some(p) = u.pg.clone() else { return Msg::SqlAltered { conn, database: db.pg_database(), schema, table, res: Err("This undo is not a PostgreSQL one.".into()), undo: None, sql_mode: None, alter: false, unsure: false } };
    let unit = Unit { steps: &p.steps, exclusive: p.exclusive, trig: &p.trig, func_schema: &p.func_schema, func: &p.func, before: &p.expect, undo: true };
    let out = crate::db::sql_scope(db.conn, &schema, async { run(db, &schema, &table, &unit, read_only).await }).await;
    let unsure = matches!(out, Err(Fail::Unknown(_)));
    let res: Res<String> = out.map(|_| p.said.clone()).map_err(|f| fail_text(f, true));
    // The whole undo is one transaction, so a failure ran nothing and the action stays for another try, unless the commit is
    // the one that failed, when it may have run.
    let keep = (res.is_err() && !unsure).then(|| UndoAction { at, ..undo_action(db, conn, &schema, &table, u) });
    Msg::SqlAltered { conn, database: db.pg_database(), schema, table, res, undo: keep, sql_mode: None, alter: false, unsure }
}

impl Worker {
    /// Saves, replaces or drops a PostgreSQL trigger and its function (see `plan`). The answer carries the undo.
    #[allow(clippy::too_many_arguments)]
    pub fn pg_trigger(&self, conn: u64, db: Db, schema: String, table: String, job: TrigJob, read_only: bool, done: String) {
        self.spawn_sql(db.conn, &schema.clone(), async move {
            let (res, undo, unsure) = match plan(&schema, &table, &job) {
                Err(why) => (Err(why), None, false),
                Ok(p) => {
                    let unit = Unit { steps: &p.steps, exclusive: p.exclusive, trig: &p.trig, func_schema: &p.func_schema, func: &p.func, before: &p.before, undo: false };
                    match run(&db, &schema, &table, &unit, read_only).await {
                        Ok(after) => {
                            let mut pg = p.undo.clone();
                            // After the change the trigger exists and its function exists, or the drop took the function too.
                            pg.expect = after;
                            let u = TrigUndo { name: p.trig.clone(), restore: None, left: None, label: p.label.clone(), pg: Some(pg) };
                            (Ok(done), Some(undo_action(&db, conn, &schema, &table, u)), false)
                        }
                        Err(f) => {
                            let unsure = matches!(f, Fail::Unknown(_));
                            (Err(fail_text(f, false)), None, unsure)
                        }
                    }
                }
            };
            Msg::SqlAltered { conn, database: db.pg_database(), schema, table, res, undo, sql_mode: None, alter: false, unsure }
        });
    }
}

/// The draft a new trigger starts as: a BEFORE INSERT row trigger whose function returns the row it was given.
pub fn blank() -> TrigDef {
    TrigDef { name: String::new(), timing: "BEFORE".into(), event: "INSERT".into(), body: "BEGIN\n  RETURN NEW;\nEND".into(), pg: PgDef::default() }
}

/// The editor's definition of a trigger that exists: its function's body stands in for the body.
pub fn def_of(t: &super::structure::Trigger) -> TrigDef {
    let Some(pg) = t.pg.as_deref() else { return TrigDef { name: t.name.clone(), timing: t.timing.clone(), event: t.event.clone(), body: t.body.clone(), pg: PgDef::default() } };
    TrigDef {
        name: t.name.clone(),
        timing: t.timing.to_ascii_uppercase(),
        event: canonical(&events_of(&t.event)),
        body: pg.src.clone(),
        pg: PgDef { per_row: pg.per_row, of_cols: pg.of_cols.join(", "), when: norm(&pg.when), mode: pg.mode.clone(), func: pg.func.clone() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sql::ddl::TrigSave;
    use crate::db::sql::structure::Trigger;

    fn draft(name: &str) -> TrigDef {
        TrigDef { name: name.into(), ..blank() }
    }

    fn live() -> Trigger {
        let pg = PgTrig {
            per_row: true,
            mode: "O".into(),
            rel_kind: "r".into(),
            func_schema: "app".into(),
            func: "orders_audit_fn".into(),
            lang: "plpgsql".into(),
            src: "\nBEGIN\n  RETURN NEW;\nEND\n".into(),
            func_def: "CREATE OR REPLACE FUNCTION app.orders_audit_fn()\n RETURNS trigger\n LANGUAGE plpgsql\n SECURITY DEFINER\n SET search_path TO 'app'\nAS $function$\nBEGIN\n  RETURN NEW;\nEND\n$function$\n".into(),
            func_owner: "me".into(),
            func_secdef: true,
            func_mine: true,
            func_can: true,
            trig_def: "CREATE TRIGGER orders_audit BEFORE INSERT ON app.orders FOR EACH ROW EXECUTE FUNCTION orders_audit_fn()".into(),
            seen_trig: "t".into(),
            seen_func: "f".into(),
            ..Default::default()
        };
        Trigger { name: "orders_audit".into(), timing: "BEFORE".into(), event: "INSERT".into(), pg: Some(Box::new(pg)), ..Default::default() }
    }

    fn edit(was: &Trigger, f: impl FnOnce(&mut TrigDef)) -> Result<PgPlan, String> {
        let mut d = def_of(was);
        f(&mut d);
        plan("app", "orders", &TrigJob::Edit { was: was.clone(), save: TrigSave { def: d, definer: None, order: None } })
    }

    #[test]
    fn a_new_trigger_is_a_function_and_a_trigger() {
        let p = plan("app", "orders", &TrigJob::Create(TrigSave { def: draft("audit"), definer: None, order: None })).unwrap();
        assert_eq!(p.steps.len(), 2);
        assert_eq!(p.steps[0], "CREATE FUNCTION \"app\".\"orders_audit_fn\"()\n RETURNS trigger\n LANGUAGE plpgsql\nAS $function$\nBEGIN\n  RETURN NEW;\nEND\n$function$\n");
        assert_eq!(p.steps[1], "CREATE TRIGGER \"audit\" BEFORE INSERT ON \"app\".\"orders\" FOR EACH ROW EXECUTE FUNCTION \"app\".\"orders_audit_fn\"()");
        assert!(!p.exclusive, "CREATE TRIGGER needs only SHARE ROW EXCLUSIVE");
        assert_eq!(p.undo.steps, ["DROP TRIGGER \"audit\" ON \"app\".\"orders\"", "DROP FUNCTION \"app\".\"orders_audit_fn\"()"]);
    }

    #[test]
    fn the_function_name_stays_within_63_bytes() {
        let n = func_name(&"t".repeat(40), &"x".repeat(40));
        assert_eq!(n.len(), 63);
        assert!(n.ends_with("_fn"));
        let multi = func_name("ü", &"é".repeat(40));
        assert!(multi.len() <= 63 && multi.ends_with("_fn"));
    }

    #[test]
    fn a_body_never_holds_its_own_closing_tag() {
        assert_eq!(tag_for("BEGIN END"), "$function$");
        assert_eq!(tag_for("x $$ y"), "$function$");
        assert_eq!(tag_for("a $function$ b"), "$functionx$");
        assert_eq!(tag_for("a $function$ b $functionx$ c"), "$functionxx$");
        let mut d = draft("t");
        d.body = "BEGIN\n  PERFORM $$a$$, $function$b$function$;\n  RETURN NEW;\nEND".into();
        let p = plan("s", "t", &TrigJob::Create(TrigSave { def: d.clone(), definer: None, order: None })).unwrap();
        assert!(p.steps[0].contains("AS $functionx$\nBEGIN") && p.steps[0].ends_with("\n$functionx$\n"));
        // the closing tag appears once, at the end
        assert_eq!(p.steps[0].matches("$functionx$").count(), 2);
        d.body = "BEGIN\0END".into();
        assert!(plan("s", "t", &TrigJob::Create(TrigSave { def: d, definer: None, order: None })).unwrap_err().contains("NUL"));
    }

    #[test]
    fn only_the_body_of_a_function_is_replaced() {
        let t = live();
        let p = edit(&t, |d| d.body = "BEGIN\n  NEW.x := 1;\n  RETURN NEW;\nEND".into()).unwrap();
        assert_eq!(p.steps.len(), 1, "the trigger part is the same, so it is not dropped");
        assert!(p.steps[0].starts_with("CREATE OR REPLACE FUNCTION app.orders_audit_fn()\n RETURNS trigger\n LANGUAGE plpgsql\n SECURITY DEFINER\n SET search_path TO 'app'\nAS $function$BEGIN"), "{}", p.steps[0]);
        assert!(!p.exclusive);
        assert_eq!(p.undo.steps, [t.pg.as_ref().unwrap().func_def.clone()]);
        assert!(p.warnings.iter().any(|w| w.contains("SECURITY DEFINER")), "{:?}", p.warnings);
        // a definition that does not end with its body is not guessed at
        let mut odd = live();
        odd.pg.as_mut().unwrap().func_def = "CREATE OR REPLACE FUNCTION app.f()\n RETURNS trigger\n LANGUAGE plpgsql\nAS $function$ something else $function$\n".into();
        assert!(edit(&odd, |d| d.body = "BEGIN END".into()).unwrap_err().contains("taken apart"));
    }

    #[test]
    fn a_changed_trigger_is_dropped_and_created_again_with_its_old_text_as_the_undo() {
        let t = live();
        let p = edit(&t, |d| {
            d.timing = "AFTER".into();
            d.event = "INSERT OR UPDATE".into();
            d.pg.of_cols = "a, b".into();
            d.pg.when = "NEW.qty > 0".into();
            d.pg.mode = "D".into();
        })
        .unwrap();
        assert_eq!(p.steps[0], "DROP TRIGGER IF EXISTS \"orders_audit\" ON \"app\".\"orders\"");
        assert_eq!(p.steps[1], "CREATE TRIGGER \"orders_audit\" AFTER INSERT OR UPDATE OF \"a\", \"b\" ON \"app\".\"orders\" FOR EACH ROW WHEN (NEW.qty > 0) EXECUTE FUNCTION \"app\".\"orders_audit_fn\"()");
        assert_eq!(p.steps[2], "ALTER TABLE \"app\".\"orders\" DISABLE TRIGGER \"orders_audit\"");
        assert!(p.exclusive, "DROP TRIGGER takes ACCESS EXCLUSIVE");
        assert_eq!(p.undo.steps, ["DROP TRIGGER IF EXISTS \"orders_audit\" ON \"app\".\"orders\"", t.pg.as_ref().unwrap().trig_def.as_str()]);
        assert!(p.undo.exclusive);
    }

    #[test]
    fn enabling_or_disabling_alone_takes_the_weak_lock_and_keeps_the_trigger() {
        let t = live();
        let p = edit(&t, |d| d.pg.mode = "D".into()).unwrap();
        assert_eq!(p.steps, ["ALTER TABLE \"app\".\"orders\" DISABLE TRIGGER \"orders_audit\""]);
        assert!(!p.exclusive && !p.undo.exclusive);
        assert_eq!(p.undo.steps, ["ALTER TABLE \"app\".\"orders\" ENABLE TRIGGER \"orders_audit\""]);
        assert_eq!(edit(&t, |_| {}).unwrap_err(), "Nothing to save.");
    }

    #[test]
    fn what_cannot_be_written_back_is_refused() {
        let mut t = live();
        t.pg.as_mut().unwrap().refuse = "It is a constraint trigger, which is not rewritten here.".into();
        assert!(edit(&t, |d| d.body = "BEGIN END".into()).unwrap_err().contains("constraint trigger"));
        assert!(plan("app", "orders", &TrigJob::Drop(t.clone())).unwrap_err().contains("constraint trigger"));
        let mut a = live();
        a.pg.as_mut().unwrap().no_rewrite = "It passes arguments to its function, which are not rewritten here.".into();
        assert!(edit(&a, |d| d.timing = "AFTER".into()).unwrap_err().contains("arguments"));
        assert!(edit(&a, |d| d.body = "BEGIN\n RETURN NEW;\nEND".into()).is_ok(), "its function can still change");
        let mut py = live();
        py.pg.as_mut().unwrap().lang = "plpython3u".into();
        assert!(edit(&py, |d| d.body = "x".into()).unwrap_err().contains("plpython3u"));
        assert!(edit(&py, |d| d.timing = "AFTER".into()).is_ok());
        let mut other = live();
        other.pg.as_mut().unwrap().func_can = false;
        assert!(edit(&other, |d| d.body = "BEGIN END".into()).unwrap_err().contains("only its owner"));
    }

    #[test]
    fn a_dropped_trigger_takes_its_function_only_when_the_function_can_come_back() {
        let t = live();
        let p = plan("app", "orders", &TrigJob::Drop(t.clone())).unwrap();
        assert_eq!(p.steps, ["DROP TRIGGER \"orders_audit\" ON \"app\".\"orders\"", "DROP FUNCTION \"app\".\"orders_audit_fn\"()"]);
        assert_eq!(p.undo.steps[0], t.pg.as_ref().unwrap().func_def);
        assert_eq!(p.undo.steps[1], t.pg.as_ref().unwrap().trig_def);
        let mut shared = live();
        shared.pg.as_mut().unwrap().func_others = 2;
        let p = plan("app", "orders", &TrigJob::Drop(shared)).unwrap();
        assert_eq!(p.steps.len(), 1);
        assert!(p.warnings[0].contains("also run by 2 other triggers"), "{:?}", p.warnings);
        let mut acl = live();
        acl.pg.as_mut().unwrap().func_acl = true;
        assert_eq!(plan("app", "orders", &TrigJob::Drop(acl)).unwrap().steps.len(), 1);
        let mut other_owner = live();
        other_owner.pg.as_mut().unwrap().func_mine = false;
        assert_eq!(plan("app", "orders", &TrigJob::Drop(other_owner)).unwrap().steps.len(), 1);
    }

    #[test]
    fn a_shared_function_warns_when_it_is_rewritten() {
        let mut t = live();
        t.pg.as_mut().unwrap().func_others = 1;
        t.pg.as_mut().unwrap().func_mine = false;
        let p = edit(&t, |d| d.body = "BEGIN\n  RETURN NEW;\nEND -- x".into()).unwrap();
        assert!(p.warnings.iter().any(|w| w.contains("also runs 1 other trigger")), "{:?}", p.warnings);
        assert!(p.warnings.iter().any(|w| w.contains("owned by me")), "{:?}", p.warnings);
        let q = edit(&t, |d| d.timing = "AFTER".into()).unwrap();
        assert!(q.warnings.is_empty(), "the function is not touched");
    }

    #[test]
    fn the_draft_is_checked_before_anything_is_written() {
        let create = |f: &dyn Fn(&mut TrigDef)| {
            let mut d = draft("t");
            f(&mut d);
            plan("s", "t", &TrigJob::Create(TrigSave { def: d, definer: None, order: None })).err()
        };
        assert!(create(&|d| d.name = String::new()).unwrap().contains("needs a name"));
        assert!(create(&|d| d.name = "x".repeat(64)).unwrap().contains("63 bytes"));
        assert!(create(&|d| d.timing = "DURING".into()).unwrap().contains("Timing"));
        assert!(create(&|d| d.event = "".into()).unwrap().contains("at least one event"));
        assert!(create(&|d| d.event = "INSERT OR INSERT".into()).unwrap().contains("twice"));
        assert!(create(&|d| d.pg.of_cols = "a".into()).unwrap().contains("UPDATE event"));
        assert!(create(&|d| d.event = "TRUNCATE".into()).unwrap().contains("FOR EACH STATEMENT"));
        assert!(create(&|d| (d.event, d.pg.per_row) = ("TRUNCATE".into(), false)).is_none());
        assert!(create(&|d| (d.timing, d.pg.when) = ("INSTEAD OF".into(), "true".into())).unwrap().contains("WHEN"));
        assert!(create(&|d| d.pg.when = "1); DROP TABLE t; --".into()).is_some(), "a second statement in the condition");
        assert!(create(&|d| d.pg.when = "NEW.a > 'x'".into()).is_none());
        assert!(create(&|d| d.pg.when = "NEW.a > 1)".into()).unwrap().contains("bracket"));
        assert!(create(&|d| d.pg.func = "x".repeat(64)).unwrap().contains("63 bytes"));
        assert!(create(&|d| d.body = "  ".into()).unwrap().contains("empty"));
    }

    #[test]
    fn events_are_written_in_the_servers_order() {
        let mut d = draft("t");
        d.event = "UPDATE OR INSERT OR DELETE".into();
        d.pg.of_cols = "a".into();
        let p = plan("s", "t", &TrigJob::Create(TrigSave { def: d, definer: None, order: None })).unwrap();
        assert!(p.steps[1].contains(" BEFORE INSERT OR DELETE OR UPDATE OF \"a\" ON "), "{}", p.steps[1]);
        assert_eq!(canonical(&events_of("DELETE OR INSERT")), "INSERT OR DELETE");
    }
}
