//! Creating and dropping PostgreSQL databases, schemas and tables. Every name is quoted, and the only user text besides names
//! is a column type and a default expression, which `ddl_pg` checks the way the structure editor does. Where MySQL's DDL
//! commits by itself, PostgreSQL's is transactional, so a schema or a table is made and dropped inside one transaction
//! and an undo checks and drops in the same one. A database is the exception: CREATE and DROP DATABASE refuse to run in a
//! transaction block, so they run alone on a connection of their own taken from the pool of the connection's own database
//! (never from the pool of the database being dropped), and the undo of a create checks that the database is empty a
//! round trip before it drops it, which no lock can close.

use super::create::{Ddl, DropInfo};
use super::ddl::{snapshot, ColDef};
use super::ddl_pg::{col_body, comment_stmt, exec, lock_error, may_lock, name_ok, q, read_in, sql_state, table_state, LOCK_WAIT};
use super::exec::{Cells, Conn, SqlRow, Tx};
use super::structure::{num, Structure};
use super::value::Param;
use super::worker::text;
use super::write::{self, SqlUndo};
use super::{se, Db, Dialect, Pool};
use crate::db::{Msg, Res, UndoAction, UndoKind, Worker};
use std::time::{Duration, Instant};

const D: Dialect = Dialect::Postgres;

/// Databases the server needs; creating or dropping one is refused before a statement exists. PostgreSQL compares
/// database names exactly, so `Postgres` is a database of its own and not this one.
pub const SYSTEM_DBS: [&str; 3] = ["postgres", "template0", "template1"];

/// Schemas the server owns. `pg_toast*` and `pg_temp*` are one family each: every backend has its own.
pub fn system_schema(name: &str) -> bool {
    matches!(name, "pg_catalog" | "information_schema") || name.starts_with("pg_toast") || name.starts_with("pg_temp")
}

pub fn system_db(name: &str) -> bool {
    SYSTEM_DBS.contains(&name)
}

/// How many dependent objects a confirmation lists before it says "and N more".
pub const LISTED: usize = 20;

const LOCKED: &str = "Writes are locked on this connection. Unlock them first.";

/// A name this app creates. PostgreSQL would take a newline or a tab, but nobody could type it back.
fn new_name(what: &str, name: &str) -> Res<()> {
    name_ok(name, what)?;
    if name.chars().any(|c| c.is_control()) {
        return Err(format!("{what} name cannot contain control characters or line breaks."));
    }
    Ok(())
}

/// A name that is already on the server and is dropped: the quoted name reads back as the one name whatever it holds.
fn existing(what: &str, name: &str) -> Res<()> {
    if name.is_empty() {
        return Err(format!("{what} needs a name."));
    }
    if name.contains('\0') {
        return Err(format!("{what} name cannot hold a NUL character."));
    }
    Ok(())
}

pub fn create_db_sql(name: &str, template: Option<&str>, templates: &[String]) -> Res<String> {
    new_name("A database", name)?;
    if system_db(name) {
        return Err(format!("{name} belongs to the server and is left alone."));
    }
    let with = match template {
        None => String::new(),
        // The list is the server's own. A name outside it would be a database someone is connected to, which PostgreSQL refuses to copy.
        Some(t) if templates.iter().any(|k| k == t) => format!(" TEMPLATE {}", q(t)),
        Some(t) => return Err(format!("{t} is not a template database on this server.")),
    };
    Ok(format!("CREATE DATABASE {}{with}", q(name)))
}

pub fn drop_db_sql(name: &str, force: bool) -> Res<String> {
    existing("A database", name)?;
    if system_db(name) {
        return Err(format!("{name} belongs to the server and is left alone."));
    }
    Ok(format!("DROP DATABASE {}{}", q(name), if force { " WITH (FORCE)" } else { "" }))
}

pub fn create_schema_sql(name: &str) -> Res<String> {
    new_name("A schema", name)?;
    if system_schema(name) || name.starts_with("pg_") {
        return Err(format!("{name} is reserved: PostgreSQL keeps the names that start with pg_ and information_schema for its own schemas."));
    }
    Ok(format!("CREATE SCHEMA {}", q(name)))
}

pub fn drop_schema_sql(name: &str, cascade: bool) -> Res<String> {
    existing("A schema", name)?;
    if system_schema(name) {
        return Err(format!("{name} belongs to the server and is left alone."));
    }
    Ok(format!("DROP SCHEMA {}{}", q(name), if cascade { " CASCADE" } else { "" }))
}

pub fn drop_table_sql(schema: &str, table: &str, view: bool, cascade: bool) -> Res<String> {
    existing("A schema", schema)?;
    existing("A table", table)?;
    if system_schema(schema) {
        return Err(format!("{schema} belongs to the server and is left alone."));
    }
    Ok(format!("DROP {} {}{}", if view { "VIEW" } else { "TABLE" }, D.qualified(schema, table), if cascade { " CASCADE" } else { "" }))
}

/// What the New table dialog collects. A column is the structure editor's `ColDef`, so its type, default and identity are written
/// and checked by the same code that adds a column to an existing table.
#[derive(Clone, Debug, Default)]
pub struct TableSpec {
    pub schema: String,
    pub name: String,
    pub cols: Vec<ColDef>,
    /// The columns of the primary key, in key order.
    pub pk: Vec<String>,
    /// Each entry is one UNIQUE constraint over its columns.
    pub uniques: Vec<Vec<String>>,
    pub comment: String,
}

/// The statements that make the table: the CREATE TABLE, then the comments. PostgreSQL names the key and unique constraints
/// itself (`t_pkey`, `t_c_key`), which is the name every other client and the structure editor already know.
pub fn create_table_stmts(spec: &TableSpec) -> Res<Vec<String>> {
    new_name("A schema", &spec.schema)?;
    if system_schema(&spec.schema) {
        return Err(format!("{} belongs to the server and is left alone.", spec.schema));
    }
    new_name("A table", &spec.name)?;
    if spec.cols.is_empty() {
        return Err("Add at least one column.".into());
    }
    let mut seen: Vec<String> = Vec::new();
    let mut lines = Vec::new();
    for c in &spec.cols {
        new_name("A column", &c.name)?;
        // The structure editor tells columns apart without case, so a table with `A` and `a` could not be edited later.
        if seen.contains(&c.name.to_lowercase()) {
            return Err(format!("Two columns are called {} (names that differ only in case are refused).", c.name));
        }
        seen.push(c.name.to_lowercase());
        let mut def = c.clone();
        // A key column is never null, so the statement says so instead of leaving it to the key.
        def.nullable = def.nullable && !spec.pk.contains(&c.name);
        lines.push(format!("  {} {}", q(&def.name), col_body(&def).map_err(|e| format!("Column {}: {e}", c.name))?));
    }
    let has = |n: &String| spec.cols.iter().any(|c| &c.name == n);
    let group = |what: &str, cols: &[String]| -> Res<String> {
        if cols.is_empty() {
            return Err(format!("A {what} needs at least one column."));
        }
        if let Some(n) = cols.iter().find(|n| !has(n)) {
            return Err(format!("The {what} names {n}, which is not a column."));
        }
        if cols.iter().enumerate().any(|(i, n)| cols[..i].contains(n)) {
            return Err(format!("The {what} names a column twice."));
        }
        Ok(cols.iter().map(|n| q(n)).collect::<Vec<_>>().join(", "))
    };
    if !spec.pk.is_empty() {
        lines.push(format!("  PRIMARY KEY ({})", group("primary key", &spec.pk)?));
    }
    for u in &spec.uniques {
        lines.push(format!("  UNIQUE ({})", group("unique constraint", u)?));
    }
    let t = D.qualified(&spec.schema, &spec.name);
    let mut out = vec![format!("CREATE TABLE {t} (\n{}\n)", lines.join(",\n"))];
    if !spec.comment.is_empty() {
        out.push(comment_stmt(&format!("TABLE {t}"), &spec.comment)?);
    }
    for c in spec.cols.iter().filter(|c| !c.comment.is_empty()) {
        out.push(comment_stmt(&format!("COLUMN {t}.{}", q(&c.name)), &c.comment)?);
    }
    Ok(out)
}

/// How the structure editor reads a table, as comparable lines: every column and index as the server prints it, in order, plus the
/// constraints and what depends on the table. Two equal lists are the same table for an undo.
fn fingerprint(st: &Structure) -> Vec<String> {
    let cols: Vec<String> = st.columns.iter().map(|c| c.name.clone()).collect();
    let idx: Vec<String> = st.indexes.iter().map(|i| i.name.clone()).collect();
    let mut v = vec![format!("columns {}", cols.join(","))];
    v.extend(snapshot(st, &cols, &idx, &[]).into_iter().map(|(k, x)| format!("{k}={}", x.unwrap_or_default())));
    v.extend(table_state(st));
    v
}

// Reading from the server.

/// The objects that would go with `refobjid` of catalog `refclass` when it is dropped CASCADE, found by walking pg_depend. `$3` lists the
/// dependency kinds the walk starts from: a schema holds its objects by a normal or auto dependency, while a table loses its
/// indexes, constraints and sequences whatever the mode and only the normal ones (views, keys of other tables, functions)
/// make a plain DROP refuse. A view is reached through its rewrite rule, so the rule is mapped to the view at every step.
/// `$4` is an object to leave out of the answer (the table itself).
const DEPENDENTS: &str = "WITH RECURSIVE d(classid, objid) AS (
    SELECT x.classid, x.objid FROM pg_depend p CROSS JOIN LATERAL (SELECT CASE WHEN p.classid = 'pg_rewrite'::regclass THEN 'pg_class'::regclass::oid ELSE p.classid END AS classid, CASE WHEN p.classid = 'pg_rewrite'::regclass THEN (SELECT ev_class FROM pg_rewrite WHERE oid = p.objid) ELSE p.objid END AS objid) x
    WHERE p.refclassid = $1::text::regclass AND p.refobjid = $2::text::oid AND p.deptype::text = ANY (string_to_array($3, ','))
  UNION
    SELECT x.classid, x.objid FROM d JOIN pg_depend p ON p.refclassid = d.classid AND p.refobjid = d.objid AND p.deptype IN ('n', 'a')
    CROSS JOIN LATERAL (SELECT CASE WHEN p.classid = 'pg_rewrite'::regclass THEN 'pg_class'::regclass::oid ELSE p.classid END AS classid, CASE WHEN p.classid = 'pg_rewrite'::regclass THEN (SELECT ev_class FROM pg_rewrite WHERE oid = p.objid) ELSE p.objid END AS objid) x
)
SELECT DISTINCT pg_describe_object(d.classid, d.objid, 0) AS what FROM d
WHERE NOT (d.classid = 'pg_class'::regclass AND d.objid = $4::text::oid)
  AND ((d.classid = 'pg_class'::regclass AND EXISTS (SELECT 1 FROM pg_class c WHERE c.oid = d.objid AND c.relkind IN ('r', 'p', 'v', 'm', 'f', 'S')
         AND NOT EXISTS (SELECT 1 FROM pg_depend o WHERE o.classid = 'pg_class'::regclass AND o.objid = c.oid AND o.refclassid = 'pg_class'::regclass AND o.deptype IN ('a', 'i'))))
    OR d.classid = 'pg_proc'::regclass
    OR (d.classid = 'pg_type'::regclass AND EXISTS (SELECT 1 FROM pg_type t WHERE t.oid = d.objid AND t.typtype IN ('d', 'e', 'r', 'm')))
    OR d.classid IN ('pg_trigger'::regclass, 'pg_policy'::regclass, 'pg_constraint'::regclass))
ORDER BY 1";

/// Anything that is not a system schema, an extension's own, or `public` in a database that was just made: what the undo of a
/// create counts to call the database empty. Relations of every kind but indexes and toast tables, functions, schemas and the
/// standalone types.
const NOT_EMPTY: &str = "SELECT (
    (SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.relkind IN ('r', 'p', 'v', 'm', 'f', 'S', 'c')
       AND n.nspname <> 'information_schema' AND n.nspname !~ '^pg_' AND NOT EXISTS (SELECT 1 FROM pg_depend e WHERE e.classid = 'pg_class'::regclass AND e.objid = c.oid AND e.deptype = 'e'))
  + (SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname <> 'information_schema' AND n.nspname !~ '^pg_'
       AND NOT EXISTS (SELECT 1 FROM pg_depend e WHERE e.classid = 'pg_proc'::regclass AND e.objid = p.oid AND e.deptype = 'e'))
  + (SELECT count(*) FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace WHERE t.typtype IN ('d', 'e', 'r', 'm') AND n.nspname <> 'information_schema' AND n.nspname !~ '^pg_'
       AND NOT EXISTS (SELECT 1 FROM pg_depend e WHERE e.classid = 'pg_type'::regclass AND e.objid = t.oid AND e.deptype = 'e'))
  + (SELECT count(*) FROM pg_namespace n WHERE n.nspname NOT IN ('public', 'information_schema') AND n.nspname !~ '^pg_'
       AND NOT EXISTS (SELECT 1 FROM pg_depend e WHERE e.classid = 'pg_namespace'::regclass AND e.objid = n.oid AND e.deptype = 'e'))
  )::bigint AS n";

/// Runs reads in a transaction that is always rolled back, on a connection of its own, with every value bound.
async fn read(db: &Db, scope: &str, steps: &[(&str, Vec<Param>)]) -> Res<Vec<Vec<SqlRow>>> {
    let mut c = db.acquire_conn().await?;
    let out = async {
        let mut tx = c.begin_write().await.map_err(se)?;
        let mut all = Vec::new();
        let mut res = Ok(());
        for (sql, args) in steps {
            let t = Instant::now();
            let r = tx.fetch_bound(sql, args).await;
            db.note(scope, sql, t, &r);
            match r {
                Ok(rows) => all.push(rows),
                Err(x) => {
                    res = Err(se(x));
                    break;
                }
            }
        }
        let _ = tx.rollback().await;
        res.map(|_| all)
    }
    .await;
    let _ = c.close().await;
    out
}

fn names(rows: &[SqlRow]) -> Vec<String> {
    rows.iter().filter_map(|r| text(r, "what")).collect()
}

/// Every non-template database that can be connected to, read on the connection that ran a statement.
async fn db_list(c: &mut Conn) -> Res<Vec<String>> {
    Ok(c.fetch_all(super::pg::DB_LIST).await.map_err(se)?.iter().filter_map(|r| text(r, "datname")).collect())
}

async fn dependents(db: &Db, scope: &str, class: &str, oid: &str, kinds: &str, skip: &str) -> Res<Vec<String>> {
    let args = vec![Param::Text(class.into()), Param::Text(oid.into()), Param::Text(kinds.into()), Param::Text(skip.into())];
    Ok(names(&read(db, scope, &[(DEPENDENTS, args)]).await?.remove(0)))
}

async fn db_info(db: &Db, name: &str) -> Res<DropInfo> {
    let sql = "SELECT (SELECT count(*) FROM pg_database WHERE datname = $1)::bigint AS found, COALESCE((SELECT datistemplate::int FROM pg_database WHERE datname = $1), 0)::bigint AS tmpl, \
               COALESCE((SELECT pg_database_size(datname) FROM pg_database WHERE datname = $1), 0)::bigint AS size, (SELECT count(*) FROM pg_stat_activity WHERE datname = $1 AND pid <> pg_backend_pid())::bigint AS sessions";
    let rows = read(db, "", &[(sql, vec![Param::Text(name.into())])]).await?.remove(0);
    let r = rows.first().ok_or("The server gave no answer")?;
    let own = matches!(&db.pool, Pool::Pg(p) if p.default_database() == name);
    Ok(DropInfo { exists: num(r, "found").unwrap_or(0) > 0, is_template: num(r, "tmpl").unwrap_or(0) > 0, size: num(r, "size"), sessions: num(r, "sessions"), own_database: own, ..Default::default() })
}

async fn schema_info(db: &Db, schema: &str) -> Res<DropInfo> {
    let found = read(db, schema, &[("SELECT oid::text AS what FROM pg_namespace WHERE nspname = $1", vec![Param::Text(schema.into())])]).await?.remove(0);
    let Some(oid) = found.first().and_then(|r| text(r, "what")) else { return Ok(DropInfo::default()) };
    let all = dependents(db, schema, "pg_namespace", &oid, "n,a", "0").await?;
    Ok(DropInfo { exists: true, dependents_total: all.len() as u64, dependents: all.into_iter().take(LISTED).collect(), ..Default::default() })
}

async fn table_info(db: &Db, schema: &str, table: &str) -> Res<DropInfo> {
    let name = D.qualified(schema, table);
    let sql = "SELECT c.oid::text AS what, c.reltuples::bigint AS est FROM pg_class c WHERE c.oid = to_regclass($1)";
    let found = read(db, schema, &[(sql, vec![Param::Text(name.clone())])]).await?.remove(0);
    let Some(oid) = found.first().and_then(|r| text(r, "what")) else { return Ok(DropInfo::default()) };
    let est = found.first().and_then(|r| r.int("est")).filter(|n| *n >= 0).map(|n| n as u64);
    let all = dependents(db, schema, "pg_class", &oid, "n", &oid).await?;
    let mut info = DropInfo { exists: true, rows: est, rows_estimated: true, dependents_total: all.len() as u64, dependents: all.into_iter().take(LISTED).collect(), ..Default::default() };
    // An exact count within a short wait, else the estimate stays and the dialog says "about".
    let count = format!("SELECT count(*)::bigint AS n FROM {name}");
    let mut c = db.acquire_conn().await?;
    let exact = async {
        let mut tx = c.begin_write().await.ok()?;
        let r = async {
            tx.exec_bound("SET LOCAL statement_timeout = '2s'", &[]).await.ok()?;
            tx.fetch_bound(&count, &[]).await.ok()?.first().and_then(|r| num(r, "n"))
        }
        .await;
        let _ = tx.rollback().await;
        r
    }
    .await;
    let _ = c.close().await;
    if let Some(n) = exact {
        (info.rows, info.rows_estimated) = (Some(n), false);
    }
    Ok(info)
}

// Statements that run alone.

/// A connection for a statement PostgreSQL will not run inside a transaction block. It comes from the pool of the connection's own
/// database and is closed afterwards, so the session changes made here never reach a pooled connection. Quarry's sessions start
/// read-only and with a statement timeout, and copying a big template takes longer than that. Behind a pooler nothing can be set
/// on a session, and its backends are not read-only to begin with.
async fn maintenance(db: &Db, read_only: bool) -> Res<Conn> {
    if read_only {
        return Err(LOCKED.into());
    }
    let mut c = db.acquire_conn().await?;
    if !db.pooler() {
        if let Err(x) = c.execute("SET default_transaction_read_only = off; SET statement_timeout = 0; SET lock_timeout = '10s'").await {
            let _ = c.close().await;
            return Err(se(x));
        }
    }
    Ok(c)
}

async fn alone(c: &mut Conn, db: &Db, sql: &str) -> Result<(), sqlx::Error> {
    let t = Instant::now();
    let r = c.execute(sql).await;
    db.note("", sql, t, &r);
    r.map(|_| ())
}

/// Another session on the template or on the database: the server's own words, with what Quarry did about it.
const IN_USE: &str = "55006";

async fn create_db(db: &Db, name: &str, template: Option<&str>, read_only: bool) -> Res<Vec<String>> {
    let mut c = maintenance(db, read_only).await?;
    let out = async {
        let templates = template_list(&mut c).await?;
        let sql = create_db_sql(name, template, &templates)?;
        match alone(&mut c, db, &sql).await {
            Ok(()) => {}
            // Two CREATE DATABASE at once both want template1 to themselves.
            Err(x) if sql_state(&x).as_deref() == Some(IN_USE) => return Err(format!("{}\nAnother session is using the template database, and a CREATE DATABASE running at the same moment does. Try again in a moment.", se(x))),
            Err(x) => return Err(se(x)),
        }
        db_list(&mut c).await
    }
    .await;
    let _ = c.close().await;
    out
}

async fn template_list(c: &mut Conn) -> Res<Vec<String>> {
    Ok(c.fetch_all("SELECT datname::text AS datname FROM pg_database WHERE datistemplate ORDER BY 1").await.map_err(se)?.iter().filter_map(|r| text(r, "datname")).collect())
}

async fn drop_db(db: &Db, name: &str, force: bool, read_only: bool) -> Res<Vec<String>> {
    let sql = drop_db_sql(name, force)?;
    let Pool::Pg(pools) = &db.pool else { return Err("Not a PostgreSQL connection".into()) };
    // The connection's own database holds Quarry's maintenance pool, and PostgreSQL cannot drop the database it is working on.
    if pools.default_database() == name {
        return Err(format!("This connection opens on {name}, so it cannot drop it from here. Connect with another database in the address (postgres, say), then drop {name}."));
    }
    let mut c = maintenance(db, read_only).await?;
    let out = async {
        // Quarry's own connections go first: an idle one in a pool is a session on the database, and the server refuses the drop for it.
        pools.close_database(name).await;
        let mut r = alone(&mut c, db, &sql).await;
        if !force && r.as_ref().err().and_then(sql_state).as_deref() == Some(IN_USE) {
            // A read that was already on its way when the pool closed can make a new one before the drop runs. Once more, then the server's answer stands.
            pools.close_database(name).await;
            tokio::time::sleep(Duration::from_millis(250)).await;
            r = alone(&mut c, db, &sql).await;
        }
        match r {
            Ok(()) => {}
            Err(x) if sql_state(&x).as_deref() == Some(IN_USE) => {
                let who = if db.pooler() { "A connection pooler keeps a server connection open on it after Quarry's own are closed, or another session is using it" } else { "Another session or application is using it" };
                return Err(format!("{}\nQuarry closed its own connections to {name} first. {who}. Nothing was dropped. WITH (FORCE) in the confirmation disconnects them.", se(x)));
            }
            Err(x) => return Err(se(x)),
        }
        db_list(&mut c).await
    }
    .await;
    let _ = c.close().await;
    out
}

// One transaction.

async fn begin<'c>(c: &'c mut Conn, db: &Db, scope: &str) -> Res<Tx<'c>> {
    let mut tx = c.begin_write().await.map_err(se)?;
    exec(&mut tx, db, scope, &format!("SET LOCAL lock_timeout = '{LOCK_WAIT}'")).await.map_err(se)?;
    Ok(tx)
}

/// COMMIT, or ROLLBACK when `out` failed. A failed COMMIT does not say that nothing changed, so it says so.
async fn finish<T>(tx: Tx<'_>, out: Res<T>) -> Res<T> {
    match out {
        Ok(v) => tx.commit().await.map(|_| v).map_err(|x| format!("{}. The server did not confirm the commit, so the change may or may not have been applied. Refresh the lists and check before running it again.", se(x))),
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e)
        }
    }
}

async fn namespace_oid(tx: &mut Tx<'_>, schema: &str) -> Res<Option<String>> {
    let rows = tx.fetch_bound("SELECT oid::text AS n FROM pg_namespace WHERE nspname = $1", &[Param::Text(schema.into())]).await.map_err(se)?;
    Ok(rows.first().and_then(|r| text(r, "n")))
}

async fn relation_oid(tx: &mut Tx<'_>, name: &str) -> Res<Option<String>> {
    let rows = tx.fetch_bound("SELECT to_regclass($1)::oid::text AS n", &[Param::Text(name.into())]).await.map_err(se)?;
    Ok(rows.first().and_then(|r| text(r, "n")))
}

async fn create_schema(db: &Db, name: &str, read_only: bool) -> Res<String> {
    let sql = create_schema_sql(name)?;
    let mut c = write::writer_conn(db, "", read_only).await?;
    let out = async {
        let mut tx = begin(&mut c, db, name).await?;
        let r = async {
            exec(&mut tx, db, name, &sql).await.map_err(se)?;
            namespace_oid(&mut tx, name).await?.ok_or_else(|| "The schema was made but cannot be read back.".to_string())
        }
        .await;
        finish(tx, r).await
    }
    .await;
    let _ = c.close().await;
    out
}

async fn drop_schema(db: &Db, name: &str, cascade: bool, read_only: bool) -> Res<()> {
    let sql = drop_schema_sql(name, cascade)?;
    let mut c = write::writer_conn(db, "", read_only).await?;
    let out = async {
        let mut tx = begin(&mut c, db, name).await?;
        let r = exec(&mut tx, db, name, &sql).await.map_err(|x| match (sql_state(&x).as_deref(), lock_error(&x, name)) {
            (_, Some(why)) => why,
            // 2BP01 is dependent_objects_still_exist.
            (Some("2BP01"), _) => format!("{}\nTick CASCADE in the confirmation to drop what is in {name} with it. Nothing was dropped.", se(x)),
            _ => se(x),
        });
        finish(tx, r).await
    }
    .await;
    let _ = c.close().await;
    out
}

/// What a create made, for the undo: the table's oid and how it reads right after.
struct Made {
    oid: String,
    state: Vec<String>,
}

async fn create_table(db: &Db, spec: &TableSpec, read_only: bool) -> Res<Made> {
    let stmts = create_table_stmts(spec)?;
    let mut c = write::writer_conn(db, "", read_only).await?;
    let out = async {
        let mut tx = begin(&mut c, db, &spec.schema).await?;
        let r = async {
            // The types and the default expressions resolve under the schema's own path, as they will when the structure editor reads the table back.
            exec(&mut tx, db, &spec.schema, &format!("SET LOCAL search_path = {}", q(&spec.schema))).await.map_err(se)?;
            for col in &spec.cols {
                let said = tx.fetch_bound("SELECT to_regtype($1)::text AS t", &[Param::Text(col.ty.trim().to_string())]).await;
                // PostgreSQL before 16 raises an error for text that is not a type name, later ones answer NULL.
                if said.ok().and_then(|rows| rows.first().and_then(|r| text(r, "t"))).is_none() {
                    return Err(format!("Column {}: {:?} is not a type the server knows. Write only the type, like numeric(10,2), without constraints or other column clauses. Nothing was created.", col.name, col.ty));
                }
            }
            for s in &stmts {
                exec(&mut tx, db, &spec.schema, s).await.map_err(|x| lock_error(&x, &spec.name).unwrap_or_else(|| se(x)))?;
            }
            let st = read_in(&mut tx, &spec.schema, &spec.name).await?;
            let oid = relation_oid(&mut tx, &D.qualified(&spec.schema, &spec.name)).await?.ok_or("The table was made but cannot be read back.")?;
            Ok(Made { oid, state: fingerprint(&st) })
        }
        .await;
        finish(tx, r).await
    }
    .await;
    let _ = c.close().await;
    out
}

async fn drop_table(db: &Db, schema: &str, table: &str, view: bool, cascade: bool, read_only: bool) -> Res<()> {
    let sql = drop_table_sql(schema, table, view, cascade)?;
    let mut c = write::writer_conn(db, "", read_only).await?;
    let out = async {
        let mut tx = begin(&mut c, db, schema).await?;
        let r = exec(&mut tx, db, schema, &sql).await.map_err(|x| match (sql_state(&x).as_deref(), lock_error(&x, table)) {
            (_, Some(why)) => why,
            (Some("2BP01"), _) => format!("{}\nTick CASCADE in the confirmation to drop what depends on it too. Nothing was dropped.", se(x)),
            _ => se(x),
        });
        finish(tx, r).await
    }
    .await;
    let _ = c.close().await;
    out
}

// Undo of a create.

async fn undo_schema(db: &Db, name: &str, oid: &str, read_only: bool) -> Res<()> {
    let sql = drop_schema_sql(name, false)?;
    let mut c = write::writer_conn(db, "", read_only).await?;
    let out = async {
        let mut tx = begin(&mut c, db, name).await?;
        let r = async {
            match namespace_oid(&mut tx, name).await? {
                None => return Err(format!("Not undone: the schema {name} is already gone.")),
                Some(now) if now != oid => return Err(format!("Not undone: {name} was dropped and made again since, so it is not the schema this created.")),
                Some(_) => {}
            }
            // Without CASCADE the server refuses a schema that holds anything, inside this transaction, so no object can arrive between a check and the drop.
            exec(&mut tx, db, name, &sql).await.map_err(|x| match (sql_state(&x).as_deref(), lock_error(&x, name)) {
                (_, Some(why)) => format!("Not undone: {why}"),
                (Some("2BP01"), _) => format!("Not undone: {name} has objects in it now.\n{}", se(x)),
                _ => se(x),
            })
        }
        .await;
        finish(tx, r).await
    }
    .await;
    let _ = c.close().await;
    out
}

async fn undo_table(db: &Db, schema: &str, table: &str, made: &Made, read_only: bool) -> Res<()> {
    let sql = drop_table_sql(schema, table, false, false)?;
    let name = D.qualified(schema, table);
    let mut c = write::writer_conn(db, "", read_only).await?;
    let out = async {
        let mut tx = begin(&mut c, db, schema).await?;
        let r = async {
            // Locked before anything is read, so a row cannot arrive between the check and the drop. A role without the privilege for the explicit lock skips it, and the DROP takes its own.
            if may_lock(&mut tx, &name).await {
                if let Err(x) = exec(&mut tx, db, schema, &format!("LOCK TABLE {name} IN ACCESS EXCLUSIVE MODE")).await {
                    return Err(match (sql_state(&x).as_deref(), lock_error(&x, table)) {
                        (_, Some(why)) => format!("Not undone: {why}"),
                        (Some("42P01"), _) => format!("Not undone: the table {table} is already gone."),
                        _ => se(x),
                    });
                }
            }
            match relation_oid(&mut tx, &name).await? {
                None => return Err(format!("Not undone: the table {table} is already gone.")),
                Some(now) if now != made.oid => return Err(format!("Not undone: {table} was dropped and made again since, so it is not the table this created.")),
                Some(_) => {}
            }
            exec(&mut tx, db, schema, &format!("SET LOCAL search_path = {}", q(schema))).await.map_err(se)?;
            let st = read_in(&mut tx, schema, table).await?;
            if fingerprint(&st) != made.state {
                return Err(format!("Not undone: the structure of {table} changed since it was made, so dropping it would lose more than the create made."));
            }
            if !tx.fetch_bound(&format!("SELECT 1 AS n FROM {name} LIMIT 1"), &[]).await.map_err(se)?.is_empty() {
                return Err(format!("Not undone: {table} has rows now."));
            }
            exec(&mut tx, db, schema, &sql).await.map_err(|x| match sql_state(&x).as_deref() {
                // Another table's key or a view was made on it since.
                Some("2BP01") => format!("Not undone: something depends on {table} now.\n{}", se(x)),
                _ => se(x),
            })
        }
        .await;
        finish(tx, r).await
    }
    .await;
    let _ = c.close().await;
    out
}

async fn undo_db(db: &Db, name: &str, read_only: bool) -> Res<Vec<String>> {
    let sql = drop_db_sql(name, false)?;
    let Pool::Pg(pools) = &db.pool else { return Err("Not a PostgreSQL connection".into()) };
    let mut c = maintenance(db, read_only).await?;
    let out = async {
        let at = db.at(name);
        let n = read(&at, "", &[(NOT_EMPTY, Vec::new())]).await.map_err(|e| format!("Not undone: {name} could not be checked ({e})"))?.remove(0);
        if n.first().and_then(|r| num(r, "n")).unwrap_or(1) > 0 {
            return Err(format!("Not undone: {name} has tables or other objects in it now."));
        }
        // The check connected to the database, and so did anything else of Quarry's. A session of someone else makes the drop refuse, which is the other half of the promise.
        pools.close_database(name).await;
        match alone(&mut c, db, &sql).await {
            Ok(()) => {}
            Err(x) if sql_state(&x).as_deref() == Some(IN_USE) => return Err(format!("Not undone: another session is connected to {name}.\n{}", se(x))),
            Err(x) => return Err(se(x)),
        }
        db_list(&mut c).await
    }
    .await;
    let _ = c.close().await;
    out
}

fn entry(db: &Db, conn: u64, schema: &str, table: &str, undo: SqlUndo) -> UndoAction {
    UndoAction { at: Instant::now(), conn, db: schema.into(), coll: table.into(), kind: UndoKind::Sql { database: db.pg_database(), cols: Vec::new(), undo } }
}

fn answer(res: Res<Option<Vec<String>>>, said: String) -> (Res<String>, Option<Vec<String>>) {
    match res {
        Ok(dbs) => (Ok(said), dbs),
        Err(err) => (Err(err), None),
    }
}

impl Worker {
    /// The template databases the New database dialog offers.
    pub fn pg_templates(&self, conn: u64, db: Db) {
        self.spawn_sql(db.conn, "", async move {
            let res = async {
                let mut c = db.acquire_conn().await?;
                let out = template_list(&mut c).await;
                let _ = c.close().await;
                out
            }
            .await;
            Msg::PgTemplates { conn, res }
        });
    }

    /// What dropping a database (`schema` and `table` empty), a schema (`table` empty) or a table or view will take, read from
    /// the database `database` where it lives.
    pub fn pg_drop_info(&self, conn: u64, db: Db, database: String, schema: String, table: String) {
        self.spawn_sql(db.conn, &schema.clone(), async move {
            let at = db.at(&database);
            let res = match (schema.is_empty(), table.is_empty()) {
                (true, true) => db_info(&db, &database).await,
                (false, true) => schema_info(&at, &schema).await,
                _ => table_info(&at, &schema, &table).await,
            };
            Msg::SqlDropInfo { conn, database, schema, table, res }
        });
    }

    pub fn pg_create_db(&self, conn: u64, db: Db, name: String, template: Option<String>, read_only: bool) {
        self.spawn_sql(db.conn, "", async move {
            let res = create_db(&db, &name, template.as_deref(), read_only).await;
            let undo = res.is_ok().then(|| entry(&db, conn, &name, "", SqlUndo::CreatedDb));
            let (res, dbs) = answer(res.map(Some), format!("Created database {name}"));
            Msg::SqlDdl { conn, database: String::new(), schema: name, table: String::new(), what: Ddl::CreatedDb, res, undo, dbs, sql_mode: None }
        });
    }

    /// DROP DATABASE, after Quarry's own connections to it are closed. `force` is WITH (FORCE), which disconnects the other sessions.
    pub fn pg_drop_db(&self, conn: u64, db: Db, name: String, force: bool, read_only: bool) {
        self.spawn_sql(db.conn, "", async move {
            let res = drop_db(&db, &name, force, read_only).await;
            let (res, dbs) = answer(res.map(Some), format!("Dropped database {name}"));
            Msg::SqlDdl { conn, database: String::new(), schema: name, table: String::new(), what: Ddl::DroppedDb, res, undo: None, dbs, sql_mode: None }
        });
    }

    /// `db` points at the database that gets the schema.
    pub fn pg_create_schema(&self, conn: u64, db: Db, name: String, read_only: bool) {
        self.spawn_sql(db.conn, &name.clone(), async move {
            let res = create_schema(&db, &name, read_only).await;
            let undo = res.as_ref().ok().map(|oid| entry(&db, conn, &name, "", SqlUndo::CreatedSchema { oid: oid.clone() }));
            let (res, _) = answer(res.map(|_| None), format!("Created schema {name}"));
            Msg::SqlDdl { conn, database: db.pg_database(), schema: name, table: String::new(), what: Ddl::CreatedSchema, res, undo, dbs: None, sql_mode: None }
        });
    }

    pub fn pg_drop_schema(&self, conn: u64, db: Db, name: String, cascade: bool, read_only: bool) {
        self.spawn_sql(db.conn, &name.clone(), async move {
            let res = drop_schema(&db, &name, cascade, read_only).await;
            let (res, _) = answer(res.map(|_| None), format!("Dropped schema {name}"));
            Msg::SqlDdl { conn, database: db.pg_database(), schema: name, table: String::new(), what: Ddl::DroppedSchema, res, undo: None, dbs: None, sql_mode: None }
        });
    }

    pub fn pg_create_table(&self, conn: u64, db: Db, spec: TableSpec, read_only: bool) {
        self.spawn_sql(db.conn, &spec.schema.clone(), async move {
            let res = create_table(&db, &spec, read_only).await;
            let undo = res.as_ref().ok().map(|m| entry(&db, conn, &spec.schema, &spec.name, SqlUndo::PgCreatedTable { oid: m.oid.clone(), state: m.state.clone() }));
            let (res, _) = answer(res.map(|_| None), format!("Created table {}", spec.name));
            Msg::SqlDdl { conn, database: db.pg_database(), schema: spec.schema, table: spec.name, what: Ddl::CreatedTable, res, undo, dbs: None, sql_mode: None }
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub fn pg_drop_table(&self, conn: u64, db: Db, schema: String, table: String, view: bool, cascade: bool, read_only: bool) {
        self.spawn_sql(db.conn, &schema.clone(), async move {
            let res = drop_table(&db, &schema, &table, view, cascade, read_only).await;
            let (res, _) = answer(res.map(|_| None), format!("Dropped {} {table}", if view { "view" } else { "table" }));
            Msg::SqlDdl { conn, database: db.pg_database(), schema, table, what: Ddl::DroppedTable, res, undo: None, dbs: None, sql_mode: None }
        });
    }
}

/// Undoes a create of a database, schema or table (`sql_undo_create` hands PostgreSQL's here). Only an empty, unchanged object goes. A
/// refused undo hands the entry back, so it stays in the list with the reason.
pub(super) async fn undo(db: Db, a: UndoAction, read_only: bool) -> Msg {
    let UndoAction { at, conn, db: schema, coll: table, kind } = a;
    let UndoKind::Sql { database, cols, undo } = kind else { return Msg::SqlDdl { conn, database: String::new(), schema, table, what: Ddl::DroppedTable, res: Err("Not a create".into()), undo: None, dbs: None, sql_mode: None } };
    let (what, res, dbs, said) = match &undo {
        SqlUndo::CreatedDb => {
            let r = undo_db(&db, &schema, read_only).await;
            (Ddl::DroppedDb, r.as_ref().map(|_| ()).map_err(Clone::clone), r.ok(), format!("Removed database {schema}"))
        }
        SqlUndo::CreatedSchema { oid } => (Ddl::DroppedSchema, undo_schema(&db, &schema, oid, read_only).await, None, format!("Removed schema {schema}")),
        SqlUndo::PgCreatedTable { oid, state } => (Ddl::DroppedTable, undo_table(&db, &schema, &table, &Made { oid: oid.clone(), state: state.clone() }, read_only).await, None, format!("Removed table {table}")),
        _ => (Ddl::DroppedTable, Err("Not a create".into()), None, String::new()),
    };
    let kept = res.is_err().then(|| UndoAction { at, conn, db: schema.clone(), coll: table.clone(), kind: UndoKind::Sql { database: database.clone(), cols, undo } });
    // A refused undo changes nothing, so the app has nothing to refresh.
    Msg::SqlDdl { conn, database, schema, table, what, res: res.map(|_| said), undo: kept, dbs, sql_mode: None }
}
