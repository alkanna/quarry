//! ALTER TABLE for the Structure view: column and index changes are staged as `Change`s, several of
//! them make one statement, and the statements that undo them are worked out from the structure read
//! before applying. Everything here but the worker part is pure text building. Names go through
//! `quote_ident`, strings through `lit`, and the few places that take SQL from the user (an expression
//! default) are checked. Every statement runs as a prepared statement, which holds exactly one.

use super::se;
use super::structure::{load, split_type, Column, ForeignKey, Index, Structure, Trigger};
use super::{Db, Dialect};
#[cfg(test)]
use my::definition;
#[cfg(test)]
use super::Pool;
use crate::db::{e, Msg, Res, UndoAction, UndoKind, Worker};
use std::collections::HashSet;
use std::time::Instant;

mod fk;
mod my;
mod sim;
mod trig;
mod worker;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod live;
#[cfg(test)]
mod review_live;
#[cfg(test)]
mod live_rel;

pub use fk::*;
pub use my::alter;
pub use sim::*;
pub use trig::*;
pub use worker::*;

/// How a session reads statement text: backslash escapes in strings (off under NO_BACKSLASH_ESCAPES) and double
/// quotes as identifiers (ANSI_QUOTES). Literals and the expression check are written for one of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode {
    pub bs: bool,
    pub ansi: bool,
}

impl Mode {
    pub const DEFAULT: Mode = Mode { bs: true, ansi: false };

    /// From `@@sql_mode`, where ANSI also shows as its parts.
    pub fn of(sql_mode: &str) -> Mode {
        let u = sql_mode.to_ascii_uppercase();
        let has = |w: &str| u.split(',').any(|p| p.trim() == w);
        Mode { bs: !has("NO_BACKSLASH_ESCAPES"), ansi: has("ANSI_QUOTES") || has("ANSI") }
    }
}

/// The session's own sql_mode text. Statements and literals are written for the mode of the very connection that
/// runs them, never for the one read at connect, which may have changed since.
pub(super) async fn session_sql_mode(c: &mut sqlx::MySqlConnection) -> Res<String> {
    sqlx::query_scalar("SELECT @@SESSION.sql_mode").fetch_one(&mut *c).await.map_err(se)
}

/// `session_sql_mode`, read as a `Mode`.
pub(super) async fn session_mode(c: &mut sqlx::MySqlConnection) -> Res<Mode> {
    Ok(Mode::of(&session_sql_mode(c).await?))
}

#[derive(Clone, Debug, PartialEq, Default)]
pub enum Dv {
    /// No DEFAULT clause.
    #[default]
    None,
    Null,
    /// Text of a literal, quoted (or written bare for numbers) when the statement is built.
    Value(String),
    /// SQL such as CURRENT_TIMESTAMP, checked by `check_expr`.
    Expr(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColDef {
    pub name: String,
    /// One word, upper case: "DECIMAL".
    pub ty: String,
    /// "12,2" or "'a','b'": a length list, or the values of an ENUM or SET written as the server writes
    /// them, with backslash escapes in every sql_mode.
    pub args: String,
    pub unsigned: bool,
    pub zerofill: bool,
    pub nullable: bool,
    pub default: Dv,
    pub auto_increment: bool,
    /// The charset follows from the collation's name.
    pub collation: Option<String>,
    /// Kept from the server, such as "CURRENT_TIMESTAMP".
    pub on_update: Option<String>,
    pub comment: String,
    /// MySQL's SRID of a spatial column, which a MODIFY without it would drop.
    pub srid: Option<u32>,
    /// PostgreSQL: the identity column is GENERATED ALWAYS, not BY DEFAULT. `auto_increment` says it is an identity (or
    /// a serial, which carries its nextval default) at all.
    pub identity_always: bool,
    /// PostgreSQL: the auto increment is a serial, an integer whose default is the `nextval` of a sequence the column owns, as
    /// the catalog read it. A typed `nextval(...)` default is not one: it names a sequence the column does not own.
    pub serial: bool,
}

impl ColDef {
    pub fn new(name: &str, ty: &str) -> ColDef {
        ColDef { name: name.into(), ty: ty.into(), args: String::new(), unsigned: false, zerofill: false, nullable: true, default: Dv::Null, auto_increment: false, collation: None, on_update: None, comment: String::new(), srid: None, identity_always: false, serial: false }
    }

    /// What the structure read holds about a column. Generated, invisible and compressed columns and MariaDB
    /// column CHECKs carry attributes this definition does not write, so redefining them would lose them:
    /// those are refused.
    pub fn from_column(c: &Column) -> Result<ColDef, String> {
        // Only a PostgreSQL column has a catalog kind.
        if c.kind.is_some() {
            return super::pg::ddl::col_def(c);
        }
        let extra = c.extra.to_ascii_uppercase().replace("DEFAULT_GENERATED", "");
        if extra.contains("GENERATED") || extra.contains("INVISIBLE") || extra.contains("PERSISTENT") {
            return Err(format!("Column {} is generated or invisible. Edit it with a query.", c.name));
        }
        if c.full_type.to_ascii_uppercase().contains("COMPRESSED") {
            return Err(format!("Column {} is COMPRESSED, which this editor would drop. Edit it with a query.", c.name));
        }
        let (mut ty, mut args, unsigned) = split_type(&c.full_type);
        let mut collation = c.collation.clone();
        if let Some(chk) = &c.check {
            // MariaDB's JSON is a LONGTEXT with this CHECK, and declaring JSON again brings both back.
            if ty == "LONGTEXT" && *chk == format!("json_valid({})", Dialect::MariaDb.quote_ident(&c.name)) {
                (ty, args, collation) = ("JSON".into(), String::new(), None);
            } else {
                return Err(format!("Column {} has its own CHECK ({chk}), which this editor would drop. Edit it with a query.", c.name));
            }
        }
        let tail = match c.full_type.rfind(')') {
            Some(i) => &c.full_type[i + 1..],
            None => c.full_type.split_once(' ').map_or("", |(_, t)| t),
        };
        let zerofill = tail.to_ascii_uppercase().split_whitespace().any(|w| w == "ZEROFILL");
        let lower = c.extra.to_ascii_lowercase();
        let on_update = lower.find("on update ").map(|i| c.extra[i + 10..].trim().to_string()).filter(|s| !s.is_empty());
        if on_update.as_deref().is_some_and(|s| !s.bytes().all(|b| b.is_ascii_alphanumeric() || b" _(),".contains(&b))) {
            return Err(format!("Column {} has an ON UPDATE clause that cannot be edited here.", c.name));
        }
        let default = match (&c.default, c.default_expr) {
            (Some(d), true) => Dv::Expr(d.clone()),
            (Some(d), false) => Dv::Value(d.clone()),
            (None, _) if c.nullable => Dv::Null,
            (None, _) => Dv::None,
        };
        Ok(ColDef { name: c.name.clone(), ty, args, unsigned, zerofill, nullable: c.nullable, default, auto_increment: lower.contains("auto_increment"), collation, on_update, comment: c.comment.clone(), srid: c.srid, identity_always: false, serial: false })
    }

    /// `from_column` for a column of `st`. A collation that is the table's default is left out, so an edit or an
    /// undo does not pin a charset the column only inherited.
    pub fn in_table(st: &Structure, c: &Column) -> Result<ColDef, String> {
        let mut d = ColDef::from_column(c)?;
        if d.collation.as_deref().is_some_and(|x| x == st.status.collation) {
            d.collation = None;
        }
        Ok(d)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Pos {
    /// A new column goes last; an existing one stays where it is.
    Keep,
    First,
    After(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum IndexKind {
    #[default]
    Index,
    Unique,
    Fulltext,
    Spatial,
    Primary,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IdxCol {
    pub name: String,
    pub prefix: Option<u64>,
    pub desc: bool,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct IndexDef {
    pub name: String,
    pub kind: IndexKind,
    pub cols: Vec<IdxCol>,
    pub comment: String,
    /// INVISIBLE on MySQL, IGNORED on MariaDB.
    pub invisible: bool,
    /// A key part is an expression, which `cols` cannot hold, so the index cannot be written back.
    pub functional: bool,
    /// PostgreSQL: a primary key or unique constraint owns the index, so it is dropped and renamed as the constraint.
    pub constraint: bool,
    /// PostgreSQL: the access method when it is not btree (hash, gin, gist, brin, spgist), lower case.
    pub method: String,
    /// PostgreSQL: the constraint is DEFERRABLE, and INITIALLY DEFERRED.
    pub deferrable: bool,
    pub deferred: bool,
    /// PostgreSQL: the comment on the constraint, apart from the one on its index.
    pub con_comment: String,
    /// PostgreSQL: the WHERE condition of a partial index, as `pg_get_expr` prints it or as the person writes it.
    pub predicate: String,
    /// PostgreSQL: columns stored in the index beside its key (INCLUDE).
    pub include: Vec<String>,
    /// PostgreSQL: CREATE INDEX CONCURRENTLY, which cannot run in the transaction the other changes share.
    pub concurrently: bool,
    /// PostgreSQL: `pg_get_indexdef`, the server's own text for the index. Only an index that `functional` says the fields
    /// cannot describe is written from it, which is what lets an undo bring any dropped index back exactly.
    pub raw: String,
}

impl IndexDef {
    pub fn from_index(ix: &Index) -> IndexDef {
        // PostgreSQL's index text says what the columns alone do not: a condition, included columns, an operator class.
        let pg = !ix.def.is_empty();
        let kind = if ix.name == "PRIMARY" || ix.primary {
            IndexKind::Primary
        } else if ix.kind.eq_ignore_ascii_case("FULLTEXT") {
            IndexKind::Fulltext
        } else if ix.kind.eq_ignore_ascii_case("SPATIAL") {
            IndexKind::Spatial
        } else if ix.unique {
            IndexKind::Unique
        } else {
            IndexKind::Index
        };
        let functional = ix.cols.iter().any(|c| c.expr.is_some()) || (pg && ix.complex) || (ix.constraint && !ix.primary && !ix.unique);
        let method = if pg && !ix.kind.eq_ignore_ascii_case("btree") { ix.kind.to_ascii_lowercase() } else { String::new() };
        IndexDef { name: ix.name.clone(), kind, cols: ix.cols.iter().map(|c| IdxCol { name: c.name.clone(), prefix: c.prefix, desc: c.desc }).collect(), comment: ix.comment.clone(), invisible: ix.invisible, functional, constraint: ix.constraint, method, deferrable: ix.deferrable, deferred: ix.deferred, con_comment: ix.con_comment.clone(), predicate: ix.predicate.clone(), include: ix.include.clone(), concurrently: false, raw: if pg { ix.def.clone() } else { String::new() } }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Add { def: ColDef, pos: Pos },
    /// `from` is the name now; a different `def.name` renames the column (CHANGE).
    Modify { from: String, def: ColDef, pos: Pos },
    Drop(String),
    AddIndex(IndexDef),
    DropIndex(String),
    /// `def` is the index as it is now, for servers that cannot RENAME INDEX.
    RenameIndex { from: String, to: String, def: IndexDef },
    /// `db` of the key is the table's own schema; a `ref_db` equal to it is written without a schema.
    AddFk(ForeignKey),
    DropFk(String),
    /// PostgreSQL only: RENAME CONSTRAINT.
    RenameFk { from: String, to: String },
    /// PostgreSQL only: ALTER CONSTRAINT, which changes whether a key is deferrable and starts deferred without dropping it
    /// and so without the lock a drop takes.
    SetFkDeferral { name: String, deferrable: bool, deferred: bool },
}

impl Change {
    /// One line for the staged list.
    pub fn describe(&self) -> String {
        match self {
            Change::Add { def, .. } => format!("Add column {}", def.name),
            Change::Modify { from, def, .. } if *from != def.name => format!("Change column {from} to {}", def.name),
            Change::Modify { from, pos, .. } if *pos != Pos::Keep => format!("Change and move column {from}"),
            Change::Modify { from, .. } => format!("Change column {from}"),
            Change::Drop(n) => format!("Drop column {n}"),
            Change::AddIndex(d) => format!("Add index {}", if d.kind == IndexKind::Primary { "PRIMARY" } else { &d.name }),
            Change::DropIndex(n) => format!("Drop index {n}"),
            Change::RenameIndex { from, to, .. } => format!("Rename index {from} to {to}"),
            Change::AddFk(k) => format!("Add foreign key {}", k.name),
            Change::DropFk(n) => format!("Drop foreign key {n}"),
            Change::RenameFk { from, to } => format!("Rename foreign key {from} to {to}"),
            Change::SetFkDeferral { name, .. } => format!("Change when foreign key {name} is checked"),
        }
    }
}

/// Column and index names compare without case on both engines.
pub fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Whether two foreign key names are the same key: exactly on PostgreSQL, which tells `fk` from `FK`, without case on MySQL.
pub fn fk_name_is(pg: bool, a: &str, b: &str) -> bool {
    if pg { a == b } else { same_name(a, b) }
}

/// A server limit on names: 64 characters, no trailing space.
pub fn check_ident(name: &str, what: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(format!("{what} needs a name."));
    }
    if name.chars().count() > 64 || name.ends_with(' ') || name.contains('\0') {
        return Err(format!("{what} name is too long or ends with a space."));
    }
    Ok(())
}

/// An expression typed by the person must stay one expression: balanced brackets and quotes, no
/// second statement and no comment that could swallow the rest of the ALTER. Quotes end where the
/// session's sql_mode ends them: under ANSI_QUOTES a double quote is an identifier, where a backslash
/// escapes nothing.
pub fn check_expr(s: &str, m: Mode) -> Result<(), String> {
    let t = s.trim();
    if t.is_empty() {
        return Err("The default expression is empty.".into());
    }
    let b: Vec<char> = t.chars().collect();
    let (mut i, mut depth) = (0, 0i32);
    while i < b.len() {
        let c = b[i];
        match c {
            '\'' | '"' | '`' => {
                let ident = c == '`' || (c == '"' && m.ansi);
                i += 1;
                loop {
                    match b.get(i) {
                        None => return Err("The default expression has an unclosed quote.".into()),
                        Some('\\') if m.bs && !ident => i += 2,
                        Some(&q) if q == c => {
                            if b.get(i + 1) == Some(&c) {
                                i += 2;
                            } else {
                                break;
                            }
                        }
                        Some(_) => i += 1,
                    }
                }
            }
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return Err("The default expression closes a bracket it never opened.".into());
                }
            }
            ';' | '#' => return Err("The default expression must be one expression, without ; or comments.".into()),
            '-' if b.get(i + 1) == Some(&'-') => return Err("The default expression must be one expression, without ; or comments.".into()),
            '/' if b.get(i + 1) == Some(&'*') => return Err("The default expression must be one expression, without ; or comments.".into()),
            _ => {}
        }
        i += 1;
    }
    if depth != 0 {
        return Err("The default expression has an unclosed bracket.".into());
    }
    Ok(())
}

pub fn is_text(ty: &str) -> bool {
    matches!(ty, "CHAR" | "VARCHAR" | "TINYTEXT" | "TEXT" | "MEDIUMTEXT" | "LONGTEXT" | "ENUM" | "SET")
}

pub fn is_numeric(ty: &str) -> bool {
    matches!(ty, "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "INTEGER" | "BIGINT" | "DECIMAL" | "NUMERIC" | "FLOAT" | "DOUBLE" | "REAL")
}

pub fn is_integer(ty: &str) -> bool {
    matches!(ty, "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "INTEGER" | "BIGINT")
}

pub(super) fn is_geometry(ty: &str) -> bool {
    matches!(ty, "GEOMETRY" | "POINT" | "LINESTRING" | "POLYGON" | "MULTIPOINT" | "MULTILINESTRING" | "MULTIPOLYGON" | "GEOMETRYCOLLECTION")
}

pub(super) fn is_binary(ty: &str) -> bool {
    matches!(ty, "BINARY" | "VARBINARY" | "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB")
}

/// `0x6162`, the way MySQL reports the default of a binary column.
pub(super) fn is_hex_lit(v: &str) -> bool {
    v.strip_prefix("0x").is_some_and(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// `b'101'`, the way both engines report the default of a BIT column.
pub(super) fn is_bit_lit(v: &str) -> bool {
    v.len() > 3 && v.starts_with("b'") && v.ends_with('\'') && v[2..v.len() - 1].bytes().all(|b| b == b'0' || b == b'1')
}

/// The values of an ENUM or SET written as 'a','b' with '' for a quote and MySQL's backslash escapes. The
/// server writes COLUMN_TYPE that way in every sql_mode, so the escapes are read whatever the session does.
pub fn parse_values(args: &str) -> Result<Vec<String>, String> {
    let b: Vec<char> = args.trim().chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    loop {
        while b.get(i).is_some_and(|c| c.is_whitespace()) {
            i += 1;
        }
        if b.get(i) != Some(&'\'') {
            return Err("List the values as 'a','b'.".into());
        }
        i += 1;
        let mut v = String::new();
        loop {
            match b.get(i) {
                None => return Err("A value is missing its closing quote.".into()),
                Some('\\') => match b.get(i + 1) {
                    Some(&n) => {
                        match n {
                            '0' => v.push('\0'),
                            'b' => v.push('\x08'),
                            'n' => v.push('\n'),
                            'r' => v.push('\r'),
                            't' => v.push('\t'),
                            'Z' => v.push('\x1a'),
                            // MySQL keeps the backslash before these, for LIKE patterns.
                            '%' | '_' => {
                                v.push('\\');
                                v.push(n);
                            }
                            o => v.push(o),
                        }
                        i += 2;
                    }
                    None => return Err("A value is missing its closing quote.".into()),
                },
                Some('\'') if b.get(i + 1) == Some(&'\'') => {
                    v.push('\'');
                    i += 2;
                }
                Some('\'') => {
                    i += 1;
                    break;
                }
                Some(&c) => {
                    v.push(c);
                    i += 1;
                }
            }
        }
        out.push(v);
        while b.get(i).is_some_and(|c| c.is_whitespace()) {
            i += 1;
        }
        match b.get(i) {
            None => return Ok(out),
            Some(',') => i += 1,
            Some(_) => return Err("Separate the values with commas.".into()),
        }
    }
}

/// The values as the server writes them and `parse_values` reads them: 'a','it''s','back\\slash'.
#[cfg(test)]
pub(super) fn values_text(vals: &[String]) -> String {
    let one = |v: &String| {
        let mut s = String::from("'");
        for c in v.chars() {
            match c {
                '\\' => s.push_str("\\\\"),
                '\'' => s.push_str("''"),
                '\n' => s.push_str("\\n"),
                '\r' => s.push_str("\\r"),
                '\0' => s.push_str("\\0"),
                '\x08' => s.push_str("\\b"),
                '\x1a' => s.push_str("\\Z"),
                c => s.push(c),
            }
        }
        s.push('\'');
        s
    };
    vals.iter().map(one).collect::<Vec<_>>().join(",")
}

pub(super) fn is_number(s: &str) -> bool {
    let t = s.strip_prefix(['-', '+']).unwrap_or(s);
    let (mant, exp) = match t.split_once(['e', 'E']) {
        Some((m, x)) => (m, Some(x.strip_prefix(['-', '+']).unwrap_or(x))),
        None => (t, None),
    };
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    let ok = match mant.split_once('.') {
        Some((a, b)) => (digits(a) || a.is_empty()) && (digits(b) || b.is_empty()) && (digits(a) || digits(b)),
        None => digits(mant),
    };
    ok && exp.is_none_or(digits)
}

/// The actions a foreign key can take, as the server writes them.
pub const FK_ACTIONS: [&str; 4] = ["RESTRICT", "CASCADE", "SET NULL", "NO ACTION"];
