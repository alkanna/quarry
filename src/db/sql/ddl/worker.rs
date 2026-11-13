//! The worker side of structure changes: sessions, applying a plan, undo, trigger saves and orphan reads.

use super::*;

/// A writable connection whose session reads text the way the statements were written for. When the server's
/// sql_mode changed since, nothing runs and the error carries the mode now in force.
pub(in crate::db::sql) async fn session(db: &Db, schema: &str, read_only: bool, m: Mode) -> Result<sqlx::MySqlConnection, (String, Option<String>)> {
    let mut c = crate::db::sql::write::writer_conn(db, schema, read_only).await.and_then(|c| c.into_my()).map_err(|x| (x, None))?;
    let now = session_sql_mode(&mut c).await.map_err(|x| (x, None))?;
    if Mode::of(&now) != m {
        return Err((format!("Nothing ran: the server's sql_mode is now {now:?}, which reads quotes or backslashes differently from when the statement was written."), Some(now)));
    }
    Ok(c)
}

/// One statement, prepared, so the server parses exactly one whatever the text holds.
pub(in crate::db::sql) async fn exec(c: &mut sqlx::MySqlConnection, sql: &str) -> Res<()> {
    let t = Instant::now();
    let res = sqlx::query(sql).persistent(false).execute(&mut *c).await;
    crate::db::note_here(sql, t, &res);
    res.map_err(se)?;
    Ok(())
}

pub(in crate::db::sql) fn undo_action(conn: u64, schema: &str, table: &str, u: AlterUndo) -> UndoAction {
    UndoAction { agent: None, at: Instant::now(), conn, db: schema.into(), coll: table.into(), kind: UndoKind::Sql { database: String::new(), cols: Vec::new(), undo: crate::db::sql::write::SqlUndo::Alter(Box::new(u)), ids: Default::default() } }
}

/// Runs a recorded undo after checking the table is still as the change left it. The answer is the same as for
/// applying one; the action comes back for another try only when nothing ran.
pub(in crate::db::sql) async fn undo(db: &Db, a: UndoAction, u: AlterUndo, read_only: bool) -> Msg {
    if db.dialect == Dialect::Postgres {
        return crate::db::sql::pg::ddl::undo(db, a, u, read_only).await;
    }
    let UndoAction { at, conn, db: schema, coll: table, .. } = a;
    let (mut ran, mut seen, mut lost) = (0, None, false);
    let res: Res<String> = crate::db::sql_scope(db.conn, &schema, async {
        let st = load(db, &schema, &table).await?;
        let now = snapshot(&st, &u.cols, &u.idx, &u.fks);
        if now.len() != u.expect.len() {
            return Err("Undo refused: the table could not be compared with how the change left it.".to_string());
        }
        if let Some((what, _)) = u.expect.iter().zip(&now).find(|(a, b)| a != b).map(|(a, _)| a) {
            return Err(format!("Undo refused: {what} changed after this change ran, so undoing it now would not put the table back."));
        }
        let mut c = crate::db::sql::write::writer_conn(db, &schema, read_only).await?.into_my()?;
        let mode = session_sql_mode(&mut c).await?;
        let m = Mode::of(&mode);
        // The steps were written for the mode of the change. A session that reads quotes or backslashes another
        // way now gets the same inverse written for it, instead of a refusal that would hold forever.
        if m != u.mode {
            seen = Some(mode.clone());
        }
        let steps = u.steps_for(db.dialect, &db.version, &table, m).map_err(|why| {
            lost = true;
            format!("Undo removed from Recent changes: the server's sql_mode is now {mode:?}, and the undo cannot be written for it ({why}). Nothing ran.")
        })?;
        for s in &steps {
            exec(&mut c, s).await.map_err(|err| if ran == 0 { err } else { format!("Undo stopped at step {} of {}: {err}. The steps before it ran, so the table is only partly put back.", ran + 1, steps.len()) })?;
            ran += 1;
        }
        Ok("Undid the structure change".to_string())
    })
    .await;
    let keep = (res.is_err() && ran == 0 && !lost).then(|| {
        let mut k = undo_action(conn, &schema, &table, u);
        // Keeps the age of the original so a failed undo does not live longer.
        k.at = at;
        k
    });
    Msg::SqlAltered { conn, database: String::new(), schema, table, res, undo: keep, sql_mode: seen, alter: true, unsure: false }
}

impl Worker {
    /// Runs one ALTER TABLE on a writable connection of its own, written for the sql_mode `m`. `undo` reverses it
    /// when there is one; it is handed back with the answer along with how the table looked right after.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_alter(&self, conn: u64, db: Db, schema: String, table: String, sql: String, m: Mode, undo: Option<AlterUndo>, read_only: bool, done: String) {
        self.spawn_sql(db.conn, &schema.clone(), async move {
            let (res, seen) = match session(&db, &schema, read_only, m).await {
                Ok(mut c) => (exec(&mut c, &sql).await, None),
                Err((why, now)) => (Err(why), now),
            };
            let mut done = done;
            let undo = match (res.is_ok(), undo) {
                (true, Some(mut u)) => match load(&db, &schema, &table).await {
                    Ok(st) => {
                        u.expect = snapshot(&st, &u.cols, &u.idx, &u.fks);
                        Some(undo_action(conn, &schema, &table, u))
                    }
                    Err(err) => {
                        done = format!("{done}. It cannot be undone: the table could not be read back ({err})");
                        None
                    }
                },
                _ => None,
            };
            Msg::SqlAltered { conn, database: String::new(), schema, table, res: res.map(|_| done), undo, sql_mode: seen, alter: true, unsure: false }
        });
    }
}

/// SHOW CREATE TRIGGER's statement and session, with the trigger's neighbours in `all`.
pub(super) async fn stored(c: &mut sqlx::MySqlConnection, d: Dialect, schema: &str, all: &[Trigger], t: &Trigger) -> Res<Stored> {
    let show = format!("SHOW CREATE TRIGGER {}", d.qualified(schema, &t.name));
    let row = raw_first(c, &show).await?.ok_or_else(|| format!("Could not read the definition of {}.", t.name))?;
    let col = |i: usize| crate::db::sql::worker::text_at(&row, i);
    let stmt = col(2).ok_or_else(|| format!("Could not read the definition of {}.", t.name))?;
    let (before, after) = neighbours(all, &t.name);
    Ok(Stored { stmt, sql_mode: col(1).unwrap_or_default(), charset: col(3).unwrap_or_default(), collation: col(4).unwrap_or_default(), timing: t.timing.clone(), event: t.event.clone(), before, after })
}

/// Sets the session's sql_mode, client character set and connection collation, each bound as a value.
pub(super) async fn set_session(c: &mut sqlx::MySqlConnection, mode: &str, charset: &str, collation: &str) -> Res<()> {
    sqlx::query("SET SESSION sql_mode = ?").bind(mode).execute(&mut *c).await.map_err(se)?;
    sqlx::query("SET SESSION character_set_client = ?").bind(charset).execute(&mut *c).await.map_err(se)?;
    sqlx::query("SET SESSION collation_connection = ?").bind(collation).execute(&mut *c).await.map_err(se)?;
    Ok(())
}

/// Runs a stored trigger statement in the session it was made in, so its body reads and runs as before (a
/// PIPES_AS_CONCAT `||` stays a concatenation, an ANSI_QUOTES `"x"` a name), then puts the session back. The text
/// goes out as UTF-8, so a non-UTF-8 client character set is kept only for an ASCII statement; the collation, which
/// decides what its strings are, is always the stored one.
pub(super) async fn replay(c: &mut sqlx::MySqlConnection, d: Dialect, s: &Stored, order: Option<&TrigOrder>, replace: bool) -> Res<()> {
    let sql = restate(d, &s.stmt, Mode::of(&s.sql_mode), order, replace)?;
    let (mode, charset, collation): (String, String, String) = sqlx::query_as("SELECT @@SESSION.sql_mode, @@SESSION.character_set_client, @@SESSION.collation_connection").fetch_one(&mut *c).await.map_err(se)?;
    let utf8 = s.charset.to_ascii_lowercase().starts_with("utf8");
    let cs = if s.charset.is_empty() || !(utf8 || sql.is_ascii()) { charset.as_str() } else { s.charset.as_str() };
    let coll = if s.collation.is_empty() { collation.as_str() } else { s.collation.as_str() };
    let ran = match set_session(c, &s.sql_mode, cs, coll).await {
        Ok(()) => run_text(c, &sql).await,
        Err(x) => Err(x),
    };
    let back = set_session(c, &mode, &charset, &collation).await;
    ran?;
    back
}

/// Runs a statement over the text protocol, for the statements a server will not prepare, and returns its first row.
/// Read as a stream: `fetch_one` and `execute` of a raw statement on a borrowed connection are refused by the
/// compiler inside a spawned task.
pub(super) async fn raw_first(c: &mut sqlx::MySqlConnection, sql: &str) -> Res<Option<sqlx::mysql::MySqlRow>> {
    let t = Instant::now();
    let mut s = sqlx::raw_sql(sql).fetch_many(&mut *c);
    let mut first = None;
    while let Some(item) = futures_util::StreamExt::next(&mut s).await {
        match item {
            Ok(sqlx::Either::Right(r)) => {
                first.get_or_insert(r);
            }
            Ok(_) => {}
            Err(x) => {
                crate::db::note_here(sql, t, &Err::<(), _>(&x));
                return Err(e(x));
            }
        }
    }
    crate::db::note_here(sql, t, &Ok::<(), String>(()));
    Ok(first)
}

/// Runs our own statement text (names quoted by us, a body checked by `one_trigger_statement`) over the text
/// protocol.
pub(super) async fn run_text(c: &mut sqlx::MySqlConnection, sql: &str) -> Res<()> {
    raw_first(c, sql).await.map(|_| ())
}

/// The trigger named `name` among `all`.
pub(super) fn find<'a>(all: &'a [Trigger], name: &str) -> Option<&'a Trigger> {
    all.iter().find(|t| same_name(&t.name, name))
}

/// A scratch name for trying a new trigger before the old one is dropped.
pub(super) fn scratch_name() -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("quarry_check_{:x}", n & 0xffff_ffff_ffff)
}

/// MySQL cannot replace a trigger in place, so the old one is dropped and the new one created while the table is
/// write-locked, which keeps every row write from slipping between the two. The new trigger is first created under a
/// scratch name and dropped again, so a body or definer the server refuses stops before the old one is touched.
/// When the CREATE still fails, `back` is put back. UNLOCK TABLES runs on every way out.
pub(super) async fn swap_locked(c: &mut sqlx::MySqlConnection, d: Dialect, schema: &str, table: &str, name: &str, try_first: Option<(&str, &str)>, create: Swap<'_>, back: &Stored, back_order: Option<&TrigOrder>) -> Res<()> {
    run_text(c, &format!("LOCK TABLES {} WRITE", d.qualified(schema, table))).await.map_err(|x| format!("{x}. Replacing a trigger on MySQL locks the table so no row is written without one, which needs the LOCK TABLES privilege. Nothing was changed."))?;
    let res = swap_inner(c, d, name, try_first, create, back, back_order).await;
    let unlocked = run_text(c, "UNLOCK TABLES").await;
    res?;
    unlocked
}

/// What a locked swap creates: our own CREATE text, or a stored trigger replayed in its session.
pub(super) enum Swap<'a> {
    Text(&'a str),
    Stored(&'a Stored, Option<&'a TrigOrder>),
}

pub(super) async fn swap_inner(c: &mut sqlx::MySqlConnection, d: Dialect, name: &str, try_first: Option<(&str, &str)>, create: Swap<'_>, back: &Stored, back_order: Option<&TrigOrder>) -> Res<()> {
    if let Some((trial, scratch)) = try_first {
        run_text(c, trial).await?;
        run_text(c, &drop_trigger(d, scratch, true)).await?;
    }
    run_text(c, &drop_trigger(d, name, false)).await?;
    let made = match create {
        Swap::Text(sql) => run_text(c, sql).await,
        Swap::Stored(s, o) => replay(c, d, s, o, false).await,
    };
    let Err(err) = made else { return Ok(()) };
    Err(match replay(c, d, back, back_order, false).await {
        Ok(()) => format!("{err}. The trigger was put back as it was."),
        Err(again) => format!("{err}. The trigger could not be put back ({again}). Its definition:\n{}", back.stmt),
    })
}

/// Saves, replaces or drops a trigger, after checking on the connection that writes that its session reads the text
/// as it was written and that the trigger is as the editor saw it. Returns what undo needs.
pub(super) async fn trigger_job(db: &Db, schema: &str, table: &str, job: &TrigJob, m: Mode, read_only: bool) -> Result<(String, TrigUndo), (String, Option<String>)> {
    let d = db.dialect;
    let mut c = session(db, schema, read_only, m).await?;
    let fail = |x: String| (x, None);
    let all = crate::db::sql::structure::triggers_on(&mut c, d, schema, table).await.map_err(fail)?;
    let changed = |n: &str| format!("Nothing ran: the trigger {n} changed on the server since it was read. Reload the structure and try again.");
    match job {
        TrigJob::Create(s) => {
            let sql = create_trigger(d, m, table, s, false).map_err(fail)?;
            one_trigger_statement(&sql, m).map_err(fail)?;
            if find(&all, &s.def.name).is_some() {
                return Err(fail(format!("The table already has a trigger called {}.", s.def.name)));
            }
            run_text(&mut c, &sql).await.map_err(fail)?;
            let live = crate::db::sql::structure::triggers_on(&mut c, d, schema, table).await.ok().and_then(|v| find(&v, &s.def.name).cloned());
            Ok((s.def.name.clone(), TrigUndo { name: s.def.name.clone(), restore: None, left: live, label: format!("created trigger {}", s.def.name), pg: None }))
        }
        TrigJob::Edit { was, save } => {
            let sql = create_trigger(d, m, table, save, matches!(d, Dialect::MariaDb)).map_err(fail)?;
            one_trigger_statement(&sql, m).map_err(fail)?;
            let Some(live) = find(&all, &was.name).filter(|l| same_trigger(l, was)) else { return Err(fail(changed(&was.name))) };
            let old = stored(&mut c, d, schema, &all, live).await.map_err(fail)?;
            if matches!(d, Dialect::MariaDb) {
                run_text(&mut c, &sql).await.map_err(|x| fail(definer_note(x, save.definer.as_deref())))?;
            } else {
                let scratch = TrigSave { def: TrigDef { name: scratch_name(), ..save.def.clone() }, ..save.clone() };
                let trial = create_trigger(d, m, table, &scratch, false).map_err(fail)?;
                one_trigger_statement(&trial, m).map_err(fail)?;
                let back_order = order_among(&all, &was.name, &old.timing, &old.event, old.before.as_deref(), old.after.as_deref());
                swap_locked(&mut c, d, schema, table, &was.name, Some((&trial, &scratch.def.name)), Swap::Text(&sql), &old, back_order.as_ref()).await.map_err(|x| fail(definer_note(x, save.definer.as_deref())))?;
            }
            let left = crate::db::sql::structure::triggers_on(&mut c, d, schema, table).await.ok().and_then(|v| find(&v, &save.def.name).cloned());
            Ok((was.name.clone(), TrigUndo { name: was.name.clone(), restore: Some(old), left, label: format!("changed trigger {}", was.name), pg: None }))
        }
        TrigJob::Drop(was) => {
            let Some(live) = find(&all, &was.name).filter(|l| same_trigger(l, was)) else { return Err(fail(changed(&was.name))) };
            let old = stored(&mut c, d, schema, &all, live).await.map_err(fail)?;
            run_text(&mut c, &drop_trigger(d, &was.name, false)).await.map_err(fail)?;
            Ok((was.name.clone(), TrigUndo { name: was.name.clone(), restore: Some(old), left: None, label: format!("dropped trigger {}", was.name), pg: None }))
        }
    }
}

/// Puts a trigger back as a change found it. It runs only when the trigger is as the change left it, or absent
/// where a previous undo stopped halfway, so it never overwrites a newer change; done twice it changes nothing.
pub(super) async fn undo_trigger_job(db: &Db, schema: &str, table: &str, u: &TrigUndo, read_only: bool) -> Res<String> {
    let d = db.dialect;
    let mut c = crate::db::sql::write::writer_conn(db, schema, read_only).await?.into_my()?;
    let all = crate::db::sql::structure::triggers_on(&mut c, d, schema, table).await?;
    let live = find(&all, &u.name).cloned();
    let refused = format!("Undo refused: the trigger {} changed after this change ran, so undoing it now would overwrite that. Undo it by hand.", u.name);
    match (&u.restore, &u.left, &live) {
        // Undo of a create: drop what it made.
        (None, Some(left), Some(l)) if same_trigger(l, left) => run_text(&mut c, &drop_trigger(d, &u.name, true)).await.map(|_| "Dropped the trigger the change created".into()),
        (None, _, None) => Ok("The trigger the change created is already gone".into()),
        // Undo of a drop: make it again where it was.
        (Some(old), None, None) => {
            let o = order_among(&all, &u.name, &old.timing, &old.event, old.before.as_deref(), old.after.as_deref());
            replay(&mut c, d, old, o.as_ref(), false).await.map(|_| "Put the dropped trigger back".into())
        }
        // Undo of an edit, or a retry of one whose earlier run left no trigger.
        (Some(old), Some(left), now) if now.as_ref().is_none_or(|l| same_trigger(l, left)) => {
            let o = order_among(&all, &u.name, &old.timing, &old.event, old.before.as_deref(), old.after.as_deref());
            match now {
                None => replay(&mut c, d, old, o.as_ref(), false).await,
                Some(_) if matches!(d, Dialect::MariaDb) => replay(&mut c, d, old, o.as_ref(), true).await,
                Some(l) => {
                    // The trigger as the edit left it, which is put back if the old one is refused.
                    let cur = stored(&mut c, d, schema, &all, l).await?;
                    let cur_o = order_among(&all, &u.name, &cur.timing, &cur.event, cur.before.as_deref(), cur.after.as_deref());
                    swap_locked(&mut c, d, schema, table, &u.name, None, Swap::Stored(old, o.as_ref()), &cur, cur_o.as_ref()).await
                }
            }
            .map(|_| "Put the trigger back as it was".into())
        }
        _ => Err(refused),
    }
}

/// Runs a recorded undo of a trigger change. On failure the action stays in the list: the checks above make a retry
/// safe.
pub(in crate::db::sql) async fn undo_trigger(db: &Db, a: UndoAction, u: TrigUndo, read_only: bool) -> Msg {
    if u.pg.is_some() {
        return crate::db::sql::pg::ddl_trig::undo(db, a, u, read_only).await;
    }
    let UndoAction { at, conn, db: schema, coll: table, .. } = a;
    let res = crate::db::sql_scope(db.conn, &schema, undo_trigger_job(db, &schema, &table, &u, read_only)).await;
    let keep = res.is_err().then(|| UndoAction { agent: None, at, ..trigger_undo_action(conn, &schema, &table, u) });
    Msg::SqlAltered { conn, database: String::new(), schema, table, res, undo: keep, sql_mode: None, alter: false, unsure: false }
}

pub(super) fn trigger_undo_action(conn: u64, schema: &str, table: &str, u: TrigUndo) -> UndoAction {
    UndoAction { agent: None, at: Instant::now(), conn, db: schema.into(), coll: table.into(), kind: UndoKind::Sql { database: String::new(), cols: Vec::new(), undo: crate::db::sql::write::SqlUndo::Trigger(Box::new(u)), ids: Default::default() } }
}

/// The count of orphan rows on PostgreSQL and a few of them, from reads of their own.
pub(super) async fn pg_orphans(db: &Db, schema: &str, table: &str, fk: &ForeignKey) -> (Res<u64>, Vec<String>) {
    let n = match db.meta(schema, &orphans_sql(db.dialect, schema, table, fk), []).await {
        Ok(rows) => rows.first().and_then(|r| crate::db::sql::structure::num(r, "n")).unwrap_or(0),
        Err(e) => return (Err(e), Vec::new()),
    };
    if n == 0 {
        return (Ok(0), Vec::new());
    }
    let sample = db.meta(schema, &orphan_sample_sql(db.dialect, schema, table, fk, ORPHAN_SAMPLE), []).await.map(|rows| rows.iter().map(|r| orphan_row(r, fk.cols.len())).collect()).unwrap_or_default();
    (Ok(n), sample)
}

/// How many orphan rows are shown.
pub const ORPHAN_SAMPLE: u32 = 5;

/// One sampled row as `(v1, v2)`.
pub(in crate::db::sql) fn orphan_row(r: &crate::db::sql::exec::SqlRow, n: usize) -> String {
    format!("({})", (0..n).map(|i| crate::db::sql::worker::text(r, &format!("k{i}")).unwrap_or_else(|| "NULL".into())).collect::<Vec<_>>().join(", "))
}

impl Worker {
    /// Saves, replaces or drops a trigger written for the sql_mode `m`. The definition it replaces is read first with
    /// SHOW CREATE TRIGGER, with the session it was made in: it is what a refused MySQL swap and undo put back.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_trigger(&self, conn: u64, db: Db, schema: String, table: String, job: TrigJob, m: Mode, read_only: bool, done: String) {
        if db.dialect == Dialect::Postgres {
            return self.pg_trigger(conn, db, schema, table, job, read_only, done);
        }
        self.spawn_sql(db.conn, &schema.clone(), async move {
            let (res, undo, seen) = match trigger_job(&db, &schema, &table, &job, m, read_only).await {
                Ok((_, u)) => {
                    // A trigger the server shows no more after a create or edit cannot be guarded, so it gets no undo.
                    let usable = u.left.is_some() || matches!(job, TrigJob::Drop(_));
                    (Ok(done), usable.then(|| trigger_undo_action(conn, &schema, &table, u)), None)
                }
                Err((why, now)) => (Err(why), None, now),
            };
            Msg::SqlAltered { conn, database: String::new(), schema, table, res, undo, sql_mode: seen, alter: false, unsure: false }
        });
    }

    /// How many rows of `table` hold a key that `fk` would find nothing for.
    pub fn sql_orphans(&self, req: u64, db: Db, schema: String, table: String, fk: ForeignKey) {
        self.spawn(async move {
            if db.dialect == Dialect::Postgres {
                let (res, sample) = pg_orphans(&db, &schema, &table, &fk).await;
                return Msg::SqlOrphans { req, res, sample };
            }
            let pool = match db.my_pool() { Ok(p) => p, Err(err) => return Msg::SqlOrphans { req, res: Err(err), sample: Vec::new() } };
            let sql = orphans_sql(db.dialect, &schema, &table, &fk);
            let t = Instant::now();
            let res = sqlx::query(&sql).fetch_one(pool).await;
            db.note(&schema, &sql, t, &res);
            let res = res.map_err(se).map(|r| crate::db::sql::structure::num(&r, "n").unwrap_or(0));
            Msg::SqlOrphans { req, res, sample: Vec::new() }
        });
    }

    /// Whether the server takes a foreign key only on a whole primary or unique key (MySQL 8.4's
    /// restrict_fk_on_non_standard_key). Servers without the variable take any index that starts with the columns.
    pub fn sql_fk_rules(&self, req: u64, db: Db) {
        self.spawn(async move {
            let Ok(pool) = db.my_pool() else { return Msg::SqlFkRules { req, strict: false } };
            let strict = sqlx::query_scalar::<_, i64>("SELECT @@restrict_fk_on_non_standard_key").fetch_one(pool).await.is_ok_and(|n| n != 0);
            Msg::SqlFkRules { req, strict }
        });
    }
}
