//! Creating and editing SQL views. The statement is built here from quoted names and one SELECT that the lexer has checked is a
//! single read, and it runs as one prepared statement on a writable connection of its own. An edit is CREATE OR REPLACE VIEW.
//! PostgreSQL refuses that when the column list changes (42P16), and then the caller may ask again with `drop_first`, which drops
//! and creates in one transaction. MySQL and MariaDB keep their ALGORITHM, SQL SECURITY, DEFINER and CHECK OPTION through an edit.
//! The undo of a create drops the view and the undo of an edit puts the old definition back, each only while the view is still as
//! the change left it.

use super::create::SYSTEM_SCHEMAS;
use super::ddl::{self, Mode};
use super::exec::{sql_state, SqlRow, Tx};
use super::lex;
use super::pg::create::system_schema;
use super::pg::ddl::{exec as pg_exec, lock_error, name_ok, run_locked, set_path, Fail};
use super::value::Param;
use super::worker::{text, text_at};
use super::write::SqlUndo;
use super::{se, Db, Dialect};
use crate::db::{Msg, Res, UndoAction, UndoKind, Worker};
use sqlx::Connection;
use std::time::Instant;

const D: Dialect = Dialect::Postgres;

/// Stands in for the error text of a PostgreSQL edit that CREATE OR REPLACE refused because the columns changed. The answer carries
/// it as a flag, so the person is asked about dropping and nothing shows this.
const NEEDS_DROP: &str = "view columns changed";

/// What the server holds of a view, or what a dialog asks to make of one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewDef {
    pub schema: String,
    pub name: String,
    /// The SELECT, without the CREATE VIEW before it or a semicolon after it.
    pub body: String,
    /// PostgreSQL: a materialized view, which has no CREATE OR REPLACE.
    pub materialized: bool,
    /// MySQL and MariaDB: UNDEFINED, MERGE or TEMPTABLE.
    pub algorithm: String,
    /// MySQL and MariaDB: DEFINER or INVOKER.
    pub security: String,
    /// MySQL and MariaDB: the account the view runs as, as user and host.
    pub definer: Option<(String, String)>,
    /// MySQL and MariaDB: empty, CASCADED or LOCAL.
    pub check: String,
    /// PostgreSQL: the reloptions the view has, like `security_invoker=true`. CREATE OR REPLACE resets what it is not given.
    pub options: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ViewSpec {
    pub def: ViewDef,
    /// CREATE OR REPLACE, for an edit.
    pub replace: bool,
    /// PostgreSQL: drop the view and create it in the same transaction, for an edit that changes the columns.
    pub drop_first: bool,
}

/// How a change to a view is put back.
#[derive(Clone, Debug)]
pub struct ViewUndo {
    /// The change made the view, so the undo drops it.
    pub created: bool,
    /// The statement that makes the old definition again, for an edit.
    pub restore: Option<String>,
    /// The SELECT as the change left it. The undo runs only while the view still reads this.
    pub left: String,
    /// The edit dropped the view first, so the restore does too.
    pub dropped: bool,
    pub label: String,
}

/// What the answer to a save or an undo tells the app to do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewDone {
    Created,
    Replaced,
    /// An undo of a create: the view is gone.
    Removed,
    /// An undo of an edit: the old definition is back.
    Restored,
}

fn check_name(d: Dialect, v: &ViewDef, replace: bool) -> Res<()> {
    if v.schema.is_empty() {
        return Err("Pick the database the view belongs to.".into());
    }
    if d == Dialect::Postgres {
        if system_schema(&v.schema) {
            return Err(format!("{} belongs to the server and is left alone.", v.schema));
        }
        if replace {
            return if v.name.is_empty() || v.name.contains('\0') { Err("The view needs a name.".into()) } else { Ok(()) };
        }
        name_ok(&v.name, "View")?;
        if v.name.chars().any(|c| c.is_control()) {
            return Err("View name cannot contain control characters or line breaks.".into());
        }
        return Ok(());
    }
    if SYSTEM_SCHEMAS.iter().any(|s| s.eq_ignore_ascii_case(&v.schema)) {
        return Err(format!("{} belongs to the server and is left alone.", v.schema));
    }
    if replace {
        return if v.name.is_empty() || v.name.chars().any(|c| c.is_control()) { Err("The view needs a name without control characters.".into()) } else { Ok(()) };
    }
    super::create::check_name("view", &v.name, false)
}

/// The text of a view's SELECT as one statement: a read of one SELECT, WITH or TABLE with nothing after it, so the view cannot carry
/// a second statement or write. The server still takes the whole statement as one prepared statement.
pub fn check_body(d: Dialect, text: &str) -> Res<String> {
    let t = text.trim();
    if t.is_empty() {
        return Err("Write the SELECT the view shows.".into());
    }
    let stmts = lex::split_statements(t, d);
    if stmts.len() > 1 {
        return Err("A view holds one SELECT. Remove the statements after the first.".into());
    }
    let stmt = stmts.first().map_or(t, |(_, s)| *s).trim().trim_end_matches(';').trim_end();
    if stmt.is_empty() || !lex::pageable(stmt, d) {
        return Err("A view is one SELECT, WITH or TABLE statement that writes nothing and has no INTO clause.".into());
    }
    Ok(stmt.to_string())
}

fn pg_options(opts: &[String]) -> Res<Vec<String>> {
    opts.iter()
        .map(|o| {
            let (k, v) = o.split_once('=').ok_or_else(|| format!("{o} is not a view option."))?;
            let (k, v) = (k.trim(), v.trim());
            let plain = !v.is_empty() && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !plain || !["security_barrier", "security_invoker", "check_option"].contains(&k) {
                return Err(format!("{o} is not a view option this app writes."));
            }
            Ok(format!("{k} = {v}"))
        })
        .collect()
}

fn one_of(what: &str, v: &str, allowed: &[&str], default: &str) -> Res<String> {
    let v = if v.is_empty() { default } else { v };
    allowed.iter().find(|a| a.eq_ignore_ascii_case(v)).map(|a| a.to_string()).ok_or_else(|| format!("{v} is not a view {what}."))
}

/// The statement for a view. Everything but the SELECT is a quoted name or one of a short list of words.
pub fn create_view_sql(d: Dialect, s: &ViewSpec) -> Res<String> {
    let v = &s.def;
    check_name(d, v, s.replace)?;
    let body = check_body(d, &v.body)?;
    let head = if s.replace { "CREATE OR REPLACE" } else { "CREATE" };
    let name = d.qualified(&v.schema, &v.name);
    if d == Dialect::Postgres {
        let opts = pg_options(&v.options)?;
        let with = if opts.is_empty() { String::new() } else { format!(" WITH ({})", opts.join(", ")) };
        return Ok(format!("{head} VIEW {name}{with} AS\n{body}"));
    }
    let alg = one_of("algorithm", &v.algorithm, &["UNDEFINED", "MERGE", "TEMPTABLE"], "UNDEFINED")?;
    let sec = one_of("security", &v.security, &["DEFINER", "INVOKER"], "DEFINER")?;
    let check = one_of("check option", &v.check, &["NONE", "CASCADED", "LOCAL"], "NONE")?;
    let definer = v.definer.as_ref().map_or(String::new(), |(u, h)| format!(" DEFINER = {}@{}", d.quote_ident(u), d.quote_ident(h)));
    let tail = if check == "NONE" { String::new() } else { format!("\nWITH {check} CHECK OPTION") };
    Ok(format!("{head} ALGORITHM = {alg}{definer} SQL SECURITY {sec} VIEW {name} AS\n{body}{tail}"))
}

/// The statement of a PostgreSQL view that is dropped before it is made again.
pub fn drop_view_sql(d: Dialect, schema: &str, name: &str) -> String {
    format!("DROP VIEW {}", d.qualified(schema, name))
}

// Reading a definition.

struct Cursor<'a> {
    s: &'a str,
    at: usize,
}

impl<'a> Cursor<'a> {
    fn skip_space(&mut self) {
        self.at += self.s[self.at..].len() - self.s[self.at..].trim_start().len();
    }

    /// Consumes `word` (case-blind) when it comes next as a whole word.
    fn word(&mut self, word: &str) -> bool {
        self.skip_space();
        let rest = &self.s[self.at..];
        let ok = rest.len() >= word.len() && rest.is_char_boundary(word.len()) && rest[..word.len()].eq_ignore_ascii_case(word) && !rest[word.len()..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_');
        if ok {
            self.at += word.len();
        }
        ok
    }

    fn punct(&mut self, c: char) -> bool {
        self.skip_space();
        let ok = self.s[self.at..].starts_with(c);
        if ok {
            self.at += c.len_utf8();
        }
        ok
    }

    /// An identifier or account part: quoted with a quote character (doubled inside) or bare up to a space or an at sign.
    fn name(&mut self) -> Option<String> {
        self.skip_space();
        let rest = &self.s[self.at..];
        let q = rest.chars().next().filter(|c| matches!(c, '`' | '\'' | '"'));
        let Some(q) = q else {
            let n = rest.find(|c: char| c.is_whitespace() || c == '@' || c == '.').unwrap_or(rest.len());
            if n == 0 {
                return None;
            }
            self.at += n;
            return Some(rest[..n].to_string());
        };
        let mut out = String::new();
        let mut it = rest.char_indices().skip(1).peekable();
        while let Some((i, c)) = it.next() {
            if c == q {
                if it.peek().is_some_and(|(_, n)| *n == q) {
                    out.push(q);
                    it.next();
                    continue;
                }
                self.at += i + 1;
                return Some(out);
            }
            out.push(c);
        }
        None
    }
}

/// A MySQL or MariaDB `SHOW CREATE VIEW` text read into its parts. None when it is not in the form the servers write:
/// `CREATE [OR REPLACE] ALGORITHM=x DEFINER=u@h SQL SECURITY y VIEW name AS select ... [WITH CHECK OPTION]`.
pub fn parse_create_view(sql: &str) -> Option<ViewDef> {
    let mut c = Cursor { s: sql, at: 0 };
    let mut v = ViewDef::default();
    if !c.word("CREATE") {
        return None;
    }
    c.word("OR");
    c.word("REPLACE");
    loop {
        if c.word("ALGORITHM") {
            if !c.punct('=') {
                return None;
            }
            v.algorithm = c.name()?.to_ascii_uppercase();
        } else if c.word("DEFINER") {
            if !c.punct('=') {
                return None;
            }
            let user = c.name()?;
            if c.punct('@') {
                v.definer = Some((user, c.name()?));
            } else {
                // CURRENT_USER and its call form name no account to keep.
                c.punct('(');
                c.punct(')');
            }
        } else if c.word("SQL") {
            if !c.word("SECURITY") {
                return None;
            }
            v.security = c.name()?.to_ascii_uppercase();
        } else {
            break;
        }
    }
    if !c.word("VIEW") {
        return None;
    }
    let first = c.name()?;
    (v.schema, v.name) = if c.punct('.') { (first, c.name()?) } else { (String::new(), first) };
    if !c.word("AS") {
        return None;
    }
    let mut body = sql[c.at..].trim();
    let low = body.to_ascii_uppercase();
    for (tail, check) in [(" WITH CASCADED CHECK OPTION", "CASCADED"), (" WITH LOCAL CHECK OPTION", "LOCAL"), (" WITH CHECK OPTION", "CASCADED")] {
        if low.ends_with(tail) {
            body = body[..body.len() - tail.len()].trim_end();
            v.check = check.into();
            break;
        }
    }
    v.body = body.trim_end_matches(';').trim_end().to_string();
    (!v.body.is_empty()).then_some(v)
}

const PG_DEF: &str = "SELECT c.relkind::text AS kind, COALESCE(pg_get_viewdef(c.oid, true), '')::text AS def, COALESCE(array_to_string(c.reloptions, ','), '')::text AS opts \
                      FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind IN ('v', 'm')";

fn pg_def(rows: &[SqlRow], schema: &str, name: &str) -> Option<ViewDef> {
    let r = rows.first()?;
    let options = text(r, "opts").unwrap_or_default().split(',').map(str::trim).filter(|o| !o.is_empty()).map(String::from).collect();
    Some(ViewDef { schema: schema.into(), name: name.into(), body: text(r, "def").unwrap_or_default().trim().trim_end_matches(';').trim_end().to_string(), materialized: text(r, "kind").as_deref() == Some("m"), options, ..Default::default() })
}

/// The definition of a view as the server has it, read under the search path the edit will run under so the names print the way
/// they will be read back.
async fn read_def(db: &Db, schema: &str, name: &str) -> Res<ViewDef> {
    if db.dialect == Dialect::Postgres {
        let rows = db.meta_with(schema, PG_DEF, [schema, name], true).await?;
        return pg_def(&rows, schema, name).ok_or_else(|| format!("{name} is not a view in {schema}."));
    }
    let rows = db.read_text(schema, &db.dialect.create_stmt(true, schema, name), false).await?;
    let create = rows.first().and_then(|r| text_at(r, 1)).ok_or_else(|| format!("{name} is not a view in {schema}."))?;
    parse_create_view(&create).map(|v| ViewDef { schema: schema.into(), name: name.into(), ..v }).ok_or_else(|| "The server's definition of this view is in a form this app cannot read.".to_string())
}

async fn my_def(c: &mut sqlx::MySqlConnection, d: Dialect, schema: &str, name: &str) -> Res<ViewDef> {
    let sql = d.create_stmt(true, schema, name);
    let rows = sqlx::query(&sql).persistent(false).fetch_all(&mut *c).await.map_err(se)?;
    let create = rows.first().and_then(|r| text_at(r, 1));
    let (schema, name) = (schema.to_string(), name.to_string());
    let create = create.ok_or_else(|| format!("{name} is not a view in {schema}."))?;
    parse_create_view(&create).map(|v| ViewDef { schema, name, ..v }).ok_or_else(|| "The server's definition of this view is in a form this app cannot read.".to_string())
}

async fn pg_def_in(tx: &mut Tx<'_>, schema: &str, name: &str) -> Res<Option<ViewDef>> {
    let rows = tx.fetch_bound(PG_DEF, &[Param::Text(schema.into()), Param::Text(name.into())]).await.map_err(se)?;
    Ok(pg_def(&rows, schema, name))
}

// Running a change.

struct Applied {
    name: String,
    restore: Option<String>,
    left: String,
}

fn restore_sql(d: Dialect, old: &ViewDef) -> Option<String> {
    create_view_sql(d, &ViewSpec { def: old.clone(), replace: true, drop_first: false }).ok()
}

/// The sql_mode of the session may have changed since the dialog read it, in which case the statement is read again under the new one.
async fn mode_session(db: &Db, read_only: bool) -> Res<(sqlx::MySqlConnection, Option<String>)> {
    match ddl::session(db, "", read_only, Mode::of(&db.sql_mode)).await {
        Ok(c) => Ok((c, None)),
        Err((_, Some(now))) => ddl::session(db, "", read_only, Mode::of(&now)).await.map(|c| (c, Some(now))).map_err(|(x, _)| x),
        Err((x, None)) => Err(x),
    }
}

/// A refused DEFINER says whose it is, since keeping another account as the definer needs a privilege this login may lack.
fn definer_note(err: String, definer: &Option<(String, String)>) -> String {
    match definer {
        Some((u, h)) if err.contains("1227") || err.contains("SET_USER_ID") || err.contains("SUPER") => format!("{err}. The view runs as {u}@{h}, and keeping that definer needs the SET_USER_ID (or SUPER) privilege, which this login lacks. Untick Keep the definer to run it as this login. Nothing was changed."),
        _ => err,
    }
}

async fn my_apply(db: &Db, spec: &ViewSpec, read_only: bool) -> (Res<Applied>, Option<String>) {
    let d = db.dialect;
    let (mut c, seen) = match mode_session(db, read_only).await {
        Ok(x) => x,
        Err(e) => return (Err(e), None),
    };
    let (spec, d) = (spec.clone(), d);
    let out = async {
        let (schema, typed, definer) = (spec.def.schema.clone(), spec.def.name.clone(), spec.def.definer.clone());
        let sql = create_view_sql(d, &spec)?;
        let old = if spec.replace { Some(my_def(&mut c, d, &schema, &typed).await.map_err(|e| format!("Could not read the current definition of {typed}: {e}"))?) } else { None };
        ddl::exec(&mut c, &sql).await.map_err(|e| definer_note(e, &definer))?;
        // A server that folds names stores the view under another spelling than the one typed.
        let find = "SELECT CAST(TABLE_NAME AS CHAR) AS n FROM information_schema.VIEWS WHERE TABLE_SCHEMA = ? AND LOWER(TABLE_NAME) = LOWER(?)";
        let row = sqlx::query(find).bind(schema.clone()).bind(typed.clone()).fetch_all(&mut c).await.map_err(se)?;
        let name = row.iter().filter_map(|r| text(r, "n")).find(|n| *n == typed).or_else(|| row.first().and_then(|r| text(r, "n"))).unwrap_or(typed);
        let now = my_def(&mut c, d, &schema, &name).await?;
        Ok(Applied { restore: old.as_ref().and_then(|o| restore_sql(d, o)), name, left: now.body })
    }
    .await;
    let _ = c.close().await;
    (out, seen)
}

async fn pg_apply(db: &Db, spec: &ViewSpec, read_only: bool) -> Res<Applied> {
    let sql = create_view_sql(D, spec)?;
    let (spec, owned) = (spec.clone(), db.clone());
    let drop = drop_view_sql(D, &spec.def.schema, &spec.def.name);
    let scope = spec.def.schema.clone();
    let run = run_locked(db, &scope, read_only, &[], "Refresh the lists and check before running it again.", async move |tx| {
        let (db, v) = (&owned, &spec.def);
        set_path(tx, db, &v.schema).await.map_err(|x| Fail::Run(se(x)))?;
        let old = if spec.replace {
            match pg_def_in(tx, &v.schema, &v.name).await.map_err(Fail::Run)? {
                Some(o) if o.materialized => return Err(Fail::Run(format!("{} is a materialized view, which has no CREATE OR REPLACE.", v.name))),
                Some(o) => Some(o),
                None => return Err(Fail::Run(format!("The view {} is gone.", v.name))),
            }
        } else {
            None
        };
        if spec.drop_first {
            // Without CASCADE the server refuses while another view or object depends on this one, and nothing is dropped.
            pg_exec(tx, db, &v.schema, &drop).await.map_err(|x| Fail::Run(lock_error(&x, &v.name).unwrap_or_else(|| se(x))))?;
        }
        if let Err(x) = pg_exec(tx, db, &v.schema, &sql).await {
            let columns = spec.replace && !spec.drop_first && sql_state(&x).as_deref() == Some("42P16");
            return Err(Fail::Run(if columns { NEEDS_DROP.into() } else { lock_error(&x, &v.name).unwrap_or_else(|| se(x)) }));
        }
        let now = pg_def_in(tx, &v.schema, &v.name).await.map_err(Fail::Run)?.ok_or_else(|| Fail::Run("The view was made but cannot be read back.".into()))?;
        Ok(Applied { restore: old.as_ref().and_then(|o| restore_sql(D, o)), name: v.name.clone(), left: now.body })
    })
    .await;
    run.map_err(|f| match f {
        Fail::Run(e) | Fail::Unknown(e) | Fail::Changed(e) => e,
    })
}

fn entry(db: &Db, conn: u64, schema: &str, name: &str, u: ViewUndo) -> UndoAction {
    UndoAction { agent: None, at: Instant::now(), conn, db: schema.into(), coll: name.into(), kind: UndoKind::Sql { database: db.pg_database(), cols: Vec::new(), undo: SqlUndo::View(Box::new(u)), ids: Default::default() } }
}

/// Puts back or removes what a change did, if the view still reads as the change left it.
async fn undo_my(db: &Db, schema: &str, name: &str, u: &ViewUndo, read_only: bool) -> Res<()> {
    let d = db.dialect;
    let (mut c, _) = mode_session(db, read_only).await?;
    let drop = drop_view_sql(d, schema, name);
    let out = async {
        let now = my_def(&mut c, d, schema, name).await.map_err(|_| format!("Not undone: the view {name} is gone."))?;
        if now.body != u.left {
            return Err(format!("Not undone: {name} was changed after this, so undoing now would overwrite that."));
        }
        match (&u.restore, u.created) {
            (_, true) => ddl::exec(&mut c, &drop).await,
            (Some(sql), false) => ddl::exec(&mut c, sql).await,
            (None, false) => Err("Not undone: the old definition was not kept.".into()),
        }
    }
    .await;
    let _ = c.close().await;
    out
}

/// The old definition goes back with CREATE OR REPLACE where the columns allow it. An edit that added or removed columns cannot be
/// undone that way, so then the view is dropped and made again, which the person asked for by undoing and which the server refuses
/// while another object depends on the view.
async fn undo_pg(db: &Db, schema: &str, name: &str, u: &ViewUndo, read_only: bool) -> Res<()> {
    match undo_pg_with(db, schema, name, u, read_only, false).await {
        Err(e) if e == NEEDS_DROP => undo_pg_with(db, schema, name, u, read_only, true).await,
        r => r,
    }
}

async fn undo_pg_with(db: &Db, schema: &str, name: &str, u: &ViewUndo, read_only: bool, force_drop: bool) -> Res<()> {
    let (owned, u, scope, name) = (db.clone(), u.clone(), schema.to_string(), name.to_string());
    let run = run_locked(db, &scope.clone(), read_only, &[], "Refresh the lists and check before running it again.", async move |tx| {
        let (db, u, schema, name) = (&owned, &u, scope.as_str(), name.as_str());
        set_path(tx, db, schema).await.map_err(|x| Fail::Run(se(x)))?;
        let now = pg_def_in(tx, schema, name).await.map_err(Fail::Run)?.filter(|n| !n.materialized).ok_or_else(|| Fail::Run(format!("Not undone: the view {name} is gone.")))?;
        if now.body != u.left {
            return Err(Fail::Run(format!("Not undone: {name} was changed after this, so undoing now would overwrite that.")));
        }
        let drop = u.created || u.dropped || force_drop;
        if drop {
            pg_exec(tx, db, schema, &drop_view_sql(D, schema, name)).await.map_err(|x| Fail::Run(lock_error(&x, name).unwrap_or_else(|| format!("Not undone: {}", se(x)))))?;
        }
        if !u.created {
            let sql = u.restore.as_deref().ok_or_else(|| Fail::Run("Not undone: the old definition was not kept.".into()))?;
            if let Err(x) = pg_exec(tx, db, schema, sql).await {
                let columns = !drop && sql_state(&x).as_deref() == Some("42P16");
                return Err(Fail::Run(if columns { NEEDS_DROP.into() } else { lock_error(&x, name).unwrap_or_else(|| se(x)) }));
            }
        }
        Ok(())
    })
    .await;
    run.map_err(|f| match f {
        Fail::Run(e) | Fail::Unknown(e) | Fail::Changed(e) => e,
    })
}

impl Worker {
    /// Reads a view's definition for the edit dialog.
    pub fn sql_view_def(&self, conn: u64, db: Db, schema: String, name: String) {
        self.spawn_sql(db.conn, &schema.clone(), async move {
            let res = read_def(&db, &schema, &name).await;
            Msg::SqlViewDef { conn, database: db.pg_database(), schema, name, res }
        });
    }

    /// Creates a view, or replaces one when `spec.replace` is set.
    pub fn sql_save_view(&self, conn: u64, db: Db, spec: ViewSpec, read_only: bool) {
        self.spawn_sql(db.conn, &spec.def.schema.clone(), async move {
            let (res, seen) = if db.dialect == Dialect::Postgres { (pg_apply(&db, &spec, read_only).await, None) } else { my_apply(&db, &spec, read_only).await };
            let need_drop = matches!(&res, Err(e) if e == NEEDS_DROP);
            let (schema, database) = (spec.def.schema.clone(), db.pg_database());
            let (name, undo, res) = match res {
                Ok(a) => {
                    let u = (!spec.replace || a.restore.is_some()).then(|| ViewUndo { created: !spec.replace, restore: a.restore.clone(), left: a.left.clone(), dropped: spec.drop_first, label: if spec.replace { format!("changed view {}", a.name) } else { format!("created view {}", a.name) } });
                    let said = format!("{} view {}", if spec.replace { "Saved" } else { "Created" }, a.name);
                    (a.name.clone(), u.map(|u| entry(&db, conn, &schema, &a.name, u)), Ok(said))
                }
                Err(_) if need_drop => (spec.def.name.clone(), None, Err("CREATE OR REPLACE refuses to change the columns of a view.".to_string())),
                Err(e) => (spec.def.name.clone(), None, Err(e)),
            };
            let kind = if spec.replace { ViewDone::Replaced } else { ViewDone::Created };
            Msg::SqlViewSaved { conn, database, schema, name, kind, res, undo, need_drop, sql_mode: seen }
        });
    }

    /// Undoes a create or an edit of a view. A refused undo hands the entry back.
    pub fn sql_undo_view(&self, db: Db, a: UndoAction, read_only: bool) {
        let scope = a.db.clone();
        self.spawn_sql(db.conn, &scope, async move {
            let UndoAction { at, conn, db: schema, coll: name, kind, .. } = a;
            let UndoKind::Sql { database, cols, undo: SqlUndo::View(u), ids } = kind else {
                return Msg::SqlViewSaved { conn, database: String::new(), schema, name, kind: ViewDone::Restored, res: Err("Not a view change".into()), undo: None, need_drop: false, sql_mode: None };
            };
            let res = if db.dialect == Dialect::Postgres { undo_pg(&db, &schema, &name, &u, read_only).await } else { undo_my(&db, &schema, &name, &u, read_only).await };
            let done = if u.created { ViewDone::Removed } else { ViewDone::Restored };
            let said = if u.created { format!("Removed view {name}") } else { format!("Put back the old definition of {name}") };
            let kept = res.is_err().then(|| UndoAction { agent: None, at, conn, db: schema.clone(), coll: name.clone(), kind: UndoKind::Sql { database: database.clone(), cols, undo: SqlUndo::View(u), ids } });
            Msg::SqlViewSaved { conn, database, schema, name, kind: done, res: res.map(|_| said), undo: kept, need_drop: false, sql_mode: None }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(d: Dialect, name: &str, body: &str) -> ViewSpec {
        let mut s = ViewSpec { def: ViewDef { schema: "shop".into(), name: name.into(), body: body.into(), ..Default::default() }, ..Default::default() };
        if d != Dialect::Postgres {
            (s.def.algorithm, s.def.security) = ("UNDEFINED".into(), "DEFINER".into());
        }
        s
    }

    #[test]
    fn a_mysql_view_names_its_algorithm_and_security_and_quotes_every_name() {
        let s = spec(Dialect::MySql, "big`orders", "SELECT * FROM orders WHERE total > 100;");
        assert_eq!(create_view_sql(Dialect::MySql, &s).unwrap(), "CREATE ALGORITHM = UNDEFINED SQL SECURITY DEFINER VIEW `shop`.`big``orders` AS\nSELECT * FROM orders WHERE total > 100");
    }

    #[test]
    fn an_edit_keeps_the_definer_and_the_check_option() {
        let mut s = spec(Dialect::MariaDb, "v", "select 1 AS a");
        s.replace = true;
        s.def.definer = Some(("app".into(), "%".into()));
        (s.def.algorithm, s.def.security, s.def.check) = ("merge".into(), "invoker".into(), "local".into());
        assert_eq!(create_view_sql(Dialect::MariaDb, &s).unwrap(), "CREATE OR REPLACE ALGORITHM = MERGE DEFINER = `app`@`%` SQL SECURITY INVOKER VIEW `shop`.`v` AS\nselect 1 AS a\nWITH LOCAL CHECK OPTION");
        s.def.algorithm = "x; DROP TABLE t".into();
        assert!(create_view_sql(Dialect::MariaDb, &s).unwrap_err().contains("not a view algorithm"));
    }

    #[test]
    fn a_postgresql_view_writes_its_options_back_so_a_replace_does_not_reset_them() {
        let mut s = spec(Dialect::Postgres, "v", "SELECT 1 AS a");
        s.replace = true;
        s.def.options = vec!["security_invoker=true".into(), "check_option=local".into()];
        assert_eq!(create_view_sql(Dialect::Postgres, &s).unwrap(), "CREATE OR REPLACE VIEW \"shop\".\"v\" WITH (security_invoker = true, check_option = local) AS\nSELECT 1 AS a");
        s.def.options = vec!["fillfactor=70".into()];
        assert!(create_view_sql(Dialect::Postgres, &s).is_err());
        s.def.options = vec!["security_barrier=true) AS SELECT 1; DROP TABLE t; --".into()];
        assert!(create_view_sql(Dialect::Postgres, &s).is_err());
        s.def.options.clear();
        assert_eq!(create_view_sql(Dialect::Postgres, &s).unwrap(), "CREATE OR REPLACE VIEW \"shop\".\"v\" AS\nSELECT 1 AS a");
    }

    #[test]
    fn the_body_is_one_read_and_nothing_else() {
        for d in [Dialect::MySql, Dialect::MariaDb, Dialect::Postgres] {
            assert_eq!(check_body(d, "  select a from t ;  ").unwrap(), "select a from t");
            assert_eq!(check_body(d, "WITH x AS (SELECT 1) SELECT * FROM x").unwrap(), "WITH x AS (SELECT 1) SELECT * FROM x");
            assert_eq!(check_body(d, "select 'a;b' AS c").unwrap(), "select 'a;b' AS c");
            for bad in ["", "   ", "select 1; drop table t", "drop table t", "delete from t", "select * into backup from t", "insert into t values (1)", "-- just a note"] {
                assert!(check_body(d, bad).is_err(), "{d:?} took {bad:?}");
            }
        }
    }

    #[test]
    fn names_that_belong_to_the_server_or_cannot_be_read_back_are_refused() {
        let mut s = spec(Dialect::MySql, "v", "select 1");
        s.def.schema = "MySQL".into();
        assert!(create_view_sql(Dialect::MySql, &s).unwrap_err().contains("belongs to the server"));
        s.def.schema = "shop".into();
        s.def.name = String::new();
        assert!(create_view_sql(Dialect::MySql, &s).unwrap_err().contains("name"));
        let mut p = spec(Dialect::Postgres, &"n".repeat(64), "select 1");
        assert!(create_view_sql(Dialect::Postgres, &p).unwrap_err().contains("63 bytes"));
        p.replace = true;
        assert!(create_view_sql(Dialect::Postgres, &p).is_ok(), "a view already on the server is edited whatever it is called");
        p.def.schema = "pg_catalog".into();
        assert!(create_view_sql(Dialect::Postgres, &p).is_err());
    }

    #[test]
    fn show_create_view_reads_into_its_parts() {
        let v = parse_create_view("CREATE ALGORITHM=UNDEFINED DEFINER=`root`@`%` SQL SECURITY DEFINER VIEW `shop`.`big` AS select `o`.`id` AS `id` from `orders` `o` where (`o`.`total` > 100)").unwrap();
        assert_eq!((v.algorithm.as_str(), v.security.as_str(), v.definer.clone()), ("UNDEFINED", "DEFINER", Some(("root".to_string(), "%".to_string()))));
        assert_eq!((v.schema.as_str(), v.name.as_str(), v.check.as_str()), ("shop", "big", ""));
        assert_eq!(v.body, "select `o`.`id` AS `id` from `orders` `o` where (`o`.`total` > 100)");
        // MariaDB writes the name alone, an account with a quote in it, and the check option after the select.
        let m = parse_create_view("CREATE ALGORITHM=MERGE DEFINER=`we``ird user`@`10.0.%` SQL SECURITY INVOKER VIEW `v` AS select 1 AS `a` WITH LOCAL CHECK OPTION").unwrap();
        assert_eq!((m.algorithm.as_str(), m.security.as_str(), m.check.as_str(), m.name.as_str()), ("MERGE", "INVOKER", "LOCAL", "v"));
        assert_eq!(m.definer, Some(("we`ird user".to_string(), "10.0.%".to_string())));
        assert_eq!(m.body, "select 1 AS `a`");
        let c = parse_create_view("CREATE ALGORITHM=UNDEFINED DEFINER=`a`@`b` SQL SECURITY DEFINER VIEW `v` AS select 'x WITH CHECK OPTION' AS `c`").unwrap();
        assert_eq!((c.check.as_str(), c.body.as_str()), ("", "select 'x WITH CHECK OPTION' AS `c`"));
        assert!(parse_create_view("CREATE TABLE `t` (`a` int)").is_none() && parse_create_view("garbage").is_none() && parse_create_view("CREATE VIEW `v` AS ").is_none());
    }

    #[test]
    fn a_definition_read_from_the_server_builds_the_statement_that_puts_it_back() {
        let old = parse_create_view("CREATE ALGORITHM=MERGE DEFINER=`root`@`localhost` SQL SECURITY INVOKER VIEW `shop`.`v` AS select 1 AS `a` WITH CASCADED CHECK OPTION").unwrap();
        let back = restore_sql(Dialect::MySql, &ViewDef { schema: "shop".into(), name: "v".into(), ..old }).unwrap();
        assert_eq!(back, "CREATE OR REPLACE ALGORITHM = MERGE DEFINER = `root`@`localhost` SQL SECURITY INVOKER VIEW `shop`.`v` AS\nselect 1 AS `a`\nWITH CASCADED CHECK OPTION");
    }

    #[test]
    fn a_refused_definer_names_the_account_and_the_way_out() {
        let who = Some(("app".to_string(), "%".to_string()));
        let said = definer_note("Access denied; you need (at least one of) the SUPER or SET_USER_ID privilege(s) (1227)".into(), &who);
        assert!(said.contains("app@%") && said.contains("Untick Keep the definer"), "{said}");
        assert_eq!(definer_note("Table 'x' doesn't exist".into(), &who), "Table 'x' doesn't exist");
        assert_eq!(definer_note("1227".into(), &None), "1227");
    }
}
