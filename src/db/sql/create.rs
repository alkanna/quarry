//! Creating and dropping databases and tables. Every statement is built here from quoted identifiers,
//! numbers checked digit by digit and charset, collation and engine names that must appear in the
//! lists the server itself gave, so no user text ever reaches the server as SQL. Each statement is
//! sent alone, as a prepared statement so the server takes exactly one, on a connection of its own that is
//! switched to READ WRITE, like the row writes.

use super::se;
use super::ddl::{self, Mode};
use super::value::lit;
use super::worker::{database_names, text, text_at};
use super::write::{writer_conn, SqlUndo};
use super::{Db, Dialect};
use crate::db::{Msg, Res, UndoAction, UndoKind, Worker};
use sqlx::{Connection, Executor};
use std::time::{Duration, Instant};

/// Databases the server owns; creating or dropping them is refused before a statement exists.
pub const SYSTEM_SCHEMAS: [&str; 4] = ["information_schema", "mysql", "performance_schema", "sys"];

/// What a finished create or drop changed, so the app knows what to refresh.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ddl {
    CreatedDb,
    CreatedTable,
    CreatedSchema,
    DroppedDb,
    DroppedTable,
    DroppedSchema,
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
    /// lower_case_table_names is not 0: the server compares database and table names without case.
    pub fold_case: bool,
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
    /// lower_case_table_names is not 0, so names compare without case; read for a database only.
    pub fold_case: bool,
    /// PostgreSQL: the database is a template, which the server refuses to drop until it is turned back into a plain one.
    pub is_template: bool,
    /// PostgreSQL: the size of a database in bytes, and how many other sessions are connected to it right now.
    pub size: Option<u64>,
    pub sessions: Option<u64>,
    /// PostgreSQL: the connection opens on this database, so it cannot be dropped from here.
    pub own_database: bool,
    /// PostgreSQL: what a CASCADE would drop with a schema or table, the first few as the server describes them, and how many there are.
    pub dependents: Vec<String>,
    pub dependents_total: u64,
}

// Identifiers.

/// MySQL's own limits plus what quoting cannot make safe: control characters would make a name that
/// no one can read back, and a trailing space is stripped by some versions and refused by others.
/// Databases and tables are also directories and files, so path characters are refused for them.
/// These rules are for names this app creates; `check_existing` is the one for names already there.
pub fn check_name(what: &str, name: &str, path_chars: bool) -> Res<()> {
    check_existing(what, name)?;
    if name.chars().count() > 64 {
        return Err(format!("A {what} name is at most 64 characters."));
    }
    if name.starts_with(' ') || name.ends_with(' ') {
        return Err(format!("A {what} name cannot start or end with a space."));
    }
    if path_chars && name.contains(['/', '\\', '.']) {
        return Err(format!("A {what} name cannot contain / \\ or a dot."));
    }
    Ok(())
}

/// A database or table already on the server can be dropped whatever it is called (`a.b` or ` lead` exist
/// when another client made them), as long as the quoted name reads back as the one name.
fn check_existing(what: &str, name: &str) -> Res<()> {
    if name.is_empty() {
        return Err(format!("Give the {what} a name."));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(format!("A {what} name cannot contain control characters or line breaks."));
    }
    Ok(())
}

fn check_schema(name: &str) -> Res<()> {
    check_name("database", name, true)
}

/// `fold` is whether the server compares names without case (lower_case_table_names is not 0). Where it
/// does not, `MySQL` is a database of its own and not the server's `mysql`.
fn not_system(name: &str, fold: bool) -> Res<()> {
    if SYSTEM_SCHEMAS.iter().any(|s| if fold { s.eq_ignore_ascii_case(name) } else { *s == name }) {
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
    not_system(name, meta.fold_case)?;
    let cs = listed("charset", charset, meta.charsets.iter().map(|c| c.name.as_str()))?;
    let co = listed("collation", collation, meta.collations.iter().map(|c| c.name.as_str()))?;
    if meta.charset_of(co) != Some(cs) {
        return Err(format!("{co} does not belong to {cs}."));
    }
    Ok(format!("CREATE DATABASE {}\n  CHARACTER SET {cs} COLLATE {co}", d.quote_ident(name)))
}

pub fn drop_db_sql(d: Dialect, name: &str, fold: bool) -> Res<String> {
    check_existing("database", name)?;
    not_system(name, fold)?;
    Ok(format!("DROP DATABASE {}", d.quote_ident(name)))
}

pub fn drop_table_sql(d: Dialect, schema: &str, table: &str, view: bool, fold: bool) -> Res<String> {
    check_existing("database", schema)?;
    not_system(schema, fold)?;
    check_existing("table", table)?;
    Ok(format!("DROP {} {}", if view { "VIEW" } else { "TABLE" }, d.qualified(schema, table)))
}

pub fn truncate_sql(d: Dialect, schema: &str, table: &str, fold: bool) -> Res<String> {
    check_existing("database", schema)?;
    not_system(schema, fold)?;
    check_existing("table", table)?;
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
    check_existing("database", &spec.schema)?;
    not_system(&spec.schema, meta.fold_case)?;
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
const SERVER_DEFAULTS: &str = "SELECT @@character_set_server AS cs, @@collation_server AS co, @@default_storage_engine AS en, @@lower_case_table_names AS lc";
const SCHEMA_DEFAULT: &str = "SELECT DEFAULT_COLLATION_NAME AS co FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = ?";

async fn load_meta(db: &Db, schema: &str) -> Res<Meta> {
    let pool = db.my_pool()?;
    let (cs, co, en, df) = tokio::try_join!(
        async { sqlx::query(CHARSETS).fetch_all(pool).await.map_err(se) },
        async { sqlx::query(COLLATIONS).fetch_all(pool).await.map_err(se) },
        async { sqlx::query(ENGINES).fetch_all(pool).await.map_err(se) },
        async { sqlx::query(SERVER_DEFAULTS).fetch_one(pool).await.map_err(se) },
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
        fold_case: super::structure::num(&df, "lc").unwrap_or(0) != 0,
    };
    if !schema.is_empty() {
        meta.schema_collation = sqlx::query(SCHEMA_DEFAULT).bind(schema).fetch_optional(pool).await.map_err(se)?.and_then(|r| text(&r, "co"));
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
    let pool = db.my_pool()?;
    let exists = sqlx::query("SELECT 1 AS n FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = ?").bind(schema).fetch_optional(pool).await.map_err(se)?.is_some();
    let kinds = sqlx::query("SELECT TABLE_TYPE AS kind, COUNT(*) AS n, COALESCE(SUM(TABLE_ROWS), 0) AS r FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? GROUP BY TABLE_TYPE").bind(schema).fetch_all(pool).await.map_err(se)?;
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
    info.fold_case = sqlx::query("SELECT @@lower_case_table_names AS n").fetch_one(pool).await.ok().and_then(|r| super::structure::num(&r, "n")).unwrap_or(0) != 0;
    info.triggers = count(pool, "SELECT COUNT(*) AS n FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = ?", schema).await;
    info.routines = count(pool, "SELECT COUNT(*) AS n FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = ?", schema).await;
    info.events = count(pool, "SELECT COUNT(*) AS n FROM information_schema.EVENTS WHERE EVENT_SCHEMA = ?", schema).await;
    Ok(info)
}

/// How long the exact count of a drop confirmation may take before the server's estimate stands in.
const COUNT_WAIT: Duration = Duration::from_secs(3);

/// Rows, and the tables that point at this one. The reads run on a connection taken out of the pool, so the
/// session setting below stays off the pool and a count cut short never hands a busy connection back to it.
async fn table_info(db: &Db, schema: &str, table: &str) -> Res<DropInfo> {
    let pool = db.my_pool()?;
    let d = db.dialect;
    let mut c = db.acquire().await?.detach();
    fresh_stats(&mut c, d).await;
    let st = sqlx::query("SELECT TABLE_ROWS AS est, TABLE_TYPE AS kind FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?").bind(schema).bind(table).fetch_optional(&mut c).await.map_err(se);
    let st = match st {
        Ok(Some(st)) => st,
        other => {
            let _ = c.close().await;
            return other.map(|_| DropInfo::default());
        }
    };
    let mut info = DropInfo { exists: true, ..Default::default() };
    // Counting a view runs its query, which may take as long as it likes, and dropping one loses no rows.
    if text(&st, "kind").is_some_and(|k| k.contains("VIEW")) {
        let _ = c.close().await;
    } else {
        let sql = format!("SELECT COUNT(*) AS n FROM {}", d.qualified(schema, table));
        (info.rows, info.rows_estimated) = match bounded_count(c, pool, &sql).await {
            Some(n) => (Some(n), false),
            None => (super::structure::num(&st, "est"), true),
        };
    }
    let refs = sqlx::query(d.referenced_by()).bind(schema).bind(table).fetch_all(pool).await.map_err(se)?;
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

/// MySQL 8 caches TABLE_ROWS and the other statistics of information_schema.TABLES for a day, so a table
/// that filled since reads as 0. Only for this connection, which is never handed back to the pool; MariaDB
/// has no such cache and no variable.
async fn fresh_stats(c: &mut sqlx::MySqlConnection, d: Dialect) {
    match d {
        Dialect::MySql => {
            let _ = c.execute("SET SESSION information_schema_stats_expiry = 0").await;
        }
        Dialect::MariaDb | Dialect::Postgres => {}
    }
}

/// The exact count, or None after COUNT_WAIT. A count cut short is ended on the server before this returns,
/// so no COUNT keeps a table lock wait, the disk or a connection busy once the dialog has its answer.
async fn bounded_count(mut c: sqlx::MySqlConnection, pool: &sqlx::MySqlPool, sql: &str) -> Option<u64> {
    let id = sqlx::query("SELECT CONNECTION_ID() AS n").fetch_one(&mut c).await.ok().and_then(|r| super::structure::num(&r, "n"));
    let got = tokio::time::timeout(COUNT_WAIT, sqlx::query(sql).persistent(false).fetch_one(&mut c)).await;
    match got {
        Ok(r) => {
            let _ = c.close().await;
            r.ok().and_then(|r| super::structure::num(&r, "n"))
        }
        Err(_) => {
            // Left in the middle of a statement, so it cannot be used again; the KILL ends it on the server.
            drop(c);
            if let Some(id) = id {
                kill(pool, id).await;
            }
            None
        }
    }
}

/// Ends connection `id` and its statement, then waits for the server to let it go, since KILL only flags the
/// thread and it stops at its next check.
async fn kill(pool: &sqlx::MySqlPool, id: u64) {
    let _ = sqlx::raw_sql(&format!("KILL {id}")).execute(pool).await;
    for _ in 0..30 {
        let left = sqlx::query("SELECT COUNT(*) AS n FROM information_schema.PROCESSLIST WHERE ID = ?").bind(id).fetch_one(pool).await.ok().and_then(|r| super::structure::num(&r, "n"));
        if left.unwrap_or(0) == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn table_names(c: &mut sqlx::MySqlConnection, d: Dialect, schema: &str) -> Res<Vec<String>> {
    let rows = sqlx::query(&format!("SHOW TABLES FROM {}", d.quote_ident(schema))).persistent(false).fetch_all(&mut *c).await.map_err(se)?;
    Ok(rows.iter().filter_map(|r| text_at(r, 0)).collect())
}

/// The name the server lists for what was just created as `name`. With lower_case_table_names=1 it stores
/// `Billing` as `billing`, and the sidebar, the tabs and undo must all use the stored one.
pub fn stored_name(listed: &[String], name: &str) -> String {
    if listed.iter().any(|n| n == name) {
        return name.to_string();
    }
    let low = name.to_lowercase();
    listed.iter().find(|n| n.to_lowercase() == low).cloned().unwrap_or_else(|| name.to_string())
}

/// Whether the server compares database and table names without case (lower_case_table_names 1 or 2).
async fn folds_case(c: &mut sqlx::MySqlConnection) -> Res<bool> {
    let r = sqlx::query("SELECT @@lower_case_table_names AS n").fetch_one(&mut *c).await.map_err(se)?;
    Ok(super::structure::num(&r, "n").unwrap_or(0) != 0)
}

/// When the table was made, as the server writes it, so an undo drops it only while it is still that table.
async fn create_time(c: &mut sqlx::MySqlConnection, schema: &str, table: &str) -> Option<String> {
    let r = sqlx::query("SELECT CAST(CREATE_TIME AS CHAR) AS t FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?").bind(schema).bind(table).fetch_optional(&mut *c).await.ok()??;
    text(&r, "t")
}

/// A writable connection and the sql_mode its session reads text with. The CREATE TABLE is built for that
/// mode and runs on this same connection, so a mode changed since connecting (NO_BACKSLASH_ESCAPES turned on
/// or off) cannot make a backslash in a default value end the string. A changed mode is handed back too, so
/// the app's previews follow the server from then on.
async fn mode_session(db: &Db, read_only: bool) -> Res<(sqlx::MySqlConnection, Mode, Option<String>)> {
    let m = Mode::of(&db.sql_mode);
    match ddl::session(db, "", read_only, m).await {
        Ok(c) => Ok((c, m, None)),
        Err((_, Some(now))) => {
            let m = Mode::of(&now);
            ddl::session(db, "", read_only, m).await.map(|c| (c, m, Some(now))).map_err(|(x, _)| x)
        }
        Err((x, None)) => Err(x),
    }
}

/// Runs the statement `build` makes (told whether the server folds name case) on a writable connection of
/// its own, prepared so the server takes exactly one statement, and for changes to the list of databases
/// reads that list back on the same connection.
async fn run(db: &Db, read_only: bool, list: bool, build: impl FnOnce(bool) -> Res<String>) -> Res<Option<Vec<String>>> {
    let mut c = writer_conn(db, "", read_only).await?.into_my()?;
    let out = async {
        let sql = build(folds_case(&mut c).await?)?;
        ddl::exec(&mut c, &sql).await?;
        if list { database_names(&mut c).await.map(Some) } else { Ok(None) }
    }
    .await;
    let _ = c.close().await;
    out
}

fn created(conn: u64, schema: &str, table: &str, undo: SqlUndo) -> UndoAction {
    UndoAction { at: Instant::now(), conn, db: schema.into(), coll: table.into(), kind: UndoKind::Sql { database: String::new(), cols: Vec::new(), undo, ids: Default::default() } }
}

/// The part of a table undo that runs with the table locked: it must still be the table the create made,
/// and empty. CREATE_TIME has one second steps, so a table dropped and made again within the same second as
/// the create passes; it is then just as empty.
async fn drop_if_same_and_empty(c: &mut sqlx::MySqlConnection, schema: &str, table: &str, made: Option<&str>, q: &str, sql: &str) -> Res<()> {
    if let Some(made) = made {
        if create_time(c, schema, table).await.as_deref() != Some(made) {
            return Err(format!("Not undone: {table} was dropped and made again since, so it is not the table this created."));
        }
    }
    let has = sqlx::query(&format!("SELECT 1 AS n FROM {q} LIMIT 1")).persistent(false).fetch_optional(&mut *c).await.map_err(se)?.is_some();
    if has {
        return Err(format!("Not undone: {table} has rows now."));
    }
    ddl::exec(c, sql).await
}

/// Undo of a create: the object goes only while it is still empty. The writable connection is opened first,
/// so a locked connection refuses before anything is read and the check and the drop run one round trip
/// apart on one connection. A table is also locked, so a row cannot arrive between the check and the drop.
/// A database cannot be locked, so a table created in it in that instant would go with it.
async fn undo_create(db: &Db, schema: &str, table: &str, made: Option<&str>, read_only: bool) -> Res<(Ddl, Option<Vec<String>>)> {
    let d = db.dialect;
    let mut c = writer_conn(db, "", read_only).await?.into_my()?;
    let res = async {
        let fold = folds_case(&mut c).await?;
        if table.is_empty() {
            let sql = drop_db_sql(d, schema, fold)?;
            let n = sqlx::query("SELECT (SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = ?) + (SELECT COUNT(*) FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = ?) + (SELECT COUNT(*) FROM information_schema.EVENTS WHERE EVENT_SCHEMA = ?) AS n")
                .bind(schema)
                .bind(schema)
                .bind(schema)
                .fetch_one(&mut c)
                .await
                .map_err(se)?;
            if super::structure::num(&n, "n").unwrap_or(0) > 0 {
                return Err(format!("Not undone: {schema} has tables or other objects in it now."));
            }
            ddl::exec(&mut c, &sql).await?;
            return database_names(&mut c).await.map(|l| (Ddl::DroppedDb, Some(l)));
        }
        let sql = drop_table_sql(d, schema, table, false, fold)?;
        fresh_stats(&mut c, d).await;
        let q = d.qualified(schema, table);
        // Without the LOCK TABLES privilege the checks still run, only without the lock. MySQL cannot prepare LOCK
        // TABLES (error 1295), so it goes as text; it holds nothing but the quoted names.
        let locked = c.execute(format!("LOCK TABLES {q} WRITE").as_str()).await.is_ok();
        let res = drop_if_same_and_empty(&mut c, schema, table, made, &q, &sql).await;
        if locked {
            let _ = c.execute("UNLOCK TABLES").await;
        }
        res.map(|_| (Ddl::DroppedTable, None))
    }
    .await;
    let _ = c.close().await;
    res
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
            Msg::SqlDropInfo { conn, database: String::new(), schema, table, res }
        });
    }

    pub fn sql_create_db(&self, conn: u64, db: Db, name: String, charset: String, collation: String, meta: Meta, read_only: bool) {
        self.spawn_sql(db.conn, "", async move {
            let mut meta = meta;
            let res = run(&db, read_only, true, |fold| {
                meta.fold_case = fold;
                create_db_sql(db.dialect, &name, &charset, &collation, &meta)
            })
            .await;
            let name = res.as_ref().ok().and_then(|l| l.as_deref()).map_or(name.clone(), |l| stored_name(l, &name));
            let undo = res.is_ok().then(|| created(conn, &name, "", SqlUndo::CreatedDb { oid: String::new(), baseline: 0 }));
            let (res, dbs) = split(res, format!("Created database {name}"));
            Msg::SqlDdl { conn, database: String::new(), schema: name, table: String::new(), what: Ddl::CreatedDb, res, undo, dbs, sql_mode: None }
        });
    }

    pub fn sql_create_table(&self, conn: u64, db: Db, spec: TableSpec, meta: Meta, read_only: bool) {
        self.spawn_sql(db.conn, &spec.schema.clone(), async move {
            let (mut name, mut made, mut seen) = (spec.name.clone(), None, None);
            let res: Res<()> = async {
                let (mut c, m, now) = mode_session(&db, read_only).await?;
                seen = now;
                let out = async {
                    let mut meta = meta;
                    meta.fold_case = folds_case(&mut c).await?;
                    let sql = create_table_sql(db.dialect, m.bs, &spec, &meta)?;
                    ddl::exec(&mut c, &sql).await?;
                    // The table exists from here on, so a failed read back only costs the stored spelling.
                    name = stored_name(&table_names(&mut c, db.dialect, &spec.schema).await.unwrap_or_default(), &spec.name);
                    fresh_stats(&mut c, db.dialect).await;
                    made = create_time(&mut c, &spec.schema, &name).await;
                    Ok(())
                }
                .await;
                let _ = c.close().await;
                out
            }
            .await;
            let undo = res.is_ok().then(|| created(conn, &spec.schema, &name, SqlUndo::CreatedTable { made }));
            let (res, dbs) = split(res.map(|_| None), format!("Created table {name}"));
            Msg::SqlDdl { conn, database: String::new(), schema: spec.schema, table: name, what: Ddl::CreatedTable, res, undo, dbs, sql_mode: seen }
        });
    }

    /// The database list and the tables of `schema`, for what a script may have created or dropped anywhere.
    pub fn sql_lists(&self, conn: u64, db: Db, schema: String) {
        self.spawn(async move {
            let res = async {
                let mut c = db.acquire().await?;
                let dbs = database_names(&mut *c).await?;
                let tables = if schema.is_empty() || !dbs.contains(&schema) { None } else { Some(table_names(&mut *c, db.dialect, &schema).await?) };
                Ok((dbs, tables))
            }
            .await;
            Msg::SqlLists { conn, schema, res }
        });
    }

    /// DROP DATABASE, DROP TABLE, DROP VIEW or TRUNCATE; `table` is empty for a database. No undo.
    #[allow(clippy::too_many_arguments)]
    pub fn sql_drop(&self, conn: u64, db: Db, schema: String, table: String, view: bool, truncate: bool, read_only: bool) {
        self.spawn_sql(db.conn, &schema.clone(), async move {
            let d = db.dialect;
            let (what, done) = if table.is_empty() {
                (Ddl::DroppedDb, format!("Dropped database {schema}"))
            } else if truncate {
                (Ddl::Emptied, format!("Emptied table {table}"))
            } else {
                (Ddl::DroppedTable, format!("Dropped {} {table}", if view { "view" } else { "table" }))
            };
            let res = run(&db, read_only, what == Ddl::DroppedDb, |fold| match what {
                Ddl::DroppedDb => drop_db_sql(d, &schema, fold),
                Ddl::Emptied => truncate_sql(d, &schema, &table, fold),
                _ => drop_table_sql(d, &schema, &table, view, fold),
            })
            .await;
            let (res, dbs) = split(res, done);
            Msg::SqlDdl { conn, database: String::new(), schema, table, what, res, undo: None, dbs, sql_mode: None }
        });
    }

    /// Undoes a create when the object is still empty. A refused undo hands the entry back.
    pub fn sql_undo_create(&self, db: Db, a: UndoAction, read_only: bool) {
        let scope = a.db.clone();
        if db.dialect == Dialect::Postgres {
            return self.spawn_sql(db.conn, &scope, super::create_pg::undo(db.clone(), a, read_only));
        }
        self.spawn_sql(db.conn, &scope, async move {
            let UndoAction { at, conn, db: schema, coll: table, kind } = a;
            let made = match &kind {
                UndoKind::Sql { undo: SqlUndo::CreatedTable { made }, .. } => made.clone(),
                _ => None,
            };
            let res = undo_create(&db, &schema, &table, made.as_deref(), read_only).await;
            let (what, undone) = match &res {
                Ok((w, _)) => (*w, true),
                Err(_) => (if table.is_empty() { Ddl::DroppedDb } else { Ddl::DroppedTable }, false),
            };
            let said = if table.is_empty() { format!("Removed database {schema}") } else { format!("Removed table {table}") };
            let (res, dbs) = split(res.map(|(_, l)| l), said);
            let undo = (!undone).then(|| UndoAction { at, conn, db: schema.clone(), coll: table.clone(), kind });
            // A refused undo changes nothing, so the app has nothing to refresh.
            Msg::SqlDdl { conn, database: String::new(), schema, table, what, res, undo, dbs, sql_mode: None }
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
            fold_case: false,
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
            assert_eq!(lex::count_with(sql, crate::db::sql::Dialect::MySql, bs, ansi), 1, "bs={bs} ansi={ansi}: {sql}");
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
            let t = drop_table_sql(Dialect::MySql, "shop", name, false, false).unwrap();
            assert_eq!(t, format!("DROP TABLE `shop`.{}", Dialect::MySql.quote_ident(name)));
            one_statement(&t);
        }
        for name in ["", "a\nb", "a\rb", "tab\tx", "nul\0x", " lead", "trail ", "a/b", "a.b", "a\\b", &"x".repeat(65)] {
            assert!(create_db_sql(Dialect::MySql, name, "utf8mb4", "utf8mb4_bin", &m).is_err(), "{name:?}");
            assert!(create_table_sql(Dialect::MySql, true, &spec(name, vec![ColumnSpec::id()]), &m).is_err(), "{name:?}");
        }
        // Only an empty name or a control character stops a drop: the rest are names that exist.
        for name in ["", "a\nb", "a\rb", "tab\tx", "nul\0x"] {
            assert!(drop_db_sql(Dialect::MySql, name, false).is_err(), "{name:?}");
            assert!(truncate_sql(Dialect::MySql, "shop", name, false).is_err(), "{name:?}");
            assert!(drop_table_sql(Dialect::MySql, name, "t", false, false).is_err(), "{name:?}");
        }
        assert!(create_db_sql(Dialect::MySql, &"x".repeat(64), "utf8mb4", "utf8mb4_bin", &m).is_ok());
    }

    /// Review finding 4: tables and databases another client named `a.b`, ` lead` or `x.y` could not be dropped
    /// or emptied, because the rules for new names applied to drops too.
    #[test]
    fn existing_names_with_dots_slashes_or_spaces_can_be_dropped() {
        let d = Dialect::MySql;
        for name in ["a.b", " lead", "x.y", "trail ", "a/b", "a\\b", &"x".repeat(65)] {
            let q = d.quote_ident(name);
            assert_eq!(drop_db_sql(d, name, false).unwrap(), format!("DROP DATABASE {q}"));
            assert_eq!(drop_table_sql(d, name, name, false, false).unwrap(), format!("DROP TABLE {q}.{q}"));
            assert_eq!(drop_table_sql(d, "shop", name, true, false).unwrap(), format!("DROP VIEW `shop`.{q}"));
            assert_eq!(truncate_sql(d, name, name, false).unwrap(), format!("TRUNCATE TABLE {q}.{q}"));
            one_statement(&truncate_sql(d, name, name, false).unwrap());
        }
        // A new table may still go into an existing database with such a name.
        let mut s = spec("t", vec![ColumnSpec::id()]);
        s.schema = "x.y".into();
        assert!(create_table_sql(d, true, &s, &meta()).unwrap().starts_with("CREATE TABLE `x.y`.`t`"));
    }

    #[test]
    fn system_schemas_are_left_alone() {
        let folding = Meta { fold_case: true, ..meta() };
        for s in ["mysql", "MYSQL", "sys", "information_schema", "performance_schema"] {
            assert!(drop_db_sql(Dialect::MySql, s, true).is_err());
            assert!(create_db_sql(Dialect::MySql, s, "utf8mb4", "utf8mb4_bin", &folding).is_err());
            assert!(drop_table_sql(Dialect::MySql, s, "user", false, true).is_err());
            assert!(truncate_sql(Dialect::MySql, s, "user", true).is_err());
        }
        for s in ["mysql", "sys", "information_schema", "performance_schema"] {
            assert!(drop_db_sql(Dialect::MySql, s, false).is_err());
            assert!(create_db_sql(Dialect::MySql, s, "utf8mb4", "utf8mb4_bin", &meta()).is_err());
        }
    }

    /// Review finding 6: with lower_case_table_names=0 the server keeps `MySQL` apart from `mysql`, so it is a
    /// user database that can be made and dropped.
    #[test]
    fn a_system_name_in_other_case_is_a_user_database_where_case_counts() {
        assert_eq!(drop_db_sql(Dialect::MySql, "MySQL", false).unwrap(), "DROP DATABASE `MySQL`");
        assert!(create_db_sql(Dialect::MySql, "SYS", "utf8mb4", "utf8mb4_bin", &meta()).is_ok());
        assert!(truncate_sql(Dialect::MySql, "Information_Schema", "t", false).is_ok());
        assert!(drop_db_sql(Dialect::MySql, "MySQL", true).is_err());
    }

    /// Review finding 6: with lower_case_table_names=1 `Billing` is stored as `billing`, and the app must open
    /// and undo the stored name.
    #[test]
    fn a_created_name_is_mapped_to_how_the_server_lists_it() {
        let listed: Vec<String> = ["billing", "Shop", "ünï"].map(String::from).to_vec();
        assert_eq!(stored_name(&listed, "Billing"), "billing");
        assert_eq!(stored_name(&listed, "Shop"), "Shop");
        assert_eq!(stored_name(&listed, "ÜNÏ"), "ünï");
        assert_eq!(stored_name(&listed, "other"), "other");
        // An exact match wins over a folded one.
        let both: Vec<String> = ["a", "A"].map(String::from).to_vec();
        assert_eq!(stored_name(&both, "A"), "A");
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
                assert_eq!(lex::count_with(&sql, crate::db::sql::Dialect::MySql, bs, ansi), 1, "bs={bs} ansi={ansi}: {sql}");
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
        assert_eq!(drop_table_sql(d, "shop", "orders", false, false).unwrap(), "DROP TABLE `shop`.`orders`");
        assert_eq!(drop_table_sql(d, "shop", "v", true, false).unwrap(), "DROP VIEW `shop`.`v`");
        assert_eq!(drop_db_sql(d, "shop", false).unwrap(), "DROP DATABASE `shop`");
        assert_eq!(truncate_sql(d, "sh`op", "or`ders", false).unwrap(), "TRUNCATE TABLE `sh``op`.`or``ders`");
        one_statement(&truncate_sql(d, "sh`op", "x; DROP TABLE y", false).unwrap());
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
        /// The whole SqlDdl answer, with the names it reports and the sql_mode it hands back.
        fn answer(&self) -> Answer {
            let Msg::SqlDdl { schema, table, res, undo, sql_mode, .. } = self.next() else { panic!("expected SqlDdl") };
            Answer { schema, table, res, undo, sql_mode }
        }
        fn text_of(&self, sql: &str) -> String {
            self.w.rt.block_on(async { crate::db::sql::worker::text_at(&sqlx::query(sql).fetch_one(&self.pool).await.unwrap_or_else(|x| panic!("{sql}: {x}")), 0).unwrap_or_default() })
        }
        fn utf8mb4(&self) -> (Meta, String) {
            let m = self.meta("");
            let co = m.default_collation("utf8mb4").unwrap().to_string();
            (m, co)
        }
        fn drop_info(&self, schema: &str, table: &str) -> (DropInfo, Duration) {
            let t = Instant::now();
            self.w.sql_drop_info(1, self.db.clone(), schema.into(), table.into());
            let Msg::SqlDropInfo { res, .. } = self.next() else { panic!("expected SqlDropInfo") };
            (res.unwrap_or_else(|x| panic!("{}: {x}", self.label)), t.elapsed())
        }
        /// Statements still running on the server whose text starts with `start`.
        fn running(&self, start: &str) -> i64 {
            self.scalar(&format!("SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE INFO LIKE {}", lit(&format!("{start}%"), true)))
        }
    }

    struct Answer {
        schema: String,
        table: String,
        res: Res<String>,
        undo: Option<UndoAction>,
        sql_mode: Option<String>,
    }

    fn table_spec(schema: &str, name: &str, co: &str, engine: &str, columns: Vec<ColumnSpec>) -> TableSpec {
        TableSpec { schema: schema.into(), name: name.into(), engine: engine.into(), collation: co.into(), columns }
    }

    fn text_default(name: &str, value: &str) -> ColumnSpec {
        ColumnSpec { default: DefaultKind::Value, default_text: value.into(), ..col(name, "VARCHAR", "80") }
    }

    /// Review finding 1: the connect-time sql_mode had NO_BACKSLASH_ESCAPES and the session no longer does, so a
    /// default written for the old mode ended its string at the backslash and the rest ran as statements; it
    /// dropped `victim`. The other way round, `C:\temp` was stored as `C:\\temp`.
    #[test]
    fn live_a_default_value_stays_text_when_the_sql_mode_changed_since_connecting() {
        each(|l| {
            let (schema, victim) = (unique("qcd_inj"), unique("qcd_victim"));
            l.exec(&format!("CREATE DATABASE `{schema}`"));
            l.exec(&format!("CREATE DATABASE `{victim}`"));
            let (m, co) = l.utf8mb4();
            let evil = format!("\\'); DROP DATABASE {victim}; -- ");
            // The app still believes in the mode it read when it connected.
            let mut stale = l.db.clone();
            stale.sql_mode = format!("{},NO_BACKSLASH_ESCAPES", stale.sql_mode);
            l.w.sql_create_table(1, stale, table_spec(&schema, "t", &co, &m.server_engine, vec![ColumnSpec::id(), text_default("note", &evil)]), m.clone(), false);
            let a = l.answer();
            assert!(a.res.is_ok(), "{}: {:?}", l.label, a.res);
            assert!(l.exists_db(&victim), "{}: the default value ran as a statement", l.label);
            assert_eq!(a.sql_mode.as_deref().map(Mode::of), Some(Mode::of(&l.db.sql_mode)), "{}: the mode in force goes back to the app", l.label);
            l.exec(&format!("INSERT INTO `{schema}`.t () VALUES ()"));
            assert_eq!(l.text_of(&format!("SELECT note FROM `{schema}`.t")), evil, "{}", l.label);
            // The session reads backslashes as plain characters, the app thinks it does not.
            let nbe = crate::db::sql::testing::with_session(&l.w, &l.db, "SET SESSION sql_mode = CONCAT(@@SESSION.sql_mode, ',NO_BACKSLASH_ESCAPES')");
            l.w.sql_create_table(1, nbe.clone(), table_spec(&schema, "path", &co, &m.server_engine, vec![ColumnSpec::id(), text_default("p", "C:\\temp")]), m.clone(), false);
            let a = l.answer();
            assert!(a.res.is_ok(), "{}: {:?}", l.label, a.res);
            assert!(a.sql_mode.is_some_and(|s| !Mode::of(&s).bs), "{}", l.label);
            l.exec(&format!("INSERT INTO `{schema}`.path () VALUES ()"));
            assert_eq!(l.text_of(&format!("SELECT p FROM `{schema}`.path")), "C:\\temp", "{}", l.label);
            // A mode that matches is not handed back.
            l.w.sql_create_table(1, l.db.clone(), table_spec(&schema, "same", &co, &m.server_engine, vec![ColumnSpec::id(), text_default("p", "a\\b")]), m, false);
            let a = l.answer();
            assert!(a.res.is_ok() && a.sql_mode.is_none(), "{}: {:?}", l.label, a.res);
            l.exec(&format!("INSERT INTO `{schema}`.same () VALUES ()"));
            assert_eq!(l.text_of(&format!("SELECT p FROM `{schema}`.same")), "a\\b", "{}", l.label);
            l.exec(&format!("DROP DATABASE `{schema}`"));
            l.exec(&format!("DROP DATABASE `{victim}`"));
        });
    }

    /// Review findings 2 and 3: a table whose cached TABLE_ROWS is 0 filled while another session holds LOCK
    /// TABLES ... WRITE. The COUNT waited on the lock and held a pool connection, and the estimate said empty.
    /// Now the answer comes after about 3 s with a fresh estimate, and no COUNT is left on the server.
    #[test]
    fn live_a_locked_table_gets_a_fresh_estimate_in_about_3s_and_no_count_is_left() {
        each(|l| {
            let schema = unique("qcd_cnt");
            l.exec(&format!("CREATE DATABASE `{schema}`"));
            l.exec(&format!("CREATE TABLE `{schema}`.t (id INT PRIMARY KEY AUTO_INCREMENT, v INT) ENGINE=InnoDB"));
            // A read with the default expiry caches the empty table's statistics on MySQL.
            assert_eq!(l.scalar(&format!("SELECT CAST(COALESCE(TABLE_ROWS, 0) AS SIGNED) FROM information_schema.TABLES WHERE TABLE_SCHEMA = '{schema}' AND TABLE_NAME = 't'")), 0);
            let values = (1..=1000).map(|i| format!("({i})")).collect::<Vec<_>>().join(",");
            let mut lock = l.w.rt.block_on(async {
                let mut c = sqlx::MySqlConnection::connect_with(l.db.opts.my().unwrap()).await.unwrap();
                c.execute(format!("LOCK TABLES `{schema}`.t WRITE").as_str()).await.unwrap();
                c.execute(format!("INSERT INTO `{schema}`.t (v) VALUES {values}").as_str()).await.unwrap();
                c
            });
            let (info, took) = l.drop_info(&schema, "t");
            assert!(info.exists && info.rows_estimated, "{}: {info:?}", l.label);
            assert!(info.rows.is_some_and(|n| n > 0), "{}: an estimate of 0 rows is not an empty table: {info:?}", l.label);
            assert!(took >= Duration::from_millis(2500) && took < Duration::from_secs(8), "{}: {took:?}", l.label);
            assert_eq!(l.running(&format!("SELECT COUNT(*) AS n FROM `{schema}`")), 0, "{}: a COUNT is still running", l.label);
            l.w.rt.block_on(async {
                lock.execute("UNLOCK TABLES").await.unwrap();
                lock.close().await.unwrap();
            });
            // Unlocked, the count is exact again.
            let (info, _) = l.drop_info(&schema, "t");
            assert!(info.rows == Some(1000) && !info.rows_estimated, "{}: {info:?}", l.label);
            l.exec(&format!("DROP DATABASE `{schema}`"));
        });
    }

    /// Review finding 3: a view was counted, which runs its query; one that sleeps held the dialog and a pool
    /// connection for as long as it slept.
    #[test]
    fn live_a_slow_view_is_not_counted() {
        each(|l| {
            let schema = unique("qcd_view");
            l.exec(&format!("CREATE DATABASE `{schema}`"));
            l.exec(&format!("CREATE VIEW `{schema}`.slow AS SELECT SLEEP(25) AS s"));
            let (info, took) = l.drop_info(&schema, "slow");
            assert!(info.exists && info.rows.is_none() && !info.rows_estimated, "{}: {info:?}", l.label);
            assert!(took < Duration::from_secs(2), "{}: {took:?}", l.label);
            assert_eq!(l.running(&format!("SELECT COUNT(*) AS n FROM `{schema}`")), 0, "{}", l.label);
            l.exec(&format!("DROP DATABASE `{schema}`"));
        });
    }

    /// Review finding 4: names another client made, with a dot or a leading space, were refused for drop and empty.
    #[test]
    fn live_existing_names_with_a_dot_or_a_leading_space_can_be_emptied_and_dropped() {
        each(|l| {
            let schema = format!("{}.y", unique("qcd_x"));
            l.exec(&format!("CREATE DATABASE `{schema}`"));
            l.exec(&format!("CREATE TABLE `{schema}`.` lead` (a INT)"));
            l.exec(&format!("CREATE TABLE `{schema}`.`a.b` (a INT)"));
            l.exec(&format!("INSERT INTO `{schema}`.` lead` VALUES (1),(2)"));
            l.w.sql_drop(1, l.db.clone(), schema.clone(), " lead".into(), false, true, false);
            assert!(l.ddl().1.is_ok(), "{}", l.label);
            assert_eq!(l.scalar(&format!("SELECT COUNT(*) FROM `{schema}`.` lead`")), 0);
            l.w.sql_drop(1, l.db.clone(), schema.clone(), "a.b".into(), false, false, false);
            let (_, res, _, _) = l.ddl();
            assert!(res.is_ok(), "{}: {res:?}", l.label);
            assert!(!l.exists_table(&schema, "a.b") && l.exists_table(&schema, " lead"));
            l.w.sql_drop(1, l.db.clone(), schema.clone(), String::new(), false, false, false);
            let (_, res, _, dbs) = l.ddl();
            assert!(res.is_ok(), "{}: {res:?}", l.label);
            assert!(!dbs.unwrap().contains(&schema) && !l.exists_db(&schema));
        });
    }

    /// Review finding 5: the undo of a database create checked on the pool before it had a writable connection;
    /// now a locked connection refuses first, whatever the database holds.
    #[test]
    fn live_undo_of_a_database_create_opens_the_writer_before_checking() {
        each(|l| {
            let name = unique("qcd_uw");
            let (_, co) = l.utf8mb4();
            let (_, res, undo, _) = l.create_db(&name, "utf8mb4", &co, false);
            assert!(res.is_ok(), "{}: {res:?}", l.label);
            l.exec(&format!("CREATE TABLE `{name}`.t (a INT)"));
            let (_, res, back, _) = l.undo(undo.unwrap(), true);
            assert!(res.as_ref().unwrap_err().contains("locked"), "{}: {res:?}", l.label);
            let (_, res, back, _) = l.undo(back.unwrap(), false);
            assert!(res.unwrap_err().contains("Not undone"), "{}", l.label);
            l.exec(&format!("DROP TABLE `{name}`.t"));
            assert!(l.undo(back.unwrap(), false).1.is_ok());
            assert!(!l.exists_db(&name));
        });
    }

    /// Review finding 5: a table dropped and made again by someone else after the create was dropped by the undo
    /// of the create. The undo now compares CREATE_TIME under the lock.
    #[test]
    fn live_undo_of_a_table_create_refuses_a_table_made_again_since() {
        each(|l| {
            let schema = unique("qcd_again");
            l.exec(&format!("CREATE DATABASE `{schema}`"));
            let (m, co) = l.utf8mb4();
            l.w.sql_create_table(1, l.db.clone(), table_spec(&schema, "t", &co, &m.server_engine, vec![ColumnSpec::id()]), m, false);
            let a = l.answer();
            assert!(a.res.is_ok(), "{}: {:?}", l.label, a.res);
            let undo = a.undo.unwrap();
            assert!(matches!(&undo.kind, UndoKind::Sql { undo: SqlUndo::CreatedTable { made: Some(_) }, .. }), "{}", l.label);
            // CREATE_TIME counts whole seconds.
            std::thread::sleep(Duration::from_millis(1100));
            l.exec(&format!("DROP TABLE `{schema}`.t"));
            l.exec(&format!("CREATE TABLE `{schema}`.t (other INT)"));
            let (_, res, back, _) = l.undo(undo, false);
            assert!(res.unwrap_err().contains("made again"), "{}", l.label);
            assert!(back.is_some() && l.exists_table(&schema, "t"));
            l.exec(&format!("DROP DATABASE `{schema}`"));
        });
    }

    /// Review finding 6: under lower_case_table_names=1 `Qcd_Caps` is stored as `qcd_caps`, and the app opened and
    /// undid the name it asked for. The answer now carries what SHOW DATABASES and SHOW TABLES list. Run it
    /// against a server started with --lower-case-table-names=1 to see the folding; elsewhere names stay as typed.
    #[test]
    fn live_a_created_name_comes_back_as_the_server_lists_it() {
        each(|l| {
            let fold = l.scalar("SELECT CAST(@@lower_case_table_names AS SIGNED)") == 1;
            let name = unique("Qcd_Caps");
            let (m, co) = l.utf8mb4();
            l.w.sql_create_db(1, l.db.clone(), name.clone(), "utf8mb4".into(), co.clone(), m.clone(), false);
            let a = l.answer();
            assert!(a.res.is_ok(), "{}: {:?}", l.label, a.res);
            let listed = if fold { name.to_lowercase() } else { name.clone() };
            assert_eq!(a.schema, listed, "{}", l.label);
            assert_eq!(a.undo.as_ref().unwrap().db, listed);
            l.w.sql_create_table(1, l.db.clone(), table_spec(&listed, "Caps_T", &co, &m.server_engine, vec![ColumnSpec::id()]), m, false);
            let t = l.answer();
            assert!(t.res.is_ok(), "{}: {:?}", l.label, t.res);
            assert_eq!(t.table, if fold { "caps_t" } else { "Caps_T" }, "{}", l.label);
            assert!(l.exists_table(&listed, &t.table));
            assert!(l.undo(t.undo.unwrap(), false).1.is_ok(), "{}", l.label);
            assert!(l.undo(a.undo.unwrap(), false).1.is_ok(), "{}", l.label);
            assert!(!l.exists_db(&listed));
        });
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
            let undo = created(1, &name, "", SqlUndo::CreatedDb { oid: String::new(), baseline: 0 });
            let (_, res, back, _) = l.undo(undo, true);
            assert!(res.is_err() && back.is_some());
            l.exec(&format!("DROP DATABASE `{name}`"));
        });
    }
}
