//! Creating and dropping PostgreSQL databases, schemas and tables. Every name is quoted, and the only user text besides names
//! is a column type and a default expression, which `pg::ddl` checks the way the structure editor does. Where MySQL's DDL
//! commits by itself, PostgreSQL's is transactional, so a schema or a table is made and dropped inside one transaction
//! and an undo checks and drops in the same one. A database is the exception: CREATE and DROP DATABASE refuse to run in a
//! transaction block, so they run alone on a connection of their own taken from the pool of the connection's own database
//! (never from the pool of the database being dropped), and the undo of a create checks that the database is empty a
//! round trip before it drops it, which no lock can close.

use crate::db::sql::create::{Ddl, DropInfo};
use crate::db::sql::ddl::{fk_problem, ref_state, snapshot, ColDef};
use crate::db::sql::pg::ddl::{check_constraint_pg, col_body, comment_stmt, exec, fail_text, fk_refs, lock_error, may_lock, may_lock_one, name_ok, q, read_in, run_locked, set_local, set_lock_wait, set_path, table_state, unknown_type, Fail, Lock};
use crate::db::sql::exec::{sql_state, Cells, Conn, SqlRow, Tx};
use sqlx::Connection as _;
use crate::db::sql::structure::{num, Column, ForeignKey, Index, IndexCol, Structure};
use crate::db::sql::value::Param;
use crate::db::sql::worker::text;
use crate::db::sql::write::{self, SqlUndo};
use crate::db::sql::pg::{LOCK_WAIT_SECS, WRITE_LOCK_SECS};
use crate::db::sql::{se, Db, Dialect, Pool};
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
pub(in crate::db::sql) const LISTED: usize = 20;

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

/// A template database and the settings a copy of it keeps unless template0 is the template.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Template {
    pub name: String,
    pub encoding: String,
    pub collate: String,
    pub ctype: String,
    /// "c" libc, "i" ICU, "b" builtin (17 and later). Always "c" before 15.
    pub provider: String,
    /// The ICU locale of an ICU template, empty otherwise.
    pub icu: String,
}

/// What the server offers a new database, read when the New database dialog opens and again right before the create.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DbChoices {
    /// server_version_num, such as 180001.
    pub version: u32,
    pub templates: Vec<Template>,
    /// The server encodings, by name.
    pub encodings: Vec<String>,
    /// The libc locales the server knows, each with the encoding it is for, empty for any.
    pub locales: Vec<(String, String)>,
    /// The ICU locales, empty when the server has no ICU.
    pub icu_locales: Vec<String>,
    /// The roles the connected role may make the owner: those it may SET ROLE to (16 and later) or is a member of (before 16). A
    /// superuser gets every role. The server's predefined pg_ roles are left out.
    pub owners: Vec<String>,
    /// The connected role.
    pub me: String,
}

impl DbChoices {
    /// The ICU provider and ICU_LOCALE came with PostgreSQL 15, and need a server built with ICU.
    pub fn icu_ok(&self) -> bool {
        self.version >= 150000 && !self.icu_locales.is_empty()
    }

    pub fn template(&self, name: Option<&str>) -> Option<&Template> {
        let name = name.unwrap_or(DEFAULT_TEMPLATE);
        self.templates.iter().find(|t| t.name == name)
    }

    /// The libc locales that work with `encoding`.
    pub fn locales_for<'a>(&'a self, encoding: &'a str) -> impl Iterator<Item = &'a String> + 'a {
        self.locales.iter().filter(move |(_, e)| e.is_empty() || e.eq_ignore_ascii_case(encoding)).map(|(l, _)| l)
    }

    /// The libc locales offered with `encoding`, sorted: the imported ones plus the template's own when the encoding is the
    /// template's. An image without imported OS locales lists only C and POSIX, yet the server takes the locale it was made with.
    pub fn locales_with(&self, encoding: &str, tpl: Option<&Template>) -> Vec<String> {
        let mut v: Vec<String> = self.locales_for(encoding).cloned().collect();
        if let Some(t) = tpl.filter(|t| t.encoding.eq_ignore_ascii_case(encoding)) {
            v.extend([&t.collate, &t.ctype].into_iter().filter(|l| !l.is_empty()).cloned());
        }
        v.sort();
        v.dedup();
        v
    }

    /// The provider a new database gets, "c", "i" or "b": the one picked, else the template's.
    pub fn provider_of(&self, o: &DbOpts) -> String {
        match o.provider.as_deref() {
            Some("icu") => "i".into(),
            Some(_) => "c".into(),
            None => self.template(o.template.as_deref()).map(|t| t.provider.clone()).filter(|p| !p.is_empty()).unwrap_or_else(|| "c".into()),
        }
    }

    /// From 16 LOCALE also sets the ICU or builtin locale when the provider is not libc, where before it set only LC_COLLATE and
    /// LC_CTYPE. Offering it only where it means the same on every version keeps the preview honest.
    pub fn locale_word_ok(&self, o: &DbOpts) -> bool {
        self.version < 160000 || self.provider_of(o) == "c"
    }

    /// Why an encoding picked with the template's locale left in place would be refused: the template's LC_COLLATE or LC_CTYPE
    /// does not go with it. SQL_ASCII is left to the server, which takes any locale with it for a superuser.
    pub fn template_locale_problem(&self, o: &DbOpts) -> Option<String> {
        let enc = o.encoding.as_deref()?;
        let t = self.template(o.template.as_deref())?;
        if enc.eq_ignore_ascii_case("SQL_ASCII") || o.locale.is_some() {
            return None;
        }
        let offered = self.locales_with(enc, Some(t));
        let kept = [(o.lc_collate.is_none(), &t.collate), (o.lc_ctype.is_none(), &t.ctype)];
        let (_, l) = kept.into_iter().find(|(k, l)| *k && !l.is_empty() && !offered.contains(l))?;
        Some(format!("The template's locale {l} does not go with the encoding {enc}. Pick a locale for it too (C goes with every encoding)."))
    }
}

/// How the dialog names a provider letter of pg_database.
pub fn provider_name(p: &str) -> &'static str {
    match p {
        "i" => "icu",
        "b" => "builtin",
        _ => "libc",
    }
}

/// What CREATE DATABASE copies when no template is named.
pub const DEFAULT_TEMPLATE: &str = "template1";

/// The only template a copy of may take another encoding, locale or provider: the server refuses that for any other, because its
/// data may already be in the old ones.
pub const PRISTINE: &str = "template0";

/// The options of a new database. None is the template's own setting (or the connected role, for the owner).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DbOpts {
    pub template: Option<String>,
    pub encoding: Option<String>,
    /// LOCALE, which sets LC_COLLATE and LC_CTYPE at once; the server refuses it together with either of those.
    pub locale: Option<String>,
    pub lc_collate: Option<String>,
    pub lc_ctype: Option<String>,
    /// LOCALE_PROVIDER: "libc" or "icu".
    pub provider: Option<String>,
    pub icu_locale: Option<String>,
    pub owner: Option<String>,
}

/// A setting written as a plain string literal. Every value comes from a list the server gave, and none holds a quote, a backslash or a
/// control character, so the literal reads the same whatever standard_conforming_strings is on the session that runs it.
fn setting(what: &str, v: &str) -> Res<String> {
    if v.is_empty() || v.chars().any(|c| c == '\'' || c == '\\' || c.is_control()) {
        return Err(format!("{what} {v:?} is not one this server lists."));
    }
    Ok(format!("'{v}'"))
}

pub fn create_db_sql(name: &str, o: &DbOpts, ch: &DbChoices) -> Res<String> {
    new_name("A database", name)?;
    if system_db(name) {
        return Err(format!("{name} belongs to the server and is left alone."));
    }
    let mut sql = format!("CREATE DATABASE {}", q(name));
    if let Some(r) = &o.owner {
        if !ch.owners.contains(r) {
            return Err(format!("{r} is not a role this connection can make the owner."));
        }
        sql.push_str(&format!(" OWNER {}", q(r)));
    }
    if let Some(t) = &o.template {
        // The list is the server's own. A name outside it would be a database someone is connected to, which PostgreSQL refuses to copy.
        if !ch.templates.iter().any(|k| &k.name == t) {
            return Err(format!("{t} is not a template database on this server."));
        }
        sql.push_str(&format!(" TEMPLATE {}", q(t)));
    }
    let tpl = ch.template(o.template.as_deref());
    if let Some(e) = &o.encoding {
        if !ch.encodings.iter().any(|k| k == e) {
            return Err(format!("{e} is not an encoding this server can store."));
        }
        sql.push_str(&format!(" ENCODING {}", setting("The encoding", e)?));
    }
    if o.locale.is_some() && (o.lc_collate.is_some() || o.lc_ctype.is_some()) {
        return Err("Pick either one locale or LC_COLLATE and LC_CTYPE, not both.".into());
    }
    let enc = o.encoding.clone().or_else(|| tpl.map(|t| t.encoding.clone())).unwrap_or_default();
    if o.locale.is_some() && !ch.locale_word_ok(o) {
        let p = provider_name(&ch.provider_of(o));
        return Err(format!("On PostgreSQL 16 and later LOCALE also sets the {p} locale of a database with the {p} provider. Pick LC_COLLATE and LC_CTYPE instead."));
    }
    let offered = ch.locales_with(&enc, tpl);
    let own = |v: &String| tpl.is_some_and(|t| *v == t.collate || *v == t.ctype);
    for (word, v) in [("LOCALE", &o.locale), ("LC_COLLATE", &o.lc_collate), ("LC_CTYPE", &o.lc_ctype)] {
        let Some(v) = v else { continue };
        if !ch.locales.iter().any(|(l, _)| l == v) && !own(v) {
            return Err(format!("{v} is not a locale this server knows."));
        }
        if !offered.contains(v) {
            return Err(format!("The locale {v} does not go with the encoding {enc}."));
        }
        sql.push_str(&format!(" {word} {}", setting("The locale", v)?));
    }
    if (o.provider.is_some() || o.icu_locale.is_some()) && ch.version < 150000 {
        return Err("A locale provider and an ICU locale need PostgreSQL 15 or later.".into());
    }
    let icu = match o.provider.as_deref() {
        None => tpl.is_some_and(|t| t.provider == "i"),
        Some("icu") if !ch.icu_ok() => return Err("This server was built without ICU.".into()),
        Some(p @ ("libc" | "icu")) => {
            sql.push_str(&format!(" LOCALE_PROVIDER {p}"));
            p == "icu"
        }
        Some(p) => return Err(format!("{p} is not a locale provider Quarry offers.")),
    };
    match &o.icu_locale {
        Some(_) if !icu => return Err("An ICU locale needs the ICU provider.".into()),
        Some(v) if !ch.icu_locales.contains(v) => return Err(format!("{v} is not an ICU locale this server knows.")),
        Some(v) => sql.push_str(&format!(" ICU_LOCALE {}", setting("The ICU locale", v)?)),
        // PostgreSQL 15 refuses the ICU provider without an ICU locale, and later ones would take the template's.
        None if o.provider.as_deref() == Some("icu") => return Err("Pick an ICU locale for the ICU provider.".into()),
        None => {}
    }
    // A copy of any template but template0 keeps the template's encoding and locale, and the server refuses a different one.
    if let Some(t) = tpl.filter(|t| t.name != PRISTINE) {
        let mut differ = Vec::new();
        if o.encoding.as_ref().is_some_and(|e| !e.eq_ignore_ascii_case(&t.encoding)) {
            differ.push(format!("encoding {}", t.encoding));
        }
        if o.locale.as_ref().is_some_and(|l| *l != t.collate || *l != t.ctype) || o.lc_collate.as_ref().is_some_and(|l| *l != t.collate) || o.lc_ctype.as_ref().is_some_and(|l| *l != t.ctype) {
            differ.push(format!("locale {}", t.collate));
        }
        let prov = |p: &str| if p == "icu" { "i" } else { "c" };
        if o.provider.as_deref().is_some_and(|p| prov(p) != t.provider) || o.icu_locale.as_ref().is_some_and(|l| *l != t.icu) {
            differ.push("its locale provider".into());
        }
        if !differ.is_empty() {
            return Err(format!("Another encoding or locale needs TEMPLATE template0: a copy of {} keeps its {}.", t.name, differ.join(" and ")));
        }
    }
    if let Some(why) = ch.template_locale_problem(o) {
        return Err(why);
    }
    Ok(sql)
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

/// One UNIQUE constraint over its columns, in key order. An empty name lets the server name it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Unique {
    pub name: String,
    pub cols: Vec<String>,
}

/// One CHECK constraint. An empty name lets the server name it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Check {
    pub name: String,
    pub expr: String,
}

/// A plain btree index made right after the table, in the same transaction. An empty name lets the server name it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NewIndex {
    pub name: String,
    pub cols: Vec<String>,
    pub unique: bool,
}

/// What the New table dialog collects. A column is the structure editor's `ColDef`, so its type, default and identity are written
/// and checked by the same code that adds a column to an existing table, and a foreign key is the Relations editor's `ForeignKey`
/// (an empty name lets the server name it, `ref_db` is the parent's schema and empty for the table's own).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TableSpec {
    pub schema: String,
    pub name: String,
    pub cols: Vec<ColDef>,
    /// The columns of the primary key, in key order.
    pub pk: Vec<String>,
    pub uniques: Vec<Unique>,
    pub checks: Vec<Check>,
    pub fks: Vec<ForeignKey>,
    pub indexes: Vec<NewIndex>,
    pub comment: String,
}

impl TableSpec {
    /// The schema of a key's parent.
    pub fn parent_schema<'a>(&'a self, k: &'a ForeignKey) -> &'a str {
        if k.ref_db.is_empty() { &self.schema } else { &k.ref_db }
    }

    /// A key that points back at the table being made, whose parent is this spec rather than a table on the server.
    pub fn self_ref(&self, k: &ForeignKey) -> bool {
        self.parent_schema(k) == self.schema && k.ref_table == self.name
    }

    /// The other tables the keys point at, each once, in the one order every structure change locks tables (by schema, then name),
    /// so two writers that share them never wait on each other in a circle.
    pub fn parents(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self.fks.iter().filter(|k| !self.self_ref(k)).map(|k| (self.parent_schema(k).to_string(), k.ref_table.clone())).collect();
        v.sort();
        v.dedup();
        v
    }

    /// The table as the structure editor would read it once made, as far as a foreign key check needs it: the columns with the types
    /// as typed, and the primary key and UNIQUE constraints. The indexes are left out because they are made after the table, too
    /// late for a key of its own to lean on.
    pub fn as_structure(&self) -> Structure {
        let col = |c: &ColDef| Column { name: c.name.clone(), full_type: c.ty.trim().to_string(), nullable: c.nullable && !self.pk.contains(&c.name), collation: c.collation.clone(), ..Default::default() };
        let key = |name: &str, cols: &[String], primary: bool| Index { name: name.to_string(), unique: true, primary, constraint: true, cols: cols.iter().map(|n| IndexCol { name: n.clone(), ..Default::default() }).collect(), ..Default::default() };
        let mut indexes: Vec<Index> = self.uniques.iter().map(|u| key(&u.name, &u.cols, false)).collect();
        if !self.pk.is_empty() {
            indexes.insert(0, key("", &self.pk, true));
        }
        Structure { columns: self.cols.iter().map(col).collect(), indexes, pg_schema: Some(self.schema.clone()), ..Default::default() }
    }
}

/// `CONSTRAINT "name" ` when the person named it, else nothing, so the server picks the name.
fn named(what: &str, name: &str) -> Res<String> {
    if name.is_empty() {
        return Ok(String::new());
    }
    new_name(what, name)?;
    Ok(format!("CONSTRAINT {} ", q(name)))
}

/// The statements that make the table: the CREATE TABLE with its keys, checks and foreign keys, the indexes, then the comments.
/// PostgreSQL names an unnamed constraint or index itself (`t_pkey`, `t_c_key`, `t_c_fkey`, `t_c_idx`), which is the name every
/// other client and the structure editor already know.
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
    // Constraints and indexes share one name space per schema for the indexes behind them, and the server's refusal would come
    // after the work, so a name given twice is refused here.
    let given: Vec<&str> = spec.uniques.iter().map(|u| u.name.as_str()).chain(spec.checks.iter().map(|c| c.name.as_str())).chain(spec.fks.iter().map(|k| k.name.as_str())).chain(spec.indexes.iter().map(|i| i.name.as_str())).filter(|n| !n.is_empty()).collect();
    if let Some(n) = given.iter().enumerate().find(|(i, n)| given[..*i].contains(n)).map(|(_, n)| n) {
        return Err(format!("Two constraints or indexes are called {n}."));
    }
    if !spec.pk.is_empty() {
        lines.push(format!("  PRIMARY KEY ({})", group("primary key", &spec.pk)?));
    }
    for u in &spec.uniques {
        lines.push(format!("  {}UNIQUE ({})", named("A unique constraint", &u.name)?, group("unique constraint", &u.cols)?));
    }
    for c in &spec.checks {
        check_constraint_pg(&c.expr)?;
        lines.push(format!("  {}CHECK ({})", named("A check constraint", &c.name)?, c.expr.trim()));
    }
    let own = spec.as_structure();
    for k in &spec.fks {
        if k.not_valid {
            return Err("A new table has no rows for NOT VALID to skip, so a key is always checked.".into());
        }
        let cols = group("foreign key", &k.cols)?;
        // A key on the table itself leans on a key made by this statement, so it is checked here; one on another table is checked
        // against that table as the transaction finds it under its lock.
        if spec.self_ref(k) && let Some(p) = fk_problem(&own, &spec.name, &own, k, false) {
            return Err(p);
        }
        lines.push(format!("  {}FOREIGN KEY ({cols}) {}", named("A foreign key", &k.name)?, fk_refs(&spec.schema, k)?));
    }
    let t = D.qualified(&spec.schema, &spec.name);
    let mut out = vec![format!("CREATE TABLE {t} (\n{}\n)", lines.join(",\n"))];
    for i in &spec.indexes {
        let name = if i.name.is_empty() { String::new() } else { new_name("An index", &i.name).map(|_| format!("{} ", q(&i.name)))? };
        out.push(format!("CREATE {}INDEX {name}ON {t} ({})", if i.unique { "UNIQUE " } else { "" }, group("index", &i.cols)?));
    }
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
pub(in crate::db::sql) async fn read(db: &Db, scope: &str, steps: &[(&str, Vec<Param>)]) -> Res<Vec<Vec<SqlRow>>> {
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
    Ok(c.fetch_all(crate::db::sql::pg::DB_LIST).await.map_err(se)?.iter().filter_map(|r| text(r, "datname")).collect())
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
    if !db.pooler() && let Err(x) = c.execute(&format!("SET default_transaction_read_only = off; SET statement_timeout = 0; SET lock_timeout = '{WRITE_LOCK_SECS}s'")).await {
        let _ = c.close().await;
        return Err(se(x));
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
    /// Why the create has no undo, when the connected role could not drop what it made.
    no_undo: Option<String>,
}

/// DROP DATABASE needs the owner's rights, which a role holds only when it inherits them. CREATE DATABASE OWNER asks for less (SET
/// from 16, MEMBER before), so a role can give a database away and then be unable to drop it: such a create gets no undo.
async fn undo_blocked_by_owner(c: &mut Conn, db: &Db, owner: &str) -> Option<String> {
    let sql = "SELECT COALESCE((SELECT pg_has_role(oid, 'USAGE') FROM pg_roles WHERE rolname = $1), false)::int::bigint AS n";
    let ok = read_on(c, db, "", &[(sql, vec![Param::Text(owner.into())])]).await.ok().and_then(|mut v| v.pop()).and_then(|rows| rows.first().and_then(|r| num(r, "n"))).is_some_and(|n| n != 0);
    (!ok).then(|| format!("No undo: this role does not inherit the rights of {owner}, the owner, so it could not drop the database."))
}

async fn create_db(db: &Db, name: &str, opts: &DbOpts, read_only: bool) -> Res<Created> {
    let mut c = maintenance(db, read_only).await?;
    let out = async {
        // Read again on the connection that runs the statement, so a role or template that changed since the dialog opened is checked as it is now.
        let choices = db_choices(&mut c).await?;
        let sql = create_db_sql(name, opts, &choices)?;
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
        let no_undo = match opts.owner.as_ref().filter(|r| **r != choices.me) {
            Some(r) => undo_blocked_by_owner(&mut c, db, r).await,
            None => None,
        };
        Ok(Created { dbs, oid, baseline, no_undo })
    }
    .await;
    let _ = c.close().await;
    out
}

/// The template databases with their settings. The columns of the locale provider and the ICU locale came in 15 and were renamed in
/// 17, so they are read through the row's JSON, which answers NULL for a column the server does not have.
const TEMPLATES: &str = "SELECT datname::text AS name, pg_encoding_to_char(encoding)::text AS enc, datcollate::text AS coll, datctype::text AS ctype, COALESCE(to_jsonb(d)->>'datlocprovider', 'c') AS prov, COALESCE(to_jsonb(d)->>'datlocale', to_jsonb(d)->>'daticulocale', '') AS icu FROM pg_database d WHERE datistemplate ORDER BY 1";

/// The server encodings are the ids up to KOI8U (PG_ENCODING_BE_LAST, 34 since 8.3); the ones after it are for clients only.
const ENCODINGS: &str = "SELECT pg_encoding_to_char(i)::text AS name FROM generate_series(0, 34) AS i ORDER BY 1";

/// The libc locales come from the collations initdb imported from the operating system, which are the ones the server can use.
const LOCALES: &str = "SELECT DISTINCT to_jsonb(c)->>'collcollate' AS name, CASE WHEN collencoding = -1 THEN '' ELSE pg_encoding_to_char(collencoding)::text END AS enc FROM pg_collation c WHERE collprovider = 'c' AND to_jsonb(c)->>'collcollate' <> '' ORDER BY 1, 2";

const ICU_LOCALES: &str = "SELECT DISTINCT COALESCE(to_jsonb(c)->>'colllocale', to_jsonb(c)->>'colliculocale', to_jsonb(c)->>'collcollate') AS name FROM pg_collation c WHERE collprovider = 'i' ORDER BY 1";

/// CREATE DATABASE OWNER needs the right to SET ROLE to the owner from 16 on and membership in it before, which pg_has_role answers
/// (true for every role for a superuser).
const OWNERS: &str = "SELECT rolname::text AS name FROM pg_roles WHERE rolname !~ '^pg_' AND pg_has_role(current_user, oid, CASE WHEN current_setting('server_version_num')::int >= 160000 THEN 'SET' ELSE 'MEMBER' END) ORDER BY 1";

async fn db_choices(c: &mut Conn) -> Res<DbChoices> {
    let names = |rows: Vec<SqlRow>| rows.iter().filter_map(|r| text(r, "name")).collect::<Vec<_>>();
    let head = c.fetch_all("SELECT current_setting('server_version_num') AS v, current_user::text AS me").await.map_err(se)?;
    let version = head.first().and_then(|r| text(r, "v")).and_then(|v| v.parse().ok()).unwrap_or(0);
    let me = head.first().and_then(|r| text(r, "me")).unwrap_or_default();
    let templates = c.fetch_all(TEMPLATES).await.map_err(se)?.iter().filter_map(|r| Some(Template { name: text(r, "name")?, encoding: text(r, "enc").unwrap_or_default(), collate: text(r, "coll").unwrap_or_default(), ctype: text(r, "ctype").unwrap_or_default(), provider: text(r, "prov").unwrap_or_default(), icu: text(r, "icu").unwrap_or_default() })).collect();
    let encodings = names(c.fetch_all(ENCODINGS).await.map_err(se)?).into_iter().filter(|e| !e.is_empty()).collect();
    let locales = c.fetch_all(LOCALES).await.map_err(se)?.iter().filter_map(|r| Some((text(r, "name")?, text(r, "enc").unwrap_or_default()))).collect();
    // Before 15 a database cannot use ICU, so its long list is not read.
    let icu_locales = if version >= 150000 { names(c.fetch_all(ICU_LOCALES).await.map_err(se)?) } else { Vec::new() };
    let owners = names(c.fetch_all(OWNERS).await.map_err(se)?);
    Ok(DbChoices { version, templates, encodings, locales, icu_locales, owners, me })
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
pub(in crate::db::sql) async fn check_ids(db: &Db, schema: &str, table: &str, ids: &write::PgIds) -> Res<()> {
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

/// The first function a CHECK of the table `$1` (an oid) reaches that is not IMMUTABLE, with its constraint: called by name, behind an
/// operator or through a cast. pg_depend records what the server resolved, so an overload, a search_path hit or a user operator is
/// the function that really runs. Built-ins are pinned and never recorded, which the name list of `check_constraint_pg` covers.
const CHECK_VOLATILE: &str = "WITH d AS (
    SELECT k.conname, d.refclassid, d.refobjid FROM pg_constraint k JOIN pg_depend d ON d.classid = 'pg_constraint'::regclass AND d.objid = k.oid
    WHERE k.conrelid = $1::text::oid AND k.contype = 'c'
), f AS (
    SELECT conname, refobjid AS fn FROM d WHERE refclassid = 'pg_proc'::regclass
    UNION ALL SELECT d.conname, o.oprcode::oid FROM d JOIN pg_operator o ON o.oid = d.refobjid WHERE d.refclassid = 'pg_operator'::regclass
    UNION ALL SELECT d.conname, c.castfunc FROM d JOIN pg_cast c ON c.oid = d.refobjid WHERE d.refclassid = 'pg_cast'::regclass AND c.castfunc <> 0
)
SELECT f.conname::text AS con, p.oid::regprocedure::text AS fn, p.provolatile::text AS vol FROM f JOIN pg_proc p ON p.oid = f.fn WHERE p.provolatile <> 'i' ORDER BY 1, 2 LIMIT 1";

/// Refuses a CHECK that reaches a function that is not IMMUTABLE. PostgreSQL assumes a CHECK is immutable and runs it on every row
/// written, so a volatile or stable user function there (one that writes, calls out or reads other tables) would act on each insert.
/// Read inside the create's transaction, so a refusal rolls the whole table back.
async fn checks_immutable(tx: &mut Tx<'_>, oid: &str) -> Res<()> {
    let rows = tx.fetch_bound(CHECK_VOLATILE, &[Param::Text(oid.into())]).await.map_err(se)?;
    let Some(r) = rows.first() else { return Ok(()) };
    let (con, f) = (text(r, "con").unwrap_or_default(), text(r, "fn").unwrap_or_default());
    let vol = if text(r, "vol").as_deref() == Some("s") { "STABLE" } else { "VOLATILE" };
    Err(format!("The CHECK {con} calls {f}, which is {vol}. PostgreSQL runs a CHECK on every row written and assumes it is IMMUTABLE, so only IMMUTABLE functions are allowed there. Nothing was created."))
}

async fn create_table(db: &Db, spec: &TableSpec, read_only: bool) -> Res<Made> {
    let stmts = create_table_stmts(spec)?;
    // A key takes SHARE ROW EXCLUSIVE on its parent, as ADD FOREIGN KEY does in the structure editor. The parents are locked first, in
    // the order every plan takes, and read only once they are held, so the key the dialog checked is the one the server sees.
    let locks: Vec<Lock> = spec.parents().into_iter().map(|(s, t)| {
        let name = D.qualified(&s, &t);
        Lock { gone: Some(format!("the referenced table {name} (it is gone)")), name, mode: "SHARE ROW EXCLUSIVE", plain_only: false }
    }).collect();
    let run = run_locked(db, &spec.schema, read_only, &locks, "Refresh the lists and check before running it again.", async |tx| {
        let own = spec.as_structure();
        for k in spec.fks.iter().filter(|k| !spec.self_ref(k)) {
            let (ps, name) = (spec.parent_schema(k), D.qualified(spec.parent_schema(k), &k.ref_table));
            // Read under the parent's own path, as the dialog's read of it was, so both print its types the same way.
            set_path(tx, db, ps).await.map_err(|e| Fail::Run(se(e)))?;
            let parent = read_in(tx, ps, &k.ref_table).await.map_err(Fail::Run)?;
            if !k.ref_seen.is_empty() && ref_state(&parent, &k.ref_cols) != k.ref_seen {
                return Err(Fail::Changed(format!("the referenced table {name}")));
            }
            if let Some(why) = fk_problem(&own, &spec.name, &parent, k, false) {
                return Err(Fail::Run(format!("{why} Nothing was created.")));
            }
        }
        // The types and the default expressions resolve under the schema first and then the Advanced saved list, the path every other writer and the structure editor read under, so a type from a saved schema works here too.
        set_path(tx, db, &spec.schema).await.map_err(|e| Fail::Run(se(e)))?;
        let types: Vec<String> = spec.cols.iter().map(|c| c.ty.trim().to_string()).collect();
        if let Some(bad) = unknown_type(tx, &types).await {
            let col = spec.cols.iter().find(|c| c.ty.trim() == bad).map_or("", |c| c.name.as_str());
            return Err(Fail::Run(format!("Column {col}: {:?} is not a type the server knows. Write only the type, like numeric(10,2), without constraints or other column clauses. Nothing was created.", bad)));
        }
        for (i, s) in stmts.iter().enumerate() {
            exec(tx, db, &spec.schema, s).await.map_err(|x| Fail::Run(lock_error(&x, &spec.name).unwrap_or_else(|| if stmts.len() > 1 { format!("{} (statement {} of {}). Nothing was created.", se(x), i + 1, stmts.len()) } else { se(x) })))?;
        }
        let oid = relation_oid(tx, &D.qualified(&spec.schema, &spec.name)).await.map_err(Fail::Run)?.ok_or_else(|| Fail::Run("The table was made but cannot be read back.".into()))?;
        checks_immutable(tx, &oid).await.map_err(Fail::Run)?;
        let state = state_in(tx, &spec.schema, &spec.name).await.map_err(Fail::Run)?;
        Ok(Made { oid, state })
    })
    .await;
    run.map_err(|f| fail_text(f, |what| format!("Nothing was created: {what} changed since the dialog read it. Pick the key again.")))
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

/// The other tables the foreign keys of `$1` point at.
const KEY_PARENTS: &str = "SELECT DISTINCT n.nspname::text AS s, c.relname::text AS t FROM pg_constraint k JOIN pg_class c ON c.oid = k.confrelid JOIN pg_namespace n ON n.oid = c.relnamespace WHERE k.contype = 'f' AND k.conrelid = to_regclass($1) AND k.confrelid <> k.conrelid";

async fn undo_table(db: &Db, schema: &str, table: &str, made: &Made, read_only: bool) -> Res<()> {
    let sql = drop_table_sql(schema, table, false, false)?;
    let name = D.qualified(schema, table);
    in_tx(db, schema, read_only, async |tx| {
        // Locked before anything is read, so a row cannot arrive between the check and the drop. Without the lock the check
        // could be stale by the time the DROP runs, so a role that cannot take it does not get the undo.
        if !may_lock_one(tx, &name).await {
            return Err(format!("Not undone: {table} cannot be locked with this role's privileges, so it cannot be checked for rows safely. Drop it from its menu instead."));
        }
        // Dropping the table drops its keys, and the server takes ACCESS EXCLUSIVE on each parent to remove the key's triggers there,
        // so the parents are locked with it, in the order every plan takes. A key added or dropped between this read and the locks
        // changes the fingerprint, which refuses the undo below.
        let rows = tx.fetch_bound(KEY_PARENTS, &[Param::Text(name.clone())]).await.map_err(se)?;
        let mut all: Vec<(String, String)> = rows.iter().filter_map(|r| Some((text(r, "s")?, text(r, "t")?))).collect();
        all.push((schema.to_string(), table.to_string()));
        all.sort();
        all.dedup();
        let names: Vec<String> = all.iter().map(|(s, t)| D.qualified(s, t)).collect();
        // A parent the role may not lock is left to the DROP, which takes the lock itself under the same timeout.
        let may = may_lock(tx, &names.iter().map(|n| (n.as_str(), false)).collect::<Vec<_>>()).await;
        let mut skipped = Vec::new();
        for ((n, may), (s, t)) in names.iter().zip(may).zip(&all) {
            let own = s == schema && t == table;
            if !own && !may {
                skipped.push(n.clone());
                continue;
            }
            if let Err(x) = exec(tx, db, schema, &format!("LOCK TABLE {n} IN ACCESS EXCLUSIVE MODE")).await {
                return Err(match (sql_state(&x).as_deref(), lock_error(&x, if own { table } else { n })) {
                    (_, Some(why)) => format!("Not undone: {why}"),
                    (Some("42P01"), _) if own => format!("Not undone: the table {table} is already gone."),
                    _ => format!("Not undone: {}", se(x)),
                });
            }
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
            // Only a parent the role could not lock ahead is still to be locked here, after the table and out of the usual order.
            Some("55P03") if !skipped.is_empty() => format!(
                "Not undone: {} is locked by another session that did not let go within {LOCK_WAIT_SECS} seconds. Dropping {table} removes its foreign keys' triggers there, which needs that table's strongest lock, and this role may not lock it ahead of the drop. Nothing was changed.",
                skipped.join(", ")
            ),
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
        if blocked && let Err(d) = &dropped {
            // The database stays, so it takes connections again as it did before.
            if let Err(x) = alone(&mut c, db, &format!("ALTER DATABASE {} WITH ALLOW_CONNECTIONS true", q(name))).await {
                return Err(format!("{}\nQuarry could not open {name} to connections again ({}). Run ALTER DATABASE {} ALLOW_CONNECTIONS true.", d, se(x), q(name)));
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
    /// The templates, encodings, locales and owners the New database dialog offers.
    pub fn pg_db_choices(&self, conn: u64, db: Db) {
        self.spawn_sql(db.conn, "", async move {
            let res = async {
                let mut c = db.acquire_conn().await?;
                let out = db_choices(&mut c).await;
                let _ = c.close().await;
                out
            }
            .await;
            Msg::PgDbChoices { conn, res }
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

    pub fn pg_create_db(&self, conn: u64, db: Db, name: String, opts: DbOpts, read_only: bool) {
        self.spawn_sql(db.conn, "", async move {
            let res = create_db(&db, &name, &opts, read_only).await;
            let undo = res.as_ref().ok().filter(|m| m.no_undo.is_none()).map(|m| entry(&db, conn, &name, "", SqlUndo::CreatedDb { oid: m.oid.clone(), baseline: m.baseline }));
            let said = match res.as_ref().ok().and_then(|m| m.no_undo.as_ref()) {
                Some(why) => format!("Created database {name}. {why}"),
                None => format!("Created database {name}"),
            };
            let (res, dbs) = answer(res.map(|m| Some(m.dbs)), said);
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
pub(in crate::db::sql) async fn undo(db: Db, a: UndoAction, read_only: bool) -> Msg {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> TableSpec {
        let id = ColDef { auto_increment: true, nullable: false, ..ColDef::new("id", "integer") };
        TableSpec { schema: "app".into(), name: "orders".into(), cols: vec![id, ColDef::new("customer", "bigint"), ColDef::new("Code", "text"), ColDef::new("price", "numeric(10,2)")], pk: vec!["id".into()], ..Default::default() }
    }

    fn fk(cols: &[&str], schema: &str, table: &str, refs: &[&str]) -> ForeignKey {
        let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect();
        ForeignKey { cols: v(cols), ref_db: schema.into(), ref_table: table.into(), ref_cols: v(refs), on_delete: "NO ACTION".into(), on_update: "NO ACTION".into(), ..Default::default() }
    }

    fn body(s: &TableSpec) -> String {
        create_table_stmts(s).unwrap_or_else(|e| panic!("{e}"))[0].clone()
    }

    #[test]
    fn a_unique_over_two_columns_is_named_by_the_server_unless_the_person_names_it() {
        let mut s = spec();
        s.uniques = vec![Unique { name: String::new(), cols: vec!["customer".into(), "Code".into()] }, Unique { name: "one \"code\"".into(), cols: vec!["Code".into()] }];
        let b = body(&s);
        assert!(b.contains("  UNIQUE (\"customer\", \"Code\")"), "{b}");
        assert!(b.contains("  CONSTRAINT \"one \"\"code\"\"\" UNIQUE (\"Code\")"), "{b}");
        s.uniques[0].name = "x".repeat(64);
        assert!(create_table_stmts(&s).unwrap_err().contains("63 bytes"));
        s.uniques[0] = Unique { name: String::new(), cols: vec!["customer".into(), "nope".into()] };
        assert!(create_table_stmts(&s).unwrap_err().contains("not a column"));
    }

    #[test]
    fn a_check_is_one_expression_without_calls_that_act_outside_the_row() {
        let mut s = spec();
        s.checks = vec![Check { name: String::new(), expr: " price >= 0 ".into() }, Check { name: "code_set".into(), expr: "length(\"Code\") > 0 AND \"Code\" <> 'x;y'".into() }];
        let b = body(&s);
        assert!(b.contains("  CHECK (price >= 0)") && b.contains("  CONSTRAINT \"code_set\" CHECK (length(\"Code\") > 0 AND \"Code\" <> 'x;y')"), "{b}");
        for bad in ["price > 0); DROP TABLE x; --", "price > 0 -- x", "price > (0", "", "pg_advisory_lock(1) IS NOT NULL", "dblink_exec('x', 'DROP TABLE y') = 'OK'", "(1).pg_terminate_backend", "\"lo_unlink\"(1) = 1", "pg_catalog.pg_reload_conf()", "U&\"\\0070g_sleep\"(1) IS NULL", "pg_sleep(1) IS NULL", "pg_catalog.pg_sleep_for('1s') IS NULL", "(1).pg_sleep IS NULL"] {
            s.checks = vec![Check { name: String::new(), expr: bad.into() }];
            assert!(create_table_stmts(&s).is_err(), "{bad:?} was not refused");
        }
        s.checks = vec![Check { name: String::new(), expr: "pg_advisory_lock(1) IS NOT NULL".into() }];
        assert!(create_table_stmts(&s).unwrap_err().contains("pg_advisory_lock"));
    }

    #[test]
    fn a_foreign_key_names_its_parent_with_the_schema_and_keeps_its_options() {
        let mut s = spec();
        let mut k = fk(&["customer"], "sales", "Customers", &["id"]);
        (k.on_delete, k.on_update, k.match_full, k.deferrable, k.deferred) = ("cascade".into(), "SET NULL".into(), true, true, true);
        s.fks = vec![k, ForeignKey { name: "to_me".into(), ..fk(&["customer"], "", "orders", &["id"]) }];
        let b = body(&s);
        assert!(b.contains("  FOREIGN KEY (\"customer\") REFERENCES \"sales\".\"Customers\" (\"id\") MATCH FULL ON DELETE CASCADE ON UPDATE SET NULL DEFERRABLE INITIALLY DEFERRED"), "{b}");
        // An empty ref_db is the table's own schema, and a key on the table itself leans on its primary key.
        assert!(b.contains("  CONSTRAINT \"to_me\" FOREIGN KEY (\"customer\") REFERENCES \"app\".\"orders\" (\"id\") ON DELETE NO ACTION ON UPDATE NO ACTION"), "{b}");
        assert_eq!(s.parents(), vec![("sales".to_string(), "Customers".to_string())], "the table itself is no parent to lock");
        let refused = |k: ForeignKey| {
            let mut s = spec();
            s.fks = vec![k];
            create_table_stmts(&s).unwrap_err()
        };
        assert!(refused(fk(&["Code"], "", "orders", &["Code"])).contains("primary key or unique constraint"), "a key on itself needs a key of its own there");
        assert!(refused(fk(&["customer", "Code"], "s", "p", &["id"])).contains("same number"));
        assert!(refused(fk(&["nope"], "s", "p", &["id"])).contains("not a column"));
        assert!(refused(ForeignKey { on_delete: "DROP".into(), ..fk(&["customer"], "s", "p", &["id"]) }).contains("not a foreign key action"));
        assert!(refused(ForeignKey { deferred: true, ..fk(&["customer"], "s", "p", &["id"]) }).contains("DEFERRABLE"));
        assert!(refused(ForeignKey { not_valid: true, ..fk(&["customer"], "s", "p", &["id"]) }).contains("NOT VALID"));
        assert!(refused(ForeignKey { name: "k".repeat(64), ..fk(&["customer"], "s", "p", &["id"]) }).contains("63 bytes"));
        // Two keys on one parent lock it once, and the parents go by schema, then name.
        let mut s = spec();
        s.fks = vec![fk(&["customer"], "b", "t", &["id"]), fk(&["id"], "a", "z", &["id"]), fk(&["Code"], "b", "t", &["c"])];
        assert_eq!(s.parents(), vec![("a".into(), "z".into()), ("b".into(), "t".into())]);
    }

    #[test]
    fn indexes_come_after_the_table_and_names_are_not_given_twice() {
        let mut s = spec();
        s.indexes = vec![NewIndex { name: String::new(), cols: vec!["customer".into(), "price".into()], unique: false }, NewIndex { name: "by code".into(), cols: vec!["Code".into()], unique: true }];
        s.comment = "c".into();
        let all = create_table_stmts(&s).unwrap();
        assert_eq!(all[1], "CREATE INDEX ON \"app\".\"orders\" (\"customer\", \"price\")");
        assert_eq!(all[2], "CREATE UNIQUE INDEX \"by code\" ON \"app\".\"orders\" (\"Code\")");
        assert!(all[3].starts_with("COMMENT ON TABLE"), "the comments come last");
        s.checks = vec![Check { name: "by code".into(), expr: "true".into() }];
        assert!(create_table_stmts(&s).unwrap_err().contains("Two constraints or indexes are called by code"));
        s.checks.clear();
        s.indexes[0].cols.clear();
        assert!(create_table_stmts(&s).unwrap_err().contains("at least one column"));
    }

    #[test]
    fn the_spec_reads_as_a_structure_for_the_key_checks() {
        let mut s = spec();
        s.uniques = vec![Unique { name: "u".into(), cols: vec!["customer".into(), "Code".into()] }];
        s.indexes = vec![NewIndex { name: String::new(), cols: vec!["price".into()], unique: true }];
        let st = s.as_structure();
        assert!(st.is_pg() && st.columns.len() == 4 && !st.columns[0].nullable && st.columns[1].nullable);
        assert_eq!(st.indexes.iter().map(|i| i.cols.len()).collect::<Vec<_>>(), [1, 2], "the key and the UNIQUE, not the index made after the table");
    }

    fn choices() -> DbChoices {
        let t = |name: &str| Template { name: name.into(), encoding: "UTF8".into(), collate: "en_US.utf8".into(), ctype: "en_US.utf8".into(), provider: "c".into(), icu: String::new() };
        DbChoices {
            version: 180001,
            templates: vec![t("template0"), t("template1")],
            encodings: vec!["LATIN1".into(), "SQL_ASCII".into(), "UTF8".into()],
            locales: vec![("C".into(), String::new()), ("POSIX".into(), String::new()), ("en_US.utf8".into(), "UTF8".into()), ("de_DE".into(), "LATIN1".into())],
            icu_locales: vec!["en-US".into(), "und".into()],
            owners: vec!["app_owner".into(), "me".into()],
            me: "me".into(),
        }
    }

    #[test]
    fn a_database_takes_an_encoding_a_locale_and_an_owner_on_template0() {
        let ch = choices();
        let o = DbOpts { template: Some("template0".into()), encoding: Some("UTF8".into()), locale: Some("C".into()), owner: Some("app_owner".into()), ..Default::default() };
        assert_eq!(create_db_sql("app", &o, &ch).unwrap(), "CREATE DATABASE \"app\" OWNER \"app_owner\" TEMPLATE \"template0\" ENCODING 'UTF8' LOCALE 'C'");
        let o = DbOpts { template: Some("template0".into()), encoding: Some("LATIN1".into()), lc_collate: Some("de_DE".into()), lc_ctype: Some("C".into()), ..Default::default() };
        assert_eq!(create_db_sql("app", &o, &ch).unwrap(), "CREATE DATABASE \"app\" TEMPLATE \"template0\" ENCODING 'LATIN1' LC_COLLATE 'de_DE' LC_CTYPE 'C'");
        let o = DbOpts { template: Some("template0".into()), provider: Some("icu".into()), icu_locale: Some("und".into()), ..Default::default() };
        assert_eq!(create_db_sql("app", &o, &ch).unwrap(), "CREATE DATABASE \"app\" TEMPLATE \"template0\" LOCALE_PROVIDER icu ICU_LOCALE 'und'");
        // The template's own settings need no template0.
        let o = DbOpts { encoding: Some("UTF8".into()), locale: Some("en_US.utf8".into()), ..Default::default() };
        assert!(create_db_sql("app", &o, &ch).is_ok());
    }

    #[test]
    fn database_options_the_server_or_the_template_would_refuse_are_refused_first() {
        let ch = choices();
        let on0 = |o: DbOpts| DbOpts { template: Some("template0".into()), ..o };
        let why = |o: DbOpts| create_db_sql("app", &o, &ch).unwrap_err();
        assert!(why(DbOpts { encoding: Some("LATIN1".into()), ..Default::default() }).contains("needs TEMPLATE template0"));
        assert!(why(DbOpts { template: Some("template1".into()), locale: Some("C".into()), ..Default::default() }).contains("needs TEMPLATE template0"));
        assert!(why(on0(DbOpts { encoding: Some("KOI8X".into()), ..Default::default() })).contains("not an encoding"));
        assert!(why(on0(DbOpts { encoding: Some("LATIN1".into()), locale: Some("en_US.utf8".into()), ..Default::default() })).contains("does not go with the encoding LATIN1"));
        assert!(why(on0(DbOpts { locale: Some("C".into()), lc_ctype: Some("C".into()), ..Default::default() })).contains("not both"));
        assert!(why(on0(DbOpts { locale: Some("C'; DROP DATABASE x; --".into()), ..Default::default() })).contains("not a locale"));
        assert!(why(on0(DbOpts { icu_locale: Some("und".into()), ..Default::default() })).contains("needs the ICU provider"));
        assert!(why(on0(DbOpts { provider: Some("icu".into()), ..Default::default() })).contains("Pick an ICU locale"));
        assert!(why(on0(DbOpts { provider: Some("builtin".into()), ..Default::default() })).contains("not a locale provider"));
        assert!(why(DbOpts { owner: Some("someone".into()), ..Default::default() }).contains("not a role"));
        // Before 15 there is no provider to pick, and a server without ICU offers none either.
        let old = DbChoices { version: 130023, icu_locales: Vec::new(), ..choices() };
        assert!(create_db_sql("app", &on0(DbOpts { provider: Some("icu".into()), icu_locale: Some("und".into()), ..Default::default() }), &old).unwrap_err().contains("15 or later"));
        assert!(!old.icu_ok() && ch.icu_ok());
        let no_icu = DbChoices { icu_locales: Vec::new(), ..choices() };
        assert!(create_db_sql("app", &on0(DbOpts { provider: Some("icu".into()), icu_locale: Some("und".into()), ..Default::default() }), &no_icu).unwrap_err().contains("without ICU"));
        // A value the server listed with a quote in it is still never written into the statement.
        let odd = DbChoices { locales: vec![("x'y".into(), String::new())], ..choices() };
        assert!(create_db_sql("app", &on0(DbOpts { locale: Some("x'y".into()), ..Default::default() }), &odd).is_err());
    }

    /// A cluster made with ICU (or builtin on 17) whose templates still carry a libc LC_COLLATE and LC_CTYPE.
    fn icu_cluster(version: u32, provider: &str) -> DbChoices {
        let mut ch = DbChoices { version, ..choices() };
        for t in &mut ch.templates {
            (t.provider, t.icu) = (provider.into(), if provider == "b" { "C.UTF-8".into() } else { "en-US".into() });
        }
        ch
    }

    #[test]
    fn locale_is_not_offered_where_it_would_also_set_the_icu_or_builtin_locale() {
        let o = DbOpts { template: Some("template0".into()), locale: Some("C".into()), ..Default::default() };
        // 15 reads LOCALE as LC_COLLATE and LC_CTYPE only; 16 and later also as the ICU or builtin locale.
        assert!(create_db_sql("app", &o, &icu_cluster(150004, "i")).is_ok());
        for (v, p, word) in [(160002, "i", "icu"), (170000, "b", "builtin")] {
            let ch = icu_cluster(v, p);
            assert!(!ch.locale_word_ok(&o));
            let why = create_db_sql("app", &o, &ch).unwrap_err();
            assert!(why.contains(&format!("the {word} locale")) && why.contains("LC_COLLATE and LC_CTYPE"), "{why}");
            let both = DbOpts { locale: None, lc_collate: Some("C".into()), lc_ctype: Some("C".into()), ..o.clone() };
            assert!(create_db_sql("app", &both, &ch).is_ok(), "{v}");
            // With libc picked, LOCALE means the same everywhere.
            let libc = DbOpts { provider: Some("libc".into()), ..o.clone() };
            assert!(ch.locale_word_ok(&libc) && create_db_sql("app", &libc, &ch).is_ok(), "{v}");
        }
        assert_eq!((provider_name("b"), provider_name("i"), provider_name("c")), ("builtin", "icu", "libc"));
    }

    #[test]
    fn an_encoding_left_with_the_templates_locale_is_checked_and_the_templates_locale_is_offered() {
        let ch = choices();
        let on0 = |o: DbOpts| DbOpts { template: Some("template0".into()), ..o };
        // template0 is en_US.utf8, which is UTF8 only.
        let why = create_db_sql("app", &on0(DbOpts { encoding: Some("LATIN1".into()), ..Default::default() }), &ch).unwrap_err();
        assert!(why.contains("template's locale en_US.utf8") && why.contains("LATIN1"), "{why}");
        assert_eq!(ch.template_locale_problem(&on0(DbOpts { encoding: Some("LATIN1".into()), ..Default::default() })), Some(why));
        assert!(create_db_sql("app", &on0(DbOpts { encoding: Some("LATIN1".into()), locale: Some("C".into()), ..Default::default() }), &ch).is_ok());
        // Only LC_CTYPE picked: LC_COLLATE stays the template's and is checked.
        assert!(create_db_sql("app", &on0(DbOpts { encoding: Some("LATIN1".into()), lc_ctype: Some("C".into()), ..Default::default() }), &ch).is_err());
        assert!(create_db_sql("app", &on0(DbOpts { encoding: Some("UTF8".into()), ..Default::default() }), &ch).is_ok());
        assert!(create_db_sql("app", &on0(DbOpts { encoding: Some("SQL_ASCII".into()), ..Default::default() }), &ch).is_ok(), "left to the server");
        // An image with no imported OS locales lists C and POSIX only, and still offers and takes the template's own locale.
        let bare = DbChoices { locales: vec![("C".into(), String::new()), ("POSIX".into(), String::new())], ..choices() };
        let t0 = bare.template(Some("template0"));
        assert_eq!(bare.locales_with("UTF8", t0), ["C", "POSIX", "en_US.utf8"]);
        assert_eq!(bare.locales_with("LATIN1", t0), ["C", "POSIX"]);
        assert!(create_db_sql("app", &on0(DbOpts { encoding: Some("UTF8".into()), locale: Some("en_US.utf8".into()), ..Default::default() }), &bare).is_ok());
        assert!(create_db_sql("app", &on0(DbOpts { encoding: Some("LATIN1".into()), locale: Some("en_US.utf8".into()), ..Default::default() }), &bare).unwrap_err().contains("does not go with"));
    }
}
