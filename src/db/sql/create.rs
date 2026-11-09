//! Creating and dropping databases and tables. Every statement is built here from quoted identifiers,
//! numbers checked digit by digit and charset, collation and engine names that must appear in the
//! lists the server itself gave, so no user text ever reaches the server as SQL. Each statement is
//! sent alone on a connection of its own that is switched to READ WRITE, like the row writes.

use super::value::lit;
use super::worker::{raw, text};
use super::write::{writer, SqlUndo};
use super::{Db, Dialect, Pool};
use crate::db::{e, Msg, Res, UndoAction, UndoKind, Worker};
use sqlx::{Connection, Executor};
use std::time::{Duration, Instant};

/// Databases the server owns; creating or dropping them is refused before a statement exists.
pub const SYSTEM_SCHEMAS: [&str; 4] = ["information_schema", "mysql", "performance_schema", "sys"];

/// What a finished create or drop changed, so the app knows what to refresh.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ddl {
    CreatedDb,
    CreatedTable,
    DroppedDb,
    DroppedTable,
    Emptied,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Charset {
    pub name: String,
    pub desc: String,
    pub default_collation: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Collation {
    pub name: String,
    pub charset: String,
    pub is_default: bool,
}

/// What the server offers for new databases and tables, read from information_schema.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Meta {
    pub charsets: Vec<Charset>,
    pub collations: Vec<Collation>,
    /// Usable storage engines, by the server's own spelling.
    pub engines: Vec<String>,
    pub server_charset: String,
    pub server_collation: String,
    pub server_engine: String,
    /// The default collation of the database a new table goes into, when there is one.
    pub schema_collation: Option<String>,
}

impl Meta {
    pub fn loaded(&self) -> bool {
        !self.charsets.is_empty() && !self.collations.is_empty()
    }

    pub fn collations_of<'a>(&'a self, charset: &'a str) -> impl Iterator<Item = &'a Collation> {
        self.collations.iter().filter(move |c| c.charset == charset)
    }

    pub fn default_collation(&self, charset: &str) -> Option<&str> {
        self.charsets.iter().find(|c| c.name == charset).map(|c| c.default_collation.as_str()).filter(|c| !c.is_empty())
    }

    /// The collation a new table or database starts with: the server's own when it is listed.
    pub fn starting_collation(&self, preferred: Option<&str>) -> String {
        [preferred, Some(self.server_collation.as_str())].into_iter().flatten().find(|c| self.charset_of(c).is_some()).map(String::from).or_else(|| self.default_collation(&self.server_charset).map(String::from)).unwrap_or_default()
    }

    pub fn charset_of(&self, collation: &str) -> Option<&str> {
        self.collations.iter().find(|c| c.name == collation).map(|c| c.charset.as_str())
    }
}

/// What the confirmation dialogs say will be lost. Counts of a database, or the rows of a table and
/// the tables that hold a foreign key to it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DropInfo {
    pub exists: bool,
    pub rows: Option<u64>,
    /// The count is the server's estimate, because counting took too long.
    pub rows_estimated: bool,
    pub referenced_by: Vec<String>,
    pub tables: u64,
    pub views: u64,
    pub triggers: u64,
    pub routines: u64,
    pub events: u64,
}

// Identifiers.

/// MySQL's own limits plus what quoting cannot make safe: control characters would make a name that
/// no one can read back, and a trailing space is stripped by some versions and refused by others.
/// Databases and tables are also directories and files, so path characters are refused for them.
pub fn check_name(what: &str, name: &str, path_chars: bool) -> Res<()> {
    if name.is_empty() {
        return Err(format!("Give the {what} a name."));
    }
    if name.chars().count() > 64 {
        return Err(format!("A {what} name is at most 64 characters."));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(format!("A {what} name cannot contain control characters or line breaks."));
    }
    if name.starts_with(' ') || name.ends_with(' ') {
        return Err(format!("A {what} name cannot start or end with a space."));
    }
    if path_chars && name.contains(['/', '\\', '.']) {
        return Err(format!("A {what} name cannot contain / \\ or a dot."));
    }
    Ok(())
}

fn check_schema(name: &str) -> Res<()> {
    check_name("database", name, true)
}

fn not_system(name: &str) -> Res<()> {
    if SYSTEM_SCHEMAS.iter().any(|s| s.eq_ignore_ascii_case(name)) {
        return Err(format!("{name} belongs to the server and is left alone."));
    }
    Ok(())
}

/// A charset, collation or engine name as the server spells it. The list is the server's own, and the
/// name must also be plain words, so even a hostile list could not carry SQL.
fn listed<'a>(what: &str, name: &str, known: impl IntoIterator<Item = &'a str>) -> Res<&'a str> {
    let plain = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    known.into_iter().find(|k| plain && k.eq_ignore_ascii_case(name) && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).ok_or_else(|| format!("The server has no {what} called {name}."))
}

pub fn create_db_sql(d: Dialect, name: &str, charset: &str, collation: &str, meta: &Meta) -> Res<String> {
    check_schema(name)?;
    not_system(name)?;
    let cs = listed("charset", charset, meta.charsets.iter().map(|c| c.name.as_str()))?;
    let co = listed("collation", collation, meta.collations.iter().map(|c| c.name.as_str()))?;
    if meta.charset_of(co) != Some(cs) {
        return Err(format!("{co} does not belong to {cs}."));
    }
    Ok(format!("CREATE DATABASE {}\n  CHARACTER SET {cs} COLLATE {co}", d.quote_ident(name)))
}

pub fn drop_db_sql(d: Dialect, name: &str) -> Res<String> {
    check_schema(name)?;
    not_system(name)?;
    Ok(format!("DROP DATABASE {}", d.quote_ident(name)))
}

pub fn drop_table_sql(d: Dialect, schema: &str, table: &str, view: bool) -> Res<String> {
    check_schema(schema)?;
    not_system(schema)?;
    check_name("table", table, true)?;
    Ok(format!("DROP {} {}", if view { "VIEW" } else { "TABLE" }, d.qualified(schema, table)))
}

pub fn truncate_sql(d: Dialect, schema: &str, table: &str) -> Res<String> {
    check_schema(schema)?;
    not_system(schema)?;
    check_name("table", table, true)?;
    Ok(format!("TRUNCATE TABLE {}", d.qualified(schema, table)))
}

// Column types.

/// What a type takes inside its parentheses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    /// Nothing: INT, TEXT, DATE.
    Plain,
    /// A count of characters or bytes, `(min..=max)`; `required` types cannot leave it out.
    Size { min: u32, max: u32, required: bool },
    /// DECIMAL's `(precision, scale)`.
    Precision,
    /// Fractional seconds, `(0..=6)`.
    Fsp,
}

#[derive(Clone, Copy, Debug)]
pub struct TypeDef {
    pub name: &'static str,
    pub shape: Shape,
    /// Numbers: UNSIGNED applies and a default is a number.
    pub numeric: bool,
    pub integer: bool,
    /// TEXT, BLOB and JSON take no literal default.
    pub no_default: bool,
    pub temporal_now: bool,
}

const fn t(name: &'static str, shape: Shape, numeric: bool, integer: bool, no_default: bool, temporal_now: bool) -> TypeDef {
    TypeDef { name, shape, numeric, integer, no_default, temporal_now }
}

pub const TYPES: &[TypeDef] = &[
    t("TINYINT", Shape::Plain, true, true, false, false),
    t("SMALLINT", Shape::Plain, true, true, false, false),
    t("MEDIUMINT", Shape::Plain, true, true, false, false),
    t("INT", Shape::Plain, true, true, false, false),
    t("BIGINT", Shape::Plain, true, true, false, false),
    t("DECIMAL", Shape::Precision, true, false, false, false),
    t("FLOAT", Shape::Plain, true, false, false, false),
    t("DOUBLE", Shape::Plain, true, false, false, false),
    t("BOOLEAN", Shape::Plain, true, false, false, false),
    t("CHAR", Shape::Size { min: 0, max: 255, required: false }, false, false, false, false),
    t("VARCHAR", Shape::Size { min: 1, max: 65535, required: true }, false, false, false, false),
    t("BINARY", Shape::Size { min: 1, max: 255, required: false }, false, false, false, false),
    t("VARBINARY", Shape::Size { min: 1, max: 65535, required: true }, false, false, false, false),
    t("TINYTEXT", Shape::Plain, false, false, true, false),
    t("TEXT", Shape::Plain, false, false, true, false),
    t("MEDIUMTEXT", Shape::Plain, false, false, true, false),
    t("LONGTEXT", Shape::Plain, false, false, true, false),
    t("TINYBLOB", Shape::Plain, false, false, true, false),
    t("BLOB", Shape::Plain, false, false, true, false),
    t("MEDIUMBLOB", Shape::Plain, false, false, true, false),
    t("LONGBLOB", Shape::Plain, false, false, true, false),
    t("JSON", Shape::Plain, false, false, true, false),
    t("DATE", Shape::Plain, false, false, false, false),
    t("TIME", Shape::Fsp, false, false, false, false),
    t("DATETIME", Shape::Fsp, false, false, false, true),
    t("TIMESTAMP", Shape::Fsp, false, false, false, true),
    t("YEAR", Shape::Plain, false, false, false, false),
];

pub fn type_def(name: &str) -> Option<&'static TypeDef> {
    TYPES.iter().find(|d| d.name.eq_ignore_ascii_case(name))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum DefaultKind {
    #[default]
    None,
    Null,
    /// CURRENT_TIMESTAMP, for DATETIME and TIMESTAMP.
    Now,
    /// The text of `ColumnSpec::default_text`, as a number or a string by the type.
    Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColumnSpec {
    pub name: String,
    pub ty: String,
    /// The text between the parentheses, such as `255`, `10,2` or `3`.
    pub len: String,
    pub unsigned: bool,
    pub nullable: bool,
    pub pk: bool,
    pub auto: bool,
    pub default: DefaultKind,
    pub default_text: String,
}

impl Default for ColumnSpec {
    fn default() -> Self {
        ColumnSpec { name: String::new(), ty: "VARCHAR".into(), len: "255".into(), unsigned: false, nullable: true, pk: false, auto: false, default: DefaultKind::None, default_text: String::new() }
    }
}

impl ColumnSpec {
    /// The preset first column: an unsigned auto-increment key.
    pub fn id() -> ColumnSpec {
        ColumnSpec { name: "id".into(), ty: "INT".into(), len: String::new(), unsigned: true, nullable: false, pk: true, auto: true, ..Default::default() }
    }
}

#[derive(Clone, Debug)]
pub struct TableSpec {
    pub schema: String,
    pub name: String,
    pub engine: String,
    pub collation: String,
    pub columns: Vec<ColumnSpec>,
}

fn digits(s: &str) -> Option<u32> {
    (!s.is_empty() && s.len() <= 6 && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten()
}

/// `-12` or `3.50`: digits only, so the text can be written into the statement as it is.
fn plain_number(s: &str) -> bool {
    let s = s.strip_prefix('-').unwrap_or(s);
    let (whole, frac) = s.split_once('.').map_or((s, None), |(w, f)| (w, Some(f)));
    let ok = |p: &str| !p.is_empty() && p.len() <= 66 && p.bytes().all(|b| b.is_ascii_digit());
    ok(whole) && frac.is_none_or(ok)
}

/// The type as written in the statement, such as `DECIMAL(10,2) UNSIGNED`.
fn type_text(c: &ColumnSpec) -> Res<(String, &'static TypeDef)> {
    let def = type_def(&c.ty).ok_or_else(|| format!("Column {}: {} is not a type this dialog offers.", c.name, c.ty))?;
    let len = c.len.trim();
    let bad = |why: &str| format!("Column {}: {why}", c.name);
    let args = match def.shape {
        Shape::Plain if len.is_empty() => String::new(),
        Shape::Plain => return Err(bad(&format!("{} takes no length.", def.name))),
        Shape::Size { min, max, required } => match (len.is_empty(), digits(len)) {
            (true, _) if required => return Err(bad(&format!("{} needs a length.", def.name))),
            (true, _) => String::new(),
            (false, Some(n)) if (min..=max).contains(&n) => format!("({n})"),
            _ => return Err(bad(&format!("the length of {} is a number from {min} to {max}.", def.name))),
        },
        Shape::Fsp => match (len.is_empty(), digits(len)) {
            (true, _) => String::new(),
            (false, Some(n)) if n <= 6 => format!("({n})"),
            _ => return Err(bad("fractional seconds are a number from 0 to 6.")),
        },
        Shape::Precision if len.is_empty() => String::new(),
        Shape::Precision => {
            let (p, s) = len.split_once(',').map_or((len, "0"), |(p, s)| (p.trim(), s.trim()));
            match (digits(p), digits(s)) {
                (Some(p), Some(s)) if (1..=65).contains(&p) && s <= 30 && s <= p => format!("({p},{s})"),
                _ => return Err(bad("DECIMAL is written as precision,scale, such as 10,2 (precision up to 65, scale up to 30 and not above the precision).")),
            }
        }
    };
    let unsigned = if c.unsigned && def.numeric && def.name != "BOOLEAN" { " UNSIGNED" } else { "" };
    Ok((format!("{}{args}{unsigned}", def.name), def))
}

fn default_text(bs: bool, c: &ColumnSpec, def: &TypeDef) -> Res<String> {
    match c.default {
        DefaultKind::None => Ok(String::new()),
        DefaultKind::Null if c.nullable && !c.pk => Ok(" DEFAULT NULL".into()),
        DefaultKind::Null => Err(format!("Column {}: DEFAULT NULL needs a column that allows NULL.", c.name)),
        DefaultKind::Now if def.temporal_now => {
            let fsp = c.len.trim();
            Ok(if fsp.is_empty() { " DEFAULT CURRENT_TIMESTAMP".into() } else { format!(" DEFAULT CURRENT_TIMESTAMP({})", digits(fsp).ok_or_else(|| format!("Column {}: fractional seconds are a number from 0 to 6.", c.name))?) })
        }
        DefaultKind::Now => Err(format!("Column {}: only DATETIME and TIMESTAMP can default to the current time.", c.name)),
        DefaultKind::Value if def.no_default => Err(format!("Column {}: {} cannot have a default value.", c.name, def.name)),
        DefaultKind::Value if def.numeric => {
            let v = c.default_text.trim();
            if plain_number(v) { Ok(format!(" DEFAULT {v}")) } else { Err(format!("Column {}: the default of a number column is a number such as 0 or -1.5.", c.name)) }
        }
        DefaultKind::Value => Ok(format!(" DEFAULT {}", lit(&c.default_text, bs))),
    }
}

/// The CREATE TABLE statement, or the first thing wrong with the form.
pub fn create_table_sql(d: Dialect, bs: bool, spec: &TableSpec, meta: &Meta) -> Res<String> {
    check_schema(&spec.schema)?;
    not_system(&spec.schema)?;
    check_name("table", &spec.name, true)?;
    if !meta.loaded() {
        return Err("The server's engines and collations are still loading.".into());
    }
    let engine = listed("engine", &spec.engine, meta.engines.iter().map(String::as_str))?;
    let co = listed("collation", &spec.collation, meta.collations.iter().map(|c| c.name.as_str()))?;
    let cs = meta.charset_of(co).ok_or_else(|| format!("The server has no collation called {co}."))?;
    if spec.columns.is_empty() {
        return Err("Add at least one column.".into());
    }
    let mut seen = std::collections::HashSet::new();
    let mut lines = Vec::new();
    let mut keys = Vec::new();
    let mut autos = 0;
    for c in &spec.columns {
        check_name("column", &c.name, false)?;
        if !seen.insert(c.name.to_lowercase()) {
            return Err(format!("Two columns are called {}.", c.name));
        }
        let (ty, def) = type_text(c)?;
        let mut line = format!("  {} {ty}{}", d.quote_ident(&c.name), if c.nullable && !c.pk { " NULL" } else { " NOT NULL" });
        if c.auto {
            autos += 1;
            if !def.integer || !c.pk {
                return Err(format!("Column {}: AUTO_INCREMENT needs an integer primary key column.", c.name));
            }
            if c.default != DefaultKind::None {
                return Err(format!("Column {}: an AUTO_INCREMENT column has no default.", c.name));
            }
            line.push_str(" AUTO_INCREMENT");
        }
        line.push_str(&default_text(bs, c, def)?);
        if c.pk {
            keys.push(d.quote_ident(&c.name));
        }
        lines.push(line);
    }
    if autos > 1 {
        return Err("Only one column can be AUTO_INCREMENT.".into());
    }
    if !keys.is_empty() {
        lines.push(format!("  PRIMARY KEY ({})", keys.join(", ")));
    }
    Ok(format!("CREATE TABLE {} (\n{}\n) ENGINE={engine} DEFAULT CHARSET={cs} COLLATE={co}", d.qualified(&spec.schema, &spec.name), lines.join(",\n")))
}

/// How a statement is shown in a preview: with its closing semicolon.
pub fn shown(sql: &str) -> String {
    format!("{sql};")
}

// Reading from the server.

const CHARSETS: &str = "SELECT CHARACTER_SET_NAME AS name, DESCRIPTION AS descr, DEFAULT_COLLATE_NAME AS def FROM information_schema.CHARACTER_SETS ORDER BY CHARACTER_SET_NAME";
const COLLATIONS: &str = "SELECT COLLATION_NAME AS name, CHARACTER_SET_NAME AS charset FROM information_schema.COLLATIONS ORDER BY COLLATION_NAME";
const ENGINES: &str = "SELECT ENGINE AS name, SUPPORT AS support FROM information_schema.ENGINES ORDER BY ENGINE";
const SERVER_DEFAULTS: &str = "SELECT @@character_set_server AS cs, @@collation_server AS co, @@default_storage_engine AS en";
const SCHEMA_DEFAULT: &str = "SELECT DEFAULT_COLLATION_NAME AS co FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = ?";

async fn load_meta(db: &Db, schema: &str) -> Res<Meta> {
    let Pool::My(pool) = &db.pool;
    let (cs, co, en, df) = tokio::try_join!(
        async { sqlx::query(CHARSETS).fetch_all(pool).await.map_err(e) },
        async { sqlx::query(COLLATIONS).fetch_all(pool).await.map_err(e) },
        async { sqlx::query(ENGINES).fetch_all(pool).await.map_err(e) },
        async { sqlx::query(SERVER_DEFAULTS).fetch_one(pool).await.map_err(e) },
    )?;
    let charsets: Vec<Charset> = cs.iter().filter_map(|r| Some(Charset { name: text(r, "name")?, desc: text(r, "descr").unwrap_or_default(), default_collation: text(r, "def").unwrap_or_default() })).collect();
    let mut meta = Meta {
        collations: collations_of_rows(&co, &charsets),
        charsets,
        engines: en.iter().filter(|r| text(r, "support").is_some_and(|s| matches!(s.to_ascii_uppercase().as_str(), "YES" | "DEFAULT"))).filter_map(|r| text(r, "name")).collect(),
        server_charset: text(&df, "cs").unwrap_or_default(),
        server_collation: text(&df, "co").unwrap_or_default(),
        server_engine: text(&df, "en").unwrap_or_default(),
        schema_collation: None,
    };
    if !schema.is_empty() {
        meta.schema_collation = sqlx::query(SCHEMA_DEFAULT).bind(schema).fetch_optional(pool).await.map_err(e)?.and_then(|r| text(&r, "co"));
    }
    Ok(meta)
}

/// MariaDB lists its uca1400 collations once, without a charset, and accepts them spelled with any
/// Unicode charset in front (`utf8mb4_uca1400_ai_ci`), which is also how the server reports its own
/// default. Those are added per charset so the list holds every name the server takes.
fn collations_of_rows(rows: &[sqlx::mysql::MySqlRow], charsets: &[Charset]) -> Vec<Collation> {
    let mut out = Vec::new();
    for r in rows {
        let Some(name) = text(r, "name") else { continue };
        match text(r, "charset").filter(|c| !c.is_empty()) {
            Some(charset) => out.push(Collation { name, charset, is_default: false }),
            None => out.extend(charsets.iter().filter(|c| matches!(c.name.as_str(), "utf8mb3" | "utf8mb4" | "ucs2" | "utf16" | "utf32")).map(|c| Collation { name: format!("{}_{name}", c.name), charset: c.name.clone(), is_default: false })),
        }
    }
    // The charset's own default decides, whichever way the server spells its IS_DEFAULT column.
    for c in &mut out {
        c.is_default = charsets.iter().any(|s| s.name == c.charset && s.default_collation == c.name);
    }
    out
}

async fn count(pool: &sqlx::MySqlPool, sql: &str, schema: &str) -> u64 {
    sqlx::query(sql).bind(schema).fetch_one(pool).await.ok().and_then(|r| super::structure::num(&r, "n")).unwrap_or(0)
}

/// Counts for the database confirmation: the sum is what DROP DATABASE takes with it.
async fn db_info(db: &Db, schema: &str) -> Res<DropInfo> {
    let Pool::My(pool) = &db.pool;
    let exists = sqlx::query("SELECT 1 AS n FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = ?").bind(schema).fetch_optional(pool).await.map_err(e)?.is_some();
    let kinds = sqlx::query("SELECT TABLE_TYPE AS kind, COUNT(*) AS n, COALESCE(SUM(TABLE_ROWS), 0) AS r FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? GROUP BY TABLE_TYPE").bind(schema).fetch_all(pool).await.map_err(e)?;
    let mut info = DropInfo { exists, ..Default::default() };
    for r in &kinds {
        let n = super::structure::num(r, "n").unwrap_or(0);
        if text(r, "kind").is_some_and(|k| k.contains("VIEW")) {
            info.views += n;
        } else {
            info.tables += n;
            info.rows = Some(info.rows.unwrap_or(0) + text(r, "r").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0) as u64);
        }
    }
    info.rows_estimated = true;
    info.triggers = count(pool, "SELECT COUNT(*) AS n FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = ?", schema).await;
    info.routines = count(pool, "SELECT COUNT(*) AS n FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = ?", schema).await;
    info.events = count(pool, "SELECT COUNT(*) AS n FROM information_schema.EVENTS WHERE EVENT_SCHEMA = ?", schema).await;
    Ok(info)
}

/// Rows, and the tables that point at this one. The count has a few seconds to finish before the
/// server's estimate stands in for it, so a huge table never holds the dialog up.
async fn table_info(db: &Db, schema: &str, table: &str) -> Res<DropInfo> {
    let Pool::My(pool) = &db.pool;
    let d = db.dialect;
    let st = sqlx::query("SELECT TABLE_ROWS AS est FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?").bind(schema).bind(table).fetch_optional(pool).await.map_err(e)?;
    let mut info = DropInfo { exists: st.is_some(), ..Default::default() };
    let Some(st) = st else { return Ok(info) };
    let exact = tokio::time::timeout(Duration::from_secs(3), async {
        let sql = format!("SELECT COUNT(*) AS n FROM {}", d.qualified(schema, table));
        sqlx::query(&sql).fetch_one(pool).await.ok().and_then(|r| super::structure::num(&r, "n"))
    })
    .await
    .ok()
    .flatten();
    (info.rows, info.rows_estimated) = match exact {
        Some(n) => (Some(n), false),
        None => (super::structure::num(&st, "est"), true),
    };
    let refs = sqlx::query(d.referenced_by()).bind(schema).bind(table).fetch_all(pool).await.map_err(e)?;
    for r in &refs {
        let (rdb, rt) = (text(r, "db").unwrap_or_default(), text(r, "tbl").unwrap_or_default());
        if rdb == schema && rt == table {
            continue;
        }
        let name = if rdb == schema { rt } else { format!("{rdb}.{rt}") };
        if !info.referenced_by.contains(&name) {
            info.referenced_by.push(name);
        }
    }
    Ok(info)
}

async fn database_names(c: &mut sqlx::MySqlConnection) -> Res<Vec<String>> {
    let rows = sqlx::query("SHOW DATABASES").fetch_all(&mut *c).await.map_err(e)?;
    let mut dbs: Vec<String> = rows.iter().filter_map(|r| raw(r, 0).map(|b| String::from_utf8_lossy(b).into_owned())).collect();
    dbs.sort();
    Ok(dbs)
}

/// Runs one statement on a connection of its own with write access, and for changes to the list of
/// databases reads that list back on the same connection.
async fn run(db: &Db, sql: String, read_only: bool, list: bool) -> Res<Option<Vec<String>>> {
    let mut c = writer(db, "", read_only).await?;
    let done = c.execute(sql.as_str()).await.map_err(e);
    let out = match done {
        Err(err) => Err(err),
        Ok(_) if list => database_names(&mut c).await.map(Some),
        Ok(_) => Ok(None),
    };
    let _ = c.close().await;
    out
}

fn created(conn: u64, schema: &str, table: &str, undo: SqlUndo) -> UndoAction {
    UndoAction { at: Instant::now(), conn, db: schema.into(), coll: table.into(), kind: UndoKind::Sql { cols: Vec::new(), undo } }
}

/// The part of a table undo that runs with the table locked.
async fn drop_if_empty(c: &mut sqlx::MySqlConnection, q: &str, table: &str, sql: &str) -> Res<()> {
    let has = sqlx::query(&format!("SELECT 1 AS n FROM {q} LIMIT 1")).fetch_optional(&mut *c).await.map_err(e)?.is_some();
    if has {
        return Err(format!("Not undone: {table} has rows now."));
    }
    c.execute(sql).await.map_err(e)?;
    Ok(())
}

/// Undo of a create: the object goes only while it is still empty. A table is locked first so a row
/// cannot arrive between the check and the drop. A database cannot be locked, so a table created in
/// it in that instant would be dropped with it; the window is one round trip.
async fn undo_create(db: &Db, schema: &str, table: &str, read_only: bool) -> Res<(Ddl, Option<Vec<String>>)> {
    let d = db.dialect;
    let Pool::My(pool) = &db.pool;
    if table.is_empty() {
        let n = sqlx::query("SELECT (SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = ?) + (SELECT COUNT(*) FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = ?) + (SELECT COUNT(*) FROM information_schema.EVENTS WHERE EVENT_SCHEMA = ?) AS n")
            .bind(schema)
            .bind(schema)
            .bind(schema)
            .fetch_one(pool)
            .await
            .map_err(e)?;
        if super::structure::num(&n, "n").unwrap_or(0) > 0 {
            return Err(format!("Not undone: {schema} has tables or other objects in it now."));
        }
        let sql = drop_db_sql(d, schema)?;
        return run(db, sql, read_only, true).await.map(|l| (Ddl::DroppedDb, l));
    }
    let sql = drop_table_sql(d, schema, table, false)?;
    let mut c = writer(db, "", read_only).await?;
    let q = d.qualified(schema, table);
    // Without the LOCK TABLES privilege the check still runs, only without the lock.
    let locked = c.execute(format!("LOCK TABLES {q} WRITE").as_str()).await.is_ok();
    let res = drop_if_empty(&mut c, &q, table, &sql).await;
    if locked {
        let _ = c.execute("UNLOCK TABLES").await;
    }
    let _ = c.close().await;
    res.map(|_| (Ddl::DroppedTable, None))
}

impl Worker {
    /// Charsets, collations and engines, plus the default collation of `schema` when it is not empty.
    pub fn sql_create_meta(&self, conn: u64, db: Db, schema: String) {
        self.spawn(async move {
            let res = load_meta(&db, &schema).await;
            Msg::SqlMeta { conn, schema, res }
        });
    }

    /// What dropping `table` of `schema` (the whole database when `table` is empty) will take.
    pub fn sql_drop_info(&self, conn: u64, db: Db, schema: String, table: String) {
        self.spawn(async move {
            let res = if table.is_empty() { db_info(&db, &schema).await } else { table_info(&db, &schema, &table).await };
            Msg::SqlDropInfo { conn, schema, table, res }
        });
    }

    pub fn sql_create_db(&self, conn: u64, db: Db, name: String, charset: String, collation: String, meta: Meta, read_only: bool) {
        self.spawn(async move {
            let res = async {
                let sql = create_db_sql(db.dialect, &name, &charset, &collation, &meta)?;
                run(&db, sql, read_only, true).await
            }
            .await;
            let undo = res.is_ok().then(|| created(conn, &name, "", SqlUndo::CreatedDb));
            let (res, dbs) = split(res, format!("Created database {name}"));
            Msg::SqlDdl { conn, schema: name, table: String::new(), what: Ddl::CreatedDb, res, undo, dbs }
        });
    }

    pub fn sql_create_table(&self, conn: u64, db: Db, spec: TableSpec, meta: Meta, read_only: bool) {
        self.spawn(async move {
            let res = async {
                let sql = create_table_sql(db.dialect, db.backslash_escapes(), &spec, &meta)?;
                run(&db, sql, read_only, false).await
            }
            .await;
            let undo = res.is_ok().then(|| created(conn, &spec.schema, &spec.name, SqlUndo::CreatedTable));
            let (res, dbs) = split(res, format!("Created table {}", spec.name));
            Msg::SqlDdl { conn, schema: spec.schema, table: spec.name, what: Ddl::CreatedTable, res, undo, dbs }
        });
    }

    /// DROP DATABASE, DROP TABLE, DROP VIEW or TRUNCATE; `table` is empty for a database. No undo.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_drop(&self, conn: u64, db: Db, schema: String, table: String, view: bool, truncate: bool, read_only: bool) {
        self.spawn(async move {
            let d = db.dialect;
            let (what, sql, done) = if table.is_empty() {
                (Ddl::DroppedDb, drop_db_sql(d, &schema), format!("Dropped database {schema}"))
            } else if truncate {
                (Ddl::Emptied, truncate_sql(d, &schema, &table), format!("Emptied table {table}"))
            } else {
                (Ddl::DroppedTable, drop_table_sql(d, &schema, &table, view), format!("Dropped {} {table}", if view { "view" } else { "table" }))
            };
            let res = match sql {
                Ok(sql) => run(&db, sql, read_only, what == Ddl::DroppedDb).await,
                Err(err) => Err(err),
            };
            let (res, dbs) = split(res, done);
            Msg::SqlDdl { conn, schema, table, what, res, undo: None, dbs }
        });
    }

    /// Undoes a create when the object is still empty. A refused undo hands the entry back.
    pub fn sql_undo_create(&self, db: Db, a: UndoAction, read_only: bool) {
        self.spawn(async move {
            let UndoAction { at, conn, db: schema, coll: table, kind } = a;
            let res = undo_create(&db, &schema, &table, read_only).await;
            let (what, undone) = match &res {
                Ok((w, _)) => (*w, true),
                Err(_) => (if table.is_empty() { Ddl::DroppedDb } else { Ddl::DroppedTable }, false),
            };
            let said = if table.is_empty() { format!("Removed database {schema}") } else { format!("Removed table {table}") };
            let (res, dbs) = split(res.map(|(_, l)| l), said);
            let undo = (!undone).then(|| UndoAction { at, conn, db: schema.clone(), coll: table.clone(), kind });
            // A refused undo changes nothing, so the app has nothing to refresh.
            Msg::SqlDdl { conn, schema, table, what, res, undo, dbs }
        });
    }
}

fn split(res: Res<Option<Vec<String>>>, said: String) -> (Res<String>, Option<Vec<String>>) {
    match res {
        Ok(dbs) => (Ok(said), dbs),
        Err(err) => (Err(err), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sql::lex;

    fn meta() -> Meta {
        let cs = |n: &str, d: &str| Charset { name: n.into(), desc: String::new(), default_collation: d.into() };
        let co = |n: &str, c: &str, d: bool| Collation { name: n.into(), charset: c.into(), is_default: d };
        Meta {
            charsets: vec![cs("latin1", "latin1_swedish_ci"), cs("utf8mb4", "utf8mb4_0900_ai_ci")],
            collations: vec![co("latin1_swedish_ci", "latin1", true), co("utf8mb4_0900_ai_ci", "utf8mb4", true), co("utf8mb4_bin", "utf8mb4", false)],
            engines: vec!["InnoDB".into(), "MyISAM".into()],
            server_charset: "utf8mb4".into(),
            server_collation: "utf8mb4_0900_ai_ci".into(),
            server_engine: "InnoDB".into(),
            schema_collation: None,
        }
    }

    fn spec(name: &str, columns: Vec<ColumnSpec>) -> TableSpec {
        TableSpec { schema: "shop".into(), name: name.into(), engine: "InnoDB".into(), collation: "utf8mb4_0900_ai_ci".into(), columns }
    }

    fn col(name: &str, ty: &str, len: &str) -> ColumnSpec {
        ColumnSpec { name: name.into(), ty: ty.into(), len: len.into(), ..Default::default() }
    }

    /// Every mode the server may read a statement under: one statement either way.
    fn one_statement(sql: &str) {
        for (bs, ansi) in [(true, false), (false, false), (true, true), (false, true)] {
            assert_eq!(lex::count_with(sql, bs, ansi), 1, "bs={bs} ansi={ansi}: {sql}");
        }
    }

    #[test]
    fn create_database_matches_the_design() {
        let sql = create_db_sql(Dialect::MySql, "billing", "utf8mb4", "utf8mb4_0900_ai_ci", &meta()).unwrap();
        assert_eq!(shown(&sql), "CREATE DATABASE `billing`\n  CHARACTER SET utf8mb4 COLLATE utf8mb4_0900_ai_ci;");
    }

    #[test]
    fn hostile_names_are_quoted_or_refused() {
        let m = meta();
        for name in ["a`b", "x`; DROP DATABASE shop; --", "it's", "a\"b", "back\\slash`", "ünï😀", "a``b", "x' OR '1'='1", "$(rm)", "x`y`z"] {
            // Path characters are refused for databases, so only names without them get this far.
            let r = create_db_sql(Dialect::MySql, name, "utf8mb4", "utf8mb4_bin", &m);
            if name.contains(['/', '\\', '.']) {
                assert!(r.is_err(), "{name}");
                continue;
            }
            let sql = r.unwrap_or_else(|err| panic!("{name}: {err}"));
            assert!(sql.starts_with(&format!("CREATE DATABASE {}", Dialect::MySql.quote_ident(name))), "{sql}");
            one_statement(&sql);
            let t = drop_table_sql(Dialect::MySql, "shop", name, false).unwrap();
            assert_eq!(t, format!("DROP TABLE `shop`.{}", Dialect::MySql.quote_ident(name)));
            one_statement(&t);
        }
        for name in ["", "a\nb", "a\rb", "tab\tx", "nul\0x", " lead", "trail ", "a/b", "a.b", "a\\b", &"x".repeat(65)] {
            assert!(create_db_sql(Dialect::MySql, name, "utf8mb4", "utf8mb4_bin", &m).is_err(), "{name:?}");
            assert!(drop_db_sql(Dialect::MySql, name).is_err(), "{name:?}");
            assert!(truncate_sql(Dialect::MySql, "shop", name).is_err(), "{name:?}");
        }
        assert!(create_db_sql(Dialect::MySql, &"x".repeat(64), "utf8mb4", "utf8mb4_bin", &m).is_ok());
    }

    #[test]
    fn system_schemas_are_left_alone() {
        for s in ["mysql", "MYSQL", "sys", "information_schema", "performance_schema"] {
            assert!(drop_db_sql(Dialect::MySql, s).is_err());
            assert!(create_db_sql(Dialect::MySql, s, "utf8mb4", "utf8mb4_bin", &meta()).is_err());
            assert!(drop_table_sql(Dialect::MySql, s, "user", false).is_err());
            assert!(truncate_sql(Dialect::MySql, s, "user").is_err());
        }
    }

    #[test]
    fn charset_collation_and_engine_must_be_the_servers() {
        let m = meta();
        assert!(create_db_sql(Dialect::MySql, "a", "utf8mb4; DROP", "utf8mb4_bin", &m).is_err());
        assert!(create_db_sql(Dialect::MySql, "a", "utf8mb4", "utf8mb4_bin` ", &m).is_err());
        assert!(create_db_sql(Dialect::MySql, "a", "utf9", "utf8mb4_bin", &m).is_err());
        // A real collation of another charset is refused too.
        assert!(create_db_sql(Dialect::MySql, "a", "utf8mb4", "latin1_swedish_ci", &m).is_err());
        // The server's own spelling is what gets written, whatever case came in.
        assert!(create_db_sql(Dialect::MySql, "a", "UTF8MB4", "UTF8MB4_BIN", &m).unwrap().contains("CHARACTER SET utf8mb4 COLLATE utf8mb4_bin"));
        let mut s = spec("t", vec![ColumnSpec::id()]);
        s.engine = "InnoDB; DROP".into();
        assert!(create_table_sql(Dialect::MySql, true, &s, &m).is_err());
        s.engine = "innodb".into();
        assert!(create_table_sql(Dialect::MySql, true, &s, &m).unwrap().contains("ENGINE=InnoDB "));
        s.engine = "Memory".into();
        assert!(create_table_sql(Dialect::MySql, true, &s, &m).is_err());
        // A form whose lists have not arrived builds nothing.
        s.engine = "InnoDB".into();
        assert!(create_table_sql(Dialect::MySql, true, &s, &Meta::default()).is_err());
    }

    #[test]
    fn create_table_statement() {
        let amount = ColumnSpec { nullable: false, ..col("amount", "DECIMAL", "10,2") };
        let at = ColumnSpec { nullable: false, default: DefaultKind::Now, ..col("created_at", "DATETIME", "") };
        let s = spec("refunds", vec![ColumnSpec::id(), amount, col("reason", "VARCHAR", "255"), at]);
        let sql = create_table_sql(Dialect::MySql, true, &s, &meta()).unwrap();
        assert_eq!(
            sql,
            "CREATE TABLE `shop`.`refunds` (\n  `id` INT UNSIGNED NOT NULL AUTO_INCREMENT,\n  `amount` DECIMAL(10,2) NOT NULL,\n  `reason` VARCHAR(255) NULL,\n  `created_at` DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,\n  PRIMARY KEY (`id`)\n) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci"
        );
        one_statement(&sql);
    }

    #[test]
    fn hostile_table_and_column_names_and_defaults() {
        let m = meta();
        for name in ["a`b", "x`) ; DROP TABLE t; --", "it's", "q\"q", "ünï😀", "``", "back\\"] {
            for (bs, ansi) in [(true, false), (false, false), (true, true), (false, true)] {
                let mut c = ColumnSpec { default: DefaultKind::Value, default_text: format!("it's\\ {name}\n'; DROP TABLE x; -- "), ..col(name, "VARCHAR", "20") };
                c.nullable = true;
                let mut s = spec(name, vec![ColumnSpec { name: "id".into(), ..ColumnSpec::id() }, c]);
                if name.contains(['.', '/', '\\']) {
                    s.name = "t".into();
                }
                let sql = create_table_sql(Dialect::MySql, bs, &s, &m).unwrap_or_else(|err| panic!("{name}: {err}"));
                let q = Dialect::MySql.quote_ident(name);
                assert!(sql.contains(&format!("  {q} VARCHAR(20) NULL DEFAULT '")), "{sql}");
                assert_eq!(lex::count_with(&sql, bs, ansi), 1, "bs={bs} ansi={ansi}: {sql}");
            }
        }
        // The literal doubles quotes always and backslashes only where the server reads them as escapes.
        let c = ColumnSpec { default: DefaultKind::Value, default_text: "a'b\\c".into(), ..col("v", "VARCHAR", "9") };
        let s = spec("t", vec![ColumnSpec::id(), c]);
        assert!(create_table_sql(Dialect::MySql, true, &s, &m).unwrap().contains("DEFAULT 'a''b\\\\c'"));
        assert!(create_table_sql(Dialect::MySql, false, &s, &m).unwrap().contains("DEFAULT 'a''b\\c'"));
        // Control characters in any name are refused, not quoted.
        let s = spec("t", vec![ColumnSpec { name: "a\nb".into(), ..ColumnSpec::id() }]);
        assert!(create_table_sql(Dialect::MySql, true, &s, &m).is_err());
        let s = spec("t\n2", vec![ColumnSpec::id()]);
        assert!(create_table_sql(Dialect::MySql, true, &s, &m).is_err());
        // A column may hold a dot or a slash, a table may not.
        let s = spec("t", vec![ColumnSpec { name: "a.b/c".into(), ..ColumnSpec::id() }]);
        assert!(create_table_sql(Dialect::MySql, true, &s, &m).unwrap().contains("`a.b/c` INT UNSIGNED"));
    }

    #[test]
    fn types_lengths_and_defaults_are_checked() {
        let m = meta();
        let build = |c: ColumnSpec| create_table_sql(Dialect::MySql, true, &spec("t", vec![c]), &m);
        assert!(build(col("a", "VARCHAR", "")).is_err());
        assert!(build(col("a", "VARCHAR", "0")).is_err());
        assert!(build(col("a", "VARCHAR", "65536")).is_err());
        assert!(build(col("a", "VARCHAR", "10); DROP TABLE x; --")).is_err());
        assert!(build(col("a", "VARCHAR(10) NULL; --", "")).is_err());
        assert!(build(col("a", "INT", "11")).is_err());
        assert!(build(col("a", "DECIMAL", "10,2,3")).is_err());
        assert!(build(col("a", "DECIMAL", "66,2")).is_err());
        assert!(build(col("a", "DECIMAL", "5,6")).is_err());
        assert!(build(col("a", "DECIMAL", "-1")).is_err());
        assert!(build(col("a", "DATETIME", "7")).is_err());
        assert!(build(col("a", "ENUM", "")).is_err());
        assert!(build(col("a", "DECIMAL", "10")).unwrap().contains("DECIMAL(10,0)"));
        assert!(build(col("a", "DATETIME", "3")).unwrap().contains("DATETIME(3) NULL"));
        assert!(build(col("a", "TEXT", "")).unwrap().contains("`a` TEXT NULL"));
        assert!(build(ColumnSpec { unsigned: true, ..col("a", "BIGINT", "") }).unwrap().contains("BIGINT UNSIGNED"));
        // UNSIGNED means nothing for text, so it is dropped instead of written.
        assert!(!build(ColumnSpec { unsigned: true, ..col("a", "VARCHAR", "5") }).unwrap().contains("UNSIGNED"));
        let num = |text: &str| build(ColumnSpec { default: DefaultKind::Value, default_text: text.into(), ..col("a", "INT", "") });
        assert!(num("-5").unwrap().contains("DEFAULT -5"));
        assert!(num("1.5").unwrap().contains("DEFAULT 1.5"));
        for bad in ["", "1; DROP", "1e5", "0x1", "'1'", "--1", "1 OR 1", "+1", ".5", "5."] {
            assert!(num(bad).is_err(), "{bad}");
        }
        let blob = |k: DefaultKind| build(ColumnSpec { default: k, default_text: "x".into(), ..col("a", "TEXT", "") });
        assert!(blob(DefaultKind::Value).is_err());
        assert!(blob(DefaultKind::Now).is_err());
        assert!(blob(DefaultKind::Null).unwrap().contains("DEFAULT NULL"));
        assert!(build(ColumnSpec { nullable: false, default: DefaultKind::Null, ..col("a", "INT", "") }).is_err());
        assert!(build(ColumnSpec { default: DefaultKind::Now, ..col("a", "TIMESTAMP", "6") }).unwrap().contains("DEFAULT CURRENT_TIMESTAMP(6)"));
        assert!(build(ColumnSpec { default: DefaultKind::Now, ..col("a", "INT", "") }).is_err());
    }

    #[test]
    fn keys_and_auto_increment_rules() {
        let m = meta();
        let build = |cols: Vec<ColumnSpec>| create_table_sql(Dialect::MySql, true, &spec("t", cols), &m);
        assert!(build(vec![]).is_err());
        // Names clash without regard to case, as the server compares them.
        assert!(build(vec![col("A", "INT", ""), col("a", "INT", "")]).is_err());
        // AUTO_INCREMENT needs an integer key, once, without a default.
        assert!(build(vec![ColumnSpec { pk: false, ..ColumnSpec::id() }]).is_err());
        assert!(build(vec![ColumnSpec { ty: "VARCHAR".into(), len: "9".into(), ..ColumnSpec::id() }]).is_err());
        assert!(build(vec![ColumnSpec { default: DefaultKind::Value, default_text: "1".into(), ..ColumnSpec::id() }]).is_err());
        assert!(build(vec![ColumnSpec::id(), ColumnSpec { name: "id2".into(), ..ColumnSpec::id() }]).is_err());
        // A composite key lists its columns in order, and key columns are never nullable.
        let a = ColumnSpec { pk: true, nullable: true, ..col("a", "INT", "") };
        let b = ColumnSpec { pk: true, ..col("b", "VARCHAR", "8") };
        let sql = build(vec![a, b]).unwrap();
        assert!(sql.contains("`a` INT NOT NULL") && sql.contains("`b` VARCHAR(8) NOT NULL") && sql.contains("PRIMARY KEY (`a`, `b`)"), "{sql}");
        // No key at all is allowed.
        assert!(!build(vec![col("a", "INT", "")]).unwrap().contains("PRIMARY KEY"));
    }

    #[test]
    fn drop_and_truncate_statements() {
        let d = Dialect::MariaDb;
        assert_eq!(drop_table_sql(d, "shop", "orders", false).unwrap(), "DROP TABLE `shop`.`orders`");
        assert_eq!(drop_table_sql(d, "shop", "v", true).unwrap(), "DROP VIEW `shop`.`v`");
        assert_eq!(drop_db_sql(d, "shop").unwrap(), "DROP DATABASE `shop`");
        assert_eq!(truncate_sql(d, "sh`op", "or`ders").unwrap(), "TRUNCATE TABLE `sh``op`.`or``ders`");
        one_statement(&truncate_sql(d, "sh`op", "x; DROP TABLE y").unwrap());
    }

    #[test]
    fn meta_lookups() {
        let m = meta();
        assert_eq!(m.default_collation("utf8mb4"), Some("utf8mb4_0900_ai_ci"));
        assert_eq!(m.charset_of("utf8mb4_bin"), Some("utf8mb4"));
        assert_eq!(m.collations_of("utf8mb4").count(), 2);
        assert!(m.loaded() && !Meta::default().loaded());
    }

    // Live tests run against whichever of QUARRY_TEST_MYSQL and QUARRY_TEST_MARIADB is set, each against
    // names of its own so parallel runs never meet.
    struct Live {
        w: Worker,
        rx: std::sync::mpsc::Receiver<Msg>,
        db: Db,
        pool: sqlx::MySqlPool,
        label: &'static str,
    }

    fn live(var: &'static str) -> Option<Live> {
        let uri = std::env::var(var).ok()?;
        let (tx, rx) = std::sync::mpsc::channel();
        let w = Worker::new(tx, eframe::egui::Context::default());
        let pool = w.rt.block_on(async { sqlx::MySqlPool::connect(&uri).await.unwrap() });
        w.sql_connect(1, uri, Duration::from_secs(30), crate::db::Net::default());
        let Msg::SqlConnected { res: Ok((db, _, _)), .. } = rx.recv_timeout(Duration::from_secs(20)).unwrap() else { panic!("connect") };
        Some(Live { w, rx, db, pool, label: var })
    }

    fn each(f: impl Fn(&Live)) {
        for var in ["QUARRY_TEST_MYSQL", "QUARRY_TEST_MARIADB"] {
            if let Some(l) = live(var) {
                f(&l);
            }
        }
    }

    fn unique(base: &str) -> String {
        format!("{base}_{}_{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos())
    }

    impl Live {
        fn next(&self) -> Msg {
            self.rx.recv_timeout(Duration::from_secs(20)).expect("worker answer")
        }
        fn exec(&self, sql: &str) {
            self.w.rt.block_on(async { sqlx::raw_sql(sql).execute(&self.pool).await.unwrap() });
        }
        fn scalar(&self, sql: &str) -> i64 {
            self.w.rt.block_on(async { sqlx::query_scalar(sql).fetch_one(&self.pool).await.unwrap() })
        }
        fn meta(&self, schema: &str) -> Meta {
            self.w.sql_create_meta(1, self.db.clone(), schema.into());
            let Msg::SqlMeta { res, .. } = self.next() else { panic!("meta") };
            res.unwrap()
        }
        fn ddl(&self) -> (Ddl, Res<String>, Option<UndoAction>, Option<Vec<String>>) {
            let Msg::SqlDdl { what, res, undo, dbs, .. } = self.next() else { panic!("expected SqlDdl") };
            (what, res, undo, dbs)
        }
        fn exists_db(&self, name: &str) -> bool {
            self.scalar(&format!("SELECT COUNT(*) FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = {}", lit(name, true))) == 1
        }
        fn exists_table(&self, schema: &str, name: &str) -> bool {
            self.scalar(&format!("SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {}", lit(schema, true), lit(name, true))) == 1
        }
        fn create_db(&self, name: &str, cs: &str, co: &str, ro: bool) -> (Ddl, Res<String>, Option<UndoAction>, Option<Vec<String>>) {
            self.w.sql_create_db(1, self.db.clone(), name.into(), cs.into(), co.into(), self.meta(""), ro);
            self.ddl()
        }
        fn undo(&self, a: UndoAction, ro: bool) -> (Ddl, Res<String>, Option<UndoAction>, Option<Vec<String>>) {
            self.w.sql_undo_create(self.db.clone(), a, ro);
            self.ddl()
        }
    }

    #[test]
    fn live_meta_lists_come_from_the_server() {
        each(|l| {
            let m = l.meta("mysql");
            assert!(m.loaded(), "{}", l.label);
            assert!(m.engines.iter().any(|e| e == "InnoDB"), "{}: {:?}", l.label, m.engines);
            assert!(m.engines.contains(&m.server_engine), "{}: {} not in {:?}", l.label, m.server_engine, m.engines);
            assert!(m.charset_of(&m.server_collation).is_some(), "{}", l.label);
            assert!(m.schema_collation.is_some(), "{}", l.label);
            assert!(m.collations_of("utf8mb4").count() > 1);
        });
    }

    #[test]
    fn live_create_database_with_charset_and_undo_when_empty() {
        each(|l| {
            let name = unique("qcd_db");
            let (what, res, undo, dbs) = l.create_db(&name, "latin1", "latin1_swedish_ci", false);
            assert_eq!(what, Ddl::CreatedDb);
            assert_eq!(res.unwrap(), format!("Created database {name}"), "{}", l.label);
            assert!(dbs.unwrap().contains(&name));
            let got = l.w.rt.block_on(async { text(&sqlx::query(&format!("SELECT DEFAULT_COLLATION_NAME AS d FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = '{name}'")).fetch_one(&l.pool).await.unwrap(), "d").unwrap() });
            assert_eq!(got, "latin1_swedish_ci");
            // Creating it again is an error and offers no undo that would drop the first one.
            let (_, again, undo2, _) = l.create_db(&name, "latin1", "latin1_swedish_ci", false);
            assert!(again.is_err() && undo2.is_none(), "{}", l.label);
            // Still empty, so undo drops it.
            let (what, res, back, dbs) = l.undo(undo.unwrap(), false);
            assert_eq!(what, Ddl::DroppedDb);
            assert!(res.is_ok() && back.is_none(), "{}: {res:?}", l.label);
            assert!(!dbs.unwrap().contains(&name));
            assert!(!l.exists_db(&name));
        });
    }

    #[test]
    fn live_undo_of_create_database_refused_when_not_empty() {
        each(|l| {
            let name = unique("qcd_full");
            let m = l.meta("");
            let co = m.default_collation("utf8mb4").unwrap().to_string();
            let (_, res, undo, _) = l.create_db(&name, "utf8mb4", &co, false);
            assert!(res.is_ok());
            l.exec(&format!("CREATE TABLE `{name}`.t (a INT)"));
            let (_, res, back, _) = l.undo(undo.unwrap(), false);
            assert!(res.unwrap_err().contains("Not undone"), "{}", l.label);
            assert!(back.is_some(), "a refused undo keeps its entry");
            assert!(l.exists_db(&name) && l.exists_table(&name, "t"));
            // Once it is empty again the same entry works.
            l.exec(&format!("DROP TABLE `{name}`.t"));
            assert!(l.undo(back.unwrap(), false).1.is_ok());
            assert!(!l.exists_db(&name));
        });
    }

    #[test]
    fn live_create_table_with_columns_and_undo_rules() {
        each(|l| {
            let schema = unique("qcd_t");
            let m = l.meta("");
            l.exec(&format!("CREATE DATABASE `{schema}`"));
            let m = Meta { schema_collation: None, ..m };
            let co = m.default_collation("utf8mb4").unwrap().to_string();
            let amount = ColumnSpec { nullable: false, ..col("amount", "DECIMAL", "10,2") };
            let at = ColumnSpec { nullable: false, default: DefaultKind::Now, ..col("created at", "DATETIME", "") };
            let note = ColumnSpec { default: DefaultKind::Value, default_text: "it's `x` \\ ok".into(), ..col("no`te", "VARCHAR", "40") };
            let s = TableSpec { schema: schema.clone(), name: "re`funds".into(), engine: m.server_engine.clone(), collation: co.clone(), columns: vec![ColumnSpec::id(), amount, note, at] };
            l.w.sql_create_table(1, l.db.clone(), s.clone(), m.clone(), false);
            let (what, res, undo, _) = l.ddl();
            assert_eq!(what, Ddl::CreatedTable);
            assert!(res.is_ok(), "{}: {res:?}", l.label);
            let cols: Vec<(String, String, String)> = l.w.rt.block_on(async {
                sqlx::query(&format!("SELECT COLUMN_NAME AS n, COLUMN_TYPE AS t, COLUMN_KEY AS k FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = '{schema}' AND TABLE_NAME = 're`funds' ORDER BY ORDINAL_POSITION"))
                    .fetch_all(&l.pool)
                    .await
                    .unwrap()
                    .iter()
                    .map(|r| (text(r, "n").unwrap(), text(r, "t").unwrap().to_lowercase(), text(r, "k").unwrap()))
                    .collect()
            });
            // MariaDB still reports the display width, int(10) unsigned.
            assert!(cols[0].1.starts_with("int") && cols[0].1.ends_with("unsigned") && (cols[0].0.as_str(), cols[0].2.as_str()) == ("id", "PRI"), "{}: {:?}", l.label, cols[0]);
            assert_eq!(cols[1].1, "decimal(10,2)");
            assert_eq!(cols[2].0, "no`te");
            assert_eq!(cols[3].0, "created at");
            // A row arrives, so undo is refused and the table stays.
            l.exec(&format!("INSERT INTO `{schema}`.`re``funds` (amount, `created at`) VALUES (1.5, NOW())"));
            // The default text went in whole, quote, backtick and backslash included. Read back through a row,
            // because MariaDB reports COLUMN_DEFAULT as a quoted literal.
            let note = l.w.rt.block_on(async { text(&sqlx::query(&format!("SELECT `no``te` AS d FROM `{schema}`.`re``funds`")).fetch_one(&l.pool).await.unwrap(), "d").unwrap() });
            assert_eq!(note, "it's `x` \\ ok", "{}", l.label);
            let (_, res, back, _) = l.undo(undo.unwrap(), false);
            assert!(res.unwrap_err().contains("has rows"), "{}", l.label);
            assert!(back.is_some() && l.exists_table(&schema, "re`funds"));
            // Emptied, the undo goes through and the table is gone.
            l.exec(&format!("DELETE FROM `{schema}`.`re``funds`"));
            let (what, res, back, _) = l.undo(back.unwrap(), false);
            assert_eq!(what, Ddl::DroppedTable);
            assert!(res.is_ok() && back.is_none(), "{}: {res:?}", l.label);
            assert!(!l.exists_table(&schema, "re`funds"));
            // A second table that fails to build writes nothing.
            let bad = TableSpec { columns: vec![col("a", "VARCHAR", "")], ..s };
            l.w.sql_create_table(1, l.db.clone(), bad, m, false);
            let (_, res, undo, _) = l.ddl();
            assert!(res.is_err() && undo.is_none());
            l.exec(&format!("DROP DATABASE `{schema}`"));
        });
    }

    #[test]
    fn live_drop_info_and_drops() {
        each(|l| {
            let schema = unique("qcd_drop");
            l.exec(&format!("CREATE DATABASE `{schema}`"));
            l.exec(&format!("CREATE TABLE `{schema}`.parent (id INT PRIMARY KEY) ENGINE=InnoDB"));
            l.exec(&format!("CREATE TABLE `{schema}`.child (id INT PRIMARY KEY, p INT, FOREIGN KEY (p) REFERENCES `{schema}`.parent(id)) ENGINE=InnoDB"));
            l.exec(&format!("INSERT INTO `{schema}`.parent VALUES (1),(2),(3)"));
            l.exec(&format!("CREATE VIEW `{schema}`.v AS SELECT id FROM `{schema}`.parent"));
            l.exec(&format!("CREATE TRIGGER `{schema}`.trg BEFORE INSERT ON `{schema}`.child FOR EACH ROW SET NEW.id = NEW.id"));
            l.w.sql_drop_info(1, l.db.clone(), schema.clone(), "parent".into());
            let Msg::SqlDropInfo { res, .. } = l.next() else { panic!() };
            let info = res.unwrap();
            assert!(info.exists && info.rows == Some(3) && !info.rows_estimated, "{}: {info:?}", l.label);
            assert_eq!(info.referenced_by, ["child"]);
            l.w.sql_drop_info(1, l.db.clone(), schema.clone(), String::new());
            let Msg::SqlDropInfo { res, .. } = l.next() else { panic!() };
            let info = res.unwrap();
            assert_eq!((info.tables, info.views, info.triggers), (2, 1, 1), "{}: {info:?}", l.label);
            // The server refuses to drop a table another one points at, and nothing is lost.
            l.w.sql_drop(1, l.db.clone(), schema.clone(), "parent".into(), false, false, false);
            let (what, res, undo, _) = l.ddl();
            assert_eq!(what, Ddl::DroppedTable);
            assert!(res.is_err() && undo.is_none(), "{}", l.label);
            assert!(l.exists_table(&schema, "parent"));
            // Truncate is refused for the same reason, a plain table can be emptied.
            l.w.sql_drop(1, l.db.clone(), schema.clone(), "child".into(), false, true, false);
            let (what, res, _, _) = l.ddl();
            assert_eq!(what, Ddl::Emptied);
            assert!(res.is_ok(), "{}: {res:?}", l.label);
            l.w.sql_drop(1, l.db.clone(), schema.clone(), "v".into(), true, false, false);
            assert!(l.ddl().1.is_ok());
            l.w.sql_drop(1, l.db.clone(), schema.clone(), "child".into(), false, false, false);
            assert!(l.ddl().1.is_ok());
            l.w.sql_drop(1, l.db.clone(), schema.clone(), "parent".into(), false, false, false);
            let (_, res, _, _) = l.ddl();
            assert_eq!(res.unwrap(), "Dropped table parent");
            assert!(!l.exists_table(&schema, "parent"));
            l.w.sql_drop(1, l.db.clone(), schema.clone(), String::new(), false, false, false);
            let (what, res, undo, dbs) = l.ddl();
            assert_eq!(what, Ddl::DroppedDb);
            assert!(res.is_ok() && undo.is_none(), "{}: {res:?}", l.label);
            assert!(!dbs.unwrap().contains(&schema));
            assert!(!l.exists_db(&schema));
        });
    }

    #[test]
    fn live_everything_is_refused_while_locked() {
        each(|l| {
            let name = unique("qcd_lock");
            let m = l.meta("");
            let co = m.default_collation("utf8mb4").unwrap().to_string();
            let (_, res, undo, _) = l.create_db(&name, "utf8mb4", &co, true);
            assert!(res.unwrap_err().contains("locked") && undo.is_none(), "{}", l.label);
            assert!(!l.exists_db(&name));
            l.exec(&format!("CREATE DATABASE `{name}`"));
            l.exec(&format!("CREATE TABLE `{name}`.t (a INT)"));
            let s = TableSpec { schema: name.clone(), name: "t2".into(), engine: m.server_engine.clone(), collation: co, columns: vec![ColumnSpec::id()] };
            l.w.sql_create_table(1, l.db.clone(), s, m, true);
            assert!(l.ddl().1.unwrap_err().contains("locked"));
            for (table, view, trunc) in [("t", false, false), ("t", false, true), ("", false, false)] {
                l.w.sql_drop(1, l.db.clone(), name.clone(), table.into(), view, trunc, true);
                assert!(l.ddl().1.unwrap_err().contains("locked"), "{}", l.label);
            }
            assert!(l.exists_table(&name, "t") && !l.exists_table(&name, "t2"));
            // A refused undo hands its entry back.
            let undo = created(1, &name, "", SqlUndo::CreatedDb);
            let (_, res, back, _) = l.undo(undo, true);
            assert!(res.is_err() && back.is_some());
            l.exec(&format!("DROP DATABASE `{name}`"));
        });
    }
}
