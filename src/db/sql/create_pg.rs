//! Creating and dropping PostgreSQL databases, schemas and tables. Every name is quoted, and the only user text besides names
//! is a column type and a default expression, which `ddl_pg` checks the way the structure editor does. Where MySQL's DDL
//! commits by itself, PostgreSQL's is transactional, so a schema or a table is made and dropped inside one transaction
//! and an undo checks and drops in the same one. A database is the exception: CREATE and DROP DATABASE refuse to run in a
//! transaction block, so they run alone on a connection of their own taken from the pool of the connection's own database
//! (never from the pool of the database being dropped), and the undo of a create checks that the database is empty a
//! round trip before it drops it, which no lock can close.

use super::create::{Ddl, DropInfo};
use super::ddl::{snapshot, ColDef};
use super::ddl_pg::{col_body, comment_stmt, exec, lock_error, may_lock_one, name_ok, q, read_in, run_locked, set_local, set_lock_wait, set_path, table_state, unknown_type, Fail};
use super::exec::{sql_state, Cells, Conn, SqlRow, Tx};
use sqlx::Connection as _;
use super::structure::{num, Structure};
use super::value::Param;
use super::worker::text;
use super::write::{self, SqlUndo};
use super::pg::{LOCK_WAIT_SECS, WRITE_LOCK_SECS};
use super::{se, Db, Dialect, Pool};
use crate::db::errors::{in_use, lock_held_cascade, InUse};
use crate::db::{Msg, Res, UndoAction, UndoKind, Worker};
use std::collections::{HashMap, HashSet};
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
pub(super) const LISTED: usize = 20;

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

/// What the structure editor does not read but a person can add to a table: row security and its policies, triggers and the table's
/// comment. A table with any of these is more than the create made.
const EXTRAS: &str = "SELECT what FROM (
    SELECT 'rls ' || c.relrowsecurity::text || ' ' || c.relforcerowsecurity::text AS what FROM pg_class c WHERE c.oid = to_regclass($1)
  UNION ALL SELECT 'policy ' || p.polname::text || ' ' || p.polcmd::text || ' ' || p.polpermissive::text || ' ' || p.polroles::text || ' ' || COALESCE(pg_get_expr(p.polqual, p.polrelid), '') || ' ' || COALESCE(pg_get_expr(p.polwithcheck, p.polrelid), '') FROM pg_policy p WHERE p.polrelid = to_regclass($1)
  UNION ALL SELECT 'trigger ' || pg_get_triggerdef(t.oid) FROM pg_trigger t WHERE t.tgrelid = to_regclass($1) AND NOT t.tgisinternal
  UNION ALL SELECT 'comment ' || obj_description(to_regclass($1), 'pg_class')::text WHERE obj_description(to_regclass($1), 'pg_class') IS NOT NULL
) x ORDER BY 1";

/// The whole fingerprint of a table, read inside the transaction that makes or drops it.
async fn state_in(tx: &mut Tx<'_>, schema: &str, table: &str) -> Res<Vec<String>> {
    let st = read_in(tx, schema, table).await?;
    let mut v = fingerprint(&st);
    let extras = tx.fetch_bound(EXTRAS, &[Param::Text(D.qualified(schema, table))]).await.map_err(se)?;
    v.extend(extras.iter().filter_map(|r| text(r, "what")));
    Ok(v)
}

// Reading from the server.

/// The first oid PostgreSQL hands to an object a user made (FirstNormalObjectId). Everything below it is the server's own.
const FIRST_USER_OID: u32 = 16384;

/// Catalogs whose rows are objects with an oid of their own, as `(catalog, namespace column)`. The snapshot lists the rows of
/// each that a user made, so a drop's effect is read off the server instead of rebuilt from pg_depend's rules.
const OBJECT_CATALOGS: [(&str, &str); 15] = [
    ("pg_proc", "pronamespace"),
    ("pg_operator", "oprnamespace"),
    ("pg_opclass", "opcnamespace"),
    ("pg_opfamily", "opfnamespace"),
    ("pg_collation", "collnamespace"),
    ("pg_conversion", "connamespace"),
    ("pg_ts_config", "cfgnamespace"),
    ("pg_ts_dict", "dictnamespace"),
    ("pg_ts_parser", "prsnamespace"),
    ("pg_ts_template", "tmplnamespace"),
    ("pg_extension", "extnamespace"),
    ("pg_cast", ""),
    ("pg_foreign_data_wrapper", ""),
    ("pg_foreign_server", ""),
    ("pg_event_trigger", ""),
];

/// Every user object the server describes, as `key, what, owner, nsp, est, qname`: the key names the object, `what` is the server's
/// own description of it, `owner` is the key of the object it is a part of (an index of its table, a default of its table), `nsp` is
/// its schema, and `est` and `qname` are set for tables so a count can be taken. A column is listed only when something
/// depends on it, which is the only way a drop can take a column without its table.
static SNAPSHOT: std::sync::LazyLock<String> = std::sync::LazyLock::new(snapshot_sql);

fn snapshot_sql() -> String {
    let first = FIRST_USER_OID;
    let ext = |cat: &str, a: &str| format!("(SELECT 'pg_extension:' || e.refobjid FROM pg_depend e WHERE e.classid = '{cat}'::regclass AND e.objid = {a}.oid AND e.deptype = 'e' LIMIT 1)");
    let rel_ns = |col: &str| format!("(SELECT r.relnamespace::text FROM pg_class r WHERE r.oid = {col})");
    let mut parts = vec![
        format!(
            "SELECT 'pg_class:' || c.oid AS key, pg_describe_object('pg_class'::regclass, c.oid, 0) AS what,
               COALESCE(CASE WHEN c.relkind = 't' THEN (SELECT 'pg_class:' || x.oid FROM pg_class x WHERE x.reltoastrelid = c.oid LIMIT 1)
                             WHEN c.relkind IN ('i', 'I') THEN COALESCE((SELECT 'pg_constraint:' || k.oid FROM pg_constraint k WHERE k.conindid = c.oid AND k.contype IN ('p', 'u', 'x') AND k.conrelid = i.indrelid LIMIT 1), 'pg_class:' || i.indrelid)
                             WHEN c.relkind = 'S' THEN (SELECT 'pg_class:' || d.refobjid FROM pg_depend d WHERE d.classid = 'pg_class'::regclass AND d.objid = c.oid AND d.refclassid = 'pg_class'::regclass AND d.deptype IN ('a', 'i') LIMIT 1) END, {}) AS owner,
               c.relnamespace::text AS nsp, CASE WHEN c.relkind IN ('r', 'm') THEN c.reltuples::bigint::text END AS est,
               CASE WHEN c.relkind IN ('r', 'm') THEN quote_ident(n.nspname) || '.' || quote_ident(c.relname) END AS qname
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace LEFT JOIN pg_index i ON i.indexrelid = c.oid
             WHERE c.oid >= {first} AND c.relkind IN ('r', 'p', 'v', 'm', 'f', 'S', 'c', 'i', 'I', 't')",
            ext("pg_class", "c")
        ),
        format!("SELECT 'pg_type:' || t.oid, pg_describe_object('pg_type'::regclass, t.oid, 0), {}, t.typnamespace::text, NULL::text, NULL::text FROM pg_type t WHERE t.oid >= {first} AND t.typtype IN ('d', 'e', 'r', 'b') AND t.typcategory <> 'A'", ext("pg_type", "t")),
        format!("SELECT 'pg_constraint:' || k.oid, pg_describe_object('pg_constraint'::regclass, k.oid, 0), CASE WHEN k.conrelid <> 0 THEN 'pg_class:' || k.conrelid ELSE 'pg_type:' || k.contypid END, COALESCE({}, k.connamespace::text), NULL::text, NULL::text FROM pg_constraint k WHERE k.oid >= {first} AND k.contype <> 'n'", rel_ns("k.conrelid")),
        format!("SELECT 'pg_attrdef:' || a.oid, pg_describe_object('pg_attrdef'::regclass, a.oid, 0), 'pg_class:' || a.adrelid, {}, NULL::text, NULL::text FROM pg_attrdef a WHERE a.oid >= {first}", rel_ns("a.adrelid")),
        format!("SELECT 'pg_attribute:' || a.attrelid || ':' || a.attnum, pg_describe_object('pg_class'::regclass, a.attrelid, a.attnum), 'pg_class:' || a.attrelid, c.relnamespace::text, NULL::text, NULL::text FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid
             WHERE a.attrelid >= {first} AND a.attnum > 0 AND NOT a.attisdropped AND c.relkind IN ('r', 'p', 'v', 'm', 'f', 'c') AND EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_class'::regclass AND d.objid = a.attrelid AND d.objsubid = a.attnum)"),
        format!("SELECT 'pg_trigger:' || t.oid, pg_describe_object('pg_trigger'::regclass, t.oid, 0), 'pg_class:' || t.tgrelid, {}, NULL::text, NULL::text FROM pg_trigger t WHERE t.oid >= {first} AND NOT t.tgisinternal", rel_ns("t.tgrelid")),
        format!("SELECT 'pg_policy:' || p.oid, pg_describe_object('pg_policy'::regclass, p.oid, 0), 'pg_class:' || p.polrelid, {}, NULL::text, NULL::text FROM pg_policy p WHERE p.oid >= {first}", rel_ns("p.polrelid")),
        format!("SELECT 'pg_rewrite:' || w.oid, pg_describe_object('pg_rewrite'::regclass, w.oid, 0), 'pg_class:' || w.ev_class, {}, NULL::text, NULL::text FROM pg_rewrite w WHERE w.oid >= {first}", rel_ns("w.ev_class")),
        format!("SELECT 'pg_statistic_ext:' || x.oid, pg_describe_object('pg_statistic_ext'::regclass, x.oid, 0), 'pg_class:' || x.stxrelid, x.stxnamespace::text, NULL::text, NULL::text FROM pg_statistic_ext x WHERE x.oid >= {first}"),
    ];
    for (cat, ns) in OBJECT_CATALOGS {
        let nsp = if ns.is_empty() { "NULL::text".to_string() } else { format!("x.{ns}::text") };
        parts.push(format!("SELECT '{cat}:' || x.oid, pg_describe_object('{cat}'::regclass, x.oid, 0), {}, {nsp}, NULL::text, NULL::text FROM {cat} x WHERE x.oid >= {first}", ext(cat, "x")));
    }
    parts.join("\nUNION ALL\n")
}

/// Counts what a database holds that its template did not give it, in every catalog an object of a user can be in, plus large
/// objects. Whatever the server created itself has an oid below `FIRST_USER_OID`, and the undo of a create compares this count with
/// the one read right after the create, so objects the template carried (an extension installed in template1) do not count.
static NOT_EMPTY: std::sync::LazyLock<String> = std::sync::LazyLock::new(not_empty_sql);

fn not_empty_sql() -> String {
    let first = FIRST_USER_OID;
    let mut terms = vec![format!("(SELECT count(*) FROM pg_class WHERE oid >= {first} AND relkind <> 't')"), "(SELECT count(*) FROM pg_largeobject_metadata)".to_string()];
    for cat in [
        "pg_namespace", "pg_proc", "pg_type", "pg_constraint", "pg_trigger", "pg_policy", "pg_rewrite", "pg_attrdef", "pg_extension", "pg_publication", "pg_foreign_data_wrapper", "pg_foreign_server",
        "pg_event_trigger", "pg_operator", "pg_collation", "pg_conversion", "pg_ts_config", "pg_ts_dict", "pg_ts_parser", "pg_ts_template", "pg_cast", "pg_opclass", "pg_opfamily", "pg_language", "pg_transform",
        "pg_default_acl", "pg_statistic_ext",
    ] {
        terms.push(format!("(SELECT count(*) FROM {cat} WHERE oid >= {first})"));
    }
    format!("SELECT ({})::bigint AS n", terms.join(" + "))
}

/// Runs reads in a `BEGIN READ ONLY` that is always rolled back, on a connection of its own, with every value bound.
pub(super) async fn read(db: &Db, scope: &str, steps: &[(&str, Vec<Param>)]) -> Res<Vec<Vec<SqlRow>>> {
    let mut c = db.acquire_conn().await?;
    let out = read_on(&mut c, db, scope, steps).await;
    let _ = c.close().await;
    out
}

async fn read_on(c: &mut Conn, db: &Db, scope: &str, steps: &[(&str, Vec<Param>)]) -> Res<Vec<Vec<SqlRow>>> {
    let Conn::Pg(pc) = c else { return Err("Not a PostgreSQL connection".into()) };
    let mut tx = Tx::Pg(pc.begin_with(db.pg_begin("BEGIN READ ONLY", "")).await.map_err(se)?);
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

/// Every non-template database that can be connected to, read on the connection that ran a statement.
async fn db_list(c: &mut Conn) -> Res<Vec<String>> {
    Ok(c.fetch_all(super::pg::DB_LIST).await.map_err(se)?.iter().filter_map(|r| text(r, "datname")).collect())
}

async fn db_info(db: &Db, name: &str) -> Res<DropInfo> {
    let sql = "SELECT (SELECT count(*) FROM pg_database WHERE datname = $1)::bigint AS found, COALESCE((SELECT datistemplate::int FROM pg_database WHERE datname = $1), 0)::bigint AS tmpl, \
               COALESCE((SELECT pg_database_size(datname) FROM pg_database WHERE datname = $1), 0)::bigint AS size, (SELECT count(*) FROM pg_stat_activity WHERE datname = $1 AND pid <> pg_backend_pid())::bigint AS sessions";
    let rows = read(db, "", &[(sql, vec![Param::Text(name.into())])]).await?.remove(0);
    let r = rows.first().ok_or("The server gave no answer")?;
    let own = matches!(&db.pool, Pool::Pg(p) if p.default_database() == name);
    Ok(DropInfo { exists: num(r, "found").unwrap_or(0) > 0, is_template: num(r, "tmpl").unwrap_or(0) > 0, size: num(r, "size"), sessions: num(r, "sessions"), own_database: own, ..Default::default() })
}

/// One object of the snapshot.
struct Obj {
    what: String,
    owner: Option<String>,
    nsp: Option<String>,
    est: Option<i64>,
    qname: Option<String>,
}

async fn snapshot_objects(tx: &mut Tx<'_>) -> Res<HashMap<String, Obj>> {
    let rows = tx.fetch_bound(&SNAPSHOT, &[]).await.map_err(se)?;
    // pg_describe_object looks the object up with the latest catalog state and answers NULL for one another session has dropped since this
    // transaction's snapshot, while the row of it is still in the snapshot. The object is still counted as there, or it would look dropped by us.
    Ok(rows.iter().filter_map(|r| Some((text(r, "key")?, Obj { what: text(r, "what").or_else(|| text(r, "key"))?, owner: text(r, "owner"), nsp: text(r, "nsp"), est: text(r, "est").and_then(|n| n.parse().ok()), qname: text(r, "qname") }))).collect())
}

fn rows_word(n: i64) -> String {
    format!("{n} row{}", if n == 1 { "" } else { "s" })
}

/// What `drop_sql` (a DROP ... CASCADE) takes, read off the server: the user objects are described before and after the real
/// statement runs inside a transaction that is always rolled back, and the difference is what CASCADE drops. Nothing of
/// PostgreSQL's dependency rules is rebuilt here, so a rule the server has and this lacks cannot leave something out of what the
/// person agrees to. An object that is only part of another dropped one (an index of its table) is folded into it, the object
/// `skip` is the target itself, and `schema_oid` marks the objects of a schema being dropped, so the ones outside it come first.
/// Tables name their rows. The lines are returned with the full count.
async fn cascade_effects(db: &Db, scope: &str, drop_sql: &str, skip: Option<&str>, schema_oid: Option<&str>, read_only: bool) -> Res<(u64, Vec<String>)> {
    // The statement really runs, in a transaction that is rolled back, so a locked connection must not get as far as a READ WRITE one.
    let mut c = write::writer_conn(db, "", read_only).await?;
    let mut out = Err(String::new());
    // A session that drops or changes an object while this reads makes the server answer "cache lookup failed" (XX000) or a
    // serialization failure (40001), which says nothing about this drop, so the reading starts over a few times, a little later each time.
    for attempt in 0..5 {
        out = async {
            let mut tx = begin(&mut c, db, scope).await?;
            let r = effects_in(&mut tx, db, scope, drop_sql, skip, schema_oid).await;
            let _ = tx.rollback().await;
            r
        }
        .await;
        if out.is_ok() || attempt == 4 || !out.as_ref().is_err_and(|e| e.contains("XX000") || e.contains("40001")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(150 * (attempt + 1))).await;
    }
    let _ = c.close().await;
    out
}

async fn effects_in(tx: &mut Tx<'_>, db: &Db, scope: &str, drop_sql: &str, skip: Option<&str>, schema_oid: Option<&str>) -> Res<(u64, Vec<String>)> {
    // One snapshot for both readings, so what another session commits between them is not taken for the drop's doing. Our own DROP stays visible to us.
    tx.exec_bound("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ", &[]).await.map_err(se)?;
    // Descriptions name the schema of every object only when none is on the path.
    set_local(tx, db, scope, "search_path", "pg_catalog").await.map_err(se)?;
    let before = snapshot_objects(tx).await?;
    tx.exec_bound("SAVEPOINT quarry_cascade", &[]).await.map_err(se)?;
    if let Err(x) = tx.exec_bound(drop_sql, &[]).await {
        return Err(match sql_state(&x).as_deref() {
            Some("55P03") => lock_held_cascade(LOCK_WAIT_SECS),
            _ => se(x),
        });
    }
    let after = snapshot_objects(tx).await?;
    tx.exec_bound("ROLLBACK TO SAVEPOINT quarry_cascade", &[]).await.map_err(se)?;
    let gone: HashSet<&String> = before.keys().filter(|k| !after.contains_key(*k)).collect();
    let mut shown: Vec<(bool, &Obj)> = before
        .iter()
        .filter(|(k, o)| gone.contains(k) && Some(k.as_str()) != skip && !o.owner.as_ref().is_some_and(|w| gone.contains(w)))
        .map(|(_, o)| (schema_oid.is_some() && o.nsp.as_deref() == schema_oid, o))
        .collect();
    // Outside the schema first, because those are the ones the person did not expect to lose.
    shown.sort_by(|a, b| (a.0, &a.1.what).cmp(&(b.0, &b.1.what)));
    let total = shown.len() as u64;
    let mut lines = Vec::new();
    for (inside, o) in shown.into_iter().take(LISTED) {
        let mut notes = Vec::new();
        if schema_oid.is_some() && !inside {
            notes.push("outside this schema".to_string());
        }
        if let (Some(q), Some(est)) = (&o.qname, o.est) {
            // A count that takes too long or that row security hides falls back to the planner's estimate.
            let counted = async {
                tx.exec_bound("SAVEPOINT quarry_count", &[]).await.ok()?;
                let n = async {
                    set_local(tx, db, scope, "statement_timeout", "2s").await.ok()?;
                    set_local(tx, db, scope, "row_security", "off").await.ok()?;
                    tx.fetch_bound(&format!("SELECT count(*)::bigint AS n FROM {q}"), &[]).await.ok()?.first().and_then(|r| num(r, "n"))
                }
                .await;
                if n.is_none() {
                    tx.exec_bound("ROLLBACK TO SAVEPOINT quarry_count", &[]).await.ok()?;
                }
                tx.exec_bound("RELEASE SAVEPOINT quarry_count", &[]).await.ok()?;
                n
            }
            .await;
            match counted {
                Some(n) => notes.push(rows_word(n as i64)),
                None if est > 0 => notes.push(format!("about {}", rows_word(est))),
                None => {}
            }
        }
        lines.push(if notes.is_empty() { o.what.clone() } else { format!("{} ({})", o.what, notes.join(", ")) });
    }
    Ok((total, lines))
}

async fn schema_info(db: &Db, schema: &str, read_only: bool) -> Res<DropInfo> {
    let found = read(db, schema, &[("SELECT oid::text AS what FROM pg_namespace WHERE nspname = $1", vec![Param::Text(schema.into())])]).await?.remove(0);
    let Some(oid) = found.first().and_then(|r| text(r, "what")) else { return Ok(DropInfo::default()) };
    let (total, lines) = cascade_effects(db, schema, &drop_schema_sql(schema, true)?, None, Some(&oid), read_only).await?;
    Ok(DropInfo { exists: true, dependents_total: total, dependents: lines, ..Default::default() })
}

async fn table_info(db: &Db, schema: &str, table: &str, read_only: bool) -> Res<DropInfo> {
    let name = D.qualified(schema, table);
    let sql = "SELECT c.oid::text AS what, c.reltuples::bigint AS est, c.relkind::text AS kind FROM pg_class c WHERE c.oid = to_regclass($1)";
    let found = read(db, schema, &[(sql, vec![Param::Text(name.clone())])]).await?.remove(0);
    let Some(oid) = found.first().and_then(|r| text(r, "what")) else { return Ok(DropInfo::default()) };
    let est = found.first().and_then(|r| r.int("est")).filter(|n| *n >= 0).map(|n| n as u64);
    let kind = found.first().and_then(|r| text(r, "kind")).unwrap_or_default();
    // The keyword has to match the relation, or the server refuses the DROP it is asked to simulate.
    let word = match kind.as_str() {
        "v" => "VIEW",
        "m" => "MATERIALIZED VIEW",
        "f" => "FOREIGN TABLE",
        "S" => "SEQUENCE",
        _ => "TABLE",
    };
    let (total, lines) = cascade_effects(db, schema, &format!("DROP {word} {name} CASCADE"), Some(&format!("pg_class:{oid}")), None, read_only).await?;
    let mut info = DropInfo { exists: true, rows: est, rows_estimated: true, dependents_total: total, dependents: lines, ..Default::default() };
    // An exact count within a short wait, else the estimate stays and the dialog says "about".
    let count = format!("SELECT count(*)::bigint AS n FROM {name}");
    // Read only, because counting a view runs whatever functions it calls.
    let exact = read(db, schema, &[("SET LOCAL statement_timeout = '2s'", Vec::new()), (&count, Vec::new())]).await.ok().and_then(|mut all| all.pop()).and_then(|rows| rows.first().and_then(|r| num(r, "n")));
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
        if let Err(x) = c.execute(&format!("SET default_transaction_read_only = off; SET statement_timeout = 0; SET lock_timeout = '{WRITE_LOCK_SECS}s'")).await {
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

/// How many more times a statement that met another session on the database or its template is tried, and how long the first pause
/// is (it doubles). The server itself waits about five seconds for the other sessions before it refuses, so a retry that comes
/// after a refusal has already waited; no retry is started once `WITHIN` has passed since the first try.
const RETRIES: u32 = 2;
const FIRST_WAIT: Duration = Duration::from_millis(150);
const WITHIN: Duration = Duration::from_secs(8);

/// What a create gives back for the undo: the new list of databases, the new database's oid, and how many objects of a user it
/// holds right after the create.
struct Created {
    dbs: Vec<String>,
    oid: String,
    baseline: i64,
}

async fn create_db(db: &Db, name: &str, template: Option<&str>, read_only: bool) -> Res<Created> {
    let mut c = maintenance(db, read_only).await?;
    let out = async {
        let templates = template_list(&mut c).await?;
        let sql = create_db_sql(name, template, &templates)?;
        // Two CREATE DATABASE at once both want the template to themselves, and so does anyone copying it (another instance of this
        // app, a script), so the server's refusal is waited out a few times before it is reported.
        let (mut wait, began) = (FIRST_WAIT, Instant::now());
        for attempt in 0..=RETRIES {
            match alone(&mut c, db, &sql).await {
                Ok(()) => break,
                Err(x) if sql_state(&x).as_deref() == Some(IN_USE) && attempt < RETRIES && began.elapsed() < WITHIN => {
                    tokio::time::sleep(wait).await;
                    wait *= 2;
                }
                Err(x) if sql_state(&x).as_deref() == Some(IN_USE) => return Err(in_use(&se(x), InUse::Template)),
                Err(x) => return Err(se(x)),
            }
        }
        let oids = c.fetch_all("SELECT datname::text AS datname, oid::text AS oid FROM pg_database WHERE NOT datistemplate").await.map_err(se)?;
        let oid = oids.iter().find(|r| text(r, "datname").as_deref() == Some(name)).and_then(|r| text(r, "oid")).unwrap_or_default();
        let dbs = db_list(&mut c).await?;
        // What the template carried in is not the person's work, so the undo compares with this count and not with zero. An unreadable
        // database counts as empty, and the undo then refuses it if it cannot read it either.
        let baseline = read(&db.at(name), "", &[(&NOT_EMPTY, Vec::new())]).await.ok().and_then(|mut v| v.pop()).and_then(|rows| rows.first().and_then(|r| num(r, "n"))).unwrap_or(0) as i64;
        Ok(Created { dbs, oid, baseline })
    }
    .await;
    let _ = c.close().await;
    out
}

async fn template_list(c: &mut Conn) -> Res<Vec<String>> {
    Ok(c.fetch_all("SELECT datname::text AS datname FROM pg_database WHERE datistemplate ORDER BY 1").await.map_err(se)?.iter().filter_map(|r| text(r, "datname")).collect())
}

/// A drop must not go on while Quarry's own import, script or shell still holds a connection to the database: the server would refuse it
/// and the refusal would blame another session, or WITH (FORCE) would kill that work halfway.
fn ours_still_open(name: &str, ours: usize) -> Option<String> {
    (ours > 0).then(|| format!("Quarry itself still has {ours} connection{} open on {name}, for a running import, script file or shell. Stop it first, then drop the database. Nothing was dropped.", if ours == 1 { "" } else { "s" }))
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
        let ours = pools.close_database(name).await;
        if let Some(why) = ours_still_open(name, ours) {
            return Err(why);
        }
        let mut r = alone(&mut c, db, &sql).await;
        // A read that was already on its way when the pools closed can make a new pool and a new session before the drop runs, and
        // so can a session that is still ending. Each retry closes what the last one left. The server waits about five seconds for the
        // other sessions before it refuses, so the retries are bounded by `WITHIN`, after which its answer stands. FORCE ends the
        // sessions itself and needs none of this.
        let (mut wait, began) = (FIRST_WAIT, Instant::now());
        for _ in 0..RETRIES {
            if force || r.as_ref().err().and_then(sql_state).as_deref() != Some(IN_USE) || began.elapsed() >= WITHIN {
                break;
            }
            tokio::time::sleep(wait).await;
            wait *= 2;
            let ours = pools.close_database(name).await;
            if let Some(why) = ours_still_open(name, ours) {
                return Err(why);
            }
            r = alone(&mut c, db, &sql).await;
        }
        match r {
            Ok(()) => {}
            Err(x) if sql_state(&x).as_deref() == Some(IN_USE) => return Err(in_use(&se(x), InUse::Drop { name, pooler: db.pooler() })),
            Err(x) => return Err(se(x)),
        }
        db_list(&mut c).await
    }
    .await;
    let _ = c.close().await;
    out
}

// One transaction.

/// A connection with its write transaction open and the lock timeout of a structure change set, for a read that always ends in
/// ROLLBACK (`cascade_effects`).
async fn begin<'c>(c: &'c mut Conn, db: &Db, scope: &str) -> Res<Tx<'c>> {
    let mut tx = c.begin_write(db).await.map_err(se)?;
    set_lock_wait(&mut tx, db, scope).await.map_err(se)?;
    Ok(tx)
}

/// One write transaction of a create or a drop, ended by COMMIT, or by ROLLBACK when `body` failed (`run_locked`). A failed COMMIT
/// does not say that nothing changed, so it says so.
async fn in_tx<T>(db: &Db, scope: &str, read_only: bool, body: impl AsyncFnOnce(&mut Tx<'_>) -> Res<T>) -> Res<T> {
    let run = run_locked(db, scope, read_only, &[], "Refresh the lists and check before running it again.", async |tx| body(tx).await.map_err(Fail::Run)).await;
    run.map_err(|f| match f {
        Fail::Run(e) | Fail::Unknown(e) | Fail::Changed(e) => e,
    })
}

async fn namespace_oid(tx: &mut Tx<'_>, schema: &str) -> Res<Option<String>> {
    let rows = tx.fetch_bound("SELECT oid::text AS n FROM pg_namespace WHERE nspname = $1", &[Param::Text(schema.into())]).await.map_err(se)?;
    Ok(rows.first().and_then(|r| text(r, "n")))
}

async fn pg_ids_now(db: &Db, schema: &str, table: &str) -> Res<(Option<String>, Option<String>)> {
    let sql = "SELECT (SELECT oid::text FROM pg_database WHERE datname = current_database()) AS d, to_regclass($1)::oid::text AS t";
    let rows = read(db, "", &[(sql, vec![Param::Text(D.qualified(schema, table))])]).await?.remove(0);
    Ok((rows.first().and_then(|r| text(r, "d")), rows.first().and_then(|r| text(r, "t"))))
}

/// Refuses an undo whose database or table is not the one the change ran on, by oid. A drop of the database (or a dump that drops
/// first) followed by a create under the same name gives the same structure, and the undo would otherwise act on the new one.
pub(super) async fn check_ids(db: &Db, schema: &str, table: &str, ids: &write::PgIds) -> Res<()> {
    if ids.db.is_empty() && ids.table.is_empty() {
        return Ok(());
    }
    let (d, t) = pg_ids_now(db, schema, table).await.map_err(|e| format!("Undo refused: the table could not be checked ({e}). Nothing was changed."))?;
    if !ids.db.is_empty() && d.as_deref() != Some(ids.db.as_str()) {
        return Err(format!("Undo refused: the database was dropped and made again since this change ran, so {table} is not the table it changed. Nothing was changed."));
    }
    match t {
        None if !ids.table.is_empty() => Err(format!("Undo refused: {table} is gone. Nothing was changed.")),
        Some(now) if !ids.table.is_empty() && now != ids.table => Err(format!("Undo refused: {table} was dropped and made again since this change ran, so it is not the table it changed. Nothing was changed.")),
        _ => Ok(()),
    }
}

async fn relation_oid(tx: &mut Tx<'_>, name: &str) -> Res<Option<String>> {
    let rows = tx.fetch_bound("SELECT to_regclass($1)::oid::text AS n", &[Param::Text(name.into())]).await.map_err(se)?;
    Ok(rows.first().and_then(|r| text(r, "n")))
}

async fn create_schema(db: &Db, name: &str, read_only: bool) -> Res<String> {
    let sql = create_schema_sql(name)?;
    in_tx(db, name, read_only, async |tx| {
        exec(tx, db, name, &sql).await.map_err(se)?;
        namespace_oid(tx, name).await?.ok_or_else(|| "The schema was made but cannot be read back.".to_string())
    })
    .await
}

async fn drop_schema(db: &Db, name: &str, cascade: bool, read_only: bool) -> Res<()> {
    let sql = drop_schema_sql(name, cascade)?;
    in_tx(db, name, read_only, async |tx| {
        exec(tx, db, name, &sql).await.map_err(|x| match (sql_state(&x).as_deref(), lock_error(&x, name)) {
            (_, Some(why)) => why,
            // 2BP01 is dependent_objects_still_exist.
            (Some("2BP01"), _) => format!("{}\nTick CASCADE in the confirmation to drop what is in {name} with it. Nothing was dropped.", se(x)),
            _ => se(x),
        })
    })
    .await
}

/// What a create made, for the undo: the table's oid and how it reads right after.
struct Made {
    oid: String,
    state: Vec<String>,
}

async fn create_table(db: &Db, spec: &TableSpec, read_only: bool) -> Res<Made> {
    let stmts = create_table_stmts(spec)?;
    in_tx(db, &spec.schema, read_only, async |tx| {
        // The types and the default expressions resolve under the schema first and then the Advanced saved list, the path every other writer and the structure editor read under, so a type from a saved schema works here too.
        set_path(tx, db, &spec.schema).await.map_err(se)?;
        let types: Vec<String> = spec.cols.iter().map(|c| c.ty.trim().to_string()).collect();
        if let Some(bad) = unknown_type(tx, &types).await {
            let col = spec.cols.iter().find(|c| c.ty.trim() == bad).map_or("", |c| c.name.as_str());
            return Err(format!("Column {col}: {:?} is not a type the server knows. Write only the type, like numeric(10,2), without constraints or other column clauses. Nothing was created.", bad));
        }
        for s in &stmts {
            exec(tx, db, &spec.schema, s).await.map_err(|x| lock_error(&x, &spec.name).unwrap_or_else(|| se(x)))?;
        }
        let state = state_in(tx, &spec.schema, &spec.name).await?;
        let oid = relation_oid(tx, &D.qualified(&spec.schema, &spec.name)).await?.ok_or("The table was made but cannot be read back.")?;
        Ok(Made { oid, state })
    })
    .await
}

async fn drop_table(db: &Db, schema: &str, table: &str, view: bool, cascade: bool, read_only: bool) -> Res<()> {
    let sql = drop_table_sql(schema, table, view, cascade)?;
    in_tx(db, schema, read_only, async |tx| {
        exec(tx, db, schema, &sql).await.map_err(|x| match (sql_state(&x).as_deref(), lock_error(&x, table)) {
            (_, Some(why)) => why,
            (Some("2BP01"), _) => format!("{}\nTick CASCADE in the confirmation to drop what depends on it too. Nothing was dropped.", se(x)),
            _ => se(x),
        })
    })
    .await
}

// Undo of a create.

async fn undo_schema(db: &Db, name: &str, oid: &str, read_only: bool) -> Res<()> {
    let sql = drop_schema_sql(name, false)?;
    in_tx(db, name, read_only, async |tx| {
        match namespace_oid(tx, name).await? {
            None => return Err(format!("Not undone: the schema {name} is already gone.")),
            Some(now) if now != oid => return Err(format!("Not undone: {name} was dropped and made again since, so it is not the schema this created.")),
            Some(_) => {}
        }
        // Without CASCADE the server refuses a schema that holds anything, inside this transaction, so no object can arrive between a check and the drop.
        exec(tx, db, name, &sql).await.map_err(|x| match (sql_state(&x).as_deref(), lock_error(&x, name)) {
            (_, Some(why)) => format!("Not undone: {why}"),
            (Some("2BP01"), _) => format!("Not undone: {name} has objects in it now.\n{}", se(x)),
            _ => se(x),
        })
    })
    .await
}

async fn undo_table(db: &Db, schema: &str, table: &str, made: &Made, read_only: bool) -> Res<()> {
    let sql = drop_table_sql(schema, table, false, false)?;
    let name = D.qualified(schema, table);
    in_tx(db, schema, read_only, async |tx| {
        // Locked before anything is read, so a row cannot arrive between the check and the drop. Without the lock the check
        // could be stale by the time the DROP runs, so a role that cannot take it does not get the undo.
        if !may_lock_one(tx, &name).await {
            return Err(format!("Not undone: {table} cannot be locked with this role's privileges, so it cannot be checked for rows safely. Drop it from its menu instead."));
        }
        if let Err(x) = exec(tx, db, schema, &format!("LOCK TABLE {name} IN ACCESS EXCLUSIVE MODE")).await {
            return Err(match (sql_state(&x).as_deref(), lock_error(&x, table)) {
                (_, Some(why)) => format!("Not undone: {why}"),
                (Some("42P01"), _) => format!("Not undone: the table {table} is already gone."),
                _ => se(x),
            });
        }
        match relation_oid(tx, &name).await? {
            None => return Err(format!("Not undone: the table {table} is already gone.")),
            Some(now) if now != made.oid => return Err(format!("Not undone: {table} was dropped and made again since, so it is not the table this created.")),
            Some(_) => {}
        }
        set_path(tx, db, schema).await.map_err(se)?;
        if state_in(tx, schema, table).await? != made.state {
            return Err(format!("Not undone: the structure of {table} changed since it was made, so dropping it would lose more than the create made."));
        }
        // Row level security can hide every row from this role, even the table's owner under FORCE, and the check would then call a full table empty. With it off the
        // server answers 42501 when a policy would apply and the role cannot bypass it, which is a refusal and never a guess.
        set_local(tx, db, schema, "row_security", "off").await.map_err(se)?;
        match tx.fetch_bound(&format!("SELECT 1 AS n FROM {name} LIMIT 1"), &[]).await {
            Ok(rows) if !rows.is_empty() => return Err(format!("Not undone: {table} has rows now.")),
            Ok(_) => {}
            Err(x) if sql_state(&x).as_deref() == Some("42501") => return Err(format!("Not undone: row level security applies to {table} for this role, so Quarry cannot tell whether it has rows.")),
            Err(x) => return Err(se(x)),
        }
        exec(tx, db, schema, &sql).await.map_err(|x| match sql_state(&x).as_deref() {
            // Another table's key or a view was made on it since.
            Some("2BP01") => format!("Not undone: something depends on {table} now.\n{}", se(x)),
            _ => se(x),
        })
    })
    .await
}

/// How many objects of a user, large objects included, the database holds, counted on a connection that stays open.
async fn held_count(c: &mut Conn, db: &Db) -> Res<i64> {
    let rows = read_on(c, db, "", &[(&NOT_EMPTY, Vec::new())]).await?.remove(0);
    rows.first().and_then(|r| num(r, "n")).map(|n| n as i64).ok_or_else(|| "The server gave no count".to_string())
}

async fn undo_db(db: &Db, name: &str, oid: &str, baseline: i64, read_only: bool) -> Res<Vec<String>> {
    let sql = drop_db_sql(name, false)?;
    let Pool::Pg(pools) = &db.pool else { return Err("Not a PostgreSQL connection".into()) };
    let mut c = maintenance(db, read_only).await?;
    let out = async {
        // The name is not the database: one dropped and made again under it has another oid and is not what this created.
        let now = read(db, "", &[("SELECT oid::text AS what FROM pg_database WHERE datname = $1", vec![Param::Text(name.into())])]).await?.remove(0);
        match now.first().and_then(|r| text(r, "what")) {
            None => return Err(format!("Not undone: the database {name} is already gone.")),
            Some(n) if !oid.is_empty() && n != oid => return Err(format!("Not undone: {name} was dropped and made again since, so it is not the database this created.")),
            Some(_) => {}
        }
        let at = db.at(name);
        let mut seen = at.acquire_conn().await.map_err(|e| format!("Not undone: {name} could not be checked ({e})"))?;
        let held = async {
            let n = held_count(&mut seen, &at).await.map_err(|e| format!("Not undone: {name} could not be checked ({e})"))?;
            if n > baseline {
                return Err(format!("Not undone: {name} has tables or other objects in it now."));
            }
            // From here nobody new can connect, so the second look through the connection that is still open sees what the sessions already
            // there did meanwhile; a session still connected when the DROP runs makes it refuse, and that is the other half of the promise.
            alone(&mut c, db, &format!("ALTER DATABASE {} WITH ALLOW_CONNECTIONS false", q(name))).await.map_err(|x| format!("Not undone: {name} could not be closed to new connections ({})", se(x)))?;
            let again = held_count(&mut seen, &at).await;
            Ok(again)
        }
        .await;
        let _ = seen.close().await;
        let blocked = held.is_ok();
        let verdict = match held {
            Ok(Ok(n)) if n > baseline => Err(format!("Not undone: {name} has tables or other objects in it now.")),
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(format!("Not undone: {name} could not be checked ({e})")),
            Err(e) => Err(e),
        };
        let dropped = async {
            verdict?;
            // Quarry's own connections to it go too, or the server refuses for them.
            pools.close_database(name).await;
            match alone(&mut c, db, &sql).await {
                Ok(()) => Ok(()),
                Err(x) if sql_state(&x).as_deref() == Some(IN_USE) => Err(in_use(&se(x), InUse::Undo { name })),
                Err(x) => Err(se(x)),
            }
        }
        .await;
        if dropped.is_err() && blocked {
            // The database stays, so it takes connections again as it did before.
            if let Err(x) = alone(&mut c, db, &format!("ALTER DATABASE {} WITH ALLOW_CONNECTIONS true", q(name))).await {
                return Err(format!("{}\nQuarry could not open {name} to connections again ({}). Run ALTER DATABASE {} ALLOW_CONNECTIONS true.", dropped.unwrap_err(), se(x), q(name)));
            }
        }
        dropped?;
        db_list(&mut c).await
    }
    .await;
    let _ = c.close().await;
    out
}

fn entry(db: &Db, conn: u64, schema: &str, table: &str, undo: SqlUndo) -> UndoAction {
    UndoAction { at: Instant::now(), conn, db: schema.into(), coll: table.into(), kind: UndoKind::Sql { database: db.pg_database(), cols: Vec::new(), undo, ids: write::PgIds::default() } }
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
    pub fn pg_drop_info(&self, conn: u64, db: Db, database: String, schema: String, table: String, read_only: bool) {
        self.spawn_sql(db.conn, &schema.clone(), async move {
            let at = db.at(&database);
            let res = match (schema.is_empty(), table.is_empty()) {
                (true, true) => db_info(&db, &database).await,
                (false, true) => schema_info(&at, &schema, read_only).await,
                _ => table_info(&at, &schema, &table, read_only).await,
            };
            Msg::SqlDropInfo { conn, database, schema, table, res }
        });
    }

    pub fn pg_create_db(&self, conn: u64, db: Db, name: String, template: Option<String>, read_only: bool) {
        self.spawn_sql(db.conn, "", async move {
            let res = create_db(&db, &name, template.as_deref(), read_only).await;
            let undo = res.as_ref().ok().map(|m| entry(&db, conn, &name, "", SqlUndo::CreatedDb { oid: m.oid.clone(), baseline: m.baseline }));
            let (res, dbs) = answer(res.map(|m| Some(m.dbs)), format!("Created database {name}"));
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
    let UndoKind::Sql { database, cols, undo, ids } = kind else { return Msg::SqlDdl { conn, database: String::new(), schema, table, what: Ddl::DroppedTable, res: Err("Not a create".into()), undo: None, dbs: None, sql_mode: None } };
    let (what, res, dbs, said) = match &undo {
        SqlUndo::CreatedDb { oid, baseline } => {
            let r = undo_db(&db, &schema, oid, *baseline, read_only).await;
            (Ddl::DroppedDb, r.as_ref().map(|_| ()).map_err(Clone::clone), r.ok(), format!("Removed database {schema}"))
        }
        SqlUndo::CreatedSchema { oid } => (Ddl::DroppedSchema, undo_schema(&db, &schema, oid, read_only).await, None, format!("Removed schema {schema}")),
        SqlUndo::PgCreatedTable { oid, state } => (Ddl::DroppedTable, undo_table(&db, &schema, &table, &Made { oid: oid.clone(), state: state.clone() }, read_only).await, None, format!("Removed table {table}")),
        _ => (Ddl::DroppedTable, Err("Not a create".into()), None, String::new()),
    };
    let kept = res.is_err().then(|| UndoAction { at, conn, db: schema.clone(), coll: table.clone(), kind: UndoKind::Sql { database: database.clone(), cols, undo, ids } });
    // A refused undo changes nothing, so the app has nothing to refresh.
    Msg::SqlDdl { conn, database, schema, table, what, res: res.map(|_| said), undo: kept, dbs, sql_mode: None }
}
